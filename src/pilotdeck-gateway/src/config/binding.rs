//! Read-only StaffDeck runtime binding transformation. No queues or provisioning.
use super::{Env, Error, exists, graph, host_io_error, read_error, structured_clone, value::*};
use crate::metadata::{Files, Metadata, object, resolve};
use pilotdeck_protocol::{JsValue, parse_json};
use pilotdeck_yaml::{Options, parse_document};

fn failure(message: impl Into<DiagnosticText>) -> Error {
    let mut e = Error::new("gateway_request_failed", message);
    e.name = "TypeError";
    e
}
fn property<'a>(v: &'a JsValue, k: &str) -> Result<&'a JsValue, Error> {
    if nullish(v) {
        return Err(failure(format!(
            "Cannot read properties of {} (reading '{k}')",
            js_string(v)
        )));
    }
    Ok(field(v, k))
}
fn truthy(v: &JsValue) -> bool {
    match v {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}
fn set(v: &mut JsValue, key: &str, value: JsValue) {
    if let JsValue::Object(o) = v {
        o.insert(key.into(), value);
    }
    // Array named properties do not participate in config validation/serialization.
}
/// StringNumericLiteral, including JS whitespace, radix prefixes and Infinity.
/// Non-decimal prefixes cannot carry a sign; Rust's inf/NaN aliases are not JS literals.
pub(super) fn lock_pid(bytes: &[u8]) -> Option<i32> {
    let text = String::from_utf8_lossy(bytes);
    let text = crate::metadata::js_trim(&text);
    let number = if text.is_empty() {
        0.0
    } else if matches!(text, "Infinity" | "+Infinity" | "-Infinity") {
        f64::INFINITY
    } else if text.starts_with("0x")
        || text.starts_with("0X")
        || text.starts_with("0o")
        || text.starts_with("0O")
        || text.starts_with("0b")
        || text.starts_with("0B")
    {
        let radix = match text.as_bytes()[1].to_ascii_lowercase() {
            b'x' => 16,
            b'o' => 8,
            _ => 2,
        };
        if text.len() == 2 {
            return None;
        }
        let mut n = 0.0;
        for c in text[2..].chars() {
            n = n * f64::from(radix) + f64::from(c.to_digit(radix)?);
        }
        n
    } else {
        static DECIMAL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"^[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?$")
                .unwrap()
        });
        if !DECIMAL.is_match(text) {
            return None;
        }
        text.parse::<f64>().ok()?
    };
    // Number.isSafeInteger and Node process.kill's int32 check both precede kill.
    (number.is_finite() && number.fract() == 0.0 && number > 0.0 && number <= f64::from(i32::MAX))
        .then_some(number as i32)
}
fn lock_live<F: Files>(metadata: &Metadata<F>) -> Result<bool, Error> {
    let bytes = match metadata
        .files
        .read(&metadata.home.join("staffdeck/project-bindings.json.lock"))
    {
        Ok(bytes) => bytes,
        Err(e) if crate::node_io::lookup(&e).is_some() => return Ok(false),
        Err(_) => return Err(host_io_error()),
    };
    Ok(lock_pid(&bytes).is_some_and(|pid| metadata.files.process_alive(pid)))
}

pub(super) fn apply<F: Files>(
    metadata: &Metadata<F>,
    raw: JsValue,
    project: Option<&str>,
    env: &Env,
) -> Result<JsValue, Error> {
    if env
        .get("STAFFDECK_PROJECT_BINDING_ENABLED")
        .map(String::as_str)
        != Some("true")
    {
        return Ok(raw);
    }
    let mut binding = None;
    if let Some(project) = project.filter(|p| !p.is_empty()) {
        let path = metadata.home.join("staffdeck/project-bindings.json");
        if exists(metadata, &path)? {
            let invalid = || {
                Error::new(
                    "PROJECT_BINDING_STORE_INVALID",
                    "StaffDeck binding storage is invalid.",
                )
            };
            let bytes = metadata.files.read(&path).map_err(|e| {
                if crate::node_io::lookup(&e).is_some() {
                    invalid()
                } else {
                    host_io_error()
                }
            })?;
            let store = parse_json(&String::from_utf8_lossy(&bytes)).map_err(|_| invalid())?;
            let version = property(&store, "version")?;
            if version.as_number() != Some(1.0)
                || !truthy(field(&store, "installationId"))
                || !matches!(field(&store, "projects"), JsValue::Array(_))
            {
                return Err(invalid());
            }
            let JsValue::Array(rows) = field(&store, "projects") else {
                unreachable!()
            };
            let root = resolve(&metadata.cwd, project)
                .to_string_lossy()
                .into_owned();
            for row in rows {
                if property(row, "projectRoot")?
                    .as_string()
                    .is_some_and(|s| s.equals_str(&root))
                {
                    binding = Some(row.clone());
                    break;
                }
            }
        }
    }
    let mut next = structured_clone(&raw);
    let mut modules = field(&next, "modules").clone();
    if nullish(&modules) {
        modules = object([]);
    }
    let live = if let Some(b) = &binding {
        eq(field(b, "status"), "bound")
            || (eq(field(b, "status"), "binding")
                && truthy(field(b, "runtimeReady"))
                && lock_live(metadata)?)
    } else {
        false
    };
    let matches = binding.as_ref().is_some_and(|b| {
        field(b, "tenantId")
            == &env
                .get("STAFFDECK_COPY_TENANT_ID")
                .map_or(JsValue::Undefined, |s| s.as_str().into())
            && field(b, "origin")
                == &env
                    .get("STAFFDECK_FORMAL_API_ORIGIN")
                    .map_or(JsValue::Undefined, |s| {
                        s.strip_suffix('/').unwrap_or(s).into()
                    })
    });
    // The disabled branch assigns knowledge immediately; the enabled branch first
    // reads/parses the SOP bundle and preserves timeouts before the assignment.
    if !live || !matches {
        ensure_modules(&modules, "knowledge")?;
        set(
            &mut modules,
            "knowledge",
            object([("enabled", JsValue::Bool(false))]),
        );
        set(
            &mut modules,
            "sop",
            object([("enabled", JsValue::Bool(false))]),
        );
        let mut webui = spread_object(field(&next, "webui"));
        set(
            &mut webui,
            "staffdeckCopy",
            object([("enabled", JsValue::Bool(false))]),
        );
        set(&mut next, "webui", webui);
    } else {
        let b = binding.as_ref().unwrap();
        let binding_id = field(b, "bindingId");
        let Some(id) = binding_id
            .as_string()
            .map(|s| String::from_utf16_lossy(s.units()))
        else {
            let Some(received) = crate::metadata::argument_received(binding_id) else {
                let mut e = Error::new("JS_HOST_REQUIRED", "");
                e.requires_host = true;
                return Err(e);
            };
            return Err(Error::new(
                "ERR_INVALID_ARG_TYPE",
                format!("The \"path\" argument must be of type string. Received {received}"),
            ));
        };
        // path.join normalizes segments, but does not reset at an absolute later segment.
        let definitions = resolve(
            &metadata.cwd,
            &format!("{}/staffdeck/{id}/sops.json", metadata.home.display()),
        );
        let bundle = if exists(metadata, &definitions)? {
            let bytes = metadata
                .files
                .read(&definitions)
                .map_err(|e| read_error(&e, &definitions.to_string_lossy()))?;
            let doc = parse_document(
                &String::from_utf8_lossy(&bytes).into_owned().into(),
                Options::default(),
            );
            if let Some(e) = doc.errors.first() {
                return Err(Error::new(e.code, e.message.clone()));
            }
            graph::inspect_sops(
                &doc.to_json()
                    .map_err(|e| Error::new("gateway_request_failed", e.message))?,
            )
        } else {
            object([("sops", JsValue::Array(vec![]))])
        };
        let sops = property(&bundle, "sops")?;
        let first = match sops {
            JsValue::Array(a) => a.first().unwrap_or(&UNDEFINED),
            _ => field(sops, "0"),
        };
        let default = if nullish(field(first, "id")) {
            field(first, "skill_id").clone()
        } else {
            field(first, "id").clone()
        };
        let origin = field(b, "origin").clone();
        let endpoint = format!("{}/api/v1", js_string(&origin));
        let agent = field(b, "agentId").clone();
        let encoded = encode_uri_component(&agent)?;
        let previous_sop = field(&modules, "sop");
        let mut sop = object([
            ("enabled", JsValue::Bool(true)),
            ("provider", "staffdeck".into()),
            (
                "endpoint",
                env.get("STAFFDECK_SOP_RUNTIME_ORIGIN")
                    .map_or(JsValue::Undefined, |s| s.as_str().into()),
            ),
            (
                "definitionsPath",
                definitions.to_string_lossy().as_ref().into(),
            ),
            ("defaultSopId", default.clone()),
            ("discoveryEndpoint", endpoint.as_str().into()),
            ("discoveryAgentId", agent.clone()),
            (
                "discoveryApiKey",
                "${STAFFDECK_SOP_MANAGEMENT_API_KEY}".into(),
            ),
            ("timeoutMs", JsValue::Number(30000.0)),
            ("discoveryTimeoutMs", JsValue::Number(90000.0)),
        ]);
        for key in ["timeoutMs", "discoveryTimeoutMs"] {
            let v = field(previous_sop, key);
            if v.as_number()
                .is_some_and(|n| n > 0.0 && n <= 9007199254740991.0 && n.fract() == 0.0)
            {
                set(&mut sop, key, v.clone());
            }
        }
        ensure_modules(&modules, "knowledge")?;
        set(
            &mut modules,
            "knowledge",
            object([
                ("enabled", JsValue::Bool(true)),
                ("implementationId", "staffdeck.knowledge".into()),
                ("contract", "staffdeck.knowledge/v1".into()),
                ("transport", "module-http-v2".into()),
                ("endpoint", origin.clone()),
                (
                    "manifestPath",
                    "/api/v1/knowledge-module/module-manifest".into(),
                ),
                (
                    "callPath",
                    format!("/api/v1/agents/{encoded}/knowledge-module/v2/module/call")
                        .as_str()
                        .into(),
                ),
                ("methods", JsValue::Array(vec!["query".into()])),
                ("agentId", agent.clone()),
                ("credentialEnv", "STAFFDECK_SOP_MANAGEMENT_API_KEY".into()),
            ]),
        );
        let mut webui = spread_object(field(&next, "webui"));
        set(
            &mut webui,
            "staffdeckCopy",
            object([
                ("enabled", JsValue::Bool(true)),
                ("contract", "staffdeck.enterprise-copy/v1".into()),
                (
                    "methods",
                    JsValue::Array(
                        [
                            "list_agents",
                            "list_knowledge_bases",
                            "list_skills",
                            "import_resources",
                        ]
                        .into_iter()
                        .map(JsValue::from)
                        .collect(),
                    ),
                ),
                ("endpoint", origin),
                ("tenantId", field(b, "tenantId").clone()),
                (
                    "actorUserId",
                    env.get("STAFFDECK_COPY_ACTOR_USER_ID")
                        .map_or(JsValue::Undefined, |s| s.as_str().into()),
                ),
                ("targetAgentId", agent.clone()),
                (
                    "pilotDeckUserId",
                    env.get("STAFFDECK_COPY_PILOTDECK_USER_ID")
                        .map_or(JsValue::Undefined, |s| s.as_str().into()),
                ),
                ("userTokenEnv", "STAFFDECK_COPY_USER_TOKEN".into()),
            ]),
        );
        set(&mut next, "webui", webui);
        set(
            &mut sop,
            "management",
            object([
                ("enabled", JsValue::Bool(true)),
                ("endpoint", endpoint.as_str().into()),
                ("apiKeyEnv", "STAFFDECK_SOP_MANAGEMENT_API_KEY".into()),
                (
                    "credentialIdEnv",
                    "STAFFDECK_SOP_MANAGEMENT_CREDENTIAL_ID".into(),
                ),
                ("agentId", agent),
                (
                    "methods",
                    JsValue::Array(
                        [
                            "list",
                            "create",
                            "get_draft",
                            "replace_draft",
                            "validate",
                            "publish",
                            "archive",
                            "list_versions",
                            "get_version",
                            "rollback",
                        ]
                        .into_iter()
                        .map(JsValue::from)
                        .collect(),
                    ),
                ),
            ]),
        );
        set(
            &mut modules,
            "sop",
            if truthy(&default) {
                sop
            } else {
                object([("enabled", JsValue::Bool(false))])
            },
        );
    }
    set(&mut next, "modules", modules);
    Ok(next)
}
fn spread_object(v: &JsValue) -> JsValue {
    if record(v) {
        return v.clone();
    }
    let mut result = object([]);
    if let JsValue::Object(o) = &mut result {
        match v {
            JsValue::Array(items) => {
                for (i, value) in items.iter().enumerate() {
                    o.insert(i.to_string().into(), value.clone());
                }
            }
            JsValue::String(s) => {
                for (i, unit) in s.units().iter().enumerate() {
                    o.insert(
                        i.to_string().into(),
                        JsValue::String(pilotdeck_protocol::JsString::from_utf16(vec![*unit])),
                    );
                }
            }
            _ => {}
        }
    }
    result
}
fn ensure_modules(m: &JsValue, key: &str) -> Result<(), Error> {
    if record(m) || matches!(m, JsValue::Array(_)) {
        return Ok(());
    }
    let kind = match m {
        JsValue::Number(_) => "number",
        JsValue::Bool(_) => "boolean",
        _ => "string",
    };
    Err(failure(format!(
        "Cannot create property '{key}' on {kind} '{}'",
        js_string(m)
    )))
}
fn encode_uri_component(v: &JsValue) -> Result<String, Error> {
    if !string_coercible(v) {
        return Err(primitive_error());
    }
    let s = if let JsValue::String(s) = v {
        s.to_utf8()
            .map_err(|_| Error::new("gateway_request_failed", "URI malformed"))?
    } else {
        js_string(v)
    };
    let mut output = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            output.push(char::from(b));
        } else {
            output.push_str(&format!("%{b:02X}"));
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn webui_spread_preserves_array_indices_in_both_binding_branches() {
        let fixture = parse_json(include_str!(
            "../../tests/fixtures/session-model/webui-spread.json"
        ))
        .unwrap();
        let JsValue::Array(rows) = field(&fixture, "spread") else {
            panic!("spread fixtures")
        };
        let io_fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/session-model/io-errors.json"
        ))
        .unwrap();
        // A bound store with matching identity enables the binding; omitting
        // that identity exercises the disabled branch over the same input.
        let metadata = crate::session_model::tests::snapshot(
            io_fixture["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["id"] == "io-transcript-read-missing")
                .unwrap(),
        );
        for row in rows {
            let mut env = Env::from([("STAFFDECK_PROJECT_BINDING_ENABLED".into(), "true".into())]);
            if field(row, "enabled") == &JsValue::Bool(true) {
                env.insert(
                    "STAFFDECK_FORMAL_API_ORIGIN".into(),
                    "https://staffdeck.invalid".into(),
                );
                env.insert("STAFFDECK_COPY_TENANT_ID".into(), "tenant".into());
            }
            let next = apply(
                &metadata,
                field(row, "input").clone(),
                Some("/fixture/home"),
                &env,
            )
            .unwrap();
            assert_eq!(
                pilotdeck_protocol::stringify_json(field(&next, "webui")).unwrap(),
                pilotdeck_protocol::stringify_json(field(row, "webui")).unwrap(),
            );
        }
    }
    #[test]
    fn js_number_and_node_int32_gate_match_recorded_node_validation() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/session-model/numbers.json"
        ))
        .unwrap();
        assert_eq!(fixture["syscallInjected"], true);
        for row in fixture["cases"].as_array().unwrap() {
            let text = row["text"].as_str().unwrap();
            let expected = row["pid"].as_i64().map(|n| i32::try_from(n).unwrap());
            assert_eq!(lock_pid(text.as_bytes()), expected, "{text:?}");
        }
    }
}

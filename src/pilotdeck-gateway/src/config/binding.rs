//! Read-only StaffDeck runtime binding transformation. No queues or provisioning.
use super::{Env, Error, exists, graph, host_io_error, read_error, structured_clone, value::*};
use crate::metadata::{Files, Metadata, object};
use pilotdeck_protocol::{JsValue, parse_json};
use pilotdeck_yaml::{Options, parse_document};

fn failure(message: impl Into<DiagnosticText>) -> Error {
    let mut e = Error::new("gateway_request_failed", message);
    e.name = "TypeError";
    e
}
pub(super) fn invalid_path_argument(received: &pilotdeck_protocol::JsString) -> Error {
    let mut e = Error::new(
        "ERR_INVALID_ARG_TYPE",
        crate::metadata::invalid_arg_type_message("path", received),
    );
    // `path.join` validates its segments before anything else, and a store row
    // is not validated (`readProjectBinding` returns it as parsed), so a
    // non-string `bindingId` throws Node's `TypeError [ERR_INVALID_ARG_TYPE]`
    // (Node v22.22.0 probe:
    // `_tmp/20261112-issue-1833/impl-r1/node-probe/a1-error-classes-probe.out`).
    // The class matters: only a `PilotConfigError` replaces the retained
    // reload diagnostics, and TS propagates the raw TypeError (#1833 A1).
    e.name = "TypeError";
    e
}
/// `encodeURIComponent` on a string `String(value)` kept with lone surrogates
/// throws `URIError: URI malformed`; TS propagates that class, so the reload
/// path must not mistake it for a `PilotConfigError` (#1550 修 r6).
fn uri_error() -> Error {
    let mut e = Error::new("gateway_request_failed", "URI malformed");
    e.name = "URIError";
    e
}
fn host_required() -> Error {
    let mut e = Error::new("JS_HOST_REQUIRED", "");
    e.requires_host = true;
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
        Err(e) if crate::node_io::config_lookup(&e).is_some() => return Ok(false),
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
                if crate::node_io::config_lookup(&e).is_some() {
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
            let root =
                crate::metadata::resolve_process_path(&metadata.cwd, std::path::Path::new(project))
                    .map_err(super::cwd_error)?
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
            let received = match crate::metadata::argument_received(binding_id) {
                crate::metadata::ArgumentReceived::Text(received) => received,
                crate::metadata::ArgumentReceived::Throws { .. } => return Err(host_required()),
            };
            return Err(invalid_path_argument(&received));
        };
        // path.join normalizes segments, but does not reset at an absolute later segment.
        let definitions = crate::metadata::resolve_process_path(
            &metadata.cwd,
            std::path::Path::new(&format!(
                "{}/staffdeck/{id}/sops.json",
                metadata.home.display()
            )),
        )
        .map_err(super::cwd_error)?;
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
                // The baseline reads the bundle with yaml's `parse`, which
                // throws `doc.errors[0]` itself — a `YAMLParseError` (Node
                // v22.22.0 probe:
                // `_tmp/20261112-issue-1833/impl-r1/node-probe/a2-sops-probe.out`).
                // The default `PilotConfigError` name would make a failed
                // reload replace the retained diagnostics (#1833 A2).
                let mut error = Error::new(e.code, e.message.clone());
                error.name = e.name;
                return Err(error);
            }
            match doc.to_json() {
                // `parse` runs `doc.toJS` unguarded: an alias-count or depth
                // failure throws its own class (`ReferenceError`,
                // `RangeError`), which the default name would collapse the
                // same way (#1833 A2).
                Ok(graph) => graph::inspect_sops(&graph),
                Err(e) => {
                    let mut error = Error::new("gateway_request_failed", e.message);
                    error.name = e.name;
                    return Err(error);
                }
            }
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
    require_string_coercible(v)?;
    // `String(array)` keeps lone surrogates and `encodeURIComponent` then
    // throws `URIError: URI malformed`; the lossy `js_string` used to encode
    // U+FFFD and succeed (#1550 修 r5).
    let s = match v {
        JsValue::String(s) => s.to_utf8().ok(),
        JsValue::Array(_) => crate::js_value::to_string_units(v)
            .ok()
            .and_then(|units| String::from_utf16(&units).ok()),
        _ => Some(js_string(v)),
    }
    .ok_or_else(uri_error)?;
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

    /// `agentId` comes straight out of `project-bindings.json`; a deep array
    /// must fail the coercion instead of recursing until the process aborts.
    #[test]
    fn uri_component_coercion_is_bounded_instead_of_aborting() {
        let mut deep = JsValue::from("x");
        for _ in 0..crate::js_value::MAX_ARRAY_DEPTH + 1 {
            deep = JsValue::Array(vec![deep]);
        }
        let error = encode_uri_component(&deep).expect_err("past the shared depth bound");
        assert_eq!(error.name, "RangeError");
        assert_eq!(
            error.message.to_utf8().expect("utf-8 message"),
            crate::js_value::StackOverflow::MESSAGE
        );

        let with_to_string = object([("toString", JsValue::Null)]);
        let error = encode_uri_component(&with_to_string).expect_err("own toString property");
        assert_eq!(error.name, "TypeError");
        assert_eq!(
            error.message.to_utf8().expect("utf-8 message"),
            crate::js_value::ToStringError::NOT_PRIMITIVE
        );

        let mut at_bound = JsValue::from("x");
        for _ in 0..crate::js_value::MAX_ARRAY_DEPTH {
            at_bound = JsValue::Array(vec![at_bound]);
        }
        assert!(encode_uri_component(&at_bound).is_ok());
    }

    /// `String(["\ud800"])` keeps the lone surrogate and TS's
    /// `encodeURIComponent` then throws `URIError: URI malformed`. The array
    /// branch used to go through the lossy `js_string`, which replaced it with
    /// U+FFFD and encoded `%EF%BF%BD` (#1550 修 r5). The error class matters:
    /// `config/store.rs` only records reload diagnostics for
    /// `PilotConfigError`, which is the name `Error::new` defaults to, while
    /// TS propagates a plain `URIError` (#1550 修 r6).
    #[test]
    fn uri_component_rejects_a_lone_surrogate_inside_an_array() {
        let lone = pilotdeck_protocol::JsString::from_utf16(vec![0xd800]);
        let error = encode_uri_component(&JsValue::Array(vec![JsValue::String(lone)]))
            .expect_err("encodeURIComponent throws URIError");
        assert_eq!(error.name, "URIError");
        assert_eq!(
            error.message.to_utf8().ok().as_deref(),
            Some("URI malformed")
        );
    }

    /// The scalar path reaches the same `URIError` (a lone surrogate survives
    /// `String(value)` in both shapes).
    #[test]
    fn uri_component_rejects_a_lone_surrogate_string() {
        let lone = pilotdeck_protocol::JsString::from_utf16(vec![0xd800]);
        let error = encode_uri_component(&JsValue::String(lone))
            .expect_err("encodeURIComponent throws URIError");
        assert_eq!(error.name, "URIError");
        assert_eq!(
            error.message.to_utf8().ok().as_deref(),
            Some("URI malformed")
        );
    }

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

    /// A path-keyed file table for the sops read surface. `read` failures are
    /// errno-shaped so `read_error` maps them like a real fs error, and a
    /// present-but-unreadable entry keeps `stat` succeeding — the
    /// existsSync/readFileSync race the loader really runs (#1833 A1).
    struct SopsFiles {
        files: Vec<(std::path::PathBuf, Vec<u8>)>,
        unreadable: Vec<std::path::PathBuf>,
    }
    impl Files for SopsFiles {
        fn entries(&self, _: &std::path::Path) -> std::io::Result<Vec<(String, bool)>> {
            Err(std::io::Error::from_raw_os_error(libc::ENOENT))
        }
        fn read(&self, path: &std::path::Path) -> std::io::Result<Vec<u8>> {
            if self.unreadable.iter().any(|p| p == path) {
                return Err(std::io::Error::from_raw_os_error(libc::EACCES));
            }
            self.files
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, bytes)| bytes.clone())
                .ok_or_else(|| std::io::Error::from_raw_os_error(libc::ENOENT))
        }
        fn stat(&self, path: &std::path::Path) -> Result<crate::metadata::Stat, std::io::Error> {
            let present = self.files.iter().any(|(p, _)| p == path)
                || self.unreadable.iter().any(|p| p == path);
            if present {
                Ok(crate::metadata::Stat {
                    directory: false,
                    size: 0,
                    birth_ms: 0.0,
                    ctime_ms: 0.0,
                    mtime_ms: 0.0,
                })
            } else {
                Err(std::io::Error::from_raw_os_error(libc::ENOENT))
            }
        }
    }
    fn binding_env() -> Env {
        Env::from([
            ("STAFFDECK_PROJECT_BINDING_ENABLED".into(), "true".into()),
            (
                "STAFFDECK_FORMAL_API_ORIGIN".into(),
                "https://staffdeck.invalid".into(),
            ),
            ("STAFFDECK_COPY_TENANT_ID".into(), "tenant".into()),
        ])
    }
    fn sops_metadata(
        files: Vec<(std::path::PathBuf, Vec<u8>)>,
        unreadable: Vec<std::path::PathBuf>,
    ) -> Metadata<SopsFiles> {
        Metadata {
            home: "/fixture/home".into(),
            cwd: "/fixture".into(),
            files: SopsFiles { files, unreadable },
        }
    }
    fn binding_store(binding_id: JsValue) -> Vec<u8> {
        let store = object([
            ("version", JsValue::Number(1.0)),
            ("installationId", "installation".into()),
            (
                "projects",
                JsValue::Array(vec![object([
                    ("projectRoot", "/fixture/home".into()),
                    ("bindingId", binding_id),
                    ("agentId", "agent".into()),
                    ("status", "bound".into()),
                    ("origin", "https://staffdeck.invalid".into()),
                    ("tenantId", "tenant".into()),
                ])]),
            ),
        ]);
        pilotdeck_protocol::stringify_json(&store)
            .expect("#1833 serialize the binding store")
            .expect("#1833 serialize the binding store")
            .into_bytes()
    }
    fn definitions_path() -> std::path::PathBuf {
        std::path::Path::new("/fixture/home/staffdeck/binding-1/sops.json").to_path_buf()
    }

    /// #1833 A1: a non-string `bindingId` reaches `path.join`, which throws
    /// `TypeError [ERR_INVALID_ARG_TYPE]` (Node v22.22.0 probe:
    /// `_tmp/20261112-issue-1833/impl-r1/node-probe/a1-error-classes-probe.out`).
    /// The class must survive `apply`: only a `PilotConfigError` replaces the
    /// retained reload diagnostics, and TS propagates the raw TypeError.
    #[test]
    fn non_string_binding_id_reports_the_path_join_type_error() {
        let store = std::path::Path::new("/fixture/home")
            .join("staffdeck/project-bindings.json")
            .to_path_buf();
        let metadata = sops_metadata(vec![(store, binding_store(JsValue::Number(5.0)))], vec![]);
        let error = apply(&metadata, object([]), Some("/fixture/home"), &binding_env())
            .expect_err("#1833 a non-string bindingId must fail the binding");
        assert_eq!(
            error.name, "TypeError",
            "#1833 path.join's TypeError class must not collapse to PilotConfigError"
        );
        assert_eq!(
            error.code, "ERR_INVALID_ARG_TYPE",
            "#1833 the Node code must stay"
        );
    }

    /// #1833 A1: `sops.json` exists but the read fails — the exists/read race.
    /// TS's `readFileSync` throws Node's fs error (`Error` with the errno
    /// code, same probe as above), so the class must be `Error`, not the
    /// default `PilotConfigError` a reload would treat as a config failure.
    #[test]
    fn sops_read_race_reports_the_node_error_class() {
        let store = std::path::Path::new("/fixture/home")
            .join("staffdeck/project-bindings.json")
            .to_path_buf();
        let metadata = sops_metadata(
            vec![(store, binding_store("binding-1".into()))],
            vec![definitions_path()],
        );
        let error = apply(&metadata, object([]), Some("/fixture/home"), &binding_env())
            .expect_err("#1833 an unreadable sops.json must fail the binding");
        assert_eq!(
            error.name, "Error",
            "#1833 the fs error class must not collapse to PilotConfigError"
        );
        assert_eq!(error.code, "EACCES", "#1833 the errno code must stay");
        assert_eq!(
            error.message.to_utf8().ok().as_deref(),
            Some("EACCES: permission denied, open '/fixture/home/staffdeck/binding-1/sops.json'"),
            "#1833 the message must be Node's fs wording"
        );
    }

    /// #1833 A2: the baseline reads the bundle with yaml's `parse`, which
    /// throws `doc.errors[0]` itself — a `YAMLParseError` (Node v22.22.0
    /// probe:
    /// `_tmp/20261112-issue-1833/impl-r1/node-probe/a2-sops-probe.out`).
    /// Rebuilding the error with `Error::new` dropped the class and made a
    /// failed reload replace the retained diagnostics.
    #[test]
    fn sops_yaml_parse_failure_keeps_the_yaml_parse_error_class() {
        let store = std::path::Path::new("/fixture/home")
            .join("staffdeck/project-bindings.json")
            .to_path_buf();
        let metadata = sops_metadata(
            vec![
                (store, binding_store("binding-1".into())),
                (definitions_path(), b"sops: [unclosed\n".to_vec()),
            ],
            vec![],
        );
        let error = apply(&metadata, object([]), Some("/fixture/home"), &binding_env())
            .expect_err("#1833 invalid sops yaml must fail the binding");
        assert_eq!(
            error.name, "YAMLParseError",
            "#1833 parse()'s doc.errors[0] class must survive apply"
        );
        assert!(
            error.message.to_utf8().ok().is_some_and(|m| m
                .starts_with("Flow sequence in block collection must be sufficiently indented")),
            "#1833 the yaml diagnostic message must stay (got {:?})",
            String::from_utf16_lossy(error.message.units())
        );
    }

    /// #1833 A2: `parse` runs `doc.toJS` unguarded; an excessive alias count
    /// throws its own `ReferenceError` (Node v22.22.0 probe:
    /// `_tmp/20261112-issue-1833/impl-r1/node-probe/excessive-alias-inline.out`).
    /// The `doc.to_json()` failure path used to rebuild the error with the
    /// default name, dropping the class the same way.
    #[test]
    fn sops_alias_conversion_failure_keeps_its_error_class() {
        let store = std::path::Path::new("/fixture/home")
            .join("staffdeck/project-bindings.json")
            .to_path_buf();
        // count(*a) = 11 references, aliasCount(a) = 10 inner aliases: 110 >
        // yaml's maxAliasCount (100), the same arithmetic as
        // nodes/Alias.js:68-70.
        let aliases = |n: usize, name: &str| vec![name; n].join(", ");
        let sops = format!(
            "b: &b [1]\na: &a [{}]\nz: [{}]\n",
            aliases(10, "*b"),
            aliases(11, "*a"),
        );
        let metadata = sops_metadata(
            vec![
                (store, binding_store("binding-1".into())),
                (definitions_path(), sops.into_bytes()),
            ],
            vec![],
        );
        let error = apply(&metadata, object([]), Some("/fixture/home"), &binding_env())
            .expect_err("#1833 an excessive alias count must fail the binding");
        assert_eq!(
            error.name, "ReferenceError",
            "#1833 toJS's ReferenceError class must survive apply"
        );
        assert_eq!(
            error.message.to_utf8().ok().as_deref(),
            Some("Excessive alias count indicates a resource exhaustion attack"),
            "#1833 the alias-count message must stay"
        );
    }
}

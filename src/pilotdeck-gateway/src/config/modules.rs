use super::{Diagnostics, Env, model::credential, value::*};
use pilotdeck_protocol::JsValue;
use std::{collections::HashSet, sync::OnceLock};
/// Core module slots, in the baseline order. The snapshot projection
/// (`snapshot::modules`) iterates the same list.
pub(super) const CORE: &[&str] = &[
    "agentLoop",
    "skills",
    "modelProvider",
    "tools",
    "context",
    "knowledge",
];
/// Module contract table shared by validation and the snapshot projection.
pub(super) fn data() -> &'static JsValue {
    static DATA: OnceLock<JsValue> = OnceLock::new();
    DATA.get_or_init(|| pilotdeck_protocol::parse_json(include_str!("contracts.json")).unwrap())
}
fn warn_unknown(v: &JsValue, allowed: &[&str], path: &str, d: &mut Diagnostics) {
    for (k, _) in entries(v) {
        let k = js_string(&JsValue::String(k.clone()));
        if !allowed.contains(&k.as_str()) {
            d.warn(
                "MODULE_UNKNOWN_FIELD",
                format!("Unknown field '{path}.{k}'."),
                &format!("{path}.{k}"),
            );
        }
    }
}
fn methods(v: &JsValue) -> Option<Vec<String>> {
    let JsValue::Array(a) = v else { return None };
    if a.is_empty() {
        return None;
    }
    let items: Option<Vec<_>> = a
        .iter()
        .map(|v| {
            v.as_string()
                .map(trim)
                .filter(|s| !s.is_empty())
                .map(|s| js_string(&JsValue::String(s)))
        })
        .collect();
    let items = items?;
    (items.iter().collect::<HashSet<_>>().len() == items.len()).then_some(items)
}
fn port(v: &JsValue) -> bool {
    number(v, true, true) && v.as_number().unwrap() <= 65535.0
}
fn deployment(v: &JsValue, path: &str, d: &mut Diagnostics) {
    if nullish(v) {
        return;
    }
    let path = format!("{path}.deployment");
    if !record(v) {
        d.fatal(
            "MODULE_DEPLOYMENT_INVALID",
            format!("{path} must be an object."),
            &path,
        );
        return;
    }
    let mode = nonempty(field(v, "mode"));
    if !mode
        .as_deref()
        .is_some_and(|s| ["build", "image", "external"].contains(&s))
    {
        d.fatal(
            "MODULE_DEPLOYMENT_MODE_INVALID",
            format!("{path}.mode must be build, image, or external."),
            &format!("{path}.mode"),
        );
        return;
    }
    if mode.as_deref() == Some("external") {
        return;
    }
    if !port(field(v, "port")) {
        d.fatal(
            "MODULE_DEPLOYMENT_PORT_INVALID",
            format!("{path}.port must be an integer between 1 and 65535."),
            &format!("{path}.port"),
        );
    }
    let health = field(v, "healthPath");
    if !health.is_undefined() && !nonempty(health).is_some_and(|s| s.starts_with('/')) {
        d.fatal(
            "MODULE_DEPLOYMENT_HEALTH_PATH_INVALID",
            format!("{path}.healthPath must start with '/'."),
            &format!("{path}.healthPath"),
        );
    }
    let fields: &[(&str, &str, &str)] = if mode.as_deref() == Some("image") {
        &[(
            "image",
            "MODULE_DEPLOYMENT_IMAGE_INVALID",
            "a non-empty string",
        )]
    } else {
        &[
            (
                "context",
                "MODULE_DEPLOYMENT_CONTEXT_INVALID",
                "a non-empty path",
            ),
            (
                "dockerfile",
                "MODULE_DEPLOYMENT_DOCKERFILE_INVALID",
                "a non-empty path",
            ),
        ]
    };
    for (key, code, msg) in fields {
        if *key == "dockerfile" && field(v, key).is_undefined() {
            continue;
        }
        if nonempty(field(v, key)).is_none() {
            d.fatal(
                code,
                format!("{path}.{key} must be {msg}."),
                &format!("{path}.{key}"),
            );
        }
    }
}
fn contract(slot: &str, c: &str, t: &str, m: &[String], d: &mut Diagnostics) {
    let expected = text(field(field(data(), slot), "contract")).unwrap();
    let transports: &[&str] = if slot == "agentLoop" {
        &["module-stdio-v2", "module-tcp-v2"]
    } else {
        &["module-http-v2"]
    };
    let allowed = field(field(data(), slot), "methods");
    let required: &[&str] = match slot {
        "modelProvider" => &["prepare", "stream"],
        "skills" => &["list", "read"],
        "tools" | "agentLoop" => &["execute"],
        "context" => &["prepare_for_model"],
        _ => &["query"],
    };
    let msg = if c != expected {
        Some(format!("modules.{slot}.contract must be {expected}."))
    } else if !transports.contains(&t) {
        Some(format!(
            "modules.{slot}.transport must be one of {}.",
            transports.join(", ")
        ))
    } else if let Some(unknown) = m.iter().find(|s| match allowed {
        JsValue::Array(a) => !a.iter().any(|v| eq(v, s)),
        _ => true,
    }) {
        Some(format!(
            "modules.{slot}.methods contains unsupported operation '{unknown}'."
        ))
    } else {
        required
            .iter()
            .find(|s| !m.iter().any(|m| m == **s))
            .map(|s| format!("modules.{slot}.methods must include '{s}'."))
    };
    if let Some(msg) = msg {
        d.fatal(
            "MODULE_CONTRACT_UNSUPPORTED",
            msg,
            &format!("modules.{slot}"),
        );
    }
}
pub(super) fn parse(raw: &JsValue, env: &Env, d: &mut Diagnostics) {
    if nullish(raw) {
        return;
    }
    if !record(raw) {
        d.fatal("MODULES_INVALID", "modules must be an object.", "modules");
        return;
    }
    for (k, _) in entries(raw) {
        let k = js_string(&JsValue::String(k.clone()));
        if !CORE.contains(&k.as_str()) && k != "sop" {
            d.warn(
                "MODULES_UNKNOWN_FIELD",
                format!("Unknown modules field '{k}'."),
                &format!("modules.{k}"),
            );
        }
    }
    let parsed: Vec<_> = CORE
        .iter()
        .map(|name| core(name, field(raw, name), d))
        .collect();
    if parsed[0] == Some(true) && parsed.iter().skip(1).any(|v| *v == Some(true)) {
        for (name, p) in CORE.iter().zip(parsed) {
            if p.is_none() {
                d.fatal("MODULE_CORE_BINDING_REQUIRED",format!("modules.{name} must be explicitly bound when any external core module is selected."),&format!("modules.{name}"));
            }
        }
    }
    sop(field(raw, "sop"), env, d);
}
fn core(slot: &str, v: &JsValue, d: &mut Diagnostics) -> Option<bool> {
    if nullish(v) {
        return None;
    }
    let path = format!("modules.{slot}");
    if !record(v) {
        d.fatal(
            "MODULE_INVALID",
            format!("{path} must be an object."),
            &path,
        );
        return None;
    }
    if !field(v, "stateMode").is_undefined() {
        d.fatal("MODULE_STATE_MODE_UNSUPPORTED",format!("{path}.stateMode is not part of the published module contract and cannot be accepted implicitly."),&format!("{path}.stateMode"));
    }
    warn_unknown(
        v,
        &[
            "enabled",
            "provider",
            "implementationId",
            "contract",
            "transport",
            "endpoint",
            "manifestPath",
            "callPath",
            "timeoutMs",
            "methods",
            "catalog",
            "frontendModule",
            "command",
            "args",
            "env",
            "host",
            "port",
            "connectTimeoutMs",
            "deployment",
            "defaultBaseId",
            "resultLimit",
            "tenantId",
            "actorUserId",
            "agentId",
            "credentialEnv",
        ],
        &path,
        d,
    );
    if field(v, "enabled").as_bool() == Some(false) && ["skills", "knowledge"].contains(&slot) {
        return Some(false);
    }
    if field(v, "enabled").as_bool() != Some(true) {
        d.fatal(
            "MODULE_ENABLED_INVALID",
            format!("{path}.enabled must be true for a required binding."),
            &format!("{path}.enabled"),
        );
        return None;
    }
    let external = [
        "implementationId",
        "contract",
        "transport",
        "endpoint",
        "methods",
        "deployment",
    ]
    .iter()
    .any(|key| !field(v, key).is_undefined());
    if !field(v, "provider").is_undefined() && external {
        d.fatal(
            "MODULE_BINDING_CONFLICT",
            format!("{path} must use either provider or protocol binding fields, not both."),
            &path,
        );
        return None;
    }
    if !external {
        if !eq(field(v, "provider"), "pilotdeck") {
            d.fatal(
                "MODULE_PROVIDER_UNSUPPORTED",
                format!("{path} provider must be pilotdeck or use a protocol binding."),
                &format!("{path}.provider"),
            );
            return None;
        }
        return Some(false);
    }
    let implementation = nonempty(field(v, "implementationId"));
    let c = nonempty(field(v, "contract"));
    let t = nonempty(field(v, "transport"));
    let ep = nonempty(field(v, "endpoint"));
    let manifest = nonempty(field(v, "manifestPath")).unwrap_or_else(|| "/module-manifest".into());
    let call = nonempty(field(v, "callPath")).unwrap_or_else(|| "/v2/module/call".into());
    let parsed_methods = methods(field(v, "methods"));
    let default_methods = match field(field(data(), slot), "methods") {
        JsValue::Array(a) => a.iter().map(js_string).collect(),
        _ => vec![],
    };
    let m = parsed_methods.as_ref().unwrap_or(&default_methods);
    for (key, missing, code) in [
        (
            "implementationId",
            implementation.is_none(),
            "MODULE_IMPLEMENTATION_ID_INVALID",
        ),
        ("contract", c.is_none(), "MODULE_CONTRACT_INVALID"),
        ("transport", t.is_none(), "MODULE_TRANSPORT_INVALID"),
    ] {
        if missing {
            d.fatal(
                code,
                format!("{path}.{key} must be a non-empty string."),
                &format!("{path}.{key}"),
            );
        }
    }
    let methods_valid = field(v, "methods").is_undefined() || parsed_methods.is_some();
    if slot == "agentLoop" {
        if !methods_valid {
            d.fatal(
                "MODULE_METHODS_INVALID",
                format!("{path}.methods must be a non-empty array of unique strings."),
                &format!("{path}.methods"),
            );
        }
        if let (Some(c), Some(t)) = (&c, &t) {
            contract(slot, c, t, m, d);
        }
        let expected = eq(field(v, "contract"), "pilotdeck.agent-loop/v1");
        match t.as_deref() {
            Some("module-stdio-v2") => {
                let command = nonempty(field(v, "command"));
                let args = field(v, "args");
                let args_valid = args.is_undefined() || strings(args);
                let env = field(v, "env");
                let env_valid =
                    record(env) && entries(env).iter().all(|(_, v)| v.as_string().is_some());
                if command.is_none() {
                    d.fatal(
                        "MODULE_COMMAND_INVALID",
                        format!("{path}.command must be a non-empty string."),
                        &format!("{path}.command"),
                    );
                }
                if !args_valid {
                    d.fatal(
                        "MODULE_ARGS_INVALID",
                        format!("{path}.args must be an array of strings."),
                        &format!("{path}.args"),
                    );
                }
                if !env.is_undefined() && !env_valid {
                    d.fatal(
                        "MODULE_ENV_INVALID",
                        format!("{path}.env must contain string values."),
                        &format!("{path}.env"),
                    );
                }
                deployment(field(v, "deployment"), &path, d);
                if implementation.is_some()
                    && expected
                    && command.is_some()
                    && args_valid
                    && methods_valid
                {
                    return Some(true);
                }
            }
            Some("module-tcp-v2") => {
                let host = nonempty(field(v, "host"));
                let port_valid = port(field(v, "port"));
                let timeout = field(v, "connectTimeoutMs");
                deployment(field(v, "deployment"), &path, d);
                if host.is_none() {
                    d.fatal(
                        "MODULE_HOST_INVALID",
                        format!("{path}.host must be a non-empty string."),
                        &format!("{path}.host"),
                    );
                }
                if !port_valid {
                    d.fatal(
                        "MODULE_PORT_INVALID",
                        format!("{path}.port must be an integer between 1 and 65535."),
                        &format!("{path}.port"),
                    );
                }
                if !timeout.is_undefined() && !number(timeout, true, true) {
                    d.fatal(
                        "MODULE_TIMEOUT_INVALID",
                        format!("{path}.connectTimeoutMs must be a positive integer."),
                        &format!("{path}.connectTimeoutMs"),
                    );
                }
                if implementation.is_some()
                    && expected
                    && host.is_some()
                    && port_valid
                    && methods_valid
                {
                    return Some(true);
                }
            }
            _ => {}
        }
        return None;
    }
    if !ep.as_deref().is_some_and(|s| http_url(s).is_some()) {
        d.fatal(
            "MODULE_ENDPOINT_INVALID",
            format!("{path}.endpoint must be an absolute http(s) URL."),
            &format!("{path}.endpoint"),
        );
    }
    for (key, s, code) in [
        ("manifestPath", &manifest, "MODULE_MANIFEST_PATH_INVALID"),
        ("callPath", &call, "MODULE_CALL_PATH_INVALID"),
    ] {
        if !s.starts_with('/') {
            d.fatal(
                code,
                format!("{path}.{key} must start with '/'."),
                &format!("{path}.{key}"),
            );
        }
    }
    let timeout = field(v, "timeoutMs");
    if !timeout.is_undefined() && !number(timeout, true, true) {
        d.fatal(
            "MODULE_TIMEOUT_INVALID",
            format!("{path}.timeoutMs must be a positive integer."),
            &format!("{path}.timeoutMs"),
        );
    }
    if !methods_valid {
        d.fatal(
            "MODULE_METHODS_INVALID",
            format!("{path}.methods must be a non-empty array of unique strings."),
            &format!("{path}.methods"),
        );
    }
    if let (Some(c), Some(t)) = (&c, &t) {
        contract(slot, c, t, m, d);
    }
    let mut tools = 0;
    if slot == "tools"
        && let JsValue::Array(a) = field(v, "catalog")
    {
        for (i, tool) in a.iter().enumerate() {
            if !record(tool)
                || nonempty(field(tool, "name")).is_none()
                || nonempty(field(tool, "description")).is_none()
                || !record(field(tool, "inputSchema"))
            {
                d.fatal(
                    "MODULE_TOOL_CATALOG_INVALID",
                    format!("{path}.catalog[{i}] requires name, description, and inputSchema."),
                    &format!("{path}.catalog[{i}]"),
                );
            } else {
                tools += 1;
            }
        }
    }
    deployment(field(v, "deployment"), &path, d);
    let credential = nonempty(field(v, "credentialEnv"));
    if !field(v, "credentialEnv").is_undefined()
        && (slot != "knowledge"
            || credential
                .as_deref()
                .is_none_or(|s| !regex::Regex::new(r"^[A-Z][A-Z0-9_]*$").unwrap().is_match(s)))
    {
        d.fatal(
            "MODULE_CREDENTIAL_ENV_INVALID",
            format!(
                "{path}.credentialEnv must name a server environment variable for Knowledge only."
            ),
            &format!("{path}.credentialEnv"),
        );
    }
    if credential.is_some() && slot == "knowledge" {
        let agent = nonempty(field(v, "agentId"));
        let expected = agent
            .as_deref()
            .map(encode_component)
            .map(|id| format!("/api/v1/agents/{id}/knowledge-module/v2/module/call"));
        if implementation.as_deref() != Some("staffdeck.knowledge")
            || manifest != "/api/v1/knowledge-module/module-manifest"
            || expected.as_deref() != Some(call.as_str())
            || nonempty(field(v, "tenantId")).is_some()
            || nonempty(field(v, "actorUserId")).is_some()
        {
            d.fatal("MODULE_AUTHENTICATED_KNOWLEDGE_BINDING_INVALID",format!("{path} must bind the public Knowledge read endpoint and its fixed agentId without caller identity fields."),&path);
        }
    }
    if slot == "tools" && tools == 0 {
        d.fatal("MODULE_TOOL_CATALOG_REQUIRED",format!("{path}.catalog must declare at least one tool for the synchronous ToolPort list operation."),&format!("{path}.catalog"));
    }
    (implementation.is_some()
        && c.is_some()
        && t.as_deref() == Some("module-http-v2")
        && ep.as_deref().is_some_and(|s| http_url(s).is_some()))
    .then_some(true)
}
fn encode_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
fn sop(v: &JsValue, env: &Env, d: &mut Diagnostics) {
    if nullish(v) {
        return;
    }
    let path = "modules.sop";
    if !record(v) {
        d.fatal("SOP_MODULE_INVALID", "modules.sop must be an object.", path);
        return;
    }
    if !field(v, "stateMode").is_undefined() {
        d.fatal("MODULE_STATE_MODE_UNSUPPORTED","modules.sop.stateMode is not part of the published SOP contract and cannot be accepted implicitly.","modules.sop.stateMode");
    }
    warn_unknown(
        v,
        &[
            "enabled",
            "provider",
            "implementationId",
            "contract",
            "transport",
            "manifestPath",
            "endpoint",
            "definitionsPath",
            "defaultSopId",
            "discoveryEndpoint",
            "discoveryAgentId",
            "discoveryApiKey",
            "discoveryPath",
            "discoveryTimeoutMs",
            "timeoutMs",
            "contextMode",
            "deployment",
            "frontendModule",
        ],
        path,
        d,
    );
    if field(v, "enabled").as_bool() == Some(false) {
        return;
    }
    if field(v, "enabled").as_bool() != Some(true) {
        d.fatal(
            "SOP_MODULE_ENABLED_INVALID",
            "modules.sop.enabled must be a boolean.",
            "modules.sop.enabled",
        );
        return;
    }
    let binding = ["implementationId", "contract", "transport", "manifestPath"]
        .iter()
        .any(|key| !field(v, key).is_undefined());
    if !field(v, "provider").is_undefined() && binding {
        d.fatal(
            "SOP_MODULE_BINDING_CONFLICT",
            "modules.sop must use either legacy provider or protocol binding fields, not both.",
            path,
        );
        return;
    }
    let endpoint = nonempty(field(v, "endpoint"));
    let definitions = nonempty(field(v, "definitionsPath"));
    let default = nonempty(field(v, "defaultSopId"));
    for (key, valid, code, msg) in [
        (
            "endpoint",
            endpoint.as_deref().is_some_and(|s| http_url(s).is_some()),
            "SOP_MODULE_ENDPOINT_INVALID",
            "an absolute http(s) URL",
        ),
        (
            "definitionsPath",
            definitions.is_some(),
            "SOP_MODULE_DEFINITIONS_PATH_INVALID",
            "a non-empty path",
        ),
        (
            "defaultSopId",
            default.is_some(),
            "SOP_MODULE_DEFAULT_ID_INVALID",
            "a non-empty string",
        ),
    ] {
        if !valid {
            d.fatal(
                code,
                format!("{path}.{key} must be {msg}."),
                &format!("{path}.{key}"),
            );
        }
    }
    let discovery = ["discoveryEndpoint", "discoveryAgentId", "discoveryApiKey"]
        .iter()
        .any(|key| !field(v, key).is_undefined());
    deployment(field(v, "deployment"), path, d);
    let timeout = field(v, "timeoutMs");
    if !timeout.is_undefined() && !number(timeout, true, true) {
        d.fatal(
            "SOP_MODULE_TIMEOUT_INVALID",
            "modules.sop.timeoutMs must be a positive integer.",
            "modules.sop.timeoutMs",
        );
    }
    let context = field(v, "contextMode");
    if !context.is_undefined() && !eq(context, "new_session") && !eq(context, "inherit") {
        d.fatal(
            "SOP_MODULE_CONTEXT_MODE_INVALID",
            "modules.sop.contextMode must be new_session or inherit.",
            "modules.sop.contextMode",
        );
    }
    if discovery {
        if !nonempty(field(v, "discoveryEndpoint")).is_some_and(|s| http_url(&s).is_some()) {
            d.fatal(
                "SOP_DISCOVERY_ENDPOINT_INVALID",
                "modules.sop.discoveryEndpoint must be an absolute http(s) URL.",
                "modules.sop.discoveryEndpoint",
            );
        }
        if nonempty(field(v, "discoveryAgentId")).is_none() {
            d.fatal("SOP_DISCOVERY_AGENT_ID_INVALID","modules.sop.discoveryAgentId must be a non-empty string when discovery is configured.","modules.sop.discoveryAgentId");
        }
        if credential(field(v, "discoveryApiKey"), env).is_err() {
            d.fatal("SOP_DISCOVERY_API_KEY_INVALID","modules.sop.discoveryApiKey must be a non-empty string when discovery is configured.","modules.sop.discoveryApiKey");
        }
    }
    let timeout = field(v, "discoveryTimeoutMs");
    if !timeout.is_undefined() && !number(timeout, true, true) {
        d.fatal(
            "SOP_DISCOVERY_TIMEOUT_INVALID",
            "modules.sop.discoveryTimeoutMs must be a positive integer.",
            "modules.sop.discoveryTimeoutMs",
        );
    }
    if endpoint.is_none() || definitions.is_none() || default.is_none() {
        return;
    }
    if !binding {
        if !eq(field(v, "provider"), "staffdeck") {
            d.fatal(
                "SOP_MODULE_PROVIDER_INVALID",
                "modules.sop.provider must be staffdeck or use a protocol binding.",
                "modules.sop.provider",
            );
        }
        return;
    }
    if nonempty(field(v, "implementationId")).is_none() {
        d.fatal(
            "SOP_MODULE_IMPLEMENTATION_ID_INVALID",
            "modules.sop.implementationId must be a non-empty string.",
            "modules.sop.implementationId",
        );
    }
    if !eq(field(v, "contract"), "sop.lifecycle/v2") {
        d.fatal(
            "SOP_MODULE_CONTRACT_UNSUPPORTED",
            "modules.sop.contract must be sop.lifecycle/v2.",
            "modules.sop.contract",
        );
    }
    if !eq(field(v, "transport"), "sop-http-v2") {
        d.fatal(
            "SOP_MODULE_TRANSPORT_UNSUPPORTED",
            "modules.sop.transport must be sop-http-v2.",
            "modules.sop.transport",
        );
    }
    if !nonempty(field(v, "manifestPath"))
        .unwrap_or_else(|| "/healthz".into())
        .starts_with('/')
    {
        d.fatal(
            "SOP_MODULE_MANIFEST_PATH_INVALID",
            "modules.sop.manifestPath must start with '/'.",
            "modules.sop.manifestPath",
        );
    }
}

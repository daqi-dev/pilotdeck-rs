//! Global PilotConfig loading for the native read-only gateway. Every section
//! participates in validation, even when catalog projection does not use it.
mod graph;
mod host;
mod model;
mod modules;
mod router;
mod sections;
mod value;
use crate::metadata::{Files, Metadata, object};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use pilotdeck_yaml::{Options, parse_document};
use std::collections::HashMap;
use value::*;
pub(crate) type Env = HashMap<String, String>;
pub(crate) struct Config {
    pub providers: Vec<model::Provider>,
    pub selection: (JsString, JsString),
    pub router_enabled: bool,
    pub diagnostics: Vec<JsValue>,
}
#[derive(Debug)]
pub(crate) struct Error {
    pub code: String,
    pub message: JsString,
    pub diagnostics: Vec<JsValue>,
    pub name: &'static str,
    pub requires_host: bool,
}
impl Error {
    fn new(code: &str, message: impl Into<DiagnosticText>) -> Self {
        Self {
            code: code.into(),
            message: message.into().0,
            diagnostics: vec![],
            name: "PilotConfigError",
            requires_host: false,
        }
    }
}
pub(super) struct Diagnostics(pub Vec<JsValue>);
impl Diagnostics {
    fn add(&mut self, code: &str, severity: &str, message: impl Into<DiagnosticText>, path: &str) {
        let mut d = JsObject::new();
        d.insert("code".into(), code.into());
        d.insert("severity".into(), severity.into());
        d.insert("message".into(), JsValue::String(message.into().0));
        if !path.is_empty() {
            d.insert("path".into(), JsValue::String(DiagnosticText::from(path).0));
        }
        d.insert("recoverable".into(), JsValue::Bool(severity != "fatal"));
        self.0.push(JsValue::Object(d));
    }
    fn fatal(&mut self, code: &str, msg: impl Into<DiagnosticText>, path: &str) {
        self.add(code, "fatal", msg, path)
    }
    fn warn(&mut self, code: &str, msg: impl Into<DiagnosticText>, path: &str) {
        self.add(code, "warning", msg, path)
    }
    fn check(&self) -> Result<(), Error> {
        if let Some(d) = self.0.iter().find(|d| eq(field(d, "severity"), "fatal")) {
            let mut e = Error::new(&js_string(field(d, "code")), js_string(field(d, "message")));
            e.diagnostics = self.0.clone();
            Err(e)
        } else {
            Ok(())
        }
    }
}
fn merge(left: &mut JsValue, right: JsValue) {
    let mut output = JsObject::new();
    if let JsValue::Object(base) = left {
        for (k, v) in base.entries() {
            output.insert(k.clone(), v.clone());
        }
    }
    if let JsValue::Object(override_value) = &right {
        for (k, v) in override_value.entries() {
            let mut previous = if k.equals_str("__proto__") && !output.contains_key("__proto__") {
                output.prototype().cloned().unwrap_or_else(|| object([]))
            } else {
                output
                    .get_property(&k.to_utf8().unwrap_or_default())
                    .cloned()
                    .unwrap_or(JsValue::Undefined)
            };
            let value = if record(&previous) && record(v) {
                merge(&mut previous, v.clone());
                previous
            } else {
                v.clone()
            };
            if k.equals_str("__proto__") && !output.contains_key("__proto__") {
                if record(&value) || matches!(value, JsValue::Array(_) | JsValue::Null) {
                    output.set_prototype(value);
                }
            } else {
                output.insert(k.clone(), value);
            }
        }
    }
    *left = JsValue::Object(output);
}
pub(crate) fn load<F: Files>(metadata: &Metadata<F>, env: &Env) -> Result<Config, Error> {
    let path = env
        .get("PILOTDECK_CONFIG_PATH")
        .map(|s| crate::metadata::js_trim(s).to_owned())
        .unwrap_or_else(|| {
            metadata
                .home
                .join("pilotdeck.yaml")
                .to_string_lossy()
                .into_owned()
        });
    let mut d = Diagnostics(vec![]);
    let mut raw = object([]);
    let mut source_graph = None;
    // existsSync('') is false, whereas path.resolve('') denotes cwd.
    if !path.is_empty() {
        let absolute = crate::metadata::resolve(&metadata.cwd, &path);
        if metadata.files.stat(&absolute).is_ok() {
            match metadata.files.read(&absolute) {
                Ok(bytes) => {
                    let content = JsString::from(String::from_utf8_lossy(&bytes).into_owned());
                    let document = parse_document(
                        &content,
                        Options {
                            pretty_errors: false,
                            ..Default::default()
                        },
                    );
                    if !document.errors.is_empty() {
                        source_error(
                            &mut d,
                            "CONFIG_YAML_INVALID",
                            "Failed to parse default config YAML.",
                            &path,
                            Some(
                                DiagnosticText::from(
                                    document
                                        .errors
                                        .iter()
                                        .map(|e| js_string(&JsValue::String(e.message.clone())))
                                        .collect::<Vec<_>>()
                                        .join("; "),
                                )
                                .0,
                            ),
                        );
                    } else {
                        match document.to_json().map(|g| {
                            let value = graph::inspect(&g);
                            source_graph = Some(g);
                            value
                        }) {
                            Ok(value) if nullish(&value) => {}
                            Ok(value) if record(&value) => merge(&mut raw, value),
                            Ok(_) => source_error(
                                &mut d,
                                "CONFIG_ROOT_INVALID",
                                "default config root must be an object.",
                                &path,
                                None,
                            ),
                            Err(e) => source_error(
                                &mut d,
                                "CONFIG_YAML_INVALID",
                                "Failed to parse default config YAML.",
                                &path,
                                Some(e.message),
                            ),
                        }
                    }
                }
                Err(e) => source_error(
                    &mut d,
                    "CONFIG_READ_FAILED",
                    "Failed to read default config.",
                    &path,
                    Some(JsString::from(read_hint(&e, &path))),
                ),
            }
        }
    }
    if source_graph.as_ref().is_some_and(graph::merge_cycle) {
        let mut e = Error::new("gateway_request_failed", "Maximum call stack size exceeded");
        e.name = "RangeError";
        return Err(e);
    }
    if let Some(v) = env.get("PILOT_AGENT_MODEL").filter(|v| !v.is_empty()) {
        merge(
            &mut raw,
            object([("agent", object([("model", v.as_str().into())]))]),
        );
    }
    if env
        .get("STAFFDECK_PROJECT_BINDING_ENABLED")
        .is_some_and(|s| s == "true")
    {
        raw = structured_clone(&raw);
        let mut m = field(&raw, "modules").clone();
        if nullish(&m) {
            m = object([]);
        }
        if !record(&m) && !matches!(m, JsValue::Array(_)) {
            let kind = match m {
                JsValue::Number(_) => "number",
                JsValue::Bool(_) => "boolean",
                _ => "string",
            };
            let mut e = Error::new(
                "gateway_request_failed",
                format!(
                    "Cannot create property 'knowledge' on {kind} '{}'",
                    js_string(&m)
                ),
            );
            e.name = "TypeError";
            return Err(e);
        }
        if let JsValue::Object(ref mut m) = m {
            m.insert(
                "knowledge".into(),
                object([("enabled", JsValue::Bool(false))]),
            );
            m.insert("sop".into(), object([("enabled", JsValue::Bool(false))]));
        }
        if let JsValue::Object(ref mut raw) = raw {
            raw.insert("modules".into(), m);
        }
    }
    if !field(&raw, "pilot").is_undefined() {
        d.fatal(
            "CONFIG_PILOT_SECTION_FORBIDDEN",
            "YAML config must not contain a pilot section. Use PILOT_HOME for PilotHome.",
            "pilot",
        );
    }
    for (k, _) in entries(&raw) {
        let k = js_string(&JsValue::String(k.clone()));
        if ![
            "schemaVersion",
            "agent",
            "model",
            "extension",
            "memory",
            "gateway",
            "adapters",
            "router",
            "alwaysOn",
            "cron",
            "tools",
            "proxy",
            "webui",
            "telemetry",
            "modules",
            "runPolicy",
        ]
        .contains(&k.as_str())
        {
            d.warn(
                "CONFIG_UNKNOWN_FIELD",
                format!("Unknown top-level config field {k}."),
                &k,
            );
        }
    }
    let version = field(&raw, "schemaVersion");
    if !string_coercible(version) {
        return Err(primitive_error());
    }
    if version.is_undefined() {
        d.warn(
            "CONFIG_SCHEMA_VERSION_MISSING",
            "schemaVersion is missing; assuming schemaVersion 1.",
            "schemaVersion",
        );
    } else if version.as_number() != Some(1.0) {
        d.fatal(
            "CONFIG_SCHEMA_VERSION_UNSUPPORTED",
            format!("Unsupported schemaVersion {}.", js_string(version)),
            "schemaVersion",
        );
    }
    for (name, code) in [
        ("agent", "CONFIG_AGENT_MISSING"),
        ("model", "CONFIG_MODEL_MISSING"),
    ] {
        if field(&raw, name).is_undefined() {
            d.fatal(
                code,
                format!("Config must contain an {name} section.").replace("an model", "a model"),
                name,
            );
        }
    }
    d.check()?;
    if host::required(&raw) {
        let mut error = Error::new(
            "JS_HOST_REQUIRED",
            "Configuration depends on JavaScript host intrinsic state.",
        );
        error.requires_host = true;
        return Err(error);
    }
    let providers = model::parse(field(&raw, "model"), env, &mut d)?;
    let selection = sections::agent(field(&raw, "agent"), &providers, &mut d)?;
    sections::extension(field(&raw, "extension"), &mut d);
    sections::memory(field(&raw, "memory"), &providers, &mut d)?;
    sections::gateway(field(&raw, "gateway"), &mut d);
    sections::adapters(field(&raw, "adapters"), &mut d);
    let router_enabled = router::parse(field(&raw, "router"), &providers, &selection, &mut d);
    sections::always_on(field(&raw, "alwaysOn"), &mut d)?;
    sections::cron(field(&raw, "cron"), &mut d);
    sections::tools(field(&raw, "tools"), &mut d);
    modules::parse(field(&raw, "modules"), env, &mut d);
    sections::run_policy(field(&raw, "runPolicy"), &mut d);
    sections::proxy(&raw, &mut d);
    d.check()?;
    if source_graph
        .as_ref()
        .is_some_and(|g| graph::retained_cycle(g, &providers, &raw))
    {
        let mut error = Error::new("gateway_request_failed", "Maximum call stack size exceeded");
        error.name = "RangeError";
        return Err(error);
    }
    Ok(Config {
        providers,
        selection,
        router_enabled,
        diagnostics: d.0,
    })
}
fn source_error(d: &mut Diagnostics, code: &str, msg: &str, path: &str, hint: Option<JsString>) {
    let mut fields = JsObject::new();
    fields.insert("code".into(), code.into());
    fields.insert("severity".into(), "fatal".into());
    fields.insert("message".into(), msg.into());
    fields.insert("path".into(), path.into());
    fields.insert(
        "source".into(),
        object([("kind", "default".into()), ("path", path.into())]),
    );
    if let Some(h) = hint {
        fields.insert("hint".into(), JsValue::String(h));
    }
    fields.insert("recoverable".into(), JsValue::Bool(false));
    d.0.push(JsValue::Object(fields));
}
#[cfg(test)]
mod tests;

fn read_hint(e: &std::io::Error, path: &str) -> String {
    let name = match e.kind() {
        std::io::ErrorKind::PermissionDenied => Some(("EACCES", "permission denied")),
        std::io::ErrorKind::NotFound => Some(("ENOENT", "no such file or directory")),
        std::io::ErrorKind::IsADirectory => {
            return "EISDIR: illegal operation on a directory, read".into();
        }
        _ => None,
    };
    if let Some((code, msg)) = name {
        format!("{code}: {msg}, open '{path}'")
    } else {
        e.to_string()
    }
}

fn structured_clone(v: &JsValue) -> JsValue {
    match v {
        JsValue::Object(o) => {
            let mut result = JsObject::new();
            if let Some(tag) = o.native_tag() {
                result.set_native_tag(tag);
            }
            for (k, v) in o.entries() {
                result.insert(k.clone(), structured_clone(v));
            }
            JsValue::Object(result)
        }
        JsValue::Array(a) => JsValue::Array(a.iter().map(structured_clone).collect()),
        _ => v.clone(),
    }
}

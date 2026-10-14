//! Global/project PilotConfig loading for the native read-only gateway. Every section
//! participates in validation, even when catalog projection does not use it.
mod binding;
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
pub(crate) use value::string_coercible;
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
    load_project(metadata, env, None)
}
pub(crate) fn load_project<F: Files>(
    metadata: &Metadata<F>,
    env: &Env,
    project: Option<&str>,
) -> Result<Config, Error> {
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
    let mut source_graphs = vec![];
    read_source(
        metadata,
        &path,
        "default",
        &mut raw,
        &mut d,
        &mut source_graphs,
    )?;
    if let Some(project) = project.filter(|p| !p.is_empty()) {
        let path =
            crate::metadata::resolve(&metadata.cwd, project).join(".pilotdeck/pilotdeck.yaml");
        read_source(
            metadata,
            &path.to_string_lossy(),
            "project",
            &mut raw,
            &mut d,
            &mut source_graphs,
        )?;
    }
    if source_graphs.iter().any(graph::merge_cycle) {
        let mut e = Error::new("gateway_request_failed", "Maximum call stack size exceeded");
        e.name = "RangeError";
        return Err(e);
    }
    let merged_graph = graph::merged_sources(&source_graphs)?;
    if let Some(v) = env.get("PILOT_AGENT_MODEL").filter(|v| !v.is_empty()) {
        merge(
            &mut raw,
            object([("agent", object([("model", v.as_str().into())]))]),
        );
    }
    raw = binding::apply(metadata, raw, project, env)?;
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
    if graph::retained_cycle(&merged_graph, &providers, &raw) {
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
fn read_source<F: Files>(
    metadata: &Metadata<F>,
    path: &str,
    kind: &str,
    raw: &mut JsValue,
    d: &mut Diagnostics,
    source_graphs: &mut Vec<pilotdeck_yaml::value::ValueGraph>,
) -> Result<(), Error> {
    // existsSync('') is false, whereas path.resolve('') denotes cwd.
    if !path.is_empty() {
        let absolute = crate::metadata::resolve(&metadata.cwd, path);
        if exists(metadata, &absolute)? {
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
                            kind,
                            d,
                            "CONFIG_YAML_INVALID",
                            &format!("Failed to parse {kind} config YAML."),
                            path,
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
                            source_graphs.push(g);
                            value
                        }) {
                            Ok(value) if nullish(&value) => {}
                            Ok(value) if record(&value) => merge(raw, value),
                            Ok(_) => source_error(
                                kind,
                                d,
                                "CONFIG_ROOT_INVALID",
                                &format!("{kind} config root must be an object."),
                                path,
                                None,
                            ),
                            Err(e) => source_error(
                                kind,
                                d,
                                "CONFIG_YAML_INVALID",
                                &format!("Failed to parse {kind} config YAML."),
                                path,
                                Some(e.message),
                            ),
                        }
                    }
                }
                Err(e) => source_error(
                    kind,
                    d,
                    "CONFIG_READ_FAILED",
                    &format!("Failed to read {kind} config."),
                    path,
                    Some(JsString::from(read_hint(&e, path)?)),
                ),
            }
        }
    }
    Ok(())
}
fn source_error(
    kind: &str,
    d: &mut Diagnostics,
    code: &str,
    msg: &str,
    path: &str,
    hint: Option<JsString>,
) {
    let mut fields = JsObject::new();
    fields.insert("code".into(), code.into());
    fields.insert("severity".into(), "fatal".into());
    fields.insert("message".into(), msg.into());
    fields.insert("path".into(), path.into());
    fields.insert(
        "source".into(),
        object([("kind", kind.into()), ("path", path.into())]),
    );
    if let Some(h) = hint {
        fields.insert("hint".into(), JsValue::String(h));
    }
    fields.insert("recoverable".into(), JsValue::Bool(false));
    d.0.push(JsValue::Object(fields));
}
#[cfg(test)]
mod tests;

fn host_io_error() -> Error {
    let mut error = Error::new("NODE_IO_REQUIRED", "");
    error.requires_host = true;
    error
}
fn exists<F: Files>(metadata: &Metadata<F>, path: &std::path::Path) -> Result<bool, Error> {
    // Node existsSync suppresses validatePath's NUL error before any syscall.
    // This is a measured argument rule, not an inferred OS errno.
    if path.to_string_lossy().contains('\0') {
        return Ok(false);
    }
    match metadata.files.stat(path) {
        Ok(_) => Ok(true),
        Err(e) if crate::node_io::config_lookup(&e).is_some() => Ok(false),
        Err(_) => Err(host_io_error()),
    }
}
fn read_hint(e: &std::io::Error, path: &str) -> Result<String, Error> {
    Ok(crate::node_io::config_read_file(e, path)
        .ok_or_else(host_io_error)?
        .1)
}
pub(crate) fn read_error(e: &std::io::Error, path: &str) -> Error {
    match crate::node_io::config_read_file(e, path) {
        Some((code, message)) => Error::new(code, message),
        None => host_io_error(),
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

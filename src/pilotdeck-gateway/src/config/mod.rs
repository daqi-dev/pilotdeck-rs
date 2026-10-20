//! Global/project PilotConfig loading for the native read-only gateway. Every section
//! participates in validation, even when catalog projection does not use it.
mod binding;
pub mod changes;
mod graph;
mod host;
mod model;
mod modules;
pub mod permission;
mod router;
mod sections;
pub mod session_overrides;
pub mod session_value;
pub mod snapshot;
pub mod store;
mod value;
use crate::metadata::{Files, Metadata, object};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use pilotdeck_yaml::{Options, parse_document};
use std::collections::HashMap;
use value::*;
pub(crate) use value::{builtin_property, string_coercible};
pub type Env = HashMap<String, String>;
pub struct Config {
    pub(crate) providers: Vec<model::Provider>,
    pub(crate) selection: (JsString, JsString),
    pub(crate) router_enabled: bool,
    pub diagnostics: Vec<JsValue>,
    pub(crate) snapshot: Option<JsValue>,
}
#[derive(Debug)]
pub struct Error {
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
                // #261: merge reads `output[key]` with the same JavaScript
                // lookup as the diff (own, explicit prototype chain, builtin
                // members), not the narrower object-only helper.
                changes::property_value(&output, k)
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
    load_project_with_files(metadata, env, None)
}

/// Load the complete executable configuration snapshot described by the fixed
/// TypeScript baseline's `loadPilotConfig`. `loaded_at` is passed explicitly so
/// callers and fixtures can make the otherwise wall-clock field deterministic.
pub fn load_snapshot(
    pilot_home: &std::path::Path,
    cwd: &std::path::Path,
    env: &Env,
    project: Option<&str>,
    version: f64,
    loaded_at: &str,
) -> Result<JsValue, Error> {
    let metadata = Metadata {
        home: pilot_home.to_path_buf(),
        cwd: absolute_cwd(cwd),
        files: crate::metadata::NativeFiles,
    };
    Ok(load_project_with_files_inner(
        &metadata,
        env,
        project,
        Some((version, JsString::from(loaded_at))),
    )?
    .snapshot
    .expect("snapshot loader always records a snapshot"))
}
/// `cwd` stands in for the baseline's `process.cwd()`, which is always
/// absolute; `path.resolve` anchors a relative argument the same way.
pub(crate) fn absolute_cwd(cwd: &std::path::Path) -> std::path::PathBuf {
    if cwd.is_absolute() {
        cwd.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|dir| dir.join(cwd))
            .unwrap_or_else(|_| cwd.to_path_buf())
    }
}

/// `resolvePilotConfigPath` for an explicit PilotHome: a trimmed
/// `PILOTDECK_CONFIG_PATH` wins (even when empty), else `<home>/pilotdeck.yaml`.
pub(crate) fn global_config_path(pilot_home: &std::path::Path, env: &Env) -> String {
    env.get("PILOTDECK_CONFIG_PATH")
        .map(|s| crate::metadata::js_trim(s).to_owned())
        .unwrap_or_else(|| {
            pilot_home
                .join("pilotdeck.yaml")
                .to_string_lossy()
                .into_owned()
        })
}

/// `getPilotProjectConfigFilePath(projectRoot)` with `projectRoot` resolved
/// against `cwd` as the loader does; `None` for an absent or empty project.
pub(crate) fn project_config_path(
    cwd: &std::path::Path,
    project: Option<&str>,
) -> Option<std::path::PathBuf> {
    project
        .filter(|p| !p.is_empty())
        .map(|project| crate::metadata::resolve(cwd, project).join(".pilotdeck/pilotdeck.yaml"))
}

/// Load and validate the global and optional project configuration using explicit
/// paths and environment. This is the same loader used by gateway methods; it
/// neither reads the process environment nor starts a gateway or a JS fallback.
/// Callers must handle `Error::requires_host` as an unsupported native branch.
pub fn load_project(
    pilot_home: &std::path::Path,
    cwd: &std::path::Path,
    env: &Env,
    project: Option<&str>,
) -> Result<Config, Error> {
    let metadata = Metadata {
        home: pilot_home.to_path_buf(),
        cwd: absolute_cwd(cwd),
        files: crate::metadata::NativeFiles,
    };
    load_project_with_files(&metadata, env, project)
}

pub(crate) fn load_project_with_files<F: Files>(
    metadata: &Metadata<F>,
    env: &Env,
    project: Option<&str>,
) -> Result<Config, Error> {
    load_project_with_files_inner(metadata, env, project, None)
}

fn load_project_with_files_inner<F: Files>(
    metadata: &Metadata<F>,
    env: &Env,
    project: Option<&str>,
    snapshot_context: Option<(f64, JsString)>,
) -> Result<Config, Error> {
    let path = global_config_path(&metadata.home, env);
    let mut d = Diagnostics(vec![]);
    let mut raw = object([]);
    let mut source_graphs = vec![];
    let full_values = snapshot_context.is_some();
    let mut sources = vec![];
    if env.get("PILOT_HOME").is_some_and(|v| !v.is_empty()) {
        sources.push(snapshot::SourceRecord::bootstrap_env());
    }
    read_source(
        metadata,
        &path,
        "default",
        10.0,
        &mut raw,
        &mut d,
        &mut source_graphs,
        &mut sources,
        full_values,
    )?;
    if let Some(path) = project_config_path(&metadata.cwd, project) {
        read_source(
            metadata,
            &path.to_string_lossy(),
            "project",
            20.0,
            &mut raw,
            &mut d,
            &mut source_graphs,
            &mut sources,
            full_values,
        )?;
    }
    if source_graphs.iter().any(graph::merge_cycle) {
        let mut e = Error::new("gateway_request_failed", "Maximum call stack size exceeded");
        e.name = "RangeError";
        return Err(e);
    }
    let merged_graph = graph::merged_sources(&source_graphs)?;
    let mut env_config = JsValue::Undefined;
    if let Some(v) = env.get("PILOT_AGENT_MODEL").filter(|v| !v.is_empty()) {
        let override_value = object([("agent", object([("model", v.as_str().into())]))]);
        env_config = override_value.clone();
        merge(&mut raw, override_value);
    }
    if !env_config.is_undefined() {
        sources.push(snapshot::SourceRecord::merge_env(&env_config));
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
    let snapshot = snapshot_context.map(|(version, loaded_at)| {
        snapshot::build(
            metadata, env, &raw, &providers, &selection, sources, version, &loaded_at, &d.0,
        )
    });
    Ok(Config {
        providers,
        selection,
        router_enabled,
        diagnostics: d.0,
        snapshot,
    })
}
#[allow(clippy::too_many_arguments)]
fn read_source<F: Files>(
    metadata: &Metadata<F>,
    path: &str,
    kind: &str,
    priority: f64,
    raw: &mut JsValue,
    d: &mut Diagnostics,
    source_graphs: &mut Vec<pilotdeck_yaml::value::ValueGraph>,
    sources: &mut Vec<snapshot::SourceRecord>,
    full_values: bool,
) -> Result<(), Error> {
    // existsSync('') is false, whereas path.resolve('') denotes cwd.
    if !path.is_empty() {
        let absolute = crate::metadata::resolve(&metadata.cwd, path);
        if exists(metadata, &absolute)? {
            match metadata.files.read(&absolute) {
                Ok(bytes) => {
                    let content = JsString::from(String::from_utf8_lossy(&bytes).into_owned());
                    sources.push(snapshot::SourceRecord::file(kind, priority, path, &content));
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
                            let value = if full_values {
                                graph::inspect_full(&g)
                            } else {
                                graph::inspect(&g)
                            };
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

pub(crate) fn collation_host_error() -> Error {
    let mut error = Error::new("JS_HOST_REQUIRED", "");
    error.requires_host = true;
    error
}
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

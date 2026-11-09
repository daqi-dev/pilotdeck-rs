//! Web UI configuration defaults, legacy Office migration and runtime env.

use super::{Env, value::*};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    fs, io,
    io::Write,
    path::{Path, PathBuf},
};

const DEFAULT_HOST: &str = "0.0.0.0";
const DEFAULT_SERVER_PORT: f64 = 3001.0;
const DEFAULT_VITE_PORT: f64 = 5173.0;
const DEFAULT_API_TIMEOUT_MS: f64 = 120_000.0;

/// UI-service validation. Gateway config loading intentionally does not call
/// this: the fixed baseline reserves and round-trips `webui.*` without parsing
/// it, while the UI configuration service validates before saving/starting.
pub fn validation_errors(raw: &JsValue) -> Vec<String> {
    let mut errors = Vec::new();
    let office = field(field(raw, "webui"), "officePreview");
    if let Some(service) = field(office, "service").as_string() {
        let configured = lower_ascii(service);
        let legacy = field(office, "spreadsheetMode")
            .as_string()
            .map(lower_ascii)
            .unwrap_or_default();
        if legacy.is_empty()
            && !configured.equals_str("")
            && !configured.equals_str("none")
            && !configured.equals_str("builtin")
            && !configured.equals_str("libreoffice")
        {
            errors
                .push("webui.officePreview.service must be \"builtin\" or \"libreoffice\"".into());
        }
    }
    let binary = field(office, "binaryPath");
    if !binary.is_undefined() && binary.as_string().is_none() {
        errors.push("webui.officePreview.binaryPath must be a string".into());
    }
    errors
}

fn cloned_object(value: &JsValue) -> JsObject {
    value.as_object().cloned().unwrap_or_default()
}

fn lower_ascii(value: &JsString) -> JsString {
    JsString::from(String::from_utf16_lossy(trim(value).units()).to_ascii_lowercase())
}

fn configured_value(runtime: &JsValue, key: &str, fallback: JsValue) -> JsValue {
    match field(runtime, key) {
        JsValue::Undefined => fallback,
        value => value.clone(),
    }
}

fn configured_env(env: &Env, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| env.get(*key).filter(|value| !value.is_empty()).cloned())
}

/// The UI-side normalization used for reads/saves. It adds the runtime and
/// Office defaults without changing unrelated gateway-owned sections.
pub fn normalize(raw: &JsValue, pilot_home: &Path, user_home: &Path) -> JsValue {
    let normalized = cloned_object(raw);
    let source_webui = field(raw, "webui");
    let source_runtime = field(source_webui, "runtime");
    let source_office = field(source_webui, "officePreview");

    let mut runtime = cloned_object(source_runtime);
    for (key, value) in [
        (
            "host",
            configured_value(source_runtime, "host", DEFAULT_HOST.into()),
        ),
        (
            "serverPort",
            configured_value(
                source_runtime,
                "serverPort",
                JsValue::Number(DEFAULT_SERVER_PORT),
            ),
        ),
        (
            "vitePort",
            configured_value(
                source_runtime,
                "vitePort",
                JsValue::Number(DEFAULT_VITE_PORT),
            ),
        ),
        (
            "apiTimeoutMs",
            configured_value(
                source_runtime,
                "apiTimeoutMs",
                JsValue::Number(DEFAULT_API_TIMEOUT_MS),
            ),
        ),
        (
            "databasePath",
            configured_value(
                source_runtime,
                "databasePath",
                JsValue::String(
                    pilot_home
                        .join("auth.db")
                        .to_string_lossy()
                        .into_owned()
                        .into(),
                ),
            ),
        ),
        (
            "workspacesRoot",
            configured_value(
                source_runtime,
                "workspacesRoot",
                JsValue::String(user_home.to_string_lossy().into_owned().into()),
            ),
        ),
    ] {
        runtime.insert(key.into(), value);
    }

    let legacy = field(source_office, "spreadsheetMode")
        .as_string()
        .map(lower_ascii)
        .unwrap_or_default();
    let configured = field(source_office, "service")
        .as_string()
        .map(lower_ascii)
        .unwrap_or_default();
    let service = if !legacy.is_empty() {
        if legacy.equals_str("print") && configured.equals_str("libreoffice") {
            "libreoffice"
        } else {
            "builtin"
        }
    } else if configured.is_empty() || configured.equals_str("none") {
        "builtin"
    } else {
        return normalize_with_office(normalized, source_webui, runtime, configured, source_office);
    };
    normalize_with_office(
        normalized,
        source_webui,
        runtime,
        service.into(),
        source_office,
    )
}

fn normalize_with_office(
    mut normalized: JsObject,
    source_webui: &JsValue,
    runtime: JsObject,
    service: JsString,
    source_office: &JsValue,
) -> JsValue {
    let mut office = JsObject::new();
    office.insert("service".into(), JsValue::String(service));
    office.insert(
        "binaryPath".into(),
        match field(source_office, "binaryPath") {
            JsValue::Undefined => "".into(),
            value => value.clone(),
        },
    );
    for (key, value) in entries(source_office) {
        if !key.equals_str("spreadsheetMode")
            && !key.equals_str("service")
            && !key.equals_str("binaryPath")
        {
            office.insert(key.clone(), value.clone());
        }
    }
    let mut webui = cloned_object(source_webui);
    webui.insert("runtime".into(), JsValue::Object(runtime));
    webui.insert("officePreview".into(), JsValue::Object(office));
    normalized.insert("webui".into(), JsValue::Object(webui));
    JsValue::Object(normalized)
}

fn string_value(value: &JsValue) -> Option<String> {
    value.as_string()?.to_utf8().ok()
}

fn expand_tilde(value: &str, home: &Path) -> String {
    let value = value.trim();
    if value == "~" {
        home.to_string_lossy().into_owned()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest).to_string_lossy().into_owned()
    } else {
        value.to_owned()
    }
}

/// Project the UI bridge's environment. Caller-supplied process values win for
/// SERVER_PORT/VITE_PORT/HOST; customEnv is applied last and may override all.
pub fn runtime_env(raw: &JsValue, pilot_home: &Path, user_home: &Path, process_env: &Env) -> Env {
    let normalized = normalize(raw, pilot_home, user_home);
    let runtime = field(field(&normalized, "webui"), "runtime");
    let runtime_string = |key: &str, fallback: &str| {
        let value = field(runtime, key);
        if nullish(value) {
            fallback.into()
        } else {
            js_string(value)
        }
    };
    let mut env = Env::from([
        (
            "SERVER_PORT".into(),
            configured_env(process_env, &["SERVER_PORT", "PORT"])
                .unwrap_or_else(|| runtime_string("serverPort", "3001")),
        ),
        (
            "VITE_PORT".into(),
            configured_env(process_env, &["VITE_PORT"])
                .unwrap_or_else(|| runtime_string("vitePort", "5173")),
        ),
        (
            "HOST".into(),
            configured_env(process_env, &["HOST"])
                .unwrap_or_else(|| runtime_string("host", DEFAULT_HOST)),
        ),
        (
            "API_TIMEOUT_MS".into(),
            runtime_string("apiTimeoutMs", "120000"),
        ),
    ]);
    for (key, field_name) in [
        ("DATABASE_PATH", "databasePath"),
        ("WORKSPACES_ROOT", "workspacesRoot"),
    ] {
        if let Some(value) = string_value(field(runtime, field_name)).filter(|v| !v.is_empty()) {
            env.insert(key.into(), expand_tilde(&value, user_home));
        }
    }
    if let Some(custom) = field(&normalized, "customEnv").as_object() {
        for (key, value) in custom.entries() {
            if let Some(value) = string_value(value).filter(|value| !value.trim().is_empty())
                && let Ok(key) = key.to_utf8()
            {
                env.insert(key, value);
            }
        }
    }
    env
}

/// `pilotdeckConfig.js:724-726` / `applyConfigToProcessEnv` for native CONFIG.
pub fn merge_runtime_env(
    raw: &JsValue,
    pilot_home: &Path,
    user_home: &Path,
    process_env: &Env,
) -> Env {
    let projection = runtime_env(raw, pilot_home, user_home, process_env);
    let mut merged = process_env.clone();
    for (key, value) in projection {
        merged.insert(key, value);
    }
    merged
}

pub fn user_home_from_options(pilot_home: &Path, process_env: &Env) -> PathBuf {
    process_env
        .get("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| pilot_home.parent().map(PathBuf::from))
        .unwrap_or_else(|| pilot_home.to_path_buf())
}

pub fn office_preview(raw: &JsValue, pilot_home: &Path, user_home: &Path) -> (String, String) {
    let normalized = normalize(raw, pilot_home, user_home);
    let office = field(field(&normalized, "webui"), "officePreview");
    let configured = string_value(field(office, "service"))
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    (
        if configured == "libreoffice" {
            configured
        } else {
            "builtin".into()
        },
        string_value(field(office, "binaryPath"))
            .unwrap_or_default()
            .trim()
            .to_owned(),
    )
}

#[derive(Debug)]
pub(crate) struct FileError {
    pub code: &'static str,
    pub message: String,
}

pub(crate) fn read_file(path: &Path) -> Result<(bool, String, JsValue), FileError> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((false, String::new(), JsValue::Object(JsObject::new())));
        }
        Err(error) => {
            return Err(FileError {
                code: "WEBUI_CONFIG_READ_FAILED",
                message: format!("Unable to read Web UI config: {error}"),
            });
        }
    };
    Ok((true, source.clone(), parse_source(&source)?))
}

pub(crate) fn parse_source(source: &str) -> Result<JsValue, FileError> {
    let document = pilotdeck_yaml::parse_document(
        &source.into(),
        pilotdeck_yaml::Options {
            pretty_errors: false,
            ..Default::default()
        },
    );
    if let Some(error) = document.errors.first() {
        return Err(FileError {
            code: "WEBUI_CONFIG_YAML_INVALID",
            message: format!(
                "Invalid Web UI config YAML: {}",
                String::from_utf16_lossy(error.message.units())
            ),
        });
    }
    let value = document
        .to_json()
        .and_then(|value| value.to_js_value())
        .map_err(|error| FileError {
            code: "WEBUI_CONFIG_YAML_INVALID",
            message: format!(
                "Invalid Web UI config YAML: {}",
                String::from_utf16_lossy(error.message.units())
            ),
        })?;
    let value = match value {
        JsValue::Undefined | JsValue::Null => JsValue::Object(JsObject::new()),
        value => value,
    };
    if value.as_object().is_none() {
        return Err(FileError {
            code: "WEBUI_CONFIG_YAML_INVALID",
            message: "Web UI config YAML root must be an object".into(),
        });
    }
    Ok(value)
}

pub(crate) fn validate_source(
    source: &str,
    pilot_home: &Path,
    cwd: &Path,
    env: &Env,
) -> Result<Vec<JsValue>, super::Error> {
    use crate::metadata::{Files, Metadata, Stat};

    struct CandidateFiles {
        path: PathBuf,
        source: Vec<u8>,
    }
    impl Files for CandidateFiles {
        fn entries(&self, _: &Path) -> io::Result<Vec<(String, bool)>> {
            Err(io::ErrorKind::NotFound.into())
        }
        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            (path == self.path)
                .then(|| self.source.clone())
                .ok_or_else(|| io::ErrorKind::NotFound.into())
        }
        fn stat(&self, path: &Path) -> io::Result<Stat> {
            if path != self.path {
                return Err(io::ErrorKind::NotFound.into());
            }
            Ok(Stat {
                directory: false,
                size: self.source.len() as u64,
                birth_ms: 0.0,
                ctime_ms: 0.0,
                mtime_ms: 0.0,
            })
        }
    }

    let candidate = pilot_home.join(".webui-validation.yaml");
    let mut candidate_env = env.clone();
    candidate_env.insert(
        "PILOTDECK_CONFIG_PATH".into(),
        candidate.to_string_lossy().into_owned(),
    );
    let metadata = Metadata {
        home: pilot_home.to_path_buf(),
        cwd: super::absolute_cwd(cwd),
        files: CandidateFiles {
            path: candidate,
            source: source.as_bytes().to_vec(),
        },
    };
    super::load_project_with_files(&metadata, &candidate_env, None).map(|config| config.diagnostics)
}

pub(crate) fn commit_file(
    path: &Path,
    source: &str,
    pilot_home: &Path,
    cwd: &Path,
    env: &Env,
) -> Result<(), FileError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| FileError {
            code: "WEBUI_CONFIG_WRITE_FAILED",
            message: format!("Unable to create Web UI config directory: {error}"),
        })?;
    }
    let candidate = sibling_candidate(path);
    let outcome = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)?;
        if let Ok(metadata) = fs::metadata(path) {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(source.as_bytes())?;
        file.sync_all()?;
        Ok::<_, std::io::Error>(())
    })();
    if let Err(error) = outcome {
        let _ = fs::remove_file(&candidate);
        return Err(FileError {
            code: "WEBUI_CONFIG_WRITE_FAILED",
            message: format!("Unable to write Web UI config: {error}"),
        });
    }

    let mut candidate_env = env.clone();
    candidate_env.insert(
        "PILOTDECK_CONFIG_PATH".into(),
        candidate.to_string_lossy().into_owned(),
    );
    if let Err(error) = super::load_project(
        pilot_home,
        cwd,
        &candidate_env,
        Some(&cwd.to_string_lossy()),
    ) {
        let _ = fs::remove_file(&candidate);
        return Err(FileError {
            code: "WEBUI_CONFIG_INVALID",
            message: format!(
                "Web UI config failed PilotDeck validation: {}",
                String::from_utf16_lossy(error.message.units())
            ),
        });
    }
    let result = fs::rename(&candidate, path).map_err(|error| FileError {
        code: "WEBUI_CONFIG_WRITE_FAILED",
        message: format!("Unable to replace Web UI config: {error}"),
    });
    let _ = fs::remove_file(candidate);
    result
}

fn sibling_candidate(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("pilotdeck.yaml");
    path.with_file_name(format!(".{name}.webui-{}.tmp", uuid::Uuid::new_v4()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::object;
    use pilotdeck_protocol::parse_json;

    fn json(source: &str) -> JsValue {
        parse_json(source).unwrap()
    }

    #[test]
    fn defaults_and_office_migration_match_the_ui_service() {
        let normalized = normalize(
            &json(
                r#"{"webui":{"officePreview":{"service":" LibreOffice ","spreadsheetMode":"print","binaryPath":" /soffice "}}}"#,
            ),
            Path::new("/pilot"),
            Path::new("/user"),
        );
        let runtime = field(field(&normalized, "webui"), "runtime");
        assert!(
            field(runtime, "host")
                .as_string()
                .unwrap()
                .equals_str("0.0.0.0")
        );
        assert_eq!(field(runtime, "serverPort").as_number(), Some(3001.0));
        assert!(
            field(runtime, "databasePath")
                .as_string()
                .unwrap()
                .equals_str("/pilot/auth.db")
        );
        assert_eq!(
            office_preview(&normalized, Path::new("/pilot"), Path::new("/user")),
            ("libreoffice".into(), "/soffice".into())
        );
        assert!(
            field(
                field(field(&normalized, "webui"), "officePreview"),
                "spreadsheetMode"
            )
            .is_undefined()
        );

        let builtin = normalize(
            &json(
                r#"{"webui":{"officePreview":{"service":"libreoffice","spreadsheetMode":"auto"}}}"#,
            ),
            Path::new("/pilot"),
            Path::new("/user"),
        );
        assert!(
            field(field(field(&builtin, "webui"), "officePreview"), "service")
                .as_string()
                .unwrap()
                .equals_str("builtin")
        );
    }

    #[test]
    fn custom_env_is_string_only_nonempty_and_wins_last() {
        let raw = object([
            (
                "webui",
                object([(
                    "runtime",
                    object([
                        ("serverPort", JsValue::Number(4000.0)),
                        ("databasePath", "~/db.sqlite".into()),
                    ]),
                )]),
            ),
            (
                "customEnv",
                object([
                    ("SERVER_PORT", "9000".into()),
                    ("EMPTY", "   ".into()),
                    ("NUMBER", JsValue::Number(5.0)),
                ]),
            ),
        ]);
        let process = Env::from([
            ("SERVER_PORT".into(), "".into()),
            ("PORT".into(), "8000".into()),
            ("HOST".into(), "".into()),
        ]);
        let env = runtime_env(&raw, Path::new("/pilot"), Path::new("/user"), &process);
        assert_eq!(env.get("SERVER_PORT").map(String::as_str), Some("9000"));
        assert_eq!(
            env.get("DATABASE_PATH").map(String::as_str),
            Some("/user/db.sqlite")
        );
        assert_eq!(env.get("HOST").map(String::as_str), Some("0.0.0.0"));
        assert!(!env.contains_key("EMPTY"));
        assert!(!env.contains_key("NUMBER"));
    }

    #[test]
    fn explicit_null_runtime_values_survive_normalization_and_use_env_fallbacks() {
        let raw = json(
            r#"{"webui":{"runtime":{"host":null,"serverPort":null,"vitePort":null,"apiTimeoutMs":null,"databasePath":null,"workspacesRoot":null}}}"#,
        );
        let normalized = normalize(&raw, Path::new("/pilot"), Path::new("/user"));
        let runtime = field(field(&normalized, "webui"), "runtime");
        for key in [
            "host",
            "serverPort",
            "vitePort",
            "apiTimeoutMs",
            "databasePath",
            "workspacesRoot",
        ] {
            assert!(matches!(field(runtime, key), JsValue::Null), "{key}");
        }
        let env = runtime_env(&raw, Path::new("/pilot"), Path::new("/user"), &Env::new());
        assert_eq!(env.get("HOST").map(String::as_str), Some("0.0.0.0"));
        assert_eq!(env.get("SERVER_PORT").map(String::as_str), Some("3001"));
        assert_eq!(env.get("VITE_PORT").map(String::as_str), Some("5173"));
        assert_eq!(
            env.get("API_TIMEOUT_MS").map(String::as_str),
            Some("120000")
        );
        assert!(!env.contains_key("DATABASE_PATH"));
        assert!(!env.contains_key("WORKSPACES_ROOT"));
    }

    #[test]
    fn office_validation_stays_on_the_ui_boundary() {
        assert_eq!(
            validation_errors(&json(
                r#"{"webui":{"officePreview":{"service":"mistyped","binaryPath":5}}}"#
            )),
            vec![
                "webui.officePreview.service must be \"builtin\" or \"libreoffice\"",
                "webui.officePreview.binaryPath must be a string",
            ]
        );
        assert!(
            validation_errors(&json(
                r#"{"webui":{"officePreview":{"service":"LIBREOFFICE","binaryPath":"/soffice"}}}"#
            ))
            .is_empty()
        );
        assert!(
            validation_errors(&json(
                r#"{"webui":{"officePreview":{"service":"none","binaryPath":""}}}"#
            ))
            .is_empty()
        );

        let normalized = normalize(
            &json(r#"{"webui":{"officePreview":{"binaryPath":5}}}"#),
            Path::new("/pilot"),
            Path::new("/user"),
        );
        assert_eq!(validation_errors(&normalized).len(), 1);
        let normalized = normalize(
            &json(r#"{"webui":{"officePreview":{"binaryPath":null}}}"#),
            Path::new("/pilot"),
            Path::new("/user"),
        );
        assert!(matches!(
            field(
                field(field(&normalized, "webui"), "officePreview"),
                "binaryPath"
            ),
            JsValue::Null
        ));
        assert_eq!(
            validation_errors(&normalized),
            ["webui.officePreview.binaryPath must be a string"]
        );
    }

    #[test]
    fn reload_projection_updates_gateway_runtime_env() {
        let raw = object([(
            "webui",
            object([(
                "runtime",
                object([("host", JsValue::String("10.0.0.2".into()))]),
            )]),
        )]);
        let mut base = Env::new();
        base.insert("PILOTDECK_CONFIG_PATH".into(), "/cfg/pilotdeck.yaml".into());
        base.insert("HOME".into(), "/user".into());
        let env = merge_runtime_env(&raw, Path::new("/pilot"), Path::new("/user"), &base);
        assert_eq!(env.get("HOST").map(String::as_str), Some("10.0.0.2"));
        assert_eq!(
            env.get("PILOTDECK_CONFIG_PATH").map(String::as_str),
            Some("/cfg/pilotdeck.yaml")
        );
        assert_eq!(env.get("HOME").map(String::as_str), Some("/user"));
    }

    #[test]
    fn reload_config_applies_object_assign_semantics_via_routes() {
        use crate::config::{Env, routes::native::install, routes::ConfigRoutes};
        let scratch = crate::scratch::Scratch::new("webui-reload-env");
        let home = scratch.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        const MINIMAL_CONFIG: &str = "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: mock-only\n      models: {deterministic: {}}\n";
        std::fs::write(home.join("pilotdeck.yaml"), MINIMAL_CONFIG).unwrap();
        let routes = ConfigRoutes::new(home.clone(), scratch.path().to_path_buf(), Env::new()).unwrap();
        let state = install(routes);
        let mut routes = state.lock().unwrap();
        routes.env.insert("PILOTDECK_CONFIG_PATH".into(), "/keep/cfg.yaml".into());
        routes.env.insert("HOME".into(), "/keep/home".into());
        let snapshot = pilotdeck_protocol::parse_json(
            r#"{"config":{"webui":{"runtime":{"host":"192.168.1.9"}}}}"#,
        )
        .unwrap();
        routes.sync_runtime_env_from_snapshot(&snapshot);
        assert_eq!(routes.env.get("HOST").map(String::as_str), Some("192.168.1.9"));
        assert_eq!(
            routes.env.get("PILOTDECK_CONFIG_PATH").map(String::as_str),
            Some("/keep/cfg.yaml")
        );
        assert_eq!(routes.env.get("HOME").map(String::as_str), Some("/keep/home"));
    }
}

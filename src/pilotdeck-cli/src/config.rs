//! loadPilotConfig (src/pilot/config/loadPilotConfig.ts:63–128) up to its first
//! throwConfigErrorIfFatal. Section parsing after that point is not ported here.
use crate::Failure;
use crate::paths::{self, Env};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use pilotdeck_yaml::{Options, parse_document};
use std::path::{Path, PathBuf};

#[derive(Default)]
struct Diagnostics {
    first_fatal: Option<String>,
}

impl Diagnostics {
    fn fatal(&mut self, message: String) {
        self.first_fatal.get_or_insert(message);
    }
}

fn read_yaml_source(
    path: &Path,
    kind: &str,
    diagnostics: &mut Diagnostics,
) -> Result<Option<JsObject>, Failure> {
    if !path.exists() {
        return Ok(None);
    }
    let Ok(bytes) = std::fs::read(path) else {
        diagnostics.fatal(format!("Failed to read {kind} config."));
        return Ok(None);
    };
    let source = JsString::from(&*String::from_utf8_lossy(&bytes));
    let document = parse_document(
        &source,
        Options {
            pretty_errors: false,
            ..Options::default()
        },
    );
    let graph = match document.to_json() {
        Ok(graph) if document.errors.is_empty() => graph,
        _ => {
            diagnostics.fatal(format!("Failed to parse {kind} config YAML."));
            return Ok(None);
        }
    };
    let value = graph
        .to_js_value()
        .map_err(|_| Failure::Todo("config YAML values outside plain JSON (Map/Set/cycles)"))?;
    if has_prototype_key(&value) {
        return Err(Failure::Todo("config keys named __proto__"));
    }
    // JsValue implements Drop, so the object cannot be moved out of it.
    match &value {
        JsValue::Null | JsValue::Undefined => Ok(Some(JsObject::new())),
        JsValue::Object(object) => Ok(Some(object.clone())),
        _ => {
            diagnostics.fatal(format!("{kind} config root must be an object."));
            Ok(None)
        }
    }
}

/// `{...base}` and `output[key] = value` give `__proto__` prototype semantics.
fn has_prototype_key(value: &JsValue) -> bool {
    match value {
        JsValue::Object(object) => {
            object.prototype().is_some()
                || object
                    .entries()
                    .iter()
                    .any(|(key, child)| key.equals_str("__proto__") || has_prototype_key(child))
        }
        JsValue::Array(items) => items.iter().any(has_prototype_key),
        _ => false,
    }
}

/// deepMerge from src/pilot/config/merge.ts.
fn deep_merge(base: &JsObject, overrides: &JsObject) -> JsObject {
    let mut output = base.clone();
    for (key, value) in overrides.entries() {
        let merged = match (output.get_key(key), value) {
            (Some(JsValue::Object(previous)), JsValue::Object(next)) => {
                JsValue::Object(deep_merge(previous, next))
            }
            _ => value.clone(),
        };
        output.insert(key.clone(), merged);
    }
    output
}

/// String(value) for values produced by YAML toJSON.
fn js_to_string(value: &JsValue) -> String {
    match value {
        JsValue::Undefined => "undefined".into(),
        JsValue::Null => "null".into(),
        JsValue::Bool(value) => value.to_string(),
        JsValue::Number(number) if number.is_nan() => "NaN".into(),
        JsValue::Number(number) if number.is_infinite() => if *number > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        }
        .into(),
        JsValue::Number(number) if *number == 0.0 => "0".into(),
        JsValue::Number(number) => ryu_js::Buffer::new().format(*number).to_owned(),
        JsValue::String(text) => String::from_utf16_lossy(text.units()),
        JsValue::Array(items) => items
            .iter()
            .map(|item| match item {
                JsValue::Null | JsValue::Undefined => String::new(),
                other => js_to_string(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        JsValue::Object(_) => "[object Object]".into(),
    }
}

/// Ok(()) means the config passed the first fatal check.
pub fn check(env: &Env, cwd: &Path, pilot_home: &Path) -> Result<(), Failure> {
    let mut diagnostics = Diagnostics::default();
    // resolvePilotConfigPath (loadPilotConfig.ts:57-61): the trimmed override is used
    // as-is, so "" reaches existsSync("") (false) instead of path.resolve.
    let default_path = match env.string("PILOTDECK_CONFIG_PATH") {
        Some(value) => PathBuf::from(paths::js_trim(&value)),
        None => pilot_home.join("pilotdeck.yaml"),
    };
    let project_root = paths::resolve(cwd, Path::new("."));
    let mut sources = vec![
        read_yaml_source(&default_path, "default", &mut diagnostics)?,
        read_yaml_source(
            &project_root.join(".pilotdeck").join("pilotdeck.yaml"),
            "project",
            &mut diagnostics,
        )?,
    ];
    if let Some(model) = env.string("PILOT_AGENT_MODEL").filter(|v| !v.is_empty()) {
        let mut agent = JsObject::new();
        agent.insert("model".into(), JsValue::String(model.as_str().into()));
        let mut overrides = JsObject::new();
        overrides.insert("agent".into(), JsValue::Object(agent));
        sources.push(Some(overrides));
    }
    let raw = sources
        .iter()
        .flatten()
        .fold(JsObject::new(), |output, source| {
            deep_merge(&output, source)
        });
    // TS runs applyProjectBinding on the merged config (loadPilotConfig.ts:105) before
    // validateTopLevel (:106) and the first throwConfigErrorIfFatal (:128), and it can
    // throw on its own (projectBinding.ts:49,51), so the TODO stays at that point.
    if env.get("STAFFDECK_PROJECT_BINDING_ENABLED") == Some(std::ffi::OsStr::new("true")) {
        return Err(Failure::Todo(
            "StaffDeck project binding (applyProjectBinding)",
        ));
    }

    if raw.contains_key("pilot") {
        diagnostics.fatal(
            "YAML config must not contain a pilot section. Use PILOT_HOME for PilotHome.".into(),
        );
    }
    match raw.get("schemaVersion") {
        None | Some(JsValue::Undefined) => {}
        Some(JsValue::Number(version)) if *version == 1.0 => {}
        Some(other) => {
            diagnostics.fatal(format!(
                "Unsupported schemaVersion {}.",
                js_to_string(other)
            ));
        }
    }
    for (name, article) in [("agent", "an"), ("model", "a")] {
        if matches!(raw.get(name), None | Some(JsValue::Undefined)) {
            diagnostics.fatal(format!("Config must contain {article} {name} section."));
        }
    }
    match diagnostics.first_fatal {
        Some(message) => Err(Failure::Error(message)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn message(result: Result<(), Failure>) -> String {
        match result {
            Ok(()) => "ok".into(),
            Err(Failure::Error(message)) => message,
            Err(Failure::Todo(what)) => format!("TODO {what}"),
        }
    }

    fn fixture(name: &str, yaml: &str) -> (PathBuf, Env) {
        let dir =
            std::env::temp_dir().join(format!("pdrs-cli-config-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("project")).unwrap();
        fs::write(dir.join("pilotdeck.yaml"), yaml).unwrap();
        let path = dir.join("pilotdeck.yaml").to_string_lossy().into_owned();
        (
            dir,
            Env::from_pairs(&[("PILOTDECK_CONFIG_PATH", path.as_str())]),
        )
    }

    fn run(name: &str, yaml: &str, extra: Option<(&str, &str)>) -> String {
        let (dir, mut env) = fixture(name, yaml);
        if let Some((key, value)) = extra {
            let path = dir.join("pilotdeck.yaml").to_string_lossy().into_owned();
            env = Env::from_pairs(&[("PILOTDECK_CONFIG_PATH", path.as_str()), (key, value)]);
        }
        let result = message(check(&env, &dir.join("project"), &dir));
        fs::remove_dir_all(dir).unwrap();
        result
    }

    #[test]
    fn first_fatal_follows_ts_order() {
        assert_eq!(
            run("empty", "schemaVersion: 1\n", None),
            "Config must contain an agent section."
        );
        assert_eq!(
            run("model", "schemaVersion: 1\nagent: {}\n", None),
            "Config must contain a model section."
        );
        assert_eq!(
            run(
                "env-agent",
                "schemaVersion: 1\n",
                Some(("PILOT_AGENT_MODEL", "p/m"))
            ),
            "Config must contain a model section."
        );
        assert_eq!(
            run("version", "schemaVersion: '2'\n", None),
            "Unsupported schemaVersion 2."
        );
        assert_eq!(
            run("pilot", "pilot: 1\nschemaVersion: 3.5\n", None),
            "YAML config must not contain a pilot section. Use PILOT_HOME for PilotHome."
        );
        assert_eq!(
            run("root", "- 1\n", None),
            "default config root must be an object."
        );
        assert_eq!(
            run("yaml", "a: [\n", None),
            "Failed to parse default config YAML."
        );
        assert_eq!(
            run("valid", "schemaVersion: 1\nagent: {}\nmodel: {}\n", None),
            "ok"
        );
    }

    #[test]
    fn project_binding_runs_before_the_first_fatal_like_ts() {
        let binding = Some(("STAFFDECK_PROJECT_BINDING_ENABLED", "true"));
        for (name, yaml) in [
            ("binding-agent", "schemaVersion: 1\n"),
            ("binding-yaml", "a: [\n"),
        ] {
            assert_eq!(
                run(name, yaml, binding),
                "TODO StaffDeck project binding (applyProjectBinding)"
            );
        }
        assert_eq!(
            run(
                "binding-off",
                "schemaVersion: 1\n",
                Some(("STAFFDECK_PROJECT_BINDING_ENABLED", "1"))
            ),
            "Config must contain an agent section."
        );
    }

    #[test]
    fn missing_config_file_is_not_a_read_error() {
        // An empty or blank override stays "" (not nullish, not resolved to cwd), and
        // existsSync("") is false: no config source at all.
        // Values as recorded by probes/p1-config-path-empty (Node v22.22.0).
        for value in ["", "  ", "\t", "\u{feff}", "\u{3000} ", "\u{feff}\t"] {
            let env = Env::from_pairs(&[("PILOTDECK_CONFIG_PATH", value)]);
            assert_eq!(
                message(check(
                    &env,
                    Path::new("/nonexistent-pdrs"),
                    Path::new("/nonexistent-pdrs")
                )),
                "Config must contain an agent section."
            );
        }
    }

    #[test]
    fn deep_merge_replaces_non_records_and_merges_records() {
        let mut inner = JsObject::new();
        inner.insert("a".into(), JsValue::Number(1.0));
        let mut base = JsObject::new();
        base.insert("x".into(), JsValue::Object(inner));
        let mut inner2 = JsObject::new();
        inner2.insert("b".into(), JsValue::Number(2.0));
        let mut over = JsObject::new();
        over.insert("x".into(), JsValue::Object(inner2));
        let merged = deep_merge(&base, &over);
        let x = merged.get("x").and_then(JsValue::as_object).unwrap();
        assert_eq!(x.len(), 2);
    }

    #[test]
    fn string_conversion_matches_js() {
        assert_eq!(js_to_string(&JsValue::Number(1e21)), "1e+21");
        assert_eq!(js_to_string(&JsValue::Number(2.5)), "2.5");
        assert_eq!(js_to_string(&JsValue::Number(-0.0)), "0");
        assert_eq!(
            js_to_string(&JsValue::Array(vec![
                JsValue::Number(1.0),
                JsValue::Null,
                "a".into()
            ])),
            "1,,a"
        );
    }
}

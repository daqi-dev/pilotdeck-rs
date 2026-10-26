#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::config::{
    Env,
    settings::resolved_settings,
    snapshot::content_hash,
    store::{ConfigStore, StoreOptions},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

const ORACLE: &str = include_str!("fixtures/settings/oracle.json");
// The fixture records every wall-clock `Date` as this fixed instant; the store
// is created with a clock returning exactly it.
const FIXED_LOADED_AT: &str = "1970-01-01T00:00:00.000Z";

#[test]
fn resolve_settings_replays_real_typescript_oracle() {
    // The shared guard parses with the protocol parser and, when PDRS_BASELINE
    // is set, recomputes every recorded source hash against the fixed commit.
    let oracle = baseline::oracle(ORACLE);
    let JsValue::Array(cases) = oracle.get("cases").expect("cases") else {
        panic!("cases array")
    };
    assert_eq!(
        oracle.get("count").and_then(JsValue::as_number),
        Some(cases.len() as f64)
    );
    assert_eq!(cases.len(), 12, "fixture count changed");
    let sources = oracle
        .get("sources")
        .and_then(JsValue::as_object)
        .expect("sources");
    assert_eq!(sources.len(), 6, "source hash set changed");
    for (_, value) in sources.entries() {
        assert!(
            value
                .as_string()
                .and_then(|s| s.to_utf8().ok())
                .is_some_and(|s| s.len() == 64),
            "sourceSha256 must be recorded for every source"
        );
    }

    let root = unique_root();
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let mut failures = vec![];
    for case in cases {
        let id = text(case.get("id")).expect("case id");
        // Each case owns its home/project pair, like the generator's per-case
        // scratch tree; `<ROOT>` in the fixture is this per-case root.
        let case_root = root.join(&id);
        let home = case_root.join("home");
        fs::create_dir_all(&home).expect("home");
        let yaml = text(case.get("yaml")).expect("yaml");
        if case.get("noGlobal").and_then(JsValue::as_bool) != Some(true) {
            fs::write(home.join("pilotdeck.yaml"), yaml).expect("global yaml");
        }
        let project = case
            .get("project")
            .and_then(JsValue::as_string)
            .map(|name| case_root.join(String::from_utf16_lossy(name.units())));
        if let (Some(project), Some(yaml)) = (
            &project,
            case.get("projectYaml").and_then(JsValue::as_string),
        ) {
            fs::create_dir_all(project.join(".pilotdeck")).expect("project dir");
            fs::write(
                project.join(".pilotdeck/pilotdeck.yaml"),
                String::from_utf16_lossy(yaml.units()),
            )
            .expect("project yaml");
        }
        let mut env = Env::new();
        if let Some(values) = case.get("env").and_then(JsValue::as_object) {
            for (key, value) in values.entries() {
                let value = text(Some(value))
                    .expect("env value")
                    .replace("<ROOT>", &case_root.to_string_lossy())
                    .replace("<CWD>", &cwd.to_string_lossy());
                env.insert(String::from_utf16_lossy(key.units()), value);
            }
        }
        env.insert("HOME".into(), home.to_string_lossy().into_owned());
        // The generator's boot config injects PILOT_HOME from pilotHome; the
        // bootstrap source appears only when it is set.
        env.insert("PILOT_HOME".into(), home.to_string_lossy().into_owned());
        let project_path = project.as_ref().map(|p| p.to_string_lossy().into_owned());
        let store = match ConfigStore::new(StoreOptions {
            pilot_home: home,
            cwd: cwd.clone(),
            env,
            project: project_path,
            version: None,
            loaded_at: Arc::new(|| FIXED_LOADED_AT.to_owned()),
        }) {
            Ok(store) => store,
            Err(error) => {
                // The real entry has no response body here: gateway assembly
                // fails while creating the config store, so the fixture records
                // the boot error and the replay compares against it.
                if let Some(expected) = case.get("error") {
                    failures.extend(error_failures(&id, &error, expected));
                } else {
                    failures.push(format!(
                        "{id}: unexpected error {} {}",
                        error.code,
                        String::from_utf16_lossy(error.message.units())
                    ));
                }
                continue;
            }
        };
        let Some(expected) = case.get("response") else {
            failures.push(format!(
                "{id}: store loaded but the fixture records an error"
            ));
            continue;
        };
        let response = store.resolved_settings();
        assert_eq!(
            response
                .get("contentHash")
                .and_then(|v| v.as_string())
                .and_then(|s| s.to_utf8().ok()),
            Some(content_hash(response.get("config").expect("config"))),
            "{id}: production contentHash is not self-consistent",
        );
        let mut normalized = normalize(&response, &case_root, &cwd);
        let hash = content_hash(normalized.get("config").expect("normalized config"));
        if let JsValue::Object(object) = &mut normalized {
            object.insert("contentHash".into(), hash.as_str().into());
        }
        let expected = decode_tags(expected);
        let actual_hash = normalized.get("contentHash").cloned();
        let expected_hash = expected.get("contentHash").cloned();
        if !js_eq(&normalized, &expected) {
            failures.push(format!(
                "{id}: {}\nactual   {}",
                first_diff(&normalized, &expected, "$").unwrap_or_else(|| "value differs".into()),
                stringify_json(&normalized).unwrap().unwrap_or_default()
            ));
        }
        if actual_hash != expected_hash {
            failures.push(format!(
                "{id}: contentHash {:?} != {:?}",
                actual_hash.and_then(|v| v.as_string().and_then(|s| s.to_utf8().ok())),
                expected_hash.and_then(|v| v.as_string().and_then(|s| s.to_utf8().ok()))
            ));
        }
    }
    let _ = fs::remove_dir_all(&root);
    assert!(
        failures.is_empty(),
        "{} response differences; first entries:\n{}",
        failures.len(),
        failures.into_iter().take(3).collect::<Vec<_>>().join("\n")
    );
}

fn error_failures(
    id: &str,
    error: &pilotdeck_gateway::config::Error,
    expected: &JsValue,
) -> Vec<String> {
    let mut failures = vec![];
    let expect = |key: &str| {
        expected
            .get(key)
            .and_then(JsValue::as_string)
            .and_then(|s| s.to_utf8().ok())
    };
    if error.name != expect("name").as_deref().unwrap_or_default() {
        failures.push(format!(
            "{id}: error.name {} != {}",
            error.name,
            expect("name").unwrap_or_default()
        ));
    }
    if error.code != expect("code").unwrap_or_default() {
        failures.push(format!(
            "{id}: error.code {} != {}",
            error.code,
            expect("code").unwrap_or_default()
        ));
    }
    let message = String::from_utf16_lossy(error.message.units());
    if message != expect("message").unwrap_or_default() {
        failures.push(format!(
            "{id}: error.message {message:?} != {:?}",
            expect("message")
        ));
    }
    let first = error.diagnostics.first().and_then(JsValue::as_object);
    // The fixture records the first diagnostic's code as `diagnosticCode`
    // (distinct from the error's own `code`) and its `hint` when present.
    for (actual_key, expected_key) in [("code", "diagnosticCode"), ("hint", "hint")] {
        let actual = first
            .and_then(|d| d.get(actual_key))
            .and_then(JsValue::as_string)
            .and_then(|s| s.to_utf8().ok());
        if actual != expect(expected_key) {
            failures.push(format!(
                "{id}: diagnostic.{actual_key} {actual:?} != {:?}",
                expect(expected_key)
            ));
        }
    }
    failures
}

#[test]
fn recoverable_follows_presence_not_truthiness() {
    // The baseline spreads `recoverable` on `!== undefined`, unlike every other
    // optional field: `false` must survive, an absent key must stay absent.
    let response = resolved_settings(&snapshot(
        JsValue::Array(vec![
            diagnostic(&[("recoverable", bool(false))]),
            diagnostic(&[]),
            diagnostic(&[("recoverable", bool(true))]),
        ]),
        JsValue::Array(vec![]),
    ));
    let diagnostics = items(response.get("diagnostics"));
    assert_eq!(diagnostics.len(), 3);
    let objects: Vec<&JsObject> = diagnostics
        .iter()
        .map(|d| d.as_object().expect("diagnostic object"))
        .collect();
    assert_eq!(
        objects[0].get("recoverable"),
        Some(&bool(false)),
        "recoverable: false must be kept"
    );
    assert!(
        !objects[1].contains_key("recoverable"),
        "an absent recoverable must stay absent"
    );
    assert_eq!(objects[2].get("recoverable"), Some(&bool(true)));
}

#[test]
fn optional_fields_follow_truthiness() {
    // `...(value ? {...} : {})`: empty strings and other falsy values are
    // dropped, everything else is copied verbatim.
    let response = resolved_settings(&snapshot(
        JsValue::Array(vec![
            diagnostic(&[
                ("path", text_value("")),
                ("hint", text_value("")),
                ("redactedValue", text_value("abc")),
            ]),
            diagnostic(&[
                ("path", text_value("model")),
                ("hint", text_value("correct it")),
            ]),
        ]),
        JsValue::Array(vec![source(&[
            ("path", text_value("")),
            ("contentHash", text_value("h")),
            ("phase", text_value("merge")),
        ])]),
    ));
    let diagnostics = items(response.get("diagnostics"));
    let first = diagnostics[0].as_object().expect("first diagnostic");
    assert!(!first.contains_key("path"), "an empty path is dropped");
    assert!(!first.contains_key("hint"), "an empty hint is dropped");
    assert_eq!(first.get("redactedValue"), Some(&text_value("abc")));
    let second = diagnostics[1].as_object().expect("second diagnostic");
    assert_eq!(second.get("path"), Some(&text_value("model")));
    assert_eq!(second.get("hint"), Some(&text_value("correct it")));
    let sources = items(response.get("sources"));
    let projected_source = sources[0].as_object().expect("projected source");
    assert!(
        !projected_source.contains_key("path"),
        "an empty source path is dropped"
    );
    assert_eq!(projected_source.get("contentHash"), Some(&text_value("h")));
    assert_eq!(projected_source.get("phase"), Some(&text_value("merge")));
}

#[test]
fn response_and_element_key_orders_match_the_baseline() {
    let response = resolved_settings(&snapshot(
        JsValue::Array(vec![diagnostic(&[
            ("path", text_value("p")),
            ("source", text_value("s")),
            ("hint", text_value("h")),
            ("redactedValue", text_value("r")),
            ("recoverable", bool(true)),
        ])]),
        JsValue::Array(vec![source(&[
            ("path", text_value("p")),
            ("contentHash", text_value("c")),
            ("phase", text_value("merge")),
        ])]),
    ));
    let keys = |value: &JsValue| {
        value
            .as_object()
            .expect("object")
            .entries()
            .iter()
            .map(|(k, _)| String::from_utf16_lossy(k.units()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        keys(&response),
        [
            "schemaVersion",
            "version",
            "loadedAt",
            "contentHash",
            "config",
            "sources",
            "diagnostics"
        ]
    );
    let sources = items(response.get("sources"));
    assert_eq!(
        keys(&sources[0]),
        [
            "kind",
            "priority",
            "loadedAt",
            "path",
            "contentHash",
            "phase"
        ]
    );
    let diagnostics = items(response.get("diagnostics"));
    assert_eq!(
        keys(&diagnostics[0]),
        [
            "code",
            "severity",
            "message",
            "path",
            "source",
            "hint",
            "redactedValue",
            "recoverable"
        ]
    );
}

#[test]
fn scalar_fields_pass_through_verbatim() {
    // `schemaVersion`/`version` are copied from the snapshot without
    // normalization, so a non-default version survives the projection.
    let response = resolved_settings(&snapshot_with(
        JsValue::Array(vec![]),
        JsValue::Array(vec![]),
        Some(JsValue::Number(7.0)),
    ));
    assert_eq!(response.get("version"), Some(&JsValue::Number(7.0)));
    assert_eq!(
        response.get("schemaVersion"),
        Some(&text_value("1")),
        "schemaVersion is copied verbatim, not coerced"
    );
    assert_eq!(
        response.get("loadedAt"),
        Some(&text_value("1970-01-01T00:00:00.000Z"))
    );
    assert_eq!(response.get("contentHash"), Some(&text_value("hash")));
}

#[test]
fn config_is_projected_through_the_shared_redaction() {
    // The projection reuses the snapshot's own redactConfig, so secret-shaped
    // keys are redacted in the response while neighbours survive verbatim.
    let mut provider = JsObject::new();
    provider.insert("apiKey".into(), text_value("secret"));
    provider.insert("url".into(), text_value("http://127.0.0.1:1/v1"));
    let mut snapshot = snapshot_with(JsValue::Array(vec![]), JsValue::Array(vec![]), None);
    let config = object(&[("model", JsValue::Object(provider))]);
    if let JsValue::Object(object) = &mut snapshot {
        object.insert("config".into(), config);
    }
    let response = resolved_settings(&snapshot);
    let projected = response
        .get("config")
        .and_then(|c| c.get("model"))
        .and_then(|m| m.as_object())
        .expect("projected config model");
    assert_eq!(projected.get("apiKey"), Some(&text_value("<redacted>")));
    assert_eq!(
        projected.get("url"),
        Some(&text_value("http://127.0.0.1:1/v1"))
    );
}

#[test]
#[ignore = "requires Node 22 and PDRS_BASELINE"]
fn settings_fixtures_regenerate_identically_from_ts() {
    baseline::regenerates_identically("settings");
}

/// A synthetic snapshot with the given diagnostics/sources, matching the
/// baseline's own snapshot shape.
fn snapshot(diagnostics: JsValue, sources: JsValue) -> JsValue {
    snapshot_with(diagnostics, sources, None)
}

fn snapshot_with(diagnostics: JsValue, sources: JsValue, version: Option<JsValue>) -> JsValue {
    let mut out = JsObject::new();
    out.insert("schemaVersion".into(), text_value("1"));
    out.insert("version".into(), version.unwrap_or(JsValue::Number(1.0)));
    out.insert("loadedAt".into(), text_value(FIXED_LOADED_AT));
    out.insert("contentHash".into(), text_value("hash"));
    out.insert("config".into(), object(&[]));
    out.insert("sources".into(), sources);
    out.insert("diagnostics".into(), diagnostics);
    JsValue::Object(out)
}

fn diagnostic(extra: &[(&str, JsValue)]) -> JsValue {
    let mut out = JsObject::new();
    out.insert("code".into(), text_value("CONFIG_TEST"));
    out.insert("severity".into(), text_value("warning"));
    out.insert("message".into(), text_value("a message"));
    for (key, value) in extra {
        out.insert((*key).into(), value.clone());
    }
    JsValue::Object(out)
}

fn source(extra: &[(&str, JsValue)]) -> JsValue {
    let mut out = JsObject::new();
    out.insert("kind".into(), text_value("env"));
    out.insert("priority".into(), JsValue::Number(1.0));
    out.insert("loadedAt".into(), text_value(FIXED_LOADED_AT));
    for (key, value) in extra {
        out.insert((*key).into(), value.clone());
    }
    JsValue::Object(out)
}

fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in entries {
        out.insert((*key).into(), value.clone());
    }
    JsValue::Object(out)
}

fn items(value: Option<&JsValue>) -> &[JsValue] {
    match value {
        Some(JsValue::Array(items)) => items,
        _ => &[],
    }
}

fn text_value(value: &str) -> JsValue {
    JsValue::String(JsString::from(value))
}

fn bool(value: bool) -> JsValue {
    JsValue::Bool(value)
}

/// Atomically create a root this process owns. Never delete a path before
/// owning it: a recycled PID must not expose another run's leftovers.
fn unique_root() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("pdrs-settings-{}-{n}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return path,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("cannot create settings temp root: {error}"),
        }
    }
}

fn text(value: Option<&JsValue>) -> Option<String> {
    value?.as_string()?.to_utf8().ok()
}

fn replace_units(value: &JsString, needle: &[u16], replacement: &[u16]) -> JsString {
    if needle.is_empty() {
        return value.clone();
    }
    let units = value.units();
    let mut out = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        if units[index..].starts_with(needle) {
            out.extend_from_slice(replacement);
            index += needle.len();
        } else {
            out.push(units[index]);
            index += 1;
        }
    }
    JsString::from_utf16(out)
}

fn normalize(value: &JsValue, root: &Path, cwd: &Path) -> JsValue {
    // Operate on UTF-16 units so lone surrogates in real values survive the
    // temporary-path substitution.
    let root: Vec<u16> = root.to_string_lossy().encode_utf16().collect();
    let cwd: Vec<u16> = cwd.to_string_lossy().encode_utf16().collect();
    fn walk(
        value: &JsValue,
        root: &[u16],
        cwd: &[u16],
        root_replacement: &[u16],
        cwd_replacement: &[u16],
    ) -> JsValue {
        match value {
            JsValue::String(s) => {
                let text = replace_units(s, root, root_replacement);
                let text = replace_units(&text, cwd, cwd_replacement);
                JsValue::String(text)
            }
            JsValue::Array(items) => JsValue::Array(
                items
                    .iter()
                    .map(|v| walk(v, root, cwd, root_replacement, cwd_replacement))
                    .collect(),
            ),
            JsValue::Object(object) => {
                let mut out = JsObject::new();
                for (key, value) in object.entries() {
                    let key = replace_units(key, root, root_replacement);
                    let key = replace_units(&key, cwd, cwd_replacement);
                    out.insert(
                        key,
                        walk(value, root, cwd, root_replacement, cwd_replacement),
                    );
                }
                JsValue::Object(out)
            }
            other => other.clone(),
        }
    }
    walk(
        value,
        &root,
        &cwd,
        &"<ROOT>".encode_utf16().collect::<Vec<_>>(),
        &"<CWD>".encode_utf16().collect::<Vec<_>>(),
    )
}

fn decode_tags(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(decode_tags).collect()),
        JsValue::Object(object) => {
            if let Some(tag) = object
                .get("$")
                .and_then(JsValue::as_string)
                .and_then(|s| s.to_utf8().ok())
            {
                return match tag.as_str() {
                    "undefined" => JsValue::Undefined,
                    "NaN" => JsValue::Number(f64::NAN),
                    "Infinity" => JsValue::Number(f64::INFINITY),
                    "-Infinity" => JsValue::Number(f64::NEG_INFINITY),
                    "-0" => JsValue::Number(-0.0),
                    other => panic!("unknown fixture tag {other}"),
                };
            }
            let mut out = JsObject::new();
            for (key, value) in object.entries() {
                out.insert(key.clone(), decode_tags(value));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

fn first_diff(actual: &JsValue, expected: &JsValue, path: &str) -> Option<String> {
    match (actual, expected) {
        (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => None,
        (JsValue::Bool(a), JsValue::Bool(b)) => {
            (a != b).then(|| format!("{path}: bool {a} != {b}"))
        }
        (JsValue::String(a), JsValue::String(b)) => {
            (a != b).then(|| format!("{path}: {a:?} != {b:?}"))
        }
        (JsValue::Number(a), JsValue::Number(b)) => {
            let same = (a.is_nan() && b.is_nan())
                || (a == b && (a != &0.0 || a.is_sign_negative() == b.is_sign_negative()));
            (!same).then(|| format!("{path}: {a} != {b}"))
        }
        (JsValue::Array(a), JsValue::Array(b)) => {
            if a.len() != b.len() {
                return Some(format!("{path}: array length {} != {}", a.len(), b.len()));
            }
            a.iter()
                .zip(b)
                .enumerate()
                .find_map(|(i, (a, b))| first_diff(a, b, &format!("{path}[{i}]")))
        }
        (JsValue::Object(a), JsValue::Object(b)) => {
            for (index, (key, value)) in a.entries().iter().enumerate() {
                let name = String::from_utf16_lossy(key.units());
                let Some((other_key, other)) = b.entries().get(index) else {
                    return Some(format!("{path}.{name}: unexpected key"));
                };
                if key != other_key {
                    return Some(format!(
                        "{path}.{name}: key order differs (expected {})",
                        String::from_utf16_lossy(other_key.units())
                    ));
                }
                if let Some(d) = first_diff(value, other, &format!("{path}.{name}")) {
                    return Some(d);
                }
            }
            if let Some((key, _)) = b.entries().get(a.len()) {
                return Some(format!(
                    "{path}.{}: missing key",
                    String::from_utf16_lossy(key.units())
                ));
            }
            None
        }
        _ => Some(format!("{path}: type mismatch")),
    }
}

fn js_eq(actual: &JsValue, expected: &JsValue) -> bool {
    match (actual, expected) {
        (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => true,
        (JsValue::Bool(a), JsValue::Bool(b)) => a == b,
        (JsValue::String(a), JsValue::String(b)) => a == b,
        (JsValue::Number(a), JsValue::Number(b)) => {
            (a.is_nan() && b.is_nan())
                || (a == b && (a != &0.0 || a.is_sign_negative() == b.is_sign_negative()))
        }
        (JsValue::Array(a), JsValue::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| js_eq(a, b))
        }
        (JsValue::Object(a), JsValue::Object(b)) => {
            a.len() == b.len()
                && a.entries().iter().zip(b.entries()).all(
                    |((left_key, left), (right_key, right))| {
                        left_key == right_key && js_eq(left, right)
                    },
                )
        }
        _ => false,
    }
}

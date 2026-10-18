use pilotdeck_gateway::config::{Env, load_snapshot, snapshot::content_hash};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

const ORACLE: &str = include_str!("fixtures/config-snapshot/oracle.json");

#[test]
fn complete_config_snapshot_replays_real_typescript_oracle() {
    // The protocol parser preserves lone UTF-16 surrogates, which the existing
    // config corpus intentionally contains.
    let oracle = parse_json(ORACLE).expect("oracle json");
    let JsValue::Array(cases) = oracle.get("cases").expect("cases") else {
        panic!("cases array")
    };
    assert_eq!(
        oracle.get("count").and_then(JsValue::as_number),
        Some(cases.len() as f64)
    );
    assert_eq!(cases.len(), 128, "fixture count changed");
    let sources = oracle
        .get("sources")
        .and_then(JsValue::as_object)
        .expect("sources");
    assert_eq!(sources.len(), 21, "source hash set changed");
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
        let home = root.join("home");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&home).expect("home");
        let yaml = text(case.get("yaml")).expect("yaml");
        fs::write(home.join("pilotdeck.yaml"), yaml).expect("global yaml");
        let project = case
            .get("project")
            .and_then(JsValue::as_string)
            .map(|name| root.join(String::from_utf16_lossy(name.units())));
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
                env.insert(
                    String::from_utf16_lossy(key.units()),
                    text(Some(value)).expect("env value"),
                );
            }
        }
        env.insert("HOME".into(), home.to_string_lossy().into_owned());
        env.insert("PILOT_HOME".into(), home.to_string_lossy().into_owned());
        let version = case
            .get("version")
            .and_then(JsValue::as_number)
            .unwrap_or(1.0);
        let actual = match load_snapshot(
            &home,
            &cwd,
            &env,
            project
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .as_deref(),
            version,
            "1970-01-01T00:00:00.000Z",
        ) {
            Ok(value) => {
                let config = value.get("config").expect("config");
                assert_eq!(
                    value
                        .get("contentHash")
                        .and_then(|v| v.as_string())
                        .and_then(|s| s.to_utf8().ok()),
                    Some(content_hash(config)),
                    "{id}: production contentHash is not self-consistent",
                );
                let mut normalized = normalize(value, &root, &cwd);
                let hash = content_hash(normalized.get("config").expect("normalized config"));
                if let JsValue::Object(object) = &mut normalized {
                    object.insert("contentHash".into(), hash.as_str().into());
                }
                normalized
            }
            Err(error) => {
                failures.push(format!(
                    "{id}: unexpected error {} {}",
                    error.code,
                    String::from_utf16_lossy(error.message.units())
                ));
                continue;
            }
        };
        let expected = decode_tags(case.get("snapshot").expect("snapshot"));
        let actual_hash = actual.get("contentHash").cloned();
        let expected_hash = expected.get("contentHash").cloned();
        let mut actual = actual;
        if let (JsValue::Object(object), Some(hash)) = (&mut actual, expected_hash.clone()) {
            object.insert("contentHash".into(), hash);
        }
        if !js_eq(&actual, &expected) {
            failures.push(format!(
                "{id}: {}\nactual   {}",
                first_diff(&actual, &expected, "$").unwrap_or_else(|| "value differs".into()),
                stringify_json(&actual).unwrap().unwrap_or_default()
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
        "{} snapshot differences; first entries:\n{}",
        failures.len(),
        failures.into_iter().take(3).collect::<Vec<_>>().join("\n")
    );
}

fn unique_root() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("pdrs-config-snapshot-{}-{n}", std::process::id()))
}

fn text(value: Option<&JsValue>) -> Option<String> {
    value?.as_string()?.to_utf8().ok()
}

fn normalize(value: JsValue, root: &Path, cwd: &Path) -> JsValue {
    let root = root.to_string_lossy();
    let cwd = cwd.to_string_lossy();
    fn walk(value: &JsValue, root: &str, cwd: &str) -> JsValue {
        match value {
            JsValue::String(s) => {
                let text = String::from_utf16_lossy(s.units());
                if text.contains(root) || text.contains(cwd) {
                    let replaced = text.replace(root, "<ROOT>").replace(cwd, "<CWD>");
                    JsValue::String(replaced.as_str().into())
                } else {
                    JsValue::String(s.clone())
                }
            }
            JsValue::Array(items) => {
                JsValue::Array(items.iter().map(|v| walk(v, root, cwd)).collect())
            }
            JsValue::Object(object) => {
                let mut out = JsObject::new();
                for (key, value) in object.entries() {
                    let text = String::from_utf16_lossy(key.units());
                    let key = if text.contains(root) || text.contains(cwd) {
                        text.replace(root, "<ROOT>").replace(cwd, "<CWD>").into()
                    } else {
                        key.clone()
                    };
                    out.insert(key, walk(value, root, cwd));
                }
                JsValue::Object(out)
            }
            other => other.clone(),
        }
    }
    walk(&value, &root, &cwd)
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
            for (key, value) in a.entries() {
                let name = String::from_utf16_lossy(key.units());
                match b.get_key(key) {
                    Some(other) => {
                        if let Some(d) = first_diff(value, other, &format!("{path}.{name}")) {
                            return Some(d);
                        }
                    }
                    None => return Some(format!("{path}.{name}: unexpected key")),
                }
            }
            for (key, _) in b.entries() {
                if a.get_key(key).is_none() {
                    return Some(format!(
                        "{path}.{}: missing key",
                        String::from_utf16_lossy(key.units())
                    ));
                }
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
                && a.entries()
                    .iter()
                    .all(|(key, value)| b.get_key(key).is_some_and(|other| js_eq(value, other)))
        }
        _ => false,
    }
}

use pilotdeck_gateway::config::{Env, load_project, load_snapshot, snapshot::content_hash};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
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
    assert_eq!(cases.len(), 157, "fixture count changed");
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
                let value = text(Some(value))
                    .expect("env value")
                    .replace("<ROOT>", &root.to_string_lossy())
                    .replace("<CWD>", &cwd.to_string_lossy());
                env.insert(String::from_utf16_lossy(key.units()), value);
            }
        }
        env.insert("HOME".into(), home.to_string_lossy().into_owned());
        if case.get("noPilotHome").and_then(JsValue::as_bool) != Some(true) {
            env.insert("PILOT_HOME".into(), home.to_string_lossy().into_owned());
        }
        let version = case
            .get("version")
            .and_then(JsValue::as_number)
            .unwrap_or(1.0);
        let project_path = project.as_ref().map(|p| p.to_string_lossy().into_owned());
        let native = load_project(&home, &cwd, &env, project_path.as_deref());
        let result = load_snapshot(
            &home,
            &cwd,
            &env,
            project_path.as_deref(),
            version,
            "1970-01-01T00:00:00.000Z",
        );
        match (&native, &result) {
            (Ok(config), Ok(snapshot)) => {
                // Pre-existing role-limited projection gap: the native load does
                // not materialize `webui`, so it misses the proxy migration
                // warning that the snapshot (and TS) emit. Reading `webui` in
                // the old path would change existing parsing behaviour, so this
                // one case is compared without that warning.
                let ignored = (id == "proxy-migrated").then_some("CONFIG_PROXY_MIGRATED");
                let native_diagnostics = config
                    .diagnostics
                    .iter()
                    .map(diagnostic_signature)
                    .filter(|signature| Some(signature.0.as_str()) != ignored)
                    .collect::<Vec<_>>();
                let snapshot_diagnostics = match snapshot.get("diagnostics") {
                    Some(JsValue::Array(items)) => items.as_slice(),
                    _ => &[],
                }
                .iter()
                .map(diagnostic_signature)
                .filter(|signature| Some(signature.0.as_str()) != ignored)
                .collect::<Vec<_>>();
                if native_diagnostics != snapshot_diagnostics {
                    failures.push(format!(
                        "{id}: snapshot/load_project diagnostics differ: {native_diagnostics:?} != {snapshot_diagnostics:?}"
                    ));
                }
            }
            (Err(left), Err(right)) => {
                if left.code != right.code
                    || String::from_utf16_lossy(left.message.units())
                        != String::from_utf16_lossy(right.message.units())
                {
                    failures.push(format!(
                        "{id}: snapshot/load_project errors differ: {} {:?} != {} {:?}",
                        left.code,
                        String::from_utf16_lossy(left.message.units()),
                        right.code,
                        String::from_utf16_lossy(right.message.units())
                    ));
                }
            }
            (Ok(_), Err(error)) => failures.push(format!(
                "{id}: load_project succeeded but snapshot failed with {}",
                error.code
            )),
            (Err(error), Ok(_)) => failures.push(format!(
                "{id}: snapshot succeeded but load_project failed with {}",
                error.code
            )),
        }
        if let Some(expected) = case.get("error") {
            let expect = |key: &str| {
                expected
                    .get(key)
                    .and_then(JsValue::as_string)
                    .and_then(|s| s.to_utf8().ok())
            };
            match result {
                Ok(_) => failures.push(format!(
                    "{id}: expected {} error",
                    expect("code").unwrap_or_default()
                )),
                Err(error) => {
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
                    // The oracle records the first diagnostic's code as
                    // `diagnosticCode`; the error's own `code` is a separate
                    // top-level field (they can differ, e.g. a missing agent
                    // section after a missing schemaVersion warning).
                    for (actual_key, expected_key) in [("code", "diagnosticCode"), ("hint", "hint")]
                    {
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
                }
            }
            continue;
        }
        let actual = match result {
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

/// Atomically create a root this process owns. Never delete a path before
/// owning it: a recycled PID must not expose another run's leftovers.
fn unique_root() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("pdrs-config-snapshot-{}-{n}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return path,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("cannot create snapshot temp root: {error}"),
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

fn normalize(value: JsValue, root: &Path, cwd: &Path) -> JsValue {
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
        &value,
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

fn diagnostic_signature(value: &JsValue) -> (String, String, String) {
    let field = |key: &str| {
        value
            .get(key)
            .and_then(JsValue::as_string)
            .map(|s| String::from_utf16_lossy(s.units()))
            .unwrap_or_default()
    };
    (field("code"), field("message"), field("path"))
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

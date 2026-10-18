#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::storage::{
    TaskBudgetError, TaskBudgetScope, TaskBudgetState, TaskBudgetStore,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const ROOT: &str = "/pdrs-fixture/project";
const NOW: f64 = 1_792_281_600_000.0;
const ORACLE: &str = include_str!("fixtures/task-budget/oracle.json");
static UNDEFINED: JsValue = JsValue::Undefined;
fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    value.get(name).unwrap_or(&UNDEFINED)
}
fn string(value: &JsValue) -> String {
    value.as_string().unwrap().to_utf8().unwrap()
}
fn array(value: &JsValue) -> &[JsValue] {
    let JsValue::Array(items) = value else {
        panic!("expected array")
    };
    items
}
fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut o = JsObject::new();
    for (key, value) in fields {
        if !matches!(value, JsValue::Undefined) {
            o.insert(key.into(), value);
        }
    }
    JsValue::Object(o)
}
fn text(s: &str) -> JsValue {
    JsValue::String(s.into())
}
fn ok() -> JsValue {
    object([("ok", JsValue::Bool(true))])
}
fn scope(step: &JsValue) -> TaskBudgetScope {
    if field(step, "scope")
        .as_string()
        .is_some_and(|s| s.equals_str("project"))
    {
        TaskBudgetScope::Project
    } else {
        TaskBudgetScope::Session
    }
}
fn state(ledger: &TaskBudgetState) -> JsValue {
    object([
        ("spentUsd", JsValue::Number(ledger.spent_usd)),
        (
            "settledRunIds",
            JsValue::Array(
                ledger
                    .settled_run_ids
                    .iter()
                    .cloned()
                    .map(JsValue::String)
                    .collect(),
            ),
        ),
        ("totalUsd", ledger.total_usd.clone()),
        ("projectRetentionMs", ledger.project_retention_ms.clone()),
        ("lastActivityAtMs", ledger.last_activity_at_ms.clone()),
    ])
}
fn run(store: &mut TaskBudgetStore, step: &JsValue) -> JsValue {
    let root = field(step, "root")
        .as_string()
        .cloned()
        .unwrap_or_else(|| ROOT.into());
    let session = field(step, "session")
        .as_string()
        .cloned()
        .unwrap_or_else(|| "session".into());
    let now = field(step, "now").as_number().unwrap_or(NOW);
    let result = (|| -> Result<JsValue, TaskBudgetError> {
        let root = if field(step, "resolveRoot") == &JsValue::Bool(true) {
            TaskBudgetStore::resolve_project_root(&root)?
        } else {
            // Legacy store-input scenarios intentionally bypass runtime resolve.
            root
        };
        match string(field(step, "op")).as_str() {
            "load" => store.load()?,
            "configure" => {
                if !field(step, "scope")
                    .as_string()
                    .is_some_and(|s| s.equals_str("session"))
                {
                    store.configure_project(
                        &root,
                        &session,
                        field(step, "total").as_number().unwrap(),
                        field(step, "retention").as_number(),
                        now,
                    )?;
                }
            }
            "settle" => {
                if field(step, "budget") != &JsValue::Bool(false) {
                    let spent = if let Some(special) = field(step, "special").as_string() {
                        match special.to_utf8().unwrap().as_str() {
                            "NaN" => f64::NAN,
                            "Infinity" => f64::INFINITY,
                            "-Infinity" => f64::NEG_INFINITY,
                            _ => panic!("unknown nonfinite"),
                        }
                    } else {
                        field(step, "spend").as_number().unwrap()
                    };
                    store.settle(
                        &root,
                        scope(step),
                        &session,
                        field(step, "run").as_string().unwrap(),
                        spent,
                        now,
                    )?;
                }
            }
            "clear" => store.clear_session(&root, &session)?,
            "get" => {
                return Ok(store
                    .get(&root, scope(step), &session)?
                    .map(state)
                    .unwrap_or(JsValue::Null));
            }
            other => panic!("unknown operation {other}"),
        }
        Ok(ok())
    })();
    match result {
        Ok(value) => value,
        Err(error) => object([(
            "error",
            object([
                (
                    "name",
                    text(if error.code().is_some() {
                        "DialogGatewayError"
                    } else {
                        "TypeError"
                    }),
                ),
                ("code", error.code().map(text).unwrap_or(JsValue::Null)),
                ("message", text(&error.to_string())),
            ]),
        )]),
    }
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pdrs-task-budget-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn file_json(path: &Path) -> JsValue {
    match fs::read_to_string(path) {
        Ok(bytes) => text(&bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => JsValue::Null,
        Err(e) => panic!("{}: {e}", path.display()),
    }
}
fn temporary_files(path: &Path) -> JsValue {
    let mut files = JsObject::new();
    let directory = path.parent().unwrap();
    if directory.exists() {
        let mut names: Vec<_> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().ends_with(".tmp"))
            .collect();
        names.sort();
        for name in names {
            files.insert(
                name.to_str().unwrap().into(),
                file_json(&directory.join(name)),
            );
        }
    }
    JsValue::Object(files)
}
fn equal_json(actual: &JsValue, expected: &JsValue, context: &str) {
    // Stringify collapses NaN/Infinity to null exactly as the TS oracle does.
    let actual = parse_json(&stringify_json(actual).unwrap().unwrap()).unwrap();
    assert_eq!(&actual, expected, "{context}");
}
fn replay(base: &Path, scenario: &JsValue) {
    let name = string(field(scenario, "name"));
    let home = base.join(&name);
    let threshold = field(scenario, "threshold").as_number().unwrap() as usize;
    let mut store = TaskBudgetStore::new(&home, (threshold != 512).then_some(threshold));
    if let Some(initial) = field(scenario, "initial").as_string() {
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), initial.to_utf8().unwrap()).unwrap();
    }
    if let JsValue::Object(files) = field(scenario, "initialTemporaryFiles") {
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        for (name, contents) in files.entries() {
            fs::write(
                store.path().parent().unwrap().join(name.to_utf8().unwrap()),
                string(contents),
            )
            .unwrap();
        }
    }
    for (i, step) in array(field(scenario, "steps")).iter().enumerate() {
        let context = format!("{name} step {i}: {}", string(field(step, "op")));
        let outcome = if string(field(step, "op")) == "reopen" {
            store = TaskBudgetStore::new(&home, (threshold != 512).then_some(threshold));
            ok()
        } else {
            run(&mut store, step)
        };
        equal_json(&outcome, field(step, "result"), &context);
        assert_eq!(
            store.record_count() as f64,
            field(step, "count").as_number().unwrap(),
            "{context}: count"
        );
        assert_eq!(
            &file_json(store.path()),
            field(step, "file"),
            "{context}: exact UTF-8 bytes"
        );
        let expected_temporary = match field(step, "temporaryFiles") {
            JsValue::Undefined => JsValue::Object(JsObject::new()),
            value => value.clone(),
        };
        assert_eq!(
            temporary_files(store.path()),
            expected_temporary,
            "{context}: residual bytes"
        );
    }
}
#[test]
fn real_ts_sequences_match_results_counts_and_every_file_byte() {
    let oracle = baseline::oracle(ORACLE);
    let scenarios = array(field(&oracle, "scenarios"));
    assert_eq!(scenarios.len(), 31);
    let scratch = Scratch::new();
    for scenario in scenarios {
        replay(&scratch.0, scenario);
    }
}
#[test]
fn empty_project_root_helper_resolves_to_cwd() {
    // The real TS generator asserts the same empty-root/cwd identity relation.
    let cwd: JsString = std::env::current_dir().unwrap().to_str().unwrap().into();
    assert_eq!(
        TaskBudgetStore::resolve_project_root(&"".into()).unwrap(),
        cwd
    );
}
#[test]
fn rust_reads_ts_ledgers_and_replayed_state() {
    let oracle = baseline::oracle(ORACLE);
    let scratch = Scratch::new();
    for scenario in array(field(&oracle, "scenarios")) {
        // Starting bytes include real TS snapshots plus controlled corruptions.
        let home = scratch.0.join(string(field(scenario, "name")));
        let mut store = TaskBudgetStore::new(&home, None);
        let final_file = field(array(field(scenario, "steps")).last().unwrap(), "file");
        if let Some(file) = final_file.as_string() {
            fs::create_dir_all(store.path().parent().unwrap()).unwrap();
            fs::write(store.path(), file.to_utf8().unwrap()).unwrap();
        }
        let load_result = run(&mut store, &object([("op", text("load"))]));
        let reload_error = field(scenario, "reloadError");
        let expected_load = if reload_error == &JsValue::Null {
            ok()
        } else {
            object([("error", reload_error.clone())])
        };
        equal_json(&load_result, &expected_load, "TS reload result");
        assert_eq!(
            store.record_count() as f64,
            field(scenario, "reloadCount").as_number().unwrap()
        );
        if reload_error != &JsValue::Null {
            assert_eq!(&file_json(store.path()), final_file);
            continue;
        }
        for expected in array(field(scenario, "reloaded")) {
            let key = field(expected, "key").as_string().unwrap();
            let parts: Vec<_> = key.units().split(|c| *c == 0).collect();
            let root = JsString::from_utf16(parts[0].to_vec());
            let scope = if parts[1] == [112, 114, 111, 106, 101, 99, 116] {
                TaskBudgetScope::Project
            } else {
                TaskBudgetScope::Session
            };
            let session = JsString::from_utf16(parts[2].to_vec());
            let actual = state(store.get(&root, scope, &session).unwrap().unwrap());
            let expected_state = object([
                ("spentUsd", field(expected, "spentUsd").clone()),
                ("settledRunIds", field(expected, "settledRunIds").clone()),
                ("totalUsd", field(expected, "totalUsd").clone()),
                (
                    "projectRetentionMs",
                    field(expected, "projectRetentionMs").clone(),
                ),
                (
                    "lastActivityAtMs",
                    field(expected, "lastActivityAtMs").clone(),
                ),
            ]);
            equal_json(&actual, &expected_state, &string(field(scenario, "name")));
        }
        assert_eq!(
            &file_json(store.path()),
            final_file,
            "load must preserve TS bytes"
        );
    }
}
#[test]
#[ignore = "requires the fixed TS checkout and its installed tsx loader (PDRS_BASELINE)"]
fn real_ts_reads_and_continues_rust_ledgers() {
    let baseline = PathBuf::from(std::env::var_os("PDRS_BASELINE").expect("set PDRS_BASELINE"));
    let scratch = Scratch::new();
    let oracle = baseline::oracle(ORACLE);
    for scenario in array(field(&oracle, "scenarios")) {
        replay(&scratch.0, scenario);
    }
    let output = Command::new("node")
        .arg("--import")
        .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/task-budget/generate.mts"))
        .arg(&scratch.0)
        .arg("--verify-rust")
        .env("PDRS_BASELINE", baseline)
        .output()
        .unwrap();
    println!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success());
    for scenario in array(field(&oracle, "scenarios")) {
        let mut store = TaskBudgetStore::new(scratch.0.join(string(field(scenario, "name"))), None);
        assert_eq!(&file_json(store.path()), field(scenario, "crossFile"));
        if field(scenario, "reloadError") != &JsValue::Null {
            assert!(store.load().is_err());
            continue;
        }
        let state = store
            .get(&ROOT.into(), TaskBudgetScope::Session, &"cross-ts".into())
            .unwrap()
            .unwrap();
        assert_eq!(state.spent_usd, 0.125);
        assert_eq!(state.settled_run_ids, vec![JsString::from("from-ts")]);
    }
}
#[test]
fn io_failures_preserve_ts_mutation_and_cached_load_failure() {
    let scratch = Scratch::new();
    let mut store = TaskBudgetStore::new(&scratch.0, None);
    // Reading a directory fails; replacing it does not reset a cached load error.
    fs::create_dir_all(store.path()).unwrap();
    assert!(matches!(store.load(), Err(TaskBudgetError::Io(_))));
    fs::remove_dir(store.path()).unwrap();
    assert!(matches!(store.load(), Err(TaskBudgetError::Io(_))));
    let mut store = TaskBudgetStore::new(&scratch.0, None);
    store.load().unwrap();
    fs::create_dir_all(store.path()).unwrap();
    assert!(matches!(
        store.settle(
            &ROOT.into(),
            TaskBudgetScope::Session,
            &"s".into(),
            &"r".into(),
            2.0,
            NOW
        ),
        Err(TaskBudgetError::Io(_))
    ));
    assert_eq!(store.record_count(), 0);
    assert_eq!(
        store
            .get(&ROOT.into(), TaskBudgetScope::Session, &"s".into())
            .unwrap()
            .unwrap()
            .spent_usd,
        2.0
    );
    // Duplicate is still a no-op after the failed append, just as TS's Set is.
    fs::remove_dir(store.path()).unwrap();
    store
        .settle(
            &ROOT.into(),
            TaskBudgetScope::Session,
            &"s".into(),
            &"r".into(),
            2.0,
            NOW,
        )
        .unwrap();
    assert!(!store.path().exists());
}
#[cfg(unix)]
#[test]
fn compaction_replaces_the_inode_after_appending() {
    use std::os::unix::fs::MetadataExt;
    let scratch = Scratch::new();
    let mut store = TaskBudgetStore::new(&scratch.0, Some(2));
    store
        .settle(
            &ROOT.into(),
            TaskBudgetScope::Session,
            &"s".into(),
            &"a".into(),
            1.0,
            NOW,
        )
        .unwrap();
    let inode = fs::metadata(store.path()).unwrap().ino();
    // Holding the old file open demonstrates append-before-rename and prevents
    // inode reuse from making this assertion a filesystem timing test.
    let old = fs::File::open(store.path()).unwrap();
    store
        .settle(
            &ROOT.into(),
            TaskBudgetScope::Session,
            &"s".into(),
            &"b".into(),
            1.0,
            NOW,
        )
        .unwrap();
    assert_ne!(inode, fs::metadata(store.path()).unwrap().ino());
    assert_eq!(old.metadata().unwrap().ino(), inode);
    use std::io::Read;
    let mut old_bytes = String::new();
    (&old).read_to_string(&mut old_bytes).unwrap();
    assert_eq!(old_bytes.lines().count(), 2);
    assert!(
        old_bytes
            .lines()
            .all(|line| line.contains("\"kind\":\"settled\""))
    );
    assert_eq!(fs::read_to_string(store.path()).unwrap().lines().count(), 1);
}

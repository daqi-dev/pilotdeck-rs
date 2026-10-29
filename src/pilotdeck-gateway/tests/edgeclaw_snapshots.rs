//! Bidirectional real-TS snapshot transcripts, including intermediate disk/SQL.
use base64::{Engine, engine::general_purpose::STANDARD};
use pilotdeck_gateway::runtime::memory::edgeclaw::{
    EdgeClawFileStores, FileMemoryError, L0SessionRecord, StorageError,
    storage::snapshots::{MemoryRepositoryStage, SnapshotIoOperation},
};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/exact_receipt.rs"]
mod exact_receipt;
const ORACLE: &str = include_str!("fixtures/edgeclaw-snapshots/oracle.json");
const NOW: &str = "2026-10-22T00:00:00.000Z";
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("pdrs-snapshots-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
fn make_writable(p: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
    }
    if let Ok(entries) = fs::read_dir(p) {
        for e in entries.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                make_writable(&e.path());
            }
        }
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        make_writable(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn cases(o: &Value) -> Vec<&Value> {
    let filter = std::env::var("PDRS_SNAPSHOT_CASE").ok();
    let cases = o["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| {
            filter
                .as_ref()
                .is_none_or(|name| c["name"] == name.as_str())
        })
        .collect::<Vec<_>>();
    assert!(!cases.is_empty(), "no scenario matched PDRS_SNAPSHOT_CASE");
    cases
}
fn oracle() -> Value {
    baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).unwrap()
}
fn js(v: &Value) -> JsValue {
    parse_json(&v.to_string()).unwrap()
}
fn obj(v: &Value) -> JsObject {
    js(v).as_object().unwrap().clone()
}
fn value(v: JsValue) -> Value {
    if matches!(v, JsValue::Undefined) {
        Value::Null
    } else {
        serde_json::from_str(&stringify_json(&v).unwrap().unwrap()).unwrap()
    }
}
fn clean_text(s: &str, root: &Path) -> String {
    let s = s
        .replace(fs::canonicalize(root).unwrap().to_str().unwrap(), "$ROOT")
        .replace(root.to_str().unwrap(), "$ROOT");
    // Temp suffixes are process-unique names, not observable memory content.
    static TEMPS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re=TEMPS.get_or_init(||regex::Regex::new(r"\.(memory|global|control\.sqlite|last_dream)\.([a-zA-Z0-9_-]+?)-[a-z0-9]+(?:-[a-z0-9]+)?(/|$)").unwrap());
    re.replace_all(&s, ".$1.$2-TEMP$3").into_owned()
}
fn clean(v: &Value, root: &Path) -> Value {
    match v {
        Value::String(s) => json!(clean_text(s, root)),
        Value::Array(a) => json!(a.iter().map(|v| clean(v, root)).collect::<Vec<_>>()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, v)| (k.clone(), clean(v, root))).collect())
        }
        v => v.clone(),
    }
}
fn open(root: &Path) -> EdgeClawFileStores {
    EdgeClawFileStores::with_workspace_clock(
        root.join("control.sqlite"),
        root.join("memory"),
        root.join("global"),
        "/fixture/workspace",
        root.join("home/.pilotdeck"),
        Arc::new(|| NOW.into()),
    )
    .unwrap()
}
fn candidate(kind: &str, name: &str) -> Value {
    json!({"type":kind,"scope":if kind=="user"{"global"}else{"project"},"name":name,"description":format!("{name} description"),"summary":format!("{name} summary"),"profile":format!("{name} profile"),"capturedAt":NOW,"sourceSessionKey":format!("session-{name}")})
}
fn seed(root: &Path) {
    let r = open(root);
    r.storage
        .insert_l0_session(&L0SessionRecord {
            l0_index_id: "retained-l0".into(),
            session_key: "retained-session".into(),
            timestamp: NOW.into(),
            messages: parse_json(r#"[{"role":"user","content":"retained"}]"#).unwrap(),
            source: "openclaw".into(),
            indexed: false,
            created_at: NOW.into(),
        })
        .unwrap();
    r.workspace_memory
        .upsert_project_meta(&obj(&json!({"projectName":"Snapshot project"})))
        .unwrap();
    for kind in ["project", "feedback"] {
        r.workspace_memory
            .upsert_candidate(&obj(&candidate(kind, kind)))
            .unwrap();
    }
    r.global_user_memory
        .upsert_candidate(&obj(&candidate("user", "user")))
        .unwrap();
    for (k, v) in [
        ("workspaceDir", json!(" /fixture/workspace ")),
        ("lastDreamAt", json!(" 2026-before ")),
        ("lastDreamStatus", json!("success")),
        ("lastDreamSummary", json!(" before summary ")),
        ("unrelated", json!({"keep":true})),
    ] {
        r.storage.set_pipeline_state(k, &js(&v)).unwrap();
    }
    r.close().unwrap();
}
fn query(c: &Connection, sql: &str) -> Value {
    let mut stmt = c.prepare(sql).unwrap();
    let names = stmt
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let mut rows = stmt.query([]).unwrap();
    let mut out = vec![];
    while let Some(row) = rows.next().unwrap() {
        let mut o = serde_json::Map::new();
        for (i, name) in names.iter().enumerate() {
            let v = match row.get_ref(i).unwrap() {
                rusqlite::types::ValueRef::Null => Value::Null,
                rusqlite::types::ValueRef::Integer(n) => json!(n),
                rusqlite::types::ValueRef::Real(n) => json!(n),
                rusqlite::types::ValueRef::Text(s) => json!(std::str::from_utf8(s).unwrap()),
                _ => panic!("unexpected blob in snapshot database"),
            };
            o.insert(name.clone(), v);
        }
        out.push(Value::Object(o));
    }
    json!(out)
}
fn iso_clock() -> &'static regex::Regex {
    static ISO: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    ISO.get_or_init(|| regex::Regex::new(r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$").unwrap())
}
fn disk(root: &Path) -> Value {
    fn walk(root: &Path, p: &Path, out: &mut Vec<Value>) {
        for e in fs::read_dir(p).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let relative = clean_text(p.strip_prefix(root).unwrap().to_str().unwrap(), root);
            if e.file_type().unwrap().is_symlink() {
                out.push(json!({"relativePath":relative,"link":clean_text(fs::read_link(&p).unwrap().to_str().unwrap(),root)}));
            } else if e.file_type().unwrap().is_dir() {
                out.push(json!({"relativePath":relative,"dir":true}));
                walk(root, &p, out)
            } else if name.contains("control.sqlite") {
                if !name.ends_with("-wal") && !name.ends_with("-shm") {
                    let c =
                        Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                            .unwrap();
                    let mut rows = query(&c, "SELECT * FROM pipeline_state ORDER BY state_key");
                    for r in rows.as_array_mut().unwrap() {
                        assert!(iso_clock().is_match(r["updated_at"].as_str().unwrap()));
                        r["updated_at"] = json!("<CLOCK>");
                    }
                    let l0 = query(&c, "SELECT * FROM l0_sessions ORDER BY l0_index_id");
                    let schema = query(
                        &c,
                        "SELECT name,type,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name,type",
                    );
                    out.push(
                        json!({"relativePath":relative,"rows":rows,"l0Rows":l0,"schema":schema}),
                    );
                }
            } else {
                out.push(json!({"relativePath":relative,"content":clean_text(&fs::read_to_string(p).unwrap(),root)}));
            }
        }
    }
    let mut out = vec![];
    walk(root, root, &mut out);
    out.sort_by(|a, b| a["relativePath"].as_str().cmp(&b["relativePath"].as_str()));
    json!(out)
}
fn error(e: FileMemoryError) -> Value {
    let io_code = e.io_code();
    match e {
        FileMemoryError::Io(e) => {
            json!({"error":io_code.unwrap_or_else(|| panic!("unclassified IO {e:?}"))})
        }
        FileMemoryError::Storage(StorageError::Sqlite(_)) => json!({"error":"ERR_SQLITE_ERROR"}),
        FileMemoryError::Storage(StorageError::Closed) => json!({"error":"ERR_INVALID_STATE"}),
        e => {
            let message = e.to_string();
            if message.starts_with("Cannot read properties") {
                json!({"error":"TypeError","message":message})
            } else {
                json!({"error":if message.starts_with("Invalid imported snapshot file path:"){"MemoryBundleValidationError"}else{"Error"},"message":message})
            }
        }
    }
}
fn metadata(before: &JsValue, after: &JsValue, extra: &Value) -> JsValue {
    // Construct using JsObject insertion order, like the TS literal.
    let mut o = JsObject::new();
    for (k, v) in [
        ("version", json!(1)),
        ("capturedAt", json!("2026-10-21T01:02:03.000Z")),
        ("sourceAction", json!("dream")),
        ("sourceWorkspaceDir", json!("/fixture/workspace")),
    ] {
        o.insert(k.into(), js(&v));
    }
    for (k, b) in [("before", before), ("after", after)] {
        let mut o2 = JsObject::new();
        for f in [
            "workspaceVersion",
            "globalVersion",
            "counts",
            "runtimeState",
        ] {
            o2.insert(f.into(), b.get(f).unwrap().clone());
        }
        o.insert(k.into(), JsValue::Object(o2));
    }
    if let Some(extra) = extra.as_object() {
        for k in ["trigger", "dreamTraceId", "summary"] {
            if let Some(v) = extra.get(k) {
                o.insert(k.into(), js(v));
            }
        }
    }
    JsValue::Object(o)
}
fn assert_value(a: &Value, b: &Value, label: &str) {
    match (a, b) {
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (i, (a, b)) in a.iter().zip(b).enumerate() {
                assert_value(a, b, &format!("{label}/{i}"));
            }
        }
        (Value::Object(a), Value::Object(b)) if a.keys().eq(b.keys()) => {
            for (k, a) in a {
                assert_value(a, &b[k], &format!("{label}/{k}"));
            }
        }
        _ => assert_eq!(a, b, "{label}"),
    }
}
fn replay(root: &Path, case: &Value) -> Value {
    let mut r = open(root);
    let mut second: Option<EdgeClawFileStores> = None;
    let mut vars: HashMap<String, JsValue> = HashMap::new();
    let mut stages: HashMap<String, MemoryRepositoryStage> = HashMap::new();
    let mut observations = vec![];
    for (i, op) in case["ops"].as_array().unwrap().iter().enumerate() {
        let faults = op["faults"].as_array().cloned().unwrap_or_default();
        let fired = Arc::new(Mutex::new(vec![]));
        let counters = Arc::new(Mutex::new(vec![0usize; faults.len()]));
        let f2 = fired.clone();
        let root2 = root.to_path_buf();
        let hook = Arc::new(
            move |kind: SnapshotIoOperation, p: &Path, dst: Option<&Path>| -> io::Result<()> {
                let path = format!(
                    "{}{}",
                    clean_text(&p.to_string_lossy(), &root2),
                    dst.map(|p| format!(" -> {}", clean_text(&p.to_string_lossy(), &root2)))
                        .unwrap_or_default()
                );
                let mut c = counters.lock().unwrap();
                for (i, f) in faults.iter().enumerate() {
                    if format!("{kind:?}") == f["kind"].as_str().unwrap()
                        && path.contains(f["contains"].as_str().unwrap())
                    {
                        c[i] += 1;
                        if c[i] == f["nth"].as_u64().unwrap() as usize {
                            f2.lock().unwrap().push(i);
                            return Err(io::Error::from_raw_os_error(
                                match f["code"].as_str().unwrap() {
                                    "EIO" => 5,
                                    "EACCES" => 13,
                                    "EPERM" => 1,
                                    "ENOSPC" => 28,
                                    s => panic!("unknown fault {s}"),
                                },
                            ));
                        }
                    }
                }
                Ok(())
            },
        );
        r.set_snapshot_io_hook(hook.clone());
        if let Some(s) = &mut second {
            s.set_snapshot_io_hook(hook);
        }
        if op["method"] == "second" {
            second = Some(open(
                &op["rootAlias"]
                    .as_str()
                    .map(|p| root.join(p))
                    .unwrap_or_else(|| root.to_path_buf()),
            ));
        }
        let repo = if op["second"] == true {
            second.as_ref().unwrap()
        } else {
            &r
        };
        let id = op["id"].as_str().unwrap_or("");
        let result: Result<JsValue, FileMemoryError> = (|| match op["method"].as_str().unwrap() {
            "options" => {
                let w = repo.workspace_store_options();
                let g = repo.global_store_options();
                assert!(w.manifest_user_entries_provider.is_none());
                assert!(g.manifest_user_entries_provider.is_none());
                Ok(js(
                    &json!([{"workspaceMode":w.workspace_mode,"manageProjectMeta":w.manage_project_meta,"manageProjectFiles":w.manage_project_files,"manageUserProfile":w.manage_user_profile,"enableManifest":w.enable_manifest},{"manageProjectMeta":g.manage_project_meta,"manageProjectFiles":g.manage_project_files,"manageUserProfile":g.manage_user_profile,"userProfileRelativePath":g.user_profile_relative_path,"userNotesRelativeDir":g.user_notes_relative_dir,"appendOnlyUserEntries":g.append_only_user_entries,"enableManifest":g.enable_manifest}]),
                ))
            }
            "capture" => {
                let v = repo.capture_current_memory_snapshot()?;
                if let Some(k) = op["saveAs"].as_str() {
                    vars.insert(k.into(), v.clone());
                }
                Ok(v)
            }
            "state" => {
                repo.storage
                    .set_pipeline_state(op["key"].as_str().unwrap(), &js(&op["value"]))?;
                Ok(JsValue::Null)
            }
            "edit" => (if op["type"] == "user" {
                repo.get_global_user_store()
            } else {
                repo.get_file_memory_store()
            })
            .upsert_candidate(&obj(&candidate(
                op["type"].as_str().unwrap(),
                op["name"].as_str().unwrap(),
            ))),
            "stage" => {
                let st = repo.create_dream_stage(id)?;
                let v = json!({"snapshot":value(st.snapshot.clone()),"workspace":st.staged_workspace_root,"global":st.staged_global_root,"db":st.staged_db_path});
                stages.insert(id.into(), st);
                Ok(js(&v))
            }
            "stageEdit" => (if op["type"] == "user" {
                stages[id].repository.get_global_user_store()
            } else {
                stages[id].repository.get_file_memory_store()
            })
            .upsert_candidate(&obj(&candidate(
                op["type"].as_str().unwrap(),
                op["name"].as_str().unwrap(),
            ))),
            "stageCapture" => stages[id].repository.capture_current_memory_snapshot(),
            "closeStage" => stages[id].repository.close().map(|_| JsValue::Null),
            "swap" => repo
                .replace_live_roots_with_stage(&stages[id], &vars[op["fallback"].as_str().unwrap()])
                .map(|_| JsValue::Null),
            "dispose" => stages[id].dispose().map(|_| JsValue::Null),
            "install" => {
                let before = &vars[op["before"].as_str().unwrap()];
                let after = &vars[op["after"].as_str().unwrap()];
                let mut snap = before.clone();
                if let Some(p) = op["path"].as_str() {
                    let mut o = snap.as_object().unwrap().clone();
                    o.insert(
                        "workspaceFiles".into(),
                        js(&json!([{"relativePath":p,"content":"bad"}])),
                    );
                    snap = JsValue::Object(o);
                }
                repo.install_last_dream_snapshot(&snap, &metadata(before, after, &op["extra"]))?;
                Ok(JsValue::Null)
            }
            "overview" => repo.get_last_dream_snapshot_overview(),
            "rollback" => repo.rollback_last_dream_snapshot(),
            "clear" => repo.clear_last_dream_snapshot().map(|_| JsValue::Null),
            "raw" => {
                let p = root.join(op["path"].as_str().unwrap());
                fs::create_dir_all(p.parent().unwrap())?;
                fs::write(p, op["content"].as_str().unwrap())?;
                Ok(JsValue::Null)
            }
            "mkdir" => {
                fs::create_dir_all(root.join(op["path"].as_str().unwrap()))?;
                Ok(JsValue::Null)
            }
            "chmod" => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(
                        root.join(op["path"].as_str().unwrap()),
                        fs::Permissions::from_mode(op["mode"].as_u64().unwrap() as u32),
                    )?;
                }
                Ok(JsValue::Null)
            }
            "symlink" => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(
                    op["target"].as_str().unwrap(),
                    root.join(op["path"].as_str().unwrap()),
                )?;
                Ok(JsValue::Null)
            }
            "readlinkLexical" => Ok(JsValue::Bool(
                fs::read_link(root.join(op["path"].as_str().unwrap()))?
                    == root.join(op["target"].as_str().unwrap()),
            )),
            "sql" => {
                Connection::open(root.join("control.sqlite"))
                    .unwrap()
                    .execute_batch(op["sql"].as_str().unwrap())
                    .unwrap();
                Ok(JsValue::Null)
            }
            "second" => Ok(JsValue::Null),
            m => panic!("unknown method {m}"),
        })();
        let actual = json!({"result":clean(&match result{Ok(v)=>value(v),Err(e)=>error(e)},root),"fired":*fired.lock().unwrap(),"disk":disk(root)});
        assert_value(
            &actual,
            &case["observations"][i],
            &format!("scenario {} observation {i}: {op}", case["name"]),
        );
        observations.push(actual);
    }
    for s in stages.values() {
        let _ = s.repository.close();
    }
    if let Some(s) = second {
        s.close().unwrap();
    }
    r.close().unwrap();
    json!(observations)
}
fn seed_ts(root: &Path, case: &Value) {
    for e in case["seedDisk"].as_array().unwrap() {
        let p = root.join(e["relativePath"].as_str().unwrap());
        if e["dir"] == true {
            fs::create_dir_all(p).unwrap();
        } else if let Some(s) = e["content"].as_str() {
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, s).unwrap();
        }
    }
    fs::write(
        root.join("control.sqlite"),
        STANDARD.decode(case["database"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
}
// Oracle collation is en-US. Re-exec avoids unsafe process-wide set_var in tests.
fn in_oracle_locale(test: &str) -> bool {
    if std::env::var("PDRS_SNAPSHOT_LOCALE_CHILD").as_deref() == Ok(test) {
        return true;
    }
    let output = Command::new(std::env::current_exe().expect("#273 locale child path"))
        .args(["--exact", test, "--include-ignored", "--nocapture"])
        .env("PDRS_SNAPSHOT_LOCALE_CHILD", test)
        .env("LANG", "en_US.UTF-8")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .output()
        .expect("#273 locale child spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // #273: libtest exits 0 even when --exact matches nothing after a rename;
    // only the libtest receipt proves the locale rerun really ran the test.
    assert!(
        exact_receipt::single_pass(&output.status, test, &stdout),
        "#273 snapshot locale rerun matched no test ({test}): {status}\n{stdout}\n{stderr}",
        status = output.status
    );
    false
}
#[test]
fn ts_databases_and_trees_replay_snapshot_lifecycle() {
    if !in_oracle_locale("ts_databases_and_trees_replay_snapshot_lifecycle") {
        return;
    }
    let o = oracle();
    for c in cases(&o) {
        let s = Scratch::new();
        seed_ts(&s.0, c);
        replay(&s.0, c);
    }
}
#[test]
fn rust_seed_and_snapshot_lifecycle_match_ts() {
    if !in_oracle_locale("rust_seed_and_snapshot_lifecycle_match_ts") {
        return;
    }
    let o = oracle();
    for c in cases(&o) {
        let s = Scratch::new();
        seed(&s.0);
        assert_eq!(disk(&s.0), c["seedDisk"], "seed {}", c["name"]);
        replay(&s.0, c);
    }
}
/// #1036 fix-r1 P2-2: a failed `dispose` must not latch the `disposed` flag.
/// TS's `dispose` was repeatable cleanup, so a first attempt whose `remove`
/// fails has to stay retryable — the retry must really remove the staged
/// roots instead of returning a success that never cleaned anything. (`Drop`
/// deliberately does NOT take that retry: the `restore-dispose-failure`
/// oracle cell keeps a failed explicit dispose's residue observable, exactly
/// like TS.)
#[test]
fn failed_dispose_retry_still_removes_staged_roots() {
    let scratch = Scratch::new();
    let root = &scratch.0;
    seed(root);
    let mut r = open(root);
    // The stage captures the hook at creation, so arm the fault before it;
    // EIO on the first Remove of the staged workspace root only.
    let armed = Arc::new(AtomicBool::new(false));
    let armed_hook = armed.clone();
    r.set_snapshot_io_hook(Arc::new(move |op, p, _| {
        if op == SnapshotIoOperation::Remove
            && armed_hook.load(Ordering::SeqCst)
            && p.to_string_lossy().contains(".memory.retry-")
        {
            return Err(io::Error::from_raw_os_error(5));
        }
        Ok(())
    }));
    let stage = r
        .create_dream_stage("retry")
        .expect("#1036 dispose-retry: create the dream stage");
    let staged = [
        stage.staged_workspace_root.clone(),
        stage.staged_global_root.clone(),
        stage.staged_db_path.clone(),
    ];
    for p in &staged {
        assert!(
            p.exists(),
            "#1036 dispose-retry: the staged temp {p:?} must exist before dispose"
        );
    }
    armed.store(true, Ordering::SeqCst);
    stage
        .dispose()
        .expect_err("#1036 dispose-retry: the faulted first dispose must fail");
    armed.store(false, Ordering::SeqCst);
    stage
        .dispose()
        .expect("#1036 dispose-retry: the retried dispose must clean the staged roots");
    for p in &staged {
        assert!(
            !p.exists(),
            "#1036 dispose-retry: the retried dispose left the staged temp {p:?} behind"
        );
    }
    r.close()
        .expect("#1036 dispose-retry: close the repository");
}
fn node(script: &str, args: &[&Path]) {
    let base = std::env::var("PDRS_BASELINE").unwrap();
    let mut child = Command::new("node")
        .env("LANG", "en_US.UTF-8")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .arg("--import")
        .arg(Path::new(&base).join("node_modules/tsx/dist/loader.mjs"))
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/edgeclaw-snapshots")
                .join(script),
        )
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(55) {
            child.kill().unwrap();
            // Reap before panicking: a killed-but-unwaited child is a zombie on
            // Unix and would keep the output pipes open (issue #1036 P2-4).
            let _ = child.wait();
            panic!("Node verifier exceeded 55s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
#[test]
#[ignore = "requires real fixed TS, Node 22 and tsx"]
fn real_ts_reopens_rust_seed_and_snapshot_artifacts() {
    if !in_oracle_locale("real_ts_reopens_rust_seed_and_snapshot_artifacts") {
        return;
    }
    let o = oracle();
    let scratch = Scratch::new();
    for c in cases(&o) {
        let root = scratch.0.join(c["name"].as_str().unwrap());
        fs::create_dir_all(&root).unwrap();
        seed(&root);
    }
    let output = scratch.0.join("verify.json");
    node("verify.mts", &[&scratch.0, &output]);
    let actual: Value = serde_json::from_str(&fs::read_to_string(output).unwrap()).unwrap();
    for (i, c) in o["cases"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            actual[i], c["observations"],
            "TS reopened Rust seed {}",
            c["name"]
        );
    }
    // Re-open every final Rust artifact through genuine TS, including installed
    // rollback metadata, retained stages, and partial SQL state after triggers.
    let final_root = Scratch::new();
    let mut expected = vec![];
    for c in cases(&o) {
        let root = final_root.0.join(c["name"].as_str().unwrap());
        fs::create_dir_all(&root).unwrap();
        seed(&root);
        replay(&root, c);
        let r = open(&root);
        let snapshot = r
            .capture_current_memory_snapshot()
            .map(value)
            .unwrap_or_else(error);
        let overview = r
            .get_last_dream_snapshot_overview()
            .map(value)
            .unwrap_or_else(error);
        r.close().unwrap();
        expected.push(json!({"snapshot":clean(&snapshot,&root),"overview":clean(&overview,&root),"disk":disk(&root)}));
    }
    let output = final_root.0.join("inspect.json");
    node(
        "verify.mts",
        &[&final_root.0, &output, Path::new("inspect")],
    );
    let actual: Value = serde_json::from_str(&fs::read_to_string(output).unwrap()).unwrap();
    assert_eq!(actual, json!(expected));
}
#[test]
#[ignore = "regenerates all real TS failure scenarios"]
fn real_ts_regenerates_identically() {
    if !in_oracle_locale("real_ts_regenerates_identically") {
        return;
    }
    baseline::regenerates_identically("edgeclaw-snapshots");
}

fn signal(root: &Path, name: &str) {
    fs::write(root.join("signals").join(name), "go").unwrap();
}
fn wait_signal(root: &Path, name: &str) {
    let start = Instant::now();
    while !root.join("signals").join(name).exists() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "barrier timeout {name}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn process_worker(root: &Path, id: &str) {
    let mut r = open(root);
    signal(root, &format!("boot-{id}"));
    wait_signal(root, &format!("stage-start-{id}"));
    let root2 = root.to_path_buf();
    let id2 = id.to_string();
    let paused = Mutex::new(false);
    r.set_snapshot_io_hook(Arc::new(move |op, p, _| {
        let path = p.to_string_lossy();
        if op == SnapshotIoOperation::Mkdir && path.contains(&format!(".memory.{id2}-")) {
            let mut paused = paused.lock().unwrap();
            if !*paused {
                *paused = true;
                signal(&root2, &format!("stage-paused-{id2}"));
                wait_signal(&root2, "stage-release");
            }
        }
        if op == SnapshotIoOperation::Rename && id2 == "one" && path.contains(".memory.one-") {
            signal(&root2, "swap-paused");
            wait_signal(&root2, "swap-release");
        }
        Ok(())
    }));
    let fallback = r.capture_current_memory_snapshot().unwrap();
    let stage = r.create_dream_stage(id).unwrap();
    stage
        .repository
        .workspace_memory
        .upsert_candidate(&obj(&candidate(
            if id == "one" { "project" } else { "feedback" },
            id,
        )))
        .unwrap();
    stage.repository.close().unwrap();
    signal(root, &format!("ready-{id}"));
    wait_signal(root, &format!("swap-start-{id}"));
    let result = r
        .replace_live_roots_with_stage(&stage, &fallback)
        .map(|_| Value::Null)
        .unwrap_or_else(error);
    stage.dispose().unwrap();
    r.close().unwrap();
    fs::write(
        root.join("signals").join(format!("result-{id}")),
        result.to_string(),
    )
    .unwrap();
}
struct Children(Vec<std::process::Child>);
impl Drop for Children {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
#[test]
fn concurrent_stage_and_swap_matches_real_ts() {
    if !in_oracle_locale("concurrent_stage_and_swap_matches_real_ts") {
        return;
    }
    if let Ok(root) = std::env::var("PDRS_SNAPSHOT_WORKER_ROOT") {
        process_worker(
            Path::new(&root),
            &std::env::var("PDRS_SNAPSHOT_WORKER_ID").unwrap(),
        );
        return;
    }
    let o = oracle();
    let scratch = Scratch::new();
    let root = &scratch.0;
    seed(root);
    fs::create_dir(root.join("signals")).unwrap();
    let mut children = Children(vec![]);
    for id in ["one", "two"] {
        // #273: worker logs go to regular files under signals/ (excluded from
        // the disk comparison). Never pipes: a blocked worker cannot fill a
        // file and wedge the barrier protocol, and the log outlives the Child.
        children.0.push(
            Command::new(std::env::current_exe().expect("#273 snapshot worker path"))
                .args([
                    "--exact",
                    "concurrent_stage_and_swap_matches_real_ts",
                    "--nocapture",
                ])
                .env("PDRS_SNAPSHOT_WORKER_ROOT", root)
                .env("PDRS_SNAPSHOT_WORKER_ID", id)
                .stdout(
                    fs::File::create(root.join("signals").join(format!("worker-{id}.stdout")))
                        .expect("#273 snapshot worker log create"),
                )
                .stderr(
                    fs::File::create(root.join("signals").join(format!("worker-{id}.stderr")))
                        .expect("#273 snapshot worker log create"),
                )
                .spawn()
                .expect("#273 snapshot worker spawn"),
        );
        wait_signal(root, &format!("boot-{id}"));
    }
    signal(root, "stage-start-one");
    wait_signal(root, "stage-paused-one");
    signal(root, "stage-start-two");
    wait_signal(root, "stage-paused-two");
    signal(root, "stage-release");
    wait_signal(root, "ready-one");
    wait_signal(root, "ready-two");
    signal(root, "swap-start-one");
    wait_signal(root, "swap-paused");
    signal(root, "swap-start-two");
    wait_signal(root, "result-two");
    signal(root, "swap-release");
    wait_signal(root, "result-one");
    for (child, id) in children.0.iter_mut().zip(["one", "two"]) {
        let status = child.wait().expect("#273 snapshot worker wait");
        // A missing log reads as empty and fails here — exit 0 alone never
        // proves the worker really ran its test.
        let stdout = fs::read_to_string(root.join("signals").join(format!("worker-{id}.stdout")))
            .unwrap_or_default();
        let stderr = fs::read_to_string(root.join("signals").join(format!("worker-{id}.stderr")))
            .unwrap_or_default();
        assert!(
            exact_receipt::single_pass(
                &status,
                "concurrent_stage_and_swap_matches_real_ts",
                &stdout
            ),
            "#273 snapshot worker rerun matched no test ({id}): {status}\n{stdout}\n{stderr}"
        );
    }
    let r = open(root);
    let snapshot = clean(&value(r.capture_current_memory_snapshot().unwrap()), root);
    r.close().unwrap();
    let results = ["one", "two"].map(|id| {
        serde_json::from_str::<Value>(
            &fs::read_to_string(root.join("signals").join(format!("result-{id}"))).unwrap(),
        )
        .unwrap()
    });
    let disk = disk(root)
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| !e["relativePath"].as_str().unwrap().starts_with("signals"))
        .cloned()
        .collect::<Vec<_>>();
    assert_value(
        &json!({"results":results,"snapshot":snapshot,"disk":disk}),
        &o["concurrent"],
        "controlled two-process overlap",
    );
}
#[test]
fn concurrent_instances_and_manifest_provider_finish_without_deadlock() {
    if !in_oracle_locale("concurrent_instances_and_manifest_provider_finish_without_deadlock") {
        return;
    }
    let scratch = Scratch::new();
    seed(&scratch.0);
    let barrier = Arc::new(std::sync::Barrier::new(4));
    let (tx, rx) = std::sync::mpsc::channel();
    let mut workers = vec![];
    for id in 0..3 {
        let r = open(&scratch.0);
        let b = barrier.clone();
        let tx = tx.clone();
        workers.push(std::thread::spawn(move || {
            b.wait();
            for n in 0..16 {
                if id == 2 {
                    r.repair_workspace_manifest().unwrap();
                } else {
                    let before = r.capture_current_memory_snapshot().unwrap();
                    let stage = r.create_dream_stage(&format!("t{id}n{n}")).unwrap();
                    stage.repository.close().unwrap();
                    r.replace_live_roots_with_stage(&stage, &before).unwrap();
                    stage.dispose().unwrap();
                }
            }
            r.close().unwrap();
            tx.send(()).unwrap();
        }));
    }
    barrier.wait();
    for _ in 0..3 {
        rx.recv_timeout(Duration::from_secs(20))
            .expect("repository root lock/provider deadlock");
    }
    for w in workers {
        w.join().unwrap();
    }
    let r = open(&scratch.0);
    let snapshot = value(r.capture_current_memory_snapshot().unwrap());
    assert_eq!(snapshot["counts"]["memoryFiles"], 3);
    r.close().unwrap();
}

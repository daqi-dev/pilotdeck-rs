//! Real TS repository accessor transcript: TS .db -> Rust, and Rust -> TS.
use base64::{Engine, engine::general_purpose::STANDARD};
use pilotdeck_gateway::runtime::memory::edgeclaw::{
    EdgeClawFileStores, FileMemoryError, StorageError,
};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
#[path = "common/baseline.rs"]
mod baseline;
const ORACLE: &str = include_str!("fixtures/edgeclaw-accessors/oracle.json");
const NOW: &str = "2026-10-22T00:00:00.000Z";
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("pdrs-accessors-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn oracle() -> Value {
    baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).unwrap()
}
fn expand(v: &Value, root: &Path) -> Value {
    match v {
        Value::String(s) => json!(s.replace("$ROOT", &root.to_string_lossy())),
        Value::Array(a) => Value::Array(a.iter().map(|v| expand(v, root)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), expand(v, root)))
                .collect(),
        ),
        v => v.clone(),
    }
}
fn clean(v: &Value, root: &Path) -> Value {
    match v {
        Value::String(s) => json!(s.replace(root.to_str().unwrap(), "$ROOT")),
        Value::Array(a) => Value::Array(a.iter().map(|v| clean(v, root)).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, v)| (k.clone(), clean(v, root))).collect())
        }
        v => v.clone(),
    }
}
fn js(v: &Value) -> JsObject {
    parse_json(&v.to_string())
        .unwrap()
        .as_object()
        .unwrap()
        .clone()
}
fn value(v: JsValue) -> Value {
    if matches!(v, JsValue::Undefined) {
        Value::Null
    } else {
        serde_json::from_str(&stringify_json(&v).unwrap().unwrap()).unwrap()
    }
}
fn error(e: FileMemoryError) -> Value {
    match e {
        FileMemoryError::Storage(StorageError::Closed) => {
            json!({"error":"ERR_INVALID_STATE","message":"database is not open"})
        }
        e => panic!("unexpected repository error: {e}"),
    }
}
fn open(root: &Path, id: &str, workspace: &str) -> EdgeClawFileStores {
    EdgeClawFileStores::with_workspace_clock(
        root.join("workspaces").join(id).join("control.sqlite"),
        root.join("workspaces").join(id).join("memory"),
        root.join("global"),
        workspace,
        root.join("home/.pilotdeck"),
        Arc::new(|| NOW.into()),
    )
    .unwrap()
}
fn repos(root: &Path, o: &Value) -> HashMap<String, EdgeClawFileStores> {
    o["configs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let id = c["id"].as_str().unwrap();
            (
                id.into(),
                if c["defaults"] == true {
                    EdgeClawFileStores::open_repository(
                        root.join("workspaces").join(id).join("control.sqlite"),
                        None,
                        None,
                        None,
                    )
                    .unwrap()
                } else {
                    open(root, id, expand(&c["workspace"], root).as_str().unwrap())
                },
            )
        })
        .collect()
}
fn files(root: &Path, dir: &Path, out: &mut Vec<Value>) {
    let mut entries = fs::read_dir(dir)
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        if e.file_type().unwrap().is_dir() {
            files(root, &path, out);
        } else {
            let name = path.file_name().unwrap().to_str().unwrap();
            if ["control.sqlite", "control.sqlite-wal", "control.sqlite-shm"].contains(&name) {
                continue;
            }
            out.push(json!({"relativePath":path.strip_prefix(root).unwrap().to_str().unwrap(),"content":fs::read_to_string(&path).unwrap()}));
        }
    }
}
fn tree(root: &Path) -> Value {
    let mut out = vec![];
    files(root, root, &mut out);
    json!(out)
}
fn invoke(repos: &mut HashMap<String, EdgeClawFileStores>, op: &Value, root: &Path) -> Value {
    let id = op["repo"].as_str().unwrap_or("main");
    let r = &repos[id];
    let input = expand(op.get("input").unwrap_or(&json!({})), root);
    let method = op["method"].as_str().unwrap();
    let result: Result<JsValue, FileMemoryError> = match method {
        "close" => r.close().map(|_| JsValue::Null),
        "mode" => Ok(JsValue::from(r.get_workspace_mode())),
        "accessorIdentity" => Ok(parse_json(
            &json!([
                std::ptr::eq(r.get_file_memory_store(), r.get_file_memory_store()),
                std::ptr::eq(r.get_global_user_store(), r.get_global_user_store()),
                !std::ptr::eq(r.get_file_memory_store(), r.get_global_user_store())
            ])
            .to_string(),
        )
        .unwrap()),
        "roots" => Ok(parse_json(
            &json!([
                r.get_file_memory_store().get_root_dir(),
                r.get_global_user_store().get_root_dir()
            ])
            .to_string(),
        )
        .unwrap()),
        "workspace" => r
            .get_file_memory_store()
            .execute(op["action"].as_str().unwrap(), &js(&input)),
        "global" => r
            .get_global_user_store()
            .execute(op["action"].as_str().unwrap(), &js(&input)),
        "state" => r
            .storage
            .set_pipeline_state("workspaceDir", &parse_json(&input.to_string()).unwrap())
            .map(|_| JsValue::Null)
            .map_err(Into::into),
        "readState" => r
            .storage
            .get_pipeline_state("workspaceDir")
            .map(|v| v.unwrap_or(JsValue::Null))
            .map_err(Into::into),
        "listReadableProjectCatalog" => r.list_readable_project_catalog(),
        "project" => r.get_readable_project(input["id"].as_str().unwrap()),
        "entries" => r.list_readable_project_entries(
            input["id"].as_str().unwrap(),
            &js(input.get("options").unwrap_or(&json!({}))),
        ),
        "getUserSummary" => r.get_user_summary(),
        "listGlobalMemoryEntries" => r.list_global_memory_entries(&js(&input)),
        "repairWorkspaceManifest" => r.repair_workspace_manifest(),
        "raw" => {
            let p = root.join(input["path"].as_str().unwrap());
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, input["content"].as_str().unwrap()).unwrap();
            Ok(JsValue::Null)
        }
        "remove" => {
            fs::remove_dir_all(root.join(input["path"].as_str().unwrap())).unwrap();
            Ok(JsValue::Null)
        }
        "reopen" => {
            let _ = r.close();
            let r = open(root, id, input["workspace"].as_str().unwrap());
            repos.insert(id.into(), r);
            Ok(JsValue::Null)
        }
        _ => panic!("unknown method {method}"),
    };
    clean(
        &match result {
            Ok(v) => value(v),
            Err(e) => error(e),
        },
        root,
    )
}
fn replay(root: &Path, o: &Value) {
    let mut repos = repos(root, o);
    for (i, op) in o["operations"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            invoke(&mut repos, op, root),
            o["observations"][i],
            "observation {i}: {op}"
        );
    }
    for r in repos.values() {
        let _ = r.close();
    }
    assert_eq!(clean(&tree(root), root), o["files"], "final file tree");
}
#[test]
fn ts_databases_and_file_tree_replay_repository_accessors() {
    let o = oracle();
    let scratch = Scratch::new();
    for f in o["seedFiles"].as_array().unwrap() {
        let p = scratch.0.join(f["relativePath"].as_str().unwrap());
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, expand(&f["content"], &scratch.0).as_str().unwrap()).unwrap();
    }
    for db in o["databases"].as_array().unwrap() {
        let p = scratch
            .0
            .join("workspaces")
            .join(db["id"].as_str().unwrap())
            .join("control.sqlite");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, STANDARD.decode(db["base64"].as_str().unwrap()).unwrap()).unwrap();
    }
    replay(&scratch.0, &o);
}
fn seed_rust(root: &Path, o: &Value) {
    let mut repos = repos(root, o);
    for op in o["seed"].as_array().unwrap() {
        invoke(&mut repos, op, root);
    }
    for r in repos.values() {
        r.close().unwrap();
    }
    fs::create_dir_all(root.join("workspaces/bad/memory")).unwrap();
    fs::write(root.join("workspaces/bad/control.sqlite"), "not sqlite").unwrap();
    fs::write(root.join("workspaces/plain"), "not a directory").unwrap();
}
#[test]
fn rust_seed_and_replay_match_real_ts() {
    let o = oracle();
    let scratch = Scratch::new();
    seed_rust(&scratch.0, &o);
    let mut expected = o["seedFiles"].as_array().unwrap().clone();
    expected.retain(|v| v["relativePath"] != "workspaces/bad/control.sqlite");
    assert_eq!(
        clean(&tree(&scratch.0), &scratch.0),
        json!(expected),
        "Rust seed file tree"
    );
    replay(&scratch.0, &o);
}
#[test]
#[ignore = "requires fixed PDRS_BASELINE with Node 22 and tsx"]
fn real_ts_reads_rust_databases_and_files() {
    let o = oracle();
    let scratch = Scratch::new();
    seed_rust(&scratch.0, &o);
    let baseline = std::env::var("PDRS_BASELINE").unwrap();
    let output_dir = Scratch::new();
    let output = output_dir.0.join("verify.json");
    let root = &scratch.0;
    let mut child = Command::new("node")
        .arg("--import")
        .arg(Path::new(&baseline).join("node_modules/tsx/dist/loader.mjs"))
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/edgeclaw-accessors/verify.mts"),
        )
        .arg(root)
        .arg(&output)
        .env("PDRS_BASELINE", baseline)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(45) {
            child.kill().unwrap();
            panic!("Node verifier exceeded 45s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let actual: Value = serde_json::from_str(&fs::read_to_string(output).unwrap()).unwrap();
    assert_eq!(actual["observations"], o["observations"]);
    assert_eq!(actual["files"], o["files"]);
}

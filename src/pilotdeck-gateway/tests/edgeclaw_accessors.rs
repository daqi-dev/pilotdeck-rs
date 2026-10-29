//! Real TS repository accessor transcript: TS .db -> Rust, and Rust -> TS.
use base64::{Engine, engine::general_purpose::STANDARD};
use pilotdeck_gateway::runtime::memory::edgeclaw::{
    EdgeClawFileStores, FileMemoryError, StorageError, file_memory::FileMemoryClock,
};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
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
#[path = "common/exact_receipt.rs"]
mod exact_receipt;
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
/// `{$number:"NaN"}` transports probe values JSON cannot represent; the TS
/// generator (`observe.mts`) decodes the same sentinel before invoking TS.
fn decode_probe(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(decode_probe).collect()),
        JsValue::Object(object) => {
            let number = object
                .get("$number")
                .and_then(JsValue::as_string)
                .and_then(|kind| kind.to_utf8().ok())
                .and_then(|kind| match kind.as_str() {
                    "NaN" => Some(f64::NAN),
                    "Infinity" => Some(f64::INFINITY),
                    "-Infinity" => Some(f64::NEG_INFINITY),
                    _ => None,
                });
            if object.len() == 1
                && let Some(number) = number
            {
                return JsValue::Number(number);
            }
            let mut decoded = JsObject::new();
            for (key, value) in object.entries() {
                decoded.insert(key.clone(), decode_probe(value));
            }
            JsValue::Object(decoded)
        }
        other => other.clone(),
    }
}
fn js(v: &Value) -> JsObject {
    decode_probe(&parse_json(&v.to_string()).unwrap())
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
        FileMemoryError::Type(message) => json!({"error":"TypeError","message":message}),
        e => panic!("unexpected repository error: {e}"),
    }
}
fn open(root: &Path, id: &str, workspace: &str, global: Option<&Path>) -> EdgeClawFileStores {
    EdgeClawFileStores::with_workspace_clock(
        root.join("workspaces").join(id).join("control.sqlite"),
        root.join("workspaces").join(id).join("memory"),
        global
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.join("global")),
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
                    // Configs may point at an isolated global store (`probe`);
                    // default configs keep the shared `$ROOT/global`.
                    let global = expand(c.get("global").unwrap_or(&json!("$ROOT/global")), root);
                    open(
                        root,
                        id,
                        expand(&c["workspace"], root).as_str().unwrap(),
                        Some(Path::new(global.as_str().unwrap())),
                    )
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
            let r = open(root, id, input["workspace"].as_str().unwrap(), None);
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
    // Regular files (not pipes) mean a chatty verifier can never block on a
    // full pipe; the helper still bounds the run and kills the whole group.
    let stdout = output_dir.0.join("verify.stdout");
    let stderr = output_dir.0.join("verify.stderr");
    let mut command = Command::new("node");
    command
        .arg("--import")
        .arg(Path::new(&baseline).join("node_modules/tsx/dist/loader.mjs"))
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/edgeclaw-accessors/verify.mts"),
        )
        .arg(root)
        .arg(&output)
        .env("PDRS_BASELINE", baseline)
        .stdin(Stdio::null())
        .stdout(Stdio::from(fs::File::create(&stdout).unwrap()))
        .stderr(Stdio::from(fs::File::create(&stderr).unwrap()));
    #[cfg(unix)]
    command.process_group(0);
    let child = command.spawn().unwrap();
    let status = wait_for_process(child, &stdout, &stderr, Duration::from_secs(45))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        status.success(),
        "{}{}",
        fs::read_to_string(&stdout).unwrap(),
        fs::read_to_string(&stderr).unwrap()
    );
    let actual: Value = serde_json::from_str(&fs::read_to_string(output).unwrap()).unwrap();
    assert_eq!(actual["observations"], o["observations"]);
    assert_eq!(actual["files"], o["files"]);
}

/// Poll a child whose stdout/stderr already point at files. On timeout kill its
/// process group (so tsx/esbuild helpers die too), reap it, then report both
/// streams instead of losing them with `wait_with_output`.
fn wait_for_process(
    mut child: std::process::Child,
    stdout: &Path,
    stderr: &Path,
    limit: Duration,
) -> std::result::Result<std::process::ExitStatus, String> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Ok(status);
        }
        if start.elapsed() > limit {
            #[cfg(unix)]
            let group = rustix::process::kill_process_group(
                rustix::process::Pid::from_raw(child.id() as i32).expect("child pid"),
                rustix::process::Signal::KILL,
            );
            #[cfg(not(unix))]
            let group: std::io::Result<()> = Ok(());
            let direct = child.kill();
            let reaped = child.wait();
            return Err(format!(
                "subprocess exceeded {limit:?} (group kill {group:?}, direct kill {direct:?}, reap {reaped:?}):\n{}\n{}",
                fs::read_to_string(stdout).unwrap_or_default(),
                fs::read_to_string(stderr).unwrap_or_default()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The old verifier piped 64 KiB-bounded streams without reading them; a large
/// diagnostic dump deadlocked the child. Files keep the runner unblocked.
#[cfg(unix)]
#[test]
fn subprocess_runner_drains_output_larger_than_a_pipe_buffer() {
    let scratch = Scratch::new();
    let stdout = scratch.0.join("stdout");
    let stderr = scratch.0.join("stderr");
    let child = Command::new("sh")
        .arg("-c")
        .arg("head -c 262144 /dev/zero | tr '\\0' x; head -c 262144 /dev/zero | tr '\\0' y >&2")
        .stdin(Stdio::null())
        .stdout(Stdio::from(fs::File::create(&stdout).unwrap()))
        .stderr(Stdio::from(fs::File::create(&stderr).unwrap()))
        .spawn()
        .unwrap();
    let status = wait_for_process(child, &stdout, &stderr, Duration::from_secs(20)).unwrap();
    assert!(status.success());
    assert!(fs::metadata(&stdout).unwrap().len() >= 262144);
    assert!(fs::metadata(&stderr).unwrap().len() >= 262144);
}

/// A timeout must kill descendants, not just the `sh` leader: tsx leaves
/// esbuild helpers in the same owned group. Without the group kill the
/// background subshell survives and writes the marker.
#[cfg(unix)]
#[test]
fn subprocess_timeout_kills_the_whole_process_group() {
    let scratch = Scratch::new();
    let stdout = scratch.0.join("stdout");
    let stderr = scratch.0.join("stderr");
    let marker = scratch.0.join("survived");
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(format!("(sleep 1; : > {}) & wait", marker.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::from(fs::File::create(&stdout).unwrap()))
        .stderr(Stdio::from(fs::File::create(&stderr).unwrap()))
        .process_group(0);
    let child = command.spawn().unwrap();
    let result = wait_for_process(child, &stdout, &stderr, Duration::from_millis(200));
    assert!(result.is_err(), "unexpected exit: {result:?}");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !marker.exists(),
        "timeout left a descendant alive in the process group"
    );
}

#[cfg(target_os = "macos")]
fn system_passwd_home() -> PathBuf {
    let output = Command::new("/usr/bin/id").arg("-P").output().unwrap();
    let line = String::from_utf8(output.stdout).unwrap();
    let fields: Vec<&str> = line.trim_end().split(':').collect();
    PathBuf::from(fields[fields.len() - 2])
}

#[cfg(all(unix, not(target_os = "macos")))]
fn system_passwd_home() -> PathBuf {
    let uid = Command::new("id").arg("-u").output().unwrap();
    let uid = String::from_utf8(uid.stdout).unwrap();
    let output = Command::new("getent")
        .args(["passwd", uid.trim()])
        .output()
        .unwrap();
    let line = String::from_utf8(output.stdout).unwrap();
    PathBuf::from(line.trim_end().split(':').nth(5).unwrap())
}

/// Shared spawn for the two `home_fallback_child` rerun sites (#942 fix-r1):
/// the child is an `--ignored` test whose libtest summary is the receipt, so
/// the spawn pins `--test-threads=2` — single-threaded libtest prints
/// `test <name> ... ` before the body runs and would splice the summary under
/// an ambient RUST_TEST_THREADS=1 — and the callers assert
/// [`exact_receipt::single_pass`] instead of a bare `test result:` substring
/// that a multi-summary or ignored-count output could satisfy.
fn spawn_home_fallback_child(mode: &str, expected: &Path) -> std::process::Output {
    let mut command = Command::new(std::env::current_exe().expect("#942 homedir test binary"));
    command
        .args([
            "--exact",
            "home_fallback_child",
            "--ignored",
            "--nocapture",
            "--test-threads=2",
        ])
        .env("PDRS_HOME_FALLBACK_CHILD", mode)
        .env("PDRS_EXPECTED_GENERAL", expected);
    if mode == "unset" {
        command.env_remove("HOME");
    } else {
        command.env("HOME", "");
    }
    command.output().expect("#942 homedir child spawn")
}

#[test]
fn repository_home_fallback_matches_ts_homedir() {
    if std::env::var_os("PDRS_HOME_FALLBACK_CHILD").is_some() {
        home_fallback_child();
        return;
    }
    for (mode, expected) in [
        ("unset", system_passwd_home().join(".pilotdeck")),
        ("empty", std::env::current_dir().unwrap().join(".pilotdeck")),
    ] {
        let output = spawn_home_fallback_child(mode, &expected);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{mode} child failed: {stdout}{stderr}"
        );
        // #942: exit 0 alone is not a receipt — a renamed child makes this
        // `--exact` filter match nothing and libtest still exits 0.
        assert!(
            exact_receipt::single_pass(&output.status, "home_fallback_child", &stdout),
            "#942 homedir rerun matched no test ({mode}):\n{stdout}{stderr}"
        );
    }
}

#[test]
#[ignore = "requires fixed PDRS_BASELINE with Node 22 and tsx"]
fn real_ts_homedir_matches_rust_without_home() {
    let baseline = PathBuf::from(std::env::var_os("PDRS_BASELINE").expect("PDRS_BASELINE"));
    for mode in ["unset", "empty"] {
        let mut probe = Command::new("node");
        probe
            .arg("--import")
            .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/edgeclaw-accessors/home.mts"),
            )
            .env("PDRS_BASELINE", &baseline)
            .stdin(Stdio::null());
        if mode == "unset" {
            probe.env_remove("HOME");
        } else {
            probe.env("HOME", "");
        }
        let output = probe.output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let observed: Value = serde_json::from_slice(&output.stdout).unwrap();
        let general = observed["general"].as_str().expect("general workspace");
        if mode == "empty" {
            assert_eq!(
                observed["home"].as_str(),
                Some(""),
                "empty HOME must not fall back"
            );
        } else {
            assert_eq!(
                Path::new(general),
                system_passwd_home().join(".pilotdeck").as_path()
            );
        }
        let output = spawn_home_fallback_child(mode, Path::new(general));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{mode} Rust child disagreed with TS {general}: {stdout}{stderr}"
        );
        // #942: same receipt as the non-baseline site — exit 0 alone is not a
        // receipt, a renamed child would run zero tests and still exit 0.
        assert!(
            exact_receipt::single_pass(&output.status, "home_fallback_child", &stdout),
            "#942 homedir rerun matched no test ({mode}, TS baseline):\n{stdout}{stderr}"
        );
    }
}

#[test]
#[ignore = "spawned by repository_home_fallback_matches_ts_homedir"]
fn home_fallback_child() {
    let Ok(mode) = std::env::var("PDRS_HOME_FALLBACK_CHILD") else {
        return;
    };
    let expected = PathBuf::from(std::env::var("PDRS_EXPECTED_GENERAL").unwrap());
    let scratch = Scratch::new();
    let clock: FileMemoryClock = Arc::new(|| NOW.into());
    // Explicit general workspace: no HOME lookup at all.
    EdgeClawFileStores::with_workspace_clock(
        scratch.0.join("explicit/control.sqlite"),
        scratch.0.join("explicit/memory"),
        scratch.0.join("global"),
        scratch.0.join("workspace"),
        scratch.0.join("explicit/general"),
        clock.clone(),
    )
    .unwrap()
    .close()
    .unwrap();
    // Default constructor: succeeds through the same homedir resolution Node
    // uses (HOME wins, then passwd), including an empty HOME.
    EdgeClawFileStores::with_clock(
        scratch.0.join("default/control.sqlite"),
        scratch.0.join("default/memory"),
        scratch.0.join("global"),
        "single",
        clock,
    )
    .unwrap()
    .close()
    .unwrap();
    let stores = EdgeClawFileStores::open_repository(
        scratch.0.join("repo/control.sqlite"),
        None,
        Some(&scratch.0.join("global")),
        Some(&expected),
    )
    .unwrap();
    assert_eq!(stores.get_workspace_mode(), "general", "{mode}");
    stores.close().unwrap();
}

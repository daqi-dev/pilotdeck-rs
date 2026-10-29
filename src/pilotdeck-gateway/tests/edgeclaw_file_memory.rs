//! Real TS FileMemoryStore transcript and disk-tree replay (cc7c4e6).
use pilotdeck_gateway::runtime::memory::edgeclaw::{
    EdgeClawFileStores, FileMemoryError, FileMemoryStore, FileMemoryStoreOptions,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Barrier},
    time::{Duration, Instant},
};
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/node_provenance.rs"]
mod node_provenance;
const ORACLE: &str = include_str!("fixtures/edgeclaw-file-memory/oracle.json");
const NOW: &str = "2026-10-22T00:00:00.000Z";
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("pdrs-file-memory-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        fn writable(path: &Path) {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = fs::symlink_metadata(path)
                && meta.is_dir()
            {
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
                if let Ok(entries) = fs::read_dir(path) {
                    for entry in entries.flatten() {
                        writable(&entry.path());
                    }
                }
            }
        }
        writable(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn field<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    v.get(k).unwrap_or_else(|| panic!("missing {k}"))
}
fn arr(v: &JsValue) -> &[JsValue] {
    if let JsValue::Array(a) = v {
        a
    } else {
        panic!("not array {v:?}")
    }
}
fn txt(v: &JsValue) -> String {
    String::from_utf16_lossy(v.as_string().expect("string").units())
}
fn text(o: &JsObject, k: &str) -> String {
    o.get(k).map(txt).unwrap_or_default()
}
fn obj(fields: &[(&str, JsValue)]) -> JsObject {
    let mut o = JsObject::new();
    for (k, v) in fields {
        o.insert((*k).into(), v.clone());
    }
    o
}
fn string(v: &str) -> JsValue {
    JsValue::from(v)
}
fn options(v: &JsValue) -> FileMemoryStoreOptions {
    let o = v.as_object().unwrap();
    let mut options = FileMemoryStoreOptions::default();
    if let Some(v) = o.get("workspaceMode") {
        options.workspace_mode = txt(v);
    }
    for (key, target) in [
        ("manageProjectMeta", &mut options.manage_project_meta),
        ("manageProjectFiles", &mut options.manage_project_files),
        ("manageUserProfile", &mut options.manage_user_profile),
        (
            "appendOnlyUserEntries",
            &mut options.append_only_user_entries,
        ),
        ("enableManifest", &mut options.enable_manifest),
    ] {
        if let Some(JsValue::Bool(v)) = o.get(key) {
            *target = *v;
        }
    }
    for (key, target) in [
        (
            "userProfileRelativePath",
            &mut options.user_profile_relative_path,
        ),
        ("userNotesRelativeDir", &mut options.user_notes_relative_dir),
    ] {
        if let Some(v) = o.get(key) {
            *target = v.as_string().map(|_| txt(v));
        }
    }
    options
}
fn clean(v: JsValue, root: &Path) -> JsValue {
    match &v {
        JsValue::String(s) => {
            let root = JsString::from(root.to_string_lossy().as_ref());
            let mut out = vec![];
            let mut index = 0;
            while index < s.units().len() {
                if s.units()[index..].starts_with(root.units()) {
                    out.extend("$ROOT".encode_utf16());
                    index += root.units().len();
                } else {
                    out.push(s.units()[index]);
                    index += 1;
                }
            }
            JsValue::String(JsString::from_utf16(out))
        }
        JsValue::Array(a) => JsValue::Array(a.iter().map(|v| clean(v.clone(), root)).collect()),
        JsValue::Object(o) => {
            let mut out = JsObject::new();
            for (k, v) in o.entries() {
                out.insert(k.clone(), clean(v.clone(), root));
            }
            JsValue::Object(out)
        }
        v => v.clone(),
    }
}
fn references(v: &JsValue, vars: &HashMap<String, JsValue>) -> JsValue {
    match &v {
        JsValue::String(s) if s.units().first() == Some(&u16::from(b'@')) => {
            let name = txt(v);
            let mut parts = name[1..].split('.');
            let mut value = vars.get(parts.next().unwrap()).expect("reference");
            for key in parts {
                value = field(value, key);
            }
            value.clone()
        }
        JsValue::Array(a) => JsValue::Array(a.iter().map(|v| references(v, vars)).collect()),
        JsValue::Object(o) => {
            let mut out = JsObject::new();
            for (k, v) in o.entries() {
                out.insert(k.clone(), references(v, vars));
            }
            JsValue::Object(out)
        }
        _ => v.clone(),
    }
}
fn error(e: FileMemoryError) -> JsValue {
    let io_code = e.io_code();
    match e {
        FileMemoryError::Io(e) => {
            let code = io_code.unwrap_or_else(|| panic!("unhandled IO error {e}"));
            JsValue::Object(obj(&[("error", string(code))]))
        }
        e => JsValue::Object(obj(&[
            ("error", string(e.js_name())),
            ("message", JsValue::String(e.js_message())),
        ])),
    }
}
fn equal(a: &JsValue, b: &JsValue) -> bool {
    match (a, b) {
        (JsValue::Object(a), JsValue::Object(b)) => {
            a.len() == b.len()
                && a.entries()
                    .iter()
                    .all(|(k, v)| b.get_key(k).is_some_and(|other| equal(v, other)))
        }
        (JsValue::Array(a), JsValue::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
        }
        _ => a == b,
    }
}
fn assert_json(a: &JsValue, b: &JsValue, label: &str) {
    assert!(
        equal(a, b),
        "{label}\nactual: {}\nexpected: {}",
        stringify_json(a).unwrap().unwrap(),
        stringify_json(b).unwrap().unwrap()
    );
}
fn tree(root: &Path, dir: &Path) -> Vec<JsValue> {
    let mut paths = fs::read_dir(root.join(dir))
        .unwrap()
        .map(|e| e.unwrap())
        .collect::<Vec<_>>();
    paths.sort_by_key(|e| {
        e.file_name()
            .to_string_lossy()
            .encode_utf16()
            .collect::<Vec<_>>()
    });
    let mut result = vec![];
    for e in paths {
        let path = dir.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            result.extend(tree(root, &path));
        } else {
            result.push(JsValue::Object(obj(&[
                ("relativePath", string(&path.to_string_lossy())),
                (
                    "content",
                    string(&String::from_utf8_lossy(&fs::read(e.path()).unwrap())),
                ),
            ])));
        }
    }
    result
}
fn external(root: &Path, method: &str, input: &JsObject) -> Option<JsValue> {
    let path = root.join(text(input, "relativePath"));
    match method {
        "writeRaw" => {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text(input, "content")).unwrap();
        }
        "removeDir" => {
            if path.exists() {
                fs::remove_dir_all(path).unwrap();
            }
        }
        "chmod" => {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                path,
                fs::Permissions::from_mode(input.get("mode").unwrap().as_number().unwrap() as u32),
            )
            .unwrap();
        }
        _ => return None,
    }
    Some(JsValue::Null)
}
fn run_scenario(scenario: &JsValue, root: &Path) -> FileMemoryStore {
    let name = txt(field(scenario, "name"));
    let store = FileMemoryStore::with_clock(
        root,
        options(field(scenario, "options")),
        Arc::new(|| NOW.into()),
    )
    .unwrap();
    let mut vars = HashMap::new();
    for (i, (op, expected)) in arr(field(scenario, "operations"))
        .iter()
        .zip(arr(field(scenario, "observations")))
        .enumerate()
    {
        let method = txt(field(op, "method"));
        let input = references(field(op, "input"), &vars);
        let input = input.as_object().unwrap();
        let result = if let Some(v) = external(root, &method, input) {
            v
        } else {
            store.execute(&method, input).unwrap_or_else(error)
        };
        if let Some(key) = op.get("saveAs") {
            vars.insert(txt(key), result.clone());
        }
        assert_json(
            &clean(result, root),
            expected,
            &format!("{name} operation {i} {method}"),
        );
    }
    assert_json(
        &clean(readback(&store), root),
        field(scenario, "readback"),
        &format!("{name} readback"),
    );
    assert_json(
        &JsValue::Array(tree(root, Path::new(""))),
        field(scenario, "files"),
        &format!("{name} disk bytes"),
    );
    store
}
fn readback(store: &FileMemoryStore) -> JsValue {
    let entries = store
        .list_memory_entries(&obj(&[
            (
                "kinds",
                JsValue::Array(
                    ["user", "project", "feedback", "general_project_meta"]
                        .iter()
                        .map(|v| string(v))
                        .collect(),
                ),
            ),
            ("includeDeprecated", JsValue::Bool(true)),
            ("limit", JsValue::Number(5000.0)),
        ]))
        .unwrap();
    let ids = JsValue::Array(
        arr(&entries)
            .iter()
            .map(|e| field(e, "relativePath").clone())
            .collect(),
    );
    JsValue::Object(obj(&[
        ("rootDir", string(&store.get_root_dir().to_string_lossy())),
        ("workspaceMode", string(store.get_workspace_mode())),
        ("generalMode", JsValue::Bool(store.is_general_mode())),
        (
            "profilePath",
            store
                .get_user_profile_relative_path()
                .map(string)
                .unwrap_or(JsValue::Null),
        ),
        ("entries", entries),
        (
            "records",
            store
                .get_memory_records_by_ids(&obj(&[
                    ("ids", ids),
                    ("maxLines", JsValue::Number(5000.0)),
                ]))
                .unwrap(),
        ),
        ("summary", store.get_user_summary().unwrap()),
        ("metas", store.list_project_metas().unwrap()),
        (
            "hints",
            store.list_project_identity_hints(&JsObject::new()).unwrap(),
        ),
        (
            "overview",
            store
                .get_overview(&obj(&[("lastDreamAt", string("2026-10-20"))]))
                .unwrap(),
        ),
        ("bundle", store.export_bundle_records().unwrap()),
        (
            "version",
            store
                .get_snapshot_version(&obj(&[("lastDreamAt", string("2026-10-20"))]))
                .unwrap(),
        ),
        ("snapshot", store.export_snapshot_files().unwrap()),
    ]))
}
#[test]
fn rust_reads_and_writes_the_real_ts_transcripts() {
    let oracle = baseline::oracle(ORACLE);
    let scratch = Scratch::new();
    for scenario in arr(field(&oracle, "scenarios")) {
        run_scenario(scenario, &scratch.0.join(txt(field(scenario, "name"))));
    }
}
#[test]
fn rust_reads_ts_written_files_and_repairs_corrupt_manifest() {
    let oracle = baseline::oracle(ORACLE);
    let scratch = Scratch::new();
    let scenario = &arr(field(&oracle, "scenarios"))[0];
    for file in arr(field(scenario, "files")) {
        let path = scratch.0.join(txt(field(file, "relativePath")));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, txt(field(file, "content"))).unwrap();
    }
    let store = FileMemoryStore::with_clock(
        &scratch.0,
        FileMemoryStoreOptions::default(),
        Arc::new(|| NOW.into()),
    )
    .unwrap();
    assert_json(
        &clean(readback(&store), &scratch.0),
        field(scenario, "readback"),
        "Rust readback of TS files",
    );
    let expected = store.export_bundle_records().unwrap();
    fs::write(scratch.0.join("MEMORY.md"), "broken manifest").unwrap();
    let repair = store.repair_manifests().unwrap();
    assert_eq!(field(&repair, "changed").as_number(), Some(1.0));
    assert_json(
        &store.export_bundle_records().unwrap(),
        &expected,
        "manifest is derived from files",
    );
    // All bytes except the deliberately corrupt manifest match the TS tree.
    assert_json(
        &JsValue::Array(tree(&scratch.0, Path::new(""))),
        field(scenario, "files"),
        "TS files read/repair",
    );
}
#[test]
fn concurrent_writes_are_serialized_without_lost_manifest_entries() {
    concurrency_case(false);
}
#[test]
fn independent_stores_on_the_same_root_serialize_writes() {
    concurrency_case(true);
}
fn concurrency_case(independent: bool) {
    let scratch = Scratch::new();
    let store = Arc::new(
        FileMemoryStore::with_clock(
            &scratch.0,
            FileMemoryStoreOptions {
                manage_project_meta: false,
                ..Default::default()
            },
            Arc::new(|| NOW.into()),
        )
        .unwrap(),
    );
    let start = Arc::new(Barrier::new(12));
    let handles = (0..12)
        .map(|i| {
            let store = if independent {
                Arc::new(
                    FileMemoryStore::with_clock(
                        &scratch.0,
                        FileMemoryStoreOptions {
                            manage_project_meta: false,
                            ..Default::default()
                        },
                        Arc::new(|| NOW.into()),
                    )
                    .unwrap(),
                )
            } else {
                store.clone()
            };
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                store
                    .upsert_candidate(&obj(&[
                        ("type", string("project")),
                        ("scope", string("project")),
                        ("name", string(&format!("并发{i}😀"))),
                        ("description", string("test")),
                        ("sourceSessionKey", string(&format!("s{i}"))),
                        ("capturedAt", string(NOW)),
                    ]))
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let mut paths = vec![];
    for h in handles {
        paths.push(txt(field(&h.join().unwrap(), "relativePath")));
    }
    paths.sort();
    paths.dedup();
    assert_eq!(paths.len(), 12);
    let entries = store.list_memory_entries(&JsObject::new()).unwrap();
    assert_eq!(arr(&entries).len(), 12);
    let manifest = fs::read_to_string(scratch.0.join("MEMORY.md")).unwrap();
    for path in paths {
        assert!(manifest.contains(&path), "lost {path}");
    }
    let oracle = baseline::oracle(ORACLE);
    let scenario = arr(field(&oracle, "scenarios"))
        .iter()
        .find(|s| txt(field(s, "name")) == "concurrent")
        .unwrap();
    assert_json(
        &JsValue::Array(tree(&scratch.0, Path::new(""))),
        field(scenario, "files"),
        "concurrent writes versus sequential TS bytes",
    );
}
#[test]
fn sqlite_and_global_file_stores_share_the_ts_layout_and_links() {
    let scratch = Scratch::new();
    let repo = EdgeClawFileStores::with_clock(
        scratch.0.join("db.sqlite"),
        scratch.0.join("workspace/memory"),
        scratch.0.join("global"),
        "single",
        Arc::new(|| NOW.into()),
    )
    .unwrap();
    let candidate = obj(&[
        ("type", string("user")),
        ("scope", string("global")),
        ("name", string("User😀")),
        ("description", string("身份")),
        ("profile", string("Rust 工程师")),
        ("capturedAt", string(NOW)),
        ("sourceSessionKey", string("s")),
    ]);
    let oracle = baseline::oracle(ORACLE);
    assert_json(
        &JsValue::Object(candidate.clone()),
        field(field(&oracle, "wiring"), "candidate"),
        "wiring input",
    );
    let user = repo
        .global_user_memory
        .upsert_candidate(&candidate)
        .unwrap();
    repo.workspace_memory.repair_manifests().unwrap();
    let manifest = fs::read_to_string(scratch.0.join("workspace/memory/MEMORY.md")).unwrap();
    let path = txt(field(&user, "relativePath"));
    assert!(
        manifest.contains(&format!("../../global/{path}")),
        "{manifest}"
    );
    assert!(!scratch.0.join("global/MEMORY.md").exists());
    assert_json(
        &clean(readback(&repo.global_user_memory), &scratch.0),
        field(field(&oracle, "wiring"), "global"),
        "TS global store wiring",
    );
    assert_json(
        &clean(readback(&repo.workspace_memory), &scratch.0),
        field(field(&oracle, "wiring"), "workspace"),
        "TS workspace provider wiring",
    );
    let actual_files = tree(&scratch.0, Path::new(""))
        .into_iter()
        .filter(|v| {
            let p = txt(field(v, "relativePath"));
            p.starts_with("global/") || p.starts_with("workspace/")
        })
        .collect();
    assert_json(
        &JsValue::Array(actual_files),
        field(field(&oracle, "wiring"), "files"),
        "TS wired file bytes",
    );
    repo.storage
        .set_pipeline_state("file-memory-check", &user)
        .unwrap();
    assert_json(
        &repo
            .storage
            .get_pipeline_state("file-memory-check")
            .unwrap()
            .unwrap(),
        &user,
        "public storage API",
    );
    drop(repo);
    let reopen = EdgeClawFileStores::with_clock(
        scratch.0.join("db.sqlite"),
        scratch.0.join("workspace/memory"),
        scratch.0.join("global"),
        "single",
        Arc::new(|| NOW.into()),
    )
    .unwrap();
    assert_eq!(
        arr(&reopen
            .global_user_memory
            .list_memory_entries(&JsObject::new())
            .unwrap())
        .len(),
        1
    );
    assert_json(
        &reopen
            .storage
            .get_pipeline_state("file-memory-check")
            .unwrap()
            .unwrap(),
        &user,
        "reopened public storage API",
    );
}
#[test]
fn same_root_provider_reentry_finishes_like_synchronous_ts() {
    // Keep all disk ownership in the worker: a deadlock must fail this test
    // within five seconds, without joining the stuck thread or deleting its files.
    let (finished, completion) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let scratch = Scratch::new();
        let root = scratch.0.join("memory");
        let oracle = baseline::oracle(ORACLE);
        let expected = field(&oracle, "sameRootWiring");
        let repo = EdgeClawFileStores::with_clock(
            scratch.0.join("db.sqlite"),
            &root,
            &root,
            "single",
            Arc::new(|| NOW.into()),
        )
        .unwrap();
        repo.global_user_memory
            .upsert_candidate(
                field(field(&oracle, "wiring"), "candidate")
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        repo.workspace_memory
            .upsert_candidate(field(expected, "candidate").as_object().unwrap())
            .unwrap();
        repo.workspace_memory.repair_manifests().unwrap();
        assert_json(
            &clean(readback(&repo.global_user_memory), &root),
            field(expected, "global"),
            "TS same-root global readback",
        );
        assert_json(
            &clean(readback(&repo.workspace_memory), &root),
            field(expected, "workspace"),
            "TS same-root workspace readback",
        );
        assert_json(
            &JsValue::Array(tree(&root, Path::new(""))),
            field(expected, "files"),
            "TS same-root file bytes",
        );
        finished.send(()).unwrap();
    });
    completion
        .recv_timeout(Duration::from_secs(5))
        .expect("same-root provider operation must finish within 5s");
    worker.join().unwrap();
}
#[test]
#[ignore = "requires fixed TS baseline / Node 22; real TS reopens Rust files"]
fn real_ts_reads_rust_written_files() {
    let oracle = baseline::oracle(ORACLE);
    let scratch = Scratch::new();
    for scenario in arr(field(&oracle, "scenarios")) {
        run_scenario(scenario, &scratch.0.join(txt(field(scenario, "name"))));
    }
    let baseline = PathBuf::from(std::env::var_os("PDRS_BASELINE").expect("PDRS_BASELINE"));
    let mut command = node_provenance::pin(baseline::node_command());
    command
        .arg("--import")
        .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/edgeclaw-file-memory/verify.mts"),
        )
        .arg(&scratch.0)
        .env("PDRS_BASELINE", baseline);
    run_node(&mut command, &scratch.0);
}
#[test]
#[ignore = "requires fixed TS baseline and Node v22.22.0"]
fn real_ts_generator_is_reproducible() {
    let _ = baseline::oracle(ORACLE);
    let _ = node_provenance::pin(baseline::node_command());
    baseline::regenerates_identically("edgeclaw-file-memory");
}
#[test]
#[ignore = "requires fixed TS baseline and Node v22.22.0"]
fn real_ts_write_fixture_is_reproducible() {
    let oracle = include_str!("fixtures/edgeclaw-memory-write/oracle.json");
    let _ = baseline::oracle(oracle);
    let _ = node_provenance::pin(baseline::node_command());
    baseline::regenerates_identically("edgeclaw-memory-write");
}

fn run_node(command: &mut Command, scratch: &Path) {
    let stdout = scratch.join("node.stdout");
    let stderr = scratch.join("node.stderr");
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(fs::File::create(&stdout).unwrap()))
        .stderr(Stdio::from(fs::File::create(&stderr).unwrap()))
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "Node exceeded 30s: {}{}",
                fs::read_to_string(&stdout).unwrap(),
                fs::read_to_string(&stderr).unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = fs::read_to_string(stdout).unwrap();
    let stderr = fs::read_to_string(stderr).unwrap();
    assert!(status.success(), "{stdout}{stderr}");
    println!("{stdout}{stderr}");
}

#[test]
fn concurrent_same_origin_keeps_one_record_like_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let single = arr(field(&oracle, "scenarios"))
        .iter()
        .find(|s| txt(field(s, "name")) == "origin")
        .unwrap();
    let mut ts_paths = std::collections::HashSet::new();
    for observation in arr(field(single, "observations")) {
        ts_paths.insert(txt(field(observation, "relativePath")));
    }
    assert!(!ts_paths.is_empty());
    let scratch = Scratch::new();
    let barrier = Arc::new(Barrier::new(16));
    let handles = (0..16)
        .map(|i| {
            let store = FileMemoryStore::with_clock(
                &scratch.0,
                FileMemoryStoreOptions {
                    manage_project_meta: false,
                    ..Default::default()
                },
                Arc::new(|| NOW.into()),
            )
            .unwrap();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .upsert_candidate(&obj(&[
                        ("type", string("project")),
                        ("scope", string("project")),
                        ("name", string(&format!("Origin {i}"))),
                        ("description", string("same origin")),
                        ("capturedAt", string(NOW)),
                        ("sourceSessionKey", string("same-session")),
                    ]))
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let mut rust_paths = std::collections::HashSet::new();
    for h in handles {
        rust_paths.insert(txt(field(&h.join().unwrap(), "relativePath")));
    }
    assert_eq!(
        rust_paths.len(),
        ts_paths.len(),
        "same-origin concurrent writes created extra files"
    );
    let store = FileMemoryStore::with_clock(
        &scratch.0,
        FileMemoryStoreOptions {
            manage_project_meta: false,
            ..Default::default()
        },
        Arc::new(|| NOW.into()),
    )
    .unwrap();
    assert_eq!(
        arr(&store.list_memory_entries(&JsObject::new()).unwrap()).len(),
        ts_paths.len()
    );
}

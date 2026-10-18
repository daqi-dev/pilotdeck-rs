//! WorkspaceSnapshot storage-contract tests. Scenario outcomes and snapshot
//! trees come from the real TS recorder/provider via
//! tests/fixtures/workspace-snapshot/generate.mts (see that README): the Rust
//! side replays every step against its own scratch copy of the workspace and
//! compares outcomes, restore trees, and full snapshot-root trees item by
//! item. Only the recorder's own top-level `createdAt` is masked (`<ts>`,
//! matched by the scenario's `dynamicIds`); a marker's `error` path has the
//! scenario directory replaced (`<dir>`), and `<cwd>` masks the process cwd in
//! env-config cases. Dates in failureReason, entries paths, linkTargets,
//! hand-planted manifests and restored user `.json` files stay raw.
//!
//! The reverse cross-check (`--verify-rust`) is an ignored test that spawns
//! the real TS provider over a snapshot root this test writes; it needs
//! PDRS_BASELINE and Node/tsx, like the run-registry fixture.
use base64::Engine as _;
use pilotdeck_gateway::storage::{
    ContentAddressedWorkspaceSnapshotRecorder, SnapshotState, WorkspaceSnapshotConfig,
    WorkspaceSnapshotDescriptor, WorkspaceSnapshotError, WorkspaceSnapshotInput,
    WorkspaceSnapshotResult, create_workspace_snapshot_provider, resolve_workspace_snapshot_config,
};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs,
    os::unix::{ffi::OsStringExt, fs::symlink},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/workspace-snapshot"
);

fn oracle() -> JsValue {
    let path = format!("{FIXTURES}/oracle.json");
    let Ok(text) = fs::read_to_string(&path) else {
        panic!(
            "oracle.json is missing; generate it first (tests/fixtures/workspace-snapshot/README.md):\n\
             PDRS_BASELINE=/absolute/path/to/PilotDeck node --import \"$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs\" \
             {FIXTURES}/generate.mts {FIXTURES}"
        );
    };
    assert!(
        !text.contains(".generate-"),
        "oracle.json leaks a generation path"
    );
    parse_json(&text).unwrap()
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let raw = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../_tmp/20261018-a66-pr3/test-scratch")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        // Lexically normalized (no `..`): the recorder resolves the snapshot
        // root and workspace dir like Node's path.resolve, and the replay's
        // masked-path comparisons assume the scenario directory is already in
        // resolved form.
        let mut dir = PathBuf::new();
        for component in raw.components() {
            match component {
                Component::ParentDir => {
                    dir.pop();
                }
                Component::CurDir => {}
                other => dir.push(other.as_os_str()),
            }
        }
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ------------------------------------------------------------- JsValue tools

static UNDEFINED: JsValue = JsValue::Undefined;
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn s(value: &JsValue) -> String {
    value
        .as_string()
        .expect("expected a string")
        .to_utf8()
        .unwrap()
}
fn arr(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(items) = value {
        items
    } else {
        &[]
    }
}
fn strs(value: &JsValue) -> Vec<String> {
    arr(value).iter().map(s).collect()
}
fn object(pairs: &[(&str, JsValue)]) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in pairs {
        out.insert((*key).into(), value.clone());
    }
    JsValue::Object(out)
}
fn decode(text: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .unwrap()
}
/// Both sides go through the same codec, so stringify equality is a faithful
/// deep comparison (numbers are f64 on both, key order matches the shapes the
/// generator records). Only the recorder-written `createdAt` of a snapshot in
/// `dynamic` is run-varying: it is masked by snapshotId on both sides, while
/// every other field (failureReason, entries paths, linkTarget, hand-planted
/// fixed dates, user `.json` content) stays raw.
fn assert_same(actual: &JsValue, expected: &JsValue, context: &str, dynamic: &HashSet<String>) {
    let actual = stringify_json(&mask_runtime_created_at(actual, dynamic));
    let expected = stringify_json(&mask_runtime_created_at(expected, dynamic));
    assert_eq!(actual, expected, "{context}");
}
fn mask_runtime_created_at(value: &JsValue, dynamic: &HashSet<String>) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(
            items
                .iter()
                .map(|item| mask_runtime_created_at(item, dynamic))
                .collect(),
        ),
        JsValue::Object(source) => {
            let mask_created_at = source
                .get("snapshotId")
                .and_then(JsValue::as_string)
                .and_then(|id| id.to_utf8().ok())
                .is_some_and(|id| dynamic.contains(&id));
            let mut out = JsObject::new();
            for (key, item) in source.entries() {
                if mask_created_at && key.equals_str("createdAt") && item.as_string().is_some() {
                    out.insert(key.clone(), JsValue::from("<ts>"));
                } else {
                    out.insert(key.clone(), mask_runtime_created_at(item, dynamic));
                }
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

// ------------------------------------------------------------ disk comparison

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
/// The bytes a tree row's size and sha256 both describe. Only recorder-written
/// manifest/marker partitions in `dynamic` are projected: the top-level
/// `createdAt` becomes `<ts>`, and a marker's `error` value has the scenario
/// directory replaced (it embeds the run-specific workspace path). Hand-planted
/// manifests, fixed dates in any field, and user `.json` files stay raw;
/// generate.mts `projectArtifact` projects identically.
fn tree_bytes<'a>(
    rel: &str,
    bytes: &'a [u8],
    dir: &str,
    dynamic: &HashSet<String>,
) -> Cow<'a, [u8]> {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    if name != "manifest.json" && name != "_FAILED.json" {
        return Cow::Borrowed(bytes);
    }
    let partition = rel.rsplit('/').nth(1).unwrap_or("");
    if !dynamic.contains(partition) {
        return Cow::Borrowed(bytes);
    }
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let (body, newline) = match line.strip_suffix('\n') {
            Some(body) => (body, "\n"),
            None => (line, ""),
        };
        if let Some(rest) = body.strip_prefix("  \"createdAt\": \"")
            && let Some(end) = rest.rfind('"')
        {
            out.push_str("  \"createdAt\": \"<ts>");
            out.push_str(&rest[end..]);
            out.push_str(newline);
            continue;
        }
        if name == "_FAILED.json" && body.starts_with("  \"error\": ") {
            out.push_str(&body.replace(dir, "<dir>"));
            out.push_str(newline);
            continue;
        }
        out.push_str(line);
    }
    Cow::Owned(out.into_bytes())
}
/// generate.mts recordTree: lstat semantics (symlinks are rows, not followed),
/// dirs recurse, rows sorted by path in UTF-16 order (the JS `<` the generator
/// sorts with). A restore target tree is user content: pass an empty `dynamic`
/// so nothing is projected.
fn record_tree(root: &Path, dir: &str, dynamic: &HashSet<String>) -> Vec<JsValue> {
    let mut rows: Vec<(String, JsValue)> = Vec::new();
    walk_tree(root, root, dir, dynamic, &mut rows);
    rows.sort_by(|left, right| utf16_cmp(&left.0, &right.0));
    rows.into_iter().map(|(_, row)| row).collect()
}
fn walk_tree(
    root: &Path,
    directory: &Path,
    dir: &str,
    dynamic: &HashSet<String>,
    rows: &mut Vec<(String, JsValue)>,
) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = directory.join(entry.file_name());
        let Ok(info) = fs::symlink_metadata(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let file_type = info.file_type();
        if file_type.is_symlink() {
            let target = fs::read_link(&path).unwrap().to_string_lossy().into_owned();
            rows.push((
                rel.clone(),
                object(&[
                    ("path", JsValue::from(rel.as_str())),
                    ("kind", JsValue::from("symlink")),
                    ("target", JsValue::from(target.as_str())),
                ]),
            ));
        } else if file_type.is_dir() {
            rows.push((
                rel.clone(),
                object(&[
                    ("path", JsValue::from(rel.as_str())),
                    ("kind", JsValue::from("dir")),
                ]),
            ));
            walk_tree(root, &path, dir, dynamic, rows);
        } else {
            let bytes = fs::read(&path).unwrap();
            let projected = tree_bytes(&rel, &bytes, dir, dynamic);
            let digest = sha256_hex(&projected);
            rows.push((
                rel.clone(),
                object(&[
                    ("path", JsValue::from(rel.as_str())),
                    ("kind", JsValue::from("file")),
                    ("size", JsValue::Number(projected.len() as f64)),
                    ("sha256", JsValue::from(digest.as_str())),
                ]),
            ));
        }
    }
}
fn utf16_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}
/// Byte-exact non-UTF-8 names need a byte-transparent filesystem; generate.mts
/// probes identically and drops the scenario where this fails (APFS/HFS+
/// reject the bytes with EINVAL, Linux allows them).
fn non_utf8_names_writable(dir: &Path) -> bool {
    let probe = dir.join(OsString::from_vec(b"probe-\xff".to_vec()));
    let writable = fs::write(&probe, b"").is_ok();
    let _ = fs::remove_file(&probe);
    writable
}

fn no_temporaries(directory: &Path) {
    for entry in fs::read_dir(directory).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        assert!(!name.contains(".tmp-"), "leftover temporary {name}");
        let path = directory.join(entry.file_name());
        if fs::symlink_metadata(&path)
            .map(|info| info.is_dir())
            .unwrap_or(false)
        {
            no_temporaries(&path);
        }
    }
}
/// generate.mts tick: wait for the millisecond to change so consecutive
/// captures get comparable, strictly newer createdAt values.
fn tick() {
    let millis = || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
    };
    let at = millis();
    while millis() == at {
        thread::sleep(Duration::from_micros(100));
    }
}

// ------------------------------------------------------------ outcome shapes

fn result_json(result: &WorkspaceSnapshotResult, mask: &dyn Fn(&str) -> String) -> JsValue {
    let mut out = JsObject::new();
    out.insert("snapshotId".into(), result.snapshot_id.as_str().into());
    out.insert("phase".into(), result.phase.as_str().into());
    out.insert("state".into(), result.state.as_str().into());
    if let Some(path) = &result.manifest_path {
        out.insert("manifestPath".into(), mask(path).as_str().into());
    }
    if let Some(path) = &result.failure_marker_path {
        out.insert("failureMarkerPath".into(), mask(path).as_str().into());
    }
    if let Some(error) = &result.error {
        out.insert("error".into(), mask(error).as_str().into());
    }
    JsValue::Object(out)
}
fn thrown_json(error: &WorkspaceSnapshotError, mask: &dyn Fn(&str) -> String) -> JsValue {
    let message = mask(&error.to_string());
    let name = match error {
        WorkspaceSnapshotError::Syntax(_) => "SyntaxError",
        WorkspaceSnapshotError::Invalid(message)
            if message.starts_with("Cannot read properties of null") =>
        {
            "TypeError"
        }
        _ => "Error",
    };
    object(&[(
        "error",
        object(&[
            ("name", JsValue::from(name)),
            ("message", JsValue::from(message.as_str())),
        ]),
    )])
}
/// The list outcome records descriptors with manifestPath relative to the
/// snapshot root (generate.mts does the same relative() mapping).
fn descriptor_json(descriptor: &WorkspaceSnapshotDescriptor, root: &Path) -> JsValue {
    let mut out = JsObject::new();
    out.insert("snapshotId".into(), descriptor.snapshot_id.as_str().into());
    out.insert(
        "workspaceId".into(),
        descriptor.workspace_id.as_str().into(),
    );
    out.insert("sessionId".into(), descriptor.session_id.as_str().into());
    out.insert("turnId".into(), descriptor.turn_id.as_str().into());
    out.insert("runId".into(), descriptor.run_id.as_str().into());
    out.insert("phase".into(), descriptor.phase.as_str().into());
    out.insert("state".into(), SnapshotState::Committed.as_str().into());
    if let Some(created) = &descriptor.created_at {
        out.insert("createdAt".into(), JsValue::String(created.clone()));
    }
    let manifest_rel = Path::new(&descriptor.manifest_path)
        .strip_prefix(root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    out.insert("manifestPath".into(), manifest_rel.as_str().into());
    JsValue::Object(out)
}
fn input_of(raw: &JsValue, dir: &Path) -> WorkspaceSnapshotInput {
    let text = |key: &str| s(field(raw, key));
    let workspace_dir = {
        let value = text("workspaceDir").replace("<dir>", &dir.to_string_lossy());
        if value.starts_with('/') {
            value
        } else {
            dir.join(value).to_string_lossy().into_owned()
        }
    };
    WorkspaceSnapshotInput {
        workspace_id: text("workspaceId"),
        session_id: text("sessionId"),
        turn_id: text("turnId"),
        run_id: text("runId"),
        workspace_dir,
        workspace_stable: field(raw, "workspaceStable").as_bool(),
        failure_kind: field(raw, "failureKind")
            .as_string()
            .map(|value| value.to_utf8().unwrap()),
        failure_reason: field(raw, "failureReason")
            .as_string()
            .map(|value| value.to_utf8().unwrap()),
    }
}
fn aliased_id(step: &JsValue, ids: &HashMap<String, String>, context: &str) -> String {
    let raw = s(field(step, "snapshotId"));
    if let Some(alias) = raw.strip_prefix('$') {
        ids.get(alias)
            .cloned()
            .unwrap_or_else(|| panic!("{context}: unknown id alias {raw}"))
    } else {
        raw
    }
}
/// Snapshot-root steps address either a root-relative path or a name inside a
/// previously captured failure-marker partition (the generator's markerOf).
fn snapshot_path(step: &JsValue, root: &Path, marker_dirs: &HashMap<String, String>) -> PathBuf {
    let base = step
        .get("markerOf")
        .map(|alias| marker_dirs[&s(alias)].clone())
        .unwrap_or_default();
    let rel = step
        .get("path")
        .map(s)
        .unwrap_or_else(|| s(field(step, "name")));
    root.join(if base.is_empty() {
        rel
    } else {
        format!("{base}/{rel}")
    })
}

// ------------------------------------------------------------------- replay

#[test]
fn scenarios_match_ts_outcomes_and_trees() {
    let oracle = oracle();
    for scenario in arr(field(&oracle, "scenarios")) {
        replay_scenario(scenario);
    }
}

fn replay_scenario(scenario: &JsValue) {
    let name = s(field(scenario, "name"));
    let scratch = Scratch::new();
    let dir = scratch.0.clone();
    let workspace_dir = dir.join("workspace");
    fs::create_dir_all(&workspace_dir).unwrap();

    let spec = field(scenario, "workspace");
    for file in arr(field(spec, "files")) {
        let path = workspace_dir.join(s(field(file, "path")));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, decode(&s(field(file, "b64")))).unwrap();
    }
    for empty in strs(field(spec, "emptyDirs")) {
        fs::create_dir_all(workspace_dir.join(empty)).unwrap();
    }
    for link in arr(field(spec, "symlinks")) {
        let path = workspace_dir.join(s(field(link, "path")));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(s(field(link, "target")), &path).unwrap();
    }
    let raw_files = arr(field(spec, "rawFiles"));
    if !raw_files.is_empty() && !non_utf8_names_writable(&dir) {
        // generate.mts probes the same way and omits the scenario where the
        // filesystem rejects the bytes (macOS APFS/HFS+ EINVAL); an oracle
        // generated on a byte-transparent filesystem skips locally.
        eprintln!("scenario {name}: skipped, filesystem rejects non-UTF-8 file names");
        return;
    }
    for raw in raw_files {
        // Byte-exact non-UTF-8 file names.
        let file_name = OsString::from_vec(decode(&s(field(raw, "nameB64"))));
        fs::write(
            dir.join(s(field(raw, "dir"))).join(file_name),
            decode(&s(field(raw, "b64"))),
        )
        .unwrap();
    }

    let dir_text = dir.to_string_lossy().into_owned();
    let mask = |value: &str| value.replace(&dir_text, "<dir>");
    let root = PathBuf::from(s(field(scenario, "snapshotRoot")).replace("<dir>", &dir_text));
    let root_text = root.to_string_lossy().into_owned();
    let config = WorkspaceSnapshotConfig {
        root: root_text.clone(),
        snapshot_root: root_text,
    };
    let resolve_step_path = |value: &str| -> PathBuf {
        let value = value.replace("<dir>", &dir_text);
        if value.starts_with('/') {
            PathBuf::from(value)
        } else {
            dir.join(value)
        }
    };
    let mut recorders = vec![ContentAddressedWorkspaceSnapshotRecorder::new(
        config.clone(),
    )];
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut marker_dirs: HashMap<String, String> = HashMap::new();
    let dynamic: HashSet<String> = strs(field(scenario, "dynamicIds")).into_iter().collect();

    let steps = arr(field(scenario, "steps"));
    for (index, step) in steps.iter().enumerate() {
        let op = s(field(step, "op"));
        let context = format!("{name} step {index} ({op})");
        let mut outcome: Option<JsValue> = None;
        let mut target_tree: Option<Vec<JsValue>> = None;
        match op.as_str() {
            "tick" => tick(),
            "recorder" => recorders.push(ContentAddressedWorkspaceSnapshotRecorder::new(
                config.clone(),
            )),
            "writeFile" => {
                let path = resolve_step_path(&s(field(step, "path")));
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, decode(&s(field(step, "b64")))).unwrap();
            }
            "mkdir" => {
                fs::create_dir_all(resolve_step_path(&s(field(step, "path")))).unwrap();
            }
            "remove" => {
                let path = resolve_step_path(&s(field(step, "path")));
                let removed = if field(step, "recursive").as_bool().unwrap_or(false) {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_file(&path)
                };
                match removed {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => panic!("{context}: {e}"),
                }
            }
            "writeSnapshot" => {
                let path = snapshot_path(step, &root, &marker_dirs);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, s(field(step, "text"))).unwrap();
            }
            "mkdirSnapshot" => {
                fs::create_dir_all(root.join(s(field(step, "path")))).unwrap();
            }
            "symlinkSnapshot" => {
                let path = root.join(s(field(step, "path")));
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                if fs::symlink_metadata(&path).is_ok() {
                    let _ = fs::remove_dir_all(&path);
                    let _ = fs::remove_file(&path);
                }
                symlink(s(field(step, "target")), &path).unwrap();
            }
            "removeSnapshot" => {
                let path = snapshot_path(step, &root, &marker_dirs);
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => panic!("{context}: {e}"),
                }
            }
            "capture" => {
                let input = input_of(field(step, "input"), &dir);
                // generate.mts defaults to the latest recorder: 'recorder'
                // steps opt the next capture into fresh state.
                let current = field(step, "recorder")
                    .as_number()
                    .map(|number| number as usize)
                    .unwrap_or(recorders.len() - 1);
                let captured = if s(field(step, "phase")) == "pre_user" {
                    recorders[current].capture_pre_user(&input)
                } else {
                    recorders[current].capture_post_agent(&input)
                };
                outcome = Some(match captured {
                    Ok(result) => {
                        if let Some(alias) = step.get("as") {
                            let alias = s(alias);
                            ids.insert(alias.clone(), result.snapshot_id.clone());
                            if let Some(marker) = &result.failure_marker_path {
                                let rel = Path::new(marker).strip_prefix(&root).unwrap();
                                marker_dirs.insert(
                                    alias,
                                    rel.parent().unwrap().to_string_lossy().into_owned(),
                                );
                            }
                        }
                        result_json(&result, &mask)
                    }
                    Err(error) => thrown_json(&error, &mask),
                });
            }
            "list" => {
                outcome = Some(
                    match create_workspace_snapshot_provider(config.clone()).list() {
                        Ok(descriptors) => JsValue::Array(
                            descriptors
                                .iter()
                                .map(|descriptor| descriptor_json(descriptor, &root))
                                .collect(),
                        ),
                        Err(error) => thrown_json(&error, &mask),
                    },
                );
            }
            "get" => {
                let snapshot_id = aliased_id(step, &ids, &context);
                outcome = Some(
                    match create_workspace_snapshot_provider(config.clone()).get(&snapshot_id) {
                        Ok(Some(value)) => object(&[("value", value.clone())]),
                        Ok(None) => object(&[("value", JsValue::Null)]),
                        Err(error) => thrown_json(&error, &mask),
                    },
                );
            }
            "restore" => {
                let snapshot_id = aliased_id(step, &ids, &context);
                let target = dir.join(s(field(step, "target")));
                outcome = Some(
                    match create_workspace_snapshot_provider(config.clone())
                        .restore(&snapshot_id, &target.to_string_lossy())
                    {
                        Ok(restored) => object(&[
                            ("restored", JsValue::Bool(restored.restored)),
                            (
                                "workspaceKey",
                                JsValue::from(mask(&restored.workspace_key).as_str()),
                            ),
                        ]),
                        Err(error) => thrown_json(&error, &mask),
                    },
                );
                target_tree = Some(record_tree(&target, &dir_text, &HashSet::new()));
            }
            other => panic!("{context}: unknown op {other}"),
        }
        if let Some(expected) = step.get("outcome") {
            let actual = outcome
                .as_ref()
                .unwrap_or_else(|| panic!("{context}: rust produced no outcome"));
            assert_same(actual, expected, &context, &dynamic);
        } else if outcome.is_some() {
            panic!("{context}: rust produced an outcome but the oracle recorded none");
        }
        if let Some(expected) = step.get("targetTree") {
            let actual = target_tree
                .as_ref()
                .unwrap_or_else(|| panic!("{context}: rust recorded no target tree"));
            assert_same(
                &JsValue::Array(actual.clone()),
                expected,
                &context,
                &dynamic,
            );
        }
    }

    let tree = record_tree(&root, &dir_text, &dynamic);
    assert_same(
        &JsValue::Array(tree),
        field(scenario, "tree"),
        &format!("{name} snapshot tree"),
        &dynamic,
    );
    no_temporaries(&dir);
}

// ------------------------------------------------------------ env config

#[test]
fn config_env_table_matches_ts() {
    let oracle = oracle();
    let cwd = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    for case in arr(field(&oracle, "configCases")) {
        let mut env = HashMap::new();
        if let Some(table) = field(case, "env").as_object() {
            for (key, value) in table.entries() {
                env.insert(key.to_utf8().unwrap(), s(value));
            }
        }
        let resolved = resolve_workspace_snapshot_config(|key| env.get(key).cloned())
            .expect("env config never fails here");
        let expected = field(case, "value");
        match resolved {
            None => {
                let text = stringify_json(expected)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "<no JSON text>".to_owned());
                assert!(
                    matches!(expected, JsValue::Null),
                    "config case expected a value: {text}"
                );
            }
            Some(config) => assert_same(
                &object(&[
                    (
                        "root",
                        JsValue::from(config.root.replace(&cwd, "<cwd>").as_str()),
                    ),
                    (
                        "snapshotRoot",
                        JsValue::from(config.snapshot_root.replace(&cwd, "<cwd>").as_str()),
                    ),
                ]),
                expected,
                "config env case",
                &HashSet::new(),
            ),
        }
    }
}

// ------------------------------------------------- forward cross: Rust reads TS

/// The oracle ships the exact tree TS wrote for the normal scenario; the Rust
/// provider must list/get/restore it, object digests must match contents, and
/// a Rust recorder must honor the TS `_COMMITTED` marker (early exit, no
/// rescan).
#[test]
fn rust_reads_ts_written_snapshots() {
    let oracle = oracle();
    let cross = field(&oracle, "cross");
    let scratch = Scratch::new();
    let root = scratch.0.join("ts-written");
    for dir in strs(field(cross, "dirs")) {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    for file in arr(field(cross, "files")) {
        let path = root.join(s(field(file, "path")));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, decode(&s(field(file, "b64")))).unwrap();
    }
    let root_text = root.to_string_lossy().into_owned();
    let config = WorkspaceSnapshotConfig {
        root: root_text.clone(),
        snapshot_root: root_text,
    };
    let provider = create_workspace_snapshot_provider(config.clone());

    let list = provider.list().unwrap();
    let listed = JsValue::Array(list.iter().map(|d| descriptor_json(d, &root)).collect());
    assert_same(
        &listed,
        field(cross, "listValue"),
        "cross list",
        &HashSet::new(),
    );

    let pre = s(field(cross, "preId"));
    let value = provider.get(&pre).unwrap().expect("pre manifest");
    assert_same(
        &value,
        field(cross, "getValue"),
        "cross get",
        &HashSet::new(),
    );
    for entry in arr(field(&value, "entries")) {
        if s(field(entry, "entryType")) == "file" {
            let bytes = fs::read(root.join(s(field(entry, "objectKey")))).unwrap();
            assert_eq!(
                field(entry, "size").as_number().unwrap(),
                bytes.len() as f64,
                "object size for {}",
                s(field(entry, "path"))
            );
            assert_eq!(sha256_hex(&bytes), s(field(entry, "digest")));
        }
    }

    let target = scratch.0.join("rust-restore");
    let restored = provider
        .restore(&pre, &target.to_string_lossy())
        .expect("restore pre");
    assert!(restored.restored);
    assert_eq!(restored.workspace_key, target.to_string_lossy());
    assert_same(
        &JsValue::Array(record_tree(
            &target,
            &scratch.0.to_string_lossy(),
            &HashSet::new(),
        )),
        field(cross, "restoreTargetTree"),
        "cross restore tree",
        &HashSet::new(),
    );

    // The same identity must early-exit on the TS _COMMITTED marker.
    let input = input_of(field(cross, "input"), &scratch.0);
    let result = ContentAddressedWorkspaceSnapshotRecorder::new(config)
        .capture_pre_user(&input)
        .unwrap();
    assert_eq!(result.snapshot_id, pre);
    assert_eq!(result.state, SnapshotState::Committed);
    assert!(result.failure_marker_path.is_none());
    assert!(result.error.is_none());
    let expected = arr(field(cross, "listValue"))
        .iter()
        .find(|descriptor| s(field(descriptor, "snapshotId")) == pre)
        .expect("pre descriptor");
    let expected_path = root.join(s(field(expected, "manifestPath")));
    assert_eq!(
        result.manifest_path,
        Some(expected_path.to_string_lossy().into_owned())
    );
    no_temporaries(&scratch.0);
}

// ------------------------------------------- reverse cross: TS verifies Rust

#[test]
#[ignore = "requires fixed TS baseline and Node/tsx; run with PDRS_BASELINE"]
fn real_ts_verifies_rust_written_snapshots() {
    let baseline = std::env::var("PDRS_BASELINE").expect("set fixed baseline path");
    let scratch = Scratch::new();
    let workspace = scratch.0.join("workspace");
    fs::create_dir_all(workspace.join("nested")).unwrap();
    fs::write(workspace.join("a.txt"), "alpha\n").unwrap();
    fs::write(workspace.join("nested/b.txt"), "beta 中文\n").unwrap();
    symlink("a.txt", workspace.join("link")).unwrap();

    let root = scratch.0.join("snapshots");
    let root_text = root.to_string_lossy().into_owned();
    let config = WorkspaceSnapshotConfig {
        root: root_text.clone(),
        snapshot_root: root_text,
    };
    let recorder = ContentAddressedWorkspaceSnapshotRecorder::new(config);
    let base = |turn: &str| WorkspaceSnapshotInput {
        workspace_id: "ws-rust".into(),
        session_id: "sess-rust".into(),
        turn_id: turn.into(),
        run_id: "run-rust".into(),
        workspace_dir: workspace.to_string_lossy().into_owned(),
        workspace_stable: Some(true),
        failure_kind: None,
        failure_reason: None,
    };
    let pre = recorder.capture_pre_user(&base("turn-1")).unwrap();
    assert_eq!(pre.state, SnapshotState::Committed);
    tick();
    let post = recorder
        .capture_post_agent(&WorkspaceSnapshotInput {
            failure_kind: Some("agent_error".into()),
            failure_reason: Some("line1\n\"quoted\" 中文 end".into()),
            ..base("turn-1")
        })
        .unwrap();
    assert_eq!(post.state, SnapshotState::Committed);
    assert_ne!(pre.snapshot_id, post.snapshot_id);
    drop(recorder);

    let output = std::process::Command::new("node")
        .arg("--import")
        .arg(format!("{baseline}/node_modules/tsx/dist/loader.mjs"))
        .arg(format!("{FIXTURES}/generate.mts"))
        .arg(&root)
        .arg("--verify-rust")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("{stdout}");
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "TS --verify-rust failed");
    assert!(stdout.contains("PASS"), "missing PASS marker");
    no_temporaries(&scratch.0);
}

/// Negative control for the projection rule itself: only the recorder's
/// top-level `createdAt` (and a marker's `error` path) may change. Dates in
/// failureReason, entries paths and linkTargets must survive, and a partition
/// the recorder did not write is never projected.
#[test]
fn tree_projection_only_masks_runtime_created_at() {
    let dir = "/tmp/ws-snapshot-scenario";
    let dynamic: HashSet<String> = HashSet::from(["abc".to_string()]);
    let manifest = concat!(
        "{\n",
        "  \"snapshotId\": \"x\",\n",
        "  \"failureReason\": \"failed 2024-05-06T07:08:09.010Z\",\n",
        "  \"entries\": [\n",
        "    {\n",
        "      \"path\": \"dated-2023-04-05T06:07:08.009Z.txt\",\n",
        "      \"linkTarget\": \"target-2022-03-04T05:06:07.008Z\"\n",
        "    }\n",
        "  ],\n",
        "  \"createdAt\": \"2026-01-01T00:00:00.000Z\"\n",
        "}"
    );
    let rel = "workspaces/w/sessions/s/snapshots/abc/manifest.json";
    let projected = tree_bytes(rel, manifest.as_bytes(), dir, &dynamic);
    let text = std::str::from_utf8(&projected).unwrap();
    assert!(text.contains("\"createdAt\": \"<ts>\""));
    for date in [
        "2024-05-06T07:08:09.010Z",
        "2023-04-05T06:07:08.009Z",
        "2022-03-04T05:06:07.008Z",
    ] {
        assert!(text.contains(date), "{date} must stay raw\n{text}");
    }
    let hand_planted = rel.replace("/abc/", "/manual/");
    assert_eq!(
        tree_bytes(&hand_planted, manifest.as_bytes(), dir, &dynamic).as_ref(),
        manifest.as_bytes()
    );

    let marker = concat!(
        "{\n",
        "  \"failureReason\": \"/tmp/ws-snapshot-scenario/keep\",\n",
        "  \"error\": \"ENOENT: no such file or directory, scandir '/tmp/ws-snapshot-scenario/x'\",\n",
        "  \"createdAt\": \"2026-01-01T00:00:00.000Z\"\n",
        "}"
    );
    let marker_rel = "workspaces/w/sessions/s/snapshots/abc/_FAILED.json";
    let projected = tree_bytes(marker_rel, marker.as_bytes(), dir, &dynamic);
    let text = std::str::from_utf8(&projected).unwrap();
    assert!(text.contains("\"failureReason\": \"/tmp/ws-snapshot-scenario/keep\""));
    assert!(text.contains("scandir '<dir>/x'"));
}

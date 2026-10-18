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
#[path = "common/baseline.rs"]
mod baseline;

use base64::Engine as _;
use pilotdeck_gateway::storage::{
    ContentAddressedWorkspaceSnapshotRecorder, SnapshotState, WorkspaceSnapshotConfig,
    WorkspaceSnapshotDescriptor, WorkspaceSnapshotError, WorkspaceSnapshotInput,
    WorkspaceSnapshotResult, create_workspace_snapshot_provider, resolve_workspace_snapshot_config,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs, io,
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
    // Every scenario path is a per-run mkdtemp directory masked as `<dir>`.
    for fixed in ["/tmp/pdrs-r9-", "/tmp/ws-snapshot-scenario", "/tmp/pdrs-"] {
        assert!(
            !text.contains(fixed),
            "oracle.json keeps a fixed absolute path: {fixed}"
        );
    }
    baseline::oracle(&text)
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        // mkdtemp semantics under the short /tmp prefix: create_dir is
        // exclusive, so the directory this guard recursively removes on Drop
        // is proven to be the one we made. The short path also keeps Node's
        // ERR_INVALID_ARG_VALUE quoting (128 UTF-16 units) identical to the
        // generator's masked `<dir>` text.
        let base = PathBuf::from("/tmp");
        fs::create_dir_all(&base).unwrap();
        loop {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .subsec_nanos();
            let token = u64::from(nanos)
                ^ (u64::from(std::process::id()) << 20)
                ^ (NEXT.fetch_add(1, Ordering::Relaxed) as u64).wrapping_mul(0x9e37_79b9);
            let token = format!("{token:06x}");
            // Same 20-character shape as Node's `/tmp/pdrs-r10-` mkdtemp, so
            // the masked `<dir>` path and Node's 128-unit quote limit agree.
            let dir = base.join(format!("pdrs-r10-{}", &token[token.len() - 6..]));
            match fs::create_dir(&dir) {
                Ok(()) => return Self(dir),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create scratch {}: {error}", dir.display()),
            }
        }
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = remove_owned(&self.0, &self.0, true);
    }
}

/// Lexical ownership check: every delete may only target the per-run tree that
/// owns it. The old fixed-path P0 removed `/tmp/pdrs-r9-*` while every sentinel
/// lived inside a per-run tree, so those sentinels could not observe the
/// regression (round-4 P1). This check runs before the filesystem is touched,
/// which makes it independent of where a sentinel lives.
fn outside_run_error(root: &Path, path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "remove refused: {} is outside the run directory {}",
            path.display(),
            root.display()
        ),
    )
}

fn owned_path<'a>(root: &Path, path: &'a Path) -> io::Result<&'a Path> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| outside_run_error(root, path))?;
    let mut depth = 0usize;
    for component in relative.components() {
        match component {
            Component::ParentDir => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| outside_run_error(root, path))?;
            }
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => {
                return Err(outside_run_error(root, path));
            }
        }
    }
    Ok(path)
}

/// The `remove` hook is injectable so the negative control can prove a refused
/// delete never reaches the filesystem (`remove_owned_with` with a recorder).
fn remove_owned_with<F>(root: &Path, path: &Path, recursive: bool, remove: F) -> io::Result<()>
where
    F: FnOnce(&Path, bool) -> io::Result<()>,
{
    owned_path(root, path)?;
    remove(path, recursive)
}

fn remove_owned(root: &Path, path: &Path, recursive: bool) -> io::Result<()> {
    remove_owned_with(root, path, recursive, |path, recursive| {
        if recursive {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
    })
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
/// reject the bytes with EINVAL, Linux allows them). The committed oracle has
/// no non-utf8 scenario because it was generated on macOS, and CI never
/// regenerates the fixtures: this path is currently uncovered. Linux
/// generation is pending #283.
fn non_utf8_names_writable(dir: &Path) -> bool {
    let probe = dir.join(OsString::from_vec(b"probe-\xff".to_vec()));
    let writable = fs::write(&probe, b"").is_ok();
    let _ = remove_owned(dir, &probe, false);
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

/// Replace the scenario directory inside a raw UTF-16 string without decoding
/// it: a marker error may carry lone surrogates that `to_utf8()` rejects.
fn mask_js_string(value: &JsString, dir: &str) -> JsString {
    let needle: Vec<u16> = dir.encode_utf16().collect();
    let replacement: Vec<u16> = "<dir>".encode_utf16().collect();
    let units = value.units();
    if needle.is_empty() {
        return value.clone();
    }
    let mut out = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        if units[index..].starts_with(&needle) {
            out.extend_from_slice(&replacement);
            index += needle.len();
        } else {
            out.push(units[index]);
            index += 1;
        }
    }
    JsString::from_utf16(out)
}

fn result_json(
    result: &WorkspaceSnapshotResult,
    mask: &dyn Fn(&str) -> String,
    dir: &str,
) -> JsValue {
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
        out.insert("error".into(), JsValue::String(mask_js_string(error, dir)));
    }
    JsValue::Object(out)
}
fn thrown_json(
    error: &WorkspaceSnapshotError,
    mask: &dyn Fn(&str) -> String,
    dir: &str,
) -> JsValue {
    let name = match error {
        WorkspaceSnapshotError::Syntax(_) => "SyntaxError",
        WorkspaceSnapshotError::TypeError(_) => "TypeError",
        WorkspaceSnapshotError::Invalid(message)
            if message.starts_with("Cannot read properties of null") =>
        {
            "TypeError"
        }
        _ => "Error",
    };
    // A validatePath message quotes the raw argument: mask the run directory
    // without decoding, or a lone surrogate next to the NUL turns into U+FFFD.
    let message = match error {
        WorkspaceSnapshotError::TypeError(message) => JsValue::String(mask_js_string(message, dir)),
        _ => JsValue::from(mask(&error.to_string()).as_str()),
    };
    object(&[(
        "error",
        object(&[("name", JsValue::from(name)), ("message", message)]),
    )])
}
/// The list outcome records descriptors with manifestPath relative to the
/// snapshot root (generate.mts does the same relative() mapping).
fn descriptor_json(descriptor: &WorkspaceSnapshotDescriptor, root: &Path) -> JsValue {
    let mut out = JsObject::new();
    out.insert(
        "snapshotId".into(),
        JsValue::String(descriptor.snapshot_id.clone()),
    );
    out.insert(
        "workspaceId".into(),
        JsValue::String(descriptor.workspace_id.clone()),
    );
    out.insert(
        "sessionId".into(),
        JsValue::String(descriptor.session_id.clone()),
    );
    out.insert("turnId".into(), JsValue::String(descriptor.turn_id.clone()));
    out.insert("runId".into(), JsValue::String(descriptor.run_id.clone()));
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
    let path = root.join(if base.is_empty() {
        rel
    } else {
        format!("{base}/{rel}")
    });
    owned_path(root, &path).expect("snapshot step path stays under its root");
    path
}

// ------------------------------------------------------------------- replay

#[test]
fn scenarios_match_ts_outcomes_and_trees() {
    let oracle = oracle();
    // Secondary sentinel check: pre-place the old fixed names in a per-run
    // directory. On its own this cannot see the legacy `/tmp/pdrs-r9-*`
    // delete (round-4 P1); `out_of_bounds_removal_is_refused_and_sentinel_survives`
    // below is the load-bearing ownership negative control.
    let guard = Scratch::new();
    let sentinels: Vec<PathBuf> = [
        "pdrs-r9-linkout",
        "pdrs-r9-path",
        "pdrs-r9-target",
        "ws-snapshot-scenario",
    ]
    .into_iter()
    .map(|name| {
        let dir = guard.0.join(name);
        fs::create_dir_all(&dir).unwrap();
        let sentinel = dir.join("sentinel");
        fs::write(&sentinel, b"keep\n").unwrap();
        sentinel
    })
    .collect();
    for scenario in arr(field(&oracle, "scenarios")) {
        replay_scenario(scenario);
    }
    for sentinel in &sentinels {
        assert_eq!(
            fs::read(sentinel).unwrap(),
            b"keep\n",
            "run removed an unowned sentinel {}",
            sentinel.display()
        );
    }
}

/// Round-4 P1 negative control: injecting the legacy fixed-path delete must be
/// refused before the filesystem is touched, and a sentinel that actually sits
/// outside the guarded run tree must survive the attempt. The old sentinel
/// test above could not see that regression because both it and the replay had
/// their own per-run trees.
#[test]
fn out_of_bounds_removal_is_refused_and_sentinel_survives() {
    let run = Scratch::new();
    let outside = Scratch::new();
    let sentinel = outside.0.join("pdrs-r9-linkout");
    fs::write(&sentinel, b"keep\n").unwrap();

    // 1. The exact historical target: a recording stub proves no removal call
    //    is made; only the ownership check runs.
    let mut reached = Vec::new();
    let error = remove_owned_with(
        &run.0,
        Path::new("/tmp/pdrs-r9-linkout"),
        true,
        |path, _| {
            reached.push(path.to_path_buf());
            Ok(())
        },
    )
    .expect_err("the legacy fixed path is outside this run's tree");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(
        error.to_string().contains("outside the run directory"),
        "{error}"
    );
    assert!(
        reached.is_empty(),
        "a refused delete must never reach the filesystem: {reached:?}"
    );

    // 2. A real outside sentinel through the real remover: refused, and the
    //    file is still readable afterwards.
    let error = remove_owned(&run.0, &sentinel, false)
        .expect_err("an out-of-tree sentinel must be refused");
    assert!(
        error.to_string().contains("outside the run directory"),
        "{error}"
    );
    assert_eq!(
        fs::read(&sentinel).unwrap(),
        b"keep\n",
        "the out-of-tree sentinel survived the refused delete"
    );
}

/// R19 host boundary for R9's first P1: Node keeps distinct weights for
/// unpaired surrogates (the recorded TS list is the review's B,A case), while
/// ICU4X folds them to U+FFFD. The port must refuse the comparison instead of
/// silently returning a different order.
#[test]
fn unpaired_surrogate_collation_requires_host() {
    let oracle = oracle();
    let case = field(
        field(&oracle, "hostBoundaryCases"),
        "unpairedSurrogateCollation",
    );
    let scratch = Scratch::new();
    let root = scratch.0.join("surrogate-sort-root");
    for manifest in arr(field(case, "manifests")) {
        let path = root.join(s(field(manifest, "path")));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, s(field(manifest, "text"))).unwrap();
    }
    let ts_order: Vec<String> = arr(field(case, "tsList"))
        .iter()
        .map(|descriptor| s(field(descriptor, "snapshotId")))
        .collect();
    assert_eq!(
        ts_order,
        ["b-replacement", "c-pua", "a-lone-high", "d-supplementary"],
        "real TS keeps the lone-surrogate weight (review's B before A)"
    );
    let root_text = root.to_string_lossy().into_owned();
    let error = create_workspace_snapshot_provider(WorkspaceSnapshotConfig {
        root: root_text.clone(),
        snapshot_root: root_text,
    })
    .list()
    .expect_err("an unpaired surrogate sorting key must be a host boundary");
    assert!(error.requires_host(), "callers must be able to forward it");
    assert_eq!(
        error.to_string(),
        "Snapshot createdAt collation requires the Node host for unpaired surrogates."
    );

    // Positive control: a single descriptor never calls the comparator, so its
    // lone-surrogate createdAt still lists (the boundary is exact, not a ban).
    let single = scratch.0.join("single-surrogate");
    let first = &arr(field(case, "manifests"))[0];
    let path = single.join(s(field(first, "path")));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, s(field(first, "text"))).unwrap();
    let single_text = single.to_string_lossy().into_owned();
    let listed = create_workspace_snapshot_provider(WorkspaceSnapshotConfig {
        root: single_text.clone(),
        snapshot_root: single_text,
    })
    .list()
    .expect("one descriptor never compares sorting keys");
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].created_at.as_ref().unwrap().units(),
        [0xd800],
        "the lone surrogate survives the list read-back"
    );
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
        // generate.mts probes the same way. The committed oracle was generated
        // on macOS, where APFS/HFS+ reject the bytes and the scenario is
        // dropped; CI never regenerates the fixtures, so this path currently
        // has no coverage. Linux generation is pending #283.
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
        let path = if value.starts_with('/') {
            PathBuf::from(value)
        } else {
            dir.join(value)
        };
        owned_path(&dir, &path)
            .unwrap_or_else(|error| panic!("step path outside the run directory: {error}"));
        path
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
                let removed = remove_owned(
                    &dir,
                    &path,
                    field(step, "recursive").as_bool().unwrap_or(false),
                );
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
                    let _ = remove_owned(&root, &path, true);
                    let _ = remove_owned(&root, &path, false);
                }
                symlink(s(field(step, "target")), &path).unwrap();
            }
            "removeSnapshot" => {
                let path = snapshot_path(step, &root, &marker_dirs);
                match remove_owned(&root, &path, false) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => panic!("{context}: {e}"),
                }
            }
            "capture" => {
                let input = input_of(field(step, "input"), &dir);
                // generate.mts defaults to the latest recorder: 'recorder'
                // steps opt the next capture into fresh state. A step-level
                // snapshotRoot rebuilds a recorder with that (NUL) config.
                let current = field(step, "recorder")
                    .as_number()
                    .map(|number| number as usize)
                    .unwrap_or(recorders.len() - 1);
                let captured = if field(step, "snapshotRoot").as_string().is_some() {
                    let root = s(field(step, "snapshotRoot")).replace("<dir>", &dir_text);
                    let config = WorkspaceSnapshotConfig {
                        root: root.clone(),
                        snapshot_root: root,
                    };
                    let recorder = ContentAddressedWorkspaceSnapshotRecorder::new(config);
                    if s(field(step, "phase")) == "pre_user" {
                        recorder.capture_pre_user(&input)
                    } else {
                        recorder.capture_post_agent(&input)
                    }
                } else if s(field(step, "phase")) == "pre_user" {
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
                        result_json(&result, &mask, &dir_text)
                    }
                    Err(error) => thrown_json(&error, &mask, &dir_text),
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
                        Err(error) => thrown_json(&error, &mask, &dir_text),
                    },
                );
            }
            "get" => {
                let snapshot_id = aliased_id(step, &ids, &context);
                outcome = Some(
                    match create_workspace_snapshot_provider(config.clone()).get(&snapshot_id) {
                        Ok(Some(value)) => object(&[("value", value.clone())]),
                        Ok(None) => object(&[("value", JsValue::Null)]),
                        Err(error) => thrown_json(&error, &mask, &dir_text),
                    },
                );
            }
            "restore" => {
                let snapshot_id = aliased_id(step, &ids, &context);
                let target = resolve_step_path(&s(field(step, "target")));
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
                        Err(error) => thrown_json(&error, &mask, &dir_text),
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
    // Per-run directory text: the projection rule replaces exactly the path it
    // is given, so a fixed /tmp prefix would test a different string than the
    // one the replay uses.
    let scratch = Scratch::new();
    let dir = scratch.0.to_string_lossy().into_owned();
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
    let projected = tree_bytes(rel, manifest.as_bytes(), &dir, &dynamic);
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
        tree_bytes(&hand_planted, manifest.as_bytes(), &dir, &dynamic).as_ref(),
        manifest.as_bytes()
    );

    let marker = format!(
        concat!(
            "{{\n",
            "  \"failureReason\": \"{dir}/keep\",\n",
            "  \"error\": \"ENOENT: no such file or directory, scandir '{dir}/x'\",\n",
            "  \"createdAt\": \"2026-01-01T00:00:00.000Z\"\n",
            "}}"
        ),
        dir = dir
    );
    let marker_rel = "workspaces/w/sessions/s/snapshots/abc/_FAILED.json";
    let projected = tree_bytes(marker_rel, marker.as_bytes(), &dir, &dynamic);
    let text = std::str::from_utf8(&projected).unwrap();
    assert!(text.contains(&format!("\"failureReason\": \"{dir}/keep\"")));
    assert!(text.contains("scandir '<dir>/x'"));
}

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
#[path = "common/scratch.rs"]
mod scratch;

use base64::Engine as _;
use pilotdeck_gateway::storage::{
    ContentAddressedWorkspaceSnapshotRecorder, SnapshotRoundHook, SnapshotRoundStage,
    SnapshotState, WorkspaceSnapshotConfig, WorkspaceSnapshotDescriptor, WorkspaceSnapshotError,
    WorkspaceSnapshotInput, WorkspaceSnapshotResult, create_workspace_snapshot_provider,
    resolve_workspace_snapshot_config,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use scratch::{owned_path, remove_owned, remove_owned_with};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs, io,
    os::unix::{ffi::OsStringExt, fs::symlink},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
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
            "#322 oracle.json is missing; generate it first (tests/fixtures/workspace-snapshot/README.md):\n\
             PDRS_BASELINE=/absolute/path/to/PilotDeck node --import \"$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs\" \
             {FIXTURES}/generate.mts {FIXTURES}"
        );
    };
    assert!(
        !text.contains(".generate-"),
        "#322 oracle.json leaks a generation path"
    );
    // Every scenario path is a per-run mkdtemp directory masked as `<dir>`.
    for fixed in ["/tmp/pdrs-r9-", "/tmp/ws-snapshot-scenario", "/tmp/pdrs-"] {
        assert!(
            !text.contains(fixed),
            "#322 oracle.json keeps a fixed absolute path: {fixed}"
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
        fs::create_dir_all(&base).expect("#322 create scratch base");
        loop {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("#322 system time after the epoch")
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
                Err(error) => panic!("#322 create scratch {}: {error}", dir.display()),
            }
        }
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = remove_owned(&self.0, &self.0, true);
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
        .expect("#322 fixture value is a string")
        .to_utf8()
        .expect("#322 fixture string is UTF-8")
}
/// #293: a missing or non-array oracle field is a broken corpus, not an
/// empty one — the old silent fold let a truncated oracle replay zero
/// scenarios and still pass.
fn arr<'a>(value: &'a JsValue, context: &str) -> &'a [JsValue] {
    if let JsValue::Array(items) = value {
        items
    } else {
        panic!("#293 oracle field {context} is not an array: {value:?}")
    }
}
fn strs(value: &JsValue, context: &str) -> Vec<String> {
    arr(value, context).iter().map(s).collect()
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
        let name = entry.file_name().to_string_lossy().into_owned();
        // #293 fix-r3: the root-level `.staging` parent is the Rust-side
        // private area (TS has none — the same registered boundary the
        // provider walk skips), and the oracle tree is the TS view. Its
        // emptiness is asserted by `no_temporaries` instead.
        if directory == root && name == ".staging" {
            continue;
        }
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
        let path = directory.join(entry.file_name());
        // `.staging` is the #293 fix-r3 persistent staging parent: the fix-r2
        // code removed it best-effort on every settle, which raced another
        // round's create_dir_all into a permanent `_FAILED.json`. It stays
        // now, and must hold no leftover round directory (uuid staging dirs
        // and `.tmp-` files alike).
        if name == ".staging" {
            let leftovers: Vec<String> = fs::read_dir(&path)
                .expect("#293 fix-r3 the staging parent is listable")
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            assert!(
                leftovers.is_empty(),
                "#293 fix-r3 leftover staging directories: {leftovers:?}"
            );
            continue;
        }
        assert!(!name.contains(".tmp-"), "leftover temporary {name}");
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

/// #322-3: the workspace-snapshot corpus must not be allowed to shrink. The
/// old `>= 27` check let a truncated oracle (valid provenance, fewer scenarios)
/// replay zero/fewer rows and still pass. Every name and both per-scenario
/// counts are pinned; the generator asserts the same totals.
const EXPECTED: &[(&str, usize, usize)] = &[
    ("normal", 17, 61),
    ("quiescent", 13, 13),
    ("failure-fields", 5, 15),
    ("mask-scope", 4, 15),
    ("identity-sanitize", 5, 16),
    ("identity-invalid", 6, 13),
    ("snapshot-inside", 3, 19),
    ("workspace-missing", 2, 7),
    ("root-equals-workspace", 3, 8),
    ("provider-quirks", 36, 46),
    ("provider-null", 3, 7),
    ("provider-order", 8, 17),
    ("manifest-syntax", 3, 7),
    ("manifest-read-dir", 3, 8),
    ("nul-workspace-dir", 2, 7),
    ("surrogate-readback", 7, 13),
    ("nul-restore-target", 3, 13),
    ("nul-restore-object", 2, 7),
    ("nul-restore-link", 2, 7),
    ("nul-restore-link-output", 2, 7),
    ("nul-restore-path", 3, 12),
    ("nul-restore-link-existing", 4, 7),
    ("nul-link-target-surrogate", 2, 7),
    ("nul-output-surrogate", 2, 7),
    ("nul-object-surrogate", 2, 7),
    ("nul-file-output-surrogate", 3, 12),
    ("nul-capture-root", 2, 0),
];

#[test]
fn scenarios_match_ts_outcomes_and_trees() {
    let oracle = oracle();
    assert_eq!(
        s(field(&oracle, "node")),
        "v22.22.0",
        "workspace-snapshot oracle must record the Node that produced it"
    );
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
        fs::create_dir_all(&dir).expect("#322 create sentinel directory");
        let sentinel = dir.join("sentinel");
        fs::write(&sentinel, b"keep\n").expect("#322 write sentinel");
        sentinel
    })
    .collect();
    let scenarios = arr(field(&oracle, "scenarios"), "scenarios");
    assert_eq!(
        scenarios.len(),
        EXPECTED.len(),
        "#322 workspace-snapshot corpus is pinned"
    );
    assert_eq!(
        scenarios
            .iter()
            .map(|scenario| s(field(scenario, "name")))
            .collect::<Vec<_>>(),
        EXPECTED
            .iter()
            .map(|(name, _, _)| (*name).to_owned())
            .collect::<Vec<_>>(),
        "#322 workspace-snapshot scenario names are pinned"
    );
    for (scenario, (name, steps, tree)) in scenarios.iter().zip(EXPECTED) {
        assert_eq!(
            s(field(scenario, "name")),
            *name,
            "#322 scenario name order is pinned"
        );
        assert_eq!(
            arr(field(scenario, "steps"), "scenarios[].steps").len(),
            *steps,
            "#322 {name}: step count is pinned"
        );
        assert_eq!(
            arr(field(scenario, "tree"), "scenarios[].tree").len(),
            *tree,
            "#322 {name}: tree row count is pinned"
        );
        replay_scenario(scenario);
    }
    for sentinel in &sentinels {
        assert_eq!(
            fs::read(sentinel).expect("#322 read sentinel"),
            b"keep\n",
            "#322 run removed an unowned sentinel {}",
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

/// #293 (registered divergence A6.6-SNAPSHOT-OBJECTKEY-001): TS resolves
/// restore objectKeys without validation and copies snapshot-root-external
/// content into the target workspace (Node v22.22.0 probe
/// `_tmp/20261026-issue-293/probe-a-objectkey.out`); Rust refuses the read
/// instead. A key that folds back inside the root still restores, matching
/// the TS-resolvable spelling.
#[test]
fn restore_refuses_object_keys_outside_the_snapshot_root() {
    let run = Scratch::new();
    let root = run.0.join("snapshots");
    let manifest_dir = root.join("workspaces/w/sessions/s/snapshots");
    fs::create_dir_all(manifest_dir.join("esc-rel")).expect("#293 create the esc-rel partition");
    fs::create_dir_all(manifest_dir.join("esc-abs")).expect("#293 create the esc-abs partition");
    fs::create_dir_all(manifest_dir.join("fold-in")).expect("#293 create the fold-in partition");
    fs::create_dir_all(root.join("objects/sub")).expect("#293 create the objects dir");
    fs::write(root.join("objects/ok.txt"), b"inside-root\n").expect("#293 seed the in-root object");
    let outside = run.0.join("outside.txt");
    fs::write(&outside, b"SECRET-outside\n").expect("#293 seed the root-external object");
    let absolute = run.0.join("absolute.txt");
    fs::write(&absolute, b"SECRET-absolute\n").expect("#293 seed the absolute-path object");

    let manifest = |snapshot_id: &str, entries: &str| {
        format!(
            "{{\"snapshotId\":\"{snapshot_id}\",\"workspaceId\":\"w\",\"sessionId\":\"s\",\
             \"turnId\":\"t\",\"runId\":\"r\",\"phase\":\"pre_user\",\
             \"createdAt\":\"2026-01-01T00:00:00.000Z\",\"entries\":[{entries}]}}"
        )
    };
    fs::write(
        manifest_dir.join("esc-rel/manifest.json"),
        manifest(
            "esc-rel",
            concat!(
            "{\"path\":\"ok.txt\",\"entryType\":\"file\",\"size\":12,\"objectKey\":\"objects/ok.txt\"},",
            "{\"path\":\"escaped.txt\",\"entryType\":\"file\",\"size\":15,\"objectKey\":\"../outside.txt\"},",
            "{\"path\":\"after.txt\",\"entryType\":\"file\",\"size\":17,\"objectKey\":\"after.txt\"}",
        )),
    )
    .expect("#293 seed the esc-rel manifest");
    fs::write(
        manifest_dir.join("esc-abs/manifest.json"),
        manifest(
            "esc-abs",
            &format!(
                "{{\"path\":\"absolute.txt\",\"entryType\":\"file\",\"size\":17,\
             \"objectKey\":\"{}\"}}",
                absolute.to_string_lossy()
            ),
        ),
    )
    .expect("#293 seed the esc-abs manifest");
    fs::write(
        manifest_dir.join("fold-in/manifest.json"),
        manifest(
            "fold-in",
            "{\"path\":\"folded.txt\",\"entryType\":\"file\",\"size\":12,\
             \"objectKey\":\"objects/sub/../ok.txt\"}",
        ),
    )
    .expect("#293 seed the fold-in manifest");

    let root_text = root.to_string_lossy().into_owned();
    let provider = create_workspace_snapshot_provider(WorkspaceSnapshotConfig {
        root: root_text.clone(),
        snapshot_root: root_text,
    });

    // Positive control first: an in-root objectKey restores.
    let target = run.0.join("target-fold");
    let restored = provider
        .restore("fold-in", &target.to_string_lossy())
        .expect("#293 an in-root folding objectKey must restore");
    assert!(restored.restored, "#293 fold-in restore reports restored");
    assert_eq!(
        fs::read(target.join("folded.txt")).expect("#293 fold-in restored a file"),
        b"inside-root\n",
        "#293 fold-in restored the root object"
    );

    // The `#293 expected ObjectKeyEscape` wording in the expect_err messages
    // below (and in the sibling checks further down) is the kill criterion of
    // the `objectkey-escape-allowed` negative control
    // (tests/fixtures/workspace-snapshot-gate/negative_controls.py).
    // `../outside.txt`: refused after the earlier in-root entry restored.
    let target = run.0.join("target-rel");
    let error = provider
        .restore("esc-rel", &target.to_string_lossy())
        .expect_err("#293 expected ObjectKeyEscape; a root-escaping objectKey must be refused");
    assert!(
        matches!(error, WorkspaceSnapshotError::ObjectKeyEscape),
        "#293 expected ObjectKeyEscape, got {error:?}"
    );
    assert_eq!(
        error.to_string(),
        "Snapshot object key escapes the snapshot root.",
        "#293 refusal message"
    );
    assert_eq!(
        fs::read(target.join("ok.txt")).expect("#293 the pre-refusal entry restored a file"),
        b"inside-root\n",
        "#293 the in-root entry before the refusal still restored"
    );
    for refused in ["escaped.txt", "after.txt"] {
        assert!(
            !target.join(refused).exists(),
            "#293 nothing after the escaping key may be written: {refused}"
        );
    }

    // Absolute objectKey: refused before the read.
    let target = run.0.join("target-abs");
    let error = provider
        .restore("esc-abs", &target.to_string_lossy())
        .expect_err("#293 expected ObjectKeyEscape; an absolute objectKey must be refused");
    assert!(
        matches!(error, WorkspaceSnapshotError::ObjectKeyEscape),
        "#293 expected ObjectKeyEscape, got {error:?}"
    );
    assert!(
        !target.join("absolute.txt").exists(),
        "#293 the root-external absolute object was not copied into the target"
    );
}

/// #293 fix-r2 (registered divergence A6.6-SNAPSHOT-CLEANUP-OWNERSHIP-001
/// -R1, -R2): two recorder instances sharing one snapshot root and one
/// capture key; the failing round's cleanup removes only its own private
/// staging directory and never touches the published partition (TS unlinks
/// the shared manifest/`_COMMITTED` unconditionally, Node v22.22.0 probe
/// `_tmp/20261026-issue-293/probe-b-concurrent.out`). The interleave is
/// deterministic, not inferred from file times: R2's recorder holds the
/// `BeforeFailureCleanup` seam after failing its scan at a missing
/// workspace directory — an ENOENT scandir that also fails when the tests
/// run as root — the main thread runs R1 to a full commit, and only then
/// releases R2's cleanup, so the cleanup provably runs after the other
/// round's commit. No sleeps, no big-file timing window. The same held
/// position covers the r1/r2 review interleavings around the old
/// shared-path deletes (the r2-P0-1 graveyard-return window, the r1-P0-3
/// check-then-unlink, the r1-P0-2 rename-stat capture): this cleanup has no
/// step that touches a published path, whatever a third round does while it
/// runs.
#[test]
fn concurrent_failure_cleanup_keeps_other_rounds_commit() {
    let run = Scratch::new();
    let snapshot_root = run.0.join("snapshots");
    fs::create_dir_all(&snapshot_root).expect("#293 create the snapshot root");

    // R2 (will fail): its workspace directory does not exist, so the scan
    // fails with a deterministic ENOENT scandir.
    let slow_ws = run.0.join("ws-gone");

    // R1 (will succeed): a single small file.
    let fast_ws = run.0.join("ws-fast");
    fs::create_dir_all(&fast_ws).expect("#293 create R1's workspace");
    fs::write(fast_ws.join("only.txt"), b"fast\n").expect("#293 seed R1's workspace");

    let input = |workspace_dir: &Path, run_id: &str| WorkspaceSnapshotInput {
        workspace_id: "w".into(),
        session_id: "s".into(),
        turn_id: "t".into(),
        run_id: run_id.into(),
        workspace_dir: workspace_dir.to_string_lossy().into_owned(),
        workspace_stable: None,
        failure_kind: None,
        failure_reason: None,
    };
    let config = WorkspaceSnapshotConfig {
        root: run.0.to_string_lossy().into_owned(),
        snapshot_root: snapshot_root.to_string_lossy().into_owned(),
    };
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    let hook: SnapshotRoundHook = Arc::new(move |stage| {
        if stage == SnapshotRoundStage::BeforeFailureCleanup {
            entered_tx
                .send(())
                .expect("#293 the failure-cleanup seam channel stays open");
            release_rx
                .lock()
                .expect("#293 the failure-cleanup seam mutex")
                .recv()
                .expect("#293 the failure-cleanup release signal arrives");
        }
    });
    let r2 = ContentAddressedWorkspaceSnapshotRecorder::new_with_hook(config.clone(), hook);
    let r1 = ContentAddressedWorkspaceSnapshotRecorder::new(config.clone());
    let slow_input = input(&slow_ws, "run-r2");
    let fast_input = input(&fast_ws, "run-r1");
    let r2handle = thread::spawn(move || r2.capture_pre_user(&slow_input));

    // Deterministic interleave: R2 holds at the failure-cleanup seam (it
    // passed the marker checks and failed inside the try block) until R1
    // has fully committed. 15s bound; a missing signal is a broken probe,
    // never a pass.
    entered_rx
        .recv_timeout(Duration::from_secs(15))
        .expect("#293 R2 reached the failure-cleanup seam (marker checks passed, scan failed)");
    let r1result = r1
        .capture_pre_user(&fast_input)
        .expect("#293 R1 must commit its capture");
    release_tx
        .send(())
        .expect("#293 release the failure-cleanup seam");
    let r2result = r2handle
        .join()
        .expect("#293 R2 must not panic")
        .expect("#293 R2 must return a failed result, not a hard error");
    let partition = snapshot_root
        .join("workspaces/w/sessions/s/snapshots")
        .join(sha256_hex(b"s\0t\0pre_user"));
    let manifest_path = partition.join("manifest.json");
    let committed_path = partition.join("_COMMITTED");
    let failed_path = partition.join("_FAILED.json");

    assert_eq!(
        r1result.state,
        SnapshotState::Committed,
        "#293 R1 committed before R2's cleanup ran"
    );
    assert_eq!(
        r2result.state,
        SnapshotState::Failed,
        "#293 R2 must fail at the missing workspace directory"
    );
    let manifest = fs::read_to_string(&manifest_path)
        .expect("#293 R1's committed manifest survives R2's failure cleanup");
    assert!(
        manifest.contains("\"runId\": \"run-r1\""),
        "#293 the surviving manifest is R1's, not R2's: {manifest}"
    );
    assert!(
        committed_path.exists(),
        "#293 R1's committed marker survives R2's failure cleanup"
    );
    let failed = fs::read_to_string(&failed_path)
        .expect("#293 R2 wrote its failure marker alongside R1's commit");
    assert!(
        failed.contains("ENOENT") && failed.contains("scandir"),
        "#293 R2's marker records the scandir failure: {failed}"
    );
    // R2 removed its own staging directory and nothing else: the persistent
    // `.staging` parent (fix-r3) stays behind, holding no round directory.
    assert!(
        snapshot_root.join(".staging").is_dir(),
        "#293 fix-r3 the staging parent survives the failed round's cleanup"
    );
    let mut staging_names: Vec<String> = fs::read_dir(snapshot_root.join(".staging"))
        .expect("#293 fix-r3 the staging parent is listable after the interleave")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    staging_names.sort();
    assert!(
        staging_names.is_empty(),
        "#293 R2 removed its own staging directory, leaving the persistent parent empty: {staging_names:?}"
    );
    let mut names: Vec<String> = fs::read_dir(&partition)
        .expect("#293 the partition is listable after the interleave")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["_COMMITTED", "_FAILED.json", "manifest.json"],
        "#293 the partition holds exactly the concurrent commit and the failure marker"
    );
    // A third round early-exits on R1's _COMMITTED instead of rescanning.
    let r3 = ContentAddressedWorkspaceSnapshotRecorder::new(config);
    let r3result = r3
        .capture_pre_user(&input(&slow_ws, "run-r3"))
        .expect("#293 the committed marker short-circuits the third round");
    assert_eq!(r3result.state, SnapshotState::Committed);
    assert_eq!(
        r3result.manifest_path.as_deref(),
        Some(manifest_path.to_string_lossy().as_ref())
    );
}

/// #293 fix-r2 (A6.6-SNAPSHOT-CLEANUP-OWNERSHIP-001-R2): a round whose
/// staging directory is complete when a concurrent round has already
/// published the same key degrades to TS's own per-file renames, which
/// overwrite (Node v22.22.0 probe
/// `_tmp/20261026-issue-293/fix-r2/probe-c.out`: TS commits onto a
/// pre-existing partition, a planted `keep.txt` survives, and
/// `rename(file, file)` overwrites), so the last writer's manifest wins.
/// The interleave is deterministic: R2 holds the `BeforeStagingPublish`
/// seam — its `_COMMITTED` and `manifest.json` are already in staging —
/// while R1 commits, so R2's publishing rename provably meets an occupied
/// partition (ENOTEMPTY, the measured dir-onto-dir errno).
#[test]
fn publish_over_a_concurrent_commit_overwrites_like_ts() {
    let run = Scratch::new();
    let snapshot_root = run.0.join("snapshots");
    fs::create_dir_all(&snapshot_root).expect("#293 create the snapshot root");

    let r2ws = run.0.join("ws-r2");
    fs::create_dir_all(&r2ws).expect("#293 create R2's workspace");
    fs::write(r2ws.join("only.txt"), b"r2\n").expect("#293 seed R2's workspace");
    let r1ws = run.0.join("ws-r1");
    fs::create_dir_all(&r1ws).expect("#293 create R1's workspace");
    fs::write(r1ws.join("only.txt"), b"r1\n").expect("#293 seed R1's workspace");

    let input = |workspace_dir: &Path, run_id: &str| WorkspaceSnapshotInput {
        workspace_id: "w".into(),
        session_id: "s".into(),
        turn_id: "t".into(),
        run_id: run_id.into(),
        workspace_dir: workspace_dir.to_string_lossy().into_owned(),
        workspace_stable: None,
        failure_kind: None,
        failure_reason: None,
    };
    let config = WorkspaceSnapshotConfig {
        root: run.0.to_string_lossy().into_owned(),
        snapshot_root: snapshot_root.to_string_lossy().into_owned(),
    };
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    let hook: SnapshotRoundHook = Arc::new(move |stage| {
        if stage == SnapshotRoundStage::BeforeStagingPublish {
            entered_tx
                .send(())
                .expect("#293 the publish seam channel stays open");
            release_rx
                .lock()
                .expect("#293 the publish seam mutex")
                .recv()
                .expect("#293 the publish release signal arrives");
        }
    });
    let r2 = ContentAddressedWorkspaceSnapshotRecorder::new_with_hook(config.clone(), hook);
    let r1 = ContentAddressedWorkspaceSnapshotRecorder::new(config.clone());
    let r2_input = input(&r2ws, "run-r2");
    let r1_input = input(&r1ws, "run-r1");
    let r2handle = thread::spawn(move || r2.capture_pre_user(&r2_input));

    // Deterministic interleave: R2 holds the fully-written staging directory
    // at the publish seam until R1 has fully committed the same key.
    entered_rx
        .recv_timeout(Duration::from_secs(15))
        .expect("#293 R2 reached the publish seam with staging complete");
    let r1result = r1
        .capture_pre_user(&r1_input)
        .expect("#293 R1 must commit its capture");
    release_tx.send(()).expect("#293 release the publish seam");
    let r2result = r2handle
        .join()
        .expect("#293 R2 must not panic")
        .expect("#293 R2 must return a result, not a hard error");
    let partition = snapshot_root
        .join("workspaces/w/sessions/s/snapshots")
        .join(sha256_hex(b"s\0t\0pre_user"));
    let manifest_path = partition.join("manifest.json");

    assert_eq!(
        r1result.state,
        SnapshotState::Committed,
        "#293 R1 committed while R2 held the publish seam"
    );
    assert_eq!(
        r2result.state,
        SnapshotState::Committed,
        "#293 R2 commits over R1's occupied partition like TS"
    );
    assert_eq!(
        r2result.manifest_path.as_deref(),
        Some(manifest_path.to_string_lossy().as_ref()),
        "#293 the overwriting round reports the shared manifest path"
    );
    let manifest =
        fs::read_to_string(&manifest_path).expect("#293 the overwritten manifest is readable");
    assert!(
        manifest.contains("\"runId\": \"run-r2\""),
        "#293 the last writer's manifest wins: {manifest}"
    );
    let mut names: Vec<String> = fs::read_dir(&partition)
        .expect("#293 the partition is listable after both rounds")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["_COMMITTED", "manifest.json"],
        "#293 the partition holds exactly the manifest and the committed marker"
    );
    assert!(
        snapshot_root.join(".staging").is_dir(),
        "#293 fix-r3 the staging parent survives both settles"
    );
    assert!(
        fs::read_dir(snapshot_root.join(".staging"))
            .expect("#293 fix-r3 the staging parent is listable after both rounds")
            .flatten()
            .next()
            .is_none(),
        "#293 both rounds consumed their staging directories"
    );
}

/// #293 fix-r2 (registered divergence A6.6-SNAPSHOT-CLEANUP-OWNERSHIP-001
/// -R2): `list()` ignores the root's `.staging` directory — an in-flight
/// round is not a published snapshot. TS's walk has no private staging area
/// and sees a round's `partition/.tmp-*/manifest.json` mid-flight, so this
/// is a deliberate divergence. The interleave is deterministic: R2 holds
/// the `BeforeStagingPublish` seam with staging complete while the main
/// thread lists.
#[test]
fn list_ignores_in_flight_staging_directories() {
    let run = Scratch::new();
    let snapshot_root = run.0.join("snapshots");
    fs::create_dir_all(&snapshot_root).expect("#293 create the snapshot root");
    let workspace = run.0.join("ws");
    fs::create_dir_all(&workspace).expect("#293 create the workspace");
    fs::write(workspace.join("only.txt"), b"x\n").expect("#293 seed the workspace");

    let input = WorkspaceSnapshotInput {
        workspace_id: "w".into(),
        session_id: "s".into(),
        turn_id: "t".into(),
        run_id: "run-r2".into(),
        workspace_dir: workspace.to_string_lossy().into_owned(),
        workspace_stable: None,
        failure_kind: None,
        failure_reason: None,
    };
    let config = WorkspaceSnapshotConfig {
        root: run.0.to_string_lossy().into_owned(),
        snapshot_root: snapshot_root.to_string_lossy().into_owned(),
    };
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    let hook: SnapshotRoundHook = Arc::new(move |stage| {
        if stage == SnapshotRoundStage::BeforeStagingPublish {
            entered_tx
                .send(())
                .expect("#293 the publish seam channel stays open");
            release_rx
                .lock()
                .expect("#293 the publish seam mutex")
                .recv()
                .expect("#293 the publish release signal arrives");
        }
    });
    let r2 = ContentAddressedWorkspaceSnapshotRecorder::new_with_hook(config.clone(), hook);
    let hooked_input = input.clone();
    let r2handle = thread::spawn(move || r2.capture_pre_user(&hooked_input));

    entered_rx
        .recv_timeout(Duration::from_secs(15))
        .expect("#293 R2 reached the publish seam with staging complete");
    let in_flight = create_workspace_snapshot_provider(config.clone())
        .list()
        .expect("#293 the list call itself succeeds while a round is in flight");
    assert!(
        in_flight.is_empty(),
        "#293 an in-flight staging manifest is not listed: {in_flight:?}"
    );
    release_tx.send(()).expect("#293 release the publish seam");
    let r2result = r2handle
        .join()
        .expect("#293 R2 must not panic")
        .expect("#293 R2 must return a result, not a hard error");
    assert_eq!(
        r2result.state,
        SnapshotState::Committed,
        "#293 the in-flight round commits after the release"
    );
    let published = create_workspace_snapshot_provider(config)
        .list()
        .expect("#293 the list call succeeds after the commit");
    assert_eq!(
        published
            .iter()
            .map(|descriptor| descriptor
                .snapshot_id
                .to_utf8()
                .expect("#293 UTF-8 snapshot id"))
            .collect::<Vec<_>>(),
        [sha256_hex(b"s\0t\0pre_user")],
        "#293 the released round's snapshot lists after it commits"
    );
    assert!(
        snapshot_root.join(".staging").is_dir(),
        "#293 fix-r3 the staging parent survives the committed round"
    );
    assert!(
        fs::read_dir(snapshot_root.join(".staging"))
            .expect("#293 fix-r3 the staging parent is listable after the commit")
            .flatten()
            .next()
            .is_none(),
        "#293 the committed round consumed its staging directory"
    );
}

/// #293 fix-r1 (registered divergence A6.6-SNAPSHOT-OBJECTKEY-001-R1): the
/// lexical boundary check cannot see symlinks, and a canonicalize-then-read
/// leaves a swap-link race. A link anywhere below the snapshot root — final
/// component, mid-directory, dangling, even pointing back inside the root —
/// must be refused with ObjectKeyEscape, never followed into root-external
/// content.
#[test]
fn restore_refuses_object_keys_through_symlinks() {
    let run = Scratch::new();
    let root = run.0.join("snapshots");
    let manifest_dir = root.join("workspaces/w/sessions/s/snapshots");
    fs::create_dir_all(manifest_dir.join("esc-link")).expect("#293 create the esc-link partition");
    fs::create_dir_all(root.join("objects/real")).expect("#293 create the objects dir");
    fs::write(root.join("objects/ok.txt"), b"inside-root\n").expect("#293 seed the in-root object");
    let outside_dir = run.0.join("outside-dir");
    fs::create_dir_all(&outside_dir).expect("#293 create the root-external dir");
    fs::write(outside_dir.join("secret.txt"), b"SECRET-outside\n")
        .expect("#293 seed the root-external secret");

    // A final-component link to outside, a mid-directory link, a dangling
    // link, and a link that points back inside the root: all refused (the
    // real target of a link is never the key's own path).
    symlink(
        outside_dir
            .join("secret.txt")
            .to_string_lossy()
            .into_owned(),
        root.join("objects/link-out"),
    )
    .expect("#293 plant the outside-pointing link");
    symlink(
        outside_dir.to_string_lossy().into_owned(),
        root.join("objects/dir-link"),
    )
    .expect("#293 plant the mid-directory link");
    symlink(
        run.0.join("nowhere").to_string_lossy().into_owned(),
        root.join("objects/dangling"),
    )
    .expect("#293 plant the dangling link");
    symlink("ok.txt", root.join("objects/link-in")).expect("#293 plant the in-root-pointing link");

    let manifest = |snapshot_id: &str, object_key: &str| {
        format!(
            "{{\"snapshotId\":\"{snapshot_id}\",\"workspaceId\":\"w\",\"sessionId\":\"s\",\
             \"turnId\":\"t\",\"runId\":\"r\",\"phase\":\"pre_user\",\
             \"createdAt\":\"2026-01-01T00:00:00.000Z\",\"entries\":[\
             {{\"path\":\"out.txt\",\"entryType\":\"file\",\"size\":7,\"objectKey\":\"{object_key}\"}}]}}"
        )
    };
    let cases = [
        ("esc-link", "objects/link-out"),
        ("esc-mid", "objects/dir-link/secret.txt"),
        ("esc-dangling", "objects/dangling"),
        ("esc-inroot", "objects/link-in"),
    ];
    for (id, key) in cases {
        fs::create_dir_all(manifest_dir.join(id))
            .unwrap_or_else(|error| panic!("#293 create the {id} partition: {error}"));
        fs::write(
            manifest_dir.join(id).join("manifest.json"),
            manifest(id, key),
        )
        .unwrap_or_else(|error| panic!("#293 seed the {id} manifest: {error}"));
    }
    let root_text = root.to_string_lossy().into_owned();
    let provider = create_workspace_snapshot_provider(WorkspaceSnapshotConfig {
        root: root_text.clone(),
        snapshot_root: root_text,
    });
    for (id, key) in cases {
        let target = run.0.join(format!("target-{id}"));
        // The expect_err wording is the kill criterion of the
        // `objectkey-symlink-followed` negative control
        // (tests/fixtures/workspace-snapshot-gate/negative_controls.py).
        let error = provider
            .restore(id, &target.to_string_lossy())
            .expect_err("#293 expected ObjectKeyEscape; a symlinked objectKey must be refused");
        assert!(
            matches!(error, WorkspaceSnapshotError::ObjectKeyEscape),
            "#293 expected ObjectKeyEscape for {key}, got {error:?}"
        );
        assert!(
            !target.join("out.txt").exists(),
            "#293 nothing behind a symlinked key may be written: {key}"
        );
    }
}

/// #293 fix-r3 (registered divergence A6.6-SNAPSHOT-CLEANUP-OWNERSHIP-001
/// -R3): the fallback publish over an occupied partition writes the manifest
/// first and the committed marker last — the marker is the commit point. The
/// planted partition — a `manifest.json` directory plus `keep.txt`, no
/// markers — makes the manifest rename fail with EISDIR deterministically
/// before the marker is published, exactly what the real TS recorder leaves
/// in the same scenario (Node v22.22.0 probe
/// `_tmp/20261026-issue-293/fix-r2/probe-c.out` c6: `state: failed`, no
/// `_COMMITTED` on the partition, `_FAILED.json` written). The fix-r2 order
/// (marker first) instead left a published `_COMMITTED` with no readable
/// manifest, and the next round's early-exit then reported Committed over a
/// snapshot that could never be restored (r3 review P1-A).
#[test]
fn fallback_publish_writes_the_manifest_before_the_committed_marker() {
    let run = Scratch::new();
    let snapshot_root = run.0.join("snapshots");
    let workspace = run.0.join("ws");
    fs::create_dir_all(&workspace).expect("#293 create the workspace");
    fs::write(workspace.join("a.txt"), b"a\n").expect("#293 seed the workspace");
    let partition = snapshot_root
        .join("workspaces/w/sessions/s/snapshots")
        .join(sha256_hex(b"s\0t\0pre_user"));
    // The planted partition: occupied, so the staging rename falls back to
    // the per-file publishes; manifest.json is a directory, so the manifest
    // rename (now first) fails before the marker is published.
    fs::create_dir_all(partition.join("manifest.json")).expect("#293 plant the manifest directory");
    fs::write(
        partition.join("manifest.json/inner.txt"),
        b"not-a-manifest\n",
    )
    .expect("#293 seed the planted manifest directory");
    fs::write(partition.join("keep.txt"), b"keep\n").expect("#293 plant keep.txt");

    let input = WorkspaceSnapshotInput {
        workspace_id: "w".into(),
        session_id: "s".into(),
        turn_id: "t".into(),
        run_id: "run-r2".into(),
        workspace_dir: workspace.to_string_lossy().into_owned(),
        workspace_stable: None,
        failure_kind: None,
        failure_reason: None,
    };
    let config = WorkspaceSnapshotConfig {
        root: run.0.to_string_lossy().into_owned(),
        snapshot_root: snapshot_root.to_string_lossy().into_owned(),
    };
    let recorder = ContentAddressedWorkspaceSnapshotRecorder::new(config.clone());
    let result = recorder
        .capture_pre_user(&input)
        .expect("#293 the failing round still returns a result, not a hard error");
    assert_eq!(
        result.state,
        SnapshotState::Failed,
        "#293 the fallback publish fails on the planted manifest directory"
    );
    assert!(
        result
            .error
            .as_ref()
            .and_then(|error| error.to_utf8().ok())
            .is_some_and(|message| {
                message.contains("EISDIR")
                    && message.contains("rename '")
                    && message.ends_with("manifest.json'")
            }),
        "#293 the failure is the manifest rename over the planted directory: {:?}",
        result.error
    );
    // The load-bearing order assertion (the kill criterion of the
    // `fallback-order` negative control): the marker is the commit point, so
    // a manifest publish that failed must leave no published marker at all —
    // the residue a reader could mistake for a committed snapshot.
    assert!(
        !partition.join("_COMMITTED").exists(),
        "#293 fix-r3 the committed marker is the commit point: not published while the manifest publish failed"
    );
    assert!(
        partition.join("manifest.json").is_dir(),
        "#293 the planted manifest directory is untouched by the failure"
    );
    assert!(
        fs::read(partition.join("manifest.json/inner.txt"))
            .is_ok_and(|bytes| bytes == b"not-a-manifest\n"),
        "#293 the planted directory's contents survive"
    );
    assert!(
        fs::read(partition.join("keep.txt")).is_ok_and(|bytes| bytes == b"keep\n"),
        "#293 the planted keep.txt survives the failed publish"
    );
    assert!(
        partition.join("_FAILED.json").is_file(),
        "#293 the failure marker is written beside the residue"
    );
    // The staging parent is persistent (fix-r3 P1-B): it stays, holding no
    // leftover round directory.
    assert!(
        snapshot_root.join(".staging").is_dir(),
        "#293 fix-r3 the staging parent survives the failed round"
    );
    assert!(
        fs::read_dir(snapshot_root.join(".staging"))
            .expect("#293 fix-r3 the staging parent is listable after the failed round")
            .flatten()
            .next()
            .is_none(),
        "#293 the failed round's staging directory is removed, leaving the parent empty"
    );
    // A third round on the failed partition early-exits on `_FAILED.json`
    // (no committed marker exists), exactly like a TS round over the probe-c
    // c6 residue: not a false Committed, and never a rescan.
    let third = ContentAddressedWorkspaceSnapshotRecorder::new(config);
    let third_result = third
        .capture_pre_user(&input)
        .expect("#293 the third round returns a result, not a hard error");
    assert_eq!(
        third_result.state,
        SnapshotState::Failed,
        "#293 fix-r3 the third round sees the failure marker, not a phantom commit"
    );
    assert!(
        !partition.join("_COMMITTED").exists(),
        "#293 fix-r3 no committed marker appears behind the failure marker"
    );
}

/// #293 fix-r3 (registered divergence A6.6-SNAPSHOT-CLEANUP-OWNERSHIP-001
/// -R3): a committed snapshot is a partition whose manifest is present as a
/// file AND whose `_COMMITTED` marker exists — the early exit no longer
/// trusts the marker alone. The fix-r2 fallback order could leave exactly
/// that marker-without-manifest residue, and every later round then reported
/// Committed over a manifest that could never be read or restored (r3 review
/// P1-A); the fix-r3 order (manifest first, marker last) cannot produce it,
/// and a hand-planted or foreign marker must not fake a commit either. TS
/// early-exits on the marker alone (read, not measured — the baseline tree
/// was not available in this round), but its own write order never leaves
/// that residue. The manifest-only residue (an interrupted publish between
/// the two renames, no markers) must likewise republish and overwrite.
#[test]
fn orphan_committed_marker_without_a_manifest_recaptures() {
    let run = Scratch::new();
    let snapshot_root = run.0.join("snapshots");
    let workspace = run.0.join("ws");
    fs::create_dir_all(&workspace).expect("#293 fix-r3 create the workspace");
    fs::write(workspace.join("only.txt"), b"x\n").expect("#293 fix-r3 seed the workspace");
    let partitions = snapshot_root.join("workspaces/w/sessions/s/snapshots");
    let marker_only = partitions.join(sha256_hex(b"s\0t1\0pre_user"));
    fs::create_dir_all(&marker_only).expect("#293 fix-r3 create the marker-only partition");
    // The fix-r2 residue: a committed marker with no manifest beside it.
    fs::write(marker_only.join("_COMMITTED"), b"")
        .expect("#293 fix-r3 plant the orphan committed marker");
    let manifest_only = partitions.join(sha256_hex(b"s\0t2\0pre_user"));
    fs::create_dir_all(&manifest_only).expect("#293 fix-r3 create the manifest-only partition");
    // An interrupted publish between the two renames: the manifest landed,
    // the marker never did, and no `_FAILED.json` exists.
    fs::write(
        manifest_only.join("manifest.json"),
        "{\n  \"snapshotId\": \"interrupted\"\n}\n",
    )
    .expect("#293 fix-r3 plant the interrupted manifest");

    let input = |turn_id: &str| WorkspaceSnapshotInput {
        workspace_id: "w".into(),
        session_id: "s".into(),
        turn_id: turn_id.into(),
        run_id: "run-r3".into(),
        workspace_dir: workspace.to_string_lossy().into_owned(),
        workspace_stable: None,
        failure_kind: None,
        failure_reason: None,
    };
    let config = WorkspaceSnapshotConfig {
        root: run.0.to_string_lossy().into_owned(),
        snapshot_root: snapshot_root.to_string_lossy().into_owned(),
    };

    // The orphan marker must not short-circuit: the round recaptures and
    // republishes a complete snapshot over it.
    let recorder = ContentAddressedWorkspaceSnapshotRecorder::new(config.clone());
    let result = recorder
        .capture_pre_user(&input("t1"))
        .expect("#293 fix-r3 the round over the orphan marker returns a result");
    assert_eq!(
        result.state,
        SnapshotState::Committed,
        "#293 fix-r3 an orphan marker must not short-circuit: the round recaptured"
    );
    assert_eq!(
        result.manifest_path.as_deref(),
        Some(marker_only.join("manifest.json").to_string_lossy().as_ref()),
        "#293 fix-r3 the recaptured round reports the shared manifest path"
    );
    let manifest = fs::read_to_string(marker_only.join("manifest.json"))
        .expect("#293 fix-r3 the republished manifest is readable");
    assert!(
        manifest.contains("\"runId\": \"run-r3\""),
        "#293 fix-r3 the republished manifest is this round's, not a stale one: {manifest}"
    );
    assert!(
        marker_only.join("_COMMITTED").is_file(),
        "#293 fix-r3 the republish refreshed the committed marker"
    );
    let listed = create_workspace_snapshot_provider(config.clone())
        .list()
        .expect("#293 fix-r3 the provider lists the republished snapshot");
    assert_eq!(
        listed.len(),
        1,
        "#293 fix-r3 the recaptured snapshot is the only listed one: {listed:?}"
    );

    // The interrupted manifest must be overwritten by the next round.
    let recorder = ContentAddressedWorkspaceSnapshotRecorder::new(config);
    let result = recorder
        .capture_pre_user(&input("t2"))
        .expect("#293 fix-r3 the round over the interrupted manifest returns a result");
    assert_eq!(
        result.state,
        SnapshotState::Committed,
        "#293 fix-r3 an unmarked manifest residue is republished over, not trusted"
    );
    let manifest = fs::read_to_string(manifest_only.join("manifest.json"))
        .expect("#293 fix-r3 the overwritten manifest is readable");
    assert!(
        manifest.contains("\"runId\": \"run-r3\"")
            && manifest.contains(sha256_hex(b"s\0t2\0pre_user").as_str()),
        "#293 fix-r3 the overwrite replaced the interrupted manifest: {manifest}"
    );
    assert!(
        manifest_only.join("_COMMITTED").is_file(),
        "#293 fix-r3 the overwrite published the committed marker"
    );
}

/// #293 fix-r3 (A6.6-SNAPSHOT-CLEANUP-OWNERSHIP-001-R3): `.staging` is a
/// long-lived parent. No settle path may remove it: a best-effort removal
/// racing another round's `create_dir_all` (which creates the parent, then
/// retries the child once — std `DirBuilder::create_dir_all`) turns an
/// innocent capture into a permanent `_FAILED.json` (r3 review P1-B, hit
/// independently by two reviewers). Both settle paths — the committed round's
/// publish tail and the failed round's cleanup — must leave the parent in
/// place, empty. Its bounded reproduction is
/// `removing_the_staging_parent_between_mkdirs_fails_the_round` in the lib
/// tests.
#[test]
fn staging_parent_survives_round_settlement() {
    let run = Scratch::new();
    let snapshot_root = run.0.join("snapshots");
    fs::create_dir_all(&snapshot_root).expect("#293 fix-r3 create the snapshot root");
    let workspace = run.0.join("ws");
    fs::create_dir_all(&workspace).expect("#293 fix-r3 create the workspace");
    fs::write(workspace.join("only.txt"), b"x\n").expect("#293 fix-r3 seed the workspace");
    let gone = run.0.join("ws-gone");

    let input = |turn_id: &str, workspace_dir: &Path| WorkspaceSnapshotInput {
        workspace_id: "w".into(),
        session_id: "s".into(),
        turn_id: turn_id.into(),
        run_id: "run-r3".into(),
        workspace_dir: workspace_dir.to_string_lossy().into_owned(),
        workspace_stable: None,
        failure_kind: None,
        failure_reason: None,
    };
    let config = WorkspaceSnapshotConfig {
        root: run.0.to_string_lossy().into_owned(),
        snapshot_root: snapshot_root.to_string_lossy().into_owned(),
    };

    // A committed round: the publish tail must leave the parent in place.
    let committed = ContentAddressedWorkspaceSnapshotRecorder::new(config.clone())
        .capture_pre_user(&input("t1", &workspace))
        .expect("#293 fix-r3 the first round commits");
    assert_eq!(
        committed.state,
        SnapshotState::Committed,
        "#293 fix-r3 the first round commits"
    );
    assert!(
        snapshot_root.join(".staging").is_dir(),
        "#293 fix-r3 the staging parent survives a committed round"
    );
    assert!(
        fs::read_dir(snapshot_root.join(".staging"))
            .expect("#293 fix-r3 the staging parent is listable after the committed round")
            .flatten()
            .next()
            .is_none(),
        "#293 fix-r3 a committed round leaves no staging directory behind"
    );

    // A failed round (its workspace directory does not exist): the failure
    // cleanup must leave the parent in place too — this is the arm that used
    // to race another round's create_dir_all.
    let failed = ContentAddressedWorkspaceSnapshotRecorder::new(config)
        .capture_pre_user(&input("t2", &gone))
        .expect("#293 fix-r3 the second round returns a failed result, not a hard error");
    assert_eq!(
        failed.state,
        SnapshotState::Failed,
        "#293 fix-r3 the second round fails at the missing workspace directory"
    );
    assert!(
        snapshot_root.join(".staging").is_dir(),
        "#293 fix-r3 the staging parent survives a failed round"
    );
    assert!(
        fs::read_dir(snapshot_root.join(".staging"))
            .expect("#293 fix-r3 the staging parent is listable after the failed round")
            .flatten()
            .next()
            .is_none(),
        "#293 R2 removed its own staging directory, leaving the persistent parent empty"
    );
}

/// #293: a missing or non-array oracle field must fail loudly with the
/// dedicated message, not silently replay as an empty corpus. The old `arr`
/// folded both to `&[]`, so a truncated oracle (valid JSON, `scenarios`
/// dropped or a non-array value) replayed zero scenarios and passed.
#[test]
fn arr_panics_on_missing_or_non_array_oracle_fields() {
    for (value, context) in [
        (JsValue::Null, "scenarios"),
        (JsValue::Undefined, "configCases"),
        (object(&[("scenarios", JsValue::Number(27.0))]), "scenarios"),
    ] {
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = arr(field(&value, context), context);
        }))
        .expect_err("#293 a non-array oracle field must panic, not fold to empty");
        let message = panicked
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panicked.downcast_ref::<&str>().map(|text| text.to_string()))
            .unwrap_or_default();
        assert!(
            message.contains(&format!("#293 oracle field {context} is not an array")),
            "#293 the panic names the broken field: {message}"
        );
    }
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
    for manifest in arr(
        field(case, "manifests"),
        "hostBoundaryCases.unpairedSurrogateCollation.manifests",
    ) {
        let path = root.join(s(field(manifest, "path")));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, s(field(manifest, "text"))).unwrap();
    }
    let ts_order: Vec<String> = arr(
        field(case, "tsList"),
        "hostBoundaryCases.unpairedSurrogateCollation.tsList",
    )
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
    let first = &arr(
        field(case, "manifests"),
        "hostBoundaryCases.unpairedSurrogateCollation.manifests",
    )[0];
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
    for file in arr(field(spec, "files"), &format!("{name}: workspace.files")) {
        let path = workspace_dir.join(s(field(file, "path")));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, decode(&s(field(file, "b64")))).unwrap();
    }
    for empty in strs(
        field(spec, "emptyDirs"),
        &format!("{name}: workspace.emptyDirs"),
    ) {
        fs::create_dir_all(workspace_dir.join(empty)).unwrap();
    }
    for link in arr(
        field(spec, "symlinks"),
        &format!("{name}: workspace.symlinks"),
    ) {
        let path = workspace_dir.join(s(field(link, "path")));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(s(field(link, "target")), &path).unwrap();
    }
    let raw_files = arr(
        field(spec, "rawFiles"),
        &format!("{name}: workspace.rawFiles"),
    );
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
    let dynamic: HashSet<String> = strs(
        field(scenario, "dynamicIds"),
        &format!("{name}: dynamicIds"),
    )
    .into_iter()
    .collect();

    let steps = arr(field(scenario, "steps"), &format!("{name}: steps"));
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
                // Same ownership guard as remove/writeSnapshot: a step path
                // outside the run tree is refused before any mkdir/symlink.
                let path = owned_path(&root, &root.join(s(field(step, "path"))))
                    .expect("symlinkSnapshot path inside run")
                    .to_path_buf();
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                if fs::symlink_metadata(&path).is_ok() {
                    for recursive in [true, false] {
                        match remove_owned(&root, &path, recursive) {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => panic!("{context}: {e}"),
                        }
                    }
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
    let cases = arr(field(&oracle, "configCases"), "configCases");
    assert_eq!(
        cases.len(),
        7,
        "#322 config case set is pinned (lost: {})",
        cases.len()
    );
    for case in cases {
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
    assert_eq!(
        strs(field(cross, "dirs"), "cross.dirs").len(),
        41,
        "#322 cross dir set is pinned"
    );
    assert_eq!(
        arr(field(cross, "files"), "cross.files").len(),
        20,
        "#322 cross file set is pinned"
    );
    let scratch = Scratch::new();
    let root = scratch.0.join("ts-written");
    for dir in strs(field(cross, "dirs"), "cross.dirs") {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    for file in arr(field(cross, "files"), "cross.files") {
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
    for entry in arr(field(&value, "entries"), "cross.restoreTargetTree entries") {
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
    let expected = arr(field(cross, "listValue"), "cross.listValue")
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

#[test]
#[ignore = "requires fixed TS baseline and Node/tsx; run with PDRS_BASELINE"]
fn workspace_snapshot_fixture_regenerates_identically() {
    baseline::regenerates_identically("workspace-snapshot");
}

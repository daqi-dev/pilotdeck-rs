//! Content-addressed workspace snapshots, ported from TS
//! `src/storage/workspaceSnapshot.ts` (fixed baseline). Storage contract
//! with project resource composition in `runtime::project::persistence`.
//! This does not enable capture in the gateway turn or default proxy route.
//!
//! On-disk layout and bytes are the TS ones: `objects/sha256/<xy>/<zw>/<digest>`
//! object files deduplicated by content, and
//! `workspaces/<id>/sessions/<id>/snapshots/<sha256 of session\0turn\0phase>/`
//! partitions holding `manifest.json`, an empty `_COMMITTED`, or a pretty
//! `_FAILED.json`. Markers and manifests are written to a sibling temporary
//! and renamed into place; manifests are `JSON.stringify(value, null, 2)`
//! with no trailing newline.
//!
//! Known divergences and storage-contract notes (ledger entries are unapproved):
//! specs/gateway-parity/accepted-diffs.md A6.6-SNAPSHOT-IO-001 (its R1
//! correction in diffs/ supersedes the fallback shape described there) and
//! A6.6-SNAPSHOT-COLLATION-001; the #293 hardening entries live in
//! diffs/A6.6-SNAPSHOT-OBJECTKEY-001(-R1) and
//! diffs/A6.6-SNAPSHOT-CLEANUP-OWNERSHIP-001(-R1). Synchronous execution and
//! UTF-16 preservation below describe the contract, not independently
//! measured output differences.
//! - IO error text inside `_FAILED.json` uses the Node libuv wording from the
//!   measured table (`node_io`); errnos outside it are the explicit host
//!   boundary (`code: None`, empty message) per the node_io doctrine, never a
//!   Rust `io::Error` Display on the wire. Manifest JSON syntax errors use the
//!   V8 messages recorded by the CLI json-syntax fixture
//!   (`pilotdeck_protocol::json_syntax`).
//! - The recorder is synchronous, but in-flight key sharing matches TS: a
//!   hard failure of the round is delivered to every caller already waiting
//!   on that key, and is not cached for callers that arrive after the round
//!   settled (they retry, like the TS `finally` deletes `inFlight`).
//! - Strings read back from disk keep their raw UTF-16 units (`JsString`), so
//!   a hand-planted manifest or marker with lone surrogates lists and reads
//!   back exactly as TS does.
//! - `localeCompare` over an unpaired surrogate in a sorting key is an
//!   explicit host boundary (R19): ICU4X folds every unpaired surrogate to
//!   U+FFFD while Node keeps distinct weights, so `list()` returns an error
//!   instead of a silently different order when such a key actually has to be
//!   compared. All valid Unicode and U+FFFD still sort natively.
use super::{
    ledger::field,
    transcript::{timestamp, uuid},
};
use crate::{
    js_value::number_string,
    metadata::{js_trim, resolve as lexical_resolve, resolve_units},
    project_files::{has_unpaired_surrogate, node_collator, node_locale},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, symlink},
    },
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, PoisonError},
};

type Result<T> = std::result::Result<T, WorkspaceSnapshotError>;

// workspaceSnapshot.ts:20-34.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSnapshotConfig {
    pub root: String,
    pub snapshot_root: String,
}

/// workspaceSnapshot.ts:25-34. Missing or whitespace-only
/// `PILOTDECK_STORAGE_ROOT` disables snapshots; `PILOTDECK_SNAPSHOT_ROOT`
/// falls back to the storage root. Relative roots resolve against the
/// process cwd, and an unreadable cwd fails like TS `path.resolve`.
pub fn resolve_workspace_snapshot_config(
    env: impl Fn(&str) -> Option<String>,
) -> Result<Option<WorkspaceSnapshotConfig>> {
    let Some(root) = env("PILOTDECK_STORAGE_ROOT").map(|value| js_trim(&value).to_owned()) else {
        return Ok(None);
    };
    if root.is_empty() {
        return Ok(None);
    }
    let snapshot_root = env("PILOTDECK_SNAPSHOT_ROOT")
        .map(|value| js_trim(&value).to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| root.clone());
    Ok(Some(WorkspaceSnapshotConfig {
        root: resolve(&root)?,
        snapshot_root: resolve(&snapshot_root)?,
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotPhase {
    PreUser,
    PostAgent,
}
impl SnapshotPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreUser => "pre_user",
            Self::PostAgent => "post_agent",
        }
    }
    fn of_manifest(value: &JsValue) -> Option<Self> {
        match value.as_string()? {
            text if text.equals_str("pre_user") => Some(Self::PreUser),
            text if text.equals_str("post_agent") => Some(Self::PostAgent),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotState {
    Committed,
    Failed,
}
impl SnapshotState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Failed => "failed",
        }
    }
}

/// workspaceSnapshot.ts:43-52. `failure_kind` passes the wire string through
/// unvalidated, exactly as the TS runtime does (the union is compile-time
/// only); `workspace_stable` fails the capture only when explicitly `false`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSnapshotInput {
    pub workspace_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub run_id: String,
    pub workspace_dir: String,
    pub workspace_stable: Option<bool>,
    pub failure_kind: Option<String>,
    pub failure_reason: Option<String>,
}

/// workspaceSnapshot.ts:54-61. Paths are absolute; the JSON shape matches the
/// TS result object (key order included) via [`Self::as_json`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSnapshotResult {
    pub snapshot_id: String,
    pub phase: SnapshotPhase,
    pub state: SnapshotState,
    pub manifest_path: Option<String>,
    pub failure_marker_path: Option<String>,
    /// Raw UTF-16: a marker written by someone else may carry lone surrogates.
    pub error: Option<JsString>,
}
impl WorkspaceSnapshotResult {
    pub fn as_json(&self) -> JsValue {
        let mut out = JsObject::new();
        out.insert("snapshotId".into(), self.snapshot_id.as_str().into());
        out.insert("phase".into(), self.phase.as_str().into());
        out.insert("state".into(), self.state.as_str().into());
        if let Some(path) = &self.manifest_path {
            out.insert("manifestPath".into(), path.as_str().into());
        }
        if let Some(path) = &self.failure_marker_path {
            out.insert("failureMarkerPath".into(), path.as_str().into());
        }
        if let Some(error) = &self.error {
            out.insert("error".into(), JsValue::String(error.clone()));
        }
        JsValue::Object(out)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceSnapshotError {
    /// TS plain `Error`/`TypeError` outside the capture try block: identity
    /// rejections (`Invalid snapshot identity.`) and restore shape errors.
    Invalid(String),
    /// Manifest JSON parse failure: V8 `SyntaxError` wording.
    Syntax(String),
    /// Node-formatted filesystem diagnostic. `code` is the measured libuv
    /// errno code when [`crate::node_io::lookup`] recognized it; the fallback
    /// for an errno outside that table is `None` with an empty `message` (the
    /// host boundary, never a Rust Display string on the wire). Gateway
    /// methods must admit `code` through
    /// `node_io::lookup_for` before putting it on the wire; keeping the code
    /// here avoids re-parsing the message prefix.
    Io {
        code: Option<&'static str>,
        message: String,
    },
    /// `Snapshot <id> was not found.` (workspaceSnapshot.ts:114). The id is a
    /// raw JS string: a lone surrogate must not become U+FFFD in the message
    /// (the same identity rule `get_units`/`find_units` use).
    NotFound(JsString),
    /// workspaceSnapshot.ts:120.
    UnsafePath,
    /// workspaceSnapshot.ts:123.
    Escape,
    /// #293 security hardening (registered divergence
    /// A6.6-SNAPSHOT-OBJECTKEY-001): restore refuses an `objectKey` that
    /// resolves outside the snapshot root instead of reading it. TS resolves
    /// `objectKey` without any validation and copies root-external content
    /// into the target workspace (Node v22.22.0, probe
    /// `_tmp/20261026-issue-293/probe-a-objectkey.out`).
    ObjectKeyEscape,
    /// Node `ERR_INVALID_ARG_VALUE` from `fs.validatePath` (a NUL byte in a
    /// path argument). Only raised outside the capture try block, where TS
    /// rejects instead of writing a failed marker. The message keeps the raw
    /// UTF-16 units of the offending argument, so a lone surrogate next to
    /// the NUL stays `\ud800` instead of becoming U+FFFD.
    TypeError(JsString),
    /// R19 host boundary: the comparison needs Node's collator for unpaired
    /// surrogates. Callers wire this to the host instead of accepting a
    /// silently different order.
    HostRequired(String),
}
impl WorkspaceSnapshotError {
    /// True for errors that the host must answer because the native port
    /// cannot represent the input (R19 unpaired-surrogate collation).
    pub fn requires_host(&self) -> bool {
        matches!(self, Self::HostRequired(_))
    }
}
impl fmt::Display for WorkspaceSnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message)
            | Self::Syntax(message)
            | Self::HostRequired(message)
            | Self::Io { message, .. } => f.write_str(message),
            // Display cannot carry a lone surrogate; callers that serialize an
            // error to JSON use the raw `JsString` instead.
            Self::NotFound(message) | Self::TypeError(message) => {
                f.write_str(&String::from_utf16_lossy(message.units()))
            }
            Self::UnsafePath => f.write_str("Snapshot contains an unsafe relative path."),
            Self::Escape => f.write_str("Snapshot path escapes target workspace."),
            Self::ObjectKeyEscape => f.write_str("Snapshot object key escapes the snapshot root."),
        }
    }
}
impl std::error::Error for WorkspaceSnapshotError {}

// workspaceSnapshot.ts:68-85. `state` is the literal "committed" of list:
// only committed snapshots leave a manifest on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSnapshotDescriptor {
    pub snapshot_id: JsString,
    pub workspace_id: JsString,
    pub session_id: JsString,
    pub turn_id: JsString,
    pub run_id: JsString,
    pub phase: SnapshotPhase,
    /// Raw UTF-16 units: `localeCompare` sees the same string TS does,
    /// including any lone surrogate a hand-planted manifest may carry.
    pub created_at: Option<JsString>,
    pub manifest_path: String,
}
impl WorkspaceSnapshotDescriptor {
    pub fn as_json(&self) -> JsValue {
        let mut out = JsObject::new();
        out.insert(
            "snapshotId".into(),
            JsValue::String(self.snapshot_id.clone()),
        );
        out.insert(
            "workspaceId".into(),
            JsValue::String(self.workspace_id.clone()),
        );
        out.insert("sessionId".into(), JsValue::String(self.session_id.clone()));
        out.insert("turnId".into(), JsValue::String(self.turn_id.clone()));
        out.insert("runId".into(), JsValue::String(self.run_id.clone()));
        out.insert("phase".into(), self.phase.as_str().into());
        out.insert("state".into(), SnapshotState::Committed.as_str().into());
        if let Some(created) = &self.created_at {
            out.insert("createdAt".into(), JsValue::String(created.clone()));
        }
        out.insert("manifestPath".into(), self.manifest_path.as_str().into());
        JsValue::Object(out)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoredSnapshot {
    pub restored: bool,
    pub workspace_key: String,
}

/// workspaceSnapshot.ts:86-136 and 138-158. `list` walks every
/// `manifest.json` under the snapshot root in the same order Node's `readdir`
/// yields it, validates descriptor fields, and sorts by `createdAt`
/// descending with the pinned Node/ICU collation (`localeCompare`, stable
/// like `Array#sort`).
#[derive(Clone)]
pub struct WorkspaceSnapshotProvider {
    config: WorkspaceSnapshotConfig,
}

/// workspaceSnapshot.ts:86.
pub fn create_workspace_snapshot_provider(
    config: WorkspaceSnapshotConfig,
) -> WorkspaceSnapshotProvider {
    WorkspaceSnapshotProvider { config }
}

impl WorkspaceSnapshotProvider {
    /// workspaceSnapshot.ts:88-107.
    pub fn list(&self) -> Result<Vec<WorkspaceSnapshotDescriptor>> {
        let root = self.root()?;
        let mut manifests = Vec::new();
        walk_manifests(Path::new(&root), &mut manifests)?;
        let mut descriptors = Vec::new();
        for (manifest_path, manifest) in &manifests {
            if let Some(descriptor) = describe(manifest, manifest_path) {
                descriptors.push(descriptor);
            }
        }
        let locale = node_locale(|key| std::env::var(key).ok());
        let collator = node_collator(&locale)
            .map_err(|(_, message)| WorkspaceSnapshotError::Invalid(message))?;
        let keys: Vec<&[u16]> = descriptors
            .iter()
            .map(|descriptor| {
                descriptor
                    .created_at
                    .as_ref()
                    .map_or(&[][..], JsString::units)
            })
            .collect();
        // ICU4X folds every unpaired surrogate to U+FFFD, so it cannot tell
        // Node's distinct weights apart. Reject only when such a key really
        // takes part in a comparison: one descriptor (or one distinct key
        // everywhere) never calls Array#sort's comparator.
        let has_distinct_keys = keys
            .first()
            .is_some_and(|first| keys.iter().any(|key| key != first));
        let requires_host = has_distinct_keys
            && descriptors.iter().any(|descriptor| {
                descriptor
                    .created_at
                    .as_ref()
                    .is_some_and(has_unpaired_surrogate)
            });
        drop(keys);
        if requires_host {
            return Err(WorkspaceSnapshotError::HostRequired(
                "Snapshot createdAt collation requires the Node host for unpaired surrogates."
                    .into(),
            ));
        }
        descriptors.sort_by(|left, right| {
            collator.compare_utf16(
                right.created_at.as_ref().map_or(&[][..], JsString::units),
                left.created_at.as_ref().map_or(&[][..], JsString::units),
            )
        });
        Ok(descriptors)
    }

    /// workspaceSnapshot.ts:108-110 with 152-158: full walk, last match wins.
    pub fn get(&self, snapshot_id: &str) -> Result<Option<JsValue>> {
        self.get_units(&snapshot_id.encode_utf16().collect::<Vec<_>>())
    }

    /// Raw-UTF-16 form for callers that already hold the JS string. TS compares
    /// `manifest.snapshotId === snapshotId` by code unit, so a lone surrogate
    /// must never be replacement-decoded into another snapshot's id.
    pub fn get_units(&self, snapshot_id: &[u16]) -> Result<Option<JsValue>> {
        let Some((_, manifest)) = self.find_units(snapshot_id)? else {
            return Ok(None);
        };
        Ok(Some(manifest))
    }

    /// workspaceSnapshot.ts:112-134.
    pub fn restore(&self, snapshot_id: &str, target_root: &str) -> Result<RestoredSnapshot> {
        self.restore_units(&snapshot_id.encode_utf16().collect::<Vec<_>>(), target_root)
    }

    /// Raw-UTF-16 form of [`Self::restore`]; see [`Self::get_units`].
    pub fn restore_units(
        &self,
        snapshot_id: &[u16],
        target_root: &str,
    ) -> Result<RestoredSnapshot> {
        let Some((_, manifest)) = self.find_units(snapshot_id)? else {
            let mut units = "Snapshot ".encode_utf16().collect::<Vec<_>>();
            units.extend_from_slice(snapshot_id);
            units.extend(" was not found.".encode_utf16());
            return Err(WorkspaceSnapshotError::NotFound(JsString::from_utf16(
                units,
            )));
        };
        // workspaceSnapshot.ts:115: `Array.isArray(...) ? ... : []`. A
        // non-array `entries` (5, {}, "x", null) is a deliberate empty list,
        // not a TypeError; measured against the real provider, restore returns
        // Ok(restored: true) and creates only the destination directory.
        let entries = match field(&manifest, "entries") {
            JsValue::Array(entries) => entries.clone(),
            _ => Vec::new(),
        };
        let snapshot_root = self.root()?;
        let destination = resolve(target_root)?;
        // Node path.resolve keeps NUL bytes; fs.validatePath rejects them at
        // the first syscall, so mkdir(destination) is where it surfaces.
        validate_nul_path(&JsString::from(destination.as_str()), "path")?;
        fs::create_dir_all(&destination)
            .map_err(|e| io_error(&e, "mkdir", Some(Path::new(&destination))))?;
        for entry in &entries {
            // Property access on a null entry throws before the path checks.
            if matches!(entry, JsValue::Null) {
                return Err(WorkspaceSnapshotError::Invalid(
                    "Cannot read properties of null (reading 'path')".into(),
                ));
            }
            let Some(path) = field(entry, "path").as_string() else {
                return Err(WorkspaceSnapshotError::UnsafePath);
            };
            // TS checks the raw segments before resolving (workspaceSnapshot.ts:119).
            if path.units().first() == Some(&SLASH)
                || path.units().split(|unit| *unit == SLASH).any(is_dot_dot)
            {
                return Err(WorkspaceSnapshotError::UnsafePath);
            }
            // path.resolve and startsWith compare the original JS string; keep
            // its units so validatePath can quote a lone surrogate next to NUL.
            let output_units = resolve_path_units(&destination, path);
            if !units_within(&destination, output_units.units()) {
                return Err(WorkspaceSnapshotError::Escape);
            }
            let output = String::from_utf16_lossy(output_units.units());
            let output_path = PathBuf::from(&output);
            if let Some(parent_units) = parent_path_units(output_units.units()) {
                let parent = JsString::from_utf16(parent_units.to_vec());
                validate_nul_path(&parent, "path")?;
                let parent = PathBuf::from(String::from_utf16_lossy(parent.units()));
                fs::create_dir_all(&parent).map_err(|e| io_error(&e, "mkdir", Some(&parent)))?;
            }
            // workspaceSnapshot.ts:125/128 makes the type-specific field part of
            // the same guard: an entry whose entryType matches but whose
            // linkTarget/objectKey is missing is deliberately skipped, and
            // restore still resolves `restored:true`. The provider-quirks gold
            // sample pins both cases.
            let entry_type = field(entry, "entryType");
            if entry_type
                .as_string()
                .is_some_and(|t| t.equals_str("symlink"))
                && let Some(target) = field(entry, "linkTarget").as_string()
            {
                // TS: `await rm(output, { force: true }).catch(() => undefined)`
                // runs before `symlink(target, output)`, so an existing output
                // file is gone even when the target is rejected. The rm call's
                // own validatePath rejection is swallowed with the catch.
                let _ = fs::remove_file(&output_path);
                // Node symlink validates `target` before `path`.
                validate_nul_path(target, "target")?;
                validate_nul_path(&output_units, "path")?;
                let target = String::from_utf16_lossy(target.units());
                symlink(&target, &output_path).map_err(|e| {
                    two_path_message(&e, "symlink", Path::new(&target), Path::new(&output))
                })?;
            } else if entry_type.as_string().is_some_and(|t| t.equals_str("file"))
                && let Some(object_key) = field(entry, "objectKey").as_string()
            {
                let source_units = resolve_path_units(&snapshot_root, object_key);
                validate_nul_path(&source_units, "path")?;
                // #293 security hardening (registered divergence
                // A6.6-SNAPSHOT-OBJECTKEY-001, extended by its R1 correction):
                // TS resolves `objectKey` without validation and reads outside
                // the snapshot root (`resolve(config.snapshotRoot,
                // entry.objectKey)` then readFile, Node v22.22.0 probe
                // `_tmp/20261026-issue-293/probe-a-objectkey.out`); Rust
                // refuses the restore instead of copying root-external
                // content into the target workspace. The resolved units are
                // checked, like the destination side above, so an in-root
                // `sub/../x` still resolves and restores.
                if !units_within(&snapshot_root, source_units.units()) {
                    return Err(WorkspaceSnapshotError::ObjectKeyEscape);
                }
                let source_text = String::from_utf16_lossy(source_units.units());
                // #293 fix-r1 (A6.6-SNAPSHOT-OBJECTKEY-001-R1): the lexical
                // check above cannot see symlinks, and a canonicalize-then-
                // read would leave a swap-link race. The read below opens the
                // root once and then every remaining component with
                // O_NOFOLLOW, so a link in the tree — final component,
                // dangling, or a mid-directory link — is refused instead of
                // read, and the boundary check and the open are the same
                // syscall.
                let contents =
                    read_object_within_root(&snapshot_root, source_units.units(), &source_text)?;
                // writeFile(output) validates after readFile has succeeded,
                // exactly the order of the TS expression.
                validate_nul_path(&output_units, "path")?;
                write_file(Path::new(&output), &contents)?;
            }
        }
        Ok(RestoredSnapshot {
            restored: true,
            workspace_key: destination,
        })
    }

    fn root(&self) -> Result<String> {
        resolve(&self.config.snapshot_root)
    }

    /// workspaceSnapshot.ts:152-158: the walk visits manifests in Node readdir
    /// (libuv byte) order, so a duplicated snapshotId resolves to the last one
    /// seen. Exact UTF-16 comparison: `manifest.snapshotId === snapshotId`.
    fn find_units(&self, snapshot_id: &[u16]) -> Result<Option<(String, JsValue)>> {
        let root = self.root()?;
        let mut manifests = Vec::new();
        walk_manifests(Path::new(&root), &mut manifests)?;
        Ok(manifests.into_iter().rfind(|(_, manifest)| {
            field(manifest, "snapshotId")
                .as_string()
                .is_some_and(|id| id.units() == snapshot_id)
        }))
    }
}

/// workspaceSnapshot.ts:138-150. A missing or unreadable root lists nothing
/// (TS catches every readdir rejection); per-entry iteration failures skip
/// the entry, the closest synchronous mapping. Names come back in libuv
/// `uv_fs_scandir` order (`strcmp` on the raw bytes), not native directory
/// order; directories recurse in that same order and symlinks are never
/// followed (`lstat`).
fn walk_manifests(root: &Path, manifests: &mut Vec<(String, JsValue)>) -> Result<()> {
    let Ok(entries) = fs::read_dir(root) else {
        return Ok(());
    };
    let mut names: Vec<_> = entries.flatten().map(|entry| entry.file_name()).collect();
    names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    for raw_name in names {
        // libuv sorts the raw names, then Node decodes each one. Later
        // syscalls re-encode that lossy string, so a non-UTF-8 directory
        // name fails its own lstat (ENOENT) exactly as in TS.
        let name = raw_name.to_string_lossy().into_owned();
        let path = root.join(&name);
        let Ok(info) = fs::symlink_metadata(&path) else {
            continue;
        };
        if info.is_dir() {
            walk_manifests(&path, manifests)?;
        } else if name == "manifest.json" {
            let path_text = path.to_string_lossy().into_owned();
            let bytes = fs::read(&path).map_err(|error| {
                crate::node_io::read_file(&error, &path_text)
                    .map(|(code, message)| WorkspaceSnapshotError::Io {
                        code: Some(code),
                        message,
                    })
                    .unwrap_or_else(|| io_error(&error, "open", Some(&path)))
            })?;
            let text = String::from_utf8_lossy(&bytes);
            let manifest = parse_json(&text).map_err(|error| {
                WorkspaceSnapshotError::Syntax(
                    pilotdeck_protocol::json_syntax::parse_error(&text)
                        .unwrap_or_else(|| error.to_string()),
                )
            })?;
            if matches!(manifest, JsValue::Null) {
                // typeof manifest.snapshotId on null throws in TS.
                return Err(WorkspaceSnapshotError::Invalid(
                    "Cannot read properties of null (reading 'snapshotId')".into(),
                ));
            }
            manifests.push((path_text, manifest));
        }
    }
    Ok(())
}

/// workspaceSnapshot.ts:91-104 descriptor validation. Every string field keeps
/// its raw UTF-16 units: a hand-planted manifest may carry lone surrogates, and
/// `JSON.stringify` on the descriptor must reproduce them. Only their presence
/// (typeof string) and `createdAt` collation matter here.
fn describe(manifest: &JsValue, manifest_path: &str) -> Option<WorkspaceSnapshotDescriptor> {
    let text = |key: &str| field(manifest, key).as_string().cloned();
    Some(WorkspaceSnapshotDescriptor {
        snapshot_id: text("snapshotId")?,
        workspace_id: text("workspaceId")?,
        session_id: text("sessionId")?,
        turn_id: text("turnId")?,
        run_id: text("runId")?,
        phase: SnapshotPhase::of_manifest(field(manifest, "phase"))?,
        created_at: field(manifest, "createdAt").as_string().cloned(),
        manifest_path: manifest_path.to_owned(),
    })
}

/// workspaceSnapshot.ts:169-358.
pub struct ContentAddressedWorkspaceSnapshotRecorder {
    config: WorkspaceSnapshotConfig,
    /// TS `completed`/`inFlight` maps (workspaceSnapshot.ts:170-171); each
    /// in-flight entry carries the condition variable that stands in for
    /// awaiting the shared promise.
    state: Mutex<RecorderState>,
    /// Deterministic interleave seam (#293 fix-r1); `new` leaves it unset.
    hook: Option<SnapshotRoundHook>,
}

/// Deterministic seams used by the #293 cleanup-ownership interleave tests
/// (the same shape as the heartbeat pipeline's `HeartbeatStage`/`HeartbeatHook`).
/// Production callers construct with `new` and never install a hook; the
/// callback runs synchronously at the named point and may mutate the
/// partition the way a concurrent round would while this round sits between
/// two filesystem steps.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotRoundStage {
    /// After this round's manifest rename, before the committed marker.
    AfterManifestRename,
    /// After the committed-marker rename, before the temporary directory is
    /// removed (the last commit step).
    AfterCommittedMarker,
    /// At the failure cleanup, before any owned file is considered.
    BeforeFailureCleanup,
    /// After the owned identity was confirmed, immediately before deleting
    /// this round's own file (the old check-then-unlink window).
    BeforeOwnedUnlink,
}

/// See [`SnapshotRoundStage`].
#[doc(hidden)]
pub type SnapshotRoundHook = Arc<dyn Fn(SnapshotRoundStage) + Send + Sync>;

fn run_round_hook(hook: Option<&SnapshotRoundHook>, stage: SnapshotRoundStage) {
    if let Some(hook) = hook {
        hook(stage);
    }
}
#[derive(Default)]
struct RecorderState {
    completed: HashMap<String, WorkspaceSnapshotResult>,
    in_flight: HashMap<String, Arc<InFlight>>,
}
/// One `inFlight` promise. Waiters that joined this round read the full
/// `Result`, including a hard `Err`; the entry is removed when the round
/// settles, so later callers retry instead of replaying the failure.
#[derive(Default)]
struct InFlight {
    outcome: Mutex<Option<Result<WorkspaceSnapshotResult>>>,
    wake: Condvar,
}
enum CaptureEntry {
    Done(WorkspaceSnapshotResult),
    Leader(Arc<InFlight>),
    Follower(Arc<InFlight>),
}

/// Publishes a hard failure if the leader unwinds before `capture_once`
/// returns, so waiters never block on a round that cannot settle.
struct CaptureGuard<'a> {
    recorder: &'a ContentAddressedWorkspaceSnapshotRecorder,
    key: &'a str,
    pending: &'a Arc<InFlight>,
    settled: bool,
}
impl Drop for CaptureGuard<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.recorder.publish(
                self.key,
                self.pending,
                &Err(WorkspaceSnapshotError::Invalid(
                    "Snapshot capture panicked before publishing a result.".into(),
                )),
            );
        }
    }
}

impl ContentAddressedWorkspaceSnapshotRecorder {
    pub fn new(config: WorkspaceSnapshotConfig) -> Self {
        Self {
            config,
            state: Mutex::new(RecorderState::default()),
            hook: None,
        }
    }

    /// Test-only constructor with the deterministic interleave seam; see
    /// [`SnapshotRoundStage`]. Production keeps `new`.
    #[doc(hidden)]
    pub fn new_with_hook(config: WorkspaceSnapshotConfig, hook: SnapshotRoundHook) -> Self {
        Self {
            state: Mutex::new(RecorderState::default()),
            hook: Some(hook),
            ..Self::new(config)
        }
    }

    /// workspaceSnapshot.ts:175-177.
    pub fn capture_pre_user(
        &self,
        input: &WorkspaceSnapshotInput,
    ) -> Result<WorkspaceSnapshotResult> {
        self.capture(input, SnapshotPhase::PreUser)
    }

    /// workspaceSnapshot.ts:179-181.
    pub fn capture_post_agent(
        &self,
        input: &WorkspaceSnapshotInput,
    ) -> Result<WorkspaceSnapshotResult> {
        self.capture(input, SnapshotPhase::PostAgent)
    }

    /// workspaceSnapshot.ts:183-202.
    fn capture(
        &self,
        input: &WorkspaceSnapshotInput,
        phase: SnapshotPhase,
    ) -> Result<WorkspaceSnapshotResult> {
        let key = capture_key(&input.session_id, &input.turn_id, phase);
        match self.begin(&key) {
            CaptureEntry::Done(done) => Ok(done),
            CaptureEntry::Follower(pending) => Self::wait_in_flight(&pending),
            CaptureEntry::Leader(pending) => {
                // TS's `finally` deletes the in-flight entry on every exit,
                // panic included. Without this guard a panicking leader would
                // leave the entry (and every waiter) stuck forever.
                let mut guard = CaptureGuard {
                    recorder: self,
                    key: &key,
                    pending: &pending,
                    settled: false,
                };
                let outcome = self.capture_once(input, phase);
                self.publish(&key, &pending, &outcome);
                guard.settled = true;
                outcome
            }
        }
    }

    /// The synchronous `capture` prologue: `completed`, then an existing
    /// `inFlight`, then this caller becomes the leader for a new round.
    fn begin(&self, key: &str) -> CaptureEntry {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(done) = state.completed.get(key) {
            return CaptureEntry::Done(done.clone());
        }
        if let Some(pending) = state.in_flight.get(key) {
            return CaptureEntry::Follower(pending.clone());
        }
        let pending = Arc::new(InFlight::default());
        state.in_flight.insert(key.to_owned(), pending.clone());
        CaptureEntry::Leader(pending)
    }

    fn wait_in_flight(pending: &InFlight) -> Result<WorkspaceSnapshotResult> {
        let mut outcome = pending
            .outcome
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(result) = outcome.as_ref() {
                return result.clone();
            }
            outcome = pending
                .wake
                .wait(outcome)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// TS `completed.set(result)` + `finally inFlight.delete(key)`: fulfilled
    /// results are cached, rejections are only handed to this round's waiters.
    fn publish(&self, key: &str, pending: &InFlight, outcome: &Result<WorkspaceSnapshotResult>) {
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.in_flight.remove(key);
            if let Ok(done) = outcome {
                state.completed.insert(key.to_owned(), done.clone());
            }
        }
        *pending
            .outcome
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(outcome.clone());
        pending.wake.notify_all();
    }

    /// workspaceSnapshot.ts:204-281. Everything inside the TS try block
    /// fails into the `_FAILED.json` marker (`Err(String)` below); only
    /// identity rejection and marker-write failures escape as `Err`.
    fn capture_once(
        &self,
        input: &WorkspaceSnapshotInput,
        phase: SnapshotPhase,
    ) -> Result<WorkspaceSnapshotResult> {
        let key = capture_key(&input.session_id, &input.turn_id, phase);
        let snapshot_id = sha256_hex(key.as_bytes());
        let snapshot_root = resolve(&self.config.snapshot_root)?;
        let partition = Path::new(&snapshot_root)
            .join("workspaces")
            .join(safe_part(&input.workspace_id)?)
            .join("sessions")
            .join(safe_part(&input.session_id)?)
            .join("snapshots")
            .join(&snapshot_id);
        let manifest_path = partition.join("manifest.json");
        let committed_path = partition.join("_COMMITTED");
        let failure_marker_path = partition.join("_FAILED.json");
        let paths = CapturePaths {
            snapshot_root,
            partition,
            manifest_path,
            committed_path,
            failure_marker_path,
        };

        // workspaceSnapshot.ts:225-231: committed and failed snapshots return
        // from the markers without rescanning the workspace.
        if exists(&paths.committed_path) {
            return Ok(WorkspaceSnapshotResult {
                snapshot_id,
                phase,
                state: SnapshotState::Committed,
                manifest_path: Some(path_string(&paths.manifest_path)),
                failure_marker_path: None,
                error: None,
            });
        }
        if exists(&paths.failure_marker_path) {
            return Ok(WorkspaceSnapshotResult {
                snapshot_id,
                phase,
                state: SnapshotState::Failed,
                manifest_path: None,
                failure_marker_path: Some(path_string(&paths.failure_marker_path)),
                error: read_failure_message(&paths.failure_marker_path),
            });
        }

        // workspaceSnapshot.ts:233: the temporary directory is derived only
        // after the marker early-exits above, so a UUID failure cannot
        // preempt them.
        let temporary_dir = paths.partition.join(format!(
            ".tmp-{}",
            random_uuid().map_err(|_| host_boundary_io())?
        ));
        let mut markers = RoundMarkers::default();
        let committed = commit(
            input,
            phase,
            &snapshot_id,
            &paths,
            &temporary_dir,
            &mut markers,
            self.hook.as_ref(),
        );
        match committed {
            Ok(()) => Ok(WorkspaceSnapshotResult {
                snapshot_id,
                phase,
                state: SnapshotState::Committed,
                manifest_path: Some(path_string(&paths.manifest_path)),
                failure_marker_path: None,
                error: None,
            }),
            Err(message) => {
                // workspaceSnapshot.ts:260-280, #293 registered divergence
                // A6.6-SNAPSHOT-CLEANUP-OWNERSHIP-001: the cleanup removes
                // only the files this round itself wrote (inode ownership),
                // never a concurrent round's committed snapshot, which TS
                // unconditionally unlinks (Node v22.22.0 probe
                // `_tmp/20261026-issue-293/probe-b-concurrent.out`).
                // #293 fix-r1 (A6.6-SNAPSHOT-CLEANUP-OWNERSHIP-001-R1): the
                // ownership is captured when the round itself creates the
                // file, and the delete goes through a private graveyard name,
                // so no check-then-unlink window on the shared publish path
                // remains. The seam below lets a test hold the round here
                // while a concurrent round commits.
                run_round_hook(self.hook.as_ref(), SnapshotRoundStage::BeforeFailureCleanup);
                failure_cleanup(&markers, &temporary_dir, self.hook.as_ref());
                // This mkdir sits outside the TS catch: a NUL partition path
                // rejects the capture promise instead of writing a marker.
                validate_nul_path(&JsString::from(path_string(&paths.partition)), "path")?;
                fs::create_dir_all(&paths.partition)
                    .map_err(|e| io_error(&e, "mkdir", Some(&paths.partition)))?;
                let marker = marker_json(input, phase, &snapshot_id, &message);
                let text = stringify_pretty(&marker);
                atomic_marker(&paths.failure_marker_path, &text, None)?;
                Ok(WorkspaceSnapshotResult {
                    snapshot_id,
                    phase,
                    state: SnapshotState::Failed,
                    manifest_path: None,
                    failure_marker_path: Some(path_string(&paths.failure_marker_path)),
                    error: Some(JsString::from(message)),
                })
            }
        }
    }
}

/// The paths one capture derives up front (workspaceSnapshot.ts:211-223):
/// the resolved snapshot root, and inside it the partition the sanitized
/// identity selects, with the manifest and marker paths. The temporary
/// directory stays separate: workspaceSnapshot.ts:233 derives it only after
/// the marker early-exits in `capture_once`.
struct CapturePaths {
    snapshot_root: String,
    partition: PathBuf,
    manifest_path: PathBuf,
    committed_path: PathBuf,
    failure_marker_path: PathBuf,
}

/// #293: the partition files one capture round itself wrote, by inode
/// identity. The failure cleanup removes only these; a concurrent round of
/// the same key (its own recorder instance, so no in-flight sharing) may
/// have committed between this round's marker checks and its failure, and
/// its manifest/`_COMMITTED` must survive (registered divergence
/// A6.6-SNAPSHOT-CLEANUP-OWNERSHIP-001, its R1 correction for the capture
/// and delete protocol below).
#[derive(Default)]
struct RoundMarkers {
    manifest: Option<OwnedFile>,
    committed: Option<OwnedFile>,
}
struct OwnedFile {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl OwnedFile {
    /// The same identity under the shared publish path it was renamed to
    /// (rename preserves dev/ino, so the pre-rename capture stays valid).
    fn renamed_to(&self, path: &Path) -> OwnedFile {
        OwnedFile {
            path: path.to_owned(),
            dev: self.dev,
            ino: self.ino,
        }
    }
    fn is(&self, other: &OwnedFile) -> bool {
        self.dev == other.dev && self.ino == other.ino
    }
}

/// Inode identity of a path as it stands now.
fn owned_file(path: &Path) -> Option<OwnedFile> {
    let metadata = fs::symlink_metadata(path).ok()?;
    Some(OwnedFile {
        path: path.to_owned(),
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

/// Graveyard names are unique per attempt: pid across processes, a process
/// counter across recorder instances and repeated cleanups.
fn graveyard_name(path: &Path) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    path.with_file_name(format!(".tmp-grave-{}-{sequence}", std::process::id()))
}

/// Unlink only the file whose identity this round captured when it created
/// it — without a check-then-unlink window on the shared publish path
/// (#293 fix-r1): the path is first renamed into this round's private
/// graveyard name (same directory, so the same filesystem), the identity is
/// re-checked there, and only then deleted. A file another round renamed
/// over the path since is renamed back; if the path was re-occupied in the
/// meantime, the newest file wins like any rename-over publish (registered
/// residual, A6.6-SNAPSHOT-CLEANUP-OWNERSHIP-001-R1).
fn remove_if_owned(owned: Option<&OwnedFile>, hook: Option<&SnapshotRoundHook>) {
    let Some(file) = owned else {
        return;
    };
    let graveyard = graveyard_name(&file.path);
    // Move off the shared publish path first: whatever sits there goes to a
    // name only this round can reach, so the delete below can never hit a
    // concurrent round's file however it interleaves.
    if fs::rename(&file.path, &graveyard).is_err() {
        return; // already gone, or not movable: nothing provably ours to delete
    }
    if !owned_file(&graveyard).is_some_and(|current| current.is(file)) {
        // Someone else's publish sat at the path; put it back unless a newer
        // file has taken the path in the meantime.
        if fs::symlink_metadata(&file.path).is_err() {
            let _ = fs::rename(&graveyard, &file.path);
        }
        return;
    }
    run_round_hook(hook, SnapshotRoundStage::BeforeOwnedUnlink);
    let _ = fs::remove_file(&graveyard);
}

/// The TS catch-block deletions (workspaceSnapshot.ts:260-280), restricted to
/// this round's own writes. The temporary directory is uuid-private, so it
/// needs no ownership check.
fn failure_cleanup(markers: &RoundMarkers, temporary_dir: &Path, hook: Option<&SnapshotRoundHook>) {
    remove_if_owned(markers.manifest.as_ref(), hook);
    remove_if_owned(markers.committed.as_ref(), hook);
    let _ = fs::remove_dir_all(temporary_dir);
}

/// workspaceSnapshot.ts:234-259, the try block. Every failure becomes the
/// marker `error` message, Node-formatted. `markers` records the partition
/// files this round wrote, for the ownership-checked failure cleanup: each
/// identity is captured from the temporary file while this round still holds
/// the write handle (#293 fix-r1 — a stat of the shared path after the
/// rename could record a concurrent round's inode), and the rename that
/// publishes preserves the inode.
fn commit(
    input: &WorkspaceSnapshotInput,
    phase: SnapshotPhase,
    snapshot_id: &str,
    paths: &CapturePaths,
    temporary_dir: &Path,
    markers: &mut RoundMarkers,
    hook: Option<&SnapshotRoundHook>,
) -> std::result::Result<(), String> {
    if input.workspace_stable == Some(false) {
        return Err("Workspace did not become quiescent before snapshot capture.".into());
    }
    if let Some(message) = nul_path_message(
        &JsString::from(temporary_dir.to_string_lossy().as_ref()),
        "path",
    ) {
        return Err(String::from_utf16_lossy(message.units()));
    }
    fs::create_dir_all(temporary_dir).map_err(|e| io_message(&e, "mkdir", Some(temporary_dir)))?;
    let workspace_dir = resolve(&input.workspace_dir).map_err(|e| e.to_string())?;
    let entries = scan_workspace(&workspace_dir, &paths.snapshot_root)?;
    let manifest = manifest_json(input, phase, snapshot_id, entries)?;
    let text = stringify_pretty(&manifest);
    let temporary_manifest = temporary_dir.join("manifest.json");
    let owned_manifest =
        write_file_identified(&temporary_manifest, text.as_bytes()).map_err(|e| e.to_string())?;
    fs::create_dir_all(&paths.partition)
        .map_err(|e| io_message(&e, "mkdir", Some(&paths.partition)))?;
    fs::rename(&temporary_manifest, &paths.manifest_path).map_err(|e| {
        two_path_message(&e, "rename", &temporary_manifest, &paths.manifest_path).to_string()
    })?;
    // The seams sit between the publishing rename and the identity
    // bookkeeping so a test can land a concurrent round's rename exactly in
    // the window the identity capture has to be immune to (#293 fix-r1).
    run_round_hook(hook, SnapshotRoundStage::AfterManifestRename);
    markers.manifest = Some(owned_manifest.renamed_to(&paths.manifest_path));
    let committed_marker = atomic_marker(
        &paths.committed_path,
        "",
        hook.map(|hook| (hook, SnapshotRoundStage::AfterCommittedMarker)),
    )
    .map_err(|error| error.to_string())?;
    markers.committed = Some(committed_marker);
    // workspaceSnapshot.ts:258: rm(force) ignores a missing directory.
    match fs::remove_dir_all(temporary_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_message(&e, "rm", Some(temporary_dir))),
    }
    Ok(())
}

/// workspaceSnapshot.ts:283-314.
fn scan_workspace(
    workspace_dir: &str,
    snapshot_root: &str,
) -> std::result::Result<Vec<JsValue>, String> {
    // TS computes relative() (a pure string operation) before the first
    // filesystem call, so the root-equals-workspace rejection wins.
    if workspace_dir == snapshot_root {
        return Err("Snapshot root must not be the workspace root.".into());
    }
    // Node fs.validatePath rejects NUL before the syscall; this is the
    // TypeError message the capture try block turns into the marker error.
    if workspace_dir.contains('\0') {
        let message = crate::node_io::nul_message(&JsString::from(workspace_dir));
        return Err(String::from_utf16_lossy(message.units()));
    }
    let snapshot_root = Path::new(snapshot_root);
    let inside = workspace_is_parent(workspace_dir, snapshot_root);
    let mut entries = Vec::new();
    scan_visit(
        Path::new(workspace_dir),
        "",
        snapshot_root,
        inside,
        &mut entries,
    )?;
    Ok(entries)
}

/// One `visit(directory)` frame (workspaceSnapshot.ts:291-310). Names are the
/// UTF-8 lossy decoding Node's readdir returns, and later syscalls re-encode
/// that string, so a non-UTF-8 name fails its own lstat with ENOENT exactly
/// as in TS. `names.sort()` is the JS default: UTF-16 code units, stable.
fn scan_visit(
    directory: &Path,
    relative: &str,
    snapshot_root: &Path,
    inside: bool,
    entries: &mut Vec<JsValue>,
) -> std::result::Result<(), String> {
    let listed = fs::read_dir(directory).map_err(|e| io_message(&e, "scandir", Some(directory)))?;
    let mut names: Vec<String> = listed
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort_by(|left, right| utf16_cmp(left, right));
    for name in names {
        let absolute = directory.join(&name);
        if inside && absolute.starts_with(snapshot_root) {
            continue;
        }
        let path = if relative.is_empty() {
            name.clone()
        } else {
            format!("{relative}/{name}")
        };
        let info = fs::symlink_metadata(&absolute)
            .map_err(|e| io_message(&e, "lstat", Some(absolute.as_path())))?;
        let kind = info.file_type();
        if kind.is_symlink() {
            let target = fs::read_link(&absolute)
                .map_err(|e| io_message(&e, "readlink", Some(absolute.as_path())))?;
            let mut entry = JsObject::new();
            entry.insert("path".into(), path.as_str().into());
            entry.insert("entryType".into(), "symlink".into());
            entry.insert("size".into(), JsValue::Number(0.0));
            entry.insert(
                "linkTarget".into(),
                target.to_string_lossy().into_owned().as_str().into(),
            );
            entries.push(JsValue::Object(entry));
        } else if kind.is_dir() {
            scan_visit(&absolute, &path, snapshot_root, inside, entries)?;
        } else if kind.is_file() {
            let stored = store_file(&absolute, snapshot_root)?;
            let mut entry = JsObject::new();
            entry.insert("path".into(), path.as_str().into());
            entry.insert("entryType".into(), "file".into());
            entry.insert("size".into(), JsValue::Number(stored.size));
            entry.insert("digest".into(), stored.digest.as_str().into());
            entry.insert("objectKey".into(), stored.object_key.as_str().into());
            entries.push(JsValue::Object(entry));
        }
        // Other kinds (sockets, FIFOs, devices) produce no entry, as in TS.
    }
    Ok(())
}

struct StoredObject {
    digest: String,
    size: f64,
    object_key: String,
}

/// workspaceSnapshot.ts:316-357: stream the source through a sha256 transform
/// into an exclusively created temporary in `objects/sha256`, then rename to
/// the content-addressed name; an existing object wins and the temporary is
/// dropped. Neither TS nor this port fsyncs object files.
fn store_file(source: &Path, snapshot_root: &Path) -> std::result::Result<StoredObject, String> {
    let object_root = snapshot_root.join("objects").join("sha256");
    fs::create_dir_all(&object_root)
        .map_err(|e| io_message(&e, "mkdir", Some(object_root.as_path())))?;
    let temporary = object_root.join(format!(
        ".tmp-{}",
        // The uuid randomness TS never fails on is the host boundary here
        // too: an empty marker error, never the Rust Display.
        random_uuid().map_err(|_| host_boundary_io().to_string())?
    ));
    let attempt = || -> std::result::Result<StoredObject, String> {
        let mut input = File::open(source).map_err(|e| io_message(&e, "open", Some(source)))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| io_message(&e, "open", Some(temporary.as_path())))?;
        let mut hasher = Sha256::new();
        let mut size: u64 = 0;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = input
                .read(&mut buffer)
                .map_err(|e| io_message(&e, "read", None))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            size += read as u64;
            output
                .write_all(&buffer[..read])
                .map_err(|e| io_message(&e, "write", None))?;
        }
        let digest = hex_lower(&hasher.finalize());
        let object_key = format!("objects/sha256/{}/{}/{digest}", &digest[..2], &digest[2..4]);
        let object_path = snapshot_root.join(&object_key);
        if let Some(parent) = object_path.parent() {
            fs::create_dir_all(parent).map_err(|e| io_message(&e, "mkdir", Some(parent)))?;
        }
        if exists(&object_path) {
            fs::remove_file(&temporary)
                .map_err(|e| io_message(&e, "unlink", Some(temporary.as_path())))?;
            return Ok(StoredObject {
                digest,
                size: size as f64,
                object_key,
            });
        }
        match fs::rename(&temporary, &object_path) {
            Ok(()) => {}
            // workspaceSnapshot.ts:346-351: a lost rename race keeps the
            // object the winner already wrote.
            Err(error) => {
                if !exists(&object_path) {
                    return Err(
                        two_path_message(&error, "rename", &temporary, &object_path).to_string()
                    );
                }
                let _ = fs::remove_file(&temporary);
            }
        }
        Ok(StoredObject {
            digest,
            size: size as f64,
            object_key,
        })
    };
    let result = attempt();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// workspaceSnapshot.ts:240-252 key order; undefined failure fields drop out
/// of the JSON text, matching `JSON.stringify`. The timestamp never fails in
/// TS (`new Date()`), so its Rust-only failure is the marker message.
/// Ledger: specs/gateway-parity/accepted-diffs.md A6.6-SNAPSHOT-CLOCK-001 (unapproved).
fn manifest_json(
    input: &WorkspaceSnapshotInput,
    phase: SnapshotPhase,
    snapshot_id: &str,
    entries: Vec<JsValue>,
) -> std::result::Result<JsValue, String> {
    let mut manifest = JsObject::new();
    manifest.insert("snapshotId".into(), snapshot_id.into());
    manifest.insert("workspaceId".into(), input.workspace_id.as_str().into());
    manifest.insert("sessionId".into(), input.session_id.as_str().into());
    manifest.insert("turnId".into(), input.turn_id.as_str().into());
    manifest.insert("runId".into(), input.run_id.as_str().into());
    manifest.insert("phase".into(), phase.as_str().into());
    manifest.insert(
        "abnormal".into(),
        JsValue::Bool(matches!(phase, SnapshotPhase::PostAgent)),
    );
    if let Some(kind) = &input.failure_kind {
        manifest.insert("failureKind".into(), kind.as_str().into());
    }
    if let Some(reason) = &input.failure_reason {
        manifest.insert("failureReason".into(), reason.as_str().into());
    }
    manifest.insert("entries".into(), JsValue::Array(entries));
    manifest.insert("createdAt".into(), JsValue::String(created_at()?));
    Ok(JsValue::Object(manifest))
}

/// workspaceSnapshot.ts:266-278 key order.
fn marker_json(
    input: &WorkspaceSnapshotInput,
    phase: SnapshotPhase,
    snapshot_id: &str,
    message: &str,
) -> JsValue {
    let mut marker = JsObject::new();
    marker.insert("snapshotId".into(), snapshot_id.into());
    marker.insert("workspaceId".into(), input.workspace_id.as_str().into());
    marker.insert("sessionId".into(), input.session_id.as_str().into());
    marker.insert("turnId".into(), input.turn_id.as_str().into());
    marker.insert("runId".into(), input.run_id.as_str().into());
    marker.insert("phase".into(), phase.as_str().into());
    if let Some(kind) = &input.failure_kind {
        marker.insert("failureKind".into(), kind.as_str().into());
    }
    if let Some(reason) = &input.failure_reason {
        marker.insert("failureReason".into(), reason.as_str().into());
    }
    marker.insert("state".into(), SnapshotState::Failed.as_str().into());
    marker.insert("error".into(), message.into());
    marker.insert(
        "createdAt".into(),
        JsValue::String(created_at().unwrap_or_default()),
    );
    JsValue::Object(marker)
}

/// workspaceSnapshot.ts:360-364. The message names the failing stage; uuid
/// randomness never fails in TS. Returns the marker's inode identity as
/// captured from the temporary file before the publishing rename (#293
/// fix-r1), so a failing round can prove what it itself wrote.
fn atomic_marker(
    path: &Path,
    content: &str,
    seam: Option<(&SnapshotRoundHook, SnapshotRoundStage)>,
) -> Result<OwnedFile> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = path.with_file_name(format!(
        "{name}.tmp-{}",
        random_uuid().map_err(|_| host_boundary_io())?
    ));
    let owned = write_file_identified(&temporary, content.as_bytes())?;
    fs::rename(&temporary, path).map_err(|e| two_path_message(&e, "rename", &temporary, path))?;
    if let Some((hook, stage)) = seam {
        run_round_hook(Some(hook), stage);
    }
    Ok(owned.renamed_to(path))
}

/// workspaceSnapshot.ts:366-373: any read or parse failure yields no error,
/// and only a string `error` field is returned. The value keeps its raw UTF-16
/// units (a marker can carry lone surrogates).
fn read_failure_message(path: &Path) -> Option<JsString> {
    let bytes = fs::read(path).ok()?;
    let value = parse_json(&String::from_utf8_lossy(&bytes)).ok()?;
    field(&value, "error").as_string().cloned()
}

/// workspaceSnapshot.ts:375-376 (`access` follows symlinks).
fn exists(path: &Path) -> bool {
    fs::metadata(path).is_ok()
}

/// workspaceSnapshot.ts:379-387.
fn safe_part(value: &str) -> Result<String> {
    let invalid = || WorkspaceSnapshotError::Invalid("Invalid snapshot identity.".into());
    let raw = js_trim(value);
    if raw.is_empty()
        || raw.contains('\0')
        || raw
            .split(['\\', '/'])
            .any(|part| part == "." || part == "..")
    {
        return Err(invalid());
    }
    // /[^A-Za-z0-9_.-]+/ → "-", one dash per maximal run.
    let mut replaced = String::with_capacity(raw.len());
    let mut dashes = false;
    for character in raw.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-') {
            replaced.push(character);
            dashes = false;
        } else if !dashes {
            replaced.push('-');
            dashes = true;
        }
    }
    // /^-+|-+$/g → "".
    let safe = replaced.trim_matches('-');
    if safe.is_empty() || safe == "." || safe == ".." {
        return Err(invalid());
    }
    Ok(safe.to_owned())
}

// ------------------------------------------------------------------ helpers

/// `${sessionId}\0${turnId}\0${phase}` (workspaceSnapshot.ts:187, 208-210).
fn capture_key(session_id: &str, turn_id: &str, phase: SnapshotPhase) -> String {
    format!("{session_id}\0{turn_id}\0{}", phase.as_str())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_lower(&hasher.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 15)]));
    }
    out
}

/// Node `path.resolve` for a single segment: absolute inputs normalize
/// lexically, relative ones join the process cwd first. Unlike Node, the cwd
/// is re-read per call instead of cached (no fixture depends on a deleted
/// cwd).
fn resolve(value: &str) -> Result<String> {
    let cwd = if value.starts_with('/') {
        "/".to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| io_error(&e, "uv_cwd", None))?
            .to_string_lossy()
            .into_owned()
    };
    Ok(lexical_resolve(Path::new(&cwd), value)
        .to_string_lossy()
        .into_owned())
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

const SLASH: u16 = b'/' as u16;
const DOT: u16 = b'.' as u16;

fn resolve_path_units(base: &str, value: &JsString) -> JsString {
    resolve_units(base, value)
}

fn is_dot_dot(segment: &[u16]) -> bool {
    segment.len() == 2 && segment[0] == DOT && segment[1] == DOT
}

/// Lexical containment shared by both restore boundary checks
/// (workspaceSnapshot.ts:124's `startsWith(`${destination}${sep}`)`, kept
/// unit-for-unit): the candidate is inside `base` when it is the base itself
/// or starts with `base + '/'`. A base of exactly `/` keeps TS's `//`
/// spelling (Node v22.22.0 probe
/// `_tmp/20261026-issue-293/fix-r1/probe-root-prefix.out`), so nothing
/// resolves inside it.
fn units_within(base: &str, candidate: &[u16]) -> bool {
    let base_units: Vec<u16> = base.encode_utf16().collect();
    if candidate == base_units.as_slice() {
        return true;
    }
    let mut prefix = base_units;
    prefix.push(SLASH);
    candidate.starts_with(&prefix)
}

/// The `readFile` diagnostic shape behind every object read (open errors name
/// the path, an EISDIR surfaces at the read stage like Node's readFile).
fn read_object_error(error: &io::Error, source_text: &str) -> WorkspaceSnapshotError {
    crate::node_io::read_file(error, source_text)
        .map(|(code, message)| WorkspaceSnapshotError::Io {
            code: Some(code),
            message,
        })
        .unwrap_or_else(|| io_error(error, "open", Some(Path::new(source_text))))
}

/// True when an openat failure under O_NOFOLLOW means the component is a
/// symlink: ELOOP directly, or ENOTDIR with a NOFOLLOW stat of that name on
/// the pinned parent proving one (the same classification as
/// `project_files::leaf_open_error`).
#[cfg(unix)]
fn symlink_component(parent: &File, name: &std::ffi::OsStr, error: &io::Error) -> bool {
    use rustix::fs::{AtFlags, FileType, statat};
    match error.raw_os_error() {
        Some(libc::ELOOP) => true,
        Some(libc::ENOTDIR) => statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
            .map(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::Symlink)
            .unwrap_or(false),
        _ => false,
    }
}

/// #293 fix-r1 (registered divergence A6.6-SNAPSHOT-OBJECTKEY-001-R1): read
/// the object behind a lexically in-root objectKey without following any
/// symlink component of the key. The snapshot root is the deployment's own
/// configured path (followed like every other snapshot operation here); each
/// remaining component is opened relative to the previous one with
/// O_NOFOLLOW — a link inside the tree is refused with
/// [`WorkspaceSnapshotError::ObjectKeyEscape`] whatever it points at
/// (outside the root, dangling, or in-root: the real target of a link is
/// not the key's own path), and no gap remains between the boundary check
/// and the open. NONBLOCK keeps a raced-in FIFO from hanging the read.
#[cfg(unix)]
fn read_object_within_root(
    snapshot_root: &str,
    source_units: &[u16],
    source_text: &str,
) -> Result<Vec<u8>> {
    use rustix::fs::{Mode, OFlags, openat};
    use std::path::Component;
    let root_units: Vec<u16> = snapshot_root.encode_utf16().collect();
    let io = |error: io::Error| read_object_error(&error, source_text);
    if source_units == root_units.as_slice() {
        // The key resolves to the root itself: keep Node's readFile of a
        // directory (EISDIR at the read stage).
        return fs::read(snapshot_root).map_err(io);
    }
    let relative = String::from_utf16_lossy(&source_units[root_units.len() + 1..]);
    let mut directory = File::open(snapshot_root).map_err(io)?;
    let components: Vec<_> = Path::new(&relative).components().collect();
    for (index, component) in components.iter().enumerate() {
        // The lexical resolve above already folded `.`/`..`; anything else
        // cannot name an object (defensive refusal, not a measured error).
        let Component::Normal(name) = component else {
            return Err(WorkspaceSnapshotError::ObjectKeyEscape);
        };
        let last = index + 1 == components.len();
        let mut flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
        if last {
            flags |= OFlags::NONBLOCK;
        } else {
            flags |= OFlags::DIRECTORY;
        }
        let opened = openat(&directory, *name, flags, Mode::empty())
            .map_err(io::Error::from)
            .map_err(|error| {
                if symlink_component(&directory, name, &error) {
                    WorkspaceSnapshotError::ObjectKeyEscape
                } else {
                    io(error)
                }
            })?;
        let mut file = File::from(opened);
        if last {
            let mut contents = Vec::new();
            file.read_to_end(&mut contents).map_err(io)?;
            return Ok(contents);
        }
        directory = file;
    }
    // The prefix check guarantees at least one component below the root.
    Err(WorkspaceSnapshotError::ObjectKeyEscape)
}

/// Non-Unix keeps the measured Node read (unverified), like
/// `project_files::open_beneath`'s fallback.
#[cfg(not(unix))]
fn read_object_within_root(
    _snapshot_root: &str,
    _source_units: &[u16],
    source_text: &str,
) -> Result<Vec<u8>> {
    fs::read(source_text).map_err(|error| read_object_error(&error, source_text))
}

/// `Path::parent` on a resolved absolute path, without decoding the units.
fn parent_path_units(path: &[u16]) -> Option<&[u16]> {
    let slash = path.iter().rposition(|unit| *unit == SLASH)?;
    if slash == 0 {
        (path.len() > 1).then(|| &path[..1])
    } else {
        Some(&path[..slash])
    }
}

/// Node `fs.validatePath`: a NUL byte rejects before any syscall with
/// `ERR_INVALID_ARG_VALUE`, naming the fs call's own parameter (`path` for
/// mkdir/readFile/writeFile, `target` for symlink's first argument).
fn nul_path_message(path: &JsString, argument: &str) -> Option<JsString> {
    if !path.units().contains(&0) {
        return None;
    }
    Some(crate::node_io::nul_message_for(path, argument))
}

/// The rejection form of [`nul_path_message`] for call sites that must throw
/// (restore's public shape) rather than turn the message into a failed marker.
fn validate_nul_path(path: &JsString, argument: &str) -> Result<()> {
    match nul_path_message(path, argument) {
        Some(message) => Err(WorkspaceSnapshotError::TypeError(message)),
        None => Ok(()),
    }
}

/// `relative(workspaceDir, snapshotRoot)` is neither "" nor ".."-prefixed
/// exactly when the snapshot root sits strictly inside the workspace
/// (workspaceSnapshot.ts:284-288); both paths are resolved here.
fn workspace_is_parent(workspace_dir: &str, snapshot_root: &Path) -> bool {
    let workspace_dir = Path::new(workspace_dir);
    snapshot_root.starts_with(workspace_dir) && snapshot_root != workspace_dir
}

/// JS default string ordering: UTF-16 code units (workspaceSnapshot.ts:293).
fn utf16_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

/// Node-formatted IO diagnostic from the measured libuv table. An errno
/// outside that table is the explicit host boundary (`code: None`, empty
/// message — `project_error` answers `NODE_IO_REQUIRED`), never a Rust
/// Display string on the wire (node_io.rs doctrine); the measured code
/// travels alongside the message so gateway method admission never has to
/// parse the text prefix again.
fn io_error(error: &io::Error, syscall: &str, path: Option<&Path>) -> WorkspaceSnapshotError {
    let text = path.map(|path| path.to_string_lossy().into_owned());
    match crate::node_io::diagnostic(error, syscall, text.as_deref()) {
        Some((code, message)) => WorkspaceSnapshotError::Io {
            code: Some(code),
            message,
        },
        None => host_boundary_io(),
    }
}

/// The shared host-boundary fallback (`code: None`, empty message) for an
/// errno outside the measured table — and for the uuid randomness TS never
/// fails on. One construction site, one explanation.
fn host_boundary_io() -> WorkspaceSnapshotError {
    WorkspaceSnapshotError::Io {
        code: None,
        message: String::new(),
    }
}

/// `writeFile` (node:fs/promises) behind the snapshot writes: an open error
/// names its path, a descriptor write error (ENOSPC, EFBIG, ...) does not.
/// `fs::write` would conflate the two into the open label.
fn write_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = fs::File::create(path).map_err(|e| io_error(&e, "open", Some(path)))?;
    file.write_all(contents)
        .map_err(|e| io_error(&e, "write", None))
}

/// [`write_file`] plus the inode identity of the file just written, taken
/// from the handle this round still holds (fstat: no path race). rename
/// preserves the identity, so the caller may record it as the identity of
/// the shared path it publishes to (#293 fix-r1).
fn write_file_identified(path: &Path, contents: &[u8]) -> Result<OwnedFile> {
    let mut file = fs::File::create(path).map_err(|e| io_error(&e, "open", Some(path)))?;
    file.write_all(contents)
        .map_err(|e| io_error(&e, "write", None))?;
    let metadata = file
        .metadata()
        .map_err(|e| io_error(&e, "fstat", Some(path)))?;
    Ok(OwnedFile {
        path: path.to_owned(),
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

/// Capture-path helper: the failure marker stores only the message, so the
/// structured code is not needed there.
fn io_message(error: &io::Error, syscall: &str, path: Option<&Path>) -> String {
    io_error(error, syscall, path).to_string()
}

/// Node two-path syscall message (`rename 'a' -> 'b'`, `symlink 'a' -> 'b'`),
/// built on the shared `node_io::diagnostic` like `skills::error`'s `io_pair`
/// (no second formatter); an errno outside the measured table is the host
/// boundary (`code: None`, empty message), never a Rust Display string.
fn two_path_message(
    error: &io::Error,
    syscall: &str,
    from: &Path,
    to: &Path,
) -> WorkspaceSnapshotError {
    match crate::node_io::diagnostic(error, syscall, Some(&from.to_string_lossy())) {
        Some((code, message)) => WorkspaceSnapshotError::Io {
            code: Some(code),
            message: format!("{message} -> '{}'", to.display()),
        },
        None => host_boundary_io(),
    }
}

fn created_at() -> std::result::Result<JsString, String> {
    timestamp().map(JsString::from).map_err(|e| e.to_string())
}

fn random_uuid() -> std::result::Result<String, io::Error> {
    uuid()
}

// ------------------------------------------------------- JSON.stringify(,2)

/// `JSON.stringify(value, null, 2)`: two-space indent, `": "` between key
/// and value, no trailing newline, `[]`/`{}` stay closed, object `undefined`
/// values drop out and array `undefined` becomes `null`. Number text and
/// string quoting follow the compact codec (ryu-js, lowercase `\u` escapes,
/// lone surrogates escaped); keys keep insertion order, which is the
/// property order of every object built here.
fn stringify_pretty(value: &JsValue) -> String {
    let mut out = String::new();
    write_pretty(value, 0, &mut out);
    out
}

fn write_pretty(value: &JsValue, depth: usize, out: &mut String) {
    match value {
        JsValue::Undefined | JsValue::Null => out.push_str("null"),
        JsValue::Bool(bit) => out.push_str(if *bit { "true" } else { "false" }),
        JsValue::Number(number) => out.push_str(&number_string(*number)),
        JsValue::String(text) => quoted(text, out),
        JsValue::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                indent(depth + 1, out);
                write_pretty(item, depth + 1, out);
            }
            out.push('\n');
            indent(depth, out);
            out.push(']');
        }
        JsValue::Object(object) => {
            let entries: Vec<&(JsString, JsValue)> = object
                .entries()
                .iter()
                .filter(|(_, value)| !value.is_undefined())
                .collect();
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (index, (key, item)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                indent(depth + 1, out);
                quoted(key, out);
                out.push_str(": ");
                write_pretty(item, depth + 1, out);
            }
            out.push('\n');
            indent(depth, out);
            out.push('}');
        }
    }
}

fn indent(depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// JSON string quoting shared with the protocol codec; the manifest and
/// `_FAILED.json` bytes must stay identical, including lone surrogates.
fn quoted(value: &JsString, out: &mut String) {
    out.push_str(&pilotdeck_protocol::quote_json_string(value));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    /// Child half of `write_file_reports_the_write_stage_like_node`.
    #[cfg(unix)]
    #[test]
    #[ignore = "spawned by write_file_reports_the_write_stage_like_node"]
    fn write_file_child_over_file_size_limit() {
        let Ok(path) = std::env::var(crate::test_write_stage::CHILD_PATH_ENV) else {
            panic!(
                "{} not set; spawn via write_file_reports_the_write_stage_like_node",
                crate::test_write_stage::CHILD_PATH_ENV
            );
        };
        crate::test_write_stage::ignore_sigxfsz_like_gateway();
        let error = write_file(std::path::Path::new(&path), &[b'a'; 4096])
            .expect_err("a write past RLIMIT_FSIZE must fail");
        println!("WRITE_ERROR={error}");
    }

    /// The snapshot writes go through `writeFile` (node:fs/promises): a
    /// descriptor write error must read like Node's `EFBIG: file too large,
    /// write` with no path (node v22.22.0,
    /// `_tmp/20261025-issue-656/node-probe/write-stages.out`), not the open
    /// label with the temporary path.
    #[cfg(unix)]
    #[test]
    fn write_file_reports_the_write_stage_like_node() {
        let scratch = crate::test_scratch::Scratch::new("snapshot-write-stage");
        let target = scratch.path().join("big.txt");
        let line = crate::test_write_stage::child_write_error(
            "storage::workspace_snapshot::tests::write_file_child_over_file_size_limit",
            1,
            &target,
        );
        assert_eq!(line, "EFBIG: file too large, write");
    }

    /// workspaceSnapshot.ts:190-201: callers already waiting on the same key
    /// receive the round's rejection. After `finally` deletes `inFlight`, a
    /// later call retries instead of replaying the cached failure.
    #[test]
    fn hard_failure_is_shared_with_waiters_and_not_cached() {
        let recorder = ContentAddressedWorkspaceSnapshotRecorder::new(WorkspaceSnapshotConfig {
            root: "/tmp/ws-snapshot-test-root".into(),
            snapshot_root: "/tmp/ws-snapshot-test-snapshots".into(),
        });
        let key = "s\0t\0pre_user";
        let CaptureEntry::Leader(pending) = recorder.begin(key) else {
            panic!("first caller must lead");
        };
        let entered = Arc::new(AtomicBool::new(false));
        thread::scope(|scope| {
            let follower = {
                let recorder = &recorder;
                let entered = entered.clone();
                scope.spawn(move || match recorder.begin(key) {
                    CaptureEntry::Follower(pending) => {
                        entered.store(true, Ordering::SeqCst);
                        ContentAddressedWorkspaceSnapshotRecorder::wait_in_flight(&pending)
                    }
                    _ => panic!("second caller must join the in-flight round"),
                })
            };
            while !entered.load(Ordering::SeqCst) {
                thread::yield_now();
            }
            let failure: Result<WorkspaceSnapshotResult> = Err(WorkspaceSnapshotError::Invalid(
                "Invalid snapshot identity.".into(),
            ));
            recorder.publish(key, &pending, &failure);
            assert_eq!(follower.join().unwrap(), failure);
        });
        assert!(matches!(recorder.begin(key), CaptureEntry::Leader(_)));
    }

    /// A panicking leader never reaches `publish`; the drop guard must still
    /// settle the round so waiters do not block forever (review r2 P2).
    #[test]
    fn panicking_leader_publishes_failure_to_waiters() {
        let recorder = ContentAddressedWorkspaceSnapshotRecorder::new(WorkspaceSnapshotConfig {
            root: "/tmp/ws-snapshot-test-root".into(),
            snapshot_root: "/tmp/ws-snapshot-test-snapshots".into(),
        });
        let key = "s\0t\0pre_user";
        let CaptureEntry::Leader(pending) = recorder.begin(key) else {
            panic!("first caller must lead");
        };
        let entered = Arc::new(AtomicBool::new(false));
        thread::scope(|scope| {
            let follower = {
                let recorder = &recorder;
                let entered = entered.clone();
                scope.spawn(move || match recorder.begin(key) {
                    CaptureEntry::Follower(pending) => {
                        entered.store(true, Ordering::SeqCst);
                        ContentAddressedWorkspaceSnapshotRecorder::wait_in_flight(&pending)
                    }
                    _ => panic!("second caller must join the in-flight round"),
                })
            };
            while !entered.load(Ordering::SeqCst) {
                thread::yield_now();
            }
            let panicked = catch_unwind(AssertUnwindSafe(|| {
                let _guard = CaptureGuard {
                    recorder: &recorder,
                    key,
                    pending: &pending,
                    settled: false,
                };
                panic!("simulated capture panic");
            }));
            assert!(panicked.is_err());
            let error = follower.join().unwrap().expect_err("waiter must be woken");
            assert_eq!(
                error.to_string(),
                "Snapshot capture panicked before publishing a result."
            );
        });
        assert!(matches!(recorder.begin(key), CaptureEntry::Leader(_)));
    }

    /// #293: `two_path_message` builds on the shared `node_io::diagnostic`
    /// (like `skills::error`'s `io_pair`, no second formatter); an errno
    /// outside the measured table is the host boundary (code None, empty
    /// message), never a Rust Display string on the wire.
    #[test]
    fn two_path_message_uses_the_shared_diagnostic() {
        let error = two_path_message(
            &io::Error::from_raw_os_error(libc::ENOENT),
            "rename",
            Path::new("/tmp/pdrs-from"),
            Path::new("/tmp/pdrs-to"),
        );
        assert!(
            matches!(
                &error,
                WorkspaceSnapshotError::Io {
                    code: Some("ENOENT"),
                    ..
                }
            ),
            "#293 in-table errno keeps its code: {error:?}"
        );
        assert_eq!(
            error.to_string(),
            "ENOENT: no such file or directory, rename '/tmp/pdrs-from' -> '/tmp/pdrs-to'",
            "#293 two-path rename message"
        );
        let error = two_path_message(
            &io::Error::from_raw_os_error(libc::EEXIST),
            "symlink",
            Path::new("target"),
            Path::new("/tmp/pdrs-link"),
        );
        assert_eq!(
            error.to_string(),
            "EEXIST: file already exists, symlink 'target' -> '/tmp/pdrs-link'",
            "#293 the destination suffix is part of the message"
        );
        // EDQUOT is outside the measured table (node_io's own test): host
        // boundary, empty message, never the Rust Display.
        let error = two_path_message(
            &io::Error::from_raw_os_error(libc::EDQUOT),
            "rename",
            Path::new("/tmp/pdrs-from"),
            Path::new("/tmp/pdrs-to"),
        );
        assert!(
            matches!(&error, WorkspaceSnapshotError::Io { code: None, message } if message.is_empty()),
            "#293 out-of-table errno is the empty-message host boundary: {error:?}"
        );
        assert_eq!(error.to_string(), "", "#293 no Rust Display leaks");
        // The single-path helper shares the same fallback (sibling of the
        // same pattern, changed with it).
        let error = io_error(
            &io::Error::from_raw_os_error(libc::EDQUOT),
            "mkdir",
            Some(Path::new("/tmp/pdrs-dir")),
        );
        assert!(
            matches!(&error, WorkspaceSnapshotError::Io { code: None, message } if message.is_empty()),
            "#293 io_error's out-of-table errno is the same host boundary: {error:?}"
        );
        assert_eq!(error.to_string(), "", "#293 no Rust Display leaks");
    }

    /// #293: the failure cleanup deletes a partition file only while its
    /// inode still matches this round's own write.
    #[test]
    fn remove_if_owned_checks_inode_ownership() {
        let scratch = crate::test_scratch::Scratch::new("snapshot-ownership");
        let path = scratch.path().join("manifest.json");
        fs::write(&path, b"ours\n").expect("#293 write the round's own manifest");
        let owned = owned_file(&path).expect("#293 stat the round's own manifest");
        // Another round renames over the same path: a different inode.
        let foreign = scratch.path().join("foreign.json");
        fs::write(&foreign, b"theirs\n").expect("#293 write a foreign manifest");
        fs::rename(&foreign, &path).expect("#293 the foreign round renames over the path");
        remove_if_owned(Some(&owned), None);
        assert_eq!(
            fs::read(&path).expect("#293 the foreign manifest is still readable"),
            b"theirs\n",
            "#293 a foreign inode is not deleted"
        );
        // Still ours: the recorded inode matches, so it is removed.
        let owned = owned_file(&path).expect("#293 stat the foreign manifest");
        remove_if_owned(Some(&owned), None);
        assert!(!path.exists(), "#293 an owned file is removed");
        // Nothing recorded: no deletion attempt at all.
        remove_if_owned(None, None);
    }

    /// #293: a round that fails after its own manifest rename deletes that
    /// manifest, and a foreign round that renames over it in the meantime
    /// survives. The committed-marker target is a directory, so
    /// `atomic_marker`'s rename fails after the manifest rename — the
    /// deterministic post-rename failure point.
    #[test]
    fn failing_round_deletes_only_its_own_partition_writes() {
        let scratch = crate::test_scratch::Scratch::new("snapshot-failure-cleanup");
        let snapshot_root = scratch.path().join("snapshots");
        let partition = snapshot_root.join("workspaces/w/sessions/s/snapshots/id");
        fs::create_dir_all(&partition).expect("#293 create the partition");
        let paths = CapturePaths {
            snapshot_root: snapshot_root.to_string_lossy().into_owned(),
            partition: partition.clone(),
            manifest_path: partition.join("manifest.json"),
            committed_path: partition.join("_COMMITTED"),
            failure_marker_path: partition.join("_FAILED.json"),
        };
        let workspace = scratch.path().join("ws");
        fs::create_dir_all(&workspace).expect("#293 create the workspace");
        fs::write(workspace.join("only.txt"), b"x\n").expect("#293 seed the workspace");
        let input = WorkspaceSnapshotInput {
            workspace_id: "w".into(),
            session_id: "s".into(),
            turn_id: "t".into(),
            run_id: "r".into(),
            workspace_dir: workspace.to_string_lossy().into_owned(),
            workspace_stable: None,
            failure_kind: None,
            failure_reason: None,
        };
        let mut markers = RoundMarkers::default();
        let temporary_dir = partition.join(".tmp-unit");

        // Own write: the manifest rename lands, the committed-marker rename
        // fails on the directory, and the cleanup removes this round's own
        // manifest while leaving the directory it did not write.
        fs::create_dir_all(&paths.committed_path).expect("#293 block the committed marker");
        let error = commit(
            &input,
            SnapshotPhase::PreUser,
            "id",
            &paths,
            &temporary_dir,
            &mut markers,
            None,
        )
        .expect_err("#293 the committed-marker rename must fail on the directory");
        assert!(
            error.contains(" -> '"),
            "#293 the failure is the two-path rename message: {error}"
        );
        assert!(
            paths.manifest_path.is_file(),
            "#293 the round's own manifest landed before the failure"
        );
        failure_cleanup(&markers, &temporary_dir, None);
        assert!(
            !paths.manifest_path.exists(),
            "#293 the round's own manifest is removed"
        );
        assert!(
            paths.committed_path.is_dir(),
            "#293 a write the round did not make is untouched"
        );
        assert!(
            !temporary_dir.exists(),
            "#293 the round's temporary directory is removed"
        );

        // Foreign write: another round renames over the path after this
        // round's manifest rename, and the cleanup keeps it.
        let foreign = scratch.path().join("foreign.json");
        fs::write(&foreign, b"theirs\n").expect("#293 write the foreign manifest");
        let mut markers = RoundMarkers::default();
        let error = commit(
            &input,
            SnapshotPhase::PreUser,
            "id",
            &paths,
            &temporary_dir,
            &mut markers,
            None,
        )
        .expect_err("#293 the committed-marker rename must fail again");
        assert!(
            error.contains(" -> '"),
            "#293 the failure is the two-path rename message: {error}"
        );
        fs::rename(&foreign, &paths.manifest_path)
            .expect("#293 the foreign round renames over the manifest path");
        failure_cleanup(&markers, &temporary_dir, None);
        assert_eq!(
            fs::read(&paths.manifest_path).expect("#293 the foreign manifest is readable"),
            b"theirs\n",
            "#293 a manifest replaced by another round survives the cleanup"
        );
    }
}

/// #322 sibling: the provider's own `Snapshot <id> was not found.` must
/// keep raw UTF-16 like `runtime::snapshot::snapshot_not_found`; the old
/// `from_utf16_lossy` turned `"\ud800"` into U+FFFD for direct callers.
#[test]
fn provider_not_found_keeps_raw_snapshot_units() {
    let scratch = crate::test_scratch::Scratch::new("snapshot-not-found");
    let root = scratch.path().to_string_lossy().into_owned();
    let provider = create_workspace_snapshot_provider(WorkspaceSnapshotConfig {
        root: root.clone(),
        snapshot_root: root,
    });
    let id = [0xD800u16];
    let error = provider
        .restore_units(&id, &scratch.path().join("target").to_string_lossy())
        .expect_err("#888 an absent snapshot must be NotFound");
    let WorkspaceSnapshotError::NotFound(message) = error else {
        panic!("#888 expected NotFound, got {error:?}");
    };
    let expected = "Snapshot "
        .encode_utf16()
        .chain([0xD800])
        .chain(" was not found.".encode_utf16())
        .collect::<Vec<_>>();
    assert_eq!(message.units(), expected, "#888 raw units survive");
    assert!(
        !message.units().contains(&0xFFFD),
        "#888 no replacement decoding"
    );
}

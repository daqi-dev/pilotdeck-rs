//! Content-addressed workspace snapshots, ported from TS
//! `src/storage/workspaceSnapshot.ts` (fixed baseline). Storage contract
//! only: the recorder and provider are not wired into the host, any gateway
//! method, or the default proxy route; the WS projection is A6.6-pr4.
//!
//! On-disk layout and bytes are the TS ones: `objects/sha256/<xy>/<zw>/<digest>`
//! object files deduplicated by content, and
//! `workspaces/<id>/sessions/<id>/snapshots/<sha256 of session\0turn\0phase>/`
//! partitions holding `manifest.json`, an empty `_COMMITTED`, or a pretty
//! `_FAILED.json`. Markers and manifests are written to a sibling temporary
//! and renamed into place; manifests are `JSON.stringify(value, null, 2)`
//! with no trailing newline.
//!
//! Known divergences (otherwise the ports are byte-identical):
//! - IO error text inside `_FAILED.json` uses the Node libuv wording from the
//!   measured table (`node_io`); errnos outside it fall back to the Rust
//!   `io::Error` Display. Manifest JSON syntax errors use the V8 messages
//!   recorded by the CLI json-syntax fixture (`pilotdeck_protocol::json_syntax`).
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
    metadata::{js_trim, resolve as lexical_resolve},
    project_files::{has_unpaired_surrogate, node_collator, node_locale},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::{ffi::OsStrExt, fs::symlink},
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
    /// Node-formatted filesystem diagnostics; the fallback for errnos
    /// outside the measured libuv table is the Rust `io::Error` Display.
    Io(String),
    /// `Snapshot <id> was not found.` (workspaceSnapshot.ts:114).
    NotFound(String),
    /// workspaceSnapshot.ts:120.
    UnsafePath,
    /// workspaceSnapshot.ts:123.
    Escape,
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
            | Self::Io(message)
            | Self::NotFound(message)
            | Self::HostRequired(message) => f.write_str(message),
            // Display cannot carry a lone surrogate; callers that serialize an
            // error to JSON use `TypeError`'s raw `JsString` instead.
            Self::TypeError(message) => f.write_str(&String::from_utf16_lossy(message.units())),
            Self::UnsafePath => f.write_str("Snapshot contains an unsafe relative path."),
            Self::Escape => f.write_str("Snapshot path escapes target workspace."),
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
        let Some((_, manifest)) = self.find(snapshot_id)? else {
            return Ok(None);
        };
        Ok(Some(manifest))
    }

    /// workspaceSnapshot.ts:112-134.
    pub fn restore(&self, snapshot_id: &str, target_root: &str) -> Result<RestoredSnapshot> {
        let Some((_, manifest)) = self.find(snapshot_id)? else {
            return Err(WorkspaceSnapshotError::NotFound(format!(
                "Snapshot {snapshot_id} was not found."
            )));
        };
        let entries = match field(&manifest, "entries") {
            JsValue::Array(entries) => entries.clone(),
            _ => Vec::new(),
        };
        let snapshot_root = self.root()?;
        let destination = resolve(target_root)?;
        // Node path.resolve keeps NUL bytes; fs.validatePath rejects them at
        // the first syscall, so mkdir(destination) is where it surfaces.
        validate_nul_path(&JsString::from(destination.as_str()), "path")?;
        fs::create_dir_all(&destination).map_err(|e| {
            WorkspaceSnapshotError::Io(io_error(&e, "mkdir", Some(Path::new(&destination))))
        })?;
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
            let destination_units: Vec<u16> = destination.encode_utf16().collect();
            let mut prefix_units = destination_units.clone();
            prefix_units.push(SLASH);
            if output_units.units() != destination_units.as_slice()
                && !output_units.units().starts_with(&prefix_units)
            {
                return Err(WorkspaceSnapshotError::Escape);
            }
            let output = String::from_utf16_lossy(output_units.units());
            let output_path = PathBuf::from(&output);
            if let Some(parent_units) = parent_path_units(output_units.units()) {
                let parent = JsString::from_utf16(parent_units.to_vec());
                validate_nul_path(&parent, "path")?;
                let parent = PathBuf::from(String::from_utf16_lossy(parent.units()));
                fs::create_dir_all(&parent).map_err(|e| {
                    WorkspaceSnapshotError::Io(io_error(&e, "mkdir", Some(&parent)))
                })?;
            }
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
                    WorkspaceSnapshotError::Io(two_path_message(
                        &e,
                        "symlink",
                        Path::new(&target),
                        Path::new(&output),
                    ))
                })?;
            } else if entry_type.as_string().is_some_and(|t| t.equals_str("file"))
                && let Some(object_key) = field(entry, "objectKey").as_string()
            {
                let source_units = resolve_path_units(&snapshot_root, object_key);
                validate_nul_path(&source_units, "path")?;
                let source_text = String::from_utf16_lossy(source_units.units());
                let source = PathBuf::from(&source_text);
                let contents = fs::read(&source).map_err(|error| {
                    crate::node_io::read_file(&error, &source_text)
                        .map(|(_, message)| WorkspaceSnapshotError::Io(message))
                        .unwrap_or_else(|| {
                            WorkspaceSnapshotError::Io(io_error(
                                &error,
                                "open",
                                Some(source.as_path()),
                            ))
                        })
                })?;
                // writeFile(output) validates after readFile has succeeded,
                // exactly the order of the TS expression.
                validate_nul_path(&output_units, "path")?;
                fs::write(&output_path, contents).map_err(|e| {
                    WorkspaceSnapshotError::Io(io_error(&e, "open", Some(Path::new(&output))))
                })?;
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
    /// seen.
    fn find(&self, snapshot_id: &str) -> Result<Option<(String, JsValue)>> {
        let root = self.root()?;
        let mut manifests = Vec::new();
        walk_manifests(Path::new(&root), &mut manifests)?;
        Ok(manifests.into_iter().rfind(|(_, manifest)| {
            field(manifest, "snapshotId")
                .as_string()
                .is_some_and(|id| id.equals_str(snapshot_id))
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
                    .map(|(_, message)| WorkspaceSnapshotError::Io(message))
                    .unwrap_or_else(|| {
                        WorkspaceSnapshotError::Io(io_error(&error, "open", Some(&path)))
                    })
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
            random_uuid().map_err(|e| WorkspaceSnapshotError::Io(e.to_string()))?
        ));
        let committed = commit(input, phase, &snapshot_id, &paths, &temporary_dir);
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
                // workspaceSnapshot.ts:260-280.
                let _ = fs::remove_file(&paths.manifest_path);
                let _ = fs::remove_file(&paths.committed_path);
                let _ = fs::remove_dir_all(&temporary_dir);
                // This mkdir sits outside the TS catch: a NUL partition path
                // rejects the capture promise instead of writing a marker.
                validate_nul_path(&JsString::from(path_string(&paths.partition)), "path")?;
                fs::create_dir_all(&paths.partition).map_err(|e| {
                    WorkspaceSnapshotError::Io(io_error(&e, "mkdir", Some(&paths.partition)))
                })?;
                let marker = marker_json(input, phase, &snapshot_id, &message);
                let text = stringify_pretty(&marker);
                atomic_marker(&paths.failure_marker_path, &text)
                    .map_err(WorkspaceSnapshotError::Io)?;
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

/// workspaceSnapshot.ts:234-259, the try block. Every failure becomes the
/// marker `error` message, Node-formatted.
fn commit(
    input: &WorkspaceSnapshotInput,
    phase: SnapshotPhase,
    snapshot_id: &str,
    paths: &CapturePaths,
    temporary_dir: &Path,
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
    fs::create_dir_all(temporary_dir).map_err(|e| io_error(&e, "mkdir", Some(temporary_dir)))?;
    let workspace_dir = resolve(&input.workspace_dir).map_err(|e| e.to_string())?;
    let entries = scan_workspace(&workspace_dir, &paths.snapshot_root)?;
    let manifest = manifest_json(input, phase, snapshot_id, entries)?;
    let text = stringify_pretty(&manifest);
    let temporary_manifest = temporary_dir.join("manifest.json");
    fs::write(&temporary_manifest, text)
        .map_err(|e| io_error(&e, "open", Some(temporary_manifest.as_path())))?;
    fs::create_dir_all(&paths.partition)
        .map_err(|e| io_error(&e, "mkdir", Some(&paths.partition)))?;
    fs::rename(&temporary_manifest, &paths.manifest_path)
        .map_err(|e| two_path_message(&e, "rename", &temporary_manifest, &paths.manifest_path))?;
    atomic_marker(&paths.committed_path, "")?;
    // workspaceSnapshot.ts:258: rm(force) ignores a missing directory.
    match fs::remove_dir_all(temporary_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_error(&e, "rm", Some(temporary_dir))),
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
    let listed = fs::read_dir(directory).map_err(|e| io_error(&e, "scandir", Some(directory)))?;
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
            .map_err(|e| io_error(&e, "lstat", Some(absolute.as_path())))?;
        let kind = info.file_type();
        if kind.is_symlink() {
            let target = fs::read_link(&absolute)
                .map_err(|e| io_error(&e, "readlink", Some(absolute.as_path())))?;
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
        .map_err(|e| io_error(&e, "mkdir", Some(object_root.as_path())))?;
    let temporary = object_root.join(format!(
        ".tmp-{}",
        random_uuid().map_err(|e| e.to_string())?
    ));
    let attempt = || -> std::result::Result<StoredObject, String> {
        let mut input = File::open(source).map_err(|e| io_error(&e, "open", Some(source)))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| io_error(&e, "open", Some(temporary.as_path())))?;
        let mut hasher = Sha256::new();
        let mut size: u64 = 0;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = input
                .read(&mut buffer)
                .map_err(|e| io_error(&e, "read", None))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            size += read as u64;
            output
                .write_all(&buffer[..read])
                .map_err(|e| io_error(&e, "write", None))?;
        }
        let digest = hex_lower(&hasher.finalize());
        let object_key = format!("objects/sha256/{}/{}/{digest}", &digest[..2], &digest[2..4]);
        let object_path = snapshot_root.join(&object_key);
        if let Some(parent) = object_path.parent() {
            fs::create_dir_all(parent).map_err(|e| io_error(&e, "mkdir", Some(parent)))?;
        }
        if exists(&object_path) {
            fs::remove_file(&temporary)
                .map_err(|e| io_error(&e, "unlink", Some(temporary.as_path())))?;
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
                    return Err(two_path_message(&error, "rename", &temporary, &object_path));
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
/// randomness never fails in TS.
fn atomic_marker(path: &Path, content: &str) -> std::result::Result<(), String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = path.with_file_name(format!(
        "{name}.tmp-{}",
        random_uuid().map_err(|e| e.to_string())?
    ));
    fs::write(&temporary, content).map_err(|e| io_error(&e, "open", Some(temporary.as_path())))?;
    fs::rename(&temporary, path).map_err(|e| two_path_message(&e, "rename", &temporary, path))?;
    Ok(())
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
            .map_err(|e| WorkspaceSnapshotError::Io(io_error(&e, "uv_cwd", None)))?
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

/// Node `path.resolve(base, value)` over raw UTF-16 units. A lone surrogate
/// survives the lexical pass, so `fs.validatePath` can quote the original JS
/// string instead of the U+FFFD a lossy conversion would show.
fn resolve_path_units(base: &str, value: &JsString) -> JsString {
    let mut source = Vec::new();
    if value.units().first() != Some(&SLASH) {
        source.extend(base.encode_utf16());
        source.push(SLASH);
    }
    source.extend_from_slice(value.units());
    let mut segments: Vec<&[u16]> = Vec::new();
    for segment in source.split(|unit| *unit == SLASH) {
        if segment.is_empty() || (segment.len() == 1 && segment[0] == DOT) {
            continue;
        }
        if is_dot_dot(segment) {
            segments.pop();
        } else {
            segments.push(segment);
        }
    }
    let mut output = Vec::with_capacity(source.len().max(1));
    output.push(SLASH);
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            output.push(SLASH);
        }
        output.extend_from_slice(segment);
    }
    JsString::from_utf16(output)
}

fn is_dot_dot(segment: &[u16]) -> bool {
    segment.len() == 2 && segment[0] == DOT && segment[1] == DOT
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

/// Node-formatted IO diagnostic from the measured libuv table, with the Rust
/// Display as the documented fallback for unmeasured errnos.
fn io_error(error: &io::Error, syscall: &str, path: Option<&Path>) -> String {
    let text = path.map(|path| path.to_string_lossy().into_owned());
    crate::node_io::diagnostic(error, syscall, text.as_deref())
        .map(|(_, message)| message)
        .unwrap_or_else(|| error.to_string())
}

/// Node two-path syscall message (`rename 'a' -> 'b'`, `symlink 'a' -> 'b'`).
fn two_path_message(error: &io::Error, syscall: &str, from: &Path, to: &Path) -> String {
    match crate::node_io::lookup(error) {
        Some((code, description)) => format!(
            "{code}: {description}, {syscall} '{}' -> '{}'",
            from.display(),
            to.display()
        ),
        None => format!(
            "{error}: {syscall} '{}' -> '{}'",
            from.display(),
            to.display()
        ),
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

/// The protocol codec's JSON string quoting, duplicated because it is
/// private to `pilotdeck-protocol`.
fn quoted(value: &JsString, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let escape = |unit: u16, out: &mut String| {
        out.push_str("\\u");
        for shift in [12, 8, 4, 0] {
            out.push(char::from(HEX[usize::from((unit >> shift) & 15)]));
        }
    };
    out.push('"');
    let mut units = value.units().iter().copied().peekable();
    while let Some(unit) = units.next() {
        match unit {
            8 => out.push_str("\\b"),
            9 => out.push_str("\\t"),
            10 => out.push_str("\\n"),
            12 => out.push_str("\\f"),
            13 => out.push_str("\\r"),
            0..=31 => escape(unit, out),
            34 => out.push_str("\\\""),
            92 => out.push_str("\\\\"),
            0xd800..=0xdbff => {
                if let Some(low @ 0xdc00..=0xdfff) = units.peek().copied() {
                    units.next();
                    let scalar =
                        0x10000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00);
                    if let Some(character) = char::from_u32(scalar) {
                        out.push(character);
                    }
                } else {
                    escape(unit, out);
                }
            }
            0xdc00..=0xdfff => escape(unit, out),
            _ => {
                if let Some(character) = char::from_u32(u32::from(unit)) {
                    out.push(character);
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

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
}

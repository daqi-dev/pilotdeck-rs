//! Snapshot management projection from TS `createLocalGateway.ts:1271-1313`.
//!
//! The projection is deliberately not wired into the gateway: `runtime/mod.rs`
//! only exports this module and A8 owns the WS entry. The provider, project-key
//! registry and `resolveProjectStorageId` are dependencies because their ports
//! are separate work (the project registry / storage-id layout does not exist in
//! this crate yet). The filtering, cursor, limit, ownership and `mkdtemp`
//! behavior below is the TS projection.
//!
//! Errors preserve the TS split: `DialogGatewayError`s carry a wire `code` and
//! the `DialogGatewayError` name; provider errors keep their `Error` /
//! `TypeError` / `SyntaxError` name and only the Node fs errnos (`ENOENT`, ...)
//! or `ERR_INVALID_ARG_VALUE` also carry a code, as in the real runtime. An fs
//! errno outside the method's source-oracle set is an explicit host boundary
//! (`NODE_IO_REQUIRED` + `requires_host`), never a guessed Rust Display string.
use crate::storage::{
    RestoredSnapshot, WorkspaceSnapshotDescriptor, WorkspaceSnapshotError,
    WorkspaceSnapshotProvider,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{fmt, fs, os::unix::fs::DirBuilderExt};

static UNDEFINED: JsValue = JsValue::Undefined;
const MKDTEMP_TEMPLATE_SUFFIX: &str = ".pilotdeck-restore-XXXXXX";

/// The project-key registry and storage-id resolver are host dependencies:
/// `createDialogProjectRegistry.resolveProjectKey` and
/// `resolveProjectStorageId` are not ported yet. Implementations must return
/// the resolved key in the first method and that key's on-disk workspace id in
/// the second.
pub trait SnapshotProjectResolver: Send + Sync {
    /// `resolveProjectKey` compares JS strings by code unit (projectRegistry.ts:
    /// 41-59), so the port receives the raw UTF-16 units of `input.projectKey`.
    /// A lone surrogate must not be replacement-decoded before the lookup: on a
    /// filesystem that can store U+FFFD, `"\ud800"` and `"\ufffd"` are different
    /// projects and `from_utf16_lossy` would alias one onto the other. Use
    /// [`SnapshotProjectionError::unknown_project_key`] for the rejection so the
    /// wire message also keeps the original units.
    fn resolve_project_key(&self, project_key: &[u16]) -> Result<String, SnapshotProjectionError>;
    /// The key returned by `resolve_project_key` (a registry path, never a lone
    /// surrogate), so the storage-id resolver keeps its UTF-8 signature.
    fn workspace_id(&self, project_key: &str) -> String;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotProjectionError {
    code: Option<String>,
    name: &'static str,
    message: JsString,
    /// R19 host boundary: the native port cannot represent this input, so the
    /// A8 entry must forward the whole request to the Node host instead of
    /// treating the message as an ordinary projection error.
    requires_host: bool,
}

impl SnapshotProjectionError {
    pub fn gateway(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code: Some(code.to_owned()),
            name: "DialogGatewayError",
            message: JsString::from(message.into()),
            requires_host: false,
        }
    }
    pub fn plain(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            code: None,
            name,
            message: JsString::from(message.into()),
            requires_host: false,
        }
    }
    fn plain_code(name: &'static str, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: Some(code.into()),
            name,
            message: JsString::from(message.into()),
            requires_host: false,
        }
    }
    /// `PROJECT_NOT_FOUND`, `Unknown projectKey: <raw key>` (projectRegistry.ts:
    /// 56/62). The registry port builds the rejection with the original units so
    /// a lone surrogate survives to the wire instead of becoming U+FFFD.
    pub fn unknown_project_key(project_key: &[u16]) -> Self {
        let mut units = "Unknown projectKey: ".encode_utf16().collect::<Vec<_>>();
        units.extend_from_slice(project_key);
        Self {
            code: Some("PROJECT_NOT_FOUND".into()),
            name: "DialogGatewayError",
            message: JsString::from_utf16(units),
            requires_host: false,
        }
    }
    /// R19 host boundary for an fs errno the method's source oracle never
    /// observed: the native port must not guess Node's wording. Same shape as
    /// `session_model`/`skills` (`NODE_IO_REQUIRED`, empty message,
    /// `requires_host`).
    fn node_io_required() -> Self {
        Self {
            code: Some("NODE_IO_REQUIRED".into()),
            name: "Error",
            message: JsString::default(),
            requires_host: true,
        }
    }
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }
    pub fn name(&self) -> &'static str {
        self.name
    }
    pub fn message(&self) -> &JsString {
        &self.message
    }
    /// True when the native projection hit a measured host boundary (R19): the
    /// A8 entry must forward the request, not surface this as a TS-shaped
    /// error. Two shapes carry it: the fs-errno admission boundary
    /// (`NODE_IO_REQUIRED`, empty message, as in `session_model`/`skills`) and
    /// the storage provider's `HostRequired` (`JS_HOST_REQUIRED`).
    pub fn requires_host(&self) -> bool {
        self.requires_host
    }
    /// Wire-shaped error used by the replay fixture and the future A8 entry.
    pub fn as_json(&self) -> JsValue {
        let mut out = JsObject::new();
        out.insert("name".into(), self.name.into());
        if let Some(code) = &self.code {
            out.insert("code".into(), code.as_str().into());
        }
        out.insert("message".into(), JsValue::String(self.message.clone()));
        JsValue::Object(out)
    }
}
impl fmt::Display for SnapshotProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf16_lossy(self.message.units()))
    }
}
impl std::error::Error for SnapshotProjectionError {}
/// Provider errors are projected per gateway method: the fs errno set is
/// admitted from `node_io::LOCAL_ERRNOS`, so an errno the method's source oracle
/// never observed becomes `NODE_IO_REQUIRED` instead of a guessed wire string.
fn project_error(error: WorkspaceSnapshotError, method: &str) -> SnapshotProjectionError {
    match error {
        WorkspaceSnapshotError::Invalid(message) => {
            if message.starts_with("Cannot read properties of ") {
                SnapshotProjectionError::plain("TypeError", message)
            } else {
                SnapshotProjectionError::plain("Error", message)
            }
        }
        WorkspaceSnapshotError::Syntax(message) => {
            SnapshotProjectionError::plain("SyntaxError", message)
        }
        WorkspaceSnapshotError::Io { code, message } => match code {
            Some(code) if crate::node_io::admits(method, code) => {
                SnapshotProjectionError::plain_code("Error", code, message)
            }
            _ => SnapshotProjectionError::node_io_required(),
        },
        WorkspaceSnapshotError::NotFound(message) => {
            SnapshotProjectionError::plain("Error", message)
        }
        WorkspaceSnapshotError::UnsafePath => {
            SnapshotProjectionError::plain("Error", "Snapshot contains an unsafe relative path.")
        }
        WorkspaceSnapshotError::Escape => {
            SnapshotProjectionError::plain("Error", "Snapshot path escapes target workspace.")
        }
        WorkspaceSnapshotError::TypeError(message) => SnapshotProjectionError {
            code: Some("ERR_INVALID_ARG_VALUE".into()),
            name: "TypeError",
            message,
            requires_host: false,
        },
        WorkspaceSnapshotError::HostRequired(message) => SnapshotProjectionError {
            code: Some("JS_HOST_REQUIRED".into()),
            name: "Error",
            message: JsString::from(message),
            requires_host: true,
        },
    }
}

/// `createLocalGateway.ts:1271-1313`. `provider` is `None` when the host has no
/// `PILOTDECK_STORAGE_ROOT`; every method then raises `CAPABILITY_UNAVAILABLE`
/// before resolving a project, exactly like the TS guard.
pub struct SnapshotProjection<R> {
    provider: Option<WorkspaceSnapshotProvider>,
    resolver: R,
    fallback_project_root: String,
}

impl<R: SnapshotProjectResolver> SnapshotProjection<R> {
    pub fn new(
        provider: Option<WorkspaceSnapshotProvider>,
        resolver: R,
        fallback_project_root: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            resolver,
            fallback_project_root: fallback_project_root.into(),
        }
    }

    /// createLocalGateway.ts:1271-1293.
    pub fn list(&self, input: &JsValue) -> Result<JsValue, SnapshotProjectionError> {
        let provider = self.provider()?;
        let project_key = self.resolve_source_project(input)?;
        let workspace_id = self.resolver.workspace_id(&project_key);
        let limit = snapshot_limit(field(input, "limit"))?;
        let session_key = field(input, "sessionKey");
        let cursor = field(input, "cursor");
        let descriptors = provider
            .list()
            .map_err(|error| project_error(error, "snapshot_list"))?;
        let items: Vec<WorkspaceSnapshotDescriptor> = descriptors
            .into_iter()
            .filter(|item| item.workspace_id.equals_str(&workspace_id))
            .filter(|item| {
                !truthy(session_key)
                    || session_key
                        .as_string()
                        .is_some_and(|key| item.session_id == *key)
            })
            .collect();
        let start = if cursor.is_undefined() {
            0
        } else {
            match cursor
                .as_string()
                .and_then(|key| items.iter().position(|item| item.snapshot_id == *key))
            {
                Some(index) => index + 1,
                None => 0,
            }
        };
        if !cursor.is_undefined() && start == 0 {
            return Err(SnapshotProjectionError::gateway(
                "INVALID_SNAPSHOT_CURSOR",
                "snapshot cursor does not belong to this project.",
            ));
        }
        let end = (start + limit).min(items.len());
        let page = &items[start..end];
        let mut result = JsObject::new();
        result.insert(
            "items".into(),
            JsValue::Array(
                page.iter()
                    .map(WorkspaceSnapshotDescriptor::as_json)
                    .collect(),
            ),
        );
        if start + page.len() < items.len() {
            // TS `page.at(-1)!.snapshotId`; the condition guarantees a non-empty page.
            let last = page.last().expect("non-empty page has a cursor");
            result.insert(
                "nextCursor".into(),
                JsValue::String(last.snapshot_id.clone()),
            );
        }
        Ok(JsValue::Object(result))
    }

    /// createLocalGateway.ts:1294-1303.
    pub fn get(&self, input: &JsValue) -> Result<JsValue, SnapshotProjectionError> {
        let provider = self.provider()?;
        let project_key = self.resolve_source_project(input)?;
        let workspace_id = self.resolver.workspace_id(&project_key);
        let snapshot_id = field(input, "snapshotId");
        let snapshot_units = string_units(snapshot_id);
        let manifest = provider
            .get_units(&snapshot_units)
            .map_err(|error| project_error(error, "snapshot_get"))?;
        let Some(manifest) = manifest.filter(|manifest| {
            field(manifest, "workspaceId")
                .as_string()
                .is_some_and(|id| id.equals_str(&workspace_id))
        }) else {
            return Err(snapshot_not_found(snapshot_id));
        };
        let mut result = JsObject::new();
        result.insert("snapshot".into(), manifest);
        Ok(JsValue::Object(result))
    }

    /// createLocalGateway.ts:1304-1314.
    pub fn restore(&self, input: &JsValue) -> Result<JsValue, SnapshotProjectionError> {
        let provider = self.provider()?;
        let source_project = self.resolve_source_project(input)?;
        let workspace_id = self.resolver.workspace_id(&source_project);
        let snapshot_id = field(input, "snapshotId");
        let snapshot_units = string_units(snapshot_id);
        let manifest = provider
            .get_units(&snapshot_units)
            .map_err(|error| project_error(error, "snapshot_restore"))?;
        if !manifest.is_some_and(|manifest| {
            field(&manifest, "workspaceId")
                .as_string()
                .is_some_and(|id| id.equals_str(&workspace_id))
        }) {
            return Err(snapshot_not_found(snapshot_id));
        }
        // TS uses `input.targetProjectKey ?? input.projectKey` (no fallback).
        let target = field(input, "targetProjectKey");
        let target_key = if nullish(target) {
            string_units(field(input, "projectKey"))
        } else {
            string_units(target)
        };
        let target_project = self.resolver.resolve_project_key(&target_key)?;
        let target_root = make_temp_restore_dir(&target_project)?;
        let RestoredSnapshot {
            restored,
            workspace_key,
        } = provider
            .restore_units(&snapshot_units, &target_root)
            .map_err(|error| project_error(error, "snapshot_restore"))?;
        let mut result = JsObject::new();
        result.insert("restored".into(), JsValue::Bool(restored));
        result.insert("workspaceKey".into(), workspace_key.as_str().into());
        Ok(JsValue::Object(result))
    }

    fn provider(&self) -> Result<&WorkspaceSnapshotProvider, SnapshotProjectionError> {
        self.provider.as_ref().ok_or_else(|| {
            SnapshotProjectionError::gateway(
                "CAPABILITY_UNAVAILABLE",
                "Gateway snapshot provider is unavailable.",
            )
        })
    }

    fn resolve_source_project(&self, input: &JsValue) -> Result<String, SnapshotProjectionError> {
        let requested = field(input, "projectKey");
        let units = if nullish(requested) {
            self.fallback_project_root.encode_utf16().collect()
        } else {
            string_units(requested)
        };
        self.resolver.resolve_project_key(&units)
    }
}

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn nullish(value: &JsValue) -> bool {
    matches!(value, JsValue::Null | JsValue::Undefined)
}
/// `String(value)` rendering for non-string inputs and the rare fallback that
/// must produce a UTF-8 `String` (such as `Display`). Identifiers go through
/// [`string_units`] instead, so a lone surrogate is never replacement-decoded
/// before a JS-equality lookup.
fn string_text(value: &JsValue) -> String {
    match value {
        JsValue::String(text) => String::from_utf16_lossy(text.units()),
        JsValue::Undefined => "undefined".into(),
        JsValue::Null => "null".into(),
        JsValue::Bool(value) => value.to_string(),
        JsValue::Number(number) => crate::js_value::number_string(*number),
        _ => String::from_utf16_lossy(&crate::js_value::to_string_units(value).unwrap_or_default()),
    }
}
/// `String(value)` with JS string identity preserved: the gateway entry has
/// already validated the identifiers as strings, and both snapshot ids and
/// project keys are compared by code unit. Keep the raw units instead of
/// `from_utf16_lossy`: `"\ud800"` and `"\ufffd"` name different snapshots and
/// different projects in TS. Non-string values keep the old `String()`
/// rendering (the projection is only reachable after entry validation).
fn string_units(value: &JsValue) -> Vec<u16> {
    match value {
        JsValue::String(text) => text.units().to_vec(),
        _ => string_text(value).encode_utf16().collect(),
    }
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        _ => true,
    }
}
/// `input.limit ?? 100` + `Number.isSafeInteger(limit) && 1 <= limit <= 500`.
fn snapshot_limit(value: &JsValue) -> Result<usize, SnapshotProjectionError> {
    let limit = if nullish(value) {
        100.0
    } else {
        value.as_number().unwrap_or(f64::NAN)
    };
    if limit.fract() != 0.0 || !(1.0..=500.0).contains(&limit) {
        return Err(SnapshotProjectionError::gateway(
            "INVALID_SNAPSHOT_LIMIT",
            "limit must be an integer between 1 and 500.",
        ));
    }
    Ok(limit as usize)
}
/// `Snapshot ${input.snapshotId} was not found.` with raw UTF-16 units kept.
fn snapshot_not_found(snapshot_id: &JsValue) -> SnapshotProjectionError {
    let mut units = "Snapshot ".encode_utf16().collect::<Vec<_>>();
    match snapshot_id {
        JsValue::String(text) => units.extend_from_slice(text.units()),
        _ => units.extend(string_text(snapshot_id).encode_utf16()),
    }
    units.extend(" was not found.".encode_utf16());
    SnapshotProjectionError {
        code: Some("SNAPSHOT_NOT_FOUND".into()),
        name: "DialogGatewayError",
        message: JsString::from_utf16(units),
        requires_host: false,
    }
}
/// `await mkdtemp(joinPath(targetProject, ".pilotdeck-restore-"))`
/// (createLocalGateway.ts:1311). `joinPath` is Node's `path.join`, which
/// normalizes lexically (`.`, `..`, duplicate separators) before the syscall.
/// A plain `Path::join` would leave `..` for the OS: when a segment is a
/// symlink, `link/..` resolves through the link target and creates the
/// directory somewhere other than `path.join` would. Node/libuv then replaces
/// the six `X` characters with `[A-Za-z0-9]` and creates the directory with
/// mode 0700. On failure Node quotes the attempted random candidate, not the
/// `XXXXXX` template; the replay masks that suffix as `<random>`.
fn make_temp_restore_dir(target_project: &str) -> Result<String, SnapshotProjectionError> {
    let template = join_path(target_project, MKDTEMP_TEMPLATE_SUFFIX);
    for _ in 0..10 {
        let candidate = replace_suffix(&template, &random_suffix());
        match fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(mkdtemp_error(&error, &candidate));
            }
        }
    }
    Err(SnapshotProjectionError::plain_code(
        "Error",
        "EEXIST",
        format!("EEXIST: file already exists, mkdtemp '{template}'"),
    ))
}
/// Gateway method admission for `mkdtemp`: only the errnos this method's Node
/// oracle observed may become a wire code. `lookup_for` also rejects an errno
/// outside libuv's table entirely, so a Rust Display string can never reach the
/// wire; the fallback is the same `NODE_IO_REQUIRED` host boundary as
/// `session_model`/`skills`/`native_archive`/`project_files`.
fn mkdtemp_error(error: &std::io::Error, candidate: &str) -> SnapshotProjectionError {
    match crate::node_io::lookup_for("snapshot_restore", error)
        .and_then(|_| crate::node_io::diagnostic(error, "mkdtemp", Some(candidate)))
    {
        Some((code, message)) => SnapshotProjectionError::plain_code("Error", code, message),
        None => SnapshotProjectionError::node_io_required(),
    }
}
/// Node `path.join(left, right)` for POSIX. A later absolute-looking argument
/// is nested, not reset; `..` pops the previous lexical segment (and is kept
/// at the start of a relative path). This mirrors `path.posix.join` for the
/// registry-returned project paths, which are not guaranteed to be normalized.
fn join_path(left: &str, right: &str) -> String {
    let mut combined = String::with_capacity(left.len() + right.len() + 1);
    combined.push_str(left);
    if !combined.is_empty() && !combined.ends_with('/') {
        combined.push('/');
    }
    combined.push_str(right);
    let absolute = combined.starts_with('/');
    let mut segments: Vec<&str> = Vec::new();
    for segment in combined.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.last().is_some_and(|last| *last != "..") {
                    segments.pop();
                } else if !absolute {
                    segments.push("..");
                }
            }
            segment => segments.push(segment),
        }
    }
    let mut output = String::with_capacity(combined.len());
    if absolute {
        output.push('/');
    }
    output.push_str(&segments.join("/"));
    if output.is_empty() {
        ".".into()
    } else {
        output
    }
}
fn replace_suffix(template: &str, suffix: &str) -> String {
    let start = template.len() - 6;
    format!("{}{suffix}", &template[..start])
}
/// `crypto`-quality randomness is not required for a temp name; Node only needs
/// six `[A-Za-z0-9]` characters. uuid v4 already draws from the OS RNG.
fn random_suffix() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let bytes = uuid::Uuid::new_v4();
    let mut suffix = String::with_capacity(6);
    for byte in bytes.as_bytes().iter().take(6) {
        suffix.push(ALPHABET[(*byte as usize) % ALPHABET.len()] as char);
    }
    suffix
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn mkdtemp_errno_uses_the_method_whitelist() {
        let missing = mkdtemp_error(
            &io::Error::from_raw_os_error(libc::ENOENT),
            "/tmp/x/.pilotdeck-restore-abc123",
        );
        assert_eq!(missing.code(), Some("ENOENT"));
        assert_eq!(missing.name(), "Error");
        assert!(!missing.requires_host());
        assert!(
            missing.message().units().starts_with(
                "ENOENT: no such file or directory, mkdtemp '/tmp/x/.pilotdeck-restore-abc123'"
                    .encode_utf16()
                    .collect::<Vec<_>>()
                    .as_slice()
            )
        );

        // EACCES is in the global libuv table but was never observed for
        // snapshotRestore: only method admission (not table membership)
        // decides, and guessing the Node wording would put an unmeasured
        // code/message on the wire.
        let denied = mkdtemp_error(
            &io::Error::from_raw_os_error(libc::EACCES),
            "/tmp/x/.pilotdeck-restore-abc123",
        );
        assert_eq!(denied.code(), Some("NODE_IO_REQUIRED"));
        assert_eq!(denied.name(), "Error");
        assert!(denied.requires_host());
        assert!(denied.message().units().is_empty());

        // EDQUOT is outside libuv's table entirely (`diagnostic` returns None);
        // the Rust Display must not leak either.
        let quota = mkdtemp_error(
            &io::Error::from_raw_os_error(libc::EDQUOT),
            "/tmp/x/.pilotdeck-restore-abc123",
        );
        assert_eq!(quota.code(), Some("NODE_IO_REQUIRED"));
        assert_eq!(quota.name(), "Error");
        assert!(quota.requires_host());
        assert!(quota.message().units().is_empty());
    }

    #[test]
    fn provider_io_is_admitted_per_gateway_method() {
        let io_error = |code, message: &str| WorkspaceSnapshotError::Io {
            code,
            message: message.into(),
        };
        // snapshotRestore's oracle observed ENOENT (mkdtemp and open).
        let admitted = project_error(
            io_error(
                Some("ENOENT"),
                "ENOENT: no such file or directory, open '/x'",
            ),
            "snapshot_restore",
        );
        assert_eq!(admitted.code(), Some("ENOENT"));
        assert!(!admitted.requires_host());

        // The same code is not in snapshotList's measured set.
        let stale = project_error(
            io_error(
                Some("ENOENT"),
                "ENOENT: no such file or directory, open '/x'",
            ),
            "snapshot_list",
        );
        assert_eq!(stale.code(), Some("NODE_IO_REQUIRED"));
        assert!(stale.requires_host());
        assert!(stale.message().units().is_empty());

        // An errno outside libuv's table carries no code; the Rust Display must
        // not become the wire message.
        let unknown = project_error(
            io_error(None, "Disk quota exceeded (os error 122)"),
            "snapshot_restore",
        );
        assert_eq!(unknown.code(), Some("NODE_IO_REQUIRED"));
        assert!(unknown.requires_host());
        assert!(unknown.message().units().is_empty());
    }

    #[test]
    fn unknown_project_key_keeps_raw_units() {
        let error = SnapshotProjectionError::unknown_project_key(&[0xD800]);
        assert_eq!(error.code(), Some("PROJECT_NOT_FOUND"));
        assert_eq!(error.name(), "DialogGatewayError");
        assert!(!error.requires_host());
        assert_eq!(
            error.message().units(),
            "Unknown projectKey: "
                .encode_utf16()
                .chain([0xD800])
                .collect::<Vec<_>>()
                .as_slice()
        );
        assert!(!error.message().units().contains(&0xFFFD));
    }
}

//! Snapshot management projection from TS `createLocalGateway.ts:1271-1313`,
//! plus the `InProcessGateway.snapshot*` method adapter
//! (`InProcessGateway.ts:2810-2827,3144-3162`).
//!
//! The projection and adapter are deliberately not registered in the native
//! dispatch table: `dispatch.rs` keeps `snapshot_*` forwarded to the TS host
//! until A8 owns the WS entry. The provider is wired through the existing
//! `runtime/project/persistence.rs` resource; `SnapshotProjects` is the native
//! `createDialogProjectRegistry.resolveProjectKey` +
//! `resolveProjectStorageId` implementation over the same metadata catalog.
//! The filtering, cursor, limit, ownership and `mkdtemp` behavior below is the
//! TS projection.
//!
//! Errors preserve the TS split: `DialogGatewayError`s carry a wire `code` and
//! the `DialogGatewayError` name; provider errors keep their `Error` /
//! `TypeError` / `SyntaxError` name and only the Node fs errnos (`ENOENT`, ...)
//! or `ERR_INVALID_ARG_VALUE` also carry a code, as in the real runtime. An fs
//! errno outside the method's source-oracle set is an explicit host boundary
//! (`NODE_IO_REQUIRED` + `requires_host`), never a guessed Rust Display string.
use crate::metadata::{Metadata, NativeFiles, js_trim};
use crate::storage::{
    RestoredSnapshot, WorkspaceSnapshotDescriptor, WorkspaceSnapshotError,
    WorkspaceSnapshotProvider,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{fmt, fs, os::unix::fs::DirBuilderExt, path::PathBuf};

static UNDEFINED: JsValue = JsValue::Undefined;
const MKDTEMP_TEMPLATE_SUFFIX: &str = ".pilotdeck-restore-XXXXXX";

/// The project-key registry and storage-id resolver seam. Production uses
/// [`SnapshotProjects`] over `metadata::Metadata`; tests inject a fixed table.
/// Implementations must return the resolved key in the first method and that
/// key's on-disk workspace id in the second.
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

/// Production `SnapshotProjectResolver`: the real `listProjects` catalog and
/// `resolveProjectStorageId`. `home` is the pilot home; `cwd` is the process
/// cwd the TS `path.resolve` uses for relative project keys and `.cwd`
/// markers. The underlying `Metadata` is generic; this public wrapper pins the
/// native filesystem so the port has no private-bound surface.
pub struct SnapshotProjects {
    metadata: Metadata<NativeFiles>,
}

impl SnapshotProjects {
    pub fn new(home: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            metadata: Metadata {
                home: home.into(),
                cwd: cwd.into(),
                files: NativeFiles,
            },
        }
    }
}

impl SnapshotProjectResolver for SnapshotProjects {
    fn resolve_project_key(&self, project_key: &[u16]) -> Result<String, SnapshotProjectionError> {
        self.metadata
            .resolve_dialog_project(&JsString::from_utf16(project_key.to_vec()))
            .ok_or_else(|| SnapshotProjectionError::unknown_project_key(project_key))
    }

    fn workspace_id(&self, project_key: &str) -> String {
        self.metadata.project_storage_id(project_key)
    }
}

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
        WorkspaceSnapshotError::NotFound(message) => SnapshotProjectionError {
            code: None,
            name: "Error",
            message,
            requires_host: false,
        },
        WorkspaceSnapshotError::UnsafePath => {
            SnapshotProjectionError::plain("Error", "Snapshot contains an unsafe relative path.")
        }
        WorkspaceSnapshotError::Escape => {
            SnapshotProjectionError::plain("Error", "Snapshot path escapes target workspace.")
        }
        // #293 registered divergence A6.6-SNAPSHOT-OBJECTKEY-001: TS restores
        // objectKeys outside the snapshot root; Rust refuses the restore.
        WorkspaceSnapshotError::ObjectKeyEscape => SnapshotProjectionError::plain(
            "Error",
            "Snapshot object key escapes the snapshot root.",
        ),
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

/// One native gateway method outcome. `Forward` is the A8 contract: the
/// projection hit a measured host boundary (`requires_host`), so the WS entry
/// must send the whole request to the Node host instead of surfacing a
/// synthesized error.
#[derive(Clone, Debug, PartialEq)]
pub enum SnapshotDispatch {
    Response(JsValue),
    Forward,
}

/// `InProcessGateway.snapshot*` adapter (`InProcessGateway.ts:2810-2827`).
///
/// The method validators run **before** the capability check, exactly like the
/// TS wrapper; the inner `createLocalGateway` methods check the provider first
/// after entry validation. `dispatch` is the only wire-facing entry and is
/// deliberately absent from `dispatch.rs`'s `LOCAL_METHODS`.
pub struct SnapshotGateway<R> {
    projection: SnapshotProjection<R>,
}

impl<R: SnapshotProjectResolver> SnapshotGateway<R> {
    pub fn new(
        provider: Option<WorkspaceSnapshotProvider>,
        resolver: R,
        fallback_project_root: impl Into<String>,
    ) -> Self {
        Self {
            projection: SnapshotProjection::new(provider, resolver, fallback_project_root),
        }
    }

    pub fn from_projection(projection: SnapshotProjection<R>) -> Self {
        Self { projection }
    }

    /// `snapshot_list`: projectKey, optional sessionKey, then the projection.
    pub fn list(&self, params: &JsValue) -> Result<JsValue, SnapshotProjectionError> {
        require_management_project_key(parameter(params, "projectKey")?)?;
        require_optional_session_key(parameter(params, "sessionKey")?)?;
        self.projection.list(params)
    }

    /// `snapshot_get`: projectKey, snapshotId, then the projection.
    pub fn get(&self, params: &JsValue) -> Result<JsValue, SnapshotProjectionError> {
        require_management_project_key(parameter(params, "projectKey")?)?;
        require_management_identifier(parameter(params, "snapshotId")?, "snapshotId")?;
        self.projection.get(params)
    }

    /// `snapshot_restore`: projectKey, snapshotId, optional targetProjectKey
    /// (validated only when it is not `undefined`), then the projection.
    pub fn restore(&self, params: &JsValue) -> Result<JsValue, SnapshotProjectionError> {
        require_management_project_key(parameter(params, "projectKey")?)?;
        require_management_identifier(parameter(params, "snapshotId")?, "snapshotId")?;
        let target = field(params, "targetProjectKey");
        if !target.is_undefined() {
            require_management_project_key(target)?;
        }
        self.projection.restore(params)
    }

    /// WS method names (`snapshot_list` / `snapshot_get` / `snapshot_restore`).
    /// Unknown methods return `None` so the caller keeps forwarding by default.
    pub fn dispatch(&self, method: &str, params: &JsValue) -> Option<SnapshotDispatch> {
        let outcome = match method {
            "snapshot_list" => self.list(params),
            "snapshot_get" => self.get(params),
            "snapshot_restore" => self.restore(params),
            _ => return None,
        };
        Some(match outcome {
            Ok(value) => SnapshotDispatch::Response(value),
            Err(error) if error.requires_host() => SnapshotDispatch::Forward,
            Err(error) => SnapshotDispatch::Response(error.as_json()),
        })
    }
}

/// Reads one own property with the TypeError Node raises for a null/undefined
/// params object. Primitive params box like JS and simply have no property.
fn parameter<'a>(params: &'a JsValue, key: &str) -> Result<&'a JsValue, SnapshotProjectionError> {
    if matches!(params, JsValue::Undefined | JsValue::Null) {
        let kind = if params.is_undefined() {
            "undefined"
        } else {
            "null"
        };
        return Err(SnapshotProjectionError::plain(
            "TypeError",
            format!("Cannot read properties of {kind} (reading '{key}')"),
        ));
    }
    Ok(field(params, key))
}

/// `requireManagementProjectKey` (InProcessGateway.ts:3144-3148).
fn require_management_project_key(value: &JsValue) -> Result<(), SnapshotProjectionError> {
    match value {
        JsValue::String(text) if !js_trim(&String::from_utf16_lossy(text.units())).is_empty() => {
            Ok(())
        }
        _ => Err(SnapshotProjectionError::gateway(
            "PROJECT_NOT_FOUND",
            "projectKey is required.",
        )),
    }
}

/// `requireOptionalManagementSessionKey` (InProcessGateway.ts:3150-3154).
fn require_optional_session_key(value: &JsValue) -> Result<(), SnapshotProjectionError> {
    match value {
        JsValue::Undefined => Ok(()),
        JsValue::String(text) if !js_trim(&String::from_utf16_lossy(text.units())).is_empty() => {
            Ok(())
        }
        _ => Err(SnapshotProjectionError::gateway(
            "INVALID_SESSION_KEY",
            "sessionKey must be non-empty when provided.",
        )),
    }
}

/// `requireManagementIdentifier` (InProcessGateway.ts:3156-3160).
fn require_management_identifier(
    value: &JsValue,
    label: &str,
) -> Result<(), SnapshotProjectionError> {
    match value {
        JsValue::String(text) if !js_trim(&String::from_utf16_lossy(text.units())).is_empty() => {
            Ok(())
        }
        _ => Err(SnapshotProjectionError::gateway(
            "INVALID_MANAGEMENT_IDENTIFIER",
            format!("{label} is required."),
        )),
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
///
/// #322: after the tenth `EEXIST` Node/libuv give up with its own wording, but
/// this method's source oracle never observed an EEXIST *wire* error and the
/// template it would quote is not the attempted candidate. Fabricating
/// `EEXIST: file already exists, mkdtemp '<template>'` would bypass the
/// per-method errno admission invariant below, so exhaustion is the explicit
/// `NODE_IO_REQUIRED` host boundary instead (same shape as any unmeasured
/// errno). `mkdtemp_with` exposes the six-character suffix for a deterministic
/// ten-collision test; production always uses [`random_suffix`].
fn make_temp_restore_dir(target_project: &str) -> Result<String, SnapshotProjectionError> {
    mkdtemp_with(target_project, random_suffix)
}
fn mkdtemp_with(
    target_project: &str,
    mut suffix: impl FnMut() -> String,
) -> Result<String, SnapshotProjectionError> {
    let template = join_path(target_project, MKDTEMP_TEMPLATE_SUFFIX);
    for _ in 0..10 {
        let candidate = replace_suffix(&template, &suffix());
        match fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(mkdtemp_error(&error, &candidate));
            }
        }
    }
    Err(SnapshotProjectionError::node_io_required())
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
    use crate::storage::WorkspaceSnapshotConfig;
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

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("pdrs-888-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).expect("#888 create unit-test directory");
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn utf16(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn params(pairs: &[(&str, JsValue)]) -> JsValue {
        let mut object = JsObject::new();
        for (key, value) in pairs {
            object.insert((*key).into(), value.clone());
        }
        JsValue::Object(object)
    }

    /// #322-1 regression: ten `EEXIST` collisions must not synthesize an
    /// `EEXIST` wire error or Node's `mkdtemp '<template>'` wording. The
    /// deterministic suffix makes the otherwise unreachable branch real; the
    /// method admission invariant was bypassed by the old hand-written pair.
    #[test]
    fn mkdtemp_exhaustion_is_a_host_boundary() {
        let scratch = TempDir::new();
        let root = scratch.0.to_string_lossy().into_owned();
        let template = join_path(&root, MKDTEMP_TEMPLATE_SUFFIX);
        let candidate = replace_suffix(&template, "AAAAAA");
        fs::create_dir_all(&candidate).expect("#888 pre-create the colliding candidate");
        let error = mkdtemp_with(&root, || "AAAAAA".into())
            .expect_err("#888 ten EEXIST collisions must not succeed");
        assert_eq!(error.code(), Some("NODE_IO_REQUIRED"), "#888 host boundary");
        assert_eq!(error.name(), "Error", "#888 host boundary name");
        assert!(error.requires_host(), "#888 A8 must forward exhaustion");
        assert!(
            error.message().units().is_empty(),
            "#888 no fabricated Node EEXIST wording on the wire"
        );
        let directories = fs::read_dir(&root)
            .expect("#888 list test root")
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".pilotdeck-restore-")
            })
            .count();
        assert_eq!(directories, 1, "#888 no extra directories were left behind");
    }

    /// #322-2 regression: the doc names two host-boundary shapes and both must
    /// set `requires_host` with their own wire code; an ordinary projection
    /// error never does.
    #[test]
    fn requires_host_covers_both_boundary_shapes() {
        let errno = mkdtemp_error(
            &io::Error::from_raw_os_error(libc::EACCES),
            "/tmp/x/.pilotdeck-restore-abc123",
        );
        assert_eq!(errno.code(), Some("NODE_IO_REQUIRED"), "#888 errno shape");
        assert_eq!(errno.name(), "Error", "#888 errno shape name");
        assert!(errno.requires_host(), "#888 errno shape forwards");
        assert!(
            errno.message().units().is_empty(),
            "#888 errno has no message"
        );

        let collation = project_error(
            WorkspaceSnapshotError::HostRequired(
                "Snapshot createdAt collation requires the Node host for unpaired surrogates."
                    .into(),
            ),
            "snapshot_list",
        );
        assert_eq!(
            collation.code(),
            Some("JS_HOST_REQUIRED"),
            "#888 provider shape"
        );
        assert_eq!(collation.name(), "Error", "#888 provider shape name");
        assert!(collation.requires_host(), "#888 provider shape forwards");
        assert!(
            !collation.message().units().is_empty(),
            "#888 provider shape keeps its explanation"
        );

        let ordinary = project_error(
            WorkspaceSnapshotError::Invalid("ordinary".into()),
            "snapshot_list",
        );
        assert_eq!(ordinary.code(), None, "#888 ordinary shape code");
        assert!(
            !ordinary.requires_host(),
            "#888 ordinary shape does not forward"
        );
    }

    /// Production `resolveProjectKey` / `resolveProjectStorageId` over real
    /// `.cwd` markers: absolute and relative markers, the virtual General
    /// workspace, a marker pointing at a file, a missing marker target, and
    /// the legacy slug fallback when no marker matches.
    #[test]
    fn native_project_resolver_matches_ts_registry_rules() {
        let scratch = TempDir::new();
        let root = &scratch.0;
        let home = root.join("pilot-home");
        let project = root.join("project");
        let target = root.join("target-project");
        let not_a_dir = root.join("not-a-dir");
        fs::create_dir_all(&home).expect("#888 create home");
        fs::create_dir_all(&project).expect("#888 create project");
        fs::create_dir_all(&target).expect("#888 create target");
        fs::write(&not_a_dir, b"x").expect("#888 create marker-to-file");
        let registry = |id: &str, marker: Option<&str>| {
            let dir = home.join("projects").join(id);
            fs::create_dir_all(&dir).expect("#888 create registry dir");
            if let Some(marker) = marker {
                fs::write(dir.join(".cwd"), marker).expect("#888 write marker");
            }
        };
        registry("p-main", Some(project.to_string_lossy().as_ref()));
        // Relative marker: `resolve(marker)` uses cwd, exactly like TS.
        registry("p-target", Some("target-project"));
        registry("p-general", Some(home.to_string_lossy().as_ref()));
        registry("p-file", Some(not_a_dir.to_string_lossy().as_ref()));
        registry("p-gone", Some(root.join("gone").to_string_lossy().as_ref()));
        registry("p-empty", Some(""));
        registry("p-no-marker", None);

        let resolver = SnapshotProjects::new(&home, root);
        let project_key = project.to_string_lossy().into_owned();
        let target_key = target.to_string_lossy().into_owned();
        let home_key = home.to_string_lossy().into_owned();

        assert_eq!(
            resolver
                .resolve_project_key(&utf16(&project_key))
                .expect("#888 absolute marker resolves"),
            project_key,
            "#888 absolute marker resolves"
        );
        assert_eq!(
            resolver
                .resolve_project_key(&utf16("target-project"))
                .expect("#888 relative marker resolves"),
            "target-project",
            "#888 relative marker returns the registered spelling, like TS"
        );
        assert_eq!(
            resolver
                .resolve_project_key(&utf16(&home_key))
                .expect("#888 General workspace resolves"),
            home_key,
            "#888 General workspace is pilotHome"
        );
        assert_eq!(
            resolver
                .resolve_project_key(&utf16(&format!("{project_key}/../target-project")))
                .expect("#888 `..` folds before the registry lookup"),
            "target-project",
            "#888 `..` folds before the registry lookup; the registered spelling wins"
        );
        assert_eq!(
            resolver
                .resolve_project_key(&utf16(&format!("{project_key}/")))
                .expect("#888 trailing slash folds"),
            project_key,
            "#888 trailing slash folds"
        );
        for unknown in [
            root.join("outside").to_string_lossy().into_owned(),
            not_a_dir.to_string_lossy().into_owned(),
            root.join("gone").to_string_lossy().into_owned(),
        ] {
            let error = resolver
                .resolve_project_key(&utf16(&unknown))
                .expect_err("#888 unregistered key must fail");
            assert_eq!(error.code(), Some("PROJECT_NOT_FOUND"), "#888 unknown code");
            assert_eq!(
                error
                    .message()
                    .to_utf8()
                    .expect("#888 unknown key message is UTF-8"),
                format!("Unknown projectKey: {unknown}"),
                "#888 unknown keeps the raw key"
            );
        }

        assert_eq!(
            resolver.workspace_id(&project_key),
            "p-main",
            "#888 marker owns storage id"
        );
        assert_eq!(
            resolver.workspace_id(&target_key),
            "p-target",
            "#888 relative marker owns storage id"
        );
        assert_eq!(
            resolver.workspace_id(&home_key),
            "p-general",
            "#888 General workspace marker owns storage id"
        );
        // No marker matches this path: `createProjectId` slug.
        let outside = root.join("outside");
        assert_eq!(
            resolver.workspace_id(&outside.to_string_lossy()),
            crate::metadata::project_id(&outside),
            "#888 no-marker fallback is the path slug"
        );
    }

    /// `InProcessGateway.snapshot*` validation order: params TypeError, then
    /// projectKey, sessionKey/snapshotId/targetProjectKey, then capability.
    #[test]
    fn gateway_validators_match_inprocess_order() {
        let scratch = TempDir::new();
        let home = scratch.0.join("pilot-home");
        let cwd = scratch.0.clone();
        fs::create_dir_all(&home).expect("#888 create home");
        let gateway = SnapshotGateway::new(None, SnapshotProjects::new(&home, &cwd), "/fallback");

        let undefined = gateway
            .list(&JsValue::Undefined)
            .expect_err("#888 undefined params");
        assert_eq!(undefined.name(), "TypeError", "#888 params name");
        assert_eq!(undefined.code(), None, "#888 params code");
        assert_eq!(
            undefined
                .message()
                .to_utf8()
                .expect("#888 wire message is UTF-8"),
            "Cannot read properties of undefined (reading 'projectKey')",
            "#888 params message"
        );

        let required = gateway
            .list(&params(&[("projectKey", JsValue::Null)]))
            .expect_err("#888 null projectKey");
        assert_eq!(required.code(), Some("PROJECT_NOT_FOUND"), "#888 key code");
        assert_eq!(
            required
                .message()
                .to_utf8()
                .expect("#888 wire message is UTF-8"),
            "projectKey is required.",
            "#888 key message"
        );

        let session = gateway
            .list(&params(&[
                ("projectKey", "/registered".into()),
                ("sessionKey", JsValue::Null),
            ]))
            .expect_err("#888 null sessionKey");
        assert_eq!(
            session.code(),
            Some("INVALID_SESSION_KEY"),
            "#888 session code"
        );
        assert_eq!(
            session
                .message()
                .to_utf8()
                .expect("#888 wire message is UTF-8"),
            "sessionKey must be non-empty when provided.",
            "#888 session message"
        );

        let identifier = gateway
            .get(&params(&[("projectKey", "/registered".into())]))
            .expect_err("#888 missing snapshotId");
        assert_eq!(
            identifier.code(),
            Some("INVALID_MANAGEMENT_IDENTIFIER"),
            "#888 identifier code"
        );
        assert_eq!(
            identifier
                .message()
                .to_utf8()
                .expect("#888 wire message is UTF-8"),
            "snapshotId is required.",
            "#888 identifier message"
        );

        let target = gateway
            .restore(&params(&[
                ("projectKey", "/registered".into()),
                ("snapshotId", "s".into()),
                ("targetProjectKey", JsValue::Null),
            ]))
            .expect_err("#888 null target");
        assert_eq!(target.code(), Some("PROJECT_NOT_FOUND"), "#888 target code");
        assert_eq!(
            target
                .message()
                .to_utf8()
                .expect("#888 wire message is UTF-8"),
            "projectKey is required.",
            "#888 target message"
        );

        // Validation runs before the provider capability check.
        let unavailable = gateway
            .list(&params(&[("projectKey", "/registered".into())]))
            .expect_err("#888 no provider");
        assert_eq!(
            unavailable.code(),
            Some("CAPABILITY_UNAVAILABLE"),
            "#888 capability code"
        );
        assert_eq!(
            unavailable
                .message()
                .to_utf8()
                .expect("#888 wire message is UTF-8"),
            "Gateway snapshot provider is unavailable.",
            "#888 capability message"
        );

        assert!(
            gateway
                .dispatch("snapshot_upload", &JsValue::Null)
                .is_none(),
            "#888 unknown methods stay forwardable"
        );
        let routed = gateway
            .dispatch("snapshot_list", &params(&[("projectKey", JsValue::Null)]))
            .expect("#888 known method routes");
        assert_eq!(
            routed,
            SnapshotDispatch::Response(required.as_json()),
            "#888 ordinary errors become a wire response"
        );
    }

    /// A8.2-pr2 keeps the native component unregistered: default dispatch must
    /// keep forwarding all three methods to the TS host.
    #[test]
    fn snapshot_methods_stay_outside_default_dispatch() {
        for method in ["snapshot_list", "snapshot_get", "snapshot_restore"] {
            let request = params(&[
                ("type", "request".into()),
                ("id", "1".into()),
                ("method", method.into()),
            ]);
            assert!(
                !crate::dispatch::is_local(&request),
                "#888 {method} must not join LOCAL_METHODS in this package"
            );
        }
    }

    /// `dispatch` forwards the whole request when the projection hits the
    /// measured collation host boundary; it never synthesizes that error.
    #[test]
    fn dispatch_forwards_the_collation_host_boundary() {
        let scratch = TempDir::new();
        let home = scratch.0.join("pilot-home");
        let project = scratch.0.join("project");
        let snapshot_root = scratch.0.join("snapshots");
        fs::create_dir_all(&home).expect("#888 create home");
        fs::create_dir_all(&project).expect("#888 create project");
        let registry = home.join("projects").join("p-main");
        fs::create_dir_all(&registry).expect("#888 create registry dir");
        fs::write(registry.join(".cwd"), project.to_string_lossy().as_bytes())
            .expect("#888 write marker");
        let manifest = |id: &str, created_at: &str| {
            let dir = snapshot_root
                .join("workspaces/p-main/sessions/s/snapshots")
                .join(id);
            fs::create_dir_all(&dir).expect("#888 create manifest dir");
            fs::write(
                dir.join("manifest.json"),
                format!(
                    r#"{{"snapshotId":"{id}","workspaceId":"p-main","sessionId":"s","turnId":"t","runId":"r","phase":"pre_user","createdAt":{created_at},"entries":[]}}"#
                ),
            )
            .expect("#888 write manifest");
        };
        manifest("a", r#""2026-01-01T00:00:00.000Z""#);
        manifest("b", r#""\ufffd""#);
        // A raw lone surrogate cannot be written as UTF-8; JSON `\ud800`
        // parses back into the exact JS code unit the collator must host.
        manifest("c", r#""\ud800""#);
        let provider =
            crate::storage::create_workspace_snapshot_provider(WorkspaceSnapshotConfig {
                root: snapshot_root.to_string_lossy().into_owned(),
                snapshot_root: snapshot_root.to_string_lossy().into_owned(),
            });
        let gateway = SnapshotGateway::new(
            Some(provider),
            SnapshotProjects::new(&home, &scratch.0),
            "/fallback",
        );
        let routed = gateway
            .dispatch(
                "snapshot_list",
                &params(&[("projectKey", project.to_string_lossy().as_ref().into())]),
            )
            .expect("#888 known method routes");
        assert_eq!(
            routed,
            SnapshotDispatch::Forward,
            "#888 collation host boundary forwards the request"
        );
    }
}

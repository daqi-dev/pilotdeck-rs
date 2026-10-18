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
//! or `ERR_INVALID_ARG_VALUE` also carry a code, as in the real runtime.
use crate::storage::{
    RestoredSnapshot, WorkspaceSnapshotDescriptor, WorkspaceSnapshotError,
    WorkspaceSnapshotProvider,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{fmt, fs, os::unix::fs::DirBuilderExt, path::Path};

static UNDEFINED: JsValue = JsValue::Undefined;
const MKDTEMP_TEMPLATE_SUFFIX: &str = ".pilotdeck-restore-XXXXXX";

/// The project-key registry and storage-id resolver are host dependencies:
/// `createDialogProjectRegistry.resolveProjectKey` and
/// `resolveProjectStorageId` are not ported yet. Implementations must return
/// the resolved key in the first method and that key's on-disk workspace id in
/// the second.
pub trait SnapshotProjectResolver: Send + Sync {
    fn resolve_project_key(&self, project_key: &str) -> Result<String, SnapshotProjectionError>;
    fn workspace_id(&self, project_key: &str) -> String;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotProjectionError {
    code: Option<String>,
    name: &'static str,
    message: JsString,
}

impl SnapshotProjectionError {
    pub fn gateway(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code: Some(code.to_owned()),
            name: "DialogGatewayError",
            message: JsString::from(message.into()),
        }
    }
    pub fn plain(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            code: None,
            name,
            message: JsString::from(message.into()),
        }
    }
    fn plain_code(name: &'static str, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: Some(code.into()),
            name,
            message: JsString::from(message.into()),
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
impl From<WorkspaceSnapshotError> for SnapshotProjectionError {
    fn from(error: WorkspaceSnapshotError) -> Self {
        match error {
            WorkspaceSnapshotError::Invalid(message) => {
                if message.starts_with("Cannot read properties of ") {
                    Self::plain("TypeError", message)
                } else {
                    Self::plain("Error", message)
                }
            }
            WorkspaceSnapshotError::Syntax(message) => Self::plain("SyntaxError", message),
            WorkspaceSnapshotError::Io(message) => match io_code(&message) {
                Some(code) => Self::plain_code("Error", code, message),
                None => Self::plain("Error", message),
            },
            WorkspaceSnapshotError::NotFound(message) => Self::plain("Error", message),
            WorkspaceSnapshotError::UnsafePath => {
                Self::plain("Error", "Snapshot contains an unsafe relative path.")
            }
            WorkspaceSnapshotError::Escape => {
                Self::plain("Error", "Snapshot path escapes target workspace.")
            }
            WorkspaceSnapshotError::TypeError(message) => Self {
                code: Some("ERR_INVALID_ARG_VALUE".into()),
                name: "TypeError",
                message,
            },
            WorkspaceSnapshotError::HostRequired(message) => Self::plain("Error", message),
        }
    }
}

/// Node fs errors carry their errno in `error.code`; the native port keeps the
/// code inside the Node-formatted message (`ENOENT: ...`). Surface it again for
/// the wire-shaped replay/error object.
fn io_code(message: &str) -> Option<String> {
    let (head, _) = message.split_once(": ")?;
    (head.len() >= 2
        && head
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'))
    .then(|| head.to_owned())
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
        let descriptors = provider.list()?;
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
        let manifest = provider.get(&string_text(snapshot_id))?;
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
        let snapshot_text = string_text(snapshot_id);
        let manifest = provider.get(&snapshot_text)?;
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
            string_text(field(input, "projectKey"))
        } else {
            string_text(target)
        };
        let target_project = self.resolver.resolve_project_key(&target_key)?;
        let target_root = make_temp_restore_dir(&target_project)?;
        let RestoredSnapshot {
            restored,
            workspace_key,
        } = provider.restore(&snapshot_text, &target_root)?;
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
        let requested = if nullish(requested) {
            self.fallback_project_root.clone()
        } else {
            string_text(requested)
        };
        self.resolver.resolve_project_key(&requested)
    }
}

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn nullish(value: &JsValue) -> bool {
    matches!(value, JsValue::Null | JsValue::Undefined)
}
/// `String(value)` for the inputs the gateway entry has already validated as
/// strings. Preserve lone surrogates lossily rather than panicking; the
/// project/snapshot identifier contract is a UTF-8 path/id.
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
    }
}
/// `await mkdtemp(joinPath(targetProject, ".pilotdeck-restore-"))`
/// (createLocalGateway.ts:1311). Node/libuv replaces the six `X` characters
/// with `[A-Za-z0-9]` and creates the directory with mode 0700; the error text
/// quotes the template, so a missing parent keeps the `XXXXXX` suffix.
fn make_temp_restore_dir(target_project: &str) -> Result<String, SnapshotProjectionError> {
    let template_path = Path::new(target_project).join(MKDTEMP_TEMPLATE_SUFFIX);
    let template = template_path.to_string_lossy().into_owned();
    for _ in 0..10 {
        let candidate = replace_suffix(&template, &random_suffix());
        match fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(
                    match crate::node_io::diagnostic(&error, "mkdtemp", Some(&template)) {
                        Some((code, message)) => {
                            SnapshotProjectionError::plain_code("Error", code, message)
                        }
                        None => SnapshotProjectionError::plain("Error", error.to_string()),
                    },
                );
            }
        }
    }
    Err(SnapshotProjectionError::plain_code(
        "Error",
        "EEXIST",
        format!("EEXIST: file already exists, mkdtemp '{template}'"),
    ))
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

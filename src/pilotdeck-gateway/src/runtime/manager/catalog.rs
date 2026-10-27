//! Read-only session catalog seam for the manager projection.
//!
//! TS `createGatewaySessionCatalogConsumer` (`GatewaySessionCatalog.ts:20-37`)
//! adapts a provider-neutral `SessionCatalogPort` to Gateway's list shape; the
//! local `managerSessions` method then asks it for 100 sessions and filters by
//! `sessionKey` (`createLocalGateway.ts:1259-1268`). The catalog is read-only:
//! the writer owns the transcript and the catalog only observes already durable
//! sessions.

use super::projection::ManagerError;
use crate::storage::{AdapterError, AsyncTranscriptAdapter, ProjectKey};
use pilotdeck_protocol::JsValue;
use std::sync::Arc;

/// TS `managerSessions` passes the literal `limit: 100` and no cursor.
pub(crate) const MANAGER_SESSION_LIMIT: f64 = 100.0;

/// `SessionCatalogListInput` (`SessionCatalogPort.ts:16-24`).
#[derive(Clone, Debug, PartialEq)]
pub struct SessionCatalogListInput {
    pub project_root: std::path::PathBuf,
    pub pilot_home: std::path::PathBuf,
    pub limit: Option<f64>,
    pub offset: Option<f64>,
    /// Native JSONL compatibility: `always-on-*` helper sessions are hidden by
    /// default. `managerSessions` never passes the flag, so its value is false.
    pub include_internal: bool,
}

impl SessionCatalogListInput {
    pub fn new(
        project_root: impl Into<std::path::PathBuf>,
        pilot_home: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            project_root: project_root.into(),
            pilot_home: pilot_home.into(),
            limit: None,
            offset: None,
            include_internal: false,
        }
    }

    /// The exact shape the TS catalog receives from `registry.listSessions`.
    #[cfg(test)]
    pub(crate) fn as_call_json(&self) -> JsValue {
        let mut object = pilotdeck_protocol::JsObject::new();
        object.insert(
            "projectRoot".into(),
            self.project_root.to_string_lossy().as_ref().into(),
        );
        object.insert(
            "pilotHome".into(),
            self.pilot_home.to_string_lossy().as_ref().into(),
        );
        object.insert(
            "limit".into(),
            self.limit.map_or(JsValue::Undefined, JsValue::Number),
        );
        object.insert(
            "offset".into(),
            self.offset.map_or(JsValue::Undefined, JsValue::Number),
        );
        JsValue::Object(object)
    }
}

/// Provider-neutral read-only catalog (`SessionCatalogPort.ts:26-28`).
///
/// Errors are already TS-shaped: the provider owns its error name/code/message
/// and the projection only adds the `{items: ...}` envelope. Rust never guesses
/// a provider's wording.
pub trait SessionCatalogPort: Send + Sync {
    fn list(&self, input: &SessionCatalogListInput) -> Result<Vec<JsValue>, ManagerError>;
}

/// The consumer used by the manager projection.
#[derive(Clone)]
pub struct GatewaySessionCatalogConsumer {
    catalog: Arc<dyn SessionCatalogPort>,
}

impl GatewaySessionCatalogConsumer {
    pub fn new(catalog: Arc<dyn SessionCatalogPort>) -> Self {
        Self { catalog }
    }

    pub fn catalog(&self) -> &Arc<dyn SessionCatalogPort> {
        &self.catalog
    }

    pub fn list(&self, input: &SessionCatalogListInput) -> Result<Vec<JsValue>, ManagerError> {
        self.catalog.list(input)
    }

    /// `registry.listSessions({ projectKey, limit: 100 })`.
    pub fn list_manager_sessions(
        &self,
        project_root: impl Into<std::path::PathBuf>,
        pilot_home: impl Into<std::path::PathBuf>,
    ) -> Result<Vec<JsValue>, ManagerError> {
        self.catalog.list(&SessionCatalogListInput {
            project_root: project_root.into(),
            pilot_home: pilot_home.into(),
            limit: Some(MANAGER_SESSION_LIMIT),
            offset: Some(0.0),
            include_internal: false,
        })
    }
}

/// Native JSONL bridge over the existing read-only transcript adapter.
///
/// The adapter already owns record projection (`project_session_info`) and the
/// TS `listSessions` limit/offset math. This wrapper adds only the catalog
/// contract above it: the internal-session filter and the unavailable error.
/// Filtering happens before pagination, exactly like `listProjectSessions`
/// (`SessionList.ts:23-51`).
pub struct TranscriptSessionCatalog {
    adapter: Arc<AsyncTranscriptAdapter>,
}

impl TranscriptSessionCatalog {
    pub fn new(adapter: Arc<AsyncTranscriptAdapter>) -> Self {
        Self { adapter }
    }

    pub fn adapter(&self) -> &Arc<AsyncTranscriptAdapter> {
        &self.adapter
    }
}

impl SessionCatalogPort for TranscriptSessionCatalog {
    fn list(&self, input: &SessionCatalogListInput) -> Result<Vec<JsValue>, ManagerError> {
        let project = ProjectKey {
            project_root: input.project_root.clone(),
            pilot_home: input.pilot_home.clone(),
        };
        // Read the whole ordered list, filter catalog-internal sessions, then
        // apply the same slice semantics as TS. Asking the adapter for a page
        // first would let an internal helper consume the manager's limit.
        let sessions = match self.adapter.list_sessions(&project, None, None) {
            Some(Ok(sessions)) => sessions,
            Some(Err(error)) => return Err(adapter_error(error)),
            None => return Err(ManagerError::catalog_unavailable()),
        };
        let sessions = sessions
            .into_iter()
            .filter(|session| input.include_internal || !is_internal_session(session))
            .collect();
        Ok(paginate(sessions, input.offset.unwrap_or(0.0), input.limit))
    }
}

/// `mergeMetadata`-style provider errors keep their JS name/code in the
/// existing adapter test oracle; reuse that exact mapping here.
fn adapter_error(error: AdapterError) -> ManagerError {
    match error {
        AdapterError::Validation { code, message } => ManagerError::new(
            "SessionEventValidationError",
            Some(code.to_owned()),
            message,
        ),
        AdapterError::Type(message) => ManagerError::plain("TypeError", message),
        AdapterError::Range(message) => ManagerError::plain("RangeError", message),
        AdapterError::ProjectionUnavailable(message) => {
            ManagerError::plain("ProjectionUnavailable", message)
        }
        AdapterError::Domain { code, message } => ManagerError::new(
            "SessionDomainValidationError",
            Some(code.to_owned()),
            message,
        ),
        AdapterError::Store(error) => ManagerError::plain("Error", error.to_string()),
    }
}

fn is_internal_session(session: &JsValue) -> bool {
    let Some(session_id) = session.get("sessionId").and_then(JsValue::as_string) else {
        return false;
    };
    let units = session_id.units();
    ["discovery", "workspace", "report"].iter().any(|kind| {
        let prefix: Vec<u16> = format!("always-on-{kind}").encode_utf16().collect();
        if !units.starts_with(&prefix) {
            return false;
        }
        matches!(units.get(prefix.len()), Some(0x3a | 0x2d))
    })
}

fn paginate(sessions: Vec<JsValue>, offset: f64, limit: Option<f64>) -> Vec<JsValue> {
    let offset = if offset.is_nan() {
        offset
    } else {
        offset.max(0.0)
    };
    let len = sessions.len();
    let start = slice_index(offset, len);
    let limit = limit.unwrap_or(len as f64);
    let end = if limit == 0.0 {
        len
    } else {
        slice_index(offset + limit, len)
    };
    sessions
        .into_iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

/// `Array.prototype.slice` index conversion over a finite array: NaN and
/// negatives clamp to 0, values at/above the length clamp to the length.
fn slice_index(value: f64, len: usize) -> usize {
    if value.is_nan() || value <= 0.0 {
        0
    } else if value >= len as f64 {
        len
    } else {
        value as usize
    }
}

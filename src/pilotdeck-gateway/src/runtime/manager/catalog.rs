//! Read-only session catalog seam for the manager projection.
//!
//! TS `createGatewaySessionCatalogConsumer` (`GatewaySessionCatalog.ts:20-37`)
//! adapts a provider-neutral `SessionCatalogPort` to Gateway's list shape; the
//! local `managerSessions` method then asks it for 100 sessions and filters by
//! `sessionKey` (`createLocalGateway.ts:1259-1268`). The catalog is read-only:
//! the writer owns the transcript and the catalog only observes already durable
//! sessions.
//!
//! [`TranscriptSessionCatalog`] is the native JSONL implementation selected by
//! `createLocalGateway.ts:685` (`createNodeSessionCatalog` -> `SessionList.ts`).
//! Its projection and partial-tail semantics live in [`super::file_catalog`].

use super::file_catalog;
use super::projection::ManagerError;
use crate::storage::{AsyncTranscriptAdapter, ProjectKey, slice_index};
use pilotdeck_protocol::JsValue;
use std::{path::PathBuf, sync::Arc};

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

/// Native JSONL bridge over the TS file catalog.
///
/// `createLocalGateway.ts:685` uses `createNodeSessionCatalog`, whose `list`
/// is `SessionList.ts::listProjectSessions`. That provider reads JSONL files
/// directly; it does not use the `AsyncTranscriptStorageAdapter` projection.
/// The adapter is retained only for the project chat-directory mapping, which
/// is the same `getPilotProjectChatDir` seam the host already owns.
pub struct TranscriptSessionCatalog {
    chat_dir: Arc<dyn Fn(&ProjectKey) -> PathBuf + Send + Sync>,
}

impl TranscriptSessionCatalog {
    pub fn new(adapter: Arc<AsyncTranscriptAdapter>) -> Self {
        Self {
            chat_dir: Arc::new(move |project| adapter.project_chat_dir(project)),
        }
    }
}

impl SessionCatalogPort for TranscriptSessionCatalog {
    fn list(&self, input: &SessionCatalogListInput) -> Result<Vec<JsValue>, ManagerError> {
        let project = ProjectKey {
            project_root: input.project_root.clone(),
            pilot_home: input.pilot_home.clone(),
        };
        // Filter `always-on-*` names before opening any file (P1-2), then sort
        // by mtime and apply JS `slice` pagination (P2 negative-end).
        let sessions = file_catalog::list_sessions(
            &(self.chat_dir)(&project),
            &input.project_root,
            input.include_internal,
        )
        .map_err(|message| ManagerError::with_message("SyntaxError", None, message))?;
        Ok(paginate(
            sessions.into_iter().map(|(_, info)| info).collect(),
            input.offset.unwrap_or(0.0),
            input.limit,
        ))
    }
}

fn paginate(sessions: Vec<JsValue>, offset: f64, limit: Option<f64>) -> Vec<JsValue> {
    // `Math.max(0, offset ?? 0)`; a NaN offset stays NaN and `slice` maps it 0.
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

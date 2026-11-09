//! Manager session/browser projection for U4 (`createLocalGateway.ts:1259-1270`).
//!
//! This component is deliberately **not** registered in the native dispatch
//! table: `dispatch.rs` keeps `manager_sessions` / `manager_browsers` owned by
//! the TS host until A8 takes the WS entry. The projection consumes the same
//! two seams the real local Gateway does:
//!
//! - sessions: `ProjectRuntimeRegistry.listSessions` -> the injected
//!   `SessionCatalogPort`, called as `{ projectRoot, pilotHome, limit: 100,
//!   offset: 0 }`;
//! - browsers: the host-injected manager browser provider, whose successful
//!   result is returned unchanged.
//!
//! `TranscriptSessionCatalog` is the native JSONL file catalog selected by
//! `createLocalGateway.ts:685` (`createNodeSessionCatalog` ->
//! `SessionList.ts::listProjectSessions`), not the unrelated
//! `AsyncTranscriptStorageAdapter` projection. It reads head/tail lite
//! snapshots, keeps the committed prefix when an append leaves a half-written
//! tail, filters `always-on-*` sessions before opening any file, and emits the
//! TS field order including `cwd`. It never writes and does not change the
//! shared transcript adapter.
//!
//! The browser success path is provider-driven, not a permanent
//! `CAPABILITY_UNAVAILABLE`: an absent provider produces the TS gateway-method
//! error, while an injected provider owns both values and errors.

mod browser;
mod catalog;
pub mod session_mcp;
pub use browser::{BrowserManager, BrowserRuntimeLease};
mod file_catalog;
mod projection;
#[cfg(test)]
mod tests;

pub use catalog::{
    GatewaySessionCatalogConsumer, SessionCatalogListInput, SessionCatalogPort,
    TranscriptSessionCatalog,
};
pub use projection::{
    ManagerBrowserPort, ManagerBrowsers, ManagerDispatch, ManagerError, ManagerGateway,
    ManagerProjectResolver, ManagerProjection, ManagerProjects, ManagerSessions,
};

/// Wire method names owned by this component. They are intentionally absent
/// from `dispatch.rs::LOCAL_METHODS` in this package.
pub const MANAGER_METHODS: &[&str] = &["manager_sessions", "manager_browsers"];

/// SOP authorization must include internal sessions and must not paginate.
pub(crate) fn contains_session_units(
    home: &std::path::Path,
    cwd: &std::path::Path,
    project: &str,
    session: &pilotdeck_protocol::JsString,
) -> Result<bool, crate::runtime::connection::RequestError> {
    let metadata = crate::metadata::Metadata {
        home: crate::metadata::resolve_path(cwd, home),
        cwd: cwd.to_owned(),
        files: crate::metadata::NativeFiles,
    };
    let requested = crate::metadata::resolve_path(cwd, std::path::Path::new(project));
    if requested != metadata.home
        && !metadata
            .has_registered_project(&requested)
            .map_err(|err| crate::runtime::connection::RequestError::new(err.to_string()))?
    {
        return Ok(false);
    }
    let chat = metadata.chat_dir(project);
    let sessions = file_catalog::list_sessions(&chat, std::path::Path::new(project), true)
        .map_err(crate::runtime::connection::RequestError::new)?;
    Ok(sessions.iter().any(|(_, item)| {
        item.get("sessionId")
            .and_then(pilotdeck_protocol::JsValue::as_string)
            == Some(session)
    }))
}

#[cfg(test)]
mod sop_membership_tests {
    #[test]
    fn sop_catalog_includes_internal_and_unpaginated_sessions() {
        let root =
            std::env::temp_dir().join(format!("pdrs-sop-membership-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let project = root.to_string_lossy().into_owned();
        let chats = crate::session_chat_dir(&root, &root, &project);
        std::fs::create_dir_all(&chats).unwrap();
        let entry = r#"{"type":"session_metadata","metadata":{"title":"fixture"}}"#;
        for index in 0..101 {
            std::fs::write(chats.join(format!("session-{index}.jsonl")), entry).unwrap();
        }
        std::fs::write(chats.join("always-on-report:hidden.jsonl"), entry).unwrap();
        assert!(crate::metadata::is_internal_session(
            "always-on-report:hidden"
        ));
        let oldest = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        std::fs::File::options()
            .write(true)
            .open(chats.join("session-0.jsonl"))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(oldest))
            .unwrap();

        assert!(
            super::contains_session_units(&root, &root, &project, &"session-0".into()).unwrap()
        );
        assert!(
            super::contains_session_units(
                &root,
                &root,
                &project,
                &"always-on-report:hidden".into()
            )
            .unwrap()
        );
        assert!(!super::contains_session_units(&root, &root, &project, &"missing".into()).unwrap());
        assert!(
            !super::contains_session_units(
                &root,
                &root,
                "/unregistered-project",
                &"session-100".into()
            )
            .unwrap()
        );
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let registration = root.join("projects/registered");
        std::fs::create_dir_all(&registration).unwrap();
        std::fs::write(registration.join(".cwd"), ".").unwrap();
        let relative_chats = crate::session_chat_dir(&root, &workspace, ".");
        std::fs::create_dir_all(&relative_chats).unwrap();
        std::fs::write(relative_chats.join("relative-session.jsonl"), entry).unwrap();
        assert!(
            super::contains_session_units(&root, &workspace, ".", &"relative-session".into())
                .unwrap()
        );
    }
}

pub(crate) fn browser_provider_unavailable() -> RequestError {
    RequestError {
        code: "CAPABILITY_UNAVAILABLE".into(),
        ..RequestError::new("Gateway manager browser provider is unavailable.")
    }
}

use crate::metadata::{Metadata, NativeFiles, resolve_path, resolved_identity};
use crate::runtime::connection::RequestError;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::path::{Path, PathBuf};

pub async fn sessions(home: &Path, cwd: &Path, params: &JsValue) -> Result<JsValue, RequestError> {
    let (home, cwd, params) = (home.to_owned(), cwd.to_owned(), params.clone());
    tokio::task::spawn_blocking(move || {
        let scope = resolve_scope(&home, &cwd, &params)?;
        let chat = scope.metadata.chat_dir(&scope.project.to_string_lossy());
        let mut listed = file_catalog::list_sessions(&chat, &scope.project, false)
            .map_err(RequestError::new)?.into_iter().map(|(_, item)| item).collect::<Vec<_>>();
        // createLocalGateway.ts:1263-1267 filters only after the catalog's limit.
        listed.truncate(100);
        if truthy(&scope.session) {
            listed.retain(|item| {
                item.get("sessionId") == Some(&scope.session)
                    || item.get("sessionKey") == Some(&scope.session)
            });
        }
        Ok(items(listed))
    })
    .await
    .map_err(|error| RequestError::new(error.to_string()))?
}


/// LocalServer uses the same registry its session MCP owners register with.
pub async fn browsers(home: &Path, cwd: &Path, params: &JsValue) -> Result<JsValue, RequestError> {
    BrowserManager::production()
        .browsers(home, cwd, params)
        .await
}

struct Scope {
    metadata: Metadata,
    project: PathBuf,
    session: JsValue,
}

fn resolve_scope(home: &Path, cwd: &Path, params: &JsValue) -> Result<Scope, RequestError> {
    if matches!(params, JsValue::Null | JsValue::Undefined) {
        let kind = if params.is_undefined() {
            "undefined"
        } else {
            "null"
        };
        return Err(RequestError::new(format!(
            "Cannot read properties of {kind} (reading 'projectKey')"
        )));
    }
    let home = resolve_path(cwd, home);
    let metadata = Metadata {
        home,
        cwd: cwd.to_owned(),
        files: NativeFiles,
    };
    let undefined = JsValue::Undefined;
    let raw = params.get("projectKey").unwrap_or(&undefined);
    let key = match raw {
        JsValue::Undefined | JsValue::Null => JsString::from(cwd.to_string_lossy().as_ref()),
        JsValue::String(key) => key.clone(),
        _ => {
            let received = match crate::metadata::argument_received(raw) {
                crate::metadata::ArgumentReceived::Text(text) => text,
                crate::metadata::ArgumentReceived::Throws { message, .. } => {
                    return Err(RequestError::new(message));
                }
            };
            let mut message = "The \"paths[0]\" argument must be of type string. Received "
                .encode_utf16().collect::<Vec<_>>();
            message.extend_from_slice(received.units());
            let mut error = RequestError::new(JsString::from_utf16(message));
            error.code = "ERR_INVALID_ARG_TYPE".into();
            return Err(error);
        }
    };
    let requested = resolved_identity(cwd, &key).map(PathBuf::from);
    let registered = match requested.as_ref() {
        Some(project) if project == &metadata.home => true,
        Some(project) => metadata.has_registered_project(project)
            .map_err(|error| RequestError::new(error.to_string()))?,
        None => false,
    };
    let Some(project) = requested.filter(|_| registered) else {
        let mut message: Vec<u16> = "Unknown projectKey: ".encode_utf16().collect();
        message.extend_from_slice(key.units());
        let mut error = RequestError::new(JsString::from_utf16(message));
        error.code = "PROJECT_NOT_FOUND".into();
        return Err(error);
    };
    Ok(Scope {
        metadata,
        project,
        session: params
            .get("sessionKey")
            .cloned()
            .unwrap_or(JsValue::Undefined),
    })
}

fn items(values: Vec<JsValue>) -> JsValue {
    let mut result = JsObject::new();
    result.insert("items".into(), JsValue::Array(values));
    JsValue::Object(result)
}

fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}

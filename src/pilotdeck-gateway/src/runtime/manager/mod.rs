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

mod catalog;
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

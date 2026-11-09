//! SESSION19 composition. Domain owners return values; the WS boundary owns frames.
pub mod history;
pub mod host;
mod lifecycle;
pub mod live;
pub(crate) mod passthrough;
mod server;
pub mod store;
pub mod transcript;

use super::SessionOwner;
use crate::{
    metadata::{Metadata, NativeFiles, object},
    runtime::{
        connection::{Call, RequestError},
        interaction::GatewayInteractionCoordinator,
        replacement::{ReplacementCoordinator, ReplacementError},
        turn::coordinator::TurnEventCoordinator,
    },
    storage::AsyncTranscriptAdapter,
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{path::PathBuf, sync::Arc};

pub const METHODS: &[&str] = &[
    "new_session",
    "resume_session",
    "close_session",
    "close_project_sessions",
    "delete_session",
    "rename_session",
    "tag_session",
    "list_sessions",
    "read_session_messages",
    "read_subagent_messages",
    "fork_session",
    "replace_last_turn",
    "finalize_last_turn_replacement",
    "export_session_transcript",
    "restore_session_transcript",
    "seed_read_state",
    "record_agent_status_message",
    "reconnect_interaction",
    "describe_server",
];

pub struct SessionRoutes {
    owner: SessionOwner,
    interactions: Arc<GatewayInteractionCoordinator>,
    lifecycle: lifecycle::LifecycleRoutes,
    history: history::HistoryRoutes,
    transcript: transcript::TranscriptRoutes,
    pub replacement: Arc<ReplacementCoordinator>,
    pub history_storage: Arc<history::HistoryStorage>,
    run_registry: bool,
}

pub type CloseSession =
    Arc<dyn Fn(&JsString, &JsString) -> Result<(), crate::config::Error> + Send + Sync>;

pub struct SessionRouteOptions {
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub owner: SessionOwner,
    pub interactions: Arc<GatewayInteractionCoordinator>,
    pub coordinator: Arc<TurnEventCoordinator>,
    pub adapter: Arc<AsyncTranscriptAdapter>,
    pub history_storage: Arc<history::HistoryStorage>,
    pub live: Arc<live::LiveBindings>,
    pub transcript_host: Arc<dyn transcript::TranscriptHost>,
    pub on_close: CloseSession,
    pub run_registry: bool,
}

impl SessionRoutes {
    pub fn new(options: SessionRouteOptions) -> Self {
        let storage = options.history_storage;
        let replacement = Arc::new(ReplacementCoordinator::new(
            storage.replacement_storage(),
            Arc::new(options.owner.clone()),
            30_000,
            None,
        ));
        let history = history::HistoryRoutes::new(
            storage.clone(),
            options.coordinator,
            replacement.as_ref().clone(),
        );
        let interactions = options.interactions.clone();
        let close = options.on_close;
        let lifecycle = lifecycle::LifecycleRoutes::new(
            options.owner.clone(),
            Metadata {
                home: options.home,
                cwd: options.cwd.clone(),
                files: NativeFiles,
            },
            options.adapter,
            options.live,
            options.cwd,
            Arc::new(move |session, reason| {
                interactions.close_session(session, reason);
                close(session, reason)
            }),
        );
        Self {
            transcript: transcript::TranscriptRoutes::new(
                options.owner.clone(),
                options.transcript_host,
            ),
            owner: options.owner,
            interactions: options.interactions,
            lifecycle,
            history,
            replacement,
            history_storage: storage,
            run_registry: options.run_registry,
        }
    }

    pub fn describe_server(&self) -> JsValue {
        server::describe(&self.owner, self.run_registry)
    }

    pub fn handle<'a>(
        &'a self,
        method: &str,
        params: &'a JsValue,
    ) -> Option<Call<'a, Result<JsValue, RequestError>>> {
        let index = METHODS.iter().position(|candidate| *candidate == method)?;
        let method = METHODS[index];
        Some(Box::pin(async move {
            match method {
                "describe_server" => return Ok(self.describe_server()),
                "reconnect_interaction" => return server::reconnect(&self.interactions, params),
                _ => {}
            }
            if let Some(result) = self.lifecycle.handle(method, params).await {
                return result.map_err(config_error);
            }
            if let Some(result) = self.history.handle(method, params).await {
                return result.map_err(replacement_error);
            }
            self.transcript
                .handle(method, params)
                .await
                .expect("SESSION19 method belongs to one registered domain")
                .map_err(config_error)
        }))
    }
}

pub(super) fn config_error(error: crate::config::Error) -> RequestError {
    RequestError {
        code: if error.code.is_empty() {
            "gateway_request_failed".into()
        } else {
            error.code
        },
        message: error.message,
        details: None,
        validation: None,
    }
}

fn replacement_error(error: ReplacementError) -> RequestError {
    RequestError {
        code: error.code.unwrap_or("gateway_request_failed").into(),
        message: error.message.into(),
        details: error.details,
        validation: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_surface_is_exactly_nineteen_unique_methods() {
        let names: std::collections::HashSet<_> = METHODS.iter().collect();
        assert_eq!(METHODS.len(), 19);
        assert_eq!(names.len(), 19);
        assert!(!METHODS.contains(&"submit_turn"));
        assert!(!METHODS.contains(&"permission_decide"));
    }
}

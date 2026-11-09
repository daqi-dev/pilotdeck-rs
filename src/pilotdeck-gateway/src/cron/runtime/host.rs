use super::{EventSink, HostFuture};
use crate::cron::always_on::{AlwaysOnHost, RunTools, TurnRequest};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// All fields are host-internal; this does not extend the gateway wire protocol.
pub struct NativeTurnRequest {
    pub project_key: String,
    pub session_key: String,
    pub run_id: String,
    pub channel_key: String,
    pub message: String,
    pub cwd: PathBuf,
    pub permission_mode: &'static str,
    pub can_prompt: bool,
    pub excluded_tools: Vec<&'static str>,
    pub permission_rules: Value,
    pub tools: Option<Arc<RunTools>>,
    pub timeout_ms: Option<u64>,
    pub on_event: EventSink,
}

/// The implementation runs on the same serial host as foreground submissions.
/// It must call on_event while streaming and finish durable cleanup before return.
pub trait NativeHost: Send + Sync {
    fn prepare_task<'a>(&'a self, task: &'a Value) -> HostFuture<'a, ()>;
    fn release_task<'a>(&'a self, task: &'a Value) -> HostFuture<'a, ()>;
    fn turn(&self, request: NativeTurnRequest) -> HostFuture<'_, Vec<Value>>;
    fn abort<'a>(
        &'a self,
        session_key: &'a str,
        run_id: &'a str,
        reason: &'a str,
    ) -> HostFuture<'a, ()>;
    fn close<'a>(&'a self, session_key: &'a str) -> HostFuture<'a, ()>;
    fn on_turn_event(&self, session_key: &str, channel_key: &str, event: &Value);
    fn on_worktree_created(&self, _run_id: &str, _cwd: &Path) {}
    fn on_worktree_removed(&self, _cwd: &Path) {}
    fn on_phase_event(&self, event: &Value);
    fn deliver_result<'a>(&'a self, delivery: &'a Value) -> HostFuture<'a, ()>;
}

/// Both automation domains share one native submit/abort/close implementation.
pub struct NativeAlwaysOnAdapter(pub Arc<dyn NativeHost>);
impl AlwaysOnHost for NativeAlwaysOnAdapter {
    fn turn(&self, request: TurnRequest) -> HostFuture<'_, Vec<Value>> {
        self.0.turn(NativeTurnRequest {
            project_key: request.project_key,
            session_key: request.session_key,
            run_id: request.run_id,
            channel_key: request.channel_key,
            message: request.message,
            cwd: request.cwd,
            permission_mode: request.permission_mode,
            can_prompt: request.can_prompt,
            excluded_tools: request.excluded_tools,
            permission_rules: request.permission_rules,
            tools: Some(request.tools),
            timeout_ms: None,
            on_event: request.on_event,
        })
    }
    fn abort<'a>(&'a self, session: &'a str, run: &'a str, reason: &'a str) -> HostFuture<'a, ()> {
        self.0.abort(session, run, reason)
    }
    fn close<'a>(&'a self, session: &'a str) -> HostFuture<'a, ()> {
        self.0.close(session)
    }
    fn on_turn_event(&self, session: &str, channel: &str, event: &Value) {
        self.0.on_turn_event(session, channel, event);
    }
    fn on_worktree_created(&self, run: &str, cwd: &Path) {
        self.0.on_worktree_created(run, cwd);
    }
    fn on_worktree_removed(&self, cwd: &Path) {
        self.0.on_worktree_removed(cwd);
    }
}

//! Workspace phase tool. The real provider owns its subprocess and file jobs.
use std::{future::Future, pin::Pin, sync::{Arc, atomic::{AtomicUsize, Ordering}}};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use crate::runtime::tools::{ToolError, ToolFuture, ToolOutput, ToolRunContext, builtin::bash::AbortSignal};
use super::{tools::PhaseToolHandler, workspace::{NativeWorkspace, WorkspaceError, WorkspaceHandle}};

pub(crate) trait WorkspacePrepare: Send + Sync {
    fn prepare<'a>(&'a self, strategy: &'a str, run: JsString, title: JsString, signal: Option<AbortSignal>)
        -> Pin<Box<dyn Future<Output = Result<WorkspaceHandle, WorkspaceError>> + Send + 'a>>;
}
impl WorkspacePrepare for NativeWorkspace {
    fn prepare<'a>(&'a self, strategy: &'a str, run: JsString, title: JsString, signal: Option<AbortSignal>)
        -> Pin<Box<dyn Future<Output = Result<WorkspaceHandle, WorkspaceError>> + Send + 'a>> {
        Box::pin(NativeWorkspace::prepare(self, strategy, run, title, signal))
    }
}
#[derive(Default)]
pub(crate) struct WorkspaceState { pub handle: Option<WorkspaceHandle> }
#[derive(Default)]
pub(crate) struct WorkspacePending { count: AtomicUsize, changed: tokio::sync::Notify }
impl WorkspacePending {
    pub(crate) async fn drain(&self) {
        loop { let changed = self.changed.notified(); if self.count.load(Ordering::Acquire) == 0 { return; } changed.await; }
    }
}
struct PendingGuard(Arc<WorkspacePending>);
impl Drop for PendingGuard {
    fn drop(&mut self) { if self.0.count.fetch_sub(1, Ordering::AcqRel) == 1 { self.0.changed.notify_one(); } }
}
pub(crate) struct WorkspaceHandler {
    pub run_id: JsString,
    pub title: JsString,
    pub workspace: Arc<dyn WorkspacePrepare>,
    pub state: Arc<tokio::sync::Mutex<WorkspaceState>>,
    pub pending: Arc<WorkspacePending>,
}
impl PhaseToolHandler for WorkspaceHandler {
    fn execute(&self, name: &'static str, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        if name != "always_on_prepare_workspace" {
            return Box::pin(async move { Err(ToolError::failed(format!("{name} is not available in this Always-On phase").as_str())) });
        }
        let strategy = input.get("strategy").and_then(JsValue::as_string).and_then(|s| s.to_utf8().ok());
        let Some(strategy) = strategy.filter(|s| ["auto", "git-worktree", "snapshot-copy"].contains(&s.as_str())) else {
            return Box::pin(async { Err(ToolError::new("invalid_input", "Invalid workspace strategy")) });
        };
        if context.is_aborted() { return Box::pin(async { Err(ToolError::aborted()) }); }
        let signal = AbortSignal::from_context(&context); let workspace = self.workspace.clone();
        let run = self.run_id.clone(); let title = self.title.clone(); let state = self.state.clone();
        self.pending.count.fetch_add(1, Ordering::AcqRel); let pending = PendingGuard(self.pending.clone());
        let work = tokio::spawn(async move {
            let _pending = pending; let mut state = state.lock().await;
            if state.handle.is_some() { return Err(ToolError::failed("workspace_already_prepared: a workspace was already created for this run.")); }
            let handle = workspace.prepare(&strategy, run.clone(), title, signal).await
                .map_err(|error| ToolError::failed(error.message))?;
            let mut data = JsObject::new(); data.insert("ok".into(), JsValue::Bool(true));
            data.insert("strategy".into(), handle.strategy.as_str().into()); data.insert("cwd".into(), JsValue::String(handle.cwd.clone()));
            data.insert("reused".into(), JsValue::Bool(false));
            let mut text = "Workspace prepared at ".encode_utf16().collect::<Vec<_>>(); text.extend_from_slice(handle.cwd.units());
            text.extend(format!(" ({}).", handle.strategy).encode_utf16());
            state.handle = Some(handle);
            let mut block = JsObject::new(); block.insert("type".into(), "text".into()); block.insert("text".into(), JsValue::String(JsString::from_utf16(text)));
            let mut json = JsObject::new(); json.insert("type".into(), "json".into()); json.insert("value".into(), JsValue::Object(data.clone()));
            let mut metadata = JsObject::new(); metadata.insert("runId".into(), JsValue::String(run));
            Ok(ToolOutput { content: vec![JsValue::Object(block), JsValue::Object(json)], data: Some(JsValue::Object(data)),
                metadata: Some(metadata), ..Default::default() })
        });
        Box::pin(async move { work.await.map_err(|e| ToolError::failed(e.to_string().as_str()))? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext};
    struct Provider;
    impl WorkspacePrepare for Provider {
        fn prepare<'a>(&'a self, strategy: &'a str, run: JsString, _: JsString, _: Option<AbortSignal>)
            -> Pin<Box<dyn Future<Output = Result<WorkspaceHandle, WorkspaceError>> + Send + 'a>> {
            Box::pin(async move { Ok(WorkspaceHandle { run_id: run, project_key: "/p".into(), strategy: strategy.into(), cwd: "/p/workspace".into(), metadata: JsObject::new(), created_identity: None }) })
        }
    }
    fn context() -> ToolRunContext {
        ToolRunContext::new(Arc::new(ToolRuntimeContext {
            fields: JsObject::new(), files: HostToolCheckpoint::default().tool_context_state(),
            abort_signal: None, model: None, elicitation: None, user_dialog: None,
            audit_recorder: None, file_history: None, file_update_notifier: None,
            now: None, progress: None, subagent: None, plan_todo: None,
            goal: None, plan_directory: None,
        }))
    }
    #[tokio::test]
    async fn prepared_workspace_is_owned_and_duplicate_is_refused() {
        let state = Arc::new(tokio::sync::Mutex::new(WorkspaceState::default()));
        let handler = WorkspaceHandler { run_id: "run".into(), title: "title".into(), workspace: Arc::new(Provider),
            state: state.clone(), pending: Arc::default() };
        let mut input = JsObject::new(); input.insert("strategy".into(), "snapshot-copy".into());
        let output = handler.execute("always_on_prepare_workspace", JsValue::Object(input.clone()), context()).await.unwrap();
        assert_eq!(output.data.as_ref().and_then(|d| d.get("reused")), Some(&JsValue::Bool(false)));
        assert!(handler.execute("always_on_prepare_workspace", JsValue::Object(input), context()).await.is_err());
        assert!(state.lock().await.handle.as_ref().unwrap().cwd.equals_str("/p/workspace"));
        handler.pending.drain().await;
    }
}

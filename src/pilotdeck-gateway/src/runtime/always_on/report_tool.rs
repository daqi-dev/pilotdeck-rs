//! The report phase's concrete handler, backed by the bounded report store.
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use crate::runtime::tools::{ToolError, ToolFuture, ToolOutput, ToolRunContext};
use super::{report_contract::{self, ReportMetadata}, reports::DiscoveryReportStore,
    tools::PhaseToolHandler};

#[derive(Clone)]
pub(crate) struct SavedReport {
    pub markdown: JsString,
    pub file_path: JsString,
    pub finished_at: JsString,
}
#[derive(Default)]
pub(crate) struct ReportState {
    pub calls: usize,
    pub report: Option<SavedReport>,
}
#[derive(Default)]
pub(crate) struct ReportPending { count: AtomicUsize, changed: tokio::sync::Notify }
impl ReportPending {
    pub(crate) async fn drain(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.count.load(Ordering::Acquire) == 0 { return; }
            notified.await;
        }
    }
}
struct PendingGuard(Arc<ReportPending>);
impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.0.count.fetch_sub(1, Ordering::AcqRel) == 1 { self.0.changed.notify_waiters(); }
    }
}
pub(crate) struct ReportHandler {
    pub run_id: JsString,
    pub plan_id: JsString,
    pub started_at: Option<JsString>,
    pub workspace_strategy: JsString,
    pub workspace_handle: JsString,
    pub store: DiscoveryReportStore,
    pub state: Arc<tokio::sync::Mutex<ReportState>>,
    pub pending: Arc<ReportPending>,
}
impl PhaseToolHandler for ReportHandler {
    fn execute(&self, name: &'static str, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        if name != "always_on_report" {
            return Box::pin(async move { Err(ToolError::failed(format!("{name} is not available in this Always-On phase").as_str())) });
        }
        let Some(content) = input.get("content").and_then(JsValue::as_string).cloned() else {
            return Box::pin(async { Err(ToolError::new("invalid_input", "content must be a string")) });
        };
        if context.is_aborted() { return Box::pin(async { Err(ToolError::aborted()) }); }
        let finished = match context.now_iso() {
            Ok(value) => value,
            Err(error) => return Box::pin(async move { Err(ToolError::failed(error.to_string().as_str())) }),
        };
        let run = self.run_id.clone(); let plan = self.plan_id.clone();
        let metadata = ReportMetadata {
            run_id: run.clone(), plan_id: plan.clone(),
            started_at: self.started_at.clone().unwrap_or_else(|| finished.clone()),
            finished_at: finished.clone(), outcome: "executed".into(),
            workspace_strategy: self.workspace_strategy.clone(), workspace_handle: self.workspace_handle.clone(),
        };
        let store = self.store.clone(); let state = self.state.clone();
        self.pending.count.fetch_add(1, Ordering::AcqRel);
        let pending = PendingGuard(self.pending.clone());
        // The owned task retains the serial state lock across the complete
        // bounded store write even if the caller drops its response future.
        let work = tokio::spawn(async move {
            let _pending = pending;
            let mut state = state.lock().await;
            state.calls = state.calls.saturating_add(1);
            let parsed = report_contract::parse_report(&content, &metadata);
            let path = store.write_report(run.clone(), parsed.raw.clone()).await
                .map_err(|e| ToolError::failed(e.to_string().as_str()))?;
            let path = path.to_str().ok_or_else(|| ToolError::failed("Report path is not UTF-8"))?;
            let file_path = JsString::from(path);
            state.report = Some(SavedReport { markdown: parsed.raw, file_path: file_path.clone(), finished_at: finished });
            let mut data = JsObject::new();
            data.insert("ok".into(), JsValue::Bool(true));
            data.insert("reportFilePath".into(), JsValue::String(file_path.clone()));
            data.insert("fallbacks".into(), JsValue::Array(parsed.fallbacks.into_iter().map(JsValue::String).collect()));
            let mut text = "Work report saved at ".encode_utf16().collect::<Vec<_>>();
            text.extend_from_slice(file_path.units()); text.push(u16::from(b'.'));
            let mut text_block = JsObject::new(); text_block.insert("type".into(), "text".into());
            text_block.insert("text".into(), JsValue::String(JsString::from_utf16(text)));
            let mut json_block = JsObject::new(); json_block.insert("type".into(), "json".into());
            json_block.insert("value".into(), JsValue::Object(data.clone()));
            let mut output_meta = JsObject::new(); output_meta.insert("runId".into(), JsValue::String(run));
            output_meta.insert("planId".into(), JsValue::String(plan));
            Ok(ToolOutput { content: vec![JsValue::Object(text_block), JsValue::Object(json_block)],
                data: Some(JsValue::Object(data)), metadata: Some(output_meta), ..Default::default() })
        });
        Box::pin(async move { work.await.map_err(|e| ToolError::failed(e.to_string().as_str()))? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext};
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
    async fn actual_report_store_persists_normalized_content_and_updates_phase_state() {
        let root = std::env::temp_dir().join(format!("pdrs-always-on-report-{}", uuid::Uuid::new_v4()));
        let metadata = crate::metadata::Metadata { home: root.clone(), cwd: Default::default(), files: crate::metadata::NativeFiles };
        let paths = super::super::paths::resolve_always_on_paths(&root, "/fixture/project", &metadata).unwrap();
        let state = Arc::new(tokio::sync::Mutex::new(ReportState::default()));
        let handler = ReportHandler { run_id: "run".into(), plan_id: "plan".into(), started_at: None,
            workspace_strategy: "snapshot-copy".into(), workspace_handle: "/workspace".into(),
            store: DiscoveryReportStore::new(paths), state: state.clone(), pending: Arc::default() };
        let mut input = JsObject::new(); input.insert("content".into(), "# Result\n## Notes\nactual notes\n".into());
        let output = handler.execute("always_on_report", JsValue::Object(input), context()).await.unwrap();
        assert_eq!(output.data.as_ref().and_then(|d| d.get("ok")), Some(&JsValue::Bool(true)));
        let state = state.lock().await;
        assert_eq!(state.calls, 1);
        let report = state.report.as_ref().expect("saved report");
        let text = std::fs::read_to_string(report.file_path.to_utf8().unwrap()).unwrap();
        assert!(text.contains("actual notes"));
        assert!(text.contains("section-missing(Plan Reference)"));
        assert!(text.contains("> runId: run"));
        drop(state);
        handler.pending.drain().await;
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod concurrent_drain_tests {
    use super::*;

    #[tokio::test]
    async fn all_waiters_finish_after_the_last_owned_job() {
        let pending = Arc::new(ReportPending::default());
        pending.count.store(1, Ordering::Release);
        let guard = PendingGuard(pending.clone());
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let mut waiters = Vec::new();
        for _ in 0..2 {
            let pending = pending.clone();
            let barrier = barrier.clone();
            waiters.push(tokio::spawn(async move {
                barrier.wait().await;
                pending.drain().await;
            }));
        }
        barrier.wait().await;
        tokio::task::yield_now().await;
        drop(guard);
        for waiter in waiters {
            tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
                .await.expect("drain must wake all waiters").expect("waiter panicked");
        }
    }
}

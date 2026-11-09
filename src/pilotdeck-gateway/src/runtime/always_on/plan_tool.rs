//! Discovery plan tool backed by the project-bound bounded plan store.
use super::{
    cycles::{DiscoveryPlanRecord, DiscoveryPlanStore},
    plan_contract, storage,
    tools::PhaseToolHandler,
};
use crate::runtime::tools::{ToolError, ToolFuture, ToolOutput, ToolRunContext};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
pub(crate) struct PlanState {
    pub calls: usize,
    pub plan: Option<(DiscoveryPlanRecord, JsString)>,
}
#[derive(Default)]
pub(crate) struct PlanPending {
    count: AtomicUsize,
    changed: tokio::sync::Notify,
}
impl PlanPending {
    pub(crate) async fn drain(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.count.load(Ordering::Acquire) == 0 {
                return;
            }
            changed.await;
        }
    }
}
struct PendingGuard(Arc<PlanPending>);
impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.0.count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.changed.notify_waiters();
        }
    }
}
pub(crate) struct PlanHandler {
    pub run_id: JsString,
    pub store: DiscoveryPlanStore,
    pub state: Arc<tokio::sync::Mutex<PlanState>>,
    pub pending: Arc<PlanPending>,
}
fn whitespace(c: u16) -> bool {
    matches!(c, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a
        | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn trimmed(s: &JsString) -> JsString {
    let units = s.units();
    let a = units
        .iter()
        .position(|c| !whitespace(*c))
        .unwrap_or(units.len());
    let b = units
        .iter()
        .rposition(|c| !whitespace(*c))
        .map_or(a, |i| i + 1);
    JsString::from_utf16(units[a..b].to_vec())
}
fn required(input: &JsValue, name: &str) -> Result<JsString, ToolError> {
    input
        .get(name)
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| ToolError::new("invalid_input", format!("{name} must be a string").as_str()))
}
impl PhaseToolHandler for PlanHandler {
    fn execute(
        &self,
        name: &'static str,
        input: JsValue,
        context: ToolRunContext,
    ) -> ToolFuture<ToolOutput> {
        if name != "always_on_discovery_plan" {
            return Box::pin(async move {
                Err(ToolError::failed(
                    format!("{name} is not available in this Always-On phase").as_str(),
                ))
            });
        }
        let fields = (|| {
            Ok::<_, ToolError>((
                required(&input, "title")?,
                required(&input, "summary")?,
                required(&input, "rationale")?,
                required(&input, "dedupeKey")?,
                required(&input, "content")?,
            ))
        })();
        let (title, summary, rationale, dedupe, content) = match fields {
            Ok(fields) => fields,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        if context.is_aborted() {
            return Box::pin(async { Err(ToolError::aborted()) });
        }
        let now = match context.now_iso() {
            Ok(value) => value,
            Err(error) => {
                return Box::pin(async move { Err(ToolError::failed(error.to_string().as_str())) });
            }
        };
        let run = self.run_id.clone();
        let store = self.store.clone();
        let state = self.state.clone();
        self.pending.count.fetch_add(1, Ordering::AcqRel);
        let pending = PendingGuard(self.pending.clone());
        let work = tokio::spawn(async move {
            let _pending = pending;
            let mut state = state.lock().await;
            state.calls = state.calls.saturating_add(1);
            if state.plan.is_some() {
                return Err(ToolError::failed(
                    "plan_quota_exhausted: Always-On discovery permits at most one plan per fire.",
                ));
            }
            let parsed = plan_contract::parse_plan(&content, 100_000, 200).map_err(|error| {
                let mut text = "plan_invalid: ".encode_utf16().collect::<Vec<_>>();
                text.extend_from_slice(error.message().units());
                ToolError::failed(JsString::from_utf16(text))
            })?;
            let parsed_id = parsed
                .metadata
                .get("id")
                .and_then(JsValue::as_string)
                .expect("validated id")
                .clone();
            let parsed_dedupe = parsed
                .metadata
                .get("dedupeKey")
                .and_then(JsValue::as_string)
                .expect("validated dedupe")
                .clone();
            let source = parsed
                .metadata
                .get("sourceRunId")
                .and_then(JsValue::as_string)
                .expect("validated source")
                .clone();
            let title = trimmed(&title);
            let title = if title.is_empty() {
                parsed.title.clone()
            } else {
                title
            };
            let dedupe = trimmed(&dedupe);
            let dedupe = if dedupe.is_empty() {
                parsed_dedupe
            } else {
                dedupe
            };
            let markdown = parsed.raw.clone();
            let markdown_for_job = markdown.clone();
            let run_for_job = run.clone();
            let record = storage::run(move || {
                let path = store.write_plan_markdown(&parsed_id, &markdown_for_job)?;
                let path = path.to_str().ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "Plan path is not UTF-8")
                })?;
                let mut record = JsObject::new();
                for (key, value) in [
                    ("id", parsed_id),
                    ("title", title),
                    ("createdAt", now),
                    ("summary", trimmed(&summary)),
                    ("rationale", trimmed(&rationale)),
                    ("dedupeKey", dedupe),
                    ("sourceRunId", source),
                    ("planFilePath", JsString::from(path)),
                ] {
                    record.insert(key.into(), JsValue::String(value));
                }
                record.insert("status".into(), "ready".into());
                let stored = store.upsert(&record)?;
                Ok((stored, run_for_job))
            })
            .await
            .map_err(|e| ToolError::failed(e.to_string().as_str()))?;
            let (record, run) = record;
            let path = record
                .raw
                .get("planFilePath")
                .and_then(JsValue::as_string)
                .cloned()
                .ok_or_else(|| ToolError::failed("Stored plan has no path"))?;
            let dedupe = record
                .raw
                .get("dedupeKey")
                .and_then(JsValue::as_string)
                .cloned()
                .ok_or_else(|| ToolError::failed("Stored plan has no dedupe key"))?;
            let id = record.id.clone();
            state.plan = Some((record, markdown));
            let mut data = JsObject::new();
            data.insert("ok".into(), JsValue::Bool(true));
            data.insert("planId".into(), JsValue::String(id.clone()));
            data.insert("planFilePath".into(), JsValue::String(path));
            data.insert("dedupeKey".into(), JsValue::String(dedupe.clone()));
            let mut text = "Plan saved as ".encode_utf16().collect::<Vec<_>>();
            text.extend_from_slice(id.units());
            text.extend(" (".encode_utf16());
            text.extend_from_slice(dedupe.units());
            text.extend(").".encode_utf16());
            let mut block = JsObject::new();
            block.insert("type".into(), "text".into());
            block.insert("text".into(), JsValue::String(JsString::from_utf16(text)));
            let mut json = JsObject::new();
            json.insert("type".into(), "json".into());
            json.insert("value".into(), JsValue::Object(data.clone()));
            let mut meta = JsObject::new();
            meta.insert("runId".into(), JsValue::String(run));
            Ok(ToolOutput {
                content: vec![JsValue::Object(block), JsValue::Object(json)],
                data: Some(JsValue::Object(data)),
                metadata: Some(meta),
                ..Default::default()
            })
        });
        Box::pin(async move {
            work.await
                .map_err(|e| ToolError::failed(e.to_string().as_str()))?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext};
    fn context() -> ToolRunContext {
        ToolRunContext::new(Arc::new(ToolRuntimeContext {
            fields: JsObject::new(),
            files: HostToolCheckpoint::default().tool_context_state(),
            abort_signal: None,
            model: None,
            elicitation: None,
            user_dialog: None,
            audit_recorder: None,
            file_history: None,
            file_update_notifier: None,
            now: None,
            progress: None,
            subagent: None,
            plan_todo: None,
            goal: None,
            plan_directory: None,
        }))
    }
    #[tokio::test]
    async fn actual_plan_store_persists_one_valid_plan_and_enforces_quota() {
        let root =
            std::env::temp_dir().join(format!("pdrs-always-on-plan-{}", uuid::Uuid::new_v4()));
        let metadata = crate::metadata::Metadata {
            home: root.clone(),
            cwd: Default::default(),
            files: crate::metadata::NativeFiles,
        };
        let paths =
            super::super::paths::resolve_always_on_paths(&root, "/fixture/project", &metadata)
                .unwrap();
        let project_dir = paths.project_dir();
        let state = Arc::new(tokio::sync::Mutex::new(PlanState::default()));
        let handler = PlanHandler {
            run_id: "run".into(),
            store: DiscoveryPlanStore::new(paths),
            state: state.clone(),
            pending: Arc::default(),
        };
        let fixture = pilotdeck_protocol::parse_json(include_str!(
            "../../../tests/fixtures/always-on-cli/plan-oracle.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = fixture.get("records").unwrap() else {
            panic!("cases")
        };
        let markdown = cases[0].get("input").unwrap().clone();
        let mut input = JsObject::new();
        for (key, value) in [
            ("title", "Named"),
            ("summary", "summary"),
            ("rationale", "why"),
            ("dedupeKey", "key"),
        ] {
            input.insert(key.into(), value.into());
        }
        input.insert("content".into(), markdown);
        let output = handler
            .execute(
                "always_on_discovery_plan",
                JsValue::Object(input.clone()),
                context(),
            )
            .await
            .unwrap();
        assert_eq!(
            output.data.as_ref().and_then(|d| d.get("ok")),
            Some(&JsValue::Bool(true))
        );
        let error = handler
            .execute(
                "always_on_discovery_plan",
                JsValue::Object(input),
                context(),
            )
            .await
            .unwrap_err();
        assert!(
            error
                .message
                .to_utf8()
                .unwrap()
                .contains("plan_quota_exhausted")
        );
        let state = state.lock().await;
        assert_eq!(state.calls, 2);
        let (record, markdown) = state.plan.as_ref().expect("saved plan");
        assert!(record.id.equals_str("p1"));
        assert!(markdown.to_utf8().unwrap().contains("## Verification"));
        let path = record
            .raw
            .get("planFilePath")
            .and_then(JsValue::as_string)
            .unwrap()
            .to_utf8()
            .unwrap();
        assert!(!std::path::Path::new(&path).is_absolute());
        assert!(
            std::fs::read_to_string(project_dir.join(path))
                .unwrap()
                .contains("## Verification")
        );
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
        let pending = Arc::new(PlanPending::default());
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
                .await
                .expect("drain must wake all waiters")
                .expect("waiter panicked");
        }
    }
}

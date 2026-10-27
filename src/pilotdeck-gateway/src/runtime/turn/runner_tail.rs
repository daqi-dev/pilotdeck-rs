//! Session metadata and host tail ports. Native model policy is deliberately a
//! separate dependency: absent generators do not advertise title/suggestion support.
use super::{TurnRunner, object};
use crate::{
    host::Cancellation,
    storage::recorder::{Recorder, RecorderResult},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use pilotdeck_transport::{server::ExecutionClock, system_clock::SystemExecutionClock};
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[path = "runner_artifacts.rs"]
mod artifacts;
pub type GenerateFuture = Pin<Box<dyn Future<Output = Result<Option<JsString>, JsString>> + Send>>;
#[derive(Clone)]
pub struct TitleInput {
    pub text: JsString,
    pub session_id: JsString,
    pub turn_id: JsString,
    pub message_sequences: Vec<u64>,
    pub signal: Cancellation,
    pub model: Option<JsValue>,
}
#[derive(Clone)]
pub struct SuggestionInput {
    pub user_prompt: JsString,
    pub assistant_response: JsString,
    pub session_id: JsString,
    pub turn_id: JsString,
    pub signal: Cancellation,
}
#[derive(Clone)]
pub struct TitleProvider {
    pub generate: Arc<dyn Fn(TitleInput) -> GenerateFuture + Send + Sync>,
    pub provider_id: Option<JsString>,
    pub model_provenance: Option<JsValue>,
}
pub type SuggestionGenerator = Arc<dyn Fn(SuggestionInput) -> GenerateFuture + Send + Sync>;
#[derive(Clone)]
pub struct TailConfig {
    pub cwd: Option<PathBuf>,
    pub collect_file_artifacts: bool,
    pub auto_generate_title: bool,
    pub title_provider: Option<TitleProvider>,
    pub suggestion_generator: Option<SuggestionGenerator>,
    /// Host diagnostic sink, not an AgentEvent or transcript entry.
    pub debug: Arc<dyn Fn(JsString) + Send + Sync>,
}
impl Default for TailConfig {
    fn default() -> Self {
        Self {
            cwd: None,
            collect_file_artifacts: true,
            auto_generate_title: false,
            title_provider: None,
            suggestion_generator: None,
            debug: Arc::new(|message| eprintln!("{}", String::from_utf16_lossy(message.units()))),
        }
    }
}
type WriteOwner = (
    Arc<crate::runtime::project::RuntimeLease>,
    Option<Arc<dyn Send + Sync>>,
);
struct PendingTitle {
    source: Cancellation,
    signal: Cancellation,
    unlink: Cancellation,
    done: Cancellation,
    linked: AtomicBool,
}
impl PendingTitle {
    fn detach(&self) {
        // A cancellation observed before finalization remains sticky.
        if self.linked.swap(false, Ordering::SeqCst) && self.source.is_cancelled() {
            self.signal.cancel(None);
        }
        self.unlink.cancel(None);
    }
}
#[derive(Default)]
struct State {
    metadata: JsObject,
    pending: Option<Arc<PendingTitle>>,
}
/// Shared across consecutive turns. Background titles retain neither the
/// TurnRunner nor the host active-slot/lease ownership guard.
pub struct SessionTail {
    recorder: Arc<Recorder>,
    session: JsString,
    config: TailConfig,
    now: Arc<dyn Fn() -> JsString + Send + Sync>,
    state: Mutex<State>,
    writes: Arc<tokio::sync::Mutex<()>>,
    disposed: Cancellation,
}
impl SessionTail {
    pub fn new(recorder: Arc<Recorder>, session: JsString, config: TailConfig) -> Arc<Self> {
        Arc::new(Self {
            recorder,
            session,
            config,
            now: Arc::new(|| SystemExecutionClock.now_iso()),
            state: Mutex::default(),
            writes: Arc::new(tokio::sync::Mutex::new(())),
            disposed: Cancellation::default(),
        })
    }
    pub fn snapshot(&self) -> JsValue {
        JsValue::Object(
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .metadata
                .clone(),
        )
    }
    pub fn restore(&self, metadata: JsObject) {
        merge(
            &mut self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .metadata,
            &metadata,
        );
    }
    pub fn dispose(&self) {
        self.disposed.cancel(Some("session_closed".into()));
        if let Some(pending) = &self.state.lock().unwrap_or_else(|e| e.into_inner()).pending {
            pending.signal.cancel(Some("session_closed".into()));
            pending.detach();
        }
    }
    /// Invalidate immediately, then close without blocking the async executor
    /// behind a synchronous transcript append already in flight.
    pub async fn close(&self) -> RecorderResult<()> {
        self.dispose();
        let writer = self.recorder.writer().clone();
        tokio::task::spawn_blocking(move || writer.close())
            .await
            .map_err(|e| super::RecorderError::State(e.to_string()))
    }
    async fn record(
        self: &Arc<Self>,
        turn: JsString,
        metadata: JsObject,
        snapshot: bool,
        owner: Option<WriteOwner>,
    ) -> RecorderResult<()> {
        let serial = self.writes.clone().lock_owned().await;
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let _owner = owner;
            let _serial = serial;
            if this.disposed.is_cancelled() {
                return Ok(());
            }
            this.recorder.writer().record_session_metadata(
                &this.session,
                &turn,
                JsValue::Object(metadata.clone()),
            )?;
            if !snapshot {
                merge(
                    &mut this
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .metadata,
                    &metadata,
                );
            }
            Ok(())
        })
        .await
        .map_err(|e| super::RecorderError::State(e.to_string()))?
    }
    fn begin_title(
        self: &Arc<Self>,
        turn: JsString,
        text: JsString,
        sequence: u64,
        cancel: Cancellation,
        options: &JsValue,
    ) {
        if !self.config.auto_generate_title || text.is_empty() {
            return;
        }
        let Some(provider) = self.config.title_provider.clone() else {
            return;
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if self.disposed.is_cancelled()
            || titled(&state.metadata)
            || state
                .pending
                .as_ref()
                .is_some_and(|p| !p.done.is_cancelled())
        {
            return;
        }
        let pending = Arc::new(PendingTitle {
            source: cancel.clone(),
            signal: Cancellation::default(),
            unlink: Cancellation::default(),
            done: Cancellation::default(),
            linked: AtomicBool::new(true),
        });
        if cancel.is_cancelled() {
            pending.signal.cancel(None);
        }
        state.pending = Some(pending.clone());
        drop(state);
        let selected = options
            .get("modelOverride")
            .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
            .or_else(|| {
                options
                    .get("modelSelection")
                    .filter(|v| v.get("mode") == Some(&"model".into()))
            });
        let model = selected.map(|m| {
            object([
                (
                    "provider",
                    m.get("provider").cloned().unwrap_or(JsValue::Undefined),
                ),
                (
                    "model",
                    m.get("model").cloned().unwrap_or(JsValue::Undefined),
                ),
            ])
        });
        let input = TitleInput {
            text,
            session_id: self.session.clone(),
            turn_id: turn.clone(),
            message_sequences: vec![sequence],
            signal: pending.signal.clone(),
            model,
        };
        let weak = Arc::downgrade(self);
        let disposed = self.disposed.clone();
        let future = (provider.generate)(input);
        tokio::spawn(async move {
            tokio::pin!(future);
            let title = loop {
                tokio::select! {
                    biased;
                    _=disposed.cancelled()=>break None,
                    _=cancel.cancelled(), if pending.linked.load(Ordering::SeqCst)=>{if pending.linked.swap(false,Ordering::SeqCst) { pending.signal.cancel(None); }},
                    _=pending.unlink.cancelled(), if pending.linked.load(Ordering::SeqCst)=>{pending.linked.store(false,Ordering::SeqCst);},
                    result=&mut future=>break result.ok().flatten(),
                }
            };
            if !disposed.is_cancelled()
                && !pending.signal.is_cancelled()
                && let Some(title) = title.filter(|s| !s.is_empty())
                && let Some(this) = weak.upgrade()
                && !titled(this.snapshot().as_object().expect("metadata object"))
            {
                let mut patch = object([
                    ("aiTitle", JsValue::String(title)),
                    (
                        "titleSource",
                        if provider.provider_id.as_ref().is_some_and(|s| !s.is_empty()) {
                            "provider"
                        } else {
                            "fallback"
                        }
                        .into(),
                    ),
                    ("titleSourceTurnId", JsValue::String(turn.clone())),
                ]);
                if let JsValue::Object(fields) = &mut patch {
                    if let Some(id) = provider.provider_id {
                        fields.insert("titleProviderId".into(), JsValue::String(id));
                    }
                    if let Some(model) = provider.model_provenance {
                        fields.insert("titleModel".into(), model);
                    }
                    fields.insert(
                        "titleMessageSequences".into(),
                        JsValue::Array(vec![JsValue::Number(sequence as f64)]),
                    );
                    fields.insert("updatedAt".into(), JsValue::String((this.now)()));
                }
                let _ = this
                    .record(
                        turn,
                        patch.as_object().expect("title patch").clone(),
                        false,
                        None,
                    )
                    .await;
            }
            pending.detach();
            pending.done.cancel(None);
        });
    }
    async fn finalize(self: &Arc<Self>, turn: &JsString, owner: WriteOwner) {
        if let Some(pending) = &self.state.lock().unwrap_or_else(|e| e.into_inner()).pending {
            pending.detach();
        }
        let mut snapshot = self
            .snapshot()
            .as_object()
            .expect("metadata snapshot")
            .clone();
        if !snapshot
            .entries()
            .iter()
            .any(|(_, v)| !matches!(v, JsValue::Undefined))
        {
            return;
        }
        snapshot.insert("isSnapshot".into(), JsValue::Bool(true));
        snapshot.insert("updatedAt".into(), JsValue::String((self.now)()));
        let _ = self.record(turn.clone(), snapshot, true, Some(owner)).await;
    }
}
impl Drop for SessionTail {
    fn drop(&mut self) {
        self.dispose();
    }
}
fn merge(target: &mut JsObject, patch: &JsObject) {
    for (key, value) in patch.entries() {
        if ["title", "linkedPullRequest"]
            .iter()
            .any(|s| key.equals_str(s))
            && matches!(value, JsValue::Null | JsValue::Undefined)
        {
            continue;
        }
        target.insert(key.clone(), value.clone());
    }
}
fn titled(snapshot: &JsObject) -> bool {
    ["title", "aiTitle"]
        .iter()
        .any(|k| snapshot.get(k).is_some_and(super::super::submit::truthy))
}
fn trim(text: JsString) -> JsString {
    let space = |u: &u16| char::from_u32(*u as u32).is_some_and(crate::metadata::js_space);
    let units = text.units();
    let start = units.iter().position(|u| !space(u)).unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|u| !space(u))
        .map_or(start, |i| i + 1);
    JsString::from_utf16(units[start..end].to_vec())
}
fn content_text(content: &JsValue) -> JsString {
    let Some(JsValue::Array(blocks)) = content.get("content") else {
        return JsString::default();
    };
    let mut units = vec![];
    let mut first = true;
    for block in blocks {
        if block.get("type") != Some(&"text".into()) {
            continue;
        }
        if !first {
            units.push(10);
        }
        first = false;
        if let Some(value) = block
            .get("text")
            .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
            && let Ok(text) = crate::js_value::to_string_units_checked(value)
        {
            units.extend(text);
        }
    }
    JsString::from_utf16(units)
}
fn human_text(messages: &[JsValue]) -> JsString {
    let mut units = vec![];
    for message in messages {
        if message.get("role") != Some(&"user".into())
            || message
                .get("metadata")
                .and_then(|m| m.get("synthetic"))
                .is_some_and(super::super::submit::truthy)
        {
            continue;
        }
        let text = trim(content_text(message));
        if !text.is_empty() {
            if !units.is_empty() {
                units.push(10);
            }
            units.extend_from_slice(text.units());
        }
    }
    JsString::from_utf16(units)
}
pub(super) struct TurnTail {
    session: Arc<SessionTail>,
    cancel: Cancellation,
    options: JsValue,
    sequence: u64,
    human: Vec<JsValue>,
    prompt: JsString,
    collector: Option<artifacts::Collector>,
}
impl TurnRunner {
    pub fn with_tail(
        mut self,
        session: Arc<SessionTail>,
        cancel: Cancellation,
        options: JsValue,
    ) -> Self {
        self.tail = Some(TurnTail {
            session,
            cancel,
            options,
            sequence: 0,
            human: vec![],
            prompt: JsString::default(),
            collector: None,
        });
        self
    }
    pub(super) async fn listing(
        &mut self,
        history: &[JsValue],
        accepted: &[JsValue],
        metadata: Option<&JsObject>,
    ) {
        let Some(tail) = &mut self.tail else {
            return;
        };
        tail.sequence = self.recorder.snapshot_state().0;
        tail.human = history.iter().chain(accepted).cloned().collect();
        let prompt = human_text(accepted);
        let model = metadata.and_then(|m| m.get("modelSelection"));
        if prompt.is_empty() && model.is_none() {
            return;
        }
        let mut patch = JsObject::new();
        if !prompt.is_empty() {
            let bounded = JsValue::String(JsString::from_utf16(
                prompt.units()[..prompt.len_utf16().min(1200)].to_vec(),
            ));
            if !tail
                .session
                .snapshot()
                .get("firstPrompt")
                .is_some_and(super::super::submit::truthy)
            {
                patch.insert("firstPrompt".into(), bounded.clone());
            }
            patch.insert("lastPrompt".into(), bounded);
        }
        if let Some(model) = model {
            patch.insert("modelSelection".into(), model.clone());
        }
        patch.insert("updatedAt".into(), JsValue::String((tail.session.now)()));
        let _ = tail
            .session
            .record(
                self.turn.clone(),
                patch,
                false,
                Some((self.lease.clone(), self.owner.clone())),
            )
            .await;
    }
    pub(super) async fn start_collector(&mut self) {
        let Some(tail) = &mut self.tail else {
            return;
        };
        if !tail.session.config.collect_file_artifacts {
            return;
        }
        let Some(cwd) = tail.session.config.cwd.clone() else {
            return;
        };
        let allowed = match tail.options.get("allowedReadFiles") {
            Some(JsValue::Array(paths)) => paths
                .iter()
                .filter_map(|v| v.as_string()?.to_utf8().ok().map(PathBuf::from))
                .collect(),
            _ => vec![],
        };
        let owner = (self.lease.clone(), self.owner.clone());
        tail.collector = tokio::task::spawn_blocking(move || {
            let _owner = owner;
            artifacts::Collector::start(cwd, &allowed)
        })
        .await
        .ok();
    }
    pub(super) fn set_tail_prompt(&mut self, prompt: JsString) {
        if let Some(tail) = &mut self.tail {
            tail.prompt = prompt;
        }
    }
    /// Call after delivering user_prompt_submitted and before executing the loop.
    pub fn begin_tail_work(&mut self) {
        if let Some(tail) = &mut self.tail
            && !self.prompt_blocked
        {
            tail.session.begin_title(
                self.turn.clone(),
                human_text(&tail.human),
                tail.sequence,
                tail.cancel.clone(),
                &tail.options,
            );
        }
    }
    pub fn observe_tool_result(&mut self, result: &JsValue) {
        if let Some(collector) = self.tail.as_mut().and_then(|tail| tail.collector.as_mut()) {
            collector.observe(result);
        }
    }
    pub(super) async fn finish_artifacts(&mut self, result: &JsValue) {
        let Some(tail) = &mut self.tail else {
            return;
        };
        let Some(collector) = tail.collector.take() else {
            return;
        };
        let status = if result.get("type") == Some(&"success".into()) {
            "complete"
        } else {
            "incomplete"
        };
        let now = tail.session.now.clone();
        let owner = (self.lease.clone(), self.owner.clone());
        let artifacts = tokio::task::spawn_blocking(move || {
            let _owner = owner;
            collector.finish(status, now.as_ref())
        })
        .await
        .unwrap_or_default();
        if artifacts.is_empty() {
            return;
        }
        let written = artifacts.clone();
        let _ = self
            .io(move |r, s, t| {
                let draft = object([
                    ("type", "file_artifacts".into()),
                    ("artifacts", JsValue::Array(written)),
                ]);
                Ok(r.writer().record_session_event(
                    s,
                    t,
                    draft.as_object().expect("artifact draft").clone(),
                )?)
            })
            .await;
        self.emit_tail(self.event("file_artifacts", [("artifacts", JsValue::Array(artifacts))]))
            .await;
    }
    pub(super) async fn suggestion(&mut self, result: &JsValue) {
        let Some(tail) = &self.tail else {
            return;
        };
        let Some(generate) = tail.session.config.suggestion_generator.clone() else {
            return;
        };
        if result.get("type") != Some(&"success".into()) || tail.cancel.is_cancelled() {
            return;
        }
        let response = trim(
            result
                .get("finalMessage")
                .map(content_text)
                .unwrap_or_default(),
        );
        if response.is_empty() {
            return;
        }
        let input = SuggestionInput {
            user_prompt: tail.prompt.clone(),
            assistant_response: response,
            session_id: self.session.clone(),
            turn_id: self.turn.clone(),
            signal: tail.cancel.clone(),
        };
        match generate(input).await {
            Ok(Some(suggestion)) if !suggestion.is_empty() => {
                self.emit_tail(self.event(
                    "prompt_suggestion",
                    [("suggestion", JsValue::String(suggestion))],
                ))
                .await
            }
            Err(message) => {
                let message = JsString::from_utf16(
                    "[prompt-suggestion] generation skipped (provider_error): "
                        .encode_utf16()
                        .chain(message.units().iter().take(200).copied())
                        .collect(),
                );
                (tail.session.config.debug)(message);
            }
            _ => {}
        }
    }
    pub(super) async fn finalize_metadata(&self) {
        if let Some(tail) = &self.tail {
            tail.session
                .finalize(&self.turn, (self.lease.clone(), self.owner.clone()))
                .await;
        }
    }
}
#[cfg(test)]
#[path = "runner_tail_tests.rs"]
mod tests;

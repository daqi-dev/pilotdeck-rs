//! Host-side preparation and cancellation-safe ownership of recorder transitions.
//! This port does not execute the AgentLoop or manufacture its lifecycle events.
use super::{AgentInput, RecorderError, RecorderResult, Transition, TurnInput, TurnRunner};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{future::Future, pin::Pin, sync::Arc};

impl TurnInput {
    pub fn accepted_messages(&self) -> Vec<JsValue> {
        let content = match &self.input {
            AgentInput::Text(text) => vec![super::object([
                ("type", "text".into()),
                ("text", JsValue::String(text.clone())),
            ])],
            AgentInput::Blocks(content) => content.clone(),
        };
        vec![super::object([
            ("role", "user".into()),
            ("content", JsValue::Array(content)),
        ])]
    }

    pub fn loop_messages(&self) -> Vec<JsValue> {
        self.history
            .iter()
            .cloned()
            .chain(self.accepted_messages())
            .chain(self.synthetic.iter().cloned())
            .collect()
    }

    /// Parse the original AgentInput, never a flattened loop message list.
    /// History and synthetic messages are host-owned, separate input domains.
    pub fn from_agent_input(
        raw: &JsValue,
        options: &JsValue,
        history: Vec<JsValue>,
        synthetic: Vec<JsValue>,
    ) -> RecorderResult<Self> {
        let invalid = || RecorderError::State("Expected text or blocks AgentInput.".into());
        let input = match raw.get("type").and_then(JsValue::as_string) {
            Some(kind) if kind.equals_str("text") => AgentInput::Text(
                raw.get("text")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .ok_or_else(invalid)?,
            ),
            Some(kind) if kind.equals_str("blocks") => match raw.get("content") {
                Some(JsValue::Array(blocks)) => AgentInput::Blocks(blocks.clone()),
                _ => return Err(invalid()),
            },
            _ => return Err(invalid()),
        };
        let mut metadata = JsObject::new();
        // TurnRunner.ts acceptedInputMetadata: allowPlanModeTools preserves
        // false/null; the other fields use JS truthiness, not presence.
        for key in [
            "modelSelection",
            "permissionMode",
            "runMode",
            "basePermissionMode",
        ] {
            if let Some(value) = options.get(key).filter(|v| super::super::submit::truthy(v)) {
                metadata.insert(
                    key.into(),
                    if key == "modelSelection" {
                        JsValue::Object(own_properties(value))
                    } else {
                        value.clone()
                    },
                );
            }
        }
        if let Some(value) = options
            .get("allowPlanModeTools")
            .filter(|v| !matches!(v, JsValue::Undefined))
        {
            metadata.insert("allowPlanModeTools".into(), value.clone());
        }
        Ok(Self {
            input,
            history,
            synthetic,
            metadata: (!metadata.is_empty()).then_some(metadata),
        })
    }
}

// JS object spread copies own properties, including UTF-16 string indices.
fn own_properties(value: &JsValue) -> JsObject {
    let mut out = JsObject::new();
    match value {
        JsValue::Object(object) => {
            for (key, value) in object.entries() {
                out.insert(key.clone(), value.clone());
            }
        }
        JsValue::Array(array) => {
            for (index, value) in array.iter().enumerate() {
                out.insert(index.to_string().into(), value.clone());
            }
        }
        JsValue::String(string) => {
            for (index, unit) in string.units().iter().enumerate() {
                out.insert(
                    index.to_string().into(),
                    JsValue::String(JsString::from_utf16(vec![*unit])),
                );
            }
        }
        _ => {}
    }
    out
}

pub enum RecordingAction {
    Start,
    Accept(TurnInput),
    SubmitPrompt(super::AgentInput, crate::host::Cancellation),
    LoopFailed(JsValue),
    Status(JsValue),
    Durable(JsValue),
    Complete(JsValue),
    Fail(JsValue),
}

/// A LiveTurn can retain this driver across calls to next(). Cancelling settle
/// only drops its borrow: the transition future and its runner remain owned.
/// Dropping the driver cancels pending async work; an already-running blocking
/// write retains both the runtime lease and the supplied host ownership guard.
/// No timeout releases that guard while synchronous I/O is still running.
pub struct Recording {
    runner: Option<TurnRunner>,
    pending: Option<Pin<Box<dyn Future<Output = Transition> + Send>>>,
    progress:
        Option<tokio::sync::mpsc::UnboundedReceiver<(JsValue, tokio::sync::oneshot::Sender<()>)>>,
    resume: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Recording {
    /// `owner` must be the actual shared active-slot guard when used by a host,
    /// not an unrelated liveness marker. The caller may retain its own clone.
    pub fn new(mut runner: TurnRunner, owner: Arc<dyn Send + Sync>) -> Self {
        runner.owner = Some(owner);
        Self {
            runner: Some(runner),
            pending: None,
            progress: None,
            resume: None,
        }
    }
    pub fn runner(&self) -> Option<&TurnRunner> {
        self.runner.as_ref()
    }
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn runner_mut(&mut self) -> Option<&mut TurnRunner> {
        self.runner.as_mut()
    }
    pub(crate) fn callback_error(&self) -> Option<String> {
        self.runner.as_ref()?.callback_error.clone()
    }
    pub(crate) fn take_callback_error(&mut self) -> Option<String> {
        self.runner.as_mut()?.callback_error.take()
    }
    pub fn begin(&mut self, action: RecordingAction) -> RecorderResult<()> {
        if self.pending.is_some() {
            return Err(RecorderError::State(
                "Recording transition is pending.".into(),
            ));
        }
        if self.runner.as_ref().is_none_or(TurnRunner::is_complete) {
            return Err(RecorderError::State("Recording is closed.".into()));
        }
        let mut runner = self.runner.take().expect("checked above");
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        runner.progress = Some(sender);
        self.progress = Some(receiver);
        self.pending = Some(Box::pin(async move {
            match action {
                RecordingAction::Start => runner.start().await,
                RecordingAction::Accept(input) => runner.accept(input).await,
                RecordingAction::SubmitPrompt(input, cancel) => {
                    runner.submit_prompt(input, cancel).await
                }
                RecordingAction::LoopFailed(error) => runner.loop_failed(error).await,
                RecordingAction::Status(event) => runner.loop_status(event).await,
                RecordingAction::Durable(message) => runner.durable_message(message).await,
                RecordingAction::Complete(result) => runner.complete(result).await,
                RecordingAction::Fail(error) => runner.fail(error).await,
            }
        }));
        Ok(())
    }
    /// A failed transition consumes its runner; do not replay a possibly
    /// partially persisted write. Call begin only after a successful settle.
    pub async fn settle(&mut self) -> RecorderResult<Vec<JsValue>> {
        if let Some(resume) = self.resume.take() {
            let _ = resume.send(());
        }
        let pending = self
            .pending
            .as_mut()
            .ok_or_else(|| RecorderError::State("No recording transition is pending.".into()))?;
        let outcome = tokio::select! {
            outcome = pending => outcome,
            Some((event, resume)) = self.progress.as_mut().expect("pending progress").recv() => {
                self.resume = Some(resume);
                return Ok(vec![event]);
            }
        };
        self.pending = None;
        self.progress = None;
        let (mut runner, events) = outcome?;
        runner.progress = None;
        self.runner = Some(runner);
        Ok(events)
    }
}

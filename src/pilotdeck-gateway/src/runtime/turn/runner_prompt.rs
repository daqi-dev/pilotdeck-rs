//! Host-only UserPromptSubmit, after durable acceptance and before execute.
use super::{AgentInput, Phase, RecorderError, Transition, TurnRunner, object};
use crate::{
    host::{Cancellation, modules::lifecycle::LifecyclePort},
    runtime::hooks::{self, HookEffect, LifecycleDispatchInput, LifecycleDispatchResult},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::Arc;

pub(crate) struct PromptHooks {
    pub port: Arc<dyn LifecyclePort>,
    pub cwd: JsString,
    pub transcript_path: JsString,
}
struct AbortOnDrop(Option<hooks::Cancellation>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(signal) = &self.0 {
            signal.abort();
        }
    }
}
impl TurnRunner {
    pub(crate) fn with_prompt_hooks(mut self, hooks: Option<PromptHooks>) -> Self {
        self.prompt_hooks = hooks;
        self
    }
    pub(crate) fn prompt_block_error(&self) -> Option<JsValue> {
        self.prompt_blocked.then(|| {
            object([
                ("code", "agent_unsupported_feature".into()),
                (
                    "message",
                    "UserPromptSubmit hook blocked model execution.".into(),
                ),
            ])
        })
    }
    pub async fn submit_prompt(mut self, input: AgentInput, cancel: Cancellation) -> Transition {
        self.require(Phase::Accepted)?;
        if self.prompt_submitted {
            return Err(RecorderError::State("Prompt already submitted.".into()));
        }
        self.start_collector().await;
        let prompt = match input {
            AgentInput::Text(text) => text,
            AgentInput::Blocks(blocks) => {
                let mut units = Vec::new();
                let mut first = true;
                for block in &blocks {
                    if matches!(block, JsValue::Null | JsValue::Undefined) {
                        let kind = if *block == JsValue::Null {
                            "null"
                        } else {
                            "undefined"
                        };
                        return Err(RecorderError::State(format!(
                            "Cannot read properties of {kind} (reading 'type')"
                        )));
                    }
                    if block.get("type") != Some(&"text".into()) {
                        continue;
                    }
                    if !first {
                        units.push(10);
                    }
                    first = false;
                    if let Some(text) = block
                        .get("text")
                        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                    {
                        units.extend(
                            crate::js_value::to_string_units_checked(text)
                                .map_err(|e| RecorderError::State(e.message().into()))?,
                        );
                    }
                }
                JsString::from_utf16(units)
            }
        };
        if let Some(hooks) = &self.prompt_hooks {
            let signal = hooks::Cancellation::new();
            let mut on_drop = AbortOnDrop(Some(signal.clone()));
            let dispatch = hooks.port.dispatch(LifecycleDispatchInput {
                event: "UserPromptSubmit".into(),
                base_input: object([
                    ("sessionId", JsValue::String(self.session.clone())),
                    (
                        "transcriptPath",
                        JsValue::String(hooks.transcript_path.clone()),
                    ),
                    ("cwd", JsValue::String(hooks.cwd.clone())),
                ])
                .as_object()
                .expect("host prompt base input")
                .clone(),
                payload: Some(
                    object([("prompt", JsValue::String(prompt.clone()))])
                        .as_object()
                        .expect("host prompt payload")
                        .clone(),
                ),
                match_query: Some("UserPromptSubmit".into()),
                cwd: hooks.cwd.clone(),
                env: None,
                signal: Some(signal),
            });
            // A cancelled next() borrow retains this future in Recording. A
            // cancelled turn, however, must not wait on an uncooperative hook.
            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => LifecycleDispatchResult::default(),
                result = dispatch => {
                    on_drop.0 = None;
                    result.map_err(|e| RecorderError::State(e.message))?
                },
            };
            self.prompt_blocked = result
                .effects
                .iter()
                .any(|e| matches!(e, HookEffect::Block { .. }));
            if !self.prompt_blocked
                && let Some(JsValue::Array(messages)) = result.to_value().get("messages")
            {
                self.messages.extend(messages.iter().cloned());
            }
        }
        self.set_tail_prompt(prompt.clone());
        self.prompt_submitted = true;
        let event = self.event(
            super::HOST_LIFECYCLE_EVENTS[2],
            [("prompt", JsValue::String(prompt))],
        );
        Ok((self, vec![event]))
    }
}

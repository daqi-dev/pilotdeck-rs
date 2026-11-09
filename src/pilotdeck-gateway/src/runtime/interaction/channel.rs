//! Host-owned question channel; the wire can supply answers, never authority.
use super::{GatewayInteractionCoordinator, PendingEntry, Registration};
use crate::host::HostError;
use crate::runtime::tools::{ToolError, ToolRunContext};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DEFAULT_QUESTION_TIMEOUT: Duration = Duration::from_secs(120);

pub type QuestionEmitter = Arc<dyn Fn(JsObject) -> Result<(), HostError> + Send + Sync>;

pub struct QuestionChannel {
    interactions: Arc<GatewayInteractionCoordinator>,
    owner: JsString,
    emit: QuestionEmitter,
    timeout: Option<std::time::Duration>,
}

impl QuestionChannel {
    pub fn new(
        interactions: Arc<GatewayInteractionCoordinator>,
        owner: JsString,
        emit: QuestionEmitter,
    ) -> Self {
        let timeout = interactions.elicitation_timeout();
        Self {
            interactions,
            owner,
            emit,
            timeout,
        }
    }

    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    // Retain the shared native ToolError ABI.
    #[allow(clippy::result_large_err)]
    pub async fn ask(
        &self,
        input: &JsValue,
        context: &ToolRunContext,
    ) -> Result<JsValue, ToolError> {
        let request_id: JsString = uuid::Uuid::new_v4().to_string().into();
        let tool_call_id = context
            .fields
            .get("turnId")
            .cloned()
            .unwrap_or(JsValue::Undefined);
        let mut payload = JsObject::new();
        payload.insert("toolCallId".into(), tool_call_id.clone());
        payload.insert("toolName".into(), "ask_user_question".into());
        for key in ["questions", "metadata"] {
            if let Some(value) = input.get(key) {
                payload.insert(key.into(), value.clone());
            }
        }
        let (send, receive) = tokio::sync::oneshot::channel();
        let send = Arc::new(Mutex::new(Some(send)));
        let resolve = send.clone();
        let entry = PendingEntry {
            request_id: request_id.clone(),
            tool_call_id: tool_call_id.as_string().cloned().unwrap_or_default(),
            tool_name: "ask_user_question".into(),
            resolve: Arc::new(move |answer| {
                if let Some(send) = resolve.lock().unwrap().take() {
                    let _ = send.send(Ok(answer));
                }
            }),
            reject: Arc::new(move |reason| {
                if let Some(send) = send.lock().unwrap().take() {
                    let _ = send.send(Err(reason));
                }
            }),
        };
        let registration = self
            .interactions
            .elicitation_bus()
            .register(&self.owner, entry, Some(JsValue::Object(payload.clone())))
            .map_err(ToolError::failed)?;
        // Bus registrations deliberately require disposal; an abandoned tool future must remove its own entry.
        struct Cleanup(Registration);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                self.0.dispose();
            }
        }
        let _cleanup = Cleanup(registration);
        payload.insert("type".into(), "elicitation_request".into());
        payload.insert("requestId".into(), JsValue::String(request_id.clone()));
        (self.emit)(payload).map_err(|e| ToolError::failed(e.message))?;
        let timeout = self.timeout;
        let timeout_answer = async {
            match timeout {
                Some(timeout) => tokio::time::sleep(timeout).await,
                None => std::future::pending().await,
            }
            let _entry =
                match self
                    .interactions
                    .elicitation_bus()
                    .consume(&self.owner, &request_id, None)
                {
                    Some(entry) => entry,
                    None => std::future::pending().await,
                };
            super::object([("type", "cancelled".into()), ("reason", "timeout".into())])
        };
        tokio::select! {
            answer = receive => answer.map_err(|_| ToolError::failed("Elicitation channel closed."))?
                .map_err(ToolError::failed),
            _ = context.cancelled() => {
                let mut event = JsObject::new();
                event.insert("type".into(), "elicitation_cancelled".into());
                event.insert("requestId".into(), JsValue::String(request_id.clone()));
                event.insert("reason".into(), "aborted".into());
                let _ = (self.emit)(event);
                Ok(super::object([("type", "cancelled".into()), ("reason", "aborted".into())]))
            }
            answer = timeout_answer => {
                let mut event = JsObject::new();
                event.insert("type".into(), "elicitation_cancelled".into());
                event.insert("requestId".into(), JsValue::String(request_id.clone()));
                event.insert("reason".into(), "timeout".into());
                let _ = (self.emit)(event);
                Ok(answer)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext};
    use std::future::Future;
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
    async fn question_channel_delivers_once_and_dropped_future_removes_pending() {
        let owner = Arc::new(GatewayInteractionCoordinator::new(Default::default()));
        let frames = Arc::new(Mutex::new(Vec::new()));
        let sink = frames.clone();
        let channel = QuestionChannel::new(
            owner.clone(),
            "session".into(),
            Arc::new(move |event| {
                sink.lock().unwrap().push(event);
                Ok(())
            }),
        );
        let input = super::super::object([("questions", JsValue::Array(vec![]))]);
        let ctx = context();
        let mut pending = Box::pin(channel.ask(&input, &ctx));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(
                pending.as_mut().poll(cx).is_pending()
            ))
            .await
        );
        let request = frames.lock().unwrap()[0]
            .get("requestId")
            .unwrap()
            .as_string()
            .unwrap()
            .clone();
        let answer = super::super::object([
            ("type", "answered".into()),
            ("answers", super::super::object([("Q", "Yes".into())])),
        ]);
        assert!(!owner.respond_elicitation(&"wrong".into(), &request, answer.clone(), None));
        assert!(owner.respond_elicitation(&"session".into(), &request, answer.clone(), None));
        assert_eq!(pending.await.unwrap(), answer);
        assert!(!owner.respond_elicitation(&"session".into(), &request, JsValue::Null, None));
        let mut abandoned = Box::pin(channel.ask(&input, &ctx));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(
                abandoned.as_mut().poll(cx).is_pending()
            ))
            .await
        );
        assert_eq!(owner.elicitation_bus().pending_count(&"session".into()), 1);
        drop(abandoned);
        assert_eq!(owner.elicitation_bus().pending_count(&"session".into()), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn configured_question_timeout_cancels_and_removes_pending() {
        let owner = Arc::new(GatewayInteractionCoordinator::new(
            crate::runtime::interaction::InteractionOptions {
                elicitation_timeout: Some(std::time::Duration::from_millis(1)),
                ..Default::default()
            },
        ));
        let frames = Arc::new(Mutex::new(Vec::new()));
        let sink = frames.clone();
        let channel = QuestionChannel::new(
            owner.clone(),
            "session".into(),
            Arc::new(move |event| {
                sink.lock().unwrap().push(event);
                Ok(())
            }),
        );
        let input = super::super::object([("questions", JsValue::Array(vec![]))]);
        let context = context();
        let result = channel.ask(&input, &context).await.unwrap();
        assert_eq!(result.get("type"), Some(&"cancelled".into()));
        assert_eq!(result.get("reason"), Some(&"timeout".into()));
        assert_eq!(owner.elicitation_bus().pending_count(&"session".into()), 0);
        assert_eq!(
            frames.lock().unwrap().last().unwrap().get("reason"),
            Some(&"timeout".into())
        );
    }
}

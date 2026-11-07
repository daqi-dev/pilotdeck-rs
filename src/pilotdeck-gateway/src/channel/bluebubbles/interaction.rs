//! The `Gateway` half of the TS helper protocol that #1327's `TurnGateway`
//! port does not carry: `respondElicitation` / `permissionDecide`. The channel
//! speaks the shared [`InteractionPort`] (`im_interaction.rs`, the wecom/feishu
//! lane) and this unit supplies its real implementation, backed by the
//! ported `ImElicitationHelper` / `ImPermissionHelper` state machines plus a
//! small responder port. The golden replay installs a scripted responder; the
//! production assembly mount installs [`crate::channel::bluebubbles::no_interaction`]
//! until the A7 boundary grows the respond calls (registered diff
//! A8.3-CH-BLUEBUBBLES-002).
use std::sync::{Arc, Mutex as StdMutex};

use pilotdeck_protocol::{JsString, JsValue};

use crate::channel::im_interaction::{
    InteractionFuture, InteractionPort, PermissionAnswer as PortAnswer,
};
use crate::channel::protocol::{BoxFuture, ChannelError};
use crate::channel::sms::{
    self, AnswerGuard, BeginAnswer, DecisionReply, ElicitationResponse, ImElicitationHelper,
    ImPermissionHelper, PermissionAnswer, PermissionDecision, PermissionDecisionOutcome,
    PermissionState,
};

/// `gateway.respondElicitation(response)` / `gateway.permissionDecide(decision)`
/// — everything the TS helpers need from the gateway besides `submitTurn`.
pub trait InteractionResponder: Send + Sync {
    fn respond_elicitation(
        &self,
        response: ElicitationResponse,
    ) -> BoxFuture<Result<(), ChannelError>>;
    fn permission_decide(
        &self,
        decision: PermissionDecision,
    ) -> BoxFuture<Result<PermissionDecisionOutcome, ChannelError>>;
}

pub type ResponderRef = Arc<dyn InteractionResponder>;

/// The `InteractionPort` the channel drives, with the real helper state
/// machines (`ImElicitationHelper.ts` / `ImPermissionHelper.ts`) behind it.
pub struct HelperInteraction {
    responder: ResponderRef,
    elicitation: StdMutex<ImElicitationHelper>,
    permissions: PermissionState,
}

impl HelperInteraction {
    pub fn new(responder: ResponderRef) -> Self {
        Self {
            responder,
            elicitation: StdMutex::new(ImElicitationHelper::default()),
            permissions: Arc::new(StdMutex::new(ImPermissionHelper::default())),
        }
    }

    /// The port's `&str` boundary meets the helpers' `JsString` keys here
    /// (feishu/wecom accept the same scalar-string boundary at their ports).
    fn js_id(chat_id: &str) -> JsString {
        JsString::from(chat_id)
    }
}

/// `ImPermissionAnswerResult` → the port's scalar-text answer shape.
fn port_answer(answer: PermissionAnswer) -> PortAnswer {
    PortAnswer {
        text: answer
            .text
            .map(|text| String::from_utf16_lossy(text.units())),
        can_advance: answer.can_advance,
        retry_prompt: answer.retry_prompt,
        answer_token: answer.answer_token.map(u64::from),
    }
}

impl InteractionPort for HelperInteraction {
    fn elicitation_pending(&self, chat_id: &str) -> bool {
        let id = Self::js_id(chat_id);
        self.elicitation
            .lock()
            .expect("#1343 elicitation mutex")
            .has_pending(&id)
    }

    /// `capture`'s question text (`String` through the port boundary).
    fn elicitation_capture(&self, chat_id: &str, session_key: &str, event: &JsValue) -> String {
        let id = Self::js_id(chat_id);
        let session_key = JsString::from(session_key);
        let text = self
            .elicitation
            .lock()
            .expect("#1343 elicitation mutex")
            .capture(&id, &session_key, event);
        String::from_utf16_lossy(text.units())
    }

    /// `ImElicitationHelper.answer` including the `respondElicitation` await:
    /// the channel sees the confirmation text or the error, never the helper
    /// internals. `take` runs in the same lock section as the pending check
    /// (#1339 fix-r1's fused shape — TS runs the stretch synchronously).
    fn elicitation_answer<'a>(
        &'a self,
        chat_id: &'a str,
        text: &'a str,
    ) -> InteractionFuture<'a, Result<Option<String>, ChannelError>> {
        Box::pin(async move {
            let id = Self::js_id(chat_id);
            let entry = {
                let mut elicitation = self.elicitation.lock().expect("#1343 elicitation mutex");
                elicitation.take(&id)
            };
            let Some(entry) = entry else {
                return Ok(None);
            };
            let trimmed =
                JsString::from_utf16(sms::trim_units(JsString::from(text).units()).to_vec());
            if trimmed.equals_str("0") {
                let response = sms::elicitation::cancelled_response(&entry);
                self.responder.respond_elicitation(response).await?;
                Ok(Some("已取消。".to_owned()))
            } else {
                let response = sms::elicitation::answered_response(&entry, &trimmed);
                self.responder.respond_elicitation(response).await?;
                Ok(None)
            }
        })
    }

    fn elicitation_clear(&self, chat_id: &str) {
        let id = Self::js_id(chat_id);
        self.elicitation
            .lock()
            .expect("#1343 elicitation mutex")
            .clear(&id);
    }

    fn permission_pending(&self, chat_id: &str) -> bool {
        let id = Self::js_id(chat_id);
        self.permissions
            .lock()
            .expect("#1343 permissions mutex")
            .has_pending(&id)
    }

    fn permission_capture(
        &self,
        chat_id: &str,
        session_key: &str,
        event: &JsValue,
    ) -> Option<String> {
        let id = Self::js_id(chat_id);
        let session_key = JsString::from(session_key);
        self.permissions
            .lock()
            .expect("#1343 permissions mutex")
            .capture(&id, &session_key, event)
            .map(|text| String::from_utf16_lossy(text.units()))
    }

    fn permission_confirm_initial(&self, chat_id: &str, delivered: bool, request_id: Option<&str>) {
        let id = Self::js_id(chat_id);
        let expected = request_id.map(JsString::from);
        self.permissions
            .lock()
            .expect("#1343 permissions mutex")
            .confirm_initial_prompt(&id, delivered, expected.as_ref());
    }

    /// `ImPermissionHelper.answerWithState`: the state machine's synchronous
    /// prefix (`begin_answer`), the single `permissionDecide` await, then
    /// `finish_answer` / `fail_answer`. The `AnswerGuard` owns the in-flight
    /// release across the await (#1790), exactly like the sms lane.
    fn permission_answer<'a>(
        &'a self,
        chat_id: &'a str,
        text: &'a str,
    ) -> InteractionFuture<'a, Result<Option<PortAnswer>, ChannelError>> {
        Box::pin(async move {
            let id = Self::js_id(chat_id);
            let text = JsString::from(text);
            let began = {
                let mut permissions = self.permissions.lock().expect("#1343 permissions mutex");
                permissions.begin_answer(&id, &text)
            };
            match began {
                BeginAnswer::None => Ok(None),
                BeginAnswer::Status(answer) => Ok(Some(port_answer(answer))),
                BeginAnswer::Decide {
                    entry,
                    deny,
                    remember,
                    answer_token,
                    generation,
                    shifted_rest,
                } => {
                    let mut guard = AnswerGuard::new(
                        self.permissions.clone(),
                        id.clone(),
                        generation,
                        entry.clone(),
                        shifted_rest.clone(),
                    );
                    let decision = PermissionDecision {
                        session_key: entry.session_key.clone(),
                        request_id: entry.request_id.clone(),
                        deny,
                        remember,
                    };
                    let reply = if deny {
                        DecisionReply::Deny
                    } else if remember {
                        DecisionReply::AllowSession
                    } else {
                        DecisionReply::AllowOnce
                    };
                    match self.responder.permission_decide(decision).await {
                        Err(error) => {
                            self.permissions
                                .lock()
                                .expect("#1343 permissions mutex")
                                .fail_answer(&id, generation, entry, shifted_rest);
                            guard.settle();
                            Err(error)
                        }
                        Ok(outcome) => {
                            let answer = self
                                .permissions
                                .lock()
                                .expect("#1343 permissions mutex")
                                .finish_answer(
                                    &id,
                                    answer_token,
                                    generation,
                                    reply,
                                    shifted_rest,
                                    outcome,
                                );
                            guard.settle();
                            Ok(answer.map(port_answer))
                        }
                    }
                }
            }
        })
    }

    /// The TS channel's catch-side `releaseAnswer(chatGuid, answerToken)` only
    /// ever ran with an assigned token when the reply POST failed to deliver;
    /// the decide-failure path leaves the token unset (the helper's own catch
    /// restored the state), so the error arm here releases nothing.
    fn permission_release_answer(&self, chat_id: &str, answer_token: Option<u64>) {
        let id = Self::js_id(chat_id);
        self.permissions
            .lock()
            .expect("#1343 permissions mutex")
            .release_answer(&id, answer_token.map(|token| token as u32));
    }

    fn permission_take_next_prompt(
        &self,
        chat_id: &str,
        answer_token: Option<u64>,
    ) -> Option<String> {
        let id = Self::js_id(chat_id);
        self.permissions
            .lock()
            .expect("#1343 permissions mutex")
            .take_next_prompt(&id, answer_token.map(|token| token as u32))
            .map(|text| String::from_utf16_lossy(text.units()))
    }

    fn permission_prompt_request_id(
        &self,
        chat_id: &str,
        answer_token: Option<u64>,
    ) -> Option<String> {
        let id = Self::js_id(chat_id);
        self.permissions
            .lock()
            .expect("#1343 permissions mutex")
            .get_prompt_request_id(&id, answer_token.map(|token| token as u32))
            .map(|text| String::from_utf16_lossy(text.units()))
    }

    fn permission_confirm_next(
        &self,
        chat_id: &str,
        delivered: bool,
        request_id: Option<&str>,
        answer_token: Option<u64>,
    ) {
        let id = Self::js_id(chat_id);
        let expected = request_id.map(JsString::from);
        self.permissions
            .lock()
            .expect("#1343 permissions mutex")
            .confirm_next_prompt(
                &id,
                delivered,
                expected.as_ref(),
                answer_token.map(|token| token as u32),
            );
    }

    fn permission_clear_after_turn(&self, chat_id: &str) {
        let id = Self::js_id(chat_id);
        self.permissions
            .lock()
            .expect("#1343 permissions mutex")
            .clear_after_turn(&id);
    }
}

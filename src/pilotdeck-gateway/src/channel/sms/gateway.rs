//! A7 gateway boundary the sms channel speaks (`SmsChannel.ts` calls
//! `gateway.submitTurn` / `respondElicitation` / `permissionDecide`).
//!
//! #1327 deliberately does not define the A7 inbound port, so this unit pins
//! only the slice the SMS adapter needs. `submit_turn` returns the turn's
//! event stream as one future per event — the shape of the TS async
//! generator, so a stream that pauses after an elicitation/permission event
//! (waiting for the helper's answer) replays exactly: the channel consumes
//! events while the turn is still open.
use std::sync::Arc;

use pilotdeck_protocol::{JsObject, JsString, JsValue};

use crate::channel::protocol::{BoxFuture, ChannelError};

/// `gateway.submitTurn({sessionKey, channelKey, message})`.
#[derive(Clone, Debug)]
pub struct SubmitTurnRequest {
    pub session_key: JsString,
    pub channel_key: JsString,
    pub message: JsString,
}
impl SubmitTurnRequest {
    /// The exact TS argument object, insertion order included.
    pub fn to_js(&self) -> JsObject {
        let mut object = JsObject::new();
        object.insert(
            "sessionKey".into(),
            JsValue::String(self.session_key.clone()),
        );
        object.insert(
            "channelKey".into(),
            JsValue::String(self.channel_key.clone()),
        );
        object.insert("message".into(), JsValue::String(self.message.clone()));
        object
    }
}

/// `gateway.respondElicitation({sessionKey, requestId, answer})`; `answer`
/// keeps the raw `{"type":"cancelled",...}` / `{"type":"answered",...}` shape
/// built by the elicitation helper.
#[derive(Clone, Debug)]
pub struct ElicitationResponse {
    pub session_key: JsString,
    /// #1339 fix-r1: a missing event `requestId` is real `undefined` on the
    /// TS side, so `to_js` records `undefined` — the JSON record then omits
    /// the key exactly like TS `JSON.stringify`.
    pub request_id: Option<JsString>,
    pub answer: JsValue,
}
impl ElicitationResponse {
    pub fn to_js(&self) -> JsObject {
        let mut object = JsObject::new();
        object.insert(
            "sessionKey".into(),
            JsValue::String(self.session_key.clone()),
        );
        object.insert(
            "requestId".into(),
            self.request_id
                .clone()
                .map(JsValue::String)
                .unwrap_or(JsValue::Undefined),
        );
        object.insert("answer".into(), self.answer.clone());
        object
    }
}

/// `gateway.permissionDecide({sessionKey, requestId, decision, ...})` — the
/// spread puts `reason` (deny) or `remember` (allow) after `decision`.
#[derive(Clone, Debug)]
pub struct PermissionDecision {
    pub session_key: JsString,
    /// #1339 fix-r1: same as `ElicitationResponse::request_id` — a missing
    /// event `requestId` stays `undefined`, not the string "undefined".
    pub request_id: Option<JsString>,
    pub deny: bool,
    /// `remember: trimmed === "2"` on the allow branch.
    pub remember: bool,
}
impl PermissionDecision {
    pub fn to_js(&self) -> JsObject {
        let mut object = JsObject::new();
        object.insert(
            "sessionKey".into(),
            JsValue::String(self.session_key.clone()),
        );
        object.insert(
            "requestId".into(),
            self.request_id
                .clone()
                .map(JsValue::String)
                .unwrap_or(JsValue::Undefined),
        );
        object.insert(
            "decision".into(),
            JsValue::String(if self.deny { "deny" } else { "allow" }.into()),
        );
        if self.deny {
            object.insert(
                "reason".into(),
                JsValue::String("User denied permission from IM channel.".into()),
            );
        } else {
            object.insert("remember".into(), JsValue::Bool(self.remember));
        }
        object
    }
}

/// `permissionDecide` resolves `{delivered}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PermissionDecisionOutcome {
    pub delivered: bool,
}

/// The TS async generator `submitTurn` returns, as one future per event:
/// `None` completes the turn, `Err` is the generator throwing mid-stream (the
/// channel's for-await surfaces it at the awaiting boundary).
pub trait TurnEventStream: Send {
    fn next_event(&mut self) -> BoxFuture<Option<Result<JsValue, ChannelError>>>;
}

pub trait SmsGateway: Send + Sync {
    fn submit_turn(
        &self,
        request: SubmitTurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>>;
    fn respond_elicitation(
        &self,
        response: ElicitationResponse,
    ) -> BoxFuture<Result<(), ChannelError>>;
    fn permission_decide(
        &self,
        decision: PermissionDecision,
    ) -> BoxFuture<Result<PermissionDecisionOutcome, ChannelError>>;
}

pub type GatewayRef = Arc<dyn SmsGateway>;

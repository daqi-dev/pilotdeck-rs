//! Twilio REST boundary (`require("twilio")` + `client.messages.create`).
//!
//! The TS module tries to load the `twilio` package at import time and warns
//! on start when it is absent. The Rust product has no Twilio client, so the
//! factory is injected; `None` reproduces the "twilio not installed" branch
//! exactly (that is what the loader-mounted adapter runs with until the sender
//! unit lands). Recorded in the #1339 diffs ledger.
use std::sync::Arc;

use pilotdeck_protocol::{JsObject, JsString, JsValue};

use crate::channel::protocol::{BoxFuture, ChannelError};

/// `client.messages.create({body, from, to})`.
#[derive(Clone, Debug)]
pub struct SmsOutgoingMessage {
    pub body: JsString,
    pub from: JsString,
    pub to: JsString,
}
impl SmsOutgoingMessage {
    pub fn to_js(&self) -> JsObject {
        let mut object = JsObject::new();
        object.insert("body".into(), JsValue::String(self.body.clone()));
        object.insert("from".into(), JsValue::String(self.from.clone()));
        object.insert("to".into(), JsValue::String(self.to.clone()));
        object
    }
}

pub trait SmsSender: Send + Sync {
    fn send(&self, message: SmsOutgoingMessage) -> BoxFuture<Result<(), ChannelError>>;
}

/// `twilio(accountSid, authToken)` — the constructor call in `start()`.
pub type TwilioFactory =
    Arc<dyn Fn(&JsString, &JsString) -> Result<Arc<dyn SmsSender>, ChannelError> + Send + Sync>;

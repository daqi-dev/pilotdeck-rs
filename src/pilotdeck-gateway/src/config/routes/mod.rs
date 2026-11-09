//! Shared channel errors and transports, independent of CONFIG RPC ownership.
pub mod channel_http;
pub mod channel_state;
pub mod im_cron;

use pilotdeck_protocol::{JsObject, JsValue};
use std::{future::Future, pin::Pin};

pub type ChannelCall<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
pub type Result<T = JsValue> = std::result::Result<T, RouteError>;

#[derive(Clone, Debug)]
pub struct RouteError {
    pub code: String,
    pub message: String,
    /// Retain UTF-16, provider details and validation at the ordered boundary.
    pub ordered_error: Option<Box<JsValue>>,
}
impl RouteError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into(), ordered_error: None }
    }
    pub fn from_ordered(error: JsValue) -> Self {
        let text = |key| error.get(key).and_then(JsValue::as_string)
            .map(|s| String::from_utf16_lossy(s.units()));
        Self {
            code: text("code").unwrap_or_else(|| "gateway_request_failed".into()),
            message: text("message").unwrap_or_else(|| "Native channel request failed.".into()),
            ordered_error: Some(Box::new(error)),
        }
    }
    pub fn to_js(&self) -> JsValue {
        if let Some(error) = &self.ordered_error { return (**error).clone(); }
        let mut error = JsObject::new();
        error.insert("code".into(), self.code.as_str().into());
        error.insert("message".into(), self.message.as_str().into());
        JsValue::Object(error)
    }
}
impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for RouteError {}
impl From<crate::runtime::connection::RequestError> for RouteError {
    fn from(error: crate::runtime::connection::RequestError) -> Self {
        let mut value = JsObject::new();
        value.insert("code".into(), error.code.as_str().into());
        value.insert("message".into(), JsValue::String(error.message));
        if let Some(v) = error.validation { value.insert("validation".into(), *v); }
        else if let Some(v) = error.details { value.insert("details".into(), *v); }
        Self::from_ordered(JsValue::Object(value))
    }
}

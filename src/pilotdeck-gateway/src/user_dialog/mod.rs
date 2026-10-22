//! Opt-in user-dialog persistence and in-process ownership primitives.
//!
//! This module does not register Gateway methods or attach a WS/session runtime.
//! Values remain in the JS boundary model to preserve TS disk key order, numbers,
//! undefined fields and UTF-16 strings. Answer validation belongs to the caller,
//! before consuming the bus or submitting to the hosted store.

mod bus;
mod channel;
#[cfg(test)]
mod channel_tests;
mod journal;
mod store;

pub use bus::{ConsumeResult, GatewayUserDialogBus, PendingDialog};
pub use channel::{
    AbortSignal, ChannelOptions, ChannelRequest, ConfirmRequest, FormRequest,
    GatewayUserDialogChannel, InputRequest, SelectChoice, SelectRequest, UserDialogStore,
};
pub use journal::{
    GatewayUserDialogJournal, JournalOptions, UserDialogJournal, create_gateway_user_dialog_journal,
};
pub use store::{FileGatewayUserDialogStore, StoreOptions};

use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use pilotdeck_transport::system_clock::{format_epoch_iso, parse_node22_date};
use std::{fmt, io, sync::Arc, time::SystemTime};

#[derive(Debug)]
pub enum DialogError {
    Io(io::Error),
    Invalid(String),
}

impl fmt::Display for DialogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Invalid(message) => message.fmt(f),
        }
    }
}
impl std::error::Error for DialogError {}
impl From<io::Error> for DialogError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
pub type Result<T> = std::result::Result<T, DialogError>;

/// Injected wall clock and opaque identifiers; file-lock ages use real wall time.
#[derive(Clone)]
pub struct DialogRuntime {
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub uuid: Arc<dyn Fn() -> String + Send + Sync>,
}
impl Default for DialogRuntime {
    fn default() -> Self {
        Self {
            now: Arc::new(|| {
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|duration| duration.as_millis() as i64)
                    .unwrap_or_else(|error| -(error.duration().as_millis() as i64))
            }),
            uuid: Arc::new(|| uuid::Uuid::new_v4().to_string()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct StoreKey {
    pub project_root: JsString,
    pub pilot_home: JsString,
    pub session_id: JsString,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredDialog {
    pub request: JsValue,
    pub created_at: JsString,
}
impl StoredDialog {
    pub fn to_value(&self) -> JsValue {
        object([
            ("request", self.request.clone()),
            ("createdAt", JsValue::String(self.created_at.clone())),
        ])
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum DialogAnswer {
    Answered(JsValue),
    Cancelled(Option<JsString>),
}
impl DialogAnswer {
    pub fn to_value(&self) -> JsValue {
        match self {
            Self::Answered(value) => {
                object([("behavior", "answered".into()), ("value", value.clone())])
            }
            Self::Cancelled(reason) => {
                let mut value = object([("behavior", "cancelled".into())]);
                if let Some(reason) = reason.as_ref().filter(|reason| !reason.is_empty()) {
                    set(&mut value, "reason", JsValue::String(reason.clone()));
                }
                value
            }
        }
    }
    /// Native channel answer shape (`type`, not the store's `behavior`). The TS
    /// channel keeps an empty cancellation reason here; the hosted-store writer
    /// drops it, so the two serializers intentionally differ.
    pub fn to_channel_value(&self) -> JsValue {
        match self {
            Self::Answered(value) => {
                object([("type", "answered".into()), ("value", value.clone())])
            }
            Self::Cancelled(reason) => {
                let mut value = object([("type", "cancelled".into())]);
                if let Some(reason) = reason {
                    set(&mut value, "reason", JsValue::String(reason.clone()));
                }
                value
            }
        }
    }
    fn from_value(value: &JsValue) -> Result<Self> {
        match string(value, "behavior")?.to_utf8().as_deref() {
            Ok("answered") => Ok(Self::Answered(
                value.get("value").cloned().unwrap_or(JsValue::Undefined),
            )),
            Ok("cancelled") => Ok(Self::Cancelled(optional_string(value, "reason")?)),
            _ => Err(invalid_record()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredAnswer {
    pub request_id: JsString,
    pub result: DialogAnswer,
    pub submitted_at: JsString,
}
impl StoredAnswer {
    pub fn to_value(&self) -> JsValue {
        object([
            ("requestId", JsValue::String(self.request_id.clone())),
            ("result", self.result.to_value()),
            ("submittedAt", JsValue::String(self.submitted_at.clone())),
        ])
    }
    fn from_value(value: &JsValue) -> Result<Self> {
        Ok(Self {
            request_id: string(value, "requestId")?.clone(),
            result: DialogAnswer::from_value(value.get("result").ok_or_else(invalid_record)?)?,
            submitted_at: string(value, "submittedAt")?.clone(),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lease {
    pub lease_id: JsString,
    pub expires_at: JsString,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LeaseClaim {
    Claimed(Lease),
    Busy { expires_at: JsString },
    NotPending,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OwnerClaim {
    Owned {
        owner_id: JsString,
        expires_at: JsString,
    },
    Busy {
        expires_at: JsString,
    },
    NotPending,
}

fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    let mut object = JsObject::new();
    for (name, value) in fields {
        object.insert(name.into(), value);
    }
    JsValue::Object(object)
}
fn set(value: &mut JsValue, name: &str, field: JsValue) {
    if let JsValue::Object(object) = value {
        object.insert(name.into(), field);
    }
}
fn delete(value: &mut JsValue, name: &str) {
    if let JsValue::Object(source) = value {
        let mut result = JsObject::new();
        for (key, field) in source.entries() {
            if !key.equals_str(name) {
                result.insert(key.clone(), field.clone());
            }
        }
        *value = JsValue::Object(result);
    }
}
fn string<'a>(value: &'a JsValue, name: &str) -> Result<&'a JsString> {
    value
        .get(name)
        .and_then(JsValue::as_string)
        .ok_or_else(invalid_record)
}
fn optional_string(value: &JsValue, name: &str) -> Result<Option<JsString>> {
    match value.get(name) {
        None | Some(JsValue::Undefined) => Ok(None),
        Some(JsValue::String(value)) => Ok(Some(value.clone())),
        _ => Err(invalid_record()),
    }
}
fn invalid_record() -> DialogError {
    DialogError::Invalid("invalid store record".into())
}
fn json(value: &JsValue) -> Result<String> {
    stringify_json(value)
        .map_err(|error| DialogError::Invalid(error.to_string()))?
        .ok_or_else(invalid_record)
}
fn parse_expiry(value: &JsString) -> Result<f64> {
    parse_node22_date(value)
        .map_err(|_| invalid_record())?
        .ok_or_else(invalid_record)
}
fn expiry(now: i64, ttl_ms: f64) -> Result<JsString> {
    let ms = now as f64 + ttl_ms;
    if !ms.is_finite() || ms.abs() > 8_640_000_000_000_000.0 {
        return Err(DialogError::Invalid("Invalid time value".into()));
    }
    Ok(format_epoch_iso(ms as i64).into())
}
fn positive_integer(value: f64, name: &str) -> Result<()> {
    if !value.is_finite() || value <= 0.0 || value > 9_007_199_254_740_991.0 || value.fract() != 0.0
    {
        return Err(DialogError::Invalid(format!(
            "{name} must be a positive safe integer."
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;

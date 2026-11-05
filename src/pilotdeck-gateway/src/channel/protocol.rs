//! Minimal channel lifecycle contract. Inbound turn submission is a later unit.
use std::{fmt, future::Future, pin::Pin, sync::Arc};

use pilotdeck_protocol::{JsString, JsValue};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// TS GatewayChannelKey is an open string union (including non-UTF-8 JS strings).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ChannelKey(JsString);
impl ChannelKey {
    pub fn new(value: impl Into<JsString>) -> Self {
        Self(value.into())
    }
    pub fn as_js(&self) -> &JsString {
        &self.0
    }
}
impl fmt::Display for ChannelKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf16_lossy(self.0.units()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelError {
    Error(String),
    Plain(String),
    Aggregate {
        message: String,
        errors: Vec<ChannelError>,
    },
}
impl ChannelError {
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error(message.into())
    }
    pub fn message(&self) -> &str {
        match self {
            Self::Error(message) | Self::Plain(message) | Self::Aggregate { message, .. } => {
                message
            }
        }
    }
    pub fn to_js_string(&self) -> String {
        match self {
            Self::Error(message) => format!("Error: {message}"),
            Self::Plain(message) => message.clone(),
            Self::Aggregate { message, .. } => format!("AggregateError: {message}"),
        }
    }
}
impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_js_string())
    }
}
impl std::error::Error for ChannelError {}

/// The lifecycle reports these states; channel-specific login states belong to adapters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelState {
    Starting,
    Connected,
    Stopped,
    Failed,
}
#[derive(Clone, Debug)]
pub struct ChannelStatus {
    pub key: ChannelKey,
    pub state: ChannelState,
    pub message: String,
    pub error: Option<String>,
}
pub type StatusReporter = Arc<dyn Fn(ChannelStatus) + Send + Sync>;
pub type ChannelLog = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// One inbound message handed to the shared A7 acceptor. The adapter supplies
/// only the TS `submitTurn` inputs; session ownership, admission, projection and
/// durable acceptance stay in the runtime that owns the `TurnSubmitter`.
#[derive(Clone, Debug)]
pub struct TurnRequest {
    pub session_key: JsString,
    pub channel_key: ChannelKey,
    pub message: JsString,
}

/// The event stream of one accepted turn. Dropping it is the TS consumer
/// cleanup: the underlying submission drains its own writer; it never creates
/// a second session owner.
pub trait TurnEventStream: Send {
    fn next(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>>;
}

/// A7 inbound port. The production implementation wraps the same
/// `runtime::turn::submit::TurnSubmitter` the WS host uses; tests replace this
/// port with a loopback mock and never touch an account, model or user home.
pub trait TurnGateway: Send + Sync {
    fn submit(
        &self,
        request: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>>;
}

/// Config is shared with each adapter. The A7 port is optional, like TS's
/// `deps.gateway`: a channel that needs it and does not get one answers the
/// same "Gateway not ready" failure instead of fabricating a session owner.
#[derive(Clone, Default)]
pub struct ChannelStartDeps {
    pub config: Option<Arc<JsValue>>,
    pub report_status: Option<StatusReporter>,
    pub log: Option<ChannelLog>,
    pub turn_gateway: Option<Arc<dyn TurnGateway>>,
}

pub trait ChannelHandle: Send + Sync {
    /// Stop accepting messages when called, then resolve after in-flight work drains.
    /// Returning an error leaves a tracked handle available for a later stop attempt.
    /// A late start after shutdown was never tracked: its stop error becomes a
    /// startup failure without retry or restoration, as in the TS lifecycle.
    fn stop(&self, reason: &str) -> BoxFuture<Result<(), ChannelError>>;
}
pub trait ChannelAdapter: Send + Sync {
    fn channel_key(&self) -> &ChannelKey;
    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>>;
}

/// Only the shutdown ordering of the enclosing host is needed in this component.
pub type CloseHost = Arc<dyn Fn() -> BoxFuture<Result<(), ChannelError>> + Send + Sync>;
#[derive(Default)]
pub struct ReconcileInput {
    pub channels: Vec<Arc<dyn ChannelAdapter>>,
    pub managed_channel_keys: Vec<ChannelKey>,
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconcileResult {
    pub started: Vec<ChannelKey>,
    pub stopped: Vec<ChannelKey>,
}

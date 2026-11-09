//! Minimal channel lifecycle contract. Inbound turn submission enters through
//! the [`TurnGateway`] port (A7 submitTurn shape; first consumer: #1334).
use std::{fmt, future::Future, pin::Pin, sync::Arc};

use pilotdeck_protocol::{JsString, JsValue};

use super::im_cron::CronResultDelivery;

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

/// Optional submitTurn fields used by richer adapters. The common runtime
/// remains responsible for project authority, attachment preparation and policy.
#[derive(Clone, Debug, Default)]
pub struct ChannelTurnOptions {
    pub project_key: Option<JsString>,
    pub attachments: Vec<JsValue>,
    pub allow_plan_mode_tools: Option<bool>,
    pub synthetic_messages: Vec<JsValue>,
    pub mode: Option<JsString>,
    pub base_permission_mode: Option<JsString>,
    pub timeout_ms: Option<f64>,
}

impl ChannelTurnOptions {
    pub fn is_empty(&self) -> bool {
        self.project_key.is_none()
            && self.attachments.is_empty()
            && self.allow_plan_mode_tools.is_none()
            && self.synthetic_messages.is_empty()
            && self.mode.is_none()
            && self.base_permission_mode.is_none()
            && self.timeout_ms.is_none()
    }
}

/// Commands share the real Gateway request dispatcher; adapters do not get a
/// second session registry. Optional local search is injected independently.
pub trait ChannelControlPort: Send + Sync {
    fn call(
        &self,
        method: &'static str,
        params: JsValue,
    ) -> BoxFuture<Result<JsValue, ChannelError>>;
}

pub trait ChannelSearchPort: Send + Sync {
    fn search(&self, params: JsValue) -> BoxFuture<Result<JsValue, ChannelError>>;
}

/// A source-install update runs through the same native CLI transaction.
pub struct ChannelUpdateResult {
    pub code: i32,
    pub output: String,
}
pub trait ChannelUpdatePort: Send + Sync {
    fn run(&self, check: bool) -> BoxFuture<Result<ChannelUpdateResult, ChannelError>>;
}
pub struct ChannelStatusSnapshot {
    pub branch: String,
    pub commit: String,
    pub uptime: f64,
    pub platform: String,
    pub runtime_version: String,
}
pub trait ChannelStatusPort: Send + Sync {
    fn read(&self) -> BoxFuture<Result<ChannelStatusSnapshot, ChannelError>>;
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

    fn submit_with_options(
        &self,
        request: TurnRequest,
        options: ChannelTurnOptions,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        if options.is_empty() {
            self.submit(request)
        } else {
            Box::pin(async {
                Err(ChannelError::error(
                    "Channel gateway does not support extended turn inputs",
                ))
            })
        }
    }
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
    /// TS optional `deliverCronResult`. The default is the absent method:
    /// the channel did not take the delivery.
    fn deliver_cron_result(&self, _delivery: &CronResultDelivery) -> BoxFuture<bool> {
        Box::pin(async { false })
    }
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

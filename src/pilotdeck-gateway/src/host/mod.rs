//! Host side of Module 2.0 over an owned stdio child (the inverse of transport/).
//!
//! One client owns one turn. `execute` consumes it and always closes the child;
//! `spawn_resumable` can replace the stdio transport once, without re-executing
//! the turn. The replacement must reach the same logical sidecar instance;
//! ordinary TS stdio processes have no process-external replay store.
//! Callbacks must be cooperative async work, and may complete concurrently with
//! incoming frames. Dropping their futures must stop host work they own. Transport
//! failure after acceptance is uncertainty, never permission to retry an effect.
//!
//! Resource policy (#191): pipe input uses bounded-channel backpressure; the
//! event application queue fails closed with `Limit` when full so a stalled
//! consumer cannot indefinitely hide cancellation or input errors. TS's async
//! generator instead pulls events serially (client:659-684). No event is dropped.
//! `max_buffered_bytes` counts retained NDJSON bytes, including partial input,
//! queued/in-flight event copies, callback requests, completed responses/cache
//! and required output. Allocator overhead, temporary JSON encoding/parsing and
//! handler-owned allocations are not an RSS guarantee. ACK and cancel each have
//! an independent bounded latest slot plus one in-flight frame, at most twice
//! `max_frame_bytes` each; they cannot spend required-write capacity or budget.
//!
//! `execution_timeout` is a configurable local watchdog (default 300 seconds),
//! not operationDeadline or a TS cancel timeout. Session hosts must configure it
//! for their longest legitimate turn. After final application it is replaced by
//! `terminal_drain_timeout`; unfinished cooperative callbacks are then dropped.
//! Unix close/Drop kills the owned process group, including grandchildren that
//! retain pipes. A subprocess that deliberately leaves that group is not owned.
//! Abrupt host death that bypasses close/Drop has no descendant cleanup guarantee.
mod budget;
mod connection;
mod execution;
mod recovery;

use connection::Connection;
use pilotdeck_protocol::{
    JsObject, JsString, JsValue,
    admission::{
        ExecutionIdentity, SidecarBinding, parse_sidecar_binding, parse_sidecar_capabilities,
        same_binding,
    },
    module::ModuleMethod,
    wire::{EnvelopeRef, MessageRef},
};
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{process::Command, sync::watch, time::timeout};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub handshake_timeout: Duration,
    /// Overall local resource bound, not an operationDeadline or cancel request.
    pub execution_timeout: Duration,
    /// After a validated terminal is applied, allow callbacks this long to finish.
    pub terminal_drain_timeout: Duration,
    pub io_timeout: Duration,
    pub close_timeout: Duration,
    pub queue_capacity: usize,
    pub max_frame_bytes: usize,
    pub max_stderr_bytes: usize,
    pub max_module_calls: usize,
    /// Combined retained NDJSON bytes in required writes, input, events and cache.
    /// ACK/cancel each have a separate latest slot and in-flight allowance,
    /// each bounded by max_frame_bytes (see module resource policy).
    /// Handler-owned allocations and allocator overhead are outside this budget.
    /// Must cover at least one max_frame_bytes frame. Concurrent copies/cache
    /// can still exhaust this shared budget and fail closed with Limit.
    pub max_buffered_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(10),
            execution_timeout: Duration::from_secs(300),
            terminal_drain_timeout: Duration::from_millis(100),
            io_timeout: Duration::from_secs(5),
            close_timeout: Duration::from_secs(1),
            queue_capacity: 256,
            max_frame_bytes: 16 * 1024 * 1024,
            max_stderr_bytes: 64 * 1024,
            max_module_calls: 4096,
            max_buffered_bytes: 64 * 1024 * 1024,
        }
    }
}
impl Limits {
    fn validate(self) -> Result<(), HostError> {
        if [
            self.handshake_timeout,
            self.execution_timeout,
            self.io_timeout,
            self.terminal_drain_timeout,
            self.close_timeout,
        ]
        .contains(&Duration::ZERO)
            || self.queue_capacity == 0
            || self.max_frame_bytes == 0
            || self.max_stderr_bytes == 0
            || self.max_module_calls == 0
            || self.max_buffered_bytes == 0
        {
            return Err(HostError::protocol("Sidecar limits must be positive."));
        }
        // Bound allocations and keep channel capacities below Semaphore::MAX_PERMITS.
        for (name, value, maximum) in [
            ("queue_capacity", self.queue_capacity, 1 << 20),
            ("max_module_calls", self.max_module_calls, 1 << 20),
            ("max_frame_bytes", self.max_frame_bytes, 64 << 20),
            ("max_stderr_bytes", self.max_stderr_bytes, 64 << 20),
            ("max_buffered_bytes", self.max_buffered_bytes, 1 << 30),
        ] {
            if value > maximum {
                return Err(HostError::new(
                    HostErrorKind::Limit,
                    format!("Sidecar {name} exceeds maximum {maximum}."),
                ));
            }
        }
        if self.max_buffered_bytes < self.max_frame_bytes {
            return Err(HostError::protocol(
                "Sidecar max_buffered_bytes must be at least max_frame_bytes.",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostErrorKind {
    Protocol,
    Disconnected,
    Io,
    Timeout,
    Limit,
    Callback,
    ResultUnknown,
}
#[derive(Debug, Clone)]
pub struct HostError {
    pub kind: HostErrorKind,
    pub message: String,
    /// Set for every failure after a stream was accepted, even a protocol error.
    pub accepted: bool,
    pub stream_id: Option<JsString>,
    /// Last successfully applied event; -1 means no event has been applied.
    pub last_applied_sequence: Option<f64>,
}
impl HostError {
    pub fn new(kind: HostErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            accepted: false,
            stream_id: None,
            last_applied_sequence: None,
        }
    }
    fn protocol(message: impl Into<String>) -> Self {
        Self::new(HostErrorKind::Protocol, message)
    }
    fn io(error: std::io::Error) -> Self {
        Self::new(HostErrorKind::Io, error.to_string())
    }
}
impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} (accepted={}, stream_id={:?}, last_applied_sequence={:?}): {}",
            self.kind, self.accepted, self.stream_id, self.last_applied_sequence, self.message
        )
    }
}
impl std::error::Error for HostError {}

pub type HostFuture<T> = Pin<Box<dyn Future<Output = Result<T, HostError>> + Send>>;
/// Returned module payload is wrapped with the callback's own correlation IDs.
/// Events arrive in sequence and are ACKed only after `event` succeeds. A pending
/// module callback never prevents an event callback or input validation progressing.
pub trait HostHandler: Send + Sync + 'static {
    fn module_call(&self, call: JsValue) -> HostFuture<JsObject>;
    fn event(&self, _event: JsValue) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
    /// Passive, synchronous telemetry. Must not block; panics are isolated from
    /// execution, like TS client:1135-1140's best-effort transport observer.
    fn observe_transport(&self, _observation: ReconnectObservation) {}
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReconnectObservation {
    Started {
        attempt: u32,
        last_applied_sequence: f64,
    },
    Succeeded {
        attempt: u32,
    },
    Failed {
        attempt: u32,
    },
    SidecarInstanceRestarted,
}

#[derive(Debug, Clone)]
pub struct ExecuteRequest {
    pub identity: ExecutionIdentity,
    pub payload: JsObject,
    pub idempotency_key: Option<JsString>,
    pub operation_deadline: Option<JsString>,
}
/// Protocol facts; the later session host projects these into its domain terminal.
#[derive(Debug)]
pub enum ExecuteResult {
    Terminal(JsValue),
    /// TS abortedResult (client:330,593,1338-1350,1434-1448). The session layer
    /// projects type=aborted, stopReason=aborted_streaming, turns=0 and retains
    /// its original input messages. None covers both an input cancelled before
    /// execute and the incomplete compaction final; raw incomplete payloads are
    /// never exposed as a terminal or delivered to the event handler.
    Aborted {
        terminal: Option<JsValue>,
    },
    /// TS client:780-789 accepts a final failed execute response even after a
    /// stream was accepted. This is an authoritative failure, not retry permission.
    Rejected(JsValue),
}

/// One-shot cancellation, like AbortSignal. The first reason wins; empty/absent
/// reasons are transmitted as host_abort (TS client:931-958).
#[derive(Clone)]
pub struct Cancellation(watch::Sender<Option<JsString>>);
impl Default for Cancellation {
    fn default() -> Self {
        Self(watch::channel(None).0)
    }
}
impl Cancellation {
    pub fn cancel(&self, reason: Option<JsString>) {
        self.0.send_if_modified(|current| {
            if current.is_some() {
                return false;
            }
            *current = Some(reason.unwrap_or_else(|| "host_abort".into()));
            true
        });
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.borrow().is_some()
    }
    pub(super) fn reason(&self) -> Option<JsString> {
        self.0.borrow().clone()
    }
}

#[derive(Default)]
pub(super) struct Progress {
    stream_id: Option<JsString>,
    last_applied_sequence: Arc<AtomicU64>,
    terminal_lease: Option<budget::Lease>,
}

pub struct HostClient {
    connection: Connection,
    binding: SidecarBinding,
    ack_enabled: bool,
    cancel_enabled: bool,
    limits: Limits,
    reconnect_command: Option<Command>,
    reconnect_attempted: bool,
    reconnect_succeeded: bool,
}
impl HostClient {
    /// Command is explicit so the caller controls args, cwd and isolated env.
    pub async fn spawn(
        command: &mut Command,
        expected_module_id: Option<&JsString>,
        limits: Limits,
    ) -> Result<Self, HostError> {
        limits.validate()?;
        let mut connection = Connection::spawn(command, limits)?;
        let handshake = timeout(
            limits.handshake_timeout,
            handshake(&mut connection, expected_module_id, None),
        )
        .await
        .unwrap_or_else(|_| {
            Err(HostError::new(
                HostErrorKind::Timeout,
                "Sidecar handshake timed out.",
            ))
        });
        match handshake {
            Ok((binding, ack_enabled, cancel_enabled)) => Ok(Self {
                connection,
                binding,
                ack_enabled,
                cancel_enabled,
                limits,
                reconnect_command: None,
                reconnect_attempted: false,
                reconnect_succeeded: false,
            }),
            Err(mut error) => {
                let _ = connection.close().await;
                append_diagnostic(&mut error, &connection);
                Err(error)
            }
        }
    }
    /// Opt in only when this command reconnects to a retained stream owner.
    /// Own the original builder so cwd, env_clear, env removals and platform
    /// options survive replacement exactly. No second execute is ever sent.
    /// TS client:895-928 permits one attempt with no backoff; plain TS stdio
    /// intentionally uses `spawn`, because restarting its process loses replay.
    pub async fn spawn_resumable(
        mut command: Command,
        expected_module_id: Option<&JsString>,
        limits: Limits,
    ) -> Result<Self, HostError> {
        let mut client = Self::spawn(&mut command, expected_module_id, limits).await?;
        client.reconnect_command = Some(command);
        Ok(client)
    }
    pub fn binding(&self) -> &SidecarBinding {
        &self.binding
    }
    pub fn child_id(&self) -> Option<u32> {
        self.connection.pid()
    }
    pub async fn close(mut self) -> Result<(), HostError> {
        self.connection.close().await.map_err(|mut error| {
            append_diagnostic(&mut error, &self.connection);
            error
        })
    }
    pub async fn execute(
        self,
        request: ExecuteRequest,
        handler: Arc<dyn HostHandler>,
    ) -> Result<ExecuteResult, HostError> {
        self.execute_cancellable(request, handler, Cancellation::default())
            .await
    }
    /// Cancellation is best-effort and never overrides a valid final. There is
    /// no separate cancel timer in TS; execution_timeout bounds the entire wait.
    pub async fn execute_cancellable(
        mut self,
        request: ExecuteRequest,
        handler: Arc<dyn HostHandler>,
        cancellation: Cancellation,
    ) -> Result<ExecuteResult, HostError> {
        let mut progress = Progress {
            last_applied_sequence: Arc::new(AtomicU64::new((-1.0_f64).to_bits())),
            ..Progress::default()
        };
        let result = execution::run(
            &mut self,
            request,
            handler.clone(),
            &mut progress,
            cancellation,
        )
        .await;
        // TS client:1143-1146: only a started but not successful reconnect
        // emits failure. Later replay/terminal errors cannot reverse success.
        if result.is_err() && self.reconnect_attempted && !self.reconnect_succeeded {
            recovery::observe(&handler, ReconnectObservation::Failed { attempt: 1 });
        }
        let result = match result {
            Ok(terminal @ (ExecuteResult::Terminal(_) | ExecuteResult::Aborted { .. })) => {
                self.connection.finish_terminal().await.map(|()| terminal)
            }
            Ok(rejected) => self.connection.flush().await.map(|()| rejected),
            Err(error) => Err(error),
        };
        let cleanup = self.connection.close().await;
        let result = result.and_then(|terminal| cleanup.map(|()| terminal));
        result.map_err(|mut error| {
            error.accepted = progress.stream_id.is_some();
            error.stream_id = progress.stream_id;
            error.last_applied_sequence = error
                .accepted
                .then(|| f64::from_bits(progress.last_applied_sequence.load(Ordering::SeqCst)));
            append_diagnostic(&mut error, &self.connection);
            error
        })
    }
}
fn append_diagnostic(error: &mut HostError, connection: &Connection) {
    let stderr = connection.diagnostic();
    if !stderr.is_empty() {
        error.message.push_str(&format!(" stderr: {stderr}"));
    }
}

async fn handshake(
    connection: &mut Connection,
    expected_module_id: Option<&JsString>,
    previous: Option<(&SidecarBinding, &Arc<dyn HostHandler>)>,
) -> Result<(SidecarBinding, bool, bool), HostError> {
    let mut initial = None;
    // TS client:724-738, 801-840. Generation is opaque connection context,
    // not an event field and not an ordinal (protocol.ts:340-341).
    for method in ["hello", "capabilities"] {
        let id = JsString::from(method);
        connection.send(&request_frame(method, &id, JsObject::new()))?;
        let raw = connection.receive().await?;
        let MessageRef::Response(response) = decode(&raw)? else {
            return Err(HostError::protocol(
                "Sidecar sent a non-response before handshake completed.",
            ));
        };
        if response.in_reply_to != &id || !response.ok {
            return Err(HostError::protocol(format!(
                "Sidecar {method} response rejected or incorrectly correlated."
            )));
        }
        let binding = parse_sidecar_binding(response)
            .map_err(|e| HostError::protocol(String::from_utf16_lossy(e.message.units())))?;
        if method == "hello" {
            if expected_module_id.is_some_and(|id| *id != binding.module_id) {
                return Err(HostError::protocol(
                    "Sidecar module identity does not match configured implementation.",
                ));
            }
            // TS client:814-820: generation may change, the stream owner may not.
            if let Some((previous, handler)) = previous {
                if previous.module_id != binding.module_id {
                    return Err(HostError::protocol(
                        "Sidecar changed its module identity while resuming a stream.",
                    ));
                }
                if previous.module_instance_id != binding.module_instance_id {
                    recovery::observe(handler, ReconnectObservation::SidecarInstanceRestarted);
                    return Err(HostError::protocol(
                        "Sidecar instance restarted while resuming a stream.",
                    ));
                }
            }
            initial = Some(binding);
            continue;
        }
        if !same_binding(initial.as_ref().expect("hello binding"), &binding) {
            return Err(HostError::protocol(
                "Sidecar changed its binding during handshake (including connection generation).",
            ));
        }
        let caps = parse_sidecar_capabilities(response, &binding.capabilities_version)
            .map_err(|e| HostError::protocol(String::from_utf16_lossy(e.message.units())))?;
        let execute = caps
            .methods
            .iter()
            .find(|m| m.name == ModuleMethod::Execute)
            .filter(|m| m.fields.get("enabled").and_then(JsValue::as_bool) != Some(false))
            .filter(|m| {
                matches!(m.fields.get("profiles"), Some(JsValue::Array(profiles))
                if profiles.iter().any(|p| text_is(Some(p), "streaming")))
            })
            .ok_or_else(|| {
                HostError::protocol("Sidecar does not advertise a streaming execute capability.")
            })?;
        let enabled = |name| {
            caps.methods.iter().any(|m| {
                m.name == name && m.fields.get("enabled").and_then(JsValue::as_bool) != Some(false)
            })
        };
        let ack = text_is(execute.fields.get("resumeSupport"), "streaming")
            && enabled(ModuleMethod::Resume)
            && enabled(ModuleMethod::Ack);
        let cancel = enabled(ModuleMethod::Cancel)
            || execute.fields.get("cancel").and_then(JsValue::as_bool) == Some(true);
        return Ok((binding, ack, cancel));
    }
    unreachable!("handshake has two phases")
}
fn decode(value: &JsValue) -> Result<MessageRef<'_>, HostError> {
    EnvelopeRef::decode(value).map(|e| e.message).map_err(|e| {
        HostError::protocol(format!(
            "Invalid sidecar message: {}: {}",
            e.code, e.message
        ))
    })
}
fn text_is(value: Option<&JsValue>, expected: &str) -> bool {
    value
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.equals_str(expected))
}
fn request_frame(method: &str, id: &JsString, payload: JsObject) -> JsValue {
    let mut frame = JsObject::new();
    frame.insert("kind".into(), "request".into());
    frame.insert("messageId".into(), JsValue::String(id.clone()));
    frame.insert("method".into(), method.into());
    frame.insert("payload".into(), JsValue::Object(payload));
    JsValue::Object(frame)
}

#[cfg(test)]
mod tests;

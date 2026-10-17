//! Host side of Module 2.0 over an owned stdio child (the inverse of transport/).
//!
//! One client owns one turn. `execute` consumes it and always closes the child;
//! there is deliberately no reconnect, replay, cancel or session persistence here.
//! Callbacks must be cooperative async work, and may complete concurrently with
//! incoming frames. Dropping their futures must stop host work they own. Transport
//! failure after acceptance is uncertainty, never permission to retry an effect.
mod connection;
mod execution;

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
use std::{fmt, future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::{process::Command, time::timeout};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub handshake_timeout: Duration,
    /// Overall local resource bound, not an operationDeadline or cancel request.
    pub execution_timeout: Duration,
    pub io_timeout: Duration,
    pub close_timeout: Duration,
    pub queue_capacity: usize,
    pub max_frame_bytes: usize,
    pub max_stderr_bytes: usize,
    pub max_module_calls: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(10),
            execution_timeout: Duration::from_secs(300),
            io_timeout: Duration::from_secs(5),
            close_timeout: Duration::from_secs(1),
            queue_capacity: 256,
            max_frame_bytes: 16 * 1024 * 1024,
            max_stderr_bytes: 64 * 1024,
            max_module_calls: 4096,
        }
    }
}
impl Limits {
    fn validate(self) -> Result<(), HostError> {
        if [
            self.handshake_timeout,
            self.execution_timeout,
            self.io_timeout,
            self.close_timeout,
        ]
        .contains(&Duration::ZERO)
            || self.queue_capacity == 0
            || self.max_frame_bytes == 0
            || self.max_stderr_bytes == 0
            || self.max_module_calls == 0
        {
            return Err(HostError::protocol("Sidecar limits must be positive."));
        }
        // Bound allocations and keep channel capacities below Semaphore::MAX_PERMITS.
        for (name, value, maximum) in [
            ("queue_capacity", self.queue_capacity, 1 << 20),
            ("max_module_calls", self.max_module_calls, 1 << 20),
            ("max_frame_bytes", self.max_frame_bytes, 64 << 20),
            ("max_stderr_bytes", self.max_stderr_bytes, 64 << 20),
        ] {
            if value > maximum {
                return Err(HostError::new(
                    HostErrorKind::Limit,
                    format!("Sidecar {name} exceeds maximum {maximum}."),
                ));
            }
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
}
impl HostError {
    pub fn new(kind: HostErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            accepted: false,
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
            "{:?} (accepted={}): {}",
            self.kind, self.accepted, self.message
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
    /// TS client:785-789 accepts a final failed execute response before a stream.
    Rejected(JsValue),
}

pub struct HostClient {
    connection: Connection,
    binding: SidecarBinding,
    ack_enabled: bool,
    limits: Limits,
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
            handshake(&mut connection, expected_module_id),
        )
        .await
        .unwrap_or_else(|_| {
            Err(HostError::new(
                HostErrorKind::Timeout,
                "Sidecar handshake timed out.",
            ))
        });
        match handshake {
            Ok((binding, ack_enabled)) => Ok(Self {
                connection,
                binding,
                ack_enabled,
                limits,
            }),
            Err(mut error) => {
                let _ = connection.close().await;
                append_diagnostic(&mut error, &connection);
                Err(error)
            }
        }
    }
    pub fn binding(&self) -> &SidecarBinding {
        &self.binding
    }
    pub fn child_id(&self) -> Option<u32> {
        self.connection.pid()
    }
    pub async fn close(mut self) -> Result<(), HostError> {
        self.connection.close().await
    }
    pub async fn execute(
        mut self,
        request: ExecuteRequest,
        handler: Arc<dyn HostHandler>,
    ) -> Result<ExecuteResult, HostError> {
        let mut accepted = false;
        let result = timeout(
            self.limits.execution_timeout,
            execution::run(
                &mut self.connection,
                request,
                handler,
                self.ack_enabled,
                self.limits,
                &mut accepted,
            ),
        )
        .await
        .unwrap_or_else(|_| {
            Err(HostError::new(
                HostErrorKind::Timeout,
                "Sidecar execute timed out.",
            ))
        });
        let result = match result {
            Ok(terminal @ ExecuteResult::Terminal(_)) => {
                self.connection.finish_terminal().await.map(|()| terminal)
            }
            Ok(rejected) => self.connection.flush().await.map(|()| rejected),
            Err(error) => Err(error),
        };
        let cleanup = self.connection.close().await;
        let result = result.and_then(|terminal| cleanup.map(|()| terminal));
        result.map_err(|mut error| {
            error.accepted = accepted;
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
) -> Result<(SidecarBinding, bool), HostError> {
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
        return Ok((binding, ack));
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

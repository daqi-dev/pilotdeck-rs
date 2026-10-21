//! Native tool registration and batch execution. The scheduler owns every
//! child future, so cancellation of its host operation cannot detach work.
pub mod builtin;
mod recovery;
pub mod registry;
pub mod scheduler;

use crate::host::{
    HostError, ModuleError,
    modules::{capability::ToolDefinition, tool_context::ToolRuntimeContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use pilotdeck_transport::system_clock::{format_epoch_iso, parse_node22_date};
pub use recovery::{build_tool_error_recovery, format_tool_error_content};
pub use registry::{ToolRegistration, ToolRegistry};
pub use scheduler::{SchedulerMode, ToolRuntime};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;

pub type ToolFuture<T> = Pin<Box<dyn Future<Output = Result<T, ToolError>> + Send>>;

/// The native counterpart of a PilotDeckToolDefinition. Futures must own their
/// I/O and stop it on Drop; spawning detached tasks violates this port contract.
///
/// Registered exception: `builtin::bash::runner` keeps the baseline's 3 s
/// post-termination SIGKILL. That bounded task owns only the unreaped shell
/// leader and its process-group guard, and kills the group at once if the
/// runtime drops it (see the runner's module docs).
pub trait ExecutableTool: ToolDefinition {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput>;
    /// `None` is TS `undefined` or `{ ok: true }`; `Some(issues)` is
    /// `{ ok: false, issues }`, including an empty issue list.
    fn validate_input(
        &self,
        _input: JsValue,
        _context: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Clone, Debug)]
pub struct ToolError {
    pub code: String,
    pub message: JsString,
    pub details: Option<JsObject>,
}
impl ToolError {
    pub fn new(code: impl Into<String>, message: impl Into<JsString>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }
    pub fn with_details(mut self, issues: Vec<JsObject>) -> Self {
        self.details = Some(object([(
            "issues",
            JsValue::Array(issues.into_iter().map(JsValue::Object).collect()),
        )]));
        self
    }
    pub fn failed(message: impl Into<JsString>) -> Self {
        Self::new("tool_execution_failed", message)
    }
    pub fn non_error() -> Self {
        Self::failed("Tool execution failed with a non-Error value.")
    }
    pub fn aborted() -> Self {
        Self::new("tool_aborted", "Tool execution was aborted.")
    }
}
#[derive(Clone, Debug, Default)]
pub struct ToolOutput {
    pub content: Vec<JsValue>,
    pub supplemental_messages: Option<Vec<JsValue>>,
    pub data: Option<JsValue>,
    pub metadata: Option<JsObject>,
}

/// Host-owned abort signal, shared with provider and hook cancellation.
pub use crate::runtime::network::Cancellation as ToolCancellation;

#[derive(Clone)]
pub struct ToolRunContext {
    /// Original turn services and shared checkpoint maps retain their identity.
    pub host: Arc<ToolRuntimeContext>,
    /// Detached per-call fields, including derived budget and permission result.
    pub fields: JsObject,
}
impl ToolRunContext {
    pub fn new(host: Arc<ToolRuntimeContext>) -> Self {
        Self {
            fields: host.fields.clone(),
            host,
        }
    }
    /// The signal only when it is a [`ToolCancellation`]; prefer
    /// [`Self::cancelled`], which also accepts the host `Cancellation`.
    pub fn cancellation(&self) -> Option<&ToolCancellation> {
        self.host.abort_signal.as_ref()?.downcast_ref()
    }
    fn host_cancellation(&self) -> Option<&crate::host::Cancellation> {
        self.host.abort_signal.as_ref()?.downcast_ref()
    }
    pub fn is_aborted(&self) -> bool {
        self.cancellation()
            .is_some_and(ToolCancellation::is_cancelled)
            || self
                .host_cancellation()
                .is_some_and(crate::host::Cancellation::is_cancelled)
    }
    /// Only timestamped event delivery can be ordered against pending timers.
    /// The legacy host signal and explicit synchronous aborts keep immediate
    /// delivery semantics.
    pub(crate) fn abort_event_at(&self) -> Option<Instant> {
        self.cancellation()?.abort_event_at()
    }
    /// Resolves once the abort signal fires, whichever of the two signal types
    /// the host stored. Without a recognised signal it never resolves, like an
    /// AbortSignal that is never aborted.
    pub async fn cancelled(&self) {
        if let Some(signal) = self.cancellation() {
            signal.cancelled().await;
        } else if let Some(signal) = self.host_cancellation() {
            signal.cancelled().await;
        } else {
            std::future::pending::<()>().await;
        }
    }
    pub fn now_ms(&self) -> Result<f64, HostError> {
        let Some(now) = &self.host.now else {
            return Ok(SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| ModuleError::new(e.to_string()))?
                .as_secs_f64()
                * 1000.0);
        };
        match &now() {
            JsValue::Number(n) => Ok(*n),
            JsValue::String(value) => parse_node22_date(value)
                .map(|n| n.unwrap_or(f64::NAN))
                .map_err(|e| ModuleError::new(e).into()),
            _ => Ok(f64::NAN),
        }
    }
    pub fn now_iso(&self) -> Result<JsString, HostError> {
        let n = self.now_ms()?;
        if !n.is_finite() || n.abs() > 8.64e15 {
            return Err(ModuleError::new("Invalid time value").into());
        }
        Ok(format_epoch_iso(n as i64).into())
    }
}

/// executionDeadline.ts narrows only the subagent budget; the operation owner
/// is responsible for enforcing its deadline by dropping the batch future.
pub fn with_operation_deadline_budget(
    context: &mut ToolRunContext,
    execution: &JsObject,
) -> Result<(), HostError> {
    let Some(deadline) = execution
        .get("operationDeadline")
        .and_then(JsValue::as_string)
        .filter(|s| !s.is_empty())
    else {
        return Ok(());
    };
    let Some(deadline) = parse_node22_date(deadline).map_err(ModuleError::new)? else {
        return Ok(());
    };
    let now = context.now_ms()?;
    if !now.is_finite() {
        return Ok(());
    }
    // `configured !== undefined && !Number.isFinite(configured)`: any present
    // non-number (string, null, boolean, object) keeps the context unchanged.
    let configured = match context.fields.get("subagentTimeoutMs") {
        None | Some(JsValue::Undefined) => None,
        Some(JsValue::Number(n)) if n.is_finite() => Some(*n),
        Some(_) => return Ok(()),
    };
    let timeout = configured
        .unwrap_or(3_600_000.0)
        .min((deadline - now).max(0.0));
    if configured == Some(timeout) || (configured.is_none() && timeout == 3_600_000.0) {
        return Ok(());
    }
    context
        .fields
        .insert("subagentTimeoutMs".into(), JsValue::Number(timeout));
    Ok(())
}

pub fn create_tool_error_result(
    id: &JsString,
    name: &JsString,
    error: &ToolError,
    started_at: &JsString,
    context: &ToolRunContext,
) -> Result<JsObject, HostError> {
    let recovery = build_tool_error_recovery(
        &error.code,
        name,
        &error.message,
        &string_field(&context.fields, "cwd"),
        &string_field(&context.fields, "permissionMode"),
        error.details.as_ref(),
    );
    let message = recovery
        .get("message")
        .and_then(JsValue::as_string)
        .expect("recovery message");
    Ok(object([
        ("type", "error".into()),
        ("toolCallId", string(id.clone())),
        ("toolName", string(name.clone())),
        (
            "error",
            JsValue::Object(object([
                ("code", string(error.code.clone())),
                ("message", string(error.message.clone())),
                (
                    "details",
                    error
                        .details
                        .clone()
                        .map(JsValue::Object)
                        .unwrap_or(JsValue::Undefined),
                ),
            ])),
        ),
        (
            "content",
            JsValue::Array(vec![JsValue::Object(object([
                ("type", "text".into()),
                (
                    "text",
                    string(format_tool_error_content(message, error.details.as_ref())?),
                ),
            ]))]),
        ),
        (
            "metadata",
            JsValue::Object(object([(
                "recovery",
                recovery.get("advice").unwrap().clone(),
            )])),
        ),
        ("startedAt", string(started_at.clone())),
        ("completedAt", string(context.now_iso()?)),
    ]))
}
pub(super) fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsObject {
    let mut result = JsObject::new();
    for (key, value) in entries {
        result.insert(key.into(), value);
    }
    result
}
pub(super) fn text(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}
pub(super) fn string(value: impl Into<JsString>) -> JsValue {
    JsValue::String(value.into())
}
pub(super) fn named(prefix: &str, name: &JsString, suffix: &str) -> JsString {
    JsString::from_utf16(
        prefix
            .encode_utf16()
            .chain(name.units().iter().copied())
            .chain(suffix.encode_utf16())
            .collect(),
    )
}
pub(super) fn string_field(fields: &JsObject, key: &str) -> JsString {
    fields
        .get(key)
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_else(|| "undefined".into())
}
pub(crate) fn js_space(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000D}' | ' ' | '\u{00A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}')
}
pub(super) fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(v) => *v,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}

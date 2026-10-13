//! Stage-one Module2.0 envelope validation. Consumer admission is a separate layer.
use crate::{JsObject, JsString, JsValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleOutcome {
    Completed,
    Failed,
    Cancelled,
    ResultUnknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retryability {
    Safe,
    Unsafe,
    RetryAfterStatus,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationState {
    Pending,
    Running,
    Completed,
    Failed,
    CancelRequested,
    Cancelled,
    Resolving,
    ResultUnknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecuteProfile {
    Unary,
    Streaming,
    SideEffect,
    Tool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleMethod {
    Execute,
    Cancel,
    Status,
    Resume,
    Ack,
    ModuleCall,
    Hello,
    Capabilities,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleTarget {
    Model,
    Budget,
    Turn,
    Capability,
    Permission,
    Checkpoint,
    Context,
    Skills,
    Knowledge,
    Lifecycle,
    Event,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleBinding {
    pub module_instance_id: JsString,
    pub connection_generation: JsString,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub code: &'static str,
    pub message: String,
}

pub const HOST_MODEL_METHODS: &[&str] = &[
    "prepare",
    "materialize_prepared_request",
    "stream",
    "stream_next",
    "close_stream",
    "get_metadata",
];
pub const HOST_BUDGET_METHODS: &[&str] = &[
    "estimate_request_input",
    "evaluate_request_budget",
    "estimate_usage_cost",
];
pub const HOST_TURN_METHODS: &[&str] =
    &["drain_steer", "drain_or_close_steer", "persist_compaction"];
pub const HOST_CONTEXT_METHODS: &[&str] = &[
    "prepare_for_model",
    "apply_tool_results",
    "recover_from_model_error",
    "capture_turn",
    "try_auto_compact",
];
pub const HOST_CAPABILITY_METHODS: &[&str] =
    &["execute", "execute_batch", "list_tools", "plan_todo"];
pub const HOST_PERMISSION_METHODS: &[&str] = &["decide"];
pub const HOST_LIFECYCLE_METHODS: &[&str] = &["dispatch"];
pub const HOST_EVENT_METHODS: &[&str] = &["emit"];

/// Filter advertised methods, retaining original order and duplicates.
pub fn read_host_methods(value: &JsValue, supported: &[&str]) -> Vec<JsString> {
    let JsValue::Array(items) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| item.as_string())
        .filter(|method| supported.iter().any(|name| method.equals_str(name)))
        .cloned()
        .collect()
}
fn text(value: Option<&JsValue>, expected: &str) -> bool {
    value
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.equals_str(expected))
}
fn id(value: Option<&JsValue>) -> bool {
    value
        .and_then(JsValue::as_string)
        .is_some_and(|s| !s.is_empty())
}
fn present(value: Option<&JsValue>) -> bool {
    value.is_some_and(|v| !v.is_undefined())
}
fn record(value: Option<&JsValue>) -> bool {
    value.and_then(JsValue::as_object).is_some()
}
fn string(value: Option<&JsValue>) -> bool {
    value.and_then(JsValue::as_string).is_some()
}
fn boolean(value: Option<&JsValue>) -> bool {
    value.and_then(JsValue::as_bool).is_some()
}
fn sequence(value: Option<&JsValue>, minimum: f64) -> bool {
    value
        .and_then(JsValue::as_number)
        .is_some_and(|v| v.is_finite() && v.fract() == 0.0 && v >= minimum)
}
fn outcome(value: Option<&JsValue>) -> bool {
    ["completed", "failed", "cancelled", "result_unknown"]
        .iter()
        .any(|s| text(value, s))
}
fn binding(value: Option<&JsValue>) -> bool {
    value
        .and_then(JsValue::as_object)
        .is_some_and(|v| id(v.get("moduleInstanceId")) && id(v.get("connectionGeneration")))
}
fn invalid(code: &'static str, message: impl Into<String>) -> Result<(), ValidationError> {
    Err(ValidationError {
        code,
        message: message.into(),
    })
}

/// Preserve original error precedence and its deliberate stage-one lax optional fields.
pub fn validate_module_message(value: &JsValue) -> Result<(), ValidationError> {
    let Some(m) = value.as_object() else {
        return invalid("INVALID_MESSAGE", "Module message must be an object.");
    };
    let get = |name| m.get(name);
    if !["request", "response", "event", "error"]
        .iter()
        .any(|v| text(get("kind"), v))
    {
        return invalid("INVALID_KIND", "Module message kind is invalid.");
    }
    if !text(get("kind"), "event") && !id(get("messageId")) {
        return invalid("MISSING_MESSAGE_ID", "Message messageId is required.");
    }
    if text(get("kind"), "event") && present(get("messageId")) && !id(get("messageId")) {
        return invalid("INVALID_EVENT_FIELD", "Event messageId is invalid.");
    }
    if text(get("kind"), "request") {
        return validate_request(m);
    }
    if text(get("kind"), "event") {
        for field in ["eventType", "streamId", "runId", "operationId", "requestId"] {
            if !id(get(field)) {
                return invalid(
                    "MISSING_EVENT_FIELD",
                    format!("Event field {field} is required."),
                );
            }
        }
        if !sequence(get("sequence"), 0.0) || !boolean(get("final")) || !record(get("payload")) {
            return invalid(
                "MISSING_EVENT_FIELD",
                "Event sequence, final, and payload are required.",
            );
        }
        if get("final").and_then(JsValue::as_bool) == Some(true) && !outcome(get("outcome")) {
            return invalid("MISSING_OUTCOME", "Final event requires outcome.");
        }
        if present(get("outcome")) && !outcome(get("outcome")) {
            return invalid("INVALID_OUTCOME", "Event outcome is invalid.");
        }
        if present(get("code")) && !id(get("code")) {
            return invalid("INVALID_EVENT_FIELD", "Event code is invalid.");
        }
        if m.contains_key("moduleInstanceId") || m.contains_key("connectionGeneration") {
            return invalid(
                "UNSUPPORTED_FIELD",
                "Connection binding belongs to transport context.",
            );
        }
        return Ok(());
    }
    if text(get("kind"), "response") {
        if !id(get("inReplyTo")) || !boolean(get("ok")) {
            return invalid("INVALID_RESPONSE", "Response requires inReplyTo and ok.");
        }
        if get("final").and_then(JsValue::as_bool) == Some(true) && !outcome(get("outcome")) {
            return invalid("MISSING_OUTCOME", "Final response requires outcome.");
        }
        if present(get("outcome")) && !outcome(get("outcome")) {
            return invalid("INVALID_OUTCOME", "Response outcome is invalid.");
        }
        return Ok(());
    }
    if !id(get("code")) || !id(get("message")) {
        return invalid("INVALID_ERROR", "Error requires code and message.");
    }
    if !["safe", "unsafe", "retry_after_status"]
        .iter()
        .any(|v| text(get("retryability"), v))
    {
        return invalid("INVALID_ERROR", "Error retryability is invalid.");
    }
    Ok(())
}
fn validate_request(m: &JsObject) -> Result<(), ValidationError> {
    let get = |name| m.get(name);
    if text(get("method"), "execute") || text(get("method"), "module_call") {
        let execute = text(get("method"), "execute");
        let prefix = if execute {
            "Execute request"
        } else {
            "Module call"
        };
        let code = if execute {
            "MISSING_EXECUTE_FIELD"
        } else {
            "MISSING_MODULE_CALL_FIELD"
        };
        for field in ["runId", "operationId", "requestId"] {
            if !id(get(field)) {
                return invalid(code, format!("{prefix} field {field} is required."));
            }
        }
        if !record(get("payload")) {
            return invalid(code, format!("{prefix} field payload is required."));
        }
        let invalid_code = if execute {
            "INVALID_EXECUTE_FIELD"
        } else {
            "INVALID_MODULE_CALL_FIELD"
        };
        if present(get("idempotencyKey")) && !id(get("idempotencyKey")) {
            return invalid(invalid_code, format!("{prefix} idempotencyKey is invalid."));
        }
        if execute {
            for field in ["operationDeadline", "attemptDeadline"] {
                if present(get(field)) && !string(get(field)) {
                    return invalid(invalid_code, format!("Execute request {field} is invalid."));
                }
            }
            if m.contains_key("attemptId") || m.contains_key("stepId") {
                return invalid(
                    "UNSUPPORTED_FIELD",
                    "attemptId and stepId are not public fields.",
                );
            }
        } else if ![
            "model",
            "budget",
            "turn",
            "capability",
            "permission",
            "checkpoint",
            "context",
            "skills",
            "knowledge",
            "lifecycle",
            "event",
        ]
        .iter()
        .any(|target| text(get("module"), target))
        {
            return invalid("INVALID_MODULE", "Module call target is invalid.");
        }
        return Ok(());
    }
    if text(get("method"), "cancel") {
        if !id(get("runId")) || !id(get("operationId")) || !id(get("reason")) {
            return invalid(
                "INVALID_CANCEL_REQUEST",
                "Cancel requires runId, operationId, and reason.",
            );
        }
        if present(get("requestId")) && !id(get("requestId")) {
            return invalid("INVALID_CANCEL_REQUEST", "Cancel requestId is invalid.");
        }
        return Ok(());
    }
    if text(get("method"), "status") {
        return if id(get("requestId")) {
            Ok(())
        } else {
            invalid("INVALID_STATUS_REQUEST", "Status requires requestId.")
        };
    }
    if text(get("method"), "resume") {
        return if id(get("streamId"))
            && binding(get("previousBinding"))
            && sequence(get("lastAppliedSequence"), -1.0)
        {
            Ok(())
        } else {
            invalid(
                "INVALID_RESUME_REQUEST",
                "Resume requires streamId, previousBinding, and lastAppliedSequence.",
            )
        };
    }
    if text(get("method"), "ack") {
        return if id(get("streamId")) && sequence(get("lastAppliedSequence"), 0.0) {
            Ok(())
        } else {
            invalid(
                "INVALID_ACK_REQUEST",
                "Ack requires streamId and lastAppliedSequence.",
            )
        };
    }
    if text(get("method"), "hello") || text(get("method"), "capabilities") {
        return if record(get("payload")) {
            Ok(())
        } else {
            invalid("INVALID_HANDSHAKE_REQUEST", "Handshake requires payload.")
        };
    }
    invalid("INVALID_METHOD", "Module request method is invalid.")
}

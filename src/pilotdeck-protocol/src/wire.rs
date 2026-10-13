//! Typed, borrowed views over accepted Module2.0 boundary values.
//! Optional fields remain raw where stage-one validation intentionally permits them.
use crate::{
    JsObject, JsString, JsValue,
    module::{self, ModuleOutcome, ModuleTarget, Retryability, ValidationError},
};

#[derive(Debug, Clone, Copy)]
pub enum Presence<'a> {
    Missing,
    Undefined,
    Null,
    Value(&'a JsValue),
}
impl<'a> Presence<'a> {
    pub fn of(value: Option<&'a JsValue>) -> Self {
        match value {
            None => Self::Missing,
            Some(JsValue::Undefined) => Self::Undefined,
            Some(JsValue::Null) => Self::Null,
            Some(value) => Self::Value(value),
        }
    }
    pub fn as_object(self) -> Option<&'a JsObject> {
        if let Self::Value(value) = self {
            value.as_object()
        } else {
            None
        }
    }
}
/// Module integers have binary64 semantics; they are not SDK safe integers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModuleSequence(f64);
impl ModuleSequence {
    pub fn new(value: f64) -> Option<Self> {
        (value.is_finite() && value.fract() == 0.0 && value >= 0.0).then_some(Self(value))
    }
    pub fn value(self) -> f64 {
        self.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResumeSequence(f64);
impl ResumeSequence {
    pub fn new(value: f64) -> Option<Self> {
        (value.is_finite() && value.fract() == 0.0 && value >= -1.0).then_some(Self(value))
    }
    pub fn value(self) -> f64 {
        self.0
    }
}
#[derive(Debug, Clone, Copy)]
pub struct OperationIdentityRef<'a> {
    pub run_id: &'a JsString,
    pub operation_id: &'a JsString,
    pub request_id: &'a JsString,
}
#[derive(Debug, Clone, Copy)]
pub struct BindingRef<'a> {
    pub module_instance_id: &'a JsString,
    pub connection_generation: &'a JsString,
}
#[derive(Debug, Clone, Copy)]
pub enum RequestBodyRef<'a> {
    Execute {
        identity: OperationIdentityRef<'a>,
        payload: &'a JsObject,
    },
    ModuleCall {
        identity: OperationIdentityRef<'a>,
        target: ModuleTarget,
        payload: &'a JsObject,
    },
    Cancel {
        run_id: &'a JsString,
        operation_id: &'a JsString,
        reason: &'a JsString,
    },
    Status {
        request_id: &'a JsString,
    },
    Resume {
        stream_id: &'a JsString,
        previous_binding: BindingRef<'a>,
        last_applied_sequence: ResumeSequence,
    },
    Ack {
        stream_id: &'a JsString,
        last_applied_sequence: ModuleSequence,
    },
    Hello {
        payload: &'a JsObject,
    },
    Capabilities {
        payload: &'a JsObject,
    },
}
#[derive(Debug, Clone, Copy)]
pub struct RequestRef<'a> {
    pub message_id: &'a JsString,
    pub body: RequestBodyRef<'a>,
    pub fields: &'a JsObject,
}
#[derive(Debug, Clone, Copy)]
pub struct ResponseRef<'a> {
    pub message_id: &'a JsString,
    pub in_reply_to: &'a JsString,
    pub ok: bool,
    pub payload: Presence<'a>,
    pub error: Presence<'a>,
    pub final_field: Presence<'a>,
    pub outcome: Option<ModuleOutcome>,
    pub fields: &'a JsObject,
}
#[derive(Debug, Clone, Copy)]
pub struct EventRef<'a> {
    pub message_id: Option<&'a JsString>,
    pub event_type: &'a JsString,
    pub stream_id: &'a JsString,
    pub sequence: ModuleSequence,
    pub identity: OperationIdentityRef<'a>,
    pub final_event: bool,
    pub outcome: Option<ModuleOutcome>,
    pub payload: &'a JsObject,
    pub fields: &'a JsObject,
}
#[derive(Debug, Clone, Copy)]
pub struct ErrorRef<'a> {
    pub message_id: &'a JsString,
    pub code: &'a JsString,
    pub message: &'a JsString,
    pub retryability: Retryability,
    pub fields: &'a JsObject,
}
#[derive(Debug, Clone, Copy)]
pub enum MessageRef<'a> {
    Request(RequestRef<'a>),
    Response(ResponseRef<'a>),
    Event(EventRef<'a>),
    Error(ErrorRef<'a>),
}
#[derive(Debug, Clone, Copy)]
pub struct EnvelopeRef<'a> {
    pub message: MessageRef<'a>,
    pub raw: &'a JsValue,
}

fn invariant() -> ValidationError {
    ValidationError {
        code: "INVALID_MESSAGE",
        message: "Module envelope invariant is invalid.".into(),
    }
}
fn string<'a>(object: &'a JsObject, field: &str) -> Result<&'a JsString, ValidationError> {
    object
        .get(field)
        .and_then(JsValue::as_string)
        .ok_or_else(invariant)
}
fn object<'a>(object: &'a JsObject, field: &str) -> Result<&'a JsObject, ValidationError> {
    object
        .get(field)
        .and_then(JsValue::as_object)
        .ok_or_else(invariant)
}
fn boolean(object: &JsObject, field: &str) -> Result<bool, ValidationError> {
    object
        .get(field)
        .and_then(JsValue::as_bool)
        .ok_or_else(invariant)
}
fn number(object: &JsObject, field: &str) -> Result<f64, ValidationError> {
    object
        .get(field)
        .and_then(JsValue::as_number)
        .ok_or_else(invariant)
}
fn identity(m: &JsObject) -> Result<OperationIdentityRef<'_>, ValidationError> {
    Ok(OperationIdentityRef {
        run_id: string(m, "runId")?,
        operation_id: string(m, "operationId")?,
        request_id: string(m, "requestId")?,
    })
}
fn outcome(m: &JsObject) -> Option<ModuleOutcome> {
    let value = m.get("outcome")?.as_string()?;
    [
        ("completed", ModuleOutcome::Completed),
        ("failed", ModuleOutcome::Failed),
        ("cancelled", ModuleOutcome::Cancelled),
        ("result_unknown", ModuleOutcome::ResultUnknown),
    ]
    .into_iter()
    .find_map(|(name, outcome)| value.equals_str(name).then_some(outcome))
}
fn target(value: &JsString) -> Result<ModuleTarget, ValidationError> {
    [
        ("model", ModuleTarget::Model),
        ("budget", ModuleTarget::Budget),
        ("turn", ModuleTarget::Turn),
        ("capability", ModuleTarget::Capability),
        ("permission", ModuleTarget::Permission),
        ("checkpoint", ModuleTarget::Checkpoint),
        ("context", ModuleTarget::Context),
        ("skills", ModuleTarget::Skills),
        ("knowledge", ModuleTarget::Knowledge),
        ("lifecycle", ModuleTarget::Lifecycle),
        ("event", ModuleTarget::Event),
    ]
    .into_iter()
    .find_map(|(name, target)| value.equals_str(name).then_some(target))
    .ok_or_else(invariant)
}
impl<'a> EnvelopeRef<'a> {
    /// This guarantees stage-one shape only. Each consumer still owns admission,
    /// including version/binding/correlation/capabilities and optional field types.
    pub fn decode(value: &'a JsValue) -> Result<Self, ValidationError> {
        module::validate_module_message(value)?;
        let m = value.as_object().ok_or_else(invariant)?;
        let kind = string(m, "kind")?;
        let message = if kind.equals_str("request") {
            let method = string(m, "method")?;
            let body = if method.equals_str("execute") {
                RequestBodyRef::Execute {
                    identity: identity(m)?,
                    payload: object(m, "payload")?,
                }
            } else if method.equals_str("module_call") {
                RequestBodyRef::ModuleCall {
                    identity: identity(m)?,
                    target: target(string(m, "module")?)?,
                    payload: object(m, "payload")?,
                }
            } else if method.equals_str("cancel") {
                RequestBodyRef::Cancel {
                    run_id: string(m, "runId")?,
                    operation_id: string(m, "operationId")?,
                    reason: string(m, "reason")?,
                }
            } else if method.equals_str("status") {
                RequestBodyRef::Status {
                    request_id: string(m, "requestId")?,
                }
            } else if method.equals_str("resume") {
                let binding = object(m, "previousBinding")?;
                RequestBodyRef::Resume {
                    stream_id: string(m, "streamId")?,
                    previous_binding: BindingRef {
                        module_instance_id: string(binding, "moduleInstanceId")?,
                        connection_generation: string(binding, "connectionGeneration")?,
                    },
                    last_applied_sequence: ResumeSequence::new(number(m, "lastAppliedSequence")?)
                        .ok_or_else(invariant)?,
                }
            } else if method.equals_str("ack") {
                RequestBodyRef::Ack {
                    stream_id: string(m, "streamId")?,
                    last_applied_sequence: ModuleSequence::new(number(m, "lastAppliedSequence")?)
                        .ok_or_else(invariant)?,
                }
            } else if method.equals_str("hello") {
                RequestBodyRef::Hello {
                    payload: object(m, "payload")?,
                }
            } else if method.equals_str("capabilities") {
                RequestBodyRef::Capabilities {
                    payload: object(m, "payload")?,
                }
            } else {
                return Err(invariant());
            };
            MessageRef::Request(RequestRef {
                message_id: string(m, "messageId")?,
                body,
                fields: m,
            })
        } else if kind.equals_str("response") {
            MessageRef::Response(ResponseRef {
                message_id: string(m, "messageId")?,
                in_reply_to: string(m, "inReplyTo")?,
                ok: boolean(m, "ok")?,
                payload: Presence::of(m.get("payload")),
                error: Presence::of(m.get("error")),
                final_field: Presence::of(m.get("final")),
                outcome: outcome(m),
                fields: m,
            })
        } else if kind.equals_str("event") {
            MessageRef::Event(EventRef {
                message_id: m.get("messageId").and_then(JsValue::as_string),
                event_type: string(m, "eventType")?,
                stream_id: string(m, "streamId")?,
                sequence: ModuleSequence::new(number(m, "sequence")?).ok_or_else(invariant)?,
                identity: identity(m)?,
                final_event: boolean(m, "final")?,
                outcome: outcome(m),
                payload: object(m, "payload")?,
                fields: m,
            })
        } else {
            let retry = string(m, "retryability")?;
            let retryability = if retry.equals_str("safe") {
                Retryability::Safe
            } else if retry.equals_str("unsafe") {
                Retryability::Unsafe
            } else {
                Retryability::RetryAfterStatus
            };
            MessageRef::Error(ErrorRef {
                message_id: string(m, "messageId")?,
                code: string(m, "code")?,
                message: string(m, "message")?,
                retryability,
                fields: m,
            })
        };
        Ok(Self {
            message,
            raw: value,
        })
    }
}

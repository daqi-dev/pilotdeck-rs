//! Host DTO projection over pilotdeck-protocol's sole Module2 validator.
use serde_json::{Map, Value};

/// Module Protocol 版本（与 sidecar 握手声明的版本一致）。
pub use pilotdeck_protocol::MODULE_PROTOCOL_VERSION;

/// 帧错误：`code` 与 sidecar 的 `ValidationError::code` 同名。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameError {
    pub code: &'static str,
    pub message: String,
}

impl FrameError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// L-C 错误码（字符串形态；由桥接层映射为 `-32xxx`）。
    pub fn code(&self) -> &'static str {
        self.code
    }
}

/// 消息类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageKind {
    Request,
    Response,
    Event,
    Error,
}

impl MessageKind {
    /// 线上 `kind` 文本。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
            Self::Event => "event",
            Self::Error => "error",
        }
    }
}

/// 重试语义（与 sidecar 的 `Retryability` 同名）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retryability {
    Safe,
    Unsafe,
    RetryAfterStatus,
}

impl Retryability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Unsafe => "unsafe",
            Self::RetryAfterStatus => "retry_after_status",
        }
    }

    fn parse(value: Option<&Value>) -> Option<Self> {
        [Self::Safe, Self::Unsafe, Self::RetryAfterStatus]
            .into_iter()
            .find(|retryability| text(value, retryability.as_str()))
    }
}

/// 模块调用目标（`module_call.module`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

impl ModuleTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Budget => "budget",
            Self::Turn => "turn",
            Self::Capability => "capability",
            Self::Permission => "permission",
            Self::Checkpoint => "checkpoint",
            Self::Context => "context",
            Self::Skills => "skills",
            Self::Knowledge => "knowledge",
            Self::Lifecycle => "lifecycle",
            Self::Event => "event",
        }
    }

    fn parse(value: Option<&Value>) -> Option<Self> {
        [
            Self::Model,
            Self::Budget,
            Self::Turn,
            Self::Capability,
            Self::Permission,
            Self::Checkpoint,
            Self::Context,
            Self::Skills,
            Self::Knowledge,
            Self::Lifecycle,
            Self::Event,
        ]
        .into_iter()
        .find(|target| text(value, target.as_str()))
    }
}

/// 一个请求帧（`kind=request`）。
#[derive(Clone, Debug, PartialEq)]
pub struct RequestFrame {
    pub message_id: String,
    /// `execute` / `module_call` / `cancel` / `status` / `resume` / `ack` / `hello` / `capabilities`.
    pub method: String,
    pub module: Option<ModuleTarget>,
    /// `module_call` 的宿主操作名（线上字段 `operation`；与 `module` 同级）。
    pub operation: Option<String>,
    pub run_id: Option<String>,
    pub operation_id: Option<String>,
    pub request_id: Option<String>,
    pub reason: Option<String>,
    pub stream_id: Option<String>,
    pub payload: Option<Value>,
    /// 原始 JSON（保留未知字段；解码→再编码不丢信息）。
    pub raw: Value,
}

/// 一个响应帧（`kind=response`）。
#[derive(Clone, Debug, PartialEq)]
pub struct ResponseFrame {
    pub request_id: Option<String>,
    pub message_id: String,
    pub in_reply_to: String,
    pub ok: bool,
    pub final_field: bool,
    pub outcome: Option<String>,
    pub payload: Option<Value>,
    pub error: Option<Value>,
    pub code: Option<String>,
    pub raw: Option<Value>,
}

impl ResponseFrame {
    /// 构造一个"已受理"响应（host → sidecar）。
    pub fn accepted(message_id: impl Into<String>, in_reply_to: impl Into<String>) -> Self {
        Self {
            request_id: None,
            message_id: message_id.into(),
            in_reply_to: in_reply_to.into(),
            ok: true,
            final_field: false,
            outcome: None,
            payload: None,
            error: None,
            code: None,
            raw: None,
        }
    }

    /// 构造一个"终态失败"响应（不伪造成功）。
    pub fn failed(
        message_id: impl Into<String>,
        in_reply_to: impl Into<String>,
        code: impl Into<String>,
    ) -> Self {
        Self {
            request_id: None,
            message_id: message_id.into(),
            in_reply_to: in_reply_to.into(),
            ok: false,
            final_field: true,
            outcome: Some("failed".to_string()),
            payload: None,
            error: None,
            code: Some(code.into()),
            raw: None,
        }
    }
}

/// 一个事件帧（`kind=event`；sidecar → host 的流式事件）。
#[derive(Clone, Debug, PartialEq)]
pub struct EventFrame {
    pub message_id: Option<String>,
    pub event_type: String,
    pub stream_id: String,
    pub sequence: f64,
    pub run_id: String,
    pub operation_id: String,
    pub request_id: String,
    pub final_event: bool,
    pub outcome: Option<String>,
    pub payload: Value,
    pub raw: Option<Value>,
}

/// 一个错误帧（`kind=error`）。
#[derive(Clone, Debug, PartialEq)]
pub struct ErrorFrame {
    pub message_id: String,
    pub code: String,
    pub message: String,
    pub retryability: Retryability,
    pub raw: Option<Value>,
}

impl ErrorFrame {
    pub fn new(
        message_id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
        retryability: Retryability,
    ) -> Self {
        Self {
            message_id: message_id.into(),
            code: code.into(),
            message: message.into(),
            retryability,
            raw: None,
        }
    }
}

/// 任意帧。
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    Request(RequestFrame),
    Response(ResponseFrame),
    Event(EventFrame),
    Error(ErrorFrame),
}

impl Frame {
    pub fn kind(&self) -> MessageKind {
        match self {
            Self::Request(_) => MessageKind::Request,
            Self::Response(_) => MessageKind::Response,
            Self::Event(_) => MessageKind::Event,
            Self::Error(_) => MessageKind::Error,
        }
    }
}

fn text(value: Option<&Value>, expected: &str) -> bool {
    value.and_then(Value::as_str) == Some(expected)
}
fn required(map: &Map<String, Value>, field: &str) -> Result<String, FrameError> {
    map.get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            FrameError::new(
                "INVALID_MESSAGE",
                format!("validated frame missing {field}"),
            )
        })
}
fn optional_string(map: &Map<String, Value>, field: &str) -> Option<String> {
    map.get(field).and_then(Value::as_str).map(str::to_owned)
}
/// 无损边界：解析并沿用 Protocol 的唯一 Module2 验证器。
/// UTF-16、显式 null 和扩展字段仍为原 JsValue；调用方可借用 EnvelopeRef。
pub fn decode_boundary_line(line: &str) -> Result<pilotdeck_protocol::JsValue, FrameError> {
    let boundary = pilotdeck_protocol::parse_json(line)
        .map_err(|error| FrameError::new("INVALID_JSON", format!("{error:?}")))?;
    pilotdeck_protocol::wire::EnvelopeRef::decode(&boundary)
        .map_err(|error| FrameError::new(error.code, error.message))?;
    Ok(boundary)
}
/// 显式 serde 互操作投影。合法但 serde 无法表示的帧并非 INVALID_JSON。
/// 需要完整 Module2 值域的调用方应使用 decode_boundary_line。
pub fn decode_line(line: &str) -> Result<Frame, FrameError> {
    decode_boundary_line(line)?;
    let value: Value = serde_json::from_str(line).map_err(|error| {
        FrameError::new(
            "UNREPRESENTABLE_FRAME",
            format!("validated Module2 frame cannot project to serde JSON: {error}"),
        )
    })?;
    project(value)
}
pub fn decode_frame(value: Value) -> Result<Frame, FrameError> {
    decode_line(&value.to_string())
}
fn project(raw: Value) -> Result<Frame, FrameError> {
    let map = raw
        .as_object()
        .ok_or_else(|| FrameError::new("INVALID_MESSAGE", "object required"))?;
    let get = |key: &str| map.get(key).cloned();
    match map.get("kind").and_then(Value::as_str) {
        Some("request") => Ok(Frame::Request(RequestFrame {
            message_id: required(map, "messageId")?,
            method: required(map, "method")?,
            module: ModuleTarget::parse(map.get("module")),
            operation: optional_string(map, "operation"),
            run_id: optional_string(map, "runId"),
            operation_id: optional_string(map, "operationId"),
            request_id: optional_string(map, "requestId"),
            reason: optional_string(map, "reason"),
            stream_id: optional_string(map, "streamId"),
            payload: get("payload"),
            raw,
        })),
        Some("response") => Ok(Frame::Response(ResponseFrame {
            request_id: optional_string(map, "requestId"),
            message_id: required(map, "messageId")?,
            in_reply_to: required(map, "inReplyTo")?,
            ok: map
                .get("ok")
                .and_then(Value::as_bool)
                .ok_or_else(|| FrameError::new("INVALID_RESPONSE", "ok required"))?,
            final_field: map.get("final").and_then(Value::as_bool) == Some(true),
            outcome: optional_string(map, "outcome"),
            payload: get("payload"),
            error: get("error"),
            code: optional_string(map, "code"),
            raw: Some(raw),
        })),
        Some("event") => Ok(Frame::Event(EventFrame {
            message_id: optional_string(map, "messageId"),
            event_type: required(map, "eventType")?,
            stream_id: required(map, "streamId")?,
            sequence: map
                .get("sequence")
                .and_then(Value::as_f64)
                .ok_or_else(|| FrameError::new("INVALID_EVENT_FIELD", "sequence required"))?,
            run_id: required(map, "runId")?,
            operation_id: required(map, "operationId")?,
            request_id: required(map, "requestId")?,
            final_event: map.get("final").and_then(Value::as_bool) == Some(true),
            outcome: optional_string(map, "outcome"),
            payload: get("payload")
                .ok_or_else(|| FrameError::new("INVALID_EVENT_FIELD", "payload required"))?,
            raw: Some(raw),
        })),
        Some("error") => Ok(Frame::Error(ErrorFrame {
            message_id: required(map, "messageId")?,
            code: required(map, "code")?,
            message: required(map, "message")?,
            retryability: Retryability::parse(map.get("retryability"))
                .ok_or_else(|| FrameError::new("INVALID_ERROR", "retryability required"))?,
            raw: Some(raw),
        })),
        _ => Err(FrameError::new("INVALID_KIND", "invalid kind")),
    }
}
pub fn encode_frame(frame: &Frame) -> String {
    let value = match frame {
        Frame::Request(request) => request.raw.clone(),
        Frame::Response(response) => response
            .raw
            .clone()
            .unwrap_or_else(|| response_value(response)),
        Frame::Event(event) => event.raw.clone().unwrap_or_else(|| event_value(event)),
        Frame::Error(error) => error.raw.clone().unwrap_or_else(|| error_value(error)),
    };
    value.to_string()
}

fn response_value(response: &ResponseFrame) -> Value {
    let mut object = Map::new();
    object.insert("kind".to_string(), Value::from("response"));
    object.insert(
        "messageId".to_string(),
        Value::from(response.message_id.clone()),
    );
    object.insert(
        "inReplyTo".to_string(),
        Value::from(response.in_reply_to.clone()),
    );
    if let Some(request_id) = &response.request_id {
        object.insert("requestId".into(), Value::from(request_id.clone()));
    }
    object.insert("ok".to_string(), Value::from(response.ok));
    if response.final_field {
        object.insert("final".to_string(), Value::from(true));
    }
    if let Some(outcome) = &response.outcome {
        object.insert("outcome".to_string(), Value::from(outcome.clone()));
    }
    if let Some(payload) = &response.payload {
        object.insert("payload".to_string(), payload.clone());
    }
    if let Some(error) = &response.error {
        object.insert("error".to_string(), error.clone());
    }
    if let Some(code) = &response.code {
        object.insert("code".to_string(), Value::from(code.clone()));
    }
    Value::Object(object)
}

fn event_value(event: &EventFrame) -> Value {
    let mut object = Map::new();
    object.insert("kind".to_string(), Value::from("event"));
    if let Some(message_id) = &event.message_id {
        object.insert("messageId".to_string(), Value::from(message_id.clone()));
    }
    object.insert(
        "eventType".to_string(),
        Value::from(event.event_type.clone()),
    );
    object.insert("streamId".to_string(), Value::from(event.stream_id.clone()));
    object.insert(
        "sequence".to_string(),
        serde_json::Number::from_f64(event.sequence)
            .map(Value::Number)
            .unwrap_or(Value::Null),
    );
    object.insert("runId".to_string(), Value::from(event.run_id.clone()));
    object.insert(
        "operationId".to_string(),
        Value::from(event.operation_id.clone()),
    );
    object.insert(
        "requestId".to_string(),
        Value::from(event.request_id.clone()),
    );
    object.insert("final".to_string(), Value::from(event.final_event));
    if let Some(outcome) = &event.outcome {
        object.insert("outcome".to_string(), Value::from(outcome.clone()));
    }
    object.insert("payload".to_string(), event.payload.clone());
    Value::Object(object)
}

fn error_value(error: &ErrorFrame) -> Value {
    let mut object = Map::new();
    object.insert("kind".to_string(), Value::from("error"));
    object.insert(
        "messageId".to_string(),
        Value::from(error.message_id.clone()),
    );
    object.insert("code".to_string(), Value::from(error.code.clone()));
    object.insert("message".to_string(), Value::from(error.message.clone()));
    object.insert(
        "retryability".to_string(),
        Value::from(error.retryability.as_str()),
    );
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_validator_owns_admission_and_raw_roundtrip_keeps_null_and_extensions() {
        let raw = r#"{"kind":"response","messageId":"m","inReplyTo":"q","ok":true,"payload":null,"future":{"nullable":null}}"#;
        let frame = decode_line(raw).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&encode_frame(&frame)).unwrap(),
            serde_json::from_str::<Value>(raw).unwrap()
        );
        let Frame::Response(frame) = frame else {
            panic!("response expected")
        };
        assert_eq!(frame.payload, Some(Value::Null));
        let missing =
            decode_line(r#"{"kind":"response","messageId":"m","inReplyTo":"q","ok":true}"#)
                .unwrap();
        let Frame::Response(missing) = missing else {
            panic!("response expected")
        };
        assert!(missing.payload.is_none());
    }
    #[test]
    fn malformed_execute_uses_the_original_pd_validator() {
        let raw = r#"{"kind":"request","messageId":"m","method":"execute","runId":"r","operationId":"o","payload":{}}"#;
        let source = pilotdeck_protocol::parse_json(raw).unwrap();
        let expected = pilotdeck_protocol::wire::EnvelopeRef::decode(&source).unwrap_err();
        assert_eq!(decode_line(raw).unwrap_err().code, expected.code);
    }
}

#[cfg(test)]
mod surrogate_boundary_tests {
    #[test]
    fn original_utf16_payload_is_lossless_and_interop_failure_is_explicit() {
        let raw = r#"{"kind":"request","messageId":"utf16","method":"module_call","runId":"r","operationId":"o","requestId":"q","module":"event","operation":"emit","payload":{"text":"\ud800"}}"#;
        let value = super::decode_boundary_line(raw).unwrap();
        pilotdeck_protocol::wire::EnvelopeRef::decode(&value).unwrap();
        let text = value
            .get("payload")
            .unwrap()
            .get("text")
            .unwrap()
            .as_string()
            .unwrap();
        assert_eq!(text.units(), &[0xd800]);
        assert_eq!(
            super::decode_line(raw).unwrap_err().code,
            "UNREPRESENTABLE_FRAME"
        );
        assert_eq!(
            super::decode_boundary_line("{").unwrap_err().code,
            "INVALID_JSON"
        );
    }
}

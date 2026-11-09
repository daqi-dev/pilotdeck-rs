//! Public native capability and approval transports (fixed TS composition baseline).
use crate::{Body, ProxyError, runtime::network::Cancellation};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Method, Request, Response, StatusCode, body::Incoming, header};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

const OPERATIONS: &[&str] = &[
    "list_tools",
    "create_tool",
    "update_tool",
    "test_tool",
    "probe_unsaved_tool",
    "remove_tool",
    "list_general_skills",
    "import_general_skill",
    "publish_general_skill",
    "archive_general_skill",
    "test_general_skill",
    "list_model_catalog",
    "extract_sop_text",
    "model_prepare",
    "model_stream",
    "file_parse",
    "task_start",
    "task_status",
    "task_result",
    "task_cancel",
    "task_events",
    "read_sop_authority",
];
const HEADERS: &[&str] = &[
    "content-type",
    "etag",
    "retry-after",
    "x-request-id",
    "x-pilotdeck-model-id",
    "x-pilotdeck-provider-id",
];

#[derive(Clone, Default)]
pub struct Bindings {
    pub provider: Option<Arc<dyn ProviderResolver>>,
    pub approvals: Option<Arc<dyn ApprovalBridge>>,
}

pub type TransportFuture<T> = Pin<Box<dyn Future<Output = Result<T, TransportError>> + Send>>;

#[derive(Clone, Debug)]
pub struct TransportError {
    pub status: Option<StatusCode>,
    pub code: Option<String>,
    pub message: Option<String>,
    /// Exact JavaScript strings; the legacy String fields remain available.
    pub ordered_code: Option<pilotdeck_protocol::JsString>,
    pub ordered_message: Option<pilotdeck_protocol::JsString>,
}
impl TransportError {
    pub fn new(status: StatusCode, code: &str) -> Self {
        Self {
            status: Some(status),
            code: Some(code.into()),
            message: None,
            ordered_code: None,
            ordered_message: None,
        }
    }
    /// Preserve JSON.parse strings in errors raised by an exact runtime port.
    pub fn ordered(
        status: Option<StatusCode>,
        code: Option<pilotdeck_protocol::JsString>,
        message: Option<pilotdeck_protocol::JsString>,
    ) -> Self {
        Self {
            status,
            code: code
                .as_ref()
                .map(|value| String::from_utf16_lossy(value.units())),
            message: message
                .as_ref()
                .map(|value| String::from_utf16_lossy(value.units())),
            ordered_code: code,
            ordered_message: message,
        }
    }
    fn host_response(message: &str) -> Self {
        Self {
            status: Some(StatusCode::BAD_GATEWAY),
            code: Some("PILOTDECK_HOST_RESPONSE_INVALID".into()),
            message: Some(message.into()),
            ordered_code: None,
            ordered_message: None,
        }
    }
}
impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            self.message
                .as_deref()
                .or(self.code.as_deref())
                .unwrap_or("Host transport failed"),
        )
    }
}
impl std::error::Error for TransportError {}

/// The root resolves the existing principal's runtime; the transport owns no ACL.
pub trait ProviderResolver: Send + Sync {
    fn resolve_ordered(
        &self,
        principal: pilotdeck_protocol::JsValue,
        signal: Cancellation,
    ) -> TransportFuture<Arc<dyn CapabilityProvider>> {
        self.resolve(js_json_lossy(&principal), signal)
    }
    fn resolve(
        &self,
        principal: Value,
        signal: Cancellation,
    ) -> TransportFuture<Arc<dyn CapabilityProvider>>;
}
pub trait CapabilityProvider: Send + Sync {
    fn operations(&self) -> Vec<String>;
    /// Exact JSON.parse view for providers whose contract includes UTF-16.
    fn call_exact(
        &self,
        operation: String,
        input: pilotdeck_protocol::JsValue,
        principal: pilotdeck_protocol::JsValue,
        signal: Cancellation,
    ) -> TransportFuture<CapabilityResponse> {
        self.call_ordered(
            operation,
            js_json_lossy(&input),
            js_json_lossy(&principal),
            signal,
            Some(input),
        )
    }
    fn call_ordered(
        &self,
        operation: String,
        input: Value,
        principal: Value,
        signal: Cancellation,
        _original: Option<pilotdeck_protocol::JsValue>,
    ) -> TransportFuture<CapabilityResponse> {
        self.call(operation, input, principal, signal)
    }
    fn call(
        &self,
        operation: String,
        input: Value,
        principal: Value,
        signal: Cancellation,
    ) -> TransportFuture<CapabilityResponse>;
}
pub struct CapabilityResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: CapabilityBody,
}
impl CapabilityResponse {
    pub fn json(status: StatusCode, body: Value) -> Self {
        Self {
            status: status.as_u16(),
            headers: HeaderMap::new(),
            body: CapabilityBody::Json(body),
        }
    }
}
pub enum CapabilityBody {
    Json(Value),
    OrderedJson(pilotdeck_protocol::JsValue),
    Bytes(Bytes),
    Text(String),
    /// An already serialized response, corresponding to TS rawBody.
    Raw(String),
    Stream(Box<dyn CapabilityStream>),
}
pub enum StreamChunk {
    Bytes(Bytes),
    Text(String),
    Json(Value),
    OrderedJson(pilotdeck_protocol::JsValue),
}
/// Pulling is driven by Hyper, so the transport never pre-buffers a stream.
pub trait CapabilityStream: Send {
    fn poll_next(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<StreamChunk, TransportError>>>;
}
impl CapabilityStream for tokio::sync::mpsc::Receiver<Result<StreamChunk, TransportError>> {
    fn poll_next(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<StreamChunk, TransportError>>> {
        self.poll_recv(cx)
    }
}

#[derive(Clone, Debug)]
pub struct ApprovalBinding {
    pub tenant_id: String,
    pub agent_id: String,
    pub pilot_deck_user_id: String,
}
/// Implementations must authenticate the header and authorize the original
/// session/locked resume store. NativeApprovalBridge implements that boundary.
pub trait ApprovalBridge: Send + Sync {
    fn binding(&self) -> ApprovalBinding;
    fn dynamic_binding(&self) -> bool {
        false
    }
    fn status(&self, input: Value, signal: Cancellation) -> TransportFuture<Value>;
    fn resume(&self, input: Value, signal: Cancellation) -> TransportFuture<Value>;
    fn status_ordered(
        &self,
        input: pilotdeck_protocol::JsValue,
        signal: Cancellation,
    ) -> TransportFuture<pilotdeck_protocol::JsValue> {
        let result = self.status(js_json_lossy(&input), signal);
        Box::pin(async move { Ok(json_value(&result.await?)) })
    }
    fn resume_ordered(
        &self,
        input: pilotdeck_protocol::JsValue,
        signal: Cancellation,
    ) -> TransportFuture<pilotdeck_protocol::JsValue> {
        let result = self.resume(js_json_lossy(&input), signal);
        Box::pin(async move { Ok(json_value(&result.await?)) })
    }
}

struct CancelOnDrop(Cancellation);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub(super) async fn handle(request: Request<Incoming>, state: &super::HttpState) -> Response<Body> {
    let path = request.uri().path().to_owned();
    if let Some(response) = preflight(
        &path,
        request.method(),
        request.headers(),
        &state.token,
        &state.module_host,
    ) {
        return response;
    }
    let approval = path.starts_with("/api/module-host/approvals/");
    let headers = request.headers().clone();
    let signal = Cancellation::new();
    let guard = CancelOnDrop(signal.clone());
    let bytes = match super::read_body(
        request.into_body(),
        if approval {
            64 * 1024
        } else {
            32 * 1024 * 1024
        },
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(response) => {
            let status = response.status();
            return fail(
                status,
                if approval {
                    if status == StatusCode::PAYLOAD_TOO_LARGE {
                        "APPROVAL_INPUT_TOO_LARGE"
                    } else {
                        "APPROVAL_INPUT_INVALID"
                    }
                } else if status == StatusCode::PAYLOAD_TOO_LARGE {
                    "PUBLIC_HOST_INPUT_TOO_LARGE"
                } else {
                    "PUBLIC_HOST_INPUT_INVALID"
                },
            );
        }
    };
    // Buffer.toString('utf8') replaces malformed byte sequences before JSON.parse.
    let raw = String::from_utf8_lossy(&bytes);
    // JSON.parse accepts unpaired UTF-16 surrogates. Use the shared JS parser
    // as the admission boundary, then expose a lossy UTF-8 view only to Rust
    // ports that require serde_json. The exact input subtree is retained below.
    let original = match pilotdeck_protocol::parse_json(&raw) {
        Ok(input) => input,
        Err(_) => {
            return fail(
                StatusCode::BAD_REQUEST,
                if approval {
                    "APPROVAL_INPUT_INVALID"
                } else {
                    "PUBLIC_HOST_INPUT_INVALID"
                },
            );
        }
    };
    let result = if approval {
        approval_request_exact(
            &path,
            original,
            &headers,
            state
                .module_host
                .approvals
                .as_ref()
                .expect("preflight binding")
                .as_ref(),
            signal.clone(),
        )
        .await
    } else {
        host_request_exact(
            &path,
            original,
            state
                .module_host
                .provider
                .as_ref()
                .expect("preflight binding")
                .as_ref(),
            signal.clone(),
        )
        .await
    };
    match result {
        Ok(response) => {
            // Keep the cancellation lease until Hyper consumes or drops the body.
            response.map(|body| {
                LeasedBody {
                    body,
                    _guard: guard,
                }
                .boxed()
            })
        }
        Err(error) => error_response(error, approval),
    }
}

fn preflight(
    path: &str,
    method: &Method,
    headers: &HeaderMap,
    token: &str,
    bindings: &Bindings,
) -> Option<Response<Body>> {
    let approval = matches!(
        path,
        "/api/module-host/approvals/status" | "/api/module-host/approvals/resume"
    );
    if !approval && !matches!(path, "/api/module-host/describe" | "/api/module-host/call") {
        return Some(fail(StatusCode::NOT_FOUND, "PUBLIC_HOST_ROUTE_NOT_FOUND"));
    }
    // This transport deliberately uses the exact TS header comparison.
    if (approval && token.is_empty())
        || headers.get_all(header::AUTHORIZATION).iter().count() != 1
        || headers
            .get(header::AUTHORIZATION)
            .is_none_or(|value| value.as_bytes() != format!("Bearer {token}").as_bytes())
    {
        return Some(fail(
            StatusCode::UNAUTHORIZED,
            "PUBLIC_HOST_TRANSPORT_UNAUTHORIZED",
        ));
    }
    if method != Method::POST {
        return Some(fail(
            StatusCode::METHOD_NOT_ALLOWED,
            "PUBLIC_HOST_METHOD_NOT_ALLOWED",
        ));
    }
    if approval && bindings.approvals.is_none() {
        return Some(fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "APPROVAL_BINDING_UNAVAILABLE",
        ));
    }
    if !approval && bindings.provider.is_none() {
        return Some(fail(
            StatusCode::NOT_IMPLEMENTED,
            "PUBLIC_HOST_PORT_UNAVAILABLE",
        ));
    }
    None
}

fn fail(status: StatusCode, code: &str) -> Response<Body> {
    super::json(status, json!({ "code": code }))
}
fn error_response(error: TransportError, approval: bool) -> Response<Body> {
    use pilotdeck_protocol::{JsObject, JsValue};
    let code = error.ordered_code.or_else(|| error.code.map(Into::into));
    let status = error.status.unwrap_or_else(|| {
        if approval
            && code
                .as_ref()
                .is_some_and(|code| code.equals_str("SOP_RESUME_REQUEST_CONFLICT"))
        {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_GATEWAY
        }
    });
    let mut body = JsObject::new();
    body.insert(
        "code".into(),
        JsValue::String(code.unwrap_or_else(|| {
            if approval {
                "PUBLIC_APPROVAL_BRIDGE_FAILED"
            } else {
                "PUBLIC_HOST_TRANSPORT_FAILED"
            }
            .into()
        })),
    );
    if !approval
        && let Some(message) = error
            .ordered_message
            .or_else(|| error.message.map(Into::into))
    {
        body.insert("message".into(), JsValue::String(message));
    }
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(full(Bytes::from(
            ordered_json(&JsValue::Object(body)).expect("error strings are JSON serializable"),
        )))
        .expect("valid response")
}
#[cfg(test)]
fn nonempty(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| !pilotdeck_protocol::js_value::js_trim(s).is_empty())
}

#[cfg(test)]
async fn host_request(
    path: &str,
    input: Value,
    resolver: &dyn ProviderResolver,
    signal: Cancellation,
) -> Result<Response<Body>, TransportError> {
    host_request_ordered(path, input, resolver, signal, None).await
}
#[cfg(test)]
async fn host_request_ordered(
    path: &str,
    input: Value,
    resolver: &dyn ProviderResolver,
    signal: Cancellation,
    original: Option<pilotdeck_protocol::JsValue>,
) -> Result<Response<Body>, TransportError> {
    let principal = &input["principal"];
    if !["pilotDeckUserId", "tenantId", "actorUserId", "agentId"]
        .iter()
        .all(|field| nonempty(&principal[field]))
    {
        return Err(TransportError::new(
            StatusCode::BAD_REQUEST,
            "PUBLIC_HOST_PRINCIPAL_REQUIRED",
        ));
    }
    let provider = resolver.resolve(principal.clone(), signal.clone()).await?;
    let operations: Vec<String> = provider
        .operations()
        .into_iter()
        .filter(|op| OPERATIONS.contains(&op.as_str()))
        .collect();
    if path == "/api/module-host/describe" {
        return Ok(super::json(
            StatusCode::OK,
            json!({ "operations": operations }),
        ));
    }
    let Some(operation) = input["operation"]
        .as_str()
        .filter(|op| operations.iter().any(|item| item == op))
    else {
        return Err(TransportError::new(
            StatusCode::CONFLICT,
            "PILOTDECK_HOST_CAPABILITY_UNAVAILABLE",
        ));
    };
    let Some(payload) = input["input"].as_object() else {
        return Err(TransportError::new(
            StatusCode::BAD_REQUEST,
            "PUBLIC_HOST_INPUT_INVALID",
        ));
    };
    if [
        "tenantId",
        "tenant_id",
        "actorUserId",
        "actor_user_id",
        "credentialId",
        "agentId",
        "agent_id",
    ]
    .iter()
    .any(|key| payload.contains_key(*key))
    {
        return Err(TransportError::new(
            StatusCode::BAD_REQUEST,
            "PUBLIC_SCOPE_OVERRIDE",
        ));
    }
    let response = provider
        .call_ordered(
            operation.into(),
            input["input"].clone(),
            principal.clone(),
            signal.clone(),
            original,
        )
        .await?;
    capability_response(response, signal)
}

fn json_value(value: &Value) -> pilotdeck_protocol::JsValue {
    pilotdeck_protocol::parse_json(&value.to_string()).expect("serde JSON is valid JS JSON")
}

async fn host_request_exact(
    path: &str,
    input: pilotdeck_protocol::JsValue,
    resolver: &dyn ProviderResolver,
    signal: Cancellation,
) -> Result<Response<Body>, TransportError> {
    use pilotdeck_protocol::JsValue;
    let undefined = JsValue::Undefined;
    let principal = input.get("principal").unwrap_or(&undefined);
    if !["pilotDeckUserId", "tenantId", "actorUserId", "agentId"]
        .iter()
        .all(|key| {
            principal
                .get(key)
                .and_then(JsValue::as_string)
                .is_some_and(|s| {
                    !crate::metadata::js_trim(&String::from_utf16_lossy(s.units())).is_empty()
                })
        })
    {
        return Err(TransportError::new(
            StatusCode::BAD_REQUEST,
            "PUBLIC_HOST_PRINCIPAL_REQUIRED",
        ));
    }
    let provider = resolver
        .resolve_ordered(principal.clone(), signal.clone())
        .await?;
    let operations: Vec<_> = provider
        .operations()
        .into_iter()
        .filter(|op| OPERATIONS.contains(&op.as_str()))
        .collect();
    if path.ends_with("/describe") {
        return Ok(super::json(
            StatusCode::OK,
            json!({"operations":operations}),
        ));
    }
    let operation = input
        .get("operation")
        .and_then(JsValue::as_string)
        .and_then(|op| operations.iter().find(|name| op.equals_str(name)));
    let Some(operation) = operation else {
        return Err(TransportError::new(
            StatusCode::CONFLICT,
            "PILOTDECK_HOST_CAPABILITY_UNAVAILABLE",
        ));
    };
    let Some(payload) = input.get("input").and_then(JsValue::as_object) else {
        return Err(TransportError::new(
            StatusCode::BAD_REQUEST,
            "PUBLIC_HOST_INPUT_INVALID",
        ));
    };
    if [
        "tenantId",
        "tenant_id",
        "actorUserId",
        "actor_user_id",
        "credentialId",
        "agentId",
        "agent_id",
    ]
    .iter()
    .any(|key| payload.get(key).is_some())
    {
        return Err(TransportError::new(
            StatusCode::BAD_REQUEST,
            "PUBLIC_SCOPE_OVERRIDE",
        ));
    }
    let response = provider
        .call_exact(
            operation.clone(),
            JsValue::Object(payload.clone()),
            principal.clone(),
            signal.clone(),
        )
        .await?;
    capability_response(response, signal)
}

async fn approval_request_exact(
    path: &str,
    mut input: pilotdeck_protocol::JsValue,
    headers: &HeaderMap,
    bridge: &dyn ApprovalBridge,
    signal: Cancellation,
) -> Result<Response<Body>, TransportError> {
    use pilotdeck_protocol::{JsObject, JsValue};
    let resume = path.ends_with("/resume");
    let Some(object) = input.as_object() else {
        return Err(TransportError::new(
            StatusCode::BAD_REQUEST,
            "APPROVAL_INPUT_INVALID",
        ));
    };
    if object.entries().iter().any(|(key, _)| {
        !["tenantId", "agentId", "sessionKey", "projectKey"]
            .iter()
            .any(|name| key.equals_str(name))
            && !(resume
                && [
                    "source",
                    "requestId",
                    "waitId",
                    "expectedRevision",
                    "message",
                    "slotUpdates",
                ]
                .iter()
                .any(|name| key.equals_str(name)))
    }) {
        return Err(TransportError::new(
            StatusCode::BAD_REQUEST,
            "APPROVAL_AUTHORITY_OVERRIDE",
        ));
    }
    let binding = bridge.binding();
    if !bridge.dynamic_binding()
        && (input.get("tenantId") != Some(&JsValue::from(binding.tenant_id.as_str()))
            || input.get("agentId") != Some(&JsValue::from(binding.agent_id.as_str())))
    {
        return Err(TransportError::new(
            StatusCode::FORBIDDEN,
            "APPROVAL_SUBJECT_MISMATCH",
        ));
    }
    if let Some(value) = headers
        .get("x-staffdeck-approver-authorization")
        .and_then(|v| v.to_str().ok())
        && let JsValue::Object(object) = &mut input
    {
        object.insert("approverAuthorization".into(), value.into());
    }
    let result = if resume {
        bridge.resume_ordered(input, signal.clone()).await?
    } else {
        let status = bridge.status_ordered(input, signal.clone()).await?;
        let mut object = JsObject::new();
        object.insert("status".into(), status);
        JsValue::Object(object)
    };
    capability_response(
        CapabilityResponse {
            status: 200,
            headers: HeaderMap::new(),
            body: CapabilityBody::OrderedJson(result),
        },
        signal,
    )
}

#[cfg(test)]
async fn approval_request(
    path: &str,
    mut input: Value,
    headers: &HeaderMap,
    bridge: &dyn ApprovalBridge,
    signal: Cancellation,
) -> Result<Response<Body>, TransportError> {
    let resume = path == "/api/module-host/approvals/resume";
    let Some(object) = input.as_object_mut() else {
        return Err(TransportError::new(
            StatusCode::BAD_REQUEST,
            "APPROVAL_INPUT_INVALID",
        ));
    };
    if object.keys().any(|key| {
        !["tenantId", "agentId", "sessionKey", "projectKey"].contains(&key.as_str())
            && !(resume
                && [
                    "source",
                    "requestId",
                    "waitId",
                    "expectedRevision",
                    "message",
                    "slotUpdates",
                ]
                .contains(&key.as_str()))
    }) {
        return Err(TransportError::new(
            StatusCode::BAD_REQUEST,
            "APPROVAL_AUTHORITY_OVERRIDE",
        ));
    }
    let binding = bridge.binding();
    if !bridge.dynamic_binding()
        && (object.get("tenantId").and_then(Value::as_str) != Some(&binding.tenant_id)
            || object.get("agentId").and_then(Value::as_str) != Some(&binding.agent_id))
    {
        return Err(TransportError::new(
            StatusCode::FORBIDDEN,
            "APPROVAL_SUBJECT_MISMATCH",
        ));
    }
    if let Some(value) = headers
        .get("x-staffdeck-approver-authorization")
        .and_then(|v| v.to_str().ok())
    {
        object.insert("approverAuthorization".into(), Value::String(value.into()));
    }
    let result = if resume {
        bridge.resume(input, signal).await?
    } else {
        json!({ "status": bridge.status(input, signal).await? })
    };
    Ok(super::json(StatusCode::OK, result))
}

fn full(bytes: Bytes) -> Body {
    Full::new(bytes)
        .map_err(|never: Infallible| match never {})
        .boxed()
}

pub(super) fn js_json_lossy(value: &pilotdeck_protocol::JsValue) -> Value {
    use pilotdeck_protocol::JsValue;
    match value {
        JsValue::Undefined | JsValue::Null => Value::Null,
        JsValue::Bool(value) => Value::Bool(*value),
        JsValue::Number(value) => Value::from(*value),
        JsValue::String(value) => Value::String(String::from_utf16_lossy(value.units())),
        JsValue::Array(values) => Value::Array(values.iter().map(js_json_lossy).collect()),
        JsValue::Object(object) => Value::Object(
            object
                .own_entries()
                .into_iter()
                .filter(|(_, value)| !value.is_undefined())
                .map(|(key, value)| (String::from_utf16_lossy(key.units()), js_json_lossy(value)))
                .collect(),
        ),
    }
}
fn capability_response(
    response: CapabilityResponse,
    signal: Cancellation,
) -> Result<Response<Body>, TransportError> {
    if !(100..=599).contains(&response.status) {
        return Err(TransportError {
            status: Some(StatusCode::BAD_GATEWAY),
            code: None,
            message: Some("Invalid host status".into()),
            ordered_code: None,
            ordered_message: None,
        });
    }
    let mut headers = HeaderMap::new();
    for &name in HEADERS {
        if let Some(value) = response.headers.get(name) {
            headers.insert(name, value.clone());
        }
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    let ndjson = content_type.is_some_and(|s| s.starts_with("application/x-ndjson"));
    let streaming = ndjson || content_type.is_some_and(|s| s.starts_with("text/event-stream"));
    let body = match response.body {
        CapabilityBody::Stream(source) => {
            if !streaming {
                return Err(TransportError::host_response(
                    "Host stream content-type is required.",
                ));
            }
            HostStreamBody {
                source: Mutex::new((source, false)),
                ndjson,
                _guard: CancelOnDrop(signal),
            }
            .boxed()
        }
        CapabilityBody::Json(_) | CapabilityBody::OrderedJson(_) if streaming => {
            return Err(TransportError::host_response("Missing host stream"));
        }
        CapabilityBody::Bytes(_) | CapabilityBody::Raw(_) | CapabilityBody::Text(_)
            if streaming =>
        {
            return Err(TransportError::host_response("Missing host stream"));
        }
        CapabilityBody::Bytes(bytes) => full(bytes),
        CapabilityBody::Raw(text) => full(Bytes::from(text)),
        CapabilityBody::OrderedJson(pilotdeck_protocol::JsValue::String(ref text))
            if content_type.is_some_and(|s| !s.contains("json")) =>
        {
            full(Bytes::from(String::from_utf16_lossy(text.units())))
        }
        CapabilityBody::OrderedJson(value) => {
            if !headers.contains_key(header::CONTENT_TYPE) {
                headers.insert(
                    header::CONTENT_TYPE,
                    hyper::header::HeaderValue::from_static("application/json"),
                );
            }
            full(Bytes::from(ordered_json(&value)?))
        }
        CapabilityBody::Text(text) if content_type.is_some_and(|s| !s.contains("json")) => {
            full(Bytes::from(text))
        }
        body => {
            let value = match body {
                CapabilityBody::Json(value) => value,
                CapabilityBody::Text(text) => Value::String(text),
                _ => unreachable!("raw and streaming bodies handled above"),
            };
            if !headers.contains_key(header::CONTENT_TYPE) {
                headers.insert(
                    header::CONTENT_TYPE,
                    hyper::header::HeaderValue::from_static("application/json"),
                );
            }
            full(Bytes::from(serde_json::to_vec(&value).map_err(|_| {
                TransportError::host_response("Host body must be JSON serializable.")
            })?))
        }
    };
    let mut output = Response::new(body);
    *output.status_mut() = StatusCode::from_u16(response.status).expect("validated host status");
    *output.headers_mut() = headers;
    Ok(output)
}
fn encode_chunk(chunk: StreamChunk, ndjson: bool) -> Result<Bytes, TransportError> {
    match chunk {
        StreamChunk::Bytes(bytes) => Ok(bytes),
        StreamChunk::Text(text) => Ok(Bytes::from(text)),
        StreamChunk::OrderedJson(value) if ndjson && value.as_object().is_some() => {
            Ok(Bytes::from(format!("{}\n", ordered_json(&value)?)))
        }
        StreamChunk::Json(value) if ndjson && value.is_object() => {
            let mut bytes = serde_json::to_vec(&value).map_err(|_| {
                TransportError::host_response("Host event must be JSON serializable.")
            })?;
            bytes.push(b'\n');
            Ok(Bytes::from(bytes))
        }
        _ => Err(TransportError::host_response(
            "Host stream requires bytes, strings, or declared NDJSON events.",
        )),
    }
}

fn ordered_json(value: &pilotdeck_protocol::JsValue) -> Result<String, TransportError> {
    pilotdeck_protocol::stringify_json(value)
        .ok()
        .flatten()
        .ok_or_else(|| TransportError::host_response("Host body must be JSON serializable."))
}
struct HostStreamBody {
    source: Mutex<(Box<dyn CapabilityStream>, bool)>,
    ndjson: bool,
    _guard: CancelOnDrop,
}
impl hyper::body::Body for HostStreamBody {
    type Data = Bytes;
    type Error = ProxyError;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, ProxyError>>> {
        let mut state = self.source.lock().expect("host stream lock");
        if state.1 {
            return Poll::Ready(None);
        }
        match state.0.poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                state.1 = true;
                Poll::Ready(None)
            }
            Poll::Ready(Some(chunk)) => {
                let result = chunk.and_then(|chunk| encode_chunk(chunk, self.ndjson));
                if result.is_err() {
                    state.1 = true;
                    self._guard.0.cancel();
                }
                Poll::Ready(Some(
                    result
                        .map(hyper::body::Frame::data)
                        .map_err(|error| -> ProxyError { Box::new(error) }),
                ))
            }
        }
    }
    fn is_end_stream(&self) -> bool {
        self.source.lock().expect("host stream lock").1
    }
}
struct LeasedBody {
    body: Body,
    _guard: CancelOnDrop,
}
impl hyper::body::Body for LeasedBody {
    type Data = Bytes;
    type Error = ProxyError;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, ProxyError>>> {
        Pin::new(&mut self.body).poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> hyper::body::SizeHint {
        self.body.size_hint()
    }
}

/// Supplies the real tool/session context, including the public principal and cancellation.
pub trait PublicToolContext: Send + Sync {
    /// Native contexts can preserve the principal's full JSON.parse view.
    /// Existing serde contexts retain their compatibility adapter.
    fn prepare_ordered(
        &self,
        call: crate::host::modules::capability::ToolCall,
        principal: pilotdeck_protocol::JsValue,
        signal: Cancellation,
    ) -> crate::host::HostFuture<crate::host::modules::capability::ToolInvocationContext> {
        self.prepare(call, js_json_lossy(&principal), signal)
    }
    fn prepare(
        &self,
        call: crate::host::modules::capability::ToolCall,
        principal: Value,
        signal: Cancellation,
    ) -> crate::host::HostFuture<crate::host::modules::capability::ToolInvocationContext>;
}
#[derive(Clone)]
pub struct NativeCapabilityProvider {
    pub tools: Arc<dyn crate::host::modules::capability::ToolExecution>,
    pub context: Arc<dyn PublicToolContext>,
}
impl ProviderResolver for NativeCapabilityProvider {
    fn resolve(
        &self,
        _principal: Value,
        _signal: Cancellation,
    ) -> TransportFuture<Arc<dyn CapabilityProvider>> {
        let provider: Arc<dyn CapabilityProvider> = Arc::new(self.clone());
        Box::pin(async move { Ok(provider) })
    }
}
impl CapabilityProvider for NativeCapabilityProvider {
    fn operations(&self) -> Vec<String> {
        vec!["list_tools".into(), "test_tool".into()]
    }
    fn call_exact(
        &self,
        operation: String,
        input: pilotdeck_protocol::JsValue,
        principal: pilotdeck_protocol::JsValue,
        signal: Cancellation,
    ) -> TransportFuture<CapabilityResponse> {
        let provider = self.clone();
        Box::pin(async move {
            use pilotdeck_protocol::{JsObject, JsString, JsValue};
            if signal.is_cancelled() {
                return Ok(CapabilityResponse::json(
                    StatusCode::from_u16(499).unwrap(),
                    json!({"code": "PUBLIC_HOST_CANCELLED"}),
                ));
            }
            let body = match operation.as_str() {
                "list_tools" => {
                    let tools = provider.tools.list().await.map_err(native_error)?;
                    let data = tools
                        .iter()
                        .map(|tool| {
                            let mut fields = JsObject::new();
                            for field in [
                                "name",
                                "aliases",
                                "title",
                                "description",
                                "kind",
                                "inputSchema",
                                "outputSchema",
                                "maxResultBytes",
                                "shouldDefer",
                                "alwaysLoad",
                                "searchHint",
                            ] {
                                if let Some(value) = tool.fields().get(field) {
                                    fields.insert(field.into(), value.clone());
                                }
                            }
                            JsValue::Object(fields)
                        })
                        .collect();
                    let mut body = JsObject::new();
                    body.insert("data".into(), JsValue::Array(data));
                    JsValue::Object(body)
                }
                "test_tool" => {
                    let name = input
                        .get("toolId")
                        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                        .or_else(|| input.get("name"))
                        .and_then(JsValue::as_string)
                        .filter(|value| {
                            !pilotdeck_protocol::js_value::js_trim(&String::from_utf16_lossy(
                                value.units(),
                            ))
                            .is_empty()
                        });
                    let Some(name) = name else {
                        return Ok(CapabilityResponse::json(
                            StatusCode::BAD_REQUEST,
                            json!({"code": "PUBLIC_HOST_INPUT_INVALID", "message": "toolId must be a non-empty string."}),
                        ));
                    };
                    let Some(arguments) = input
                        .get("input")
                        .or_else(|| input.get("body"))
                        .filter(|value| !value.is_undefined())
                    else {
                        return Ok(CapabilityResponse::json(
                            StatusCode::BAD_REQUEST,
                            json!({"code": "PUBLIC_HOST_INPUT_INVALID", "message": "test_tool requires body/input."}),
                        ));
                    };
                    let mut id: Vec<u16> = "public-test-".encode_utf16().collect();
                    id.extend_from_slice(name.units());
                    let call = crate::host::modules::capability::ToolCall {
                        id: JsString::from_utf16(id),
                        name: name.clone(),
                        input: arguments.clone(),
                    };
                    let context = provider
                        .context
                        .prepare_ordered(call.clone(), principal, signal.clone())
                        .await
                        .map_err(native_error)?;
                    let mut results = provider
                        .tools
                        .execute_all(vec![call], context)
                        .await
                        .map_err(native_error)?;
                    if results.len() != 1 {
                        return Err(TransportError::host_response(
                            "Capability port returned an incomplete result.",
                        ));
                    }
                    JsValue::Object(results.remove(0))
                }
                _ => {
                    return Ok(CapabilityResponse::json(
                        StatusCode::NOT_IMPLEMENTED,
                        json!({ "code": "PUBLIC_HOST_CAPABILITY_UNAVAILABLE", "operation": operation, "message": "No equivalent configured PilotDeck runtime capability is available." }),
                    ));
                }
            };
            Ok(CapabilityResponse {
                status: StatusCode::OK.as_u16(),
                headers: HeaderMap::new(),
                body: CapabilityBody::OrderedJson(body),
            })
        })
    }
    fn call_ordered(
        &self,
        operation: String,
        input: Value,
        principal: Value,
        signal: Cancellation,
        original: Option<pilotdeck_protocol::JsValue>,
    ) -> TransportFuture<CapabilityResponse> {
        let input = original.unwrap_or_else(|| {
            pilotdeck_protocol::parse_json(&input.to_string()).expect("serde JSON")
        });
        let principal = pilotdeck_protocol::parse_json(&principal.to_string()).expect("serde JSON");
        self.call_exact(operation, input, principal, signal)
    }
    fn call(
        &self,
        operation: String,
        input: Value,
        principal: Value,
        signal: Cancellation,
    ) -> TransportFuture<CapabilityResponse> {
        self.call_ordered(operation, input, principal, signal, None)
    }
}
#[cfg(test)]
fn js_json(value: &pilotdeck_protocol::JsValue) -> Result<Value, TransportError> {
    let raw = pilotdeck_protocol::stringify_json(value)
        .map_err(|_| TransportError::host_response("Invalid native JSON result"))?
        .unwrap_or_else(|| "null".into());
    serde_json::from_str(&raw)
        .map_err(|_| TransportError::host_response("Invalid native JSON result"))
}
fn native_error(error: crate::host::HostError) -> TransportError {
    TransportError {
        status: Some(StatusCode::BAD_GATEWAY),
        code: Some("PUBLIC_HOST_PROVIDER_ERROR".into()),
        message: Some(error.message),
        ordered_code: None,
        ordered_message: None,
    }
}

/// Original authority and session/store ports; no parallel ACL or approval state.
pub trait ApprovalAuthority: Send + Sync {
    fn resolve_binding(&self, _input: Value) -> Option<TransportFuture<ApprovalBinding>> {
        None
    }
    fn authenticate(&self, bearer: String, signal: Cancellation) -> TransportFuture<Value>;
    fn resolve_session(&self, input: Value, signal: Cancellation)
    -> TransportFuture<Option<Value>>;
    fn status(&self, input: Value) -> TransportFuture<Option<Value>>;
    /// Must authorize and replay receipts atomically in the original locked store.
    fn resume(&self, input: Value) -> TransportFuture<Value>;
    /// Exact JSON ports retain JavaScript strings and property order. Existing
    /// serde ports remain usable through these compatibility adapters.
    fn resolve_binding_ordered(
        &self,
        input: pilotdeck_protocol::JsValue,
    ) -> Option<TransportFuture<ApprovalBinding>> {
        self.resolve_binding(js_json_lossy(&input))
    }
    fn authenticate_ordered(
        &self,
        bearer: String,
        signal: Cancellation,
    ) -> TransportFuture<pilotdeck_protocol::JsValue> {
        let result = self.authenticate(bearer, signal);
        Box::pin(async move { Ok(json_value(&result.await?)) })
    }
    fn resolve_session_ordered(
        &self,
        input: pilotdeck_protocol::JsValue,
        signal: Cancellation,
    ) -> TransportFuture<Option<pilotdeck_protocol::JsValue>> {
        let result = self.resolve_session(js_json_lossy(&input), signal);
        Box::pin(async move { Ok(result.await?.as_ref().map(json_value)) })
    }
    fn status_ordered(
        &self,
        input: pilotdeck_protocol::JsValue,
    ) -> TransportFuture<Option<pilotdeck_protocol::JsValue>> {
        let result = self.status(js_json_lossy(&input));
        Box::pin(async move { Ok(result.await?.as_ref().map(json_value)) })
    }
    fn resume_ordered(
        &self,
        input: pilotdeck_protocol::JsValue,
    ) -> TransportFuture<pilotdeck_protocol::JsValue> {
        let result = self.resume(js_json_lossy(&input));
        Box::pin(async move { Ok(json_value(&result.await?)) })
    }
}
#[derive(Clone)]
pub struct NativeApprovalBridge {
    pub binding: ApprovalBinding,
    pub dynamic_binding: bool,
    pub authority: Arc<dyn ApprovalAuthority>,
}
impl NativeApprovalBridge {
    async fn access(
        &self,
        input: &pilotdeck_protocol::JsValue,
        signal: Cancellation,
    ) -> Result<(pilotdeck_protocol::JsValue, pilotdeck_protocol::JsValue), TransportError> {
        use pilotdeck_protocol::JsValue;
        let binding = if self.dynamic_binding {
            self.authority
                .resolve_binding_ordered(input.clone())
                .ok_or_else(|| {
                    TransportError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "APPROVAL_BINDING_UNAVAILABLE",
                    )
                })?
                .await?
        } else {
            self.binding.clone()
        };
        if !approval_nonempty(approval_field(input, "sessionKey")) {
            return Err(TransportError::new(
                StatusCode::BAD_REQUEST,
                "APPROVAL_INPUT_INVALID",
            ));
        }
        if input.get("authority").is_some() || input.get("subject").is_some() {
            return Err(TransportError::new(
                StatusCode::BAD_REQUEST,
                "APPROVAL_AUTHORITY_OVERRIDE",
            ));
        }
        if input
            .get("tenantId")
            .is_some_and(|v| v != &JsValue::from(binding.tenant_id.as_str()))
            || input
                .get("agentId")
                .is_some_and(|v| v != &JsValue::from(binding.agent_id.as_str()))
        {
            return Err(TransportError::new(
                StatusCode::FORBIDDEN,
                "APPROVAL_SUBJECT_MISMATCH",
            ));
        }
        if [
            &binding.tenant_id,
            &binding.agent_id,
            &binding.pilot_deck_user_id,
        ]
        .iter()
        .any(|s| pilotdeck_protocol::js_value::js_trim(s).is_empty())
        {
            return Err(TransportError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "APPROVAL_BINDING_UNAVAILABLE",
            ));
        }
        let authorization = approval_field(input, "approverAuthorization")
            .as_string()
            .and_then(|value| value.to_utf8().ok())
            .unwrap_or_default();
        let bearer = authorization
            .get(..7)
            .filter(|s| s.eq_ignore_ascii_case("bearer "))
            .and_then(|_| authorization.get(7..))
            .filter(|s| !pilotdeck_protocol::js_value::js_trim(s).is_empty())
            .ok_or_else(|| {
                TransportError::new(StatusCode::UNAUTHORIZED, "APPROVAL_AUTH_REQUIRED")
            })?;
        let subject = self
            .authority
            .authenticate_ordered(bearer.into(), signal.clone())
            .await?;
        if approval_field(&subject, "tenantId") != &JsValue::from(binding.tenant_id.as_str())
            || approval_field(&subject, "source") != &JsValue::from("web")
            || approval_truthy(approval_field(&subject, "disabled"))
            || !approval_nonempty(approval_field(&subject, "userId"))
            || !approval_field(&subject, "role")
                .as_string()
                .is_some_and(|role| role.equals_str("admin") || role.equals_str("member"))
        {
            return Err(TransportError::new(
                StatusCode::FORBIDDEN,
                "APPROVAL_SUBJECT_MISMATCH",
            ));
        }
        let mut request = approval_object([
            ("sessionKey", approval_field(input, "sessionKey").clone()),
            (
                "binding",
                JsValue::Object(approval_object([
                    ("tenantId", binding.tenant_id.as_str().into()),
                    ("agentId", binding.agent_id.as_str().into()),
                    (
                        "pilotDeckUserId",
                        binding.pilot_deck_user_id.as_str().into(),
                    ),
                ])),
            ),
            ("subject", subject.clone()),
        ]);
        if let Some(project) = input.get("projectKey") {
            request.insert("projectKey".into(), project.clone());
        }
        let session = self
            .authority
            .resolve_session_ordered(JsValue::Object(request), signal.clone())
            .await?
            .ok_or_else(|| {
                TransportError::new(StatusCode::FORBIDDEN, "SOP_APPROVAL_SESSION_FORBIDDEN")
            })?;
        if approval_field(&session, "sessionKey") != approval_field(input, "sessionKey")
            || approval_field(&session, "tenantId") != &JsValue::from(binding.tenant_id.as_str())
            || approval_field(&session, "agentId") != &JsValue::from(binding.agent_id.as_str())
            || approval_field(&session, "pilotDeckUserId")
                != &JsValue::from(binding.pilot_deck_user_id.as_str())
            || input
                .get("projectKey")
                .is_some_and(|project| Some(project) != session.get("projectKey"))
        {
            return Err(TransportError::new(
                StatusCode::FORBIDDEN,
                "SOP_APPROVAL_SESSION_FORBIDDEN",
            ));
        }
        if signal.is_cancelled() {
            return Err(TransportError::new(
                StatusCode::BAD_GATEWAY,
                "PUBLIC_APPROVAL_BRIDGE_FAILED",
            ));
        }
        Ok((session, subject))
    }
}
static APPROVAL_UNDEFINED: pilotdeck_protocol::JsValue = pilotdeck_protocol::JsValue::Undefined;
fn approval_field<'a>(
    value: &'a pilotdeck_protocol::JsValue,
    name: &str,
) -> &'a pilotdeck_protocol::JsValue {
    value.get(name).unwrap_or(&APPROVAL_UNDEFINED)
}
fn approval_nonempty(value: &pilotdeck_protocol::JsValue) -> bool {
    value.as_string().is_some_and(|value| {
        !pilotdeck_protocol::js_value::js_trim(&String::from_utf16_lossy(value.units())).is_empty()
    })
}
fn approval_truthy(value: &pilotdeck_protocol::JsValue) -> bool {
    use pilotdeck_protocol::JsValue;
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(v) => *v,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.units().is_empty(),
        _ => true,
    }
}
fn approval_object<const N: usize>(
    fields: [(&str, pilotdeck_protocol::JsValue); N],
) -> pilotdeck_protocol::JsObject {
    let mut object = pilotdeck_protocol::JsObject::new();
    for (key, value) in fields {
        object.insert(key.into(), value);
    }
    object
}
fn approval_session_input(session: &pilotdeck_protocol::JsValue) -> pilotdeck_protocol::JsObject {
    let mut input =
        approval_object([("sessionKey", approval_field(session, "sessionKey").clone())]);
    if let Some(project) = session.get("projectKey") {
        input.insert("projectKey".into(), project.clone());
    }
    input
}
impl ApprovalBridge for NativeApprovalBridge {
    fn binding(&self) -> ApprovalBinding {
        self.binding.clone()
    }
    fn dynamic_binding(&self) -> bool {
        self.dynamic_binding
    }
    fn status_ordered(
        &self,
        input: pilotdeck_protocol::JsValue,
        signal: Cancellation,
    ) -> TransportFuture<pilotdeck_protocol::JsValue> {
        use pilotdeck_protocol::JsValue;
        let bridge = self.clone();
        Box::pin(async move {
            let (session, subject) = bridge.access(&input, signal).await?;
            let snapshot = bridge
                .authority
                .status_ordered(JsValue::Object(approval_session_input(&session)))
                .await?
                .unwrap_or(JsValue::Null);
            if snapshot
                .get("sessionId")
                .is_some_and(|id| Some(id) != session.get("sessionKey"))
            {
                return Err(TransportError::new(
                    StatusCode::BAD_GATEWAY,
                    "SOP_APPROVAL_SESSION_MISMATCH",
                ));
            }
            if approval_truthy(approval_field(&snapshot, "approval"))
                && approval_field(&subject, "role") != &JsValue::from("admin")
                && approval_field(approval_field(&snapshot, "approval"), "assigneeUserId")
                    != approval_field(&subject, "userId")
            {
                return Err(TransportError::new(
                    StatusCode::FORBIDDEN,
                    "SOP_APPROVAL_FORBIDDEN",
                ));
            }
            Ok(snapshot)
        })
    }
    fn resume_ordered(
        &self,
        input: pilotdeck_protocol::JsValue,
        signal: Cancellation,
    ) -> TransportFuture<pilotdeck_protocol::JsValue> {
        use pilotdeck_protocol::JsValue;
        let bridge = self.clone();
        Box::pin(async move {
            let revision = approval_field(&input, "expectedRevision").as_number();
            if approval_field(&input, "source") != &JsValue::from("human")
                || !["waitId", "requestId", "message"]
                    .iter()
                    .all(|key| approval_nonempty(approval_field(&input, key)))
                || revision.is_none_or(|n| {
                    !n.is_finite() || n < 0.0 || n.fract() != 0.0 || n > 9_007_199_254_740_991.0
                })
            {
                return Err(TransportError::new(
                    StatusCode::BAD_REQUEST,
                    "APPROVAL_INPUT_INVALID",
                ));
            }
            let (session, subject) = bridge.access(&input, signal).await?;
            let mut request = approval_session_input(&session);
            for field in [
                "source",
                "requestId",
                "waitId",
                "message",
                "expectedRevision",
            ] {
                request.insert(field.into(), approval_field(&input, field).clone());
            }
            if input.get("slotUpdates").is_some_and(approval_truthy) {
                request.insert(
                    "slotUpdates".into(),
                    approval_field(&input, "slotUpdates").clone(),
                );
            }
            request.insert(
                "authority".into(),
                JsValue::Object(approval_object([
                    ("tenantId", approval_field(&session, "tenantId").clone()),
                    ("sessionId", approval_field(&session, "sessionKey").clone()),
                    ("subject", subject),
                ])),
            );
            // Receipt replay is authorized by the store, without a pending-status preflight.
            bridge
                .authority
                .resume_ordered(JsValue::Object(request))
                .await
        })
    }
    fn status(&self, input: Value, signal: Cancellation) -> TransportFuture<Value> {
        let result = self.status_ordered(json_value(&input), signal);
        Box::pin(async move { Ok(js_json_lossy(&result.await?)) })
    }
    fn resume(&self, input: Value, signal: Cancellation) -> TransportFuture<Value> {
        let result = self.resume_ordered(json_value(&input), signal);
        Box::pin(async move { Ok(js_json_lossy(&result.await?)) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules::capability::{
        ToolCall, ToolCatalog, ToolDefinition, ToolExecution, ToolInvocationContext,
    };
    use pilotdeck_protocol::{JsObject, parse_json};
    use std::{
        collections::HashMap,
        sync::atomic::{AtomicUsize, Ordering},
    };

    fn principal() -> Value {
        json!({"pilotDeckUserId": "pd-user", "tenantId": "tenant", "actorUserId": "actor", "agentId": "agent"})
    }
    fn host_input(operation: &str, input: Value) -> Value {
        json!({ "principal": principal(), "operation": operation, "input": input })
    }
    fn headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            hyper::header::HeaderValue::from_static("Bearer test-gateway"),
        );
        headers.insert(
            "x-staffdeck-approver-authorization",
            hyper::header::HeaderValue::from_static("Bearer test-approver"),
        );
        headers
    }
    async fn response_json(response: Response<Body>) -> Value {
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }
    fn assert_error<T>(result: Result<T, TransportError>, status: StatusCode, code: &str) {
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("expected {code}"),
        };
        assert_eq!(error.status, Some(status));
        assert_eq!(error.code.as_deref(), Some(code));
    }

    #[derive(Clone)]
    struct TestProvider {
        operations: Vec<String>,
        calls: Arc<Mutex<Vec<Value>>>,
        response: Arc<Mutex<Option<CapabilityResponse>>>,
        signal: Arc<Mutex<Option<Cancellation>>>,
    }
    impl TestProvider {
        fn new(response: CapabilityResponse) -> Self {
            Self {
                operations: vec![
                    "list_tools".into(),
                    "model_stream".into(),
                    "not_public".into(),
                ],
                calls: Arc::default(),
                response: Arc::new(Mutex::new(Some(response))),
                signal: Arc::default(),
            }
        }
    }
    impl ProviderResolver for TestProvider {
        fn resolve(
            &self,
            _principal: Value,
            _signal: Cancellation,
        ) -> TransportFuture<Arc<dyn CapabilityProvider>> {
            let provider: Arc<dyn CapabilityProvider> = Arc::new(self.clone());
            Box::pin(async move { Ok(provider) })
        }
    }
    impl CapabilityProvider for TestProvider {
        fn operations(&self) -> Vec<String> {
            self.operations.clone()
        }
        fn call(
            &self,
            operation: String,
            input: Value,
            principal: Value,
            signal: Cancellation,
        ) -> TransportFuture<CapabilityResponse> {
            self.calls
                .lock()
                .unwrap()
                .push(json!({"operation": operation, "input": input, "principal": principal}));
            *self.signal.lock().unwrap() = Some(signal);
            let response = self
                .response
                .lock()
                .unwrap()
                .take()
                .expect("one provider call");
            Box::pin(async move { Ok(response) })
        }
    }

    #[tokio::test]
    async fn preflight_checks_all_routes_before_unavailable_bindings() {
        for path in [
            "/api/module-host/describe",
            "/api/module-host/call",
            "/api/module-host/approvals/status",
            "/api/module-host/approvals/resume",
        ] {
            let absent = Bindings::default();
            let response = preflight(
                path,
                &Method::GET,
                &HeaderMap::new(),
                "test-gateway",
                &absent,
            )
            .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                response_json(response).await["code"],
                "PUBLIC_HOST_TRANSPORT_UNAUTHORIZED"
            );
            let response =
                preflight(path, &Method::GET, &headers(), "test-gateway", &absent).unwrap();
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
            let response =
                preflight(path, &Method::POST, &headers(), "test-gateway", &absent).unwrap();
            let approval = path.contains("approvals/");
            assert_eq!(
                response.status(),
                if approval {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::NOT_IMPLEMENTED
                }
            );
            assert_eq!(
                response_json(response).await["code"],
                if approval {
                    "APPROVAL_BINDING_UNAVAILABLE"
                } else {
                    "PUBLIC_HOST_PORT_UNAVAILABLE"
                }
            );
        }
        for value in [
            "bearer test-gateway",
            "Bearer wrong",
            "Bearer  test-gateway",
        ] {
            let mut input = headers();
            input.insert(header::AUTHORIZATION, value.parse().unwrap());
            assert_eq!(
                preflight(
                    "/api/module-host/call",
                    &Method::POST,
                    &input,
                    "test-gateway",
                    &Bindings::default()
                )
                .unwrap()
                .status(),
                StatusCode::UNAUTHORIZED
            );
        }
        let mut duplicate = headers();
        duplicate.append(
            header::AUTHORIZATION,
            hyper::header::HeaderValue::from_static("Bearer test-gateway"),
        );
        assert_eq!(
            preflight(
                "/api/module-host/call",
                &Method::POST,
                &duplicate,
                "test-gateway",
                &Bindings::default()
            )
            .unwrap()
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn configured_provider_describes_filters_and_executes() {
        let provider = TestProvider::new(CapabilityResponse::json(
            StatusCode::ACCEPTED,
            json!({"data": [1]}),
        ));
        let response = host_request(
            "/api/module-host/describe",
            host_input("ignored", Value::Null),
            &provider,
            Cancellation::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            response_json(response).await,
            json!({"operations": ["list_tools", "model_stream"]})
        );
        assert!(provider.calls.lock().unwrap().is_empty());
        let response = host_request(
            "/api/module-host/call",
            host_input("list_tools", json!({"query": "q"})),
            &provider,
            Cancellation::new(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(response_json(response).await, json!({"data": [1]}));
        assert_eq!(
            provider.calls.lock().unwrap()[0],
            json!({"operation": "list_tools", "input": {"query": "q"}, "principal": principal()})
        );
    }

    #[tokio::test]
    async fn principal_operation_and_scope_boundaries_do_not_execute() {
        let provider = TestProvider::new(CapabilityResponse::json(StatusCode::OK, Value::Null));
        for field in ["pilotDeckUserId", "tenantId", "actorUserId", "agentId"] {
            for value in [Value::Null, json!(12), json!(" \u{feff} "), json!("")] {
                let mut input = host_input("list_tools", json!({}));
                input["principal"][field] = value;
                assert_error(
                    host_request(
                        "/api/module-host/call",
                        input,
                        &provider,
                        Cancellation::new(),
                    )
                    .await,
                    StatusCode::BAD_REQUEST,
                    "PUBLIC_HOST_PRINCIPAL_REQUIRED",
                );
            }
        }
        for operation in ["not_public", "task_status"] {
            assert_error(
                host_request(
                    "/api/module-host/call",
                    host_input(operation, Value::Null),
                    &provider,
                    Cancellation::new(),
                )
                .await,
                StatusCode::CONFLICT,
                "PILOTDECK_HOST_CAPABILITY_UNAVAILABLE",
            );
        }
        for payload in [Value::Null, json!([]), json!("input")] {
            assert_error(
                host_request(
                    "/api/module-host/call",
                    host_input("list_tools", payload),
                    &provider,
                    Cancellation::new(),
                )
                .await,
                StatusCode::BAD_REQUEST,
                "PUBLIC_HOST_INPUT_INVALID",
            );
        }
        for key in [
            "tenantId",
            "tenant_id",
            "actorUserId",
            "actor_user_id",
            "credentialId",
            "agentId",
            "agent_id",
        ] {
            let mut payload = json!({});
            payload[key] = Value::Null;
            assert_error(
                host_request(
                    "/api/module-host/call",
                    host_input("list_tools", payload),
                    &provider,
                    Cancellation::new(),
                )
                .await,
                StatusCode::BAD_REQUEST,
                "PUBLIC_SCOPE_OVERRIDE",
            );
        }
        assert!(provider.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn response_status_headers_and_body_formats_are_preserved() {
        for status in [0, 99, 600, 999] {
            let result = capability_response(
                CapabilityResponse {
                    status,
                    headers: HeaderMap::new(),
                    body: CapabilityBody::Json(Value::Null),
                },
                Cancellation::new(),
            );
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("invalid status passed"),
            };
            assert_eq!(error.status, Some(StatusCode::BAD_GATEWAY));
        }
        let mut input_headers = HeaderMap::new();
        for name in HEADERS {
            input_headers.insert(*name, hyper::header::HeaderValue::from_static("safe"));
        }
        for name in ["authorization", "set-cookie", "connection", "x-private"] {
            input_headers.insert(name, hyper::header::HeaderValue::from_static("discard"));
        }
        let response = capability_response(
            CapabilityResponse {
                status: 207,
                headers: input_headers,
                body: CapabilityBody::Bytes(Bytes::from_static(b"raw")),
            },
            Cancellation::new(),
        )
        .unwrap();
        assert_eq!(response.status().as_u16(), 207);
        assert_eq!(response.headers().len(), HEADERS.len());
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "raw"
        );
        for (body, content_type, expected) in [
            (
                CapabilityBody::Json(json!({"ok": true})),
                None,
                "{\"ok\":true}",
            ),
            (CapabilityBody::Text("text".into()), None, "\"text\""),
            (
                CapabilityBody::Text("text".into()),
                Some("application/json"),
                "\"text\"",
            ),
            (
                CapabilityBody::Text("text".into()),
                Some("text/plain"),
                "text",
            ),
            (
                CapabilityBody::Raw("{  \"raw\": 1 }".into()),
                Some("application/json"),
                "{  \"raw\": 1 }",
            ),
        ] {
            let mut headers = HeaderMap::new();
            if let Some(value) = content_type {
                headers.insert(header::CONTENT_TYPE, value.parse().unwrap());
            }
            let response = capability_response(
                CapabilityResponse {
                    status: 200,
                    headers,
                    body,
                },
                Cancellation::new(),
            )
            .unwrap();
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                expected
            );
        }
    }

    fn stream_response(
        receiver: tokio::sync::mpsc::Receiver<Result<StreamChunk, TransportError>>,
        content_type: Option<&str>,
    ) -> CapabilityResponse {
        let mut headers = HeaderMap::new();
        if let Some(value) = content_type {
            headers.insert(header::CONTENT_TYPE, value.parse().unwrap());
        }
        CapabilityResponse {
            status: 200,
            headers,
            body: CapabilityBody::Stream(Box::new(receiver)),
        }
    }
    #[tokio::test]
    async fn stream_is_pulled_once_and_drop_cancels_provider() {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        sender
            .try_send(Ok(StreamChunk::Json(json!({"event": 1}))))
            .unwrap();
        assert!(
            sender
                .try_send(Ok(StreamChunk::Text("later\n".into())))
                .is_err()
        );
        let provider = TestProvider::new(stream_response(
            receiver,
            Some("application/x-ndjson; charset=utf-8"),
        ));
        let signal = Cancellation::new();
        let response = host_request(
            "/api/module-host/call",
            host_input("model_stream", json!({})),
            &provider,
            signal.clone(),
        )
        .await
        .unwrap();
        assert!(!signal.is_cancelled());
        let mut body = response.into_body();
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            "{\"event\":1}\n"
        );
        sender
            .try_send(Ok(StreamChunk::Bytes(Bytes::from_static(b"already\n"))))
            .unwrap();
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            "already\n"
        );
        assert!(!signal.is_cancelled());
        drop(body);
        assert!(signal.is_cancelled());
        assert!(
            provider
                .signal
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .is_cancelled()
        );
        assert!(sender.is_closed());
    }
    #[tokio::test]
    async fn sse_bytes_stay_intact_and_stream_errors_terminate() {
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        sender
            .send(Ok(StreamChunk::Text("event: token\ndata: text\n\n".into())))
            .await
            .unwrap();
        sender
            .send(Err(TransportError::host_response("bad source")))
            .await
            .unwrap();
        let signal = Cancellation::new();
        let mut body = capability_response(
            stream_response(receiver, Some("text/event-stream")),
            signal.clone(),
        )
        .unwrap()
        .into_body();
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            "event: token\ndata: text\n\n"
        );
        assert!(body.frame().await.unwrap().is_err());
        assert!(signal.is_cancelled());
        assert!(body.frame().await.is_none());
        for value in [Value::Null, json!([]), json!(1), json!("text")] {
            assert!(encode_chunk(StreamChunk::Json(value), true).is_err());
        }
        assert!(encode_chunk(StreamChunk::Json(json!({"data": 1})), false).is_err());
        let (_sender, receiver) = tokio::sync::mpsc::channel(1);
        assert_error(
            capability_response(stream_response(receiver, None), Cancellation::new()),
            StatusCode::BAD_GATEWAY,
            "PILOTDECK_HOST_RESPONSE_INVALID",
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            hyper::header::HeaderValue::from_static("application/x-ndjson"),
        );
        assert_error(
            capability_response(
                CapabilityResponse {
                    status: 200,
                    headers,
                    body: CapabilityBody::Json(json!({})),
                },
                Cancellation::new(),
            ),
            StatusCode::BAD_GATEWAY,
            "PILOTDECK_HOST_RESPONSE_INVALID",
        );
    }

    struct Definition(JsObject);
    impl ToolDefinition for Definition {
        fn fields(&self) -> &JsObject {
            &self.0
        }
    }
    #[derive(Clone, Default)]
    struct NativeTools(Arc<Mutex<Vec<Value>>>);
    impl ToolCatalog for NativeTools {
        fn list(&self) -> crate::host::HostFuture<Vec<Arc<dyn ToolDefinition>>> {
            let definition: Arc<dyn ToolDefinition> = Arc::new(Definition(parse_json(r#"{"name":"echo","title":"Echo","inputSchema":{"type":"object"},"outputSchema":{"type":"string"},"alwaysLoad":true}"#).unwrap().as_object().unwrap().clone()));
            Box::pin(async move { Ok(vec![definition]) })
        }
    }
    impl ToolExecution for NativeTools {
        fn execute_all(
            &self,
            calls: Vec<ToolCall>,
            _context: ToolInvocationContext,
        ) -> crate::host::HostFuture<Vec<JsObject>> {
            let value = js_json(&calls[0].input).unwrap();
            self.0.lock().unwrap().push(value);
            Box::pin(async {
                Ok(vec![
                    parse_json(r#"{"ok":true}"#)
                        .unwrap()
                        .as_object()
                        .unwrap()
                        .clone(),
                ])
            })
        }
    }
    #[derive(Default)]
    struct NativeContext(Mutex<Vec<Value>>);
    impl PublicToolContext for NativeContext {
        fn prepare(
            &self,
            call: ToolCall,
            principal: Value,
            signal: Cancellation,
        ) -> crate::host::HostFuture<ToolInvocationContext> {
            assert!(!signal.is_cancelled());
            self.0
                .lock()
                .unwrap()
                .push(json!({"id": call.id.to_utf8().unwrap(), "principal": principal}));
            Box::pin(async {
                Ok(ToolInvocationContext {
                    runtime: Arc::new(()),
                    execution: Arc::new(()),
                })
            })
        }
    }
    #[tokio::test]
    async fn native_provider_uses_existing_catalog_execution_and_context() {
        let tools = Arc::new(NativeTools::default());
        let context = Arc::new(NativeContext::default());
        let provider = NativeCapabilityProvider {
            tools: tools.clone(),
            context: context.clone(),
        };
        let response = host_request(
            "/api/module-host/call",
            host_input("list_tools", json!({})),
            &provider,
            Cancellation::new(),
        )
        .await
        .unwrap();
        let catalog = response_json(response).await;
        assert_eq!(
            catalog["data"][0]["outputSchema"],
            json!({"type": "string"})
        );
        assert_eq!(catalog["data"][0]["alwaysLoad"], true);
        let response = host_request(
            "/api/module-host/call",
            host_input(
                "test_tool",
                json!({"toolId": "echo", "body": {"text": "hello"}}),
            ),
            &provider,
            Cancellation::new(),
        )
        .await
        .unwrap();
        assert_eq!(response_json(response).await, json!({"ok": true}));
        assert_eq!(tools.0.lock().unwrap()[0], json!({"text": "hello"}));
        assert_eq!(
            context.0.lock().unwrap()[0],
            json!({"id": "public-test-echo", "principal": principal()})
        );
        let cancelled = Cancellation::new();
        cancelled.cancel();
        let response = provider
            .call("test_tool".into(), json!({}), principal(), cancelled)
            .await
            .unwrap();
        assert_eq!(response.status, 499);
        assert_eq!(tools.0.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn native_tool_http_preserves_surrogate_input_principal_and_result() {
        #[derive(Default)]
        struct ExactTools;
        impl ToolCatalog for ExactTools {
            fn list(&self) -> crate::host::HostFuture<Vec<Arc<dyn ToolDefinition>>> {
                Box::pin(async { Ok(vec![]) })
            }
        }
        impl ToolExecution for ExactTools {
            fn execute_all(
                &self,
                calls: Vec<ToolCall>,
                _: ToolInvocationContext,
            ) -> crate::host::HostFuture<Vec<JsObject>> {
                assert_eq!(calls[0].name.units(), &[b'e' as u16, 0xd800]);
                assert_eq!(
                    calls[0]
                        .input
                        .get("text")
                        .unwrap()
                        .as_string()
                        .unwrap()
                        .units(),
                    &[0xdfff]
                );
                Box::pin(async {
                    Ok(vec![
                        parse_json(r#"{"ok":true,"text":"\ud800"}"#)
                            .unwrap()
                            .as_object()
                            .unwrap()
                            .clone(),
                    ])
                })
            }
        }
        struct ExactContext;
        impl PublicToolContext for ExactContext {
            fn prepare(
                &self,
                _: ToolCall,
                _: Value,
                _: Cancellation,
            ) -> crate::host::HostFuture<ToolInvocationContext> {
                unreachable!("exact native context must be used")
            }
            fn prepare_ordered(
                &self,
                call: ToolCall,
                principal: pilotdeck_protocol::JsValue,
                signal: Cancellation,
            ) -> crate::host::HostFuture<ToolInvocationContext> {
                assert!(!signal.is_cancelled());
                assert_eq!(
                    principal
                        .get("agentId")
                        .unwrap()
                        .as_string()
                        .unwrap()
                        .units(),
                    &[0xd800]
                );
                assert_eq!(
                    call.input.get("text").unwrap().as_string().unwrap().units(),
                    &[0xdfff]
                );
                assert_eq!(&call.id.units()["public-test-".len()..], call.name.units());
                Box::pin(async {
                    Ok(ToolInvocationContext {
                        runtime: Arc::new(()),
                        execution: Arc::new(()),
                    })
                })
            }
        }
        let provider = Arc::new(NativeCapabilityProvider {
            tools: Arc::new(ExactTools),
            context: Arc::new(ExactContext),
        });
        let (status, bytes) = exchange_raw("/api/module-host/call", Bytes::from_static(br#"{"principal":{"pilotDeckUserId":"p","tenantId":"t","actorUserId":"u","agentId":"\ud800"},"operation":"test_tool","input":{"toolId":"e\ud800","input":{"text":"\udfff"}}}"#), Bindings { provider: Some(provider), approvals: None }).await;
        assert_eq!(status, StatusCode::OK);
        let value = parse_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
        assert_eq!(
            value.get("ok"),
            Some(&pilotdeck_protocol::JsValue::Bool(true))
        );
        assert_eq!(
            value.get("text").unwrap().as_string().unwrap().units(),
            &[0xd800]
        );
    }

    fn binding() -> ApprovalBinding {
        ApprovalBinding {
            tenant_id: "tenant".into(),
            agent_id: "agent".into(),
            pilot_deck_user_id: "pd-user".into(),
        }
    }
    fn approval_input() -> Value {
        json!({"tenantId": "tenant", "agentId": "agent", "sessionKey": "session", "projectKey": "project"})
    }
    fn resume_input() -> Value {
        let mut input = approval_input();
        for (key, value) in json!({"source": "human", "requestId": "request", "waitId": "wait", "expectedRevision": 1, "message": "approved", "slotUpdates": {}}).as_object().unwrap() { input[key] = value.clone(); }
        input
    }
    #[derive(Clone)]
    struct TestAuthority {
        subject: Value,
        session: Option<Value>,
        snapshot: Option<Value>,
        calls: Arc<Mutex<Vec<(String, Value)>>>,
        receipts: Arc<Mutex<HashMap<String, (Value, Value)>>>,
        effects: Arc<AtomicUsize>,
    }
    impl Default for TestAuthority {
        fn default() -> Self {
            Self {
                subject: json!({"tenantId": "tenant", "userId": "approver", "source": "web", "role": "member", "disabled": false}),
                session: Some(
                    json!({"tenantId": "tenant", "agentId": "agent", "pilotDeckUserId": "pd-user", "sessionKey": "session", "projectKey": "project"}),
                ),
                snapshot: Some(
                    json!({"sessionId": "session", "approval": {"assigneeUserId": "approver"}}),
                ),
                calls: Arc::default(),
                receipts: Arc::default(),
                effects: Arc::default(),
            }
        }
    }
    impl ApprovalAuthority for TestAuthority {
        fn resolve_binding(&self, _input: Value) -> Option<TransportFuture<ApprovalBinding>> {
            Some(Box::pin(async { Ok(binding()) }))
        }
        fn authenticate(&self, bearer: String, _signal: Cancellation) -> TransportFuture<Value> {
            assert_eq!(bearer, "test-approver");
            self.calls
                .lock()
                .unwrap()
                .push(("authenticate".into(), Value::Null));
            let subject = self.subject.clone();
            Box::pin(async move { Ok(subject) })
        }
        fn resolve_session(
            &self,
            input: Value,
            _signal: Cancellation,
        ) -> TransportFuture<Option<Value>> {
            self.calls
                .lock()
                .unwrap()
                .push(("resolve_session".into(), input));
            let session = self.session.clone();
            Box::pin(async move { Ok(session) })
        }
        fn status(&self, input: Value) -> TransportFuture<Option<Value>> {
            self.calls.lock().unwrap().push(("status".into(), input));
            let snapshot = self.snapshot.clone();
            Box::pin(async move { Ok(snapshot) })
        }
        fn resume(&self, input: Value) -> TransportFuture<Value> {
            self.calls
                .lock()
                .unwrap()
                .push(("resume".into(), input.clone()));
            let receipts = self.receipts.clone();
            let effects = self.effects.clone();
            Box::pin(async move {
                let mut receipts = receipts.lock().unwrap();
                let key = input["requestId"].as_str().unwrap().to_owned();
                if let Some((original, result)) = receipts.get(&key) {
                    if original != &input {
                        return Err(TransportError {
                            status: None,
                            code: Some("SOP_RESUME_REQUEST_CONFLICT".into()),
                            message: Some("private detail".into()),
                            ordered_code: None,
                            ordered_message: None,
                        });
                    }
                    return Ok(result.clone());
                }
                effects.fetch_add(1, Ordering::SeqCst);
                let result = json!({"ok": true, "requestId": key});
                receipts.insert(key, (input, result.clone()));
                Ok(result)
            })
        }
    }
    fn bridge(authority: TestAuthority) -> NativeApprovalBridge {
        NativeApprovalBridge {
            binding: binding(),
            dynamic_binding: false,
            authority: Arc::new(authority),
        }
    }

    #[tokio::test]
    async fn native_approval_ordered_authority_preserves_resume_and_status_utf16() {
        use pilotdeck_protocol::JsValue;
        #[derive(Clone, Default)]
        struct ExactAuthority(Arc<Mutex<Vec<(String, JsValue)>>>);
        impl ApprovalAuthority for ExactAuthority {
            fn authenticate(&self, _: String, _: Cancellation) -> TransportFuture<Value> {
                unreachable!("exact authentication output")
            }
            fn resolve_session(&self, _: Value, _: Cancellation) -> TransportFuture<Option<Value>> {
                unreachable!("exact session mapping")
            }
            fn status(&self, _: Value) -> TransportFuture<Option<Value>> {
                unreachable!("exact status")
            }
            fn resume(&self, _: Value) -> TransportFuture<Value> {
                unreachable!("exact receipt store")
            }
            fn resolve_binding_ordered(
                &self,
                input: JsValue,
            ) -> Option<TransportFuture<ApprovalBinding>> {
                assert_eq!(
                    approval_field(&input, "sessionKey")
                        .as_string()
                        .unwrap()
                        .units(),
                    &[0xd800]
                );
                self.0.lock().unwrap().push(("binding".into(), input));
                Some(Box::pin(async { Ok(binding()) }))
            }
            fn authenticate_ordered(
                &self,
                bearer: String,
                _: Cancellation,
            ) -> TransportFuture<JsValue> {
                assert_eq!(bearer, "test-approver");
                Box::pin(async {
                    Ok(parse_json(r#"{"tenantId":"tenant","userId":"\ud801","source":"web","role":"member","disabled":false}"#).unwrap())
                })
            }
            fn resolve_session_ordered(
                &self,
                input: JsValue,
                _: Cancellation,
            ) -> TransportFuture<Option<JsValue>> {
                assert_eq!(
                    approval_field(&input, "sessionKey")
                        .as_string()
                        .unwrap()
                        .units(),
                    &[0xd800]
                );
                assert_eq!(
                    approval_field(&input, "projectKey")
                        .as_string()
                        .unwrap()
                        .units(),
                    &[0xdc00]
                );
                assert_eq!(
                    approval_field(approval_field(&input, "subject"), "userId")
                        .as_string()
                        .unwrap()
                        .units(),
                    &[0xd801]
                );
                self.0.lock().unwrap().push(("session".into(), input));
                Box::pin(async {
                    Ok(Some(parse_json(r#"{"tenantId":"tenant","agentId":"agent","pilotDeckUserId":"pd-user","sessionKey":"\ud800","projectKey":"\udc00"}"#).unwrap()))
                })
            }
            fn status_ordered(&self, input: JsValue) -> TransportFuture<Option<JsValue>> {
                self.0.lock().unwrap().push(("status".into(), input));
                Box::pin(async {
                    Ok(Some(parse_json(r#"{"sessionId":"\ud800","approval":{"assigneeUserId":"\ud801"},"text":"\udfff"}"#).unwrap()))
                })
            }
            fn resume_ordered(&self, input: JsValue) -> TransportFuture<JsValue> {
                assert_eq!(
                    approval_field(&input, "message")
                        .as_string()
                        .unwrap()
                        .units(),
                    &[0xd802]
                );
                assert_eq!(
                    approval_field(&input, "requestId")
                        .as_string()
                        .unwrap()
                        .units(),
                    &[0xd803]
                );
                assert_eq!(
                    approval_field(&input, "waitId")
                        .as_string()
                        .unwrap()
                        .units(),
                    &[0xd804]
                );
                assert_eq!(
                    approval_field(approval_field(&input, "slotUpdates"), "text")
                        .as_string()
                        .unwrap()
                        .units(),
                    &[0xd805]
                );
                assert_eq!(
                    approval_field(
                        approval_field(approval_field(&input, "authority"), "subject"),
                        "userId"
                    )
                    .as_string()
                    .unwrap()
                    .units(),
                    &[0xd801]
                );
                assert!(input.get("approverAuthorization").is_none());
                self.0
                    .lock()
                    .unwrap()
                    .push(("resume".into(), input.clone()));
                Box::pin(async move { Ok(approval_field(&input, "slotUpdates").clone()) })
            }
        }
        let authority = ExactAuthority::default();
        let bridge = NativeApprovalBridge {
            binding: binding(),
            dynamic_binding: true,
            authority: Arc::new(authority.clone()),
        };
        let status = approval_request_exact(
            "/api/module-host/approvals/status",
            parse_json(r#"{"tenantId":"tenant","agentId":"agent","sessionKey":"\ud800","projectKey":"\udc00"}"#).unwrap(),
            &headers(), &bridge, Cancellation::new(),
        ).await.unwrap();
        let bytes = status.into_body().collect().await.unwrap().to_bytes();
        let status = parse_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
        assert_eq!(
            approval_field(approval_field(&status, "status"), "text")
                .as_string()
                .unwrap()
                .units(),
            &[0xdfff]
        );
        let result = approval_request_exact(
            "/api/module-host/approvals/resume",
            parse_json(r#"{"tenantId":"tenant","agentId":"agent","sessionKey":"\ud800","projectKey":"\udc00","source":"human","requestId":"\ud803","waitId":"\ud804","expectedRevision":1,"message":"\ud802","slotUpdates":{"text":"\ud805"}}"#).unwrap(),
            &headers(), &bridge, Cancellation::new(),
        ).await.unwrap();
        let bytes = result.into_body().collect().await.unwrap().to_bytes();
        let result = parse_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
        assert_eq!(
            approval_field(&result, "text").as_string().unwrap().units(),
            &[0xd805]
        );
        let calls = authority.0.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            [
                "binding", "session", "status", "binding", "session", "resume"
            ]
        );
    }

    #[tokio::test]
    async fn configured_approval_bridge_authenticates_maps_status_and_atomic_resume() {
        let authority = TestAuthority::default();
        let bridge = bridge(authority.clone());
        let response = approval_request(
            "/api/module-host/approvals/status",
            approval_input(),
            &headers(),
            &bridge,
            Cancellation::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            response_json(response).await["status"]["sessionId"],
            "session"
        );
        authority.calls.lock().unwrap().clear();
        let request_headers = headers();
        let (first, replay) = tokio::join!(
            approval_request(
                "/api/module-host/approvals/resume",
                resume_input(),
                &request_headers,
                &bridge,
                Cancellation::new()
            ),
            approval_request(
                "/api/module-host/approvals/resume",
                resume_input(),
                &request_headers,
                &bridge,
                Cancellation::new()
            ),
        );
        assert_eq!(
            response_json(first.unwrap()).await,
            response_json(replay.unwrap()).await
        );
        assert_eq!(authority.effects.load(Ordering::SeqCst), 1);
        {
            let calls = authority.calls.lock().unwrap();
            assert!(!calls.iter().any(|(name, _)| name == "status"));
            let resume = &calls.iter().find(|(name, _)| name == "resume").unwrap().1;
            assert_eq!(
                resume["authority"],
                json!({"tenantId": "tenant", "sessionId": "session", "subject": authority.subject})
            );
            assert!(resume.get("approverAuthorization").is_none());
            assert_eq!(resume["slotUpdates"], json!({}));
        }
        let mut changed = resume_input();
        changed["message"] = json!("changed");
        let error = match approval_request(
            "/api/module-host/approvals/resume",
            changed,
            &headers(),
            &bridge,
            Cancellation::new(),
        )
        .await
        {
            Err(error) => error,
            Ok(_) => panic!("conflicting receipt passed"),
        };
        let response = error_response(error, true);
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response_json(response).await,
            json!({"code": "SOP_RESUME_REQUEST_CONFLICT"})
        );
    }

    #[tokio::test]
    async fn approval_transport_forbids_body_authority_and_enforces_scope() {
        let authority = TestAuthority::default();
        let bridge = bridge(authority.clone());
        for key in [
            "authority",
            "subject",
            "approverAuthorization",
            "pilotDeckUserId",
            "unexpected",
            "source",
        ] {
            let mut input = approval_input();
            input[key] = Value::Null;
            assert_error(
                approval_request(
                    "/api/module-host/approvals/status",
                    input,
                    &headers(),
                    &bridge,
                    Cancellation::new(),
                )
                .await,
                StatusCode::BAD_REQUEST,
                "APPROVAL_AUTHORITY_OVERRIDE",
            );
        }
        for value in [Value::Null, json!([]), json!(true)] {
            assert_error(
                approval_request(
                    "/api/module-host/approvals/status",
                    value,
                    &headers(),
                    &bridge,
                    Cancellation::new(),
                )
                .await,
                StatusCode::BAD_REQUEST,
                "APPROVAL_INPUT_INVALID",
            );
        }
        for key in ["tenantId", "agentId"] {
            let mut input = approval_input();
            input[key] = json!("other");
            assert_error(
                approval_request(
                    "/api/module-host/approvals/status",
                    input,
                    &headers(),
                    &bridge,
                    Cancellation::new(),
                )
                .await,
                StatusCode::FORBIDDEN,
                "APPROVAL_SUBJECT_MISMATCH",
            );
        }
        assert!(authority.calls.lock().unwrap().is_empty());
        let mut missing_auth = headers();
        missing_auth.remove("x-staffdeck-approver-authorization");
        assert_error(
            approval_request(
                "/api/module-host/approvals/status",
                approval_input(),
                &missing_auth,
                &bridge,
                Cancellation::new(),
            )
            .await,
            StatusCode::UNAUTHORIZED,
            "APPROVAL_AUTH_REQUIRED",
        );
    }

    #[tokio::test]
    async fn native_approval_denies_wrong_subject_session_assignee_and_revision() {
        for (field, value) in [
            ("tenantId", json!("other")),
            ("source", json!("agent")),
            ("disabled", json!(true)),
            ("userId", json!(" ")),
            ("role", json!("guest")),
        ] {
            let mut authority = TestAuthority::default();
            authority.subject[field] = value;
            assert_error(
                approval_request(
                    "/api/module-host/approvals/status",
                    approval_input(),
                    &headers(),
                    &bridge(authority),
                    Cancellation::new(),
                )
                .await,
                StatusCode::FORBIDDEN,
                "APPROVAL_SUBJECT_MISMATCH",
            );
        }
        for field in [
            "sessionKey",
            "tenantId",
            "agentId",
            "pilotDeckUserId",
            "projectKey",
        ] {
            let mut authority = TestAuthority::default();
            authority.session.as_mut().unwrap()[field] = json!("other");
            assert_error(
                approval_request(
                    "/api/module-host/approvals/status",
                    approval_input(),
                    &headers(),
                    &bridge(authority),
                    Cancellation::new(),
                )
                .await,
                StatusCode::FORBIDDEN,
                "SOP_APPROVAL_SESSION_FORBIDDEN",
            );
        }
        let mut authority = TestAuthority::default();
        authority.snapshot.as_mut().unwrap()["sessionId"] = json!("other");
        assert_error(
            approval_request(
                "/api/module-host/approvals/status",
                approval_input(),
                &headers(),
                &bridge(authority),
                Cancellation::new(),
            )
            .await,
            StatusCode::BAD_GATEWAY,
            "SOP_APPROVAL_SESSION_MISMATCH",
        );
        let mut authority = TestAuthority::default();
        authority.snapshot.as_mut().unwrap()["approval"]["assigneeUserId"] = json!("other");
        assert_error(
            approval_request(
                "/api/module-host/approvals/status",
                approval_input(),
                &headers(),
                &bridge(authority.clone()),
                Cancellation::new(),
            )
            .await,
            StatusCode::FORBIDDEN,
            "SOP_APPROVAL_FORBIDDEN",
        );
        authority.subject["role"] = json!("admin");
        assert!(
            approval_request(
                "/api/module-host/approvals/status",
                approval_input(),
                &headers(),
                &bridge(authority),
                Cancellation::new()
            )
            .await
            .is_ok()
        );
        let authority = TestAuthority::default();
        let bridge = bridge(authority.clone());
        for value in [
            Value::Null,
            json!(-1),
            json!(1.5),
            json!(9_007_199_254_740_992u64),
            json!("1"),
        ] {
            let mut input = resume_input();
            input["expectedRevision"] = value;
            assert_error(
                approval_request(
                    "/api/module-host/approvals/resume",
                    input,
                    &headers(),
                    &bridge,
                    Cancellation::new(),
                )
                .await,
                StatusCode::BAD_REQUEST,
                "APPROVAL_INPUT_INVALID",
            );
        }
        for field in ["waitId", "requestId", "message", "source"] {
            let mut input = resume_input();
            input[field] = json!(" ");
            assert_error(
                approval_request(
                    "/api/module-host/approvals/resume",
                    input,
                    &headers(),
                    &bridge,
                    Cancellation::new(),
                )
                .await,
                StatusCode::BAD_REQUEST,
                "APPROVAL_INPUT_INVALID",
            );
        }
        assert_eq!(authority.effects.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn dynamic_binding_and_null_status_are_supported() {
        let authority = TestAuthority {
            snapshot: None,
            ..TestAuthority::default()
        };
        let mut bridge = bridge(authority);
        bridge.dynamic_binding = true;
        bridge.binding.tenant_id = "stale".into();
        let response = approval_request(
            "/api/module-host/approvals/status",
            approval_input(),
            &headers(),
            &bridge,
            Cancellation::new(),
        )
        .await
        .unwrap();
        assert_eq!(response_json(response).await, json!({"status": null}));
        let mut input = resume_input();
        input["expectedRevision"] = json!(1.0);
        assert!(
            approval_request(
                "/api/module-host/approvals/resume",
                input,
                &headers(),
                &bridge,
                Cancellation::new()
            )
            .await
            .is_ok()
        );
    }

    async fn exchange(path: &str, bytes: Bytes, bindings: Bindings) -> (StatusCode, Value) {
        let (status, bytes) = exchange_raw(path, bytes, bindings).await;
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    async fn exchange_raw(path: &str, bytes: Bytes, bindings: Bindings) -> (StatusCode, Bytes) {
        use hyper::service::service_fn;
        use hyper_util::rt::TokioIo;
        let mut state = super::super::HttpState::new(
            "test-gateway".into(),
            None,
            std::path::PathBuf::from("."),
        );
        state.module_host = bindings;
        let state = Arc::new(state);
        let (client, server) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            hyper::server::conn::http1::Builder::new()
                .serve_connection(
                    TokioIo::new(server),
                    service_fn(move |request| {
                        let state = state.clone();
                        async move { Ok::<_, Infallible>(handle(request, &state).await) }
                    }),
                )
                .await
        });
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
            .await
            .unwrap();
        let connection = tokio::spawn(connection);
        let request = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(header::AUTHORIZATION, "Bearer test-gateway")
            .header("x-staffdeck-approver-authorization", "Bearer test-approver")
            .body(Full::new(bytes))
            .unwrap();
        let response = sender.send_request(request).await.unwrap();
        let status = response.status();
        let value = response.into_body().collect().await.unwrap().to_bytes();
        drop(sender);
        connection.abort();
        server.abort();
        let _ = connection.await;
        let _ = server.await;
        (status, value)
    }
    #[tokio::test]
    async fn http_handler_reads_json_limits_and_dispatches_injected_ports() {
        let provider = TestProvider::new(CapabilityResponse::json(
            StatusCode::OK,
            json!({"native": true}),
        ));
        let bindings = Bindings {
            provider: Some(Arc::new(provider.clone())),
            approvals: Some(Arc::new(bridge(TestAuthority::default()))),
        };
        let (status, body) = exchange(
            "/api/module-host/call",
            Bytes::from(host_input("list_tools", json!({})).to_string()),
            bindings.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"native": true}));
        let (status, body) = exchange(
            "/api/module-host/approvals/status",
            Bytes::from(approval_input().to_string()),
            bindings.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"]["sessionId"], "session");
        for (path, limit, code) in [
            (
                "/api/module-host/call",
                32 * 1024 * 1024,
                "PUBLIC_HOST_INPUT_TOO_LARGE",
            ),
            (
                "/api/module-host/approvals/status",
                64 * 1024,
                "APPROVAL_INPUT_TOO_LARGE",
            ),
        ] {
            let (status, body) =
                exchange(path, Bytes::from(vec![b' '; limit + 1]), bindings.clone()).await;
            assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
            assert_eq!(body["code"], code);
            let (status, body) =
                exchange(path, Bytes::from_static(b"{broken"), bindings.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(
                body["code"],
                if path.contains("approvals/") {
                    "APPROVAL_INPUT_INVALID"
                } else {
                    "PUBLIC_HOST_INPUT_INVALID"
                }
            );
        }
        assert_eq!(provider.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn http_handler_accepts_json_parse_lone_surrogates_and_preserves_original_input() {
        #[derive(Clone, Default)]
        struct OrderedProvider(Arc<Mutex<Vec<pilotdeck_protocol::JsValue>>>);
        impl ProviderResolver for OrderedProvider {
            fn resolve(
                &self,
                _principal: Value,
                _signal: Cancellation,
            ) -> TransportFuture<Arc<dyn CapabilityProvider>> {
                let provider: Arc<dyn CapabilityProvider> = Arc::new(self.clone());
                Box::pin(async move { Ok(provider) })
            }
        }
        impl CapabilityProvider for OrderedProvider {
            fn operations(&self) -> Vec<String> {
                vec!["test_tool".into()]
            }
            fn call(
                &self,
                _operation: String,
                _input: Value,
                _principal: Value,
                _signal: Cancellation,
            ) -> TransportFuture<CapabilityResponse> {
                unreachable!("ordered call is required")
            }
            fn call_ordered(
                &self,
                _operation: String,
                input: Value,
                _principal: Value,
                _signal: Cancellation,
                original: Option<pilotdeck_protocol::JsValue>,
            ) -> TransportFuture<CapabilityResponse> {
                assert_eq!(input["text"], "�");
                self.0.lock().unwrap().push(original.unwrap());
                Box::pin(async {
                    Ok(CapabilityResponse::json(
                        StatusCode::OK,
                        json!({"accepted":true}),
                    ))
                })
            }
        }
        let provider = OrderedProvider::default();
        let bindings = Bindings {
            provider: Some(Arc::new(provider.clone())),
            approvals: None,
        };
        let raw = br#"{"principal":{"pilotDeckUserId":"pd-user","tenantId":"tenant","actorUserId":"actor","agentId":"agent"},"operation":"test_tool","input":{"text":"\ud800"}}"#;
        let (status, body) = exchange(
            "/api/module-host/call",
            Bytes::copy_from_slice(raw),
            bindings,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"accepted":true}));
        let originals = provider.0.lock().unwrap();
        assert_eq!(
            originals[0]
                .get("text")
                .unwrap()
                .as_string()
                .unwrap()
                .units(),
            [0xd800]
        );
    }

    #[tokio::test]
    async fn exact_http_ports_preserve_surrogate_principal_approval_and_output() {
        use pilotdeck_protocol::{JsValue, parse_json};
        #[derive(Clone)]
        struct ExactPort;
        impl ProviderResolver for ExactPort {
            fn resolve(
                &self,
                _: Value,
                _: Cancellation,
            ) -> TransportFuture<Arc<dyn CapabilityProvider>> {
                unreachable!("exact resolver")
            }
            fn resolve_ordered(
                &self,
                principal: JsValue,
                _: Cancellation,
            ) -> TransportFuture<Arc<dyn CapabilityProvider>> {
                assert_eq!(
                    principal
                        .get("agentId")
                        .unwrap()
                        .as_string()
                        .unwrap()
                        .units(),
                    &[0xd800]
                );
                Box::pin(async { Ok(Arc::new(ExactPort) as Arc<dyn CapabilityProvider>) })
            }
        }
        impl CapabilityProvider for ExactPort {
            fn operations(&self) -> Vec<String> {
                vec!["test_tool".into()]
            }
            fn call(
                &self,
                _: String,
                _: Value,
                _: Value,
                _: Cancellation,
            ) -> TransportFuture<CapabilityResponse> {
                unreachable!("exact provider")
            }
            fn call_exact(
                &self,
                _: String,
                input: JsValue,
                principal: JsValue,
                _: Cancellation,
            ) -> TransportFuture<CapabilityResponse> {
                assert_eq!(
                    principal
                        .get("agentId")
                        .unwrap()
                        .as_string()
                        .unwrap()
                        .units(),
                    &[0xd800]
                );
                Box::pin(async move {
                    Ok(CapabilityResponse {
                        status: 200,
                        headers: HeaderMap::new(),
                        body: CapabilityBody::OrderedJson(input),
                    })
                })
            }
        }
        impl ApprovalBridge for ExactPort {
            fn binding(&self) -> ApprovalBinding {
                ApprovalBinding {
                    tenant_id: "t".into(),
                    agent_id: "a".into(),
                    pilot_deck_user_id: "p".into(),
                }
            }
            fn status(&self, _: Value, _: Cancellation) -> TransportFuture<Value> {
                unreachable!("exact approval")
            }
            fn resume(&self, _: Value, _: Cancellation) -> TransportFuture<Value> {
                unreachable!("exact approval")
            }
            fn resume_ordered(&self, input: JsValue, _: Cancellation) -> TransportFuture<JsValue> {
                assert_eq!(
                    input.get("message").unwrap().as_string().unwrap().units(),
                    &[0xdfff]
                );
                Box::pin(async move { Ok(input.get("slotUpdates").unwrap().clone()) })
            }
        }
        let bindings = Bindings {
            provider: Some(Arc::new(ExactPort)),
            approvals: Some(Arc::new(ExactPort)),
        };
        let (status, bytes) = exchange_raw("/api/module-host/call", Bytes::from_static(br#"{"principal":{"pilotDeckUserId":"p","tenantId":"t","actorUserId":"u","agentId":"\ud800"},"operation":"test_tool","input":{"text":"\udfff"}}"#), bindings.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            parse_json(std::str::from_utf8(&bytes).unwrap())
                .unwrap()
                .get("text")
                .unwrap()
                .as_string()
                .unwrap()
                .units(),
            &[0xdfff]
        );
        let (status, bytes) = exchange_raw("/api/module-host/approvals/resume", Bytes::from_static(br#"{"tenantId":"t","agentId":"a","message":"\udfff","slotUpdates":{"key":"\ud800"}}"#), bindings).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            parse_json(std::str::from_utf8(&bytes).unwrap())
                .unwrap()
                .get("key")
                .unwrap()
                .as_string()
                .unwrap()
                .units(),
            &[0xd800]
        );
        let encoded = encode_chunk(
            StreamChunk::OrderedJson(parse_json(r#"{"text":"\udfff"}"#).unwrap()),
            true,
        )
        .unwrap();
        assert_eq!(
            &encoded[..],
            br#"{"text":"\udfff"}
"#
        );
    }

    #[tokio::test]
    async fn exact_http_errors_preserve_resolver_provider_and_approval_utf16() {
        use pilotdeck_protocol::{JsString, JsValue, parse_json};
        #[derive(Clone)]
        struct FailingPort;
        impl ProviderResolver for FailingPort {
            fn resolve(
                &self,
                _: Value,
                _: Cancellation,
            ) -> TransportFuture<Arc<dyn CapabilityProvider>> {
                unreachable!("exact resolver")
            }
            fn resolve_ordered(
                &self,
                principal: JsValue,
                _: Cancellation,
            ) -> TransportFuture<Arc<dyn CapabilityProvider>> {
                Box::pin(async move {
                    // Real fixed TS: resolver throw new Error(principal.agentId).
                    let message = principal.get("agentId").unwrap().as_string().unwrap();
                    if !message.equals_str("a") {
                        return Err(TransportError::ordered(None, None, Some(message.clone())));
                    }
                    Ok(Arc::new(FailingPort) as Arc<dyn CapabilityProvider>)
                })
            }
        }
        impl CapabilityProvider for FailingPort {
            fn operations(&self) -> Vec<String> {
                vec!["test_tool".into()]
            }
            fn call(
                &self,
                _: String,
                _: Value,
                _: Value,
                _: Cancellation,
            ) -> TransportFuture<CapabilityResponse> {
                Box::pin(async {
                    Err(TransportError::ordered(
                        Some(StatusCode::UNPROCESSABLE_ENTITY),
                        Some(JsString::from_utf16(vec![0xdfff])),
                        Some(JsString::from_utf16(vec![0xd800])),
                    ))
                })
            }
        }
        impl ApprovalBridge for FailingPort {
            fn binding(&self) -> ApprovalBinding {
                ApprovalBinding {
                    tenant_id: "t".into(),
                    agent_id: "a".into(),
                    pilot_deck_user_id: "p".into(),
                }
            }
            fn status(&self, _: Value, _: Cancellation) -> TransportFuture<Value> {
                Box::pin(async {
                    Err(TransportError::ordered(
                        None,
                        Some(JsString::from_utf16(vec![0xdfff])),
                        Some(JsString::from_utf16(vec![0xd800])),
                    ))
                })
            }
            fn resume(&self, _: Value, _: Cancellation) -> TransportFuture<Value> {
                Box::pin(async {
                    Err(TransportError::ordered(
                        None,
                        Some("SOP_RESUME_REQUEST_CONFLICT".into()),
                        Some(JsString::from_utf16(vec![0xd800])),
                    ))
                })
            }
        }
        let bindings = Bindings {
            provider: Some(Arc::new(FailingPort)),
            approvals: Some(Arc::new(FailingPort)),
        };
        let (status, bytes) = exchange_raw(
            "/api/module-host/describe",
            Bytes::from_static(br#"{"principal":{"pilotDeckUserId":"p","tenantId":"t","actorUserId":"u","agentId":"\ud800"}}"#),
            bindings.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        // Exact output recorded from unmodified publicHostHttpTransport.ts:66.
        assert_eq!(
            &bytes[..],
            br#"{"code":"PUBLIC_HOST_TRANSPORT_FAILED","message":"\ud800"}"#
        );
        let (status, bytes) = exchange_raw(
            "/api/module-host/call",
            Bytes::from_static(br#"{"principal":{"pilotDeckUserId":"p","tenantId":"t","actorUserId":"u","agentId":"a"},"operation":"test_tool","input":{}}"#),
            bindings.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(&bytes[..], br#"{"code":"\udfff","message":"\ud800"}"#);
        for (path, expected_status, expected) in [
            (
                "/api/module-host/approvals/status",
                StatusCode::BAD_GATEWAY,
                r#"{"code":"\udfff"}"#,
            ),
            (
                "/api/module-host/approvals/resume",
                StatusCode::CONFLICT,
                r#"{"code":"SOP_RESUME_REQUEST_CONFLICT"}"#,
            ),
        ] {
            let (status, bytes) = exchange_raw(
                path,
                Bytes::from_static(br#"{"tenantId":"t","agentId":"a"}"#),
                bindings.clone(),
            )
            .await;
            assert_eq!(status, expected_status);
            assert_eq!(
                parse_json(std::str::from_utf8(&bytes).unwrap()).unwrap(),
                parse_json(expected).unwrap()
            );
        }
        let response = error_response(TransportError::ordered(None, Some("".into()), None), false);
        assert_eq!(response_json(response).await, json!({"code":""}));
    }

    #[tokio::test]
    async fn ordered_text_response_writes_node_utf8_replacement_without_json_quotes() {
        let mut headers = HeaderMap::new();
        headers.insert(
            hyper::header::CONTENT_TYPE,
            hyper::header::HeaderValue::from_static("text/plain"),
        );
        let response = capability_response(
            CapabilityResponse {
                status: 200,
                headers,
                body: CapabilityBody::OrderedJson(parse_json(r#""a\ud800b""#).unwrap()),
            },
            Cancellation::new(),
        )
        .unwrap();
        use http_body_util::BodyExt;
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], "a\u{fffd}b".as_bytes());
    }
}

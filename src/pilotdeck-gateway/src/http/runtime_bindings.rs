//! Public transports reuse the native management and authenticated SOP owners.
use super::module_host::*;
use crate::{
    metadata::{Metadata, NativeFiles},
    runtime::network::Cancellation,
};
use hyper::StatusCode;
use pilotdeck_protocol::{JsValue, parse_json};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone)]
struct RuntimeProvider {
    metadata: Arc<Metadata<NativeFiles>>,
}

pub(crate) fn bindings(home: std::path::PathBuf, cwd: std::path::PathBuf) -> Bindings {
    let metadata = Arc::new(Metadata {
        home,
        cwd,
        files: NativeFiles,
    });
    Bindings {
        provider: Some(Arc::new(RuntimeProvider {
            metadata: metadata.clone(),
        })),
        approvals: Some(Arc::new(RuntimeApprovals { metadata })),
    }
}

fn wire(value: &JsValue) -> Result<Value, TransportError> {
    let raw = pilotdeck_protocol::stringify_json(value)
        .map_err(|_| failure("PUBLIC_HOST_RESPONSE_INVALID"))?
        .unwrap_or_else(|| "null".into());
    serde_json::from_str(&raw).map_err(|_| failure("PUBLIC_HOST_RESPONSE_INVALID"))
}
fn input(value: &Value) -> Result<JsValue, TransportError> {
    parse_json(&value.to_string()).map_err(|_| failure("PUBLIC_HOST_INPUT_INVALID"))
}
fn failure(code: &str) -> TransportError {
    TransportError::new(StatusCode::BAD_GATEWAY, code)
}
fn request_error(error: crate::runtime::connection::RequestError) -> TransportError {
    let status = error
        .details
        .as_deref()
        .and_then(|d| d.get("httpStatus"))
        .and_then(JsValue::as_number)
        .and_then(|n| StatusCode::from_u16(n as u16).ok())
        .unwrap_or(match error.code.as_str() {
            "APPROVAL_AUTH_REQUIRED" => StatusCode::UNAUTHORIZED,
            "APPROVAL_SUBJECT_MISMATCH"
            | "SOP_APPROVAL_SESSION_FORBIDDEN"
            | "SOP_APPROVAL_FORBIDDEN" => StatusCode::FORBIDDEN,
            "APPROVAL_INPUT_INVALID" | "APPROVAL_AUTHORITY_OVERRIDE" => StatusCode::BAD_REQUEST,
            "APPROVAL_BINDING_UNAVAILABLE" => StatusCode::SERVICE_UNAVAILABLE,
            "SOP_RESUME_REQUEST_CONFLICT" => StatusCode::CONFLICT,
            _ => StatusCode::BAD_GATEWAY,
        });
    TransportError::ordered(Some(status), Some(error.code.into()), Some(error.message))
}
impl ProviderResolver for RuntimeProvider {
    fn resolve(
        &self,
        _principal: Value,
        _signal: Cancellation,
    ) -> TransportFuture<Arc<dyn CapabilityProvider>> {
        let provider: Arc<dyn CapabilityProvider> = Arc::new(self.clone());
        Box::pin(async move {
            // Project-bound public callers require the original admission resolver.
            if std::env::var("STAFFDECK_PROJECT_BINDING_ENABLED").as_deref() == Ok("true") {
                return Err(TransportError::new(
                    StatusCode::FORBIDDEN,
                    "PROJECT_BINDING_PRINCIPAL_REQUIRED",
                ));
            }
            Ok(provider)
        })
    }
}
impl CapabilityProvider for RuntimeProvider {
    fn call_exact(
        &self,
        operation: String,
        payload: JsValue,
        principal: JsValue,
        signal: Cancellation,
    ) -> TransportFuture<CapabilityResponse> {
        if operation == "file_parse" {
            return Box::pin(async move {
                let mut response =
                    public_file(super::module_host::js_json_lossy(&payload), signal).await?;
                if let CapabilityBody::Json(body) = response.body {
                    let mut body = input(&body)?;
                    if let JsValue::Object(fields) = &mut body {
                        if let Some(filename) = payload.get("filename") {
                            fields.insert("filename".into(), filename.clone());
                        }
                        if let Some(media) = payload
                            .get("media_type")
                            .filter(|v| !matches!(v, JsValue::Null))
                        {
                            fields.insert("mediaType".into(), media.clone());
                        }
                    }
                    response.body = CapabilityBody::OrderedJson(body);
                }
                Ok(response)
            });
        }
        if operation == "read_sop_authority" {
            let metadata = self.metadata.clone();
            return Box::pin(async move {
                let result = crate::runtime::sop::authority(
                    &metadata.home,
                    &metadata.cwd,
                    &payload,
                    &principal,
                )
                .await
                .map_err(request_error)?;
                Ok(CapabilityResponse {
                    status: 200,
                    headers: Default::default(),
                    body: CapabilityBody::OrderedJson(result),
                })
            });
        }
        self.call_ordered(
            operation,
            super::module_host::js_json_lossy(&payload),
            super::module_host::js_json_lossy(&principal),
            signal,
            Some(payload),
        )
    }
    fn operations(&self) -> Vec<String> {
        vec![
            "list_general_skills".into(),
            "list_model_catalog".into(),
            "model_prepare".into(),
            "model_stream".into(),
            "file_parse".into(),
            "read_sop_authority".into(),
        ]
    }
    fn call(
        &self,
        operation: String,
        payload: Value,
        principal: Value,
        signal: Cancellation,
    ) -> TransportFuture<CapabilityResponse> {
        self.call_ordered(operation, payload, principal, signal, None)
    }
    fn call_ordered(
        &self,
        operation: String,
        payload: Value,
        principal: Value,
        signal: Cancellation,
        original: Option<JsValue>,
    ) -> TransportFuture<CapabilityResponse> {
        let metadata = self.metadata.clone();
        Box::pin(async move {
            if signal.is_cancelled() {
                return Err(failure("PUBLIC_HOST_CANCELLED"));
            }
            if matches!(operation.as_str(), "model_prepare" | "model_stream") {
                return public_model(metadata, operation, payload, signal, original).await;
            }
            if operation == "file_parse" {
                return public_file(payload, signal).await;
            }
            if operation == "read_sop_authority" {
                let result = crate::runtime::sop::authority(
                    &metadata.home,
                    &metadata.cwd,
                    &input(&payload)?,
                    &input(&principal)?,
                )
                .await
                .map_err(request_error)?;
                return Ok(CapabilityResponse::json(StatusCode::OK, wire(&result)?));
            }
            let params = crate::metadata::object([
                ("projectKey", metadata.cwd.to_string_lossy().as_ref().into()),
                ("includeAuto", JsValue::Bool(false)),
            ]);
            let result = match operation.as_str() {
                "list_general_skills" => {
                    crate::routes::domains::request(metadata, "skill_list".into(), params).await
                }
                "list_model_catalog" => {
                    crate::routes::native_metadata("model_catalog_list".into(), params, metadata)
                        .await
                }
                _ => {
                    return Err(TransportError::new(
                        StatusCode::CONFLICT,
                        "PILOTDECK_HOST_CAPABILITY_UNAVAILABLE",
                    ));
                }
            }
            .map_err(request_error)?;
            let mut result = wire(&result)?;
            let mut items = result["items"]
                .as_array()
                .cloned()
                .ok_or_else(|| failure("PUBLIC_HOST_RESPONSE_INVALID"))?;
            for item in &mut items {
                if operation == "list_general_skills" {
                    item["id"] = item["slug"].clone();
                } else {
                    item["name"] = item["displayName"].clone();
                    item["enabled"] = item["available"].clone();
                    item["is_default"] = json!(
                        result["defaultSelection"]["provider"] == item["provider"]
                            && result["defaultSelection"]["model"] == item["model"]
                    );
                }
            }
            if operation == "list_model_catalog" {
                return Ok(CapabilityResponse::json(
                    StatusCode::OK,
                    json!({"data": items, "defaultSelection": result["defaultSelection"]}),
                ));
            }
            result["items"] = json!(items);
            result["data"] = result["items"].clone();
            if operation == "list_general_skills" {
                result["next_cursor"] = result.get("nextCursor").cloned().unwrap_or(Value::Null);
            }
            Ok(CapabilityResponse::json(StatusCode::OK, result))
        })
    }
}

async fn public_file(
    payload: Value,
    signal: Cancellation,
) -> Result<CapabilityResponse, TransportError> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fail = |status, code| TransportError::new(status, code);
    let filename = payload["filename"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| fail(StatusCode::BAD_REQUEST, "PUBLIC_HOST_INPUT_INVALID"))?;
    let encoded = payload["content_base64"]
        .as_str()
        .ok_or_else(|| fail(StatusCode::BAD_REQUEST, "PUBLIC_HOST_INPUT_INVALID"))?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| fail(StatusCode::BAD_REQUEST, "PUBLIC_HOST_INPUT_INVALID"))?;
    if STANDARD.encode(&bytes) != encoded {
        return Err(fail(StatusCode::BAD_REQUEST, "PUBLIC_HOST_INPUT_INVALID"));
    }
    let extension = std::path::Path::new(filename)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !["txt", "md", "markdown", "pdf"].contains(&extension.as_str()) {
        return Err(fail(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "PUBLIC_FILE_TYPE_UNSUPPORTED",
        ));
    }
    let media = payload.get("media_type").filter(|v| !v.is_null());
    if media.is_some_and(|v| !v.is_string()) {
        return Err(fail(StatusCode::BAD_REQUEST, "PUBLIC_FILE_INPUT_INVALID"));
    }
    let limit = match payload.get("max_bytes") {
        None => 20 * 1024 * 1024,
        Some(v) => v
            .as_u64()
            .filter(|n| *n > 0 && *n <= 20 * 1024 * 1024)
            .ok_or_else(|| fail(StatusCode::BAD_REQUEST, "PUBLIC_FILE_INPUT_INVALID"))?,
    };
    if bytes.len() as u64 > limit {
        return Err(fail(StatusCode::PAYLOAD_TOO_LARGE, "PUBLIC_FILE_TOO_LARGE"));
    }
    let text = if extension == "pdf" {
        let parser = std::env::var_os("PILOTDECK_PDFTOTEXT").unwrap_or_else(|| "pdftotext".into());
        let mut child = tokio::process::Command::new(parser)
            .args(["-enc", "UTF-8", "-", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| fail(StatusCode::UNPROCESSABLE_ENTITY, "PUBLIC_FILE_PARSE_FAILED"))?;
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let parse = async {
            let write = async {
                stdin.write_all(&bytes).await?;
                stdin.shutdown().await?;
                drop(stdin);
                Ok::<_, std::io::Error>(())
            };
            let read = async {
                let mut output = Vec::new();
                stdout.read_to_end(&mut output).await?;
                Ok::<_, std::io::Error>(output)
            };
            let (_, output) = tokio::try_join!(write, read)?;
            let status = child.wait().await?;
            if !status.success() {
                return Err(std::io::Error::other("PDF parse failed"));
            }
            String::from_utf8(output)
                .map(pdf_page_text)
                .map_err(std::io::Error::other)
        };
        tokio::select! { _=signal.cancelled()=>return Err(fail(StatusCode::from_u16(499).unwrap(),"PUBLIC_HOST_CANCELLED")), result=parse=>result.map_err(|_|fail(StatusCode::UNPROCESSABLE_ENTITY,"PUBLIC_FILE_PARSE_FAILED"))? }
    } else {
        String::from_utf8(bytes.clone()).map_err(|_| {
            fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                "PUBLIC_FILE_ENCODING_UNSUPPORTED",
            )
        })?
    };
    if signal.is_cancelled() {
        return Err(fail(
            StatusCode::from_u16(499).unwrap(),
            "PUBLIC_HOST_CANCELLED",
        ));
    }
    Ok(CapabilityResponse::json(
        StatusCode::OK,
        json!({"filename":filename,"text":text,"mediaType":media.cloned().unwrap_or_else(||json!(if extension=="pdf"{"application/pdf"}else{"text/plain"})),"metadata":{"fileType":extension,"bytes":bytes.len()}}),
    ))
}

fn pdf_page_text(mut text: String) -> String {
    // Poppler terminates every page; TS joins page text with a single newline.
    if text.ends_with('\u{000c}') {
        text.pop();
    }
    text.replace('\u{000c}', "\n")
}
fn ordered_model_request(
    original: Option<&JsValue>,
    validated: &serde_json::Map<String, Value>,
) -> Result<JsValue, TransportError> {
    let mut request = original
        .and_then(|v| v.get("request"))
        .cloned()
        .unwrap_or(input(&Value::Object(validated.clone()))?);
    let JsValue::Object(ref mut fields) = request else {
        return Err(invalid_model(
            "invalid_request",
            "request must be an object",
        ));
    };
    for key in ["provider", "model", "maxOutputTokens"] {
        if let Some(value) = validated.get(key) {
            fields.insert(key.into(), input(value)?);
        }
    }
    Ok(request)
}
fn invalid_model(code: &str, message: &str) -> TransportError {
    TransportError {
        status: Some(StatusCode::BAD_REQUEST),
        code: Some(code.into()),
        message: Some(message.into()),
        ordered_code: None,
        ordered_message: None,
    }
}
fn model_error(error: crate::runtime::model::http::ModelStreamError) -> TransportError {
    use crate::runtime::model::http::ModelStreamError;
    let code = match &error {
        ModelStreamError::Request { code, .. } => code.clone(),
        _ => "PUBLIC_MODEL_PROVIDER_FAILED".into(),
    };
    TransportError::ordered(
        Some(if matches!(error, ModelStreamError::Request { .. }) {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::BAD_GATEWAY
        }),
        Some(code),
        Some(error.message()),
    )
}
async fn public_model(
    metadata: Arc<Metadata<NativeFiles>>,
    operation: String,
    payload: Value,
    signal: Cancellation,
    original: Option<JsValue>,
) -> Result<CapabilityResponse, TransportError> {
    use crate::runtime::model::{
        http::{self, HyperModelTransport, StreamOptions, StreamOutput},
        providers,
    };
    let request_id = payload["requestId"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| invalid_model("invalid_request", "requestId is required"))?;
    let mut request = payload["request"]
        .as_object()
        .cloned()
        .ok_or_else(|| invalid_model("invalid_request", "request must be an object"))?;
    let requested = payload
        .get("modelId")
        .or_else(|| request.get("model"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| invalid_model("invalid_request", "modelId is required"))?
        .to_owned();
    let params = crate::metadata::object([
        ("projectKey", metadata.cwd.to_string_lossy().as_ref().into()),
        ("includeAuto", JsValue::Bool(false)),
    ]);
    let catalog = wire(
        &crate::routes::native_metadata("model_catalog_list".into(), params, metadata.clone())
            .await
            .map_err(request_error)?,
    )?;
    let selected = catalog["items"]
        .as_array()
        .ok_or_else(|| failure("PUBLIC_HOST_RESPONSE_INVALID"))?
        .iter()
        .filter(|v| v["id"] == requested && v["available"] == true)
        .collect::<Vec<_>>();
    if selected.len() != 1 {
        return Err(invalid_model(
            if selected.is_empty() {
                "model_not_available"
            } else {
                "model_id_ambiguous"
            },
            "Selected model is not a single available catalog entry",
        ));
    }
    let selected = selected[0];
    if request
        .get("model")
        .is_some_and(|v| v != &selected["model"] && v != &selected["id"])
        || request
            .get("provider")
            .is_some_and(|v| v != &selected["provider"])
    {
        return Err(invalid_model(
            "model_selection_mismatch",
            "request differs from selected model",
        ));
    }
    if payload.get("budget").is_some_and(|v| !v.is_object()) {
        return Err(invalid_model("invalid_request", "budget must be an object"));
    }
    let mut budget = std::collections::HashMap::new();
    for key in ["maxOutputTokens", "timeoutMs", "maxInputTokens"] {
        if let Some(v) = payload["budget"].get(key) {
            let n = v
                .as_u64()
                .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)
                .ok_or_else(|| {
                    invalid_model("invalid_budget", "budget must be a positive safe integer")
                })?;
            budget.insert(key, n);
        }
    }
    if budget.contains_key("maxInputTokens") {
        return Err(invalid_model(
            "unsupported_budget",
            "maxInputTokens needs measured prompt tokens",
        ));
    }
    let messages = request
        .get("messages")
        .and_then(Value::as_array)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| invalid_model("invalid_request", "messages must be a non-empty array"))?;
    for message in messages {
        if ![Some("user"), Some("assistant")].contains(&message["role"].as_str())
            || !message["content"].as_array().is_some_and(|v| {
                !v.is_empty()
                    && v.iter().all(|b| {
                        [
                            "text",
                            "thinking",
                            "image",
                            "pdf",
                            "audio",
                            "tool_call",
                            "tool_result",
                            "tool_result_reference",
                            "media_reference",
                        ]
                        .contains(&b["type"].as_str().unwrap_or(""))
                    })
            })
        {
            return Err(invalid_model(
                "invalid_request",
                "messages require canonical content blocks",
            ));
        }
    }
    request.insert("provider".into(), selected["provider"].clone());
    request.insert("model".into(), selected["model"].clone());
    if let Some(max) = budget.get("maxOutputTokens") {
        if request
            .get("maxOutputTokens")
            .and_then(Value::as_f64)
            .is_some_and(|v| v > *max as f64)
        {
            return Err(invalid_model(
                "invalid_budget",
                "request maxOutputTokens exceeds budget",
            ));
        }
        request.insert("maxOutputTokens".into(), json!(max));
    }
    let snapshot = crate::config::load_snapshot(
        &metadata.home,
        &metadata.cwd,
        &std::env::vars().collect(),
        Some(&metadata.cwd.to_string_lossy()),
        1.0,
        "",
    )
    .map_err(|e| invalid_model(&e.code, &String::from_utf16_lossy(e.message.units())))?;
    let config = snapshot
        .get("config")
        .and_then(|c| c.get("model"))
        .cloned()
        .ok_or_else(|| failure("PUBLIC_HOST_RESPONSE_INVALID"))?;
    let transport = HyperModelTransport::from_snapshot(&snapshot);
    let canonical = ordered_model_request(original.as_ref(), &request)?;
    let (_, model) = providers::validate_model_request(&canonical, &config).map_err(model_error)?;
    if let Some(v) = request.get("maxOutputTokens") {
        let n = v
            .as_u64()
            .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)
            .ok_or_else(|| {
                invalid_model(
                    "invalid_budget",
                    "maxOutputTokens must be a positive safe integer",
                )
            })?;
        if model
            .get("capabilities")
            .and_then(|c| c.get("maxOutputTokens"))
            .and_then(JsValue::as_number)
            .is_some_and(|max| n as f64 > max)
        {
            return Err(invalid_model(
                "invalid_budget",
                "maxOutputTokens exceeds selected model limit",
            ));
        }
    }
    if operation == "model_prepare" {
        let result = crate::metadata::object([
            (
                "requestId",
                original
                    .as_ref()
                    .and_then(|v| v.get("requestId"))
                    .cloned()
                    .unwrap_or_else(|| request_id.into()),
            ),
            (
                "selection",
                input(
                    &json!({"requestedModelId":requested,"selectedModelId":selected["model"],"providerId":selected["provider"]}),
                )?,
            ),
            ("request", canonical),
        ]);
        return Ok(CapabilityResponse {
            status: 200,
            headers: Default::default(),
            body: CapabilityBody::OrderedJson(result),
        });
    }
    let options = StreamOptions {
        abort: Some(signal),
        stream_timeout_ms: budget.get("timeoutMs").map(|v| *v as f64),
        ..Default::default()
    };
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    tokio::spawn(async move {
        let sender = tx.clone();
        let sink_cancel = options.abort.clone();
        let mut sink = move |event| {
            if let StreamOutput::Event(value) = event {
                let result = Ok(StreamChunk::OrderedJson(value));
                if sender.try_send(result).is_err()
                    && let Some(cancel) = &sink_cancel
                {
                    cancel.cancel();
                }
            }
        };
        let result = http::stream_model(&transport, &canonical, &config, &options, &mut sink).await;
        if let Err(error) = result {
            let _ = tx.send(Err(model_error(error))).await;
        }
    });
    let mut response = CapabilityResponse::json(StatusCode::OK, Value::Null);
    response.headers.insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("application/x-ndjson"),
    );
    for (header, value) in [
        ("x-pilotdeck-model-id", selected["model"].as_str()),
        ("x-pilotdeck-provider-id", selected["provider"].as_str()),
    ] {
        if let Some(v) = value {
            response.headers.insert(
                hyper::header::HeaderName::from_static(header),
                hyper::header::HeaderValue::from_str(v)
                    .map_err(|_| failure("PUBLIC_HOST_RESPONSE_INVALID"))?,
            );
        }
    }
    response.body = CapabilityBody::Stream(Box::new(rx));
    Ok(response)
}
#[derive(Clone)]
struct RuntimeApprovals {
    metadata: Arc<Metadata<NativeFiles>>,
}

fn require_approver(value: &Value) -> Result<(), TransportError> {
    let authorization = value["approverAuthorization"].as_str().unwrap_or("");
    if !authorization
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("bearer "))
        || authorization.get(7..).is_none_or(|p| p.trim().is_empty())
    {
        return Err(TransportError::new(
            StatusCode::UNAUTHORIZED,
            "APPROVAL_AUTH_REQUIRED",
        ));
    }
    Ok(())
}

impl ApprovalBridge for RuntimeApprovals {
    fn status_ordered(&self, value: JsValue, _signal: Cancellation) -> TransportFuture<JsValue> {
        let metadata = self.metadata.clone();
        Box::pin(async move {
            require_approver(&super::module_host::js_json_lossy(&value))?;
            crate::runtime::sop::status(&metadata.home, &metadata.cwd, &value)
                .await
                .map_err(request_error)
        })
    }
    fn resume_ordered(&self, value: JsValue, _signal: Cancellation) -> TransportFuture<JsValue> {
        let metadata = self.metadata.clone();
        Box::pin(async move {
            require_approver(&super::module_host::js_json_lossy(&value))?;
            if !value
                .get("source")
                .and_then(JsValue::as_string)
                .is_some_and(|source| source.equals_str("human"))
            {
                return Err(TransportError::new(
                    StatusCode::BAD_REQUEST,
                    "APPROVAL_INPUT_INVALID",
                ));
            }
            crate::runtime::sop::resume(&metadata.home, &metadata.cwd, &value)
                .await
                .map_err(request_error)
        })
    }
    fn binding(&self) -> ApprovalBinding {
        ApprovalBinding {
            tenant_id: std::env::var("STAFFDECK_COPY_TENANT_ID").unwrap_or_default(),
            agent_id: std::env::var("STAFFDECK_COPY_TARGET_AGENT_ID").unwrap_or_default(),
            pilot_deck_user_id: std::env::var("STAFFDECK_COPY_PILOTDECK_USER_ID")
                .unwrap_or_default(),
        }
    }
    fn dynamic_binding(&self) -> bool {
        std::env::var("STAFFDECK_PROJECT_BINDING_ENABLED").as_deref() == Ok("true")
    }
    fn status(&self, value: Value, _signal: Cancellation) -> TransportFuture<Value> {
        let metadata = self.metadata.clone();
        Box::pin(async move {
            require_approver(&value)?;
            let params = input(&value)?;
            let result = crate::runtime::sop::status(&metadata.home, &metadata.cwd, &params)
                .await
                .map_err(request_error)?;
            wire(&result)
        })
    }
    fn resume(&self, value: Value, _signal: Cancellation) -> TransportFuture<Value> {
        let metadata = self.metadata.clone();
        Box::pin(async move {
            require_approver(&value)?;
            if value["source"] != "human" {
                return Err(TransportError::new(
                    StatusCode::BAD_REQUEST,
                    "APPROVAL_INPUT_INVALID",
                ));
            }
            let params = input(&value)?;
            let result = crate::runtime::sop::resume(&metadata.home, &metadata.cwd, &params)
                .await
                .map_err(request_error)?;
            wire(&result)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_error_adapters_preserve_exact_utf16_and_legacy_views() {
        use pilotdeck_protocol::JsString;
        let raw = JsString::from_utf16(vec![0xd800]);
        let converted = request_error(crate::runtime::connection::RequestError {
            code: "gateway_request_failed".into(),
            message: raw.clone(),
            details: None,
            validation: None,
        });
        assert_eq!(converted.ordered_message, Some(raw.clone()));
        assert_eq!(converted.message.as_deref(), Some("\u{fffd}"));
        let converted = model_error(crate::runtime::model::http::ModelStreamError::Request {
            code: JsString::from_utf16(vec![0xdfff]),
            message: raw.clone(),
            details: None,
        });
        assert_eq!(converted.status, Some(StatusCode::BAD_REQUEST));
        assert_eq!(converted.ordered_code.unwrap().units(), &[0xdfff]);
        assert_eq!(converted.ordered_message, Some(raw.clone()));
        let converted = model_error(crate::runtime::model::http::ModelStreamError::Thrown {
            name: "Error".into(),
            message: raw.clone(),
        });
        assert_eq!(converted.status, Some(StatusCode::BAD_GATEWAY));
        assert_eq!(
            converted.code.as_deref(),
            Some("PUBLIC_MODEL_PROVIDER_FAILED")
        );
        assert_eq!(converted.ordered_message, Some(raw));
    }
    #[test]
    fn public_model_preserves_nested_wire_order_for_fingerprint() {
        use sha2::{Digest, Sha256};
        let original=parse_json(r#"{"request":{"messages":[{"role":"user","content":[{"type":"text","text":"Respond PARITY_OK"}]}]}}"#).unwrap();
        let validated = json!({"messages":[{"role":"user","content":[{"type":"text","text":"Respond PARITY_OK"}]}],"provider":"proof","model":"base","maxOutputTokens":32});
        let mut value =
            ordered_model_request(Some(&original), validated.as_object().unwrap()).unwrap();
        let JsValue::Object(ref mut fields) = value else {
            panic!("object")
        };
        fields.insert("stream".into(), JsValue::Bool(true));
        let serialized = pilotdeck_protocol::stringify_json(&value).unwrap().unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(serialized.as_bytes())),
            "30da0e7c5855da70ba7b083979563575c6e005027ba7f32054e875eebbae8791"
        );
    }
    #[test]
    fn pdf_page_boundaries_preserve_blank_pages_and_newlines() {
        for (raw, expected) in [
            ("ONE\n\n\u{000c}", "ONE\n\n"),
            ("ONE\n\n\u{000c}TWO\n\n\u{000c}", "ONE\n\n\nTWO\n\n"),
            (
                "FIRST\nLINE TWO\n\n\u{000c}\u{000c}LAST\n\n\u{000c}",
                "FIRST\nLINE TWO\n\n\n\nLAST\n\n",
            ),
            ("\u{000c}", ""),
        ] {
            assert_eq!(pdf_page_text(raw.into()), expected);
        }
    }
    #[tokio::test]
    async fn public_file_extracts_utf8_and_rejects_bad_encoding_and_limits() {
        let response = public_file(
            json!({"filename":"note.MD","content_base64":"aGVsbG8="}),
            Cancellation::new(),
        )
        .await
        .unwrap();
        let CapabilityBody::Json(body) = response.body else {
            panic!("JSON required")
        };
        assert_eq!(
            body,
            json!({"filename":"note.MD","text":"hello","mediaType":"text/plain","metadata":{"fileType":"md","bytes":5}})
        );
        for (payload, code) in [
            (
                json!({"filename":"x.txt","content_base64":"/w=="}),
                "PUBLIC_FILE_ENCODING_UNSUPPORTED",
            ),
            (
                json!({"filename":"x.txt","content_base64":"aGVsbG8=","max_bytes":4}),
                "PUBLIC_FILE_TOO_LARGE",
            ),
            (
                json!({"filename":"x.txt","content_base64":"aGVsbG8"}),
                "PUBLIC_HOST_INPUT_INVALID",
            ),
        ] {
            let error = match public_file(payload, Cancellation::new()).await {
                Err(e) => e,
                Ok(_) => panic!("invalid file accepted"),
            };
            assert_eq!(error.code.as_deref(), Some(code));
        }
    }

    #[tokio::test]
    async fn public_file_exact_keeps_original_utf16_filename_and_media_type() {
        let provider = RuntimeProvider {
            metadata: Arc::new(Metadata {
                home: "/nonexistent/public-file-exact".into(),
                cwd: "/nonexistent/public-file-exact".into(),
                files: NativeFiles,
            }),
        };
        let response = provider.call_exact("file_parse".into(),
            parse_json(r#"{"filename":"\ud800.txt","media_type":"custom/\udfff","content_base64":"aGVsbG8="}"#).unwrap(),
            JsValue::Null, Cancellation::new()).await.unwrap();
        let CapabilityBody::OrderedJson(body) = response.body else {
            panic!("exact JSON required")
        };
        assert_eq!(
            body.get("filename").unwrap().as_string().unwrap().units(),
            &[0xd800, 46, 116, 120, 116]
        );
        assert_eq!(
            body.get("mediaType")
                .unwrap()
                .as_string()
                .unwrap()
                .units()
                .last(),
            Some(&0xdfff)
        );
        assert!(
            body.get("text")
                .unwrap()
                .as_string()
                .unwrap()
                .equals_str("hello")
        );
    }

    #[tokio::test]
    async fn public_model_rejects_missing_request_id_before_provider_access() {
        let metadata = Arc::new(Metadata {
            home: "/nonexistent/public-model".into(),
            cwd: "/nonexistent/public-model".into(),
            files: NativeFiles,
        });
        for operation in ["model_prepare", "model_stream"] {
            let error = match public_model(
                metadata.clone(),
                operation.into(),
                json!({}),
                Cancellation::new(),
                None,
            )
            .await
            {
                Err(error) => error,
                Ok(_) => panic!("Invalid public input must fail before provider access"),
            };
            assert_eq!(error.status, Some(StatusCode::BAD_REQUEST));
            assert_eq!(error.code.as_deref(), Some("invalid_request"));
        }
    }

    #[tokio::test]
    async fn public_provider_reads_real_skill_directory() {
        let root = std::env::temp_dir().join(format!("public-skills-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("skills/probe")).unwrap();
        std::fs::write(
            root.join("pilotdeck.yaml"),
            "schemaVersion: 1\nagent: {model: proof/base}\ntelemetry: {enabled: false}\nmodel:\n  providers:\n    proof:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: public-fixture-only\n      models:\n        base: {}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("skills/probe/SKILL.md"),
            "---\nname: Probe\ndescription: Local provider test\n---\nReal content",
        )
        .unwrap();
        let provider = RuntimeProvider {
            metadata: Arc::new(Metadata {
                home: root.clone(),
                cwd: root.clone(),
                files: NativeFiles,
            }),
        };
        let response = provider
            .call(
                "list_general_skills".into(),
                json!({}),
                json!({}),
                Cancellation::new(),
            )
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        let CapabilityBody::Json(value) = response.body else {
            panic!("JSON required")
        };
        assert!(
            value["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["slug"] == "probe")
        );
        assert_eq!(value["items"], value["data"]);
        let catalog = provider
            .call(
                "list_model_catalog".into(),
                json!({}),
                json!({}),
                Cancellation::new(),
            )
            .await
            .unwrap();
        assert_eq!(catalog.status, 200);
        let CapabilityBody::Json(catalog) = catalog.body else {
            panic!("JSON required")
        };
        assert_eq!(
            catalog
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["data", "defaultSelection"]
        );
        assert_eq!(
            catalog["defaultSelection"],
            json!({"mode":"model","provider":"proof","model":"base"})
        );
        let items = catalog["data"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        for (key, expected) in [
            ("id", json!("proof/base")),
            ("provider", json!("proof")),
            ("model", json!("base")),
            ("displayName", json!("base")),
            ("available", json!(true)),
            ("capabilities", json!({})),
            ("name", json!("base")),
            ("enabled", json!(true)),
            ("is_default", json!(true)),
        ] {
            assert_eq!(items[0][key], expected, "catalog field {key}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn approval_transport_cannot_bypass_auth_or_use_nonhuman_resume() {
        let bridge = RuntimeApprovals {
            metadata: Arc::new(Metadata {
                home: std::path::PathBuf::from("/nonexistent/public-approval"),
                cwd: std::path::PathBuf::from("/nonexistent/public-approval"),
                files: NativeFiles,
            }),
        };
        let error = bridge
            .status(json!({"sessionKey":"session"}), Cancellation::new())
            .await
            .unwrap_err();
        assert_eq!(error.code.as_deref(), Some("APPROVAL_AUTH_REQUIRED"));
        let error = bridge.resume(json!({"sessionKey":"session","approverAuthorization":"Bearer public-test","source":"external_task"}), Cancellation::new()).await.unwrap_err();
        assert_eq!(error.code.as_deref(), Some("APPROVAL_INPUT_INVALID"));
    }
}

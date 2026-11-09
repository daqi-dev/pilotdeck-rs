//! Port of `src/composition/HttpModuleClient.ts` (cc7c4e6).
use crate::runtime::model::http::{HyperModelTransport, ModelHttpTransport};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use pilotdeck_protocol::{
    JsObject, JsValue, MODULE_PROTOCOL_VERSION, module::validate_module_message, parse_json,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use url::Url;

const MODULE_HTTP_TRANSPORT: &str = "module-http-v2";

pub struct HttpModuleClient {
    binding: JsValue,
    transport: Arc<HyperModelTransport>,
    sequence: AtomicU64,
    manifest: Mutex<Option<JsValue>>,
}

impl HttpModuleClient {
    pub fn new(binding: JsValue) -> Self {
        Self {
            binding,
            transport: Arc::new(HyperModelTransport::new()),
            sequence: AtomicU64::new(0),
            manifest: Mutex::new(None),
        }
    }

    fn field<'a>(value: &'a JsValue, key: &str) -> Option<&'a JsValue> {
        value.get(key)
    }

    fn string(value: &JsValue) -> Option<String> {
        value.as_string().and_then(|s| s.to_utf8().ok())
    }

    fn timeout_ms(&self) -> u64 {
        Self::field(&self.binding, "timeoutMs")
            .and_then(JsValue::as_number)
            .filter(|n| *n > 0.0 && n.fract() == 0.0)
            .map(|n| n as u64)
            .unwrap_or(10_000)
    }

    fn endpoint_url(&self, path: &str) -> Result<Url, ModuleClientError> {
        let endpoint =
            Self::string(Self::field(&self.binding, "endpoint").unwrap_or(&JsValue::Null))
                .filter(|s| !s.is_empty())
                .ok_or_else(|| protocol_error("Module endpoint is missing."))?;
        let base = if endpoint.ends_with('/') {
            endpoint
        } else {
            format!("{endpoint}/")
        };
        base.parse::<Url>()
            .and_then(|origin| origin.join(path))
            .map_err(|_| protocol_error("Module endpoint URL is invalid."))
    }

    fn credential(&self) -> Result<Option<String>, ModuleClientError> {
        let Some(env_name) = Self::field(&self.binding, "credentialEnv").and_then(Self::string)
        else {
            return Ok(None);
        };
        let value = std::env::var(&env_name)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if value.is_none() {
            return Err(protocol_error(
                "Knowledge module account credential is not configured.",
            ));
        }
        Ok(value)
    }

    async fn manifest(&self) -> Result<(), ModuleClientError> {
        if self.manifest.lock().unwrap().is_some() {
            return Ok(());
        }
        let manifest_path = Self::field(&self.binding, "manifestPath")
            .and_then(Self::string)
            .filter(|s| s.starts_with('/'))
            .unwrap_or_else(|| "/module-manifest".into());
        let value = self
            .request_json(&manifest_path, "GET", None, false)
            .await?;
        validate_manifest(&self.binding, &value)?;
        *self.manifest.lock().unwrap() = Some(value);
        Ok(())
    }

    pub async fn call(
        &self,
        run_id: &str,
        operation_id: &str,
        request_id: &str,
        module: &str,
        operation: &str,
        input: JsObject,
        authority_context: Option<JsObject>,
    ) -> Result<JsValue, ModuleClientError> {
        self.manifest().await?;
        if !binding_methods(&self.binding).contains(&operation.to_string()) {
            return Err(protocol_error(format!(
                "Knowledge operation '{operation}' is not declared."
            )));
        }
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let message_id = format!("module-http-{sequence}");
        let mut payload = JsObject::new();
        payload.insert("operation".into(), JsValue::String(operation.into()));
        payload.insert("input".into(), JsValue::Object(input));
        if let Some(authority) = authority_context {
            payload.insert("authorityContext".into(), JsValue::Object(authority));
        }
        let mut request_body = JsObject::new();
        request_body.insert("kind".into(), JsValue::from("request"));
        request_body.insert("method".into(), JsValue::from("module_call"));
        request_body.insert(
            "messageId".into(),
            JsValue::String(message_id.clone().into()),
        );
        request_body.insert("runId".into(), JsValue::String(run_id.into()));
        request_body.insert("operationId".into(), JsValue::String(operation_id.into()));
        request_body.insert("requestId".into(), JsValue::String(request_id.into()));
        request_body.insert("module".into(), JsValue::String(module.into()));
        request_body.insert("payload".into(), JsValue::Object(payload));
        let call_path = Self::field(&self.binding, "callPath")
            .and_then(Self::string)
            .filter(|s| s.starts_with('/'))
            .unwrap_or_else(|| "/v2/module/call".into());
        let response = self
            .request_json(
                &call_path,
                "POST",
                Some(JsValue::Object(request_body)),
                true,
            )
            .await?;
        let envelope = parse_response(&response, &message_id, request_id)?;
        if !response_ok(&envelope) {
            return Err(module_failure(
                &envelope,
                &format!("Knowledge module {operation} failed."),
            ));
        }
        Ok(envelope
            .get("payload")
            .and_then(|p| p.get("result"))
            .cloned()
            .unwrap_or(JsValue::Null))
    }

    async fn request_json(
        &self,
        path: &str,
        method: &str,
        body: Option<JsValue>,
        accept_structured_error: bool,
    ) -> Result<JsValue, ModuleClientError> {
        let url = self.endpoint_url(path)?;
        let credential = self.credential()?;
        let timeout = Duration::from_millis(self.timeout_ms());
        let body_bytes = match body {
            Some(value) => {
                let text = pilotdeck_protocol::stringify_json(&value)
                    .map_err(|_| protocol_error("Module request body could not be serialized."))?
                    .unwrap_or_else(|| "{}".into());
                Bytes::from(text)
            }
            None => Bytes::new(),
        };
        let mut builder = Request::builder().method(method).uri(url.as_str());
        if method == "POST" {
            builder = builder.header("content-type", "application/json");
        }
        if let Some(token) = credential {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder = builder.header("accept", "application/json");
        let request = builder
            .body(Full::new(body_bytes))
            .map_err(|_| protocol_error("Module HTTP request is invalid."))?;
        let exchange = async {
            let response = ModelHttpTransport::send(self.transport.as_ref(), request)
                .await
                .map_err(|_| transport_error("Module HTTP request could not be completed."))?;
            let status = response.status();
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(|_| transport_error("Module HTTP response could not be read."))?
                .to_bytes();
            let value = if bytes.is_empty() {
                JsValue::Null
            } else {
                parse_json(&String::from_utf8_lossy(&bytes))
                    .map_err(|_| protocol_error("Module HTTP response must contain JSON."))?
            };
            if !status.is_success()
                && !(accept_structured_error
                    && is_module_response(&value)
                    && value.get("ok").and_then(JsValue::as_bool) == Some(false))
            {
                return Err(transport_error(
                    "Module HTTP request could not be completed.",
                ));
            }
            Ok(value)
        };
        match tokio::time::timeout(timeout, exchange).await {
            Ok(result) => result,
            Err(_) => Err(ModuleClientError {
                code: "MODULE_TIMEOUT".into(),
                message: format!("Module HTTP request exceeded {}ms.", timeout.as_millis()),
                retryability: None,
                details: None,
            }),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ModuleClientError {
    pub code: String,
    pub message: String,
    pub retryability: Option<String>,
    pub details: Option<JsValue>,
}

impl ModuleClientError {
    pub fn into_tool_error(self) -> crate::runtime::tools::ToolError {
        let mut error = crate::runtime::tools::ToolError::new(self.code, self.message);
        if let Some(details) = self.details.and_then(|v| v.as_object().cloned()) {
            error.details = Some(details);
        }
        error
    }
}

fn protocol_error(message: impl Into<String>) -> ModuleClientError {
    ModuleClientError {
        code: "MODULE_PROTOCOL_INCOMPATIBLE".into(),
        message: message.into(),
        retryability: None,
        details: None,
    }
}

fn transport_error(message: impl Into<String>) -> ModuleClientError {
    ModuleClientError {
        code: "MODULE_TRANSPORT_UNAVAILABLE".into(),
        message: message.into(),
        retryability: None,
        details: None,
    }
}

fn binding_methods(binding: &JsValue) -> Vec<String> {
    binding
        .get("methods")
        .and_then(|v| match v {
            JsValue::Array(items) => Some(
                items
                    .iter()
                    .filter_map(|item| item.as_string().and_then(|s| s.to_utf8().ok()))
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

fn validate_manifest(binding: &JsValue, value: &JsValue) -> Result<(), ModuleClientError> {
    let Some(record) = value.as_object() else {
        return Err(protocol_error("Module manifest must be an object."));
    };
    let implementation = binding
        .get("implementationId")
        .and_then(|v| v.as_string())
        .map(|s| s.to_utf8().ok())
        .flatten();
    let contract = binding
        .get("contract")
        .and_then(|v| v.as_string())
        .and_then(|s| s.to_utf8().ok());
    let transport = binding
        .get("transport")
        .and_then(|v| v.as_string())
        .and_then(|s| s.to_utf8().ok());
    let methods = record.get("methods");
    let methods_ok = matches!(methods, Some(JsValue::Array(items))
        if items.iter().all(|m| m.as_string().is_some()));
    if record.get("protocolVersion") != Some(&JsValue::from(MODULE_PROTOCOL_VERSION))
        || implementation.as_deref()
            != record
                .get("implementationId")
                .and_then(|v| v.as_string())
                .and_then(|s| s.to_utf8().ok())
                .as_deref()
        || contract.as_deref()
            != record
                .get("contract")
                .and_then(|v| v.as_string())
                .and_then(|s| s.to_utf8().ok())
                .as_deref()
        || transport.as_deref() != Some(MODULE_HTTP_TRANSPORT)
        || !methods_ok
    {
        return Err(protocol_error(
            "Module manifest identity, protocol, contract, transport, or methods are incompatible.",
        ));
    }
    let manifest_methods = binding_methods(value);
    for required in binding_methods(binding) {
        if !manifest_methods.contains(&required) {
            return Err(protocol_error(format!(
                "Module manifest does not provide required method '{required}'."
            )));
        }
    }
    Ok(())
}

fn is_module_response(value: &JsValue) -> bool {
    value
        .get("kind")
        .and_then(|k| k.as_string())
        .is_some_and(|k| k.equals_str("response"))
}

fn parse_response(
    value: &JsValue,
    message_id: &str,
    request_id: &str,
) -> Result<JsValue, ModuleClientError> {
    validate_module_message(value).map_err(|e| protocol_error(e.message))?;
    if !is_module_response(value) {
        return Err(protocol_error(
            "Module call did not return a response envelope.",
        ));
    }
    validate_response_semantics(value)?;
    let in_reply = value
        .get("inReplyTo")
        .and_then(|v| v.as_string())
        .and_then(|s| s.to_utf8().ok());
    if in_reply.as_deref() != Some(message_id) {
        return Err(protocol_error(
            "Module response correlation does not match the request.",
        ));
    }
    if let Some(response_request) = value.get("requestId").and_then(|v| v.as_string()) {
        if response_request.to_utf8().ok().as_deref() != Some(request_id) {
            return Err(protocol_error(
                "Module response correlation does not match the request.",
            ));
        }
    }
    Ok(value.clone())
}

fn response_ok(value: &JsValue) -> bool {
    value.get("ok").and_then(JsValue::as_bool) == Some(true)
}

fn validate_response_semantics(value: &JsValue) -> Result<(), ModuleClientError> {
    if response_ok(value) {
        if value.get("code").is_some()
            || value.get("error").is_some()
            || value
                .get("outcome")
                .and_then(|o| o.as_string())
                .is_some_and(|o| o.equals_str("failed"))
        {
            return Err(protocol_error(
                "Successful module response contains failure fields.",
            ));
        }
        return Ok(());
    }
    let code = value
        .get("code")
        .and_then(|v| v.as_string())
        .filter(|s| !s.is_empty());
    let error = value.get("error").and_then(|v| v.as_object());
    if code.is_none() || error.is_none() {
        return Err(protocol_error(
            "Failed module response requires code and error details.",
        ));
    }
    let error_code = error
        .and_then(|e| e.get("code"))
        .and_then(|v| v.as_string());
    let message = error
        .and_then(|e| e.get("message"))
        .and_then(|v| v.as_string());
    let retry = error
        .and_then(|e| e.get("retryability"))
        .and_then(|v| v.as_string())
        .and_then(|s| s.to_utf8().ok());
    if error_code != code
        || message.as_ref().is_none_or(|m| m.is_empty())
        || retry
            .as_deref()
            .is_none_or(|r| !["safe", "unsafe", "retry_after_status"].contains(&r))
    {
        return Err(protocol_error(
            "Failed module response error fields are inconsistent.",
        ));
    }
    Ok(())
}

fn module_failure(response: &JsValue, fallback: &str) -> ModuleClientError {
    let code = response
        .get("code")
        .and_then(|v| v.as_string())
        .and_then(|s| s.to_utf8().ok())
        .unwrap_or_else(|| "MODULE_PROTOCOL_INCOMPATIBLE".into());
    let message = response
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_string())
        .and_then(|s| s.to_utf8().ok())
        .or_else(|| {
            response
                .get("code")
                .and_then(|c| c.as_string())
                .and_then(|s| s.to_utf8().ok())
        })
        .unwrap_or_else(|| fallback.to_string());
    let retryability = response
        .get("error")
        .and_then(|e| e.get("retryability"))
        .and_then(|v| v.as_string())
        .and_then(|s| s.to_utf8().ok());
    let details = response
        .get("error")
        .and_then(|e| e.get("details"))
        .cloned();
    ModuleClientError {
        code,
        message,
        retryability,
        details,
    }
}

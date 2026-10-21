//! Execution support shared by the router and its provider port. Network
//! ordering stays in the existing provider HTTP path (pending #396).
use super::{
    http::{self, ModelHttpTransport, ModelStreamError, StreamOptions, StreamOutput},
    router::{RouterModelInvocation, field, truthy},
};
use crate::host::ModuleError;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{future::Future, pin::Pin, sync::Arc};

pub type RouterStreamFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), ModelStreamError>> + Send + 'a>>;

pub fn is_fallback_eligible(error: &JsValue) -> bool {
    let code = field(error, "code");
    if [
        "invalid_tool_arguments",
        "billing",
        "model_not_found",
        "auth_error",
    ]
    .iter()
    .any(|name| is(code, name))
    {
        return true;
    }
    truthy(field(error, "retryable"))
        && !truthy(field(error, "recoverableViaCompact"))
        && !truthy(field(error, "recoverableViaImageStrip"))
        && !["prompt_too_long", "request_too_large", "context_overflow"]
            .iter()
            .any(|name| is(code, name))
}
pub(super) fn is(value: &JsValue, text: &str) -> bool {
    value
        .as_string()
        .is_some_and(|value| value.equals_str(text))
}
pub(super) fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in fields {
        out.insert((*key).into(), value.clone());
    }
    JsValue::Object(out)
}
pub(super) fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => &[],
    }
}
pub(super) fn error_event(error: JsValue) -> JsValue {
    object(&[("type", "error".into()), ("error", error)])
}
pub(super) fn abort_error() -> ModelStreamError {
    ModelStreamError::Thrown {
        name: "AbortError".into(),
        message: "This operation was aborted".into(),
    }
}

/// Thin production stream adapter. The delegate remains the capability/judge
/// truth source. Construct a new adapter together with each new model config;
/// the captured generation belongs to this immutable HTTP configuration.
pub struct HttpRouterInvocation {
    delegate: Arc<dyn RouterModelInvocation>,
    transport: Arc<dyn ModelHttpTransport>,
    model_config: JsValue,
    generations: Vec<(JsString, JsValue)>,
}
impl HttpRouterInvocation {
    pub fn new(
        delegate: Arc<dyn RouterModelInvocation>,
        transport: Arc<dyn ModelHttpTransport>,
        model_config: JsValue,
    ) -> Self {
        let generations = model_config
            .get("providers")
            .and_then(JsValue::as_object)
            .map(|providers| {
                providers
                    .entries()
                    .iter()
                    .map(|(key, _)| (key.clone(), delegate.get_provider_generation(key)))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            delegate,
            transport,
            model_config,
            generations,
        }
    }
}
impl RouterModelInvocation for HttpRouterInvocation {
    fn get_capabilities(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<JsValue, ModuleError> {
        self.delegate.get_capabilities(provider, model)
    }
    fn get_multimodal(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<JsValue, ModuleError> {
        self.delegate.get_multimodal(provider, model)
    }
    fn get_provider_generation(&self, provider: &JsString) -> JsValue {
        self.generations
            .iter()
            .find(|(key, _)| key == provider)
            .map(|(_, generation)| generation.clone())
            .unwrap_or(JsValue::Undefined)
    }
    fn get_provider_protocol(&self, provider: &JsString) -> JsString {
        self.model_config
            .get("providers")
            .and_then(JsValue::as_object)
            .and_then(|providers| providers.get_key(provider))
            .and_then(|provider| provider.get("protocol"))
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| "openai".into())
    }
    fn supports_complete(&self) -> bool {
        self.delegate.supports_complete()
    }
    fn complete(
        &self,
        request: JsValue,
        abort: Option<super::router::AbortFlag>,
    ) -> super::router::RouterFuture<JsValue> {
        self.delegate.complete(request, abort)
    }
    fn stream<'a>(
        &'a self,
        request: &'a JsValue,
        options: &'a StreamOptions,
        sink: &'a mut (dyn FnMut(StreamOutput) + Send),
    ) -> RouterStreamFuture<'a> {
        Box::pin(http::stream_model(
            self.transport.as_ref(),
            request,
            &self.model_config,
            options,
            sink,
        ))
    }
}

pub(super) fn canonical_error(
    error: ModelStreamError,
    request: &JsValue,
    protocol: JsString,
) -> JsValue {
    if let ModelStreamError::Provider(error) = error {
        return JsValue::Object(error);
    }
    let (code, message, retryable, raw) = match error {
        ModelStreamError::Request {
            code,
            message,
            details,
        } => (code, message, false, details.map(|value| *value)),
        ModelStreamError::Thrown { name, message } => {
            let lower = String::from_utf16_lossy(message.units()).to_lowercase();
            let code = if lower.contains("timeout") || name.equals_str("TimeoutError") {
                "timeout"
            } else if lower.contains("abort") || name.equals_str("AbortError") {
                "aborted"
            } else {
                "network_error"
            };
            let retryable = name.equals_str("TimeoutError")
                || name.equals_str("AbortError")
                || [
                    "econnreset",
                    "econnrefused",
                    "etimedout",
                    "epipe",
                    "socket hang up",
                    "network",
                    "dns",
                    "fetch failed",
                    "abort",
                ]
                .iter()
                .any(|needle| lower.contains(needle));
            (code.into(), message, retryable, None)
        }
        ModelStreamError::Provider(_) => unreachable!(),
    };
    object(&[
        ("provider", field(request, "provider").clone()),
        ("protocol", JsValue::String(protocol)),
        ("code", JsValue::String(code)),
        ("message", JsValue::String(message)),
        ("retryable", JsValue::Bool(retryable)),
        ("raw", raw.unwrap_or(JsValue::Undefined)),
    ])
}

pub(super) fn downgrade_request(
    request: &JsValue,
    invoker: &dyn RouterModelInvocation,
) -> Result<JsValue, ModelStreamError> {
    let (Some(provider), Some(model)) = (
        field(request, "provider").as_string(),
        field(request, "model").as_string(),
    ) else {
        return Ok(request.clone());
    };
    let Ok(constraints) = invoker.get_multimodal(provider, model) else {
        return Ok(request.clone());
    };
    let allowed = array(field(&constraints, "input"));
    let mut request = request.as_object().cloned().unwrap_or_default();
    let mut messages = Vec::new();
    for message in array(request.get("messages").unwrap_or(&JsValue::Undefined)) {
        let mut message = message.as_object().cloned().unwrap_or_default();
        let mut content = Vec::new();
        for block in array(message.get("content").unwrap_or(&JsValue::Undefined)) {
            if is(field(block, "type"), "tool_result") {
                let mut block = block.as_object().cloned().unwrap_or_default();
                let nested = array(block.get("content").unwrap_or(&JsValue::Undefined))
                    .iter()
                    .map(|item| downgrade_block(item, allowed))
                    .collect::<Result<Vec<_>, _>>()?;
                block.insert("content".into(), JsValue::Array(nested));
                content.push(JsValue::Object(block));
            } else {
                content.push(downgrade_block(block, allowed)?);
            }
        }
        message.insert("content".into(), JsValue::Array(content));
        messages.push(JsValue::Object(message));
    }
    request.insert("messages".into(), JsValue::Array(messages));
    Ok(JsValue::Object(request))
}
pub(super) fn template(parts: &[JsValue]) -> Result<JsValue, ModelStreamError> {
    let mut units = Vec::new();
    for part in parts {
        let text = crate::js_value::to_string_units_checked(part).map_err(|error| {
            ModelStreamError::Thrown {
                name: error.name().into(),
                message: error.message().into(),
            }
        })?;
        http::append_js_string(&mut units, &text)?;
    }
    Ok(JsValue::String(JsString::from_utf16(units)))
}
fn downgrade_block(block: &JsValue, allowed: &[JsValue]) -> Result<JsValue, ModelStreamError> {
    let kind = field(block, "type");
    if allowed.iter().any(|value| value == kind) {
        return Ok(block.clone());
    }
    let mime = field(block, "mimeType").clone();
    let bytes = field(block, "bytes");
    let kb = || {
        JsValue::Number(http::js_round(
            crate::js_value::to_number(bytes).unwrap_or(f64::NAN) / 1024.0,
        ))
    };
    let parts = if is(kind, "image") {
        let mut parts = vec!["[Image: ".into(), mime];
        if truthy(bytes) {
            parts.extend([", ".into(), kb(), "KB".into()]);
        }
        parts.push(" — omitted, model does not support image input]".into());
        parts
    } else if is(kind, "pdf") {
        let mut parts = vec!["[PDF: ".into(), mime, ", ".into(), kb(), "KB".into()];
        let pages = field(block, "pages");
        if truthy(pages) {
            parts.extend([", ".into(), pages.clone(), " pages".into()]);
        }
        parts.push(" — omitted, model does not support PDF input]".into());
        parts
    } else if is(kind, "audio") {
        vec![
            "[Audio: ".into(),
            mime,
            " — omitted, model does not support audio input]".into(),
        ]
    } else {
        return Ok(block.clone());
    };
    Ok(object(&[
        ("type", "text".into()),
        ("text", template(&parts)?),
    ]))
}

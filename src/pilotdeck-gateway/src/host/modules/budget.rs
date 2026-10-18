//! Turn-bound budget ports (fixed TS dispatcher:227-267,408-415).
use super::wire;
use crate::host::{HostError, HostFuture, JsObject, JsValue, ModuleError};
use crate::model_cost::{self, PricingMap};
use pilotdeck_protocol::{JsString, model::*};
use std::sync::Arc;
use tokio::sync::watch;

/// Ports receive the shallow-validated request intact, including extensions.
/// Implementations must be cooperative: dropping a future stops its owned work.
pub trait RequestInputEstimator: Send + Sync {
    fn estimate_request_input(&self, request: JsObject) -> HostFuture<f64>;
}
pub trait RequestBudgetEvaluator: Send + Sync {
    fn evaluate_request_budget(
        &self,
        request: JsObject,
        options: EvaluationOptions,
    ) -> HostFuture<JsValue>;
}
pub trait UsageCostEstimator: Send + Sync {
    fn estimate_usage_cost(
        &self,
        usage: Option<JsObject>,
        provider: JsString,
        model: JsString,
    ) -> HostFuture<Option<f64>>;
}

#[derive(Clone)]
pub struct EvaluationOptions {
    pub max_context_tokens: f64,
    pub reserved_output_tokens: Option<f64>,
    pub use_provider_count: Option<bool>,
    pub calibration: Option<JsObject>,
    /// Trusted turn cancellation; never taken from payload.options.signal.
    pub signal: Option<watch::Receiver<bool>>,
}
#[derive(Clone, Default)]
pub struct Ports {
    pub estimate_request_input: Option<Arc<dyn RequestInputEstimator>>,
    pub evaluate_request_budget: Option<Arc<dyn RequestBudgetEvaluator>>,
    pub estimate_usage_cost: Option<Arc<dyn UsageCostEstimator>>,
}
impl Ports {
    /// Local accounting only. Provider counts/evaluation arrive in A6.4-pr3.
    pub fn local(pricing: Option<PricingMap>) -> Self {
        Self {
            estimate_request_input: Some(Arc::new(LocalRequestInput)),
            estimate_usage_cost: Some(Arc::new(LocalUsageCost { pricing })),
            ..Self::default()
        }
    }
}
#[derive(Clone)]
pub struct Handler {
    pub ports: Option<Ports>,
    pub signal: Option<watch::Receiver<bool>>,
}
impl Default for Handler {
    fn default() -> Self {
        // X0's default router represents an installed, empty budget module.
        Self {
            ports: Some(Ports::default()),
            signal: None,
        }
    }
}
impl Handler {
    /// A6.9 can use this view when composing the manifest. None omits budget;
    /// Some([]) advertises an installed budget module with no operations.
    pub fn methods(&self) -> Option<Vec<&'static str>> {
        let ports = self.ports.as_ref()?;
        let mut methods = Vec::new();
        if ports.estimate_request_input.is_some() {
            methods.push("estimate_request_input");
        }
        if ports.evaluate_request_budget.is_some() {
            methods.push("evaluate_request_budget");
        }
        if ports.estimate_usage_cost.is_some() {
            methods.push("estimate_usage_cost");
        }
        Some(methods)
    }

    pub fn call(&self, call: JsValue) -> HostFuture<JsObject> {
        let handler = self.clone();
        Box::pin(async move {
            // No installed module means TS never enters dispatchBudget.
            let ports = handler
                .ports
                .ok_or_else(|| ModuleError::new("Unsupported sidecar host module: budget"))?;
            let payload = call.get("payload").and_then(JsValue::as_object);
            let operation = wire::string_field(payload, "operation")?;
            let field = |key: &str| payload.and_then(|p| p.get(key));
            if operation.equals_str("estimate_request_input") {
                if let Some(port) = ports.estimate_request_input {
                    let request = canonical_request(field("request"))?;
                    return Ok(response(
                        "tokens",
                        JsValue::Number(port.estimate_request_input(request).await?),
                    ));
                }
            } else if operation.equals_str("evaluate_request_budget") {
                if let Some(port) = ports.evaluate_request_budget {
                    let request = canonical_request(field("request"))?;
                    let options = field("options").and_then(JsValue::as_object);
                    let option = |key: &str| options.and_then(|o| o.get(key));
                    let max_context_tokens =
                        wire::positive_finite(option("maxContextTokens"), "maxContextTokens")?;
                    let reserved_output_tokens = wire::optional_non_negative_finite(
                        option("reservedOutputTokens"),
                        "reservedOutputTokens",
                    )?;
                    let use_provider_count = match option("useProviderCount") {
                        None | Some(JsValue::Undefined) => None,
                        _ => Some(wire::boolean_field(options, "useProviderCount")?),
                    };
                    let options = EvaluationOptions {
                        max_context_tokens,
                        reserved_output_tokens,
                        use_provider_count,
                        calibration: option("calibration").and_then(JsValue::as_object).cloned(),
                        signal: handler.signal,
                    };
                    return Ok(response(
                        "snapshot",
                        port.evaluate_request_budget(request, options).await?,
                    ));
                }
            } else if operation.equals_str("estimate_usage_cost")
                && let Some(port) = ports.estimate_usage_cost
            {
                let usage = field("usage").and_then(JsValue::as_object).cloned();
                let provider = wire::string_field(payload, "provider")?.clone();
                let model = wire::string_field(payload, "model")?.clone();
                let cost = port.estimate_usage_cost(usage, provider, model).await?;
                if cost.is_some_and(|value| !value.is_finite() || value < 0.0) {
                    return Err(ModuleError::new(
                        "Host budget capability returned an invalid usage cost.",
                    )
                    .into());
                }
                return Ok(response(
                    "costUsd",
                    cost.map_or(JsValue::Null, JsValue::Number),
                ));
            }
            Err(wire::named_error(
                "Host budget capability does not support ",
                operation,
                ".",
            ))
        })
    }
    pub fn dispose(&self) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
}
fn response(key: &str, value: JsValue) -> JsObject {
    let mut result = JsObject::new();
    result.insert(key.into(), value);
    result
}
fn canonical_request(value: Option<&JsValue>) -> Result<JsObject, HostError> {
    value
        .and_then(JsValue::as_object)
        .filter(|request| {
            matches!(request.get("provider"), Some(JsValue::String(_)))
                && matches!(request.get("model"), Some(JsValue::String(_)))
                && matches!(request.get("messages"), Some(JsValue::Array(_)))
        })
        .cloned()
        .ok_or_else(|| {
            ModuleError::new("Sidecar model call contains an invalid canonical request.").into()
        })
}

pub struct LocalRequestInput;
impl RequestInputEstimator for LocalRequestInput {
    fn estimate_request_input(&self, request: JsObject) -> HostFuture<f64> {
        Box::pin(async move {
            let (request, extra_tool_tokens) = accounting_request(&request)?;
            model_cost::estimate_request_input(&request)
                .map(|tokens| (tokens + extra_tool_tokens) as f64)
                .map_err(|error| ModuleError::new(format!("{error:?}")).into())
        })
    }
}
#[derive(Default)]
pub struct LocalUsageCost {
    pub pricing: Option<PricingMap>,
}
impl UsageCostEstimator for LocalUsageCost {
    fn estimate_usage_cost(
        &self,
        usage: Option<JsObject>,
        provider: JsString,
        model: JsString,
    ) -> HostFuture<Option<f64>> {
        let result = (|| {
            let route_pricing = surrogate_pricing(&provider, &model, self.pricing.as_ref());
            let pricing = route_pricing.as_ref().or(self.pricing.as_ref());
            // Built-in patterns only use ASCII. Replacement leaves those
            // matches unchanged; configured keys are handled losslessly above.
            let provider = String::from_utf16_lossy(provider.units());
            let model = String::from_utf16_lossy(model.units());
            let usage = usage
                .as_ref()
                .map(|u| {
                    let native_cost = u.get("nativeCost").and_then(JsValue::as_number);
                    if native_cost.is_some_and(|n| n.is_finite() && n >= 0.0) {
                        return Ok(Usage {
                            input_tokens: None,
                            output_tokens: None,
                            cache_read_tokens: None,
                            cache_write_tokens: None,
                            total_tokens: None,
                            native_cost,
                        });
                    }
                    let token = |key: &str| -> Result<Option<f64>, HostError> {
                        match u.get(key) {
                            None | Some(JsValue::Undefined | JsValue::Null) => Ok(None),
                            Some(value) => crate::js_value::to_number(value)
                                .map(Some)
                                .map_err(|error| ModuleError::new(format!("{error:?}")).into()),
                        }
                    };
                    Ok::<_, HostError>(Usage {
                        input_tokens: token("inputTokens")?,
                        output_tokens: token("outputTokens")?,
                        cache_read_tokens: token("cacheReadTokens")?,
                        cache_write_tokens: token("cacheWriteTokens")?,
                        total_tokens: None,
                        native_cost: u.get("nativeCost").and_then(JsValue::as_number),
                    })
                })
                .transpose()?;
            Ok(model_cost::estimate_usage_cost(
                usage.as_ref(),
                &provider,
                &model,
                pricing,
            ))
        })();
        Box::pin(async move { result })
    }
}

// PricingMap keys are UTF-8. Only the ill-formed UTF-16 route needs this
// bridge: resolve configured matching in code units before lossy conversion
// can turn a surrogate into an exact U+FFFD key match with higher precedence.
fn surrogate_pricing(
    provider: &JsString,
    model: &JsString,
    pricing: Option<&PricingMap>,
) -> Option<PricingMap> {
    if provider.to_utf8().is_ok() && model.to_utf8().is_ok() {
        return None;
    }
    let mut projected = PricingMap::new();
    let contains = |haystack: &[u16], needle: &[u16]| {
        needle.is_empty() || haystack.windows(needle.len()).any(|part| part == needle)
    };
    if let Some((_, rate)) = pricing
        .into_iter()
        .flat_map(PricingMap::iter)
        .find(|(key, _)| {
            let key: Vec<u16> = key.encode_utf16().collect();
            contains(model.units(), &key) || contains(&key, model.units())
        })
    {
        projected.insert(
            format!(
                "{}/{}",
                String::from_utf16_lossy(provider.units()),
                String::from_utf16_lossy(model.units())
            ),
            *rate,
        );
    }
    Some(projected)
}

// This is only the local counter's projection, not canonical request admission.
// Ignore fields the counter never reads (including roles and provider metadata).
// The normal typed projection uses model_cost; lax non-object tool schemas
// use the same tokenizer because ToolSchema can only represent object schemas.
fn accounting_request(raw: &JsObject) -> Result<(CanonicalModelRequest, usize), HostError> {
    let messages = array(raw.get("messages"), "messages")?
        .iter()
        .map(|message| {
            let content = array(message.get("content"), "content")?
                .iter()
                .filter_map(|block| accounting_block(block).transpose())
                .collect::<Result<_, _>>()?;
            Ok(Message {
                role: Role::User,
                content,
                metadata: None,
            })
        })
        .collect::<Result<_, HostError>>()?;
    let mut extra_tool_tokens = 0;
    let tools = match raw.get("tools") {
        None | Some(JsValue::Null | JsValue::Undefined) => None,
        value => {
            let mut tools = Vec::new();
            for tool in array(value, "tools")? {
                let name = template_string(tool.get("name"))?;
                let description = match tool.get("description") {
                    None | Some(JsValue::Null | JsValue::Undefined) => None,
                    value => Some(template_string(value)?),
                };
                if let Some(schema) = tool.get("inputSchema").and_then(JsValue::as_object) {
                    tools.push(ToolSchema {
                        name,
                        description,
                        input_schema: schema.clone(),
                    });
                } else {
                    let mut units = name.units().to_vec();
                    if let Some(description) = description {
                        units.extend_from_slice(description.units());
                    }
                    let json = tool
                        .get("inputSchema")
                        .and_then(|schema| {
                            pilotdeck_protocol::stringify_json(schema).ok().flatten()
                        })
                        .unwrap_or_default();
                    units.extend(json.encode_utf16());
                    extra_tool_tokens +=
                        pilotdeck_tokenizer::count_tokens(&JsString::from_utf16(units))
                            .map_err(|e| ModuleError::new(format!("{e:?}")))?;
                }
            }
            Some(tools)
        }
    };
    Ok((
        CanonicalModelRequest {
            provider: string(raw.get("provider"), "provider")?,
            model: string(raw.get("model"), "model")?,
            messages,
            tools,
            system_prompt: optional_text(raw.get("systemPrompt"), "systemPrompt")?,
            tool_choice: None,
            max_output_tokens: None,
            temperature: None,
            speed: None,
            thinking: None,
            stream: None,
            metadata: None,
            output_schema: None,
            cache_breakpoints: None,
            cache_plan: None,
        },
        extra_tool_tokens,
    ))
}
fn accounting_block(block: &JsValue) -> Result<Option<ContentBlock>, HostError> {
    let text = |value: Option<&JsValue>, field| -> Result<ContentBody, HostError> {
        Ok(ContentBody::Text(TextBlock {
            text: token_text(value, field)?,
            block_id: None,
        }))
    };
    let body = match block
        .get("type")
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok())
        .as_deref()
    {
        Some("text") => text(block.get("text"), "text")?,
        Some("thinking") => text(
            block
                .get("reasoningContent")
                .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                .or_else(|| block.get("text")),
            "text",
        )?,
        // These four blocks share the fixed multimedia count; their other
        // fields do not participate in accounting and must not be validated.
        Some("image" | "pdf" | "audio" | "media_reference") => ContentBody::Image(ImageBlock {
            source: MediaSource::Base64,
            data: JsString::default(),
            mime_type: JsString::default(),
            bytes: None,
            detail: None,
        }),
        Some("tool_call") => ContentBody::ToolCall(ToolCall {
            timeline: None,
            id: JsString::default(),
            name: string(block.get("name"), "name")?,
            input: block.get("input").cloned().unwrap_or(JsValue::Undefined),
            raw: None,
        }),
        Some("tool_result") => ContentBody::ToolResult(ToolResult {
            tool_call_id: JsString::default(),
            is_error: None,
            raw: None,
            content: array(block.get("content"), "content")?
                .iter()
                .map(accounting_result)
                .collect::<Result<_, _>>()?,
        }),
        Some("tool_result_reference") => text(block.get("preview"), "preview")?,
        _ => return Ok(None), // TS's block switch returns zero for unknown tags.
    };
    Ok(Some(ContentBlock {
        timeline: None,
        body,
    }))
}
fn accounting_result(block: &JsValue) -> Result<ToolResultItem, HostError> {
    let body = match block
        .get("type")
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok())
        .as_deref()
    {
        Some("image") => ToolResultContent::Image(ImageBlock {
            source: if block
                .get("source")
                .and_then(JsValue::as_string)
                .is_some_and(|s| s.equals_str("url"))
            {
                MediaSource::Url
            } else {
                MediaSource::Base64
            },
            data: string(block.get("data"), "data")?,
            mime_type: string(block.get("mimeType"), "mimeType")?,
            bytes: None,
            detail: None,
        }),
        Some("pdf")
            if !block
                .get("mimeType")
                .and_then(JsValue::as_string)
                .is_some_and(|s| s.equals_str("application/pdf")) =>
        {
            // Typed PdfBlock fixes the MIME to application/pdf. Preserve the
            // TS interpolation for a lax wire value outside that typed shape.
            let mut units: Vec<u16> = "[PDF: ".encode_utf16().collect();
            units.extend_from_slice(template_string(block.get("mimeType"))?.units());
            units.extend(
                format!(
                    ", {} base64 characters",
                    string(block.get("data"), "data")?.len_utf16()
                )
                .encode_utf16(),
            );
            if let Some(pages) = block
                .get("pages")
                .and_then(JsValue::as_number)
                .filter(|n| *n != 0.0 && !n.is_nan())
            {
                units.extend(
                    format!(", {} pages", crate::js_value::number_string(pages)).encode_utf16(),
                );
            }
            units.push(u16::from(b']'));
            ToolResultContent::Text(TextBlock {
                text: JsString::from_utf16(units),
                block_id: None,
            })
        }
        Some("pdf") => ToolResultContent::Pdf(PdfBlock {
            data: string(block.get("data"), "data")?,
            bytes: 0.0,
            pages: block.get("pages").and_then(JsValue::as_number),
        }),
        Some("text") => ToolResultContent::Text(TextBlock {
            text: optional_text(block.get("text"), "text")?.unwrap_or_default(),
            block_id: None,
        }),
        _ => ToolResultContent::Text(TextBlock {
            text: JsString::default(),
            block_id: None,
        }),
    };
    Ok(ToolResultItem {
        timeline: None,
        body,
    })
}
fn template_string(value: Option<&JsValue>) -> Result<JsString, HostError> {
    if let Some(JsValue::String(value)) = value {
        return Ok(value.clone());
    }
    crate::js_value::display(value.unwrap_or(&JsValue::Undefined))
        .map(JsString::from)
        .map_err(|e| ModuleError::new(format!("{e:?}")).into())
}
fn token_text(value: Option<&JsValue>, field: &str) -> Result<JsString, HostError> {
    match value {
        None | Some(JsValue::Undefined) => {
            Err(ModuleError::new("Cannot read properties of undefined (reading 'length')").into())
        }
        Some(JsValue::Null) => {
            Err(ModuleError::new("Cannot read properties of null (reading 'length')").into())
        }
        _ => string(value, field),
    }
}
fn local_shape(field: &str) -> HostError {
    ModuleError::new(format!("Local token accounting cannot read {field}.")).into()
}
fn string(value: Option<&JsValue>, field: &str) -> Result<JsString, HostError> {
    value
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| local_shape(field))
}
fn optional_text(value: Option<&JsValue>, field: &str) -> Result<Option<JsString>, HostError> {
    match value {
        None | Some(JsValue::Null | JsValue::Undefined) => Ok(None),
        _ => string(value, field).map(Some),
    }
}
fn array<'a>(value: Option<&'a JsValue>, field: &str) -> Result<&'a [JsValue], HostError> {
    match value {
        Some(JsValue::Array(items)) => Ok(items),
        _ => Err(local_shape(field)),
    }
}

//! Turn-bound budget ports (fixed TS dispatcher:227-267,408-415).
use super::wire;
use crate::host::{HostError, HostFuture, JsObject, JsValue, ModuleError};
use crate::model_cost::{self, PricingMap};
use pilotdeck_protocol::{JsString, model::*};
use std::sync::Arc;
use tokio::sync::watch;

/// Ports receive the shallow-validated request intact, including extensions.
/// Implementations must be cooperative: dropping a future cancels owned I/O.
/// Local tokenizer batches already running finish under a bounded CPU permit.
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
    /// Local accounting, including budget evaluation without provider access.
    pub fn local(pricing: Option<PricingMap>) -> Self {
        Self {
            estimate_request_input: Some(Arc::new(LocalRequestInput)),
            evaluate_request_budget: Some(Arc::new(
                crate::runtime::model::token_accounting::TokenAccountingRuntime::default(),
            )),
            estimate_usage_cost: Some(Arc::new(LocalUsageCost { pricing })),
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
pub(super) fn canonical_request(value: Option<&JsValue>) -> Result<JsObject, HostError> {
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
            crate::runtime::model::cpu::run(move || {
                let (request, extra_tool_tokens) = accounting_request(&request)?;
                model_cost::estimate_request_input(&request)
                    .map(|tokens| (tokens + extra_tool_tokens) as f64)
                    .map_err(|error| ModuleError::new(format!("{error:?}")).into())
            })
            .await
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
                            Some(value) => {
                                check_primitive_conversion(value)?;
                                crate::js_value::to_number(value)
                                    .map(Some)
                                    .map_err(|_| conversion_error())
                            }
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
            // The native budget port calls router.estimateUsageCost directly.
            // AgentLoop's finite/non-negative fallback happens outside this
            // module; the dispatcher must see and reject invalid raw costs.
            Ok(Some(model_cost::estimate_cost(
                usage.as_ref(),
                &provider,
                &model,
                pricing,
            )))
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

// Project only the fields the local counter reads, in TS evaluation order.
// Dynamic wire values cannot be parsed as canonical typed blocks: e.g. strings
// are iterable, template interpolation accepts missing names, and tool-result
// Array#join converts primitives. The resulting typed request still shares the
// existing model_cost counter and tokenizer.
pub(crate) fn accounting_request(
    raw: &JsObject,
) -> Result<(CanonicalModelRequest, usize), HostError> {
    let system_prompt = raw
        .get("systemPrompt")
        .filter(|value| truthy(value))
        .map(|value| token_text(Some(value)))
        .transpose()?;
    let messages = iterable(raw.get("messages"), "request.messages")?
        .iter()
        .map(|message| {
            let content = iterable(property(Some(message), "content")?, "message.content")?
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
        value if length(value)?.as_number() == Some(0.0) => None,
        value => {
            let mut tools = Vec::new();
            for tool in iterable(value, "tools")?.iter() {
                let name = template_string(property(Some(tool), "name")?)?;
                let description = tool
                    .get("description")
                    .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
                    .map(|value| template_string(Some(value)))
                    .transpose()?;
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
                            .map_err(|error| ModuleError::new(format!("{error:?}")))?;
                }
            }
            Some(tools)
        }
    };
    Ok((
        CanonicalModelRequest {
            // These routing fields do not participate in local token accounting.
            provider: JsString::default(),
            model: JsString::default(),
            messages,
            tools,
            system_prompt,
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
    let text = |value| -> Result<ContentBody, HostError> {
        Ok(ContentBody::Text(TextBlock {
            text: token_text(value)?,
            block_id: None,
        }))
    };
    let body = match property(Some(block), "type")?
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok())
        .as_deref()
    {
        Some("text") => text(block.get("text"))?,
        Some("thinking") => text(
            block
                .get("reasoningContent")
                .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                .or_else(|| block.get("text")),
        )?,
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
            name: template_string(block.get("name"))?,
            input: block.get("input").cloned().unwrap_or(JsValue::Undefined),
            raw: None,
        }),
        Some("tool_result") => {
            let content = block.get("content");
            property(content, "map")?; // Nullish receivers throw before the call.
            let Some(JsValue::Array(items)) = content else {
                return Err(ModuleError::new("content.map is not a function").into());
            };
            // TS completes map before join coerces text values. A later null
            // block must throw before an earlier text's conversion error.
            let mapped = items
                .iter()
                .map(accounting_result)
                .collect::<Result<Vec<_>, _>>()?;
            ContentBody::ToolResult(ToolResult {
                tool_call_id: JsString::default(),
                is_error: None,
                raw: None,
                content: mapped
                    .iter()
                    .map(|value| -> Result<_, HostError> {
                        let text = if matches!(value, JsValue::Null | JsValue::Undefined) {
                            JsString::default()
                        } else {
                            template_string(Some(value))?
                        };
                        Ok(ToolResultItem {
                            timeline: None,
                            body: ToolResultContent::Text(TextBlock {
                                text,
                                block_id: None,
                            }),
                        })
                    })
                    .collect::<Result<_, _>>()?,
            })
        }
        Some("tool_result_reference") => text(block.get("preview"))?,
        _ => return Ok(None),
    };
    Ok(Some(ContentBlock {
        timeline: None,
        body,
    }))
}
fn accounting_result(block: &JsValue) -> Result<JsValue, HostError> {
    let tag = property(Some(block), "type")?
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok());
    Ok(match tag.as_deref() {
        Some("text") => block.get("text").cloned().unwrap_or(JsValue::Undefined),
        Some("image" | "pdf") => {
            let pdf = tag.as_deref() == Some("pdf");
            let mut units: Vec<u16> = (if pdf { "[PDF: " } else { "[Image: " })
                .encode_utf16()
                .collect();
            units.extend_from_slice(template_string(block.get("mimeType"))?.units());
            units.extend(", ".encode_utf16());
            if !pdf
                && block
                    .get("source")
                    .and_then(JsValue::as_string)
                    .is_some_and(|value| value.equals_str("url"))
            {
                units.extend_from_slice(template_string(block.get("data"))?.units());
            } else {
                units
                    .extend_from_slice(template_string(Some(&length(block.get("data"))?))?.units());
                units.extend(" base64 characters".encode_utf16());
            }
            if pdf && block.get("pages").is_some_and(truthy) {
                units.extend(", ".encode_utf16());
                units.extend_from_slice(template_string(block.get("pages"))?.units());
                units.extend(" pages".encode_utf16());
            }
            units.push(u16::from(b']'));
            JsValue::String(JsString::from_utf16(units))
        }
        _ => JsValue::Undefined,
    })
}
pub(crate) fn flatten_result(block: &JsValue) -> Result<JsValue, HostError> {
    let Some(JsValue::Array(items)) = block.get("content") else {
        return Err(ModuleError::new("content.map is not a function").into());
    };
    let mapped = items
        .iter()
        .map(accounting_result)
        .collect::<Result<Vec<_>, _>>()?;
    let mut units = Vec::new();
    for (index, value) in mapped.iter().enumerate() {
        if index > 0 {
            units.push(u16::from(b'\n'));
        }
        if !matches!(value, JsValue::Null | JsValue::Undefined) {
            units.extend_from_slice(template_string(Some(value))?.units());
        }
    }
    Ok(JsValue::String(JsString::from_utf16(units)))
}
fn template_string(value: Option<&JsValue>) -> Result<JsString, HostError> {
    let undefined = JsValue::Undefined;
    let value = value.unwrap_or(&undefined);
    check_primitive_conversion(value)?;
    crate::js_value::to_string_units(value)
        .map(JsString::from_utf16)
        .map_err(|_| conversion_error())
}
// Wire JSON cannot contain callable properties. An own toString shadows the
// inherited conversion method; valueOf cannot supply a primitive either.
// Array#join visits elements, but ordinary object properties are not visited.
fn check_primitive_conversion(value: &JsValue) -> Result<(), HostError> {
    let mut frames = vec![std::slice::from_ref(value).iter()];
    while let Some(frame) = frames.last_mut() {
        match frame.next() {
            Some(JsValue::Object(object)) if object.get("toString").is_some() => {
                return Err(ModuleError::new("Cannot convert object to primitive value").into());
            }
            Some(JsValue::Array(items)) => {
                if frames.len() > crate::js_value::MAX_ARRAY_DEPTH {
                    return Err(conversion_error());
                }
                frames.push(items.iter());
            }
            Some(_) => {}
            None => {
                frames.pop();
            }
        }
    }
    Ok(())
}
fn conversion_error() -> HostError {
    ModuleError::new(crate::js_value::StackOverflow::MESSAGE).into()
}
fn property<'a>(value: Option<&'a JsValue>, field: &str) -> Result<Option<&'a JsValue>, HostError> {
    match value {
        None | Some(JsValue::Undefined | JsValue::Null) => {
            let receiver = if matches!(value, Some(JsValue::Null)) {
                "null"
            } else {
                "undefined"
            };
            Err(ModuleError::new(format!(
                "Cannot read properties of {receiver} (reading '{field}')"
            ))
            .into())
        }
        Some(value) => Ok(value.get(field)),
    }
}
fn length(value: Option<&JsValue>) -> Result<JsValue, HostError> {
    Ok(match value {
        Some(JsValue::String(text)) => JsValue::Number(text.len_utf16() as f64),
        Some(JsValue::Array(items)) => JsValue::Number(items.len() as f64),
        _ => property(value, "length")?
            .cloned()
            .unwrap_or(JsValue::Undefined),
    })
}
fn token_text(value: Option<&JsValue>) -> Result<JsString, HostError> {
    // countTokens first reads length, then calls js-tiktoken's substring.
    if length(value)?.as_number() == Some(0.0) {
        return Ok(JsString::default());
    }
    match value {
        Some(JsValue::String(text)) => Ok(text.clone()),
        _ => Err(ModuleError::new("text.substring is not a function").into()),
    }
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}
fn iterable<'a>(
    value: Option<&'a JsValue>,
    name: &str,
) -> Result<std::borrow::Cow<'a, [JsValue]>, HostError> {
    match value {
        Some(JsValue::Array(items)) => Ok(std::borrow::Cow::Borrowed(items)),
        Some(JsValue::String(text)) => Ok(std::borrow::Cow::Owned(
            char::decode_utf16(text.units().iter().copied())
                .map(|ch| {
                    JsValue::String(match ch {
                        Ok(ch) => JsString::from(ch.to_string()),
                        Err(ch) => JsString::from_utf16(vec![ch.unpaired_surrogate()]),
                    })
                })
                .collect(),
        )),
        _ => Err(ModuleError::new(format!("{name} is not iterable")).into()),
    }
}

//! Public model boundary over one already-selected project generation.
//! Fixed TS: composition/activeRuntimeModelPorts.ts and publicHostRuntimeAdapter.ts.
//! Internal provider replay remains permissive; public messages are checked here.
#![allow(clippy::result_large_err)]
use crate::{
    host::ModuleError,
    metadata::object,
    module_host::{HostBody, HostEvent, HostFailure, HostResponse},
    runtime::{
        model::{
            http::{ModelStreamError, StreamOptions, StreamOutput, YieldGate},
            provider_registry::ModelInvocationProviderRegistry,
            providers::validate_model_request,
            router::{AbortFlag, RouterModelInvocation},
        },
        project::{ProjectResources, Resource, RuntimeLease, model::ModelResources},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{future::pending, sync::Arc};
use tokio::sync::mpsc;

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}
fn invalid(code: &str, message: impl Into<JsString>) -> ModelStreamError {
    ModelStreamError::Request {
        code: code.into(),
        message: message.into(),
        details: None,
    }
}
fn record(value: &JsValue) -> Result<&JsObject, ModelStreamError> {
    value
        .as_object()
        .ok_or_else(|| invalid("invalid_request", "request must be an object"))
}
fn safe_positive(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value <= 9_007_199_254_740_991.0 && value.fract() == 0.0
}
fn positive(value: &JsValue, name: &str) -> Result<Option<f64>, ModelStreamError> {
    if value.is_undefined() {
        return Ok(None);
    }
    value
        .as_number()
        .filter(|value| safe_positive(*value))
        .map(Some)
        .ok_or_else(|| {
            invalid(
                "invalid_budget",
                format!("{name} must be a positive integer"),
            )
        })
}
fn nonempty_string(value: &JsValue) -> bool {
    value
        .as_string()
        .is_some_and(|s| !crate::model_catalog::trim(s).is_empty())
}
fn validate_messages(value: &JsValue) -> Result<(), ModelStreamError> {
    let messages = array(value)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| invalid("invalid_request", "messages must be a non-empty array"))?;
    for (mi, message) in messages.iter().enumerate() {
        if message.as_object().is_none() {
            return Err(invalid(
                "invalid_request",
                format!("messages[{mi}] must be an object"),
            ));
        }
        if !field(message, "role")
            .as_string()
            .is_some_and(|s| s.equals_str("user") || s.equals_str("assistant"))
        {
            return Err(invalid(
                "invalid_request",
                format!("messages[{mi}].role must be user or assistant"),
            ));
        }
        let content = array(field(message, "content"))
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                invalid(
                    "invalid_request",
                    format!("messages[{mi}].content must be a non-empty canonical block array"),
                )
            })?;
        for (bi, block) in content.iter().enumerate() {
            if block.as_object().is_none()
                || !field(block, "type").as_string().is_some_and(|s| {
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
                    .iter()
                    .any(|kind| s.equals_str(kind))
                })
            {
                return Err(invalid(
                    "invalid_request",
                    format!("messages[{mi}].content[{bi}] must be a canonical content block"),
                ));
            }
        }
    }
    Ok(())
}
struct Prepared {
    body: JsValue,
    request: JsValue,
    timeout_ms: Option<f64>,
}
fn module_error(error: ModuleError) -> ModelStreamError {
    ModelStreamError::Request {
        code: error.code.unwrap_or_else(|| "invalid_request".into()),
        message: error.message,
        details: None,
    }
}
fn prepare(
    input: &JsValue,
    catalog: &JsValue,
    model: &ModelResources,
) -> Result<Prepared, ModelStreamError> {
    let raw = field(input, "request");
    let request = record(raw)?;
    let empty = JsObject::new();
    let budget = if field(input, "budget").is_undefined() {
        &empty
    } else {
        record(field(input, "budget"))?
    };
    let requested = match field(input, "modelId") {
        JsValue::Undefined | JsValue::Null => field(raw, "model"),
        value => value,
    };
    if !nonempty_string(requested) {
        return Err(invalid("invalid_request", "modelId is required"));
    }
    if !nonempty_string(field(input, "requestId")) {
        return Err(invalid("invalid_request", "requestId is required"));
    }
    let matches: Vec<_> = array(field(catalog, "items"))
        .into_iter()
        .flatten()
        .filter(|item| field(item, "id") == requested && truthy(field(item, "available")))
        .collect();
    if matches.len() != 1 {
        return Err(invalid(
            if matches.is_empty() {
                "model_not_available"
            } else {
                "model_id_ambiguous"
            },
            JsString::from_utf16(
                "Selected model "
                    .encode_utf16()
                    .chain(requested.as_string().unwrap().units().iter().copied())
                    .chain(" is not a single available catalog entry".encode_utf16())
                    .collect(),
            ),
        ));
    }
    let selected = matches[0];
    if !field(raw, "model").is_undefined()
        && field(raw, "model") != field(selected, "model")
        && field(raw, "model") != field(selected, "id")
    {
        return Err(invalid(
            "model_selection_mismatch",
            "request model differs from selected model",
        ));
    }
    if !field(raw, "provider").is_undefined()
        && field(raw, "provider") != field(selected, "provider")
    {
        return Err(invalid(
            "model_selection_mismatch",
            "provider differs from selected model",
        ));
    }
    static MISSING_BUDGET: JsValue = JsValue::Undefined;
    let get = |key| budget.get(key).unwrap_or(&MISSING_BUDGET);
    let max_output = positive(get("maxOutputTokens"), "maxOutputTokens")?;
    let timeout_ms = positive(get("timeoutMs"), "timeoutMs")?;
    if positive(get("maxInputTokens"), "maxInputTokens")?.is_some() {
        return Err(invalid(
            "unsupported_budget",
            "maxInputTokens needs measured prompt tokens",
        ));
    }
    let provider = field(selected, "provider")
        .as_string()
        .ok_or_else(|| invalid("invalid_request", "Selected provider must be a string"))?;
    let name = field(selected, "model")
        .as_string()
        .ok_or_else(|| invalid("invalid_request", "Selected model must be a string"))?;
    let cap = model
        .providers
        .get_capabilities(provider, name)
        .map_err(module_error)?;
    let output = max_output.or_else(|| field(raw, "maxOutputTokens").as_number());
    if output.is_some_and(|n| {
        !safe_positive(n)
            || n > field(&cap, "maxOutputTokens")
                .as_number()
                .unwrap_or(f64::NAN)
    }) {
        return Err(invalid(
            "invalid_budget",
            "maxOutputTokens exceeds selected model limit",
        ));
    }
    if field(raw, "maxOutputTokens")
        .as_number()
        .zip(max_output)
        .is_some_and(|(request, budget)| request > budget)
    {
        return Err(invalid(
            "invalid_budget",
            "request maxOutputTokens exceeds budget",
        ));
    }
    let mut canonical = request.clone();
    canonical.insert("provider".into(), field(selected, "provider").clone());
    canonical.insert("model".into(), field(selected, "model").clone());
    if let Some(output) = output {
        canonical.insert("maxOutputTokens".into(), JsValue::Number(output));
    }
    let canonical = JsValue::Object(canonical);
    validate_messages(field(&canonical, "messages"))?;
    validate_model_request(&canonical, &model.config)?;
    Ok(Prepared {
        body: object([
            ("requestId", field(input, "requestId").clone()),
            (
                "selection",
                object([
                    ("requestedModelId", requested.clone()),
                    ("selectedModelId", field(selected, "model").clone()),
                    ("providerId", field(selected, "provider").clone()),
                ]),
            ),
            ("request", canonical.clone()),
        ]),
        request: canonical,
        timeout_ms,
    })
}
fn value_response(status: u16, body: JsValue) -> HostResponse {
    HostResponse {
        status,
        headers: vec![],
        body: HostBody::Value(body),
        raw_body: None,
    }
}
fn request_error_response(error: ModelStreamError) -> HostResponse {
    match error {
        ModelStreamError::Request {
            code,
            message,
            details,
        } => {
            let mut body = object([
                ("code", JsValue::String(code)),
                ("message", JsValue::String(message)),
            ])
            .as_object()
            .unwrap()
            .clone();
            if let Some(details) = details {
                body.insert("details".into(), *details);
            }
            value_response(400, JsValue::Object(body))
        }
        ModelStreamError::Provider(body) => value_response(
            body.get("status")
                .and_then(JsValue::as_number)
                .map_or(502, |n| n as u16),
            JsValue::Object(body),
        ),
        ModelStreamError::Thrown { name, message } => value_response(
            502,
            object([
                ("code", JsValue::String(name)),
                ("message", JsValue::String(message)),
            ]),
        ),
    }
}
async fn cancellation(signal: &Option<AbortFlag>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => pending().await,
    }
}
struct AbortOnDrop(AbortFlag);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
fn stream_response(
    prepared: Prepared,
    providers: Arc<ModelInvocationProviderRegistry>,
    generation: Arc<RuntimeLease>,
    signal: Option<AbortFlag>,
) -> HostResponse {
    let headers = vec![
        ("content-type".into(), "application/x-ndjson".into()),
        (
            "x-pilotdeck-model-id".into(),
            field(field(&prepared.body, "selection"), "selectedModelId")
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap_or_else(|_| {
                    String::from_utf16_lossy(
                        field(field(&prepared.body, "selection"), "selectedModelId")
                            .as_string()
                            .unwrap()
                            .units(),
                    )
                }),
        ),
        (
            "x-pilotdeck-provider-id".into(),
            field(field(&prepared.body, "selection"), "providerId")
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap_or_else(|_| {
                    String::from_utf16_lossy(
                        field(field(&prepared.body, "selection"), "providerId")
                            .as_string()
                            .unwrap()
                            .units(),
                    )
                }),
        ),
    ];
    let (sender, receiver) = mpsc::channel(1);
    tokio::spawn(async move {
        // A provider call returning its headers must not retire this generation.
        let _generation = generation;
        let child = AbortFlag::new();
        let _abort = AbortOnDrop(child.clone());
        let gate = YieldGate::new();
        let options = StreamOptions {
            abort: Some(child.clone()),
            stream_timeout_ms: prepared.timeout_ms,
            yield_gate: Some(gate.clone()),
            ..Default::default()
        };
        let mut request = prepared.request.as_object().unwrap().clone();
        request.insert("stream".into(), JsValue::Bool(true));
        let request = JsValue::Object(request);
        let (events, mut queue) = mpsc::unbounded_channel();
        let delivery = gate.clone();
        let mut sink = move |event| {
            if let StreamOutput::Event(event) = event {
                delivery.deliver();
                let _ = events.send(event);
            }
        };
        let mut future = providers.stream(&request, &options, &mut sink);
        let mut was_cancelled = false;
        'streaming: loop {
            tokio::select! {
                _=sender.closed()=>break,
                ()=cancellation(&signal)=>{was_cancelled=true;break},
                event=queue.recv()=> {
                    if let Some(event)=event {
                        tokio::select! {
                            result=sender.send(Ok(HostEvent::Value(event)))=>if result.is_err(){break 'streaming},
                            ()=cancellation(&signal)=>{was_cancelled=true;break 'streaming},
                        }
                        gate.pull();
                    }
                },
                result=&mut future=> {
                    // Registered providers may produce a final batch without a
                    // yield gate. Preserve all events before the final outcome.
                    while let Ok(event)=queue.try_recv() {
                        tokio::select! {
                            result=sender.send(Ok(HostEvent::Value(event)))=>if result.is_err(){break 'streaming},
                            ()=cancellation(&signal)=>{was_cancelled=true;break 'streaming},
                        }
                    }
                    if let Err(error)=result {
                        let message=String::from_utf16_lossy(error.message().units());
                        tokio::select! { _=sender.send(Err(message))=>{}, ()=cancellation(&signal)=>{was_cancelled=true} }
                    }
                    break;
                }
            }
        }
        child.abort();
        // Drop the provider/network future before waiting for the body consumer.
        drop(future);
        if was_cancelled {
            // Once headers exist, cancellation is a failed body, not a successful
            // empty EOF. The HTTP transport closes the response on this error.
            let _ = sender.send(Err("This operation was aborted".into())).await;
        }
    });
    HostResponse {
        status: 200,
        headers,
        body: HostBody::Stream(receiver),
        raw_body: None,
    }
}
/// `catalog.items` must come from this lease's config, before list-catalog's
/// public `{data,...}` envelope. The root owns principal/project selection.
pub(crate) async fn call(
    input: &JsValue,
    catalog: &JsValue,
    generation: Arc<RuntimeLease>,
    stream: bool,
    signal: Option<AbortFlag>,
) -> Result<HostResponse, HostFailure> {
    if signal.as_ref().is_some_and(AbortFlag::is_aborted) {
        return Ok(value_response(
            499,
            object([("code", "PUBLIC_HOST_CANCELLED".into())]),
        ));
    }
    let resource = generation
        .runtime()
        .resources()
        .get::<ProjectResources>()
        .and_then(|resources| resources.context().resource(Resource::Model))
        .ok_or_else(|| HostFailure {
            status: Some(502),
            code: Some("PUBLIC_HOST_RUNTIME_INVALID".into()),
            message: Some("Selected project generation has no model resource.".into()),
        })?;
    let model = resource
        .as_any()
        .downcast_ref::<ModelResources>()
        .ok_or_else(|| HostFailure {
            status: Some(502),
            code: Some("PUBLIC_HOST_RUNTIME_INVALID".into()),
            message: Some("Selected model resource is not native.".into()),
        })?;
    match prepare(input, catalog, model) {
        Ok(prepared) if stream => Ok(stream_response(
            prepared,
            model.providers.clone(),
            generation,
            signal,
        )),
        Ok(prepared) => Ok(value_response(200, prepared.body)),
        Err(error) => Ok(request_error_response(error)),
    }
}
#[cfg(test)]
#[path = "public_host_model_tests.rs"]
mod tests;

fn array(value: &JsValue) -> Option<&[JsValue]> {
    match value {
        JsValue::Array(items) => Some(items),
        _ => None,
    }
}

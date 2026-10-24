//! Protocol dispatch for provider requests: `validateModelRequest.ts`,
//! `buildModelRequest.ts`, `normalizeStreamEvent.ts`, `providerEndpoint.ts`
//! (chat endpoints), `sanitizeProviderBody.ts` and `buildProviderHeaders`.
//!
//! Anthropic, OpenAI chat-completions, OpenAI Responses and Google requests
//! share validation and thinking plans. Google streams use their SDK-specific
//! attempt loop in `http.rs`; the other protocols use `StreamState` below.
pub(crate) mod anthropic;
pub(crate) mod google;
pub(crate) mod openai;
pub(crate) mod openai_responses;
pub(crate) mod thinking;

use super::count_body::{array, field, is, present, truthy};
use super::http::ModelStreamError;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::LazyLock;

fn units(value: &JsValue) -> Vec<u16> {
    crate::js_value::to_string_units(value).unwrap_or_default()
}

/// UTF-16 JSON text as a Rust string for `parse_json`. A lone surrogate is
/// only legal inside a JSON string, where `\uXXXX` denotes the same unit; one
/// that would follow an escaping backslash stays invalid (U+FFFD), and
/// anywhere else both forms are a syntax error, as in `JSON.parse`. Shared by
/// the Anthropic and OpenAI tool-argument parsers (pr5).
pub(crate) fn json_text(units: &[u16]) -> String {
    let mut out = String::with_capacity(units.len());
    let mut backslashes = 0usize;
    for item in char::decode_utf16(units.iter().copied()) {
        match item {
            Ok(c) => {
                out.push(c);
                backslashes = if c == '\\' { backslashes + 1 } else { 0 };
            }
            Err(error) => {
                if backslashes % 2 == 1 {
                    out.push('\u{FFFD}');
                } else {
                    out.push_str(&format!("\\u{:04x}", error.unpaired_surrogate()));
                }
                backslashes = 0;
            }
        }
    }
    out
}

/// `JSON.parse(text)`, else `JSON.parse(jsonrepair(text))`, as every stream
/// normalizer parses tool-call arguments (anthropic, openai, openai-responses).
/// The bool is `wasRepaired`; `None` when both parses fail. Callers handle
/// their own empty-buffer default first.
pub(crate) fn parse_tool_arguments(units: &[u16]) -> Option<(JsValue, bool)> {
    let parse = |units: &[u16]| pilotdeck_protocol::parse_json(&json_text(units)).ok();
    if let Some(input) = parse(units) {
        return Some((input, false));
    }
    let repaired = crate::runtime::model::jsonrepair::jsonrepair(units).ok()?;
    parse(&repaired).map(|input| (input, true))
}

fn message(parts: &[(&str, Option<&JsValue>)]) -> JsString {
    let mut out = Vec::new();
    for (literal, value) in parts {
        out.extend(literal.encode_utf16());
        if let Some(value) = value {
            out.extend(units(value));
        }
    }
    JsString::from_utf16(out)
}
fn request_error(code: &str, message: JsString) -> ModelStreamError {
    ModelStreamError::Request {
        code: code.into(),
        message,
        details: None,
    }
}
fn request_error_details(
    code: &str,
    message: JsString,
    fields: &[(&str, JsValue)],
) -> ModelStreamError {
    let mut raw = JsObject::new();
    for (key, value) in fields {
        raw.insert((*key).into(), value.clone());
    }
    ModelStreamError::Request {
        code: code.into(),
        message,
        details: Some(Box::new(JsValue::Object(raw))),
    }
}
fn type_error(reading: &str) -> ModelStreamError {
    ModelStreamError::Thrown {
        name: "TypeError".into(),
        message: format!("Cannot read properties of undefined (reading '{reading}')").into(),
    }
}
/// Own-property lookup by a JS key (`String(key)`).
fn own<'a>(object: &'a JsValue, key: &JsValue) -> Option<&'a JsValue> {
    object
        .as_object()?
        .get_key(&JsString::from_utf16(units(key)))
        .filter(|value| truthy(value))
}

/// `validateModelRequest(request, config)`; returns `(provider, model)`.
///
/// Known gap: `config.providers[...]`/`provider.models[...]` are own-property
/// lookups here; the baseline also sees `Object.prototype` members.
/// Ledger: specs/gateway-parity/accepted-diffs.md A6.4-MODEL-PROTOTYPE-001 (unapproved).
pub(crate) fn validate_model_request(
    request: &JsValue,
    config: &JsValue,
) -> Result<(JsValue, JsValue), ModelStreamError> {
    let provider_key = field(request, "provider");
    let model_key = field(request, "model");
    let Some(provider) = own(field(config, "providers"), provider_key) else {
        return Err(request_error(
            "provider_not_found",
            message(&[
                ("Provider ", Some(provider_key)),
                (" does not exist.", None),
            ]),
        ));
    };
    let speed = field(request, "speed");
    if !speed.is_undefined()
        && !speed
            .as_number()
            .is_some_and(|s| s.is_finite() && (0.0..=1.0).contains(&s))
    {
        return Err(request_error(
            "invalid_speed",
            "speed must be between 0 and 1.".into(),
        ));
    }
    let Some(model) = own(field(provider, "models"), model_key) else {
        return Err(request_error(
            "model_not_found",
            message(&[
                ("Model ", Some(model_key)),
                (" does not exist in provider ", Some(provider_key)),
                (".", None),
            ]),
        ));
    };
    let capabilities = field(model, "capabilities");
    let capability = |name: &'static str| -> Result<bool, ModelStreamError> {
        if !present(capabilities) {
            return Err(type_error(name));
        }
        Ok(truthy(field(capabilities, name)))
    };
    if !speed.is_undefined()
        && (field(capabilities, "supportsSpeed") != &JsValue::Bool(true)
            || field(provider, "speedMapping").is_undefined())
    {
        if !present(capabilities) {
            return Err(type_error("supportsSpeed"));
        }
        return Err(request_error(
            "unsupported_speed",
            message(&[
                ("Model ", Some(model_key)),
                (" does not support speed for provider ", Some(provider_key)),
                (".", None),
            ]),
        ));
    }
    if truthy(field(request, "stream")) && !capability("supportsStreaming")? {
        return Err(request_error(
            "unsupported_streaming",
            message(&[
                ("Model ", Some(model_key)),
                (" does not support streaming.", None),
            ]),
        ));
    }
    if truthy(field(request, "systemPrompt")) && !capability("supportsSystemPrompt")? {
        return Err(request_error(
            "unsupported_system_prompt",
            message(&[
                ("Model ", Some(model_key)),
                (" does not support system prompts.", None),
            ]),
        ));
    }
    let has_tools = match field(request, "tools") {
        JsValue::Array(tools) => !tools.is_empty(),
        _ => false,
    };
    if has_tools && !capability("supportsToolUse")? {
        return Err(request_error(
            "unsupported_tool_use",
            message(&[
                ("Model ", Some(model_key)),
                (" does not support tools.", None),
            ]),
        ));
    }
    let multimodal = field(model, "multimodal");
    for message in array(field(request, "messages")) {
        if !present(multimodal) {
            return Err(type_error("input"));
        }
        assert_content_supported(array(field(message, "content")), multimodal)?;
    }
    Ok((provider.clone(), model.clone()))
}

fn number(value: &JsValue) -> Option<f64> {
    value.as_number()
}
/// `constraints.maxX && value && value > constraints.maxX`.
fn exceeds(limit: &JsValue, value: &JsValue) -> bool {
    truthy(limit)
        && truthy(value)
        && matches!((number(value), number(limit)), (Some(v), Some(l)) if v > l)
}

/// `assertContentSupported(blocks, constraints)`.
fn assert_content_supported(
    blocks: &[JsValue],
    constraints: &JsValue,
) -> Result<(), ModelStreamError> {
    let allowed = array(field(constraints, "input"));
    let allows = |modality: &str| allowed.iter().any(|m| is(m, modality));
    let unsupported = |modality: &str| {
        request_error_details(
            "unsupported_modality",
            format!("Model does not support {modality} input.")
                .as_str()
                .into(),
            &[("modality", modality.into())],
        )
    };
    let mime_types = field(constraints, "supportedImageMimeTypes");
    let check_image = |block: &JsValue| -> Result<(), ModelStreamError> {
        let mime = field(block, "mimeType");
        if truthy(mime_types) && !array(mime_types).iter().any(|m| m == mime) {
            return Err(request_error_details(
                "unsupported_image_mime_type",
                message(&[
                    ("Model does not support image MIME type ", Some(mime)),
                    (".", None),
                ]),
                &[("mimeType", mime.clone())],
            ));
        }
        if exceeds(field(constraints, "maxImageBytes"), field(block, "bytes")) {
            return Err(request_error_details(
                "image_too_large",
                "Image content exceeds model limits.".into(),
                &[
                    ("bytes", field(block, "bytes").clone()),
                    ("maxImageBytes", field(constraints, "maxImageBytes").clone()),
                ],
            ));
        }
        Ok(())
    };
    let check_pdf = |block: &JsValue| -> Result<(), ModelStreamError> {
        let max = field(constraints, "maxPdfBytes");
        if truthy(max)
            && matches!((number(field(block, "bytes")), number(max)), (Some(v), Some(l)) if v > l)
        {
            return Err(request_error_details(
                "pdf_too_large",
                "PDF content exceeds model limits.".into(),
                &[
                    ("bytes", field(block, "bytes").clone()),
                    ("maxPdfBytes", max.clone()),
                ],
            ));
        }
        Ok(())
    };
    let mut images = 0usize;
    for block in blocks {
        let tag = field(block, "type");
        if is(tag, "tool_result") {
            let items = array(field(block, "content"));
            for item in items {
                for modality in ["image", "pdf"] {
                    if is(field(item, "type"), modality) && !allows(modality) {
                        return Err(unsupported(modality));
                    }
                }
            }
            for item in items {
                if is(field(item, "type"), "image") {
                    images += 1;
                    check_image(item)?;
                }
                if is(field(item, "type"), "pdf") {
                    check_pdf(item)?;
                }
            }
            continue;
        }
        let Some(modality) = ["text", "image", "pdf", "audio"]
            .into_iter()
            .find(|m| is(tag, m))
        else {
            continue;
        };
        if !allows(modality) {
            return Err(unsupported(modality));
        }
        match modality {
            "image" => {
                images += 1;
                check_image(block)?;
            }
            "pdf" => check_pdf(block)?,
            "audio"
                if exceeds(
                    field(constraints, "maxAudioSeconds"),
                    field(block, "durationSeconds"),
                ) =>
            {
                return Err(request_error_details(
                    "audio_too_long",
                    "Audio content exceeds model limits.".into(),
                    &[
                        ("durationSeconds", field(block, "durationSeconds").clone()),
                        (
                            "maxAudioSeconds",
                            field(constraints, "maxAudioSeconds").clone(),
                        ),
                    ],
                ));
            }
            _ => {}
        }
    }
    let max_images = field(constraints, "maxImagesPerRequest");
    if truthy(max_images) && number(max_images).is_some_and(|max| images as f64 > max) {
        return Err(request_error_details(
            "too_many_images",
            "Image count exceeds model limits.".into(),
            &[
                ("imageCount", JsValue::Number(images as f64)),
                ("maxImagesPerRequest", max_images.clone()),
            ],
        ));
    }
    Ok(())
}

/// `buildModelRequest` for an already validated `(provider, model)`.
pub(crate) fn build_model_request(
    request: &JsValue,
    provider: &JsValue,
    model: &JsValue,
) -> Result<JsValue, ModelStreamError> {
    if is(field(provider, "protocol"), "anthropic") {
        return anthropic::build_request(request, model, provider);
    }
    if is(field(provider, "protocol"), "openai-responses") {
        return openai_responses::build_request(request, model, provider);
    }
    if is(field(provider, "protocol"), "google") {
        return google::build_request(request, model);
    }
    // `buildModelRequest` falls through to the OpenAI request builder.
    openai::build_request(request, model, provider)
}

/// `sanitizeProviderBody({ ...body, ...provider.extraBody })`.
pub(crate) fn final_body(body: &JsValue, provider: &JsValue) -> JsValue {
    let mut out = body.as_object().cloned().unwrap_or_default();
    if let Some(extra) = field(provider, "extraBody").as_object() {
        for (key, value) in extra.own_entries() {
            out.insert(key.clone(), value.clone());
        }
    }
    let mut result = JsObject::new();
    for (key, value) in out.own_entries() {
        if key.equals_str("temperature") {
            continue;
        }
        let value = match value {
            JsValue::Object(config) if key.equals_str("generationConfig") => {
                let mut kept = JsObject::new();
                for (k, v) in config.own_entries() {
                    if !k.equals_str("temperature") {
                        kept.insert(k.clone(), v.clone());
                    }
                }
                JsValue::Object(kept)
            }
            other => other.clone(),
        };
        result.insert(key.clone(), value);
    }
    JsValue::Object(result)
}

/// `buildProviderHeaders(provider, body)`, in record insertion order. The
/// transport combines case-insensitive duplicates like `new Headers(record)`.
pub(crate) fn provider_headers(provider: &JsValue, body: &JsValue) -> Vec<(JsString, JsValue)> {
    let mut headers = JsObject::new();
    headers.insert("content-type".into(), "application/json".into());
    if let Some(custom) = field(provider, "headers").as_object() {
        for (name, value) in custom.own_entries() {
            headers.insert(name.clone(), value.clone());
        }
    }
    let key = String::from_utf16_lossy(&units(field(provider, "apiKey")));
    let key = crate::metadata::js_trim(&key);
    if is(field(provider, "protocol"), "anthropic") {
        headers.insert("x-api-key".into(), key.into());
        if !present(
            headers
                .get("anthropic-version")
                .unwrap_or(&JsValue::Undefined),
        ) {
            headers.insert("anthropic-version".into(), "2023-06-01".into());
        }
        if is(field(provider, "speedMapping"), "anthropic_speed")
            && is(field(body, "speed"), "fast")
        {
            append_anthropic_beta(&mut headers, "fast-mode-2026-02-01");
        }
    } else if !present(headers.get("authorization").unwrap_or(&JsValue::Undefined)) {
        headers.insert(
            "authorization".into(),
            format!("Bearer {key}").as_str().into(),
        );
    }
    headers
        .own_entries()
        .into_iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn append_anthropic_beta(headers: &mut JsObject, beta: &str) {
    let key = headers
        .own_entries()
        .into_iter()
        .map(|(k, _)| k.clone())
        .find(|k| String::from_utf16_lossy(k.units()).to_lowercase() == "anthropic-beta")
        .unwrap_or_else(|| "anthropic-beta".into());
    let current = headers
        .get_key(&key)
        .map(|v| String::from_utf16_lossy(&units(v)))
        .unwrap_or_default();
    let mut values: Vec<String> = current
        .split(',')
        .map(|v| crate::metadata::js_trim(v).to_string())
        .filter(|v| !v.is_empty())
        .collect();
    if !values.iter().any(|v| v == beta) {
        values.push(beta.to_string());
    }
    headers.insert(key, values.join(", ").as_str().into());
}

/// `buildProviderChatEndpointCandidates` for the ported protocols.
pub(crate) fn chat_endpoint_candidates(provider: &JsValue) -> Vec<String> {
    // TS model/providerEndpoint.ts:54 calls baseUrl.trim() for every ported
    // protocol. Node 22.22.0: direct calls with 123 / {} / null throw TypeError.
    // Rust keeps String(url) coercion here. Runtime config excludes those
    // values: model/config/parseModelConfig.ts:91 uses a string/catalog default,
    // mirrored by config/snapshot.rs:644-671 (as_string + catalog default).
    let base = String::from_utf16_lossy(&units(field(provider, "url")));
    let protocol = field(provider, "protocol");
    if is(protocol, "anthropic") {
        return endpoint_candidates(&base, "v1", "messages");
    }
    if is(protocol, "openai-responses") {
        return endpoint_candidates(&base, "v1", "responses");
    }
    endpoint_candidates(&base, "v1", "chat/completions")
}

fn path_segments(base: &str) -> Vec<String> {
    url::Url::parse(base)
        .map(|u| {
            u.path()
                .split('/')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

static VERSIONED_SEGMENT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[vV][0-9]+(?:[bB][eE][tT][aA][0-9]*)?$").expect("static regex")
});

/// `buildEndpointCandidates(baseUrl, defaultVersion, endpointPath)`.
fn endpoint_candidates(base: &str, version: &str, endpoint: &str) -> Vec<String> {
    let normalized = crate::metadata::js_trim(base).trim_end_matches('/');
    let endpoint = endpoint.trim_matches('/');
    let segments = path_segments(normalized);
    let wanted: Vec<String> = endpoint
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect();
    let lowered: Vec<String> = segments.iter().map(|s| s.to_lowercase()).collect();
    if lowered.len() >= wanted.len() && lowered[lowered.len() - wanted.len()..] == wanted[..] {
        return vec![normalized.to_string()];
    }
    let join = |path: &str| {
        format!(
            "{}/{}",
            normalized.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    };
    let unversioned = join(endpoint);
    if segments.iter().any(|s| VERSIONED_SEGMENT.is_match(s)) {
        return vec![unversioned];
    }
    let versioned = join(&format!("{version}/{endpoint}"));
    let mut out = Vec::new();
    for url in [versioned, unversioned] {
        if !url.is_empty() && !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

/// `isExpectedProviderResponseShape(protocol, body)` for the non-streaming
/// `complete()` path (`providerEndpoint.ts:130-143`). The caller only consults
/// this for an OK response with more than one endpoint candidate; streaming
/// accepts every OK response and single-endpoint calls never inspect the body.
/// Google is not a `complete()` protocol (it stays `UnsupportedProtocol`).
pub(crate) fn expected_complete_response_shape(protocol: &JsValue, body: &JsValue) -> bool {
    let JsValue::Object(record) = body else {
        return false;
    };
    if is(protocol, "anthropic") {
        return matches!(record.get("content"), Some(JsValue::Array(_)))
            || matches!(
                record.get("type"),
                Some(JsValue::String(value)) if value.equals_str("message")
            );
    }
    if is(protocol, "openai-responses") {
        return matches!(
            record.get("object"),
            Some(JsValue::String(value)) if value.equals_str("response")
        ) || matches!(record.get("output"), Some(JsValue::Array(_)))
            || matches!(record.get("output_text"), Some(JsValue::String(_)));
    }
    matches!(record.get("choices"), Some(JsValue::Array(_)))
}

/// `normalizeProviderBaseUrl(raw)`.
pub(crate) fn normalize_provider_base_url(raw: &JsValue) -> JsValue {
    let raw = String::from_utf16_lossy(&units(raw));
    let trimmed = crate::metadata::js_trim(&raw);
    if trimmed.is_empty() {
        return JsValue::Undefined;
    }
    let Ok(mut parsed) = url::Url::parse(trimmed) else {
        return JsValue::Undefined;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return JsValue::Undefined;
    }
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    parsed.set_query(None);
    parsed.set_fragment(None);
    let serialized = parsed.as_str();
    serialized.strip_suffix('/').unwrap_or(serialized).into()
}

/// Per-protocol stream normalizer state (`createStreamNormalizerState`).
pub(crate) enum StreamState {
    Anthropic(anthropic::StreamState),
    OpenAi(openai::StreamState),
    OpenAIResponses(openai_responses::StreamState),
}
impl StreamState {
    pub(crate) fn new(provider: &JsValue) -> Self {
        let protocol = field(provider, "protocol");
        if is(protocol, "anthropic") {
            Self::Anthropic(anthropic::StreamState::default())
        } else if is(protocol, "openai-responses") {
            Self::OpenAIResponses(openai_responses::StreamState::default())
        } else {
            Self::OpenAi(openai::StreamState::default())
        }
    }
    /// `Err` is what the TS normalizer throws (V8 string-limit `RangeError`,
    /// or the OpenAI tool-argument parse); the caller discards that chunk's
    /// events, as the baseline generator does.
    pub(crate) fn normalize(&mut self, raw: &JsValue) -> Result<Vec<JsValue>, ModelStreamError> {
        match self {
            Self::Anthropic(state) => anthropic::normalize_stream_event(raw, state),
            Self::OpenAi(state) => openai::normalize_stream_event(raw, state),
            Self::OpenAIResponses(state) => openai_responses::normalize_stream_event(raw, state),
        }
    }
}

/// `isExpectedProviderResponseShape` is only consulted for non-stream calls;
/// streaming always accepts an OK response.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_non_string_url_preserves_direct_call_coercion() {
        for (source, coerced) in [("123", "123"), ("{}", "[object Object]"), ("null", "null")] {
            for (protocol, endpoint) in [
                ("anthropic", "messages"),
                ("openai-responses", "responses"),
                ("openai", "chat/completions"),
            ] {
                let provider = pilotdeck_protocol::parse_json(&format!(
                    r#"{{"protocol":"{protocol}","url":{source}}}"#
                ))
                .unwrap();
                assert_eq!(
                    chat_endpoint_candidates(&provider),
                    [
                        format!("{coerced}/v1/{endpoint}"),
                        format!("{coerced}/{endpoint}")
                    ],
                    "{protocol} url={source}"
                );
            }
        }
    }

    #[test]
    fn anthropic_endpoint_candidates_follow_version_segments() {
        assert_eq!(
            endpoint_candidates("http://h/v1/", "v1", "messages"),
            ["http://h/v1/messages"]
        );
        assert_eq!(
            endpoint_candidates(" http://h ", "v1", "messages"),
            ["http://h/v1/messages", "http://h/messages"]
        );
        assert_eq!(
            endpoint_candidates("http://h/api/V2beta3", "v1", "messages"),
            ["http://h/api/V2beta3/messages"]
        );
        assert_eq!(
            endpoint_candidates("http://h/v1/Messages", "v1", "messages"),
            ["http://h/v1/Messages"]
        );
        assert_eq!(
            endpoint_candidates("not a url", "v1", "messages"),
            ["not a url/v1/messages", "not a url/messages"]
        );
    }
}

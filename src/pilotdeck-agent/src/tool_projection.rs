//! Source: fixed original ensureToolResultPairing/projectToolResults/result.ts.
//! Captures are immutable echoes. This module has no tool or permission authority.
use crate::model_event_reducer::{matches_fields, object, optional_string, same_value};
use crate::{
    MessageProjection, ProjectedMessage, ProjectionError, ProjectionGap, SupplementalToolMessage,
    ToolContent, ToolExecutionResult, ToolResultDisposition,
};
use pilotdeck_protocol::model::{
    BoundarySnapshot, ContentBlock, ContentBody, ImageBlock, ImageDetail, MediaSource, Message,
    MessageMetadata, PdfBlock, ProviderEcho, Role, TextBlock, ToolCall, ToolResult,
    ToolResultContent, ToolResultItem,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

pub(crate) fn pair_results(
    calls: &[ToolCall],
    results: Vec<ToolExecutionResult>,
    missing: &mut dyn FnMut(&ToolCall) -> ToolExecutionResult,
) -> Vec<ToolExecutionResult> {
    let mut queues: HashMap<JsString, VecDeque<ToolExecutionResult>> = HashMap::new();
    for result in results {
        queues
            .entry(result.tool_call_id.clone())
            .or_default()
            .push_back(result);
    }
    calls
        .iter()
        .map(|call| {
            queues
                .get_mut(&call.id)
                .and_then(VecDeque::pop_front)
                .unwrap_or_else(|| missing(call))
        })
        .collect()
}
fn invalid(message: &'static str) -> ProjectionError {
    ProjectionError::InvalidBoundary {
        message: message.into(),
    }
}
fn snapshot(content: &ToolContent) -> &BoundarySnapshot {
    match content {
        ToolContent::Text { boundary, .. }
        | ToolContent::Json { boundary, .. }
        | ToolContent::Image { boundary, .. }
        | ToolContent::Pdf { boundary, .. }
        | ToolContent::File { boundary, .. }
        | ToolContent::BoundaryOnly { boundary } => boundary,
    }
}
fn optional_number(value: Option<f64>) -> JsValue {
    value.map_or(JsValue::Undefined, JsValue::Number)
}
fn detail(value: Option<ImageDetail>) -> JsValue {
    value.map_or(JsValue::Undefined, |v| {
        JsValue::from(match v {
            ImageDetail::Auto => "auto",
            ImageDetail::Low => "low",
            ImageDetail::High => "high",
        })
    })
}
fn validate_content(content: &ToolContent) -> Result<(), ProjectionError> {
    let expected = match content {
        ToolContent::Text { text, .. } => object([
            ("type", JsValue::from("text")),
            ("text", JsValue::String(text.clone())),
        ]),
        ToolContent::Json { value, .. } => {
            object([("type", JsValue::from("json")), ("value", value.clone())])
        }
        ToolContent::Image { image, .. } => {
            if image.source != MediaSource::Base64 {
                return Err(invalid("Tool image normalized source must be base64"));
            }
            object([
                ("type", JsValue::from("image")),
                ("mimeType", JsValue::String(image.mime_type.clone())),
                ("data", JsValue::String(image.data.clone())),
                ("bytes", optional_number(image.bytes)),
                ("detail", detail(image.detail)),
            ])
        }
        ToolContent::Pdf { pdf, .. } => object([
            ("type", JsValue::from("pdf")),
            ("mimeType", JsValue::from("application/pdf")),
            ("data", JsValue::String(pdf.data.clone())),
            ("bytes", JsValue::Number(pdf.bytes)),
            ("pages", optional_number(pdf.pages)),
        ]),
        ToolContent::File {
            path,
            mime_type,
            description,
            ..
        } => object([
            ("type", JsValue::from("file")),
            ("path", JsValue::String(path.clone())),
            ("mimeType", optional_string(mime_type.as_ref())),
            ("description", optional_string(description.as_ref())),
        ]),
        ToolContent::BoundaryOnly { boundary } => {
            if boundary.value().as_object().is_none() {
                return Err(invalid("Tool content must be an admitted object"));
            }
            return Ok(());
        }
    };
    if !matches_fields(snapshot(content).value(), &expected) {
        return Err(invalid(
            "Tool content contradicts its normalized projection",
        ));
    }
    Ok(())
}
fn validate_array(boundary: &JsValue, contents: &[ToolContent]) -> Result<(), ProjectionError> {
    let JsValue::Array(array) = boundary else {
        return Err(invalid("Tool content must be an array"));
    };
    if array.len() != contents.len() {
        return Err(invalid("Tool content boundary count differs"));
    }
    for (raw, content) in array.iter().zip(contents) {
        validate_content(content)?;
        if !same_value(raw, snapshot(content).value()) {
            return Err(invalid(
                "Tool content snapshot differs from parent boundary",
            ));
        }
    }
    Ok(())
}
fn validate_supplemental(
    message: &SupplementalToolMessage,
    raw: &JsValue,
) -> Result<(), ProjectionError> {
    if !same_value(message.boundary.value(), raw) {
        return Err(invalid("Supplemental message snapshot differs"));
    }
    validate_array(
        raw.get("content")
            .ok_or_else(|| invalid("Supplemental message content missing"))?,
        &message.content,
    )
}
fn validate_result(result: &ToolExecutionResult) -> Result<(), ProjectionError> {
    let raw = result.boundary.value();
    let mut expected = object([
        (
            "type",
            JsValue::from(match result.disposition {
                ToolResultDisposition::Success { .. } => "success",
                ToolResultDisposition::Error { .. } => "error",
            }),
        ),
        ("toolCallId", JsValue::String(result.tool_call_id.clone())),
        ("toolName", JsValue::String(result.tool_name.clone())),
        ("startedAt", JsValue::String(result.started_at.clone())),
        ("completedAt", JsValue::String(result.completed_at.clone())),
        (
            "metadata",
            result
                .metadata
                .as_ref()
                .map_or(JsValue::Undefined, |v| JsValue::Object(v.clone())),
        ),
    ]);
    if let JsValue::Object(fields) = &mut expected {
        // The normalized optional echo has no object view for null. The
        // complete boundary retains its exact own null/Undefined/absence.
        if result.metadata.is_none() && matches!(raw.get("metadata"), Some(JsValue::Null)) {
            fields.insert("metadata".into(), JsValue::Null);
        }
        match &result.disposition {
            ToolResultDisposition::Success { data } => {
                fields.insert("data".into(), data.clone().unwrap_or(JsValue::Undefined));
            }
            ToolResultDisposition::Error {
                code,
                message,
                fields: extensions,
            } => {
                let mut error = extensions.clone();
                error.insert("code".into(), JsValue::String(code.clone()));
                error.insert("message".into(), JsValue::String(message.clone()));
                fields.insert("error".into(), JsValue::Object(error));
            }
        }
    }
    if !matches_fields(raw, &expected) {
        return Err(invalid("Tool result contradicts its normalized projection"));
    }
    validate_array(
        raw.get("content")
            .ok_or_else(|| invalid("Tool result content missing"))?,
        &result.content,
    )?;
    match (
        &result.supplemental_messages,
        raw.get("supplementalMessages"),
    ) {
        (Some(messages), Some(JsValue::Array(array))) if messages.len() == array.len() => {
            for (message, raw) in messages.iter().zip(array) {
                validate_supplemental(message, raw)?;
            }
        }
        (None, None | Some(JsValue::Undefined | JsValue::Null)) => (),
        _ => {
            return Err(invalid(
                "Supplemental message admission differs from boundary",
            ));
        }
    }
    Ok(())
}
fn field(value: &JsValue, key: &str) -> JsValue {
    value.get(key).cloned().unwrap_or(JsValue::Undefined)
}
fn kind(value: &JsValue, expected: &str) -> bool {
    value
        .get("type")
        .and_then(JsValue::as_string)
        .is_some_and(|v| v.equals_str(expected))
}
fn json_text(value: &JsValue) -> Result<JsValue, ProjectionError> {
    stringify_json(value)
        .map(|v| v.map_or(JsValue::Undefined, |v| JsValue::String(v.into())))
        .map_err(|error| ProjectionError::Codec {
            message: error.to_string().into(),
        })
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
/// String coercion is needed only for original file template literals. Captured
/// native toPrimitive hooks are deliberately outside this JSON-compatible domain.
fn template_string(value: &JsValue) -> Result<JsString, ProjectionError> {
    enum Piece<'a> {
        Value(&'a JsValue),
        Comma,
    }
    let mut pending = vec![Piece::Value(value)];
    let mut units = Vec::new();
    while let Some(piece) = pending.pop() {
        let Piece::Value(value) = piece else {
            units.push(0x2c);
            continue;
        };
        let string = match value {
            JsValue::Undefined => "undefined".into(),
            JsValue::Null => "null".into(),
            JsValue::Bool(value) => {
                if *value {
                    "true".into()
                } else {
                    "false".into()
                }
            }
            JsValue::String(value) => value.clone(),
            JsValue::Number(value) if value.is_nan() => "NaN".into(),
            JsValue::Number(value) if *value == f64::INFINITY => "Infinity".into(),
            JsValue::Number(value) if *value == f64::NEG_INFINITY => "-Infinity".into(),
            JsValue::Number(_) => match &json_text(value)? {
                JsValue::String(text) => text.clone(),
                _ => unreachable!("finite JSON number"),
            },
            JsValue::Object(object) => {
                // An own JSON-valued toString masks the inherited function;
                // valueOf cannot return a primitive for this native-free object.
                if object.contains_key("toString") {
                    return Err(ProjectionError::Codec {
                        message: "Cannot convert object to primitive value".into(),
                    });
                }
                "[object Object]".into()
            }
            JsValue::Array(values) => {
                for (index, value) in values.iter().enumerate().rev() {
                    if !matches!(value, JsValue::Undefined | JsValue::Null) {
                        pending.push(Piece::Value(value));
                    }
                    if index != 0 {
                        pending.push(Piece::Comma);
                    }
                }
                continue;
            }
        };
        units.extend_from_slice(string.units());
    }
    Ok(JsString::from_utf16(units))
}
fn content_to_text(value: &JsValue) -> Result<JsValue, ProjectionError> {
    if kind(value, "text") {
        return Ok(field(value, "text"));
    }
    if kind(value, "json") {
        return json_text(&field(value, "value"));
    }
    if kind(value, "file") {
        let mut units: Vec<u16> = "[File: ".encode_utf16().collect();
        units.extend_from_slice(template_string(&field(value, "path"))?.units());
        for name in ["mimeType", "description"] {
            let value = field(value, name);
            if truthy(&value) {
                units.extend(", ".encode_utf16());
                units.extend_from_slice(template_string(&value)?.units());
            }
        }
        units.push(0x5d);
        return Ok(JsValue::String(JsString::from_utf16(units)));
    }
    // image/pdf are converted before this helper in the original. A lax kind
    // falls through its switch, returning undefined rather than throwing.
    Ok(JsValue::Undefined)
}
fn convert_content(value: &JsValue, supplemental: bool) -> Result<JsValue, ProjectionError> {
    if kind(value, "image") {
        return Ok(object([
            ("type", JsValue::from("image")),
            ("source", JsValue::from("base64")),
            ("data", field(value, "data")),
            ("mimeType", field(value, "mimeType")),
            ("bytes", field(value, "bytes")),
            ("detail", field(value, "detail")),
        ]));
    }
    if kind(value, "pdf") {
        return Ok(object([
            ("type", JsValue::from("pdf")),
            ("source", JsValue::from("base64")),
            ("data", field(value, "data")),
            ("mimeType", field(value, "mimeType")),
            ("bytes", field(value, "bytes")),
            ("pages", field(value, "pages")),
        ]));
    }
    let text = if supplemental && !kind(value, "text") {
        json_text(value)?
    } else {
        content_to_text(value)?
    };
    Ok(object([("type", JsValue::from("text")), ("text", text)]))
}
fn sanitize_content(value: &JsValue) -> JsValue {
    if kind(value, "image") || kind(value, "pdf") || kind(value, "audio") {
        let mut result = JsObject::new();
        for (key, value) in value.as_object().expect("admitted content").entries() {
            if !key.equals_str("data") {
                result.insert(key.clone(), value.clone());
            }
        }
        JsValue::Object(result)
    } else {
        value.clone()
    }
}
fn sanitize_result(result: &ToolExecutionResult) -> JsValue {
    let mut raw = result
        .boundary
        .value()
        .as_object()
        .expect("validated result")
        .clone();
    raw.insert(
        "content".into(),
        JsValue::Array(
            result
                .content
                .iter()
                .map(|v| sanitize_content(snapshot(v).value()))
                .collect(),
        ),
    );
    if let Some(messages) = &result.supplemental_messages {
        raw.insert(
            "supplementalMessages".into(),
            JsValue::Array(
                messages
                    .iter()
                    .map(|v| {
                        let mut message = v
                            .boundary
                            .value()
                            .as_object()
                            .expect("validated supplemental")
                            .clone();
                        message.insert(
                            "content".into(),
                            JsValue::Array(
                                v.content
                                    .iter()
                                    .map(|v| sanitize_content(snapshot(v).value()))
                                    .collect(),
                            ),
                        );
                        JsValue::Object(message)
                    })
                    .collect(),
            ),
        );
    }
    JsValue::Object(raw)
}
fn optional_f64(raw: &JsValue, name: &str) -> Result<Option<f64>, ProjectionGap> {
    match raw.get(name) {
        None | Some(JsValue::Undefined) => Ok(None),
        Some(JsValue::Number(value)) => Ok(Some(*value)),
        _ => Err(ProjectionGap::LaxMediaFields),
    }
}
fn normalize_content(raw: &JsValue) -> Result<ToolResultContent, ProjectionGap> {
    if kind(raw, "text") {
        return match raw.get("text") {
            Some(JsValue::String(text)) => Ok(ToolResultContent::Text(TextBlock {
                text: text.clone(),
                block_id: None,
            })),
            Some(JsValue::Undefined) | None => Err(ProjectionGap::UndefinedCanonicalText),
            _ => Err(ProjectionGap::OtherBoundaryShape),
        };
    }
    if kind(raw, "image") {
        let Some(data) = raw.get("data").and_then(JsValue::as_string) else {
            return Err(ProjectionGap::LaxMediaFields);
        };
        let Some(mime) = raw.get("mimeType").and_then(JsValue::as_string) else {
            return Err(ProjectionGap::LaxMediaFields);
        };
        let detail = match raw.get("detail") {
            None | Some(JsValue::Undefined) => None,
            Some(JsValue::String(value)) if value.equals_str("auto") => Some(ImageDetail::Auto),
            Some(JsValue::String(value)) if value.equals_str("low") => Some(ImageDetail::Low),
            Some(JsValue::String(value)) if value.equals_str("high") => Some(ImageDetail::High),
            _ => return Err(ProjectionGap::LaxMediaFields),
        };
        return Ok(ToolResultContent::Image(ImageBlock {
            source: MediaSource::Base64,
            data: data.clone(),
            mime_type: mime.clone(),
            bytes: optional_f64(raw, "bytes")?,
            detail,
        }));
    }
    if kind(raw, "pdf") {
        let Some(data) = raw.get("data").and_then(JsValue::as_string) else {
            return Err(ProjectionGap::LaxMediaFields);
        };
        if !raw
            .get("mimeType")
            .and_then(JsValue::as_string)
            .is_some_and(|v| v.equals_str("application/pdf"))
        {
            return Err(ProjectionGap::LaxMediaFields);
        }
        let Some(bytes) = raw.get("bytes").and_then(JsValue::as_number) else {
            return Err(ProjectionGap::LaxMediaFields);
        };
        return Ok(ToolResultContent::Pdf(PdfBlock {
            data: data.clone(),
            bytes,
            pages: optional_f64(raw, "pages")?,
        }));
    }
    Err(ProjectionGap::UnmodeledContentKind)
}
fn add_gap(gaps: &mut Vec<ProjectionGap>, gap: ProjectionGap) {
    if !gaps.contains(&gap) {
        gaps.push(gap);
    }
}

pub(crate) fn project(
    results: &[ToolExecutionResult],
) -> Result<Vec<ProjectedMessage>, ProjectionError> {
    // Check every parent/child capture before any original projection work.
    for result in results {
        validate_result(result)?;
    }
    let mut aggregate = Vec::new();
    let mut aggregate_typed = Vec::new();
    let mut gaps = Vec::new();
    for result in results {
        let mut content: Vec<JsValue> = result
            .content
            .iter()
            .map(|v| convert_content(snapshot(v).value(), false))
            .collect::<Result<_, _>>()?;
        if content.is_empty() {
            content.push(object([
                ("type", JsValue::from("text")),
                ("text", JsValue::from("Tool completed with no output.")),
            ]));
        }
        let raw = sanitize_result(result);
        let is_error = matches!(result.disposition, ToolResultDisposition::Error { .. });
        aggregate.push(object([
            ("type", JsValue::from("tool_result")),
            ("toolCallId", JsValue::String(result.tool_call_id.clone())),
            (
                "isError",
                if is_error {
                    JsValue::Bool(true)
                } else {
                    JsValue::Undefined
                },
            ),
            ("content", JsValue::Array(content.clone())),
            ("raw", raw.clone()),
        ]));
        let mut typed = Vec::new();
        for block in &content {
            match normalize_content(block) {
                Ok(body) => typed.push(ToolResultItem {
                    timeline: None,
                    body,
                }),
                Err(gap) => add_gap(&mut gaps, gap),
            }
        }
        aggregate_typed.push(ContentBlock {
            timeline: None,
            body: ContentBody::ToolResult(ToolResult {
                tool_call_id: result.tool_call_id.clone(),
                content: typed,
                is_error: is_error.then_some(true),
                raw: Some(ProviderEcho(Arc::new(raw))),
            }),
        });
    }
    let boundary = BoundarySnapshot::capture(&object([
        ("role", JsValue::from("user")),
        ("content", JsValue::Array(aggregate)),
    ]));
    let normalized = if gaps.is_empty() {
        MessageProjection::Complete(Box::new(Message {
            role: Role::User,
            content: aggregate_typed,
            metadata: None,
        }))
    } else {
        MessageProjection::BoundaryOnly(gaps)
    };
    let mut messages = vec![ProjectedMessage {
        boundary,
        normalized,
    }];
    for result in results {
        if let Some(supplementals) = &result.supplemental_messages {
            for supplemental in supplementals {
                let content: Vec<JsValue> = supplemental
                    .content
                    .iter()
                    .map(|v| convert_content(snapshot(v).value(), true))
                    .collect::<Result<_, _>>()?;
                let mut typed = Vec::new();
                let mut gaps = Vec::new();
                for block in &content {
                    match normalize_content(block) {
                        Ok(body) => typed.push(ContentBlock {
                            timeline: None,
                            body: match body {
                                ToolResultContent::Text(v) => ContentBody::Text(v),
                                ToolResultContent::Image(v) => ContentBody::Image(v),
                                ToolResultContent::Pdf(v) => ContentBody::Pdf(v),
                            },
                        }),
                        Err(gap) => add_gap(&mut gaps, gap),
                    }
                }
                let metadata = object([
                    ("synthetic", JsValue::Bool(true)),
                    ("purpose", JsValue::from("tool_result_supplemental")),
                    ("toolCallId", JsValue::String(result.tool_call_id.clone())),
                ]);
                let boundary = BoundarySnapshot::capture(&object([
                    ("role", JsValue::from("user")),
                    ("content", JsValue::Array(content)),
                    ("metadata", metadata),
                ]));
                let normalized = if gaps.is_empty() {
                    MessageProjection::Complete(Box::new(Message {
                        role: Role::User,
                        content: typed,
                        metadata: Some(MessageMetadata {
                            synthetic: Some(true),
                            purpose: Some("tool_result_supplemental".into()),
                            tool_call_id: Some(result.tool_call_id.clone()),
                            ..MessageMetadata::default()
                        }),
                    }))
                } else {
                    MessageProjection::BoundaryOnly(gaps)
                };
                messages.push(ProjectedMessage {
                    boundary,
                    normalized,
                });
            }
        }
    }
    Ok(messages)
}

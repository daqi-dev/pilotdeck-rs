//! Supplementary author fixtures. Expected behavior comes from fixed b243fa0
//! projectToolResults.ts, ensureToolResultPairing.ts,
//! tool/protocol/result.ts, block-identity.spec.ts/openaiReasoningContent.spec.ts.
//! These tests do not claim independent original exported-function differential parity.
use pilotdeck_agent::source_model::{OriginalSourceModelReducer, SourceModelReducer};
use pilotdeck_agent::*;
use pilotdeck_protocol::model::*;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};

fn raw(value: &str) -> JsValue {
    parse_json(value).unwrap()
}
fn obj(fields: &[(&str, JsValue)]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert((*key).into(), value.clone());
    }
    JsValue::Object(object)
}
fn call(id: JsString) -> ToolCall {
    ToolCall {
        timeline: None,
        id,
        name: "read_file".into(),
        input: raw("{\"path\":\"example.txt\"}"),
        raw: None,
    }
}
fn content(boundary: JsValue) -> ToolContent {
    ToolContent::BoundaryOnly {
        boundary: BoundarySnapshot::capture(&boundary),
    }
}
fn text_content(value: &str) -> ToolContent {
    let boundary = obj(&[("type", "text".into()), ("text", value.into())]);
    ToolContent::Text {
        text: value.into(),
        boundary: BoundarySnapshot::capture(&boundary),
    }
}
fn content_boundary(content: &ToolContent) -> &BoundarySnapshot {
    match content {
        ToolContent::Text { boundary, .. }
        | ToolContent::Json { boundary, .. }
        | ToolContent::Image { boundary, .. }
        | ToolContent::Pdf { boundary, .. }
        | ToolContent::File { boundary, .. }
        | ToolContent::BoundaryOnly { boundary } => boundary,
    }
}
fn result(
    id: &str,
    content: Vec<ToolContent>,
    supplementals: Option<Vec<SupplementalToolMessage>>,
) -> ToolExecutionResult {
    let mut raw = obj(&[
        ("type", "success".into()),
        ("toolCallId", id.into()),
        ("toolName", "read_file".into()),
        (
            "content",
            JsValue::Array(
                content
                    .iter()
                    .map(|v| content_boundary(v).value().clone())
                    .collect(),
            ),
        ),
        ("startedAt", "fixed-start".into()),
        ("completedAt", "fixed-end".into()),
        ("extension", raw("{\"kept\":null}")),
    ]);
    if let (Some(messages), JsValue::Object(raw)) = (&supplementals, &mut raw) {
        raw.insert(
            "supplementalMessages".into(),
            JsValue::Array(
                messages
                    .iter()
                    .map(|v| v.boundary.value().clone())
                    .collect(),
            ),
        );
    }
    ToolExecutionResult {
        tool_call_id: id.into(),
        tool_name: "read_file".into(),
        disposition: ToolResultDisposition::Success { data: None },
        content,
        supplemental_messages: supplementals,
        metadata: None,
        started_at: "fixed-start".into(),
        completed_at: "fixed-end".into(),
        boundary: BoundarySnapshot::capture(&raw),
    }
}
fn supplemental(content: Vec<ToolContent>) -> SupplementalToolMessage {
    let raw = obj(&[
        ("role", "user".into()),
        (
            "content",
            JsValue::Array(
                content
                    .iter()
                    .map(|v| content_boundary(v).value().clone())
                    .collect(),
            ),
        ),
        ("extension", JsValue::Undefined),
    ]);
    SupplementalToolMessage {
        content,
        boundary: BoundarySnapshot::capture(&raw),
    }
}
fn array(value: &JsValue) -> &[JsValue] {
    let JsValue::Array(value) = value else {
        panic!("expected array")
    };
    value
}
fn blocks(message: &ProjectedMessage) -> &[JsValue] {
    array(message.boundary.value().get("content").unwrap())
}

#[test]
fn pairing_consumes_fifo_duplicates_ignores_extra_and_calls_missing_only_once() {
    let calls = vec![call("a".into()), call("a".into()), call("missing".into())];
    let results = vec![
        result("extra", vec![text_content("extra")], None),
        result("a", vec![text_content("first")], None),
        result("a", vec![text_content("second")], None),
    ];
    let mut missing_count = 0;
    let paired = pair_tool_results(&calls, results, &mut |call| {
        missing_count += 1;
        result(
            &call.id.to_utf8().unwrap(),
            vec![text_content("missing factory")],
            None,
        )
    });
    assert_eq!(missing_count, 1);
    assert_eq!(paired.len(), 3);
    let projected = project_tool_results(&paired).unwrap();
    let values = blocks(&projected[0]);
    assert_eq!(
        array(values[0].get("content").unwrap())[0].get("text"),
        Some(&JsValue::from("first"))
    );
    assert_eq!(
        array(values[1].get("content").unwrap())[0].get("text"),
        Some(&JsValue::from("second"))
    );
}
#[test]
fn empty_aggregate_and_empty_result_have_distinct_original_fallbacks() {
    let messages = project_tool_results(&[]).unwrap();
    assert_eq!(messages.len(), 1);
    assert!(blocks(&messages[0]).is_empty());
    let messages = project_tool_results(&[
        result("a", Vec::new(), None),
        result("b", vec![text_content("")], None),
    ])
    .unwrap();
    assert_eq!(
        array(blocks(&messages[0])[0].get("content").unwrap())[0].get("text"),
        Some(&JsValue::from("Tool completed with no output."))
    );
    assert_eq!(
        array(blocks(&messages[0])[1].get("content").unwrap())[0].get("text"),
        Some(&JsValue::from(""))
    );
    assert!(
        blocks(&messages[0])[0]
            .get("isError")
            .unwrap()
            .is_undefined()
    );
}
#[test]
fn main_json_and_file_differ_from_supplemental_full_object_stringify() {
    let json = raw("{\"type\":\"json\",\"value\":{\"b\":1,\"a\":2},\"extension\":null}");
    let file = raw("{\"type\":\"file\",\"path\":\"p\",\"mimeType\":\"\",\"description\":\"d\"}");
    let r = result(
        "a",
        vec![content(json.clone()), content(file.clone())],
        Some(vec![supplemental(vec![
            content(json.clone()),
            content(file.clone()),
        ])]),
    );
    let projected = project_tool_results(&[r]).unwrap();
    let main = array(blocks(&projected[0])[0].get("content").unwrap());
    assert_eq!(
        main[0].get("text"),
        Some(&JsValue::from("{\"b\":1,\"a\":2}"))
    );
    assert_eq!(main[1].get("text"), Some(&JsValue::from("[File: p, d]")));
    assert_eq!(
        blocks(&projected[1])[0].get("text"),
        Some(&JsValue::String(
            stringify_json(&json).unwrap().unwrap().into()
        ))
    );
    assert_eq!(
        blocks(&projected[1])[1].get("text"),
        Some(&JsValue::String(
            stringify_json(&file).unwrap().unwrap().into()
        ))
    );
    assert_eq!(
        projected[1].boundary.value().get("metadata"),
        Some(&raw(
            "{\"synthetic\":true,\"purpose\":\"tool_result_supplemental\",\"toolCallId\":\"a\"}"
        ))
    );
}
#[test]
fn undefined_json_and_unknown_kind_are_successful_boundary_only_not_errors_or_dropped() {
    let json = obj(&[("type", "json".into()), ("value", JsValue::Undefined)]);
    let unknown = raw("{\"type\":\"audio\",\"data\":\"remove-only-in-raw\",\"extension\":null}");
    let r = result(
        "a",
        vec![content(json), content(unknown.clone())],
        Some(vec![supplemental(vec![content(unknown)])]),
    );
    let projected = project_tool_results(&[r]).unwrap();
    assert!(
        matches!(&projected[0].normalized, MessageProjection::BoundaryOnly(gaps) if gaps.contains(&ProjectionGap::UndefinedCanonicalText))
    );
    let main = array(blocks(&projected[0])[0].get("content").unwrap());
    assert_eq!(main.len(), 2);
    assert!(main[0].get("text").unwrap().is_undefined());
    assert!(main[1].get("text").unwrap().is_undefined());
    let wire = stringify_json(projected[0].boundary.value())
        .unwrap()
        .unwrap();
    assert!(wire.contains("\"content\":[{\"type\":\"text\"},{\"type\":\"text\"}]"));
    assert!(!wire.contains("Tool completed with no output"));
    assert!(
        array(
            blocks(&projected[0])[0]
                .get("raw")
                .unwrap()
                .get("content")
                .unwrap()
        )[1]
        .get("data")
        .is_none()
    );
    assert!(matches!(
        &projected[1].normalized,
        MessageProjection::Complete(_)
    ));
}
#[test]
fn media_selected_fields_and_raw_sanitization_preserve_extensions_and_presence() {
    let image = obj(&[
        ("type", "image".into()),
        ("data", "base64".into()),
        ("mimeType", "image/png".into()),
        ("extension", JsValue::Undefined),
    ]);
    let pdf = raw(
        "{\"type\":\"pdf\",\"data\":\"pdf64\",\"mimeType\":\"application/pdf\",\"bytes\":9,\"pages\":2,\"extension\":null}",
    );
    let text = obj(&[
        ("type", "text".into()),
        ("text", "t".into()),
        ("data", "must-remain".into()),
    ]);
    let r = result(
        "a",
        vec![content(image.clone()), content(pdf.clone()), content(text)],
        Some(vec![supplemental(vec![content(image)])]),
    );
    let before = r.boundary.value().clone();
    let projected = project_tool_results(std::slice::from_ref(&r)).unwrap();
    assert_eq!(r.boundary.value(), &before);
    let main = array(blocks(&projected[0])[0].get("content").unwrap());
    assert!(main[0].get("bytes").unwrap().is_undefined());
    assert!(main[0].get("detail").unwrap().is_undefined());
    assert!(main[0].get("extension").is_none());
    let echo = blocks(&projected[0])[0].get("raw").unwrap();
    let echo_content = array(echo.get("content").unwrap());
    assert!(echo_content[0].get("data").is_none());
    assert!(echo_content[0].get("extension").unwrap().is_undefined());
    assert!(echo_content[1].get("data").is_none());
    assert_eq!(
        echo_content[2].get("data"),
        Some(&JsValue::from("must-remain"))
    );
    let supplemental_echo = &array(echo.get("supplementalMessages").unwrap())[0];
    assert!(supplemental_echo.get("extension").unwrap().is_undefined());
    assert!(
        array(supplemental_echo.get("content").unwrap())[0]
            .get("data")
            .is_none()
    );
    assert!(matches!(
        &projected[0].normalized,
        MessageProjection::Complete(_)
    ));
}
#[test]
fn lax_pdf_fields_keep_full_boundary_and_do_not_fabricate_typed_media() {
    let r = result(
        "a",
        vec![content(raw(
            "{\"type\":\"pdf\",\"data\":null,\"mimeType\":\"different\",\"bytes\":null,\"pages\":\"many\"}",
        ))],
        None,
    );
    let projected = project_tool_results(&[r]).unwrap();
    assert!(
        matches!(&projected[0].normalized, MessageProjection::BoundaryOnly(gaps) if gaps.contains(&ProjectionGap::LaxMediaFields))
    );
    assert_eq!(
        array(blocks(&projected[0])[0].get("content").unwrap())[0].get("data"),
        Some(&JsValue::Null)
    );
}
#[test]
fn supplemental_order_and_empty_supplemental_are_preserved() {
    let a = result(
        "a",
        vec![],
        Some(vec![
            supplemental(vec![text_content("first")]),
            supplemental(vec![]),
        ]),
    );
    let b = result(
        "b",
        vec![],
        Some(vec![supplemental(vec![text_content("last")])]),
    );
    let projected = project_tool_results(&[a, b]).unwrap();
    assert_eq!(projected.len(), 4);
    assert_eq!(
        blocks(&projected[1])[0].get("text"),
        Some(&JsValue::from("first"))
    );
    assert!(blocks(&projected[2]).is_empty());
    assert_eq!(
        projected[3]
            .boundary
            .value()
            .get("metadata")
            .unwrap()
            .get("toolCallId"),
        Some(&JsValue::from("b"))
    );
}
#[test]
fn tool_capture_contradictions_and_original_file_coercion_failure_are_distinct() {
    let mut r = result("a", vec![text_content("original")], None);
    r.tool_call_id = "wrong".into();
    assert!(matches!(
        project_tool_results(&[r]),
        Err(ProjectionError::InvalidBoundary { .. })
    ));
    let mut r = result("a", vec![text_content("original")], None);
    r.content = vec![text_content("modified")];
    assert!(matches!(
        project_tool_results(&[r]),
        Err(ProjectionError::InvalidBoundary { .. })
    ));
    let r = result(
        "a",
        vec![content(raw(
            "{\"type\":\"file\",\"path\":{\"toString\":0}}",
        ))],
        None,
    );
    assert!(matches!(
        project_tool_results(&[r]),
        Err(ProjectionError::Codec { .. })
    ));
}
#[test]
fn file_template_keeps_utf16_and_js_truthiness_without_lossy_decode() {
    let path = JsString::from_utf16(vec![0xd800]);
    let r = result(
        "a",
        vec![content(obj(&[
            ("type", "file".into()),
            ("path", JsValue::String(path)),
            ("mimeType", JsValue::Number(0.0)),
            ("description", JsValue::Bool(false)),
        ]))],
        None,
    );
    let projected = project_tool_results(&[r]).unwrap();
    let text = array(blocks(&projected[0])[0].get("content").unwrap())[0]
        .get("text")
        .unwrap()
        .as_string()
        .unwrap();
    let mut expected: Vec<u16> = "[File: ".encode_utf16().collect();
    expected.push(0xd800);
    expected.push(0x5d);
    assert_eq!(text.units(), expected);
}
#[test]
fn all_declared_content_variants_and_error_disposition_match_boundaries() {
    let json_boundary = raw("{\"type\":\"json\",\"value\":null}");
    let image_boundary = raw(
        "{\"type\":\"image\",\"data\":\"i\",\"mimeType\":\"image/png\",\"bytes\":1,\"detail\":\"high\"}",
    );
    let pdf_boundary = raw(
        "{\"type\":\"pdf\",\"data\":\"p\",\"mimeType\":\"application/pdf\",\"bytes\":2,\"pages\":0}",
    );
    let file_boundary = raw("{\"type\":\"file\",\"path\":\"path\",\"mimeType\":\"mime\"}");
    let mut r = result(
        "a",
        vec![
            ToolContent::Json {
                value: JsValue::Null,
                boundary: BoundarySnapshot::capture(&json_boundary),
            },
            ToolContent::Image {
                image: ImageBlock {
                    source: MediaSource::Base64,
                    data: "i".into(),
                    mime_type: "image/png".into(),
                    bytes: Some(1.0),
                    detail: Some(ImageDetail::High),
                },
                boundary: BoundarySnapshot::capture(&image_boundary),
            },
            ToolContent::Pdf {
                pdf: PdfBlock {
                    data: "p".into(),
                    bytes: 2.0,
                    pages: Some(0.0),
                },
                boundary: BoundarySnapshot::capture(&pdf_boundary),
            },
            ToolContent::File {
                path: "path".into(),
                mime_type: Some("mime".into()),
                description: None,
                boundary: BoundarySnapshot::capture(&file_boundary),
            },
        ],
        None,
    );
    let mut raw = r.boundary.value().as_object().unwrap().clone();
    raw.insert("type".into(), "error".into());
    let fields = obj(&[("recovery", JsValue::Null)])
        .as_object()
        .unwrap()
        .clone();
    r.disposition = ToolResultDisposition::Error {
        code: "code".into(),
        message: "message".into(),
        fields,
    };
    raw.insert(
        "error".into(),
        obj(&[
            ("code", "code".into()),
            ("message", "message".into()),
            ("recovery", JsValue::Null),
        ]),
    );
    r.boundary = BoundarySnapshot::capture(&JsValue::Object(raw));
    let projected = project_tool_results(&[r]).unwrap();
    assert_eq!(
        blocks(&projected[0])[0].get("isError"),
        Some(&JsValue::Bool(true))
    );
    let main = array(blocks(&projected[0])[0].get("content").unwrap());
    assert_eq!(main[0].get("text"), Some(&JsValue::from("null")));
    assert_eq!(
        main[3].get("text"),
        Some(&JsValue::from("[File: path, mime]"))
    );
    assert!(matches!(
        &projected[0].normalized,
        MessageProjection::Complete(_)
    ));
}
#[test]
fn deep_extensions_and_utf16_snapshot_are_detached_from_mutable_input() {
    let mut extension = JsValue::String(JsString::from_utf16(vec![0xd800, 0, 0xdc00]));
    for _ in 0..1024 {
        extension = obj(&[("next", extension)]);
    }
    let call = call(" a ".into());
    let mut input = obj(&[
        ("type", "tool_call_end".into()),
        (
            "toolCall",
            obj(&[
                ("id", " a ".into()),
                ("name", "read_file".into()),
                ("input", call.input.clone()),
                ("extension", extension),
            ]),
        ),
    ]);
    let event = BoundarySnapshot::capture(&input);
    if let JsValue::Object(input) = &mut input {
        input.insert("toolCall".into(), JsValue::Null);
    }
    let reducer = OriginalSourceModelReducer;
    let mut state = reducer.create(&JsValue::Undefined);
    reducer.apply(&mut state, &event).unwrap();
    let assembled = reducer.assemble(&mut state).unwrap();
    let calls = array(assembled.value().get("toolCalls").unwrap());
    let mut leaf = calls[0].get("extension").unwrap();
    for _ in 0..1024 {
        leaf = leaf.get("next").unwrap();
    }
    assert_eq!(leaf.as_string().unwrap().units(), &[0xd800, 0, 0xdc00]);
    assert_eq!(calls[0].get("id"), Some(&JsValue::from("a")));
}
#[test]
fn null_optional_echo_fields_normalize_none_without_losing_own_null() {
    let mut r = result("a", vec![text_content("t")], None);
    let mut raw = r.boundary.value().as_object().unwrap().clone();
    raw.insert("metadata".into(), JsValue::Null);
    raw.insert("supplementalMessages".into(), JsValue::Null);
    r.boundary = BoundarySnapshot::capture(&JsValue::Object(raw));
    let projected = project_tool_results(&[r]).unwrap();
    assert_eq!(projected.len(), 1);
    let raw = blocks(&projected[0])[0].get("raw").unwrap();
    assert_eq!(raw.get("metadata"), Some(&JsValue::Null));
    assert_eq!(raw.get("supplementalMessages"), Some(&JsValue::Null));
    assert!(matches!(
        &projected[0].normalized,
        MessageProjection::Complete(_)
    ));
}

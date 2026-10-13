//! Source-backed checks of clone.ts and sidecar client parseBinding /
//! parseCapabilities (fixed b243fa0). These are pure boundaries, not transport gates.
use pilotdeck_protocol::{
    JsObject, JsString, JsValue,
    admission::{self, AdmissionErrorKind},
    model::*,
    module::ModuleMethod,
    parse_json,
    wire::{EnvelopeRef, MessageRef, ResponseRef},
};
use std::sync::Arc;

fn json(text: &str) -> JsValue {
    parse_json(text).unwrap()
}
fn object(text: &str) -> JsObject {
    json(text).as_object().unwrap().clone()
}
fn response(value: &JsValue) -> ResponseRef<'_> {
    match EnvelopeRef::decode(value).unwrap().message {
        MessageRef::Response(response) => response,
        _ => panic!("expected response"),
    }
}
fn envelope() -> JsValue {
    json(r#"{"kind":"response","messageId":"m","inReplyTo":"h","ok":true}"#)
}
fn put(value: &mut JsValue, name: &str, field: JsValue) {
    let JsValue::Object(value) = value else {
        panic!("expected object")
    };
    value.insert(name.into(), field);
}
fn bound() -> JsValue {
    json(
        r#"{"kind":"response","messageId":"m","inReplyTo":"h","ok":true,"protocolVersion":"2.0","moduleId":"module","moduleInstanceId":"instance","connectionGeneration":"generation","capabilitiesVersion":"v"}"#,
    )
}
fn capability(payload: JsValue) -> JsValue {
    let mut result = envelope();
    put(&mut result, "payload", payload);
    result
}
fn timeline() -> TimelinePosition {
    TimelinePosition {
        turn_id: "turn".into(),
        id: "id".into(),
        previous_id: Some("previous".into()),
        order: 1.5,
        revision: 2.0,
        offset_utf16: Some(2.0),
    }
}
fn block(body: ContentBody) -> ContentBlock {
    ContentBlock {
        timeline: Some(timeline()),
        body,
    }
}
fn text() -> TextBlock {
    TextBlock {
        text: JsString::from_utf16(vec![0xd800, 0, 0xdc00]),
        block_id: Some("block".into()),
    }
}
fn image() -> ImageBlock {
    ImageBlock {
        source: MediaSource::Url,
        data: "fixture://image".into(),
        mime_type: "image/png".into(),
        bytes: Some(10.0),
        detail: Some(ImageDetail::High),
    }
}
fn pdf() -> PdfBlock {
    PdfBlock {
        data: "synthetic".into(),
        bytes: 10.0,
        pages: Some(1.0),
    }
}
fn request() -> CanonicalModelRequest {
    let echo = || {
        ProviderEcho(Arc::new(json(
            r#"{"nested":{"value":"before"},"number":-0}"#,
        )))
    };
    let content = vec![
        block(ContentBody::Text(text())),
        block(ContentBody::Thinking(ThinkingBlock {
            text: "think".into(),
            block_id: Some("b".into()),
            signature: Some("signature".into()),
            reasoning_content: Some("reason".into()),
        })),
        block(ContentBody::Image(image())),
        block(ContentBody::Pdf(pdf())),
        block(ContentBody::Audio(AudioBlock {
            source: MediaSource::Base64,
            data: "synthetic".into(),
            mime_type: "audio/wav".into(),
            bytes: Some(3.0),
            duration_seconds: Some(0.5),
        })),
        block(ContentBody::ToolCall(ToolCall {
            timeline: Some(timeline()),
            id: "call".into(),
            name: "tool".into(),
            input: json(r#"{"args":["before"]}"#),
            raw: Some(echo()),
        })),
        block(ContentBody::ToolResult(ToolResult {
            tool_call_id: "call".into(),
            content: vec![
                ToolResultItem {
                    timeline: Some(timeline()),
                    body: ToolResultContent::Text(text()),
                },
                ToolResultItem {
                    timeline: Some(timeline()),
                    body: ToolResultContent::Image(image()),
                },
                ToolResultItem {
                    timeline: Some(timeline()),
                    body: ToolResultContent::Pdf(pdf()),
                },
            ],
            is_error: Some(false),
            raw: Some(echo()),
        })),
        block(ContentBody::ToolResultReference(ToolResultReference {
            tool_call_id: "call".into(),
            is_error: Some(true),
            path: "/fixture/result".into(),
            read_file_path: Some("result".into()),
            original_bytes: 20.0,
            preview: "preview".into(),
            has_more: true,
            mime_type: Some("text/plain".into()),
            reason: Some("size".into()),
        })),
        block(ContentBody::MediaReference(MediaReference {
            tool_call_id: Some("call".into()),
            path: "/fixture/media".into(),
            original_bytes: 30.0,
            preview: "media".into(),
            has_more: false,
            mime_type: "image/png".into(),
            media_type: MediaType::Image,
            pages: Some(1.0),
            detail: Some(ImageDetail::Low),
            reason: Some("size".into()),
        })),
    ];
    CanonicalModelRequest {
        model: "model".into(),
        provider: "provider".into(),
        messages: vec![Message {
            role: Role::Assistant,
            content,
            metadata: Some(MessageMetadata {
                module_id: Some("module".into()),
                model: Some("model".into()),
                synthetic: Some(true),
                transient: Some(false),
                transient_id: Some("transient".into()),
                tool_call_id: Some("call".into()),
                compact_replacement: Some(true),
                compact_snapshot_id: Some("snapshot".into()),
                purpose: Some("purpose".into()),
                trusted_context: Some(TrustedContext {
                    source: "source".into(),
                    purpose: TrustedContextPurpose::Skill,
                    authorized_principal: "synthetic".into(),
                }),
                queue_item_id: Some("queue".into()),
                fork_carryover: Some(ForkCarryover {
                    source_session_id: "session".into(),
                    source_turn_id: Some("turn".into()),
                }),
            }),
        }],
        system_prompt: Some("system".into()),
        tools: Some(vec![ToolSchema {
            name: "tool".into(),
            description: Some("description".into()),
            input_schema: object(r#"{"nested":{"value":"before"}}"#),
        }]),
        tool_choice: Some(ToolChoice::Tool {
            name: "tool".into(),
        }),
        max_output_tokens: Some(10.0),
        temperature: Some(0.5),
        speed: Some(2.0),
        thinking: Some(ThinkingConfig {
            mode: Some(ThinkingMode::High),
            enabled: true,
            budget_tokens: Some(20.0),
            preserve: Some(true),
            split_reasoning: Some(false),
        }),
        stream: Some(true),
        metadata: Some(object(r#"{"nested":{"value":"before"}}"#)),
        output_schema: Some(OutputSchema {
            name: "output".into(),
            description: Some("desc".into()),
            schema: object(r#"{"nested":{"value":"before"}}"#),
            strict: Some(false),
        }),
        cache_breakpoints: Some(vec![0.0, 1.5]),
        cache_plan: Some(CachePlan {
            provider: Some("provider".into()),
            model: Some("model".into()),
            system: true,
            tools: false,
            messages: vec![0.0, 1.0],
            fingerprint: "fingerprint".into(),
            generation: 3.0,
        }),
    }
}
fn echo(block: &ContentBlock) -> &Arc<JsValue> {
    match &block.body {
        ContentBody::ToolCall(x) => &x.raw.as_ref().unwrap().0,
        ContentBody::ToolResult(x) => &x.raw.as_ref().unwrap().0,
        _ => panic!("expected echo"),
    }
}

#[test]
fn admission_detaches_echoes_but_ordinary_content_clone_shares_them() {
    let source = request();
    let ordinary = source.messages[0].clone();
    let prepared = PreparedRequest::capture(&source);
    assert_eq!(prepared.request(), &source);
    for index in [5, 6] {
        assert!(Arc::ptr_eq(
            echo(&source.messages[0].content[index]),
            echo(&ordinary.content[index])
        ));
        assert!(!Arc::ptr_eq(
            echo(&source.messages[0].content[index]),
            echo(&prepared.request().messages[0].content[index])
        ));
    }
    let retry = prepared.clone();
    assert!(std::ptr::eq(prepared.request(), retry.request()));
}

#[test]
fn request_capture_isolates_every_owned_request_field_and_nested_tool_result() {
    let mut source = request();
    let expected = source.clone();
    let captured = PreparedRequest::capture(&source);
    source.model = "later".into();
    source.provider = "later".into();
    source.system_prompt = None;
    source.tool_choice = None;
    source.max_output_tokens = None;
    source.temperature = None;
    source.speed = None;
    source.thinking = None;
    source.stream = None;
    source.cache_breakpoints.as_mut().unwrap().push(9.0);
    source.cache_plan.as_mut().unwrap().messages.clear();
    source
        .metadata
        .as_mut()
        .unwrap()
        .insert("nested".into(), JsValue::Null);
    source
        .output_schema
        .as_mut()
        .unwrap()
        .schema
        .insert("nested".into(), JsValue::Null);
    source.tools.as_mut().unwrap()[0]
        .input_schema
        .insert("nested".into(), JsValue::Null);
    source.messages[0].role = Role::User;
    source.messages[0].metadata.as_mut().unwrap().module_id = None;
    if let ContentBody::ToolCall(call) = &mut source.messages[0].content[5].body {
        put(&mut call.input, "args", JsValue::Null);
        call.timeline = None;
    }
    if let ContentBody::ToolResult(result) = &mut source.messages[0].content[6].body {
        result.content.clear();
    }
    assert_eq!(captured.request(), &expected);
}

#[test]
fn admission_raw_echo_is_not_mutated_by_the_original_owner() {
    let mut source = request();
    let captured = PreparedRequest::capture(&source);
    for index in [5, 6] {
        let raw = match &mut source.messages[0].content[index].body {
            ContentBody::ToolCall(x) => x.raw.as_mut().unwrap(),
            ContentBody::ToolResult(x) => x.raw.as_mut().unwrap(),
            _ => unreachable!(),
        };
        put(
            Arc::get_mut(&mut raw.0).expect("admission must detach raw"),
            "nested",
            JsValue::Null,
        );
        assert!(
            echo(&captured.request().messages[0].content[index])
                .get("nested")
                .unwrap()
                .as_object()
                .is_some()
        );
    }
}

#[test]
fn wire_snapshot_keeps_extensions_presence_wtf16_key_order_and_negative_zero() {
    let mut source = json(
        r#"{"2":1,"01":2,"x":1,"x":3,"null":null,"zero":-0,"text":"\ud800","messages":[],"future":{"a":[1,2]}}"#,
    );
    put(&mut source, "ownUndefined", JsValue::Undefined);
    let capture = PreparedWireRequest::capture(&source);
    let copy = BoundarySnapshot::capture(&source);
    put(&mut source, "future", JsValue::Null);
    put(&mut source, "null", JsValue::Undefined);
    put(&mut source, "ownUndefined", JsValue::Null);
    put(&mut source, "text", "later".into());
    let frozen = capture.request();
    assert!(frozen.get("future").unwrap().as_object().is_some());
    assert!(matches!(frozen.get("null"), Some(JsValue::Null)));
    assert!(matches!(
        frozen.get("ownUndefined"),
        Some(JsValue::Undefined)
    ));
    assert!(frozen.get("missing").is_none());
    assert_eq!(
        frozen.get("text").unwrap().as_string().unwrap().units(),
        &[0xd800]
    );
    assert_eq!(
        frozen.get("zero").unwrap().as_number().unwrap().to_bits(),
        (-0.0f64).to_bits()
    );
    assert_eq!(frozen.get("x").unwrap().as_number(), Some(3.0));
    assert_eq!(copy.value(), frozen);
    assert_eq!(
        frozen
            .as_object()
            .unwrap()
            .entries()
            .iter()
            .map(|(k, _)| k.to_utf8().unwrap())
            .collect::<Vec<_>>(),
        [
            "2",
            "01",
            "x",
            "null",
            "zero",
            "text",
            "messages",
            "future",
            "ownUndefined"
        ]
    );
}

#[test]
fn deep_boundary_and_provider_echo_capture_clone_and_drop_without_recursion() {
    let mut deep = JsValue::Undefined;
    for _ in 0..20_000 {
        deep = JsValue::Array(vec![deep]);
    }
    let boundary = BoundarySnapshot::capture(&deep);
    assert_eq!(boundary.value(), &deep);
    let mut source = request();
    if let ContentBody::ToolCall(call) = &mut source.messages[0].content[5].body {
        call.input = deep.clone();
        call.raw = Some(ProviderEcho(Arc::new(deep)));
    }
    let captured = PreparedRequest::capture(&source);
    assert_eq!(captured.request(), &source);
}

#[test]
fn binding_error_precedence_version_then_each_nonempty_field() {
    let mut fields = envelope();
    let error = admission::parse_sidecar_binding(response(&fields)).unwrap_err();
    assert_eq!(error.kind, AdmissionErrorKind::UnsupportedVersion);
    assert!(
        error
            .message
            .equals_str("Sidecar protocol version is unsupported: missing.")
    );
    put(&mut fields, "protocolVersion", "2.0".into());
    for field in [
        "moduleId",
        "moduleInstanceId",
        "connectionGeneration",
        "capabilitiesVersion",
    ] {
        let error = admission::parse_sidecar_binding(response(&fields)).unwrap_err();
        assert_eq!(error.kind, AdmissionErrorKind::MissingBinding);
        assert!(
            error
                .message
                .equals_str(&format!("Sidecar handshake response is missing {field}."))
        );
        put(&mut fields, field, " ".into());
    }
    assert!(admission::parse_sidecar_binding(response(&fields)).is_ok());
}

#[test]
fn binding_ids_use_nonempty_utf16_without_lossy_decoding() {
    let mut value = bound();
    let id = JsString::from_utf16(vec![0xd800]);
    for field in [
        "moduleId",
        "moduleInstanceId",
        "connectionGeneration",
        "capabilitiesVersion",
    ] {
        put(&mut value, field, JsValue::String(id.clone()));
    }
    let binding = admission::parse_sidecar_binding(response(&value)).unwrap();
    assert_eq!(binding.module_id, id);
    assert_eq!(binding.module_instance_id, id);
    assert_eq!(binding.connection_generation, id);
    assert_eq!(binding.capabilities_version, id);
    for invalid in [
        JsValue::Undefined,
        JsValue::Null,
        JsValue::Bool(false),
        JsValue::String("".into()),
        JsValue::Array(vec![]),
    ] {
        put(&mut value, "moduleId", invalid);
        assert_eq!(
            admission::parse_sidecar_binding(response(&value))
                .unwrap_err()
                .kind,
            AdmissionErrorKind::MissingBinding
        );
    }
}

#[test]
fn unsupported_version_interpolation_uses_js_nullish_number_and_array_rules() {
    let fixtures = vec![
        (JsValue::Null, "missing"),
        (JsValue::Undefined, "missing"),
        (JsValue::Bool(false), "false"),
        (JsValue::Number(-0.0), "0"),
        (JsValue::Number(f64::INFINITY), "Infinity"),
        (JsValue::Number(f64::NAN), "NaN"),
        (json(r#"[null,"x",[null,"y"]]"#), ",x,,y"),
        (json("{}"), "[object Object]"),
    ];
    for (version, expected) in fixtures {
        let mut value = envelope();
        put(&mut value, "protocolVersion", version);
        let error = admission::parse_sidecar_binding(response(&value)).unwrap_err();
        assert_eq!(error.kind, AdmissionErrorKind::UnsupportedVersion);
        assert!(error.message.equals_str(&format!(
            "Sidecar protocol version is unsupported: {expected}."
        )));
    }
    let mut value = envelope();
    put(
        &mut value,
        "protocolVersion",
        JsValue::String(JsString::from_utf16(vec![0xd800])),
    );
    let error = admission::parse_sidecar_binding(response(&value)).unwrap_err();
    let mut expected: Vec<u16> = "Sidecar protocol version is unsupported: "
        .encode_utf16()
        .collect();
    expected.extend([0xd800, 46]);
    assert_eq!(error.message.units(), expected);
}

#[test]
fn version_object_conversion_failure_is_type_error_before_binding_failure() {
    for version in [
        json(r#"{"toString":null}"#),
        json(r#"{"toString":"2.0","valueOf":2}"#),
        json(r#"[{"toString":false}]"#),
    ] {
        let mut value = envelope();
        put(&mut value, "protocolVersion", version);
        let error = admission::parse_sidecar_binding(response(&value)).unwrap_err();
        assert_eq!(error.kind, AdmissionErrorKind::TypeError);
        assert!(
            error
                .message
                .equals_str("Cannot convert object to primitive value")
        );
    }
    // An own valueOf alone does not shadow inherited Object#toString.
    let mut value = envelope();
    put(&mut value, "protocolVersion", json(r#"{"valueOf":null}"#));
    assert_eq!(
        admission::parse_sidecar_binding(response(&value))
            .unwrap_err()
            .kind,
        AdmissionErrorKind::UnsupportedVersion
    );
}

#[test]
fn capabilities_mismatch_precedes_invalid_method_checks() {
    for payload in [
        JsValue::Undefined,
        JsValue::Null,
        json("[]"),
        json(r#"{"capabilitiesVersion":"wrong","methods":[null]}"#),
        json(r#"{"capabilitiesVersion":"v","methods":null}"#),
    ] {
        let value = capability(payload);
        let error =
            admission::parse_sidecar_capabilities(response(&value), &"v".into()).unwrap_err();
        assert_eq!(error.kind, AdmissionErrorKind::CapabilitiesMismatch);
        assert!(
            error
                .message
                .equals_str("Sidecar capabilities response does not match its hello binding.")
        );
    }
}

#[test]
fn capabilities_only_accept_original_five_exact_names() {
    for name in ["execute", "cancel", "status", "resume", "ack"] {
        let value = capability(json(&format!(
            r#"{{"capabilitiesVersion":"v","methods":[{{"name":"{name}"}}]}}"#
        )));
        assert_eq!(
            admission::parse_sidecar_capabilities(response(&value), &"v".into())
                .unwrap()
                .methods
                .len(),
            1
        );
    }
    for method in [
        json("null"),
        json("[]"),
        json("{}"),
        json(r#"{"name":null}"#),
        json(r#"{"name":"hello"}"#),
        json(r#"{"name":"module_call"}"#),
        json(r#"{"name":"capabilities"}"#),
        json(r#"{"name":"Execute"}"#),
        json(r#"{"name":"execute "}"#),
    ] {
        let mut payload = json(r#"{"capabilitiesVersion":"v"}"#);
        put(&mut payload, "methods", JsValue::Array(vec![method]));
        let value = capability(payload);
        let error =
            admission::parse_sidecar_capabilities(response(&value), &"v".into()).unwrap_err();
        assert_eq!(error.kind, AdmissionErrorKind::InvalidMethod);
        assert!(
            error
                .message
                .equals_str("Sidecar capabilities response contains an invalid method.")
        );
    }
}

#[test]
fn capabilities_keep_duplicates_empty_list_and_lax_optional_fields_detached() {
    let mut method = json(
        r#"{"name":"execute","enabled":null,"profiles":4,"resumeSupport":false,"cancel":"yes","future":{"value":1}}"#,
    );
    put(&mut method, "ownUndefined", JsValue::Undefined);
    let mut payload = json(r#"{"capabilitiesVersion":"\ud800"}"#);
    put(
        &mut payload,
        "methods",
        JsValue::Array(vec![method.clone(), method]),
    );
    let mut value = capability(payload);
    let version = JsString::from_utf16(vec![0xd800]);
    let parsed = admission::parse_sidecar_capabilities(response(&value), &version).unwrap();
    assert_eq!(parsed.version, version);
    assert_eq!(parsed.methods.len(), 2);
    assert_eq!(parsed.methods[0].name, ModuleMethod::Execute);
    let fields = &parsed.methods[0].fields;
    assert!(matches!(fields.get("enabled"), Some(JsValue::Null)));
    assert_eq!(fields.get("profiles").unwrap().as_number(), Some(4.0));
    assert_eq!(fields.get("resumeSupport").unwrap().as_bool(), Some(false));
    assert!(fields.get("ownUndefined").unwrap().is_undefined());
    assert!(fields.get("future").unwrap().as_object().is_some());
    put(&mut value, "payload", JsValue::Null);
    assert_eq!(parsed.methods.len(), 2);
    let empty = capability(json(r#"{"capabilitiesVersion":"v","methods":[]}"#));
    assert!(
        admission::parse_sidecar_capabilities(response(&empty), &"v".into())
            .unwrap()
            .methods
            .is_empty()
    );
}

#[test]
fn hello_binding_comparison_checks_all_four_fields() {
    let value = bound();
    let original = admission::parse_sidecar_binding(response(&value)).unwrap();
    for field in [
        "moduleId",
        "moduleInstanceId",
        "connectionGeneration",
        "capabilitiesVersion",
    ] {
        let mut changed = value.clone();
        put(&mut changed, field, "changed".into());
        let binding = admission::parse_sidecar_binding(response(&changed)).unwrap();
        assert!(!admission::same_binding(&original, &binding));
    }
    assert!(admission::same_binding(&original, &original));
}

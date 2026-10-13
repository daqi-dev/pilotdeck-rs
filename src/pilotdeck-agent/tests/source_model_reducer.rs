//! Source-derived regressions. Independent original differential is mandatory.
pub mod source_model {
    pub use pilotdeck_agent::source_model::*;
}
#[path = "../src/source_model_reducer.rs"]
mod reducer;
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use source_model::*;
fn json(s: &str) -> JsValue {
    pilotdeck_protocol::parse_json(s).unwrap()
}
fn cap(v: &JsValue) -> BoundarySnapshot {
    BoundarySnapshot::capture(v)
}
fn apply(s: &mut SourceAssemblerState, text: &str) {
    reducer::apply(s, &cap(&json(text))).unwrap();
}
fn obj(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut o = JsObject::new();
    for (k, v) in fields {
        o.insert(k.into(), v);
    }
    JsValue::Object(o)
}
fn keys(o: &JsObject) -> Vec<String> {
    o.entries()
        .iter()
        .map(|(k, _)| k.to_utf8().unwrap())
        .collect()
}
fn content(s: &mut SourceAssemblerState) -> Vec<JsValue> {
    let out = reducer::assemble(s).unwrap();
    let JsValue::Array(v) = out.value().get("message").unwrap().get("content").unwrap() else {
        panic!()
    };
    v.clone()
}
fn same(a: &JsValue, b: &JsValue) {
    let mut todo = vec![(a, b)];
    while let Some((a, b)) = todo.pop() {
        match (a, b) {
            (JsValue::Number(a), JsValue::Number(b)) => {
                assert!(a.to_bits() == b.to_bits() || a.is_nan() && b.is_nan())
            }
            (JsValue::Object(a), JsValue::Object(b)) => {
                assert_eq!(a.len(), b.len());
                for ((ak, av), (bk, bv)) in a.entries().iter().zip(b.entries()) {
                    assert_eq!(ak, bk);
                    todo.push((av, bv));
                }
            }
            (JsValue::Array(a), JsValue::Array(b)) => {
                assert_eq!(a.len(), b.len());
                todo.extend(a.iter().zip(b));
            }
            _ => assert_eq!(a, b),
        }
    }
}
fn type_error(e: SourceAssemblyError, text: &str) {
    assert!(matches!(e,SourceAssemblyError::TypeError{message}if message.equals_str(text)));
}

#[test]
fn create_truthiness_and_exact_initial_own_key_order() {
    for id in [
        JsValue::Undefined,
        JsValue::Null,
        JsValue::Bool(false),
        JsValue::Number(0.0),
        JsValue::Number(-0.0),
        JsValue::Number(f64::NAN),
        "".into(),
    ] {
        let s = reducer::create(&id);
        assert_eq!(
            keys(&reducer::snapshot(&s)),
            [
                "content",
                "textBuffer",
                "thinkingBuffer",
                "thinkingReasoningContentBuffer",
                "usage",
                "hasMessageEnd",
                "toolCalls"
            ]
        );
        assert_eq!(reducer::block_id(&s, &"text".into()).unwrap(), None);
    }
    for id in [
        JsValue::Bool(true),
        JsValue::Number(7.0),
        "id".into(),
        json("[]"),
        json("{}"),
    ] {
        let s = reducer::create(&id);
        let snap = reducer::snapshot(&s);
        assert_eq!(snap.get("responseId"), Some(&id));
        assert_eq!(keys(&snap)[0], "responseId");
        assert!(reducer::block_id(&s, &"text".into()).unwrap().is_some());
    }
}

#[test]
fn later_own_undefined_fields_keep_first_assignment_order() {
    let mut s = reducer::create(&"r".into());
    apply(&mut s, r#"{"type":"error"}"#);
    apply(&mut s, r#"{"type":"request_started"}"#);
    apply(&mut s, r#"{"type":"thinking_delta","text":"think"}"#);
    apply(&mut s, r#"{"type":"message_end"}"#);
    let snap = reducer::snapshot(&s);
    assert_eq!(
        keys(&snap),
        [
            "responseId",
            "content",
            "textBuffer",
            "thinkingBuffer",
            "thinkingReasoningContentBuffer",
            "usage",
            "hasMessageEnd",
            "toolCalls",
            "error",
            "finishReason",
            "model",
            "thinkingSignature"
        ]
    );
    for key in ["error", "finishReason", "model", "thinkingSignature"] {
        assert!(snap.get(key).unwrap().is_undefined());
    }
    assert!(s.has_message_end);
    assert_eq!(
        content(&mut s)[0].get("text"),
        Some(&JsValue::from("think"))
    );
}

#[test]
fn ignored_tags_primitives_and_lax_start_shapes_are_exact_noops() {
    let mut s = reducer::create(&JsValue::Undefined);
    let before = reducer::snapshot(&s);
    for raw in [
        r#"{"type":"message_start"}"#,
        r#"{"type":"tool_call_start","id":null}"#,
        r#"{"type":"tool_call_delta","delta":false}"#,
        r#"{"type":"future","extra":{}}"#,
        r#"{"type":7}"#,
        r#""string""#,
        "1",
        "false",
        "[]",
        "{}",
    ] {
        apply(&mut s, raw);
        same(
            &JsValue::Object(reducer::snapshot(&s)),
            &JsValue::Object(before.clone()),
        );
    }
    for (raw, word) in [(JsValue::Null, "null"), (JsValue::Undefined, "undefined")] {
        type_error(
            reducer::apply(&mut s, &cap(&raw)).unwrap_err(),
            &format!("Cannot read properties of {word} (reading 'type')"),
        );
        same(
            &JsValue::Object(reducer::snapshot(&s)),
            &JsValue::Object(before.clone()),
        );
    }
}

#[test]
fn text_js_string_coercion_keeps_wtf16_and_iterative_deep_array_join() {
    let mut s = reducer::create(&"r".into());
    for raw in [
        r#"{"type":"text_delta","text":7}"#,
        r#"{"type":"text_delta","text":null}"#,
        r#"{"type":"text_delta"}"#,
        r#"{"type":"text_delta","text":true}"#,
        r#"{"type":"text_delta","text":{"valueOf":null}}"#,
        r#"{"type":"text_delta","text":["\ud800",null,[false,"x"]]}"#,
    ] {
        apply(&mut s, raw);
    }
    let mut deep = JsValue::from("deep");
    for _ in 0..2000 {
        deep = JsValue::Array(vec![deep]);
    }
    reducer::apply(
        &mut s,
        &cap(&obj([("type", "text_delta".into()), ("text", deep)])),
    )
    .unwrap();
    let mut expected = "7nullundefinedtrue[object Object]"
        .encode_utf16()
        .collect::<Vec<_>>();
    expected.extend([0xd800, 44, 44]);
    expected.extend("false,xdeep".encode_utf16());
    assert_eq!(s.text_buffer.units(), expected);
    let blocks = content(&mut s);
    assert_eq!(blocks.len(), 1);
    assert_eq!(
        blocks[0].get("text").unwrap().as_string().unwrap().units(),
        expected
    );
    assert_eq!(blocks[0].get("blockId"), Some(&"r:text:0".into()));
}

#[test]
fn signature_null_throws_after_text_and_reasoning_and_prior_flush() {
    let mut s = reducer::create(&"r".into());
    apply(&mut s, r#"{"type":"text_delta","text":"prior"}"#);
    type_error(
        reducer::apply(
            &mut s,
            &cap(&json(
                r#"{"type":"thinking_delta","text":7,"reasoningContent":null,"signature":null}"#,
            )),
        )
        .unwrap_err(),
        "Cannot read properties of null (reading 'length')",
    );
    assert!(s.text_buffer.is_empty());
    assert_eq!(s.content.len(), 1);
    assert_eq!(s.content[0].value().get("text"), Some(&"prior".into()));
    assert!(s.thinking_buffer.equals_str("7"));
    assert!(s.thinking_reasoning_content_buffer.equals_str("null"));
    assert!(matches!(s.thinking_signature, SourceField::Missing));
    let blocks = content(&mut s);
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[1].get("reasoningContent"), Some(&"null".into()));
    assert!(blocks[1].get("signature").is_none());
    assert!(
        reducer::snapshot(&s)
            .get("thinkingSignature")
            .unwrap()
            .is_undefined()
    );
}

#[test]
fn thinking_reasoning_exception_retains_same_event_text_before_signature() {
    let mut s = reducer::create(&JsValue::Undefined);
    apply(
        &mut s,
        r#"{"type":"thinking_delta","text":"old","signature":"sig"}"#,
    );
    type_error(reducer::apply(&mut s,&cap(&json(r#"{"type":"thinking_delta","text":"new","reasoningContent":{"toString":null},"signature":"replacement"}"#))).unwrap_err(),"Cannot convert object to primitive value");
    assert!(s.thinking_buffer.equals_str("oldnew"));
    assert!(s.thinking_reasoning_content_buffer.is_empty());
    assert_eq!(
        reducer::snapshot(&s).get("thinkingSignature"),
        Some(&"sig".into())
    );
    let before = reducer::snapshot(&s);
    type_error(
        reducer::apply(
            &mut s,
            &cap(&json(
                r#"{"type":"thinking_delta","text":[{"toString":0}]}"#,
            )),
        )
        .unwrap_err(),
        "Cannot convert object to primitive value",
    );
    same(
        &JsValue::Object(reducer::snapshot(&s)),
        &JsValue::Object(before.clone()),
    );
}

#[test]
fn nonstring_signatures_follow_actual_length_relational_effect() {
    for signature in [
        json(r#"["\ud800",null]"#),
        json(r#"{"length":"0x10","extra":null}"#),
        json(r#"{"length":[1]}"#),
    ] {
        let mut s = reducer::create(&JsValue::Undefined);
        reducer::apply(
            &mut s,
            &cap(&obj([
                ("type", "thinking_delta".into()),
                ("text", "t".into()),
                ("signature", signature.clone()),
            ])),
        )
        .unwrap();
        assert_eq!(content(&mut s)[0].get("signature"), Some(&signature));
    }
    for signature in [
        JsValue::Bool(true),
        JsValue::Number(7.0),
        json("[]"),
        "".into(),
        json(r#"{"length":0}"#),
        json(r#"{"length":"+0x1"}"#),
        json(r#"{"length":"\u00851"}"#),
        json(r#"{"length":{}}"#),
    ] {
        let mut s = reducer::create(&JsValue::Undefined);
        reducer::apply(
            &mut s,
            &cap(&obj([
                ("type", "thinking_delta".into()),
                ("text", "t".into()),
                ("signature", signature),
            ])),
        )
        .unwrap();
        assert!(content(&mut s)[0].get("signature").is_none());
    }
    let mut s = reducer::create(&JsValue::Undefined);
    type_error(reducer::apply(&mut s,&cap(&json(r#"{"type":"thinking_delta","text":"kept","signature":{"length":{"toString":false}}}"#))).unwrap_err(),"Cannot convert object to primitive value");
    assert!(s.thinking_buffer.equals_str("kept"));
}

#[test]
fn usage_all_own_undefined_differs_initial_missing_and_atomic_throw() {
    let mut s = reducer::create(&JsValue::Undefined);
    assert!(
        reducer::snapshot(&s)
            .get("usage")
            .unwrap()
            .as_object()
            .unwrap()
            .is_empty()
    );
    apply(&mut s, r#"{"type":"usage","usage":{}}"#);
    let snap = reducer::snapshot(&s);
    let u = snap.get("usage").unwrap().as_object().unwrap();
    assert_eq!(
        keys(u),
        [
            "inputTokens",
            "outputTokens",
            "cacheReadTokens",
            "cacheWriteTokens",
            "totalTokens",
            "nativeCost"
        ]
    );
    assert!(u.entries().iter().all(|(_, v)| v.is_undefined()));
    assert!(
        reducer::assemble(&mut s)
            .unwrap()
            .value()
            .get("usage")
            .unwrap()
            .is_undefined()
    );
    let before = reducer::snapshot(&s);
    for raw in [r#"{"type":"usage"}"#, r#"{"type":"usage","usage":null}"#] {
        assert!(matches!(
            reducer::apply(&mut s, &cap(&json(raw))),
            Err(SourceAssemblyError::TypeError { .. })
        ));
        same(
            &JsValue::Object(reducer::snapshot(&s)),
            &JsValue::Object(before.clone()),
        );
    }
}

#[test]
fn usage_js_plus_null_boolean_string_array_nonfinite_and_signed_zero() {
    let mut s = reducer::create(&JsValue::Undefined);
    apply(
        &mut s,
        r#"{"type":"usage","usage":{"inputTokens":null,"outputTokens":"7","cacheReadTokens":[1,null],"cacheWriteTokens":{},"totalTokens":false,"nativeCost":-0}}"#,
    );
    let snap = reducer::snapshot(&s);
    let u = snap.get("usage").unwrap();
    assert_eq!(u.get("inputTokens"), Some(&JsValue::Number(0.0)));
    assert_eq!(u.get("outputTokens"), Some(&"07".into()));
    assert_eq!(u.get("cacheReadTokens"), Some(&"01,".into()));
    assert_eq!(u.get("cacheWriteTokens"), Some(&"0[object Object]".into()));
    assert_eq!(u.get("totalTokens"), Some(&JsValue::Number(0.0)));
    assert_eq!(
        u.get("nativeCost").unwrap().as_number().unwrap().to_bits(),
        0.0f64.to_bits()
    );
    apply(
        &mut s,
        r#"{"type":"usage","usage":{"inputTokens":3,"outputTokens":2,"cacheReadTokens":null,"totalTokens":true}}"#,
    );
    let snap = reducer::snapshot(&s);
    let u = snap.get("usage").unwrap();
    assert_eq!(u.get("outputTokens"), Some(&"072".into()));
    assert_eq!(u.get("cacheReadTokens"), Some(&"01,0".into()));
    assert_eq!(u.get("totalTokens"), Some(&JsValue::Number(1.0)));
    let events = obj([
        ("type", "usage".into()),
        (
            "usage",
            obj([
                ("inputTokens", JsValue::Number(f64::NAN)),
                ("nativeCost", JsValue::Number(f64::INFINITY)),
            ]),
        ),
    ]);
    reducer::apply(&mut s, &cap(&events)).unwrap();
    let snap = reducer::snapshot(&s);
    let u = snap.get("usage").unwrap();
    assert!(u.get("inputTokens").unwrap().as_number().unwrap().is_nan());
    assert_eq!(
        u.get("nativeCost").unwrap().as_number(),
        Some(f64::INFINITY)
    );
    let before = reducer::snapshot(&s);
    type_error(
        reducer::apply(
            &mut s,
            &cap(&json(
                r#"{"type":"usage","usage":{"inputTokens":5,"cacheReadTokens":{"toString":null}}}"#,
            )),
        )
        .unwrap_err(),
        "Cannot convert object to primitive value",
    );
    same(
        &JsValue::Object(reducer::snapshot(&s)),
        &JsValue::Object(before.clone()),
    );
}

#[test]
fn open_model_finish_error_presence_and_truthiness_are_preserved() {
    for model in [
        JsValue::Undefined,
        JsValue::Null,
        JsValue::Bool(false),
        "".into(),
        json("[]"),
        JsValue::Number(7.0),
        json(r#"{"raw":"\ud800"}"#),
    ] {
        let mut s = reducer::create(&JsValue::Undefined);
        reducer::apply(
            &mut s,
            &cap(&obj([
                ("type", "request_started".into()),
                ("model", model.clone()),
            ])),
        )
        .unwrap();
        for finish in [
            "future_finish".into(),
            json("[]"),
            JsValue::Bool(false),
            JsValue::Null,
            JsValue::Undefined,
        ] {
            reducer::apply(
                &mut s,
                &cap(&obj([
                    ("type", "message_end".into()),
                    ("finishReason", finish.clone()),
                ])),
            )
            .unwrap();
            let out = reducer::assemble(&mut s).unwrap();
            let expected = if matches!(finish, JsValue::Null | JsValue::Undefined) {
                JsValue::from("unknown")
            } else {
                finish.clone()
            };
            assert_eq!(out.value().get("finishReason"), Some(&expected));
            let metadata = out.value().get("message").unwrap().get("metadata");
            if matches!(
                model,
                JsValue::Array(_) | JsValue::Object(_) | JsValue::Number(7.0)
            ) {
                assert_eq!(metadata.unwrap().get("model"), Some(&model));
            } else {
                assert!(metadata.is_none());
            }
            for key in ["usage", "error", "hasRepairedToolCalls"] {
                assert!(out.value().get(key).unwrap().is_undefined());
            }
        }
    }
    let mut s = reducer::create(&JsValue::Undefined);
    apply(&mut s, r#"{"type":"error","error":{}}"#);
    apply(&mut s, r#"{"type":"message_end","finishReason":null}"#);
    assert_eq!(
        reducer::assemble(&mut s)
            .unwrap()
            .value()
            .get("finishReason"),
        Some(&"error".into())
    );
}

#[test]
fn lax_tool_spreads_keep_utf16_indices_type_override_and_property_slots() {
    let mut s = reducer::create(&"r".into());
    apply(
        &mut s,
        r#"{"type":"tool_call_end","toolCall":{"9":"nine","2":"two","type":"text","id":" spaced ","name":null},"wasRepaired":[]}"#,
    );
    apply(&mut s, r#"{"type":"tool_call_end","toolCall":"\ud800x"}"#);
    assert_eq!(
        keys(s.content[0].value().as_object().unwrap()),
        ["2", "9", "type", "id", "name"]
    );
    assert_eq!(s.content[0].value().get("type"), Some(&"text".into()));
    assert_eq!(
        s.content[1]
            .value()
            .get("0")
            .unwrap()
            .as_string()
            .unwrap()
            .units(),
        &[0xd800]
    );
    let out = reducer::assemble(&mut s).unwrap();
    let JsValue::Array(calls) = out.value().get("toolCalls").unwrap() else {
        panic!()
    };
    assert_eq!(calls[0].get("id"), Some(&"spaced".into()));
    assert_eq!(calls[1].get("id"), Some(&"call_1".into()));
    // Original content mapping skips type-overridden blocks, then consumes its
    // first normalized call for the next tool block. Preserve this source quirk.
    let JsValue::Array(blocks) = out.value().get("message").unwrap().get("content").unwrap() else {
        panic!()
    };
    assert_eq!(blocks[0].get("id"), Some(&" spaced ".into()));
    assert_eq!(blocks[1].get("id"), Some(&"spaced".into()));
    assert_eq!(
        out.value().get("hasRepairedToolCalls"),
        Some(&JsValue::Bool(true))
    );
}

#[test]
fn delayed_ids_trim_utf16_collisions_and_falsy_ids_without_fabricating_input() {
    let mut s = reducer::create(&JsValue::Undefined);
    for call in [
        r#"{"id":" x "}"#,
        r#"{"id":"x_2"}"#,
        r#"{"id":"x"}"#,
        r#"{"id":"\ufeff\ud800\ufeff"}"#,
        r#"{"id":"\u0085x\u0085"}"#,
        r#"{"id":false}"#,
        r#"{}"#,
        "[]",
        "false",
        "0",
    ] {
        reducer::apply(
            &mut s,
            &cap(&obj([
                ("type", "tool_call_end".into()),
                ("toolCall", json(call)),
            ])),
        )
        .unwrap();
    }
    let out = reducer::assemble(&mut s).unwrap();
    let JsValue::Array(calls) = out.value().get("toolCalls").unwrap() else {
        panic!()
    };
    let ids = calls
        .iter()
        .map(|c| c.get("id").unwrap().as_string().unwrap().clone())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        [
            "x".into(),
            "x_2".into(),
            "x_3".into(),
            JsString::from_utf16(vec![0xd800]),
            "\u{85}x\u{85}".into(),
            "call_5".into(),
            "call_6".into(),
            "call_7".into(),
            "call_8".into(),
            "call_9".into()
        ]
    );
    assert!(calls.iter().all(|c| c.get("input").is_none()));
    let again = reducer::assemble(&mut s).unwrap();
    assert_eq!(out.value(), again.value());
}

#[test]
fn late_null_call_and_invalid_trim_keep_prior_flush_and_unnormalized_calls() {
    for (raw, message) in [
        (
            JsValue::Null,
            "Cannot read properties of null (reading 'id')",
        ),
        (
            JsValue::Undefined,
            "Cannot read properties of undefined (reading 'id')",
        ),
        (json(r#"{"id":true}"#), "rawId.trim is not a function"),
        (
            json(r#"{"id":{"trim":null}}"#),
            "rawId.trim is not a function",
        ),
    ] {
        let mut s = reducer::create(&"r".into());
        apply(
            &mut s,
            r#"{"type":"tool_call_end","toolCall":{"id":" x "}}"#,
        );
        reducer::apply(
            &mut s,
            &cap(&obj([
                ("type", "tool_call_end".into()),
                ("toolCall", raw.clone()),
            ])),
        )
        .unwrap();
        apply(&mut s, r#"{"type":"text_delta","text":"tail"}"#);
        let calls = s.tool_calls.clone();
        type_error(reducer::assemble(&mut s).unwrap_err(), message);
        assert!(s.text_buffer.is_empty());
        assert_eq!(
            s.content.last().unwrap().value().get("text"),
            Some(&"tail".into())
        );
        assert_eq!(s.tool_calls[0].value(), calls[0].value());
        assert_eq!(s.tool_calls[1].value(), calls[1].value());
        assert_eq!(s.tool_calls[0].value().get("id"), Some(&" x ".into()));
    }
}

#[test]
fn text_exception_after_thinking_flush_retains_committed_block_and_signature_reset() {
    let mut s = reducer::create(&"r".into());
    apply(
        &mut s,
        r#"{"type":"thinking_delta","text":"think","signature":[1]}"#,
    );
    type_error(
        reducer::apply(
            &mut s,
            &cap(&json(r#"{"type":"text_delta","text":{"toString":null}}"#)),
        )
        .unwrap_err(),
        "Cannot convert object to primitive value",
    );
    assert_eq!(s.content.len(), 1);
    assert_eq!(s.content[0].value().get("signature"), Some(&json("[1]")));
    assert!(s.text_buffer.is_empty());
    assert!(s.thinking_buffer.is_empty());
    assert!(
        reducer::snapshot(&s)
            .get("thinkingSignature")
            .unwrap()
            .is_undefined()
    );
}

#[test]
fn block_id_truthy_response_coercion_and_flush_error_leave_buffer() {
    let mut s = reducer::create(&JsValue::Number(7.0));
    apply(&mut s, r#"{"type":"text_delta","text":"a"}"#);
    apply(&mut s, r#"{"type":"thinking_delta","text":"b"}"#);
    apply(&mut s, r#"{"type":"text_delta","text":"c"}"#);
    assert_eq!(
        reducer::block_id(&s, &"text".into()).unwrap(),
        Some("7:text:1".into())
    );
    let blocks = content(&mut s);
    assert_eq!(blocks[1].get("blockId"), Some(&"7:thinking:0".into()));
    assert_eq!(blocks[2].get("blockId"), Some(&"7:text:1".into()));
    let mut s = reducer::create(&json(r#"{"toString":null}"#));
    apply(&mut s, r#"{"type":"text_delta","text":"kept"}"#);
    type_error(
        reducer::assemble(&mut s).unwrap_err(),
        "Cannot convert object to primitive value",
    );
    assert!(s.content.is_empty());
    assert!(s.text_buffer.equals_str("kept"));
}

#[test]
fn own_undefined_tool_fields_null_error_and_empty_text_remain_distinct() {
    let mut s = reducer::create(&JsValue::Undefined);
    let raw = obj([
        ("id", JsValue::String(JsString::from_utf16(vec![0xd800]))),
        ("input", JsValue::Undefined),
        ("extension", JsValue::Null),
    ]);
    reducer::apply(
        &mut s,
        &cap(&obj([
            ("type", "tool_call_end".into()),
            ("toolCall", raw.clone()),
        ])),
    )
    .unwrap();
    apply(&mut s, r#"{"type":"text_delta","text":""}"#);
    apply(&mut s, r#"{"type":"error","error":null}"#);
    let out = reducer::assemble(&mut s).unwrap();
    let JsValue::Array(calls) = out.value().get("toolCalls").unwrap() else {
        panic!()
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(&calls[0], &raw);
    assert!(calls[0].get("input").unwrap().is_undefined());
    assert!(calls[0].get("name").is_none());
    assert_eq!(calls[0].get("extension"), Some(&JsValue::Null));
    assert_eq!(out.value().get("error"), Some(&JsValue::Null));
    assert_eq!(out.value().get("finishReason"), Some(&"error".into()));
    assert_eq!(
        out.value().get("hasMessageEnd"),
        Some(&JsValue::Bool(false))
    );
    let JsValue::Array(blocks) = out.value().get("message").unwrap().get("content").unwrap() else {
        panic!()
    };
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].get("input").unwrap().is_undefined());
    assert!(blocks[0].get("name").is_none());
}

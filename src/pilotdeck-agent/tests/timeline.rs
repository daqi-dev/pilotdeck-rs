//! Expectations derived from pinned TurnTimeline.ts and timeline.spec.ts;
//! these are author supplementary tests, not original executable parity.
use pilotdeck_agent::{
    SourceTimelineKey, SourceTimelinePosition, SourceTimelinePrevious, TimelineErrorKind,
    TurnTimeline,
};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};

fn previous(p: &SourceTimelinePosition) -> Option<SourceTimelineKey> {
    match &p.previous_id {
        SourceTimelinePrevious::Present(k) => Some(k.clone()),
        _ => None,
    }
}
fn snapshot(raw: &str) -> BoundarySnapshot {
    BoundarySnapshot::capture(&parse_json(raw).unwrap())
}
fn num(v: &JsValue, key: &str) -> f64 {
    v.get(key).unwrap().as_number().unwrap()
}
fn text(v: &JsValue, key: &str) -> JsString {
    v.get(key).unwrap().as_string().unwrap().clone()
}
fn timeline(b: &BoundarySnapshot) -> &JsValue {
    b.value().get("timeline").unwrap()
}
fn block(b: &BoundarySnapshot, index: usize) -> &JsValue {
    match b.value().get("content").unwrap() {
        JsValue::Array(a) => &a[index],
        _ => panic!("content array"),
    }
}
fn event(t: &mut TurnTimeline, raw: &str) -> BoundarySnapshot {
    t.event(&snapshot(raw)).unwrap()
}

#[test]
fn allocation_update_and_empty_id_predecessor_match_source() {
    let mut t = TurnTimeline::new("turn".into());
    let a = t.position("".into(), false);
    assert_eq!((a.order, a.revision, previous(&a)), (0.0, 1.0, None));
    let b = t.update("b".into());
    assert_eq!((b.order, b.revision, previous(&b)), (1.0, 3.0, None));
    let again = t.position("b".into(), true);
    assert_eq!((again.order, again.revision), (1.0, 3.0));
    assert_eq!(t.update("b".into()).revision, 4.0);
}

#[test]
fn provisional_slots_do_not_enter_chain_until_commit_and_do_not_rewind_tail() {
    let mut t = TurnTimeline::new("turn".into());
    t.update("before".into());
    let reserved = t.position("tool:call".into(), true);
    let after = t.update("after".into());
    assert_eq!(previous(&after), Some("before".into()));
    let committed = t.position("tool:call".into(), false);
    assert_eq!(
        (committed.order, committed.revision),
        (reserved.order, reserved.revision)
    );
    assert_eq!(previous(&committed), Some("before".into()));
    assert_eq!(
        previous(&t.position("last".into(), false)),
        Some("after".into())
    );
}

#[test]
fn utf16_offsets_and_revisions_share_live_and_durable_coordinates() {
    let mut t = TurnTimeline::new(JsString::from_utf16(vec![0xd800]));
    let first = event(
        &mut t,
        r#"{"type":"model_event","blockId":"b","event":{"type":"text_delta","text":"😀\ud800"}}"#,
    );
    assert_eq!(
        (
            num(timeline(&first), "offset"),
            num(timeline(&first), "revision")
        ),
        (0.0, 2.0)
    );
    let second = event(
        &mut t,
        r#"{"type":"model_event","blockId":"b","event":{"type":"thinking_delta","text":"x"}}"#,
    );
    assert_eq!(
        (
            num(timeline(&second), "offset"),
            num(timeline(&second), "revision")
        ),
        (3.0, 3.0)
    );
    let complete = t
        .message(&snapshot(
            r#"{"role":"assistant","content":[{"type":"text","blockId":"b","text":"😀\ud800x"}]}"#,
        ))
        .unwrap();
    let p = block(&complete, 0).get("timeline").unwrap();
    assert_eq!(num(p, "order"), num(timeline(&first), "order"));
    assert_eq!(num(p, "revision"), 4.0);
    assert!(p.get("offset").is_none());
    assert_eq!(text(p, "turnId").units(), &[0xd800]);
}

#[test]
fn request_retry_retires_provisional_only_preserving_gaps_offsets_and_last_boundary() {
    let mut t = TurnTimeline::new("turn".into());
    event(
        &mut t,
        r#"{"type":"model_event","blockId":"first","event":{"type":"text_delta","text":"a"}}"#,
    );
    event(
        &mut t,
        r#"{"type":"model_event","event":{"type":"tool_call_start","id":"discard"}}"#,
    );
    event(&mut t, r#"{"type":"model_request_started"}"#);
    let end = event(
        &mut t,
        r#"{"type":"model_event","event":{"type":"message_end"}}"#,
    );
    assert_eq!(
        num(end.value().get("streamBoundary").unwrap(), "through"),
        0.0
    );
    let next = event(
        &mut t,
        r#"{"type":"model_event","blockId":"retry","event":{"type":"text_delta","text":"b"}}"#,
    );
    assert_eq!(num(timeline(&next), "order"), 2.0);
    assert_eq!(text(timeline(&next), "previousId"), JsString::from("first"));
    assert_eq!(t.position("tool:discard".into(), false).order, 3.0);
    let again = event(
        &mut t,
        r#"{"type":"model_event","blockId":"first","event":{"type":"text_delta","text":"c"}}"#,
    );
    assert_eq!(num(timeline(&again), "offset"), 1.0);
}

#[test]
fn start_and_complete_only_tool_frames_both_reserve_before_later_text() {
    for with_start in [true, false] {
        let mut t = TurnTimeline::new("turn".into());
        event(
            &mut t,
            r#"{"type":"model_event","blockId":"before","event":{"type":"text_delta","text":"Before"}}"#,
        );
        if with_start {
            event(
                &mut t,
                r#"{"type":"model_event","event":{"type":"tool_call_start","id":"call"}}"#,
            );
        }
        event(
            &mut t,
            r#"{"type":"model_event","event":{"type":"tool_call_end","toolCall":{"id":"call"}}}"#,
        );
        let after = event(
            &mut t,
            r#"{"type":"model_event","blockId":"after","event":{"type":"text_delta","text":"After"}}"#,
        );
        let message=t.message(&snapshot(r#"{"content":[{"type":"text","blockId":"before"},{"type":"tool_call","id":"call"},{"type":"text","blockId":"after"}]}"#)).unwrap();
        for i in 0..3 {
            assert_eq!(
                num(block(&message, i).get("timeline").unwrap(), "order"),
                i as f64
            );
        }
        assert_eq!(
            text(timeline(&after), "previousId"),
            JsString::from("before")
        );
    }
}

#[test]
fn existing_nonnull_timeline_skips_allocator_even_for_lax_direct_id() {
    let mut t = TurnTimeline::new("turn".into());
    let input = snapshot(
        r#"{"content":[{"type":"text","blockId":"b","timeline":false},{"type":"text","blockId":42,"timeline":0},{"type":"tool_call","id":"t","timeline":{"own":null}}]}"#,
    );
    let output = t.message(&input).unwrap();
    assert_eq!(output.value(), input.value());
    assert_eq!(t.position("next".into(), false).revision, 1.0);
}

#[test]
fn null_and_own_undefined_timeline_evaluate_once_then_durable_echo_is_unchanged() {
    let mut o = JsObject::new();
    o.insert("type".into(), "thinking".into());
    o.insert("blockId".into(), "think".into());
    o.insert("timeline".into(), JsValue::Undefined);
    let mut m = JsObject::new();
    m.insert(
        "content".into(),
        JsValue::Array(vec![
            JsValue::Object(o),
            parse_json(r#"{"type":"text","blockId":"text","timeline":null}"#).unwrap(),
        ]),
    );
    let mut t = TurnTimeline::new("turn".into());
    let once = t
        .message(&BoundarySnapshot::capture(&JsValue::Object(m)))
        .unwrap();
    assert_eq!(
        num(block(&once, 0).get("timeline").unwrap(), "revision"),
        2.0
    );
    assert_eq!(
        num(block(&once, 1).get("timeline").unwrap(), "revision"),
        4.0
    );
    let twice = t.message(&once).unwrap();
    assert_eq!(once.value(), twice.value());
    assert_eq!(t.position("next".into(), false).revision, 5.0);
}

#[test]
fn committed_first_tool_has_own_undefined_previous_id_appended_after_revision() {
    let mut t = TurnTimeline::new("turn".into());
    t.position("tool:t".into(), true);
    let calls = event(
        &mut t,
        r#"{"type":"tool_calls_detected","calls":[{"id":"t","timeline":"replace"}]}"#,
    );
    let call = match calls.value().get("calls").unwrap() {
        JsValue::Array(a) => &a[0],
        _ => panic!(),
    };
    let p = call.get("timeline").unwrap();
    assert!(matches!(p.get("previousId"), Some(JsValue::Undefined)));
    let keys: Vec<_> = p
        .as_object()
        .unwrap()
        .entries()
        .iter()
        .map(|(k, _)| k.to_utf8().unwrap())
        .collect();
    assert_eq!(
        keys,
        vec!["version", "turnId", "id", "order", "revision", "previousId"]
    );
    assert_eq!(num(p, "revision"), 1.0);
    let msg = t
        .message(&snapshot(r#"{"content":[{"type":"tool_call","id":"t"}]}"#))
        .unwrap();
    assert!(matches!(
        block(&msg, 0).get("timeline").unwrap().get("previousId"),
        Some(JsValue::Undefined)
    ));
}

#[test]
fn empty_predecessor_property_is_appended_when_provisional_was_created_without_it() {
    let mut t = TurnTimeline::new("turn".into());
    t.position("".into(), false);
    t.position("tool:t".into(), true);
    let msg = t
        .message(&snapshot(r#"{"content":[{"type":"tool_call","id":"t"}]}"#))
        .unwrap();
    let p = block(&msg, 0).get("timeline").unwrap();
    assert_eq!(text(p, "previousId"), JsString::from(""));
    assert!(
        p.as_object()
            .unwrap()
            .entries()
            .last()
            .unwrap()
            .0
            .equals_str("previousId")
    );
}

#[test]
fn result_reference_compaction_and_steer_share_durable_live_slots() {
    let mut t = TurnTimeline::new("turn".into());
    let result = event(
        &mut t,
        r#"{"type":"tool_result","result":{"toolCallId":"t"}}"#,
    );
    let m = t
        .message(&snapshot(
            r#"{"content":[{"type":"tool_result_reference","toolCallId":"t"}]}"#,
        ))
        .unwrap();
    assert_eq!(
        num(timeline(&result), "order"),
        num(block(&m, 0).get("timeline").unwrap(), "order")
    );
    let persisted = t.position("compact:c".into(), false);
    let started = event(&mut t, r#"{"type":"compact_started","compactionId":"c"}"#);
    let completed = event(&mut t, r#"{"type":"compact_completed","compactionId":"c"}"#);
    assert_eq!(num(timeline(&started), "order"), persisted.order);
    assert_eq!(
        num(timeline(&completed), "revision"),
        num(timeline(&started), "revision") + 1.0
    );
    let steer = event(
        &mut t,
        r#"{"type":"steer_applied","message":{"metadata":{"queueItemId":"q"},"content":[{"type":"text","text":"wait"},{"type":"image"}]}}"#,
    );
    let ms = BoundarySnapshot::capture(steer.value().get("message").unwrap());
    assert_eq!(block(&ms, 0).get("timeline"), block(&ms, 1).get("timeline"));
    assert_eq!(
        text(block(&ms, 0).get("timeline").unwrap(), "id"),
        JsString::from("steer:q")
    );
}

#[test]
fn stream_boundaries_only_for_source_kinds_and_never_for_empty_block_id() {
    let mut t = TurnTimeline::new("turn".into());
    let empty = event(
        &mut t,
        r#"{"type":"model_event","blockId":"","event":{"type":"text_delta","text":"x"}}"#,
    );
    assert!(empty.value().get("timeline").is_none());
    let early = event(&mut t, r#"{"type":"model_event","event":{"type":"error"}}"#);
    assert!(early.value().get("streamBoundary").is_none());
    event(
        &mut t,
        r#"{"type":"model_event","blockId":"b","event":{"type":"text_delta","text":"x"}}"#,
    );
    for kind in ["message_end", "tool_call_start", "error"] {
        let b = event(
            &mut t,
            &format!(r#"{{"type":"model_event","event":{{"type":"{kind}","id":"t"}}}}"#),
        );
        assert_eq!(
            num(b.value().get("streamBoundary").unwrap(), "through"),
            0.0
        );
    }
    let end = event(
        &mut t,
        r#"{"type":"model_event","event":{"type":"tool_call_end","toolCall":{"id":"t"}}}"#,
    );
    assert!(end.value().get("streamBoundary").is_none());
}

#[test]
fn source_template_ids_preserve_missing_null_numeric_and_wtf16_values() {
    let mut t = TurnTimeline::new("turn".into());
    for (raw, expected) in [
        (r#"{"type":"compact_started"}"#, "compact:undefined"),
        (
            r#"{"type":"compact_started","compactionId":null}"#,
            "compact:null",
        ),
        (
            r#"{"type":"compact_started","compactionId":-0}"#,
            "compact:0",
        ),
        (
            r#"{"type":"compact_started","compactionId":[1,null,true]}"#,
            "compact:1,,true",
        ),
    ] {
        let b = event(&mut t, raw);
        assert_eq!(text(timeline(&b), "id"), JsString::from(expected));
    }
    let b = event(
        &mut t,
        r#"{"type":"tool_result","result":{"toolCallId":"\ud800"}}"#,
    );
    assert_eq!(text(timeline(&b), "id").units().last(), Some(&0xd800));
}

#[test]
fn immutable_annotation_preserves_extensions_and_unknown_event_exactly() {
    let mut t = TurnTimeline::new("turn".into());
    let original = snapshot(
        r#"{"type":"assistant_message","unknown":{"nested":[null,"\ud800"]},"message":{"extra":false,"content":[{"type":"text","blockId":"b","extra":[1,2]}]}}"#,
    );
    let before = original.value().clone();
    let annotated = t.event(&original).unwrap();
    assert_eq!(original.value(), &before);
    assert_eq!(
        original.value().get("unknown"),
        annotated.value().get("unknown")
    );
    assert!(
        original
            .value()
            .get("message")
            .unwrap()
            .get("content")
            .is_some()
    );
    let unknown = snapshot(
        r#"{"type":"subagent_model_event","event":{"type":"text_delta","text":"x"},"blockId":"child","timeline":null}"#,
    );
    assert_eq!(t.event(&unknown).unwrap().value(), unknown.value());
    assert_eq!(t.position("next".into(), false).revision, 3.0);
}

#[test]
fn malformed_and_native_binding_inputs_are_explicit_errors_not_empty_annotations() {
    let mut t = TurnTimeline::new("turn".into());
    for raw in [
        r#"null"#,
        r#"{"type":"model_event"}"#,
        r#"{"type":"model_event","event":{"type":"text_delta","text":null},"blockId":"b"}"#,
        r#"{"type":"tool_calls_detected","calls":null}"#,
    ] {
        assert!(t.event(&snapshot(raw)).is_err(), "{raw}");
    }
    assert!(
        t.message(&snapshot(r#"{"content":[{"type":"text","blockId":{}}]}"#))
            .is_err()
    );
    assert!(
        t.event(&snapshot(
            r#"{"type":"compact_started","compactionId":{"toString":0}}"#
        ))
        .is_err()
    );
    assert_eq!(t.position("valid".into(), false).revision, 1.0);
}

#[test]
fn lax_primitives_preserve_original_no_op_paths_without_invented_empty_objects() {
    let mut t = TurnTimeline::new("turn".into());
    for raw in [
        "42",
        "false",
        r#""unknown""#,
        r#"{"type":"model_event","event":42}"#,
    ] {
        let original = snapshot(raw);
        assert_eq!(t.event(&original).unwrap().value(), original.value());
    }
    let original = snapshot(r#"{"content":[42,false,"unknown",[]]}"#);
    assert_eq!(t.message(&original).unwrap().value(), original.value());
    assert!(
        t.message(&snapshot(
            r#"{"content":[42],"metadata":{"queueItemId":"q"}}"#
        ))
        .is_err()
    );
    assert_eq!(t.position("next".into(), false).revision, 2.0);
}

// Independent original759 packet counterexamples 0734/0735/0736. Rust execution
// remains author supplementary verification; the original packet is immutable.
#[test]
fn primitive_assignment_allocates_rhs_before_exact_type_error_and_retains_prefix() {
    let input = snapshot(
        r#"{"content":[{"type":"text","blockId":"first"},7],"metadata":{"queueItemId":"queue"},"extra":"\ud800"}"#,
    );
    let mut t = TurnTimeline::new("turn".into());
    let error = t.message(&input).unwrap_err();
    assert_eq!(error.kind, TimelineErrorKind::TypeError);
    assert_eq!(
        error.message,
        JsString::from("Cannot create property 'timeline' on number '7'")
    );
    let partial = error.partial_input.unwrap();
    assert_eq!(
        num(block(&partial, 0).get("timeline").unwrap(), "revision"),
        2.0
    );
    assert_eq!(block(&partial, 1), &JsValue::Number(7.0));
    assert_eq!(partial.value().get("extra"), input.value().get("extra"));
    assert!(block(&input, 0).get("timeline").is_none());
    let next = t.position("followup".into(), false);
    assert_eq!(
        (next.order, next.revision, previous(&next)),
        (2.0, 4.0, Some("steer:queue".into()))
    );
}
#[test]
fn nullish_later_block_preserves_previous_input_mutation_and_stops_before_queue() {
    for undefined in [false, true] {
        let mut input = parse_json(r#"{"content":[{"type":"tool_call","id":"one"},null],"metadata":{"queueItemId":"queue"}}"#).unwrap().as_object().unwrap().clone();
        if undefined {
            input.insert(
                "content".into(),
                JsValue::Array(vec![
                    parse_json(r#"{"type":"tool_call","id":"one"}"#).unwrap(),
                    JsValue::Undefined,
                ]),
            );
        }
        let input = BoundarySnapshot::capture(&JsValue::Object(input));
        let mut t = TurnTimeline::new("turn".into());
        let error = t.message(&input).unwrap_err();
        assert_eq!(error.kind, TimelineErrorKind::TypeError);
        assert_eq!(
            error.message,
            JsString::from(if undefined {
                "Cannot read properties of undefined (reading 'type')"
            } else {
                "Cannot read properties of null (reading 'type')"
            })
        );
        let partial = error.partial_input.unwrap();
        assert_eq!(
            num(block(&partial, 0).get("timeline").unwrap(), "revision"),
            2.0
        );
        assert_eq!(block(&partial, 1), block(&input, 1));
        let next = t.position("followup".into(), false);
        assert_eq!(
            (next.order, next.revision, previous(&next)),
            (1.0, 3.0, Some("tool:one".into()))
        );
    }
}
#[test]
fn nullish_delta_length_and_event_type_exact_source_exceptions_do_not_allocate() {
    for undefined in [false, true] {
        let value = if undefined {
            JsValue::Undefined
        } else {
            JsValue::Null
        };
        let mut inner = JsObject::new();
        inner.insert("type".into(), "text_delta".into());
        inner.insert("text".into(), value.clone());
        let mut outer = JsObject::new();
        outer.insert("type".into(), "model_event".into());
        outer.insert("blockId".into(), "b".into());
        outer.insert("event".into(), JsValue::Object(inner));
        let input = BoundarySnapshot::capture(&JsValue::Object(outer));
        let mut t = TurnTimeline::new("turn".into());
        let error = t.event(&input).unwrap_err();
        assert_eq!(error.kind, TimelineErrorKind::TypeError);
        assert_eq!(
            error.message,
            JsString::from(if undefined {
                "Cannot read properties of undefined (reading 'length')"
            } else {
                "Cannot read properties of null (reading 'length')"
            })
        );
        assert_eq!(error.partial_input.unwrap().value(), input.value());
        let next = event(
            &mut t,
            r#"{"type":"model_event","blockId":"b","event":{"type":"text_delta","text":"x"}}"#,
        );
        assert_eq!(
            (
                num(timeline(&next), "order"),
                num(timeline(&next), "revision"),
                num(timeline(&next), "offset")
            ),
            (0.0, 2.0, 0.0)
        );
        let mut t = TurnTimeline::new("turn".into());
        let input = BoundarySnapshot::capture(&value);
        let error = t.event(&input).unwrap_err();
        assert_eq!(
            error.message,
            JsString::from(if undefined {
                "Cannot read properties of undefined (reading 'type')"
            } else {
                "Cannot read properties of null (reading 'type')"
            })
        );
        assert_eq!(error.partial_input.unwrap().value(), input.value());
        assert_eq!(t.position("tail".into(), false).revision, 1.0);
    }
}
#[test]
fn wrapped_message_failure_returns_partial_event_and_keeps_all_remaining_blocks() {
    let input = snapshot(
        r#"{"type":"assistant_message","extra":17,"message":{"content":[{"type":"text","blockId":"first"},null,{"type":"text","blockId":"unseen"}]}}"#,
    );
    let mut t = TurnTimeline::new("turn".into());
    let error = t.event(&input).unwrap_err();
    let partial = error.partial_input.unwrap();
    let message = BoundarySnapshot::capture(partial.value().get("message").unwrap());
    assert_eq!(
        num(block(&message, 0).get("timeline").unwrap(), "revision"),
        2.0
    );
    assert_eq!(block(&message, 1), &JsValue::Null);
    assert!(block(&message, 2).get("timeline").is_none());
    assert_eq!(partial.value().get("extra"), input.value().get("extra"));
    assert_eq!(t.position("tail".into(), false).order, 1.0);
}
#[test]
fn primitive_no_queue_control_does_not_allocate_and_tostring_failure_is_type_error() {
    let mut t = TurnTimeline::new("turn".into());
    let input = snapshot(r#"{"content":[7,true,"x"]}"#);
    assert_eq!(t.message(&input).unwrap().value(), input.value());
    let error = t
        .event(&snapshot(
            r#"{"type":"compact_started","compactionId":{"toString":0}}"#,
        ))
        .unwrap_err();
    assert_eq!(error.kind, TimelineErrorKind::TypeError);
    assert_eq!(
        error.message,
        JsString::from("Cannot convert object to primitive value")
    );
    assert_eq!(t.position("tail".into(), false).revision, 1.0);
}

#[test]
fn numeric_map_keys_are_not_coerced_to_strings_and_predecessors_keep_numbers() {
    let mut t = TurnTimeline::new("turn".into());
    let m = t
        .message(&snapshot(
            r#"{"content":[{"type":"text","blockId":7},{"type":"thinking","blockId":"7"}]}"#,
        ))
        .unwrap();
    let p = block(&m, 0).get("timeline").unwrap();
    assert_eq!(p.get("id"), Some(&JsValue::Number(7.0)));
    let p = block(&m, 1).get("timeline").unwrap();
    assert_eq!(p.get("id"), Some(&JsValue::from("7")));
    assert_eq!(p.get("previousId"), Some(&JsValue::Number(7.0)));
    assert_eq!(num(p, "order"), 1.0);
    let delta = event(
        &mut t,
        r#"{"type":"model_event","blockId":7,"event":{"type":"text_delta","text":"x"}}"#,
    );
    assert_eq!(num(timeline(&delta), "order"), 0.0);
    assert_eq!(timeline(&delta).get("id"), Some(&JsValue::Number(7.0)));
}
#[test]
fn string_offsets_use_js_addition_and_never_numeric_parse() {
    let mut t = TurnTimeline::new("turn".into());
    let first = event(
        &mut t,
        r#"{"type":"model_event","blockId":"b","event":{"type":"text_delta","text":{"length":"2"}}}"#,
    );
    assert_eq!(num(timeline(&first), "offset"), 0.0);
    let second = event(
        &mut t,
        r#"{"type":"model_event","blockId":"b","event":{"type":"text_delta","text":"x"}}"#,
    );
    assert_eq!(text(timeline(&second), "offset"), JsString::from("02"));
    let third = event(
        &mut t,
        r#"{"type":"model_event","blockId":"b","event":{"type":"text_delta","text":{"length":null}}}"#,
    );
    assert_eq!(text(timeline(&third), "offset"), JsString::from("021"));
    let fourth = event(
        &mut t,
        r#"{"type":"model_event","blockId":"b","event":{"type":"text_delta","text":"x"}}"#,
    );
    assert_eq!(text(timeline(&fourth), "offset"), JsString::from("021null"));
    let end = event(
        &mut t,
        r#"{"type":"model_event","event":{"type":"message_end"}}"#,
    );
    assert_eq!(
        num(end.value().get("streamBoundary").unwrap(), "revision"),
        6.0
    );
}
#[test]
fn boundary_position_and_update_preserve_own_undefined_after_provisional_commit() {
    let mut t = TurnTimeline::new("turn".into());
    t.position("a".into(), true);
    let p = t.position_boundary("a".into(), false);
    assert!(matches!(
        p.value().get("previousId"),
        Some(JsValue::Undefined)
    ));
    assert!(
        p.value()
            .as_object()
            .unwrap()
            .entries()
            .last()
            .unwrap()
            .0
            .equals_str("previousId")
    );
    let updated = t.update_boundary("a".into());
    assert!(matches!(
        updated.value().get("previousId"),
        Some(JsValue::Undefined)
    ));
    assert!(
        updated
            .value()
            .as_object()
            .unwrap()
            .entries()
            .last()
            .unwrap()
            .0
            .equals_str("previousId")
    );
    assert_eq!(num(updated.value(), "revision"), 2.0);
}

#[test]
fn source_exact_noniterable_and_inner_getter_failures_preserve_boundary() {
    for (raw, message) in [
        (r#"{"content":null}"#, "message.content is not iterable"),
        (r#"{"content":7}"#, "message.content is not iterable"),
        (r#"{}"#, "message.content is not iterable"),
    ] {
        let mut t = TurnTimeline::new("turn".into());
        let input = snapshot(raw);
        let error = t.message(&input).unwrap_err();
        assert_eq!(error.kind, TimelineErrorKind::TypeError);
        assert_eq!(error.message, JsString::from(message));
        assert_eq!(error.partial_input.unwrap().value(), input.value());
        assert_eq!(t.position("tail".into(), false).revision, 1.0);
    }
    for (raw, message) in [
        (
            r#"{"type":"model_event","event":null}"#,
            "Cannot read properties of null (reading 'type')",
        ),
        (
            r#"{"type":"model_event"}"#,
            "Cannot read properties of undefined (reading 'type')",
        ),
        (
            r#"{"type":"tool_result","result":null}"#,
            "Cannot read properties of null (reading 'toolCallId')",
        ),
        (
            r#"{"type":"model_event","event":{"type":"tool_call_end","toolCall":null}}"#,
            "Cannot read properties of null (reading 'id')",
        ),
    ] {
        let mut t = TurnTimeline::new("turn".into());
        let input = snapshot(raw);
        let error = t.event(&input).unwrap_err();
        assert_eq!(error.kind, TimelineErrorKind::TypeError);
        assert_eq!(error.message, JsString::from(message));
        assert_eq!(error.partial_input.unwrap().value(), input.value());
        assert_eq!(t.position("tail".into(), false).revision, 1.0);
    }
}
#[test]
fn detected_calls_partial_prefix_and_assignment_rhs_follow_source_order() {
    for (last, order, revision, previous_id, message) in [
        (
            "null",
            1.0,
            2.0,
            "tool:one",
            "Cannot read properties of null (reading 'id')",
        ),
        (
            "7",
            2.0,
            3.0,
            "tool:undefined",
            "Cannot create property 'timeline' on number '7'",
        ),
    ] {
        let input = snapshot(&format!(
            r#"{{"type":"tool_calls_detected","calls":[{{"id":"one"}},{last}]}}"#
        ));
        let mut t = TurnTimeline::new("turn".into());
        let error = t.event(&input).unwrap_err();
        assert_eq!(error.message, JsString::from(message));
        let partial = error.partial_input.unwrap();
        let JsValue::Array(calls) = partial.value().get("calls").unwrap() else {
            panic!()
        };
        assert_eq!(num(calls[0].get("timeline").unwrap(), "revision"), 1.0);
        let next = t.position("tail".into(), false);
        assert_eq!(
            (next.order, next.revision, previous(&next)),
            (order, revision, Some(previous_id.into()))
        );
    }
}

#[test]
fn string_iterables_preserve_source_code_point_assignment_errors_and_empty_noop() {
    let mut t = TurnTimeline::new("turn".into());
    let empty = snapshot(r#"{"type":"tool_calls_detected","calls":""}"#);
    assert_eq!(t.event(&empty).unwrap().value(), empty.value());
    let error = t
        .event(&snapshot(r#"{"type":"tool_calls_detected","calls":"😀x"}"#))
        .unwrap_err();
    assert_eq!(
        error.message,
        JsString::from("Cannot create property 'timeline' on string '😀'")
    );
    let next = t.position("tail".into(), false);
    assert_eq!(
        (next.order, next.revision, previous(&next)),
        (1.0, 2.0, Some("tool:undefined".into()))
    );
    let mut t = TurnTimeline::new("turn".into());
    let error = t
        .message(&snapshot(
            r#"{"content":"\ud800x","metadata":{"queueItemId":"queue"}}"#,
        ))
        .unwrap_err();
    let mut units: Vec<u16> = "Cannot create property 'timeline' on string '"
        .encode_utf16()
        .collect();
    units.push(0xd800);
    units.push(39);
    assert_eq!(error.message.units(), units);
    assert_eq!(t.position("tail".into(), false).order, 1.0);
}

#[test]
fn missing_wrapped_message_exception_does_not_invent_own_undefined_property() {
    let mut t = TurnTimeline::new("turn".into());
    let input = snapshot(r#"{"type":"assistant_message","extra":1}"#);
    let error = t.event(&input).unwrap_err();
    assert_eq!(
        error.message,
        JsString::from("Cannot read properties of undefined (reading 'content')")
    );
    assert_eq!(error.partial_input.unwrap().value(), input.value());
    assert_eq!(t.position("tail".into(), false).revision, 1.0);
}

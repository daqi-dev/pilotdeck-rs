//! Source-derived supplementary tests. Original oracle execution is independent.
use compaction_persistence::{
    CompactionPersistenceInput, CompactionPersistencePlan, CompactionSnapshotError,
};
use pilotdeck_agent::{
    MessageProjection, ProjectedMessage, ProjectionGap, compaction_persistence, ports, tool_context,
};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use ports::PortError;
use tool_context::ContextMessageCollection;
#[path = "../src/compaction_persistence_policy.rs"]
mod compaction_persistence_policy;

fn obj(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
fn arr(values: impl IntoIterator<Item = JsValue>) -> JsValue {
    JsValue::Array(values.into_iter().collect())
}
fn set(value: &mut JsValue, key: &str, field: JsValue) {
    let JsValue::Object(value) = value else {
        panic!("test object");
    };
    value.insert(key.into(), field);
}
fn message() -> JsValue {
    obj([
        ("role", "user".into()),
        ("content", arr([])),
        ("extension", JsValue::Undefined),
    ])
}
fn compact(messages: JsValue, result: JsValue, snapshot: JsValue) -> JsValue {
    obj([
        ("type", "compacted".into()),
        ("messages", messages),
        ("result", result),
        ("snapshot", snapshot),
        ("tier", "summary".into()),
    ])
}
fn result() -> JsValue {
    obj([
        ("compactionId", "compact-1".into()),
        ("trigger", "auto".into()),
        ("preTokens", JsValue::Number(100.0)),
        ("messagesSummarized", JsValue::Number(2.0)),
    ])
}
fn snapshot() -> JsValue {
    obj([
        ("tokens", JsValue::Number(25.0)),
        ("ratio", JsValue::Number(0.25)),
        ("maxContextTokens", JsValue::Number(100.0)),
    ])
}
fn base() -> JsValue {
    compact(arr([message()]), result(), snapshot())
}
fn evaluate(value: &JsValue, advertised: bool) -> CompactionPersistencePlan {
    let boundary = BoundarySnapshot::capture(value);
    compaction_persistence_policy::evaluate(CompactionPersistenceInput {
        compact: &boundary,
        persistence_advertised: advertised,
    })
    .unwrap()
}
fn replacement(value: &CompactionPersistencePlan) -> JsValue {
    match &value.replacement {
        ContextMessageCollection::Items(items) => {
            arr(items.iter().map(|value| value.boundary.value().clone()))
        }
        ContextMessageCollection::BoundaryOnly(value) => value.value().clone(),
    }
}
fn metadata(plan: &CompactionPersistencePlan) -> &JsValue {
    plan.commit
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .boundary
        .boundary
        .value()
        .get("compactMetadata")
        .unwrap()
}
fn extra(plan: &CompactionPersistencePlan) -> &JsValue {
    metadata(plan).get("extra").unwrap()
}
fn keys(value: &JsValue) -> Vec<String> {
    value
        .as_object()
        .unwrap()
        .entries()
        .iter()
        .map(|(key, _)| key.to_utf8().unwrap())
        .collect()
}
fn assert_error(error: &PortError, message: &str, compact: &JsValue) {
    let PortError::SourceException {
        message: actual,
        boundary,
    } = error
    else {
        panic!("known source exception, not accepted unknown");
    };
    assert!(actual.equals_str(message));
    assert_eq!(boundary.value(), compact);
}

#[test]
fn callback_absence_skips_invalid_snapshot_and_map_without_erasing_replacement() {
    let raw = compact(JsValue::Number(7.0), result(), JsValue::Null);
    let plan = evaluate(&raw, false);
    assert!(plan.commit.unwrap().is_none());
    assert_eq!(replacement(&evaluate(&raw, false)), JsValue::Number(7.0));
}
#[test]
fn all_falsy_results_skip_persistence_but_empty_record_does_not() {
    for result in [
        JsValue::Undefined,
        JsValue::Null,
        JsValue::Bool(false),
        JsValue::Number(0.0),
        JsValue::Number(-0.0),
        JsValue::Number(f64::NAN),
        "".into(),
    ] {
        let raw = compact(arr([message()]), result, JsValue::Null);
        assert!(evaluate(&raw, true).commit.unwrap().is_none());
    }
    let raw = compact(arr([]), obj([]), snapshot());
    assert!(evaluate(&raw, true).commit.unwrap().is_some());
}
#[test]
fn exact_metadata_order_own_undefined_and_stringify_omission() {
    let plan = evaluate(&base(), true);
    assert_eq!(
        keys(metadata(&plan)),
        [
            "compactionId",
            "trigger",
            "preTokens",
            "postTokens",
            "messagesSummarized",
            "summaryGenerated",
            "checkpointMerged",
            "finalRatio",
            "extra"
        ]
    );
    assert_eq!(keys(extra(&plan)), ["tier", "summarySucceeded"]);
    let raw = compact(arr([]), obj([]), obj([]));
    let plan = evaluate(&raw, true);
    assert!(metadata(&plan).get("compactionId").unwrap().is_undefined());
    assert!(extra(&plan).get("tier").is_some());
    let json =
        pilotdeck_protocol::stringify_json(plan.commit.unwrap().unwrap().boundary.boundary.value())
            .unwrap()
            .unwrap();
    assert_eq!(
        json,
        "{\"kind\":\"compact\",\"subtype\":\"compact_boundary\",\"compactMetadata\":{\"summaryGenerated\":false,\"checkpointMerged\":false,\"extra\":{\"tier\":\"summary\",\"summarySucceeded\":false}}}"
    );
}
#[test]
fn target_null_is_owned_and_undefined_is_absent() {
    for (target, present) in [
        (JsValue::Null, true),
        (JsValue::Undefined, false),
        (JsValue::Number(0.0), true),
    ] {
        let mut result = result();
        set(&mut result, "targetPostTokens", target.clone());
        let plan = evaluate(&compact(arr([]), result, snapshot()), true);
        assert_eq!(metadata(&plan).get("targetTokens").is_some(), present);
        if present {
            assert_eq!(metadata(&plan).get("targetTokens"), Some(&target));
        }
    }
}
#[test]
fn summary_nullish_fallback_preserves_explicit_nonboolean_values() {
    for (summary, expected) in [
        (JsValue::Undefined, JsValue::Bool(true)),
        (JsValue::Null, JsValue::Bool(true)),
        (JsValue::Bool(false), JsValue::Bool(false)),
        (JsValue::Number(0.0), JsValue::Number(0.0)),
        ("custom".into(), "custom".into()),
    ] {
        let mut result = result();
        set(&mut result, "summaryGenerated", summary);
        set(&mut result, "summaryMessage", JsValue::Null);
        let plan = evaluate(&compact(arr([]), result, snapshot()), true);
        assert_eq!(metadata(&plan).get("summaryGenerated"), Some(&expected));
        assert_eq!(
            extra(&plan).get("summarySucceeded"),
            Some(&JsValue::Bool(true))
        );
    }
}
#[test]
fn result_error_null_prevents_summary_success_but_does_not_set_outer_error() {
    let mut result = result();
    set(&mut result, "summaryMessage", JsValue::Null);
    set(&mut result, "error", JsValue::Null);
    let plan = evaluate(&compact(arr([]), result, snapshot()), true);
    assert_eq!(
        extra(&plan).get("summarySucceeded"),
        Some(&JsValue::Bool(false))
    );
    assert!(extra(&plan).get("error").is_none());
}
#[test]
fn checkpoint_strict_true_differs_from_cache_truthiness() {
    for (cache, merged, cached) in [
        (JsValue::Bool(true), true, true),
        (JsValue::Number(1.0), false, true),
        ("yes".into(), false, true),
        (JsValue::Bool(false), false, false),
        (obj([]), false, true),
    ] {
        let mut result = result();
        set(&mut result, "cacheReset", cache);
        let plan = evaluate(&compact(arr([]), result, snapshot()), true);
        assert_eq!(
            metadata(&plan).get("checkpointMerged"),
            Some(&JsValue::Bool(merged))
        );
        assert_eq!(extra(&plan).get("cacheReset").is_some(), cached);
    }
}
#[test]
fn explicit_checkpoint_false_zero_and_string_are_not_replaced() {
    for merged in [JsValue::Bool(false), JsValue::Number(0.0), "open".into()] {
        let mut result = result();
        set(&mut result, "checkpointMerged", merged.clone());
        set(&mut result, "cacheReset", JsValue::Bool(true));
        let plan = evaluate(&compact(arr([]), result, snapshot()), true);
        assert_eq!(metadata(&plan).get("checkpointMerged"), Some(&merged));
    }
}
#[test]
fn only_truthy_outer_error_appends_final_budget_fields_in_order() {
    for error in [
        JsValue::Null,
        JsValue::Bool(false),
        JsValue::Number(0.0),
        JsValue::Undefined,
        "".into(),
    ] {
        let mut raw = base();
        set(&mut raw, "error", error);
        assert!(extra(&evaluate(&raw, true)).get("error").is_none());
    }
    let mut raw = base();
    let error = obj([("code", "overflow".into())]);
    set(&mut raw, "error", error.clone());
    let plan = evaluate(&raw, true);
    assert_eq!(
        keys(extra(&plan)),
        [
            "tier",
            "summarySucceeded",
            "finalBudgetTokens",
            "finalUsedTokens",
            "finalBudgetRatio",
            "error"
        ]
    );
    assert_eq!(extra(&plan).get("error"), Some(&error));
}
#[test]
fn markers_only_modify_callback_copy_and_preserve_existing_slots_and_extensions() {
    let mut message = message();
    set(
        &mut message,
        "metadata",
        obj([
            ("compactSnapshotId", "old".into()),
            ("extension", JsValue::Undefined),
            ("compactReplacement", JsValue::Bool(false)),
            ("tail", JsValue::Null),
        ]),
    );
    let raw = compact(arr([message.clone()]), result(), snapshot());
    let before = raw.clone();
    let plan = evaluate(&raw, true);
    assert_eq!(raw, before);
    assert_eq!(replacement(&plan), arr([message]));
    let commit = plan.commit.unwrap().unwrap();
    let marked = commit.messages[0].boundary.value();
    let metadata = marked.get("metadata").unwrap();
    assert_eq!(
        keys(metadata),
        [
            "compactSnapshotId",
            "extension",
            "compactReplacement",
            "tail"
        ]
    );
    assert_eq!(metadata.get("compactSnapshotId"), Some(&"compact-1".into()));
    assert_eq!(
        metadata.get("compactReplacement"),
        Some(&JsValue::Bool(true))
    );
    assert!(metadata.get("extension").unwrap().is_undefined());
}
#[test]
fn primitive_metadata_spread_handles_wtf16_arrays_and_numeric_key_order() {
    let wtf = JsString::from_utf16(vec![0xd800, 0xdc00, 0xdfff]);
    let mut m = message();
    set(&mut m, "metadata", JsValue::String(wtf));
    let plan = evaluate(&compact(arr([m]), result(), snapshot()), true);
    let commit = plan.commit.unwrap().unwrap();
    let meta = commit.messages[0].boundary.value().get("metadata").unwrap();
    assert_eq!(
        keys(meta),
        ["0", "1", "2", "compactReplacement", "compactSnapshotId"]
    );
    assert_eq!(
        meta.get("2"),
        Some(&JsValue::String(JsString::from_utf16(vec![0xdfff])))
    );
    let mut m = message();
    set(
        &mut m,
        "metadata",
        obj([
            ("x", JsValue::Null),
            ("10", JsValue::Number(10.0)),
            ("2", JsValue::Number(2.0)),
            ("01", JsValue::Number(1.0)),
        ]),
    );
    let plan = evaluate(&compact(arr([m]), result(), snapshot()), true);
    let commit = plan.commit.unwrap().unwrap();
    assert_eq!(
        keys(commit.messages[0].boundary.value().get("metadata").unwrap()),
        [
            "2",
            "10",
            "x",
            "01",
            "compactReplacement",
            "compactSnapshotId"
        ]
    );
    let mut m = message();
    set(&mut m, "metadata", arr([JsValue::Undefined, JsValue::Null]));
    let plan = evaluate(&compact(arr([m]), result(), snapshot()), true);
    let commit = plan.commit.unwrap().unwrap();
    assert!(
        commit.messages[0]
            .boundary
            .value()
            .get("metadata")
            .unwrap()
            .get("0")
            .unwrap()
            .is_undefined()
    );
}
#[test]
fn primitive_messages_are_spread_not_fabricated_canonical_messages() {
    let raw = compact(
        arr([JsValue::Number(7.0), "ab".into(), JsValue::Bool(true)]),
        result(),
        snapshot(),
    );
    let plan = evaluate(&raw, true);
    let commit = plan.commit.unwrap().unwrap();
    assert_eq!(keys(commit.messages[0].boundary.value()), ["metadata"]);
    assert_eq!(
        keys(commit.messages[1].boundary.value()),
        ["0", "1", "metadata"]
    );
    assert!(matches!(
        commit.messages[0].normalized,
        MessageProjection::BoundaryOnly(_)
    ));
}
#[test]
fn metadata_boundary_error_precedes_map_and_retains_acknowledged_replacement() {
    let raw = compact(JsValue::Null, result(), JsValue::Null);
    let plan = evaluate(&raw, true);
    assert_eq!(replacement(&plan), JsValue::Null);
    let Err(CompactionSnapshotError::BoundaryConstruction(error)) = plan.commit else {
        panic!("unwrapped boundary-construction phase");
    };
    assert_error(
        &error,
        "Cannot read properties of null (reading 'tokens')",
        &raw,
    );
}
#[test]
fn missing_snapshot_is_boundary_construction_error_not_transport_unknown() {
    let raw = compact(arr([message()]), result(), JsValue::Undefined);
    let plan = evaluate(&raw, true);
    let Err(CompactionSnapshotError::BoundaryConstruction(error)) = plan.commit else {
        panic!("construction phase");
    };
    assert_error(
        &error,
        "Cannot read properties of undefined (reading 'tokens')",
        &raw,
    );
}
#[test]
fn map_and_null_message_errors_are_persistence_payload_phase() {
    for (messages, expected) in [
        (
            JsValue::Null,
            "Cannot read properties of null (reading 'map')",
        ),
        (
            JsValue::Undefined,
            "Cannot read properties of undefined (reading 'map')",
        ),
        (obj([]), "messages.map is not a function"),
        (
            arr([message(), JsValue::Null]),
            "Cannot read properties of null (reading 'metadata')",
        ),
        (
            arr([JsValue::Undefined]),
            "Cannot read properties of undefined (reading 'metadata')",
        ),
    ] {
        let raw = compact(messages.clone(), result(), snapshot());
        let plan = evaluate(&raw, true);
        assert_eq!(replacement(&plan), messages);
        let Err(CompactionSnapshotError::PersistencePayload(error)) = plan.commit else {
            panic!("wrapped persistence payload phase");
        };
        assert_error(&error, expected, &raw);
    }
}
#[test]
fn truthy_primitive_result_and_snapshot_do_not_invent_missing_fields() {
    let raw = compact(arr([]), JsValue::Number(7.0), JsValue::Bool(true));
    let plan = evaluate(&raw, true);
    assert!(metadata(&plan).get("compactionId").unwrap().is_undefined());
    assert!(metadata(&plan).get("postTokens").unwrap().is_undefined());
    assert!(plan.commit.unwrap().unwrap().messages.is_empty());
}
#[test]
fn null_compact_fails_before_replacement_capture() {
    let boundary = BoundarySnapshot::capture(&JsValue::Null);
    let error = compaction_persistence_policy::evaluate(CompactionPersistenceInput {
        compact: &boundary,
        persistence_advertised: false,
    })
    .unwrap_err();
    assert_error(
        &error,
        "Cannot read properties of null (reading 'messages')",
        &JsValue::Null,
    );
}

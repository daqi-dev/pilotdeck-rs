//! Source-derived controlled context policy checks; no real host or FS gate.
use pilotdeck_agent::{
    MessageProjection, ProjectedMessage, ProjectionGap, SupplementalToolMessage,
    ToolExecutionResult, ToolResultDisposition, ports, tool_context, turn,
};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use ports::{
    AppliedToolResults, CallControl, CancelCause, Cancellation, Captured, ExecutionIdentity,
    HostCall, HostPort, HostReply, PortError, PortResult,
};
use std::collections::VecDeque;
use tool_context::{ContextMessageCollection, ToolContextInput};
#[path = "../src/tool_context_policy.rs"]
mod tool_context_policy;

fn obj(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (k, v) in fields {
        out.insert(k.into(), v);
    }
    JsValue::Object(out)
}
fn arr(values: impl IntoIterator<Item = JsValue>) -> JsValue {
    JsValue::Array(values.into_iter().collect())
}
fn message(text: &str) -> ProjectedMessage {
    project(obj([
        ("role", "user".into()),
        (
            "content",
            arr([obj([("type", "text".into()), ("text", text.into())])]),
        ),
    ]))
}
fn project(value: JsValue) -> ProjectedMessage {
    ProjectedMessage {
        boundary: BoundarySnapshot::capture(&value),
        normalized: MessageProjection::BoundaryOnly(vec![ProjectionGap::OtherBoundaryShape]),
    }
}
fn raw(values: &[ProjectedMessage]) -> JsValue {
    arr(values.iter().map(|value| value.boundary.value().clone()))
}
fn collection_raw(value: &ContextMessageCollection) -> JsValue {
    match value {
        ContextMessageCollection::Items(values) => raw(values),
        ContextMessageCollection::BoundaryOnly(value) => value.value().clone(),
    }
}
fn input() -> turn::TurnInput {
    turn::TurnInput {
        session_id: "s".into(),
        turn_id: "t".into(),
        messages: vec![],
        max_turns: None,
        max_budget_usd: None,
        task_budget_usd: None,
        initial_task_budget_spent_usd: None,
        base_permission_mode: None,
        allow_plan_mode_tools: false,
        can_elicit: false,
        model_override: None,
    }
}
fn control() -> CallControl {
    CallControl {
        cancellation: Cancellation::default(),
        deadline: None,
    }
}
fn result(id: &str, count: usize) -> ToolExecutionResult {
    let supplements: Vec<_> = (0..count)
        .map(|_| SupplementalToolMessage {
            content: vec![],
            boundary: BoundarySnapshot::capture(&obj([("content", arr([]))])),
        })
        .collect();
    let boundary = obj([
        ("type", "success".into()),
        ("toolCallId", id.into()),
        ("toolName", "read".into()),
        ("content", arr([])),
        ("startedAt", "a".into()),
        ("completedAt", "b".into()),
        (
            "supplementalMessages",
            arr(supplements
                .iter()
                .map(|value| value.boundary.value().clone())),
        ),
    ]);
    ToolExecutionResult {
        tool_call_id: id.into(),
        tool_name: "read".into(),
        disposition: ToolResultDisposition::Success { data: None },
        content: vec![],
        supplemental_messages: Some(supplements),
        metadata: None,
        started_at: "a".into(),
        completed_at: "b".into(),
        boundary: BoundarySnapshot::capture(&boundary),
    }
}
struct Host {
    advertised: bool,
    calls: Vec<HostCall>,
    replies: VecDeque<PortResult<HostReply>>,
    cancel_ack: Option<Cancellation>,
}
impl Host {
    fn new(reply: PortResult<HostReply>) -> Self {
        Self {
            advertised: true,
            calls: vec![],
            replies: VecDeque::from([reply]),
            cancel_ack: None,
        }
    }
}
impl HostPort for Host {
    fn advertised(&self, module: &str, operation: &str) -> bool {
        self.advertised && module == "context" && operation == "apply_tool_results"
    }
    fn call(&mut self, _: &CallControl, call: HostCall) -> PortResult<HostReply> {
        assert!(matches!(call, HostCall::ApplyToolResults(_)));
        self.calls.push(call);
        if let Some(c) = &self.cancel_ack {
            c.cancel(CancelCause::Requested("afterACK".into()));
        }
        self.replies.pop_front().expect("one bounded hostcall")
    }
}
fn reply(value: JsValue) -> PortResult<HostReply> {
    Ok(HostReply::ToolResultsAppliedBoundaryOnly(
        BoundarySnapshot::capture(&value),
    ))
}
fn apply(
    host: &mut Host,
    ctl: &CallControl,
    messages: &[ProjectedMessage],
    results: &[ToolExecutionResult],
    projected: &[ProjectedMessage],
) -> PortResult<tool_context::ToolContextOutcome> {
    tool_context_policy::apply(
        host,
        ToolContextInput {
            control: ctl,
            input: &input(),
            messages,
            results,
            projected,
        },
    )
}
fn applied(messages: JsValue, appended: Option<JsValue>) -> JsValue {
    let mut value = obj([("messages", messages)]);
    if let (JsValue::Object(object), Some(appended)) = (&mut value, appended) {
        object.insert("appendedMessages".into(), appended);
    }
    value
}
fn host_input(host: &Host) -> &ports::ApplyToolResults {
    let HostCall::ApplyToolResults(value) = &host.calls[0] else {
        panic!("context call")
    };
    value
}

#[test]
fn absent_capability_appends_projection_without_host_effect() {
    let history = vec![message("history")];
    let projected = vec![message("aggregate"), message("supplement")];
    let mut host = Host::new(reply(JsValue::Null));
    host.advertised = false;
    let outcome = apply(&mut host, &control(), &history, &[], &projected).unwrap();
    assert_eq!(
        collection_raw(&outcome.messages),
        raw(&[
            history[0].clone(),
            projected[0].clone(),
            projected[1].clone()
        ])
    );
    assert_eq!(collection_raw(&outcome.appended_messages), raw(&projected));
    assert!(!outcome.reattach_tail);
    assert!(host.calls.is_empty());
}
#[test]
fn advertised_context_receives_original_fields_and_ordered_duplicate_id_supplements() {
    let history = vec![message("old")];
    let projected = vec![
        message("main"),
        message("a"),
        message("b"),
        message("c"),
        message("d"),
    ];
    let results = vec![result("same", 2), result("same", 1), result("last", 1)];
    let mut host = Host::new(reply(applied(arr([]), None)));
    apply(&mut host, &control(), &history, &results, &projected).unwrap();
    assert_eq!(host.calls.len(), 1);
    let call = host_input(&host);
    assert_eq!(call.session_id, JsString::from("s"));
    assert_eq!(call.turn_id, JsString::from("t"));
    assert_eq!(
        call.tool_result_message.boundary.value(),
        projected[0].boundary.value()
    );
    assert_eq!(raw(&call.messages), raw(&history));
    let supplements = call.supplemental_messages.as_ref().unwrap();
    assert_eq!(
        supplements
            .iter()
            .map(|v| v.tool_call_id.clone())
            .collect::<Vec<_>>(),
        vec!["same".into(), "same".into(), "same".into(), "last".into()]
    );
    assert_eq!(
        raw(&supplements
            .iter()
            .map(|v| v.message.clone())
            .collect::<Vec<_>>()),
        raw(&projected[1..])
    );
}
#[test]
fn supplement_binding_truncates_counts_and_keeps_unbound_projection() {
    let projected = vec![message("main"), message("a"), message("b")];
    let mut host = Host::new(reply(applied(arr([]), None)));
    apply(
        &mut host,
        &control(),
        &[],
        &[result("first", 10), result("last", 2)],
        &projected,
    )
    .unwrap();
    let supplements = host_input(&host).supplemental_messages.as_ref().unwrap();
    assert_eq!(supplements.len(), 2);
    assert!(
        supplements
            .iter()
            .all(|v| v.tool_call_id.equals_str("first"))
    );
    let mut host = Host::new(reply(applied(arr([]), None)));
    let outcome = apply(
        &mut host,
        &control(),
        &[],
        &[result("first", 0)],
        &projected,
    )
    .unwrap();
    assert!(
        host_input(&host)
            .supplemental_messages
            .as_ref()
            .unwrap()
            .is_empty()
    );
    assert_eq!(collection_raw(&outcome.appended_messages), raw(&projected));
}
#[test]
fn known_throw_before_assignment_uses_legacy_append_and_never_retries() {
    let history = vec![message("old")];
    let projected = vec![message("main")];
    for error in [
        PortError::HostFailure {
            code: "failed".into(),
            message: "known".into(),
            boundary: BoundarySnapshot::capture(&JsValue::Null),
        },
        PortError::TransportBeforeDispatch {
            message: "noeffect".into(),
        },
        PortError::SourceException {
            message: "Host context module returned no result.".into(),
            boundary: BoundarySnapshot::capture(&obj([])),
        },
    ] {
        let mut host = Host::new(Err(error));
        let outcome = apply(&mut host, &control(), &history, &[], &projected).unwrap();
        assert_eq!(host.calls.len(), 1);
        assert_eq!(
            collection_raw(&outcome.messages),
            raw(&[history[0].clone(), projected[0].clone()])
        );
        assert!(!outcome.reattach_tail);
    }
}
#[test]
fn nullish_applied_reply_throws_before_assignment_and_falls_back() {
    for value in [JsValue::Null, JsValue::Undefined] {
        let history = vec![message("old")];
        let projected = vec![message("main")];
        let mut host = Host::new(reply(value));
        let outcome = apply(&mut host, &control(), &history, &[], &projected).unwrap();
        assert_eq!(
            collection_raw(&outcome.messages),
            raw(&[history[0].clone(), projected[0].clone()])
        );
        assert_eq!(collection_raw(&outcome.appended_messages), raw(&projected));
    }
}
#[test]
fn absent_null_undefined_appended_keeps_returned_messages_without_tail_replacement() {
    let returned = vec![message("remote-different")];
    let projected = vec![message("canonical")];
    for supplied in [None, Some(JsValue::Null), Some(JsValue::Undefined)] {
        let mut host = Host::new(reply(applied(raw(&returned), supplied)));
        let outcome = apply(&mut host, &control(), &[], &[], &projected).unwrap();
        assert_eq!(collection_raw(&outcome.messages), raw(&returned));
        assert_eq!(collection_raw(&outcome.appended_messages), raw(&projected));
        assert!(!outcome.reattach_tail);
    }
}
#[test]
fn supplied_arrays_only_reattach_when_length_fits_including_empty_arrays() {
    let returned = vec![message("prefix"), message("remote-tail")];
    for count in [0, 1, 2, 3] {
        let appended = vec![message("canonical"); count];
        let mut host = Host::new(reply(applied(raw(&returned), Some(raw(&appended)))));
        let outcome = apply(&mut host, &control(), &[], &[], &[message("projected")]).unwrap();
        assert_eq!(outcome.reattach_tail, count <= returned.len());
        assert_eq!(collection_raw(&outcome.messages), raw(&returned));
        assert_eq!(collection_raw(&outcome.appended_messages), raw(&appended));
    }
}
#[test]
fn diagnostics_are_ignored_and_raw_message_extensions_remain_complete() {
    let value = obj([
        ("role", "user".into()),
        ("content", arr([])),
        (
            "unknown",
            obj([
                ("u", JsValue::Undefined),
                ("null", JsValue::Null),
                ("wtf", JsValue::String(JsString::from_utf16(vec![0xd800]))),
            ]),
        ),
    ]);
    let mut applied = applied(arr([value.clone()]), None);
    if let JsValue::Object(object) = &mut applied {
        object.insert("diagnostics".into(), JsValue::Number(99.0));
    }
    let mut host = Host::new(reply(applied));
    let outcome = apply(&mut host, &control(), &[], &[], &[message("main")]).unwrap();
    assert_eq!(collection_raw(&outcome.messages), arr([value]));
}
#[test]
fn typed_reply_retains_matching_full_boundary_extensions() {
    let value = obj([
        ("role", "user".into()),
        ("content", arr([])),
        ("unknown", JsValue::Undefined),
    ]);
    let raw = obj([
        ("messages", arr([value.clone()])),
        ("diagnostics", arr([])),
        ("extension", JsValue::Null),
    ]);
    let reply = HostReply::ToolResultsApplied(Captured {
        view: AppliedToolResults {
            messages: vec![project(value)],
            appended_messages: None,
            diagnostics: vec![],
        },
        boundary: BoundarySnapshot::capture(&raw),
    });
    let mut host = Host::new(Ok(reply));
    let outcome = apply(&mut host, &control(), &[], &[], &[message("main")]).unwrap();
    assert_eq!(
        collection_raw(&outcome.messages),
        raw.get("messages").unwrap().clone()
    );
}
#[test]
fn open_messages_and_falsy_nonnull_appended_are_deferred_without_invented_arrays() {
    for messages in [
        JsValue::Null,
        JsValue::Undefined,
        JsValue::Bool(false),
        JsValue::Number(2.0),
        "text".into(),
        obj([("extension", JsValue::Undefined)]),
    ] {
        for appended in [
            None,
            Some(JsValue::Bool(false)),
            Some(JsValue::Number(0.0)),
            Some("".into()),
        ] {
            let mut host = Host::new(reply(applied(messages.clone(), appended.clone())));
            let outcome = apply(&mut host, &control(), &[], &[], &[message("main")]).unwrap();
            assert_eq!(collection_raw(&outcome.messages), messages);
            assert!(!outcome.reattach_tail);
            if let Some(appended) = appended {
                assert_eq!(collection_raw(&outcome.appended_messages), appended);
            }
        }
    }
}
#[test]
fn primitive_applied_result_has_undefined_messages_without_early_source_invalid_error() {
    for value in [
        JsValue::Bool(true),
        JsValue::Number(1.0),
        "value".into(),
        arr([]),
    ] {
        let mut host = Host::new(reply(value));
        let outcome = apply(&mut host, &control(), &[], &[], &[message("main")]).unwrap();
        assert_eq!(collection_raw(&outcome.messages), JsValue::Undefined);
        assert!(!outcome.reattach_tail);
    }
}
#[test]
fn partial_assignment_length_error_catch_uses_assigned_push_target() {
    for (messages, text) in [
        (
            JsValue::Null,
            "Cannot read properties of null (reading 'push')",
        ),
        (
            JsValue::Undefined,
            "Cannot read properties of undefined (reading 'push')",
        ),
    ] {
        let mut host = Host::new(reply(applied(messages, Some(arr([])))));
        let error = apply(
            &mut host,
            &control(),
            &[message("old")],
            &[],
            &[message("main")],
        )
        .err()
        .unwrap();
        assert!(matches!(error,PortError::SourceException{message,..}if message.equals_str(text)));
        assert_eq!(host.calls.len(), 1);
    }
}
#[test]
fn slice_or_spread_error_pushes_projection_to_returned_array_not_original_history() {
    let returned = vec![message("remote")];
    let projected = vec![message("main")];
    let appended = obj([("length", JsValue::Number(0.0))]);
    let mut host = Host::new(reply(applied(raw(&returned), Some(appended.clone()))));
    let outcome = apply(&mut host, &control(), &[message("old")], &[], &projected).unwrap();
    assert_eq!(
        collection_raw(&outcome.messages),
        raw(&[returned[0].clone(), projected[0].clone()])
    );
    assert_eq!(collection_raw(&outcome.appended_messages), appended);
    assert!(!outcome.reattach_tail);
    let mut host = Host::new(reply(applied(
        obj([("length", JsValue::Number(2.0))]),
        Some(arr([])),
    )));
    assert!(
        matches!(apply(&mut host,&control(),&[],&[],&projected),Err(PortError::SourceException{message,..})if message.equals_str("messages.push is not a function"))
    );
}
#[test]
fn source_length_numeric_and_string_comparisons_do_not_tighten_open_objects() {
    // Comparison false leaves a plain open message object intact; true enters
    // missing slice/push methods and escapes the catch's source TypeError.
    for (left, right, fits) in [
        ("2", "10", false),
        ("02", "10", true),
        ("bogus", "10", false),
        ("0x10", "20", true),
    ] {
        let mut host = Host::new(reply(applied(
            obj([("length", right.into())]),
            Some(obj([("length", left.into())])),
        )));
        let outcome = apply(&mut host, &control(), &[], &[], &[message("main")]);
        if fits {
            assert!(
                matches!(outcome,Err(PortError::SourceException{message,..})if message.equals_str("messages.push is not a function"))
            );
        } else {
            let outcome = outcome.unwrap();
            assert!(!outcome.reattach_tail);
            assert_eq!(
                collection_raw(&outcome.messages),
                obj([("length", right.into())])
            );
        }
    }
}
#[test]
fn accepted_unknown_and_cancellation_preserve_target_owner_responsibility() {
    let identity = ExecutionIdentity {
        session_id: "s".into(),
        turn_id: "t".into(),
        run_id: "r".into(),
        operation_id: "o".into(),
        request_id: "req".into(),
        idempotency_key: Some("key".into()),
        module_instance_id: "instance".into(),
        connection_generation: "g".into(),
    };
    let mut host = Host::new(Err(PortError::AcceptedUnknown {
        identity: Box::new(identity.clone()),
        last_applied_sequence: Some(7.0),
    }));
    let error = apply(&mut host, &control(), &[], &[], &[message("main")])
        .err()
        .unwrap();
    assert!(
        matches!(error,PortError::AcceptedUnknown{identity:actual,last_applied_sequence:Some(7.0)}if *actual==identity)
    );
    assert_eq!(host.calls.len(), 1);
    let mut host = Host::new(Err(PortError::Cancelled {
        cause: CancelCause::OwnerRetired,
    }));
    assert!(matches!(
        apply(&mut host, &control(), &[], &[], &[message("main")]),
        Err(PortError::Cancelled {
            cause: CancelCause::OwnerRetired
        })
    ));
    assert_eq!(host.calls.len(), 1);
}
#[test]
fn cancelled_before_call_has_zero_host_effect_but_known_ack_is_not_discarded() {
    let ctl = control();
    ctl.cancellation.cancel(CancelCause::Deadline);
    let mut host = Host::new(reply(applied(arr([]), None)));
    assert!(matches!(
        apply(&mut host, &ctl, &[], &[], &[message("main")]),
        Err(PortError::Cancelled {
            cause: CancelCause::Deadline
        })
    ));
    assert!(host.calls.is_empty());
    let ctl = control();
    let returned = vec![message("accepted")];
    let mut host = Host::new(reply(applied(raw(&returned), None)));
    host.cancel_ack = Some(ctl.cancellation.clone());
    let outcome = apply(&mut host, &ctl, &[], &[], &[message("main")]).unwrap();
    assert_eq!(collection_raw(&outcome.messages), raw(&returned));
    assert!(ctl.check().is_err());
    assert_eq!(host.calls.len(), 1);
}
#[test]
fn string_returned_history_slice_spread_uses_utf16_and_code_point_iteration() {
    let appended = vec![message("canonical")];
    let returned = JsValue::String(JsString::from_utf16(vec![0xd800, 0xdc00, b'x' as u16]));
    let mut host = Host::new(reply(applied(returned, Some(raw(&appended)))));
    let outcome = apply(&mut host, &control(), &[], &[], &[message("main")]).unwrap();
    assert!(outcome.reattach_tail);
    assert_eq!(
        collection_raw(&outcome.messages),
        arr([
            JsValue::String(JsString::from_utf16(vec![0xd800, 0xdc00])),
            appended[0].boundary.value().clone()
        ])
    );
}
#[test]
fn missing_projection_aggregate_and_wrong_reply_tag_are_candidate_invariants() {
    let mut host = Host::new(reply(arr([])));
    assert!(matches!(
        apply(&mut host, &control(), &[], &[], &[]),
        Err(PortError::InvalidResponse { .. })
    ));
    assert!(host.calls.is_empty());
    let mut host = Host::new(Ok(HostReply::EventEmitted));
    assert!(matches!(
        apply(&mut host, &control(), &[], &[], &[message("main")]),
        Err(PortError::InvalidResponse { .. })
    ));
    assert_eq!(host.calls.len(), 1);
}

#[test]
fn legacy_fallback_links_canonical_projection_instances_only_before_override() {
    let projected = vec![message("main"), message("supplement")];
    let history = vec![message("old")];
    let mut absent = Host::new(reply(applied(arr([]), None)));
    absent.advertised = false;
    let outcome = apply(&mut absent, &control(), &history, &[], &projected).unwrap();
    assert!(outcome.fallback_projection_tail);
    assert!(!outcome.reattach_tail);
    assert_eq!(collection_raw(&outcome.appended_messages), raw(&projected));
    let mut failed = Host::new(Err(PortError::InvalidResponse {
        message: "known throw before result assignment".into(),
    }));
    let outcome = apply(&mut failed, &control(), &history, &[], &projected).unwrap();
    assert!(outcome.fallback_projection_tail);
    let mut null = Host::new(reply(JsValue::Null));
    let outcome = apply(&mut null, &control(), &history, &[], &projected).unwrap();
    assert!(outcome.fallback_projection_tail);
}
#[test]
fn partial_assignment_fallback_does_not_link_different_supplied_appended_instances() {
    let projected = vec![message("projected")];
    let returned = vec![message("returned")];
    let supplied = obj([
        ("length", JsValue::Number(0.0)),
        ("different", JsValue::Null),
    ]);
    let mut host = Host::new(reply(applied(raw(&returned), Some(supplied.clone()))));
    let outcome = apply(&mut host, &control(), &[message("old")], &[], &projected).unwrap();
    assert!(!outcome.fallback_projection_tail);
    assert!(!outcome.reattach_tail);
    assert_eq!(
        collection_raw(&outcome.messages),
        raw(&[returned[0].clone(), projected[0].clone()])
    );
    assert_eq!(collection_raw(&outcome.appended_messages), supplied);
    // A resolved absence retains the initial projection for emission but does
    // not push it into returned history, so no annotation linkage is invented.
    let mut resolved = Host::new(reply(applied(raw(&returned), None)));
    let outcome = apply(&mut resolved, &control(), &[], &[], &projected).unwrap();
    assert!(!outcome.fallback_projection_tail);
    let mut explicit = Host::new(reply(applied(raw(&returned), Some(raw(&projected)))));
    let outcome = apply(&mut explicit, &control(), &[], &[], &projected).unwrap();
    assert!(!outcome.fallback_projection_tail);
    assert!(outcome.reattach_tail);
}

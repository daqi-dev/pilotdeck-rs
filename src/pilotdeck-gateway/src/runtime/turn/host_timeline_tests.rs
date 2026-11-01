//! #1220: host-event timeline / streamBoundary golden replay.
//!
//! Each fixture case (tests/fixtures/host-event-timeline/oracle.json) is a
//! step script — wire payloads, host-buffer batches, 500 ms pump ticks, a
//! mid-script cancellation and the final — recorded against the fixed TS
//! baseline (cc7c4e6) with mock timers. This harness replays the same script
//! through the gateway's StreamHandler pump and asserts every projected
//! event, and the terminal coordinates, cell by cell against the golden. See
//! the fixture README for the four registered deviations and accommodations
//! that the comparison makes.

use super::*;

fn golden() -> JsValue {
    crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/host-event-timeline/oracle.json"
    ))
}

/// The golden's derived statuses carry the fixture's session/turn identity,
/// not the generic test options.
fn replay_identity() -> ExecutionIdentity {
    ExecutionIdentity {
        session_id: "s".into(),
        turn_id: "t".into(),
        run_id: "run".into(),
        operation_id: "operation".into(),
        request_id: "request".into(),
    }
}

async fn advance_pump(task: &mut HostFuture<ExecuteResult>, ms: u64) {
    tokio::time::advance(Duration::from_millis(ms)).await;
    assert_pending(task).await;
}

fn turn_input() -> TurnInput {
    parse_execute_request(&input(), &options(), vec![])
        .expect("#1220 timeline: static turn fixture must parse and retain its shape")
        .1
}

/// The compaction case starts its turn with its own loop messages: the
/// recorded history plus one accepted user message — the same split the TS
/// generator ran with, so `aborted_result`'s input fallback matches the
/// golden's retained `input.messages`.
fn case_input(case: &JsValue) -> TurnInput {
    let Some(JsValue::Object(fields)) = case.get("input") else {
        return turn_input();
    };
    let history = match fields.get("history") {
        Some(JsValue::Array(messages)) => messages.to_vec(),
        _ => Vec::new(),
    };
    let mut agent_input = JsObject::new();
    agent_input.insert("type".into(), "text".into());
    agent_input.insert(
        "text".into(),
        JsValue::String(
            fields
                .get("text")
                .and_then(JsValue::as_string)
                .expect("#1220 timeline: case input text")
                .clone(),
        ),
    );
    parse_execute_request(&JsValue::Object(agent_input), &options(), history)
        .expect("#1220 timeline: case turn fixture must parse and retain its shape")
        .1
}

/// Recursively drop the named fields so a comparison can look past them:
/// the coordinate fields for the #821 crossed comparison, and the wall-clock
/// timestamps of the synthesized aborted result.
fn strip_fields(value: &JsValue, drop: &[&str]) -> JsValue {
    match value {
        JsValue::Object(fields) => {
            let mut rebuilt = JsObject::new();
            for (name, value) in fields.entries() {
                if drop.iter().any(|key| name.equals_str(key)) {
                    continue;
                }
                rebuilt.insert(name.clone(), strip_fields(value, drop));
            }
            JsValue::Object(rebuilt)
        }
        JsValue::Array(values) => JsValue::Array(
            values
                .iter()
                .map(|value| strip_fields(value, drop))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn message_block_timeline(event: &JsValue) -> Option<&JsValue> {
    match event
        .get("message")
        .and_then(|message| message.get("content"))?
    {
        JsValue::Array(blocks) => blocks.first().and_then(|block| block.get("timeline")),
        _ => None,
    }
}

/// The recording submit() would own for a compaction case, plus the execute
/// snapshot the stream saves when it hands the runner's messages to the
/// sidecar: a real runner that accepted the turn input, then recorded every
/// durable wire message the script sent — the same settle pending_durable
/// drives in production. The snapshot must exclude those messages (TS
/// abortedResult reads input.messages), while the runner retains them.
async fn replay_recording(
    input: TurnInput,
    durable: &[JsValue],
) -> (Option<Recording>, Option<Vec<JsValue>>) {
    let runner = TurnRunner::new(test_recorder(), test_lease(), "s".into(), "t".into());
    let (runner, _) = runner
        .start()
        .await
        .expect("#1220 timeline: replay runner must start");
    let (runner, _) = runner
        .accept(input)
        .await
        .expect("#1220 timeline: replay runner must accept the case input");
    let snapshot = runner.messages().to_vec();
    let mut runner = runner;
    for message in durable {
        let (next, _) = runner
            .durable_message(message.clone())
            .await
            .expect("#1220 timeline: replay runner must record the durable wire message");
        runner = next;
    }
    (Some(Recording::new(runner, Arc::new(()))), Some(snapshot))
}

async fn replay_case(case: &JsValue, name: &str) {
    let timeline = Arc::new(Mutex::new(
        crate::runtime::turn::timeline::TurnTimeline::new("t".into()),
    ));
    let inner = Arc::new(Handler::default());
    let (events, mut rx) = mpsc::channel(64);
    let integrity = Arc::new(StreamIntegrity::new());
    *integrity
        .module_id
        .lock()
        .expect("#1220 timeline: module lock") = Some("fixture".into());
    let handler = Arc::new(StreamHandler {
        inner: inner.clone(),
        events,
        integrity: integrity.clone(),
        forwarding: Arc::new(Mutex::new(Forwarding {
            timeline: Some(timeline.clone()),
            ..Default::default()
        })),
        callback_cancel: None,
    });
    let pump_handler = handler.clone();
    let mut task: HostFuture<ExecuteResult> = Box::pin(async move {
        pump_handler
            .with_pump(std::future::pending(), replay_identity())
            .await
    });
    assert_pending(&mut task).await;
    let mut final_payload = None;
    let mut compaction_final = false;
    // The durable wire messages the script sends before its final: the settle
    // the stream's pending_durable drives on the consumer's next poll in
    // production, which this pump-only replay cannot drive itself.
    let mut durable_wire = Vec::new();
    let JsValue::Array(steps) = case.get("steps").expect("#1220 timeline: case steps") else {
        panic!("#1220 timeline: {name} must list steps")
    };
    for step in steps {
        let kind = step
            .get("kind")
            .and_then(JsValue::as_string)
            .expect("#1220 timeline: step kind");
        if kind.equals_str("wire") {
            let payload = step.get("payload").expect("#1220 timeline: wire payload");
            if let Some(kind) = payload.get("type")
                && (kind == &"assistant_message".into() || kind == &"tool_results_projected".into())
            {
                durable_wire.push(
                    payload
                        .get("message")
                        .cloned()
                        .expect("#1220 timeline: durable wire message"),
                );
            }
            deliver(&handler, payload, false)
                .await
                .expect("#1220 timeline: wire step must apply");
        } else if kind.equals_str("host") {
            let JsValue::Array(batch) = step.get("events").expect("#1220 timeline: host batch")
            else {
                panic!("#1220 timeline: {name} host step must list events")
            };
            queue_host_events(&inner, batch);
        } else if kind.equals_str("cancel") {
            // The golden's cancellation is a fact of the TS runner, not of
            // this pump: the pump keeps draining after it (the
            // cancel-window case queues its host event after one), and the
            // cancelled final still positions its messages.
        } else if kind.equals_str("tick") {
            let ms = step
                .get("ms")
                .and_then(JsValue::as_number)
                .expect("#1220 timeline: tick length") as u64;
            advance_pump(&mut task, ms).await;
        } else if kind.equals_str("final") {
            let payload = step
                .get("payload")
                .expect("#1220 timeline: final payload")
                .clone();
            if step.get("compaction").and_then(JsValue::as_bool) == Some(true) {
                // The incomplete compaction final is validated but never
                // applied to a handler (execution.rs skips its callback); the
                // execution resolves AbortedCompaction and finish()
                // synthesizes the terminal from the turn's retained input.
                compaction_final = true;
            } else {
                handler.observe_loop_completion(&payload);
                deliver(&handler, &payload, true)
                    .await
                    .expect("#1220 timeline: final step must apply");
            }
            final_payload = Some(payload);
        } else {
            panic!("#1220 timeline: {name} has unknown step kind");
        }
    }
    let mut collected = Vec::new();
    while let Ok((event, _, _)) = rx.try_recv() {
        collected.push(event);
    }
    let final_payload = final_payload.expect("#1220 timeline: case must end in a final step");
    let input = case_input(case);
    // The compaction terminal is synthesized from the turn's own message
    // source, so only that case needs the recording submit() would have owned:
    // a runner that accepted the case input — the execute-time snapshot the
    // sidecar receives — and then recorded every durable wire message. Every
    // other case completes through the peer's own terminal payload, which
    // never consults the runner.
    let (recording, execute_messages) = if compaction_final {
        replay_recording(input.clone(), &durable_wire).await
    } else {
        (None, None)
    };
    let turn = LiveTurnStream {
        execution: None,
        result: None,
        events: mpsc::channel(1).1,
        identity: replay_identity(),
        input,
        integrity,
        delivered: StreamIntegrity::new(),
        active: None,
        recording_ready: oneshot::channel().1,
        recording,
        pending_event: None,
        pending_durable: None,
        callback_cancel: None,
        accepted: false,
        preflight: 0,
        execute_ready: None,
        execute_messages,
        recorded: std::collections::VecDeque::new(),
        timeline: Some(timeline),
    };
    let completed = if compaction_final {
        turn.finish(Ok(ExecuteResult::AbortedCompaction))
            .expect("#1220 timeline: compaction terminal must project")
            .expect("#1220 timeline: compaction terminal must complete")
    } else {
        turn.completed_event(&final_payload, true)
            .expect("#1220 timeline: terminal must project")
    };
    let JsValue::Array(expected) = case.get("events").expect("#1220 timeline: case events") else {
        panic!("#1220 timeline: {name} must list events")
    };
    // The TS runner yields its own turn_completed last; the gateway synthesizes
    // its terminal from the final, so the body aligns on every earlier event.
    crate::negative_assert_eq!(
        collected.len(),
        expected.len() - 1,
        "#1220 timeline: {name} stream event count"
    );
    if case
        .get("deviation")
        .and_then(JsValue::as_string)
        .is_some_and(|deviation| deviation.equals_str("host-pending-wire-same-tick"))
    {
        // The registered #821 deviation made observable: with a locatable host
        // event pending in the buffer, the TS wire branch drains and yields it
        // before the wire event, while this port keeps it buffered until the
        // pump tick. The same two events appear in opposite order, with their
        // coordinates allocated in the respective yield order.
        crate::negative_assert_eq!(
            strip_fields(&collected[0], &["timeline", "streamBoundary"]),
            strip_fields(&expected[1], &["timeline", "streamBoundary"]),
            "#1220 timeline: {name} wire event body (crossed)"
        );
        crate::negative_assert_eq!(
            strip_fields(&collected[1], &["timeline", "streamBoundary"]),
            strip_fields(&expected[0], &["timeline", "streamBoundary"]),
            "#1220 timeline: {name} host event body (crossed)"
        );
        let rust_wire = parse_json(
            r#"{"version":1,"turnId":"t","id":"b10","order":0,"revision":2,"offset":0}"#,
        )
        .expect("#1220 timeline: literal must parse");
        let rust_host = parse_json(
            r#"{"version":1,"turnId":"t","id":"hb1","previousId":"b10","order":1,"revision":4}"#,
        )
        .expect("#1220 timeline: literal must parse");
        let ts_host = parse_json(r#"{"version":1,"turnId":"t","id":"hb1","order":0,"revision":2}"#)
            .expect("#1220 timeline: literal must parse");
        let ts_wire = parse_json(
            r#"{"version":1,"turnId":"t","id":"b10","previousId":"hb1","order":1,"revision":4,"offset":0}"#,
        )
        .expect("#1220 timeline: literal must parse");
        crate::negative_assert_eq!(
            collected[0].get("timeline"),
            Some(&rust_wire),
            "#1220 timeline: {name} wire block keeps the first Rust position"
        );
        crate::negative_assert_eq!(
            message_block_timeline(&collected[1]),
            Some(&rust_host),
            "#1220 timeline: {name} host block follows the wire block in Rust"
        );
        crate::negative_assert_eq!(
            message_block_timeline(&expected[0]),
            Some(&ts_host),
            "#1220 timeline: {name} host block keeps the first TS position"
        );
        crate::negative_assert_eq!(
            expected[1].get("timeline"),
            Some(&ts_wire),
            "#1220 timeline: {name} wire block follows the host block in TS"
        );
    } else {
        for (index, event) in collected.iter().enumerate() {
            crate::negative_assert_eq!(
                event,
                &expected[index],
                "#1220 timeline: {name} event {index}"
            );
        }
    }
    let last = expected.last().expect("#1220 timeline: terminal event");
    if compaction_final {
        // The synthesized aborted result stamps the host's wall clock; the
        // TS golden's timestamps come from its mocked Date. Compare every
        // other field exactly and pin the two timestamps to their shape.
        let undefined = JsValue::Undefined;
        crate::negative_assert_eq!(
            strip_fields(
                completed.get("result").unwrap_or(&undefined),
                &["startedAt", "completedAt"]
            ),
            strip_fields(
                last.get("result").unwrap_or(&undefined),
                &["startedAt", "completedAt"]
            ),
            "#1220 timeline: {name} terminal result"
        );
        let started = completed
            .get("result")
            .and_then(|result| result.get("startedAt"))
            .and_then(JsValue::as_string)
            .expect("#1220 timeline: {name} synthesized startedAt");
        crate::negative_assert!(
            started.to_utf8().is_ok_and(|text| text.ends_with('Z')),
            "#1220 timeline: {name} synthesized timestamps are UTC"
        );
        crate::negative_assert_eq!(
            completed
                .get("result")
                .and_then(|result| result.get("startedAt")),
            completed
                .get("result")
                .and_then(|result| result.get("completedAt")),
            "#1220 timeline: {name} synthesized timestamps share one now"
        );
    } else {
        crate::negative_assert_eq!(
            completed.get("result"),
            last.get("result"),
            "#1220 timeline: {name} terminal result"
        );
    }
    // The TS-protocol turn_completed carries no messages; run()'s returned
    // messages array is the gateway terminal's own field.
    crate::negative_assert_eq!(
        completed.get("messages"),
        case.get("returned")
            .and_then(|returned| returned.get("messages")),
        "#1220 timeline: {name} terminal messages"
    );
}

#[tokio::test(start_paused = true)]
async fn host_timeline_replay_matches_ts_golden() {
    let oracle = golden();
    let JsValue::Array(cases) = oracle.get("cases").expect("#1220 timeline: cases") else {
        panic!("#1220 timeline: oracle must list cases")
    };
    crate::negative_assert_eq!(cases.len(), 18, "#1220 timeline: case inventory");
    for case in cases {
        let name = case
            .get("name")
            .and_then(JsValue::as_string)
            .and_then(|name| name.to_utf8().ok())
            .expect("#1220 timeline: case name");
        replay_case(case, &name).await;
    }
}

#[test]
#[ignore = "requires Node v22.22.0 and PDRS_BASELINE"]
fn host_timeline_regenerates_identically() {
    crate::test_baseline::regenerates_identically("host-event-timeline");
}

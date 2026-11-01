//! #1220: host-event timeline / streamBoundary golden replay.
//!
//! Each fixture case (tests/fixtures/host-event-timeline/oracle.json) is a
//! step script — wire payloads, host-buffer batches, 500 ms pump ticks and
//! the final — recorded against the fixed TS baseline (cc7c4e6) with mock
//! timers. This harness replays the same script through the gateway's
//! StreamHandler pump and asserts every projected event, and the terminal
//! coordinates, cell by cell against the golden. See the fixture README
//! for the two registered deviations that the comparison accommodates.

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
            deliver(&handler, payload, false)
                .await
                .expect("#1220 timeline: wire step must apply");
        } else if kind.equals_str("host") {
            let JsValue::Array(batch) = step.get("events").expect("#1220 timeline: host batch")
            else {
                panic!("#1220 timeline: {name} host step must list events")
            };
            queue_host_events(&inner, batch);
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
            handler.observe_loop_completion(&payload);
            deliver(&handler, &payload, true)
                .await
                .expect("#1220 timeline: final step must apply");
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
    let turn = LiveTurnStream {
        execution: None,
        result: None,
        events: mpsc::channel(1).1,
        identity: replay_identity(),
        input: turn_input(),
        integrity,
        delivered: StreamIntegrity::new(),
        active: None,
        recording_ready: oneshot::channel().1,
        recording: None,
        pending_event: None,
        pending_durable: None,
        callback_cancel: None,
        accepted: false,
        preflight: 0,
        execute_ready: None,
        recorded: std::collections::VecDeque::new(),
        timeline: Some(timeline),
    };
    let completed = turn
        .completed_event(&final_payload, true)
        .expect("#1220 timeline: terminal must project");
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
    for (index, event) in collected.iter().enumerate() {
        crate::negative_assert_eq!(
            event,
            &expected[index],
            "#1220 timeline: {name} event {index}"
        );
    }
    let last = expected.last().expect("#1220 timeline: terminal event");
    crate::negative_assert_eq!(
        completed.get("result"),
        last.get("result"),
        "#1220 timeline: {name} terminal result"
    );
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
    crate::negative_assert_eq!(cases.len(), 15, "#1220 timeline: case inventory");
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

use super::*;

fn error(kind: HostErrorKind) -> HostError {
    HostError::new(kind, "actual sidecar terminal")
}

#[test]
fn typed_unknown_after_exact_execution_timeout_rethrows_without_turn_failure() {
    let run: JsString = "gateway-run".into();
    let cancel = Cancellation::default();
    cancel.cancel(Some("timeout:gateway-run".into()));
    assert!(timed_out_unknown(
        &error(HostErrorKind::ResultUnknown),
        Some(&run),
        Some(&cancel)
    ));
}

#[test]
fn unknown_first_and_ordinary_abort_remain_failures() {
    let run: JsString = "gateway-run".into();
    let not_aborted = Cancellation::default();
    assert!(!timed_out_unknown(
        &error(HostErrorKind::ResultUnknown),
        Some(&run),
        Some(&not_aborted)
    ));
    let ordinary = Cancellation::default();
    ordinary.cancel(Some("user:cancel".into()));
    assert!(!timed_out_unknown(
        &error(HostErrorKind::ResultUnknown),
        Some(&run),
        Some(&ordinary)
    ));
}

#[test]
fn execution_run_identity_is_not_agent_turn_or_previous_run() {
    let run: JsString = "gateway-run".into();
    for reason in ["timeout:agent-turn", "timeout:previous-run"] {
        let cancel = Cancellation::default();
        cancel.cancel(Some(reason.into()));
        assert!(!timed_out_unknown(
            &error(HostErrorKind::ResultUnknown),
            Some(&run),
            Some(&cancel)
        ));
    }
    let cancel = Cancellation::default();
    cancel.cancel(Some("timeout:gateway-run".into()));
    assert!(
        !timed_out_unknown(&error(HostErrorKind::ResultUnknown), None, Some(&cancel)),
        "missing/null/non-string execution.runId must not use transport fallback"
    );
}

#[test]
fn public_matching_reason_has_ts_behavior_and_first_abort_wins() {
    let run: JsString = "gateway-run".into();
    let public_reason = Cancellation::default();
    public_reason.cancel(Some("timeout:gateway-run".into()));
    assert!(timed_out_unknown(
        &error(HostErrorKind::ResultUnknown),
        Some(&run),
        Some(&public_reason)
    ));
    let first = Cancellation::default();
    first.cancel(Some("ordinary".into()));
    first.cancel(Some("timeout:gateway-run".into()));
    assert!(!timed_out_unknown(
        &error(HostErrorKind::ResultUnknown),
        Some(&run),
        Some(&first)
    ));
}

#[test]
fn timeout_never_swallows_protocol_or_io_failure() {
    let run: JsString = "gateway-run".into();
    let cancel = Cancellation::default();
    cancel.cancel(Some("timeout:gateway-run".into()));
    assert!(!timed_out_unknown(
        &error(HostErrorKind::Protocol),
        Some(&run),
        Some(&cancel)
    ));
}

#[tokio::test]
async fn admitted_stream_rethrows_timed_out_unknown_without_starting_failure_recording() {
    let options = parse_json(
        r#"{"sessionId":"session","turnId":"agent-turn","execution":{"runId":"gateway-run"}}"#,
    )
    .unwrap();
    let input = parse_json(r#"{"type":"text","text":"owned probe"}"#).unwrap();
    let (request, input) = parse_execute_request(&input, &options, vec![]).unwrap();
    assert_ne!(request.identity.run_id, request.identity.turn_id);
    let cancel = Cancellation::default();
    cancel.cancel(Some("timeout:gateway-run".into()));
    let mut stream = LiveTurnStream {
        execution: None,
        result: Some(Err(error(HostErrorKind::ResultUnknown))),
        events: mpsc::channel(1).1,
        identity: request.identity,
        input,
        prompt_input: None,
        should_call_model: true,
        integrity: Arc::new(StreamIntegrity::new()),
        delivered: StreamIntegrity::new(),
        active: None,
        recording_ready: oneshot::channel().1,
        recording: None,
        pending_event: None,
        pending_durable: None,
        callback_cancel: Some(cancel),
        timeout_run_id: Some("gateway-run".into()),
        host_loop: false,
        accepted: true,
        preflight: 3,
        execute_ready: None,
        execute_messages: None,
        recorded: Default::default(),
        timeline: None,
    };
    let err = stream
        .next()
        .await
        .expect_err("unknown operation must rethrow");
    assert!(
        err.message.contains("actual sidecar terminal"),
        "must preserve the original host error, not attempt the absent failure recorder"
    );
    assert!(stream.recording.is_none());
    assert!(stream.recorded.is_empty());
    assert!(stream.result.is_none());
}

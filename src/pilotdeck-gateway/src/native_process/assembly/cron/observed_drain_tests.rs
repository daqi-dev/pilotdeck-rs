use super::*;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicUsize, Ordering},
};
struct Scripted {
    steps: VecDeque<Result<Option<JsValue>, SubmitError>>,
    reads: Arc<AtomicUsize>,
}
impl CronTurnStream for Scripted {
    fn next_event(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Option<JsValue>, SubmitError>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let step = self.steps.pop_front().unwrap_or(Ok(None));
        async move { step }
    }
}
#[tokio::test]
async fn observed_drain_keeps_error_events_and_waits_for_stream_end() {
    let reads = Arc::new(AtomicUsize::new(0));
    let events = [
        r#"{"type":"error","code":"turn_timeout","message":"limit"}"#,
        r#"{"type":"assistant_text_delta","text":"after error"}"#,
    ];
    let mut stream = Scripted {
        steps: events
            .iter()
            .map(|raw| Ok(Some(pilotdeck_protocol::parse_json(raw).unwrap())))
            .chain([Ok(None)])
            .collect(),
        reads: reads.clone(),
    };
    let mut observed = Vec::new();
    let mut aborts = 0;
    drain_observed_turn(
        &mut stream,
        &mut |event| {
            observed.push(event.clone());
            Ok(())
        },
        || aborts += 1,
    )
    .await
    .unwrap();
    assert_eq!(observed.len(), 2);
    assert_eq!(reads.load(Ordering::SeqCst), 3);
    assert_eq!(aborts, 0);
}
#[tokio::test]
async fn observer_failure_aborts_once_and_drains_before_releasing_the_fence() {
    let reads = Arc::new(AtomicUsize::new(0));
    let event =
        pilotdeck_protocol::parse_json(r#"{"type":"assistant_text_delta","text":"text"}"#).unwrap();
    let mut stream = Scripted {
        steps: VecDeque::from([
            Ok(Some(event.clone())),
            Ok(Some(event)),
            Err(SubmitError::new(
                "agent_aborted",
                "producer stopped after abort",
            )),
        ]),
        reads: reads.clone(),
    };
    let mut calls = 0;
    let mut aborts = 0;
    let failure = drain_observed_turn(
        &mut stream,
        &mut |_| {
            calls += 1;
            Err(CronError::error("fixture persist failed"))
        },
        || aborts += 1,
    )
    .await
    .unwrap_err();
    assert_eq!(
        String::from_utf16_lossy(failure.message().units()),
        "fixture persist failed"
    );
    assert_eq!(reads.load(Ordering::SeqCst), 3);
    assert_eq!(calls, 1);
    assert_eq!(aborts, 1);
}

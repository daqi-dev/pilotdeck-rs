use super::*;
use crate::host::{HostHandler, modules::tool_context::HostToolCheckpoint};
use pilotdeck_protocol::{parse_json, stringify_json};
use serde_json::{Value, json};

fn value(v: &JsValue) -> Value {
    serde_json::from_str(&stringify_json(v).unwrap().unwrap()).unwrap()
}
fn grants(checkpoint: &HostToolCheckpoint) -> Vec<JsString> {
    checkpoint.tool_context_state().allowed_read_files
}
fn item(id: &str) -> SteerMessage {
    SteerMessage {
        item_id: id.into(),
        message: parse_json(&format!(
            r#"{{"role":"user","content":[],"metadata":{{"queueItemId":"{id}"}}}}"#
        ))
        .unwrap(),
        allowed_read_files: Some(vec!["/safe".into()]),
    }
}
#[derive(Default)]
struct Records {
    rows: Mutex<Vec<Value>>,
    gate: Mutex<Option<watch::Receiver<bool>>>,
    fail_discard: AtomicBool,
}
impl MutationRecorder for Records {
    fn record(&self, turn: JsString, mutation: JsObject) -> HostFuture<()> {
        let mut row = value(&JsValue::Object(mutation));
        row.as_object_mut()
            .unwrap()
            .insert("turn".into(), json!(turn.to_utf8().unwrap()));
        if row["mutation"] == "discard" && self.fail_discard.load(Ordering::SeqCst) {
            return Box::pin(async { Err(stale()) });
        }
        self.rows.lock().unwrap().push(row);
        let gate = self.gate.lock().unwrap().take();
        Box::pin(async move {
            if let Some(mut gate) = gate {
                gate.wait_for(|open| *open).await.unwrap();
            }
            Ok(())
        })
    }
}
fn bind(session: &SessionSteer, router: &mut ModuleRouter, signal: Cancellation) -> TurnSteer {
    session
        .start_turn("t".into(), "same-run".into(), vec![], signal, router)
        .unwrap()
}
async fn drain(router: &ModuleRouter, close: bool) -> Result<Value, HostError> {
    let operation = if close {
        "drain_or_close_steer"
    } else {
        "drain_steer"
    };
    router.module_call(parse_json(&json!({"module":"turn","payload":{"operation":operation,"turnId":"forged","runId":"forged"}}).to_string()).unwrap()).await.map(|v| value(&JsValue::Object(v)))
}
async fn wait_record(records: &Records, count: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while records.rows.lock().unwrap().len() < count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interaction_steer_cancel_monitor_cannot_overtake_admitted_enqueue() {
    let resource = InteractionSteer::default();
    let records = Arc::new(Records::default());
    let session = resource
        .bind_session("s".into(), Some(records.clone()))
        .unwrap();
    let signal = Cancellation::new();
    let turn = bind(&session, &mut ModuleRouter::default(), signal.clone());
    let (events, received) = std::sync::mpsc::channel();
    *turn.run.close_events.lock().unwrap() = Some(events);
    // `operation` is the hook between check and serialize. The monitor reports
    // either lock contention or close, so no sleep/scheduling probability is
    // used to decide which side of the race won.
    let admitted = turn.mutate(|run| {
        signal.cancel();
        let event = received
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        assert_eq!(
            event, "blocked",
            "close overtook check -> enqueue admission"
        );
        run.steers.mailbox.enqueue(run.turn_id.clone(), item("old"))
    });
    assert!(admitted.await.is_err());
    turn.close().await.unwrap();
    let mut router = ModuleRouter::default();
    let current = bind(&session, &mut router, Cancellation::new());
    assert_eq!(drain(&router, false).await.unwrap(), json!({"messages":[]}));
    assert!(current.enqueue(item("old")).await.unwrap().accepted);
    resource.dispose().await.unwrap();
}

#[tokio::test]
async fn interaction_steer_cancelled_current_run_can_drain_before_and_after_close() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/interaction-steer/cancel-oracle.json"
    ))
    .unwrap();
    for (index, close) in [false, true].into_iter().enumerate() {
        let resource = InteractionSteer::default();
        let session = resource.bind_session("s".into(), None).unwrap();
        let signal = Cancellation::new();
        let mut router = ModuleRouter::default();
        let turn = bind(&session, &mut router, signal.clone());
        turn.enqueue(item("a")).await.unwrap();
        signal.cancel();
        // Current-thread runtime, and these ready ports do not yield: observe
        // TS abort -> drain before the monitor's asynchronous close executes.
        let first = drain(&router, close).await.unwrap();
        assert_eq!(
            first["messages"],
            json!([value(&JsValue::Object(item("a").to_object()))])
        );
        assert_eq!(first, oracle["abortDrain"][index]["first"]);
        assert_eq!(
            drain(&router, true).await.unwrap(),
            oracle["abortDrain"][index]["terminal"]
        );
        assert_eq!(turn.close().await.unwrap().len(), 1);
        assert_eq!(drain(&router, false).await.unwrap(), json!({"messages":[]}));
        assert_eq!(
            drain(&router, true).await.unwrap(),
            json!({"messages":[],"closed":true})
        );
        assert!(turn.enqueue(item("late")).await.is_err());
        bind(&session, &mut ModuleRouter::default(), Cancellation::new());
        assert!(drain(&router, true).await.is_err()); // superseded still fails
        resource.dispose().await.unwrap();
    }
}

#[test]
fn interaction_steer_cleanup_runtime_shutdown_is_observable() {
    for close_before_shutdown in [false, true] {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let resource = InteractionSteer::default();
        let (session, turn) = {
            let _entered = executor.enter();
            let session = resource.bind_session("s".into(), None).unwrap();
            let turn = bind(&session, &mut ModuleRouter::default(), Cancellation::new());
            if close_before_shutdown {
                drop(turn.close());
            }
            (session, turn)
        };
        // No block_on yet: the scheduled close was never polled.
        drop(executor);
        drop(turn.close()); // stopped Handle must not unwind synchronously
        let observer = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        observer.block_on(async {
            let result = tokio::time::timeout(std::time::Duration::from_secs(3), turn.close())
                .await
                .unwrap();
            assert!(result.unwrap_err().to_string().contains("cleanup task"));
            assert!(session.dispose().await.is_err());
            assert!(resource.dispose().await.is_err());
        });
    }
}

#[tokio::test]
async fn interaction_steer_cleanup_poison_reports_error_instead_of_hanging() {
    let resource = InteractionSteer::default();
    let session = resource.bind_session("s".into(), None).unwrap();
    let turn = bind(&session, &mut ModuleRouter::default(), Cancellation::new());
    turn.run.steers.mailbox.poison_state_for_test();
    let error = tokio::time::timeout(std::time::Duration::from_secs(3), turn.close())
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("cleanup task"));
    assert!(session.dispose().await.is_err());
    assert!(resource.dispose().await.is_err());
}

#[tokio::test]
async fn interaction_steer_session_trace_matches_real_ts_and_preserves_compaction() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/interaction-steer/oracle.json"
    ))
    .unwrap();
    let resource = InteractionSteer::default();
    let records = Arc::new(Records::default());
    let session = resource
        .bind_session("s".into(), Some(records.clone()))
        .unwrap();
    let checkpoint = HostToolCheckpoint::default();
    let mut router = ModuleRouter::default();
    router.turn.authorizations = Some(SteerAuthorizations::new(checkpoint.clone()));
    let persist: crate::host::modules::turn::PersistCompaction =
        Arc::new(|_, _| Box::pin(async { Ok(()) }));
    router.turn.ports.persist_compaction = Some(persist.clone());
    let turn = bind(&session, &mut router, Cancellation::new());
    assert!(Arc::ptr_eq(
        router.turn.ports.persist_compaction.as_ref().unwrap(),
        &persist
    ));
    assert_eq!(turn.run_id(), &JsString::from("same-run"));
    let mut trace = vec![];
    for _ in 0..2 {
        assert!(turn.enqueue(item("a")).await.unwrap().accepted);
        trace.push(json!({"accepted":true}));
    }
    assert!(turn.cancel("b".into()).await.unwrap().cancelled);
    trace.push(json!({"cancelled":true}));
    let rejected = turn.enqueue(item("b")).await.unwrap();
    trace.push(json!({"accepted":rejected.accepted,"reason":rejected.reason.unwrap().as_str()}));
    trace.push(drain(&router, false).await.unwrap()["messages"].clone());
    assert!(grants(&checkpoint).is_empty());
    turn.persist_and_apply("a".into(), item("a").message, |_| {
        Box::pin(async { Ok(()) })
    })
    .await
    .unwrap();
    assert_eq!(grants(&checkpoint), vec![JsString::from("/safe")]);
    assert_eq!(turn.messages(), vec![item("a").message]);
    trace.push(drain(&router, true).await.unwrap());
    for _ in 0..2 {
        assert!(turn.close().await.unwrap().is_empty());
        trace.push(json!([]));
    }
    assert_eq!(json!(trace), oracle["trace"]);
    assert_eq!(json!(*records.rows.lock().unwrap()), oracle["mutations"]);
    assert!(turn.enqueue(item("late")).await.is_err());
    assert_eq!(
        oracle["afterFinish"],
        json!({"accepted":false,"reason":"no_active_turn"})
    );
    session.dispose().await.unwrap();
    resource.dispose().await.unwrap();
    resource.dispose().await.unwrap();
    assert_eq!(resource.release_count(), 1);
    assert_eq!(turn.release_count(), 1);
}

#[tokio::test]
async fn interaction_steer_rejects_old_run_session_generation_and_waits_for_insert() {
    let resource = InteractionSteer::default();
    let records = Arc::new(Records::default());
    let (gate, receiver) = watch::channel(false);
    *records.gate.lock().unwrap() = Some(receiver);
    let session = resource
        .bind_session("s".into(), Some(records.clone()))
        .unwrap();
    let mut router = ModuleRouter::default();
    let old = bind(&session, &mut router, Cancellation::new());
    let insertion = old.enqueue(item("a"));
    wait_record(&records, 1).await;
    drop(old.close()); // cleanup must outlive this future
    assert!(
        session
            .start_turn(
                "t".into(),
                "same-run".into(),
                vec![],
                Cancellation::new(),
                &mut ModuleRouter::default()
            )
            .is_err()
    );
    assert!(drain(&router, false).await.is_err());
    gate.send_replace(true);
    assert!(insertion.await.is_err());
    old.close().await.unwrap();
    assert_eq!(
        records
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v["mutation"] == "discard")
            .count(),
        1
    );
    let new = bind(&session, &mut router, Cancellation::new());
    old.close().await.unwrap();
    assert!(old.enqueue(item("late")).await.is_err());
    assert!(old.cancel("a".into()).await.is_err());
    assert_eq!(drain(&router, false).await.unwrap(), json!({"messages":[]}));
    new.enqueue(item("a")).await.unwrap();
    session.dispose().await.unwrap();
    let replacement = resource.bind_session("s".into(), None).unwrap();
    let current = bind(&replacement, &mut router, Cancellation::new());
    session.dispose().await.unwrap(); // old session cannot close replacement
    assert!(
        session
            .start_turn(
                "t".into(),
                "same-run".into(),
                vec![],
                Cancellation::new(),
                &mut ModuleRouter::default()
            )
            .is_err()
    );
    current.enqueue(item("a")).await.unwrap();
    drop(resource.dispose());
    assert!(drain(&router, false).await.is_err());
    assert!(resource.bind_session("new".into(), None).is_err());
    replacement.dispose().await.unwrap();
    assert_eq!(current.release_count(), 1);
    assert_eq!(old.release_count(), 1);
    let fresh = InteractionSteer::default();
    let fresh_session = fresh.bind_session("s".into(), None).unwrap();
    let live = bind(&fresh_session, &mut router, Cancellation::new());
    assert!(new.enqueue(item("late")).await.is_err());
    assert!(live.enqueue(item("a")).await.unwrap().accepted);
    fresh.dispose().await.unwrap();
}

#[tokio::test]
async fn interaction_steer_failed_durable_never_acks_or_grants_and_cancel_waits_for_writer() {
    let resource = InteractionSteer::default();
    let records = Arc::new(Records::default());
    let session = resource
        .bind_session("s".into(), Some(records.clone()))
        .unwrap();
    let checkpoint = HostToolCheckpoint::default();
    let mut router = ModuleRouter::default();
    router.turn.authorizations = Some(SteerAuthorizations::new(checkpoint.clone()));
    let signal = Cancellation::new();
    let turn = bind(&session, &mut router, signal.clone());
    turn.enqueue(item("a")).await.unwrap();
    drain(&router, false).await.unwrap();
    assert!(
        turn.persist_and_apply("wrong".into(), item("a").message, |_| panic!(
            "wrong item must not persist"
        ))
        .await
        .is_err()
    );
    assert!(
        turn.persist_and_apply("a".into(), item("a").message, |_| Box::pin(async {
            Err(stale())
        }))
        .await
        .is_err()
    );
    assert!(turn.messages().is_empty());
    assert!(grants(&checkpoint).is_empty());
    let (entered, mut started) = watch::channel(false);
    let (release, mut gate) = watch::channel(false);
    let applying = turn.persist_and_apply("a".into(), item("a").message, move |_| {
        Box::pin(async move {
            entered.send_replace(true);
            gate.wait_for(|open| *open).await.unwrap();
            Ok(())
        })
    });
    started.wait_for(|yes| *yes).await.unwrap();
    drop(applying);
    signal.cancel();
    assert!(turn.enqueue(item("b")).await.is_err());
    drop(session.dispose());
    assert_eq!(turn.release_count(), 0);
    release.send_replace(true);
    assert!(turn.close().await.unwrap().is_empty());
    assert!(turn.close().await.unwrap().is_empty());
    assert_eq!(turn.messages(), vec![item("a").message]);
    assert_eq!(grants(&checkpoint), vec![JsString::from("/safe")]);
    assert_eq!(
        records
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v["mutation"] == "discard")
            .count(),
        0
    );
    assert_eq!(
        records
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v["mutation"] == "claim")
            .count(),
        1
    );
    assert_eq!(turn.release_count(), 1);
    resource.dispose().await.unwrap();
}

#[tokio::test]
async fn interaction_steer_failed_cleanup_stays_failed_and_grants_do_not_cross_runs() {
    let resource = InteractionSteer::default();
    let records = Arc::new(Records::default());
    let session = resource
        .bind_session("s".into(), Some(records.clone()))
        .unwrap();
    let checkpoint = HostToolCheckpoint::default();
    let mut router = ModuleRouter::default();
    router.turn.authorizations = Some(SteerAuthorizations::new(checkpoint.clone()));
    let old = bind(&session, &mut router, Cancellation::new());
    old.enqueue(item("a")).await.unwrap();
    drain(&router, false).await.unwrap();
    old.close().await.unwrap();
    let new = bind(&session, &mut router, Cancellation::new());
    let mut next = item("a");
    next.allowed_read_files = None;
    new.enqueue(next.clone()).await.unwrap();
    drain(&router, false).await.unwrap();
    new.persist_and_apply("a".into(), next.message, |_| Box::pin(async { Ok(()) }))
        .await
        .unwrap();
    assert!(grants(&checkpoint).is_empty());
    new.enqueue(item("pending")).await.unwrap();
    records.fail_discard.store(true, Ordering::SeqCst);
    assert!(new.close().await.is_err());
    assert!(new.close().await.is_err());
    assert_eq!(new.release_count(), 1);
    assert!(
        session
            .start_turn(
                "t".into(),
                "same-run".into(),
                vec![],
                Cancellation::new(),
                &mut router
            )
            .is_err()
    );
    assert!(session.dispose().await.is_err());
    resource.bind_session("other".into(), None).unwrap();
    assert!(resource.dispose().await.is_err());
    assert!(resource.dispose().await.is_err());
}

#[tokio::test]
async fn interaction_steer_replacement_barrier_survives_unstarted_bindings() {
    let resource = InteractionSteer::default();
    let records = Arc::new(Records::default());
    let original = resource
        .bind_session("s".into(), Some(records.clone()))
        .unwrap();
    let old = bind(&original, &mut ModuleRouter::default(), Cancellation::new());
    old.enqueue(item("old")).await.unwrap();
    let (gate, receiver) = watch::channel(false);
    *records.gate.lock().unwrap() = Some(receiver);
    let waiting = resource
        .bind_session("s".into(), Some(records.clone()))
        .unwrap();
    wait_record(&records, 2).await; // the old discard has entered the recorder
    let mut superseded_wait = Box::pin(waiting.wait_ready());
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(superseded_wait.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    waiting.dispose().await.unwrap(); // no run, but must not erase old's barrier
    let latest = resource
        .bind_session("s".into(), Some(records.clone()))
        .unwrap();
    let mut latest_wait = Box::pin(latest.wait_ready());
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(latest_wait.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert!(
        latest
            .start_turn(
                "t".into(),
                "r".into(),
                vec![],
                Cancellation::new(),
                &mut ModuleRouter::default()
            )
            .is_err(),
        "direct start bypassed the cleanup barrier"
    );
    let independent = resource.bind_session("other".into(), None).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), independent.wait_ready())
        .await
        .expect("another session key must not wait on this barrier")
        .unwrap();
    bind(
        &independent,
        &mut ModuleRouter::default(),
        Cancellation::new(),
    );
    println!(
        "replacement chain: A discard pending, B disposed without start, C blocked; other key starts"
    );
    gate.send_replace(true);
    latest_wait.await.unwrap();
    assert!(
        superseded_wait.await.is_err(),
        "superseded waiter regained authority"
    );
    assert!(waiting.wait_ready().await.is_err());
    let current = bind(&latest, &mut ModuleRouter::default(), Cancellation::new());
    original.dispose().await.unwrap();
    current.enqueue(item("new")).await.unwrap();
    resource.dispose().await.unwrap();
    assert_eq!(old.release_count(), 1);
}

#[tokio::test]
async fn interaction_steer_failed_cleanup_blocks_same_key_rebinding_only() {
    let resource = InteractionSteer::default();
    let records = Arc::new(Records::default());
    let original = resource
        .bind_session("s".into(), Some(records.clone()))
        .unwrap();
    let old = bind(&original, &mut ModuleRouter::default(), Cancellation::new());
    old.enqueue(item("old")).await.unwrap();
    records.fail_discard.store(true, Ordering::SeqCst);
    assert!(original.dispose().await.is_err());
    for _ in 0..2 {
        let replacement = resource.bind_session("s".into(), None).unwrap();
        assert!(
            replacement.wait_ready().await.is_err(),
            "failed cleanup was forgotten on rebind"
        );
        assert!(
            replacement
                .start_turn(
                    "t".into(),
                    "r".into(),
                    vec![],
                    Cancellation::new(),
                    &mut ModuleRouter::default()
                )
                .is_err()
        );
        replacement.dispose().await.unwrap();
    }
    let independent = resource.bind_session("other".into(), None).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), independent.wait_ready())
        .await
        .expect("another session key must not wait on this barrier")
        .unwrap();
    bind(
        &independent,
        &mut ModuleRouter::default(),
        Cancellation::new(),
    );
    assert!(resource.dispose().await.is_err());
    println!("failed barrier: repeated same-key rebind fails; other key remains usable");
}

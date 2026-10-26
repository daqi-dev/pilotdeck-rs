use super::*;

fn golden() -> JsValue {
    crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/host-event-pump/oracle.json"
    ))
}
fn case(name: &str) -> Vec<JsValue> {
    let oracle = golden();
    let JsValue::Array(cases) = oracle.get("cases").unwrap() else {
        panic!("cases")
    };
    let case = cases
        .iter()
        .find(|c| c.get("name") == Some(&name.into()))
        .unwrap();
    let JsValue::Array(trace) = case.get("trace").unwrap() else {
        panic!("trace")
    };
    trace.clone()
}

fn identity() -> ExecutionIdentity {
    parse_execute_request(&input(), &options(), vec![])
        .unwrap()
        .0
        .identity
}
fn pump(
    handler: Arc<StreamHandler>,
    execution: HostFuture<ExecuteResult>,
) -> HostFuture<ExecuteResult> {
    Box::pin(async move { handler.with_pump(execution, identity()).await })
}
async fn advance<T>(task: &mut HostFuture<T>, ms: u64) {
    tokio::time::advance(Duration::from_millis(ms)).await;
    assert_pending(task).await;
}
async fn opened(capacity: usize) -> (Arc<Handler>, Arc<StreamHandler>, mpsc::Receiver<JsValue>) {
    let (inner, handler, mut rx) = buffered_handler(capacity);
    deliver(&handler, &host_event("turn_started"), false)
        .await
        .unwrap();
    rx.try_recv().unwrap();
    (inner, handler, rx)
}

#[tokio::test(start_paused = true)]
async fn host_pump_idle_reset_and_heartbeat_oracle() {
    for name in ["idle", "reset", "heartbeat", "interleave"] {
        let (inner, handler, mut rx) = opened(16).await;
        let mut task = pump(handler.clone(), Box::pin(std::future::pending()));
        assert_pending(&mut task).await;
        let mut elapsed = 0;
        for step in case(name) {
            let at = step.get("at").unwrap().as_number().unwrap() as u64;
            let event = step.get("event").unwrap();
            if event.get("type") != Some(&"subagent_status".into())
                && name == "reset"
                && elapsed == 0
            {
                advance(&mut task, 499).await;
                // The fixture's opening wire is equivalent to another legal
                // wire event here; the test helper already opened integrity.
                deliver(&handler, &host_event("input_accepted"), false)
                    .await
                    .unwrap();
                rx.try_recv().unwrap();
                elapsed = 499;
                continue;
            }
            while elapsed + 500 < at {
                advance(&mut task, 500).await;
                elapsed += 500;
                assert!(rx.try_recv().is_err());
            }
            if event.get("type") != Some(&"subagent_status".into()) {
                queue_host_events(&inner, std::slice::from_ref(event));
            }
            if at > elapsed {
                advance(&mut task, at - elapsed - 1).await;
                assert!(rx.try_recv().is_err());
                advance(&mut task, 1).await;
            }
            elapsed = at;
            assert_eq!(&rx.try_recv().unwrap(), event, "{name} at {at}");
        }
        if name == "interleave" {
            advance(&mut task, 2000).await;
            assert!(rx.try_recv().is_err());
        }
        drop(task);
        queue_host_events(&inner, &[host_event("after_drop")]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::task::yield_now().await;
        assert!(rx.try_recv().is_err());
    }
}

#[tokio::test(start_paused = true)]
async fn host_pump_pending_callback_does_not_hold_batches() {
    let (inner, handler, mut rx) = opened(8).await;
    let mut task = pump(handler.clone(), Box::pin(std::future::pending()));
    assert_pending(&mut task).await;
    // Off-phase callback: deadline remains 500, not 100 + 500.
    advance(&mut task, 100).await;
    let trace = case("callback");
    queue_host_events(&inner, &[trace[0].get("event").unwrap().clone()]);
    inner.block_module.store(true, Ordering::SeqCst);
    let mut callback = handler.module_call(JsValue::Object(JsObject::new()));
    assert_pending(&mut callback).await;
    advance(&mut task, 100).await;
    queue_host_events(&inner, &[trace[1].get("event").unwrap().clone()]);
    advance(&mut task, 299).await;
    assert_pending(&mut callback).await;
    assert!(rx.try_recv().is_err());
    advance(&mut task, 1).await;
    assert_pending(&mut callback).await;
    assert_eq!(rx.try_recv().unwrap(), *trace[0].get("event").unwrap());
    assert_pending(&mut task).await;
    assert_eq!(rx.try_recv().unwrap(), *trace[1].get("event").unwrap());
    queue_host_events(&inner, &[trace[2].get("event").unwrap().clone()]);
    advance(&mut task, 500).await;
    assert_eq!(rx.try_recv().unwrap(), *trace[2].get("event").unwrap());
    assert_pending(&mut callback).await;
}

#[tokio::test(start_paused = true)]
async fn host_pump_wire_predecessor_cancel_and_drop_are_bounded() {
    for stop in ["apply", "release", "cancel", "drop"] {
        let (inner, handler, mut rx) = opened(8).await;
        let cancel = Cancellation::default();
        let signal = cancel.clone();
        let (done, result) = oneshot::channel();
        let mut task = pump(
            handler.clone(),
            Box::pin(async move {
                tokio::select! {
                    result = result => result.unwrap(),
                    _ = signal.cancelled() => Ok(ExecuteResult::Aborted { terminal: None }),
                }
            }),
        );
        assert_pending(&mut task).await;
        let guard = handler
            .validate_event(&host_event("input_accepted"))
            .unwrap();
        queue_host_events(&inner, &[host_event("background")]);
        advance(&mut task, 500).await;
        advance(&mut task, 500).await;
        assert!(rx.try_recv().is_err()); // a never-completing predecessor blocks publication
        if stop == "apply" {
            handler
                .event(wire(&host_event("input_accepted"), false))
                .await
                .unwrap();
            assert_eq!(rx.try_recv().unwrap(), host_event("input_accepted"));
            assert_pending(&mut task).await;
            advance(&mut task, 499).await;
            assert!(rx.try_recv().is_err());
            advance(&mut task, 1).await;
            assert_eq!(rx.try_recv().unwrap(), host_event("background"));
            drop(guard);
            done.send(Ok(ExecuteResult::Aborted { terminal: None }))
                .unwrap();
            task.await.unwrap();
        } else if stop == "release" {
            // Dropping a reserved queue item releases the pump without bypassing
            // any surviving predecessor; covered also by existing splice tests.
            drop(guard);
            assert_pending(&mut task).await;
            assert_eq!(rx.try_recv().unwrap(), host_event("background"));
            done.send(Ok(ExecuteResult::Aborted { terminal: None }))
                .unwrap();
            task.await.unwrap();
        } else {
            if stop == "cancel" {
                cancel.cancel(None);
                task.await.unwrap();
                drop(done);
            } else {
                drop(task);
                drop(done);
            }
            drop(guard);
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_secs(10)).await;
            tokio::task::yield_now().await;
            assert!(rx.try_recv().is_err());
        }
    }
}

#[tokio::test(start_paused = true)]
async fn host_pump_finish_cancel_drop_stop_draining() {
    for name in ["finish", "cancel", "drop"] {
        let (inner, handler, mut rx) = opened(8).await;
        let cancellation = Cancellation::default();
        let (done, result) = oneshot::channel();
        let mut task = pump(
            handler.clone(),
            Box::pin(async move { result.await.unwrap() }),
        );
        assert_pending(&mut task).await;
        if name == "drop" {
            drop(task);
            drop(done);
        } else {
            if name == "cancel" {
                cancellation.cancel(None);
                assert_pending(&mut task).await;
                queue_host_events(&inner, &[host_event("during_cancel")]);
                advance(&mut task, 500).await;
                assert_eq!(rx.try_recv().unwrap(), host_event("during_cancel"));
            }
            if name == "finish" {
                queue_host_events(&inner, &[case(name)[0].get("event").unwrap().clone()]);
            }
            if name == "finish" {
                deliver(&handler, &host_event("turn_completed"), true)
                    .await
                    .unwrap();
                inner
                    .drains
                    .lock()
                    .unwrap()
                    .push_back(Err(HostError::new(HostErrorKind::Protocol, "after final")));
                advance(&mut task, 1000).await;
                assert_eq!(inner.drains.lock().unwrap().len(), 1);
                inner.drains.lock().unwrap().clear();
            }
            done.send(Ok(ExecuteResult::Aborted { terminal: None }))
                .unwrap();
            task.await.unwrap();
            if name == "finish" {
                assert_eq!(rx.try_recv().unwrap(), *case(name)[0].get("event").unwrap());
            }
        }
        queue_host_events(&inner, &[host_event("after_stop")]);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::task::yield_now().await;
        assert!(rx.try_recv().is_err());
        assert_eq!(inner.drains.lock().unwrap().len(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn host_pump_drain_error_cuts_off_execution() {
    let (inner, handler, mut rx) = opened(1).await;
    let mut task = pump(handler.clone(), Box::pin(std::future::pending()));
    assert_pending(&mut task).await;
    inner
        .drains
        .lock()
        .unwrap()
        .push_back(Err(HostError::new(HostErrorKind::Protocol, "pump failed")));
    tokio::time::advance(Duration::from_millis(500)).await;
    assert_eq!(task.await.unwrap_err().message, "pump failed");
    assert!(handler.validate_event(&host_event("model_event")).is_err());
    assert!(rx.try_recv().is_err());
}

#[test]
#[ignore = "requires Node v22.22.0 and PDRS_BASELINE"]
fn host_pump_regenerates_identically() {
    crate::test_baseline::regenerates_identically("host-event-pump");
}

#[tokio::test(start_paused = true)]
async fn host_pump_stops_before_dispose_closes_port() {
    let (inner, handler, mut rx) = opened(1).await;
    let closing = handler.clone();
    let mut task = pump(
        handler.clone(),
        Box::pin(async move {
            closing.dispose().await?;
            std::future::pending().await
        }),
    );
    assert_pending(&mut task).await;
    assert_eq!(inner.disposed.load(Ordering::SeqCst), 1);
    inner
        .drains
        .lock()
        .unwrap()
        .push_back(Err(HostError::new(HostErrorKind::Protocol, "closed port")));
    handler
        .forward(Box::pin(async { Ok(()) }), None, None, false, true)
        .await
        .unwrap();
    advance(&mut task, 1000).await;
    assert!(rx.try_recv().is_err());
    assert_eq!(inner.drains.lock().unwrap().len(), 1);
}

/// #855: a final arriving while the pump holds a reserved send slot and waits
/// in delivery.wait() (live.rs:884) publishes strictly behind that slot, and
/// the pump's post-wake re-check keeps the turn closed without refreshing the
/// activity deadline. The module-deadline probe at the end is what turns the
/// "remove the :885 stopped re-check" mutant red: the probe captures its
/// deadline at forward() entry before the non-module final send refreshes
/// activity, so correct code keeps the pre-final deadline and publishes the
/// captured batch as soon as the final's slot completes.
#[tokio::test(start_paused = true)]
async fn host_pump_final_orders_behind_the_reserved_slot() {
    let (inner, handler, mut rx) = opened(1).await;
    let mut task = pump(handler.clone(), Box::pin(std::future::pending()));
    assert_pending(&mut task).await; // activity = 0, deadline = 500
    // A module callback captures [a, b] and reserves M2; the capacity-1
    // channel parks its send mid-batch after publishing a.
    queue_host_events(&inner, &[host_event("batch_a"), host_event("batch_b")]);
    let mut module_call = handler.module_call(JsValue::Null);
    assert_pending(&mut module_call).await;
    // At the tick deadline the pump reserves K behind M2 and parks in
    // delivery.wait() — the exact live.rs:884 state the final must meet.
    advance(&mut task, 499).await;
    advance(&mut task, 1).await;
    // The final arrives while the pump is waiting on its reserved slot: it
    // marks the turn stopped and drains [e] at forward entry, then reserves F
    // behind K on the same send chain instead of overtaking it.
    queue_host_events(&inner, &[host_event("final_batch")]);
    let mut final_call = handler.event(wire(&host_event("turn_completed"), true));
    assert_pending(&mut final_call).await;
    assert_eq!(rx.try_recv().unwrap(), host_event("batch_a"));
    assert!(rx.try_recv().is_err(), "the final's batch waits behind K");
    // Completing M2 wakes the pump; the :885 re-check keeps it closed. Its
    // reserved slot K hands the chain to F, so e publishes only now.
    timeout(Duration::from_secs(5), module_call)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rx.try_recv().unwrap(), host_event("batch_b"));
    assert!(rx.try_recv().is_err(), "e still waits for the pump's slot");
    assert_pending(&mut task).await; // woke, re-checked stopped, published nothing
    // Activity probe, armed before the final publishes (the non-module send
    // refreshes activity): a blocked module callback captures one more host
    // event and reserves behind F; its deadline is read at entry, so correct
    // code (no :886 refresh) keeps the pre-final deadline 500, already due.
    queue_host_events(&inner, &[host_event("probe_batch")]);
    inner.block_module.store(true, Ordering::SeqCst);
    let mut probe = handler.module_call(JsValue::Null);
    assert_pending(&mut probe).await;
    timeout(Duration::from_secs(5), final_call)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rx.try_recv().unwrap(), host_event("final_batch"));
    assert_pending(&mut probe).await;
    assert_eq!(
        rx.try_recv().unwrap(),
        host_event("probe_batch"),
        "the probe's batch publishes at the pre-final deadline, not 500 ms later"
    );
    // The stopped pump never drains again.
    drop(probe);
    queue_host_events(&inner, &[host_event("never_drained")]);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;
    assert!(rx.try_recv().is_err());
    assert_eq!(inner.drains.lock().unwrap().len(), 1);
}

use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Waker};

const ORACLE: &str = include_str!("../../../tests/fixtures/session-admission/oracle.json");

fn js(value: &Value, field: &str) -> Option<JsString> {
    value.get(field).and_then(Value::as_str).map(JsString::from)
}

fn string(value: &JsString) -> String {
    value.to_utf8().unwrap()
}

/// Every result below is computed by the production components and compared to
/// the real Router/fence/Gateway output. Inputs and expectations share one
/// fixture; none of the Gateway abort predicate is implemented in this driver.
#[tokio::test]
async fn real_ts_admission_completion_and_gateway_abort() {
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(
        oracle["sourceCommit"],
        "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"
    );
    assert_eq!(oracle["sourceSha256"].as_object().unwrap().len(), 7);
    for scenario in oracle["scenarios"].as_array().unwrap() {
        let turns = SessionTurns::default();
        let mut handles = BTreeMap::<String, TurnCompletionHandle>::new();
        let mut settled = BTreeMap::<String, bool>::new();
        type Pending<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
        let mut pending: Vec<(String, Pending<'_>)> = Vec::new();
        let aborts = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut maintenance_key = None;
        for (index, step) in scenario["steps"].as_array().unwrap().iter().enumerate() {
            let key = js(step, "key").unwrap_or_default();
            let run = js(step, "run");
            let reason = js(step, "reason");
            let id = step["id"].as_str().unwrap_or_default();
            let mut value = Value::Null;
            match step["op"].as_str().unwrap() {
                "begin" => value = json!(turns.admission.begin_turn(&key, run.as_ref().unwrap())),
                "active" => {
                    value = json!({"active": turns.admission.has_active_turn(&key),
                        "run": turns.admission.active_turn_run_id(&key).as_ref().map(string)});
                }
                "end" => turns.admission.end_turn(&key, run.as_ref()),
                "shutdown" => turns.admission.shutdown(),
                "maintenance_start" => {
                    assert!(turns.admission.begin_maintenance(&key));
                    maintenance_key = Some(key.clone());
                }
                "maintenance_end" => turns
                    .admission
                    .end_maintenance(maintenance_key.as_ref().unwrap()),
                "fence_begin" => {
                    handles.insert(
                        id.to_owned(),
                        turns.completion.begin(&key, run.as_ref().unwrap()),
                    );
                }
                "current" => value = json!(turns.completion.is_current(&key, &handles[id])),
                "cancel" => {
                    value = json!(turns.completion.cancel(&key, reason.as_ref(), run.as_ref()));
                }
                "complete" => turns.completion.complete(&key, &handles[id]),
                "discard_handle" => {
                    handles.remove(id);
                }
                "handle_wait" => {
                    settled.insert("orphan".to_owned(), false);
                    pending.push(("orphan".to_owned(), Box::pin(handles[id].done())));
                }
                "wait" => {
                    settled.insert(id.to_owned(), false);
                    pending.push((
                        id.to_owned(),
                        Box::pin(turns.completion.wait_for_completion(&key)),
                    ));
                }
                "abort_start" => {
                    let aborts = aborts.clone();
                    let expected_target = run.clone();
                    let future = turns.abort_turn(
                        &key,
                        run.as_ref(),
                        reason.as_ref(),
                        move |target, reason| {
                            assert_eq!(target, expected_target);
                            aborts.lock().unwrap().push(string(&reason));
                            std::future::ready(())
                        },
                    );
                    settled.insert(id.to_owned(), false);
                    pending.push((id.to_owned(), Box::pin(future)));
                }
                op => panic!("unknown oracle step: {op}"),
            }
            let mut context = Context::from_waker(Waker::noop());
            pending.retain_mut(|(id, future)| {
                if future.as_mut().poll(&mut context).is_ready() {
                    *settled.get_mut(id).unwrap() = true;
                    false
                } else {
                    true
                }
            });
            let handle_states: BTreeMap<_, _> = handles
                .iter()
                .map(|(id, handle)| {
                    let reason = match handle.abort_reason() {
                        None => Value::Null,
                        Some(CompletionAbortReason::DefaultAbort) => json!({
                            "name": "AbortError", "message": "This operation was aborted",
                        }),
                        Some(CompletionAbortReason::Reason(reason)) => json!(string(&reason)),
                    };
                    (
                        id,
                        json!({"cancelled": handle.is_cancelled(), "reason": reason}),
                    )
                })
                .collect();
            let mut reasons: Vec<_> = turns
                .aborted_reasons
                .lock()
                .unwrap()
                .iter()
                .map(|(key, value)| json!([string(key), string(value)]))
                .collect();
            reasons.sort_by_key(Value::to_string);
            let actual = json!({"value": value, "handles": handle_states, "settled": settled,
                "aborts": *aborts.lock().unwrap(), "reasons": reasons});
            assert_eq!(
                actual, scenario["results"][index],
                "{} step {index}: {step}",
                scenario["name"]
            );
        }
        let pending_ids: Vec<_> = pending.iter().map(|(id, _)| id.as_str()).collect();
        let expected: Vec<_> = scenario["pending"]
            .as_array()
            .map(|ids| ids.iter().map(|id| id.as_str().unwrap()).collect())
            .unwrap_or_default();
        assert_eq!(
            pending_ids, expected,
            "{} undrained waits",
            scenario["name"]
        );
    }
}

#[test]
fn simultaneous_same_session_has_one_winner_and_other_sessions_admit() {
    let admission = Arc::new(SessionAdmission::default());
    let start = Arc::new(std::sync::Barrier::new(9));
    let threads: Vec<_> = (0..8)
        .map(|n| {
            let admission = admission.clone();
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                let same = admission.begin_turn(&"same".into(), &n.to_string().into());
                let independent =
                    admission.begin_turn(&format!("session-{n}").into(), &"run".into());
                (same, independent)
            })
        })
        .collect();
    start.wait();
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.0).count(), 1);
    assert!(results.iter().all(|result| result.1));
}

#[tokio::test]
async fn waits_capture_identity_before_first_poll_and_keep_notifications() {
    let fence = TurnCompletionFence::default();
    let key = JsString::from("a");
    let old = fence.begin(&key, &"same".into());
    let old_wait = fence.wait_for_completion(&key);
    let absent_wait = fence.wait_for_completion(&"absent".into());
    let done = old.done();
    let cancelled = old.cancelled();
    fence.cancel(&key, Some(&"cancel-old".into()), None);
    let new = fence.begin(&key, &"same".into());
    let late = fence.begin(&"absent".into(), &"late".into());
    // All state changes precede the futures' first poll.
    fence.complete(&key, &old);
    assert!(!new.is_complete());
    assert!(!late.is_complete());
    assert!(!fence.cancel(&key, Some(&"stale".into()), Some(&"other".into())));
    assert!(fence.cancel(&key, Some(&"cancel-new".into()), None));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        old_wait.await;
        absent_wait.await;
        done.await;
        cancelled.await;
    })
    .await
    .unwrap();
    assert!(fence.is_current(&key, &new));
}

#[test]
fn discarded_handle_does_not_fabricate_completion_or_cancellation() {
    let fence = TurnCompletionFence::default();
    let key = JsString::from("a");
    let old = fence.begin(&key, &"old".into());
    let mut done = Box::pin(old.done());
    let mut cancelled = Box::pin(old.cancelled());
    fence.begin(&key, &"new".into());
    drop(old);
    drop(fence);
    let mut context = Context::from_waker(Waker::noop());
    assert!(done.as_mut().poll(&mut context).is_pending());
    assert!(cancelled.as_mut().poll(&mut context).is_pending());
}

#[test]
fn keys_preserve_utf16_and_maintenance_shares_turn_admission() {
    let admission = SessionAdmission::default();
    let key = JsString::from_utf16(vec![0xd800]);
    let other = JsString::from_utf16(vec![0xfffd]);
    assert!(admission.begin_maintenance(&key));
    assert!(!admission.begin_maintenance(&key));
    assert!(!admission.begin_turn(&key, &"r".into()));
    assert!(admission.begin_turn(&other, &"r".into()));
    assert!(!admission.begin_maintenance(&other));
    admission.end_maintenance(&key);
    assert!(admission.begin_turn(&key, &"r".into()));
    admission.shutdown();
    assert!(!admission.begin_maintenance(&key));
}

#[test]
fn abort_reason_cleanup_is_exact() {
    let turns = SessionTurns::default();
    let key = JsString::from("a");
    let run = JsString::from("run");
    turns.admission.begin_turn(&key, &run);
    // The synchronous prefix and sink run when invoked, even before polling.
    let operation = turns.abort_turn(&key, Some(&run), Some(&"".into()), |target, reason| {
        assert_eq!(target.as_ref(), Some(&run));
        assert_eq!(reason, JsString::from(""));
        assert_eq!(turns.admission.active_turn_run_id(&key), Some(run.clone()));
        std::future::ready(())
    });
    assert_eq!(turns.aborted_reason(&key, &run), Some("".into()));
    turns.clear_aborted_reason(&key, &"other".into());
    assert_eq!(turns.aborted_reason(&key, &run), Some("".into()));
    turns.clear_aborted_reason(&key, &run);
    assert_eq!(turns.aborted_reason(&key, &run), None);
    drop(operation);
}

/// Pause the actual sink between the abort decision and owner-handle selection.
/// Every iteration forces end(old) + begin(new) into that window; no sleep or
/// scheduler luck is involved. Odd rounds have no completion handle, exercising
/// the admitted-before-fence fallback as well as cancellation's success branch.
#[test]
fn old_abort_never_targets_replacement_across_threads() {
    use std::sync::Barrier;

    const ROUNDS: usize = 2_000;
    let turns = SessionTurns::default();
    let key = JsString::from("session");
    let old = JsString::from("old");
    let new = JsString::from("new");
    let owner = Mutex::new(old.clone());
    let start = Barrier::new(2);
    let selected = Barrier::new(2);
    let replaced = Barrier::new(2);
    let finished = Barrier::new(2);

    let new_hits = std::thread::scope(|scope| {
        let aborter = scope.spawn(|| {
            let mut new_hits = 0;
            for _ in 0..ROUNDS {
                start.wait();
                let operation = turns.abort_turn(&key, Some(&old), None, |target, _reason| {
                    // Reentrant component reads also check that no component
                    // lock is held while calling the sink.
                    assert_eq!(turns.admission.active_turn_run_id(&key), Some(old.clone()));
                    selected.wait();
                    replaced.wait();
                    // This is the live owner's required atomic filter + handle
                    // selection, under the same lock used to publish new below.
                    let current = owner.lock().unwrap();
                    if target.as_ref().is_none_or(|id| id == &*current) && *current == new {
                        new_hits += 1;
                    }
                    std::future::ready(())
                });
                // This test exercises synchronous selection, not async drain.
                drop(operation);
                finished.wait();
            }
            new_hits
        });
        for round in 0..ROUNDS {
            assert!(turns.admission.begin_turn(&key, &old));
            *owner.lock().unwrap() = old.clone();
            let old_handle = (round % 2 == 0).then(|| turns.completion.begin(&key, &old));
            start.wait();
            selected.wait();
            turns.admission.end_turn(&key, Some(&old));
            if let Some(handle) = old_handle {
                assert!(handle.is_cancelled());
                turns.completion.complete(&key, &handle);
            }
            assert!(turns.admission.begin_turn(&key, &new));
            let new_handle = turns.completion.begin(&key, &new);
            *owner.lock().unwrap() = new.clone();
            replaced.wait();
            finished.wait();
            assert!(!new_handle.is_cancelled());
            assert_eq!(turns.aborted_reason(&key, &new), None);
            turns.completion.complete(&key, &new_handle);
            turns.admission.end_turn(&key, Some(&new));
            turns.clear_aborted_reason(&key, &old);
        }
        aborter.join().unwrap()
    });
    println!("replacement hits: {new_hits}/{ROUNDS}");
    assert_eq!(new_hits, 0, "an old abort reached the replacement owner");
}

use super::*;
use crate::{
    host::modules::{
        ModuleRouter,
        event::{EventSink, TurnIdentity},
    },
    runtime::project::{
        interaction_events::{InteractionEvents, TurnEvents},
        interaction_notice::{DiscardNotice, DiscardSource},
    },
};

fn port(resource: &InteractionEvents) -> TurnEvents {
    resource
        .bind_turn(
            TurnIdentity::new("s", "t"),
            "run".into(),
            &mut ModuleRouter::default(),
        )
        .expect("#741 notice bind")
}
fn notice() -> DiscardNotice {
    DiscardNotice::new(DiscardSource::Hooks, 2, "s".into(), "old".into())
}
fn queued() -> (InteractionEvents, TurnEvents) {
    let resource = InteractionEvents::default();
    let port = port(&resource);
    port.queue_notices(vec![notice()]);
    (resource, port)
}
fn assert_start(events: &[JsValue]) {
    assert_eq!(
        events,
        &[
            object([
                ("type", "turn_started".into()),
                ("sessionId", "s".into()),
                ("turnId", "t".into())
            ]),
            JsValue::Object(notice().to_warning(&"s".into(), &"t".into()))
        ],
        "#741 notice start output"
    );
}
async fn first(name: &str) {
    let oracle = baseline::oracle(include_str!(
        "../../../tests/fixtures/turn-start-notices/oracle.json"
    ));
    let JsValue::Array(rows) = oracle.get("rows").expect("#741 notice TS rows") else {
        panic!("#741 notice TS rows array")
    };
    let row = rows
        .iter()
        .find(|r| r.get("name") == Some(&name.into()))
        .expect("#741 notice TS row");
    let f = Fixture::new(
        if name == "start_failure" {
            "turn_started"
        } else {
            ""
        },
        1,
    );
    let (_resource, port) = queued();
    let mut recording = Recording::new(
        f.runner().with_interaction_events(Some(port.clone())),
        Arc::new(()),
    );
    if name == "start_pending" || name == "start_drop" {
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        *f.store.gate.lock().expect("#741 notice gate lock") = Some((entered, wait));
        recording
            .begin(RecordingAction::Start)
            .expect("#741 notice begin");
        let mut pending = Box::pin(recording.settle());
        tokio::select! { biased;
            result = &mut pending => panic!("#741 notice premature start: {}", result.is_ok()),
            entered = tokio::time::timeout(Duration::from_secs(5), ready) => {
                entered.expect("#741 notice entered timeout").expect("#741 notice entered signal");
            }
        }
        assert_eq!(
            row.get("beforeRelease").and_then(|v| v.get("settled")),
            Some(&JsValue::Bool(false)),
            "#741 notice TS pending"
        );
        assert!(
            port.drain().expect("#741 notice pending drain").is_empty(),
            "#741 notice no early warning"
        );
        drop(pending); // cancel the waiter, not the owned transition
        if name == "start_drop" {
            drop(recording);
            release.send(()).expect("#741 notice release drop");
            tokio::time::timeout(Duration::from_secs(5), async {
                while f
                    .registry
                    .resolve(None)
                    .expect("#741 notice runtime")
                    .lease_count()
                    != 0
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("#741 notice worker finished");
            assert_eq!(
                port.host_started()
                    .expect("#741 notice retained after drop")
                    .len(),
                1,
                "#741 notice drop did not consume"
            );
            return;
        }
        // A second cancelled waiter still cannot complete a pending predecessor.
        let mut pending = Box::pin(recording.settle());
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(
                pending.as_mut().poll(cx).is_pending()
            ))
            .await,
            "#741 notice predecessor remains pending"
        );
        drop(pending);
        release.send(()).expect("#741 notice release pending");
    } else {
        recording
            .begin(RecordingAction::Start)
            .expect("#741 notice begin");
    }
    let result = tokio::time::timeout(Duration::from_secs(5), recording.settle())
        .await
        .expect("#741 notice settle timeout");
    if name == "start_failure" {
        assert!(result.is_err(), "#741 notice start write rejected");
        assert_eq!(
            row.get("event"),
            Some(&JsValue::Null),
            "#741 notice TS failed start"
        );
        assert_eq!(
            port.host_started()
                .expect("#741 notice failure retains")
                .len(),
            1,
            "#741 notice failed write did not consume"
        );
    } else {
        assert_eq!(
            row.get("event"),
            Some(&"turn_started".into()),
            "#741 notice TS first event"
        );
        assert_start(&result.expect("#741 notice successful start"));
        assert!(
            port.host_started().expect("#741 notice repeat").is_empty(),
            "#741 notice exactly once"
        );
    }
    assert!(
        port.drain()
            .expect("#741 notice no duplicate drain")
            .is_empty(),
        "#741 notice no buffered duplicate"
    );
}
macro_rules! first_case {
    ($name:ident) => {
        #[tokio::test]
        async fn $name() {
            first(stringify!($name)).await;
        }
    };
}
first_case!(start_success);
first_case!(start_failure);
first_case!(start_pending);
first_case!(start_drop);

async fn tail(name: &str) {
    let f = Fixture::new(
        if name == "accepted_failure" {
            "accepted_input"
        } else {
            ""
        },
        1,
    );
    let (_resource, port) = queued();
    let (runner, events) = f
        .runner()
        .with_interaction_events(Some(port.clone()))
        .start()
        .await
        .expect("#741 notice tail start");
    assert_start(&events);
    let (mut runner, mut events) = runner.accept(input()).await.expect("#741 notice accept");
    if name == "loop_failure" {
        let (next, failed) = runner
            .loop_failed(object([
                ("code", "agent_invalid_state".into()),
                ("message", "loop failed".into()),
            ]))
            .await
            .expect("#741 notice loop failure");
        runner = next;
        events.extend(failed);
    }
    if !runner.is_complete() {
        let result = object([
            (
                "type",
                if name == "cancel" {
                    "aborted"
                } else if name == "loop_failure" {
                    "error"
                } else {
                    "success"
                }
                .into(),
            ),
            ("sessionId", "s".into()),
            ("turnId", "t".into()),
            ("stopReason", "completed".into()),
            ("usage", object([])),
            ("permissionDenials", JsValue::Array(vec![])),
            ("turns", JsValue::Number(1.0)),
            ("startedAt", "2026-10-26T00:00:00.000Z".into()),
            ("completedAt", "2026-10-26T00:00:00.000Z".into()),
        ]);
        events.extend(
            runner
                .complete(result)
                .await
                .expect("#741 notice complete")
                .1,
        );
    }
    assert_eq!(
        events.last().and_then(|e| e.get("type")),
        Some(&"turn_completed".into()),
        "#741 notice terminal"
    );
    assert!(
        events
            .iter()
            .all(|e| !["turn_started".into(), "warning".into()]
                .contains(e.get("type").expect("#741 notice event type"))),
        "#741 notice tail no replay"
    );
    assert!(
        port.host_started()
            .expect("#741 notice tail notify")
            .is_empty(),
        "#741 notice tail consumed once"
    );
    assert!(
        port.drain().expect("#741 notice tail drain").is_empty(),
        "#741 notice tail buffer empty"
    );
}
macro_rules! tail_case {
    ($name:ident) => {
        #[tokio::test]
        async fn $name() {
            tail(stringify!($name)).await;
        }
    };
}
tail_case!(accepted_failure);
tail_case!(loop_failure);
tail_case!(complete);
tail_case!(cancel);

#[test]
fn stale() {
    let (resource, old) = queued();
    let current = port(&resource);
    current.queue_notices(vec![notice()]);
    assert!(old.host_started().is_err(), "#741 notice stale rejected");
    assert_eq!(
        current.host_started().expect("#741 notice current").len(),
        1,
        "#741 notice current not stolen"
    );
}
#[test]
fn closed() {
    let (resource, port) = queued();
    port.close();
    assert!(port.host_started().is_err(), "#741 notice closed rejected");
    let current = self::port(&resource);
    resource.close();
    assert!(
        current.host_started().is_err(),
        "#741 notice generation rejected"
    );
}
#[test]
fn buffered_events_are_not_stolen() {
    let (_resource, port) = queued();
    let unrelated = object([("type", "agent_status".into()), ("sessionId", "s".into())]);
    port.emit(
        unrelated
            .as_object()
            .expect("#741 notice host event object")
            .clone(),
    )
    .expect("#741 notice host emit");
    assert_eq!(
        port.host_started().expect("#741 notice host start").len(),
        1,
        "#741 notice only notices"
    );
    assert_eq!(
        port.drain().expect("#741 notice unrelated drain"),
        vec![
            unrelated
                .as_object()
                .expect("#741 notice expected object")
                .clone()
        ],
        "#741 notice unrelated buffer preserved"
    );
}
#[test]
#[ignore = "requires fixed TS baseline and Node v22.22.0"]
fn regenerates_identically() {
    assert!(
        std::panic::catch_unwind(|| {
            // #996: notices-regeneration corrupts the loaded committed
            // oracle (baseline commit -> zeros), never the fixture on disk.
            baseline::regenerates_identically_with(
                "turn-start-notices",
                crate::mutants::on("notices-regeneration"),
            )
        })
        .is_ok(),
        "#741 notice fixture regeneration"
    );
}

#[test]
fn both_start_paths_consume_once() {
    for host_first in [false, true] {
        let (_resource, port) = queued();
        let mut events = vec![];
        if host_first {
            events.extend(port.host_started().expect("#741 notice first host"));
        }
        let start = object([
            ("type", "turn_started".into()),
            ("sessionId", "s".into()),
            ("turnId", "t".into()),
        ]);
        port.emit(start.as_object().expect("#741 notice start object").clone())
            .expect("#741 notice legacy start");
        events.extend(port.drain().expect("#741 notice legacy drain"));
        events.extend(port.host_started().expect("#741 notice last host"));
        assert_eq!(
            events
                .iter()
                .filter(|e| e.get("type") == Some(&"warning".into()))
                .count(),
            1,
            "#741 notice both paths once"
        );
    }
}

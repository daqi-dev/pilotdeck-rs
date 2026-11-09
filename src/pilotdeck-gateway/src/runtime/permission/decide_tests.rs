use super::*;
use crate::runtime::interaction::{InteractionBinding, PendingEntry};
use pilotdeck_protocol::JsString;
use std::sync::{
    Arc, Barrier, Mutex,
    atomic::{AtomicUsize, Ordering},
};

fn input(binding: JsValue) -> JsValue {
    object([
        ("sessionKey", "s".into()),
        ("requestId", "p".into()),
        ("decision", object([("raw", JsValue::Bool(true))])),
        ("remember", JsValue::Number(3.0)),
        ("reason", JsValue::Null),
        ("interactionBinding", binding),
    ])
}
fn delivered(params: &JsValue, owner: &GatewayInteractionCoordinator) -> bool {
    handle("permission_decide", params, owner)
        .unwrap()
        .unwrap()
        .get("delivered")
        .unwrap()
        .as_bool()
        .unwrap()
}

#[tokio::test]
async fn permission_decide_consumes_once_wakes_waiter_and_preserves_raw_decision() {
    let owner = GatewayInteractionCoordinator::default();
    let binding = InteractionBinding {
        connection_id: "current".into(),
        generation: 2,
    };
    owner
        .reconnect_for_turn(&"s".into(), binding.clone())
        .unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = Mutex::new(Some(tx));
    let reg = owner
        .permission_bus()
        .register(
            &"s".into(),
            PendingEntry {
                request_id: "p".into(),
                tool_call_id: "call".into(),
                tool_name: "bash".into(),
                resolve: Arc::new(move |value| {
                    tx.lock().unwrap().take().unwrap().send(value).unwrap()
                }),
                reject: Arc::new(|_| panic!("response is not rejection")),
            },
            None,
        )
        .unwrap();
    assert!(!delivered(
        &input(object([
            ("connectionId", "stale".into()),
            ("generation", JsValue::Number(1.0))
        ])),
        &owner
    ));
    assert!(reg.active());
    assert!(delivered(&input(binding.to_value()), &owner));
    assert!(!reg.active());
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), rx)
            .await
            .unwrap()
            .unwrap(),
        object([
            ("requestId", "p".into()),
            ("decision", object([("raw", JsValue::Bool(true))])),
            ("remember", JsValue::Number(3.0)),
            ("reason", JsValue::Null),
        ])
    );
    assert!(!delivered(&input(binding.to_value()), &owner));
    assert_eq!(owner.permission_bus().pending_count(&"s".into()), 0);
}

#[test]
fn permission_decide_sdk_resource_binding_matches_ts_falsy_and_optional_fields() {
    for bound in [false, true] {
        for binding in [
            JsValue::Undefined,
            JsValue::Null,
            JsValue::Bool(false),
            JsValue::Number(0.0),
            "".into(),
            object([]),
            JsValue::Number(7.0),
        ] {
            let owner = GatewayInteractionCoordinator::default();
            if bound {
                owner
                    .reconnect_for_turn(
                        &"s".into(),
                        InteractionBinding {
                            connection_id: "owner".into(),
                            generation: 1,
                        },
                    )
                    .unwrap();
            }
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = calls.clone();
            let reg = owner
                .permission_bus()
                .register(
                    &"s".into(),
                    PendingEntry {
                        request_id: "p".into(),
                        tool_call_id: "c".into(),
                        tool_name: "bash".into(),
                        resolve: Arc::new(move |_| {
                            observed.fetch_add(1, Ordering::SeqCst);
                        }),
                        reject: Arc::new(|_| {}),
                    },
                    None,
                )
                .unwrap();
            let truthy = super::super::truthy(&binding);
            let expected = !bound || !truthy;
            assert_eq!(delivered(&input(binding), &owner), expected);
            assert_eq!(calls.load(Ordering::SeqCst), usize::from(expected));
            assert_eq!(reg.active(), !expected);
            reg.dispose();
        }
    }
}

#[test]
fn permission_decide_missing_ids_do_not_consume_and_missing_decision_is_not_rejected() {
    let owner = GatewayInteractionCoordinator::default();
    for params in [
        object([]),
        object([("sessionKey", "s".into())]),
        object([
            ("sessionKey", JsValue::Number(7.0)),
            ("requestId", "p".into()),
        ]),
    ] {
        assert!(!delivered(&params, &owner));
    }
    for params in [JsValue::Null, JsValue::Undefined] {
        let error = handle("permission_decide", &params, &owner)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, "gateway_request_failed");
    }
    let answer = Arc::new(Mutex::new(None));
    let received = answer.clone();
    owner
        .permission_bus()
        .register(
            &"s".into(),
            PendingEntry {
                request_id: "p".into(),
                tool_call_id: "c".into(),
                tool_name: "bash".into(),
                resolve: Arc::new(move |value| *received.lock().unwrap() = Some(value)),
                reject: Arc::new(|_| {}),
            },
            None,
        )
        .unwrap();
    assert!(delivered(
        &object([("sessionKey", "s".into()), ("requestId", "p".into())]),
        &owner
    ));
    assert_eq!(
        answer.lock().unwrap().as_ref().unwrap().get("decision"),
        Some(&JsValue::Undefined)
    );
}

#[test]
fn permission_decide_racing_timeout_disposal_has_one_winner() {
    for _ in 0..32 {
        let owner = Arc::new(GatewayInteractionCoordinator::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let reg = owner
            .permission_bus()
            .register(
                &JsString::from("s"),
                PendingEntry {
                    request_id: "p".into(),
                    tool_call_id: "c".into(),
                    tool_name: "bash".into(),
                    resolve: Arc::new(move |_| {
                        observed.fetch_add(1, Ordering::SeqCst);
                    }),
                    reject: Arc::new(|_| {}),
                },
                None,
            )
            .unwrap();
        let barrier = Barrier::new(2);
        let did_deliver = std::thread::scope(|scope| {
            let response = scope.spawn(|| {
                barrier.wait();
                delivered(&input(JsValue::Undefined), &owner)
            });
            barrier.wait();
            reg.dispose();
            response.join().unwrap()
        });
        assert!(!reg.active());
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(did_deliver));
        assert!(!delivered(&input(JsValue::Undefined), &owner));
        assert_eq!(owner.permission_bus().pending_count(&"s".into()), 0);
    }
}

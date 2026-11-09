use super::*;
use crate::runtime::interaction::{InteractionBinding, InteractionOptions, PendingEntry};
use std::sync::{Arc, Mutex};

fn params(owner: &str, binding: JsValue) -> JsValue {
    object([
        ("sessionKey", owner.into()),
        ("requestId", "q".into()),
        (
            "answer",
            object([("values", JsValue::Array(vec![JsValue::Null, "原样".into()]))]),
        ),
        ("interactionBinding", binding),
    ])
}
fn delivered(params: &JsValue, owner: &GatewayInteractionCoordinator) -> bool {
    handle("elicitation_respond", params, owner)
        .unwrap()
        .unwrap()
        == object([("delivered", JsValue::Bool(true))])
}

#[tokio::test]
async fn elicitation_respond_consumes_once_wakes_waiter_then_observes_delivery() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let observed = order.clone();
    let owner = GatewayInteractionCoordinator::new(InteractionOptions {
        on_elicitation_delivered: Some(Arc::new(move |session, request| {
            assert_eq!((session, request), (&"s".into(), &"q".into()));
            observed.lock().unwrap().push("delivered");
        })),
        ..Default::default()
    });
    let binding = InteractionBinding {
        connection_id: "current".into(),
        generation: 2,
    };
    owner
        .reconnect_for_turn(&"s".into(), binding.clone())
        .unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = Mutex::new(Some(tx));
    let bus = owner.elicitation_bus().clone();
    let observed = order.clone();
    let reg = owner
        .elicitation_bus()
        .register(
            &"s".into(),
            PendingEntry {
                request_id: "q".into(),
                tool_call_id: "c".into(),
                tool_name: "askUser".into(),
                resolve: Arc::new(move |answer| {
                    assert_eq!(bus.pending_count(&"s".into()), 0);
                    observed.lock().unwrap().push("resolve");
                    tx.lock().unwrap().take().unwrap().send(answer).unwrap();
                }),
                reject: Arc::new(|_| panic!("not a rejection")),
            },
            None,
        )
        .unwrap();
    assert!(!delivered(&params("other", binding.to_value()), &owner));
    assert!(!delivered(
        &params(
            "s",
            object([
                ("connectionId", "old".into()),
                ("generation", JsValue::Number(1.0))
            ])
        ),
        &owner
    ));
    assert!(reg.active());
    assert!(order.lock().unwrap().is_empty());
    let input = params("s", binding.to_value());
    assert!(delivered(&input, &owner));
    assert!(!reg.active());
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), rx)
            .await
            .unwrap()
            .unwrap(),
        input.get("answer").unwrap().clone()
    );
    assert_eq!(*order.lock().unwrap(), vec!["resolve", "delivered"]);
    assert!(!delivered(&input, &owner));
    assert_eq!(order.lock().unwrap().len(), 2);
    assert!(owner.reconnect_port().snapshot(&"s".into()).is_empty());
}

#[test]
fn elicitation_respond_missing_fields_and_raw_undefined_match_ts() {
    let owner = GatewayInteractionCoordinator::default();
    assert!(handle("unknown", &JsValue::Null, &owner).is_none());
    for value in [JsValue::Undefined, JsValue::Null] {
        let error = handle("elicitation_respond", &value, &owner)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, "gateway_request_failed");
    }
    let answers = Arc::new(Mutex::new(Vec::new()));
    let observed = answers.clone();
    let reg = owner
        .elicitation_bus()
        .register(
            &"s".into(),
            PendingEntry {
                request_id: "q".into(),
                tool_call_id: "c".into(),
                tool_name: "askUser".into(),
                resolve: Arc::new(move |value| observed.lock().unwrap().push(value)),
                reject: Arc::new(|_| {}),
            },
            None,
        )
        .unwrap();
    for input in [
        object([]),
        JsValue::Bool(true),
        JsValue::Number(7.0),
        "text".into(),
        object([("sessionKey", "s".into())]),
        object([
            ("sessionKey", JsValue::Number(3.0)),
            ("requestId", "q".into()),
        ]),
    ] {
        assert!(!delivered(&input, &owner));
        assert!(reg.active());
    }
    assert!(delivered(
        &object([("sessionKey", "s".into()), ("requestId", "q".into())]),
        &owner
    ));
    assert_eq!(*answers.lock().unwrap(), vec![JsValue::Undefined]);
}

#[test]
fn elicitation_respond_after_disposal_does_not_wake_or_observe() {
    let owner = GatewayInteractionCoordinator::new(InteractionOptions {
        on_elicitation_delivered: Some(Arc::new(|_, _| panic!("disposed response not delivered"))),
        ..Default::default()
    });
    let reg = owner
        .elicitation_bus()
        .register(
            &"s".into(),
            PendingEntry {
                request_id: "q".into(),
                tool_call_id: "c".into(),
                tool_name: "askUser".into(),
                resolve: Arc::new(|_| panic!("disposed response not resolved")),
                reject: Arc::new(|_| {}),
            },
            None,
        )
        .unwrap();
    reg.dispose();
    assert!(!delivered(&params("s", JsValue::Undefined), &owner));
    assert!(!reg.active());
    assert_eq!(owner.elicitation_bus().pending_count(&"s".into()), 0);
}

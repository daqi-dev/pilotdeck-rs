use super::*;
use crate::test_baseline as baseline;
fn field<'a>(v: &'a JsValue, key: &str) -> &'a JsValue {
    v.get(key).unwrap()
}
fn text(v: &JsValue) -> String {
    v.as_string().unwrap().to_utf8().unwrap()
}
fn array(v: &JsValue) -> &[JsValue] {
    if let JsValue::Array(items) = v {
        items
    } else {
        panic!("expected array")
    }
}
fn index(v: &JsValue) -> usize {
    v.as_number().unwrap() as usize
}
fn number(v: &JsValue) -> f64 {
    v.as_number().unwrap_or_else(|| text(v).parse().unwrap())
}
fn entry() -> PendingEntry {
    PendingEntry {
        request_id: "r".into(),
        tool_call_id: "t".into(),
        tool_name: "tool".into(),
        resolve: Arc::new(|_| panic!("conversion must not resolve")),
        reject: Arc::new(|_| panic!("conversion must not reject")),
    }
}
fn observe(v: JsValue) -> JsValue {
    match &v {
        JsValue::Number(n) if *n == 0.0 && n.is_sign_negative() => {
            object([("$number", "-0".into())])
        }
        JsValue::Array(items) => JsValue::Array(items.iter().cloned().map(observe).collect()),
        JsValue::Object(fields) => {
            let mut o = JsObject::new();
            for (k, v) in fields.entries() {
                o.insert(k.clone(), observe(v.clone()));
            }
            JsValue::Object(o)
        }
        _ => v,
    }
}
fn outcome<const P: bool>(bus: &bus::PendingBus<P>, error: Option<ReplayValueError>) -> JsValue {
    object([
        (
            "error",
            error.map_or(JsValue::Null, |e| {
                object([
                    ("name", e.name.into()),
                    ("message", JsValue::String(e.message)),
                ])
            }),
        ),
        (
            "snapshot",
            observe(JsValue::Array(bus.snapshot(&"s".into()))),
        ),
        (
            "count",
            JsValue::Number(bus.pending_count(&"s".into()) as f64),
        ),
    ])
}
fn run<const P: bool>(case: &JsValue) {
    let nodes: Vec<_> = array(field(case, "nodes"))
        .iter()
        .map(|n| match text(field(n, "tag")).as_str() {
            "date" => ProducerValue::Date(number(field(n, "value"))),
            "bigint" => ProducerValue::BigInt(text(field(n, "value"))),
            "function" => ProducerValue::Function,
            "symbol" => ProducerValue::Symbol,
            "undefined" => ProducerValue::Value(JsValue::Undefined),
            "number" => ProducerValue::Value(JsValue::Number(number(field(n, "value")))),
            "json" => ProducerValue::Value(field(n, "value").clone()),
            "array" => ProducerValue::Array(array(field(n, "items")).iter().map(index).collect()),
            "object" | "map" | "set" | "class" => ProducerValue::Object(
                array(field(n, "fields"))
                    .iter()
                    .map(|f| {
                        let f = array(f);
                        (f[0].as_string().unwrap().clone(), index(&f[1]))
                    })
                    .collect(),
            ),
            tag => panic!("unknown node {tag}"),
        })
        .collect();
    let bus = bus::PendingBus::<P>::default();
    let error = bus
        .register_producer(&"s".into(), entry(), &nodes, index(field(case, "root")))
        .err();
    assert_eq!(
        outcome(&bus, error),
        *field(case, "expected"),
        "{}",
        text(field(case, "name"))
    );
}
fn order<const P: bool>(case: &JsValue) {
    let bus = bus::PendingBus::<P>::default();
    let state = text(field(case, "state"));
    if state == "duplicate" {
        bus.register(&"s".into(), entry(), None).unwrap();
    }
    if state == "shared-duplicate" {
        bus::PendingBus::<P>::new(bus.reconnect_port().clone())
            .register(&"s".into(), entry(), None)
            .unwrap();
    }
    let owner = if state == "empty-owner" { "" } else { "s" };
    let mut pending = entry();
    if state == "empty-request" {
        pending.request_id = "".into();
    }
    let error = bus
        .register_producer(&owner.into(), pending, &[ProducerValue::Date(f64::NAN)], 0)
        .err();
    if state == "retry-after-error" {
        bus.register_producer(&"s".into(), entry(), &[ProducerValue::Date(0.0)], 0)
            .unwrap();
    }
    let expected = object([
        ("error", field(case, "error").clone()),
        ("snapshot", field(case, "snapshot").clone()),
        ("count", field(case, "count").clone()),
    ]);
    assert_eq!(outcome(&bus, error), expected, "{state}");
}
fn lifecycle<const P: bool>(case: &JsValue) {
    let bus = bus::PendingBus::<P>::default();
    let owner = "s".into();
    let binding = InteractionBinding {
        connection_id: "c".into(),
        generation: 1,
    };
    let cleanup = text(field(case, "cleanup"));
    let old = cleanup.starts_with("stale-").then(|| {
        let mut pending = entry();
        pending.reject = Arc::new(|_| {});
        let old = bus.register(&owner, pending, None).unwrap();
        if cleanup == "stale-port-dispose" {
            bus.reconnect_port().dispose();
        } else {
            bus.reconnect_port().dispose_owner(&owner);
        }
        old
    });
    let mut pending = entry();
    if old.is_some() {
        pending.request_id = "new".into();
    }
    let first = bus.reconnect(&owner, binding.clone(), None).unwrap();
    let error = bus
        .register_producer(&owner, pending, &[ProducerValue::Date(f64::NAN)], 0)
        .err()
        .unwrap();
    match cleanup.as_str() {
        "stale-dispose" | "stale-port-dispose" => old.unwrap().dispose(),
        "stale-consume" => {
            assert!(bus.consume(&owner, &"r".into(), None).is_some());
        }
        "stale-reject" => bus.reject_session(&owner, &"cleanup".into()),
        "none" => (),
        "reject" => bus.reject_session(&owner, &"cleanup".into()),
        "bus-dispose" => bus.dispose(None),
        "owner-dispose" => bus.reconnect_port().dispose_owner(&owner),
        "port-dispose" => bus.reconnect_port().dispose(),
        cleanup => {
            let registration = bus
                .register_producer(&owner, entry(), &[ProducerValue::Date(0.0)], 0)
                .unwrap();
            if cleanup == "register-dispose" {
                registration.dispose();
            } else {
                assert!(bus.consume(&owner, &"r".into(), None).is_some());
            }
        }
    }
    let disconnected = bus.disconnect(&owner, &binding).unwrap();
    let next = bus.reconnect(&owner, binding.clone(), None).unwrap();
    let disconnected_again = bus.disconnect(&owner, &binding).unwrap();
    let next_again = bus.reconnect(&owner, binding, None).unwrap();
    assert_eq!(
        object([
            ("first", first),
            (
                "error",
                object([
                    ("name", error.name.into()),
                    ("message", JsValue::String(error.message))
                ])
            ),
            ("disconnected", JsValue::Bool(disconnected)),
            ("next", next),
            ("disconnectedAgain", JsValue::Bool(disconnected_again)),
            ("nextAgain", next_again)
        ]),
        *field(case, "expected"),
        "{}",
        text(field(case, "cleanup"))
    );
}
#[test]
fn interaction_producer_real_ts_matrix() {
    let oracle = crate::test_baseline::oracle(include_str!(
        "../../../../tests/fixtures/interaction-values/oracle.json"
    ));
    for case in array(field(&oracle, "cases")) {
        if text(field(case, "name")).starts_with("permission/") {
            run::<true>(case)
        } else {
            run::<false>(case)
        }
    }
    for case in array(field(&oracle, "lifecycle")) {
        if text(field(case, "kind")) == "permission" {
            lifecycle::<true>(case)
        } else {
            lifecycle::<false>(case)
        }
    }
    for case in array(field(&oracle, "ordering")) {
        if text(field(case, "kind")) == "permission" {
            order::<true>(case)
        } else {
            order::<false>(case)
        }
    }
}
#[test]
fn interaction_producer_malformed_graph_is_explicit() {
    for nodes in [vec![], vec![ProducerValue::Array(vec![1])]] {
        let error = to_interaction_replay_value(&nodes, 0).unwrap_err();
        assert_eq!(
            error,
            ReplayValueError::new("TypeError", "Invalid interaction producer reference")
        );
    }
    for bad in ["", "+1", "01", "-0", "-01", "x"] {
        assert_eq!(
            to_interaction_replay_value(&[ProducerValue::BigInt(bad.into())], 0).unwrap_err(),
            ReplayValueError::new("TypeError", "Invalid interaction producer BigInt")
        );
    }
}
#[test]
#[ignore = "requires Node 22 and PDRS_BASELINE; generates the real TS oracle twice"]
fn interaction_values_regenerate_twice_identically() {
    baseline::regenerates_identically("interaction-values");
    baseline::regenerates_identically("interaction-values");
}

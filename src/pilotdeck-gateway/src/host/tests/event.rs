use super::*;
use crate::host::execution::module_response;
use crate::host::modules::ModuleRouter;
use crate::host::modules::event::{EventSink, Handler, TurnIdentity};
use std::sync::{Arc, Mutex, OnceLock};

const SOURCE_PATHS: [&str; 3] = [
    "src/agent/modules/transport/agentLoopSidecarClient.ts",
    "src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts",
    "src/agent/protocol/events.ts",
];
const ORACLE_JSON: &str = include_str!("../../../tests/fixtures/host-event/oracle.json");

fn oracle() -> &'static JsValue {
    static ORACLE: OnceLock<JsValue> = OnceLock::new();
    ORACLE.get_or_init(|| crate::test_baseline::oracle(ORACLE_JSON))
}
fn array<'a>(value: &'a JsValue, key: &str) -> &'a [JsValue] {
    let JsValue::Array(items) = value.get(key).unwrap() else {
        panic!("{key}")
    };
    items
}
fn text(value: &JsValue) -> String {
    value
        .as_string()
        .unwrap_or_else(|| panic!("expected a string, got {value:?}"))
        .to_utf8()
        .unwrap()
}

/// Rebuild JS property presence from the fixture's `undefined` marker.
fn materialize(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(materialize).collect()),
        JsValue::Object(object) => {
            if object.len() == 1 && object.get("__pdrsUndefined") == Some(&JsValue::Bool(true)) {
                return JsValue::Undefined;
            }
            let mut result = JsObject::new();
            for (key, value) in object.entries() {
                result.insert(key.clone(), materialize(value));
            }
            JsValue::Object(result)
        }
        value => value.clone(),
    }
}
fn drained(handler: &Handler) -> JsValue {
    JsValue::Array(handler.drain().into_iter().map(JsValue::Object).collect())
}

#[derive(Default)]
struct FakeSink {
    emitted: Mutex<Vec<JsObject>>,
    buffer: Mutex<Vec<JsObject>>,
    attempted: Mutex<usize>,
    /// `(message, fields)` for a port whose emitter throws. The fields are fed
    /// to `ModuleError::from_js_fields` exactly like a host port that projects
    /// a thrown JS error.
    failure: Option<(JsString, JsValue)>,
}
impl FakeSink {
    fn throwing(spec: &JsValue) -> Self {
        let kind = spec
            .get("kind")
            .and_then(JsValue::as_string)
            .expect("sink kind")
            .to_utf8()
            .unwrap();
        let (message, fields) = match kind.as_str() {
            "error" => (
                spec.get("message")
                    .and_then(JsValue::as_string)
                    .expect("sink message")
                    .clone(),
                spec.get("fields").cloned().unwrap_or(JsValue::Undefined),
            ),
            "string" => (
                spec.get("value")
                    .and_then(JsValue::as_string)
                    .expect("sink value")
                    .clone(),
                JsValue::Undefined,
            ),
            other => panic!("unknown sink kind {other}"),
        };
        Self {
            failure: Some((message, fields)),
            ..Self::default()
        }
    }
}
impl EventSink for FakeSink {
    fn emit(&self, event: JsObject) -> Result<(), HostError> {
        *self.attempted.lock().unwrap() += 1;
        if let Some((message, fields)) = &self.failure {
            return Err(ModuleError::from_js_fields(message.clone(), fields).into());
        }
        self.emitted.lock().unwrap().push(event.clone());
        self.buffer.lock().unwrap().push(event);
        Ok(())
    }
    fn drain(&self) -> Vec<JsObject> {
        std::mem::take(&mut *self.buffer.lock().unwrap())
    }
}

#[tokio::test]
async fn real_ts_event_identity_and_drain() {
    let oracle = oracle();
    let cases = array(oracle, "cases");
    assert_eq!(cases.len(), 46);
    let turn = oracle.get("turn").unwrap();
    let identity = TurnIdentity::new(
        text(turn.get("sessionId").unwrap()),
        text(turn.get("turnId").unwrap()),
    );
    let mut calls = 0;
    for case in cases {
        let name = text(case.get("name").unwrap());
        let failure = case.get("sink");
        let sink = Arc::new(match failure {
            Some(spec) => FakeSink::throwing(spec),
            None => FakeSink::default(),
        });
        let handler = Handler::new(identity.clone(), sink.clone());
        for step in array(case, "steps") {
            calls += 1;
            let call = materialize(step.get("call").unwrap());
            let response = module_response(&call, handler.call(call.clone()).await);
            assert_eq!(
                &response,
                step.get("response").unwrap(),
                "{name} call={call:?}"
            );
            if let Some(expected) = step.get("drained") {
                assert_eq!(&drained(&handler), expected, "{name} drained");
            }
        }
        assert_eq!(
            &drained(&handler),
            case.get("drainAfter").unwrap(),
            "{name} drainAfter"
        );
        assert!(handler.drain().is_empty(), "{name} drains are destructive");
        let accepted = array(case, "steps")
            .iter()
            .filter(|step| {
                step.get("response")
                    .and_then(|response| response.get("ok"))
                    .and_then(JsValue::as_bool)
                    == Some(true)
            })
            .count();
        // A throwing sink receives every validated event and then rejects the
        // call, so its expected delivery count is the step count.
        let delivered = if failure.is_some() {
            array(case, "steps").len()
        } else {
            accepted
        };
        assert_eq!(
            *sink.attempted.lock().unwrap(),
            delivered,
            "{name} only validated events reach the sink"
        );
        assert_eq!(
            sink.emitted.lock().unwrap().len(),
            if failure.is_some() { 0 } else { accepted },
            "{name} throwing sinks never record events"
        );
        handler.dispose().await.unwrap();
    }
    assert_eq!(calls, 49);
}

/// `turnId` is optional by key presence: absent is accepted for session-level
/// events, while an explicitly `undefined` key still mismatches the turn.
#[tokio::test]
async fn turn_id_is_checked_by_property_presence() {
    let sink = Arc::new(FakeSink::default());
    let handler = Handler::new(TurnIdentity::new("session", "turn"), sink.clone());
    let absent = parse_json(r#"{"type":"session_started","sessionId":"session"}"#).unwrap();
    let present =
        parse_json(r#"{"type":"session_started","sessionId":"session","turnId":null}"#).unwrap();
    assert!(handler.call(event_call(absent.clone())).await.is_ok());
    assert!(handler.call(event_call(present.clone())).await.is_err());
    assert_eq!(sink.emitted.lock().unwrap().len(), 1);

    let mut event = JsObject::new();
    event.insert("type".into(), JsValue::String("session_started".into()));
    event.insert("sessionId".into(), JsValue::String("session".into()));
    event.insert("turnId".into(), JsValue::Undefined);
    assert!(
        handler
            .call(event_call(JsValue::Object(event)))
            .await
            .is_err()
    );
    assert_eq!(sink.emitted.lock().unwrap().len(), 1);
}

struct FailingSink;
impl EventSink for FailingSink {
    fn emit(&self, _event: JsObject) -> Result<(), HostError> {
        Err(HostError::new(HostErrorKind::Callback, "sink failed"))
    }
}

#[tokio::test]
async fn sink_failure_rejects_the_module_call_unchanged() {
    let handler = Handler::new(TurnIdentity::new("session", "turn"), Arc::new(FailingSink));
    let event =
        parse_json(r#"{"type":"turn_started","sessionId":"session","turnId":"turn"}"#).unwrap();
    let error = handler.call(event_call(event)).await.unwrap_err();
    assert_eq!(error.message, "sink failed");
    assert_eq!(error.kind, HostErrorKind::Callback);
    assert!(error.module_error.is_none());
    // The trait's default drain models a port installed without a buffer.
    assert!(handler.drain().is_empty());
}

#[tokio::test]
async fn module_router_reaches_the_event_handler() {
    let sink = Arc::new(FakeSink::default());
    let router = ModuleRouter {
        event: Handler::new(TurnIdentity::new("session", "turn"), sink.clone()),
        ..ModuleRouter::default()
    };
    let call = event_call(
        parse_json(r#"{"type":"turn_started","sessionId":"session","turnId":"turn"}"#).unwrap(),
    );
    let response = module_response(&call, router.module_call(call.clone()).await);
    assert_eq!(
        response.get("payload").unwrap().get("result").unwrap(),
        &JsValue::Null
    );
    assert_eq!(sink.emitted.lock().unwrap().len(), 1);
    router.dispose().await.unwrap();
}

#[tokio::test]
async fn missing_sink_fails_closed_after_wire_validation() {
    let handler = Handler::default();
    // An uninstalled module answers with the protocol's unregistered-module
    // error before any payload parsing, like the TS client's handler lookup
    // (agentLoopSidecarClient.ts:1155-1158).
    for call in [
        request(Some(parse_json(r#"{"operation":"x"}"#).unwrap())),
        request(None),
        // The TS client rejects a null payload before dispatch (client:1086);
        // an uninstalled module still reports the missing handler first.
        request(Some(JsValue::Null)),
        event_call(parse_json(r#"{"type":"x","sessionId":"session"}"#).unwrap()),
    ] {
        let response = module_response(&call, handler.call(call.clone()).await);
        assert_eq!(
            response.get("error").unwrap().get("message").unwrap(),
            &JsValue::from("Unsupported sidecar host module: event")
        );
    }
    assert!(handler.drain().is_empty());
}

fn request(payload: Option<JsValue>) -> JsValue {
    let mut call = JsObject::new();
    call.insert("messageId".into(), JsValue::from("call"));
    call.insert("requestId".into(), JsValue::from("request"));
    call.insert("module".into(), JsValue::from("event"));
    if let Some(payload) = payload {
        call.insert("payload".into(), payload);
    }
    JsValue::Object(call)
}

fn event_call(event: JsValue) -> JsValue {
    let mut payload = JsObject::new();
    payload.insert("operation".into(), JsValue::from("emit"));
    payload.insert("event".into(), event);
    request(Some(JsValue::Object(payload)))
}

#[test]
fn oracle_comes_from_the_fixed_baseline() {
    let oracle = oracle();
    let hashes = oracle.get("sourceSha256").unwrap();
    for source in SOURCE_PATHS {
        assert!(hashes.get(source).is_some(), "missing source {source}");
    }
    assert!(!ORACLE_JSON.contains(".generate-"));
}

/// Regenerates the oracle from the real TS dispatcher and compares bytes.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn event_fixtures_regenerate_identically_from_ts() {
    crate::test_baseline::regenerates_identically("host-event");
}

/// Records each event's `index`; the first emit is slow so a sink reached
/// from concurrently running callback tasks would see later events first.
#[derive(Default)]
struct OrderSink(Mutex<Vec<f64>>);
impl EventSink for OrderSink {
    fn emit(&self, event: JsObject) -> Result<(), HostError> {
        let index = match event.get("index") {
            Some(JsValue::Number(index)) => *index,
            other => panic!("probe index {other:?}"),
        };
        if index == 0.0 {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        self.0.lock().unwrap().push(index);
        Ok(())
    }
}

/// #289: 16 `event.emit` calls in flight over stdio reach the sink in the
/// sidecar's send order (TS emits before the handler's first await). With
/// emission inside the spawned callback tasks the slow first emit lets the
/// other workers overtake it (negative control).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_emits_reach_the_sink_in_arrival_order() {
    let (mut command, receipt) = peer("event-order");
    let client = HostClient::spawn(&mut command, None, protocol_limits())
        .await
        .unwrap();
    let sink = Arc::new(OrderSink::default());
    let router = Arc::new(ModuleRouter {
        event: Handler::new(TurnIdentity::new("session", "turn"), sink.clone()),
        ..ModuleRouter::default()
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        client.execute(super::request(), router),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(result, ExecuteResult::Terminal(_)));
    receipt.assert_pass("event-order");
    assert_eq!(
        *sink.0.lock().unwrap(),
        (0..16).map(f64::from).collect::<Vec<_>>()
    );
}

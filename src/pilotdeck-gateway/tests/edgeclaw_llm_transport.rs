//! #763: fixed real TS requests, results, telemetry and debug, plus D-2.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "support/llm_stub.rs"]
mod llm_stub;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame};
use hyper::{Request, Response};
use llm_stub::LlmStub;
use pilotdeck_gateway::runtime::{
    memory::edgeclaw::llm::transport::{LlmError, LlmMemoryExtractor, LlmRuntime, StructuredInput},
    model::http::{
        BodyFailure, HyperModelTransport, ModelHttpTransport, TransportFailure, TransportFuture,
    },
    network::node_timer_duration,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::task::{Context, Poll};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
struct NeverBody {
    dropped: Arc<AtomicUsize>,
}
impl Drop for NeverBody {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
impl Body for NeverBody {
    type Data = Bytes;
    type Error = BodyFailure;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BodyFailure>>> {
        Poll::Pending
    }
}
const ORACLE: &str = include_str!("fixtures/edgeclaw-llm/oracle.json");
fn js(value: Value) -> JsValue {
    parse_json(&value.to_string()).unwrap()
}
fn array(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(v) = value {
        v
    } else {
        panic!("array")
    }
}
fn utf8(value: &JsValue) -> String {
    value.as_string().unwrap().to_utf8().unwrap()
}
fn sorted(value: &JsValue, origin: &str) -> JsValue {
    match value {
        JsValue::String(s) => match s.to_utf8() {
            Ok(s) => JsValue::String(s.replace(origin, "$ORIGIN").into()),
            Err(_) => value.clone(),
        },
        JsValue::Array(v) => JsValue::Array(v.iter().map(|v| sorted(v, origin)).collect()),
        JsValue::Object(o) => {
            let mut entries = o.entries().to_vec();
            entries.sort_by(|a, b| a.0.units().cmp(b.0.units()));
            let mut out = JsObject::new();
            for (k, v) in entries {
                out.insert(k, sorted(&v, origin));
            }
            JsValue::Object(out)
        }
        _ => value.clone(),
    }
}
fn equal(actual: &JsValue, expected: &JsValue, origin: &str, name: &str) {
    assert_eq!(
        stringify_json(&sorted(actual, origin)).unwrap(),
        stringify_json(&sorted(expected, origin)).unwrap(),
        "{name}"
    );
}
#[derive(Default)]
struct Runtime {
    clock: Arc<AtomicU64>,
    sleeps: Mutex<Vec<u64>>,
    events: Mutex<Vec<JsValue>>,
    auth: String,
    auth_dropped: AtomicUsize,
    auth_entered: Notify,
    hold_sleep: bool,
    sleeping: Notify,
    release: Notify,
}
struct DropCount<'a>(&'a AtomicUsize);
impl Drop for DropCount<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl LlmRuntime for Runtime {
    fn env(&self, _: &str) -> Option<String> {
        None
    }
    fn resolve_auth<'a>(
        &'a self,
        _: &'a JsString,
        _: &'a JsValue,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JsValue>, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            if self.auth.is_empty() {
                return Ok(None);
            }
            self.events
                .lock()
                .unwrap()
                .push(js(json!({"auth":"called"})));
            let _drop = DropCount(&self.auth_dropped);
            self.auth_entered.notify_one();
            if self.auth == "pending" {
                self.release.notified().await;
            }
            if self.auth == "reject" {
                Err(LlmError::new("auth failed"))
            } else {
                Ok(Some(js(json!({"apiKey":"resolved-key"}))))
            }
        })
    }
    fn sleep(&self, ms: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            self.sleeps.lock().unwrap().push(ms);
            self.sleeping.notify_one();
            if self.hold_sleep {
                self.release.notified().await;
            }
            self.clock.fetch_add(ms, Ordering::SeqCst);
        })
    }
}
struct Transport {
    real: HyperModelTransport,
    steps: Vec<Value>,
    calls: AtomicUsize,
    deterministic: bool,
    headers: Arc<Notify>,
    dropped: Arc<AtomicUsize>,
    timeout_clock: Arc<AtomicU64>,
    timeout_ms: u64,
}
impl ModelHttpTransport for Transport {
    fn send(&self, request: Request<Full<Bytes>>) -> TransportFuture {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        let step = self.steps.get(index).or_else(|| self.steps.last()).unwrap();
        if step["kind"] == "error" {
            return TransportFuture::ready(Err(TransportFailure {
                message: step["message"].as_str().unwrap().into(),
                cause_code: None,
            }));
        }
        if self.deterministic {
            if step["kind"] == "headers-hang" {
                let dropped = self.dropped.clone();
                let clock = self.timeout_clock.clone();
                let ms = self.timeout_ms;
                return TransportFuture::io(async move {
                    let _drop = PendingDrop { dropped, clock, ms };
                    std::future::pending().await
                });
            }
            let response_body = if step["kind"] == "body-hang" {
                NeverBody {
                    dropped: self.dropped.clone(),
                }
                .boxed()
            } else {
                Full::new(Bytes::from(step["body"].as_str().unwrap_or("").to_owned()))
                    .map_err(|never| match never {})
                    .boxed()
            };
            let response = Response::builder()
                .status(step["status"].as_u64().unwrap_or(200) as u16)
                .body(response_body)
                .unwrap();
            if step["kind"] == "timer-response" {
                return TransportFuture::after(
                    Duration::from_millis(step["delay"].as_u64().unwrap()),
                    Ok(response),
                );
            }
            return TransportFuture::ready(Ok(response));
        }
        let headers = self.headers.clone();
        let pending = self.real.send(request);
        TransportFuture::io(async move {
            let result = pending.await;
            if result.is_ok() {
                headers.notify_one();
            }
            result
        })
    }
}
struct PendingDrop {
    dropped: Arc<AtomicUsize>,
    clock: Arc<AtomicU64>,
    ms: u64,
}
impl Drop for PendingDrop {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
        self.clock.fetch_add(self.ms, Ordering::SeqCst);
    }
}
fn input(spec: &Value) -> StructuredInput {
    let timeout = match &spec["timeout"] {
        Value::String(s) => match s.as_str() {
            "NaN" => Some(f64::NAN),
            "Infinity" => Some(f64::INFINITY),
            "-Infinity" => Some(f64::NEG_INFINITY),
            _ => None,
        },
        v => v.as_f64(),
    };
    StructuredInput {
        system_prompt: "system 中\n\"quoted\"".into(),
        user_prompt: "user 😀".into(),
        request_label: spec["label"].as_str().unwrap_or("recall fixture").into(),
        agent_id: None,
        timeout_ms: timeout,
    }
}
fn config(spec: &Value, origin: &str) -> JsValue {
    let model = spec["model"].as_str().unwrap_or("m");
    let mut provider = json!({"api":spec["api"],"baseUrl":spec["baseUrl"].as_str().map(str::to_owned).unwrap_or_else(||format!("{origin}{}",spec["path"].as_str().unwrap_or("/v1"))),"apiKey":spec["apiKey"].as_str().unwrap_or("fixture-key"),"headers":spec["headers"],"models":[{"id":model}]});
    if let Some(missing) = spec["missing"].as_str() {
        provider.as_object_mut().unwrap().remove(missing);
    }
    let mut encoded=json!({"agents":{"defaults":{"model":{"primary":format!("p/{model}")}}},"models":{"providers":{"p":provider}}}).to_string();
    for (field, needle) in [("apiKeyUnits", "fixture-key"), ("modelUnits", "m")] {
        if let Some(units) = spec[field].as_array() {
            let value =
                JsString::from_utf16(units.iter().map(|v| v.as_u64().unwrap() as u16).collect());
            encoded = encoded.replace(
                &format!("\"{needle}\""),
                &pilotdeck_protocol::quote_json_string(&value),
            );
            if field == "modelUnits" {
                let mut units: Vec<u16> = "p/".encode_utf16().collect();
                units.extend(value.units());
                encoded = encoded.replace(
                    "\"p/m\"",
                    &pilotdeck_protocol::quote_json_string(&JsString::from_utf16(units)),
                );
            }
        }
    }
    parse_json(&encoded).unwrap()
}
fn transport(spec: &Value, runtime: &Runtime, deterministic: bool) -> Arc<Transport> {
    let timeout = input(spec)
        .timeout_ms
        .filter(|v| v.is_finite())
        .unwrap_or(30000.0);
    Arc::new(Transport {
        real: HyperModelTransport::new(),
        steps: spec["steps"].as_array().unwrap().clone(),
        calls: AtomicUsize::new(0),
        deterministic,
        headers: Arc::new(Notify::new()),
        dropped: Arc::new(AtomicUsize::new(0)),
        timeout_clock: runtime.clock.clone(),
        timeout_ms: timeout as u64,
    })
}
fn result(value: Result<JsValue, LlmError>) -> JsValue {
    let mut out = JsObject::new();
    match value {
        Ok(v) => {
            out.insert("ok".into(), v);
        }
        Err(e) => {
            out.insert("error".into(), JsValue::String(e.message));
            out.insert("name".into(), e.name.into());
            if let Some(status) = e.status {
                out.insert("status".into(), JsValue::Number(status.into()));
            }
        }
    }
    JsValue::Object(out)
}
async fn call(
    extractor: &LlmMemoryExtractor,
    input: &StructuredInput,
    spec: &Value,
    debug: &mut Vec<JsValue>,
) -> JsValue {
    result(
        extractor
            .call_structured_json_with_debug(
                input,
                |raw| {
                    if spec["parse"] == "reject" {
                        Err(LlmError::new("parse rejected"))
                    } else if spec["parse"] == "success" {
                        Ok(parse_json(&raw.to_utf8().unwrap()).unwrap())
                    } else {
                        Ok(JsValue::String(raw.clone()))
                    }
                },
                |event| debug.push(event),
            )
            .await,
    )
}
fn spec_invalid_url(case: &JsValue) -> bool {
    case.get("spec").unwrap().get("baseUrl").is_some()
}
fn check(
    case: &JsValue,
    actual: JsValue,
    runtime: &Runtime,
    transport: &Transport,
    debug: Vec<JsValue>,
    origin: &str,
) {
    let expected = case.get("expected").unwrap();
    let name = utf8(case.get("spec").unwrap().get("name").unwrap());
    equal(
        &actual,
        expected.get("result").unwrap(),
        origin,
        &format!("result {name}"),
    );
    let auth: Vec<_> = array(expected.get("events").unwrap())
        .iter()
        .filter(|v| v.get("auth").is_some())
        .cloned()
        .collect();
    equal(
        &JsValue::Array(runtime.events.lock().unwrap().clone()),
        &JsValue::Array(auth),
        origin,
        &format!("auth {name}"),
    );
    equal(
        &JsValue::Array(debug),
        expected.get("debug").unwrap(),
        origin,
        &format!("debug {name}"),
    );
    equal(
        &js(json!(*runtime.sleeps.lock().unwrap())),
        expected.get("sleeps").unwrap(),
        origin,
        &format!("sleeps {name}"),
    );
    if spec_invalid_url(case) {
        assert_eq!(
            transport.calls.load(Ordering::SeqCst),
            0,
            "invalid URL must fail before transport"
        );
    } else {
        assert_eq!(
            transport.calls.load(Ordering::SeqCst) as f64,
            expected.get("calls").unwrap().as_number().unwrap(),
            "calls {name}"
        );
    }
}
#[tokio::test]
async fn wire_matrix() {
    let oracle = baseline::oracle(ORACLE);
    let mut count = 0;
    for case in array(oracle.get("cases").unwrap()) {
        let spec: Value =
            serde_json::from_str(&stringify_json(case.get("spec").unwrap()).unwrap().unwrap())
                .unwrap();
        if !matches!(spec["mode"].as_str(), None | Some("normal")) {
            continue;
        }
        let runtime = Arc::new(Runtime {
            auth: spec["auth"].as_str().unwrap_or("").into(),
            ..Runtime::default()
        });
        let server = LlmStub::start(
            spec["steps"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|s| s["kind"] != "error")
                .cloned()
                .collect(),
            runtime.clock.clone(),
        )
        .await;
        let transport = transport(&spec, &runtime, false);
        let extractor = LlmMemoryExtractor::new(
            config(&spec, &server.origin),
            transport.clone(),
            runtime.clone(),
        );
        let mut debug = Vec::new();
        let actual = tokio::time::timeout(
            Duration::from_secs(5),
            call(&extractor, &input(&spec), &spec, &mut debug),
        )
        .await
        .expect("bounded loopback case");
        check(case, actual, &runtime, &transport, debug, &server.origin);
        equal(
            &js(json!(*server.requests.lock().unwrap())),
            case.get("expected").unwrap().get("requests").unwrap(),
            &server.origin,
            spec["name"].as_str().unwrap(),
        );
        server.close().await;
        count += 1;
    }
    assert_eq!(count, 237);
}
#[tokio::test(start_paused = true)]
async fn timeout_matrix() {
    let oracle = baseline::oracle(ORACLE);
    let mut count = 0;
    for case in array(oracle.get("cases").unwrap()) {
        let spec: Value =
            serde_json::from_str(&stringify_json(case.get("spec").unwrap()).unwrap().unwrap())
                .unwrap();
        if spec["mode"] != "timeout" {
            continue;
        }
        let runtime = Arc::new(Runtime::default());
        let transport = transport(&spec, &runtime, true);
        let extractor = LlmMemoryExtractor::new(
            config(&spec, "http://127.0.0.1:12345"),
            transport.clone(),
            runtime.clone(),
        );
        let input = input(&spec);
        let start = tokio::time::Instant::now();
        let mut debug = Vec::new();
        let actual = call(&extractor, &input, &spec, &mut debug).await;
        check(
            case,
            actual,
            &runtime,
            &transport,
            debug,
            "http://127.0.0.1:12345",
        );
        let ms = input
            .timeout_ms
            .filter(|v| v.is_finite())
            .unwrap_or(30000.0);
        let attempts = if spec["api"] == "chat" { 2 } else { 1 };
        assert_eq!(start.elapsed(), node_timer_duration(ms) * attempts);
        assert_eq!(transport.dropped.load(Ordering::SeqCst), attempts as usize);
        count += 1;
    }
    assert_eq!(count, 20);
}
#[test]
#[ignore]
fn regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-llm");
}
async fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Option<F::Output> {
    std::future::poll_fn(|cx| {
        Poll::Ready(match future.as_mut().poll(cx) {
            Poll::Ready(value) => Some(value),
            Poll::Pending => None,
        })
    })
    .await
}
#[tokio::test(start_paused = true)]
async fn pending_and_cancellation_matrix() {
    let oracle = baseline::oracle(ORACLE);
    let mut count = 0;
    for case in array(oracle.get("cases").unwrap()) {
        let spec: Value =
            serde_json::from_str(&stringify_json(case.get("spec").unwrap()).unwrap().unwrap())
                .unwrap();
        if !matches!(
            spec["mode"].as_str(),
            Some("no-timeout" | "auth-pending" | "backoff-held")
        ) {
            continue;
        }
        let runtime = Arc::new(Runtime {
            auth: spec["auth"].as_str().unwrap_or("").into(),
            hold_sleep: true,
            ..Runtime::default()
        });
        let transport = transport(&spec, &runtime, true);
        let extractor = LlmMemoryExtractor::new(
            config(&spec, "http://127.0.0.1:12345"),
            transport.clone(),
            runtime.clone(),
        );
        let input = input(&spec);
        let mut debug = Vec::new();
        let mut future = Box::pin(call(&extractor, &input, &spec, &mut debug));
        assert!(poll_once(future.as_mut()).await.is_none());
        tokio::time::advance(Duration::from_secs(100)).await;
        assert!(
            poll_once(future.as_mut()).await.is_none(),
            "disabled deadline remains pending"
        );
        if spec["mode"] == "backoff-held" {
            // The held branch has only model_request so far; response follows release.
            runtime.release.notify_one();
            let actual = future.await;
            check(
                case,
                actual,
                &runtime,
                &transport,
                debug,
                "http://127.0.0.1:12345",
            );
        } else {
            drop(future);
            assert!(debug.is_empty());
            assert_eq!(
                transport.calls.load(Ordering::SeqCst) as f64,
                case.get("expected")
                    .unwrap()
                    .get("calls")
                    .unwrap()
                    .as_number()
                    .unwrap(),
                "#763 disabled timeout preserves transport call count"
            );
            assert_eq!(
                transport.dropped.load(Ordering::SeqCst)
                    + runtime.auth_dropped.load(Ordering::SeqCst),
                1,
                "drop cancels predecessor"
            );
        }
        count += 1;
    }
    assert_eq!(count, 6);
    // Dropping in backoff never sends the successor.
    let spec = json!({"api":"responses","steps":[{"status":503,"body":"bad"}]});
    let runtime = Arc::new(Runtime {
        hold_sleep: true,
        ..Runtime::default()
    });
    let transport = transport(&spec, &runtime, true);
    let extractor = LlmMemoryExtractor::new(
        config(&spec, "http://127.0.0.1:12345"),
        transport.clone(),
        runtime.clone(),
    );
    let input = input(&spec);
    let mut future = Box::pin(extractor.call_structured_json(&input));
    assert!(poll_once(future.as_mut()).await.is_none());
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    drop(future);
    runtime.release.notify_one();
    tokio::time::advance(Duration::from_secs(100)).await;
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test(start_paused = true)]
async fn body_deadline_matrix() {
    let oracle = baseline::oracle(ORACLE);
    let mut count = 0;
    for case in array(oracle.get("cases").unwrap()) {
        let spec: Value =
            serde_json::from_str(&stringify_json(case.get("spec").unwrap()).unwrap().unwrap())
                .unwrap();
        if spec["mode"] != "body-hang" {
            continue;
        }
        equal(
            case.get("expected").unwrap().get("result").unwrap(),
            &js(json!({"pending":true})),
            "unused",
            "TS pending D-2 witness",
        );
        let runtime = Arc::new(Runtime::default());
        let transport = transport(&spec, &runtime, true);
        let extractor = LlmMemoryExtractor::new(
            config(&spec, "http://127.0.0.1:12345"),
            transport.clone(),
            runtime.clone(),
        );
        let input = input(&spec);
        let start = tokio::time::Instant::now();
        let mut debug = Vec::new();
        let actual = call(&extractor, &input, &spec, &mut debug).await;
        equal(
            &actual,
            &js(json!({"error":"recall fixture request timed out after 7ms","name":"Error"})),
            "unused",
            "D-2 bounded body",
        );
        let calls = if spec["api"] == "chat" && spec["steps"][0]["status"] == 503 {
            2
        } else {
            1
        };
        assert_eq!(transport.calls.load(Ordering::SeqCst), calls);
        assert_eq!(transport.dropped.load(Ordering::SeqCst), calls);
        assert_eq!(start.elapsed(), Duration::from_millis(7 * calls as u64));
        assert_eq!(debug[0].get("timedOut"), Some(&JsValue::Bool(true)));
        assert!(runtime.sleeps.lock().unwrap().is_empty());
        count += 1;
    }
    assert_eq!(count, 4);
}
#[tokio::test]
async fn real_body_deadline_closes_loopback_connection() {
    let spec = json!({"api":"responses","steps":[{"kind":"body-hang","status":200}]});
    let runtime = Arc::new(Runtime::default());
    let server = LlmStub::start(
        spec["steps"].as_array().unwrap().clone(),
        runtime.clock.clone(),
    )
    .await;
    let transport = transport(&spec, &runtime, false);
    let extractor =
        LlmMemoryExtractor::new(config(&spec, &server.origin), transport.clone(), runtime);
    let input = input(&spec);
    let mut future = Box::pin(extractor.call_structured_json(&input));
    tokio::select! {
        result=&mut future=>panic!("body unexpectedly completed: {result:?}"),
        _=transport.headers.notified()=>{},
        _=tokio::time::sleep(Duration::from_secs(5))=>panic!("no loopback headers"),
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(30)).await;
    let error = future.await.unwrap_err();
    assert_eq!(
        error.message,
        JsString::from("recall fixture request timed out after 30000ms")
    );
    tokio::time::resume();
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.active.load(Ordering::SeqCst) > 0 {
            server.closed.notified().await;
        }
    })
    .await
    .expect("deadline must close the existing socket before server shutdown");
    server.close().await; // Awaiting the listener owner also awaits every connection.
}

#[tokio::test(start_paused = true)]
async fn timer_order_matrix() {
    let oracle = baseline::oracle(ORACLE);
    let mut count = 0;
    for case in array(oracle.get("cases").unwrap()) {
        let spec: Value =
            serde_json::from_str(&stringify_json(case.get("spec").unwrap()).unwrap().unwrap())
                .unwrap();
        if spec["mode"] != "timer-order" {
            continue;
        }
        let runtime = Arc::new(Runtime::default());
        let transport = transport(&spec, &runtime, true);
        let extractor = LlmMemoryExtractor::new(
            config(&spec, "http://127.0.0.1:12345"),
            transport.clone(),
            runtime.clone(),
        );
        let input = input(&spec);
        let mut debug = Vec::new();
        let mut future = Box::pin(call(&extractor, &input, &spec, &mut debug));
        assert!(poll_once(future.as_mut()).await.is_none());
        tokio::time::advance(Duration::from_millis(10)).await;
        let actual = future.await;
        check(
            case,
            actual,
            &runtime,
            &transport,
            debug,
            "http://127.0.0.1:12345",
        );
        count += 1;
    }
    assert_eq!(count, 3);
}

const FIX_R1: &str = include_str!("fixtures/edgeclaw-llm-fix-r1/oracle.json");
fn fix_cases() -> Vec<Value> {
    baseline::oracle(FIX_R1);
    serde_json::from_str::<Value>(FIX_R1).unwrap()["cases"]
        .as_array()
        .unwrap()
        .clone()
}
fn fix_input(spec: &Value) -> StructuredInput {
    StructuredInput {
        system_prompt: "s".into(),
        user_prompt: "u".into(),
        request_label: "probe".into(),
        ..input(spec)
    }
}
fn fix_spec(spec: &Value) -> Value {
    let mut spec = spec.clone();
    spec["steps"] = json!([match spec["mode"].as_str().unwrap() {
        "headers" => json!({"kind":"headers-hang"}),
        "body" => json!({"kind":"body-hang","status":spec["status"].as_u64().unwrap_or(200)}),
        _ => json!({"status":200,"body":if spec["api"] == "chat" {
            r#"{"choices":[{"message":{"content":"ok"}}]}"#
        } else { r#"{"output_text":"ok"}"# }}),
    }]);
    spec
}
fn fix_check(case: &Value, actual: JsValue, debug: Vec<JsValue>, runtime: &Runtime, prefix: &str) {
    let name = case["spec"]["name"].as_str().unwrap();
    equal(
        &actual,
        &js(case["expected"]["result"].clone()),
        "unused",
        &format!("{prefix} result {name}"),
    );
    equal(
        &JsValue::Array(debug),
        &js(case["expected"]["debug"].clone()),
        "unused",
        &format!("{prefix} debug {name}"),
    );
    assert_eq!(
        json!(*runtime.sleeps.lock().unwrap()),
        case["expected"]["sleeps"],
        "{prefix} sleeps {name}"
    );
}

#[tokio::test(start_paused = true)]
async fn headers_transport_deadline_matrix() {
    let mut count = 0;
    for case in fix_cases()
        .into_iter()
        .filter(|c| c["spec"]["mode"] == "headers" && c["spec"]["cancel"] != true)
    {
        let spec = fix_spec(&case["spec"]);
        let runtime = Arc::new(Runtime::default());
        let transport = transport(&spec, &runtime, true);
        let ms = spec["transportMs"].as_u64().unwrap();
        let mut extractor = LlmMemoryExtractor::new(
            config(&spec, "http://fixture.invalid"),
            transport.clone(),
            runtime.clone(),
        );
        // Exercise the actual default constructor as well as the CLI override.
        if ms != 300000 {
            extractor = extractor.with_transport_timeout(Duration::from_millis(ms));
        }
        let input = fix_input(&spec);
        let mut debug = Vec::new();
        let mut future = Box::pin(call(&extractor, &input, &spec, &mut debug));
        assert!(
            poll_once(future.as_mut()).await.is_none(),
            "headers initial pending"
        );
        tokio::time::advance(Duration::from_secs(100)).await;
        assert!(
            poll_once(future.as_mut()).await.is_none(),
            "headers before transport deadline"
        );
        let first_boundary = spec["timeout"]
            .as_u64()
            .filter(|value| *value > 0)
            .map_or(ms, |value| value.min(ms));
        tokio::time::advance(Duration::from_millis(first_boundary - 100001)).await;
        assert!(
            poll_once(future.as_mut()).await.is_none(),
            "headers pending before boundary"
        );
        assert_eq!(
            transport.calls.load(Ordering::SeqCst),
            1,
            "headers before boundary sends"
        );
        let mut actual = None;
        // At most six attempts. Every poll resumes after both deadlines; no unbounded await.
        for attempt in 0..6 {
            tokio::time::advance(Duration::from_millis(if attempt == 0 {
                ms + 200001 - first_boundary
            } else {
                ms + 200000
            }))
            .await;
            if let Some(value) = poll_once(future.as_mut()).await {
                actual = Some(value);
                break;
            }
            assert_eq!(
                transport.calls.load(Ordering::SeqCst),
                attempt + 2,
                "headers transport boundary"
            );
        }
        drop(future);
        fix_check(
            &case,
            actual.expect("headers bounded attempts"),
            debug,
            &runtime,
            "headers",
        );
        let calls = case["expected"]["calls"].as_u64().unwrap() as usize;
        assert_eq!(
            transport.calls.load(Ordering::SeqCst),
            calls,
            "headers attempt count"
        );
        assert_eq!(
            transport.dropped.load(Ordering::SeqCst),
            calls,
            "headers predecessor released"
        );
        count += 1;
    }
    assert_eq!(count, 20, "headers fixture inventory");
}

#[tokio::test(start_paused = true)]
async fn body_idle_deadline_matrix() {
    let mut count = 0;
    for mut case in fix_cases().into_iter().filter(|c| {
        c["spec"]["mode"] == "body" && c["spec"]["cancel"] != true && c["spec"]["frameAt"].is_null()
    }) {
        let spec = fix_spec(&case["spec"]);
        let runtime = Arc::new(Runtime::default());
        let transport = transport(&spec, &runtime, true);
        let ms = spec["transportMs"].as_u64().unwrap();
        let timeout = spec["timeout"].as_u64().unwrap();
        let extractor = LlmMemoryExtractor::new(
            config(&spec, "http://fixture.invalid"),
            transport.clone(),
            runtime.clone(),
        )
        .with_transport_timeout(Duration::from_millis(ms));
        let input = fix_input(&spec);
        let mut debug = Vec::new();
        let mut future = Box::pin(call(&extractor, &input, &spec, &mut debug));
        assert!(
            poll_once(future.as_mut()).await.is_none(),
            "body initial pending"
        );
        let mut actual = None;
        for _ in 0..2 {
            tokio::time::advance(Duration::from_millis(ms + 200000)).await;
            if let Some(value) = poll_once(future.as_mut()).await {
                actual = Some(value);
                break;
            }
        }
        drop(future);
        // TS clears the configuration timer after headers. Existing D-2 deliberately
        // extends it through the body; only these earlier/tied cells differ.
        if timeout <= ms {
            let message = format!("probe request timed out after {timeout}ms");
            case["expected"]["result"] = json!({"error":message,"name":"Error"});
            case["expected"]["debug"][0]["timedOut"] = json!(true);
            case["expected"]["debug"][0]["errorMessage"] = json!(message);
        }
        fix_check(
            &case,
            actual.expect("body bounded attempts"),
            debug,
            &runtime,
            "body",
        );
        let calls = case["expected"]["calls"].as_u64().unwrap() as usize;
        assert_eq!(
            transport.calls.load(Ordering::SeqCst),
            calls,
            "body attempt count"
        );
        assert_eq!(
            transport.dropped.load(Ordering::SeqCst),
            calls,
            "body predecessor released"
        );
        count += 1;
    }
    assert_eq!(count, 24, "body fixture inventory");
}

struct CaptureTransport {
    requests: Mutex<Vec<HeaderMap>>,
    calls: AtomicUsize,
    chat: bool,
    redirect: Option<String>,
}
use hyper::HeaderMap;
impl ModelHttpTransport for CaptureTransport {
    fn send(&self, request: Request<Full<Bytes>>) -> TransportFuture {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .unwrap()
            .push(request.headers().clone());
        if index == 0
            && let Some(url) = &self.redirect
        {
            return TransportFuture::ready(Ok(Response::builder()
                .status(307)
                .header("location", url)
                .body(
                    Full::new(Bytes::new())
                        .map_err(|never| match never {})
                        .boxed(),
                )
                .unwrap()));
        }
        TransportFuture::ready(Ok(Response::new(
            Full::new(Bytes::from_static(if self.chat {
                br#"{"choices":[{"message":{"content":"ok"}}]}"#
            } else {
                br#"{"output_text":"ok"}"#
            }))
            .map_err(|never| match never {})
            .boxed(),
        )))
    }
}
#[tokio::test]
async fn header_construction_vs_send_matrix() {
    let mut count = 0;
    for case in fix_cases()
        .into_iter()
        .filter(|c| c["spec"]["mode"] == "header")
    {
        let spec = &case["spec"];
        let runtime = Arc::new(Runtime::default());
        let transport = Arc::new(CaptureTransport {
            requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            chat: spec["api"] == "chat",
            redirect: None,
        });
        let extractor = LlmMemoryExtractor::new(
            config(spec, "http://fixture.invalid"),
            transport.clone(),
            runtime.clone(),
        );
        let mut debug = Vec::new();
        let actual = call(&extractor, &fix_input(spec), spec, &mut debug).await;
        fix_check(&case, actual, debug, &runtime, "header phase");
        assert_eq!(
            transport.calls.load(Ordering::SeqCst) as u64,
            case["expected"]["connects"].as_u64().unwrap(),
            "header phase sends {}",
            spec["name"]
        );
        count += 1;
    }
    assert_eq!(count, 24, "header fixture inventory");
}
#[tokio::test]
async fn encoding_matrix() {
    let mut count = 0;
    for case in fix_cases()
        .into_iter()
        .filter(|c| c["spec"]["mode"] == "encoding")
    {
        let spec = &case["spec"];
        let runtime = Arc::new(Runtime::default());
        let transport = Arc::new(CaptureTransport {
            requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            chat: false,
            redirect: spec["redirect"].as_str().map(str::to_owned),
        });
        let extractor = LlmMemoryExtractor::new(
            config(spec, "http://fixture.invalid"),
            transport.clone(),
            runtime.clone(),
        );
        let mut debug = Vec::new();
        let actual = call(&extractor, &fix_input(spec), spec, &mut debug).await;
        fix_check(&case, actual, debug, &runtime, "encoding");
        let encodings: Vec<_> = transport
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|headers| headers["accept-encoding"].to_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            json!(encodings),
            case["expected"]["encodings"],
            "encoding {}",
            spec["name"].as_str().unwrap()
        );
        count += 1;
    }
    assert_eq!(count, 9, "encoding fixture inventory");
}

struct FrameBody {
    _pending: NeverBody,
    frame: Arc<AtomicUsize>,
}
impl Body for FrameBody {
    type Data = Bytes;
    type Error = BodyFailure;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BodyFailure>>> {
        if self.frame.swap(0, Ordering::SeqCst) == 1 {
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"{")))))
        } else {
            Poll::Pending
        }
    }
}
struct FrameTransport {
    dropped: Arc<AtomicUsize>,
    frame: Arc<AtomicUsize>,
}
impl ModelHttpTransport for FrameTransport {
    fn send(&self, _: Request<Full<Bytes>>) -> TransportFuture {
        TransportFuture::ready(Ok(Response::new(
            FrameBody {
                _pending: NeverBody {
                    dropped: self.dropped.clone(),
                },
                frame: self.frame.clone(),
            }
            .boxed(),
        )))
    }
}
#[tokio::test(start_paused = true)]
async fn body_idle_resets_and_drop_cancels_predecessor() {
    let cases = fix_cases();
    let case = cases
        .iter()
        .find(|c| c["spec"]["name"] == "body-idle-reset")
        .unwrap();
    assert_eq!(
        case["expected"]["pendingAfterOriginalIdle"], true,
        "TS idle reset witness"
    );
    let cancelled = cases
        .iter()
        .find(|c| c["spec"]["name"] == "cancel-body-after-frame")
        .unwrap();
    assert_eq!(
        cancelled["expected"]["frameSent"], true,
        "TS frame-before-cancel witness"
    );
    assert_eq!(
        cancelled["expected"]["firstSocketDestroyed"], true,
        "TS frame cancellation witness"
    );
    for timeout in [0, 400000] {
        for cancel in [false, true] {
            let mut spec = case["spec"].clone();
            spec["timeout"] = json!(timeout);
            let dropped = Arc::new(AtomicUsize::new(0));
            let frame = Arc::new(AtomicUsize::new(0));
            let transport = Arc::new(FrameTransport {
                dropped: dropped.clone(),
                frame: frame.clone(),
            });
            let extractor = LlmMemoryExtractor::new(
                config(&spec, "http://fixture.invalid"),
                transport,
                Arc::new(Runtime::default()),
            );
            let input = fix_input(&spec);
            let mut future = Box::pin(extractor.call_structured_json(&input));
            assert!(
                poll_once(future.as_mut()).await.is_none(),
                "frame initial pending"
            );
            tokio::time::advance(Duration::from_secs(200)).await;
            frame.store(1, Ordering::SeqCst);
            assert!(
                poll_once(future.as_mut()).await.is_none(),
                "frame continuation pending"
            );
            tokio::time::advance(Duration::from_secs(150)).await;
            assert!(
                poll_once(future.as_mut()).await.is_none(),
                "idle reset retains pending body"
            );
            if !cancel {
                tokio::time::advance(Duration::from_secs(200)).await;
                let error = poll_once(future.as_mut())
                    .await
                    .expect("frame bounded completion")
                    .unwrap_err();
                assert_eq!(
                    error.message,
                    JsString::from(if timeout == 0 {
                        "terminated"
                    } else {
                        "probe request timed out after 400000ms"
                    }),
                    "frame idle versus absolute deadline"
                );
            }
            drop(future);
            assert_eq!(
                dropped.load(Ordering::SeqCst),
                1,
                "frame predecessor released"
            );
        }
    }
    for case in cases
        .iter()
        .filter(|c| c["spec"]["cancel"] == true && c["spec"]["frameAt"].is_null())
    {
        assert_eq!(
            case["expected"]["firstSocketDestroyed"], true,
            "TS cancel socket witness"
        );
        let spec = fix_spec(&case["spec"]);
        let runtime = Arc::new(Runtime::default());
        let transport = transport(&spec, &runtime, true);
        let extractor = LlmMemoryExtractor::new(
            config(&spec, "http://fixture.invalid"),
            transport.clone(),
            runtime,
        );
        let input = fix_input(&spec);
        let mut future = Box::pin(extractor.call_structured_json(&input));
        assert!(
            poll_once(future.as_mut()).await.is_none(),
            "cancel predecessor pending"
        );
        drop(future);
        tokio::time::advance(Duration::from_secs(1000)).await;
        assert_eq!(
            transport.dropped.load(Ordering::SeqCst),
            1,
            "cancel predecessor released"
        );
        assert_eq!(
            transport.calls.load(Ordering::SeqCst),
            1,
            "cancel no successor"
        );
    }
}
#[test]
#[ignore]
fn regenerates_fix_r1_identically() {
    baseline::regenerates_identically("edgeclaw-llm-fix-r1");
}

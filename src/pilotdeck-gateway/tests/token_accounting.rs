use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use pilotdeck_gateway::host::modules::budget::{
    EvaluationOptions, Handler, Ports, RequestBudgetEvaluator,
};
use pilotdeck_gateway::host::{HostFuture, ModuleError};
use pilotdeck_gateway::runtime::model::token_accounting::{
    TokenAccountingRuntime, TokenBudgetManager,
};
use pilotdeck_gateway::runtime::model::{
    CountHttpRequest, HyperCountTransport, ProviderCountTransport,
};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{net::TcpListener, sync::watch};
#[path = "common/baseline.rs"]
mod baseline;

// serde_json rejects unpaired UTF-16. Keep those strings in a lossless test
// carrier, then reconstruct JsString before entering production code.
fn js(value: &Value) -> JsValue {
    match value {
        Value::Object(fields) if fields.len() == 1 && fields.contains_key("$utf16") => {
            JsValue::String(pilotdeck_protocol::JsString::from_utf16(
                fields["$utf16"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|n| n.as_u64().unwrap() as u16)
                    .collect(),
            ))
        }
        Value::Object(fields) => {
            let mut object = JsObject::new();
            for (key, value) in fields {
                object.insert(key.as_str().into(), js(value));
            }
            JsValue::Object(object)
        }
        Value::Array(items) => JsValue::Array(items.iter().map(js).collect()),
        _ => parse_json(&value.to_string()).unwrap(),
    }
}
fn raw(value: &Value) -> JsObject {
    js(value).as_object().unwrap().clone()
}
fn json_value(value: &JsValue) -> Value {
    match value {
        JsValue::String(s) => match s.to_utf8() {
            Ok(s) => json!(s),
            Err(_) => json!({"$utf16":s.units()}),
        },
        JsValue::Array(items) => Value::Array(items.iter().map(json_value).collect()),
        JsValue::Object(object) => {
            assert!(!object.contains_key("$utf16"), "reserved test carrier");
            Value::Object(
                object
                    .entries()
                    .iter()
                    .filter(|(_, v)| !v.is_undefined())
                    .map(|(k, v)| (k.to_utf8().expect("fixture keys are UTF-8"), json_value(v)))
                    .collect(),
            )
        }
        JsValue::Undefined => Value::Null,
        _ => serde_json::from_str(&stringify_json(value).unwrap().unwrap()).unwrap(),
    }
}
fn fixture() -> Value {
    json_value(&baseline::oracle(include_str!(
        "fixtures/token-accounting/oracle.json"
    )))
}
fn options(value: &Value) -> EvaluationOptions {
    EvaluationOptions {
        max_context_tokens: value["maxContextTokens"].as_f64().unwrap(),
        reserved_output_tokens: value["reservedOutputTokens"].as_f64(),
        use_provider_count: value["useProviderCount"].as_bool(),
        calibration: value.get("calibration").filter(|v| v.is_object()).map(raw),
        signal: None,
    }
}
#[derive(Default)]
struct Exchange {
    reply: Value,
    status: u16,
    requests: Vec<Value>,
    urls: Vec<String>,
}
#[derive(Clone)]
struct Transport {
    exchange: Arc<Mutex<Exchange>>,
    origin: String,
    http: bool,
}
impl ProviderCountTransport for Transport {
    fn post(&self, mut request: CountHttpRequest) -> HostFuture<JsValue> {
        let transport = self.clone();
        Box::pin(async move {
            transport
                .exchange
                .lock()
                .unwrap()
                .urls
                .push(request.url.replace(&transport.origin, "$MOCK"));
            if request.url.starts_with("https://api.openai.com/") {
                request.url = format!(
                    "{}{}",
                    transport.origin,
                    url::Url::parse(&request.url).unwrap().path()
                );
            }
            assert!(
                request.url.starts_with(&format!("{}/", transport.origin)),
                "no external provider traffic"
            );
            if transport.http {
                return HyperCountTransport::default().post(request).await;
            }
            let mut exchange = transport.exchange.lock().unwrap();
            exchange.requests.push(json!({"method":"POST", "path":url::Url::parse(&request.url).unwrap().path(), "headers":json_value(&JsValue::Object(request.headers)), "body":json_value(&request.body)}));
            if !(200..300).contains(&exchange.status) {
                return Err(ModuleError::new(format!(
                    "Provider token count failed with HTTP {}.",
                    exchange.status
                ))
                .into());
            }
            Ok(js(&exchange.reply))
        })
    }
}
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn mock(exchange: Arc<Mutex<Exchange>>) -> (String, Server) {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("loopback HTTP fixture needs listen permission");
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                connection = listener.accept() => {
                    let (socket, _) = connection.unwrap(); let exchange = exchange.clone();
                    connections.spawn(async move {
                        let service = service_fn(move |request: Request<Incoming>| {
                            let exchange = exchange.clone();
                            async move {
                                let (parts, body) = request.into_parts();
                                let bytes = body.collect().await.unwrap().to_bytes();
                                let headers: serde_json::Map<String, Value> = parts.headers.iter()
                                    .filter(|(k, _)| ["content-type", "x-api-key", "anthropic-version", "authorization", "x-fixture"].contains(&k.as_str()))
                                    .map(|(k, v)| (k.to_string(), json!(v.to_str().unwrap()))).collect();
                                let mut exchange = exchange.lock().unwrap();
                                exchange.requests.push(json!({"method":parts.method.as_str(), "path":parts.uri.path(), "headers":headers, "body":json_value(&parse_json(&String::from_utf8_lossy(&bytes)).unwrap())}));
                                Ok::<_, Infallible>(Response::builder().status(exchange.status).header("content-type", "application/json").body(Full::new(Bytes::from(exchange.reply.to_string()))).unwrap())
                            }
                        });
                        let _ = http1::Builder::new().serve_connection(TokioIo::new(socket), service).await;
                    });
                }
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    (format!("http://{address}"), Server(task))
}
async fn replay(http: bool) {
    let fixture = fixture();
    let cases = fixture["cases"].as_array().unwrap();
    let original = baseline::oracle(include_str!("fixtures/token-accounting/oracle.json"));
    let JsValue::Array(original_cases) = original.get("cases").unwrap() else {
        panic!("cases")
    };
    // Exact population is deliberately fixed: truncation cannot pass a replay.
    assert_eq!(cases.len(), 55);
    assert_eq!(
        cases
            .iter()
            .map(|case| case["steps"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [
            6, 51, 141, 18, 2, 7, 3, 1, 2, 2, 2, 2, 11, 7, 2, 8, 3, 3, 3, 3, 3, 3, 7, 2, 8, 3, 3,
            3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 4, 1, 1
        ]
    );
    let mut steps = 0;
    for (case, original_case) in cases.iter().zip(original_cases) {
        let exchange = Arc::new(Mutex::new(Exchange::default()));
        let (origin, _server) = if http {
            let (origin, server) = mock(exchange.clone()).await;
            (origin, Some(server))
        } else {
            ("http://127.0.0.1:1".into(), None)
        };
        let config = parse_json(
            &stringify_json(original_case.get("config").unwrap())
                .unwrap()
                .unwrap()
                .replace("$MOCK", &origin),
        )
        .unwrap();
        let runtime = TokenAccountingRuntime::new(config.as_object().unwrap().clone())
            .with_cache_size(case["settings"]["cacheSize"].as_u64().unwrap_or(256) as usize)
            .with_manager(TokenBudgetManager {
                multimedia_tokens: case["settings"]["managerOptions"]["multimediaTokens"]
                    .as_f64()
                    .unwrap_or(2000.0),
                per_message_overhead: case["settings"]["managerOptions"]["perMessageOverhead"]
                    .as_f64()
                    .unwrap_or(4.0),
                warning_ratio: case["settings"]["managerOptions"]["warningRatio"]
                    .as_f64()
                    .unwrap_or(0.8),
                blocking_ratio: case["settings"]["managerOptions"]["blockingRatio"]
                    .as_f64()
                    .unwrap_or(0.9),
            })
            .with_transport(Arc::new(Transport {
                exchange: exchange.clone(),
                origin,
                http,
            }));
        let JsValue::Array(original_steps) = original_case.get("steps").unwrap() else {
            panic!("steps")
        };
        for (step, original_step) in case["steps"].as_array().unwrap().iter().zip(original_steps) {
            steps += 1;
            {
                let mut exchange = exchange.lock().unwrap();
                exchange.reply = step
                    .get("reply")
                    .cloned()
                    .unwrap_or(json!({"input_tokens":73}));
                exchange.status = step["status"].as_u64().unwrap_or(200) as u16;
                exchange.requests.clear();
                exchange.urls.clear();
            }
            let result = match runtime
                .evaluate_request_budget(
                    original_step
                        .get("request")
                        .unwrap()
                        .as_object()
                        .unwrap()
                        .clone(),
                    options(&step["options"]),
                )
                .await
            {
                Ok(snapshot) => json!({"ok":true,"snapshot":json_value(&snapshot)}),
                Err(error) => json!({"ok":false,"message":error.message}),
            };
            assert_eq!(result, step["result"], "{} step {steps}", case["name"]);
            let exchange = exchange.lock().unwrap();
            assert_eq!(
                json!(exchange.requests),
                step["requests"],
                "{} body step {steps}",
                case["name"]
            );
            assert_eq!(
                json!(exchange.urls),
                step["urls"],
                "{} URL step {steps}",
                case["name"]
            );
        }
    }
    assert_eq!(steps, 363);
}
#[tokio::test]
async fn real_ts_accounting_and_provider_projection() {
    replay(false).await;
}
#[tokio::test]
async fn real_ts_provider_requests_over_loopback_http() {
    replay(true).await;
}
#[test]
fn real_ts_budget_snapshots() {
    let fixture = fixture();
    let rows = fixture["snapshots"].as_array().unwrap();
    assert_eq!(rows.len(), 42);
    for row in rows {
        let manager = TokenBudgetManager {
            warning_ratio: row["managerOptions"]["warningRatio"]
                .as_f64()
                .unwrap_or(0.8),
            blocking_ratio: row["managerOptions"]["blockingRatio"]
                .as_f64()
                .unwrap_or(0.9),
            ..TokenBudgetManager::default()
        };
        assert_eq!(
            json_value(&manager.snapshot_from_tokens(
                row["tokens"].as_f64().unwrap(),
                row["context"].as_f64().unwrap(),
                &raw(&row["metadata"])
            )),
            row["result"]
        );
    }
}
struct NoCost;
impl pilotdeck_gateway::host::modules::budget::UsageCostEstimator for NoCost {
    fn estimate_usage_cost(
        &self,
        _: Option<JsObject>,
        _: pilotdeck_protocol::JsString,
        _: pilotdeck_protocol::JsString,
    ) -> HostFuture<Option<f64>> {
        Box::pin(async { Ok(None) })
    }
}
#[tokio::test]
async fn real_ts_operation_required_fields() {
    let fixture = fixture();
    let rows = fixture["operations"].as_array().unwrap();
    assert_eq!(rows.len(), 20);
    let handler = Handler {
        ports: Some(Ports {
            estimate_usage_cost: Some(Arc::new(NoCost)),
            ..Ports::local(None)
        }),
        signal: None,
    };
    assert_eq!(
        handler.methods().unwrap(),
        vec![
            "estimate_request_input",
            "evaluate_request_budget",
            "estimate_usage_cost"
        ]
    );
    for row in rows {
        let result = match handler.call(js(&json!({"payload":row["payload"]}))).await {
            Ok(value) => json!({"ok":true,"value":json_value(&JsValue::Object(value))}),
            Err(error) => json!({"ok":false,"message":error.message}),
        };
        assert_eq!(result, row["result"], "{}", row["payload"]);
    }
}
struct NeverCount {
    started: Arc<AtomicBool>,
    dropped: Arc<AtomicBool>,
}
struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
impl ProviderCountTransport for NeverCount {
    fn post(&self, _: CountHttpRequest) -> HostFuture<JsValue> {
        let started = self.started.clone();
        let dropped = self.dropped.clone();
        Box::pin(async move {
            let _guard = Dropped(dropped);
            started.store(true, Ordering::SeqCst);
            std::future::pending().await
        })
    }
}
fn request() -> JsObject {
    raw(&json!({"provider":"p","model":"m","messages":[]}))
}
fn configured() -> TokenAccountingRuntime {
    TokenAccountingRuntime::new(raw(
        &json!({"providers":{"p":{"id":"p","protocol":"anthropic","url":"http://127.0.0.1:1","apiKey":"fixture","headers":{},"models":{"m":{"id":"m"}}}}}),
    ))
}
async fn until_started(started: &AtomicBool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !started.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("handler yields to other tasks");
}
#[tokio::test]
async fn timeout_and_abort_drop_transport_without_blocking_handler() {
    for abort in [false, true] {
        let started = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let runtime = configured()
            .with_timeout(if abort {
                Duration::from_secs(60)
            } else {
                Duration::from_millis(20)
            })
            .with_transport(Arc::new(NeverCount {
                started: started.clone(),
                dropped: dropped.clone(),
            }));
        let (sender, signal) = watch::channel(false);
        let mut options = options(&json!({"maxContextTokens":100}));
        options.signal = Some(signal);
        let task = tokio::spawn(runtime.evaluate_request_budget(request(), options));
        until_started(&started).await;
        if abort {
            sender.send(true).unwrap();
        }
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            json_value(&result)["estimatorError"],
            "This operation was aborted"
        );
        assert_eq!(json_value(&result)["source"], "local");
        assert!(dropped.load(Ordering::SeqCst));
    }
}
#[tokio::test]
async fn dropping_evaluation_cancels_owned_transport() {
    let started = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let runtime = configured()
        .with_timeout(Duration::from_secs(60))
        .with_transport(Arc::new(NeverCount {
            started: started.clone(),
            dropped: dropped.clone(),
        }));
    let task = tokio::spawn(
        runtime.evaluate_request_budget(request(), options(&json!({"maxContextTokens":100}))),
    );
    until_started(&started).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(dropped.load(Ordering::SeqCst));
}
#[tokio::test]
async fn already_aborted_count_skips_transport() {
    let started = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let runtime = configured().with_transport(Arc::new(NeverCount {
        started: started.clone(),
        dropped,
    }));
    let (_sender, signal) = watch::channel(true);
    let mut options = options(&json!({"maxContextTokens":100}));
    options.signal = Some(signal);
    let result = runtime
        .evaluate_request_budget(request(), options)
        .await
        .unwrap();
    assert_eq!(
        json_value(&result)["estimatorError"],
        "This operation was aborted"
    );
    assert!(!started.load(Ordering::SeqCst));
}
#[test]
#[ignore = "requires fixed TS checkout, node/tsx, and loopback listen permission"]
fn regenerate_twice_identically() {
    let baseline = std::env::var_os("PDRS_BASELINE").expect("set PDRS_BASELINE");
    let baseline = std::path::Path::new(&baseline);
    let directory =
        std::env::temp_dir().join(format!("pdrs-token-accounting-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let mut generated = Vec::new();
    for _ in 0..2 {
        let result = std::process::Command::new("node")
            .arg("--import")
            .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/token-accounting/generate.mts"
            ))
            .arg(&directory)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let stdout = String::from_utf8(result.stdout).unwrap();
        let metadata: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
        assert_eq!(metadata["transport"], "loopback HTTP");
        generated.push(
            std::fs::read(
                std::path::Path::new(metadata["output"].as_str().unwrap()).join("oracle.json"),
            )
            .unwrap(),
        );
    }
    assert_eq!(generated[0], generated[1]);
    assert_eq!(
        generated[0],
        include_bytes!("fixtures/token-accounting/oracle.json")
    );
}

#[tokio::test]
async fn disabled_provider_ignores_deep_cache_only_fields() {
    let mut request = request();
    let deep = format!("{}0{}", "[".repeat(20_000), "]".repeat(20_000));
    request.insert("outputSchema".into(), parse_json(&deep).unwrap());
    let runtime = TokenAccountingRuntime::default();
    let result = runtime
        .evaluate_request_budget(
            request.clone(),
            options(&json!({"maxContextTokens":100,"useProviderCount":false})),
        )
        .await
        .unwrap();
    assert_eq!(result.get("tokens").and_then(JsValue::as_number), Some(0.0));
    // Enabled cache sorting reports a JS-style stack error, never overflows
    // the native worker stack. V8's precise cutoff depends on its call stack.
    let error = runtime
        .evaluate_request_budget(request, options(&json!({"maxContextTokens":100})))
        .await
        .unwrap_err();
    assert_eq!(error.message, "Maximum call stack size exceeded");
}

fn request_with_deep_schema(depth: usize) -> JsObject {
    let deep = format!(
        "{{\"name\":\"x\",\"schema\":{}{}{}}}",
        "{\"a\":".repeat(depth),
        "{\"leaf\":true}",
        "}".repeat(depth)
    );
    let mut request = request();
    request.insert("outputSchema".into(), parse_json(&deep).unwrap());
    request
}

#[tokio::test]
async fn cache_key_depth_matches_measured_v8_sort_boundary() {
    // Real TS (Node v22.22.0) through the public evaluator with the default
    // cacheSize: V8's recursive `sortValue` cache key succeeded at schema
    // depth 1890 (transform depth 1892) and threw "Maximum call stack size
    // exceeded" at schema depth 1895 (transform depth 1897), 5/5 runs each.
    // The project's V8 resource rule keeps the guaranteed-success side exact
    // and returns the same failure once past the measured failure point;
    // 1801..=1897 is the accepted undecidable band. CACHE_KEY_MAX_DEPTH
    // (transform depth 1800) is the conservative lower bound, so schema depth
    // 1700 must still succeed locally while 1900 is already past V8's
    // measured failure point and must fail.
    let runtime = TokenAccountingRuntime::default();
    let snapshot = runtime
        .evaluate_request_budget(
            request_with_deep_schema(1700),
            options(&json!({"maxContextTokens":100})),
        )
        .await
        .unwrap();
    assert_eq!(json_value(&snapshot)["source"], "local");
    let error = runtime
        .evaluate_request_budget(
            request_with_deep_schema(1900),
            options(&json!({"maxContextTokens":100})),
        )
        .await
        .unwrap_err();
    assert_eq!(error.message, "Maximum call stack size exceeded");
}
#[test]
fn unused_nonfinite_manager_costs_and_snapshot_nan() {
    let manager = TokenBudgetManager {
        multimedia_tokens: f64::INFINITY,
        per_message_overhead: f64::NAN,
        ..TokenBudgetManager::default()
    };
    assert_eq!(manager.estimate_messages_tokens(&[]).unwrap(), 0.0);
    let snapshot = manager.snapshot_from_tokens(0.0, f64::NAN, &JsObject::new());
    assert!(
        snapshot
            .get("totalContextTokens")
            .and_then(JsValue::as_number)
            .unwrap()
            .is_nan()
    );
    let mut metadata = JsObject::new();
    metadata.insert("reservedOutputTokens".into(), JsValue::Number(f64::NAN));
    let snapshot = manager.snapshot_from_tokens(0.0, 100.0, &metadata);
    assert!(
        snapshot
            .get("reservedOutputTokens")
            .and_then(JsValue::as_number)
            .unwrap()
            .is_nan()
    );
    assert_eq!(
        snapshot
            .get("maxContextTokens")
            .and_then(JsValue::as_number),
        Some(100.0)
    );
}

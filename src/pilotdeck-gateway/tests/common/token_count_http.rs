//! Shared by real sockets and scripted exchanges; both exercise production
//! redirect/JSON handling and compare complete TS snapshots and request traces.
use super::baseline;
use super::{CountHttpRequest, ProviderCountTransport, TokenAccountingRuntime};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Body};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub const ORACLE: &str = include_str!("../fixtures/token-accounting/http-oracle.json");
pub const HEADER_NAMES: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "content-type",
    "content-encoding",
    "content-language",
    "content-location",
    "x-api-key",
    "anthropic-version",
    "x-fixture",
];
pub struct Exchange {
    pub origins: [String; 2],
    pub responses: Vec<Value>,
    pub requests: Vec<Value>,
}
impl Default for Exchange {
    fn default() -> Self {
        Self {
            origins: [
                "http://127.0.0.1:41001".into(),
                "http://127.0.0.1:41002".into(),
            ],
            responses: Vec::new(),
            requests: Vec::new(),
        }
    }
}
pub fn materialize(text: &str, origins: &[String; 2]) -> String {
    text.replace(
        "$MOCK_AUTHORITY",
        origins[0].strip_prefix("http://").unwrap(),
    )
    .replace(
        "$OTHER_AUTHORITY",
        origins[1].strip_prefix("http://").unwrap(),
    )
    .replace("$MOCK", &origins[0])
    .replace("$OTHER", &origins[1])
}
// Preserve even lone UTF-16 error units, while ignoring JSON object key order.
fn comparable(value: &JsValue) -> Value {
    match value {
        JsValue::String(s) => json!({"$utf16": s.units()}),
        JsValue::Object(o) => Value::Object(
            o.entries()
                .iter()
                .map(|(k, v)| (k.to_utf8().unwrap(), comparable(v)))
                .collect(),
        ),
        JsValue::Array(a) => Value::Array(a.iter().map(comparable).collect()),
        _ => serde_json::from_str(&stringify_json(value).unwrap().unwrap()).unwrap(),
    }
}
pub async fn respond<B: Body<Data = Bytes>>(
    request: Request<B>,
    origin: &str,
    exchange: &Arc<Mutex<Exchange>>,
) -> Response<Full<Bytes>> {
    let (parts, body) = request.into_parts();
    if let Some(host) = parts.headers.get("host") {
        assert_eq!(
            host.to_str().unwrap(),
            url::Url::parse(origin).unwrap().authority(),
            "user Host must be discarded"
        );
    }
    let bytes = body
        .collect()
        .await
        .ok()
        .expect("mock request body")
        .to_bytes();
    let mut exchange = exchange.lock().unwrap();
    assert!(
        exchange.origins.iter().any(|allowed| allowed == origin),
        "no external provider traffic"
    );
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    let headers: serde_json::Map<String, Value> = parts
        .headers
        .iter()
        .filter(|(k, _)| HEADER_NAMES.contains(&k.as_str()))
        .map(|(k, v)| (k.to_string(), json!(v.to_str().unwrap())))
        .collect();
    let origin = if origin == exchange.origins[0] {
        "$MOCK"
    } else {
        "$OTHER"
    };
    let record = json!({"origin":origin, "path":parts.uri.path_and_query().unwrap().as_str(), "method":parts.method.as_str(), "headers":headers, "body":body});
    let index = exchange.requests.len();
    exchange.requests.push(record);
    let spec = &exchange.responses[index.min(exchange.responses.len() - 1)];
    let status = spec["status"].as_u64().unwrap_or(200) as u16;
    let mut response = Response::builder()
        .status(status)
        .header("content-type", "text/plain");
    if let Some(location) = spec["location"].as_str() {
        response = response.header("location", materialize(location, &exchange.origins));
    }
    if let Some(locations) = spec["locations"].as_array() {
        for location in locations {
            response = response.header(
                "location",
                materialize(location.as_str().unwrap(), &exchange.origins),
            );
        }
    }
    let body = if [204, 205, 304].contains(&status) {
        Vec::new()
    } else if let Some(bytes) = spec["bytes"].as_array() {
        bytes.iter().map(|v| v.as_u64().unwrap() as u8).collect()
    } else {
        spec["text"]
            .as_str()
            .unwrap_or("{\"input_tokens\":73}")
            .as_bytes()
            .to_vec()
    };
    response.body(Full::new(Bytes::from(body))).unwrap()
}

pub async fn replay(exchange: Arc<Mutex<Exchange>>, transport: Arc<dyn ProviderCountTransport>) {
    let fixture = baseline::oracle(ORACLE);
    assert_eq!(fixture.get("node").unwrap(), &JsValue::from("v22.22.0"));
    assert_eq!(
        fixture.get("v8").unwrap(),
        &JsValue::from("12.4.254.21-node.33")
    );
    let origins = exchange.lock().unwrap().origins.clone();
    let config: Value = serde_json::from_str(&materialize(
        &stringify_json(fixture.get("config").unwrap())
            .unwrap()
            .unwrap(),
        &origins,
    ))
    .unwrap();
    let JsValue::Array(cases) = fixture.get("cases").unwrap() else {
        panic!("cases")
    };
    assert_eq!(cases.len(), 152);
    let mut names = std::collections::HashSet::new();
    let mut request_count = 0;
    for case in cases {
        let name = case
            .get("name")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        assert!(names.insert(name.clone()), "duplicate case {name}");
        {
            let mut exchange = exchange.lock().unwrap();
            exchange.responses = serde_json::from_str(
                &stringify_json(case.get("responses").unwrap())
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            exchange.requests.clear();
        }
        let mut config = config.clone();
        if let Some(provider) = case.get("provider").and_then(JsValue::as_object) {
            for (key, value) in provider.entries() {
                config["providers"]["p"][key.to_utf8().unwrap()] =
                    serde_json::from_str(&stringify_json(value).unwrap().unwrap()).unwrap();
            }
        }
        let config = parse_json(&config.to_string()).unwrap();
        let runtime = TokenAccountingRuntime::new(config.as_object().unwrap().clone())
            .with_cache_size(0)
            .with_transport(transport.clone());
        let mut request = fixture.get("request").unwrap().as_object().unwrap().clone();
        if let Some(overrides) = case.get("request").and_then(JsValue::as_object) {
            for (key, value) in overrides.entries() {
                request.insert(key.clone(), value.clone());
            }
        }
        let snapshot = runtime
            .evaluate_request_budget(
                request,
                super::EvaluationOptions {
                    max_context_tokens: 100.0,
                    reserved_output_tokens: None,
                    use_provider_count: None,
                    calibration: None,
                    signal: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            comparable(&snapshot),
            comparable(case.get("snapshot").unwrap()),
            "snapshot: {name}"
        );
        let exchange = exchange.lock().unwrap();
        let expected: Value = serde_json::from_str(
            &stringify_json(case.get("requests").unwrap())
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(json!(exchange.requests), expected, "HTTP trace: {name}");
        request_count += exchange.requests.len();
    }
    assert_eq!(request_count, 327);
}

// Both callers import this trait, making runtime's public method available.
use super::RequestBudgetEvaluator;

pub fn guard(request: &CountHttpRequest, exchange: &Arc<Mutex<Exchange>>) {
    if [
        "invalid/v1/messages/count_tokens",
        "http://fixture:fixture@127.0.0.1:1/v1/messages/count_tokens",
    ]
    .contains(&request.url.as_str())
    {
        return; // Node rejects these exact fixtures before any HTTP exchange.
    }
    let origin = url::Url::parse(&request.url)
        .unwrap()
        .origin()
        .ascii_serialization();
    assert!(
        exchange.lock().unwrap().origins.contains(&origin),
        "no external initial provider traffic"
    );
}

//! #411 / #360 #411-3,4: actual provider TCP requests and certificate failures.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/model_mock.rs"]
mod model_mock;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use model_mock::{Exchange, start_mock};
use pilotdeck_gateway::runtime::model::http::{
    HyperModelTransport, ModelHttpTransport, StreamOptions, StreamOutput, stream_model,
};
use pilotdeck_gateway::runtime::model::router::AbortFlag;
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

const ORACLE: &str = include_str!("fixtures/provider-http-boundaries/oracle.json");
fn oracle() -> Value {
    std::panic::catch_unwind(|| baseline::oracle(ORACLE))
        .expect("#360 #411 #860 oracle provenance");
    serde_json::from_str(ORACLE).expect("#360 #411 parse oracle")
}
fn js(value: &Value) -> JsValue {
    parse_json(&value.to_string()).expect("#360 #411 JSON input")
}
fn value(value: &JsValue) -> Value {
    serde_json::from_str(
        &stringify_json(value)
            .expect("#360 #411 serialize output")
            .expect("#360 #411 JSON defined"),
    )
    .expect("#360 #411 output JSON")
}
fn project(event: &Value) -> Value {
    let keys: &[&str] = if event["type"] == "error" {
        &["type"]
    } else {
        &["type", "text"]
    };
    let mut result = serde_json::Map::new();
    for key in keys {
        if let Some(v) = event.get(*key) {
            result.insert((*key).into(), v.clone());
        }
    }
    if event["type"] == "error" {
        let mut error = serde_json::Map::new();
        for key in [
            "code",
            "message",
            "retryable",
            "maxOutputTokens",
            "availableOutputTokens",
            "maxContextTokens",
        ] {
            if let Some(v) = event["error"].get(key) {
                error.insert(key.into(), v.clone());
            }
        }
        result.insert("error".into(), Value::Object(error));
    }
    result.into()
}
async fn replay(prefix: &str, count: usize, issue: &str) {
    let oracle = oracle();
    let ordered = parse_json(ORACLE).expect("#360 #411 ordered oracle");
    let JsValue::Array(ordered_cases) = ordered.get("cases").expect("#360 #411 ordered cases")
    else {
        panic!("#360 #411 ordered cases array");
    };
    let rows: Vec<_> = oracle["cases"]
        .as_array()
        .expect("#360 #411 oracle cases")
        .iter()
        .filter(|c| {
            c["name"]
                .as_str()
                .expect("#360 #411 case name")
                .starts_with(prefix)
        })
        .collect();
    assert_eq!(rows.len(), count, "#360 #411 {issue} case coverage");
    for case in rows {
        let name = case["name"].as_str().expect("#360 #411 case name");
        let exchange = Arc::new(Mutex::new(Exchange {
            replies: vec![js(&case["reply"])],
            ..Default::default()
        }));
        let mock = std::panic::catch_unwind(|| start_mock(exchange.clone(), vec![], None))
            .expect("#360 #411 TCP mock startup");
        let mut provider = case["provider"].clone();
        provider["url"] = Value::String(
            provider["url"]
                .as_str()
                .expect("#360 #411 URL")
                .replace("$MOCK", &mock.origin),
        );
        let config = js(&json!({"providers":{"p":provider}}));
        // Preserve nested object insertion order in input_schema. serde_json's
        // map sorts keys; the wire-byte oracle must receive the original JS order.
        let request = ordered_cases
            .iter()
            .find(|c| {
                c.get("name")
                    .and_then(JsValue::as_string)
                    .is_some_and(|s| s.equals_str(name))
            })
            .expect("#360 #411 ordered case")
            .get("request")
            .expect("#360 #411 ordered request")
            .clone();
        let abort = AbortFlag::new();
        let clock = Arc::new(AtomicU64::new(0));
        let read_clock = clock.clone();
        let options = StreamOptions {
            abort: Some(abort.clone()),
            clock: Arc::new(move || read_clock.load(Ordering::SeqCst) as f64),
            random: Arc::new(|| 0.5),
            stream_timeout_ms: case["streamTimeoutMs"].as_f64(),
            body_timeout_ms: case["bodyTimeoutMs"].as_f64().unwrap_or(600000.0),
            ..Default::default()
        };
        let mut events = Vec::new();
        let mut sink = |output| {
            if let StreamOutput::Event(event) = output {
                let event = value(&event);
                events.push(project(&event));
                if event["type"] == "text_delta" {
                    if let Some(ms) = case["clockAfterFirst"].as_u64() {
                        clock.store(ms, Ordering::SeqCst);
                    }
                    if case["abortAfterFirst"] == true {
                        abort.abort();
                    }
                }
            }
        };
        let transport = HyperModelTransport::new();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            stream_model(&transport, &request, &config, &options, &mut sink),
        )
        .await
        .expect("#360 #411 provider TCP deadline");
        let thrown = result.err().map(|e| json!({"name":e.name().to_utf8().expect("#360 #411 error name"),"message":e.message().to_utf8().expect("#360 #411 error message")}));
        assert_eq!(
            thrown.as_ref(),
            case.get("thrown"),
            "#360 #411 {name}: thrown"
        );
        assert_eq!(
            events,
            case["events"].as_array().expect("#360 #411 events").clone(),
            "#360 #411 {name}: events"
        );
        let received = exchange.lock().expect("#360 #411 exchange lock");
        let requests: Vec<_> = received
            .requests
            .iter()
            .map(|r| {
                let row = value(&JsValue::Object(r.clone()));
                json!({"method":row["method"],"path":row["path"],"body":row["body"]})
            })
            .collect();
        assert_eq!(
            requests,
            case["requests"]
                .as_array()
                .expect("#360 #411 requests")
                .clone(),
            "#360 #411 {name}: wire requests"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_chunk_boundaries_match_node() {
    replay("repeat-", 5, "repetition").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duration_boundaries_match_node() {
    replay("duration-", 3, "duration").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_idle_fallback_and_cancel_match_node() {
    replay("zero-", 2, "idle-zero").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timer_contract_matches_node_outside_near_tie() {
    replay("idle-body-", 1, "#411 equal").await;
    replay("body-before-", 1, "#411 separated").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_body_branches_match_node() {
    replay("body-anthropic-", 5, "anthropic body").await;
    replay("body-openai-", 10, "openai body").await;
    replay("body-google-", 5, "google body").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn token_limit_branches_match_node() {
    replay("token-", 29, "token limits").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn certificate_causes_match_node_over_tls() {
    let oracle = oracle();
    let material: Value =
        serde_json::from_str(include_str!("fixtures/provider-http-boundaries/tls.json"))
            .expect("#360 #411 TLS material");
    let rows = oracle["tls"].as_array().expect("#360 #411 TLS cases");
    assert_eq!(rows.len(), 6, "#360 #411 TLS case coverage");
    for case in rows {
        let cert = material["cases"]
            .as_array()
            .expect("#360 #411 cert cases")
            .iter()
            .find(|c| c["name"] == case["name"])
            .expect("#360 #411 named cert");
        let cert = CertificateDer::from_pem_slice(
            cert["cert"]
                .as_str()
                .expect("#360 #411 cert PEM")
                .as_bytes(),
        )
        .expect("#360 #411 parse cert");
        let key = PrivateKeyDer::from_pem_slice(
            material["key"]
                .as_str()
                .expect("#360 #411 key PEM")
                .as_bytes(),
        )
        .expect("#360 #411 parse key");
        let ca = CertificateDer::from_pem_slice(
            material["ca"]
                .as_str()
                .expect("#360 #411 CA PEM")
                .as_bytes(),
        )
        .expect("#360 #411 parse CA");
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("#360 #411 TLS versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("#360 #411 server cert");
        let exchange = Arc::new(Mutex::new(Exchange {
            replies: vec![js(&json!({"chunks":[{"text":"ok"}]}))],
            ..Default::default()
        }));
        // Failure is immediately rethrown with the issue marker; no server
        // state is reused after unwinding from the shared fixture startup.
        let mock = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            start_mock(exchange.clone(), vec![], Some(Arc::new(config)))
        }))
        .expect("#360 #411 TLS mock startup");
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca).expect("#360 #411 CA root");
        let transport = HyperModelTransport::with_roots(roots);
        let request = hyper::Request::get(&mock.origin)
            .body(Full::new(Bytes::new()))
            .expect("#360 #411 TLS request");
        let response = tokio::time::timeout(Duration::from_secs(5), transport.send(request))
            .await
            .expect("#360 #411 TLS deadline");
        let actual = match response {
            Ok(response) => {
                let body =
                    tokio::time::timeout(Duration::from_secs(5), response.into_body().collect())
                        .await
                        .expect("#360 #411 TLS body deadline")
                        .expect("#360 #411 TLS body");
                json!({"text":String::from_utf8(body.to_bytes().to_vec()).expect("#360 #411 TLS UTF8")})
            }
            Err(error) => json!({"message":error.message,"cause":error.cause_code}),
        };
        assert_eq!(
            actual, case["result"],
            "#360 #411 TLS cause: {}",
            case["name"]
        );
        assert_eq!(
            exchange
                .lock()
                .expect("#360 #411 TLS requests")
                .requests
                .len(),
            usize::from(case["name"] == "valid"),
            "#360 #411 handshake failure must precede HTTP"
        );
    }
}

#[test]
#[ignore = "requires Node v22.22.0 and the fixed TS baseline"]
fn provider_http_boundaries_regenerate_identically() {
    std::panic::catch_unwind(|| baseline::regenerates_identically("provider-http-boundaries"))
        .expect("#360 #411 #860 regeneration");
}

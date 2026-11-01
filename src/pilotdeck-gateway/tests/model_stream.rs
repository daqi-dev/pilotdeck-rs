//! Provider streaming replay against the fixed TypeScript baseline
//! (`tests/fixtures/model-stream/generate.mts`). The real TS `streamModel`
//! and this Rust port talk to the same scripted loopback mock (plain HTTP and
//! HTTPS with a throwaway CA); request bytes and the event/retry/throw
//! timeline are compared in order, byte for byte. No external traffic.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/model_mock.rs"]
mod model_mock;
use model_mock::*;
use pilotdeck_protocol::{JsObject, JsValue, parse_json};
use std::sync::{Arc, Mutex};

const ORACLE: &str = include_str!("fixtures/model-stream/oracle.json");

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_ts_stream_model_over_loopback_http_and_tls() {
    replay(&baseline::oracle(ORACLE), 31, 38).await;
}

/// The production count transport shares the stream transport's rustls
/// connector: an HTTPS count call to a local self-signed provider succeeds
/// with the test CA and fails (`fetch failed`) without it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn count_transport_speaks_https_with_the_shared_connector() {
    use pilotdeck_gateway::runtime::model::{
        CountHttpRequest, HyperCountTransport, ProviderCountTransport,
    };
    let (roots, server_config) = tls_material();
    let exchange = Arc::new(Mutex::new(Exchange::default()));
    let reply = parse_json(
        r#"{"status":200,"headers":{"content-type":"application/json"},"chunks":[{"text":"{\"input_tokens\":73}"}]}"#,
    )
    .unwrap();
    exchange.lock().unwrap().replies = vec![reply];
    let https = start_mock(
        exchange.clone(),
        vec!["host".into(), "content-length".into()],
        Some(server_config),
    );
    let request = |origin: &str| CountHttpRequest {
        url: format!("{origin}/v1/messages/count_tokens"),
        headers: JsObject::new(),
        body: parse_json(r#"{"model":"m"}"#).unwrap(),
    };
    let value = HyperCountTransport::with_roots(roots)
        .post(request(&https.origin))
        .await
        .unwrap();
    assert_eq!(get(&value, "input_tokens").as_number(), Some(73.0));
    {
        let exchange = exchange.lock().unwrap();
        assert_eq!(exchange.requests.len(), 1);
        assert_eq!(
            json(&JsValue::Object(exchange.requests[0].clone())),
            r#"{"method":"POST","path":"/v1/messages/count_tokens","headers":[],"body":"{\"model\":\"m\"}"}"#
        );
    }
    // Mozilla roots do not trust the throwaway CA: the handshake fails.
    let error = HyperCountTransport::default()
        .post(request(&https.origin))
        .await
        .unwrap_err();
    assert_eq!(error.module_error.unwrap().message, "fetch failed".into());
    assert_eq!(
        exchange.lock().unwrap().requests.len(),
        1,
        "no request after a failed handshake"
    );
}

/// #360-2: the retry-after dialect oracle regenerates byte-identically from
/// the real TS baseline (`tests/fixtures/retry-after-dialects/generate.mts`).
#[test]
#[ignore = "regeneration guard; needs PDRS_BASELINE and node v22.22.0"]
fn retry_after_dialects_regenerate_identically() {
    baseline::regenerates_identically("retry-after-dialects");
}

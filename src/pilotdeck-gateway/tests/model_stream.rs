//! Provider streaming replay against the fixed TypeScript baseline
//! (`tests/fixtures/model-stream/generate.mts`). The real TS `streamModel`
//! and this Rust port talk to the same scripted loopback mock (plain HTTP and
//! HTTPS with a throwaway CA); request bytes and the event/retry/throw
//! timeline are compared in order, byte for byte. No external traffic.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/model_mock.rs"]
mod model_mock;
#[path = "common/retry_after_dialects_guard.rs"]
mod retry_after_dialects_guard;
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
/// The two-dimension provenance gate — ICU 78.2 hard-pinned, a tzdata
/// mismatch failing with the `#360-2 BLOCKED` panic instead of the old
/// println-and-return skip (regen-r3 review P1: a returning skip left cargo
/// green on a host whose byte comparison never ran) — lives in
/// `common/retry_after_dialects_guard.rs`, shared with the twin
/// `generator_provenance.rs::retry_after_dialects_regenerates_identically`.
#[test]
#[ignore = "regeneration guard; needs PDRS_BASELINE and node v22.22.0 with ICU 78.2 on the gold-recorded tzdata"]
fn retry_after_dialects_regenerate_identically() {
    retry_after_dialects_guard::assert_calibrated_runtime_and_regenerate(None);
}

/// #360-2 regen-r3 (review P1): a tzdata-mismatched host must fail the
/// regeneration gate, never print a skip marker and return leaving cargo
/// green. Injects a well-formed but mismatched tzdata (the gold records 2025c
/// on the calibrated chain) and requires the `#360-2 BLOCKED` panic; the
/// `blocked-branch-return` negative control reverts the gate's branch to the
/// old println + return and is killed here on any host, without depending
/// on a real mismatched machine.
#[test]
#[ignore = "guard semantics; needs node v22.22.0 with ICU 78.2 on PATH (no PDRS_BASELINE)"]
fn retry_after_dialects_guard_fails_rather_than_skips_on_tzdata_mismatch() {
    let failure = std::panic::catch_unwind(|| {
        retry_after_dialects_guard::assert_calibrated_runtime_and_regenerate(Some("2039z"))
    })
    .expect_err(
        "#360-2 regen-r3: under an injected tzdata mismatch the gate must fail with \
         the '#360-2 BLOCKED' panic, never return and leave the byte comparison \
         silently skipped",
    );
    let message = failure
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| failure.downcast_ref::<&str>().copied())
        .unwrap_or("#360-2 regen-r3: gate panic payload was neither String nor &str");
    assert!(
        message.contains("#360-2 BLOCKED"),
        "#360-2 regen-r3: expected the '#360-2 BLOCKED' panic under an injected \
         tzdata mismatch, got {message:?}"
    );
}

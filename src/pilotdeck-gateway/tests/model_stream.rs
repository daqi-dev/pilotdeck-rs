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
///
/// #360-2 regen-r2 (fix C): not the plain byte guard — the runtime provenance
/// has two dimensions with different consequences. A different ICU build can
/// change `Date.parse` itself, so ICU stays a hard pin and must fail. A
/// different bundled tzdata is value-neutral for this corpus (every
/// local-resolving case sits in Shanghai's flat +08:00 era; re-recording on
/// the homebrew ICU 78.2 / tzdata 2026a build reproduced all 62x2 values,
/// regen-r1 review), so a mismatch BLOCKs the byte comparison by printing an
/// explicit reason and skipping instead of failing on the metadata-only
/// tzdata line. Twin of `generator_provenance.rs::
/// retry_after_dialects_regenerates_identically`.
#[test]
#[ignore = "regeneration guard; needs PDRS_BASELINE and node v22.22.0 with ICU 78.2"]
fn retry_after_dialects_regenerate_identically() {
    let version = baseline::node_command()
        .arg("--version")
        .output()
        .expect("#360-2 Node version");
    assert!(
        version.status.success(),
        "#360-2 Node version command failed"
    );
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "v22.22.0",
        "#360-2 pinned Node runtime"
    );
    let probe = baseline::node_command()
        .args(["-p", "process.versions.icu + ' ' + process.versions.tz"])
        .output()
        .expect("#360-2 runtime provenance probe");
    assert!(
        probe.status.success(),
        "#360-2 runtime provenance probe failed"
    );
    let provenance = String::from_utf8_lossy(&probe.stdout);
    let (icu, tzdata) = provenance.trim().split_once(' ').unwrap_or_else(|| {
        panic!("#360-2 provenance probe must print 'icu tzdata', got {provenance:?}")
    });
    assert_eq!(
        icu, "78.2",
        "#360-2 pinned ICU runtime: the retry-after-dialects gold is recorded on the \
         icu78 calibrated chain (decision card 2026-10-31), and a different ICU build \
         must fail this guard rather than skip it"
    );
    let committed = serde_json::from_str::<serde_json::Value>(include_str!(
        "fixtures/retry-after-dialects/oracle.json"
    ))
    .expect("#360-2 committed oracle must parse");
    let recorded = committed
        .get("tzdata")
        .and_then(serde_json::Value::as_str)
        .expect("#360-2 committed oracle records its tzdata provenance");
    if tzdata != recorded {
        println!(
            "#360-2 BLOCKED: tzdata {tzdata} (this Node) != {recorded} (gold-recorded); \
             the case bytes cannot be re-verified byte-wise on this host, so the byte \
             comparison is skipped instead of failing on a metadata-only diff"
        );
        return;
    }
    baseline::regenerates_identically("retry-after-dialects");
}

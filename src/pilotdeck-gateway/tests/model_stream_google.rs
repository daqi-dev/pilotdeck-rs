//! Google streaming replay against the fixed TypeScript baseline. The shared
//! HTTP/TLS mock compares request bytes and event/retry/error order exactly.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/model_mock.rs"]
mod model_mock;

const ORACLE: &str = include_str!("fixtures/model-stream-google/oracle.json");

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_ts_google_stream_model_over_loopback_http() {
    model_mock::replay(&baseline::oracle(ORACLE), 26, 26).await;
}

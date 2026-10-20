//! openai-responses streaming replay against the fixed TypeScript baseline
//! (`tests/fixtures/model-stream-responses/generate.mts`): the real TS
//! `streamModel` and this port talk to the same scripted loopback mock;
//! request bytes and the event/retry/throw timeline are compared in order,
//! byte for byte. No external traffic.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/model_mock.rs"]
mod model_mock;

const ORACLE: &str = include_str!("fixtures/model-stream-responses/oracle.json");

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_ts_openai_responses_stream_over_loopback_http() {
    model_mock::replay(&baseline::oracle(ORACLE), 21, 21).await;
}

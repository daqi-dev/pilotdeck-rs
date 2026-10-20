//! OpenAI chat-completions replay against the fixed TypeScript baseline
//! (`tests/fixtures/model-stream-openai/generate.mts`). The real TS
//! `streamModel` and this Rust port talk to the same scripted loopback mock
//! (shared `common/model_mock.rs`); request bytes and the event/retry/throw
//! timeline are compared in order, byte for byte. No external traffic.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/model_mock.rs"]
mod model_mock;

const ORACLE: &str = include_str!("fixtures/model-stream-openai/oracle.json");

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_ts_openai_stream_model_over_loopback_http() {
    model_mock::replay(&baseline::oracle(ORACLE), 30, 37).await;
}

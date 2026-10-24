//! Exercise request dumping through the real OpenAI and Google send paths.
//!
//! Each case runs in its own subprocess (no unsafe `set_var`/`remove_var`)
//! against a real mock transport, so `PILOTDECK_DUMP_REQUEST` is set exactly
//! for the code under test. The parent verifies only the exact dump paths
//! the child printed and removes only those files; it never scans the temp
//! directory, so other processes' dumps (and anything else) stay untouched.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use pilotdeck_gateway::runtime::model::http::{
    ModelHttpTransport, StreamOptions, StreamOutput, TransportFailure, TransportFuture,
    stream_model,
};
use pilotdeck_protocol::parse_json;
use serde_json::{Value, json};
use std::process::Command;

#[derive(Default)]
struct CaptureTransport(std::sync::Arc<std::sync::Mutex<Vec<Value>>>);

impl ModelHttpTransport for CaptureTransport {
    fn send(&self, request: Request<Full<Bytes>>) -> TransportFuture {
        let bodies = self.0.clone();
        TransportFuture::io(async move {
            let bytes = request.into_body().collect().await.unwrap().to_bytes();
            bodies
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&bytes).unwrap());
            // Dumping happens before the exchange; no real model or network needed.
            Err(TransportFailure::fetch_failed(Some("ECONNREFUSED")))
        })
    }
}

/// The dump paths the child reported on stdout, in order. Parsing the child's
/// own output is the only source: the parent never guesses or globs paths.
fn reported_dump_paths(stdout: &str) -> Vec<std::path::PathBuf> {
    stdout
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("[model-debug] Request dumped to ")?;
            let path = rest.strip_suffix(" (model=dump-model)")?;
            Some(std::path::PathBuf::from(path))
        })
        .collect()
}

fn run_case(protocol: &str, enabled: Option<&str>) {
    let marker = format!(
        "dump-marker-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "dump_child", "--ignored", "--nocapture"])
        .env("PDRS_DUMP_PROTOCOL", protocol)
        .env("PDRS_DUMP_MARKER", &marker)
        .env_remove("PILOTDECK_DUMP_REQUEST");
    if let Some(value) = enabled {
        command.env("PILOTDECK_DUMP_REQUEST", value);
    }
    let output = command.output().expect("run isolated dump case");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        output.status.success(),
        "{protocol}/{enabled:?}:\n{stdout}\n{stderr}"
    );
    let sent: Value = serde_json::from_str(
        stdout
            .lines()
            .find_map(|line| line.strip_prefix("SENT_BODY="))
            .expect("child must exercise the transport"),
    )
    .unwrap();
    assert_eq!(sent["dumpMarker"], marker);

    let dumps = reported_dump_paths(&stdout);
    if enabled == Some("1") {
        assert_eq!(
            dumps.len(),
            1,
            "{protocol}/{enabled:?}: expected exactly one dump: {stdout}"
        );
        let path = &dumps[0];
        let contents = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("read child-reported dump {path:?}: {error}"));
        assert_eq!(serde_json::from_str::<Value>(&contents).unwrap(), sent);
        assert_eq!(contents, serde_json::to_string_pretty(&sent).unwrap());
    } else {
        assert!(
            dumps.is_empty(),
            "{protocol}/{enabled:?} wrote a dump: {dumps:?}"
        );
        assert!(!stdout.contains("[model-debug]"), "{stdout}");
    }
    assert!(!stderr.contains("[model-debug]"), "{stderr}");
    // Clean up only the files this case's own child created and reported.
    for path in &dumps {
        let _ = std::fs::remove_file(path);
    }
}

#[test]
fn dump_request_creates_file() {
    for protocol in ["openai", "google"] {
        run_case(protocol, Some("1"));
    }
}

/// TS gates strictly on `=== "1"` (streamModel.ts:195, 394); anything else —
/// including "0" and "true" — must leave the dump path cold.
#[test]
fn dump_request_disabled_by_default() {
    for protocol in ["openai", "google"] {
        for value in [None, Some("0"), Some("true")] {
            run_case(protocol, value);
        }
    }
}

#[tokio::test]
#[ignore = "subprocess helper; run through the parent tests"]
async fn dump_child() {
    let protocol = std::env::var("PDRS_DUMP_PROTOCOL").unwrap();
    let marker = std::env::var("PDRS_DUMP_MARKER").unwrap();
    let request = parse_json(
        &json!({
            "provider": "dump-provider", "model": "dump-model",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "dump test"}]}]
        })
        .to_string(),
    )
    .unwrap();
    let config = parse_json(
        &json!({"providers": {"dump-provider": {
            "id": "dump-provider", "protocol": protocol,
            "url": "http://dump.invalid/v1", "apiKey": "test-only",
            "retry": {"streamMaxRetries": 0},
            "extraBody": {"dumpMarker": marker},
            "models": {"dump-model": {
                "id": "dump-model", "capabilities": {"supportsStreaming": true},
                "multimodal": {"input": ["text"]}
            }}
        }}})
        .to_string(),
    )
    .unwrap();
    let transport = CaptureTransport::default();
    let mut error_event = false;
    let result = stream_model(
        &transport,
        &request,
        &config,
        &StreamOptions::default(),
        &mut |output| {
            if let StreamOutput::Event(event) = output {
                error_event |= event
                    .get("type")
                    .and_then(|value| value.as_string())
                    .is_some_and(|kind| kind.equals_str("error"));
            }
        },
    )
    .await;
    assert!(
        result.is_err() || error_event,
        "transport failure must propagate as an error or a Google error event"
    );
    let bodies = transport.0.lock().unwrap();
    assert_eq!(
        bodies.len(),
        1,
        "must reach the provider send path once: {result:?}"
    );
    assert_eq!(bodies[0]["dumpMarker"], marker);
    println!("SENT_BODY={}", bodies[0]);
}

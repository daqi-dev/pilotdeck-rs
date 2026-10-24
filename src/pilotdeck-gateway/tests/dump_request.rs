//! Exercise request dumping through the real OpenAI and Google send paths.
//! Each case gets its own process environment (no unsafe set_var/remove_var).

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use pilotdeck_gateway::runtime::model::http::{
    ModelHttpTransport, StreamOptions, StreamOutput, TransportFailure, TransportFuture,
    stream_model,
};
use pilotdeck_protocol::parse_json;
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct CaptureTransport(Arc<Mutex<Vec<Value>>>);

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

fn dumps_containing(marker: &str) -> Vec<(PathBuf, String)> {
    fs::read_dir("/tmp")
        .expect("read dump directory")
        .filter_map(|entry| {
            let path = entry.expect("read directory entry").path();
            let name = path.file_name()?.to_str()?;
            if !name.starts_with("pilotdeck_request_") || !name.ends_with(".json") {
                return None;
            }
            // Other processes may remove their own dumps during this scan.
            let contents = fs::read_to_string(&path).ok()?;
            contents.contains(marker).then_some((path, contents))
        })
        .collect()
}

fn run_case(protocol: &str, enabled: Option<&str>) {
    let marker = uuid::Uuid::new_v4().to_string();
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
    let dumps = dumps_containing(&marker);
    // Remove only files carrying this case's fresh ownership marker.
    for (path, _) in &dumps {
        fs::remove_file(path).expect("remove owned dump");
    }
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
    if enabled == Some("1") {
        assert_eq!(dumps.len(), 1, "{protocol}: {stdout}\n{stderr}");
        let (path, contents) = &dumps[0];
        assert_eq!(serde_json::from_str::<Value>(contents).unwrap(), sent);
        assert_eq!(contents, &serde_json::to_string_pretty(&sent).unwrap());
        assert!(stdout.contains(&format!(
            "[model-debug] Request dumped to {} (model=dump-model)",
            path.display()
        )));
    } else {
        assert!(dumps.is_empty(), "{protocol}/{enabled:?} wrote a dump");
        assert!(!stdout.contains("[model-debug]"), "{stdout}");
    }
    assert!(!stderr.contains("[model-debug]"), "{stderr}");
}

#[test]
fn dump_request_creates_file() {
    for protocol in ["openai", "google"] {
        run_case(protocol, Some("1"));
    }
}

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

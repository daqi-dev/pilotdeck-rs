//! Real stream process, isolated wire peer, and bounded child lifetimes.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    time::{Instant, timeout},
};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn input() -> Value {
    json!({"sessionKey":"stream-test", "runId":"run-test", "channelKey":"cli", "message":"hello", "canPrompt":false})
}
fn child(root: &Path, url: &str, input: &Value) -> ChildGuard {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pilotdeck-control"))
        .env_clear()
        .current_dir(root)
        .args(["--url", url, "--token-file"])
        .arg(root.join("token"))
        .arg("stream")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    ChildGuard(child)
}
async fn collect(mut child: ChildGuard) -> (bool, Vec<Value>, String) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.0.try_wait().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "stream process did not exit");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut stdout) = child.0.stdout.take() {
        stdout.read_to_string(&mut out).unwrap();
    }
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    (
        child.0.wait().unwrap().success(),
        out.lines()
            .map(|s| serde_json::from_str(s).expect("JSONL only"))
            .collect(),
        err,
    )
}
type Ws = WebSocketStream<TcpStream>;
async fn frame(socket: &mut Ws) -> Message {
    timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("frame deadline")
        .expect("frame exists")
        .expect("frame valid")
}
async fn send(socket: &mut Ws, value: Value) {
    timeout(
        Duration::from_secs(5),
        socket.send(Message::Text(value.to_string().into())),
    )
    .await
    .unwrap()
    .unwrap();
}
async fn peer(listener: &TcpListener, token: &str) -> (Ws, String) {
    let (socket, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut socket = accept_async(socket).await.unwrap();
    let hello: Value = serde_json::from_str(frame(&mut socket).await.to_text().unwrap()).unwrap();
    assert_eq!(hello["token"], token);
    send(&mut socket, json!({"type":"hello_ok"})).await;
    let request: Value = serde_json::from_str(frame(&mut socket).await.to_text().unwrap()).unwrap();
    assert_eq!(request["method"], "submit_turn");
    assert_eq!(request["params"], input());
    (socket, request["id"].as_str().unwrap().to_owned())
}
fn event(id: &str, seq: Value, last: Value, payload: Value) -> Value {
    json!({"type":"event","id":id,"seq":seq,"final":last,"event":payload})
}
fn completed() -> Value {
    json!({"type":"turn_completed","finishReason":"completed"})
}

#[tokio::test(flavor = "current_thread")]
async fn stream_process_preserves_frames_and_fails_incomplete_results() {
    let root = std::env::temp_dir().join(format!("pdrs-control-stream-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let _scratch = Scratch(root.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    // Every identifier collision must leave the consumer's dispatch keys intact.
    for token in [
        "stream-secret-123",
        "type",
        "text",
        "assistant_text_delta",
        "turn_completed",
        "completed",
    ] {
        fs::write(root.join("token"), token).unwrap();
        let child = child(&root, &url, &input());
        let (mut socket, id) = peer(&listener, token).await;
        socket
            .send(Message::Ping(b"idle".to_vec().into()))
            .await
            .unwrap();
        assert_eq!(
            frame(&mut socket).await,
            Message::Pong(b"idle".to_vec().into()),
            "stream keeps heartbeat without events"
        );
        send(
            &mut socket,
            event("unrelated", json!(999), json!(false), completed()),
        )
        .await;
        send(
            &mut socket,
            event(
                &id,
                json!(0),
                json!(false),
                json!({"type":"assistant_text_delta","text":format!("result {token}"),"finishReason":{"type":token}}),
            ),
        )
        .await;
        send(&mut socket, event(&id, json!(1), json!(false), completed())).await;
        send(&mut socket, event(&id, json!(2), json!(true), completed())).await;
        let (ok, lines, err) = collect(child).await;
        assert!(ok, "{err}");
        assert!(err.is_empty());
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["event"]["type"], "assistant_text_delta");
        assert_eq!(lines[0]["event"]["text"], "result [REDACTED]");
        assert_eq!(lines[0]["event"]["finishReason"]["type"], "[REDACTED]");
        assert_eq!(lines[2]["event"], completed());
        assert_eq!(lines[1]["final"], false);
        assert_eq!(lines[2]["final"], true);
        for (seq, line) in lines.iter().enumerate() {
            assert_eq!(line["seq"], seq);
            assert_eq!(line["id"], id);
        }
    }
    fs::write(root.join("token"), "test-secret-123").unwrap();
    let cases = [
        "gap",
        "duplicate",
        "negative",
        "fractional",
        "missing-final",
        "missing-event",
        "error",
        "aborted",
        "unknown-final",
        "response-error",
        "response-ok",
        "disconnect",
    ];
    for case in cases {
        let child = child(&root, &url, &input());
        let (mut socket, id) = peer(&listener, "test-secret-123").await;
        match case {
            "gap" => send(&mut socket, event(&id, json!(1), json!(true), completed())).await,
            "duplicate" => {
                send(&mut socket, event(&id, json!(0), json!(false), completed())).await;
                send(&mut socket, event(&id, json!(0), json!(true), completed())).await;
            }
            "negative" => send(&mut socket, event(&id, json!(-1), json!(true), completed())).await,
            "fractional" => send(&mut socket, event(&id, json!(0.5), json!(true), completed())).await,
            "missing-final" => send(&mut socket, json!({"type":"event","id":id,"seq":0,"event":completed()})).await,
            "missing-event" => send(&mut socket, json!({"type":"event","id":id,"seq":0,"final":true})).await,
            "error" => send(&mut socket, event(&id,json!(0),json!(true),json!({"type":"error","code":"result_unknown","message":"test-secret-123"}))).await,
            "aborted" => send(&mut socket, event(&id,json!(0),json!(true),json!({"type":"turn_completed","finishReason":"aborted_streaming"}))).await,
            "unknown-final" => send(&mut socket, event(&id,json!(0),json!(true),json!({"type":"assistant_text_delta","text":"partial"}))).await,
            "response-error" => send(&mut socket,json!({"type":"response","id":id,"ok":false,"error":{"code":"denied","message":"test-secret-123"}})).await,
            "response-ok" => send(&mut socket,json!({"type":"response","id":id,"ok":true,"result":{}})).await,
            "disconnect" => { socket.close(None).await.unwrap(); }
            _ => unreachable!(),
        }
        let (ok, lines, err) = collect(child).await;
        assert!(!ok, "{case} falsely succeeded");
        assert!(!err.contains("test-secret-123"));
        let error: Value = serde_json::from_str(err.trim()).expect("structured stderr");
        let expected = match case {
            "error" => "turn_error",
            "aborted" => "turn_incomplete",
            "response-error" => "gateway_error",
            "disconnect" => "transport_error",
            _ => "protocol_error",
        };
        assert_eq!(error["kind"], expected, "{case}");
        if matches!(case, "error" | "aborted" | "unknown-final") {
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0]["final"], true);
        }
    }
    // Invalid inputs fail before opening a socket or causing a submit.
    for bad in [
        json!({}),
        json!({"sessionKey":"s","runId":"r","channelKey":"cli","message":"x","canPrompt":true}),
        json!({"sessionKey":"s","runId":"r","channelKey":"cli","message":"x","canPrompt":false,"interactionBinding":"spoof"}),
    ] {
        let (ok, lines, err) = collect(child(&root, &url, &bad)).await;
        assert!(!ok && lines.is_empty() && err.contains("input_error"));
        assert!(
            timeout(Duration::from_millis(40), listener.accept())
                .await
                .is_err(),
            "invalid input connected"
        );
    }
    // Broken output is not success and cannot trigger a second request.
    let mut child = child(&root, &url, &input());
    drop(child.0.stdout.take());
    let (mut socket, id) = peer(&listener, "test-secret-123").await;
    send(&mut socket, event(&id, json!(0), json!(true), completed())).await;
    let (ok, _, err) = collect(child).await;
    assert!(!ok && err.contains("output_error"));
    assert!(
        timeout(Duration::from_millis(80), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stalled_stdout_has_a_deadline_without_hanging_the_runtime() {
    let root = std::env::temp_dir().join(format!("pdrs-control-slow-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let _scratch = Scratch(root.clone());
    fs::write(root.join("token"), "slow-secret").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let child = child(&root, &url, &input());
    let (mut socket, id) = peer(&listener, "slow-secret").await;
    send(
        &mut socket,
        event(
            &id,
            json!(0),
            json!(false),
            json!({"type":"assistant_text_delta","text":"x".repeat(1024*1024)}),
        ),
    )
    .await;
    // Keep the pipe open but never drain until after exit, forcing actual backpressure.
    let mut child = child;
    let deadline = Instant::now() + Duration::from_secs(14);
    while child.0.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "blocked stdout kept process alive"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut err = String::new();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert!(!child.0.wait().unwrap().success());
    assert!(err.contains("output_error"));
}

#[tokio::test(flavor = "current_thread")]
async fn jsonl_limit_counts_the_newline_and_preserves_the_last_valid_size() {
    let root = std::env::temp_dir().join(format!("pdrs-control-size-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let _scratch = Scratch(root.clone());
    fs::write(root.join("token"), "size-secret").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    for allowed in [true, false] {
        let mut child = child(&root, &url, &input());
        let mut stdout = child.0.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let (mut socket, id) = peer(&listener, "size-secret").await;
        let base = json!({"version":"pilotdeck-control/1", "type":"event", "id":id,
            "seq":0, "final":false, "event":{"type":"assistant_text_delta","text":""}});
        let text_size = 8 * 1024 * 1024 - base.to_string().len() - usize::from(allowed);
        send(
            &mut socket,
            event(
                &id,
                json!(0),
                json!(false),
                json!({"type":"assistant_text_delta","text":"x".repeat(text_size)}),
            ),
        )
        .await;
        if allowed {
            send(&mut socket, event(&id, json!(1), json!(true), completed())).await;
        }
        let (ok, _, err) = collect(child).await;
        let output = reader.join().unwrap();
        assert_eq!(ok, allowed, "{err}");
        if allowed {
            assert_eq!(
                output.iter().position(|b| *b == b'\n').unwrap() + 1,
                8 * 1024 * 1024
            );
        } else {
            assert!(output.is_empty() && err.contains("output_error"));
        }
    }
}

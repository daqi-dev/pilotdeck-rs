//! Actual CLI processes against a bounded, exclusively owned loopback peer.
#![cfg(unix)]
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    time::{Instant, timeout},
};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-watch-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("token"), "watch-secret-123").unwrap();
        Self(root)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Process {
    child: Child,
    lines: mpsc::Receiver<String>,
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Process {
    fn start(root: &Path, args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_pilotdeck-control"))
            .env_clear()
            .env("HOME", root)
            .env("PILOT_HOME", root)
            .env("TMPDIR", root)
            .current_dir(root)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::sync_channel(32);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if line.len() > 1024 * 1024 || tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, lines: rx }
    }
    async fn line(&self) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.lines.try_recv() {
                Ok(line) => return serde_json::from_str(&line).expect("JSONL only"),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(e) => panic!("watch output closed: {e}"),
            };
            assert!(Instant::now() < deadline, "watch output deadline");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    async fn exit(&mut self) -> (bool, String) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "watch exit deadline");
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let mut err = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut err)
            .unwrap();
        (status.success(), err)
    }
    fn interrupt(&mut self) {
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(self.child.id() as i32).unwrap(),
            rustix::process::Signal::INT,
        )
        .unwrap();
    }
    fn unread(root: &Path, args: &[&str]) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_pilotdeck-control"))
            .env_clear()
            .env("HOME", root)
            .env("PILOT_HOME", root)
            .env("TMPDIR", root)
            .current_dir(root)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (_tx, rx) = mpsc::sync_channel(1);
        Self { child, lines: rx }
    }
}
type Socket = WebSocketStream<TcpStream>;
async fn frame(socket: &mut Socket) -> Value {
    let message = timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}
async fn send(socket: &mut Socket, value: Value) {
    timeout(
        Duration::from_secs(5),
        socket.send(Message::Text(value.to_string().into())),
    )
    .await
    .unwrap()
    .unwrap();
}
async fn attach(listener: &TcpListener) -> Socket {
    let (stream, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut socket = accept_async(stream).await.unwrap();
    let hello = frame(&mut socket).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["token"], "watch-secret-123");
    send(
        &mut socket,
        json!({"type":"hello_ok","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION}),
    )
    .await;
    socket
}
async fn snapshot(socket: &mut Socket, count: usize, notification: bool) {
    let request = frame(socket).await;
    assert_eq!(request["method"], "describe_server");
    assert_eq!(request["params"], json!({}));
    if notification {
        send(socket,json!({"type":"notification","name":"config_changed","payload":{"test":"watch-secret-123"}})).await;
    }
    send(socket,json!({"type":"response","id":request["id"],"ok":true,"result":{"capabilities":["run_get"],"sessionCount":count}})).await;
}

#[tokio::test(flavor = "current_thread")]
async fn real_watch_notifies_reconnects_resynchronizes_and_stops() {
    let scratch = Scratch::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let token = scratch.0.join("token");
    let mut child = Process::start(
        &scratch.0,
        &[
            "--url",
            &url,
            "--token-file",
            token.to_str().unwrap(),
            "watch",
        ],
    );
    let mut socket = attach(&listener).await;
    snapshot(&mut socket, 1, true).await;
    let initial = child.line().await;
    assert_eq!(initial["type"], "resync_required");
    assert_eq!(initial["reason"], "initial_attach");
    assert_eq!(initial["cursor"], Value::Null);
    let notification = child.line().await;
    assert_eq!(notification["type"], "notification");
    assert_eq!(notification["notification"]["name"], "config_changed");
    assert_eq!(
        notification["notification"]["payload"]["test"],
        "[REDACTED]"
    );
    assert_eq!(notification["duringResync"], true);
    let state = child.line().await;
    assert_eq!(state["scope"], "server");
    assert_eq!(state["state"]["sessionCount"], 1);
    assert_eq!(state["atomic"], false);
    let complete = child.line().await;
    assert_eq!(complete["type"], "resync_complete");
    assert_eq!(complete["notificationReplay"], false);
    socket.close(None).await.unwrap();
    let disconnected = child.line().await;
    assert_eq!(disconnected["type"], "disconnected");
    assert_eq!(disconnected["resyncRequired"], true);
    let mut socket = attach(&listener).await;
    snapshot(&mut socket, 2, false).await;
    let resync = child.line().await;
    assert_eq!(resync["type"], "resync_required");
    assert_eq!(resync["reason"], "reconnected");
    assert_eq!(resync["seq"], 0);
    assert_ne!(resync["epoch"], initial["epoch"]);
    let state = child.line().await;
    assert_eq!(state["state"]["sessionCount"], 2);
    let complete = child.line().await;
    assert_eq!(complete["type"], "resync_complete");
    child.interrupt();
    let stopped = child.line().await;
    assert_eq!(stopped["type"], "stopped");
    let (ok, err) = child.exit().await;
    assert!(ok, "{err}");
    assert!(err.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn real_watch_reads_selected_state_and_never_submits() {
    let scratch = Scratch::new();
    fs::write(
        scratch.0.join("selection"),
        r#"{"sessionKey":"s","runId":"r","projectKey":"p","intervalMs":60000}"#,
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let token = scratch.0.join("token");
    let input = scratch.0.join("selection");
    let mut child = Process::start(
        &scratch.0,
        &[
            "watch",
            "--url",
            &url,
            "--token-file",
            token.to_str().unwrap(),
            "--input-file",
            input.to_str().unwrap(),
        ],
    );
    let mut socket = attach(&listener).await;
    snapshot(&mut socket, 1, false).await;
    for (method, result) in [
        (
            "active_turn_snapshot",
            json!({"isActive":true,"turnId":"turn"}),
        ),
        ("run_get", json!({"status":"running","runId":"r"})),
    ] {
        let request = frame(&mut socket).await;
        assert_eq!(request["method"], method);
        assert_eq!(request["params"]["sessionKey"], "s");
        assert_eq!(request["params"]["projectKey"], "p");
        if method == "active_turn_snapshot" {
            assert_eq!(request["params"]["includeEvents"], false);
        }
        send(
            &mut socket,
            json!({"type":"response","id":request["id"],"ok":true,"result":result}),
        )
        .await;
    }
    let mut scopes = Vec::new();
    loop {
        let row = child.line().await;
        if row["type"] == "snapshot" {
            scopes.push(row["scope"].as_str().unwrap().to_owned());
        }
        if row["type"] == "resync_complete" {
            break;
        }
    }
    assert_eq!(scopes, vec!["server", "activeTurn", "run"]);
    child.interrupt();
    assert_eq!(child.line().await["type"], "stopped");
    assert!(child.exit().await.0);
}

#[tokio::test(flavor = "current_thread")]
async fn real_schema_is_local_without_credentials_and_watch_rejects_bad_notifications() {
    let scratch = Scratch::new();
    let mut schema = Process::start(&scratch.0, &["schema"]);
    let row = schema.line().await;
    assert_eq!(row["schemaVersion"], 1);
    assert_eq!(row["transport"], "gateway-ws");
    assert_eq!(row["live"], Value::Null);
    assert!(
        row["registeredMethods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "read_subagent_messages")
    );
    assert!(schema.exit().await.0);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let token = scratch.0.join("token");
    let mut child = Process::start(
        &scratch.0,
        &[
            "watch",
            "--url",
            &url,
            "--token-file",
            token.to_str().unwrap(),
        ],
    );
    let mut socket = attach(&listener).await;
    snapshot(&mut socket, 1, false).await;
    loop {
        if child.line().await["type"] == "resync_complete" {
            break;
        }
    }
    send(
        &mut socket,
        json!({"type":"notification","payload":{"secret":"watch-secret-123"}}),
    )
    .await;
    let (ok, err) = child.exit().await;
    assert!(!ok);
    let error: Value = serde_json::from_str(err.trim()).unwrap();
    assert_eq!(error["kind"], "protocol_error");
    assert!(!err.contains("watch-secret-123"));
    let mut child = Process::start(
        &scratch.0,
        &[
            "watch",
            "--url",
            &url,
            "--token-file",
            token.to_str().unwrap(),
        ],
    );
    let (stream, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut socket = accept_async(stream).await.unwrap();
    assert_eq!(frame(&mut socket).await["type"], "hello");
    socket
        .close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
            code: 4003.into(),
            reason: "auth_failed".into(),
        }))
        .await
        .unwrap();
    let (ok, err) = child.exit().await;
    assert!(!ok);
    let error: Value = serde_json::from_str(err.trim()).unwrap();
    assert_eq!(error["kind"], "authentication_error");
}

#[tokio::test(flavor = "current_thread")]
async fn real_watch_interrupt_is_bounded_with_unread_stdout() {
    let scratch = Scratch::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let token = scratch.0.join("token");
    let mut child = Process::unread(
        &scratch.0,
        &[
            "watch",
            "--url",
            &url,
            "--token-file",
            token.to_str().unwrap(),
        ],
    );
    let mut socket = attach(&listener).await;
    snapshot(&mut socket, 1, false).await;
    // One valid large JSONL exceeds pipe capacity. No reader is ever started.
    send(
        &mut socket,
        json!({"type":"notification","name":"fixture","payload":{"text":"x".repeat(1024*1024)}}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    child.interrupt();
    let (ok, err) = child.exit().await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!ok);
    let error: Value = serde_json::from_str(err.trim()).unwrap();
    assert_eq!(error["kind"], "output_error");
}

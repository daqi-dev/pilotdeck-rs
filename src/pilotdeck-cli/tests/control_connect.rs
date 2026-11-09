//! Bidirectional process boundary: a live stream and interactive RPCs must
//! share the authenticated connection; reconnect must never replay mutations.
#![cfg(unix)]
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    time::{Instant, timeout},
};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

struct Peer {
    child: Child,
    lines: mpsc::Receiver<String>,
    root: PathBuf,
}
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}
impl Peer {
    fn start(url: &str, unread: bool) -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-connect-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("token"), "connect-secret-123").unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_pilotdeck-control"))
            .env_clear()
            .env("HOME", &root)
            .env("PILOT_HOME", &root)
            .env("TMPDIR", &root)
            .current_dir(&root)
            .args(["connect", "--url", url, "--token-file"])
            .arg(root.join("token"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (sender, lines) = mpsc::sync_channel(32);
        if !unread {
            let stdout = child.stdout.take().unwrap();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else {
                        break;
                    };
                    if line.len() > 2 * 1024 * 1024 || sender.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        Self { child, lines, root }
    }
    fn write(&mut self, value: Value) {
        writeln!(self.child.stdin.as_mut().unwrap(), "{value}").unwrap();
    }
    async fn line(&self) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.lines.try_recv() {
                Ok(line) => return serde_json::from_str(&line).expect("JSON-RPC only"),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(error) => panic!("closed stdout: {error}"),
            }
            assert!(Instant::now() < deadline, "response deadline");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    async fn exit(&mut self) -> (bool, String) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                let mut stderr = String::new();
                self.child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut stderr)
                    .unwrap();
                return (status.success(), stderr);
            }
            assert!(Instant::now() < deadline, "exit deadline");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}
type Socket = WebSocketStream<TcpStream>;
async fn frame(socket: &mut Socket) -> Value {
    let frame = timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(frame.to_text().unwrap()).unwrap()
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
    assert_eq!(hello["token"], "connect-secret-123");
    send(
        &mut socket,
        json!({"type":"hello_ok", "protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION}),
    )
    .await;
    socket
}
fn rpc(id: Value, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})
}

#[tokio::test(flavor = "current_thread")]
async fn actual_connection_initializes_streams_decides_and_aborts() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let mut child = Peer::start(&url, false);
    let mut socket = attach(&listener).await;
    child.write(rpc(
        json!(1),
        "initialize",
        json!({"protocolVersion":"pilotdeck-connect/1"}),
    ));
    let request = frame(&mut socket).await;
    assert_eq!(request["method"], "describe_server");
    send(&mut socket, json!({"type":"response","id":request["id"],"ok":true,"result":{"capabilities":{"permission":true},"diagnostic":"connect-secret-123"}})).await;
    let init = child.line().await;
    assert_eq!(init["result"]["server"]["capabilities"]["permission"], true);
    assert_eq!(init["result"]["server"]["diagnostic"], "[REDACTED]");
    assert_eq!(init["result"]["mutationReplay"], false);
    child.write(rpc(
        json!("turn"),
        "run.submit",
        json!({"sessionKey":"s","runId":"r","message":"write","canPrompt":true}),
    ));
    let turn = frame(&mut socket).await;
    assert_eq!(turn["method"], "submit_turn");
    assert_eq!(turn["params"]["canPrompt"], true);
    send(&mut socket,json!({"type":"event","id":turn["id"],"seq":0,"final":false,"event":{"type":"permission_request","requestId":"ask","tool":"write"}})).await;
    assert_eq!(child.line().await["method"], "run.event");
    child.write(rpc(
        json!(2),
        "permission.respond",
        json!({"requestId":"ask","decision":"allow_once"}),
    ));
    let decision = frame(&mut socket).await;
    assert_eq!(decision["method"], "permission_decide");
    send(
        &mut socket,
        json!({"type":"response","id":decision["id"],"ok":true,"result":{"accepted":true}}),
    )
    .await;
    assert_eq!(child.line().await["id"], 2);
    child.write(rpc(
        json!(3),
        "run.cancel",
        json!({"sessionKey":"s","runId":"r"}),
    ));
    let cancel = frame(&mut socket).await;
    assert_eq!(cancel["method"], "abort_turn");
    send(
        &mut socket,
        json!({"type":"response","id":cancel["id"],"ok":true,"result":{}}),
    )
    .await;
    assert_eq!(child.line().await["id"], 3);
    send(&mut socket,json!({"type":"event","id":turn["id"],"seq":1,"final":true,"event":{"type":"turn_completed","finishReason":"cancelled"}})).await;
    assert_eq!(child.line().await["params"]["final"], true);
    let final_result = child.line().await;
    assert_eq!(final_result["id"], "turn");
    assert_eq!(final_result["error"]["data"]["finishReason"], "cancelled");
    assert_eq!(final_result["error"]["code"], -32003);
    drop(child.child.stdin.take());
    assert!(child.exit().await.0);
    assert!(
        timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_requests_and_duplicate_ids_do_not_mutate_or_replace_pending() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut child = Peer::start(
        &format!("ws://{}/ws", listener.local_addr().unwrap()),
        false,
    );
    let mut socket = attach(&listener).await;
    writeln!(child.child.stdin.as_mut().unwrap(), "{{broken").unwrap();
    assert_eq!(child.line().await["error"]["code"], -32700);
    child.write(rpc(
        json!(1),
        "initialize",
        json!({"protocolVersion":"app/v1"}),
    ));
    assert_eq!(child.line().await["error"]["code"], -32602);
    child.write(rpc(
        json!(2),
        "gateway.call",
        json!({"method":"run_get","params":{"sessionKey":"s","runId":"r"}}),
    ));
    let request = frame(&mut socket).await;
    child.write(rpc(
        json!(2),
        "run.submit",
        json!({"message":"must not run"}),
    ));
    assert_eq!(child.line().await["error"]["code"], -32600);
    send(
        &mut socket,
        json!({"type":"response","id":request["id"],"ok":true,"result":{"status":"completed"}}),
    )
    .await;
    assert_eq!(child.line().await["result"]["status"], "completed");
    socket.close(None).await.unwrap();
    let (ok, error) = child.exit().await;
    assert!(!ok);
    assert!(error.contains("transport_error"));
    assert!(
        timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn interrupt_closes_owned_connection_even_when_stdout_is_unread() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut child = Peer::start(&format!("ws://{}/ws", listener.local_addr().unwrap()), true);
    let mut socket = attach(&listener).await;
    send(
        &mut socket,
        json!({"type":"notification","name":"fixture","payload":{"data":"x".repeat(1024*1024)}}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(child.child.id() as i32).unwrap(),
        rustix::process::Signal::TERM,
    )
    .unwrap();
    assert!(child.exit().await.0);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test(flavor = "current_thread")]
async fn complete_and_incomplete_turns_have_distinct_rpc_results() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut child = Peer::start(
        &format!("ws://{}/ws", listener.local_addr().unwrap()),
        false,
    );
    let mut socket = attach(&listener).await;
    for reason in ["completed", "max_tokens", "aborted"] {
        child.write(rpc(
            json!(reason),
            "run.submit",
            json!({"sessionKey":"s","runId":reason,"message":"fixture"}),
        ));
        let turn = frame(&mut socket).await;
        send(&mut socket,json!({"type":"event","id":turn["id"],"seq":0,"final":true,"event":{"type":"turn_completed","finishReason":reason}})).await;
        assert_eq!(child.line().await["method"], "run.event");
        let response = child.line().await;
        if reason == "completed" {
            assert_eq!(response["result"]["terminal"]["finishReason"], reason);
            assert!(response.get("error").is_none());
        } else {
            assert_eq!(response["error"]["code"], -32003);
            assert_eq!(response["error"]["data"]["finishReason"], reason);
            assert!(response.get("result").is_none());
        }
    }
    drop(child.child.stdin.take());
    assert!(child.exit().await.0);
}

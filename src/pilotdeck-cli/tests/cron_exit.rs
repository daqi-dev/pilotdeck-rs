//! Real subprocesses against an isolated fake discovery server and Gateway.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const LEASE: &str = "/tmp/pdrs-cron-18789.lock";
struct Lease;
impl Lease {
    async fn acquire() -> Self {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            match fs::create_dir(LEASE) {
                Ok(()) => return Self,
                Err(e)
                    if e.kind() == std::io::ErrorKind::AlreadyExists
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await
                }
                Err(e) => panic!("cron port lease unavailable: {e}"),
            }
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = fs::remove_dir(LEASE);
    }
}
struct TempRoot(PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn spawn(root: &Path, args: &[&str]) -> OwnedChild {
    OwnedChild(
        Command::new(env!("CARGO_BIN_EXE_pilotdeck"))
            .arg("cron")
            .args(args)
            .current_dir(root)
            .env_clear()
            .env("PILOT_HOME", root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    )
}
async fn output(mut child: OwnedChild) -> Output {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while child.0.try_wait().unwrap().is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "cron child did not exit"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut out = Vec::new();
    let mut err = Vec::new();
    use std::io::Read;
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut out)
        .unwrap();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut err)
        .unwrap();
    Output {
        status: child.0.wait().unwrap(),
        stdout: out,
        stderr: err,
    }
}
async fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 1024];
            let n = stream.read(&mut chunk).await.expect("HTTP read failed");
            assert!(n > 0, "HTTP request ended before headers");
            bytes.extend_from_slice(&chunk[..n]);
            assert!(bytes.len() <= 65536, "HTTP request headers too large");
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                return bytes;
            }
        }
    })
    .await
    .expect("HTTP request headers timed out")
}
async fn accept_http(listener: &TcpListener, expected_path: &str) -> TcpStream {
    let (mut stream, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("health accept timed out")
        .unwrap();
    let request = read_request(&mut stream).await;
    let expected = format!("GET {expected_path} HTTP/1.1\r\n");
    assert!(
        request.starts_with(expected.as_bytes()),
        "unexpected HTTP request: {}",
        String::from_utf8_lossy(&request)
    );
    stream
}
async fn healthy_path(listener: &TcpListener, path: &str) {
    let mut stream = accept_http(listener, path).await;
    stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
}
async fn healthy(listener: &TcpListener) {
    healthy_path(listener, "/health").await;
}
async fn next_text(ws: &mut tokio_tungstenite::WebSocketStream<TcpStream>) -> Message {
    timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("WS frame timed out")
        .expect("WS stream ended")
        .expect("WS frame error")
}
async fn socket(listener: &TcpListener) -> tokio_tungstenite::WebSocketStream<TcpStream> {
    let (stream, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ws = timeout(Duration::from_secs(5), accept_async(stream))
        .await
        .expect("WS upgrade timed out")
        .unwrap();
    let hello: Value = serde_json::from_str(next_text(&mut ws).await.to_text().unwrap()).unwrap();
    assert_eq!(
        hello,
        json!({"type":"hello","protocolVersion":"1.2","clientName":"cli","clientVersion":"0.1.0","token":"test-token"})
    );
    ws
}
async fn send(ws: &mut tokio_tungstenite::WebSocketStream<TcpStream>, text: String) {
    timeout(Duration::from_secs(10), ws.send(Message::Text(text.into())))
        .await
        .expect("WS send timed out")
        .expect("WS send failed");
}
async fn hello(ws: &mut tokio_tungstenite::WebSocketStream<TcpStream>) {
    send(ws, json!({"type":"hello_ok"}).to_string()).await;
}
async fn closed(ws: &mut tokio_tungstenite::WebSocketStream<TcpStream>) {
    let result = timeout(Duration::from_secs(10), ws.next())
        .await
        .expect("connection retained after terminal outcome");
    assert!(
        matches!(result, Some(Ok(Message::Close(_))) | None),
        "{result:?}"
    );
}
fn failure(out: Output, message: &str) {
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        format!("{message}\n")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cron_real_process_discovery_and_wire_contract() {
    let _lease = Lease::acquire().await;
    // Fail before spawning if any other service owns the fixed port.
    let listener = TcpListener::bind("127.0.0.1:18789")
        .await
        .expect("isolated test requires unoccupied port 18789");
    let root = std::env::temp_dir().join(format!("pdrs-cron-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let _root_guard = TempRoot(root.clone());
    fs::write(root.join("server-token"), "  test-token\n").unwrap();
    let oracle: Value = serde_json::from_str(include_str!("fixtures/cron/ts-oracle.json")).unwrap();
    let overflow = "9".repeat(310);
    let cases = [
        ("list", vec!["list", "--history", "--limit", "12junk"]),
        (
            "create",
            vec![
                "create",
                "--session",
                "slack:abc",
                "--message",
                "hi",
                "--once",
                "later",
                "--cron",
                "ignored",
                "--timezone",
                "UTC",
            ],
        ),
        (
            "delete",
            vec!["delete", "--task", "actual", "--stop-running"],
        ),
        ("stop", vec!["stop", "--run", "run-1"]),
        (
            "create-explicit",
            vec![
                "create",
                "--session",
                "abc",
                "--message",
                "hi",
                "--once",
                "now",
                "--channel",
                "custom",
                "--project",
                "/p",
            ],
        ),
        (
            "create-plain",
            vec![
                "create",
                "--session",
                "abc",
                "--message",
                "hi",
                "--once",
                "now",
            ],
        ),
        ("list-invalid", vec!["list", "--limit", "abc"]),
        ("list-overflow", vec!["list", "--limit", overflow.as_str()]),
        ("stop-empty", vec!["stop", "", "--run", "r"]),
    ];
    for (id, args) in cases {
        let baseline = &oracle["cases"][id];
        let expected_request = &baseline["wire"][1];
        let mut expected = expected_request["params"].clone();
        if expected.get("projectKey").and_then(Value::as_str) == Some("<cwd>") {
            expected["projectKey"] = root.to_string_lossy().to_string().into();
        }
        let method = expected_request["method"].as_str().unwrap();
        let child = spawn(&root, &args);
        healthy(&listener).await;
        let mut ws = socket(&listener).await;
        hello(&mut ws).await;
        let request: Value =
            serde_json::from_str(next_text(&mut ws).await.to_text().unwrap()).unwrap();
        assert_eq!(
            request["method"], method,
            "cron method differs from TS fixture"
        );
        assert_eq!(request["params"], expected);
        let request_id = request["id"].as_str().expect("request id must be a string");
        let uuid = uuid::Uuid::parse_str(request_id).expect("request id must be a UUID");
        assert_eq!(uuid.get_version_num(), 4, "request id must be UUID v4");
        assert_eq!(
            request,
            json!({"type":"request", "id":request_id, "method":method, "params":expected}),
            "request envelope differs from TS fixture"
        );
        send(
            &mut ws,
            json!({"type":"response","id":request["id"],"ok":true,"result":{"ok":true}})
                .to_string(),
        )
        .await;
        closed(&mut ws).await;
        let out = output(child).await;
        assert!(out.status.success());
        assert_eq!(out.stdout, baseline["stdout"].as_str().unwrap().as_bytes());
        assert!(out.stderr.is_empty());
    }
    // Redirected HTTP health still connects the fixed WebSocket endpoint.
    let child = spawn(&root, &["create"]);
    let mut stream = accept_http(&listener, "/health").await;
    stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: /ready\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
    drop(stream);
    healthy_path(&listener, "/ready").await;
    let mut ws = socket(&listener).await;
    hello(&mut ws).await;
    closed(&mut ws).await;
    failure(
        output(child).await,
        oracle["cases"]["usage"]["stderr"]
            .as_str()
            .unwrap()
            .trim_end_matches('\n'),
    );
    // Empty --message is rejected only after the legacy hello.
    let child = spawn(
        &root,
        &[
            "create",
            "--session",
            "abc",
            "--message",
            "",
            "--once",
            "now",
        ],
    );
    healthy(&listener).await;
    let mut ws = socket(&listener).await;
    hello(&mut ws).await;
    closed(&mut ws).await;
    failure(
        output(child).await,
        oracle["cases"]["create-empty"]["stderr"]
            .as_str()
            .unwrap()
            .trim_end_matches('\n'),
    );
    let child = spawn(&root, &["list"]);
    healthy(&listener).await;
    let mut ws = socket(&listener).await;
    hello(&mut ws).await;
    let request: Value = serde_json::from_str(next_text(&mut ws).await.to_text().unwrap()).unwrap();
    send(&mut ws, json!({"type":"response","id":request["id"],"ok":false,"error":{"message":"fixture denied"}}).to_string()).await;
    closed(&mut ws).await;
    failure(
        output(child).await,
        oracle["cases"]["rpc-error"]["stderr"]
            .as_str()
            .unwrap()
            .trim_end_matches('\n'),
    );
    // The pinned TS cron probe accepted 17 MiB; the separate SDK probe of the
    // same Node WebSocket accepted 100 MiB+1. Cover that budget here too.
    let child = spawn(&root, &["create"]);
    healthy(&listener).await;
    let mut ws = socket(&listener).await;
    let large_hello =
        json!({"type":"hello_ok", "padding":"x".repeat(100 * 1024 * 1024 + 1)}).to_string();
    send(&mut ws, large_hello).await;
    closed(&mut ws).await;
    failure(
        output(child).await,
        oracle["cases"]["usage"]["stderr"]
            .as_str()
            .unwrap()
            .trim_end_matches('\n'),
    );
    // A declared frame over the Rust limit is rejected from its header alone.
    let child = spawn(&root, &["list"]);
    healthy(&listener).await;
    let mut ws = socket(&listener).await;
    let mut oversized_header = vec![0x81, 0x7f];
    oversized_header.extend_from_slice(&(128_u64 * 1024 * 1024 + 1).to_be_bytes());
    timeout(
        Duration::from_secs(5),
        ws.get_mut().write_all(&oversized_header),
    )
    .await
    .expect("oversized header write timed out")
    .unwrap();
    failure(
        output(child).await,
        oracle["cases"]["http-503"]["stderr"]
            .as_str()
            .unwrap()
            .trim_end_matches('\n'),
    );
    // A valid unanswered RPC remains pending; cleanup applies only to terminal outcomes.
    let mut child = spawn(&root, &["list"]);
    healthy(&listener).await;
    let mut ws = socket(&listener).await;
    hello(&mut ws).await;
    let request: Value = serde_json::from_str(next_text(&mut ws).await.to_text().unwrap()).unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "pending RPC returned early"
    );
    send(
        &mut ws,
        json!({"type":"response","id":request["id"],"ok":true,"result":{}}).to_string(),
    )
    .await;
    closed(&mut ws).await;
    assert!(output(child).await.status.success());
    let unavailable = oracle["cases"]["http-503"]["stderr"]
        .as_str()
        .unwrap()
        .trim_end_matches('\n');
    // Undici stops after receiving the twenty-first redirect response.
    let child = spawn(&root, &["list"]);
    for _ in 0..21 {
        let mut stream = accept_http(&listener, "/health").await;
        stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: /health\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
    }
    failure(output(child).await, unavailable);
    let child = spawn(&root, &["create"]);
    let mut stream = accept_http(&listener, "/health").await;
    stream
        .write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    drop(stream);
    failure(output(child).await, unavailable);
    // Health can succeed while the WebSocket upgrade is rejected.
    let child = spawn(&root, &["list"]);
    healthy(&listener).await;
    let (mut upgrade, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let request = read_request(&mut upgrade).await;
    assert!(request.starts_with(b"GET /ws "));
    upgrade
        .write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    drop(upgrade);
    failure(output(child).await, unavailable);
    let child = spawn(&root, &["list"]);
    healthy(&listener).await;
    let mut ws = socket(&listener).await;
    timeout(Duration::from_secs(5), ws.close(None))
        .await
        .expect("WS close timed out")
        .unwrap();
    failure(output(child).await, unavailable);
    let child = spawn(&root, &["list"]);
    let mut stream = accept_http(&listener, "/health").await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let _ = stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
        .await;
    failure(output(child).await, unavailable);
    drop(stream);
    fs::remove_file(root.join("server-token")).unwrap();
    let child = spawn(&root, &["list"]);
    healthy(&listener).await;
    failure(output(child).await, unavailable);
}

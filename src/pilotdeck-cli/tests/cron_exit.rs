//! Real subprocesses against an isolated fake discovery server and Gateway.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Child, Command, Output, Stdio},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

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
async fn accept_http(listener: &TcpListener) -> TcpStream {
    let (mut stream, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut request = [0; 4096];
    let n = stream.read(&mut request).await.unwrap();
    assert!(String::from_utf8_lossy(&request[..n]).starts_with("GET /"));
    stream
}
async fn healthy(listener: &TcpListener) {
    let mut stream = accept_http(listener).await;
    stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
}
async fn socket(listener: &TcpListener) -> tokio_tungstenite::WebSocketStream<TcpStream> {
    let (stream, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ws = accept_async(stream).await.unwrap();
    let hello: Value =
        serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(
        hello,
        json!({"type":"hello","protocolVersion":"1.2","clientName":"cli","clientVersion":"0.1.0","token":"test-token"})
    );
    ws
}
async fn hello(ws: &mut tokio_tungstenite::WebSocketStream<TcpStream>) {
    ws.send(Message::Text(json!({"type":"hello_ok"}).to_string().into()))
        .await
        .unwrap();
}
async fn closed(ws: &mut tokio_tungstenite::WebSocketStream<TcpStream>) {
    let result = timeout(Duration::from_secs(3), ws.next())
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
    // Fail before spawning if any other service owns the fixed port.
    let listener = TcpListener::bind("127.0.0.1:18789")
        .await
        .expect("isolated test requires unoccupied port 18789");
    let root = std::env::temp_dir().join(format!("pdrs-cron-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    fs::write(root.join("server-token"), "  test-token\n").unwrap();
    let oracle: Value = serde_json::from_str(include_str!("fixtures/cron/ts-oracle.json")).unwrap();
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
    ];
    for (id, args) in cases {
        let baseline = &oracle["cases"][id];
        let expected_request = &baseline["wire"][1];
        let mut expected = expected_request["params"].clone();
        if id == "create" {
            expected["projectKey"] = root.to_string_lossy().to_string().into();
        }
        let method = expected_request["method"].as_str().unwrap();
        let child = spawn(&root, &args);
        healthy(&listener).await;
        let mut ws = socket(&listener).await;
        hello(&mut ws).await;
        let request: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(request["method"], method);
        assert_eq!(request["params"], expected);
        ws.send(Message::Text(
            json!({"type":"response","id":request["id"],"ok":true,"result":{"ok":true}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        closed(&mut ws).await;
        let out = output(child).await;
        assert!(out.status.success());
        assert_eq!(out.stdout, baseline["stdout"].as_str().unwrap().as_bytes());
        assert!(out.stderr.is_empty());
    }
    // Redirected HTTP health still connects the fixed WebSocket endpoint.
    let child = spawn(&root, &["create"]);
    let mut stream = accept_http(&listener).await;
    stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: /ready\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
    drop(stream);
    healthy(&listener).await;
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
    let child = spawn(&root, &["list"]);
    healthy(&listener).await;
    let mut ws = socket(&listener).await;
    hello(&mut ws).await;
    let request: Value =
        serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    ws.send(Message::Text(json!({"type":"response","id":request["id"],"ok":false,"error":{"message":"fixture denied"}}).to_string().into())).await.unwrap();
    closed(&mut ws).await;
    failure(
        output(child).await,
        oracle["cases"]["rpc-error"]["stderr"]
            .as_str()
            .unwrap()
            .trim_end_matches('\n'),
    );
    // A valid unanswered RPC remains pending; cleanup applies only to terminal outcomes.
    let mut child = spawn(&root, &["list"]);
    healthy(&listener).await;
    let mut ws = socket(&listener).await;
    hello(&mut ws).await;
    let request: Value =
        serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "pending RPC returned early"
    );
    ws.send(Message::Text(
        json!({"type":"response","id":request["id"],"ok":true,"result":{}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    closed(&mut ws).await;
    assert!(output(child).await.status.success());
    let unavailable = oracle["cases"]["http-503"]["stderr"]
        .as_str()
        .unwrap()
        .trim_end_matches('\n');
    let child = spawn(&root, &["create"]);
    let mut stream = accept_http(&listener).await;
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
    let mut request = [0; 4096];
    let count = upgrade.read(&mut request).await.unwrap();
    assert!(String::from_utf8_lossy(&request[..count]).starts_with("GET /ws "));
    upgrade
        .write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    drop(upgrade);
    failure(output(child).await, unavailable);
    let child = spawn(&root, &["list"]);
    healthy(&listener).await;
    let mut ws = socket(&listener).await;
    ws.close(None).await.unwrap();
    failure(output(child).await, unavailable);
    let child = spawn(&root, &["list"]);
    let stream = accept_http(&listener).await;
    failure(output(child).await, unavailable);
    drop(stream);
    fs::remove_file(root.join("server-token")).unwrap();
    let child = spawn(&root, &["list"]);
    healthy(&listener).await;
    failure(output(child).await, unavailable);
    fs::remove_dir_all(root).unwrap();
}

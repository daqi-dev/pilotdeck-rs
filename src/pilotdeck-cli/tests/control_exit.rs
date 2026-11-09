//! Exercise the real control binary over an isolated Gateway wire peer.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Child, Command, Output, Stdio},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

const TOKEN: &str = "control-test-secret-7db9";
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn spawn(root: &Path, url: &str, args: &[&str], input: Option<&str>) -> OwnedChild {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pilotdeck-control"));
    cmd.env_clear()
        .current_dir(root)
        .args(["--url", url, "--token-file"])
        .arg(root.join("token"))
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    OwnedChild(child)
}
async fn output(mut child: OwnedChild) -> Output {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while child.0.try_wait().unwrap().is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "control child did not exit"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    use std::io::Read;
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();
    Output {
        status: child.0.wait().unwrap(),
        stdout,
        stderr,
    }
}
async fn ws(listener: &TcpListener, token: &str) -> WebSocketStream<TcpStream> {
    let (stream, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ws = timeout(Duration::from_secs(5), accept_async(stream))
        .await
        .unwrap()
        .unwrap();
    let hello = next(&mut ws).await;
    assert_eq!(
        hello,
        json!({"type":"hello","protocolVersion":"1.2","clientName":"cli","clientVersion":"0.1.0","token":token})
    );
    ws
}
async fn next(ws: &mut WebSocketStream<TcpStream>) -> Value {
    let frame = timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(frame.to_text().unwrap()).unwrap()
}
async fn send(ws: &mut WebSocketStream<TcpStream>, value: Value) {
    timeout(
        Duration::from_secs(5),
        ws.send(Message::Text(value.to_string().into())),
    )
    .await
    .unwrap()
    .unwrap();
}
fn error(output: Output, kind: &str) -> Value {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let text = String::from_utf8(output.stderr).unwrap();
    assert!(!text.contains(TOKEN));
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["kind"], kind);
    value
}

#[tokio::test(flavor = "current_thread")]
async fn real_binary_matches_wire_and_failure_contract() {
    let root = std::env::temp_dir().join(format!("pdrs-control-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("token"), format!("{TOKEN}\n")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    for command in ["status", "capabilities", "call"] {
        let input = (command == "call").then_some(
            r#"{"method":"list_sessions","params":{"projectKey":"isolated","nested":[null,1]}}"#,
        );
        let child = spawn(&root, &url, &[command], input);
        let mut socket = ws(&listener, TOKEN).await;
        send(
            &mut socket,
            json!({"type":"hello_ok","serverInfo":{"capabilities":["unused"]}}),
        )
        .await;
        let request = next(&mut socket).await;
        assert_eq!(request["type"], "request");
        assert_eq!(
            uuid::Uuid::parse_str(request["id"].as_str().unwrap())
                .unwrap()
                .get_version_num(),
            4
        );
        assert_eq!(
            request["method"],
            if command == "call" {
                "list_sessions"
            } else {
                "describe_server"
            }
        );
        assert_eq!(
            request["params"],
            if command == "call" {
                json!({"projectKey":"isolated","nested":[null,1]})
            } else {
                json!({})
            }
        );
        send(
            &mut socket,
            json!({"type":"response","id":"unrelated","ok":true,"result":{"wrong":true}}),
        )
        .await;
        let result = json!({"mode":"native","capabilities":["actual"],"sessionCount":0});
        send(
            &mut socket,
            json!({"type":"response","id":request["id"],"ok":true,"result":result}),
        )
        .await;
        let out = output(child).await;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stderr.is_empty());
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        if command == "status" {
            assert_eq!(
                value,
                json!({"version":"pilotdeck-control/1","source":"gateway.describe_server","server":result})
            );
        } else if command == "capabilities" {
            assert_eq!(value, json!(["actual"]));
        } else {
            assert_eq!(value, json!({"ok":true,"result":result}));
        }
        assert!(
            timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_err(),
            "unexpected retry"
        );
    }
    let child = spawn(
        &root,
        &url,
        &["call"],
        Some(r#"{"method":"delete_session","params":{"sessionKey":"s"}}"#),
    );
    let mut socket = ws(&listener, TOKEN).await;
    send(&mut socket, json!({"type":"hello_ok"})).await;
    let request = next(&mut socket).await;
    send(&mut socket,json!({"type":"response","id":request["id"],"ok":false,"error":{"code":"conflict","message":"denied","details":{"revision":3}}})).await;
    let err = error(output(child).await, "gateway_error");
    assert_eq!(
        err["error"],
        json!({"code":"conflict","message":"denied","details":{"revision":3}})
    );
    assert!(
        timeout(Duration::from_millis(200), listener.accept())
            .await
            .is_err(),
        "side effect was retried"
    );
    // A successful void result is distinct from an explicit JSON null.
    for (response, expected) in [
        (json!({}), json!({"ok":true})),
        (json!({"result":null}), json!({"ok":true,"result":null})),
    ] {
        let child = spawn(
            &root,
            &url,
            &["call"],
            Some(r#"{"method":"close_session","params":{"sessionKey":"s"}}"#),
        );
        let mut socket = ws(&listener, TOKEN).await;
        send(&mut socket, json!({"type":"hello_ok"})).await;
        let request = next(&mut socket).await;
        let mut frame = json!({"type":"response","id":request["id"],"ok":true});
        if let Some(result) = response.get("result") {
            frame["result"] = result.clone();
        }
        send(&mut socket, frame).await;
        let out = output(child).await;
        assert!(out.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&out.stdout).unwrap(),
            expected
        );
    }
    // A one-character token must not reject unrelated ordinary result text.
    fs::write(root.join("token"), "a\n").unwrap();
    for command in ["status", "capabilities", "call"] {
        let child = spawn(
            &root,
            &url,
            &[command],
            (command == "call").then_some(r#"{"method":"list_sessions","params":{}}"#),
        );
        let mut socket = ws(&listener, "a").await;
        send(&mut socket, json!({"type":"hello_ok"})).await;
        let request = next(&mut socket).await;
        send(&mut socket,json!({"type":"response","id":request["id"],"ok":true,"result":{"capabilities":["actual"]}})).await;
        let out = output(child).await;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        let text = serde_json::to_string(&value).unwrap();
        assert!(text.contains("actual"));
    }
    // A token equal to the Gateway lookup key must not alter parsing.
    fs::write(root.join("token"), "capabilities\n").unwrap();
    let child = spawn(&root, &url, &["capabilities"], None);
    let mut socket = ws(&listener, "capabilities").await;
    send(&mut socket, json!({"type":"hello_ok"})).await;
    let request = next(&mut socket).await;
    send(&mut socket, json!({"type":"response","id":request["id"],"ok":true,"result":{"capabilities":["actual", "capabilities"]}})).await;
    let out = output(child).await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap(),
        json!(["actual", "[REDACTED]"])
    );
    fs::write(root.join("token"), "a\n").unwrap();
    // Literal credential echoes in values and keys are redacted, preserving
    // success/error shapes and unrelated strings (including short-token words).
    let child = spawn(
        &root,
        &url,
        &["call"],
        Some(r#"{"method":"list_sessions","params":{}}"#),
    );
    let mut socket = ws(&listener, "a").await;
    send(&mut socket, json!({"type":"hello_ok"})).await;
    let request = next(&mut socket).await;
    send(&mut socket, json!({"type":"response","id":request["id"],"ok":true,"result":{"a":"a","text":"actual a, data","items":["a","actual"]}})).await;
    let success = output(child).await;
    assert!(success.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&success.stdout).unwrap(),
        json!({"ok":true,"result":{"[REDACTED]":"[REDACTED]","text":"actual [REDACTED], data","items":["[REDACTED]","actual"]}})
    );
    let child = spawn(
        &root,
        &url,
        &["call"],
        Some(r#"{"method":"list_sessions","params":{}}"#),
    );
    let mut socket = ws(&listener, "a").await;
    send(&mut socket, json!({"type":"hello_ok"})).await;
    let request = next(&mut socket).await;
    send(&mut socket, json!({"type":"response","id":request["id"],"ok":false,"error":{"code":"actual","message":"bad a, actual"}})).await;
    let failure = error(output(child).await, "gateway_error");
    assert_eq!(
        failure["error"],
        json!({"code":"actual","message":"bad [REDACTED], actual"})
    );
    fs::write(root.join("token"), format!("{TOKEN}\n")).unwrap();
    let child = spawn(
        &root,
        &url,
        &["call"],
        Some(r#"{"method":"list_sessions","params":{}}"#),
    );
    let mut socket = ws(&listener, TOKEN).await;
    send(&mut socket, json!({"type":"hello_ok"})).await;
    let request = next(&mut socket).await;
    send(&mut socket, json!({"type":"response","id":request["id"],"ok":false,"error":{"code":"denied","message":format!("prefix{TOKEN}suffix"),"details":{"token":TOKEN}}})).await;
    let failure = error(output(child).await, "gateway_error");
    assert_eq!(
        failure["error"],
        json!({"code":"denied","message":"prefix[REDACTED]suffix","details":{"token":"[REDACTED]"}})
    );
    let child = spawn(&root, &url, &["status"], None);
    let mut socket = ws(&listener, TOKEN).await;
    timeout(Duration::from_secs(5), socket.close(None))
        .await
        .unwrap()
        .unwrap();
    error(output(child).await, "transport_error");
    let child = spawn(&root, &url, &["status"], None);
    let mut socket = ws(&listener, TOKEN).await;
    timeout(
        Duration::from_secs(5),
        socket.send(Message::Text("not json".into())),
    )
    .await
    .unwrap()
    .unwrap();
    error(output(child).await, "protocol_error");
    for (input, kind) in [
        (r#"{"method":"submit_turn","params":{}}"#, "unsupported"),
        ("{broken", "input_error"),
        (
            r#"{"method":"x","params":{"n":9007199254740993}}"#,
            "input_error",
        ),
        (r#"{"method":"x","params":{"n":1e999}}"#, "input_error"),
    ] {
        let child = spawn(&root, &url, &["call"], Some(input));
        error(output(child).await, kind);
        assert!(
            timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_err(),
            "rejected input reached Gateway"
        );
    }
    fs::write(root.join("large"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    let path = root.join("large");
    let child = spawn(
        &root,
        &url,
        &["call", "--input-file", path.to_str().unwrap()],
        None,
    );
    error(output(child).await, "input_error");
    assert!(
        timeout(Duration::from_millis(200), listener.accept())
            .await
            .is_err()
    );
    // Closing the actual stdout pipe reader works on macOS as well as Linux.
    let mut child = spawn(&root, &url, &["status"], None);
    drop(child.0.stdout.take());
    let mut socket = ws(&listener, TOKEN).await;
    send(&mut socket, json!({"type":"hello_ok"})).await;
    let request = next(&mut socket).await;
    send(
        &mut socket,
        json!({"type":"response","id":request["id"],"ok":true,"result":{"capabilities":[]}}),
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while child.0.try_wait().unwrap().is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "closed stdout did not terminate"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!child.0.wait().unwrap().success());
    let mut stderr = String::new();
    use std::io::Read;
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let delivered: Value = serde_json::from_str(&stderr).expect("output error JSON");
    assert_eq!(delivered["kind"], "output_error");
    assert!(
        delivered["message"]
            .as_str()
            .unwrap()
            .contains("may have completed")
    );
    // When supported, a failed stdout write must not masquerade as a successful call.
    if let Ok(full) = std::fs::OpenOptions::new().write(true).open("/dev/full") {
        let mut child = OwnedChild(
            Command::new(env!("CARGO_BIN_EXE_pilotdeck-control"))
                .env_clear()
                .args(["--url", &url, "--token-file"])
                .arg(root.join("token"))
                .arg("status")
                .stdin(Stdio::null())
                .stdout(Stdio::from(full))
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut socket = ws(&listener, TOKEN).await;
        send(&mut socket, json!({"type":"hello_ok"})).await;
        let request = next(&mut socket).await;
        send(
            &mut socket,
            json!({"type":"response","id":request["id"],"ok":true,"result":{"capabilities":[]}}),
        )
        .await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while child.0.try_wait().unwrap().is_none() {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!child.0.wait().unwrap().success());
        let mut error = String::new();
        child
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut error)
            .unwrap();
        assert!(error.contains("output_error") && error.contains("may have completed"));
    }
    for args in [&[][..], &["--help"][..]] {
        let out = Command::new(env!("CARGO_BIN_EXE_pilotdeck-control"))
            .args(args)
            .env_clear()
            .output()
            .unwrap();
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(
            text.contains("list_sessions")
                && text.contains("not automatically retried")
                && text.contains("1 MiB")
        );
    }
    fs::remove_dir_all(root).unwrap();
}

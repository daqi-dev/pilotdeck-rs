//! Local model validation keeps the recorded SDK request order, then owns cleanup.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    time::{Instant, timeout},
};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

const ERROR: &str = "projectKey is required when options.model is set.";
const TOKEN: &str = "fixture-token-no-project";
type Ws = WebSocketStream<TcpStream>;
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
fn child(root: &Path, url: &str, args: &[&str]) -> ChildGuard {
    ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_pilotdeck"))
            .env_clear()
            .env("HOME", root)
            .env("PILOT_HOME", root.join("pilot"))
            .env("TMPDIR", root)
            .current_dir(root)
            .args(args)
            .args(["--gateway-url", url, "--auth-token", TOKEN])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    )
}
async fn collect(mut child: ChildGuard, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.0.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "CLI failed to exit after local validation"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut out = String::new();
    let mut err = String::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert_eq!(child.0.wait().unwrap().code(), Some(1));
    assert_eq!(out, "");
    assert_eq!(err, format!("{expected}\n"));
}
async fn frame(ws: &mut Ws) -> Message {
    timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("frame deadline")
        .expect("frame exists")
        .expect("valid websocket")
}
async fn send(ws: &mut Ws, value: Value) {
    timeout(
        Duration::from_secs(5),
        ws.send(Message::Text(value.to_string().into())),
    )
    .await
    .unwrap()
    .unwrap();
}
async fn peer(listener: &TcpListener) -> Ws {
    let (socket, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ws = timeout(Duration::from_secs(5), accept_async(socket))
        .await
        .unwrap()
        .unwrap();
    let hello: Value = serde_json::from_str(frame(&mut ws).await.to_text().unwrap()).unwrap();
    assert_eq!(
        hello,
        json!({"type":"hello", "protocolVersion":"1.2", "clientName":"sdk", "clientVersion":"pilotdeck-cli", "token":TOKEN})
    );
    send(
        &mut ws,
        json!({"type":"hello_ok", "serverInfo":{"capabilities":[]}}),
    )
    .await;
    ws
}
async fn request(ws: &mut Ws, method: &str, params: Value) -> String {
    let value: Value = serde_json::from_str(frame(ws).await.to_text().unwrap()).unwrap();
    assert_eq!(value["type"], "request");
    assert_eq!(value["method"], method);
    assert_eq!(value["params"], params);
    value["id"].as_str().unwrap().to_owned()
}
async fn session_ok(ws: &mut Ws, id: &str) {
    send(
        ws,
        json!({"type":"response", "id":id,"ok":true,"result":{"sessionKey":"fixture-session"}}),
    )
    .await;
}
async fn closed(ws: &mut Ws) {
    assert_eq!(
        frame(ws).await,
        Message::Close(None),
        "no extra catalog/submit before close"
    );
    let _ = ws.flush().await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_missing_project_exits_but_remote_same_text_keeps_its_lifecycle() {
    let root = std::env::temp_dir().join(format!("pdrs-sdk-model-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let _scratch = Scratch(root.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    // These raw records come from the fixed TS process, not a Rust oracle.
    let oracle: Value =
        serde_json::from_str(include_str!("fixtures/sdk-model-project/oracle.json")).unwrap();
    assert_eq!(oracle.as_array().unwrap().len(), 4);
    for sample in oracle.as_array().unwrap() {
        let args: Vec<_> = sample["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let child = child(&root, &url, &args);
        let requests: Vec<_> = sample["wire"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["frame"]["type"] == "request")
            .collect();
        assert!(requests.len() <= 2);
        let mut peers = Vec::new();
        for row in requests {
            let mut ws = peer(&listener).await;
            let id = request(
                &mut ws,
                row["frame"]["method"].as_str().unwrap(),
                row["frame"]["params"].clone(),
            )
            .await;
            session_ok(&mut ws, &id).await;
            peers.push(ws);
        }
        if peers.len() == 2 {
            assert!(
                sample["exitBeforeCleanup"].is_null(),
                "baseline must actually retain its query"
            );
            assert_eq!(sample["openConnections"], 1);
            closed(&mut peers[1]).await;
        }
        if let Some(control) = peers.first_mut() {
            closed(control).await;
        }
        collect(
            child,
            sample["stderr"]
                .as_str()
                .unwrap()
                .strip_suffix('\n')
                .unwrap(),
        )
        .await;
        assert!(
            timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err(),
            "no extra connections"
        );
    }
    // Empty project is missing, but whitespace is a real key in the old parser.
    for project in ["", " "] {
        let child = child(
            &root,
            &url,
            &[
                "run",
                "prompt",
                "--model",
                "fixture/model",
                "--project",
                project,
            ],
        );
        let mut control = peer(&listener).await;
        let params = if project.is_empty() {
            json!({"channelKey":"cli"})
        } else {
            json!({"channelKey":"cli","projectKey":project})
        };
        let id = request(&mut control, "new_session", params).await;
        session_ok(&mut control, &id).await;
        let mut query = peer(&listener).await;
        let id = request(
            &mut query,
            "resume_session",
            json!({"sessionKey":"fixture-session"}),
        )
        .await;
        session_ok(&mut query, &id).await;
        if project.is_empty() {
            closed(&mut query).await;
            closed(&mut control).await;
        } else {
            let id = request(&mut query, "model_catalog_list", json!({"projectKey":" "})).await;
            send(
                &mut query,
                json!({"type":"response","id":id,"ok":false,"error":{"message":ERROR}}),
            )
            .await;
            closed(&mut control).await;
            assert!(
                timeout(Duration::from_millis(250), query.next())
                    .await
                    .is_err()
            );
            query.close(None).await.unwrap();
        }
        collect(child, ERROR).await;
    }
    // A gateway error with identical wording is not this local validation.
    let mut child = child(&root, &url, &["run", "prompt", "--model", "fixture/model"]);
    let mut control = peer(&listener).await;
    let id = request(&mut control, "new_session", json!({"channelKey":"cli"})).await;
    session_ok(&mut control, &id).await;
    let mut query = peer(&listener).await;
    let id = request(
        &mut query,
        "resume_session",
        json!({"sessionKey":"fixture-session"}),
    )
    .await;
    send(
        &mut query,
        json!({"type":"response","id":id,"ok":false,"error":{"message":ERROR}}),
    )
    .await;
    closed(&mut control).await;
    assert!(
        timeout(Duration::from_millis(250), query.next())
            .await
            .is_err()
    );
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "remote error must retain query"
    );
    query.close(None).await.unwrap();
    collect(child, ERROR).await;
    // Signal during query initialization keeps its existing abnormal query
    // release path, even when the next local check detects missing project.
    #[cfg(unix)]
    for signal in ["-INT", "-TERM"] {
        let child = self::child(&root, &url, &["run", "prompt", "--model", "fixture/model"]);
        let mut control = peer(&listener).await;
        let id = request(&mut control, "new_session", json!({"channelKey":"cli"})).await;
        session_ok(&mut control, &id).await;
        let mut query = peer(&listener).await;
        let id = request(
            &mut query,
            "resume_session",
            json!({"sessionKey":"fixture-session"}),
        )
        .await;
        assert!(
            Command::new("/bin/kill")
                .args([signal, &child.0.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
        query.send(Message::Ping(vec![1].into())).await.unwrap();
        assert_eq!(frame(&mut query).await, Message::Pong(vec![1].into()));
        session_ok(&mut query, &id).await;
        let end = timeout(Duration::from_secs(5), query.next()).await.unwrap();
        assert!(
            matches!(end, None | Some(Err(_))),
            "signal initialization must release query without a close frame: {end:?}"
        );
        closed(&mut control).await;
        collect(child, ERROR).await;
    }
}

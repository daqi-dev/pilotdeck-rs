//! Real default CLI with an owned discovery/WS peer; execute explicitly on mbp2.
//! The legacy discovery port is fixed: never run this against a user's service.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::{Instant, timeout},
};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};
struct Lease;
impl Lease {
    async fn acquire() -> Self {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match fs::create_dir("/tmp/pdrs-cron-18789.lock") {
                Ok(()) => return Self,
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists
                        && Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await
                }
                Err(error) => panic!("binding test port lease: {error}"),
            }
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = fs::remove_dir("/tmp/pdrs-cron-18789.lock");
    }
}
struct Scratch(PathBuf);
impl Drop for Scratch {
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
fn start(home: &Path, project: &Path) -> OwnedChild {
    start_with_sidecar(home, project, None)
}
fn start_with_sidecar(home: &Path, project: &Path, sidecar: Option<&Path>) -> OwnedChild {
    let bundled = home.join("bundled");
    fs::create_dir_all(bundled.join("fixture")).unwrap();
    fs::write(
        bundled.join("fixture/SKILL.md"),
        "---\nname: fixture\ndescription: Offline fixture.\n---\nNo operation.\n",
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_pilotdeck"));
    command.env_clear()
            .env("HOME", home)
            .env("PILOT_HOME", home)
            .env("PILOTDECK_BUNDLED_SKILLS_DIR", &bundled)
            .env("STAFFDECK_PROJECT_BINDING_ENABLED", "true")
            .env("STAFFDECK_COPY_TENANT_ID", "fixture-tenant")
            .env("STAFFDECK_FORMAL_API_ORIGIN", "https://staffdeck.invalid/")
            .env(
                "STAFFDECK_SOP_RUNTIME_ORIGIN",
                "https://staffdeck.invalid/runtime",
            )
            .env("STAFFDECK_SOP_MANAGEMENT_API_KEY", "mock-management-only")
            .current_dir(project)
            .arg("binding prompt")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    if let Some(sidecar) = sidecar {
        command.env("PILOTDECK_RS_SIDECAR", sidecar);
    }
    OwnedChild(command.spawn().unwrap())
}
async fn finish(child: OwnedChild) -> Output {
    finish_with_budget(child, Duration::from_secs(5)).await
}
async fn finish_with_budget(mut child: OwnedChild, budget: Duration) -> Output {
    let deadline = Instant::now() + budget;
    while child.0.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "binding CLI exceeded its {budget:?} budget"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
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
async fn health(listener: &TcpListener, status: &str) {
    let (mut socket, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];
    timeout(Duration::from_secs(3), async {
        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            request.extend_from_slice(&buffer[..count]);
            assert!(request.len() <= 65536);
        }
    })
    .await
    .unwrap();
    assert!(request.starts_with(b"GET /health HTTP/1.1\r\n"));
    timeout(
        Duration::from_secs(3),
        socket.write_all(
            format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
}
async fn frame(ws: &mut WebSocketStream<TcpStream>) -> Value {
    let message = timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}
async fn send(ws: &mut WebSocketStream<TcpStream>, value: Value) {
    timeout(
        Duration::from_secs(3),
        ws.send(Message::Text(value.to_string().into())),
    )
    .await
    .unwrap()
    .unwrap();
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires isolated mbp2 port18789 lease; never run against user PilotDeck"]
async fn binding_precheck_precedes_connections_and_allows_the_existing_remote_channel() {
    let _lease = Lease::acquire().await;
    let listener = TcpListener::bind("127.0.0.1:18789")
        .await
        .expect("owned test port must be free; do not stop its owner");
    let root = std::env::temp_dir().join(format!("pdrs-binding-exit-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let _scratch = Scratch(root.clone());
    let oracle: Value =
        serde_json::from_str(include_str!("fixtures/config-binding/oracle.json")).unwrap();
    for id in [
        "missing-store",
        "bound-valid-sops",
        "bound-missing-sops",
        "binding-dead",
        "malformed-store",
        "store-before-yaml-fatal",
        "bound-malformed-sops",
        "surrogate-agent-uri",
    ] {
        let case = oracle["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .unwrap();
        let base = root.join(id);
        let home = base.join("home");
        let project = base.join("project");
        fs::create_dir_all(home.join("staffdeck")).unwrap();
        fs::create_dir(&project).unwrap();
        fs::write(home.join("pilotdeck.yaml"), case["yaml"].as_str().unwrap()).unwrap();
        fs::write(home.join("server-token"), b"binding-test-token\n").unwrap();
        if let Some(store) = case["store"].as_str() {
            fs::write(
                home.join("staffdeck/project-bindings.json"),
                store.replace("<project>", project.to_str().unwrap()),
            )
            .unwrap();
        }
        if let Some(sops) = case["sops"].as_str() {
            fs::create_dir(home.join("staffdeck/fixture-binding")).unwrap();
            fs::write(home.join("staffdeck/fixture-binding/sops.json"), sops).unwrap();
        }
        if let Some(lock) = case["lock"].as_str() {
            assert_eq!(lock, "dead");
            fs::write(
                home.join("staffdeck/project-bindings.json.lock"),
                b"2147483647",
            )
            .unwrap();
        }
        let before = fs::read(home.join("pilotdeck.yaml")).unwrap();
        let store = home.join("staffdeck/project-bindings.json");
        let old_store = store.exists().then(|| fs::read(&store).unwrap());
        let child = start(&home, &project);
        if case["result"]["ok"] == true {
            health(&listener, "200 OK").await;
            let (socket, _) = timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut ws = timeout(Duration::from_secs(3), accept_async(socket))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                frame(&mut ws).await,
                json!({"type":"hello","protocolVersion":"1.2","clientName":"cli","clientVersion":"0.1.0","token":"binding-test-token"})
            );
            send(&mut ws,json!({"type":"hello_ok","protocolVersion":"1.2","serverVersion":"fixture","serverInfo":{"capabilities":[]}})).await;
            let request = frame(&mut ws).await;
            let id = request["id"].as_str().unwrap();
            assert_eq!(uuid::Uuid::parse_str(id).unwrap().get_version_num(), 4);
            assert_eq!(request["method"], "submit_turn");
            assert_eq!(
                request["params"],
                json!({"sessionKey":format!("cli:project={}:default",project.display()),"channelKey":"cli","message":"binding prompt","canPrompt":false})
            );
            send(&mut ws,json!({"type":"event","id":id,"seq":0,"final":false,"event":{"type":"assistant_text_delta","text":"binding ready"}})).await;
            send(&mut ws,json!({"type":"event","id":id,"seq":1,"final":true,"event":{"type":"turn_completed","finishReason":"completed"}})).await;
            let closed = timeout(Duration::from_secs(3), ws.next()).await.unwrap();
            assert!(matches!(closed, Some(Ok(Message::Close(_))) | None));
            let _ = timeout(Duration::from_secs(3), ws.close(None)).await;
            let output = finish(child).await;
            assert_eq!(
                output.status.code(),
                Some(0),
                "{id}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(output.stdout, b"binding ready");
            assert!(output.stderr.is_empty());
        } else {
            let output = finish(child).await;
            assert_eq!(output.status.code(), Some(1), "{id}");
            assert!(output.stdout.is_empty());
            assert_eq!(
                String::from_utf8(output.stderr).unwrap(),
                format!("{}\n", case["result"]["message"].as_str().unwrap())
            );
            assert!(
                timeout(Duration::from_millis(150), listener.accept())
                    .await
                    .is_err(),
                "{id} connected before failing config"
            );
        }
        assert_eq!(fs::read(home.join("pilotdeck.yaml")).unwrap(), before);
        assert_eq!(store.exists().then(|| fs::read(&store).unwrap()), old_store);
    }
    // Keep the real TS missing-store configuration. Its runtime binding disables
    // knowledge and removes SOP; only substitute the fixture provider's origin.
    let base = root.join("unavailable");
    let home = base.join("home");
    let project = base.join("project");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir(&project).unwrap();
    let sidecar = std::env::var_os("PDRS_TEST_SIDECAR")
        .map(PathBuf::from)
        .expect("explicit test requires PDRS_TEST_SIDECAR pointing to the built Rust sidecar");
    assert!(sidecar.is_absolute() && sidecar.is_file());
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}/v1", provider.local_addr().unwrap());
    let original = oracle["cases"][0]["yaml"].as_str().unwrap();
    assert_eq!(oracle["cases"][0]["id"], "missing-store");
    assert_eq!(original.matches("https://model.invalid/v1").count(), 1);
    let yaml = original.replace("https://model.invalid/v1", &origin);
    fs::write(home.join("pilotdeck.yaml"), &yaml).unwrap();
    let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
    let provider_task = tokio::spawn(async move {
        let mut requests = Vec::new();
        loop {
            let accepted = tokio::select! {
                _ = &mut stopped => break,
                accepted = provider.accept() => accepted.unwrap(),
            };
            assert!(requests.len() < 2, "unexpected additional provider request");
            let mut socket = accepted.0;
            let body = provider_request(&mut socket).await;
            let stream = body["stream"] == true;
            let (content_type, response) = if stream {
                ("text/event-stream", concat!(
                    "data: {\"id\":\"binding\",\"model\":\"mock\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"binding local ready\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"id\":\"binding\",\"model\":\"mock\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: [DONE]\n\n"
                ).to_owned())
            } else {
                ("application/json", json!({
                    "id":"binding-title", "choices":[{"message":{"role":"assistant",
                    "content":"{\"title\":\"Binding local fixture\"}"},"finish_reason":"stop"}]
                }).to_string())
            };
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            );
            timeout(Duration::from_secs(3), async {
                socket.write_all(header.as_bytes()).await.unwrap();
                socket.write_all(response.as_bytes()).await.unwrap();
            }).await.unwrap();
            requests.push(body);
        }
        requests
    });
    let child = start_with_sidecar(&home, &project, Some(&sidecar));
    health(&listener, "503 Service Unavailable").await;
    let output = finish_with_budget(child, Duration::from_secs(15)).await;
    let _ = stop.send(());
    let requests = timeout(Duration::from_secs(3), provider_task)
        .await.unwrap().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.stdout, b"binding local ready");
    let streams: Vec<_> = requests.iter().filter(|body| body["stream"] == true).collect();
    assert_eq!(streams.len(), 1, "one actual native main turn");
    assert_eq!(streams[0]["model"], "mock");
    assert!(streams[0]["messages"].as_array().unwrap().iter().any(|message|
        message["role"] == "user" && message["content"] == "binding prompt"
    ));
    assert!(timeout(Duration::from_millis(150), listener.accept()).await.is_err(),
        "503 fallback must not reconnect to the discovery peer");
    assert_eq!(fs::read(home.join("pilotdeck.yaml")).unwrap(), yaml.as_bytes());
    assert!(!home.join("staffdeck/project-bindings.json").exists());
    let mut pending = vec![home.join("projects")];
    let mut results = Vec::new();
    let mut visited = 0;
    while let Some(path) = pending.pop() {
        visited += 1;
        assert!(visited <= 128, "unexpected fixture history expansion");
        if path.is_dir() {
            pending.extend(fs::read_dir(path).unwrap().map(|entry| entry.unwrap().path()));
        } else if path.extension().is_some_and(|extension| extension == "jsonl") {
            let bytes = fs::read(&path).unwrap();
            assert!(bytes.len() <= 1024 * 1024);
            for line in bytes.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
                let entry: Value = serde_json::from_slice(line).unwrap();
                if entry["type"] == "turn_result" {
                    results.push((path.clone(), entry));
                }
            }
        }
    }
    assert_eq!(results.len(), 1, "native LocalHost must persist the completed turn");
    assert_eq!(results[0].1["result"]["type"], "success");
    assert_eq!(results[0].1["result"]["stopReason"], "completed");
    assert_eq!(results[0].1["result"]["finalMessage"]["content"][0]["text"], "binding local ready");
    // A fresh real LocalHost reports its missing prior transcript. Preserve the
    // complete diagnostic, including the actual persisted transcript path.
    let missing = format!("Transcript {} does not exist.", results[0].0.display());
    assert_eq!(String::from_utf8(output.stderr).unwrap(), format!(
        "[pilotdeck] gateway server unavailable; using in-process mode.\nnative session: Diagnostic {{ code: \"transcript_missing\", severity: \"warning\", line: None, message: {missing:?} }}\n"
    ));
}

async fn provider_request(socket: &mut TcpStream) -> Value {
    timeout(Duration::from_secs(3), async {
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        let head_end = loop {
            if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                break end + 4;
            }
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0, "provider request ended before its headers");
            bytes.extend_from_slice(&buffer[..count]);
            assert!(bytes.len() <= 65536);
        };
        let head = std::str::from_utf8(&bytes[..head_end]).unwrap();
        assert!(head.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"), "{head}");
        let length: usize = head.lines().filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .unwrap().1.trim().parse().unwrap();
        assert!(length <= 1024 * 1024);
        while bytes.len() < head_end + length {
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0, "provider request ended before its body");
            bytes.extend_from_slice(&buffer[..count]);
            assert!(bytes.len() <= head_end + length);
        }
        serde_json::from_slice(&bytes[head_end..]).unwrap()
    }).await.expect("bounded local provider request")
}

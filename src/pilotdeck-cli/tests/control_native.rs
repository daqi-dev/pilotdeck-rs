//! A real isolated native host plus the new CLI and an independent WS reader.
//! Run explicitly with PDRS_NATIVE_GATEWAY_BIN set to the freshly built binary.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use tokio::time::{Instant as TokioInstant, timeout, timeout_at};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const TOKEN: &str = "native-control-probe-token";
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
fn run_control(url: &str, token: &Path, command: &str, input: Option<Value>) -> Value {
    let mut child = OwnedChild(
        Command::new(env!("CARGO_BIN_EXE_pilotdeck-control"))
            .env_clear()
            .args(["--url", url, "--token-file"])
            .arg(token)
            .arg(command)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn control"),
    );
    if let Some(input) = input {
        child
            .0
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "control timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    use std::io::Read;
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
    assert!(
        child.0.wait().unwrap().success(),
        "control failed: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert!(stderr.is_empty());
    serde_json::from_slice(&stdout).expect("control JSON")
}
async fn receive(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    let frame = timeout(Duration::from_secs(10), ws.next())
        .await
        .expect("native WS timed out")
        .expect("native WS ended")
        .expect("native WS error");
    serde_json::from_str(frame.to_text().expect("native WS text")).unwrap()
}
async fn direct(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    id: &str,
    method: &str,
    params: Value,
) -> Value {
    timeout(
        Duration::from_secs(10),
        ws.send(Message::Text(
            json!({"type":"request","id":id,"method":method,"params":params})
                .to_string()
                .into(),
        )),
    )
    .await
    .unwrap()
    .unwrap();
    let deadline = TokioInstant::now() + Duration::from_secs(10);
    loop {
        let frame = timeout_at(deadline, receive(ws))
            .await
            .expect("native response deadline exceeded");
        if frame["type"] == "response" && frame["id"] == id {
            assert_eq!(frame["ok"], true, "native error: {frame}");
            return frame["result"].clone();
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly built pilotdeck-gateway binary via PDRS_NATIVE_GATEWAY_BIN"]
async fn one_native_host_is_shared_by_control_and_gateway_wire() {
    let gateway = std::env::var_os("PDRS_NATIVE_GATEWAY_BIN")
        .map(PathBuf::from)
        .expect("set PDRS_NATIVE_GATEWAY_BIN to freshly built gateway");
    assert!(gateway.is_absolute() && gateway.is_file());
    shared_host(gateway, "--native", "-TERM").await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit real foreground host integration"]
async fn one_control_serve_host_is_shared_by_control_and_gateway_wire() {
    shared_host(
        PathBuf::from(env!("CARGO_BIN_EXE_pilotdeck-control")),
        "serve",
        "-INT",
    )
    .await;
}

async fn shared_host(gateway: PathBuf, entry: &str, signal: &str) {
    let base = fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("pdrs-control-native-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&base).unwrap();
    let _scratch = Scratch(base.clone());
    let home = base.join("home");
    let project = base.join("project");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&project).unwrap();
    fs::write(home.join("pilotdeck.yaml"),"schemaVersion: 1\nagent:\n  model: test/test\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:1\n      apiKey: test-only\n      models:\n        test: {}\ncron:\n  enabled: true\n").unwrap();
    let token = base.join("token");
    fs::write(&token, format!("{TOKEN}\n")).unwrap();
    let sidecar = base.join("sidecar");
    fs::write(&sidecar, "#!/bin/sh\nexit 99\n").unwrap();
    fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = OwnedChild(
        Command::new(gateway)
            .env_clear()
            .arg(entry)
            .arg("--home")
            .arg(&home)
            .arg("--project")
            .arg(&project)
            .args(["--listen", "127.0.0.1:0"])
            .arg("--token-file")
            .arg(&token)
            .arg("--sidecar")
            .arg(&sidecar)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn native gateway"),
    );
    let stderr = child.0.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let diagnostics = Arc::new(Mutex::new(Vec::<String>::new()));
    let reader_diagnostics = diagnostics.clone();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            match line {
                Ok(line) => {
                    if let Some(address) = line.strip_prefix("native listening ") {
                        let _ = tx.send(address.to_owned());
                    }
                    let mut log = reader_diagnostics.lock().unwrap();
                    if log.len() < 64 {
                        log.push(line.chars().take(2048).collect());
                    }
                }
                Err(error) => {
                    let mut log = reader_diagnostics.lock().unwrap();
                    if log.len() < 64 {
                        log.push(format!("stderr read error: {error}"));
                    }
                    break;
                }
            }
        }
    });
    let address = rx
        .recv_timeout(Duration::from_secs(30))
        .unwrap_or_else(|error| {
            panic!(
                "native host did not advertise listener: {error}; stderr: {}",
                diagnostics.lock().unwrap().join("\n")
            )
        });
    let socket: std::net::SocketAddr = address.parse().expect("native address");
    assert!(socket.ip().is_loopback() && socket.port() != 0);
    let url = format!("ws://{address}/ws");
    let (mut ws, _) = timeout(Duration::from_secs(10), connect_async(&url))
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(10), ws.send(Message::Text(json!({"type":"hello","protocolVersion":"1.2","clientName":"cli","clientVersion":"0.1.0","token":TOKEN}).to_string().into()))).await.expect("native hello send timed out").unwrap();
    assert_eq!(receive(&mut ws).await["type"], "hello_ok");
    let direct_status = direct(&mut ws, "describe", "describe_server", json!({})).await;
    let status = run_control(&url, &token, "status", None);
    assert_eq!(status["server"], direct_status);
    assert_eq!(status["source"], "gateway.describe_server");
    assert_eq!(
        run_control(&url, &token, "capabilities", None),
        direct_status["capabilities"]
    );
    let direct_list = direct(
        &mut ws,
        "sessions",
        "list_sessions",
        json!({"projectKey":project}),
    )
    .await;
    assert_eq!(
        run_control(
            &url,
            &token,
            "call",
            Some(json!({"method":"list_sessions","params":{"projectKey":project}}))
        ),
        json!({"ok":true,"result":direct_list})
    );
    let params = json!({"projectKey":project,"message":"control fixture","sessionKey":"control-fixture","channelKey":"cli","mode":"default","schedule":{"type":"once","runAt":"2099-01-01T00:00:00.000Z"}});
    let created = run_control(
        &url,
        &token,
        "call",
        Some(json!({"method":"cron_create","params":params})),
    );
    let task_id = created["result"]["task"]["taskId"]
        .as_str()
        .expect("created task id");
    let listed = direct(
        &mut ws,
        "cron-list",
        "cron_list",
        json!({"projectKey":project}),
    )
    .await;
    assert!(
        listed["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["taskId"] == task_id),
        "direct Gateway must read control's write"
    );
    let closed = run_control(
        &url,
        &token,
        "call",
        Some(json!({"method":"close_session","params":{"sessionKey":"control-fixture"}})),
    );
    assert_eq!(
        closed,
        json!({"ok":true}),
        "native void result must remain distinguishable from null"
    );
    drop(ws);
    let status = Command::new("/bin/kill")
        .args([signal, &child.0.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "native host did not stop");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        child.0.wait().unwrap().success(),
        "native host shutdown failed"
    );
    reader.join().unwrap();
}

//! Real SDK processes, fixed-TS token facts, and owned dynamic loopback peers.
#![cfg(unix)]
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    time::{Instant, timeout},
};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};
const TYPE_ERROR: &str = "Gateway auth token file must be a regular file.\n";
const SIZE_ERROR: &str = "Gateway auth token file exceeds the 65536-byte limit.\n";
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("pdrs-sdk-token-exit-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["home", "project"] {
            fs::create_dir(root.join(name)).unwrap();
        }
        Self(root)
    }
    fn command(&self, argv: &[String], env: &Value) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pilotdeck"));
        command
            .env_clear()
            .env("HOME", self.0.join("home"))
            .env("PILOT_HOME", self.0.join("home"))
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .current_dir(self.0.join("project"))
            .args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env.as_object().unwrap() {
            command.env(key, value.as_str().unwrap());
        }
        command
    }
}
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
async fn collect(mut child: OwnedChild) -> Output {
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.0.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "SDK token preflight exceeded five seconds"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
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
fn bytes(file: &Value) -> Vec<u8> {
    if let Some(bytes) = file["bytes"].as_array() {
        bytes
            .iter()
            .map(|byte| byte.as_u64().unwrap() as u8)
            .collect()
    } else if let Some(length) = file["length"].as_u64() {
        vec![file["byte"].as_u64().unwrap() as u8; length as usize]
    } else {
        file["text"].as_str().unwrap_or("").as_bytes().to_vec()
    }
}
#[derive(Debug, PartialEq, Eq)]
struct Identity {
    ino: u64,
    mode: u32,
    size: u64,
    mtime: i64,
    mtime_nsec: i64,
}
fn identity(path: &Path) -> Option<Identity> {
    match fs::symlink_metadata(path) {
        Ok(m) => Some(Identity {
            ino: m.ino(),
            mode: m.mode(),
            size: m.len(),
            mtime: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("token fixture metadata failed: {error}"),
    }
}
struct TokenInput {
    holder: Option<fs::File>,
    expected: Option<Vec<u8>>,
    writer: Option<(fs::File, Vec<u8>)>,
}
fn seed(root: &Scratch, case: &Value) -> TokenInput {
    let path = root.0.join("home/server-token");
    let file = &case["file"];
    let mut holder = None;
    let mut expected = None;
    match file["kind"].as_str() {
        Some("missing") => {}
        Some("directory") => fs::create_dir(&path).unwrap(),
        Some("fifo") => assert!(
            Command::new("/usr/bin/mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        ),
        _ => {
            let data = bytes(file);
            fs::write(&path, &data).unwrap();
            holder = Some(fs::File::open(&path).unwrap());
            expected = Some(data);
        }
    }
    if let Some(mode) = case["inputMetadata"]["mode"].as_u64() {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode as u32)).unwrap();
    }
    let writer = case.get("writer").map(|value| {
        let data = value["text"].as_str().unwrap().as_bytes().to_vec();
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .unwrap();
        file.write_all(&data).unwrap();
        (file, data)
    });
    TokenInput {
        holder,
        expected,
        writer,
    }
}
fn token_summary(token: &str) -> Value {
    json!({"utf8":token.len(),"utf16":token.encode_utf16().count(),"sha256":format!("{:x}",Sha256::digest(token.as_bytes()))})
}
async fn frame(ws: &mut WebSocketStream<TcpStream>) -> (Value, Vec<u8>) {
    let message = timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let raw = message.to_text().unwrap().as_bytes().to_vec();
    (serde_json::from_slice(&raw).unwrap(), raw)
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
async fn sdk_token_sources_match_real_ts_wire_except_the_explicit_safety_rejections() {
    let oracle: Value =
        serde_json::from_str(include_str!("fixtures/sdk-token/oracle.json")).unwrap();
    assert_eq!(oracle["cases"].as_array().unwrap().len(), 19);
    for case in oracle["cases"].as_array().unwrap() {
        let name = case["id"].as_str().unwrap();
        let root = Scratch::new();
        let path = root.0.join("home/server-token");
        let mut input = seed(&root, case);
        let before = identity(&path);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws", listener.local_addr().unwrap());
        let mut argv = vec![
            "sessions".into(),
            "list".into(),
            "--gateway-url".into(),
            url,
        ];
        if let Some(args) = case["argv"].as_array() {
            argv.extend(args.iter().map(|arg| arg.as_str().unwrap().to_owned()));
        }
        let env = case.get("env").cloned().unwrap_or_else(|| json!({}));
        let child = OwnedChild(root.command(&argv, &env).spawn().unwrap());
        if case.get("safety").is_some() || case["connections"] == 0 {
            let output = collect(child).await;
            assert_eq!(output.status.code(), Some(1), "{name}");
            assert!(output.stdout.is_empty(), "{name}");
            let expected = match case["safety"].as_str() {
                Some("limit") => SIZE_ERROR,
                Some("non_regular") => TYPE_ERROR,
                _ => case["stderr"].as_str().unwrap(),
            };
            assert_eq!(
                String::from_utf8(output.stderr).unwrap(),
                expected,
                "{name}"
            );
            assert!(
                timeout(Duration::from_millis(50), listener.accept())
                    .await
                    .is_err(),
                "{name} must not connect"
            );
        } else {
            let (socket, _) = timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut ws = timeout(Duration::from_secs(3), accept_async(socket))
                .await
                .unwrap()
                .unwrap();
            let (mut hello, raw_hello) = frame(&mut ws).await;
            // Preserve the existing sessions transport serializer; this slice
            // compares frame fields and exact decoded token bytes. Retain each
            // original raw-frame digest as evidence without redefining D1.
            println!(
                "SDK_HELLO_RAW {}",
                json!({"case":name,"bytes":raw_hello.len(),"sha256":format!("{:x}",Sha256::digest(&raw_hello))})
            );
            let summary = token_summary(hello["token"].as_str().unwrap());
            hello["token"] = "<token>".into();
            hello["tokenSummary"] = summary;
            assert_eq!(
                hello, case["wire"][0],
                "{name} hello without plaintext token logging"
            );
            send(&mut ws,json!({"type":"hello_ok","protocolVersion":"1.2","serverVersion":"fixture","serverInfo":{"capabilities":[]}})).await;
            let (mut request, _) = frame(&mut ws).await;
            let id = request["id"].as_str().unwrap().to_owned();
            assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 4);
            request["id"] = "<uuid>".into();
            assert_eq!(request, case["wire"][1], "{name} request");
            send(
                &mut ws,
                json!({"type":"response","id":id,"ok":true,"result":{"sessions":[]}}),
            )
            .await;
            let closed = timeout(Duration::from_secs(3), ws.next()).await.unwrap();
            assert!(matches!(closed, Some(Ok(Message::Close(_))) | None));
            let _ = timeout(Duration::from_secs(3), ws.close(None)).await;
            let output = collect(child).await;
            assert_eq!(
                output.status.code(),
                case["exit"]["code"].as_i64().map(|value| value as i32),
                "{name}"
            );
            assert_eq!(
                output.stdout,
                case["stdout"].as_str().unwrap().as_bytes(),
                "{name}"
            );
            assert_eq!(
                output.stderr,
                case["stderr"].as_str().unwrap().as_bytes(),
                "{name}"
            );
            assert!(
                timeout(Duration::from_millis(20), listener.accept())
                    .await
                    .is_err(),
                "{name} duplicate connection"
            );
        }
        assert_eq!(identity(&path), before, "{name} input metadata");
        if let Some(holder) = input.holder.as_mut() {
            let mut actual = Vec::new();
            holder.take(65538).read_to_end(&mut actual).unwrap();
            assert_eq!(Some(actual), input.expected, "{name} input bytes");
        }
        if let Some((writer, bytes)) = input.writer.as_mut() {
            let mut actual = vec![0; bytes.len()];
            writer.read_exact(&mut actual).unwrap();
            assert_eq!(&actual, bytes, "{name} SDK must not consume the pipe");
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn all_sdk_command_families_refuse_resource_inputs_before_network() {
    for family in [
        vec!["run", "fixture prompt"],
        vec!["resume", "fixture-session", "fixture prompt"],
        vec!["sessions", "list"],
        vec!["settings", "get"],
        vec!["settings", "set", "{\"language\":\"zh-CN\"}"],
    ] {
        for kind in ["fifo-empty", "fifo-writer", "oversize"] {
            let root = Scratch::new();
            let descriptor = if kind == "oversize" {
                json!({"file":{"byte":120,"length":65537},"inputMetadata":{"mode":384}})
            } else if kind == "fifo-writer" {
                json!({"file":{"kind":"fifo"},"writer":{"text":"pipe token\n"},"inputMetadata":{"mode":384}})
            } else {
                json!({"file":{"kind":"fifo"},"inputMetadata":{"mode":384}})
            };
            let mut input = seed(&root, &descriptor);
            let path = root.0.join("home/server-token");
            let before = identity(&path);
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut argv: Vec<String> = family.iter().map(|arg| (*arg).into()).collect();
            argv.extend([
                "--gateway-url".into(),
                format!("ws://{}/ws", listener.local_addr().unwrap()),
            ]);
            let child = OwnedChild(root.command(&argv, &json!({})).spawn().unwrap());
            let output = collect(child).await;
            assert_eq!(output.status.code(), Some(1), "{family:?} {kind}");
            assert!(output.stdout.is_empty());
            assert_eq!(
                String::from_utf8(output.stderr).unwrap(),
                if kind == "oversize" {
                    SIZE_ERROR
                } else {
                    TYPE_ERROR
                }
            );
            assert!(
                timeout(Duration::from_millis(50), listener.accept())
                    .await
                    .is_err()
            );
            assert_eq!(identity(&path), before);
            if let Some((writer, bytes)) = input.writer.as_mut() {
                let mut actual = vec![0; bytes.len()];
                writer.read_exact(&mut actual).unwrap();
                assert_eq!(&actual, bytes);
            }
        }
    }
}

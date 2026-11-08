//! Isolated process test against TS-recorded legacy channel frames.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
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
const LEASE: &str = "/tmp/pdrs-cron-18789.lock";
struct Lease;
impl Lease {
    async fn acquire() -> Self {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match fs::create_dir(LEASE) {
                Ok(()) => return Self,
                Err(e)
                    if e.kind() == std::io::ErrorKind::AlreadyExists
                        && Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await
                }
                Err(e) => panic!("channel port lease unavailable: {e}"),
            }
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = fs::remove_dir(LEASE);
    }
}
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct ChildOwner(Child);
impl Drop for ChildOwner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
async fn request_head(stream: &mut TcpStream) -> Vec<u8> {
    timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        let mut buf = [0; 1024];
        loop {
            let n = stream.read(&mut buf).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
            assert!(bytes.len() < 65536);
            if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                return bytes;
            }
        }
    })
    .await
    .expect("HTTP request timeout")
}
async fn next(ws: &mut WebSocketStream<TcpStream>) -> Value {
    let m = timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("frame timeout")
        .expect("socket ended")
        .expect("frame error");
    serde_json::from_str(m.to_text().unwrap()).unwrap()
}
async fn send(ws: &mut WebSocketStream<TcpStream>, value: Value) {
    timeout(
        Duration::from_secs(5),
        ws.send(Message::Text(value.to_string().into())),
    )
    .await
    .expect("send timeout")
    .unwrap();
}
fn child(root: &Path, project: &Path, argv: &[&str], input: &str) -> ChildOwner {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pilotdeck"))
        .env_clear()
        .env("HOME", root)
        .env("PILOT_HOME", root)
        .current_dir(project)
        .args(argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    ChildOwner(child)
}
async fn finish(child: ChildOwner) -> (bool, String, String) {
    let output = finish_output(child).await;
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}
async fn finish_output(mut child: ChildOwner) -> Output {
    let deadline = Instant::now() + Duration::from_secs(8);
    while child.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "channel did not exit");
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
    Output {
        status: child.0.wait().unwrap(),
        stdout: out.into_bytes(),
        stderr: err.into_bytes(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn default_channel_remote_matches_ts_recordings() {
    let _lease = Lease::acquire().await;
    let listener = TcpListener::bind("127.0.0.1:18789")
        .await
        .expect("isolated channel test requires port 18789 free");
    let base = std::env::temp_dir().join(format!("pdrs-cli-channel-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&base).unwrap();
    let base = fs::canonicalize(base).unwrap();
    let _scratch = Scratch(base.clone());
    let home = base.join("home");
    let project = base.join("project");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&project).unwrap();
    fs::write(home.join("server-token"), "channel-fake-token\n").unwrap();
    fs::write(home.join("pilotdeck.yaml"),"schemaVersion: 1\nagent:\n  model: test/test\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:1\n      apiKey: fake-only\n      models:\n        test: {}\n").unwrap();
    let oracle: Value =
        serde_json::from_str(include_str!("fixtures/cli-channel/ts-oracle.json")).unwrap();
    for (id, args, input) in [
        ("argv", vec!["hello", "--strange", " world "], ""),
        ("lines", vec![], "  first  \r\n\nsecond\n"),
        ("whitespace", vec!["  "], "   \n"),
    ] {
        let baseline = &oracle["cases"][id];
        let child = child(&home, &project, &args, input);
        let (mut health, _) = timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        assert!(
            request_head(&mut health)
                .await
                .starts_with(b"GET /health HTTP/1.1\r\n")
        );
        health
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        drop(health);
        let (socket, _) = timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut ws = timeout(Duration::from_secs(5), accept_async(socket))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next(&mut ws).await, baseline["wire"][0]);
        send(&mut ws,json!({"type":"hello_ok","protocolVersion":"1.2","serverVersion":"test","serverInfo":{"capabilities":[]}})).await;
        for expected in baseline["wire"].as_array().unwrap().iter().skip(1) {
            let mut actual = next(&mut ws).await;
            let id = actual["id"].as_str().unwrap().to_owned();
            assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 4);
            actual["id"] = "<uuid>".into();
            let mut expected = expected.clone();
            expected["params"]["sessionKey"] =
                format!("cli:project={}:default", project.display()).into();
            assert_eq!(actual, expected, "legacy CLI submit envelope");
            let events = [
                json!({"type":"assistant_text_delta","text":"A"}),
                json!({"type":"assistant_thinking_delta","text":"h"}),
                json!({"type":"tool_call_started","name":"read","argsPreview":"p"}),
                json!({"type":"tool_call_finished","toolCallId":"t","ok":true,"resultPreview":"yes"}),
                json!({"type":"permission_request","toolName":"read","requestId":"r"}),
                json!({"type":"structured_output","payload":{"x":1}}),
                json!({"type":"plan_mode_changed","mode":"plan"}),
                json!({"type":"error","code":"E","message":"bad"}),
                json!({"type":"unknown","text":"ignored"}),
            ];
            for (i, event) in events.into_iter().enumerate() {
                send(&mut ws,json!({"type":"event","id":id,"seq":if i%2==0 {19} else {0},"final":false,"event":event})).await;
            }
            send(&mut ws,json!({"type":"event","id":id,"seq":9,"final":true,"event":{"type":"assistant_text_delta","text":"FINAL-OMIT"}})).await;
        }
        let closed = timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("terminal connection retained");
        assert!(
            matches!(closed, Some(Ok(Message::Close(_))) | None),
            "{closed:?}"
        );
        let (success, stdout, stderr) = finish(child).await;
        assert!(success, "{stderr}");
        assert_eq!(
            stdout,
            baseline["stdout"].as_str().unwrap(),
            "TS CliChannel renderer differs"
        );
    }
    let boundary: Value =
        serde_json::from_str(include_str!("fixtures/cli-channel/boundary-oracle.json")).unwrap();
    for (case, argv, stdin) in [
        ("bare-cr-eof", vec![], "one\rtwo\r\nthree"),
        ("final-only", vec!["go"], ""),
        ("disconnect", vec!["go"], ""),
        ("broken-stdout", vec!["go"], ""),
    ] {
        let expected = &boundary["cases"][case];
        let mut process = child(&home, &project, &argv, stdin);
        if case == "broken-stdout" {
            drop(process.0.stdout.take());
        }
        let (mut health, _) = timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        assert!(
            request_head(&mut health)
                .await
                .starts_with(b"GET /health HTTP/1.1\r\n")
        );
        health
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        drop(health);
        let (socket, _) = timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut ws = timeout(Duration::from_secs(5), accept_async(socket))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next(&mut ws).await, expected["wire"][0]);
        send(&mut ws, json!({"type":"hello_ok"})).await;
        for expected_request in expected["wire"].as_array().unwrap().iter().skip(1) {
            let mut actual = next(&mut ws).await;
            let id = actual["id"].as_str().unwrap().to_owned();
            assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 4);
            actual["id"] = "<uuid>".into();
            let mut want = expected_request.clone();
            want["params"]["sessionKey"] =
                format!("cli:project={}:default", project.display()).into();
            assert_eq!(actual, want, "boundary submit envelope");
            if case == "disconnect" {
                timeout(Duration::from_secs(5), ws.close(None))
                    .await
                    .unwrap()
                    .unwrap();
                break;
            }
            if case != "final-only" {
                send(&mut ws,json!({"type":"event","id":id,"seq":0,"final":false,"event":{"type":"assistant_text_delta","text":"A"}})).await;
            }
            if case != "broken-stdout" {
                send(&mut ws,json!({"type":"event","id":id,"seq":1,"final":true,"event":{"type":"assistant_text_delta","text":"FINAL-OMIT"}})).await;
            }
        }
        let (ok, stdout, stderr) = finish(process).await;
        match case {
            "disconnect" => {
                assert!(!ok);
                assert!(stderr.contains(expected["stderr_contains"].as_str().unwrap()));
            }
            "broken-stdout" => {
                assert!(!ok);
                assert!(stderr.to_lowercase().contains("broken pipe"));
            }
            _ => {
                assert!(ok, "{stderr}");
                assert_eq!(stdout, expected["stdout"].as_str().unwrap());
            }
        }
    }
    // Keep stdin open and idle: the process must still service WebSocket ping.
    let mut interactive = ChildOwner(
        Command::new(env!("CARGO_BIN_EXE_pilotdeck"))
            .env_clear()
            .env("HOME", &home)
            .env("PILOT_HOME", &home)
            .current_dir(&project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let (mut health, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    assert!(
        request_head(&mut health)
            .await
            .starts_with(b"GET /health HTTP/1.1\r\n")
    );
    health
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    drop(health);
    let (socket, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ws = timeout(Duration::from_secs(5), accept_async(socket))
        .await
        .unwrap()
        .unwrap();
    let _hello = next(&mut ws).await;
    send(&mut ws, json!({"type":"hello_ok"})).await;
    ws.send(Message::Ping(b"idle-probe".to_vec().into()))
        .await
        .unwrap();
    let pong = timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("idle ping was not answered")
        .unwrap()
        .unwrap();
    assert!(matches!(pong, Message::Pong(payload) if payload.as_ref()==b"idle-probe"));
    interactive
        .0
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"first\n")
        .unwrap();
    let request = next(&mut ws).await;
    send(&mut ws, json!({"type":"event","id":request["id"],"seq":0,"final":false,"event":{"type":"assistant_text_delta","text":"A"}})).await;
    // Text must be observable before the final event, not just at process exit.
    let mut pipe = interactive.0.stdout.take().unwrap();
    let mut fds = [rustix::event::PollFd::new(
        &pipe,
        rustix::event::PollFlags::IN,
    )];
    let deadline = rustix::event::Timespec {
        tv_sec: 5,
        tv_nsec: 0,
    };
    assert!(
        rustix::event::poll(&mut fds, Some(&deadline)).unwrap() > 0,
        "text delta was not flushed"
    );
    let mut first = [0u8; 1];
    pipe.read_exact(&mut first).unwrap();
    assert_eq!(&first, b"A");
    interactive.0.stdout = Some(pipe);
    send(&mut ws, json!({"type":"event","id":request["id"],"seq":1,"final":true,"event":{"type":"turn_completed"}})).await;
    ws.send(Message::Ping(b"after-turn".to_vec().into()))
        .await
        .unwrap();
    let pong = timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("post-turn ping was not answered")
        .unwrap()
        .unwrap();
    assert!(matches!(pong, Message::Pong(payload) if payload.as_ref()==b"after-turn"));
    drop(interactive.0.stdin.take());
    assert!(matches!(
        timeout(Duration::from_secs(5), ws.next()).await.unwrap(),
        Some(Ok(Message::Close(_))) | None
    ));
    let (ok, stdout, stderr) = finish(interactive).await;
    assert!(ok, "{stderr}");
    assert_eq!(stdout, "\n");
}

/// A rejected submit is terminal even when the peer keeps its socket open.
/// The fixed TS ignores this response; see the isolated mbp2 rejection probe.
#[tokio::test(flavor = "current_thread")]
async fn rejected_submit_exits_without_waiting_for_final_or_peer_close() {
    let _lease = Lease::acquire().await;
    let listener = TcpListener::bind("127.0.0.1:18789")
        .await
        .expect("isolated channel test requires port 18789 free");
    let base = std::env::temp_dir().join(format!("pdrs-cli-rejection-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&base).unwrap();
    let base = fs::canonicalize(base).unwrap();
    let _scratch = Scratch(base.clone());
    let home = base.join("home");
    let project = base.join("project");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&project).unwrap();
    fs::write(home.join("server-token"), "channel-fake-token\n").unwrap();
    fs::write(home.join("pilotdeck.yaml"), "schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:1\n      apiKey: fake-only\n      models: {mock: {}}\n").unwrap();
    // A previously migrated isolated home keeps this transport test independent
    // of the build directory's bundled-skill installation layout.
    fs::write(
        home.join(".legacy-bundled-skills-migrated-v1"),
        "completed\n",
    )
    .unwrap();
    for (case, error, expected) in [
        (
            "denied",
            json!({"message":"FIXTURE_DENIED"}),
            "FIXTURE_DENIED\n",
        ),
        ("empty", json!({"message":""}), "\n"),
        ("missing", json!({}), "Gateway stream failed.\n"),
        (
            "non-string",
            json!({"message":false}),
            "Gateway stream failed.\n",
        ),
        ("surrogate", json!({}), "�\n"),
        ("other-id", json!({"message":"must be ignored"}), ""),
    ] {
        // stdin has another complete line: a rejected interactive submit must
        // stop rather than retry or submit the queued follow-up.
        let process = if case == "denied" {
            child(&home, &project, &[], "first\nnever-submit\n")
        } else {
            child(&home, &project, &["first"], "")
        };
        let (mut health, _) = timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        assert!(
            request_head(&mut health)
                .await
                .starts_with(b"GET /health HTTP/1.1\r\n")
        );
        health
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        drop(health);
        let (socket, _) = timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut ws = timeout(Duration::from_secs(5), accept_async(socket))
            .await
            .unwrap()
            .unwrap();
        let hello = next(&mut ws).await;
        assert_eq!(hello["type"], "hello");
        send(&mut ws, json!({"type":"hello_ok"})).await;
        let request = next(&mut ws).await;
        assert_eq!(request["method"], "submit_turn");
        assert_eq!(request["params"]["message"], "first");
        let id = request["id"].as_str().unwrap();
        if case == "surrogate" {
            let raw = format!(
                r#"{{"type":"response","id":{},"ok":false,"error":{{"message":"\ud800"}}}}"#,
                serde_json::to_string(id).unwrap()
            );
            ws.send(Message::Text(raw.into())).await.unwrap();
        } else {
            send(&mut ws, json!({"type":"response","id":if case=="other-id" {"unrelated"} else {id},"ok":false,"error":error})).await;
        }
        if case == "other-id" {
            // A successful acknowledgement is not a stream final either.
            send(
                &mut ws,
                json!({"type":"response","id":id,"ok":true,"result":{}}),
            )
            .await;
            send(&mut ws, json!({"type":"event","id":id,"final":false,"event":{"type":"assistant_text_delta","text":"EVENT_POSITIVE"}})).await;
            send(&mut ws, json!({"type":"event","id":id,"final":true})).await;
        }
        // The peer sends no close or event final on rejection. Only the client
        // can end this wait; a second request is a test failure, not swallowed.
        let closed = timeout(Duration::from_secs(3), ws.next())
            .await
            .expect("rejected submit kept its connection open");
        assert!(
            matches!(closed, Some(Ok(Message::Close(_))) | None),
            "{case}: {closed:?}"
        );
        let (success, stdout, stderr) = finish(process).await;
        assert_eq!(success, case == "other-id", "{case}");
        assert_eq!(
            stdout,
            if case == "other-id" {
                "EVENT_POSITIVE"
            } else {
                ""
            },
            "{case}"
        );
        assert_eq!(stderr, expected, "{case}");
    }
}

#[cfg(unix)]
type TokenIdentity = (u64, u32, u64, i64, i64);
#[cfg(unix)]
fn token_identity(path: &Path) -> TokenIdentity {
    use std::os::unix::fs::MetadataExt;
    let m = fs::symlink_metadata(path).unwrap();
    (m.ino(), m.mode(), m.len(), m.mtime(), m.mtime_nsec())
}
#[cfg(unix)]
struct TokenCase {
    _scratch: Scratch,
    home: PathBuf,
    project: PathBuf,
    token: PathBuf,
    yaml: &'static [u8],
    before: TokenIdentity,
    _held_writer: Option<fs::File>,
}
#[cfg(unix)]
impl TokenCase {
    fn new(kind: &str) -> Self {
        use std::os::unix::fs::OpenOptionsExt;
        let base = std::env::temp_dir().join(format!(
            "pdrs-discovery-token-{kind}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&base).unwrap();
        let base = fs::canonicalize(base).unwrap();
        let scratch = Scratch(base.clone());
        let home = base.join("home");
        let project = base.join("project");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&project).unwrap();
        let bundled = base.join("bundled");
        fs::create_dir_all(bundled.join("fixture")).unwrap();
        fs::write(
            bundled.join("fixture/SKILL.md"),
            "---\nname: fixture\ndescription: Offline token regression.\n---\nNo operation.\n",
        )
        .unwrap();
        fs::write(
            home.join(".legacy-bundled-skills-migrated-v1"),
            "completed\n",
        )
        .unwrap();
        let yaml: &[u8] = b"schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:1\n      apiKey: fake-only\n      models: {mock: {}}\n";
        fs::write(home.join("pilotdeck.yaml"), yaml).unwrap();
        let token = home.join("server-token");
        let mut held_writer = None;
        if kind.starts_with("fifo-") {
            assert!(
                Command::new("/usr/bin/mkfifo")
                    .arg(&token)
                    .status()
                    .unwrap()
                    .success()
            );
            if kind == "fifo-held-writer" {
                held_writer = Some(
                    fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .custom_flags(libc::O_NONBLOCK)
                        .open(&token)
                        .unwrap(),
                );
            }
        } else if kind == "over-limit" || kind == "sparse-large" {
            fs::File::create(&token)
                .unwrap()
                .set_len(if kind == "over-limit" {
                    65_537
                } else {
                    32 * 1024 * 1024
                })
                .unwrap();
        } else if kind == "valid-boundary" {
            let mut bytes = vec![b' '; 65_536];
            let value = b"channel-fake-token";
            bytes[1..1 + value.len()].copy_from_slice(value);
            fs::write(&token, bytes).unwrap();
        } else {
            fs::write(&token, "\u{feff}\t channel-fake-token \r\n").unwrap();
        }
        let before = token_identity(&token);
        Self {
            _scratch: scratch,
            home,
            project,
            token,
            yaml,
            before,
            _held_writer: held_writer,
        }
    }
    fn spawn(&self, argv: &[&str]) -> ChildOwner {
        self.spawn_with_stdin(argv, Stdio::null())
    }
    #[cfg(target_os = "macos")]
    fn spawn_tui(&self) -> (ChildOwner, std::os::fd::OwnedFd) {
        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
        use std::os::unix::ffi::OsStrExt;
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        rustix::io::fcntl_setfd(&master, rustix::io::FdFlags::CLOEXEC).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let name = ptsname(&master, Vec::new()).unwrap();
        let slave = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(std::ffi::OsStr::from_bytes(name.as_bytes()))
            .unwrap();
        let process = self.spawn_with_stdin(&["tui"], Stdio::from(slave));
        (process, master)
    }
    fn spawn_with_stdin(&self, argv: &[&str], stdin: Stdio) -> ChildOwner {
        ChildOwner(
            Command::new(env!("CARGO_BIN_EXE_pilotdeck"))
                .env_clear()
                .env("HOME", &self.home)
                .env("PILOT_HOME", &self.home)
                .env(
                    "PILOTDECK_BUNDLED_SKILLS_DIR",
                    self._scratch.0.join("bundled"),
                )
                // A regression to fallback must neither launch a helper nor
                // contact the fixture model URL; this path intentionally is absent.
                .env(
                    "PILOTDECK_RS_SIDECAR",
                    self._scratch.0.join("must-not-launch-sidecar"),
                )
                .current_dir(&self.project)
                .args(argv)
                .stdin(stdin)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        )
    }
    fn assert_unchanged(&self, kind: &str) {
        assert_eq!(
            token_identity(&self.token),
            self.before,
            "{kind}: rejected/read token identity unchanged"
        );
        assert_eq!(
            fs::read(self.home.join("pilotdeck.yaml")).unwrap(),
            self.yaml,
            "{kind}: configuration unchanged"
        );
        assert!(
            !self.home.join("projects").exists(),
            "{kind}: no in-process host state"
        );
    }
}
#[cfg(unix)]
async fn answer_token_health(listener: &TcpListener) {
    let (mut health, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    assert!(
        request_head(&mut health)
            .await
            .starts_with(b"GET /health HTTP/1.1\r\n")
    );
    timeout(
        Duration::from_secs(3),
        health.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
    )
    .await
    .unwrap()
    .unwrap();
    drop(health);
}
#[cfg(unix)]
async fn assert_token_resource_refusal(process: ChildOwner, listener: &TcpListener, kind: &str) {
    let output = finish_output(process).await;
    assert_eq!(output.status.code(), Some(1), "{kind}");
    assert!(output.stdout.is_empty(), "{kind}");
    let expected = if kind.starts_with("fifo-") {
        "Gateway auth token file must be a regular file.\n"
    } else {
        "Gateway auth token file exceeds the 65536-byte limit.\n"
    };
    assert_eq!(
        output.stderr,
        expected.as_bytes(),
        "{kind}: explicit error without local fallback"
    );
    assert!(
        timeout(Duration::from_millis(150), listener.accept())
            .await
            .is_err(),
        "{kind}: special file must fail before WebSocket connection"
    );
}

/// Default-channel file policy is shared with the SDK but selected only after
/// this entry's actual discovery health check. Never contact an existing host.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn default_channel_token_files_are_bounded_and_preserve_ordinary_wire() {
    use std::os::unix::process::ExitStatusExt;
    let _lease = Lease::acquire().await;
    let listener = TcpListener::bind("127.0.0.1:18789")
        .await
        .expect("owned token regression requires port 18789 free");
    let oracle: Value =
        serde_json::from_str(include_str!("fixtures/cli-channel/ts-oracle.json")).unwrap();
    let baseline = &oracle["cases"]["argv"];
    for kind in [
        "fifo-empty",
        "fifo-held-writer",
        "over-limit",
        "sparse-large",
        "valid-trim",
        "valid-boundary",
        "cancel",
    ] {
        let case = TokenCase::new(kind);
        let project = &case.project;
        let process = case.spawn(&["hello", "--strange", " world "]);
        answer_token_health(&listener).await;
        if kind.starts_with("fifo-") || kind == "over-limit" || kind == "sparse-large" {
            assert_token_resource_refusal(process, &listener, kind).await;
        } else {
            let (socket, _) = timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut ws = timeout(Duration::from_secs(5), accept_async(socket))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                next(&mut ws).await,
                baseline["wire"][0],
                "{kind}: same trimmed TS auth token"
            );
            send(&mut ws,json!({"type":"hello_ok","protocolVersion":"1.2","serverVersion":"fixture","serverInfo":{"capabilities":[]}})).await;
            let mut request = next(&mut ws).await;
            let id = request["id"].as_str().unwrap().to_owned();
            assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 4);
            request["id"] = "<uuid>".into();
            let mut expected = baseline["wire"][1].clone();
            expected["params"]["sessionKey"] =
                format!("cli:project={}:default", project.display()).into();
            assert_eq!(request, expected, "{kind}: unchanged TS submit request");
            if kind == "cancel" {
                rustix::process::kill_process(
                    rustix::process::Pid::from_raw(process.0.id() as i32).unwrap(),
                    rustix::process::Signal::TERM,
                )
                .unwrap();
                let output = finish_output(process).await;
                assert_eq!(
                    output.status.signal(),
                    Some(15),
                    "owned legacy channel receives SIGTERM"
                );
                assert!(output.stdout.is_empty());
                assert!(output.stderr.is_empty());
                assert!(
                    matches!(
                        timeout(Duration::from_secs(3), ws.next()).await.unwrap(),
                        None | Some(Ok(Message::Close(_))) | Some(Err(_))
                    ),
                    "cancelled owner must close its one WebSocket"
                );
            } else {
                let events = [
                    json!({"type":"assistant_text_delta","text":"A"}),
                    json!({"type":"assistant_thinking_delta","text":"h"}),
                    json!({"type":"tool_call_started","name":"read","argsPreview":"p"}),
                    json!({"type":"tool_call_finished","toolCallId":"t","ok":true,"resultPreview":"yes"}),
                    json!({"type":"permission_request","toolName":"read","requestId":"r"}),
                    json!({"type":"structured_output","payload":{"x":1}}),
                    json!({"type":"plan_mode_changed","mode":"plan"}),
                    json!({"type":"error","code":"E","message":"bad"}),
                    json!({"type":"unknown","text":"ignored"}),
                ];
                for (seq, event) in events.into_iter().enumerate() {
                    send(
                        &mut ws,
                        json!({"type":"event","id":id,"seq":seq,"final":false,"event":event}),
                    )
                    .await;
                }
                send(&mut ws,json!({"type":"event","id":id,"seq":9,"final":true,"event":{"type":"assistant_text_delta","text":"FINAL-OMIT"}})).await;
                assert!(matches!(
                    timeout(Duration::from_secs(5), ws.next()).await.unwrap(),
                    None | Some(Ok(Message::Close(_)))
                ));
                let output = finish_output(process).await;
                assert_eq!(
                    output.status.code(),
                    Some(0),
                    "{kind}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(
                    output.stdout,
                    baseline["stdout"].as_str().unwrap().as_bytes(),
                    "{kind}: fixed TS stdout bytes"
                );
                assert!(output.stderr.is_empty(), "{kind}: {:?}", output.stderr);
            }
        }
        case.assert_unchanged(kind);
    }
}

/// Both command entry points must propagate the shared reader's resource error
/// after their actual health check, before WS or local-host/terminal startup.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "current_thread")]
async fn cron_and_tui_token_resources_fail_before_connection_or_local_fallback() {
    let _lease = Lease::acquire().await;
    let listener = TcpListener::bind("127.0.0.1:18789")
        .await
        .expect("owned cron/TUI token regression requires port 18789 free");
    for argv in [vec!["cron", "list"], vec!["tui"]] {
        for kind in [
            "fifo-empty",
            "fifo-held-writer",
            "over-limit",
            "sparse-large",
        ] {
            let case = TokenCase::new(kind);
            let (process, _terminal) = if argv[0] == "tui" {
                let (process, master) = case.spawn_tui();
                (process, Some(master))
            } else {
                (case.spawn(&argv), None)
            };
            answer_token_health(&listener).await;
            assert_token_resource_refusal(process, &listener, kind).await;
            case.assert_unchanged(kind);
        }
    }
}

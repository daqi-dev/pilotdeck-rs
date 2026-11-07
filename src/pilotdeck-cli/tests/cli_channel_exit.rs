//! Isolated process test against TS-recorded legacy channel frames.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
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
async fn finish(mut child: ChildOwner) -> (bool, String, String) {
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
    (child.0.wait().unwrap().success(), out, err)
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

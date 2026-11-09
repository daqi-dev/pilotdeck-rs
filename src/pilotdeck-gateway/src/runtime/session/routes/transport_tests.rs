use super::*;
use crate::{
    metadata::object,
    runtime::{
        session::sidecar_session::SidecarSpawner,
        turn::wiring::{self, CompositionOptions},
    },
};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use std::{
    collections::HashSet,
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

/// Exercises missing sidecar binary / stdio spawn failures; not the embedded agent path.
struct SubprocessSidecarSpawner {
    executable: std::path::PathBuf,
    cwd: std::path::PathBuf,
}

impl SidecarSpawner for SubprocessSidecarSpawner {
    fn requires_subprocess(&self) -> bool {
        true
    }

    fn command(&self) -> std::io::Result<tokio::process::Command> {
        let mut command = tokio::process::Command::new(&self.executable);
        command.arg("--stdio").current_dir(&self.cwd);
        Ok(command)
    }
}

fn compose(home: &Path) -> wiring::GatewayComposition {
    compose_with_fork_ports(home, wiring::HistoryForkPorts::default())
}

fn compose_with_fork_ports(
    home: &Path,
    fork_ports: wiring::HistoryForkPorts,
) -> wiring::GatewayComposition {
    let provider_home = home.to_path_buf();
    let provider_cwd = home.to_path_buf();
    wiring::compose_with_fork_ports(
        CompositionOptions {
            policies: Default::default(),
            activity: Default::default(),
            background: Arc::new(
                crate::cron::runtime::BackgroundService::new(std::env::temp_dir()).unwrap(),
            ),
            cron: Default::default(),
            pilot_home: home.to_path_buf(),
            cwd: home.to_string_lossy().as_ref().into(),
            spawner: Arc::new(SubprocessSidecarSpawner {
                executable: home.join("unused-sidecar"),
                cwd: home.to_path_buf(),
            }),
            providers: Box::new(move |owner| {
                wiring::disk_providers(provider_home, provider_cwd, owner)
            }),
            failure_guard_enabled: false,
            abort_turn_timeout_ms: 1000,
            server_version: "session-test".into(),
        },
        fork_ports,
    )
    .unwrap()
}

#[derive(Default)]
struct ExecutionForkProbe {
    began: Arc<AtomicUsize>,
    committed: Arc<AtomicUsize>,
    rolled_back: Arc<AtomicUsize>,
}

impl crate::runtime::session::routes::history::ExecutionStorageForkPort for ExecutionForkProbe {
    fn begin(
        &self,
        _: pilotdeck_protocol::JsString,
        _: pilotdeck_protocol::JsString,
    ) -> crate::runtime::session::routes::history::ForkFuture<
        '_,
        Arc<dyn crate::runtime::session::routes::history::ExecutionStorageFork>,
    > {
        let began = self.began.clone();
        let transaction: Arc<dyn crate::runtime::session::routes::history::ExecutionStorageFork> =
            Arc::new(ExecutionForkTransaction {
                committed: self.committed.clone(),
                rolled_back: self.rolled_back.clone(),
            });
        Box::pin(async move {
            tokio::task::yield_now().await;
            began.fetch_add(1, Ordering::SeqCst);
            Ok(transaction)
        })
    }
}

struct ExecutionForkTransaction {
    committed: Arc<AtomicUsize>,
    rolled_back: Arc<AtomicUsize>,
}

impl crate::runtime::session::routes::history::ExecutionStorageFork for ExecutionForkTransaction {
    fn commit(&self) -> crate::runtime::session::routes::history::ForkFuture<'_, ()> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            self.committed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    fn rollback(&self) -> crate::runtime::session::routes::history::ForkFuture<'_, ()> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            self.rolled_back.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

struct Socket {
    io: tokio::io::DuplexStream,
    decoder: frame::Decoder,
}
impl Socket {
    async fn connect(composition: wiring::GatewayComposition, path: &str) -> Self {
        let endpoint = Arc::new(NativeEndpoint {
            gateway: composition.turn,
            token: "session-token".into(),
            server_version: "test".into(),
        });
        let proxy = Arc::new(Proxy {
            upstream: "http://127.0.0.1:1".into(),
            client: Client::builder(TokioExecutor::new()).build_http(),
        });
        let (mut io, server) = tokio::io::duplex(1 << 20);
        tokio::spawn(endpoint.serve_connection(proxy, server));
        io.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: key\r\n\r\n").as_bytes()).await.unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(io.read_u8().await.unwrap());
        }
        assert!(headers.starts_with(b"HTTP/1.1 101"));
        let mut socket = Self {
            io,
            decoder: frame::Decoder::default(),
        };
        socket.send(&parse_json(r#"{"type":"hello","protocolVersion":"1.2","clientName":"session-test","clientVersion":"1","token":"session-token"}"#).unwrap()).await;
        let hello = socket.read().await;
        assert_eq!(hello.get("type"), Some(&"hello_ok".into()));
        assert_eq!(
            hello.get("serverInfo").unwrap().get("mode"),
            Some(&"in_process".into())
        );
        socket
    }
    async fn send(&mut self, value: &JsValue) {
        let payload = stringify_json(value).unwrap().unwrap();
        let length = if payload.len() < 126 {
            payload.len() as u8
        } else if payload.len() <= 65535 {
            126
        } else {
            127
        };
        self.io
            .write_all(&frame::client_frame(0x81, length, payload.as_bytes()))
            .await
            .unwrap();
    }
    async fn read(&mut self) -> JsValue {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(frame) = self.decoder.next_server().unwrap() {
                    assert_eq!(frame.opcode, 1, "unexpected websocket close/control");
                    return parse_json(&String::from_utf8(frame.payload).unwrap()).unwrap();
                }
                assert_ne!(self.io.read_buf(&mut self.decoder.buffer).await.unwrap(), 0);
            }
        })
        .await
        .expect("SESSION response timeout")
    }
    async fn call(
        &mut self,
        method: &str,
        params: JsValue,
        covered: &mut HashSet<String>,
    ) -> JsValue {
        covered.insert(method.to_owned());
        self.send(&object([
            ("type", "request".into()),
            ("id", method.into()),
            ("method", method.into()),
            ("params", params),
        ]))
        .await;
        let response = self.read().await;
        assert_eq!(response.get("id"), Some(&method.into()));
        assert_eq!(
            response.get("ok"),
            Some(&JsValue::Bool(true)),
            "{method}: {response:?}"
        );
        response.get("result").cloned().unwrap()
    }
}

fn input(home: &Path, session: &str) -> JsValue {
    object([
        ("sessionKey", session.into()),
        ("projectKey", home.to_string_lossy().as_ref().into()),
    ])
}
fn with(input: &JsValue, fields: &[(&str, JsValue)]) -> JsValue {
    let mut out = input.as_object().cloned().unwrap();
    for (name, value) in fields {
        out.insert((*name).into(), value.clone());
    }
    JsValue::Object(out)
}

#[tokio::test]
async fn production_composition_injects_execution_fork_transactions() {
    let scratch = crate::scratch::Scratch::new("session-execution-fork-port");
    let home = scratch.path();
    std::fs::write(
        home.join("pilotdeck.yaml"),
        "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n",
    )
    .unwrap();
    let probe = Arc::new(ExecutionForkProbe::default());
    let params = input(home, "execution-fork");
    let mut covered = HashSet::new();
    let mut socket = Socket::connect(
        compose_with_fork_ports(
            home,
            wiring::HistoryForkPorts {
                session: None,
                execution: Some(probe.clone()),
            },
        ),
        "/ws",
    )
    .await;
    socket
        .call(
            "restore_session_transcript",
            with(
                &params,
                &[(
                    "archive",
                    parse_json(
                        r#"{"schemaVersion":1,"format":"portable_text_messages","messages":[{"role":"user","text":"fork source"},{"role":"assistant","text":"source answer"}]}"#,
                    )
                    .unwrap(),
                )],
            ),
            &mut covered,
        )
        .await;
    let transcript =
        crate::session_chat_dir(home, home, &home.to_string_lossy()).join("execution-fork.jsonl");
    let entries = crate::storage::read_transcript(&transcript)
        .unwrap()
        .entries;
    let accepted = entries
        .iter()
        .find(|entry| entry.get("type") == Some(&"accepted_input".into()))
        .unwrap();
    let result = socket
        .call(
            "fork_session",
            with(
                &params,
                &[("fromEntryId", accepted.get("entryId").unwrap().clone())],
            ),
            &mut covered,
        )
        .await;
    assert!(
        result
            .get("newSessionKey")
            .and_then(JsValue::as_string)
            .is_some()
    );
    assert_eq!(probe.began.load(Ordering::SeqCst), 1);
    assert_eq!(probe.committed.load(Ordering::SeqCst), 1);
    assert_eq!(probe.rolled_back.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pending_replacement_recovers_before_restarted_direct_submit_reads_history() {
    let scratch = crate::scratch::Scratch::new("session-replacement-restart");
    let home = scratch.path();
    std::fs::write(home.join("pilotdeck.yaml"), "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n").unwrap();
    let params = input(home, "restart");
    let mut covered = HashSet::new();
    let mut socket = Socket::connect(compose(home), "/ws").await;
    socket.call("restore_session_transcript", with(&params, &[("archive", parse_json(r#"{"schemaVersion":1,"format":"portable_text_messages","messages":[{"role":"user","text":"original question"},{"role":"assistant","text":"original answer"}]}"#).unwrap())]), &mut covered).await;
    let transcript =
        crate::session_chat_dir(home, home, &home.to_string_lossy()).join("restart.jsonl");
    let original = std::fs::read(&transcript).unwrap();
    let entries = crate::storage::read_transcript(&transcript)
        .unwrap()
        .entries;
    let accepted = entries
        .iter()
        .find(|e| e.get("type") == Some(&"accepted_input".into()))
        .unwrap();
    socket
        .call(
            "replace_last_turn",
            with(
                &params,
                &[
                    ("expectedTurnId", accepted.get("turnId").unwrap().clone()),
                    ("replacementTurnId", "abandoned-edit".into()),
                ],
            ),
            &mut covered,
        )
        .await;
    assert_ne!(std::fs::read(&transcript).unwrap(), original);
    drop(socket);
    for entry in std::fs::read_dir(transcript.parent().unwrap()).unwrap() {
        let path = entry.unwrap().path();
        if path.to_string_lossy().ends_with(".replace.json") {
            let mut journal: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            journal["owner"] = serde_json::Value::Null;
            std::fs::write(path, serde_json::to_vec(&journal).unwrap()).unwrap();
        }
    }
    let mut restarted = Socket::connect(compose(home), "/ws").await;
    restarted
        .send(&object([
            ("type", "request".into()),
            ("id", "direct-submit".into()),
            ("method", "submit_turn".into()),
            (
                "params",
                with(
                    &params,
                    &[
                        ("runId", "fresh-turn".into()),
                        ("message", "new after restart".into()),
                    ],
                ),
            ),
        ]))
        .await;
    loop {
        let response = restarted.read().await;
        // submit_turn owns a seq/final event stream, not a single response.
        // Drain its terminal before the next request (TS GatewayWsConnection:152-168).
        if response.get("id") == Some(&"direct-submit".into())
            && response.get("final") == Some(&JsValue::Bool(true))
        {
            break;
        }
    }
    let bytes = std::fs::read(&transcript).unwrap();
    let read = crate::storage::read_transcript(&transcript).unwrap();
    assert!(!read.diagnostics.iter().any(|d| d.severity == "error"));
    assert!(
        read.entries
            .iter()
            .any(|e| e.get("type") == Some(&"accepted_input".into())
                && e.get("turnId") == Some(&"fresh-turn".into())),
        "direct submit must accept its real input before the missing-sidecar execution error"
    );
    assert!(String::from_utf8_lossy(&bytes).contains("original answer"));
    assert!(String::from_utf8_lossy(&bytes).contains("new after restart"));
    restarted
        .call("read_session_messages", params.clone(), &mut covered)
        .await;
    assert_eq!(
        std::fs::read(&transcript).unwrap(),
        bytes,
        "later recovery must not overwrite new input"
    );
    restarted.call("close_session", params, &mut covered).await;
}

#[tokio::test]
async fn session19_both_ws_paths_work_without_upstream_and_reopen_history() {
    for path in ["/ws", "/runtime/ws"] {
        let scratch = crate::scratch::Scratch::new("session19-ws");
        let home = scratch.path();
        std::fs::write(home.join("pilotdeck.yaml"), "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n").unwrap();
        std::fs::write(home.join("seed.txt"), "before seed\n").unwrap();
        let mut socket = Socket::connect(compose(home), path).await;
        let mut covered = HashSet::new();
        let info = socket
            .call("describe_server", JsValue::Null, &mut covered)
            .await;
        assert!(info.get("name").is_none());
        let session = socket
            .call(
                "new_session",
                object([("channelKey", "web".into())]),
                &mut covered,
            )
            .await;
        let key = session
            .get("sessionKey")
            .and_then(JsValue::as_string)
            .unwrap()
            .to_utf8()
            .unwrap();
        let params = input(home, &key);
        assert_eq!(
            socket
                .call("resume_session", params.clone(), &mut covered)
                .await,
            params
        );
        let archive = parse_json(r#"{"schemaVersion":1,"format":"portable_text_messages","title":"Imported","messages":[{"role":"user","text":"question"},{"role":"assistant","text":"answer"}]}"#).unwrap();
        socket
            .call(
                "restore_session_transcript",
                with(&params, &[("archive", archive)]),
                &mut covered,
            )
            .await;
        socket
            .call(
                "rename_session",
                with(&params, &[("value", "Renamed".into())]),
                &mut covered,
            )
            .await;
        socket
            .call(
                "tag_session",
                with(&params, &[("value", "tagged".into())]),
                &mut covered,
            )
            .await;
        let listing = socket
            .call(
                "list_sessions",
                object([("projectKey", home.to_string_lossy().as_ref().into())]),
                &mut covered,
            )
            .await;
        assert!(
            matches!(listing.get("sessions"), Some(JsValue::Array(items)) if !items.is_empty())
        );
        let exported = socket
            .call("export_session_transcript", params.clone(), &mut covered)
            .await;
        assert!(
            matches!(exported.get("messages"), Some(JsValue::Array(items)) if items.len() == 2)
        );
        let read = socket
            .call("read_session_messages", params.clone(), &mut covered)
            .await;
        assert!(matches!(read.get("messages"), Some(JsValue::Array(items)) if items.len() == 2));
        socket
            .call(
                "read_subagent_messages",
                with(&params, &[("subagentId", "missing".into())]),
                &mut covered,
            )
            .await;
        let reconnect = socket
            .call("reconnect_interaction", params.clone(), &mut covered)
            .await;
        assert_eq!(reconnect.get("outcome"), Some(&"no_pending".into()));
        let status =
            parse_json(r#"{"event":"integration","kind":"info","text":"stored"}"#).unwrap();
        socket
            .call(
                "record_agent_status_message",
                with(&params, &[("turnId", "status".into()), ("status", status)]),
                &mut covered,
            )
            .await;
        let transcript =
            crate::session_chat_dir(home, home, &home.to_string_lossy()).join(format!(
                "{}.jsonl",
                crate::storage::sanitize_session_id(&key.as_str().into())
            ));
        let entries = crate::storage::read_transcript(&transcript)
            .unwrap()
            .entries;
        let accepted = entries
            .iter()
            .find(|entry| entry.get("type") == Some(&"accepted_input".into()))
            .unwrap();
        socket
            .call(
                "fork_session",
                with(
                    &params,
                    &[("fromEntryId", accepted.get("entryId").unwrap().clone())],
                ),
                &mut covered,
            )
            .await;
        let replacement = socket
            .call(
                "replace_last_turn",
                with(
                    &params,
                    &[
                        ("expectedTurnId", accepted.get("turnId").unwrap().clone()),
                        ("replacementTurnId", "edited".into()),
                    ],
                ),
                &mut covered,
            )
            .await;
        socket
            .call(
                "finalize_last_turn_replacement",
                with(
                    &params,
                    &[
                        (
                            "transactionId",
                            replacement.get("transactionId").unwrap().clone(),
                        ),
                        ("action", "rollback".into()),
                    ],
                ),
                &mut covered,
            )
            .await;
        let before_close = socket
            .call("read_session_messages", params.clone(), &mut covered)
            .await;
        let mtime = (std::fs::metadata(home.join("seed.txt"))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
            * 1000.0)
            .floor();
        let seeded = socket
            .call(
                "seed_read_state",
                with(
                    &params,
                    &[
                        ("path", "seed.txt".into()),
                        ("mtime", JsValue::Number(mtime)),
                    ],
                ),
                &mut covered,
            )
            .await;
        assert_eq!(seeded.get("applied"), Some(&JsValue::Bool(true)));
        socket
            .call("close_session", params.clone(), &mut covered)
            .await;
        let mut cold = Socket::connect(compose(home), path).await;
        let cold_read = cold
            .call("read_session_messages", params.clone(), &mut covered)
            .await;
        assert_eq!(cold_read.get("messages"), before_close.get("messages"));
        cold.call("delete_session", params.clone(), &mut covered)
            .await;
        assert!(!transcript.exists());
        cold.call(
            "close_project_sessions",
            object([("projectKey", home.to_string_lossy().as_ref().into())]),
            &mut covered,
        )
        .await;
        assert_eq!(covered.len(), 19);
        for method in session_routes::METHODS {
            assert!(covered.contains(*method), "missing {method}");
        }
    }
}

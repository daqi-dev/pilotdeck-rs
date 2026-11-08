//! Actual product backend factory and SDK-free UDS wire; physical Store/process
//! faults remain the independent BOOT product fixture's responsibility.
use super::*;
use crate::runtime::session::factory::{ReloadSource, SessionProjection};
use crate::runtime::turn::runner::tail::{SessionTail, TailConfig};
use crate::storage::{JsonlTranscriptStore, TranscriptKey, recorder::Recorder};
use tokio::net::UnixListener;
fn context(root: &std::path::Path) -> BackendContext {
    if !root.join("transcript.jsonl").exists() {
        std::fs::write(root.join("transcript.jsonl"), b"").unwrap();
    }
    let recorder = Arc::new(Recorder::new(
        Arc::new(JsonlTranscriptStore::new()),
        TranscriptKey {
            project_root: root.into(),
            pilot_home: root.into(),
            session_id: "product-session".into(),
            transcript_path: root.join("transcript.jsonl"),
        },
    ));
    BackendContext {
        recorder: recorder.clone(),
        history: Arc::new(Vec::new),
        tail: SessionTail::new(recorder, "product-session".into(), TailConfig::default()),
        reload_source: ReloadSource {
            session_id: "product-session".into(),
            cwd: root.into(),
            transcript_path: root.join("transcript.jsonl"),
            projection: Arc::new(SessionProjection::default()),
            file_state: Arc::new(|| Ok(Default::default())),
        },
    }
}
fn factory(root: &std::path::Path, socket: &std::path::Path) -> Box<KernelBackendFactory> {
    Box::new(KernelBackendFactory {
        preflight: None,
        socket: socket.into(),
        loop_version: "fixed-revision".into(),
        durable_create: true,
        creation_query_only: false,
        owner_path: root.join("transcript.jsonl.backend.json"),
        project: root.into(),
        session: "product-session".into(),
        interactions: None,
        agent_config: JsObject::new(),
        permission_mode_source: None,
    })
}
fn scratch() -> PathBuf {
    let root = PathBuf::from("/tmp").join(format!("pd-boot-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    root
}
#[tokio::test]
async fn lost_creation_ack_reopens_original_key_by_query_and_never_creates_again() {
    for (query_confirmed, malformed, owner_fault) in [
        (true, false, false),
        (false, false, false),
        (true, true, false),
        (true, false, true),
    ] {
        let root = scratch();
        let socket = root.join("k.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let observed = requests.clone();
        let server = tokio::spawn(async move {
            let mut original_key = None;
            let mut creation_pin = Value::Null;
            for connection in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                loop {
                    let mut line = String::new();
                    if stream.read_line(&mut line).await.unwrap() == 0 {
                        break;
                    }
                    let request: Value = serde_json::from_str(&line).unwrap();
                    observed.lock().unwrap().push(request.clone());
                    let result = match request["method"].as_str().unwrap() {
                        "initialize" => {
                            json!({"server_capabilities":{"session_creation":{"version":1,"query_method":"session.creation_get","request_key":"idempotency_key","future_extension":true}}})
                        }
                        "session.create" => {
                            assert_eq!(connection, 0);
                            assert!(original_key.is_none());
                            original_key = Some(request["params"]["idempotency_key"].clone());
                            creation_pin = request["params"]["creation_pin"].clone();
                            assert_eq!(creation_pin["loop_revision"], "fixed-revision");
                            // Request accepted by the peer; its ACK is lost.
                            break;
                        }
                        "session.creation_get" => {
                            assert_eq!(connection, 1);
                            assert_eq!(
                                Some(request["params"]["idempotency_key"].clone()),
                                original_key
                            );
                            if query_confirmed {
                                {
                                    use sha2::{Digest, Sha256};
                                    let canonical = json!({"version":1,"session_spec":"host-spec","creation_pin":creation_pin});
                                    let digest = format!(
                                        "{:x}",
                                        Sha256::digest(serde_json::to_vec(&canonical).unwrap())
                                    );
                                    json!({"status":"done","effect":"read-only","data":{"version":1,"scope":{"principal":"fixture-principal","tenant":null},"creation_key":original_key,"request_digest":digest,"session_id":"original-session","generation":1,"request":canonical,"initialization":if malformed {json!([])} else {json!([{"kind":"fixture-session-spec","payload":[1,2,3]}])},"status":"confirmed"}})
                                }
                            } else {
                                json!({"status":"done","effect":"read-only","data":{"status":"not_observed"}})
                            }
                        }
                        "execution.open" => {
                            assert!(query_confirmed);
                            assert_eq!(request["params"]["session_id"], "original-session");
                            json!({"input_controller_ref":null,"default_loop_ref":"pd","loop_revision":"fixed-revision","protocol":"scheduler-v1","loop_permissions":{},"features":[]})
                        }
                        "execution.events" => json!([]),
                        "execution.history.snapshot" => json!({"revision":0,"history":[]}),
                        method => panic!("BOOT must not execute or acquire a lease: {method}"),
                    };
                    let reply = if request["method"] == "session.creation_get" {
                        result
                    } else {
                        json!({"jsonrpc":"2.0","id":request["id"],"result":result})
                    };
                    stream
                        .get_mut()
                        .write_all(format!("{reply}\n").as_bytes())
                        .await
                        .unwrap();
                }
            }
        });
        let error = match factory(&root, &socket).build(context(&root)).await {
            Ok(_) => panic!("lost ACK must remain unknown"),
            Err(error) => error,
        };
        assert_eq!(
            error.js_error().and_then(|e| e.code.as_deref()),
            Some("KERNEL_OUTCOME_UNKNOWN")
        );
        let before: Value = serde_json::from_slice(
            &std::fs::read(root.join("transcript.jsonl.backend.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(before["creation_state"], "pending");
        assert!(before["creation_key"].is_string());
        if owner_fault {
            std::fs::create_dir(root.join("transcript.jsonl.backend.json.creation.confirming"))
                .unwrap();
        }
        let second = factory(&root, &socket).build(context(&root)).await;
        if query_confirmed && !malformed && !owner_fault {
            let backend = second.unwrap_or_else(|e| panic!("{e}"));
            backend.dispose().await.unwrap();
            drop(backend);
        } else {
            let error = match second {
                Ok(_) => panic!("not_observed cannot prove absence"),
                Err(error) => error,
            };
            assert_eq!(
                error.js_error().and_then(|e| e.code.as_deref()),
                Some("KERNEL_OUTCOME_UNKNOWN")
            );
        }
        server.await.unwrap();
        let after: Value = serde_json::from_slice(
            &std::fs::read(root.join("transcript.jsonl.backend.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(after["creation_key"], before["creation_key"]);
        assert_eq!(
            after["creation_state"],
            if query_confirmed && !malformed && !owner_fault {
                "confirmed"
            } else {
                "pending"
            }
        );
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r["method"] == "session.create")
                .count(),
            1
        );
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r["method"] == "session.creation_get")
                .count(),
            1
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[tokio::test]
async fn unsupported_host_is_refused_before_dispatch_and_keeps_one_pending_identity() {
    let root = scratch();
    let socket = root.join("k.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], "initialize");
        stream
            .get_mut()
            .write_all(
                format!(
                    "{}\n",
                    json!({"jsonrpc":"2.0","id":request["id"],"result":{"server_capabilities":{}}})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        line.clear();
        assert_eq!(
            stream.read_line(&mut line).await.unwrap(),
            0,
            "unsupported host must not receive create"
        );
    });
    let error = match factory(&root, &socket).build(context(&root)).await {
        Ok(_) => panic!("capability required"),
        Err(error) => error,
    };
    assert_eq!(
        error.js_error().and_then(|e| e.code.as_deref()),
        Some("KERNEL_UNCONFIGURED_PORT")
    );
    assert!(
        !root
            .join("transcript.jsonl.backend.json.creation.dispatch")
            .exists()
    );
    let owner: Value =
        serde_json::from_slice(&std::fs::read(root.join("transcript.jsonl.backend.json")).unwrap())
            .unwrap();
    assert_eq!(owner["creation_state"], "pending");
    server.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn readonly_pending_open_queries_without_creating_or_acquiring_a_lease() {
    let root = scratch();
    let socket = root.join("k.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        for method in ["initialize", "session.creation_get"] {
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], method);
            let result = if method == "initialize" {
                json!({"server_capabilities":{"session_creation":{"version":1,"query_method":"session.creation_get","request_key":"idempotency_key"}}})
            } else {
                assert!(request["params"]["idempotency_key"].is_string());
                json!({"status":"done","effect":"read-only","data":{"status":"not_observed"}})
            };
            let reply = if method == "session.creation_get" {
                result
            } else {
                json!({"jsonrpc":"2.0","id":request["id"],"result":result})
            };
            stream
                .get_mut()
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
        }
        let mut line = String::new();
        assert_eq!(
            stream.read_line(&mut line).await.unwrap(),
            0,
            "read/query cannot create or execute"
        );
    });
    let mut readonly = factory(&root, &socket);
    readonly.creation_query_only = true;
    let error = match readonly.build(context(&root)).await {
        Ok(_) => panic!("query did not confirm creation"),
        Err(error) => error,
    };
    assert_eq!(
        error.js_error().and_then(|e| e.code.as_deref()),
        Some("KERNEL_OUTCOME_UNKNOWN")
    );
    let owner = root.join("transcript.jsonl.backend.json");
    assert!(!PathBuf::from(format!("{}.creation.dispatch", owner.display())).exists());
    let original = persisted_owner(&owner).unwrap().unwrap();
    assert_eq!(original["creation_state"], "pending");
    // A real local publication gap keeps the same backend and creation key,
    // even when startup configuration subsequently chooses legacy.
    std::fs::remove_file(&owner).unwrap();
    assert_eq!(persisted_owner(&owner).unwrap(), Some(original.clone()));
    let mut collision = factory(&root, &socket);
    collision.session = "different-product-session".into();
    let error = match collision.build(context(&root)).await {
        Ok(_) => panic!("namespace collision must fail"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("identity/profile mismatch"));
    assert_eq!(persisted_owner(&owner).unwrap(), Some(original));
    server.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn durable_creation_requires_a_real_pin_before_any_host_connection() {
    let root = scratch();
    for revision in ["", "*", "prefix*", "   "] {
        let mut invalid = factory(&root, &root.join("must-not-connect.sock"));
        invalid.loop_version = revision.into();
        let error = match invalid.build(context(&root)).await {
            Ok(_) => panic!("pin required"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("immutable loop revision"));
        assert!(!root.join("transcript.jsonl.backend.json").exists());
        assert!(
            !root
                .join("transcript.jsonl.backend.json.creation.intent")
                .exists()
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

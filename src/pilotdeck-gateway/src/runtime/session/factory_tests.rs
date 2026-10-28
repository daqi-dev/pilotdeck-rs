use super::*;
use crate::runtime::{
    mcp::runtime::{McpFuture, SessionMcpRuntime},
    project::{ProjectRuntimeRegistry, ResourceBundle},
    refresh::RefreshRouter,
    session::{HandleState, SessionOwner, SessionOwnerOptions},
    turn::host_factory::Deployment,
};
use crate::storage::JsonlTranscriptStore;
use pilotdeck_protocol::parse_json;
use std::{
    future::Future,
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tokio::sync::Semaphore;

fn js(text: &str) -> JsValue {
    parse_json(text).expect("#745 test JSON")
}
async fn bounded<T>(name: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(8), future)
        .await
        .unwrap_or_else(|_| panic!("#745 {name} bounded wait"))
}
struct Gate {
    entered: Semaphore,
    release: Semaphore,
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        })
    }
    async fn wait(&self) {
        self.entered.add_permits(1);
        self.release
            .acquire()
            .await
            .expect("#745 gate release")
            .forget();
    }
    async fn entered(&self) {
        self.entered
            .acquire()
            .await
            .expect("#745 gate entered")
            .forget();
    }
    fn open(&self) {
        self.release.add_permits(1);
    }
}
#[derive(Default)]
struct Observed {
    events: Mutex<Vec<String>>,
    creates: AtomicUsize,
    mcp: AtomicUsize,
    bindings: Mutex<Vec<SessionBinding>>,
    published: AtomicUsize,
    diagnostics: Mutex<Vec<Diagnostic>>,
    checkpoints: Mutex<Vec<(usize, WriterState)>>,
}
impl Observed {
    fn event(&self, value: impl Into<String>) {
        lock(&self.events).push(value.into());
    }
}
struct Mcp(Arc<Observed>);
impl SessionMcpRuntime for Mcp {
    fn stop(&self) -> McpFuture<'_, RuntimeResult> {
        Box::pin(async move {
            self.0.mcp.fetch_sub(1, Ordering::SeqCst);
            self.0.event("mcp.stop");
            Ok(())
        })
    }
}
struct Harness {
    root: PathBuf,
    factory: Arc<ProjectSessionFactory>,
    owner: SessionOwner,
    registry: Arc<ProjectRuntimeRegistry>,
    observed: Arc<Observed>,
    gate: Arc<Gate>,
}
impl Harness {
    fn new(name: &'static str) -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-745-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("#745 scratch");
        let registry = Arc::new(ProjectRuntimeRegistry::new(
            &root,
            Arc::new(|_: &Path, _: &mut ResourceBundle| Ok(())),
        ));
        let observed = Arc::new(Observed::default());
        let gate = Gate::new();
        let prepare_observed = observed.clone();
        let prepare_gate = gate.clone();
        let prepare_registry = registry.clone();
        let prepare: Prepare = Arc::new(move |_, mcp| {
            let obs = prepare_observed.clone();
            let gate = prepare_gate.clone();
            let registry = prepare_registry.clone();
            Box::pin(async move {
                let id = obs.creates.fetch_add(1, Ordering::SeqCst) + 1;
                obs.event(format!("prepare.{id}"));
                let mut resources = PreparedResources::default();
                let lease = registry.acquire(None)?;
                let release_obs = obs.clone();
                resources.push(move || {
                    Box::pin(async move {
                        let mut lease = lease;
                        let result = lease.release().await;
                        release_obs.event(format!("release.{id}"));
                        if name == "resume-release-reject" {
                            return Err(RuntimeError::new("PROJECT_RELEASE_BOOM"));
                        }
                        result
                    })
                });
                if name == "create-dispose-before-mcp" {
                    gate.wait().await;
                }
                obs.event("mcp.construct");
                let registration = mcp.register("session-745", Arc::new(Mcp(obs.clone())))?;
                obs.event("mcp.start");
                obs.mcp.fetch_add(1, Ordering::SeqCst);
                resources.push(move || Box::pin(async move { registration.dispose().await }));
                let host: HostBuilder = Box::new(move |binding| {
                    lock(&obs.bindings).push(binding);
                    Ok(TurnHostFactory::new(
                        registry,
                        Deployment::Native,
                        crate::host::Limits::default(),
                        None,
                        |_, _| panic!("#745 native transport cannot request ports"),
                    ))
                });
                Ok(PreparedSession {
                    resources,
                    host,
                    tail: TailConfig::default(),
                })
            })
        });
        let storage_obs = observed.clone();
        let storage_gate = gate.clone();
        let storage_root = root.clone();
        let create_storage: CreateStorage = Arc::new(move |context| {
            let obs = storage_obs.clone();
            let gate = storage_gate.clone();
            let root = storage_root.clone();
            Box::pin(async move {
                let id = obs.creates.load(Ordering::SeqCst);
                obs.event(format!("storage.{id}"));
                if id == 1
                    && [
                        "same-key-concurrent",
                        "create-owner-shutdown",
                        "create-factory-dispose",
                        "create-cancelled",
                    ]
                    .contains(&name)
                {
                    gate.wait().await;
                }
                let mut storage = FactoryStorage::new(
                    Arc::new(JsonlTranscriptStore::new()),
                    TranscriptKey {
                        project_root: root.clone(),
                        pilot_home: root.join("home"),
                        session_id: context.session_key,
                        transcript_path: root.join("session-745.jsonl"),
                    },
                );
                let fail = [
                    "resume-load-reject",
                    "resume-rollback-reject",
                    "resume-release-reject",
                    "admission-read-reject",
                ]
                .contains(&name)
                    || (id == 2
                        && ["recreate-read-reject", "recreate-rollback-reject"].contains(&name));
                if fail {
                    storage.load = Arc::new(move || {
                        Box::pin(async move {
                            Err(RuntimeError::new(if name.starts_with("recreate-") {
                                "RECREATE_LOAD_BOOM"
                            } else {
                                "LOAD_BOOM"
                            }))
                        })
                    });
                }
                if name == "recreate-running-before-read" && id == 2 {
                    let load = storage.load.clone();
                    storage.load = Arc::new(move || {
                        let load = load.clone();
                        let gate = gate.clone();
                        Box::pin(async move {
                            gate.wait().await;
                            (load)().await
                        })
                    });
                }
                let checkpoint_obs = obs.clone();
                let writer = storage.recorder.writer().clone();
                storage.load_checkpoint = Arc::new(move || {
                    let obs = checkpoint_obs.clone();
                    let state = writer.snapshot_state();
                    Box::pin(async move {
                        lock(&obs.checkpoints).push((id, state));
                        if name == "resume-checkpoint-reject" {
                            Err(RuntimeError::new("CHECKPOINT_BOOM"))
                        } else {
                            Ok(())
                        }
                    })
                });
                let close = storage.close.clone();
                let close_obs = obs.clone();
                storage.close = Arc::new(move || {
                    let close = close.clone();
                    let obs = close_obs.clone();
                    Box::pin(async move {
                        (close)().await?;
                        obs.event(format!("close.{id}"));
                        if name == "resume-rollback-reject"
                            || (id == 2 && name == "recreate-rollback-reject")
                        {
                            Err(RuntimeError::new("STORAGE_DISPOSE_BOOM"))
                        } else {
                            Ok(())
                        }
                    })
                });
                Ok(storage)
            })
        });
        let diagnostic_obs = observed.clone();
        let factory = Arc::new(ProjectSessionFactory::new(FactoryOptions {
            prepare,
            create_storage,
            admission: (name == "admission-read-reject").then(|| {
                Arc::new(
                    |_: SessionContext| -> BoxFuture<Result<Option<JsValue>, RuntimeError>> {
                        Box::pin(async { Ok(None) })
                    },
                ) as Admission
            }),
            diagnostic: Arc::new(move |d| {
                diagnostic_obs.event(d.code);
                lock(&diagnostic_obs.diagnostics).push(d.clone());
            }),
        }));
        let mut options = SessionOwnerOptions::new(factory.clone());
        options.idle_sweep_interval_ms = 0;
        let publish_observed = observed.clone();
        options.on_evict = Some(Arc::new(move |_, reason| {
            if reason == super::super::EvictionReason::DirtyRecreate {
                publish_observed.published.fetch_add(1, Ordering::SeqCst);
            }
        }));
        let owner = SessionOwner::new(options);
        Self {
            root,
            factory,
            owner,
            registry,
            observed,
            gate,
        }
    }
    fn context(&self) -> SessionContext {
        SessionContext {
            session_key: "session-745".into(),
            project_key: None,
            channel_key: "test".into(),
            allowed_tools: None,
            disallowed_tools: None,
        }
    }
    fn leases(&self) -> usize {
        self.registry
            .resolve(None)
            .expect("#745 project runtime")
            .lease_count()
    }
    async fn create(&self, name: &str) -> LiveSessionHandle {
        bounded(name, self.owner.get_or_create(self.context()))
            .await
            .unwrap_or_else(|e| panic!("#745 {name} create: {e}"))
    }
    async fn finish(&self, name: &str) {
        bounded(name, self.owner.shutdown())
            .await
            .unwrap_or_else(|e| panic!("#745 {name} shutdown: {e}"));
        bounded(name, self.factory.dispose())
            .await
            .unwrap_or_else(|e| panic!("#745 {name} factory disposal: {e}"));
        assert_eq!(self.leases(), 0, "#745 {name} lease cleanup");
        assert_eq!(
            self.observed.mcp.load(Ordering::SeqCst),
            0,
            "#745 {name} MCP cleanup"
        );
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn ordered(events: &[String], first: &str, second: &str) -> bool {
    match (
        events.iter().position(|e| e == first),
        events.iter().position(|e| e == second),
    ) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
}
fn fixture(name: &str) -> serde_json::Value {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/session-factory/oracle.json"
    ))
    .expect("#745 fixture JSON");
    oracle["cases"]
        .as_array()
        .expect("#745 fixture cases")
        .iter()
        .find(|c| c["name"] == name)
        .expect("#745 fixture cell")
        .clone()
}
fn ts_snapshot<'a>(ts: &'a serde_json::Value, label: &str) -> &'a serde_json::Value {
    ts["snapshots"]
        .as_array()
        .expect("#745 oracle snapshots")
        .iter()
        .find(|s| s["label"] == label)
        .expect("#745 oracle snapshot label")
}
fn transcript_bytes<'a>(ts: &'a serde_json::Value, field: &str) -> &'a str {
    ts[field]
        .as_array()
        .expect("#745 oracle files")
        .iter()
        .find(|f| {
            f["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("session-745.jsonl"))
        })
        .expect("#745 oracle transcript")["utf8"]
        .as_str()
        .expect("#745 oracle transcript bytes")
}
fn seed_transcript(h: &Harness, ts: &serde_json::Value, field: &str) {
    let bytes = transcript_bytes(ts, field);
    let initial: String = bytes.lines().take(2).map(|s| format!("{s}\n")).collect();
    std::fs::write(
        h.root.join("session-745.jsonl"),
        if ts["label"] == "before-recreate" {
            &initial
        } else {
            bytes
        },
    )
    .expect("#745 write original transcript bytes");
}
fn writer_json(state: &WriterState) -> serde_json::Value {
    serde_json::json!({"sequence":state.sequence,"lastEntryId":state.last_entry_id.as_ref().map(|s|String::from_utf16_lossy(s.units()))})
}
fn assert_oracle_error(name: &str, error: &RuntimeError, ts: &serde_json::Value) {
    let expected = &ts["results"]
        .as_array()
        .expect("#745 oracle results")
        .iter()
        .find(|r| r["status"] == "rejected")
        .expect("#745 oracle rejected result")["error"];
    assert_eq!(
        error.message, expected["message"],
        "#745 {name} exact error oracle"
    );
    let causes: Vec<_> = error.causes.iter().map(|e| e.message.as_str()).collect();
    let expected_causes: Vec<_> = expected["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| e["message"].as_str().expect("#745 oracle cause message"))
        .collect();
    assert_eq!(causes, expected_causes, "#745 {name} exact causes oracle");
    assert_eq!(
        error.js_error().and_then(|e| e.code.as_deref()),
        expected["code"].as_str(),
        "#745 {name} error code oracle"
    );
}
fn assert_oracle_handle(name: &str, handle: &LiveSessionHandle, expected: &serde_json::Value) {
    let reload = handle
        .reload_snapshot()
        .unwrap_or_else(|e| panic!("#745 {name} oracle reload: {e}"));
    let actual = json_value(&handle.snapshot());
    assert_eq!(
        handle
            .reload_signal_for_test()
            .expect("#745 actual reload controller")
            .is_aborted(),
        expected["session"]["abortController"]["aborted"]
            .as_bool()
            .expect("#745 oracle controller"),
        "#745 {name} controller oracle"
    );
    for key in ["messages", "status", "currentTurnId"] {
        assert_eq!(
            actual[key], expected["session"][key],
            "#745 {name} session {key} oracle"
        );
    }
    for (key, value) in [
        ("usage", JsValue::Object(reload.state.usage)),
        (
            "permissionDenials",
            JsValue::Array(reload.state.permission_denials),
        ),
        ("messages", JsValue::Array(reload.state.messages)),
    ] {
        assert_eq!(
            json_value(&value),
            expected["reload"]["state"][key],
            "#745 {name} reload {key} oracle"
        );
    }
    assert_eq!(
        json_value(&JsValue::Object(reload.state.metadata)),
        expected["reload"]["metadata"],
        "#745 {name} metadata oracle"
    );
    assert_eq!(
        writer_json(&reload.transcript_writer_state),
        expected["reload"]["transcriptWriterState"],
        "#745 {name} writer oracle"
    );
    // The loop starts without a stdio seed; real checkpoint timing is probed separately.
    assert_eq!(
        json_value(&JsValue::Object(
            crate::host::modules::tool_context::serialize_seed_state(&reload.file_state)
        )),
        expected["reload"]["fileState"],
        "#745 {name} file state oracle"
    );
}
async fn matrix(name: &'static str) {
    let ts = fixture(name);
    assert_eq!(ts["status"], "OBSERVED", "#745 {name} TS observation");
    let h = Harness::new(name);
    if name == "resume-valid" || name == "resume-checkpoint-reject" || name == "resume-malformed" {
        seed_transcript(&h, &ts, "finalFiles");
    }
    if name.starts_with("recreate-") {
        let before = ts_snapshot(&ts, "before-recreate");
        seed_transcript(&h, before, "files");
    }
    if name == "resume-eisdir" {
        std::fs::create_dir(h.root.join("session-745.jsonl")).expect("#745 eisdir fixture");
    }
    match name {
        "same-key-concurrent" | "create-owner-shutdown" | "create-factory-dispose" => {
            let first = h.owner.get_or_create(h.context());
            bounded(name, h.gate.entered()).await;
            assert_eq!(h.leases(), 1, "#745 {name} pending lease");
            if name == "same-key-concurrent" {
                let second = h.owner.get_or_create(h.context());
                h.gate.open();
                let a = bounded(name, first)
                    .await
                    .unwrap_or_else(|e| panic!("#745 {name} first: {e}"));
                let b = bounded(name, second)
                    .await
                    .unwrap_or_else(|e| panic!("#745 {name} second: {e}"));
                assert!(
                    a.same_identity(&b) && h.observed.creates.load(Ordering::SeqCst) == 1,
                    "#745 same-key-concurrent conclusion"
                );
            } else if name == "create-owner-shutdown" {
                let shutdown = h.owner.shutdown();
                h.gate.open();
                let error = bounded(name, first)
                    .await
                    .err()
                    .expect("#745 create-owner-shutdown conclusion");
                assert_oracle_error(name, &error, &ts);
                bounded(name, shutdown)
                    .await
                    .expect("#745 owner shutdown completes");
                assert_eq!(h.leases(), 0, "#745 create-owner-shutdown cleanup");
            } else {
                bounded(name, h.factory.dispose())
                    .await
                    .expect("#745 factory dispose");
                assert_eq!(
                    h.observed.mcp.load(Ordering::SeqCst),
                    0,
                    "#745 create-factory-dispose MCP stopped"
                );
                h.gate.open();
                let handle = bounded(name, first)
                    .await
                    .expect("#745 create-factory-dispose late candidate");
                assert!(
                    handle.state() == HandleState::Active && h.leases() == 1,
                    "#745 create-factory-dispose conclusion"
                );
            }
        }
        "create-dispose-before-mcp" => {
            let first = h.owner.get_or_create(h.context());
            bounded(name, h.gate.entered()).await;
            bounded(name, h.factory.dispose())
                .await
                .expect("#745 early dispose");
            h.gate.open();
            let error = bounded(name, first)
                .await
                .err()
                .expect("#745 create-dispose-before-mcp conclusion");
            assert_oracle_error(name, &error, &ts);
            for event in ["mcp.construct", "mcp.start", "mcp.stop"] {
                assert_eq!(
                    lock(&h.observed.events)
                        .iter()
                        .filter(|e| *e == event)
                        .count(),
                    ts["timeline"]
                        .as_array()
                        .expect("#745 MCP timeline")
                        .iter()
                        .filter(|e| e["type"] == event)
                        .count(),
                    "#745 {name} MCP {event}"
                );
            }
            assert_eq!(h.leases(), 0, "#745 create-dispose-before-mcp cleanup");
            assert!(
                !lock(&h.observed.events)
                    .iter()
                    .any(|e| e.starts_with("storage.")),
                "#745 no storage before failed MCP"
            );
        }
        "same-key-cached" => {
            let a = h.create(name).await;
            let b = h.create(name).await;
            assert!(
                a.same_identity(&b) && h.observed.creates.load(Ordering::SeqCst) == 1,
                "#745 same-key-cached conclusion"
            );
        }
        name if name.starts_with("resume-") || name == "admission-read-reject" => {
            let result = bounded(name, h.owner.get_or_create(h.context())).await;
            if ["resume-missing", "resume-valid", "resume-malformed"].contains(&name) {
                let handle = result.unwrap_or_else(|e| panic!("#745 {name} restored: {e}"));
                let reload = handle
                    .reload_snapshot()
                    .unwrap_or_else(|e| panic!("#745 {name} reload: {e}"));
                if name == "resume-valid" {
                    assert_eq!(
                        reload.state.metadata.get("firstPrompt"),
                        Some(&" 已存 中文 ".into()),
                        "#745 resume-valid conclusion"
                    );
                    assert_eq!(
                        reload.transcript_writer_state.sequence, 1,
                        "#745 resume-valid writer"
                    );
                    assert_eq!(
                        reload.transcript_writer_state.last_entry_id,
                        Some("seed-entry".into()),
                        "#745 resume-valid parent"
                    );
                } else {
                    let code = if name == "resume-missing" {
                        "transcript_missing"
                    } else {
                        "transcript_line_invalid"
                    };
                    assert!(
                        lock(&h.observed.events).iter().any(|e| e == code),
                        "#745 {name} conclusion"
                    );
                    assert_eq!(
                        reload.transcript_writer_state.sequence, 0,
                        "#745 {name} empty writer"
                    );
                }
            } else {
                let error = result
                    .err()
                    .unwrap_or_else(|| panic!("#745 {name} expected failure"));
                if name == "resume-rollback-reject" {
                    assert_eq!(
                        error
                            .causes
                            .iter()
                            .map(|e| e.message.as_str())
                            .collect::<Vec<_>>(),
                        vec!["LOAD_BOOM", "STORAGE_DISPOSE_BOOM"],
                        "#745 resume-rollback-reject conclusion"
                    );
                } else if name == "resume-eisdir" {
                    assert_eq!(
                        error.message, "EISDIR: illegal operation on a directory, read",
                        "#745 resume-eisdir conclusion"
                    );
                    assert_eq!(
                        error.js_error().and_then(|e| e.code.as_deref()),
                        Some("EISDIR"),
                        "#745 resume-eisdir code"
                    );
                } else {
                    assert_eq!(
                        error.message,
                        if name == "resume-checkpoint-reject" {
                            "CHECKPOINT_BOOM"
                        } else {
                            "LOAD_BOOM"
                        },
                        "#745 {name} conclusion"
                    );
                }
                assert_oracle_error(name, &error, &ts);
                assert!(
                    ordered(&lock(&h.observed.events), "close.1", "release.1"),
                    "#745 {name} rollback order"
                );
                assert_eq!(h.leases(), 0, "#745 {name} rollback leases");
            }
        }
        name if name.starts_with("recreate-") => {
            let old = h.create(name).await;
            let binding = lock(&h.observed.bindings)[0].clone();
            if name == "recreate-running" {
                for line in transcript_bytes(ts_snapshot(&ts, "before-recreate"), "files")
                    .lines()
                    .skip(2)
                {
                    binding
                        .recorder
                        .writer()
                        .record_entry(&js(line))
                        .expect("#745 running transcript replay");
                }
            }
            let activity = if name == "recreate-draining" {
                Some(old.begin_activity().expect("#745 predecessor activity"))
            } else {
                None
            };
            if name == "recreate-disposed" {
                bounded(name, old.dispose("test".into()))
                    .await
                    .expect("#745 dispose predecessor");
            }
            if [
                "recreate-read-reject",
                "recreate-rollback-reject",
                "recreate-disposed",
            ]
            .contains(&name)
            {
                let error = bounded(name, h.factory.create(h.context(), Some(old.clone())))
                    .await
                    .err()
                    .expect("#745 recreate expected failure");
                if name == "recreate-disposed" {
                    assert_eq!(
                        error.message, "Session projection driver is disposed.",
                        "#745 recreate-disposed conclusion"
                    );
                } else if name == "recreate-rollback-reject" {
                    assert_eq!(
                        error
                            .causes
                            .iter()
                            .map(|e| e.message.as_str())
                            .collect::<Vec<_>>(),
                        vec!["RECREATE_LOAD_BOOM", "STORAGE_DISPOSE_BOOM"],
                        "#745 recreate-rollback-reject conclusion"
                    );
                } else {
                    assert_eq!(
                        error.message, "RECREATE_LOAD_BOOM",
                        "#745 recreate-read-reject conclusion"
                    );
                }
                assert_oracle_error(name, &error, &ts);
                assert!(
                    ordered(&lock(&h.observed.events), "prepare.2", "release.2"),
                    "#745 {name} replacement lease acquired and released"
                );
                if name == "recreate-disposed" {
                    assert!(
                        !lock(&h.observed.events).iter().any(|e| e == "storage.2"),
                        "#745 recreate-disposed no new storage"
                    );
                }
                if name != "recreate-disposed" {
                    assert!(
                        ordered(&lock(&h.observed.events), "release.2", "close.2"),
                        "#745 {name} rollback order"
                    );
                    assert_eq!(h.leases(), 1, "#745 {name} old lease retained");
                }
            } else if name == "recreate-running" {
                let turn = old
                    .submit(
                        js(r#"{"type":"text","text":"pending"}"#),
                        js(r#"{"sessionId":"session-745","turnId":"pending"}"#),
                    )
                    .expect("#745 running predecessor submit");
                assert_eq!(
                    old.snapshot().get("status"),
                    Some(&"running".into()),
                    "#745 recreate-running old status"
                );
                let draining = old.dispose("explicit-old-drain".into());
                let new = bounded(name, h.factory.create(h.context(), Some(old.clone())))
                    .await
                    .expect("#745 recreate-running new");
                assert_eq!(
                    new.snapshot().get("status"),
                    Some(&"idle".into()),
                    "#745 recreate-running conclusion"
                );
                let state = new
                    .reload_snapshot()
                    .expect("#745 recreate-running snapshot");
                binding
                    .recorder
                    .writer()
                    .record_session_metadata(
                        &"session-745".into(),
                        &"late".into(),
                        js(r#"{"title":"late"}"#),
                    )
                    .expect("#745 predecessor late write");
                assert_eq!(
                    state.transcript_writer_state.sequence, 5,
                    "#745 recreate-running captured writer"
                );
                assert_eq!(
                    new.reload_snapshot()
                        .expect("#745 successor snapshot")
                        .transcript_writer_state
                        .sequence,
                    5,
                    "#745 recreate-running independent writer"
                );
                assert_oracle_handle(
                    name,
                    &new,
                    &ts_snapshot(&ts, "replacement-ready")["handles"][1],
                );
                drop(turn);
                drop(activity);
                bounded(name, draining)
                    .await
                    .expect("#745 predecessor drained");
                bounded(name, new.dispose("test".into()))
                    .await
                    .expect("#745 new handle dispose");
            } else {
                assert!(
                    h.owner.mark_session_dirty(&"session-745".into(), "test"),
                    "#745 mark dirty"
                );
                let future = h.owner.get_or_create(h.context());
                tokio::pin!(future);
                let new = if name == "recreate-draining" {
                    bounded(name, async {
                        loop {
                            if let Some(new) = h.owner.current_handle(&"session-745".into())
                                && !new.same_identity(&old)
                            {
                                break new;
                            }
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                } else {
                    bounded(name, &mut future)
                        .await
                        .expect("#745 recreate-idle result")
                };
                if name == "recreate-draining" {
                    assert!(
                        old.state() == HandleState::Draining && h.leases() == 2,
                        "#745 recreate-draining conclusion"
                    );
                    // The pre-existing Rust owner serializes this queued call through
                    // the predecessor drain; publication itself is already visible.
                    let cached = h.owner.get_or_create(h.context());
                    assert!(
                        h.owner
                            .current_handle(&"session-745".into())
                            .expect("#745 published handle")
                            .same_identity(&new),
                        "#745 recreate-draining cache published"
                    );
                    drop(activity);
                    bounded(name, &mut future)
                        .await
                        .expect("#745 drain completes");
                    assert!(
                        bounded(name, cached)
                            .await
                            .expect("#745 cached after drain")
                            .same_identity(&new),
                        "#745 recreate-draining cached identity"
                    );
                } else {
                    assert!(
                        !new.same_identity(&old) && old.state() == HandleState::Disposed,
                        "#745 recreate-idle conclusion"
                    );
                }
                assert_oracle_handle(name, &new, &ts_snapshot(&ts, "result")["handles"][1]);
                let reload = new.reload_snapshot().expect("#745 replacement reload");
                assert_eq!(
                    reload.state.metadata.get("title"),
                    Some(&"原题".into()),
                    "#745 {name} metadata"
                );
                assert_eq!(
                    reload.transcript_writer_state.sequence, 2,
                    "#745 {name} writer"
                );
            }
        }
        _ => panic!("#745 unknown matrix cell {name}"),
    }
    if let Some(handle) = h.owner.current_handle(&"session-745".into())
        && handle.state() == HandleState::Active
        && !name.starts_with("recreate-")
    {
        assert_oracle_handle(name, &handle, &ts_snapshot(&ts, "result")["handles"][0]);
    }
    let expected_checkpoint = &ts_snapshot(&ts, "result")["storages"][0]["entriesAtCheckpoint"];
    if !expected_checkpoint.is_null() {
        let checkpoints = lock(&h.observed.checkpoints);
        let checkpoint = &checkpoints
            .iter()
            .find(|(id, _)| *id == 1)
            .expect("#745 checkpoint observed")
            .1;
        assert_eq!(
            writer_json(checkpoint),
            *expected_checkpoint,
            "#745 {name} checkpoint writer oracle"
        );
    }
    if name == "resume-missing" || name == "resume-malformed" {
        let expected = ts_snapshot(&ts, "result")["storages"][0]["restores"][0]["diagnostics"]
            .as_array()
            .expect("#745 diagnostics oracle");
        let actual = lock(&h.observed.diagnostics);
        assert_eq!(
            actual.len(),
            expected.len(),
            "#745 {name} diagnostic count oracle"
        );
        for (actual, expected) in actual.iter().zip(expected) {
            assert_eq!(
                serde_json::json!({"code":actual.code,"severity":actual.severity,"line":actual.line}),
                serde_json::json!({"code":expected["code"],"severity":expected["severity"],"line":expected["line"]}),
                "#745 {name} diagnostic fields oracle"
            );
        }
        if name == "resume-malformed" {
            assert_eq!(
                std::fs::read_to_string(h.root.join("session-745.jsonl"))
                    .expect("#745 malformed bytes read"),
                transcript_bytes(&ts, "finalFiles"),
                "#745 resume-malformed original bytes oracle"
            );
        }
    }
    h.finish(name).await;
}
macro_rules! cases { ($($name:ident => $cell:literal),* $(,)?) => { $(#[tokio::test] async fn $name() { matrix($cell).await; })* }; }
cases! {
 same_key_concurrent=>"same-key-concurrent", same_key_cached=>"same-key-cached",
 create_owner_shutdown=>"create-owner-shutdown", create_factory_dispose=>"create-factory-dispose", create_dispose_before_mcp=>"create-dispose-before-mcp",
 recreate_idle=>"recreate-idle", recreate_draining=>"recreate-draining", recreate_running=>"recreate-running", recreate_disposed=>"recreate-disposed", recreate_read_reject=>"recreate-read-reject", recreate_rollback_reject=>"recreate-rollback-reject",
 resume_missing=>"resume-missing", resume_valid=>"resume-valid", resume_malformed=>"resume-malformed", resume_eisdir=>"resume-eisdir", resume_load_reject=>"resume-load-reject", resume_checkpoint_reject=>"resume-checkpoint-reject", resume_rollback_reject=>"resume-rollback-reject", resume_release_reject=>"resume-release-reject", admission_read_reject=>"admission-read-reject"
}

#[test]
#[ignore = "requires the pinned Node v22.22.0 TS baseline"]
fn regenerates_identically() {
    crate::test_baseline::regenerates_identically("session-factory");
}

#[test]
fn durable_projection_matches_ts() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/session-factory/oracle.json"
    ))
    .expect("#745 projection fixture");
    let rows = oracle["projections"]
        .as_array()
        .expect("#745 projection rows");
    assert_eq!(rows.len(), 13, "#745 projection denominator");
    for row in rows {
        let values = js(&row["entries"].to_string());
        let entries = array(&values).expect("#745 projection entries");
        let actual = project(entries);
        if let Some(expected) = row.get("error") {
            let error = actual.expect_err("#745 projection rejects malformed usage/metadata");
            assert_eq!(
                error.message, expected["message"],
                "#745 projection error conclusion"
            );
            assert_eq!(
                error.js_error.expect("#745 projection error type").name,
                expected["name"],
                "#745 projection error name"
            );
            continue;
        }
        let actual = actual.expect("#745 projection replay");
        let mut value = JsObject::new();
        value.insert("messages".into(), JsValue::Array(actual.messages));
        value.insert("usage".into(), JsValue::Object(actual.usage));
        value.insert(
            "permissionDenials".into(),
            JsValue::Array(actual.permission_denials),
        );
        value.insert("metadata".into(), JsValue::Object(actual.metadata));
        assert_eq!(
            json_value(&JsValue::Object(value)),
            row["state"],
            "#745 projection conclusion {}",
            row["name"]
        );
    }
}
#[test]
fn actual_file_state_matches_ts() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/session-factory/oracle.json"
    ))
    .expect("#745 file fixture");
    let expected = js(&oracle["fileState"].to_string());
    let seed = crate::host::modules::tool_context::parse_seed_state(&expected)
        .expect("#745 file parse")
        .expect("#745 file seed");
    let checkpoint = HostToolCheckpoint::new(Some(&seed), &[]).expect("#745 file checkpoint");
    let actual = checkpoint.snapshot().expect("#745 file snapshot");
    assert_eq!(
        json_value(&JsValue::Object(
            crate::host::modules::tool_context::serialize_seed_state(&actual)
        )),
        json_value(&expected),
        "#745 actual file state conclusion"
    );
}
#[tokio::test]
async fn cancelled_create_waiter_disposes_late_candidate() {
    let name = "create-cancelled";
    let h = Harness::new(name);
    let create = h.factory.create(h.context(), None);
    let task = tokio::spawn(create);
    bounded(name, h.gate.entered()).await;
    task.abort();
    assert!(
        task.await
            .err()
            .expect("#745 cancelled waiter")
            .is_cancelled(),
        "#745 waiter cancellation"
    );
    h.gate.open();
    bounded(name, async {
        loop {
            if lock(&h.observed.events).iter().any(|e| e == "close.1") {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(h.leases(), 0, "#745 cancelled create conclusion");
    h.finish(name).await;
}
#[tokio::test]
async fn production_adapter_dispatches_to_host() {
    let h = Harness::new("host-execution");
    let session = h.create("host-execution").await;
    let mut turn = session
        .submit(
            js(r#"{"type":"text","text":"hello"}"#),
            js(r#"{"sessionId":"session-745","turnId":"turn"}"#),
        )
        .expect("#745 host submit");
    let failure = loop {
        match bounded("host-execution", turn.next()).await {
            Ok(Some(_)) => {}
            Ok(None) => panic!("#745 host unexpectedly acknowledged unavailable transport"),
            Err(error) => break error,
        }
    };
    assert_eq!(
        failure.message,
        "Host execution failed: Agent loop transport 'native' has no sidecar host factory.",
        "#745 host factory execution conclusion"
    );
    h.finish("host-execution").await;
}
#[test]
fn reload_capability_is_not_fake_success() {
    struct LifecycleOnly;
    impl LiveSession for LifecycleOnly {
        fn snapshot(&self) -> JsValue {
            JsValue::Null
        }
        fn abort(&self, _: &JsString) -> RuntimeResult {
            Ok(())
        }
        fn dispose(&self) -> DisposeFuture<'_> {
            Box::pin(async { Ok(()) })
        }
    }
    let handle = LiveSessionHandle::new(Arc::new(LifecycleOnly), None);
    assert_eq!(
        handle
            .reload_snapshot()
            .expect_err("#745 unavailable reload")
            .message,
        "Agent session runtime reload is unavailable.",
        "#745 default reload conclusion"
    );
}

#[tokio::test]
async fn reload_retains_saved_files_until_turn_terminal() {
    struct Terminal(bool);
    impl LiveTurn for Terminal {
        fn next(&mut self) -> super::super::SessionNext<'_> {
            let first = std::mem::replace(&mut self.0, false);
            Box::pin(async move { Ok(first.then(|| js(r#"{"type":"turn_completed"}"#))) })
        }
    }
    let checkpoint = HostToolCheckpoint::default();
    let saved = Arc::new(Mutex::new(SeedState::default()));
    let mut turn = OwnedTurn {
        turn: Box::new(Terminal(true)),
        checkpoint: checkpoint.clone(),
        saved: saved.clone(),
        started: Arc::new(AtomicBool::new(true)),
        finished: false,
    };
    checkpoint.allow_read_files(&["seed.txt".into()]);
    assert!(
        lock(&saved).allowed_read_files.is_none(),
        "#745 file state before terminal"
    );
    assert!(
        turn.next()
            .await
            .expect("#745 file terminal next")
            .is_some(),
        "#745 file terminal event"
    );
    assert_eq!(
        lock(&saved).allowed_read_files,
        Some(vec!["seed.txt".into()]),
        "#745 file state at terminal conclusion"
    );
    checkpoint.allow_read_files(&["later-turn.txt".into()]);
    assert!(
        turn.next()
            .await
            .expect("#745 completed stream next")
            .is_none(),
        "#745 completed stream exhausted"
    );
    assert_eq!(
        lock(&saved).allowed_read_files,
        Some(vec!["seed.txt".into()]),
        "#745 completed stream next cannot publish later state"
    );
    drop(turn);
    assert_eq!(
        lock(&saved).allowed_read_files,
        Some(vec!["seed.txt".into()]),
        "#745 completed stream cannot publish later state"
    );
}
#[tokio::test]
async fn initial_reload_preserves_absent_seed() {
    let h = Harness::new("seed-absence");
    let handle = h.create("seed-absence").await;
    let seed = handle
        .reload_snapshot()
        .expect("#745 initial seed snapshot")
        .file_state;
    assert!(
        seed.allowed_read_files.is_none()
            && seed.read_file_state.is_none()
            && seed.write_snapshots.is_none(),
        "#745 initial seed absence conclusion"
    );
    h.finish("seed-absence").await;
}
#[test]
fn committed_projection_failure_is_not_rolled_back() {
    let root = std::env::temp_dir().join(format!("pdrs-745-projection-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("#745 commit scratch");
    let key = TranscriptKey {
        project_root: root.clone(),
        pilot_home: root.clone(),
        session_id: "session-745".into(),
        transcript_path: root.join("log.jsonl"),
    };
    let store = ProjectedStore {
        inner: Arc::new(JsonlTranscriptStore::new()),
        key: key.clone(),
        projection: Arc::new(SessionProjection::default()),
    };
    let invalid = js(r#"{"type":"accepted_input","messages":null}"#);
    store
        .append(&key, &invalid)
        .expect("#745 invalid projection durable commit");
    assert!(
        store.projection.snapshot().is_err(),
        "#745 committed projection failure conclusion"
    );
    assert!(
        std::fs::read_to_string(&key.transcript_path)
            .expect("#745 persisted bytes")
            .contains("accepted_input"),
        "#745 committed entry survives projection failure"
    );
    std::fs::remove_dir_all(root).expect("#745 projection cleanup");
}

fn json_value(value: &JsValue) -> serde_json::Value {
    serde_json::from_str(
        &pilotdeck_protocol::stringify_json(value)
            .expect("#745 serialize actual")
            .expect("#745 defined actual"),
    )
    .expect("#745 actual JSON")
}

#[tokio::test]
async fn never_finishing_predecessor_does_not_block_other_keys() {
    let name = "predecessor-never-finishes";
    let h = Harness::new(name);
    let old = h.create(name).await;
    let activity = old.begin_activity().expect("#745 never ending activity");
    assert!(
        h.owner.mark_session_dirty(&"session-745".into(), "reload"),
        "#745 mark stuck predecessor"
    );
    let replacement = h.owner.get_or_create(h.context());
    bounded(name, async {
        while h.observed.published.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    // Cancelling this waiter does not abandon owner cleanup or its old writer.
    drop(replacement);
    let mut other = h.context();
    other.session_key = "independent".into();
    let independent = bounded(name, h.owner.get_or_create(other))
        .await
        .expect("#745 independent creation");
    assert_eq!(
        independent.state(),
        HandleState::Active,
        "#745 never finishing predecessor conclusion"
    );
    assert_eq!(
        old.state(),
        HandleState::Draining,
        "#745 predecessor retained"
    );
    assert_eq!(
        h.leases(),
        3,
        "#745 predecessor and replacement and independent leases"
    );
    // The consumer cancellation is the explicit terminal condition for drain.
    drop(activity);
    h.finish(name).await;
}
#[tokio::test]
async fn unpolled_turn_does_not_publish_file_state() {
    let h = Harness::new("unpolled-turn");
    let session = h.create("unpolled-turn").await;
    let turn = session
        .submit(
            js(r#"{"type":"text","text":"hello"}"#),
            js(r#"{"sessionId":"session-745","turnId":"turn"}"#),
        )
        .expect("#745 unpolled submit");
    lock(&h.observed.bindings)[0]
        .checkpoint
        .allow_read_files(&["in-progress.txt".into()]);
    assert!(
        session
            .reload_snapshot()
            .expect("#745 in-progress reload")
            .file_state
            .allowed_read_files
            .is_none(),
        "#745 in-progress checkpoint not published"
    );
    drop(turn);
    let seed = session
        .reload_snapshot()
        .expect("#745 unpolled reload")
        .file_state;
    assert!(
        seed.allowed_read_files.is_none(),
        "#745 unpolled turn file conclusion"
    );
    h.finish("unpolled-turn").await;
}

#[tokio::test]
async fn invalid_projection_releases_submit_slot() {
    let h = Harness::new("submit-projection-failure");
    let session = h.create("submit-projection-failure").await;
    let projection = lock(&h.observed.bindings)[0].projection.clone();
    // A persisted malformed input is already covered by the durable-append
    // test; inject that projection state to isolate admission rollback here.
    lock(&projection.entries).push(js(r#"{"type":"accepted_input","messages":null}"#));
    let input = js(r#"{"type":"text","text":"hello"}"#);
    let options = js(r#"{"sessionId":"session-745","turnId":"turn"}"#);
    assert!(
        session.submit(input.clone(), options.clone()).is_err(),
        "#745 invalid projection rejects submission"
    );
    let snapshot = session.snapshot();
    assert!(
        is(field(&snapshot, "status"), "idle"),
        "#745 failed submit status reset"
    );
    assert!(
        matches!(field(&snapshot, "currentTurnId"), JsValue::Undefined),
        "#745 failed submit identity reset"
    );
    lock(&projection.entries).clear();
    let turn = session
        .submit(input, options)
        .expect("#745 failed submit releases active slot");
    drop(turn);
    h.finish("submit-projection-failure").await;
}

#[tokio::test]
async fn queued_candidate_cancellation_closes_storage() {
    struct Sent(tokio::sync::Notify);
    impl std::task::Wake for Sent {
        fn wake(self: Arc<Self>) {
            self.0.notify_one();
        }
    }
    let h = Harness::new("create-cancelled");
    let sent = Arc::new(Sent(tokio::sync::Notify::new()));
    let waker = std::task::Waker::from(sent.clone());
    let mut future = h.factory.create(h.context(), None);
    assert!(
        future
            .as_mut()
            .poll(&mut std::task::Context::from_waker(&waker))
            .is_pending(),
        "#745 queued candidate pending"
    );
    bounded("queued candidate preparing", h.gate.entered()).await;
    h.gate.open();
    bounded("queued candidate sent", sent.0.notified()).await;
    // The sender has completed successfully, but the receiver was never polled again.
    drop(future);
    bounded("queued candidate disposed", async {
        while !lock(&h.observed.events).iter().any(|e| e == "close.1") {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(h.leases(), 0, "#745 queued candidate lease cleanup");
    h.finish("queued-candidate").await;
}

#[tokio::test]
async fn session_started_cancellation_preserves_absent_seed() {
    let h = Harness::new("session-started-cancel");
    let session = h.create("session-started-cancel").await;
    let mut turn = session
        .submit(
            js(r#"{"type":"text","text":"hello"}"#),
            js(r#"{"sessionId":"session-745","turnId":"turn"}"#),
        )
        .expect("#745 session-started submit");
    let first = turn
        .next()
        .await
        .expect("#745 session-started next")
        .expect("#745 session-started event");
    assert!(
        is(field(&first, "type"), "session_started"),
        "#745 first session event"
    );
    drop(turn);
    assert!(
        session
            .reload_snapshot()
            .expect("#745 session-started reload")
            .file_state
            .allowed_read_files
            .is_none(),
        "#745 session-started cancellation seed"
    );
    h.finish("session-started-cancel").await;
}

#[tokio::test]
async fn storage_hydration_matches_ts_errors() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/session-factory/oracle.json"
    ))
    .expect("#745 hydrate fixture");
    for row in oracle["storageErrors"]
        .as_array()
        .expect("#745 hydrate rows")
    {
        let h = Harness::new("hydrate-errors");
        let entries = array(&js(&row["entries"].to_string()))
            .expect("#745 hydrate entries")
            .to_vec();
        let mut storage = FactoryStorage::new(
            Arc::new(JsonlTranscriptStore::new()),
            TranscriptKey {
                project_root: h.root.clone(),
                pilot_home: h.root.clone(),
                session_id: "session-745".into(),
                transcript_path: h.root.join("log.jsonl"),
            },
        );
        storage.load = Arc::new(move || {
            let entries = entries.clone();
            Box::pin(async move {
                Ok(TranscriptRead {
                    entries,
                    diagnostics: vec![],
                })
            })
        });
        let error = storage
            .restore()
            .await
            .expect_err("#745 hydrate must reject");
        assert_eq!(
            error.message, row["error"]["message"],
            "#745 hydrate error conclusion"
        );
        assert_eq!(
            storage.recorder.writer().snapshot_state().sequence,
            1,
            "#745 hydration after writer restore"
        );
        (storage.close)().await.expect("#745 hydrate writer close");
        h.finish("hydrate-errors").await;
    }
}

#[tokio::test]
async fn reload_retries_concurrent_append() {
    let h = Harness::new("reload-version");
    let storage = FactoryStorage::new(
        Arc::new(JsonlTranscriptStore::new()),
        TranscriptKey {
            project_root: h.root.clone(),
            pilot_home: h.root.clone(),
            session_id: "session-745".into(),
            transcript_path: h.root.join("log.jsonl"),
        },
    );
    let once = Arc::new(AtomicBool::new(false));
    let recorder = storage.recorder.clone();
    let source = ReloadSource {
        session_id: "session-745".into(),
        cwd: h.root.clone(),
        transcript_path: h.root.join("log.jsonl"),
        projection: storage.projection.clone(),
        file_state: Arc::new(move || {
            if !once.swap(true, Ordering::SeqCst) {
                recorder
                    .writer()
                    .record_session_metadata(
                        &"session-745".into(),
                        &"test".into(),
                        js(r#"{"title":"concurrent"}"#),
                    )
                    .expect("#745 concurrent append");
            }
            Ok(SeedState::default())
        }),
    };
    let snapshot = source
        .snapshot_consistent(&storage.recorder)
        .expect("#745 consistent reload");
    assert_eq!(
        snapshot.transcript_writer_state.sequence, 1,
        "#745 reload version conclusion"
    );
    assert_eq!(
        snapshot.state.metadata.get("title"),
        Some(&"concurrent".into()),
        "#745 reload projection version"
    );
    (storage.close)()
        .await
        .expect("#745 concurrent writer close");
    h.finish("reload-version").await;
}

fn oracle_document() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/session-factory/oracle.json"
    ))
    .expect("#745 complete oracle JSON")
}
fn oracle_seed(value: &serde_json::Value) -> SeedState {
    crate::host::modules::tool_context::parse_seed_state(&js(&value.to_string()))
        .expect("#745 oracle seed parse")
        .expect("#745 oracle seed present")
}
fn seed_json(seed: &SeedState) -> serde_json::Value {
    json_value(&JsValue::Object(
        crate::host::modules::tool_context::serialize_seed_state(seed),
    ))
}
#[tokio::test]
async fn reload_file_state_matches_ts_timing() {
    struct Terminal(bool);
    impl LiveTurn for Terminal {
        fn next(&mut self) -> super::super::SessionNext<'_> {
            let first = std::mem::replace(&mut self.0, false);
            Box::pin(async move { Ok(first.then(|| js(r#"{"type":"turn_completed"}"#))) })
        }
    }
    let oracle = oracle_document();
    let ts = &oracle["fileTiming"];
    let saved = Arc::new(Mutex::new(oracle_seed(&ts["initial"])));
    let checkpoint = HostToolCheckpoint::new(Some(&oracle_seed(&ts["terminal"])), &[])
        .expect("#745 terminal checkpoint");
    let mut turn = OwnedTurn {
        turn: Box::new(Terminal(true)),
        checkpoint: checkpoint.clone(),
        saved: saved.clone(),
        started: Arc::new(AtomicBool::new(true)),
        finished: false,
    };
    assert_eq!(
        seed_json(&lock(&saved)),
        ts["running"],
        "#745 running saved seed oracle"
    );
    assert!(
        turn.next()
            .await
            .expect("#745 seed terminal poll")
            .is_some(),
        "#745 seed terminal event"
    );
    assert_eq!(
        seed_json(&lock(&saved)),
        ts["terminal"],
        "#745 terminal seed oracle"
    );
    checkpoint.allow_read_files(&["late.txt".into()]);
    assert!(
        turn.next()
            .await
            .expect("#745 seed exhausted poll")
            .is_none(),
        "#745 seed stream exhausted"
    );
    assert_eq!(
        seed_json(&lock(&saved)),
        ts["after"],
        "#745 post-terminal seed oracle"
    );
    drop(turn);
    assert_eq!(
        seed_json(&lock(&saved)),
        ts["after"],
        "#745 dropped terminal seed oracle"
    );
}

// Characterization of the independently observed TS defect, not a safety acceptance.
// P0 remains blocked on the coordinator's alignment/fence decision.
async fn writer_interleaving(before_read: bool) {
    let name = if before_read {
        "recreate-running-before-read"
    } else {
        "writer-after-publish"
    };
    let h = Harness::new(name);
    let ts = fixture("recreate-running");
    seed_transcript(&h, ts_snapshot(&ts, "before-recreate"), "files");
    let old = h.create(name).await;
    let binding = lock(&h.observed.bindings)[0].clone();
    for line in transcript_bytes(ts_snapshot(&ts, "before-recreate"), "files")
        .lines()
        .skip(2)
    {
        binding
            .recorder
            .writer()
            .record_entry(&js(line))
            .expect("#745 interleaving pending replay");
    }
    let turn = old
        .submit(
            js(r#"{"type":"text","text":"pending input"}"#),
            js(r#"{"sessionId":"session-745","turnId":"turn-old"}"#),
        )
        .expect("#745 interleaving old submit");
    let draining = old.dispose("explicit-old-drain".into());
    let mut creation = tokio::spawn(h.factory.create(h.context(), Some(old.clone())));
    let early = if before_read {
        bounded(name, h.gate.entered()).await;
        None
    } else {
        Some(
            bounded(name, &mut creation)
                .await
                .expect("#745 interleaving new task")
                .expect("#745 interleaving new handle"),
        )
    };
    for line in transcript_bytes(&ts, "finalFiles").lines().skip(5) {
        binding
            .recorder
            .writer()
            .record_entry(&js(line))
            .expect("#745 interleaving old terminal replay");
    }
    drop(turn);
    bounded(name, draining)
        .await
        .expect("#745 interleaving old drained");
    let new = if let Some(new) = early {
        new
    } else {
        h.gate.open();
        bounded(name, &mut creation)
            .await
            .expect("#745 interleaving new task")
            .expect("#745 interleaving new handle")
    };
    let oracle = oracle_document();
    let expected = oracle["writerInterleavings"]
        .as_array()
        .expect("#745 writer oracle rows")
        .iter()
        .find(|r| {
            r["name"]
                == if before_read {
                    "before-read"
                } else {
                    "after-publish"
                }
        })
        .expect("#745 writer oracle timing");
    assert_eq!(
        writer_json(
            &new.reload_snapshot()
                .expect("#745 interleaving reload")
                .transcript_writer_state
        ),
        expected["replacementWriter"],
        "#745 interleaving restored writer oracle"
    );
    {
        let observed = lock(&h.observed.checkpoints);
        assert_eq!(
            writer_json(
                &observed
                    .iter()
                    .find(|(id, _)| *id == 2)
                    .expect("#745 interleaving checkpoint observed")
                    .1
            ),
            expected["entriesAtCheckpoint"],
            "#745 interleaving read timing oracle"
        );
    }
    let mut turn = new
        .submit(
            js(r#"{"type":"text","text":"replacement input"}"#),
            js(r#"{"sessionId":"session-745","turnId":"turn-new"}"#),
        )
        .expect("#745 interleaving new submit");
    let failure = bounded(name, async {
        loop {
            match turn.next().await {
                Ok(Some(_)) => {}
                Ok(None) => panic!("#745 interleaving expected unavailable native host"),
                Err(e) => break e,
            }
        }
    })
    .await;
    assert!(
        failure.message.contains("native"),
        "#745 interleaving new turn reached host"
    );
    drop(turn);
    // Native has no host transport in this crate. Exercise the real replacement
    // writer after that dispatch; TS supplemental probe runs a complete new turn.
    lock(&h.observed.bindings)[1]
        .recorder
        .writer()
        .record_session_metadata(
            &"session-745".into(),
            &"turn-new".into(),
            js(r#"{"title":"replacement"}"#),
        )
        .expect("#745 interleaving replacement durable append");
    bounded(name, new.dispose("test".into()))
        .await
        .expect("#745 interleaving new dispose");
    let error = bounded(name, h.factory.create(h.context(), None))
        .await
        .err()
        .expect("#745 interleaving duplicate sequence must reject resume");
    assert!(
        error
            .message
            .contains("Session event sequence must increase strictly"),
        "#745 interleaving sequence failure oracle: {}",
        error.message
    );
    assert_eq!(
        expected["error"]["code"], "sequence_not_increasing",
        "#745 TS corruption oracle"
    );
    h.finish(name).await;
}
#[tokio::test]
async fn recreate_running_before_read() {
    writer_interleaving(true).await;
}
#[tokio::test]
async fn recreate_running_after_publish_resume() {
    writer_interleaving(false).await;
}

#[tokio::test]
async fn recreate_preserves_snapshot_seed_and_uses_durable_state() {
    struct Previous(ReloadSnapshot);
    impl LiveSession for Previous {
        fn snapshot(&self) -> JsValue {
            JsValue::Null
        }
        fn reload_snapshot(&self) -> Result<ReloadSnapshot, RuntimeError> {
            Ok(self.0.clone())
        }
        fn abort(&self, _: &JsString) -> RuntimeResult {
            Ok(())
        }
        fn dispose(&self) -> DisposeFuture<'_> {
            Box::pin(async { Ok(()) })
        }
    }
    let h = Harness::new("snapshot-seed");
    let ts = fixture("recreate-idle");
    seed_transcript(&h, ts_snapshot(&ts, "before-recreate"), "files");
    let old = h.create("snapshot-seed").await;
    let mut reload = old
        .reload_snapshot()
        .expect("#745 snapshot seed old reload");
    let oracle = oracle_document();
    let memory = &oracle["memoryOnly"];
    reload.state.messages = array(&js(&memory["injected"]["messages"].to_string()))
        .expect("#745 injected messages")
        .to_vec();
    reload.state.usage = spread(&js(&memory["injected"]["usage"].to_string()));
    reload.state.permission_denials =
        array(&js(&memory["injected"]["permissionDenials"].to_string()))
            .expect("#745 injected denials")
            .to_vec();
    reload.file_state = oracle_seed(&oracle["fileState"]);
    let previous = LiveSessionHandle::new(Arc::new(Previous(reload)), None);
    let new = bounded(
        "snapshot-seed",
        h.factory.create(h.context(), Some(previous)),
    )
    .await
    .expect("#745 snapshot seed recreate");
    old.abort(&"old controller only".into())
        .expect("#745 old abort");
    let state = new
        .reload_snapshot()
        .expect("#745 snapshot seed new reload");
    assert_eq!(
        json_value(&JsValue::Array(state.state.messages)),
        memory["actual"]["messages"],
        "#745 memory-only messages follow durable oracle"
    );
    assert_eq!(
        json_value(&JsValue::Object(state.state.usage)),
        memory["actual"]["usage"],
        "#745 memory-only usage follows durable oracle"
    );
    assert_eq!(
        json_value(&JsValue::Array(state.state.permission_denials)),
        memory["actual"]["permissionDenials"],
        "#745 memory-only denials follow durable oracle"
    );
    assert_eq!(
        seed_json(&state.file_state),
        oracle["fileState"],
        "#745 recreated file seed oracle"
    );
    let view = json_value(&new.snapshot());
    assert_eq!(
        view["status"], memory["actual"]["status"],
        "#745 replacement ignores old abort"
    );
    assert_eq!(
        view["currentTurnId"], memory["actual"]["currentTurnId"],
        "#745 replacement fresh current turn"
    );
    bounded("snapshot-seed", new.dispose("test".into()))
        .await
        .expect("#745 snapshot seed dispose");
    h.finish("snapshot-seed").await;
}

#[tokio::test]
async fn recreate_controller_is_independent_of_actual_predecessor() {
    let h = Harness::new("controller");
    let old = h.create("controller").await;
    let old_signal = old
        .reload_signal_for_test()
        .expect("#745 predecessor controller");
    old_signal.abort();
    let new = bounded(
        "controller",
        h.factory.create(h.context(), Some(old.clone())),
    )
    .await
    .expect("#745 controller recreate");
    let fresh = new
        .reload_signal_for_test()
        .expect("#745 replacement controller");
    let ts = fixture("recreate-running");
    let expected = &ts_snapshot(&ts, "replacement-ready")["handles"][1];
    assert!(
        old_signal.is_aborted(),
        "#745 predecessor controller aborted"
    );
    assert_eq!(
        fresh.is_aborted(),
        expected["session"]["abortController"]["aborted"],
        "#745 replacement controller independent oracle"
    );
    old_signal.abort();
    assert!(
        !fresh.is_aborted(),
        "#745 old controller cannot abort replacement"
    );
    bounded("controller", new.dispose("test".into()))
        .await
        .expect("#745 controller new dispose");
    h.finish("controller").await;
}

#[tokio::test]
async fn production_owner_cancelled_create_close_and_shutdown() {
    let h = Harness::new("create-cancelled");
    let creating = h.owner.get_or_create(h.context());
    bounded("owner create entered", h.gate.entered()).await;
    let closing = h.owner.close(&"session-745".into());
    drop(creating);
    drop(closing);
    let shutdown = h.owner.shutdown();
    h.gate.open();
    bounded("owner cancellation drain", shutdown)
        .await
        .expect("#745 owner cancellation shutdown");
    assert!(
        h.owner.current_handle(&"session-745".into()).is_none(),
        "#745 owner cancellation removed handle"
    );
    assert_eq!(h.leases(), 0, "#745 owner cancellation lease cleanup");
    assert_eq!(
        lock(&h.observed.events)
            .iter()
            .filter(|e| *e == "close.1")
            .count(),
        1,
        "#745 owner cancellation closes storage exactly once"
    );
    let writer = lock(&h.observed.bindings)[0].recorder.writer().clone();
    let before = writer.snapshot_state();
    writer
        .record_session_metadata(
            &"session-745".into(),
            &"after-close".into(),
            js(r#"{"title":"late"}"#),
        )
        .expect("#745 closed writer append acknowledged");
    assert_eq!(
        writer.snapshot_state(),
        before,
        "#745 owner cancellation writer cannot append"
    );
    h.finish("owner cancellation").await;
}

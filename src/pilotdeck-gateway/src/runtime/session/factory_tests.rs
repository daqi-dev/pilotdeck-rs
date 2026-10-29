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
    created_handles: Mutex<Vec<LiveSessionHandle>>,
    writer_leases: Mutex<Vec<SharedWriterLease>>,
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
    drain: Arc<Gate>,
}
struct PublishGate {
    factory: Arc<ProjectSessionFactory>,
    gate: Option<Arc<Gate>>,
    observed: Arc<Observed>,
}
impl SessionFactory for PublishGate {
    fn can_recreate(&self) -> bool {
        self.factory.can_recreate()
    }
    fn create(
        &self,
        context: SessionContext,
        previous: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        let creating = self.factory.create(context, previous);
        let observed = self.observed.clone();
        Box::pin(async move {
            let handle = creating.await?;
            lock(&observed.created_handles).push(handle.clone());
            Ok(handle)
        })
    }
    fn setup(
        &self,
        _: SessionContext,
        _: LiveSessionHandle,
        previous: Option<LiveSessionHandle>,
    ) -> BoxFuture<RuntimeResult> {
        let gate = self.gate.clone();
        Box::pin(async move {
            if previous.is_some()
                && let Some(gate) = gate
            {
                gate.wait().await;
            }
            Ok(())
        })
    }
}
impl Harness {
    fn new(name: &'static str) -> Self {
        Self::with_fence_timeout(name, Duration::from_secs(1))
    }
    fn with_fence_timeout(name: &'static str, writer_busy_wait_timeout: Duration) -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-745-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("#745 scratch");
        let registry = Arc::new(ProjectRuntimeRegistry::new(
            &root,
            Arc::new(|_: &Path, _: &mut ResourceBundle| Ok(())),
        ));
        let observed = Arc::new(Observed::default());
        let gate = Gate::new();
        let drain = Gate::new();
        let prepare_drain = drain.clone();
        let prepare_observed = observed.clone();
        let prepare_gate = gate.clone();
        let prepare_registry = registry.clone();
        let prepare: Prepare = Arc::new(move |_, mcp| {
            let obs = prepare_observed.clone();
            let gate = prepare_gate.clone();
            let registry = prepare_registry.clone();
            let drain = prepare_drain.clone();
            Box::pin(async move {
                let id = obs.creates.fetch_add(1, Ordering::SeqCst) + 1;
                obs.event(format!("prepare.{id}"));
                let mut resources = PreparedResources::default();
                let lease = registry.acquire(None)?;
                let release_obs = obs.clone();
                resources.push(move || {
                    Box::pin(async move {
                        if name == "dispose-final-write" {
                            lock(&release_obs.bindings)[id - 1]
                                .recorder
                                .writer()
                                .record_session_metadata(
                                    &"session-745".into(),
                                    &"release".into(),
                                    js(r#"{"final":true}"#),
                                )
                                .expect("#745 release hook final write");
                        }
                        let mut lease = lease;
                        let result = lease.release().await;
                        release_obs.event(format!("release.{id}"));
                        if name == "resume-release-reject" {
                            return Err(RuntimeError::new("PROJECT_RELEASE_BOOM"));
                        }
                        result
                    })
                });
                if name == "create-dispose-before-mcp"
                    || (name == "snapshot-before-prepare" && id == 2)
                {
                    gate.wait().await;
                }
                obs.event("mcp.construct");
                let registration = mcp.register("session-745", Arc::new(Mcp(obs.clone())))?;
                obs.event("mcp.start");
                obs.mcp.fetch_add(1, Ordering::SeqCst);
                resources.push(move || Box::pin(async move { registration.dispose().await }));
                if name.starts_with("fence-disposing-") && id == 1 {
                    resources.push(move || {
                        Box::pin(async move {
                            drain.wait().await;
                            Ok(())
                        })
                    });
                }
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
                        "slow-storage",
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
                        transcript_path: root.join(format!(
                            "{}.jsonl",
                            String::from_utf16_lossy(context.session_key.units())
                        )),
                        session_id: context.session_key,
                    },
                );
                lock(&obs.writer_leases).push(storage.writer_lease.clone());
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
                    let gate = gate.clone();
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
                if name.starts_with("fence-") && id == 2 && !name.ends_with("publish") {
                    let load = storage.load.clone();
                    let gate = gate.clone();
                    storage.load = Arc::new(move || {
                        let load = load.clone();
                        let gate = gate.clone();
                        Box::pin(async move {
                            if name.ends_with("beforeload") {
                                gate.wait().await;
                            }
                            let read = load().await?;
                            if name.ends_with("afterload") {
                                gate.wait().await;
                            }
                            Ok(read)
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
                let close_gate = gate.clone();
                storage.close = Arc::new(move || {
                    let close = close.clone();
                    let obs = close_obs.clone();
                    let gate = close_gate.clone();
                    Box::pin(async move {
                        if name == "dispose-close-stage" && id == 1 {
                            gate.wait().await;
                        }
                        if name == "storage-close-panic" && id == 1 {
                            panic!("#745 injected storage close panic");
                        }
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
            writer_busy_wait_timeout,
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
        let owner_factory: Arc<dyn SessionFactory> = Arc::new(PublishGate {
            factory: factory.clone(),
            gate: (name.starts_with("fence-") && name.ends_with("publish")).then(|| gate.clone()),
            observed: observed.clone(),
        });
        let mut options = SessionOwnerOptions::new(owner_factory);
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
            drain,
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
                    assert_eq!(h.leases(), 0, "#745 {name} old lease closed before read");
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
                let creation = tokio::spawn(h.factory.create(h.context(), Some(old.clone())));
                wait_draining(&old).await;
                binding
                    .recorder
                    .writer()
                    .record_session_metadata(
                        &"session-745".into(),
                        &"late".into(),
                        js(r#"{"title":"late"}"#),
                    )
                    .expect("#745 predecessor late write");
                drop(turn);
                drop(activity);
                let new = bounded(name, creation)
                    .await
                    .expect("#745 running task")
                    .expect("#745 recreate-running new");
                assert_eq!(
                    new.snapshot().get("status"),
                    Some(&"idle".into()),
                    "#745 recreate-running conclusion"
                );
                assert_eq!(
                    new.reload_snapshot()
                        .expect("#745 running reload")
                        .transcript_writer_state
                        .sequence,
                    6,
                    "#745 recreate-running durable writer"
                );
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
                if name == "recreate-draining" {
                    wait_draining(&old).await;
                    assert!(
                        h.owner
                            .current_handle(&"session-745".into())
                            .expect("#745 retained old handle")
                            .same_identity(&old),
                        "#745 recreate-draining no early publication"
                    );
                    drop(activity);
                }
                let new = bounded(name, &mut future)
                    .await
                    .expect("#745 recreate result");
                assert!(
                    !new.same_identity(&old) && old.state() == HandleState::Disposed,
                    "#745 recreate-idle conclusion"
                );
                if name == "recreate-draining" {
                    let cached = bounded(name, h.owner.get_or_create(h.context()))
                        .await
                        .expect("#745 cached after drain");
                    assert!(
                        cached.same_identity(&new),
                        "#745 recreate-draining cached identity"
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
        _writer_lease: Arc::new(Mutex::new(None)),
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
    wait_draining(&old).await;
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

async fn wait_draining(old: &LiveSessionHandle) {
    bounded("writer drain cutoff", async {
        while old.state() == HandleState::Active {
            tokio::task::yield_now().await;
        }
    })
    .await;
}
fn assert_strict_transcript(h: &Harness) -> u64 {
    let text = std::fs::read_to_string(h.root.join("session-745.jsonl"))
        .expect("#745 fence transcript readable");
    let mut previous = 0;
    for line in text.lines() {
        let row: serde_json::Value =
            serde_json::from_str(line).expect("#745 fence transcript JSON");
        let sequence = row["sequence"]
            .as_u64()
            .expect("#745 fence transcript sequence");
        assert!(
            sequence > previous,
            "#745 fence sequence strictly increasing"
        );
        previous = sequence;
    }
    previous
}
// Retain the TS corruption observation; Rust must now preserve the durable tail.
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
    let creation = tokio::spawn(h.factory.create(h.context(), Some(old.clone())));
    wait_draining(&old).await;
    assert!(
        !lock(&h.observed.events).iter().any(|e| e == "storage.2"),
        "#745 fence no storage before old drain"
    );
    assert_eq!(
        lock(&h.observed.checkpoints).len(),
        1,
        "#745 fence no read before old drain"
    );
    for line in transcript_bytes(&ts, "finalFiles").lines().skip(5) {
        binding
            .recorder
            .writer()
            .record_entry(&js(line))
            .expect("#745 interleaving old terminal replay");
    }
    drop(turn);
    if before_read {
        bounded(name, h.gate.entered()).await;
        assert_eq!(
            old.state(),
            HandleState::Disposed,
            "#745 fence old closed before load"
        );
        h.gate.open();
    }
    let new = bounded(name, creation)
        .await
        .expect("#745 interleaving new task")
        .expect("#745 interleaving new handle");
    assert_eq!(
        new.reload_snapshot()
            .expect("#745 interleaving reload")
            .transcript_writer_state
            .sequence,
        7,
        "#745 interleaving durable writer after drain"
    );
    let expected = oracle_document();
    for row in expected["writerInterleavings"]
        .as_array()
        .expect("#745 TS writer rows")
    {
        assert_eq!(
            row["tsDefect"], true,
            "#745 TS defect explicitly registered"
        );
        assert_eq!(
            row["error"]["code"], "sequence_not_increasing",
            "#745 TS corruption oracle"
        );
    }
    let before = assert_strict_transcript(&h);
    binding
        .recorder
        .writer()
        .record_session_metadata(
            &"session-745".into(),
            &"old-late".into(),
            js(r#"{"title":"must not append"}"#),
        )
        .expect("#745 old closed append acknowledged");
    assert_eq!(
        assert_strict_transcript(&h),
        before,
        "#745 fence old recorder stays closed"
    );
    lock(&h.observed.bindings)[1]
        .recorder
        .writer()
        .record_session_metadata(
            &"session-745".into(),
            &"turn-new".into(),
            js(r#"{"title":"replacement"}"#),
        )
        .expect("#745 interleaving replacement durable append");
    assert_eq!(
        assert_strict_transcript(&h),
        8,
        "#745 fence successor appends next sequence"
    );
    bounded(name, new.dispose("test".into()))
        .await
        .expect("#745 interleaving new dispose");
    let resumed = bounded(name, h.factory.create(h.context(), None))
        .await
        .expect("#745 fence resume succeeds");
    assert_eq!(
        resumed
            .reload_snapshot()
            .expect("#745 fence resumed snapshot")
            .transcript_writer_state
            .sequence,
        8,
        "#745 fence resumed sequence"
    );
    bounded(name, resumed.dispose("test".into()))
        .await
        .expect("#745 resumed disposal");
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
async fn writer_fence_timeout_then_owner_resume() {
    let h = Harness::new("writer-timeout");
    let old = h.create("writer-timeout").await;
    let writer = lock(&h.observed.bindings)[0].recorder.writer().clone();
    writer
        .record_session_metadata(
            &"session-745".into(),
            &"seed".into(),
            js(r#"{"title":"preserved"}"#),
        )
        .expect("#745 timeout initial durable entry");
    let bytes =
        std::fs::read(h.root.join("session-745.jsonl")).expect("#745 timeout initial bytes");
    let activity = old.begin_activity().expect("#745 timeout held activity");
    assert!(
        h.owner.mark_session_dirty(&"session-745".into(), "test"),
        "#745 timeout dirty"
    );
    tokio::time::pause();
    let creating = h.owner.get_or_create(h.context());
    wait_draining(&old).await;
    tokio::time::advance(h.factory.options.writer_busy_wait_timeout).await;
    let error = creating
        .await
        .err()
        .expect("#745 timeout must reject publication");
    assert_eq!(
        error.js_error().and_then(|e| e.code.as_deref()),
        Some("SESSION_WRITER_BUSY"),
        "#745 timeout dedicated writer error"
    );
    assert!(
        h.owner
            .current_handle(&"session-745".into())
            .expect("#745 timeout tracked predecessor")
            .same_identity(&old),
        "#745 timeout no new handle published"
    );
    assert_eq!(
        std::fs::read(h.root.join("session-745.jsonl")).expect("#745 timeout bytes"),
        bytes,
        "#745 timeout file unchanged"
    );
    assert_eq!(
        lock(&h.observed.checkpoints).len(),
        1,
        "#745 timeout never loads replacement"
    );
    drop(activity);
    old.dispose("finish".into())
        .await
        .expect("#745 timeout late drain");
    // Real disk IO and paused auto-advance must not race the production deadline.
    tokio::time::resume();
    let resumed = bounded("writer-timeout-resume", h.owner.get_or_create(h.context()))
        .await
        .expect("#745 timeout owner resumes after drain");
    assert!(
        !resumed.same_identity(&old),
        "#745 timeout owner retries with new handle"
    );
    assert_eq!(
        resumed
            .reload_snapshot()
            .expect("#745 timeout resumed snapshot")
            .transcript_writer_state
            .sequence,
        1,
        "#745 timeout resume preserves sequence"
    );
    h.finish("writer-timeout").await;
}

#[tokio::test]
async fn writer_lease_excludes_second_factory_and_alias() {
    let h = Harness::new("writer-lease");
    let old = h.create("writer-lease").await;
    // No previous handle: only the path lease can prevent a second writer.
    let alias = h.root.join("alias");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&h.root, &alias).expect("#745 lease alias setup");
    #[cfg(not(unix))]
    let alias = h.root.join(".");
    let second = ProjectSessionFactory::new(FactoryOptions {
        writer_busy_wait_timeout: h.factory.options.writer_busy_wait_timeout,
        prepare: h.factory.options.prepare.clone(),
        create_storage: Arc::new(move |context| {
            let root = alias.clone();
            Box::pin(async move {
                Ok(FactoryStorage::new(
                    Arc::new(JsonlTranscriptStore::new()),
                    TranscriptKey {
                        project_root: root.clone(),
                        pilot_home: root.clone(),
                        session_id: context.session_key,
                        transcript_path: root.join("session-745.jsonl"),
                    },
                ))
            })
        }),
        admission: None,
        diagnostic: Arc::new(|_| {}),
    });
    let creating = second.create(h.context(), None);
    let error = creating
        .await
        .err()
        .expect("#745 lease excludes duplicate writer");
    assert_eq!(
        error.js_error().and_then(|e| e.code.as_deref()),
        Some("SESSION_WRITER_BUSY"),
        "#745 lease dedicated writer error"
    );
    assert_eq!(
        old.state(),
        HandleState::Active,
        "#745 lease does not dispose other owner"
    );
    assert_eq!(
        lock(&h.observed.checkpoints).len(),
        1,
        "#745 lease acquired before load"
    );
    second
        .dispose()
        .await
        .expect("#745 second factory disposal");
    h.finish("writer-lease").await;
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
    bounded("snapshot-seed", old.dispose("seed captured".into()))
        .await
        .expect("#745 seed predecessor closed");
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

// Pin every TS field before applying the registered Rust differences below.
// This is the late-dispose schedule; twiceReturnOrders separately retains the
// early-return schedule. Neither observation is silently normalized away.
fn assert_fence_observation(row: &serde_json::Value, name: &str) {
    let parts: Vec<_> = name.split('-').collect();
    let (state, event, timing) = (parts[1], parts[2], parts[3]);
    let running = state == "quick" || state == "never";
    let mut results = Vec::new();
    if running {
        results.push(serde_json::json!({"label":"fence-old-turn","status":"fulfilled"}));
    } else if state == "disposing" {
        results.push(serde_json::json!({"label":"fence-old-dispose","status":"fulfilled"}));
    }
    let final_handle = if event == "twice" && timing != "publish" {
        3
    } else {
        2
    };
    if event == "close" || event == "shutdown" {
        results.push(
            serde_json::json!({"label":"fence-first","status":"rejected","error":{
                "name":"Error","message":if event == "close" {
                    "Scoped service is not registered in gateway-agents: agent:session-745"
                } else { "Session router shut down during session recreation." }
            }}),
        );
    } else {
        results.push(
            serde_json::json!({"label":"fence-first","status":"fulfilled","handle":final_handle}),
        );
    }
    if event == "twice" {
        results.push(
            serde_json::json!({"label":"fence-second","status":"fulfilled","handle":final_handle}),
        );
    } else if event != "recreate" {
        results.push(serde_json::json!({"label":format!("fence-{event}"),"status":"fulfilled"}));
    }
    assert_eq!(
        row,
        &serde_json::json!({
            "name":name,
            "oldState":if state == "disposing" { "draining" } else { "active" },
            "oldInFlight":usize::from(running),
            "registryAtBoundary":1,
            "registryAfterEvent":if event == "twice" { 5-final_handle } else { 1 },
            "results":results,
            "oldPendingAtBoundary":running,
        }),
        "#745 fence TS complete observation"
    );
}

fn fence_handle_id(h: &Harness, handle: &LiveSessionHandle) -> usize {
    lock(&h.observed.created_handles)
        .iter()
        .position(|created| created.same_identity(handle))
        .expect("#745 fence actual constructed handle")
        + 1
}
fn fence_registry(h: &Harness) -> serde_json::Value {
    h.owner
        .current_handle(&"session-745".into())
        .map(|handle| serde_json::json!(fence_handle_id(h, &handle)))
        .unwrap_or(serde_json::Value::Null)
}
fn fence_error(error: &RuntimeError) -> serde_json::Value {
    let mut result = serde_json::json!({"name":error.js_error().map_or("Error", |e|e.name.as_str()), "message":error.message});
    if let Some(code) = error.js_error().and_then(|e| e.code.as_ref()) {
        result["code"] = code.clone().into();
    }
    if !error.causes.is_empty() {
        result["errors"] = error.causes.iter().map(fence_error).collect();
    }
    result
}
fn fence_result(
    h: &Harness,
    label: &str,
    result: &Result<LiveSessionHandle, RuntimeError>,
) -> serde_json::Value {
    match result {
        Ok(handle) => {
            serde_json::json!({"label":label,"status":"fulfilled","handle":fence_handle_id(h,handle)})
        }
        Err(error) => {
            serde_json::json!({"label":label,"status":"rejected","error":fence_error(error)})
        }
    }
}
fn fence_expected_rust(row: &serde_json::Value) -> serde_json::Value {
    let name = row["name"].as_str().expect("#745 fence name");
    let parts: Vec<_> = name.split('-').collect();
    let (state, event) = (parts[1], parts[2]);
    let mut expected = row.clone();
    // A7.2-SESSION-WRITER-FENCE-001-R1: no live predecessor at load;
    // never cannot reach load, so observe its draining prefix instead.
    expected["oldState"] = if state == "never" {
        "draining"
    } else {
        "disposed"
    }
    .into();
    expected["oldInFlight"] = usize::from(state == "never").into();
    expected["oldPendingAtBoundary"] = (state == "never").into();
    // A7.2-SESSION-RECREATE-DRAIN-001-R1: serial second call and immediate
    // shutdown removal; this registration remains unapproved.
    if event == "twice" {
        expected["registryAfterEvent"] = 1.into();
    }
    if event == "shutdown" {
        expected["registryAfterEvent"] = serde_json::Value::Null;
    }
    for result in expected["results"]
        .as_array_mut()
        .expect("#745 fence expected results")
    {
        let label = result["label"]
            .as_str()
            .expect("#745 fence result label")
            .to_owned();
        if label == "fence-first" || label == "fence-second" {
            if state == "never" {
                // WRITER-FENCE bound, literal shape independent of implementation.
                *result = serde_json::json!({"label":label,"status":"rejected","error":{"name":"Error","code":"SESSION_WRITER_BUSY","message":"Session writer is still draining; retry session recreation."}});
            } else if event == "twice" || event == "close" {
                *result = serde_json::json!({"label":label,"status":"fulfilled","handle":2});
            }
        }
    }
    expected
}

// Every field is checked, including exact rejection shape and identity. Rust
// intentionally differs at the drain/load boundary (WRITER-FENCE-001), and at
// second-call/close ordering (RECREATE-DRAIN-001, still pending approval).
async fn fence_matrix(name: &'static str) {
    let h = Harness::new(name);
    let oracle = oracle_document();
    let row = oracle["fenceMatrix"]
        .as_array()
        .expect("#745 fence TS matrix")
        .iter()
        .find(|r| r["name"] == name)
        .expect("#745 fence TS cell");
    assert_fence_observation(row, name);
    let parts: Vec<_> = name.split('-').collect();
    let (state, event) = (parts[1], parts[2]);
    let old = h.create(name).await;
    lock(&h.observed.bindings)[0]
        .recorder
        .writer()
        .record_session_metadata(
            &"session-745".into(),
            &"seed".into(),
            js(r#"{"title":"matrix"}"#),
        )
        .expect("#745 matrix initial durable entry");
    let mut activity = if state == "quick" || state == "never" {
        Some(old.begin_activity().expect("#745 matrix held old turn"))
    } else {
        None
    };
    let disposal = if state == "disposing" {
        let pending = old.dispose("matrix prior disposal".into());
        bounded(name, h.drain.entered()).await;
        Some(pending)
    } else {
        None
    };
    assert!(
        h.owner.mark_session_dirty(&"session-745".into(), "matrix"),
        "#745 matrix dirty"
    );
    if state == "never" {
        tokio::time::pause();
    }
    let first = h.owner.get_or_create(h.context());
    if state == "disposing" {
        bounded(name, async {
            while h.observed.creates.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        h.drain.open();
    } else if state == "quick" || state == "never" {
        wait_draining(&old).await;
        if state == "quick" {
            activity.take();
        }
    }
    if state != "never" {
        bounded(name, h.gate.entered()).await;
    }
    let old_in_flight = old.in_flight_for_test();
    let mut actual = serde_json::json!({
        "name":name,
        "oldState":match old.state() {
            HandleState::Active => "active", HandleState::Draining => "draining", HandleState::Disposed => "disposed"
        },
        "oldInFlight":old_in_flight,
        "oldPendingAtBoundary":old_in_flight != 0,
        "registryAtBoundary":fence_registry(&h),
        "registryAfterEvent":null,
        "results":[],
    });
    let second = (event == "twice").then(|| h.owner.get_or_create(h.context()));
    let closing = match event {
        "close" => Some(h.owner.close(&"session-745".into())),
        "shutdown" => Some(h.owner.shutdown()),
        _ => None,
    };
    actual["registryAfterEvent"] = fence_registry(&h);
    let mut result_rows = Vec::new();
    if state == "quick" || state == "never" {
        result_rows.push(serde_json::json!({"label":"fence-old-turn","status":"fulfilled"}));
    } else if state == "disposing" {
        result_rows.push(serde_json::json!({"label":"fence-old-dispose","status":"fulfilled"}));
    }
    if state == "never" {
        tokio::time::advance(h.factory.options.writer_busy_wait_timeout).await;
        let first = first.await;
        result_rows.push(fence_result(&h, "fence-first", &first));
        let error = first.err().expect("#745 matrix timeout rejects first");
        assert_eq!(
            error.js_error().and_then(|e| e.code.as_deref()),
            Some("SESSION_WRITER_BUSY"),
            "#745 matrix dedicated timeout"
        );
        if let Some(second) = second {
            let second = second.await;
            result_rows.push(fence_result(&h, "fence-second", &second));
            let error = second.err().expect("#745 matrix timeout rejects second");
            assert_eq!(
                error.js_error().and_then(|e| e.code.as_deref()),
                Some("SESSION_WRITER_BUSY"),
                "#745 matrix second timeout"
            );
        }
        assert_eq!(
            lock(&h.observed.checkpoints).len(),
            1,
            "#745 matrix timeout blocks load"
        );
        assert_eq!(
            h.observed.published.load(Ordering::SeqCst),
            0,
            "#745 matrix timeout blocks publish"
        );
        assert_eq!(
            assert_strict_transcript(&h),
            1,
            "#745 matrix timeout file preserved"
        );
        activity.take();
        old.dispose("matrix timeout cleanup".into())
            .await
            .expect("#745 matrix timeout drain");
        tokio::time::resume();
    } else {
        assert_eq!(
            old.state(),
            HandleState::Disposed,
            "#745 matrix predecessor closed before load"
        );
        h.gate.open();
        let first = bounded(name, first).await;
        result_rows.push(fence_result(&h, "fence-first", &first));
        let ts_first = row["results"]
            .as_array()
            .expect("#745 fence results")
            .iter()
            .find(|result| result["label"] == "fence-first")
            .expect("#745 fence first result");
        if event != "close" {
            assert_eq!(
                first.is_ok(),
                ts_first["status"] == "fulfilled",
                "#745 fence first outcome matches TS"
            );
        } else {
            // RECREATE-DRAIN-001: Rust serializes close after recreation; TS
            // removes the old registry entry before candidate publication.
            assert_eq!(
                ts_first["status"], "rejected",
                "#745 fence registered close difference"
            );
            assert_eq!(
                oracle["closeReturnOrders"],
                serde_json::json!([
                    {"order":"early","error":{"name":"Error","message":"Agent is not registered: session-745"}},
                    {"order":"late","error":{"name":"Error","message":"Scoped service is not registered in gateway-agents: agent:session-745"}}
                ]),
                "#745 close distinct return orders"
            );
        }
        if event == "shutdown" {
            assert!(first.is_err(), "#745 matrix shutdown rejects publication");
        } else {
            let new = first.expect("#745 matrix replacement succeeds");
            if let Some(second) = second {
                let second = bounded(name, second).await;
                result_rows.push(fence_result(&h, "fence-second", &second));
                let second = second.expect("#745 matrix second succeeds");
                let same_handle = new.same_identity(&second);
                if name == "fence-idle-twice-beforeload" {
                    let observations = oracle["twiceReturnOrders"]
                        .as_array()
                        .expect("#745 twice return observations");
                    assert_eq!(observations.len(), 2, "#745 twice return denominator");
                    for (observation, order) in observations.iter().zip(["early", "late"]) {
                        assert_eq!(observation["order"], order, "#745 twice return order");
                        assert_eq!(
                            observation["returnsSameHandle"],
                            observation["firstHandle"] == observation["secondHandle"],
                            "#745 twice return identity evidence"
                        );
                    }
                    assert!(
                        observations
                            .iter()
                            .any(|row| row["returnsSameHandle"] == same_handle),
                        "#745 twice Rust identity belongs to TS observations"
                    );
                    assert_eq!(
                        observations,
                        serde_json::json!([
                            {"order":"early","firstHandle":3,"secondHandle":2,"returnsSameHandle":false},
                            {"order":"late","firstHandle":3,"secondHandle":3,"returnsSameHandle":true}
                        ]).as_array().expect("#745 twice expected observations"),
                        "#745 twice distinct return orders"
                    );
                }
                assert!(same_handle, "#745 matrix two recreates share writer");
            }
        }
        assert_eq!(
            assert_strict_transcript(&h),
            1,
            "#745 matrix preserved transcript"
        );
    }
    if let Some(closing) = closing {
        bounded(name, closing)
            .await
            .expect("#745 matrix close shutdown completes");
        result_rows
            .push(serde_json::json!({"label":format!("fence-{event}"),"status":"fulfilled"}));
        assert!(
            h.owner.current_handle(&"session-745".into()).is_none(),
            "#745 matrix close leaves no handle"
        );
    }
    if let Some(disposal) = disposal {
        bounded(name, disposal)
            .await
            .expect("#745 matrix shared disposal");
    }
    actual["results"] = result_rows.into();
    let expected = fence_expected_rust(row);
    assert_eq!(
        actual, expected,
        "#745 fence runtime complete observation {name}"
    );
    h.finish(name).await;
}
macro_rules! fence_cells {
    ($($test:ident => $cell:literal),* $(,)?) => { $(
        #[tokio::test]
        async fn $test() { fence_matrix($cell).await; }
    )* };
}
fence_cells! {
    fence_idle_recreate_beforeload => "fence-idle-recreate-beforeload",
    fence_idle_recreate_afterload => "fence-idle-recreate-afterload",
    fence_idle_recreate_publish => "fence-idle-recreate-publish",
    fence_idle_twice_beforeload => "fence-idle-twice-beforeload",
    fence_idle_twice_afterload => "fence-idle-twice-afterload",
    fence_idle_twice_publish => "fence-idle-twice-publish",
    fence_idle_close_beforeload => "fence-idle-close-beforeload",
    fence_idle_close_afterload => "fence-idle-close-afterload",
    fence_idle_close_publish => "fence-idle-close-publish",
    fence_idle_shutdown_beforeload => "fence-idle-shutdown-beforeload",
    fence_idle_shutdown_afterload => "fence-idle-shutdown-afterload",
    fence_idle_shutdown_publish => "fence-idle-shutdown-publish",
    fence_quick_recreate_beforeload => "fence-quick-recreate-beforeload",
    fence_quick_recreate_afterload => "fence-quick-recreate-afterload",
    fence_quick_recreate_publish => "fence-quick-recreate-publish",
    fence_quick_twice_beforeload => "fence-quick-twice-beforeload",
    fence_quick_twice_afterload => "fence-quick-twice-afterload",
    fence_quick_twice_publish => "fence-quick-twice-publish",
    fence_quick_close_beforeload => "fence-quick-close-beforeload",
    fence_quick_close_afterload => "fence-quick-close-afterload",
    fence_quick_close_publish => "fence-quick-close-publish",
    fence_quick_shutdown_beforeload => "fence-quick-shutdown-beforeload",
    fence_quick_shutdown_afterload => "fence-quick-shutdown-afterload",
    fence_quick_shutdown_publish => "fence-quick-shutdown-publish",
    fence_never_recreate_beforeload => "fence-never-recreate-beforeload",
    fence_never_recreate_afterload => "fence-never-recreate-afterload",
    fence_never_recreate_publish => "fence-never-recreate-publish",
    fence_never_twice_beforeload => "fence-never-twice-beforeload",
    fence_never_twice_afterload => "fence-never-twice-afterload",
    fence_never_twice_publish => "fence-never-twice-publish",
    fence_never_close_beforeload => "fence-never-close-beforeload",
    fence_never_close_afterload => "fence-never-close-afterload",
    fence_never_close_publish => "fence-never-close-publish",
    fence_never_shutdown_beforeload => "fence-never-shutdown-beforeload",
    fence_never_shutdown_afterload => "fence-never-shutdown-afterload",
    fence_never_shutdown_publish => "fence-never-shutdown-publish",
    fence_disposing_recreate_beforeload => "fence-disposing-recreate-beforeload",
    fence_disposing_recreate_afterload => "fence-disposing-recreate-afterload",
    fence_disposing_recreate_publish => "fence-disposing-recreate-publish",
    fence_disposing_twice_beforeload => "fence-disposing-twice-beforeload",
    fence_disposing_twice_afterload => "fence-disposing-twice-afterload",
    fence_disposing_twice_publish => "fence-disposing-twice-publish",
    fence_disposing_close_beforeload => "fence-disposing-close-beforeload",
    fence_disposing_close_afterload => "fence-disposing-close-afterload",
    fence_disposing_close_publish => "fence-disposing-close-publish",
    fence_disposing_shutdown_beforeload => "fence-disposing-shutdown-beforeload",
    fence_disposing_shutdown_afterload => "fence-disposing-shutdown-afterload",
    fence_disposing_shutdown_publish => "fence-disposing-shutdown-publish",
}

#[tokio::test]
async fn writer_fence_slow_storage_is_not_writer_timeout() {
    let h = Harness::new("slow-storage");
    let creating = h.owner.get_or_create(h.context());
    bounded("slow storage", h.gate.entered()).await;
    tokio::time::pause();
    tokio::time::advance(h.factory.options.writer_busy_wait_timeout * 2).await;
    tokio::time::resume();
    h.gate.open();
    let new = bounded("slow storage", creating)
        .await
        .expect("#745 slow storage has full writer budget");
    assert_eq!(
        new.state(),
        HandleState::Active,
        "#745 slow storage publishes normally"
    );
    h.finish("slow-storage").await;
}
#[tokio::test]
async fn writer_fence_timeout_returns_no_candidate() {
    let h = Harness::new("timeout-candidate");
    let old = h.create("timeout-candidate").await;
    let activity = old
        .begin_activity()
        .expect("#745 candidate held predecessor");
    tokio::time::pause();
    let creating = tokio::spawn(h.factory.create(h.context(), Some(old.clone())));
    wait_draining(&old).await;
    tokio::time::advance(h.factory.options.writer_busy_wait_timeout).await;
    let error = creating
        .await
        .expect("#745 candidate task completes")
        .err()
        .expect("#745 timeout cannot return publishable candidate");
    assert_eq!(
        error.js_error().and_then(|e| e.code.as_deref()),
        Some("SESSION_WRITER_BUSY"),
        "#745 candidate timeout dedicated error"
    );
    drop(activity);
    old.dispose("late drain".into())
        .await
        .expect("#745 candidate late drain");
    tokio::time::resume();
    h.finish("timeout-candidate").await;
}
#[tokio::test]
async fn writer_fence_external_ledger_lock_rejects_before_load() {
    let h = Harness::new("external-ledger");
    let (_, exclusive) = crate::storage::LedgerLock::acquire(&h.root.join("session-745.jsonl"))
        .expect("#745 external lease acquired");
    let error = h
        .factory
        .create(h.context(), None)
        .await
        .err()
        .expect("#745 external lease excludes writer");
    assert_eq!(
        error.js_error().and_then(|e| e.code.as_deref()),
        Some("SESSION_WRITER_BUSY"),
        "#745 external lease dedicated error"
    );
    assert_eq!(
        lock(&h.observed.checkpoints).len(),
        0,
        "#745 external lease prevents load"
    );
    drop(exclusive);
    let resumed = h.create("external-ledger-resume").await;
    assert_eq!(
        resumed.state(),
        HandleState::Active,
        "#745 external lease release permits resume"
    );
    h.finish("external-ledger").await;
}

#[tokio::test]
async fn recreate_while_storage_close_is_pending() {
    let h = Harness::new("dispose-close-stage");
    let old = h.create("dispose-close-stage").await;
    let disposal = old.dispose("prior".into());
    bounded("close entered", h.gate.entered()).await;
    assert!(
        old.reload_snapshot().is_err(),
        "#745 closed adapter rejects ordinary reload"
    );
    let creation = tokio::spawn(h.factory.create(h.context(), Some(old.clone())));
    bounded("recreate prepared", async {
        while h.observed.creates.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    h.gate.open();
    let new = bounded("close-stage recreation", creation)
        .await
        .expect("#745 close-stage task")
        .expect("#745 draining closed adapter still recreates");
    disposal.await.expect("#745 close-stage old disposal");
    new.dispose("test".into())
        .await
        .expect("#745 close-stage new disposal");
    h.finish("dispose-close-stage").await;
}

#[tokio::test]
async fn recreate_snapshot_rejects_disposal_during_prepare() {
    let h = Harness::new("snapshot-before-prepare");
    let old = h.create("snapshot-before-prepare").await;
    assert!(
        h.owner
            .mark_session_dirty(&"session-745".into(), "prepare-race"),
        "#745 prepare marks cached handle dirty"
    );
    let creating = h.owner.get_or_create(h.context());
    bounded("prepare snapshot", h.gate.entered()).await;
    old.dispose("finish during prepare".into())
        .await
        .expect("#745 dispose during prepare");
    h.gate.open();
    let error = bounded("prepare snapshot", creating)
        .await
        .err()
        .expect("#745 prepare disposal rejects recreation");
    let actual =
        serde_json::json!({"status":"rejected","error":{"name":"Error","message":error.message}});
    assert_eq!(
        actual,
        oracle_document()["prepareDisposal"],
        "#745 prepare disposal matches TS error"
    );
    assert_eq!(
        h.leases(),
        0,
        "#745 prepare disposal releases prepared lease"
    );
    let new = h.create("prepare-race retry").await;
    assert!(
        !new.same_identity(&old),
        "#745 prepare disposal next retry resumes"
    );
    h.finish("snapshot-before-prepare").await;
}

#[tokio::test]
async fn writer_abort_panic_releases_lease_before_session_dispose() {
    abort_panic_case("str").await;
}

#[tokio::test]
async fn writer_abort_owned_string_panic_retains_error() {
    abort_panic_case("String").await;
}

#[tokio::test]
async fn writer_abort_non_string_panic_retains_fallback() {
    abort_panic_case("Any").await;
}

async fn abort_panic_case(payload: &'static str) {
    struct AbortPanic(Arc<dyn LiveSession>, Arc<Semaphore>, &'static str);
    impl LiveSession for AbortPanic {
        fn snapshot(&self) -> JsValue {
            self.0.snapshot()
        }
        fn abort(&self, _: &JsString) -> RuntimeResult {
            self.1.add_permits(1);
            match self.2 {
                "str" => panic!("ABORT_1022"),
                "String" => std::panic::panic_any(String::from("ABORT_1022")),
                _ => std::panic::panic_any(1022_u32),
            }
        }
        fn disposal_guard(&self) -> Option<Box<dyn Send>> {
            self.0.disposal_guard()
        }
        fn dispose(&self) -> DisposeFuture<'_> {
            self.0.dispose()
        }
    }
    let h = Harness::new("abort-panic");
    let original = followup_bounded("abort initial", h.factory.create(h.context(), None))
        .await
        .expect("#1022 abort initial create");
    let writer = lock(&h.observed.bindings)
        .first()
        .expect("#1022 abort initial binding")
        .recorder
        .writer()
        .clone();
    let aborted = Arc::new(Semaphore::new(0));
    let old = LiveSessionHandle::new(
        Arc::new(AbortPanic(
            original.session_for_test(),
            aborted.clone(),
            payload,
        )),
        None,
    );
    let activity = old.begin_activity().expect("#1022 abort active turn");
    let disposal = old.dispose("test".into());
    followup_bounded("abort observed", aborted.acquire())
        .await
        .expect("#1022 abort reached adapter")
        .forget();
    assert_eq!(
        old.state(),
        HandleState::Draining,
        "#1022 abort panic still drains active turn"
    );
    assert_eq!(h.leases(), 1, "#1022 abort resources retained while active");
    drop(disposal); // Cancelling the waiter cannot cancel owned cleanup.
    drop(activity);
    let error = followup_bounded("abort dispose", old.dispose("again".into()))
        .await
        .expect_err("#1022 abort reports failure");
    assert_eq!(
        error.message,
        oracle_document()["followups"][1]["error"]["message"],
        "#1022 abort aggregate retained"
    );
    let causes: Vec<_> = error
        .causes
        .iter()
        .map(|cause| cause.message.as_str())
        .collect();
    if payload == "Any" {
        assert_eq!(
            causes,
            ["Agent session abort panicked."],
            "#1022 abort non-string fallback retained"
        );
    } else {
        assert_eq!(
            serde_json::json!(causes),
            oracle_document()["followups"][1]["error"]["errors"],
            "#1022 abort original panic retained"
        );
    }
    assert_eq!(old.state(), HandleState::Disposed, "#1022 abort disposed");
    assert_eq!(
        h.leases(),
        0,
        "#1022 abort panic releases project resources with old handle retained"
    );
    assert_eq!(
        h.observed.mcp.load(Ordering::SeqCst),
        0,
        "#1022 abort panic releases MCP with old handle retained"
    );
    assert_eq!(
        lock(&h.observed.events)
            .iter()
            .filter(|v| *v == "close.1")
            .count(),
        1,
        "#1022 abort panic closes storage exactly once"
    );
    assert!(
        original.reload_snapshot().is_err(),
        "#1022 abort panic disposes adapter with old handle retained"
    );
    let repeat = followup_bounded("abort repeat", old.dispose("repeat".into()))
        .await
        .expect_err("#1022 abort repeat error");
    assert_eq!(
        repeat.message, error.message,
        "#1022 abort shared completion"
    );
    assert_eq!(
        lock(&h.observed.events)
            .iter()
            .filter(|v| *v == "close.1")
            .count(),
        1,
        "#1022 abort repeat does not close twice"
    );
    let new = followup_bounded("abort recreate", h.factory.create(h.context(), None))
        .await
        .expect("#1022 abort replacement not BUSY");
    let before = writer.snapshot_state();
    writer
        .record_session_metadata(&"session-745".into(), &"late".into(), JsValue::Null)
        .expect("#1022 abort retired append resolves");
    assert_eq!(
        writer.snapshot_state(),
        before,
        "#1022 abort writer retired"
    );
    followup_bounded("abort replacement dispose", new.dispose("cleanup".into()))
        .await
        .expect("#1022 abort replacement cleanup");
    h.finish("abort-panic").await;
}

#[tokio::test]
async fn writer_fence_rejects_zero_budget() {
    let h = Harness::new("zero-budget");
    let rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ProjectSessionFactory::new(FactoryOptions {
            writer_busy_wait_timeout: Duration::ZERO,
            prepare: h.factory.options.prepare.clone(),
            create_storage: h.factory.options.create_storage.clone(),
            admission: None,
            diagnostic: Arc::new(|_| {}),
        })
    }));
    assert!(rejected.is_err(), "#745 zero fence budget rejected");
    h.finish("zero-budget").await;
}

#[tokio::test]
async fn writer_fence_exhausted_drain_budget_acquires_idle_lease() {
    let h = Harness::new("exhausted-budget");
    let old = h.create("exhausted-budget").await;
    let activity = old
        .begin_activity()
        .expect("#745 exhausted budget active turn");
    tokio::time::pause();
    let mut creating = Box::pin(h.factory.construct(h.context(), Some(old.clone())));
    // Poll exactly to the drain wait, then hold the factory caller while the
    // owned disposal finishes. Both drain completion and deadline are ready
    // when we resume polling, so remaining writer wait is deterministically zero.
    let first = std::future::poll_fn(|cx| std::task::Poll::Ready(creating.as_mut().poll(cx))).await;
    assert!(
        first.is_pending(),
        "#745 exhausted budget awaits predecessor"
    );
    assert_eq!(
        old.state(),
        HandleState::Draining,
        "#745 exhausted budget drain started"
    );
    tokio::time::advance(h.factory.options.writer_busy_wait_timeout).await;
    tokio::time::resume();
    drop(activity);
    bounded("exhausted budget drain", old.dispose("finish".into()))
        .await
        .expect("#745 exhausted budget predecessor completes");
    let path = h.root.join("session-745.jsonl");
    let lease = bounded(
        "exhausted budget unlock",
        acquire_writer(
            path.clone(),
            tokio::time::Instant::now() + Duration::from_secs(1),
        ),
    )
    .await
    .expect("#745 exhausted budget predecessor releases lease");
    drop(lease);
    let new = bounded("exhausted budget recreate", creating)
        .await
        .expect("#745 exhausted budget still acquires idle writer");
    // An expired deadline still rejects an occupied lease immediately; try-once
    // must never become a bypass of exclusivity or an unbounded busy wait.
    let error = bounded(
        "exhausted budget busy",
        acquire_writer(path, tokio::time::Instant::now()),
    )
    .await
    .err()
    .expect("#745 exhausted budget busy lease rejected");
    assert_eq!(
        error.js_error().and_then(|e| e.code.as_deref()),
        Some("SESSION_WRITER_BUSY"),
        "#745 exhausted budget busy error code"
    );
    new.dispose("cleanup".into())
        .await
        .expect("#745 exhausted budget replacement cleanup");
    h.finish("exhausted-budget").await;
}

#[tokio::test]
async fn writer_close_panic_releases_lease() {
    let h = Harness::new("storage-close-panic");
    let old = bounded("panic create", h.factory.create(h.context(), None))
        .await
        .expect("#745 panic initial create");
    let writer = lock(&h.observed.bindings)[0].recorder.writer().clone();
    let result = bounded("panic dispose", old.dispose("test panic".into())).await;
    assert!(result.is_err(), "#745 panic disposal reports failure");
    assert_eq!(
        old.state(),
        HandleState::Disposed,
        "#745 panic handle disposed"
    );
    assert!(
        lock(&lock(&h.observed.writer_leases)[0]).is_none(),
        "#745 panic lease released"
    );
    let before = writer.snapshot_state();
    writer
        .record_session_metadata(
            &"session-745".into(),
            &"late".into(),
            js(r#"{"late":true}"#),
        )
        .expect("#745 panic old writer append resolves");
    assert_eq!(
        writer.snapshot_state(),
        before,
        "#745 panic old writer retired"
    );
    let new = bounded("panic recreate", h.factory.create(h.context(), None))
        .await
        .expect("#745 panic subsequent create not BUSY");
    new.dispose("cleanup".into())
        .await
        .expect("#745 panic replacement cleanup");
    h.finish("storage-close-panic").await;
}

#[tokio::test]
async fn writer_rollback_close_panic_releases_lease() {
    let h = Harness::new("rollback-close-panic");
    let mut storage = (h.factory.options.create_storage)(h.context())
        .await
        .expect("#745 rollback storage");
    *lock(&storage.writer_lease) = Some(
        acquire_writer(
            storage.recorder.writer().path().to_owned(),
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .expect("#745 rollback initial lease"),
    );
    storage.close = Arc::new(|| Box::pin(async { panic!("#745 injected rollback close panic") }));
    let storage = Arc::new(storage);
    let retained = storage.clone();
    let failed = bounded(
        "rollback panic",
        tokio::spawn(async move { retained.close_owned().await }),
    )
    .await;
    assert!(
        failed.is_err_and(|e| e.is_panic()),
        "#745 rollback hook actually panicked"
    );
    assert!(
        lock(&storage.writer_lease).is_none(),
        "#745 rollback panic lease released"
    );
    let writer = storage.recorder.writer();
    let before = writer.snapshot_state();
    writer
        .record_session_metadata(
            &"session-745".into(),
            &"late".into(),
            js(r#"{"late":true}"#),
        )
        .expect("#745 rollback panic late append resolves");
    assert_eq!(
        writer.snapshot_state(),
        before,
        "#745 rollback panic writer retired"
    );
    let new = h.create("rollback-panic retry").await;
    assert_eq!(
        new.state(),
        HandleState::Active,
        "#745 rollback panic subsequent create not BUSY"
    );
    h.finish("rollback-close-panic").await;
}

#[tokio::test]
async fn writer_fence_uses_configured_budget() {
    let h = Harness::with_fence_timeout("configured-budget", Duration::from_secs(5));
    let old = h.create("configured-budget").await;
    let activity = old
        .begin_activity()
        .expect("#745 configured budget holds old turn");
    tokio::time::pause();
    let creating = tokio::spawn(h.factory.create(h.context(), Some(old.clone())));
    wait_draining(&old).await;
    tokio::time::advance(Duration::from_millis(1100)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(
        !creating.is_finished(),
        "#745 configured budget exceeds default close timeout"
    );
    drop(activity);
    tokio::time::resume();
    let new = bounded("configured drain", creating)
        .await
        .expect("#745 configured drain task")
        .expect("#745 configured budget allows slower drain");
    let activity = new
        .begin_activity()
        .expect("#745 configured timeout holds turn");
    tokio::time::pause();
    let creating = tokio::spawn(h.factory.create(h.context(), Some(new.clone())));
    wait_draining(&new).await;
    tokio::time::advance(Duration::from_secs(5)).await;
    let error = creating
        .await
        .expect("#745 configured timeout task")
        .err()
        .expect("#745 configured timeout rejects candidate");
    assert_eq!(
        error.js_error().and_then(|e| e.code.as_deref()),
        Some("SESSION_WRITER_BUSY"),
        "#745 configured timeout dedicated error"
    );
    drop(activity);
    new.dispose("cleanup".into())
        .await
        .expect("#745 configured timeout late drain");
    tokio::time::resume();
    h.finish("configured-budget").await;
}

#[tokio::test]
async fn writer_factory_rejects_already_locked_storage() {
    // The failed factory's close thread may retain the locked store even after
    // dispose and our outer Arcs drop. Observe the OS lock, not those drops.
    async fn wait_for_unlock(path: &Path) {
        loop {
            match crate::storage::LedgerLock::acquire(path) {
                Ok((_path, lease)) => {
                    drop(lease);
                    return; // #1027 lock release observed
                }
                Err(crate::storage::LedgerError::Io(error))
                    if error.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    tokio::task::yield_now().await; // #1027 retry only while locked
                }
                Err(error) => panic!("#1027 lock release probe failed: {error}"),
            }
        }
    }
    async fn assert_waiting(future: std::pin::Pin<&mut impl Future<Output = ()>>) {
        let mut future = future;
        crate::negative_assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(
                future.as_mut().poll(cx).is_pending()
            ))
            .await,
            "#1027 lock release wait must remain pending while store is retained"
        );
    }

    let h = Harness::new("already-locked-storage");
    let (store, path) = JsonlTranscriptStore::open_locked(&h.root.join("session-745.jsonl"))
        .expect("#1027 locked storage owns ledger");
    let lock_path = path.clone();
    let store = Arc::new(store);
    let retained = store.clone();
    let root = h.root.clone();
    let factory = ProjectSessionFactory::new(FactoryOptions {
        writer_busy_wait_timeout: h.factory.options.writer_busy_wait_timeout,
        prepare: h.factory.options.prepare.clone(),
        create_storage: Arc::new(move |context| {
            let store = retained.clone();
            let root = root.clone();
            let path = path.clone();
            Box::pin(async move {
                Ok(FactoryStorage::new(
                    store,
                    TranscriptKey {
                        project_root: root.clone(),
                        pilot_home: root,
                        transcript_path: path,
                        session_id: context.session_key,
                    },
                ))
            })
        }),
        admission: None,
        diagnostic: Arc::new(|_| {}),
    });
    let error = bounded(
        "#1027 locked storage create",
        factory.create(h.context(), None),
    )
    .await
    .err()
    .expect("#1027 locked store cannot be double leased");
    assert_eq!(
        error.js_error().and_then(|e| e.code.as_deref()),
        Some("SESSION_WRITER_BUSY"),
        "#1027 locked store factory error contract"
    );
    assert_eq!(
        h.leases(),
        0,
        "#1027 locked store releases prepared resources"
    );
    factory
        .dispose()
        .await
        .expect("#1027 locked store factory cleanup");
    let delayed_release = store.clone();
    drop(factory);
    drop(store);

    // Cancellation must not release another holder's lock.
    let mut cancelled = Box::pin(wait_for_unlock(&lock_path));
    assert_waiting(cancelled.as_mut()).await;
    drop(cancelled);
    assert!(
        matches!(
            crate::storage::LedgerLock::acquire(&lock_path),
            Err(crate::storage::LedgerError::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
        ),
        "#1027 cancelled release wait leaves the store locked"
    );

    // A holder that never releases must hit the same bounded wait budget.
    tokio::time::pause();
    let mut never_released = Box::pin(wait_for_unlock(&lock_path));
    assert_waiting(never_released.as_mut()).await;
    let expires = tokio::time::timeout(Duration::from_secs(8), never_released);
    // Cross the deadline including timer rounding; a yielding waiter keeps a
    // paused runtime busy, so it cannot auto-advance the remaining timer tick.
    tokio::time::advance(Duration::from_secs(9)).await;
    expires
        .await
        .expect_err("#1027 retained store wait is bounded");
    tokio::time::resume();

    let mut released = Box::pin(wait_for_unlock(&lock_path));
    assert_waiting(released.as_mut()).await;
    drop(delayed_release);
    crate::test_negative_receipt::expect_ok(
        tokio::time::timeout(Duration::from_secs(8), released).await,
        "#1027 dropped store eventually releases its ledger lock",
    );
    let new = h.create("#1027 unlocked storage retry").await;
    assert_eq!(
        new.state(),
        HandleState::Active,
        "#1027 unlocked storage permits create"
    );
    h.finish("#1027 already-locked-storage").await;
}

#[tokio::test]
async fn writer_disposal_preserves_final_hooks() {
    let h = Harness::new("dispose-final-write");
    let old = h.create("dispose-final-write").await;
    old.dispose("test".into())
        .await
        .expect("#745 release hook disposal");
    assert_eq!(
        lock(&h.observed.bindings)[0]
            .recorder
            .writer()
            .snapshot_state()
            .sequence,
        1,
        "#745 release hook final write committed"
    );
    h.finish("dispose-final-write").await;
}

#[tokio::test]
async fn writer_rollback_close_preserves_final_hook() {
    let h = Harness::new("rollback-final-write");
    let mut storage = (h.factory.options.create_storage)(h.context())
        .await
        .expect("#745 final hook storage");
    *lock(&storage.writer_lease) = Some(
        acquire_writer(
            storage.recorder.writer().path().to_owned(),
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .expect("#745 final hook lease"),
    );
    let writer = storage.recorder.writer().clone();
    storage.close = Arc::new(move || {
        let writer = writer.clone();
        Box::pin(async move {
            writer
                .record_session_metadata(
                    &"session-745".into(),
                    &"close".into(),
                    js(r#"{"final":true}"#),
                )
                .expect("#745 close hook final write");
            Ok(())
        })
    });
    storage.close_owned().await.expect("#745 final hook close");
    assert_eq!(
        storage.recorder.writer().snapshot_state().sequence,
        1,
        "#745 close hook final write committed"
    );
    assert!(
        lock(&storage.writer_lease).is_none(),
        "#745 final hook releases lease"
    );
    h.finish("rollback-final-write").await;
}

#[tokio::test]
async fn writer_panic_cleanup_allows_runtime_progress() {
    struct BlockingStore {
        entered: tokio::sync::Notify,
        barrier: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    }
    impl TranscriptStore for BlockingStore {
        fn append(&self, _: &TranscriptKey, _: &JsValue) -> crate::storage::StoreResult<()> {
            self.entered.notify_one();
            let (released, wake) = &*self.barrier;
            let mut released = released.lock().expect("#745 panic append barrier");
            while !*released {
                released = wake.wait(released).expect("#745 panic append release");
            }
            Ok(())
        }
        fn read(
            &self,
            _: &TranscriptKey,
            _: Option<u64>,
        ) -> crate::storage::StoreResult<TranscriptRead> {
            unreachable!("#745 panic background test never loads")
        }
    }
    let h = Harness::new("panic-background");
    let barrier = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let store = Arc::new(BlockingStore {
        entered: tokio::sync::Notify::new(),
        barrier: barrier.clone(),
    });
    let mut storage = FactoryStorage::new(
        store.clone(),
        TranscriptKey {
            project_root: h.root.clone(),
            pilot_home: h.root.clone(),
            transcript_path: h.root.join("session-745.jsonl"),
            session_id: "session-745".into(),
        },
    );
    *lock(&storage.writer_lease) = Some(
        acquire_writer(
            storage.recorder.writer().path().to_owned(),
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .expect("#745 panic background lease"),
    );
    storage.close = Arc::new(|| Box::pin(async { panic!("#745 injected background close panic") }));
    let storage = Arc::new(storage);
    let writer = storage.recorder.writer().clone();
    let append = tokio::task::spawn_blocking(move || {
        writer.record_session_metadata(
            &"session-745".into(),
            &"background".into(),
            js(r#"{"final":true}"#),
        )
    });
    bounded("panic append entered", store.entered.notified()).await;
    let watchdog_fired = Arc::new(AtomicBool::new(false));
    let watched = watchdog_fired.clone();
    let watchdog_barrier = barrier.clone();
    let watchdog = std::thread::spawn(move || {
        let (released, wake) = &*watchdog_barrier;
        let (mut released, _) = wake
            .wait_timeout_while(
                released.lock().expect("#745 panic watchdog barrier"),
                Duration::from_secs(3),
                |released| !*released,
            )
            .expect("#745 panic watchdog wait");
        if !*released {
            watched.store(true, Ordering::SeqCst);
            *released = true;
            wake.notify_all();
        }
    });
    let retained = storage.clone();
    let failed = tokio::spawn(async move { retained.close_owned().await }).await;
    let retained_until_drain = matches!(
        crate::storage::LedgerLock::acquire(storage.recorder.writer().path()),
        Err(crate::storage::LedgerError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock
    );
    // This release requires the current-thread runtime to regain control.
    let (released, wake) = &*barrier;
    *released.lock().expect("#745 panic runtime releases append") = true;
    wake.notify_all();
    watchdog.join().expect("#745 panic watchdog joined");
    assert!(
        !watchdog_fired.load(Ordering::SeqCst),
        "#745 panic cleanup permits runtime progress"
    );
    assert!(
        retained_until_drain,
        "#745 panic retains lease through admitted write"
    );
    assert!(
        failed.is_err_and(|e| e.is_panic()),
        "#745 panic background hook panicked"
    );
    append
        .await
        .expect("#745 panic background append joined")
        .expect("#745 panic background append complete");
    let lease = bounded(
        "panic background lease release",
        acquire_writer(
            storage.recorder.writer().path().to_owned(),
            tokio::time::Instant::now() + Duration::from_secs(1),
        ),
    )
    .await
    .expect("#745 panic background lease eventually released");
    drop(lease);
    h.finish("panic-background").await;
}

async fn followup_bounded<T>(name: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(8), future)
        .await
        .unwrap_or_else(|_| panic!("#1022 {name} bounded wait"))
}

// No filesystem or blocking pool: paused time can advance only after both
// owned creation tasks have been polled. Removing ticket.enter reaches the
// second factory before the 1ms observation deadline, deterministically.
#[tokio::test(start_paused = true)]
async fn same_key_ticket_blocks_before_publication() {
    struct Idle;
    impl LiveSession for Idle {
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
    struct QueuedFactory {
        first: Arc<Semaphore>,
        second: Arc<Semaphore>,
        release: Arc<Semaphore>,
        creates: Arc<AtomicUsize>,
    }
    impl SessionFactory for QueuedFactory {
        fn create(
            &self,
            _: SessionContext,
            _: Option<LiveSessionHandle>,
        ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
            let id = self.creates.fetch_add(1, Ordering::SeqCst);
            let entered = if id == 0 {
                self.first.clone()
            } else {
                self.second.clone()
            };
            let release = self.release.clone();
            Box::pin(async move {
                entered.add_permits(1);
                release
                    .acquire()
                    .await
                    .expect("#1022 ticket release gate")
                    .forget();
                Ok(LiveSessionHandle::new(Arc::new(Idle), None))
            })
        }
    }
    for cancel_first in [false, true] {
        let first = Arc::new(Semaphore::new(0));
        let second = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let creates = Arc::new(AtomicUsize::new(0));
        let mut options = SessionOwnerOptions::new(Arc::new(QueuedFactory {
            first: first.clone(),
            second: second.clone(),
            release: release.clone(),
            creates: creates.clone(),
        }));
        options.idle_sweep_interval_ms = 0;
        let owner = SessionOwner::new(options);
        let context = SessionContext {
            session_key: "ticket-1022".into(),
            project_key: None,
            channel_key: "test".into(),
            allowed_tools: None,
            disallowed_tools: None,
        };
        let mut a = Some(owner.get_or_create(context.clone()));
        followup_bounded("ticket first", first.acquire())
            .await
            .expect("#1022 ticket first entered")
            .forget();
        let b = owner.get_or_create(context.clone());
        if cancel_first {
            drop(a.take());
        }
        let premature = tokio::time::timeout(Duration::from_millis(1), second.acquire()).await;
        assert!(
            premature.is_err(),
            "#1022 ticket second cannot enter before first publication"
        );
        assert_eq!(
            creates.load(Ordering::SeqCst),
            1,
            "#1022 ticket one preparation while predecessor held"
        );
        release.add_permits(2);
        let b = followup_bounded("ticket second", b)
            .await
            .expect("#1022 ticket second completes");
        let a = if let Some(a) = a {
            followup_bounded("ticket first result", a)
                .await
                .expect("#1022 ticket first result")
        } else {
            followup_bounded("ticket cached", owner.get_or_create(context))
                .await
                .expect("#1022 ticket cancelled caller owned result")
        };
        assert!(a.same_identity(&b), "#1022 ticket one published identity");
        assert_eq!(
            creates.load(Ordering::SeqCst),
            1,
            "#1022 ticket no duplicate preparation"
        );
        followup_bounded("ticket shutdown", owner.shutdown())
            .await
            .expect("#1022 ticket cleanup");
    }
}

#[tokio::test(start_paused = true)]
async fn writer_waiters_are_fifo_and_cancellation_safe() {
    let mutex = Arc::new(WriterMutex::new(()));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    let a = acquire_process_writer(mutex.clone(), deadline)
        .await
        .expect("#1022 FIFO initial owner");
    let mut b = Box::pin(acquire_process_writer(mutex.clone(), deadline));
    let mut c = Box::pin(acquire_process_writer(mutex.clone(), deadline));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(b.as_mut().poll(cx)))
            .await
            .is_pending(),
        "#1022 FIFO B waits"
    );
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(c.as_mut().poll(cx)))
            .await
            .is_pending(),
        "#1022 FIFO C waits"
    );
    drop(a);
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(c.as_mut().poll(cx)))
            .await
            .is_pending(),
        "#1022 FIFO C cannot pass B"
    );
    let b = followup_bounded("FIFO B", b)
        .await
        .expect("#1022 FIFO B receives released writer");
    drop(b);
    let c = followup_bounded("FIFO C", c)
        .await
        .expect("#1022 FIFO C follows B");
    let mut cancelled = Box::pin(acquire_process_writer(mutex.clone(), deadline));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(cancelled.as_mut().poll(cx)))
            .await
            .is_pending(),
        "#1022 FIFO cancelled waiter queues"
    );
    drop(cancelled);
    let mut expires = Box::pin(acquire_process_writer(mutex.clone(), deadline));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(expires.as_mut().poll(cx)))
            .await
            .is_pending(),
        "#1022 FIFO live waiter queues"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    let expired =
        std::future::poll_fn(|cx| std::task::Poll::Ready(expires.as_mut().poll(cx))).await;
    let std::task::Poll::Ready(Err(error)) = expired else {
        panic!("#1022 FIFO expiry is ready");
    };
    assert_eq!(
        error.js_error().and_then(|e| e.code.as_deref()),
        Some("SESSION_WRITER_BUSY"),
        "#1022 FIFO timeout error"
    );
    assert!(
        mutex.clone().try_lock_owned().is_err(),
        "#1022 FIFO timeout retains owner"
    );
    drop(c);
    acquire_process_writer(mutex, deadline)
        .await
        .expect("#1022 FIFO cancellation leaves no stale waiter");
}

#[tokio::test]
async fn writer_busy_budget_excludes_path_resolution() {
    let h = Harness::new("slow-path-1022");
    let path = crate::storage::canonical_transcript_path(&h.root.join("slow-path.jsonl"))
        .expect("#1022 path canonical identity");
    for occupied in [false, true] {
        let held = if occupied {
            Some(
                acquire_writer(
                    path.clone(),
                    tokio::time::Instant::now() + Duration::from_secs(1),
                )
                .await
                .expect("#1022 path initial owner"),
            )
        } else {
            None
        };
        tokio::time::pause();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let (send, receive) = tokio::sync::oneshot::channel();
        let mut waiting = Box::pin(acquire_writer_resolved(
            async { receive.await.expect("#1022 path resolution gate") },
            deadline,
        ));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx)))
                .await
                .is_pending(),
            "#1022 path starts resolution"
        );
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx)))
                .await
                .is_pending(),
            "#1022 path IO excluded from busy budget"
        );
        tokio::time::resume();
        send.send(Ok(path.clone()))
            .expect("#1022 path resolution completes");
        let acquired = followup_bounded("path acquire", waiting).await;
        if occupied {
            let error = acquired.err().expect("#1022 path expired busy rejects");
            assert_eq!(
                error.js_error().and_then(|e| e.code.as_deref()),
                Some("SESSION_WRITER_BUSY"),
                "#1022 path busy error"
            );
        } else {
            drop(acquired.expect("#1022 path expired idle succeeds"));
        }
        drop(held);
    }
    h.finish("slow-path-1022").await;
}

async fn drained_seed_case(
    cancel_waiter: bool,
    fail_final_seed: bool,
    retry: Option<&'static str>,
) {
    struct Pending;
    impl LiveTurn for Pending {
        fn next(&mut self) -> super::super::SessionNext<'_> {
            Box::pin(std::future::pending())
        }
    }
    // Retain the actual factory adapter/storage and use the production OwnedTurn
    // checkpoint-on-drop path with a deterministic started turn, no provider I/O.
    struct StartedSession {
        inner: Arc<dyn LiveSession>,
        checkpoint: HostToolCheckpoint,
        saved: Arc<Mutex<SeedState>>,
        captured: Arc<Semaphore>,
        snapshots: AtomicUsize,
        fail_final_seed: bool,
        retry: Option<&'static str>,
    }
    impl LiveSession for StartedSession {
        fn snapshot(&self) -> JsValue {
            self.inner.snapshot()
        }
        fn reload_snapshot(&self) -> Result<ReloadSnapshot, RuntimeError> {
            self.inner.reload_snapshot()
        }
        fn draining_reload_snapshot(&self) -> Result<ReloadSnapshot, RuntimeError> {
            let read = self.snapshots.fetch_add(1, Ordering::SeqCst);
            if read > 0 && self.fail_final_seed && (self.retry.is_none() || read == 1) {
                return Err(RuntimeError::new("#1022 final seed unavailable"));
            }
            let mut snapshot = self.inner.draining_reload_snapshot()?;
            snapshot.file_state = lock(&self.saved).clone();
            self.captured.add_permits(1);
            Ok(snapshot)
        }
        fn submit(&self, _: JsValue, _: JsValue) -> Result<Box<dyn LiveTurn>, RuntimeError> {
            Ok(Box::new(OwnedTurn {
                turn: Box::new(Pending),
                checkpoint: self.checkpoint.clone(),
                saved: self.saved.clone(),
                started: Arc::new(AtomicBool::new(true)),
                finished: false,
            }))
        }
        fn abort(&self, reason: &JsString) -> RuntimeResult {
            self.inner.abort(reason)
        }
        fn disposal_guard(&self) -> Option<Box<dyn Send>> {
            if self.retry == Some("guard-panic") {
                panic!("#1022 seed retry injected guard panic");
            }
            self.inner.disposal_guard()
        }
        fn dispose(&self) -> DisposeFuture<'_> {
            Box::pin(async move {
                self.inner.dispose().await?;
                if self.retry == Some("dispose") {
                    return Err(RuntimeError::new("DISPOSE_1022"));
                }
                Ok(())
            })
        }
    }
    struct RetryFactory {
        initial: Mutex<Option<LiveSessionHandle>>,
        factory: Arc<ProjectSessionFactory>,
    }
    impl SessionFactory for RetryFactory {
        fn can_recreate(&self) -> bool {
            true
        }
        fn create(
            &self,
            context: SessionContext,
            previous: Option<LiveSessionHandle>,
        ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
            if let Some(initial) = lock(&self.initial).take() {
                return Box::pin(async move { Ok(initial) });
            }
            self.factory.create(context, previous)
        }
    }
    let h = Harness::new("drain-seed-1022");
    let original = followup_bounded("seed initial", h.factory.create(h.context(), None))
        .await
        .expect("#1022 seed initial session");
    let checkpoint = lock(&h.observed.bindings)
        .first()
        .expect("#1022 seed initial binding")
        .checkpoint
        .clone();
    lock(&checkpoint.read_file_state).insert(
        "seed.txt".into(),
        parse_json(r#"{"mtimeMs":1,"kind":"text"}"#).expect("#1022 seed initial metadata"),
    );
    let saved = Arc::new(Mutex::new(
        checkpoint
            .snapshot()
            .expect("#1022 seed initial checkpoint"),
    ));
    lock(&checkpoint.read_file_state).insert(
        "during.txt".into(),
        parse_json(r#"{"mtimeMs":2,"kind":"text"}"#).expect("#1022 seed read metadata"),
    );
    let captured = Arc::new(Semaphore::new(0));
    let old = LiveSessionHandle::new(
        Arc::new(StartedSession {
            inner: original.session_for_test(),
            checkpoint,
            saved: saved.clone(),
            captured: captured.clone(),
            snapshots: AtomicUsize::new(0),
            fail_final_seed,
            retry,
        }),
        None,
    );
    let stream = old
        .submit(JsValue::Null, JsValue::Null)
        .expect("#1022 seed started turn");
    let retry_owner = if retry.is_some() {
        let mut options = SessionOwnerOptions::new(Arc::new(RetryFactory {
            initial: Mutex::new(Some(old.clone())),
            factory: h.factory.clone(),
        }));
        options.idle_sweep_interval_ms = 0;
        let owner = SessionOwner::new(options);
        let initial = followup_bounded("seed retry initial", owner.get_or_create(h.context()))
            .await
            .expect("#1022 seed retry initial handle");
        assert!(
            initial.same_identity(&old),
            "#1022 seed retry tracks original handle"
        );
        assert!(
            owner.mark_session_dirty(&h.context().session_key, "seed-retry"),
            "#1022 seed retry dirty"
        );
        Some(owner)
    } else {
        None
    };
    let creating = if let Some(owner) = &retry_owner {
        tokio::spawn(owner.get_or_create(h.context()))
    } else {
        tokio::spawn(h.factory.create(h.context(), Some(old.clone())))
    };
    followup_bounded("seed capture", captured.acquire())
        .await
        .expect("#1022 seed before-drain capture")
        .forget();
    if retry == Some("guard-panic") {
        let first = followup_bounded("seed retry guard panic", creating)
            .await
            .expect("#1022 seed retry guard join")
            .err()
            .expect("#1022 seed retry guard failure reported");
        assert_eq!(
            first.message, "Agent session dispose task panicked.",
            "#1022 seed retry guard original error"
        );
        assert_eq!(
            old.state(),
            HandleState::Disposed,
            "#1022 seed retry guard finished while turn active"
        );
        let owner = retry_owner.as_ref().expect("#1022 seed retry guard owner");
        let again = followup_bounded(
            "seed retry active predecessor",
            owner.get_or_create(h.context()),
        )
        .await;
        assert_eq!(
            lock(&h.observed.events)
                .iter()
                .filter(|event| event.starts_with("storage."))
                .count(),
            1,
            "#1022 seed retry active predecessor cannot create storage"
        );
        assert_eq!(
            again
                .err()
                .expect("#1022 seed retry active predecessor rejects")
                .message,
            first.message,
            "#1022 seed retry active predecessor preserves failure"
        );
        drop(stream);
        followup_bounded(
            "seed retry guard explicit cleanup",
            original.dispose("cleanup".into()),
        )
        .await
        .expect("#1022 seed retry guard explicit cleanup");
        followup_bounded("seed retry guard shutdown", owner.shutdown())
            .await
            .expect_err("#1022 seed retry guard shutdown retains original error");
        h.finish("drain-seed-1022").await;
        return;
    }
    assert_eq!(
        old.state(),
        HandleState::Draining,
        "#1022 seed predecessor draining before stream drop"
    );
    assert!(
        lock(&saved)
            .read_file_state
            .as_ref()
            .is_some_and(|s| s.get("during.txt").is_none()),
        "#1022 seed saved state remains pre-turn until drain"
    );
    if cancel_waiter {
        creating.abort();
    }
    let creating = if retry == Some("timeout") {
        let error = followup_bounded("seed retry timeout", creating)
            .await
            .expect("#1022 seed retry timeout join")
            .err()
            .expect("#1022 seed retry timeout rejects");
        assert_eq!(
            error.js_error().and_then(|error| error.code.as_deref()),
            Some("SESSION_WRITER_BUSY"),
            "#1022 seed retry timeout error"
        );
        assert_eq!(
            lock(&h.observed.bindings).len(),
            1,
            "#1022 seed retry never publishes while predecessor active"
        );
        None
    } else {
        Some(creating)
    };
    drop(stream);
    assert!(
        lock(&saved)
            .read_file_state
            .as_ref()
            .is_some_and(|s| s.get("during.txt").is_some()),
        "#1022 seed drop publishes final checkpoint"
    );
    if let Some(owner) = &retry_owner {
        if let Some(creating) = creating {
            let error = followup_bounded("seed retry first error", creating)
                .await
                .expect("#1022 seed retry first join")
                .err()
                .expect("#1022 seed retry first failure propagated");
            if retry == Some("dispose") {
                assert_eq!(
                    error
                        .causes
                        .first()
                        .expect("#1022 seed retry dispose cause")
                        .causes
                        .first()
                        .expect("#1022 seed retry nested dispose cause")
                        .message,
                    oracle_document()["followups"][2]["error"]["nestedErrors"][0],
                    "#1022 seed retry original dispose error retained"
                );
            } else {
                assert_eq!(
                    error.message, "#1022 final seed unavailable",
                    "#1022 seed retry original read error retained"
                );
            }
        }
        // Owned disposal must finish before retry; a failed waiter still leaves
        // the completed source in the owner's dirty record.
        let completed =
            followup_bounded("seed retry old completion", old.dispose("finish".into())).await;
        assert_eq!(
            completed.is_err(),
            retry == Some("dispose"),
            "#1022 seed retry shared disposal result"
        );
        assert_eq!(
            h.leases(),
            0,
            "#1022 seed retry failed preparation released"
        );
        assert_eq!(
            lock(&h.observed.bindings).len(),
            1,
            "#1022 seed retry first error no publication"
        );
        let new = followup_bounded("seed retry create", owner.get_or_create(h.context()))
            .await
            .expect("#1022 seed retry succeeds after completed drain");
        let seed = new
            .reload_snapshot()
            .expect("#1022 seed retry snapshot")
            .file_state;
        assert_eq!(
            seed_json(&seed)["readFileState"],
            oracle_document()["followups"][0]["drained"]["readFileState"],
            "#1022 seed retry retains drained read state"
        );
        assert!(
            lock(
                &lock(&h.observed.bindings)
                    .last()
                    .expect("#1022 seed retry binding")
                    .checkpoint
                    .read_file_state
            )
            .get("during.txt")
            .is_some(),
            "#1022 seed retry checkpoint retains edit authority"
        );
        assert!(
            !new.same_identity(&old),
            "#1022 seed retry publishes new identity"
        );
        assert!(
            owner
                .current_handle(&h.context().session_key)
                .expect("#1022 seed retry current handle")
                .same_identity(&new),
            "#1022 seed retry returns published identity"
        );
        assert_eq!(
            lock(&h.observed.events)
                .iter()
                .filter(|event| *event == "close.1")
                .count(),
            1,
            "#1022 seed retry old storage closes once"
        );
        followup_bounded("seed retry shutdown", owner.shutdown())
            .await
            .expect("#1022 seed retry shutdown succeeds");
    } else if cancel_waiter {
        let creating = creating.expect("#1022 seed cancellation task");
        assert!(
            creating
                .await
                .err()
                .expect("#1022 seed cancelled waiter")
                .is_cancelled(),
            "#1022 seed cancellation is expected"
        );
        followup_bounded("seed cancelled cleanup", async {
            while h.leases() != 0
                || h.observed.mcp.load(Ordering::SeqCst) != 0
                || !lock(&h.observed.events).iter().any(|v| v == "close.2")
            {
                tokio::task::yield_now().await;
            }
        })
        .await;
    } else if fail_final_seed {
        let creating = creating.expect("#1022 seed failure task");
        let error = followup_bounded("seed failure", creating)
            .await
            .expect("#1022 seed failure join")
            .err()
            .expect("#1022 seed failure propagated");
        assert_eq!(
            error.message, "#1022 final seed unavailable",
            "#1022 seed failure preserves error"
        );
        assert_eq!(
            h.leases(),
            0,
            "#1022 seed failure releases prepared project lease"
        );
        assert_eq!(
            h.observed.mcp.load(Ordering::SeqCst),
            0,
            "#1022 seed failure releases prepared MCP"
        );
        assert_eq!(
            lock(&h.observed.bindings).len(),
            1,
            "#1022 seed failure creates no replacement storage"
        );
    } else {
        let creating = creating.expect("#1022 seed recreation task");
        let new = followup_bounded("seed recreate", creating)
            .await
            .expect("#1022 seed create join")
            .expect("#1022 seed create result");
        let seed = new
            .reload_snapshot()
            .expect("#1022 seed replacement snapshot")
            .file_state;
        assert_eq!(
            seed_json(&seed)["readFileState"],
            oracle_document()["followups"][0]["drained"]["readFileState"],
            "#1022 seed replacement retains drained read state"
        );
        let binding = lock(&h.observed.bindings)
            .get(1)
            .expect("#1022 seed replacement binding")
            .checkpoint
            .clone();
        assert!(
            lock(&binding.read_file_state).get("during.txt").is_some(),
            "#1022 seed edit checkpoint retains read authority"
        );
        followup_bounded("seed replacement dispose", new.dispose("cleanup".into()))
            .await
            .expect("#1022 seed replacement cleanup");
    }
    assert!(
        lock(&h.observed.events).iter().any(|v| v == "close.1"),
        "#1022 seed old storage closed"
    );
    if cancel_waiter {
        assert!(
            lock(&h.observed.events).iter().any(|v| v == "close.2"),
            "#1022 seed cancelled candidate storage closed"
        );
    }
    h.finish("drain-seed-1022").await;
}

#[tokio::test]
async fn recreation_uses_file_state_saved_during_drain() {
    drained_seed_case(false, false, None).await;
}
#[tokio::test]
async fn cancelled_recreation_finishes_drain_and_candidate_cleanup() {
    drained_seed_case(true, false, None).await;
}

#[test]
#[ignore = "requires fixed TS baseline and Node v22.22.0"]
fn followups_regenerates_identically() {
    crate::test_baseline::regenerates_identically("session-factory");
}

#[test]
#[ignore = "requires fixed TS baseline and Node v22.22.0"]
fn dispose_failure_followups_regenerates_identically() {
    crate::test_baseline::regenerates_identically("session-factory");
}

#[tokio::test]
async fn failed_drained_seed_releases_prepared_resources() {
    drained_seed_case(false, true, None).await;
}

#[tokio::test]
async fn recreation_retry_after_dispose_error_retains_drained_seed() {
    drained_seed_case(false, false, Some("dispose")).await;
}

#[tokio::test]
async fn recreation_retry_after_timeout_retains_drained_seed() {
    drained_seed_case(false, false, Some("timeout")).await;
}

#[tokio::test]
async fn recreation_retry_after_seed_error_retains_drained_seed() {
    drained_seed_case(false, true, Some("snapshot")).await;
}

#[tokio::test]
async fn recreation_retry_rejects_disposed_predecessor_with_active_turn() {
    drained_seed_case(false, false, Some("guard-panic")).await;
}

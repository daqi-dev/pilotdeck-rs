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
    setups: AtomicUsize,
    setups_with_previous: AtomicUsize,
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
    handoff: Arc<Gate>,
}
struct PublishGate {
    factory: Arc<ProjectSessionFactory>,
    gate: Option<Arc<Gate>>,
    handoff: Option<Arc<Gate>>,
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
        let creating = self.factory.create(context, previous.clone());
        let observed = self.observed.clone();
        // #1020: park the owner's recreate attempt after it selected previous
        // but before the real factory takes the handle over. `creating` is an
        // unpolled boxed future, so the real construct has not started yet.
        let handoff = (previous.is_some() && self.handoff.is_some())
            .then(|| self.handoff.clone())
            .flatten();
        Box::pin(async move {
            if let Some(handoff) = handoff {
                handoff.wait().await;
            }
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
        let observed = self.observed.clone();
        Box::pin(async move {
            // #1020 fix-r1: record the setup(previous) contract — how many
            // attempts reached setup and whether a previous handle was handed
            // over — so the stale-retry tests can pin it. fix-r2 (review P2):
            // the event carries the attempt ordinal, so an ordering assertion
            // can target one attempt's setup instead of whichever was recorded
            // first (the initial create also records a setup.fresh).
            let ordinal = observed.setups.fetch_add(1, Ordering::SeqCst) + 1;
            let with_previous = previous.is_some();
            if with_previous {
                observed.setups_with_previous.fetch_add(1, Ordering::SeqCst);
            }
            observed.event(if with_previous {
                format!("setup.previous.{ordinal}")
            } else {
                format!("setup.fresh.{ordinal}")
            });
            if with_previous && let Some(gate) = gate {
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
        let handoff = Gate::new();
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
                let release_gate = drain.clone();
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
                        if name == "drop-release-stage" && id == 1 {
                            release_gate.wait().await;
                        }
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
                Ok(SessionStorage::Unlocked(storage))
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
            handoff: (name == "stale-handoff").then(|| handoff.clone()),
            observed: observed.clone(),
        });
        let mut options = SessionOwnerOptions::new(owner_factory);
        options.idle_sweep_interval_ms = 0;
        let publish_observed = observed.clone();
        options.on_evict = Some(Arc::new(move |_, reason| {
            if reason == super::super::EvictionReason::DirtyRecreate {
                // #1020 fix-r1: an event next to the counter lets the retry
                // test order the publication after a successful setup.
                publish_observed.event("evict.dirty-recreate");
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
            handoff,
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
fn stale_case<'a>(cases: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
    cases
        .iter()
        .find(|row| row["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("#1020 stale handoff case {name} missing from oracle"))
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
#[tokio::test]
async fn reload_capability_is_not_fake_success() {
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
    let projection = Arc::new(SessionProjection::default());
    let inner = Arc::new(JsonlTranscriptStore::new());
    let cache = Arc::new(ProjectionCache::new(
        Arc::new(FileProjectionCheckpointStore(root.join("cache.json"))),
        inner.clone(),
        key.clone(),
        projection.clone(),
    ));
    let store = ProjectedStore {
        _writer_lease: Arc::new(Mutex::new(None)),
        inner,
        key: key.clone(),
        projection,
        cache,
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
    projection.valid.store(false, Ordering::SeqCst);
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
                Ok(SessionStorage::Unlocked(FactoryStorage::new(
                    Arc::new(JsonlTranscriptStore::new()),
                    TranscriptKey {
                        project_root: root.clone(),
                        pilot_home: root.clone(),
                        session_id: context.session_key,
                        transcript_path: root.join("session-745.jsonl"),
                    },
                )))
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

/// #1019: a factory whose storage is a real `PersistenceResources::create_session`
/// recording adopted through the fence transfer. `opens` counts opener runs,
/// so exclusion tests can prove a rejected factory never opened the recording.
fn adopt_factory(
    h: &Harness,
    opens: Arc<AtomicUsize>,
    transcript_path: PathBuf,
    memory: bool,
    admission: Option<Admission>,
) -> Arc<ProjectSessionFactory> {
    let persistence = Arc::new(
        crate::runtime::project::persistence::PersistenceResources::for_tests(
            h.root.clone(),
            h.root.join("home"),
        ),
    );
    let session_id: JsString = "session-745".into();
    Arc::new(ProjectSessionFactory::new(FactoryOptions {
        writer_busy_wait_timeout: h.factory.options.writer_busy_wait_timeout,
        prepare: h.factory.options.prepare.clone(),
        create_storage: Arc::new(move |_context| {
            let persistence = persistence.clone();
            let session_id = session_id.clone();
            let open_path = if memory {
                None
            } else {
                Some(transcript_path.clone())
            };
            let plan_path = transcript_path.clone();
            let opens = opens.clone();
            Box::pin(async move {
                let open: OpenRecording = Arc::new(move || {
                    opens.fetch_add(1, Ordering::SeqCst);
                    persistence.create_session(session_id.clone(), open_path.clone())
                });
                Ok(SessionStorage::Recording(RecordingPlan {
                    transcript_path: plan_path,
                    open,
                }))
            })
        }),
        admission,
        diagnostic: Arc::new(|_| {}),
    }))
}

#[tokio::test]
async fn writer_adopt_creates_recreates_closes_and_reopens() {
    let h = Harness::new("adopt-lifecycle");
    let opens = Arc::new(AtomicUsize::new(0));
    let path = h.root.join("session-745.jsonl");
    let factory = adopt_factory(&h, opens.clone(), path.clone(), false, None);
    let first = bounded("adopt first create", factory.create(h.context(), None))
        .await
        .expect("#1019 adopted first create is not busy");
    assert_eq!(
        opens.load(Ordering::SeqCst),
        1,
        "#1019 adopted first create opens the recording once"
    );
    lock(&h.observed.bindings)[0]
        .recorder
        .writer()
        .record_session_metadata(
            &"session-745".into(),
            &"adopt".into(),
            js(r#"{"adopt":true}"#),
        )
        .expect("#1019 adopted writer appends through the transferred fence");
    // Recreate: the predecessor's close thread releases the flock before the
    // process mutex, so the deferred opener cannot observe the in-flight
    // release and the first recreation must not report busy. The close-thread
    // gate pins that thread at the exact window after the drain returned and
    // before it releases the lease, so the pinned assertions below are
    // deterministic instead of racing the spawned close.
    let canonical = crate::storage::canonical_transcript_path(&path)
        .expect("#1019 adopt lifecycle canonical path");
    let observation = writer_wait_observation(&writer_mutex(&canonical));
    let gate = install_close_thread_gate(&canonical);
    let recreate = tokio::spawn(factory.create(h.context(), Some(first.clone())));
    observation.store(0, Ordering::SeqCst);
    bounded("adopt recreate pinned window", async {
        loop {
            if first.state() == HandleState::Disposed && observation.load(Ordering::SeqCst) != 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    assert_eq!(
        opens.load(Ordering::SeqCst),
        1,
        "#1019 pinned close window keeps the opener waiting on the lease"
    );
    assert!(
        !recreate.is_finished(),
        "#1019 pinned close window keeps the recreate waiting"
    );
    drop(gate);
    let second = bounded("adopt recreate await", async {
        recreate.await.expect("#1019 adopt recreate task completes")
    })
    .await
    .expect("#1019 adopted recreate is not busy");
    assert_eq!(
        opens.load(Ordering::SeqCst),
        2,
        "#1019 adopted recreate opens the recording once more"
    );
    assert_eq!(
        second.state(),
        HandleState::Active,
        "#1019 recreated adopted handle is active"
    );
    lock(&h.observed.bindings)[1]
        .recorder
        .writer()
        .record_session_metadata(
            &"session-745".into(),
            &"adopt".into(),
            js(r#"{"recreated":true}"#),
        )
        .expect("#1019 recreated writer continues the sequence chain");
    second
        .dispose("adopt close".into())
        .await
        .expect("#1019 adopted session closes");
    let reopened = bounded("adopt reopen", factory.create(h.context(), None))
        .await
        .expect("#1019 adopted reopen after close is not busy");
    assert_eq!(
        opens.load(Ordering::SeqCst),
        3,
        "#1019 adopted reopen opens the recording once more"
    );
    lock(&h.observed.bindings)[2]
        .recorder
        .writer()
        .record_session_metadata(
            &"session-745".into(),
            &"adopt".into(),
            js(r#"{"reopened":true}"#),
        )
        .expect("#1019 reopened writer continues the sequence chain");
    reopened
        .dispose("adopt reopen cleanup".into())
        .await
        .expect("#1019 adopted reopen cleanup");
    let transcript = std::fs::read_to_string(&path).expect("#1019 adopted transcript written");
    let entries: Vec<_> = transcript.lines().map(js).collect();
    assert_eq!(
        entries.len(),
        3,
        "#1019 continuation writes land exactly three entries"
    );
    for (index, entry) in entries.iter().enumerate() {
        assert_eq!(
            entry.get("sequence").and_then(JsValue::as_number),
            Some((index + 1) as f64),
            "#1019 continuation writes keep one sequence chain"
        );
        let parent = entry.get("parentEntryId");
        if index == 0 {
            assert_eq!(
                parent,
                Some(&JsValue::Null),
                "#1019 first continuation entry has no parent"
            );
        } else {
            assert_eq!(
                parent,
                entries[index - 1].get("entryId"),
                "#1019 continuation writes chain parentEntryId"
            );
        }
    }
    factory
        .dispose()
        .await
        .expect("#1019 adopt lifecycle factory disposal");
    h.finish("adopt-lifecycle").await;
}

/// #1019 P0: the fence handoff retires the recording's writer. A stale holder
/// that retained an Arc clone of the pre-handoff writer cannot append after
/// the handoff — a typed rejection, not a silent fork of the sequence chain
/// under the factory's replacement writer — and the chain stays unbroken.
#[tokio::test]
async fn writer_adopt_retires_retained_opener_writer_clones() {
    let h = Harness::new("adopt-retired-clone");
    let opens = Arc::new(AtomicUsize::new(0));
    let path = h.root.join("session-745.jsonl");
    let retained: Arc<Mutex<Option<Arc<crate::storage::TranscriptWriter>>>> =
        Arc::new(Mutex::new(None));
    let stash = retained.clone();
    let persistence = Arc::new(
        crate::runtime::project::persistence::PersistenceResources::for_tests(
            h.root.clone(),
            h.root.join("home"),
        ),
    );
    let session_id: JsString = "session-745".into();
    let factory_path = path.clone();
    let factory_opens = opens.clone();
    let factory = Arc::new(ProjectSessionFactory::new(FactoryOptions {
        writer_busy_wait_timeout: h.factory.options.writer_busy_wait_timeout,
        prepare: h.factory.options.prepare.clone(),
        create_storage: Arc::new(move |_context| {
            let persistence = persistence.clone();
            let session_id = session_id.clone();
            let open_path = factory_path.clone();
            let plan_path = factory_path.clone();
            let opens = factory_opens.clone();
            let stash = stash.clone();
            Box::pin(async move {
                let open: OpenRecording = Arc::new(move || {
                    opens.fetch_add(1, Ordering::SeqCst);
                    let recording =
                        persistence.create_session(session_id.clone(), Some(open_path.clone()))?;
                    // One pre-handoff write through this writer, then retain a
                    // clone: the stale holder the factory cannot see or drop.
                    recording.recorder.writer().record_session_metadata(
                        &session_id,
                        &"adopt".into(),
                        js(r#"{"probe":"pre-handoff"}"#),
                    )?;
                    *lock(&stash) = Some(recording.recorder.writer().clone());
                    Ok(recording)
                });
                Ok(SessionStorage::Recording(RecordingPlan {
                    transcript_path: plan_path,
                    open,
                }))
            })
        }),
        admission: None,
        diagnostic: Arc::new(|_| {}),
    }));
    let handle = bounded("adopt retire create", factory.create(h.context(), None))
        .await
        .expect("#1019 adopted create succeeds while retiring the opener writer");
    assert_eq!(
        opens.load(Ordering::SeqCst),
        1,
        "#1019 retire clone opens the recording once"
    );
    let replacement = lock(&h.observed.bindings)[0].recorder.writer().clone();
    replacement
        .record_session_metadata(
            &"session-745".into(),
            &"adopt".into(),
            js(r#"{"probe":"replacement"}"#),
        )
        .expect("#1019 replacement writer appends after the handoff");
    let stale = lock(&retained)
        .clone()
        .expect("#1019 opener retained a writer clone");
    let rejected = |stale: &Arc<crate::storage::TranscriptWriter>| {
        stale.record_session_metadata(
            &"session-745".into(),
            &"adopt".into(),
            js(r#"{"probe":"stale-clone"}"#),
        )
    };
    match rejected(&stale) {
        Err(crate::storage::AdapterError::Retired(message)) => assert!(
            message.contains("handed its ledger fence"),
            "#1019 retained opener writer clone cannot append after the handoff: {message}"
        ),
        Err(error) => {
            panic!("#1019 retained opener writer clone cannot append after the handoff: {error:?}")
        }
        Ok(()) => panic!("#1019 retained opener writer clone cannot append after the handoff"),
    }
    handle
        .dispose("adopt retire close".into())
        .await
        .expect("#1019 retired clone session closes");
    // Closing the session does not resurrect the retired clone.
    match rejected(&stale) {
        Err(crate::storage::AdapterError::Retired(_)) => {}
        Err(error) => panic!("#1019 retired clone stays rejected after the close: {error:?}"),
        Ok(()) => panic!("#1019 retired clone stays rejected after the close"),
    }
    // The chain is unbroken: exactly the pre-handoff and replacement entries.
    let transcript =
        std::fs::read_to_string(&path).expect("#1019 retired clone transcript written");
    let entries: Vec<_> = transcript.lines().map(js).collect();
    assert_eq!(entries.len(), 2, "#1019 the stale clone never appended");
    assert_eq!(
        entries[0].get("sequence").and_then(JsValue::as_number),
        Some(1.0),
        "#1019 the pre-handoff append keeps sequence 1"
    );
    assert_eq!(
        entries[1].get("sequence").and_then(JsValue::as_number),
        Some(2.0),
        "#1019 the replacement continues the sequence chain"
    );
    assert_eq!(
        entries[1].get("parentEntryId"),
        entries[0].get("entryId"),
        "#1019 the replacement chains to the pre-handoff entry"
    );
    let reopened = bounded("adopt retire reopen", factory.create(h.context(), None))
        .await
        .expect("#1019 the retired-clone transcript reopens");
    reopened
        .dispose("adopt retire reopen cleanup".into())
        .await
        .expect("#1019 retired clone reopen cleanup");
    factory
        .dispose()
        .await
        .expect("#1019 adopt retire factory disposal");
    h.finish("adopt-retired-clone").await;
}

/// #1019 P1: the recording branch's writer-wait budget restarts after the
/// drained snapshot. A predecessor whose drained snapshot runs long must not
/// expire the adopt path's mutex-wait deadline into the expired try-once.
#[tokio::test]
async fn writer_adopt_budget_excludes_slow_drained_snapshot() {
    let h = Harness::with_fence_timeout("adopt-slow-plan", Duration::from_secs(5));
    let (old, _, _) = retry_fault(&h, "slow-snapshot").await;
    let opens = Arc::new(AtomicUsize::new(0));
    let path = h.root.join("adopt-slow.jsonl");
    let persistence = Arc::new(
        crate::runtime::project::persistence::PersistenceResources::for_tests(
            h.root.clone(),
            h.root.join("home"),
        ),
    );
    let session_id: JsString = "session-745".into();
    let factory_path = path.clone();
    let factory_opens = opens.clone();
    let factory = Arc::new(ProjectSessionFactory::new(FactoryOptions {
        writer_busy_wait_timeout: Duration::from_secs(5),
        prepare: h.factory.options.prepare.clone(),
        create_storage: Arc::new(move |_context| {
            let persistence = persistence.clone();
            let session_id = session_id.clone();
            let open_path = factory_path.clone();
            let plan_path = factory_path.clone();
            let opens = factory_opens.clone();
            Box::pin(async move {
                let open: OpenRecording = Arc::new(move || {
                    opens.fetch_add(1, Ordering::SeqCst);
                    persistence.create_session(session_id.clone(), Some(open_path.clone()))
                });
                Ok(SessionStorage::Recording(RecordingPlan {
                    transcript_path: plan_path,
                    open,
                }))
            })
        }),
        admission: None,
        diagnostic: Arc::new(|_| {}),
    }));
    let canonical = crate::storage::canonical_transcript_path(&path)
        .expect("#1019 adopt slow plan canonical path");
    let mutex = writer_mutex(&canonical);
    let observation = writer_wait_observation(&mutex);
    let guard = mutex.lock_owned().await;
    tokio::time::pause();
    let waiting = tokio::spawn(factory.create(h.context(), Some(old)));
    // Keep the test runnable while the drain's blocking filesystem work
    // completes, so the paused clock cannot auto-advance to any timeout.
    let wait_state = followup_bounded("adopt slow plan lock entered", async {
        loop {
            let state = observation.load(Ordering::SeqCst);
            if state != 0 {
                break state;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(
        wait_state, 1,
        "#1019 slow drained snapshot retains the adopt path's busy wait budget"
    );
    assert!(
        !waiting.is_finished(),
        "#1019 slow drained snapshot still waits for the occupied writer"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(
        !waiting.is_finished(),
        "#1019 the re-based adopt deadline still waits past the slow snapshot"
    );
    drop(guard);
    tokio::time::resume();
    let new = followup_bounded("adopt slow plan retry", waiting)
        .await
        .expect("#1019 adopt slow plan task completes")
        .expect("#1019 slow drained snapshot retains the adopt path's busy wait budget");
    new.dispose("adopt slow plan cleanup".into())
        .await
        .expect("#1019 adopt slow plan cleanup");
    assert_eq!(
        opens.load(Ordering::SeqCst),
        1,
        "#1019 slow plan opened the recording once"
    );
    h.finish("adopt-slow-plan").await;
}

#[tokio::test]
async fn writer_adopt_excludes_second_factory_and_alias() {
    let h = Harness::new("adopt-exclusion");
    let opens = Arc::new(AtomicUsize::new(0));
    let path = h.root.join("session-745.jsonl");
    let factory = adopt_factory(&h, opens.clone(), path.clone(), false, None);
    let first = bounded("adopt exclusion create", factory.create(h.context(), None))
        .await
        .expect("#1019 adopted exclusion baseline");
    std::fs::create_dir(h.root.join("nested")).expect("#1019 adopt alias directory");
    for label in ["direct", "alias"] {
        let alias_opens = Arc::new(AtomicUsize::new(0));
        let plan_path = if label == "direct" {
            path.clone()
        } else {
            h.root.join("nested/../session-745.jsonl")
        };
        let second = adopt_factory(&h, alias_opens.clone(), plan_path, false, None);
        let error = bounded(label, second.create(h.context(), None))
            .await
            .err()
            .expect("#1019 adopted lease excludes a second factory");
        assert_eq!(
            error.js_error().and_then(|e| e.code.as_deref()),
            Some("SESSION_WRITER_BUSY"),
            "#1019 second factory is the typed busy error"
        );
        assert_eq!(
            alias_opens.load(Ordering::SeqCst),
            0,
            "#1019 rejected second factory never opens the recording"
        );
        second
            .dispose()
            .await
            .unwrap_or_else(|e| panic!("#1019 rejected second factory {label} cleanup: {e}"));
    }
    first
        .dispose("adopt exclusion cleanup".into())
        .await
        .expect("#1019 adopted exclusion baseline cleanup");
    let after = bounded("adopt exclusion reopen", factory.create(h.context(), None))
        .await
        .expect("#1019 adopted lease is free after close");
    after
        .dispose("adopt exclusion reopen cleanup".into())
        .await
        .expect("#1019 adopted reopen cleanup");
    factory
        .dispose()
        .await
        .expect("#1019 adopt exclusion factory disposal");
    h.finish("adopt-exclusion").await;
}

#[tokio::test]
async fn writer_adopt_fence_excludes_unlocked_writer() {
    let h = Harness::new("adopt-unlocked-exclusion");
    let opens = Arc::new(AtomicUsize::new(0));
    let factory = adopt_factory(
        &h,
        opens.clone(),
        h.root.join("session-745.jsonl"),
        false,
        None,
    );
    let adopted = bounded(
        "adopt unlocked exclusion create",
        factory.create(h.context(), None),
    )
    .await
    .expect("#1019 adopted unlocked exclusion baseline");
    // The transferred fence must still exclude the factory's own unlocked path.
    let error = bounded(
        "unlocked second create",
        h.factory.create(h.context(), None),
    )
    .await
    .err()
    .expect("#1019 adopted fence excludes an unlocked second writer");
    assert_eq!(
        error.js_error().and_then(|e| e.code.as_deref()),
        Some("SESSION_WRITER_BUSY"),
        "#1019 unlocked second writer is the typed busy error"
    );
    adopted
        .dispose("adopt unlocked exclusion cleanup".into())
        .await
        .expect("#1019 adopted baseline cleanup");
    // The reverse direction: an unlocked live writer excludes the adopter on
    // the shared process mutex, before its opener ever runs.
    let unlocked = bounded("unlocked recreate", h.factory.create(h.context(), None))
        .await
        .expect("#1019 unlocked recreate after adopted close");
    let reverse = bounded("adopt reverse create", factory.create(h.context(), None))
        .await
        .err()
        .expect("#1019 live unlocked writer rejects the adopted opener");
    assert_eq!(
        reverse.js_error().and_then(|e| e.code.as_deref()),
        Some("SESSION_WRITER_BUSY"),
        "#1019 rejected adopter is the typed busy error"
    );
    assert_eq!(
        opens.load(Ordering::SeqCst),
        1,
        "#1019 excluded adopter never opens against the live unlocked writer"
    );
    unlocked
        .dispose("unlocked cleanup".into())
        .await
        .expect("#1019 unlocked cleanup");
    let resumed = bounded("adopt resumed create", factory.create(h.context(), None))
        .await
        .expect("#1019 adopted create succeeds after the unlocked writer closes");
    resumed
        .dispose("adopt resumed cleanup".into())
        .await
        .expect("#1019 adopted resumed cleanup");
    factory
        .dispose()
        .await
        .expect("#1019 adopt unlocked exclusion factory disposal");
    h.finish("adopt-unlocked-exclusion").await;
}

#[tokio::test]
async fn writer_adopt_busy_is_typed_session_writer_busy() {
    let h = Harness::new("adopt-external-lock");
    let opens = Arc::new(AtomicUsize::new(0));
    let path = h.root.join("session-745.jsonl");
    let factory = adopt_factory(&h, opens.clone(), path.clone(), false, None);
    let (_, external) =
        crate::storage::LedgerLock::acquire(&path).expect("#1019 external ledger holder setup");
    let error = bounded(
        "adopt external lock create",
        factory.create(h.context(), None),
    )
    .await
    .err()
    .expect("#1019 external fence rejects the adopted open");
    assert_eq!(
        error.js_error().and_then(|e| e.code.as_deref()),
        Some("SESSION_WRITER_BUSY"),
        "#1019 opener lock contention is the typed busy error"
    );
    assert_eq!(
        opens.load(Ordering::SeqCst),
        1,
        "#1019 opener ran once under the external lock"
    );
    drop(external);
    let handle = bounded(
        "adopt external lock retry",
        factory.create(h.context(), None),
    )
    .await
    .expect("#1019 adopted create succeeds after the external holder leaves");
    handle
        .dispose("adopt external lock cleanup".into())
        .await
        .expect("#1019 adopted external lock cleanup");
    factory
        .dispose()
        .await
        .expect("#1019 adopt external lock factory disposal");
    h.finish("adopt-external-lock").await;
}

#[tokio::test]
async fn writer_adopt_rejects_unfenced_recording() {
    let h = Harness::new("adopt-memory");
    let opens = Arc::new(AtomicUsize::new(0));
    let factory = adopt_factory(
        &h,
        opens.clone(),
        h.root.join("memory-plan.jsonl"),
        true,
        None,
    );
    let error = bounded("adopt memory create", factory.create(h.context(), None))
        .await
        .err()
        .expect("#1019 unfenced recording must not join a session");
    assert!(
        error.message.contains("no ledger fence"),
        "#1019 unfenced recording rejection message: {error}"
    );
    assert_eq!(
        opens.load(Ordering::SeqCst),
        1,
        "#1019 memory opener ran once"
    );
    assert_eq!(
        h.leases(),
        0,
        "#1019 unfenced rejection releases prepared resources"
    );
    factory
        .dispose()
        .await
        .expect("#1019 adopt memory factory disposal");
    h.finish("adopt-memory").await;
}

#[tokio::test]
async fn writer_adopt_opener_failure_releases_prepared() {
    let h = Harness::new("adopt-opener-failure");
    let opens = Arc::new(AtomicUsize::new(0));
    let path = h.root.join("eisdir.jsonl");
    std::fs::create_dir(&path).expect("#1019 opener failure path setup");
    let factory = adopt_factory(&h, opens.clone(), path.clone(), false, None);
    let error = bounded(
        "adopt opener failure create",
        factory.create(h.context(), None),
    )
    .await
    .err()
    .expect("#1019 opener failure rejects the session");
    // Node v22.22.0 on this read: code EISDIR, message without the path
    // (eisdir-probe.mjs). The opener's non-BUSY storage failures keep that
    // exact contract instead of a Rust io error rendering.
    assert_eq!(
        error.message, "EISDIR: illegal operation on a directory, read",
        "#1019 opener failure keeps the node storage error message"
    );
    assert_eq!(
        error.js_error().and_then(|e| e.code.as_deref()),
        Some("EISDIR"),
        "#1019 opener failure keeps the node storage error code"
    );
    assert_eq!(
        opens.load(Ordering::SeqCst),
        1,
        "#1019 opener failure ran the opener once"
    );
    assert_eq!(
        h.leases(),
        0,
        "#1019 opener failure releases prepared resources"
    );
    std::fs::remove_dir(&path).expect("#1019 opener failure cleanup path");
    let handle = bounded(
        "adopt opener failure retry",
        factory.create(h.context(), None),
    )
    .await
    .expect("#1019 adopted create succeeds after the opener failure is cleared");
    handle
        .dispose("adopt opener failure cleanup".into())
        .await
        .expect("#1019 adopted opener failure cleanup");
    factory
        .dispose()
        .await
        .expect("#1019 adopt opener failure factory disposal");
    h.finish("adopt-opener-failure").await;
}

#[tokio::test]
async fn writer_adopt_failure_rolls_back_and_reopens() {
    let h = Harness::new("adopt-rollback");
    let opens = Arc::new(AtomicUsize::new(0));
    let admission: Admission = Arc::new(|_: SessionContext| {
        Box::pin(async { Err(RuntimeError::new("ADOPT_ADMISSION_BOOM")) })
    });
    let factory = adopt_factory(
        &h,
        opens.clone(),
        h.root.join("session-745.jsonl"),
        false,
        Some(admission),
    );
    let error = bounded("adopt rollback create", factory.create(h.context(), None))
        .await
        .err()
        .expect("#1019 post-adoption failure rejects the session");
    assert_eq!(
        error.message, "ADOPT_ADMISSION_BOOM",
        "#1019 admission failure surfaces after the fence transfer"
    );
    assert_eq!(
        opens.load(Ordering::SeqCst),
        1,
        "#1019 rollback opened the recording once"
    );
    assert_eq!(h.leases(), 0, "#1019 rollback releases prepared resources");
    factory
        .dispose()
        .await
        .expect("#1019 adopt rollback factory disposal");
    // The rollback's close thread releases the fence before the process mutex,
    // so the next opener deterministically observes a free lock.
    let retry = adopt_factory(
        &h,
        opens.clone(),
        h.root.join("session-745.jsonl"),
        false,
        None,
    );
    let handle = bounded("adopt rollback reopen", retry.create(h.context(), None))
        .await
        .expect("#1019 rollback released the fence for the next create");
    handle
        .dispose("adopt rollback reopen cleanup".into())
        .await
        .expect("#1019 adopted rollback reopen cleanup");
    retry
        .dispose()
        .await
        .expect("#1019 adopt rollback retry factory disposal");
    h.finish("adopt-rollback").await;
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

#[test]
#[ignore = "requires the pinned Node v22.22.0 TS baseline"]
fn stale_handoff_regenerates_identically() {
    crate::test_baseline::regenerates_identically("session-stale-handoff");
}

#[test]
#[ignore = "requires the pinned Node v22.22.0 TS baseline"]
fn stale_handoff_preserves_unrelated_out_files() {
    // #1020 fix-r2 (review P0): the generator must never delete pre-existing
    // OUT content. The helper seeds OUT (including a work-notes/ directory,
    // which the fix-r1 prefix cleanup deleted), runs the real generator and
    // returns every seeded path that is missing or whose bytes changed.
    let changed =
        crate::test_baseline::unrelated_out_changes_after_regeneration("session-stale-handoff");
    assert_eq!(
        changed,
        Vec::<String>::new(),
        "#1020 session-stale-handoff: generator modified unrelated OUT content"
    );
}

#[test]
fn stale_handoff_matches_ts() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/session-stale-handoff/oracle.json"
    ))
    .expect("#1020 stale handoff fixture");
    let cases = oracle["cases"]
        .as_array()
        .expect("#1020 stale handoff case rows");
    assert_eq!(cases.len(), 4, "#1020 stale handoff denominator");
    for row in cases {
        let name = row["name"].as_str().expect("#1020 case name");
        assert_eq!(row["node"], "v22.22.0", "#1020 {name} node oracle");
    }
    let disposed =
        serde_json::json!({"name":"Error","message":"Session projection driver is disposed."});
    // cell 1: the router keeps rejecting the dead session, with no recovery.
    let previous = &stale_case(cases, "router-disposed-previous")["observations"];
    assert_eq!(
        previous["firstState"], "active",
        "#1020 router-disposed-previous first state oracle"
    );
    assert_eq!(
        previous["registrySizeBefore"], 1,
        "#1020 router-disposed-previous registry before oracle"
    );
    assert_eq!(
        previous["afterDisposeState"], "disposed",
        "#1020 router-disposed-previous disposal completes"
    );
    assert_eq!(
        previous["dirty1"], true,
        "#1020 router-disposed-previous dirty"
    );
    assert_eq!(
        previous["secondError"], disposed,
        "#1020 router-disposed-previous second error oracle"
    );
    assert_eq!(
        previous["thirdError"], disposed,
        "#1020 router keeps rejecting without recovery"
    );
    assert_eq!(
        previous["registrySizeAfter"], 1,
        "#1020 router-disposed-previous registry after oracle"
    );
    // cell 1b: re-marking dirty after the rejection still hands the dead
    // session over and is rejected again.
    let second_dirty = &stale_case(cases, "router-disposed-second-dirty")["observations"];
    assert_eq!(
        second_dirty["dirty1"], true,
        "#1020 router-disposed-second-dirty dirty1 oracle"
    );
    assert_eq!(
        second_dirty["secondError"], disposed,
        "#1020 router-disposed-second-dirty second error oracle"
    );
    assert_eq!(
        second_dirty["dirty2"], true,
        "#1020 router-disposed-second-dirty dirty2 oracle"
    );
    assert_eq!(
        second_dirty["thirdError"], disposed,
        "#1020 router-disposed-second-dirty third error oracle"
    );
    // factory-level self-check cell.
    let direct = &stale_case(cases, "direct-recreate-disposed")["observations"];
    assert_eq!(
        direct["oldState"], "active",
        "#1020 direct-recreate-disposed old state oracle"
    );
    assert_eq!(
        direct["recreateError"], disposed,
        "#1020 direct-recreate-disposed factory rejection oracle"
    );
    // cell 3: the old disposal completes inside the replacement's prepare.
    let prepare = &stale_case(cases, "router-prepare-disposal")["observations"];
    assert_eq!(
        prepare["oldState"], "active",
        "#1020 router-prepare-disposal old state oracle"
    );
    assert_eq!(
        prepare["secondError"], disposed,
        "#1020 router-prepare-disposal second error oracle"
    );
    assert_eq!(
        prepare["disposalInsidePrepare"], "fulfilled",
        "#1020 router-prepare-disposal disposal inside prepare"
    );
}

#[tokio::test]
async fn recreate_retries_stale_disposed_handoff() {
    let h = Harness::new("stale-handoff");
    let old = h.create("stale-handoff").await;
    // Hold the old handle's drain so the disposal is parked mid-flight (state
    // Draining) while the owner selects it as previous.
    let activity = old.begin_activity().expect("#1020 stale activity hold");
    let disposal = old.dispose("prior".into());
    assert_eq!(old.state(), HandleState::Draining, "#1020 prior draining");
    assert!(
        h.owner
            .mark_session_dirty(&"session-745".into(), "stale-handoff"),
        "#1020 stale dirty mark"
    );
    let creating = h.owner.get_or_create(h.context());
    // The owner worker parks inside the factory wrapper after it selected the
    // draining old handle.
    bounded("stale handoff parked", h.handoff.entered()).await;
    // The same-key disposal completes after selection, before the factory
    // takes the handle over.
    drop(activity);
    disposal.await.expect("#1020 prior disposal completes");
    assert_eq!(
        old.state(),
        HandleState::Disposed,
        "#1020 prior fully disposed at handoff"
    );
    h.handoff.open();
    let new = bounded("stale recreate", creating)
        .await
        .expect("#1020 stale handoff retried");
    assert!(
        !new.same_identity(&old),
        "#1020 replacement identity after retry"
    );
    assert_eq!(
        old.state(),
        HandleState::Disposed,
        "#1020 old stays disposed"
    );
    let (attempt_released, retry_started, no_storage) = {
        let events = lock(&h.observed.events);
        (
            ordered(&events, "prepare.2", "release.2"),
            ordered(&events, "release.2", "prepare.3"),
            !events.iter().any(|e| e == "storage.2"),
        )
    };
    assert!(attempt_released, "#1020 failed attempt released prepared");
    assert!(retry_started, "#1020 retry after rollback");
    assert!(no_storage, "#1020 rejected attempt never created storage");
    assert_eq!(
        h.observed.creates.load(Ordering::SeqCst),
        3,
        "#1020 one failed attempt plus retry"
    );
    assert_eq!(
        h.observed.published.load(Ordering::SeqCst),
        1,
        "#1020 single dirty-recreate publication"
    );
    // #1020 fix-r1 (review P2): the failed attempt never reaches setup; the
    // successful retry sets up exactly once, without the dead previous, and
    // the publication happens only after that setup.
    assert_eq!(
        h.observed.setups.load(Ordering::SeqCst),
        2,
        "#1020 successful retry reaches setup exactly once"
    );
    assert_eq!(
        h.observed.setups_with_previous.load(Ordering::SeqCst),
        0,
        "#1020 stale retry sets up a fresh replacement"
    );
    {
        let events = lock(&h.observed.events);
        assert!(
            !events.iter().any(|e| e.starts_with("setup.previous")),
            "#1020 failed attempt never reaches setup"
        );
        assert!(
            ordered(&events, "setup.fresh.2", "evict.dirty-recreate"),
            "#1020 publication follows the successful retry's setup"
        );
    }
    // dirty was cleared by the single publication; the record now caches the
    // replacement without a further factory call.
    let cached = bounded("stale cached", h.owner.get_or_create(h.context()))
        .await
        .expect("#1020 cached after retry");
    assert!(
        cached.same_identity(&new),
        "#1020 cached identity after retry"
    );
    assert_eq!(
        h.observed.creates.load(Ordering::SeqCst),
        3,
        "#1020 dirty cleared after retry"
    );
    h.finish("stale-handoff").await;
}

#[tokio::test]
async fn recreate_after_completed_disposal_resumes_without_previous() {
    let h = Harness::new("stale-completed");
    let old = h.create("stale-completed").await;
    old.dispose("prior".into())
        .await
        .expect("#1020 prior disposal completes");
    assert_eq!(old.state(), HandleState::Disposed, "#1020 prior disposed");
    assert!(
        h.owner
            .mark_session_dirty(&"session-745".into(), "stale-completed"),
        "#1020 completed-disposal dirty mark"
    );
    let new = bounded(
        "completed disposal recreate",
        h.owner.get_or_create(h.context()),
    )
    .await
    .expect("#1020 completed disposal resumes without previous");
    assert!(
        !new.same_identity(&old),
        "#1020 completed disposal replacement identity"
    );
    assert_eq!(
        h.observed.creates.load(Ordering::SeqCst),
        2,
        "#1020 completed disposal one factory attempt"
    );
    assert_eq!(
        h.observed.published.load(Ordering::SeqCst),
        1,
        "#1020 completed disposal single publication"
    );
    // #1020 fix-r1: the disposed predecessor is filtered before the factory
    // call, so the replacement is set up fresh (no previous handed over).
    assert_eq!(
        h.observed.setups.load(Ordering::SeqCst),
        2,
        "#1020 completed disposal reaches setup once per create"
    );
    assert_eq!(
        h.observed.setups_with_previous.load(Ordering::SeqCst),
        0,
        "#1020 completed disposal sets up a fresh replacement"
    );
    // #1020 fix-r2 (review P2): the replacement's setup is the second
    // recorded fresh setup; it must complete before the dirty-recreate
    // publication, not merely before some earlier attempt's setup.
    {
        let events = lock(&h.observed.events);
        assert!(
            ordered(&events, "setup.fresh.2", "evict.dirty-recreate"),
            "#1020 completed disposal publishes after the replacement setup"
        );
    }
    let cached = bounded(
        "completed disposal cached",
        h.owner.get_or_create(h.context()),
    )
    .await
    .expect("#1020 completed disposal cached");
    assert!(
        cached.same_identity(&new),
        "#1020 completed disposal cached identity"
    );
    h.finish("stale-completed").await;
}

#[tokio::test]
async fn stale_handoff_retry_is_limited_to_one() {
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
    struct StaleFactory {
        initial: Mutex<Option<LiveSessionHandle>>,
        calls: Arc<AtomicUsize>,
        setups: Arc<AtomicUsize>,
    }
    impl SessionFactory for StaleFactory {
        fn can_recreate(&self) -> bool {
            true
        }
        fn create(
            &self,
            _: SessionContext,
            previous: Option<LiveSessionHandle>,
        ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
            if let Some(handle) = lock(&self.initial).take() {
                return Box::pin(async move { Ok(handle) });
            }
            let calls = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move {
                // Calls 1-2 mark the previous stale and reject; call 3
                // succeeds, so an unbounded retry loop still terminates
                // instead of hanging the mutant. The second rejection carries
                // a distinct message, code and causes so a flattened or
                // first-error passthrough cannot pass.
                if calls < 3 {
                    if let Some(previous) = previous {
                        previous.mark_recreation_stale_handoff();
                    }
                    if calls == 1 {
                        Err(RuntimeError::new("first stale rejection"))
                    } else {
                        Err(RuntimeError {
                            message: "second stale rejection".into(),
                            causes: vec![RuntimeError::new("second stale cause")],
                            js_error: Some(Box::new(crate::runtime::project::RuntimeJsError {
                                name: "Error".into(),
                                code: Some("ESECOND_STALE".into()),
                                message: "second stale rejection".into(),
                                requires_host: false,
                            })),
                        })
                    }
                } else {
                    Ok(LiveSessionHandle::new(Arc::new(LifecycleOnly), None))
                }
            })
        }
        fn setup(
            &self,
            _: SessionContext,
            _: LiveSessionHandle,
            _: Option<LiveSessionHandle>,
        ) -> BoxFuture<RuntimeResult> {
            let setups = self.setups.clone();
            Box::pin(async move {
                setups.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
    }
    let h = Harness::new("stale-limit");
    let initial = h
        .factory
        .create(h.context(), None)
        .await
        .expect("#1020 stale limit initial");
    let calls = Arc::new(AtomicUsize::new(0));
    let setups = Arc::new(AtomicUsize::new(0));
    let mut options = SessionOwnerOptions::new(Arc::new(StaleFactory {
        initial: Mutex::new(Some(initial)),
        calls: calls.clone(),
        setups: setups.clone(),
    }));
    options.idle_sweep_interval_ms = 0;
    let evictions = Arc::new(AtomicUsize::new(0));
    let evict_count = evictions.clone();
    options.on_evict = Some(Arc::new(move |_, reason| {
        if reason == super::super::EvictionReason::DirtyRecreate {
            evict_count.fetch_add(1, Ordering::SeqCst);
        }
    }));
    let owner = SessionOwner::new(options);
    owner
        .get_or_create(h.context())
        .await
        .expect("#1020 stale limit initial cache");
    assert!(
        owner.mark_session_dirty(&h.context().session_key, "stale-limit"),
        "#1020 stale limit dirty"
    );
    let error = bounded("stale limit", owner.get_or_create(h.context()))
        .await
        .err()
        .expect("#1020 stale handoff retry is limited to one");
    // #1020 fix-r1 (review P2): the second rejection is distinct from the
    // first, so the surfaced error must be the second one, byte for byte.
    assert_eq!(
        error.message, "second stale rejection",
        "#1020 stale limit surfaces the second factory error"
    );
    assert_eq!(
        error.js_error().and_then(|e| e.code.as_deref()),
        Some("ESECOND_STALE"),
        "#1020 stale limit surfaces the second factory error code"
    );
    let causes: Vec<&str> = error.causes.iter().map(|c| c.message.as_str()).collect();
    assert_eq!(
        causes,
        ["second stale cause"],
        "#1020 stale limit surfaces the second factory causes"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "#1020 stale handoff retry is limited to one"
    );
    // The initial cache fill is the only setup on this owner; both failed
    // attempts stay on the failure path with no setup and no publication.
    assert_eq!(
        setups.load(Ordering::SeqCst),
        1,
        "#1020 stale limit failure path never reaches setup"
    );
    assert_eq!(
        evictions.load(Ordering::SeqCst),
        0,
        "#1020 stale limit failure path never publishes"
    );
    owner.shutdown().await.expect("#1020 stale limit cleanup");
    h.finish("stale-limit").await;
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
    let mut disposal_failure = None;
    let mut creating = Box::pin(h.factory.construct(
        h.context(),
        Some(old.clone()),
        &mut disposal_failure,
    ));
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
    let mut storage = match (h.factory.options.create_storage)(h.context()).await {
        Ok(SessionStorage::Unlocked(storage)) => storage,
        Ok(SessionStorage::Recording(_)) => panic!("#745 rollback storage is unlocked"),
        Err(error) => panic!("#745 rollback storage: {error}"),
    };
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
                Ok(SessionStorage::Unlocked(FactoryStorage::new(
                    store,
                    TranscriptKey {
                        project_root: root.clone(),
                        pilot_home: root,
                        transcript_path: path,
                        session_id: context.session_key,
                    },
                )))
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
    let mut storage = match (h.factory.options.create_storage)(h.context()).await {
        Ok(SessionStorage::Unlocked(storage)) => storage,
        Ok(SessionStorage::Recording(_)) => panic!("#745 final hook storage is unlocked"),
        Err(error) => panic!("#745 final hook storage: {error}"),
    };
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
                tokio::time::sleep(Duration::from_millis(1)).await;
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

async fn pending_candidate_reason(
    reason: impl Fn() -> Option<JsString>,
    mut finished: tokio::sync::oneshot::Receiver<()>,
    wait: Duration,
) -> Option<JsString> {
    let observed = tokio::time::timeout(wait, async {
        loop {
            let current = reason();
            if current.is_some() {
                return current;
            }
            if !matches!(
                finished.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ) {
                // A completed/cancelled producer may have just set the reason.
                // Its exit alone does not prove that explicit disposal ran.
                return reason();
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    // Compile-time negative control: accepting this deadline must be caught by
    // the bounded-failure test, without mutating uncommitted product sources.
    if option_env!("PDRS_1122_ACCEPT_OBSERVATION_DEADLINE").is_some() {
        return observed.unwrap_or(None);
    }
    observed.expect("#1122 candidate observation executor settles")
}

#[tokio::test]
async fn pending_candidate_observer_missing_reason_fails_by_deadline() {
    let unrelated = tokio::spawn(std::future::pending::<()>());
    let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
    let started = std::time::Instant::now();
    let observing = tokio::spawn(async move {
        // Neither reason nor cleanup completion can occur before the deadline.
        let _never_finishes = sender;
        pending_candidate_reason(|| None, receiver, Duration::from_millis(20)).await
    });
    let observed = tokio::time::timeout(Duration::from_secs(2), observing)
        .await
        .expect("#1122 missing reason observer must terminate");
    unrelated.abort();
    assert!(
        unrelated
            .await
            .expect_err("#1122 unrelated pending task must be cancelled")
            .is_cancelled(),
        "#1122 unrelated pending task cancellation"
    );
    let error = observed.expect_err("#1122 missing reason must fail by observation deadline");
    assert!(error.is_panic(), "#1122 observation deadline must panic");
    let payload = error.into_panic();
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied());
    assert!(
        message.is_some_and(|text| text.contains("#1122 candidate observation executor settles")),
        "#1122 observation deadline must have its own failure message"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "#1122 missing reason failure must respect wall deadline"
    );
}

#[tokio::test]
async fn pending_candidate_observer_ignores_unrelated_tasks() {
    let unrelated = tokio::spawn(std::future::pending::<()>());
    for cancelled in [false, true] {
        let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
        let producer = tokio::spawn(async move {
            let _drop_finished = sender;
            if cancelled {
                std::future::pending::<()>().await;
            }
        });
        if cancelled {
            producer.abort();
            assert!(
                producer
                    .await
                    .expect_err("#1122 candidate observation producer cancelled")
                    .is_cancelled(),
                "#1122 candidate observation producer cancellation"
            );
        } else {
            producer
                .await
                .expect("#1122 candidate observation producer completed");
        }
        assert_eq!(
            pending_candidate_reason(|| None, receiver, Duration::from_secs(1)).await,
            None,
            "#1122 candidate completion preserves missing explicit reason"
        );
        assert!(
            !unrelated.is_finished(),
            "#1122 observer must settle while unrelated task remains alive"
        );
    }
    unrelated.abort();
    assert!(
        unrelated
            .await
            .expect_err("#1122 observer unrelated task must be cancelled")
            .is_cancelled(),
        "#1122 observer unrelated task cancellation"
    );
}

#[test]
fn cancelled_recreation_finishes_drain_and_candidate_cleanup() {
    // Exercise the current-thread executor too; observation is candidate-local.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("#1122 candidate observation runtime");
    runtime.block_on(async {
        drained_seed_case(true, false, None).await;

        // Keep the real cancelled-recreation end-to-end check above. This
        // second, handoff-only cell observes PendingSession's explicit reason
        // on a real factory candidate. Maintenance retains a handle clone and
        // exposes cancellation without running a model or permitting the last
        // handle Drop fallback to hide a missing explicit dispose.
        let h = Harness::new("pending-candidate-reason-1122");
        let candidate =
            tokio::time::timeout(Duration::from_secs(8), h.factory.create(h.context(), None))
                .await
                .expect("#1122 candidate construction finishes")
                .expect("#1122 candidate construction succeeds");
        let activity = candidate
            .begin_maintenance()
            .expect("#1122 candidate maintenance observation starts");
        let (finished, completion) = tokio::sync::oneshot::channel();
        let mut pending = PendingSession::new(candidate.clone());
        pending.drop_finished = Some(finished);
        drop(pending);
        let reason = pending_candidate_reason(
            || activity.cancellation_reason(),
            completion,
            Duration::from_secs(8),
        )
        .await;
        // An observation timeout is an invalid control, never a kill. Save the
        // actual reason, then clean up even when the mutation omitted disposal.
        drop(activity);
        tokio::time::timeout(
            Duration::from_secs(8),
            candidate.dispose("probe-cleanup".into()),
        )
        .await
        .expect("#1122 candidate cleanup finishes")
        .expect("#1122 candidate cleanup succeeds");
        h.finish("pending-candidate-reason-1122").await;
        let ts = oracle_document();
        let expected = ts["cancelledDisposal"]["maintenance"]["during"]["reason"]
            .as_str()
            .expect("#1122 real TS explicit disposal reason exists");
        assert_eq!(
            expected, "session_create_cancelled",
            "#1122 explicit disposal reason fixture contract"
        );
        assert_eq!(
            reason,
            Some(expected.into()),
            "#1122 pending candidate explicit disposal reason"
        );
    });
}

#[test]
#[ignore = "requires fixed TS baseline and Node v22.22.0"]
fn cancelled_disposal_regenerates_identically() {
    crate::test_baseline::regenerates_identically("session-factory");
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

struct RetryFaultSession {
    inner: Arc<dyn LiveSession>,
    mode: &'static str,
    gate: Arc<Gate>,
    snapshots: AtomicUsize,
}
impl LiveSession for RetryFaultSession {
    fn snapshot(&self) -> JsValue {
        self.inner.snapshot()
    }
    fn reload_snapshot(&self) -> Result<ReloadSnapshot, RuntimeError> {
        self.inner.reload_snapshot()
    }
    fn draining_reload_snapshot(&self) -> Result<ReloadSnapshot, RuntimeError> {
        let read = self.snapshots.fetch_add(1, Ordering::SeqCst);
        if self.mode == "slow-snapshot" && read == 1 {
            // advance changes the clock before its cooperative yield. Poll once
            // in this synchronous callback; prove the clock really moved.
            let before = tokio::time::Instant::now();
            let mut advance = Box::pin(tokio::time::advance(Duration::from_secs(6)));
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            let _ = advance.as_mut().poll(&mut cx);
            assert_eq!(
                tokio::time::Instant::now() - before,
                Duration::from_secs(6),
                "#1022 r2 slow snapshot advances virtual clock"
            );
        }
        self.inner.draining_reload_snapshot()
    }
    fn abort(&self, reason: &JsString) -> RuntimeResult {
        self.inner.abort(reason)
    }
    fn disposal_guard(&self) -> Option<Box<dyn Send>> {
        if self.mode == "guard-panic" {
            panic!("#1022 r2 injected guard panic");
        }
        if self.mode == "guard-drop-panic" {
            struct PanicOnDrop {
                _writer: Option<Box<dyn Send>>,
            }
            impl Drop for PanicOnDrop {
                fn drop(&mut self) {
                    panic!("#1022 r2 injected writer guard drop panic");
                }
            }
            return Some(Box::new(PanicOnDrop {
                _writer: self.inner.disposal_guard(),
            }));
        }
        self.inner.disposal_guard()
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async move {
            if self.mode == "dispose-panic" {
                panic!("#1022 r2 injected dispose panic");
            }
            if self.mode == "error" {
                self.gate.wait().await;
            }
            self.inner.dispose().await?;
            if self.mode == "error" {
                return Err(RuntimeError::new("DISPOSE_1022"));
            }
            Ok(())
        })
    }
}
struct RetryInitialFactory {
    initial: Mutex<Option<LiveSessionHandle>>,
    factory: Arc<ProjectSessionFactory>,
}
impl SessionFactory for RetryInitialFactory {
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
async fn retry_fault(
    h: &Harness,
    mode: &'static str,
) -> (LiveSessionHandle, LiveSessionHandle, Arc<Gate>) {
    let original = followup_bounded("r2 initial", h.factory.create(h.context(), None))
        .await
        .expect("#1022 r2 initial factory session");
    let gate = Gate::new();
    let old = LiveSessionHandle::new(
        Arc::new(RetryFaultSession {
            inner: original.session_for_test(),
            mode,
            gate: gate.clone(),
            snapshots: AtomicUsize::new(0),
        }),
        None,
    );
    (old, original, gate)
}

async fn retry_idle_panic(mode: &'static str) {
    let h = Harness::new("r2-idle-panic");
    let (old, original, _) = retry_fault(&h, mode).await;
    for _ in 0..2 {
        let result = followup_bounded(
            "r2 idle panic",
            h.factory.create(h.context(), Some(old.clone())),
        )
        .await;
        assert_eq!(
            lock(&h.observed.events)
                .iter()
                .filter(|e| e.starts_with("storage."))
                .count(),
            1,
            "#1022 r2 panic predecessor never creates replacement storage"
        );
        assert_eq!(
            result
                .err()
                .expect("#1022 r2 idle panic refuses replacement")
                .message,
            "Agent session dispose task panicked.",
            "#1022 r2 idle panic remains visible"
        );
        assert!(
            !old.recreation_drained(),
            "#1022 r2 panic is not normal drain"
        );
    }
    assert_eq!(
        old.state(),
        HandleState::Disposed,
        "#1022 r2 panic terminal handle state"
    );
    // A guard panic precedes writer transfer; a dispose panic follows it.
    assert_eq!(
        lock(&lock(&h.observed.writer_leases)[0]).is_some(),
        mode == "guard-panic",
        "#1022 r2 panic writer ownership is observed"
    );
    original
        .dispose("explicit cleanup".into())
        .await
        .expect("#1022 r2 panic original cleanup");
    let new = followup_bounded(
        "r2 panic retired writer",
        h.factory.create(h.context(), None),
    )
    .await
    .expect("#1022 r2 explicit retirement permits independent writer");
    new.dispose("cleanup".into())
        .await
        .expect("#1022 r2 panic new cleanup");
    h.finish("r2-idle-panic").await;
}
#[tokio::test]
async fn recreation_idle_guard_panic_cannot_skip_drain() {
    retry_idle_panic("guard-panic").await;
}
#[tokio::test]
async fn recreation_idle_dispose_panic_cannot_skip_drain() {
    retry_idle_panic("dispose-panic").await;
}

async fn retry_cancelled_error(via_owner: bool, after_send: bool, idle_sweep: bool) {
    let now = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let h = Harness::new("r2-cancelled-error");
    let (old, _original, gate) = retry_fault(&h, "error").await;
    let owner = if via_owner {
        let mut options = SessionOwnerOptions::new(Arc::new(RetryInitialFactory {
            initial: Mutex::new(Some(old.clone())),
            factory: h.factory.clone(),
        }));
        options.idle_sweep_interval_ms = 0;
        if idle_sweep {
            let clock = now.clone();
            options.now_ms = Arc::new(move || clock.load(Ordering::SeqCst));
            options.idle_timeout_ms = 10;
        }
        let owner = SessionOwner::new(options);
        owner
            .get_or_create(h.context())
            .await
            .expect("#1022 r2 owner initial");
        assert!(
            owner.mark_session_dirty(&h.context().session_key, "r2"),
            "#1022 r2 owner dirty"
        );
        Some(owner)
    } else {
        None
    };
    let mut sent = owner.as_ref().map_or_else(
        || h.factory.creation_sent.subscribe(),
        SessionOwner::creations_sent_for_test,
    );
    let expected_sent = *sent.borrow() + 1;
    let create = || match &owner {
        Some(owner) => owner.get_or_create(h.context()),
        None => h.factory.create(h.context(), Some(old.clone())),
    };
    let mut waiting = create();
    let poll = std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx))).await;
    assert!(poll.is_pending(), "#1022 r2 error caller waits for gate");
    followup_bounded("r2 disposal gate", gate.entered()).await;
    if after_send {
        gate.open();
        followup_bounded(
            "r2 error sent",
            sent.wait_for(|count| *count >= expected_sent),
        )
        .await
        .expect("#1022 r2 result actually sent before receiver cancellation");
        drop(waiting);
    } else {
        drop(waiting);
        gate.open();
        followup_bounded(
            "r2 abandoned error sent",
            sent.wait_for(|count| *count >= expected_sent),
        )
        .await
        .expect("#1022 r2 cancelled caller completes before retry");
    }
    if idle_sweep {
        let owner = owner.as_ref().expect("#1022 r3 idle sweep owner");
        now.store(11, Ordering::SeqCst);
        owner.sweep_idle();
        assert!(
            owner
                .current_handle(&h.context().session_key)
                .is_some_and(|current| current.same_identity(&old)),
            "#1022 r3 idle sweep retains pending cleanup error"
        );
    }
    let error = followup_bounded("r2 error replay", create())
        .await
        .err()
        .expect("#1022 r2 cancelled caller cleanup error must be replayed");
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/session-disposal-retry/oracle.json"
    ))
    .expect("#1022 r2 TS oracle JSON");
    assert_eq!(
        error.message, oracle["rows"][3]["error"]["message"],
        "#1022 r2 retained error aggregate"
    );
    assert_eq!(
        error
            .causes
            .first()
            .expect("#1022 r2 disposal cause")
            .causes
            .first()
            .expect("#1022 r2 nested disposal cause")
            .message,
        "DISPOSE_1022",
        "#1022 r2 original cleanup error"
    );
    assert_eq!(
        lock(&h.observed.bindings).len(),
        1,
        "#1022 r2 error replay cannot publish replacement"
    );
    let new = followup_bounded("r2 acknowledged retry", create())
        .await
        .expect("#1022 r2 consumed cleanup error allows retry");
    assert!(!new.same_identity(&old), "#1022 r2 replacement identity");
    assert_eq!(
        lock(&h.observed.events)
            .iter()
            .filter(|e| *e == "close.1")
            .count(),
        1,
        "#1022 r2 old storage closes once"
    );
    if let Some(owner) = owner {
        followup_bounded("r2 owner cleanup", owner.shutdown())
            .await
            .expect("#1022 r2 owner cleanup");
    } else {
        new.dispose("cleanup".into())
            .await
            .expect("#1022 r2 direct cleanup");
    }
    h.finish("r2-cancelled-error").await;
}
#[tokio::test]
async fn recreation_cancelled_factory_error_is_replayed() {
    retry_cancelled_error(false, false, false).await;
}
#[tokio::test]
async fn recreation_unconsumed_factory_error_is_replayed() {
    retry_cancelled_error(false, true, false).await;
}
#[tokio::test]
async fn recreation_cancelled_owner_error_is_replayed() {
    retry_cancelled_error(true, false, false).await;
}
#[tokio::test]
async fn recreation_unconsumed_owner_error_is_replayed() {
    retry_cancelled_error(true, true, false).await;
}

#[tokio::test]
async fn recreation_cancelled_owner_error_survives_idle_sweep() {
    retry_cancelled_error(true, false, true).await;
}
#[tokio::test]
async fn recreation_unconsumed_owner_error_survives_idle_sweep() {
    retry_cancelled_error(true, true, true).await;
}

async fn cancelled_timeout_retry(finish_predecessor: bool) {
    let h = Harness::with_fence_timeout("r3-cancelled-timeout", Duration::from_millis(50));
    let old = followup_bounded("r3 initial factory", h.factory.create(h.context(), None))
        .await
        .expect("#1022 r3 initial factory succeeds");
    let mut options = SessionOwnerOptions::new(Arc::new(RetryInitialFactory {
        initial: Mutex::new(Some(old.clone())),
        factory: h.factory.clone(),
    }));
    options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(options);
    followup_bounded("r3 initial owner", owner.get_or_create(h.context()))
        .await
        .expect("#1022 r3 initial owner succeeds");
    let activity = old
        .begin_activity()
        .expect("#1022 r3 predecessor holds activity");
    assert!(
        owner.mark_session_dirty(&h.context().session_key, "r3"),
        "#1022 r3 mark dirty"
    );
    let mut sent = owner.creations_sent_for_test();
    let expected_sent = *sent.borrow() + 1;
    let mut waiting = owner.get_or_create(h.context());
    let poll = std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx))).await;
    assert!(
        poll.is_pending(),
        "#1022 r3 caller waits for active predecessor"
    );
    drop(waiting);
    followup_bounded(
        "r3 cancelled timeout sent",
        sent.wait_for(|count| *count >= expected_sent),
    )
    .await
    .expect("#1022 r3 abandoned BUSY sent before predecessor completes");
    assert_eq!(
        old.state(),
        HandleState::Draining,
        "#1022 r3 predecessor still draining at timeout"
    );
    assert_eq!(
        lock(&h.observed.bindings).len(),
        1,
        "#1022 r3 timeout cannot publish replacement"
    );
    if !finish_predecessor {
        let error = followup_bounded(
            "r3 active predecessor retry",
            owner.get_or_create(h.context()),
        )
        .await
        .err()
        .expect("#1022 r3 active predecessor still rejects retry");
        assert_eq!(
            error.js_error().and_then(|error| error.code.as_deref()),
            Some("SESSION_WRITER_BUSY"),
            "#1022 r3 active predecessor preserves BUSY"
        );
        assert_eq!(
            lock(&h.observed.bindings).len(),
            1,
            "#1022 r3 active predecessor cannot create storage"
        );
    }
    // The never-completing observation ends here: releasing the real activity
    // guard is the only event that can let the owned predecessor drain finish.
    drop(activity);
    followup_bounded("r3 predecessor completion", old.dispose("finish".into()))
        .await
        .expect("#1022 r3 predecessor completes after activity release");
    let new = followup_bounded(
        "r3 retry after completed drain",
        owner.get_or_create(h.context()),
    )
    .await
    .expect("#1022 r3 completed drain must discard abandoned BUSY");
    assert!(
        !new.same_identity(&old),
        "#1022 r3 completed drain publishes replacement"
    );
    assert_eq!(
        lock(&h.observed.bindings).len(),
        2,
        "#1022 r3 retry constructs exactly one replacement"
    );
    followup_bounded("r3 owner shutdown", owner.shutdown())
        .await
        .expect("#1022 r3 owner shutdown succeeds");
    h.finish("r3-cancelled-timeout").await;
}

#[tokio::test]
async fn recreation_cancelled_timeout_retries_after_drain() {
    cancelled_timeout_retry(true).await;
}
#[tokio::test]
async fn recreation_cancelled_timeout_keeps_active_predecessor() {
    cancelled_timeout_retry(false).await;
}

// #1055 ①: a cancelled caller left a drain-timeout BUSY behind, and the drain
// it stopped waiting on later ends with a cleanup error. The immediately
// following caller must receive that original disposal error instead of one
// more replayed BUSY round (TS: every awaiter of the cached disposePromise
// gets the same rejection; there is no busy tier to mask it).
#[tokio::test]
async fn recreation_cancelled_timeout_late_cleanup_error_is_returned_to_next_caller() {
    let h = Harness::with_fence_timeout("1218-late-cleanup", Duration::from_millis(50));
    let (old, _original, gate) = retry_fault(&h, "error").await;
    let mut options = SessionOwnerOptions::new(Arc::new(RetryInitialFactory {
        initial: Mutex::new(Some(old.clone())),
        factory: h.factory.clone(),
    }));
    options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(options);
    followup_bounded("1218 initial owner", owner.get_or_create(h.context()))
        .await
        .expect("#1218 initial owner succeeds");
    assert!(
        owner.mark_session_dirty(&h.context().session_key, "1218"),
        "#1218 mark dirty"
    );
    let mut sent = owner.creations_sent_for_test();
    let expected_sent = *sent.borrow() + 1;
    let mut waiting = owner.get_or_create(h.context());
    let poll = std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx))).await;
    assert!(
        poll.is_pending(),
        "#1218 recreate caller waits for the held drain"
    );
    drop(waiting);
    followup_bounded(
        "1218 cancelled timeout sent",
        sent.wait_for(|count| *count >= expected_sent),
    )
    .await
    .expect("#1218 abandoned BUSY sent before the drain completes");
    assert_eq!(
        old.state(),
        HandleState::Draining,
        "#1218 drain still running behind the abandoned BUSY"
    );
    gate.open();
    // Await the idempotent disposal outcome so the completion is published
    // before the next caller observes it.
    let drain_error = followup_bounded("1218 drain outcome", old.dispose("probe".into()))
        .await
        .expect_err("#1218 late drain ends with the cleanup error");
    assert_eq!(
        drain_error.message, "Failed to dispose agent handle.",
        "#1218 late drain error shape"
    );
    let error = followup_bounded("1218 next caller", owner.get_or_create(h.context()))
        .await
        .err()
        .expect("#1218 next caller receives the late cleanup error");
    assert_ne!(
        error.js_error().and_then(|error| error.code.as_deref()),
        Some("SESSION_WRITER_BUSY"),
        "#1218 takeover must not replay one more BUSY round"
    );
    assert_eq!(
        error.message, "Failed to dispose agent handle.",
        "#1218 takeover returns the original disposal error"
    );
    assert_eq!(
        error
            .causes
            .first()
            .and_then(|cause| cause.causes.first())
            .map(|cause| cause.message.as_str()),
        Some("DISPOSE_1022"),
        "#1218 takeover preserves the nested cleanup error"
    );
    assert_eq!(
        lock(&h.observed.bindings).len(),
        1,
        "#1218 takeover cannot publish a replacement"
    );
    let new = followup_bounded(
        "1218 retry after takeover",
        owner.get_or_create(h.context()),
    )
    .await
    .expect("#1218 retry after the takeover delivered the error");
    assert!(!new.same_identity(&old), "#1218 replacement identity");
    assert_eq!(
        lock(&h.observed.bindings).len(),
        2,
        "#1218 retry constructs exactly one replacement"
    );
    followup_bounded("1218 owner shutdown", owner.shutdown())
        .await
        .expect("#1218 owner shutdown succeeds");
    h.finish("1218-late-cleanup").await;
}

// #1055 ②: a record holding a retriable, cleanly-drained BUSY is ordinary
// idle-expiry reclaimable; the next caller is a fresh create (TS: failed and
// abandoned creates never park a pending record in the map at all).
#[tokio::test]
async fn recreation_cancelled_timeout_busy_is_reclaimed_by_idle_sweep() {
    let now = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let h = Harness::with_fence_timeout("1218-busy-sweep", Duration::from_millis(50));
    let old = followup_bounded("1218 initial factory", h.factory.create(h.context(), None))
        .await
        .expect("#1218 initial factory succeeds");
    let mut options = SessionOwnerOptions::new(Arc::new(RetryInitialFactory {
        initial: Mutex::new(Some(old.clone())),
        factory: h.factory.clone(),
    }));
    options.idle_sweep_interval_ms = 0;
    let clock = now.clone();
    options.now_ms = Arc::new(move || clock.load(Ordering::SeqCst));
    options.idle_timeout_ms = 10;
    let owner = SessionOwner::new(options);
    followup_bounded("1218 initial owner", owner.get_or_create(h.context()))
        .await
        .expect("#1218 initial owner succeeds");
    let activity = old
        .begin_activity()
        .expect("#1218 predecessor holds activity");
    assert!(
        owner.mark_session_dirty(&h.context().session_key, "1218"),
        "#1218 mark dirty"
    );
    let mut sent = owner.creations_sent_for_test();
    let expected_sent = *sent.borrow() + 1;
    let mut waiting = owner.get_or_create(h.context());
    let poll = std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx))).await;
    assert!(
        poll.is_pending(),
        "#1218 caller waits for the active predecessor"
    );
    drop(waiting);
    followup_bounded(
        "1218 cancelled timeout sent",
        sent.wait_for(|count| *count >= expected_sent),
    )
    .await
    .expect("#1218 fence timeout delivered BUSY before the drain completes");
    assert_eq!(
        old.state(),
        HandleState::Draining,
        "#1218 predecessor still draining at the timeout"
    );
    drop(activity);
    followup_bounded("1218 clean drain", old.dispose("finish".into()))
        .await
        .expect("#1218 predecessor drain completes clean");
    now.store(11, Ordering::SeqCst);
    owner.sweep_idle();
    assert_eq!(
        owner.cached_session_count(),
        0,
        "#1218 idle sweep reclaims the drained-BUSY record"
    );
    let fresh = followup_bounded(
        "1218 fresh create after sweep",
        owner.get_or_create(h.context()),
    )
    .await
    .expect("#1218 fresh create after idle reclamation");
    assert!(
        !fresh.same_identity(&old),
        "#1218 reclaimed record creates fresh"
    );
    followup_bounded("1218 owner shutdown", owner.shutdown())
        .await
        .expect("#1218 owner shutdown succeeds");
    h.finish("1218-busy-sweep").await;
}

// #1055 ② cell 6: while the drain behind a replayable BUSY is still running,
// idle expiry must NOT reclaim the record — evicting it now would orphan a
// late cleanup error with no delivery path. The next sweep round, after the
// drain settled, reclaims it (cell 7).
#[tokio::test]
async fn recreation_cancelled_timeout_busy_is_retained_by_sweep_while_drain_pending() {
    let now = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let h = Harness::with_fence_timeout("1218-busy-sweep-pending", Duration::from_millis(50));
    let old = followup_bounded("1218 initial factory", h.factory.create(h.context(), None))
        .await
        .expect("#1218 initial factory succeeds");
    let mut options = SessionOwnerOptions::new(Arc::new(RetryInitialFactory {
        initial: Mutex::new(Some(old.clone())),
        factory: h.factory.clone(),
    }));
    options.idle_sweep_interval_ms = 0;
    let clock = now.clone();
    options.now_ms = Arc::new(move || clock.load(Ordering::SeqCst));
    options.idle_timeout_ms = 10;
    let owner = SessionOwner::new(options);
    followup_bounded("1218 initial owner", owner.get_or_create(h.context()))
        .await
        .expect("#1218 initial owner succeeds");
    let activity = old
        .begin_activity()
        .expect("#1218 predecessor holds activity");
    assert!(
        owner.mark_session_dirty(&h.context().session_key, "1218"),
        "#1218 mark dirty"
    );
    let mut sent = owner.creations_sent_for_test();
    let expected_sent = *sent.borrow() + 1;
    let mut waiting = owner.get_or_create(h.context());
    let poll = std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx))).await;
    assert!(
        poll.is_pending(),
        "#1218 caller waits for the active predecessor"
    );
    drop(waiting);
    followup_bounded(
        "1218 cancelled timeout sent",
        sent.wait_for(|count| *count >= expected_sent),
    )
    .await
    .expect("#1218 fence timeout delivered BUSY before the drain completes");
    assert_eq!(
        old.state(),
        HandleState::Draining,
        "#1218 predecessor still draining at the timeout"
    );
    now.store(11, Ordering::SeqCst);
    owner.sweep_idle();
    assert_eq!(
        owner.cached_session_count(),
        1,
        "#1218 sweep retains the busy record until the drain settles"
    );
    drop(activity);
    followup_bounded("1218 clean drain", old.dispose("finish".into()))
        .await
        .expect("#1218 predecessor drain completes clean");
    now.store(22, Ordering::SeqCst);
    owner.sweep_idle();
    assert_eq!(
        owner.cached_session_count(),
        0,
        "#1218 next sweep round reclaims the settled busy record"
    );
    let fresh = followup_bounded(
        "1218 fresh create after retention",
        owner.get_or_create(h.context()),
    )
    .await
    .expect("#1218 fresh create after the busy record was reclaimed");
    assert!(
        !fresh.same_identity(&old),
        "#1218 reclaimed record creates fresh"
    );
    followup_bounded("1218 owner shutdown", owner.shutdown())
        .await
        .expect("#1218 owner shutdown succeeds");
    h.finish("1218-busy-sweep-pending").await;
}

// #1055 ② cell 12: a retriable BUSY is never delivered by close or shutdown —
// both return only the eviction result (the disposal outcome), so a transient
// recreation error cannot leak to the closing caller.
#[tokio::test]
async fn recreation_cancelled_timeout_busy_is_not_delivered_by_close_or_shutdown() {
    let h = Harness::with_fence_timeout("1218-busy-close", Duration::from_millis(50));
    let old = followup_bounded("1218 initial factory", h.factory.create(h.context(), None))
        .await
        .expect("#1218 initial factory succeeds");
    let mut options = SessionOwnerOptions::new(Arc::new(RetryInitialFactory {
        initial: Mutex::new(Some(old.clone())),
        factory: h.factory.clone(),
    }));
    options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(options);
    followup_bounded("1218 initial owner", owner.get_or_create(h.context()))
        .await
        .expect("#1218 initial owner succeeds");
    let activity = old
        .begin_activity()
        .expect("#1218 predecessor holds activity");
    assert!(
        owner.mark_session_dirty(&h.context().session_key, "1218"),
        "#1218 mark dirty"
    );
    let mut sent = owner.creations_sent_for_test();
    let expected_sent = *sent.borrow() + 1;
    let mut waiting = owner.get_or_create(h.context());
    let poll = std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx))).await;
    assert!(
        poll.is_pending(),
        "#1218 caller waits for the active predecessor"
    );
    drop(waiting);
    followup_bounded(
        "1218 cancelled timeout sent",
        sent.wait_for(|count| *count >= expected_sent),
    )
    .await
    .expect("#1218 fence timeout delivered BUSY before the drain completes");
    drop(activity);
    followup_bounded("1218 clean drain", old.dispose("finish".into()))
        .await
        .expect("#1218 predecessor drain completes clean");
    followup_bounded("1218 close", owner.close(&h.context().session_key))
        .await
        .expect("#1218 close returns only the evict result for a retriable BUSY");
    assert_eq!(
        owner.cached_session_count(),
        0,
        "#1218 close removed the busy record"
    );
    followup_bounded("1218 owner shutdown", owner.shutdown())
        .await
        .expect("#1218 owner shutdown succeeds");
    h.finish("1218-busy-close").await;

    let h = Harness::with_fence_timeout("1218-busy-shutdown", Duration::from_millis(50));
    let old = followup_bounded("1218 initial factory", h.factory.create(h.context(), None))
        .await
        .expect("#1218 initial factory succeeds");
    let mut options = SessionOwnerOptions::new(Arc::new(RetryInitialFactory {
        initial: Mutex::new(Some(old.clone())),
        factory: h.factory.clone(),
    }));
    options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(options);
    followup_bounded("1218 initial owner", owner.get_or_create(h.context()))
        .await
        .expect("#1218 initial owner succeeds");
    let activity = old
        .begin_activity()
        .expect("#1218 predecessor holds activity");
    assert!(
        owner.mark_session_dirty(&h.context().session_key, "1218"),
        "#1218 mark dirty"
    );
    let mut sent = owner.creations_sent_for_test();
    let expected_sent = *sent.borrow() + 1;
    let mut waiting = owner.get_or_create(h.context());
    let poll = std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx))).await;
    assert!(
        poll.is_pending(),
        "#1218 caller waits for the active predecessor"
    );
    drop(waiting);
    followup_bounded(
        "1218 cancelled timeout sent",
        sent.wait_for(|count| *count >= expected_sent),
    )
    .await
    .expect("#1218 fence timeout delivered BUSY before the drain completes");
    drop(activity);
    followup_bounded("1218 clean drain", old.dispose("finish".into()))
        .await
        .expect("#1218 predecessor drain completes clean");
    followup_bounded("1218 owner shutdown", owner.shutdown())
        .await
        .expect("#1218 shutdown returns only the evict result for a retriable BUSY");
    h.finish("1218-busy-shutdown").await;
}

// #1055 ② cell 12, faulted half: a retained, retriable BUSY whose own
// disposal later ended in an unreported cleanup error — with no next caller
// taking it over — must still not reach the closing or shutting-down caller.
// Both return only the eviction result, the replayed disposal error, with no
// BUSY cause beside it.
async fn busy_retained_beside_failed_disposal(h: &Harness) -> SessionOwner {
    let (old, _original, gate) = retry_fault(h, "error").await;
    let mut options = SessionOwnerOptions::new(Arc::new(RetryInitialFactory {
        initial: Mutex::new(Some(old.clone())),
        factory: h.factory.clone(),
    }));
    options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(options);
    followup_bounded("1218 initial owner", owner.get_or_create(h.context()))
        .await
        .expect("#1218 initial owner succeeds");
    assert!(
        owner.mark_session_dirty(&h.context().session_key, "1218"),
        "#1218 mark dirty"
    );
    let mut sent = owner.creations_sent_for_test();
    let expected_sent = *sent.borrow() + 1;
    let mut waiting = owner.get_or_create(h.context());
    let poll = std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx))).await;
    assert!(
        poll.is_pending(),
        "#1218 recreate caller waits for the held drain"
    );
    drop(waiting);
    followup_bounded(
        "1218 cancelled timeout sent",
        sent.wait_for(|count| *count >= expected_sent),
    )
    .await
    .expect("#1218 fence timeout delivered BUSY before the drain completes");
    assert_eq!(
        old.state(),
        HandleState::Draining,
        "#1218 predecessor still draining at the timeout"
    );
    gate.open();
    let drain_error = followup_bounded("1218 drain outcome", old.dispose("probe".into()))
        .await
        .expect_err("#1218 unreported drain ends with the cleanup error");
    assert_eq!(
        drain_error.message, "Failed to dispose agent handle.",
        "#1218 drain error shape"
    );
    owner
}

#[tokio::test]
async fn recreation_cancelled_timeout_busy_is_not_aggregated_with_failed_disposal_by_close() {
    let h = Harness::with_fence_timeout("1218-busy-fault-close", Duration::from_millis(50));
    let owner = busy_retained_beside_failed_disposal(&h).await;
    let error = followup_bounded("1218 close", owner.close(&h.context().session_key))
        .await
        .expect_err("#1218 close returns only the evict error beside a retained BUSY");
    assert_eq!(
        error.message, "Failed to dispose agent handle.",
        "#1218 close keeps only the failed disposal error beside a retriable BUSY"
    );
    assert_eq!(
        error.causes.len(),
        1,
        "#1218 close does not aggregate the retriable BUSY beside the failed disposal"
    );
    assert_eq!(
        error
            .causes
            .first()
            .and_then(|cause| cause.causes.first())
            .map(|cause| cause.message.as_str()),
        Some("DISPOSE_1022"),
        "#1218 close keeps the failed disposal's nested drain cause"
    );
    assert_eq!(
        owner.cached_session_count(),
        0,
        "#1218 close removed the busy record"
    );
    followup_bounded("1218 owner shutdown", owner.shutdown())
        .await
        .expect("#1218 owner shutdown succeeds");
    h.finish("1218-busy-fault-close").await;
}

#[tokio::test]
async fn recreation_cancelled_timeout_busy_is_not_aggregated_with_failed_disposal_by_shutdown() {
    let h = Harness::with_fence_timeout("1218-busy-fault-shutdown", Duration::from_millis(50));
    let owner = busy_retained_beside_failed_disposal(&h).await;
    let error = followup_bounded("1218 owner shutdown", owner.shutdown())
        .await
        .expect_err("#1218 shutdown returns only the evict error beside a retained BUSY");
    assert_eq!(
        error.message, "Failed to shut down all agent sessions.",
        "#1218 shutdown aggregate shape (TS finishShutdown AggregateError)"
    );
    assert_eq!(
        error.causes.len(),
        1,
        "#1218 shutdown carries only the record's failed disposal"
    );
    let record_error = error
        .causes
        .first()
        .expect("#1218 shutdown carries the record's failed disposal error");
    assert_eq!(
        record_error.message, "Failed to dispose agent handle.",
        "#1218 shutdown keeps only the failed disposal error beside a retriable BUSY"
    );
    assert_eq!(
        record_error.causes.len(),
        1,
        "#1218 shutdown does not aggregate the retriable BUSY beside the failed disposal"
    );
    h.finish("1218-busy-fault-shutdown").await;
}

// #1055 ② (P3-1): a BUSY record whose predecessor drain ended in an already
// reported cleanup error is reclaimable — a reported drain counts as settled
// even though the disposal itself failed, so the sweep's busy arm applies.
#[tokio::test]
async fn recreation_cancelled_timeout_reported_cleanup_error_is_reclaimed_by_idle_sweep() {
    let now = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let h = Harness::with_fence_timeout("1218-reported-sweep", Duration::from_millis(50));
    let (old, _original, gate) = retry_fault(&h, "error").await;
    let mut options = SessionOwnerOptions::new(Arc::new(RetryInitialFactory {
        initial: Mutex::new(Some(old.clone())),
        factory: h.factory.clone(),
    }));
    options.idle_sweep_interval_ms = 0;
    let clock = now.clone();
    options.now_ms = Arc::new(move || clock.load(Ordering::SeqCst));
    options.idle_timeout_ms = 10;
    let owner = SessionOwner::new(options);
    followup_bounded("1218 initial owner", owner.get_or_create(h.context()))
        .await
        .expect("#1218 initial owner succeeds");
    assert!(
        owner.mark_session_dirty(&h.context().session_key, "1218"),
        "#1218 mark dirty"
    );
    let mut sent = owner.creations_sent_for_test();
    let expected_sent = *sent.borrow() + 1;
    let mut waiting = owner.get_or_create(h.context());
    let poll = std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx))).await;
    assert!(
        poll.is_pending(),
        "#1218 recreate caller waits for the held drain"
    );
    drop(waiting);
    followup_bounded(
        "1218 cancelled timeout sent",
        sent.wait_for(|count| *count >= expected_sent),
    )
    .await
    .expect("#1218 fence timeout delivered BUSY before the drain completes");
    assert_eq!(
        old.state(),
        HandleState::Draining,
        "#1218 predecessor still draining at the timeout"
    );
    gate.open();
    let drain_error = followup_bounded("1218 drain outcome", old.dispose("probe".into()))
        .await
        .expect_err("#1218 background drain ends with the cleanup error");
    assert_eq!(
        drain_error.message, "Failed to dispose agent handle.",
        "#1218 drain error shape"
    );
    // A factory waiter consumes the drain error and reports it, so the drain
    // counts as settled even though it ended in Err.
    let reported = followup_bounded(
        "1218 factory waiter",
        h.factory.create(h.context(), Some(old.clone())),
    )
    .await
    .err()
    .expect("#1218 factory waiter consumes the reported cleanup error");
    assert_eq!(
        reported.message, "Failed to dispose agent handle.",
        "#1218 factory waiter error shape"
    );
    now.store(11, Ordering::SeqCst);
    owner.sweep_idle();
    assert_eq!(
        owner.cached_session_count(),
        0,
        "#1218 sweep reclaims the busy record after the cleanup error was reported"
    );
    let fresh = followup_bounded(
        "1218 fresh create after reclamation",
        owner.get_or_create(h.context()),
    )
    .await
    .expect("#1218 fresh create after the busy record was reclaimed");
    assert!(
        !fresh.same_identity(&old),
        "#1218 reclaimed record creates fresh"
    );
    followup_bounded("1218 owner shutdown", owner.shutdown())
        .await
        .expect("#1218 owner shutdown succeeds");
    h.finish("1218-reported-sweep").await;
}

// #1055 ② test (a): once the pending cleanup error is consumed and the retry
// published the replacement, idle expiry reclaims that record like any other
// (cell 11: consumed -> None -> sweepable).
async fn retry_consumed_error_sweep(after_send: bool) {
    let now = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let h = Harness::new("1218-consumed-sweep");
    let (old, _original, gate) = retry_fault(&h, "error").await;
    let mut options = SessionOwnerOptions::new(Arc::new(RetryInitialFactory {
        initial: Mutex::new(Some(old.clone())),
        factory: h.factory.clone(),
    }));
    options.idle_sweep_interval_ms = 0;
    let clock = now.clone();
    options.now_ms = Arc::new(move || clock.load(Ordering::SeqCst));
    options.idle_timeout_ms = 10;
    let owner = SessionOwner::new(options);
    followup_bounded("1218 initial owner", owner.get_or_create(h.context()))
        .await
        .expect("#1218 initial owner succeeds");
    assert!(
        owner.mark_session_dirty(&h.context().session_key, "1218"),
        "#1218 mark dirty"
    );
    let mut sent = owner.creations_sent_for_test();
    let expected_sent = *sent.borrow() + 1;
    let mut waiting = owner.get_or_create(h.context());
    let poll = std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx))).await;
    assert!(
        poll.is_pending(),
        "#1218 recreate caller waits for the held drain"
    );
    if after_send {
        gate.open();
        followup_bounded(
            "1218 abandoned error sent",
            sent.wait_for(|count| *count >= expected_sent),
        )
        .await
        .expect("#1218 unconsumed error sent before receiver cancellation");
        drop(waiting);
    } else {
        drop(waiting);
        gate.open();
        followup_bounded(
            "1218 abandoned error sent",
            sent.wait_for(|count| *count >= expected_sent),
        )
        .await
        .expect("#1218 cancelled caller completes before retry");
    }
    // Before consumption the non-retriable cleanup error keeps the record.
    now.store(11, Ordering::SeqCst);
    owner.sweep_idle();
    assert_eq!(
        owner.cached_session_count(),
        1,
        "#1218 unconsumed cleanup error is retained by idle sweep"
    );
    let error = followup_bounded("1218 replay", owner.get_or_create(h.context()))
        .await
        .err()
        .expect("#1218 consumed cleanup error is replayed");
    assert_eq!(
        error.message, "Failed to dispose agent handle.",
        "#1218 replay error shape"
    );
    let new = followup_bounded("1218 retry", owner.get_or_create(h.context()))
        .await
        .expect("#1218 consumed cleanup error allows retry");
    assert!(!new.same_identity(&old), "#1218 replacement identity");
    // The replacement record is ordinary: idle expiry now removes it.
    now.store(22, Ordering::SeqCst);
    owner.sweep_idle();
    assert_eq!(
        owner.cached_session_count(),
        0,
        "#1218 idle sweep reclaims the record after the error was consumed"
    );
    let fresh = followup_bounded("1218 fresh create", owner.get_or_create(h.context()))
        .await
        .expect("#1218 fresh create after idle reclamation");
    assert!(
        !fresh.same_identity(&old),
        "#1218 fresh session after reclamation"
    );
    followup_bounded("1218 owner shutdown", owner.shutdown())
        .await
        .expect("#1218 owner shutdown succeeds");
    h.finish("1218-consumed-sweep").await;
}
#[tokio::test]
async fn recreation_cancelled_consumed_owner_error_is_reclaimed_by_idle_sweep() {
    retry_consumed_error_sweep(false).await;
}
#[tokio::test]
async fn recreation_unconsumed_consumed_owner_error_is_reclaimed_by_idle_sweep() {
    retry_consumed_error_sweep(true).await;
}

// #1055 ② cell 8: the record keeps an unconsumed cleanup error and its handle
// drain already failed; close returns that original disposer error, aggregated
// with the retained cleanup error (both causes stem from the same drain
// failure, so the aggregate lists the disposer error twice).
#[tokio::test]
async fn recreation_unconsumed_cleanup_error_is_returned_by_close() {
    let h = Harness::new("1218-close-cleanup");
    let (old, _original, gate) = retry_fault(&h, "error").await;
    let mut options = SessionOwnerOptions::new(Arc::new(RetryInitialFactory {
        initial: Mutex::new(Some(old.clone())),
        factory: h.factory.clone(),
    }));
    options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(options);
    followup_bounded("1218 initial owner", owner.get_or_create(h.context()))
        .await
        .expect("#1218 initial owner succeeds");
    assert!(
        owner.mark_session_dirty(&h.context().session_key, "1218"),
        "#1218 mark dirty"
    );
    let mut sent = owner.creations_sent_for_test();
    let expected_sent = *sent.borrow() + 1;
    let mut waiting = owner.get_or_create(h.context());
    let poll = std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx))).await;
    assert!(
        poll.is_pending(),
        "#1218 recreate caller waits for the held drain"
    );
    drop(waiting);
    gate.open();
    followup_bounded(
        "1218 abandoned error sent",
        sent.wait_for(|count| *count >= expected_sent),
    )
    .await
    .expect("#1218 cancelled caller completes before close");
    let error = followup_bounded("1218 close", owner.close(&h.context().session_key))
        .await
        .expect_err("#1218 close aggregates the disposer error with the retained cleanup error");
    assert_eq!(
        error.message,
        "Failed to dispose the agent session and report its unconsumed creation or cleanup error.",
        "#1218 close keeps both errors in one aggregate"
    );
    assert_eq!(
        error.causes.len(),
        2,
        "#1218 close keeps the disposer and the retained cleanup error"
    );
    assert_eq!(
        error.causes.first().map(|cause| cause.message.as_str()),
        Some("Failed to dispose agent handle."),
        "#1218 close preserves the disposer error"
    );
    assert_eq!(
        error
            .causes
            .first()
            .and_then(|cause| cause.causes.first())
            .and_then(|cause| cause.causes.first())
            .map(|cause| cause.message.as_str()),
        Some("DISPOSE_1022"),
        "#1218 close preserves the disposer error's nested cleanup cause"
    );
    assert_eq!(
        error
            .causes
            .get(1)
            .and_then(|cause| cause.causes.first())
            .and_then(|cause| cause.causes.first())
            .map(|cause| cause.message.as_str()),
        Some("DISPOSE_1022"),
        "#1218 close retains the unconsumed cleanup error"
    );
    assert_eq!(
        owner.cached_session_count(),
        0,
        "#1218 close removed the record"
    );
    followup_bounded("1218 owner shutdown", owner.shutdown())
        .await
        .expect("#1218 owner shutdown succeeds");
    h.finish("1218-close-cleanup").await;
}

// #1055 ② tests (b): an evicted record's unconsumed, non-retriable creation
// error has no other delivery path once its caller is gone — close returns it
// and shutdown aggregates it (TS has no counterpart: failed creates never
// enter the router map).
async fn unconsumed_creation_pending(h: &Harness) -> (SessionOwner, pilotdeck_protocol::JsString) {
    let initial = followup_bounded("1218 creation initial", h.factory.create(h.context(), None))
        .await
        .expect("#1218 creation initial factory session");
    unconsumed_creation_pending_with(h, initial).await
}

async fn unconsumed_creation_pending_with(
    h: &Harness,
    initial: LiveSessionHandle,
) -> (SessionOwner, pilotdeck_protocol::JsString) {
    struct FailsOnce {
        initial: Mutex<Option<LiveSessionHandle>>,
    }
    impl SessionFactory for FailsOnce {
        fn can_recreate(&self) -> bool {
            true
        }
        fn create(
            &self,
            _: SessionContext,
            _: Option<LiveSessionHandle>,
        ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
            if let Some(initial) = lock(&self.initial).take() {
                return Box::pin(async move { Ok(initial) });
            }
            Box::pin(async move { Err(RuntimeError::new("FAILURE_1218")) })
        }
    }
    let mut options = SessionOwnerOptions::new(Arc::new(FailsOnce {
        initial: Mutex::new(Some(initial)),
    }));
    options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(options);
    followup_bounded("1218 creation cache", owner.get_or_create(h.context()))
        .await
        .expect("#1218 creation initial owner");
    assert!(
        owner.mark_session_dirty(&h.context().session_key, "1218"),
        "#1218 creation dirty"
    );
    let mut sent = owner.creations_sent_for_test();
    let expected_sent = *sent.borrow() + 1;
    let mut waiting = owner.get_or_create(h.context());
    let poll = std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx))).await;
    assert!(
        poll.is_pending(),
        "#1218 creation caller waits for the factory"
    );
    drop(waiting);
    followup_bounded(
        "1218 creation failure sent",
        sent.wait_for(|count| *count >= expected_sent),
    )
    .await
    .expect("#1218 unconsumed creation failure stored");
    (owner, h.context().session_key)
}

#[tokio::test]
async fn recreation_unconsumed_creation_error_is_returned_by_close_and_shutdown() {
    let h = Harness::new("1218-close-creation");
    let (owner, key) = unconsumed_creation_pending(&h).await;
    let error = followup_bounded("1218 close", owner.close(&key))
        .await
        .expect_err("#1218 close returns the unconsumed creation error");
    assert_eq!(
        error.message, "FAILURE_1218",
        "#1218 close creation error identity"
    );
    assert_eq!(
        owner.cached_session_count(),
        0,
        "#1218 close removed the record"
    );
    followup_bounded("1218 post-close shutdown", owner.shutdown())
        .await
        .expect("#1218 shutdown after close is clean");
    let (owner, _key) = unconsumed_creation_pending(&h).await;
    let error = followup_bounded("1218 shutdown", owner.shutdown())
        .await
        .expect_err("#1218 shutdown aggregates the unconsumed creation error");
    assert_eq!(
        error.message, "Failed to shut down all agent sessions.",
        "#1218 shutdown aggregate shape (TS finishShutdown AggregateError)"
    );
    assert_eq!(
        error.causes.first().map(|cause| cause.message.as_str()),
        Some("FAILURE_1218"),
        "#1218 shutdown creation error identity"
    );
    h.finish("1218-close-creation").await;
}

// #1055 ② tests (c): the record holds an unconsumed creation error while its
// own disposal also fails — close/shutdown aggregate both errors instead of
// silently dropping the pending one (fix-r1 P1; the same drain failure may
// surface as both causes).
async fn unconsumed_creation_with_faulted_disposal(
    h: &Harness,
) -> (SessionOwner, pilotdeck_protocol::JsString) {
    let original = followup_bounded("1218 fault initial", h.factory.create(h.context(), None))
        .await
        .expect("#1218 fault initial factory session");
    let gate = Gate::new();
    let faulted = LiveSessionHandle::new(
        Arc::new(RetryFaultSession {
            inner: original.session_for_test(),
            mode: "error",
            gate: gate.clone(),
            snapshots: AtomicUsize::new(0),
        }),
        None,
    );
    let (owner, key) = unconsumed_creation_pending_with(h, faulted).await;
    gate.open();
    (owner, key)
}

#[tokio::test]
async fn recreation_unconsumed_creation_error_is_aggregated_with_failed_disposal_by_close() {
    let h = Harness::new("1218-close-creation-fault");
    let (owner, key) = unconsumed_creation_with_faulted_disposal(&h).await;
    let error = followup_bounded("1218 close", owner.close(&key))
        .await
        .expect_err("#1218 close aggregates the failed disposal and the pending creation error");
    assert_eq!(
        error.message,
        "Failed to dispose the agent session and report its unconsumed creation or cleanup error.",
        "#1218 close keeps both errors in one aggregate"
    );
    assert_eq!(
        error.causes.first().map(|cause| cause.message.as_str()),
        Some("Failed to dispose agent handle."),
        "#1218 close keeps the failed disposal error"
    );
    assert_eq!(
        error.causes.get(1).map(|cause| cause.message.as_str()),
        Some("FAILURE_1218"),
        "#1218 close keeps the pending creation error beside the failed disposal"
    );
    assert_eq!(
        owner.cached_session_count(),
        0,
        "#1218 close removed the record"
    );
    followup_bounded("1218 post-close shutdown", owner.shutdown())
        .await
        .expect("#1218 shutdown after close is clean");
    h.finish("1218-close-creation-fault").await;
}

#[tokio::test]
async fn recreation_unconsumed_creation_error_is_aggregated_with_failed_disposal_by_shutdown() {
    let h = Harness::new("1218-shutdown-creation-fault");
    let (owner, _key) = unconsumed_creation_with_faulted_disposal(&h).await;
    let error = followup_bounded("1218 shutdown", owner.shutdown())
        .await
        .expect_err("#1218 shutdown aggregates the failed disposal and the pending creation error");
    assert_eq!(
        error.message, "Failed to shut down all agent sessions.",
        "#1218 shutdown aggregate shape (TS finishShutdown AggregateError)"
    );
    let record_error = error
        .causes
        .first()
        .expect("#1218 shutdown carries the record's aggregated error");
    assert_eq!(
        record_error.message,
        "Failed to dispose the agent session and report its unconsumed creation or cleanup error.",
        "#1218 shutdown keeps both record errors in one aggregate"
    );
    assert_eq!(
        record_error
            .causes
            .first()
            .map(|cause| cause.message.as_str()),
        Some("Failed to dispose agent handle."),
        "#1218 shutdown keeps the failed disposal error"
    );
    assert_eq!(
        record_error
            .causes
            .get(1)
            .map(|cause| cause.message.as_str()),
        Some("FAILURE_1218"),
        "#1218 shutdown keeps the pending creation error"
    );
    h.finish("1218-shutdown-creation-fault").await;
}

#[tokio::test]
async fn writer_busy_budget_excludes_drained_snapshot() {
    let h = Harness::with_fence_timeout("r2-slow-snapshot", Duration::from_secs(5));
    let (old, _, _) = retry_fault(&h, "slow-snapshot").await;
    let held = Arc::new(Mutex::new(None));
    let blocker = held.clone();
    let observation = Arc::new(Mutex::new(None));
    let observe = observation.clone();
    let create_storage = h.factory.options.create_storage.clone();
    let factory = ProjectSessionFactory::new(FactoryOptions {
        writer_busy_wait_timeout: Duration::from_secs(5),
        prepare: h.factory.options.prepare.clone(),
        create_storage: Arc::new(move |context| {
            let storage = create_storage(context);
            let blocker = blocker.clone();
            let observe = observe.clone();
            Box::pin(async move {
                let storage = match storage.await? {
                    SessionStorage::Unlocked(storage) => storage,
                    SessionStorage::Recording(_) => {
                        panic!("#1022 r2 slow snapshot storage is unlocked")
                    }
                };
                let lease = acquire_writer(
                    storage.recorder.writer().path().to_owned(),
                    tokio::time::Instant::now() + Duration::from_secs(5),
                )
                .await?;
                *lock(&observe) = Some(writer_wait_observation(
                    tokio::sync::OwnedMutexGuard::mutex(&lease._process),
                ));
                *lock(&blocker) = Some(lease);
                Ok(SessionStorage::Unlocked(storage))
            })
        }),
        admission: None,
        diagnostic: Arc::new(|_| {}),
    });
    tokio::time::pause();
    let waiting = tokio::spawn(factory.create(h.context(), Some(old)));
    // Keep the test runnable while blocking filesystem work completes, so the
    // paused clock cannot auto-advance to the writer's timeout.
    let wait_state = followup_bounded("r2 process lock entered", async {
        loop {
            if let Some(observation) = lock(&observation).as_ref() {
                let state = observation.load(Ordering::SeqCst);
                if state != 0 {
                    break state;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(
        wait_state, 1,
        "#1022 r2 slow snapshot retains busy writer wait budget"
    );
    assert!(
        !waiting.is_finished(),
        "#1022 r2 occupied writer still waits"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    drop(lock(&held).take());
    tokio::time::resume();
    let new = followup_bounded("r2 snapshot retry", waiting)
        .await
        .expect("#1022 r2 snapshot worker")
        .expect("#1022 r2 slow snapshot retains busy writer wait budget");
    new.dispose("cleanup".into())
        .await
        .expect("#1022 r2 snapshot cleanup");
    h.finish("r2-slow-snapshot").await;
}
#[test]
#[ignore = "requires fixed TS baseline and Node v22.22.0"]
fn disposal_retry_regenerates_identically() {
    crate::test_baseline::regenerates_identically("session-disposal-retry");
}

#[tokio::test]
async fn recreation_old_error_waiter_cannot_clear_new_error() {
    struct FailsInOrder {
        initial: Mutex<Option<LiveSessionHandle>>,
        calls: Arc<AtomicUsize>,
    }
    impl SessionFactory for FailsInOrder {
        fn can_recreate(&self) -> bool {
            true
        }
        fn create(
            &self,
            _: SessionContext,
            _: Option<LiveSessionHandle>,
        ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
            if let Some(handle) = lock(&self.initial).take() {
                return Box::pin(async move { Ok(handle) });
            }
            let id = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move { Err(RuntimeError::new(format!("FAILURE_{id}"))) })
        }
    }
    let h = Harness::new("r2-error-identity");
    let initial = h
        .factory
        .create(h.context(), None)
        .await
        .expect("#1022 r2 identity initial");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut options = SessionOwnerOptions::new(Arc::new(FailsInOrder {
        initial: Mutex::new(Some(initial)),
        calls: calls.clone(),
    }));
    options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(options);
    owner
        .get_or_create(h.context())
        .await
        .expect("#1022 r2 identity cache");
    assert!(
        owner.mark_session_dirty(&h.context().session_key, "r2"),
        "#1022 r2 identity dirty"
    );
    let old_waiter = owner.get_or_create(h.context());
    followup_bounded("r2 identity first sent", async {
        while calls.load(Ordering::SeqCst) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let first = owner
        .get_or_create(h.context())
        .await
        .err()
        .expect("#1022 r2 identity replay first");
    assert_eq!(
        first.message, "FAILURE_1",
        "#1022 r2 identity first failure"
    );
    let new_waiter = owner.get_or_create(h.context());
    followup_bounded("r2 identity second sent", async {
        while calls.load(Ordering::SeqCst) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(
        old_waiter
            .await
            .err()
            .expect("#1022 r2 identity old waiter")
            .message,
        "FAILURE_1",
        "#1022 r2 identity old error retained"
    );
    drop(new_waiter);
    let second = owner
        .get_or_create(h.context())
        .await
        .err()
        .expect("#1022 r2 identity replay second");
    assert_eq!(
        second.message, "FAILURE_2",
        "#1022 r2 old waiter cannot acknowledge newer failure"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "#1022 r2 replay avoids new creation"
    );
    owner.shutdown().await.expect("#1022 r2 identity cleanup");
    h.finish("r2-error-identity").await;
}

#[tokio::test]
async fn recreation_idle_guard_drop_panic_cannot_skip_drain() {
    retry_idle_panic("guard-drop-panic").await;
}

/// Matrix cell 7 (#1010, _tmp/20261026-issue-1010/plan.md): PreparedResources
/// dropped on a thread with no runtime context. The registration-time
/// executor must still run the custom async release.
#[tokio::test]
async fn prepared_resources_released_when_dropped_off_runtime() {
    let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut resources = PreparedResources::default();
    resources.push({
        let released = released.clone();
        move || {
            Box::pin(async move {
                released.store(true, Ordering::SeqCst);
                Ok(())
            })
        }
    });
    let thread = std::thread::spawn(move || drop(resources));
    thread
        .join()
        .expect("#1010 off-runtime PreparedResources drop finished");
    bounded("#1010 off-runtime release ran", async {
        while !released.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        released.load(Ordering::SeqCst),
        "#1010 the release ran on the captured executor, not the ambient one"
    );
}

/// Matrix cell 8: the registration executor is gone when PreparedResources
/// drops. The spawned task is never polled, so the async release cannot run,
/// but the captured lease's synchronous Drop must still release the count.
#[tokio::test]
async fn prepared_resources_spawn_rejection_keeps_sync_semantics() {
    let h = Harness::new("prepared-dead-executor");
    let lease = h.registry.acquire(None).expect("#1010 lease acquired");
    let dedicated = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("#1010 dedicated runtime built");
    let resources = {
        let _guard = dedicated.enter();
        let mut resources = PreparedResources::default();
        resources.retain_lease(lease);
        resources
    };
    std::thread::spawn(move || drop(dedicated))
        .join()
        .expect("#1010 dedicated runtime dropped off the async context");
    drop(resources);
    assert_eq!(
        h.leases(),
        0,
        "#1010 dead executor: the lease count is still released synchronously"
    );
    h.finish("prepared-dead-executor").await;
}

/// Matrix cell 10: the real factory chain. Dropping the only handle clone —
/// without an explicit dispose and without the owner — must still run the
/// release events, close the storage and release the project lease.
#[tokio::test]
async fn dropped_owned_session_disposes_storage_and_releases_lease() {
    let h = Harness::new("handle-drop-e2e");
    let handle = bounded(
        "#1010 direct factory create",
        h.factory.create(h.context(), None),
    )
    .await
    .expect("#1010 factory created the session");
    drop(handle);
    bounded("#1010 release event after drop", async {
        while !lock(&h.observed.events).iter().any(|e| e == "release.1") {
            tokio::task::yield_now().await;
        }
    })
    .await;
    bounded("#1010 storage close after drop", async {
        while !lock(&h.observed.events).iter().any(|e| e == "close.1") {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(
        h.leases(),
        0,
        "#1010 dropped owned session releases the project lease"
    );
    h.finish("handle-drop-e2e").await;
}

// #1010: the only executor for the real handle is returned idle by a plain
// thread. Driving it is explicit; merely queuing Drop cannot poll the drain.
fn drop_plain<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        let _ = send.send(outcome);
    });
    receive
        .recv_timeout(Duration::from_secs(12))
        .expect("#1010 plain thread completed within its bound")
        .expect("#1010 plain thread did not panic")
}
fn drop_owned_runtime(h: &Harness) -> (LiveSessionHandle, tokio::runtime::Runtime) {
    let factory = h.factory.clone();
    let context = h.context();
    drop_plain(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("#1010 dedicated runtime built");
        let handle = runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(8), factory.create(context, None))
                .await
                .expect("#1010 dedicated create completed within its bound")
                .expect("#1010 dedicated create succeeded")
        });
        (handle, runtime)
    })
}
fn drop_drive_to_gate(
    runtime: tokio::runtime::Runtime,
    gate: Arc<Gate>,
) -> tokio::runtime::Runtime {
    drop_plain(move || {
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(8), gate.entered())
                .await
                .expect("#1010 drain reached the requested cleanup gate");
        });
        runtime
    })
}
fn drop_shutdown(runtime: tokio::runtime::Runtime) {
    drop_plain(move || runtime.shutdown_timeout(Duration::from_secs(2)));
}
async fn drop_bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(8), future)
        .await
        .expect("#1010 product observation completed within its bound")
}

// Each call is a separate libtest case below. No owner retains a handle clone.
// The release/close predecessor never finishes: shutdown must cancel it.
async fn drop_shutdown_matrix(stage: &str, pending: bool) {
    let h = Harness::with_fence_timeout(
        if stage == "release" {
            "drop-release-stage"
        } else {
            "dispose-close-stage"
        },
        Duration::from_secs(3),
    );
    let (handle, mut runtime) = drop_owned_runtime(&h);
    let writer = lock(&h.observed.bindings)
        .first()
        .expect("#1010 real factory published a binding")
        .recorder
        .writer()
        .clone();
    writer
        .record_session_metadata(
            &"session-745".into(),
            &"seed".into(),
            JsValue::Object(Default::default()),
        )
        .expect("#1010 old writer seeds the transcript");
    let path = writer.path().to_owned();
    let canonical =
        crate::storage::canonical_transcript_path(&path).expect("#1010 canonical transcript path");
    let mutex = writer_mutex(&canonical);
    let observation = writer_wait_observation(&mutex);
    let close_gate = install_close_thread_gate(&path);
    if stage == "dead" {
        let candidate = if pending {
            let _entered = runtime.enter();
            Some(PendingSession::new(handle.clone()))
        } else {
            None
        };
        drop_shutdown(runtime);
        drop_plain(move || {
            assert!(
                tokio::runtime::Handle::try_current().is_err(),
                "#1010 final drop has no ambient runtime"
            );
            drop(handle);
            drop(candidate);
        });
    } else {
        // Construct PendingSession while its captured executor is entered.
        let candidate = if pending {
            let _entered = runtime.enter();
            Some(PendingSession::new(handle.clone()))
        } else {
            None
        };
        drop_plain(move || {
            assert!(
                tokio::runtime::Handle::try_current().is_err(),
                "#1010 queued drop has no ambient runtime"
            );
            drop(handle);
            drop(candidate);
        });
        if stage == "release" {
            runtime = drop_drive_to_gate(runtime, h.drain.clone());
        } else if stage == "close" {
            runtime = drop_drive_to_gate(runtime, h.gate.clone());
        }
        drop_shutdown(runtime);
    }
    let expected_release = usize::from(stage == "close");
    assert_eq!(
        h.leases(),
        0,
        "#1010 shutdown releases the project lease synchronously"
    );
    assert_eq!(
        lock(&h.observed.events)
            .iter()
            .filter(|e| *e == "release.1")
            .count(),
        expected_release,
        "#1010 shutdown release hook completes exactly the expected number of times"
    );
    assert_eq!(
        lock(&h.observed.events)
            .iter()
            .filter(|e| *e == "close.1")
            .count(),
        0,
        "#1010 shutdown never completes the parked storage close"
    );
    let before = std::fs::read(&path).expect("#1010 seeded transcript readable");
    assert!(!before.is_empty(), "#1010 seeded transcript has bytes");
    writer
        .record_session_metadata(
            &"session-745".into(),
            &"retired".into(),
            JsValue::Object(Default::default()),
        )
        .expect("#1010 retired writer silently rejects the append");
    assert_eq!(
        std::fs::read(&path).expect("#1010 retired transcript readable"),
        before,
        "#1010 shutdown writer rejects further bytes before fence release"
    );
    assert!(
        mutex.clone().try_lock_owned().is_err(),
        "#1010 pinned close retains the process fence"
    );
    assert!(
        matches!(crate::storage::LedgerLock::acquire(&path),
        Err(crate::storage::LedgerError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock),
        "#1010 pinned close retains the file fence"
    );

    // Cancellation of one create waiter must not unlock the predecessor. The
    // factory construct survives its caller; poll the waiter
    // through the actual lock attempt, then abort and reap it.
    observation.store(0, Ordering::SeqCst);
    let cancelled = tokio::spawn(h.factory.create(h.context(), None));
    drop_bounded(async {
        while observation.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        !cancelled.is_finished(),
        "#1010 cancelled successor waits for the pinned fence"
    );
    cancelled.abort();
    assert!(
        drop_bounded(cancelled)
            .await
            .is_err_and(|e| e.is_cancelled()),
        "#1010 successor cancellation is reaped"
    );
    assert!(
        mutex.clone().try_lock_owned().is_err(),
        "#1010 cancelled successor preserves predecessor fence"
    );

    // A never-finishing predecessor must return the product's bounded BUSY
    // error, not an executor/test timeout. This path avoids wall-clock sleeps.
    let busy = acquire_process_writer(mutex.clone(), tokio::time::Instant::now()).await;
    assert!(
        busy.is_err_and(
            |e| e.js_error().and_then(|e| e.code.as_deref()) == Some("SESSION_WRITER_BUSY")
        ),
        "#1010 never-finished predecessor returns bounded writer busy"
    );
    observation.store(0, Ordering::SeqCst);
    let successor = tokio::spawn(h.factory.create(h.context(), None));
    drop_bounded(async {
        while observation.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        !successor.is_finished(),
        "#1010 successor cannot acquire a pinned writer"
    );
    drop(close_gate);
    let successor = drop_bounded(successor)
        .await
        .expect("#1010 successor task completes")
        .expect("#1010 successor acquires writer after fence release");
    drop_bounded(successor.dispose("#1010 successor cleanup".into()))
        .await
        .expect("#1010 successor disposal succeeds");
    assert_eq!(
        lock(&h.observed.events)
            .iter()
            .filter(|e| *e == "release.1")
            .count(),
        expected_release,
        "#1010 shutdown never restarts the old release hook"
    );
    // The real factory's MCP registry owns shutdown of registrations whose
    // async resources were cancelled; close it explicitly after the checks.
    h.finish("#1010 shutdown matrix").await;
}

#[tokio::test]
async fn dropped_owned_session_shutdown_before_first_poll() {
    drop_shutdown_matrix("queued", false).await;
}
#[tokio::test]
async fn dropped_owned_session_shutdown_during_release() {
    drop_shutdown_matrix("release", false).await;
}
#[tokio::test]
async fn dropped_owned_session_shutdown_during_close() {
    drop_shutdown_matrix("close", false).await;
}
#[tokio::test]
async fn dropped_owned_session_after_executor_shutdown_without_runtime() {
    drop_shutdown_matrix("dead", false).await;
}
#[tokio::test]
async fn pending_owned_session_shutdown_before_first_poll() {
    drop_shutdown_matrix("queued", true).await;
}
#[tokio::test]
async fn pending_owned_session_after_executor_shutdown_without_runtime() {
    drop_shutdown_matrix("dead", true).await;
}

#[test]
#[ignore = "requires the pinned Node v22.22.0 TS baseline"]
fn projection_cache_regenerates_identically() {
    crate::test_baseline::regenerates_identically("session-factory/projection-cache");
}

#[test]
#[ignore = "bounded append benchmark; timing is not a CI assertion"]
fn projection_cache_append_benchmark() {
    struct Sink;
    impl TranscriptStore for Sink {
        fn append(&self, _: &TranscriptKey, _: &JsValue) -> crate::storage::StoreResult<()> {
            Ok(())
        }
        fn read(
            &self,
            _: &TranscriptKey,
            _: Option<u64>,
        ) -> crate::storage::StoreResult<TranscriptRead> {
            Ok(TranscriptRead {
                entries: vec![],
                diagnostics: vec![],
            })
        }
    }
    let started = std::time::Instant::now();
    for count in [256, 512, 1024] {
        let storage = FactoryStorage::new(
            Arc::new(Sink),
            TranscriptKey {
                project_root: PathBuf::from("/unused-1011"),
                pilot_home: PathBuf::from("/unused-1011"),
                session_id: "session-1011".into(),
                transcript_path: PathBuf::from("/unused-1011/bench.jsonl"),
            },
        );
        let begin = std::time::Instant::now();
        for _ in 0..count {
            storage
                .recorder
                .writer()
                .record_session_metadata(
                    &"session-1011".into(),
                    &"bench".into(),
                    js(r#"{"title":"fixed","titleSource":"user"}"#),
                )
                .expect("#1011 benchmark append");
        }
        println!(
            "#1011 append n={count} elapsed_us={} ns_per_append={}",
            begin.elapsed().as_micros(),
            begin.elapsed().as_nanos() / count
        );
        assert_eq!(
            storage.recorder.snapshot_state().0,
            count as u64,
            "#1011 benchmark sequence"
        );
        assert_eq!(
            storage
                .projection
                .snapshot()
                .expect("#1011 benchmark projection")
                .metadata
                .get("title"),
            Some(&JsValue::String("fixed".into())),
            "#1011 benchmark title"
        );
        if started.elapsed() >= Duration::from_secs(30) {
            println!("#1011 benchmark budget reached; no further sizes");
            break;
        }
    }
}

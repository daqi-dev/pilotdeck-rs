//! Matrix coverage for `host_factory.rs`: deployment selection against the TS
//! oracle, lease ownership across reload/spawn/handshake/cancel, and the
//! router manifest against the 48-case host-manifest goldens.
use super::*;
use crate::{
    host::{
        HostFuture, HostHandler,
        modules::{
            CompositionSwitches, capability, context,
            model_port::RouterModelPortOptions,
            tool_context::{
                ContextRequestIdentity, HostToolCheckpoint, ToolContextPorts, TurnBinding,
                TurnEnvironment,
            },
            turn,
        },
    },
    runtime::{
        hooks::Cancellation,
        permission::{self, DefaultPermissionContextOptions},
        project::{
            Families, Family, InteractionProfileName, ProjectResourcesFactory,
            ProjectResourcesOptions, ProjectRuntime, ProjectRuntimeRegistry, ResourceFamilySource,
            RuntimeProfile, RuntimeProfileOverrides, RuntimeResult, StageContext, StageSink,
            interaction_permission::PermissionTurnOptions,
        },
        session::LiveSession,
        steer::MutationRecorder,
        turn::{interaction::InteractionTurn, live::test_recorder},
    },
};
use pilotdeck_protocol::{
    JsObject, JsString, JsValue, admission::ExecutionIdentity, parse_json, stringify_json,
};
use std::{
    future::Future,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};
use tokio::{sync::Notify, time::timeout};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/host-factory");

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "pdrs-host-factory-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const BASE_CONFIG: &str = "schemaVersion: 1\nagent: {model: test/mock}\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock\n      models:\n        mock: {}\n";
fn config(root: &Path, text: &str) {
    std::fs::create_dir_all(root.join(".pilotdeck")).unwrap();
    std::fs::write(root.join("pilotdeck.yaml"), text).unwrap();
}
struct NoopFamily(Family);
impl ResourceFamilySource for NoopFamily {
    fn family(&self) -> Family {
        self.0
    }
    fn stage(&self, _: &StageContext, _: &mut StageSink<'_>) -> RuntimeResult {
        Ok(())
    }
}
fn registry(scratch: &Scratch) -> Arc<ProjectRuntimeRegistry> {
    registry_with_config(scratch, BASE_CONFIG)
}
fn registry_with_config(scratch: &Scratch, config_text: &str) -> Arc<ProjectRuntimeRegistry> {
    let root = scratch.0.join("project");
    let home = scratch.0.join("home");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&home).unwrap();
    config(&home, config_text);
    let mut env = Env::new();
    env.insert("PILOT_HOME".into(), home.to_string_lossy().into_owned());
    let factory = ProjectResourcesFactory::new(ProjectResourcesOptions {
        pilot_home: home.clone(),
        cwd: root.clone(),
        env,
        homedir: Some(home.clone()),
        builtin_skills_root: None,
        now: Arc::new(|| SystemTime::UNIX_EPOCH),
        telemetry: None,
        config_version: None,
        runtime_profile_overrides: RuntimeProfileOverrides::default(),
        families: Families {
            lsp: Some(Arc::new(NoopFamily(Family::LspService))),
            session_title: Some(Arc::new(NoopFamily(Family::SessionTitleProvider))),
            ..Families::unwired()
        }
        .with_native_models()
        .with_context_memory()
        .with_tools_mcp()
        .with_plugins(crate::runtime::plugins::PluginRuntimeOptions {
            project_root: root.clone(),
            pilot_home: home.clone(),
            builtin_skills_root: None,
            builtin_plugins: Vec::new(),
            builtin_plugins_enabled: std::collections::HashMap::new(),
        })
        .with_interaction(),
    });
    Arc::new(ProjectRuntimeRegistry::new(root, Arc::new(factory)))
}

fn identity() -> ExecutionIdentity {
    ExecutionIdentity {
        session_id: "session".into(),
        turn_id: "turn".into(),
        run_id: "run".into(),
        operation_id: "operation".into(),
        request_id: "request".into(),
    }
}
fn wire() -> JsValue {
    parse_json(r#"{"sessionId":"session","turnId":"turn","runId":"run"}"#).unwrap()
}
async fn bounded<T>(future: impl Future<Output = T>) -> T {
    timeout(Duration::from_secs(10), future)
        .await
        .expect("bounded wait timed out")
}
/// #978: liveness margin for the awaits in this test that cannot complete
/// without the real Python sidecar: the command-entry gate, the next stream
/// frame, and the final `registry.dispose()` that waits for the child to exit.
/// The product's own per-frame waits are 60 s (handshake and io), but wrapping
/// these in the shared 10 s `bounded()` capped them, so raising `Limits` alone
/// left an effective 10 s margin. This is a watchdog, not an assertion: every
/// await still has to complete, and no assertion or threshold changed.
/// Measured on cb8 (20 cores, 32 busy-loop burners, load 34; 24 runs, 3696
/// awaits timestamped, per label): sidecar `next` max 282 ms, gate entry
/// max 41 ms, and every in-process await together (dispose/enqueue/mailbox)
/// max 9.6 ms, so 60 s leaves ~213x over the slowest sidecar await observed
/// there. The historical false
/// reds came from a host where the same sidecar went past the 5 s io default;
/// 60 s is no longer capped at 10 s by the outer bound.
///
/// In-process state operations (session dispose, steer enqueue, mailbox
/// enqueue) keep the shared 10 s `bounded()` below: they never wait for the
/// sidecar, the same measurement put all of them under 10 ms, and widening
/// them would only make a hung mailbox or disposer take six times longer to
/// surface (#978 second review).
///
/// #1891: the same margin now also wraps the sibling peer-fixture tests in
/// this file (the live-session / reload / retire / dispose-cancel / paused
/// cleanup / spawn tests' peer-driven `turn.next()` frames, the child-exit
/// awaits of `wait_disposed`/final `registry.dispose()`, and `drive_to_peer`'s
/// pid-publication phase) and their `factory()`/`TurnHostFactory::new`
/// `Limits` — the same source #978 measured, previously left at the defaults.
/// Awaits that only observe in-process state (reload/retire transitions,
/// never-spawned commands, steer mailbox probes) deliberately keep the
/// shared 10 s bound; each keep is exempted in the #1891 result table.
const SIDECAR_LIVENESS: Duration = Duration::from_secs(60);

/// #2054: the `steer-insert` case watches the transcript while the host is
/// still closing its per-turn scope. `TurnRunner::complete()` cannot run
/// before that scope releases the terminal frame, and the scope's wait for the
/// child is bounded by `close_timeout` — so the observation window has to
/// outlast that bound. A fixed 100 ms window closed long before
/// `complete_turn` was even attempted once `close_timeout` went back to the
/// product default of 1 s, which is exactly how "steer closed before the
/// durable write" stopped being observable.
const STEER_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
/// Derived rather than a second literal plus a guard: an `as_secs()`-style
/// check truncates sub-second values, so `TIMEOUT = 1500 ms` with a 5 s window
/// would still have passed while sitting below the 5× bound.
///
/// Five rather than two: the only term this test can pin is `close_timeout`.
/// What follows it — the child's kill + reap, the task wake-up, and the
/// blocking-pool queue ahead of the durable write — has no hard bound, and the
/// mutant's write has to land inside the window for this case to stay KILLED,
/// so the margin is spent on that tail. The window is what makes the reverse
/// order observable at all; it is not a claim that the tail is bounded.
const STEER_CLOSE_WINDOW: Duration =
    Duration::from_nanos(5 * STEER_CLOSE_TIMEOUT.as_nanos() as u64);
const STEER_CLOSE_STEP: Duration = Duration::from_millis(5);

async fn bounded_liveness<T>(future: impl Future<Output = T>) -> T {
    match timeout(SIDECAR_LIVENESS, future).await {
        Ok(value) => value,
        Err(_elapsed) => {
            // #1891: receipt-wired watchdog failure; the panic text is the
            // same expect message, only the negative-control protocol sees
            // the extra step.
            crate::test_negative_receipt::record_failure("sidecar liveness wait timed out");
            panic!("sidecar liveness wait timed out")
        }
    }
}
// dispose signals cancellation; a detached handler disposer may still own the
// last lease after the stream ends. Observe release with a deadline rather than
// treating either stream EOF or child reaping as a lease barrier (#842).
async fn wait_for_lease_release(
    runtime: &ProjectRuntime,
) -> Result<(), tokio::time::error::Elapsed> {
    timeout(
        Duration::from_secs(if crate::mutants::on("late-deadline") {
            20
        } else {
            10
        }),
        async {
            if crate::mutants::on("old-instantaneous-assertion") {
                assert_eq!(
                    runtime.lease_count(),
                    0,
                    "#996 lease must already be released at stream end"
                );
            } else if crate::mutants::on("skip-observation") {
                let _ = runtime;
            } else {
                while runtime.lease_count() != 0 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        },
    )
    .await
}
/// Wait for the abort-driven mailbox close to land. `SteerMailbox::close`
/// clears the admission flag synchronously, before it serializes its discard
/// loop, so the flag is the event a durable insert is rejected against. The
/// close travels `abort()` (token only) -> monitor task -> `Run::close` ->
/// cleanup task, and under CPU contention that chain can take longer than any
/// fixed window; betting wall-clock time on it made #1847 flaky. Uses the same
/// 10 s in-process bound as the lease observation above (#1847).
async fn wait_for_mailbox_close(
    mailbox: &crate::runtime::steer::SteerMailbox,
) -> Result<(), tokio::time::error::Elapsed> {
    timeout(Duration::from_secs(10), async {
        while mailbox.open_for_test() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
}
/// Regex-safe argv token unique to one test's pid path. `peer()` appends it so
/// the reaping assertion can prove absence even when the child died before it
/// published its pid.
fn peer_marker(pid: &Path) -> String {
    let mut marker = String::from("pdrs_peer_");
    marker.extend(pid.to_string_lossy().bytes().map(|byte| {
        if byte.is_ascii_alphanumeric() {
            byte as char
        } else {
            '_'
        }
    }));
    marker
}
fn peer(mode: &str, pid: Option<&Path>) -> Deployment {
    let mut env = Env::new();
    env.insert("PATH".into(), std::env::var("PATH").unwrap_or_default());
    env.insert("PDRS_PEER_MODE".into(), mode.into());
    if let Some(pid) = pid {
        env.insert("PDRS_PEER_PID".into(), pid.to_string_lossy().into_owned());
    }
    if crate::mutants::on("gates-multi-turn-peer-marker") {
        // #996: the sidecar's execute marker loses its per-PID suffix, so the
        // second turn collides on the shared exclusive file.
        env.insert("PDRS_PEER_SHARED_MARKER".into(), "1".into());
    }
    env.insert("PILOTDECK_AGENT_LOOP_TRANSPORT".into(), "stdio".into());
    env.insert(
        "PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND".into(),
        "python3".into(),
    );
    env.insert(
        "PILOTDECK_AGENT_LOOP_SIDECAR_PATH".into(),
        format!("{FIXTURE}/peer.py"),
    );
    let mut deployment = resolve_deployment(&env, Path::new("/tmp")).unwrap();
    if let (Deployment::Stdio { args, .. }, Some(pid)) = (&mut deployment, pid) {
        // Put the pid path (and its marker) in argv as well: the published pid
        // file is the fast path, the marker proves absence when it is missing.
        args.push(mode.into());
        args.push(pid.to_string_lossy().into_owned());
        args.push(peer_marker(pid));
    }
    deployment
}
fn factory(
    registry: Arc<ProjectRuntimeRegistry>,
    deployment: Deployment,
    context_runtime: Option<Arc<dyn context::ContextRuntime>>,
) -> TurnHostFactory {
    TurnHostFactory::new(
        registry,
        deployment,
        // #1891: every `factory()` deployment is a real Python sidecar (the
        // live peer fixtures) or a command that fails before any child exists,
        // so the product's own per-frame waits carry the #978/#1851 liveness
        // margin instead of the 10 s handshake / 5 s io defaults — a host
        // running eight concurrent lib suites starved a peer past the io
        // default. These are watchdogs, not assertions: a sidecar that truly
        // hangs or rejects the handshake still fails closed, and the
        // never-spawned callers never reach the waits at all.
        Limits {
            handshake_timeout: SIDECAR_LIVENESS,
            io_timeout: SIDECAR_LIVENESS,
            ..Default::default()
        },
        Some(JsString::from("fixture")),
        move |_, _| ports(context_runtime.clone()),
    )
}

// -- session-owned ports ------------------------------------------------------------
struct NoEnvironment;
impl TurnEnvironment for NoEnvironment {
    fn build(
        &self,
        _: Option<&JsValue>,
        _: &JsString,
        _: &JsString,
        _: &JsString,
    ) -> Result<JsObject, HostError> {
        Ok(JsObject::new())
    }
}
struct NoopRecorder;
impl MutationRecorder for NoopRecorder {
    fn record(&self, _: JsString, _: JsObject) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
}
fn context_identity() -> ContextRequestIdentity {
    let config = parse_json(
        r#"{"cwd":"/workspace","permissionMode":"default","env":{},"permissionContext":{}}"#,
    )
    .unwrap()
    .as_object()
    .unwrap()
    .clone();
    let input = parse_json(r#"{"sessionId":"session","turnId":"turn"}"#)
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    ContextRequestIdentity::new(
        TurnBinding {
            config,
            input,
            checkpoint: HostToolCheckpoint::new(None, &[]).unwrap(),
            abort_signal: None,
            environment: Arc::new(NoEnvironment),
        },
        ToolContextPorts::default(),
    )
}
fn interaction(signal: Cancellation) -> InteractionTurnInput {
    InteractionTurnInput {
        permission: PermissionTurnOptions {
            profile: RuntimeProfile::resolve(
                &JsValue::Undefined,
                &RuntimeProfileOverrides {
                    interaction_profile_override: Some(InteractionProfileName::Disabled),
                    ..Default::default()
                },
            )
            .interaction,
            context: permission::create_default_permission_context(
                DefaultPermissionContextOptions {
                    cwd: "/workspace".into(),
                    can_prompt: Some(true),
                    ..Default::default()
                },
            ),
            rules: crate::config::permission::RuleSetHandle::empty(),
            coordinator: Arc::default(),
            emit: Arc::new(|_| true),
            timeout: Duration::from_millis(50),
        },
        hooks: vec![],
        sdk_hook_state: None,
        sdk_file_slot: None,
        hook_options: None,
        session_hook_options: None,
        contribution: None,
        base_env: None,
        signal,
        steer_recorder: Some(Arc::new(NoopRecorder)),
        record_message: Some(Arc::new(|_| Box::pin(async { Ok(()) }))),
        messages: vec![],
        switches: CompositionSwitches {
            plan_todo: false,
            elicitation_available: Some(true),
        },
        turn_ports: Default::default(),
    }
}
struct ToolContext;
impl capability::ToolContext for ToolContext {
    fn prepare(&self, _: JsValue) -> HostFuture<capability::ToolInvocationContext> {
        Box::pin(async {
            Ok(capability::ToolInvocationContext {
                runtime: Arc::new(()),
                execution: Arc::new(()),
            })
        })
    }
}
struct Observer;
impl capability::CapabilityResultObserver for Observer {
    fn on_capability_results(&self, _: Vec<JsObject>) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
}
struct Plan;
impl capability::PlanTodoHandler for Plan {
    fn call(&self, _: JsValue) -> HostFuture<JsObject> {
        Box::pin(async { Ok(JsObject::new()) })
    }
}
struct Tools(Vec<Arc<dyn capability::ToolDefinition>>);
impl capability::ToolCatalog for Tools {
    fn list(&self) -> HostFuture<Vec<Arc<dyn capability::ToolDefinition>>> {
        let tools = self.0.clone();
        Box::pin(async move { Ok(tools) })
    }
}
impl capability::ToolExecution for Tools {
    fn execute_all(
        &self,
        _: Vec<capability::ToolCall>,
        _: capability::ToolInvocationContext,
    ) -> HostFuture<Vec<JsObject>> {
        Box::pin(async { panic!("unused tool execution") })
    }
}
fn ports(context_runtime: Option<Arc<dyn context::ContextRuntime>>) -> SessionPorts {
    ports_with_tool_results_dir(
        context_runtime,
        std::env::temp_dir().join("pdrs-host-factory-tools"),
    )
}
fn ports_with_tool_results_dir(
    context_runtime: Option<Arc<dyn context::ContextRuntime>>,
    tool_results_dir: PathBuf,
) -> SessionPorts {
    let signal = Cancellation::new();
    SessionPorts {
        model: RouterModelPortOptions::main_agent(),
        recorder: None,
        tool_results_dir,
        context_runtime,
        context_identity: context_identity(),
        context_config: JsObject::new(),
        tool_execution: Arc::new(Tools(vec![])),
        tool_context: Arc::new(ToolContext),
        tool_observer: Arc::new(Observer),
        plan_todo: None,
        turn: turn::Ports::default(),
        interaction: interaction(signal),
    }
}

fn field<'a>(value: &'a JsValue, key: &str) -> Option<&'a JsValue> {
    value.get(key)
}
fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => panic!("expected array: {value:?}"),
    }
}
fn text(value: &JsValue) -> String {
    value.as_string().expect("string").to_utf8().expect("utf8")
}

// -- deployment ---------------------------------------------------------------------
#[test]
fn deployment_profile_matches_ts_oracle() {
    let oracle = parse_json(include_str!(
        "../../../tests/fixtures/host-factory/oracle.json"
    ))
    .expect("oracle.json");
    for spec in array(field(&oracle, "deployment").unwrap()) {
        let name = spec
            .get("name")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        let env: Env = spec
            .get("env")
            .unwrap()
            .as_object()
            .unwrap()
            .entries()
            .iter()
            .map(|(key, value)| {
                (
                    key.to_utf8().unwrap(),
                    value.as_string().unwrap().to_utf8().unwrap(),
                )
            })
            .collect();
        let cwd = PathBuf::from(
            spec.get("cwd")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap(),
        );
        let actual = resolve_deployment(&env, &cwd);
        if spec.get("ok").and_then(JsValue::as_bool) == Some(true) {
            let expected = spec.get("value").unwrap();
            match actual.unwrap() {
                Deployment::Native => {
                    assert_eq!(
                        text(field(expected, "transport").unwrap()),
                        "native",
                        "{name}"
                    );
                }
                Deployment::Tcp {
                    host,
                    port,
                    connect_timeout_ms,
                } => {
                    assert_eq!(text(field(expected, "transport").unwrap()), "tcp", "{name}");
                    assert_eq!(text(field(expected, "host").unwrap()), host, "{name}");
                    assert_eq!(
                        field(expected, "port").and_then(JsValue::as_number),
                        Some(port as f64),
                        "{name}"
                    );
                    assert_eq!(
                        field(expected, "connectTimeoutMs").and_then(JsValue::as_number),
                        connect_timeout_ms.map(|value| value as f64),
                        "{name}"
                    );
                }
                Deployment::Stdio {
                    command, args, env, ..
                } => {
                    assert_eq!(
                        text(field(expected, "transport").unwrap()),
                        "stdio",
                        "{name}"
                    );
                    assert_eq!(text(field(expected, "command").unwrap()), command, "{name}");
                    let expected_args: Vec<_> = array(field(expected, "args").unwrap())
                        .iter()
                        .map(text)
                        .collect();
                    assert_eq!(expected_args, args, "{name}");
                    let expected_env = field(expected, "env").unwrap().as_object().unwrap();
                    assert_eq!(expected_env.len(), env.len(), "{name}");
                    for (key, value) in expected_env.entries() {
                        assert_eq!(
                            env.get(&key.to_utf8().unwrap()),
                            Some(&text(value)),
                            "{name} env {key:?}"
                        );
                    }
                }
            }
        } else {
            assert_eq!(
                actual.unwrap_err(),
                text(field(spec, "error").unwrap()),
                "{name}"
            );
        }
    }
}
#[tokio::test]
async fn budget_uses_configured_model_pricing_from_lease() {
    let oracle = parse_json(include_str!(
        "../../../tests/fixtures/host-factory/oracle.json"
    ))
    .unwrap();
    let pricing = field(&oracle, "pricing").unwrap();
    let provider = text(field(pricing, "provider").unwrap());
    let model = text(field(pricing, "model").unwrap());
    let key = format!("{provider}/{model}");
    let rates = field(pricing, "modelPricing")
        .unwrap()
        .as_object()
        .unwrap()
        .get(&key)
        .unwrap()
        .as_object()
        .unwrap();
    let rate = |key: &str| rates.get(key).and_then(JsValue::as_number).unwrap();
    let config_text = format!(
        "{BASE_CONFIG}router:\n  stats:\n    modelPricing:\n      {key}: {{input: {}, output: {}, cacheRead: {}}}\n",
        rate("input"),
        rate("output"),
        rate("cacheRead")
    );
    let scratch = Scratch::new();
    let registry = registry_with_config(&scratch, &config_text);
    let lease = registry.acquire(None).unwrap();
    let (router, _, lease) = TurnHostFactory::assemble(lease, &identity(), ports(None))
        .await
        .unwrap();
    let usage = field(pricing, "usage").unwrap();
    let call = parse_json(&format!(
        r#"{{"payload":{{"operation":"estimate_usage_cost","usage":{},"provider":"{provider}","model":"{model}"}}}}"#,
        stringify_json(usage).unwrap().unwrap()
    ))
    .unwrap();
    let response = router.budget.call(call).await.unwrap();
    let expected = field(pricing, "costUsd")
        .and_then(JsValue::as_number)
        .unwrap();
    let actual = response
        .get("costUsd")
        .and_then(JsValue::as_number)
        .unwrap();
    assert_eq!(
        actual, expected,
        "configured modelPricing must reach the lease budget port"
    );
    drop(lease);
    bounded(registry.dispose()).await.unwrap();
}

fn assert_ts_lease(name: &str, leases: usize, disposals: usize, state: &str) {
    let oracle = parse_json(include_str!(
        "../../../tests/fixtures/host-factory/oracle.json"
    ))
    .unwrap();
    let lease = array(field(&oracle, "lease").unwrap());
    let case = lease.iter().find(|case| {
        case.get("name")
            .and_then(JsValue::as_string)
            .is_some_and(|value| value.to_utf8().ok().as_deref() == Some(name))
    });
    let case = case.unwrap_or_else(|| panic!("missing lease case {name}"));
    assert_eq!(
        case.get("leases").and_then(JsValue::as_number),
        Some(leases as f64)
    );
    assert_eq!(
        case.get("disposals").and_then(JsValue::as_number),
        Some(disposals as f64)
    );
    assert_eq!(
        case.get("state")
            .and_then(JsValue::as_string)
            .map(|s| s.to_utf8().unwrap()),
        Some(state.into())
    );
}

#[tokio::test]
async fn live_session_shares_recorder_and_reads_current_history() {
    let scratch = Scratch::new();
    let registry = registry(&scratch);
    let recorder = test_recorder();
    let expected_recorder = recorder.clone();
    let port_calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = port_calls.clone();
    let history = Arc::new(Mutex::new(Vec::new()));
    let current_history = history.clone();
    let history_calls = Arc::new(AtomicUsize::new(0));
    let observed_history = history_calls.clone();
    let pid_file = scratch.0.join("shared.pid");
    let session = TurnHostFactory::new(
        registry.clone(),
        peer("success", Some(&pid_file)),
        // #1891: same-source sibling of `factory()` above — a real `peer.py`
        // under `Limits::default()` is the exact shape #978 measured reddening
        // under load, so this construction gets the same liveness margin.
        Limits {
            handshake_timeout: SIDECAR_LIVENESS,
            io_timeout: SIDECAR_LIVENESS,
            ..Default::default()
        },
        Some(JsString::from("fixture")),
        move |_, shared| {
            assert!(
                Arc::ptr_eq(&shared, &expected_recorder),
                "#741k ports factory must receive the session Recorder"
            );
            observed_calls.fetch_add(1, Ordering::SeqCst);
            ports(None)
        },
    )
    .into_live_session(recorder.clone(), move || {
        observed_history.fetch_add(1, Ordering::SeqCst);
        current_history
            .lock()
            .expect("#741k input fixture setup")
            .clone()
    });
    assert_eq!(
        port_calls.load(Ordering::SeqCst),
        0,
        "#741k input fixture invariant"
    );
    assert_eq!(
        history_calls.load(Ordering::SeqCst),
        0,
        "#741k input fixture invariant"
    );
    for index in 0..2 {
        let previous = parse_json(&format!(
            r#"{{"role":"assistant","content":"history-{index}"}}"#
        ))
        .expect("#741k input fixture setup");
        *history.lock().expect("#741k input fixture setup") = vec![previous.clone()];
        let mut turn = session
            .submit_turn_for_test(
                parse_json(r#"{"type":"text","text":"hello"}"#).expect("#741k input fixture setup"),
                {
                    let mut options = wire()
                        .as_object()
                        .expect("#741k input fixture setup")
                        .clone();
                    options.insert(
                        "turnId".into(),
                        JsValue::String(format!("history-{index}").into()),
                    );
                    JsValue::Object(options)
                },
            )
            .expect("#741k input fixture setup");
        let mut completed = None;
        while let Some(event) = bounded_liveness(turn.next())
            .await
            .expect("#741k input fixture setup")
        {
            if event.get("type") == Some(&"turn_completed".into()) {
                completed = Some(event);
            }
        }
        let completed = completed.expect("#741k peer completed the turn");
        assert_eq!(
            completed
                .get("result")
                .and_then(|result| result.get("type")),
            Some(&"success".into()),
            "#741k every history turn must execute successfully in its own sidecar"
        );
        let mut previous = previous.as_object().expect("#748 history object").clone();
        previous.insert(
            "metadata".into(),
            parse_json(r#"{"moduleId":"fixture"}"#).expect("#748 module ownership"),
        );
        let previous = JsValue::Object(previous);
        assert_eq!(
            completed.get("messages"),
            Some(&JsValue::Array(vec![
                previous,
                parse_json(r#"{"role":"user","content":[{"type":"text","text":"hello"}]}"#)
                    .expect("#741k input fixture setup"),
            ])),
            "#741k fresh history must reach the sidecar on every submit"
        );
        let bytes =
            std::fs::read_to_string(recorder.writer().path()).expect("#741k input fixture setup");
        assert_eq!(
            bytes
                .lines()
                .map(|line| parse_json(line).expect("#741k input fixture setup"))
                .filter(|row| row.get("type") == Some(&"accepted_input".into()))
                .count(),
            index + 1,
            "#741k each factory turn must persist accepted_input"
        );
        assert_eq!(
            port_calls.load(Ordering::SeqCst),
            index + 1,
            "#741k input fixture invariant"
        );
        assert_eq!(
            history_calls.load(Ordering::SeqCst),
            index + 1,
            "#741k input fixture invariant"
        );
        assert_peer_reaped(&pid_file).await;
    }
    session.dispose().await.expect("#741k input fixture setup");
    bounded_liveness(registry.dispose())
        .await
        .expect("#741k input fixture setup");
}

// #885 fix-r1: a deleted-cwd composition failure must reach the turn stream as
// the original TS uv_cwd diagnostic. The session-context bundle in TS composes
// inside ProjectSessionFactory.createSession and throws the constructor error
// unchanged (SessionContextRuntimeBundle.ts:85, ProjectSessionFactory.ts:160-163);
// the Rust composition entry assembles lazily per turn, so the observable parity
// is: submit is admitted, and the first stream poll returns the exact message
// before any lifecycle event was delivered.
const SESSION_CWD_WORKER: &str =
    "runtime::turn::host_factory::tests::session_cwd_environment_worker";

#[test]
fn session_composition_uv_cwd_error_reaches_stream_unwrapped() {
    let scratch = Scratch::new();
    let root = std::fs::canonicalize(&scratch.0).expect("#885 canonical scratch root");
    std::fs::create_dir(root.join("cwd")).expect("#885 scratch cwd directory");
    crate::test_process::run_cases(
        SESSION_CWD_WORKER,
        vec![(
            "relative-deleted".into(),
            vec![
                ("PDRS_SESSION_CWD_CASE".into(), "relative-deleted".into()),
                (
                    "PDRS_SESSION_CWD_ROOT".into(),
                    root.to_str().expect("#885 UTF-8 scratch root").into(),
                ),
            ],
        )],
    );
}

#[tokio::test]
#[ignore = "worker only; parent supplies isolated cwd and checks one receipt"]
async fn session_cwd_environment_worker() {
    let name = std::env::var("PDRS_SESSION_CWD_CASE").expect("#885 parent case");
    let root = std::env::var("PDRS_SESSION_CWD_ROOT").expect("#885 parent scratch root");
    let oracle = parse_json(include_str!(
        "../../../tests/fixtures/tool-budget-cwd/oracle.json"
    ))
    .expect("#885 tool-budget-cwd oracle JSON");
    let expected = oracle
        .get("cases")
        .and_then(|cases| match cases {
            JsValue::Array(items) => Some(items),
            _ => None,
        })
        .expect("#885 oracle cases")
        .iter()
        .find(|row| {
            row.get("name")
                .and_then(JsValue::as_string)
                .is_some_and(|value| value.equals_str("relative-deleted"))
        })
        .expect("#885 oracle relative-deleted row")
        .get("outcome")
        .and_then(|outcome| outcome.get("error"))
        .and_then(|error| error.get("message"))
        .and_then(JsValue::as_string)
        .expect("#885 oracle uv_cwd message")
        .to_utf8()
        .expect("#885 oracle message UTF-8");

    let cwd = Path::new(&root).join("cwd");
    let original = std::env::current_dir().expect("#885 original cwd");
    std::env::set_current_dir(&cwd).expect("#885 enter scratch cwd");
    std::fs::remove_dir(&cwd).expect("#885 remove scratch cwd");
    assert!(
        std::env::current_dir().is_err(),
        "#885 deleted cwd must fail current_dir"
    );

    let scratch = Scratch::new();
    let registry = registry(&scratch);
    // The composition error returns from `assemble` before any sidecar spawn,
    // so the command is never executed.
    let deployment = Deployment::Stdio {
        command: "pdrs-885-never-spawned".into(),
        args: vec![],
        cwd: PathBuf::from(&root),
        env: Env::new(),
    };
    let session = TurnHostFactory::new(
        registry.clone(),
        deployment,
        // #1891: exempt on purpose — the composition error returns from
        // `assemble` before any sidecar spawn, so no wait in this test can
        // starve on a child; the default limits keep their original meaning.
        Limits::default(),
        Some(JsString::from("fixture")),
        move |_, _| ports_with_tool_results_dir(None, PathBuf::from("../spill")),
    )
    .into_live_session(test_recorder(), Vec::new);
    let mut submitted = session.submit_turn_for_test(
        parse_json(r#"{"type":"text","text":"hello"}"#).expect("#885 input JSON"),
        wire(),
    );
    let first_poll: Option<Result<Option<JsValue>, String>> = match submitted {
        Ok(ref mut turn) => Some(bounded(turn.next()).await.map_err(|error| error.message)),
        Err(ref error) => Some(Err(error.message.clone())),
    };
    // Restore before assertions, even if the product returned an unexpected result.
    std::env::set_current_dir(original).expect("#885 restore original cwd");

    let mut turn = match submitted {
        Ok(turn) => turn,
        Err(error) => panic!(
            "#885 submit must admit the turn before composition: {}",
            error.message
        ),
    };
    let first = first_poll.expect("#885 first poll result");
    match first {
        Err(message) => crate::negative_assert_eq!(
            message,
            expected,
            "#885 composition failure must preserve the TS uv_cwd message on the first poll"
        ),
        Ok(event) => panic!(
            "#885 composition failure must fail the first poll before any event, got {event:?}"
        ),
    }
    // No lifecycle event may precede the error: the next poll after the failed
    // one is the stream's end.
    assert_eq!(
        bounded(turn.next()).await.expect("#885 post-error poll"),
        None,
        "#885 failed composition must not emit further events"
    );
    drop(turn);
    bounded(registry.dispose())
        .await
        .expect("#885 registry dispose");
    drop(session);
    crate::test_process::receipt(&name);
}

#[tokio::test]
async fn reload_between_turns_uses_new_generation() {
    let scratch = Scratch::new();
    let registry = registry(&scratch);
    let old = registry.resolve(None).unwrap();
    let pid_file = scratch.0.join("between.pid");
    let session = factory(registry.clone(), peer("success", Some(&pid_file)), None)
        .into_live_session(test_recorder(), Vec::new);
    for (index, expected) in [(0, false), (1, true)] {
        if index == 1 {
            bounded(registry.reload()).await.unwrap();
        }
        let mut turn = session
            .submit_turn_for_test(parse_json(r#"{"type":"text","text":"hello"}"#).unwrap(), {
                let mut options = wire().as_object().unwrap().clone();
                options.insert(
                    "turnId".into(),
                    JsValue::String(format!("reload-{index}").into()),
                );
                JsValue::Object(options)
            })
            .unwrap();
        let mut completed = None;
        while let Some(event) = bounded_liveness(turn.next()).await.unwrap() {
            if event.get("type") == Some(&"turn_completed".into()) {
                completed = Some(event);
            }
        }
        assert_eq!(
            completed
                .as_ref()
                .and_then(|event| event.get("result"))
                .and_then(|result| result.get("type")),
            Some(&"success".into()),
            "every generation must execute successfully in its own sidecar"
        );
        assert_peer_reaped(&pid_file).await;
        assert_eq!(old.lease_count(), 0);
        let current = registry.resolve(None).unwrap();
        assert_eq!(Arc::ptr_eq(&old, &current), !expected);
        assert_eq!(current.lease_count(), 0);
    }
    assert_eq!(old.state(), crate::runtime::project::RuntimeState::Disposed);
    bounded_liveness(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn reload_mid_turn_holds_old_lease_until_turn_drops() {
    let scratch = Scratch::new();
    let registry = registry(&scratch);
    let old = registry.resolve(None).unwrap();
    let pid_file = scratch.0.join("mid-turn.pid");
    let session = factory(registry.clone(), peer("hold", Some(&pid_file)), None)
        .into_live_session(test_recorder(), Vec::new);
    let mut turn = session
        .submit_turn_for_test(
            parse_json(r#"{"type":"text","text":"hello"}"#).unwrap(),
            wire(),
        )
        .unwrap();
    assert_eq!(
        bounded_liveness(turn.next())
            .await
            .unwrap()
            .unwrap()
            .get("type"),
        Some(&"turn_started".into())
    );
    bounded_liveness(turn.next()).await.unwrap().unwrap(); // host accepted
    assert_eq!(
        bounded_liveness(turn.next())
            .await
            .expect("#741k user_prompt_submitted event")
            .expect("#741k user_prompt_submitted payload")
            .get("type"),
        Some(&"user_prompt_submitted".into())
    );
    drive_to_peer(turn.as_mut(), &pid_file, true).await;
    assert_eq!(old.lease_count(), 1);
    bounded(registry.reload()).await.unwrap();
    assert_eq!(old.state(), crate::runtime::project::RuntimeState::Retired);
    assert_eq!(old.lease_count(), 1);
    assert_ts_lease("retire-with-second-lease", 1, 0, "retired");
    drop(turn);
    // The dropped stream releases the execute future; the turn's cleanup
    // releases the lease, and only then may the old generation dispose.
    bounded_liveness(old.wait_disposed()).await.unwrap();
    assert_peer_reaped(&pid_file).await;
    assert_eq!(old.lease_count(), 0);
    assert_eq!(old.state(), crate::runtime::project::RuntimeState::Disposed);
    assert_ts_lease("after-last-release", 0, 1, "disposed");
    bounded_liveness(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn retire_does_not_dispose_while_lease_outstanding() {
    let scratch = Scratch::new();
    let pid_file = scratch.0.join("retire-held.pid");
    let registry = registry(&scratch);
    let session = factory(registry.clone(), peer("hold", Some(&pid_file)), None)
        .into_live_session(test_recorder(), Vec::new);
    let runtime = registry.resolve(None).unwrap();
    let mut turn = session
        .submit_turn_for_test(
            parse_json(r#"{"type":"text","text":"hello"}"#).unwrap(),
            wire(),
        )
        .unwrap();
    assert_eq!(
        bounded_liveness(turn.next())
            .await
            .unwrap()
            .unwrap()
            .get("type"),
        Some(&"turn_started".into())
    );
    bounded_liveness(turn.next()).await.unwrap().unwrap(); // host accepted
    assert_eq!(
        bounded_liveness(turn.next())
            .await
            .expect("#741k user_prompt_submitted event")
            .expect("#741k user_prompt_submitted payload")
            .get("type"),
        Some(&"user_prompt_submitted".into())
    );
    drive_to_peer(turn.as_mut(), &pid_file, true).await;
    assert_eq!(runtime.lease_count(), 1);
    bounded(registry.reload()).await.unwrap();
    assert_eq!(
        runtime.state(),
        crate::runtime::project::RuntimeState::Retired
    );
    assert_eq!(runtime.lease_count(), 1);
    assert_ts_lease("retire-with-second-lease", 1, 0, "retired");
    // Give the executor every chance to run the cleanup task. An outstanding
    // lease must keep the generation retired instead of disposing its
    // resources under the live turn.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        runtime.state(),
        crate::runtime::project::RuntimeState::Retired
    );
    assert_eq!(runtime.lease_count(), 1);
    assert_ts_lease("retire-with-second-lease", 1, 0, "retired");
    drop(turn);
    bounded_liveness(runtime.wait_disposed()).await.unwrap();
    assert_peer_reaped(&pid_file).await;
    assert_eq!(runtime.lease_count(), 0);
    assert_eq!(
        runtime.state(),
        crate::runtime::project::RuntimeState::Disposed
    );
    bounded_liveness(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn spawn_failure_releases_lease_once_and_returns_host_error() {
    let scratch = Scratch::new();
    let registry = registry(&scratch);
    let runtime = registry.resolve(None).unwrap();
    let mut deployment = peer("success", None);
    if let Deployment::Stdio { command, .. } = &mut deployment {
        *command = "/nonexistent-pdrs-sidecar".into();
    }
    let factory = factory(registry.clone(), deployment, None);
    for _ in 0..2 {
        let host = factory.create(&identity(), test_recorder()).await.unwrap();
        let error = host.client.await.err().unwrap();
        drop(host.lease);
        assert!(
            error.message.contains("No such file") || error.message.contains("not found"),
            "{error:?}"
        );
        assert_eq!(runtime.lease_count(), 0);
        assert_eq!(
            runtime.state(),
            crate::runtime::project::RuntimeState::Active
        );
    }
    bounded(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn spawn_failure_releases_prepared_retired_generation() {
    let scratch = Scratch::new();
    let registry = registry(&scratch);
    let runtime = registry.resolve(None).unwrap();
    let mut deployment = peer("success", None);
    if let Deployment::Stdio { command, .. } = &mut deployment {
        *command = "/nonexistent-pdrs-sidecar".into();
    }
    // Retire the generation after `create` acquired its lease but before the
    // lazy spawn attempt. The preparer owns the lease until it drops/releases
    // it; wait for that generation's disposal before asserting cleanup.
    let invalidating = registry.clone();
    let factory = TurnHostFactory::new(
        registry.clone(),
        deployment,
        // #1891: exempt on purpose — the command is `/nonexistent-pdrs-sidecar`
        // and fails at spawn, so the handshake/io waits are never entered.
        Limits::default(),
        Some(JsString::from("fixture")),
        move |_, _| {
            invalidating.invalidate(None);
            ports(None)
        },
    );
    let host = bounded(factory.create(&identity(), test_recorder()))
        .await
        .unwrap();
    let error = bounded(host.client).await.err().unwrap();
    drop(host.lease);
    bounded(runtime.wait_disposed()).await.unwrap();
    assert!(
        error.message.contains("No such file") || error.message.contains("not found"),
        "{error:?}"
    );
    assert_eq!(runtime.lease_count(), 0);
    assert_eq!(
        runtime.state(),
        crate::runtime::project::RuntimeState::Disposed
    );
    bounded(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn handshake_failure_reaps_child_and_releases_lease() {
    let scratch = Scratch::new();
    let pid_file = scratch.0.join("handshake.pid");
    let registry = registry(&scratch);
    let runtime = registry.resolve(None).unwrap();
    let factory = factory(
        registry.clone(),
        peer("handshake-reject", Some(&pid_file)),
        None,
    );
    let host = factory.create(&identity(), test_recorder()).await.unwrap();
    let error = host.client.await.err().unwrap();
    drop(host.lease);
    assert!(
        error.message.contains("hello") || error.message.contains("handshake"),
        "{error:?}"
    );
    assert_eq!(runtime.lease_count(), 0);
    assert_peer_reaped(&pid_file).await;
    bounded(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn dispose_cancels_pending_host_creation_and_releases_lease() {
    let scratch = Scratch::new();
    let pid_file = scratch.0.join("pending.pid");
    let registry = registry(&scratch);
    let runtime = registry.resolve(None).unwrap();
    let session = factory(
        registry.clone(),
        peer("handshake-hang", Some(&pid_file)),
        None,
    )
    .into_live_session(test_recorder(), Vec::new);
    let mut turn = session
        .submit_turn_for_test(
            parse_json(r#"{"type":"text","text":"hello"}"#).unwrap(),
            wire(),
        )
        .unwrap();
    bounded_liveness(turn.next()).await.unwrap().unwrap(); // host started
    bounded_liveness(turn.next()).await.unwrap().unwrap(); // host accepted
    assert_eq!(
        bounded_liveness(turn.next())
            .await
            .expect("#741k user_prompt_submitted event")
            .expect("#741k user_prompt_submitted payload")
            .get("type"),
        Some(&"user_prompt_submitted".into())
    );
    drive_to_peer(turn.as_mut(), &pid_file, false).await;
    let mut pending = Box::pin(turn.next());
    session.dispose().await.unwrap();
    // The prepared handler still owns the lease until cancellation is polled.
    assert_eq!(runtime.lease_count(), 1);
    let event = bounded_liveness(pending.as_mut()).await.unwrap().unwrap();
    assert_eq!(
        event.get("result").unwrap().get("type"),
        Some(&"aborted".into())
    );
    drop(pending);
    assert!(bounded_liveness(turn.next()).await.unwrap().is_none());
    wait_for_lease_release(&runtime)
        .await
        .unwrap_or_else(|_| panic!("host cleanup left {} lease(s)", runtime.lease_count()));
    assert_eq!(runtime.lease_count(), 0);
    assert_peer_reaped(&pid_file).await;
    bounded(registry.dispose()).await.unwrap();
}
// Hold the real detached handler disposer after execute has acquired the
// connected client. Child reaping alone must not satisfy the lease observer.
struct PausedHostCleanup {
    _scratch: Scratch,
    registry: Arc<ProjectRuntimeRegistry>,
    runtime: Arc<ProjectRuntime>,
    release: Arc<Notify>,
}
impl Drop for PausedHostCleanup {
    fn drop(&mut self) {
        // Also unblock cleanup if a test assertion fails while holding it.
        self.release.notify_one();
    }
}
impl PausedHostCleanup {
    async fn new() -> Self {
        let scratch = Scratch::new();
        let pid_file = scratch.0.join("paused-cleanup.pid");
        let registry = registry(&scratch);
        let runtime = registry.resolve(None).unwrap();
        let factory = Arc::new(factory(
            registry.clone(),
            peer("hold", Some(&pid_file)),
            None,
        ));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let gate = (entered.clone(), release.clone());
        let session = LiveSessionAdapter::with_interaction(
            test_recorder(),
            Vec::new,
            move |identity, recorder| {
                let factory = factory.clone();
                let (entered, release) = gate.clone();
                Box::pin(async move {
                    let mut host = factory.create(&identity, recorder).await?;
                    let client = host.client;
                    host.client = Box::pin(async move {
                        let mut client = client.await?;
                        client.pause_dispose_for_test(entered, release);
                        Ok(client)
                    });
                    Ok(host)
                })
            },
        );
        let cleanup = Self {
            _scratch: scratch,
            registry,
            runtime,
            release,
        };
        let mut turn = session
            .submit_turn_for_test(
                parse_json(r#"{"type":"text","text":"hello"}"#).unwrap(),
                wire(),
            )
            .unwrap();
        assert_eq!(
            bounded_liveness(turn.next())
                .await
                .unwrap()
                .unwrap()
                .get("type"),
            Some(&"turn_started".into())
        );
        bounded_liveness(turn.next()).await.unwrap().unwrap(); // host accepted
        assert_eq!(
            bounded_liveness(turn.next())
                .await
                .expect("#741k user_prompt_submitted event")
                .expect("#741k user_prompt_submitted payload")
                .get("type"),
            Some(&"user_prompt_submitted".into())
        );
        drive_to_peer(turn.as_mut(), &pid_file, true).await;
        session.dispose().await.unwrap();
        drop(turn);
        bounded_liveness(entered.notified()).await;
        assert_peer_reaped(&pid_file).await;
        assert_eq!(cleanup.runtime.lease_count(), 1, "disposer owns the lease");
        cleanup
    }

    async fn finish(&self) {
        self.release.notify_one();
        wait_for_lease_release(&self.runtime).await.unwrap();
        assert_eq!(self.runtime.lease_count(), 0);
        bounded(self.registry.dispose()).await.unwrap();
    }
}

async fn assert_pending(future: std::pin::Pin<&mut impl Future>) {
    let mut future = future;
    std::future::poll_fn(|cx| {
        assert!(
            future.as_mut().poll(cx).is_pending(),
            "lease wait returned before cleanup"
        );
        std::task::Poll::Ready(())
    })
    .await;
}

#[tokio::test]
async fn lease_wait_observes_delayed_host_cleanup() {
    let cleanup = PausedHostCleanup::new().await;
    let mut waiting = Box::pin(wait_for_lease_release(&cleanup.runtime));
    assert_pending(waiting.as_mut()).await;
    // The gate, not scheduler load, keeps the old instantaneous assertion at 1.
    println!("#842: dispose returned, child reaped, gated lease_count=1; waiter Pending");
    cleanup.release.notify_one();
    bounded(waiting).await.unwrap();
    assert_eq!(cleanup.runtime.lease_count(), 0);
    let mut released = Box::pin(wait_for_lease_release(&cleanup.runtime));
    std::future::poll_fn(|cx| {
        assert!(matches!(
            released.as_mut().poll(cx),
            std::task::Poll::Ready(Ok(()))
        ));
        std::task::Poll::Ready(())
    })
    .await;
    cleanup.finish().await;
}

#[tokio::test]
async fn lease_wait_times_out_while_host_cleanup_is_held() {
    let cleanup = PausedHostCleanup::new().await;
    // All real child I/O has finished. Virtual time now proves the observer's
    // deadline even when its predecessor never completes; no wall-clock sleep.
    tokio::time::pause();
    let mut waiting = Box::pin(wait_for_lease_release(&cleanup.runtime));
    assert_pending(waiting.as_mut()).await;
    tokio::time::advance(Duration::from_secs(10)).await;
    // Tokio timers have millisecond granularity, and advance() does not promise
    // that every elapsed timer has been processed when it returns. Allow that
    // timer-wheel tick, but bound it so a longer deadline cannot auto-advance
    // virtual time unnoticed.
    assert!(
        timeout(Duration::from_millis(10), waiting)
            .await
            .expect("held cleanup must reach the 10-second deadline")
            .is_err()
    );
    assert_eq!(cleanup.runtime.lease_count(), 1);
    cleanup.finish().await;
}

#[tokio::test]
async fn cancelling_lease_wait_does_not_release_or_block_cleanup() {
    let cleanup = PausedHostCleanup::new().await;
    let mut waiting = Box::pin(wait_for_lease_release(&cleanup.runtime));
    assert_pending(waiting.as_mut()).await;
    drop(waiting);
    assert_eq!(
        cleanup.runtime.lease_count(),
        1,
        "observer does not own the lease"
    );
    cleanup.finish().await;
}

fn published_pid(pid_file: &Path) -> Option<u32> {
    std::fs::read_to_string(pid_file).ok()?.trim().parse().ok()
}
/// Assert the sidecar is gone without assuming it won the pid-publication race.
/// `peer.py` publishes the pid atomically before reading any frame, so a
/// missing/empty file means it died before the protocol; prove no process
/// carrying this test's unique argv marker survives.
async fn assert_peer_reaped(pid_file: &Path) {
    match published_pid(pid_file) {
        Some(pid) => assert_reaped(pid).await,
        None => assert_no_peer_process(&peer_marker(pid_file)).await,
    }
}
async fn drive_to_peer(
    turn: &mut dyn crate::runtime::session::LiveTurn,
    pid_file: &Path,
    execute: bool,
) {
    let mut pending = turn.next();
    // #1891: the pid-publication (and execute-marker) wait below is the same
    // sidecar cold-start phase as the #978 command-entry gate — it advances
    // only when the real Python child makes progress, so it gets the
    // liveness margin, not the shared in-process 10 s.
    bounded_liveness(async {
        tokio::select! {
            result = &mut pending => panic!("peer should remain pending: {result:?}"),
            _ = async {
                while !published_pid(pid_file).is_some_and(|pid| {
                    !execute || PathBuf::from(format!("{}.{pid}.execute", pid_file.display())).exists()
                }) {
                    tokio::task::yield_now().await;
                }
            } => {}
        }
    })
    .await;
}
async fn assert_no_peer_process(marker: &str) {
    timeout(Duration::from_secs(10), async {
        loop {
            let output = Command::new("pgrep")
                .arg("-f")
                .arg(marker)
                .output()
                .unwrap_or_else(|error| panic!("cannot run pgrep -f {marker}: {error}"));
            match output.status.code() {
                Some(1) => break,
                Some(0) => tokio::time::sleep(Duration::from_millis(5)).await,
                _ => panic!(
                    "pgrep -f {marker} failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            }
        }
    })
    .await
    .expect("owned sidecar left a process carrying the test marker");
}
async fn assert_reaped(pid: u32) {
    timeout(Duration::from_secs(10), async {
        loop {
            match rustix::process::test_kill_process(
                rustix::process::Pid::from_raw(pid as i32).unwrap(),
            ) {
                Err(rustix::io::Errno::SRCH) => break,
                Ok(()) => tokio::time::sleep(Duration::from_millis(5)).await,
                Err(error) => panic!("cannot inspect owned child {pid}: {error}"),
            }
        }
    })
    .await
    .expect("owned sidecar was not reaped");
}

// -- manifest golden ---------------------------------------------------------------
struct AllContext;
impl context::ContextRuntime for AllContext {
    fn supports(&self, _: context::Operation) -> bool {
        true
    }
    fn prepare_for_model(&self, _: context::ContextInput) -> HostFuture<JsValue> {
        Box::pin(async { Ok(JsValue::Object(JsObject::new())) })
    }
}
struct FixtureTool {
    fields: JsObject,
    read_only: bool,
    concurrency_safe: bool,
    interaction: bool,
    capabilities: Vec<JsString>,
}
impl capability::ToolDefinition for FixtureTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(self.read_only))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(self.concurrency_safe))
    }
    fn requires_user_interaction(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(self.interaction))
    }
    fn required_runtime_capabilities(&self) -> Option<Vec<JsString>> {
        Some(self.capabilities.clone())
    }
}
fn fixture_tools(spec: &JsValue) -> Vec<Arc<dyn capability::ToolDefinition>> {
    array(field(spec, "tools").unwrap())
        .iter()
        .map(|tool| -> Arc<dyn capability::ToolDefinition> {
            let mut fields = JsObject::new();
            for key in ["name", "description", "kind", "inputSchema"] {
                if let Some(value) = tool.get(key) {
                    fields.insert(key.into(), value.clone());
                }
            }
            Arc::new(FixtureTool {
                fields,
                read_only: field(tool, "readOnly").and_then(JsValue::as_string)
                    == Some(&"true".into()),
                concurrency_safe: field(tool, "concurrencySafe").and_then(JsValue::as_string)
                    == Some(&"true".into()),
                interaction: field(tool, "requiresUserInteraction").and_then(JsValue::as_string)
                    == Some(&"true".into()),
                capabilities: field(tool, "requiredRuntimeCapabilities")
                    .map(|caps| {
                        array(caps)
                            .iter()
                            .map(|cap| cap.as_string().unwrap().clone())
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}
fn golden_turn_ports() -> turn::Ports {
    turn::Ports {
        drain_steer: Some(Arc::new(|| Box::pin(async { Ok(vec![]) }))),
        drain_or_close_steer: Some(Arc::new(|| {
            Box::pin(async {
                let mut value = JsObject::new();
                value.insert("messages".into(), JsValue::Array(vec![]));
                value.insert("closed".into(), JsValue::Bool(false));
                Ok(value)
            })
        })),
        persist_compaction: Some(Arc::new(|_, _| Box::pin(async { Ok(()) }))),
    }
}

#[tokio::test]
async fn router_manifest_matches_ts_full_composition_golden() {
    let oracle = parse_json(include_str!(
        "../../../tests/fixtures/host-manifest/oracle.json"
    ))
    .unwrap();
    let spec = array(field(&oracle, "cases").unwrap())
        .iter()
        .find(|case| {
            field(case, "name").and_then(JsValue::as_string) == Some(&"full-composition".into())
        })
        .expect("full-composition case");
    let scratch = Scratch::new();
    let registry = registry(&scratch);
    let lease = registry.acquire(None).unwrap();
    let mut session_ports = ports(Some(Arc::new(AllContext)));
    session_ports.tool_execution = Arc::new(Tools(fixture_tools(spec)));
    session_ports.plan_todo = Some(Arc::new(Plan));
    session_ports.turn = golden_turn_ports();
    let (router, input, lease) = TurnHostFactory::assemble(lease, &identity(), session_ports)
        .await
        .unwrap();
    let turn = InteractionTurn::bind(lease, &identity(), router, input)
        .await
        .unwrap();
    let expected = field(field(spec, "expected").unwrap(), "hostModules").unwrap();
    assert_eq!(
        stringify_json(turn.manifest().get("hostModules").unwrap())
            .unwrap()
            .unwrap(),
        stringify_json(expected).unwrap().unwrap(),
    );
    assert_eq!(
        stringify_json(turn.manifest().get("tools").unwrap())
            .unwrap()
            .unwrap(),
        stringify_json(field(field(spec, "expected").unwrap(), "tools").unwrap())
            .unwrap()
            .unwrap(),
    );
    turn.dispose().await.unwrap();
    bounded(registry.dispose()).await.unwrap();
}

struct SessionSteerRecorder {
    held: bool,
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
    mutations: Arc<Mutex<Vec<JsObject>>>,
}
impl MutationRecorder for Arc<SessionSteerRecorder> {
    fn record(&self, _: JsString, mutation: JsObject) -> HostFuture<()> {
        let recorder = self.clone();
        Box::pin(async move {
            if recorder.held && mutation.get("mutation") == Some(&"insert".into()) {
                recorder.entered.add_permits(1);
                recorder
                    .release
                    .acquire()
                    .await
                    .expect("#743 steer insert release")
                    .forget();
            }
            recorder
                .mutations
                .lock()
                .expect("#743 steer mutation lock")
                .push(mutation);
            Ok(())
        })
    }
}
#[tokio::test]
async fn session_hooks_wrap_real_turn_and_retain_lease_through_end() {
    for case in [
        "success",
        "start-failure",
        "abort-turn",
        "abort-terminal",
        "abort-start",
        "abort-end",
        "dispose-start",
        "dispose-turn",
        "error",
        "reuse",
        "steer-success",
        "steer-abort",
        "steer-insert",
    ] {
        let scratch = Scratch::new();
        let registry = registry(&scratch);
        let runtime = registry.resolve(None).expect("#743 production runtime");
        let trace = scratch.0.join("lifecycle.txt");
        let hook_trace = trace.clone();
        let factory_registry = registry.clone();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recorder = test_recorder();
        let mutations = Arc::new(Mutex::new(vec![]));
        let steer_recorder = Arc::new(SessionSteerRecorder {
            held: case == "steer-insert",
            entered: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
            mutations: mutations.clone(),
        });
        let factory_steer = steer_recorder.clone();
        let session = LiveSessionAdapter::with_interaction(
            recorder.clone(), Vec::new,
            move |identity, recorder| {
                let round = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mode = if matches!(case, "abort-turn" | "dispose-turn" | "steer-abort" | "steer-insert") || (case == "reuse" && round == 0) {
                    "live-session-cancel"
                } else if case == "error" { "live-double-failure" } else { "live-payload" };
                let mut deployment = peer("success", None);
                if let Deployment::Stdio { args, .. } = &mut deployment {
                    *args = vec![format!("{}/tests/fixtures/live-session/peer.py", env!("CARGO_MANIFEST_DIR")), mode.into()];
                }
                // #978: this fixture is a real Python sidecar, so the default
                // 5 s per-frame io timeout (and 10 s handshake) is a wall-clock
                // bet on the host's scheduling, not a property under test. A
                // host running eight concurrent lib suites starved the peer
                // past it, the turn ended with an `error` terminal, and
                // `abort-terminal` then reported status `failed` instead of
                // `idle` (31/48 loaded copies, the highest hit rate in the
                // scan). The waits are liveness margins only: every await is
                // still bounded below (60 s for the sidecar-dependent ones,
                // the shared 10 s for in-process state), and no assertion
                // changed. `close_timeout` is the product default 1 s rather
                // than an artificially short 20 ms: 20 ms is shorter than any
                // real deployment's SIGTERM-and-reap, so on a loaded host it
                // force-killed the child mid-cleanup for no test reason.
                // #2054: it is also how long the per-turn scope holds the
                // terminal frame, so `complete()` cannot enter before it
                // elapses (measured: ~1.0 s after `abort()`); the steer-close
                // observation window below is therefore tied to this value,
                // not to a fixed 100 ms.
                let factory = TurnHostFactory::new(factory_registry.clone(), deployment,
                    Limits {
                        close_timeout: STEER_CLOSE_TIMEOUT,
                        handshake_timeout: Duration::from_secs(60),
                        io_timeout: Duration::from_secs(60),
                        ..Default::default()
                    },
                    Some("fixture".into()), |_, _| ports(None));
                let steer_recorder = factory_steer.clone();
                let trace = hook_trace.clone();
                Box::pin(async move {
                    let mut host = factory.create(&identity, recorder).await?;
                    host.input.steer_recorder = Some(Arc::new(steer_recorder));
                    host.input.permission.context.cwd = trace.parent().expect("#743 production cwd").to_string_lossy().as_ref().into();
                    host.input.hooks = [("SessionStart", 2), ("Setup", 0), ("SessionEnd", 1)]
                        .into_iter().map(|(name, code)| {
                            let code = if name == "SessionStart" && case == "start-failure" {1} else {code};
                            let held = (name == "SessionStart" && matches!(case, "abort-start" | "dispose-start")) || (name == "SessionEnd" && case == "abort-end");
                            let gate = if held { format!("touch {name}.entered; while test ! -f release; do sleep 0.01; done; touch {name}.completed; ") } else { String::new() };
                            (name.into(), vec![crate::hooks::protocol::HookMatcher {
                                hooks: vec![crate::hooks::protocol::HookCommand::Command {
                                    command: format!("printf '{name}\\n' >> '{}'; {gate}exit {code}", trace.display()).into(),
                                    shell: None, asynchronous: None, async_rewake: None, common: Default::default(),
                                }], ..Default::default()
                            }])
                        }).collect();
                    Ok(host)
                })
            },
        ).with_close_timeout(STEER_CLOSE_TIMEOUT);
        for round in 0..if case == "reuse" { 2 } else { 1 } {
            let input =
                parse_json(r#"{"type":"text","text":"hello"}"#).expect("#743 production input");
            let mut options = wire();
            if round == 1
                && let JsValue::Object(fields) = &mut options
            {
                fields.insert("turnId".into(), "turn-second".into());
            }
            let mut stream = session
                .submit(input.clone(), options.clone())
                .expect("#743 production submit");
            let busy = session
                .submit(input, options)
                .err()
                .expect("#743 production direct busy");
            assert_eq!(
                busy.message, "Agent session already has an active turn.",
                "#743 production distinct session busy"
            );
            let mut kinds = vec![];
            let mut terminal = None;
            let mut ended = false;
            let gate_name = if matches!(case, "abort-start" | "dispose-start") {
                Some("SessionStart")
            } else if case == "abort-end" {
                Some("SessionEnd")
            } else {
                None
            };
            let mut acted = false;
            let mut mailbox = None;
            let session_id = wire()
                .get("sessionId")
                .and_then(JsValue::as_string)
                .expect("#743 steer session id")
                .clone();
            let turn_id = wire()
                .get("turnId")
                .and_then(JsValue::as_string)
                .expect("#743 steer turn id")
                .clone();
            loop {
                let mut next = stream.next();
                let should_gate = !acted
                    && gate_name.is_some()
                    && ((kinds.len() == 1 && gate_name == Some("SessionStart"))
                        || terminal.is_some());
                if should_gate {
                    let name = gate_name.expect("#743 production gate name");
                    let entered = scratch.0.join(format!("{name}.entered"));
                    tokio::select! {
                        _ = async { while !entered.exists() { tokio::time::sleep(Duration::from_millis(5)).await; } } => {},
                        result = &mut next => panic!("#743 production command must enter: {case}: {result:?}"),
                        _ = tokio::time::sleep(SIDECAR_LIVENESS) => panic!("#743 production command entry deadline: {case}"),
                    }
                    if case == "dispose-start" {
                        bounded(session.dispose())
                            .await
                            .expect("#743 production dispose start returns while stream held");
                    } else {
                        session
                            .abort(&"user-stop".into())
                            .expect("#743 production hook abort");
                    }
                    acted = true;
                }
                let Some(event) = timeout(SIDECAR_LIVENESS, next)
                    .await
                    .expect("#743 production bounded stream")
                    .expect("#743 production next")
                else {
                    break;
                };
                let kind = event
                    .get("type")
                    .and_then(JsValue::as_string)
                    .expect("#743 production type")
                    .to_utf8()
                    .expect("#743 production type utf8");
                if case.starts_with("steer-")
                    && matches!(kind.as_str(), "model_event" | "model_request_started")
                {
                    use crate::runtime::project::{
                        ProjectResources, Resource, interaction_steer::InteractionSteer,
                    };
                    let context = runtime
                        .resources()
                        .get::<ProjectResources>()
                        .expect("#743 steer project")
                        .context();
                    let resource = context
                        .resource(Resource::InteractionSteer)
                        .expect("#743 steer resource");
                    let actual = resource
                        .as_any()
                        .downcast_ref::<InteractionSteer>()
                        .expect("#743 steer type")
                        .mailbox_for_test(&session_id)
                        .expect("#743 steer current mailbox");
                    let mut enqueue = actual.enqueue(
                        turn_id.clone(),
                        crate::runtime::steer::SteerMessage {
                            item_id: "queued".into(),
                            message: JsValue::Null,
                            allowed_read_files: None,
                        },
                    );
                    if case == "steer-insert" {
                        tokio::select! {
                            permit = steer_recorder.entered.acquire() => permit.expect("#743 steer insert entered").forget(),
                            result = &mut enqueue => panic!("#743 steer insert must hold {result:?}"),
                        }
                        session
                            .abort(&"user-stop".into())
                            .expect("#743 steer insert abort");
                        let mut next = stream.next();
                        // #2054: a fixed 100 ms window closed before
                        // `complete_turn` could even be attempted once
                        // `close_timeout` went back to the product default of
                        // 1 s, so moving `close_session_steer()` past the
                        // durable write became invisible. Poll the transcript
                        // for the whole window instead. The in-flight insert
                        // below stays unreleased, which keeps `complete()`
                        // parked inside `close_session_steer()` until
                        // `run.wait()` sees the serialized mailbox drain — so
                        // a `turn_result` landing here was written before the
                        // steer closed.
                        // #2054: only complete, newline-terminated lines are
                        // evidence. The poll below races the writer's single
                        // `write_all`, and a torn tail would otherwise panic on
                        // the JSON parse (a false red, not a real violation).
                        let turn_result_written = || {
                            let bytes = std::fs::read(recorder.writer().path())
                                .expect("#743 steer transcript exists");
                            let text = String::from_utf8_lossy(&bytes);
                            let Some(end) = text.rfind('\n') else {
                                return false;
                            };
                            text[..=end].lines().any(|line| {
                                parse_json(line)
                                    .expect("#743 steer transcript JSON")
                                    .get("type")
                                    == Some(&"turn_result".into())
                            })
                        };
                        let started = tokio::time::Instant::now();
                        let mut early_result = None;
                        let mut early_event = false;
                        let mut mailbox_close = None;
                        loop {
                            if turn_result_written() {
                                early_result = Some(started.elapsed());
                                break;
                            }
                            if mailbox_close.is_none() && !actual.open_for_test() {
                                mailbox_close = Some(started.elapsed());
                            }
                            let elapsed = started.elapsed();
                            if elapsed >= STEER_CLOSE_WINDOW {
                                break;
                            }
                            let step = STEER_CLOSE_STEP.min(STEER_CLOSE_WINDOW - elapsed);
                            if timeout(step, &mut next).await.is_ok() {
                                // The mutant writes `turn_result` and only then
                                // lets the terminal frame through, so both can
                                // become visible inside one polling step.
                                // Attribute the failure to the registered
                                // anchor whenever the write is already on disk:
                                // panicking on the "terminal" assertion instead
                                // would leave the negative-control matrix
                                // reporting the mutant as INVALID (wrong panic)
                                // rather than KILLED.
                                if turn_result_written() {
                                    early_result = Some(started.elapsed());
                                } else {
                                    early_event = true;
                                }
                                break;
                            }
                        }
                        // Diagnostics only, and deliberately not proof: a
                        // window that expired before the product reached its
                        // write point prints the same
                        // `turn_result=None early_event=false` as one that
                        // covered it. What it does give a SURVIVED receipt is
                        // the shape of the run — window length, the
                        // `close_timeout` it was derived from, when the
                        // admission flag cleared, and the timing whenever the
                        // window did catch something. (`mailbox_closed_at` is
                        // sampled here for that record only; the judgement is
                        // `wait_for_mailbox_close` below.)
                        eprintln!(
                            "#2054 steer-close window {:?} (close_timeout {:?}): turn_result={:?} early_event={} mailbox_closed_at={:?}",
                            STEER_CLOSE_WINDOW,
                            STEER_CLOSE_TIMEOUT,
                            early_result,
                            early_event,
                            mailbox_close
                        );
                        crate::negative_assert!(
                            early_result.is_none(),
                            "#743 turn_result waits durable steer close"
                        );
                        crate::negative_assert!(
                            !early_event,
                            "#743 terminal waits durable steer close"
                        );
                        // #1847: the window above asserts only the negative
                        // direction (nothing settles early); it must not double
                        // as a bet that the close has landed by then. `abort()`
                        // just cancels a token, and the mailbox close travels
                        // monitor task -> Run::close -> cleanup task, so under
                        // load it can land later than any fixed window. Wait for
                        // the mailbox's own admission flag (cleared synchronously
                        // by `close`) with the file's 10 s in-process bound, then
                        // let the durable insert run: it must observe the closed
                        // mailbox and reject as turn_closing.
                        let closed = wait_for_mailbox_close(&actual).await;
                        crate::negative_assert!(
                            closed.is_ok(),
                            "#743 steer mailbox close observed"
                        );
                        steer_recorder.release.add_permits(1);
                        drop(next);
                        let result = bounded(enqueue).await.expect("#743 steer enqueue settles");
                        crate::negative_assert!(
                            !result.accepted,
                            "#743 steer in-flight insert rejected after close"
                        );
                    } else {
                        assert!(
                            bounded(enqueue).await.expect("#743 steer enqueue").accepted,
                            "#743 steer pending accepted"
                        );
                    }
                    mailbox = Some(actual);
                }
                if kind == "model_event"
                    && (matches!(
                        case,
                        "abort-turn" | "dispose-turn" | "steer-abort" | "steer-insert"
                    ) || (case == "reuse" && round == 0))
                {
                    if case == "dispose-turn" {
                        bounded(session.dispose())
                            .await
                            .expect("#743 production dispose model returns before drain");
                    } else {
                        session
                            .abort(&"user-stop".into())
                            .expect("#743 production turn abort");
                    }
                    acted = true;
                }
                if kind == "steer_unapplied" {
                    assert_eq!(
                        event.get("itemId"),
                        Some(&"queued".into()),
                        "#743 steer unapplied item"
                    );
                    assert_eq!(
                        event.get("reason"),
                        Some(&"turn_ended".into()),
                        "#743 steer unapplied reason"
                    );
                }
                if kind == "turn_completed" {
                    if let Some(mailbox) = &mailbox {
                        let result = bounded(mailbox.enqueue(
                            turn_id.clone(),
                            crate::runtime::steer::SteerMessage {
                                item_id: "late-terminal".into(),
                                message: JsValue::Null,
                                allowed_read_files: None,
                            },
                        ))
                        .await
                        .expect("#743 steer terminal probe");
                        crate::negative_assert_eq!(
                            result.reason,
                            Some(crate::runtime::steer::SteerReason::TurnClosing),
                            "#743 finish occurs after terminal yield"
                        );
                    }
                    terminal = event.get("result").and_then(|r| r.get("type")).cloned();
                    if case == "abort-terminal" {
                        session
                            .abort(&"user-stop".into())
                            .expect("#743 production terminal abort");
                    }
                }
                if kind.starts_with("session_") || kind == "setup_completed" {
                    assert!(
                        event.get("turnId").is_none(),
                        "#743 production session event omits turnId"
                    );
                }
                if kind == "session_ended" {
                    ended = true;
                    if let Some(mailbox) = &mailbox {
                        let result = bounded(mailbox.enqueue(
                            turn_id.clone(),
                            crate::runtime::steer::SteerMessage {
                                item_id: "late-end".into(),
                                message: JsValue::Null,
                                allowed_read_files: None,
                            },
                        ))
                        .await
                        .expect("#743 steer End probe");
                        crate::negative_assert_eq!(
                            result.reason,
                            Some(crate::runtime::steer::SteerReason::NoActiveTurn),
                            "#743 finish before End"
                        );
                        let mutations = mutations.lock().expect("#743 steer committed mutations");
                        assert_eq!(mutations.len(), 2, "#743 steer insert and discard");
                        assert_eq!(
                            mutations[0].get("mutation"),
                            Some(&"insert".into()),
                            "#743 steer durable insert first"
                        );
                        assert_eq!(
                            mutations[1].get("mutation"),
                            Some(&"discard".into()),
                            "#743 steer durable discard second"
                        );
                        assert_eq!(
                            mutations[1].get("reason"),
                            Some(
                                &if case == "steer-insert" {
                                    "turn_closing"
                                } else {
                                    "turn_ended"
                                }
                                .into()
                            ),
                            "#743 steer exact discard reason"
                        );
                    }
                    crate::negative_assert_eq!(
                        std::fs::read_to_string(&trace).ok().as_deref(),
                        Some(
                            "SessionStart\nSetup\nSessionEnd\n"
                                .repeat(round + 1)
                                .as_str()
                        ),
                        "#743 production all lifecycle hooks executed"
                    );
                    crate::negative_assert!(
                        runtime.lease_count() > 0,
                        "#743 production lease held through End yield"
                    );
                    let aborted = matches!(
                        case,
                        "abort-turn"
                            | "abort-start"
                            | "dispose-start"
                            | "dispose-turn"
                            | "steer-abort"
                            | "steer-insert"
                    ) || (case == "reuse" && round == 0);
                    assert_eq!(
                        event.get("reason"),
                        Some(
                            &if aborted {
                                "other"
                            } else {
                                "prompt_input_exit"
                            }
                            .into()
                        ),
                        "#743 production end reason: {case}, terminal={terminal:?}"
                    );
                    let status = if aborted || case == "abort-end" {
                        "aborted"
                    } else if case == "error" {
                        "failed"
                    } else {
                        "idle"
                    };
                    assert_eq!(
                        session.snapshot().get("status"),
                        Some(&status.into()),
                        "#743 production status: {case}"
                    );
                    assert!(
                        session.snapshot().get("currentTurnId").is_none(),
                        "#743 production current turn cleared: {case}"
                    );
                }
                kinds.push(kind);
            }
            assert_eq!(
                &kinds[..2],
                ["session_started", "setup_completed"],
                "#743 production prefix"
            );
            crate::negative_assert!(ended, "#743 production session_ended emitted");
            let result = if matches!(
                case,
                "abort-turn"
                    | "abort-start"
                    | "dispose-start"
                    | "dispose-turn"
                    | "steer-abort"
                    | "steer-insert"
            ) || (case == "reuse" && round == 0)
            {
                "aborted"
            } else if case == "error" {
                "error"
            } else {
                "success"
            };
            assert_eq!(
                terminal,
                Some(result.into()),
                "#743 production terminal unaffected by hook errors: {case}"
            );
            crate::negative_assert_eq!(
                runtime.lease_count(),
                0,
                "#743 production lease released at None"
            );
            if let Some(name) = gate_name {
                crate::negative_assert!(
                    acted && !scratch.0.join(format!("{name}.completed")).exists(),
                    "#743 production running command killed before completion"
                );
            }
            if case.starts_with("steer-") {
                crate::negative_assert_eq!(
                    kinds
                        .iter()
                        .filter(|kind| kind.as_str() == "steer_unapplied")
                        .count(),
                    usize::from(case != "steer-insert"),
                    "#743 discarded steer emits unapplied once"
                );
                if case != "steer-insert" {
                    let unapplied = kinds
                        .iter()
                        .position(|kind| kind == "steer_unapplied")
                        .expect("#743 steer unapplied present");
                    let terminal = kinds
                        .iter()
                        .position(|kind| kind == "turn_completed")
                        .expect("#743 steer terminal present");
                    assert!(unapplied < terminal, "#743 steer unapplied before terminal");
                }
            }
            println!(
                "#743 production {case} round={round}: events={kinds:?}; terminal={terminal:?}; None; lease={}",
                runtime.lease_count()
            );
            if case.starts_with("dispose-") {
                crate::test_negative_receipt::expect_ok(
                    recorder.writer().record_session_event(
                        &"s".into(),
                        &"t".into(),
                        JsObject::new(),
                    ),
                    "#743 production disposed writer closed",
                );
            }
        }
        bounded_liveness(registry.dispose())
            .await
            .expect("#743 production dispose");
    }
}

#[test]
fn tcp_listener_and_gateway_port_grammars_stay_paired() {
    for value in [
        "1",
        "65535",
        "65536",
        "0",
        "-0",
        "+80",
        "80.0",
        "1e3",
        "1e-3",
        "0x50",
        "0X000000000050",
        "0b1010000",
        "0o120",
        "+0x50",
        "0x+50",
        "0x",
        "0o8",
        "NaN",
        "Infinity",
        "inf",
        "80.5",
        "1_000",
        "\u{feff}80\u{a0}",
        "\u{85}80",
        "80\0",
    ] {
        let listener = pilotdeck_transport::tcp::optional_port(Some(value))
            .ok()
            .flatten();
        assert_eq!(listener, required_port(Some(value)).ok(), "{value:?}");
    }
}

struct SopProbeController {
    isolated: bool,
    rounds: usize,
    outcomes: Arc<Mutex<Vec<usize>>>,
}
impl super::super::loop_control::HostLoopController for SopProbeController {
    fn run<'a>(
        &'a self,
        input: super::super::loop_control::SopLoopInput,
        native: &'a dyn super::super::loop_control::SopNodeExecutor,
    ) -> super::super::loop_control::SopFuture<'a, Result<crate::host::ExecuteResult, HostError>>
    {
        Box::pin(async move {
            let mut final_result = None;
            let mut previous_node: Option<super::super::loop_control::SopNodeOutcome> = None;
            for round in 0..self.rounds {
                let context = if self.isolated && round > 0 {
                    super::super::loop_control::SopNodeContext::Isolated {
                        sop: "fixture/sop".into(),
                        node: "isolated node".into(),
                    }
                } else {
                    super::super::loop_control::SopNodeContext::Inherit
                };
                let mut output = native
                    .run(super::super::loop_control::SopNodePlan {
                        continuation: self.isolated && round == 2,
                        context,
                        request: input.request.clone(),
                        instruction: "".into(),
                        model_override: None,
                        step: crate::metadata::object([
                            ("skillId", "fixture/sop".into()),
                            ("nodeId", "isolated node".into()),
                        ]),
                        binding: None,
                    })
                    .await?;
                if self.isolated
                    && round == 2
                    && let Some(previous) = previous_node.take()
                {
                    let mut result = output.result.as_object().unwrap().clone();
                    result.insert(
                        "turns".into(),
                        JsValue::Number(
                            previous
                                .result
                                .get("turns")
                                .and_then(JsValue::as_number)
                                .unwrap()
                                + output
                                    .result
                                    .get("turns")
                                    .and_then(JsValue::as_number)
                                    .unwrap(),
                        ),
                    );
                    result.insert(
                        "startedAt".into(),
                        previous.result.get("startedAt").unwrap().clone(),
                    );
                    let mut usage = JsObject::new();
                    for key in ["inputTokens", "outputTokens", "totalTokens"] {
                        usage.insert(
                            key.into(),
                            JsValue::Number(
                                previous
                                    .result
                                    .get("usage")
                                    .and_then(|usage| usage.get(key))
                                    .and_then(JsValue::as_number)
                                    .unwrap()
                                    + output
                                        .result
                                        .get("usage")
                                        .and_then(|usage| usage.get(key))
                                        .and_then(JsValue::as_number)
                                        .unwrap(),
                            ),
                        );
                    }
                    result.insert("usage".into(), JsValue::Object(usage));
                    output.result = JsValue::Object(result);
                }
                if self.isolated && round == 1 {
                    previous_node = Some(output.clone());
                } else {
                    native.finish_node(Some(&output)).await?;
                }
                self.outcomes.lock().unwrap().push(output.messages.len());
                let aborted = output.result.get("type") == Some(&"aborted".into());
                final_result = Some(output.result);
                if aborted {
                    break;
                }
            }
            let result = final_result
                .ok_or_else(|| HostError::new(HostErrorKind::Protocol, "No native SOP result."))?;
            Ok(crate::host::ExecuteResult::Terminal(
                crate::metadata::object([(
                    "payload",
                    crate::metadata::object([
                        ("result", result),
                        ("messages", JsValue::Array(input.visible_messages)),
                    ]),
                )]),
            ))
        })
    }
}
#[tokio::test]
async fn sop_real_nodes_share_one_parent_admission_and_restore_isolated_history() {
    let scratch = Scratch::new();
    let registry = registry(&scratch);
    let recorder = test_recorder();
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let invoked = calls.clone();
    let pid = scratch.0.join("sop.pid");
    let session = TurnHostFactory::new(
        registry.clone(),
        peer("sop-success", Some(&pid)),
        Limits::default(),
        Some("fixture".into()),
        move |_, _| {
            invoked.fetch_add(1, Ordering::SeqCst);
            ports(None)
        },
    )
    .with_loop_controller(Arc::new(SopProbeController {
        isolated: true,
        rounds: 3,
        outcomes: outcomes.clone(),
    }))
    .into_live_session(recorder.clone(), || {
        vec![crate::metadata::object([
            ("role", "assistant".into()),
            ("content", "PARENT HISTORY ONLY".into()),
        ])]
    });
    let mut all_events = Vec::new();
    for index in 0..2 {
        let mut options = wire().as_object().unwrap().clone();
        options.insert(
            "turnId".into(),
            format!("sop-parent-{index}").as_str().into(),
        );
        let mut stream = session
            .submit_turn_for_test(
                parse_json(r#"{"type":"text","text":"user input"}"#).unwrap(),
                JsValue::Object(options),
            )
            .unwrap();
        let mut events = Vec::new();
        while let Some(event) = bounded_liveness(stream.next()).await.unwrap() {
            events.push(event);
        }
        drop(stream);
        for kind in ["turn_started", "input_accepted", "turn_completed"] {
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.get("type") == Some(&kind.into()))
                    .count(),
                1,
                "one parent {kind}; events={events:?}"
            );
        }
        assert_eq!(
            events
                .last()
                .and_then(|event| event.get("result"))
                .and_then(|result| result.get("type")),
            Some(&"success".into()),
            "actual terminal: {events:?}"
        );
        all_events.extend(events);
    }
    bounded(session.dispose()).await.unwrap();
    bounded(registry.dispose()).await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "each outer turn captures services once"
    );
    assert_eq!(
        *outcomes.lock().unwrap(),
        vec![2, 1, 1, 2, 2, 2],
        "correction must not accept user input again; next parent turn restores isolated history; events={all_events:?}"
    );
    assert_eq!(
        all_events
            .iter()
            .filter(|event| event.get("type") == Some(&"model_request_started".into()))
            .count(),
        6
    );
    let read = crate::storage::read_transcript(recorder.writer().path()).unwrap();
    assert_eq!(
        read.entries
            .iter()
            .filter(|entry| entry.get("type") == Some(&"accepted_input".into()))
            .count(),
        2,
        "child accepts must never be written to the parent transcript"
    );
    let executions = std::fs::read_dir(&scratch.0)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".execute"))
        .count();
    assert_eq!(
        executions, 6,
        "exactly six real child processes; no eager unused outer connection"
    );
}
#[tokio::test]
async fn sop_cancel_drains_the_real_node_and_retains_the_old_generation() {
    let scratch = Scratch::new();
    let registry = registry(&scratch);
    let old = registry.resolve(None).unwrap();
    let recorder = test_recorder();
    let pid = scratch.0.join("sop-cancel.pid");
    let session = factory(registry.clone(), peer("sop-cancel", Some(&pid)), None)
        .with_loop_controller(Arc::new(SopProbeController {
            isolated: false,
            rounds: 1,
            outcomes: Arc::default(),
        }))
        .into_live_session(recorder.clone(), Vec::new);
    let mut stream = session
        .submit_turn_for_test(
            parse_json(r#"{"type":"text","text":"cancel node"}"#).unwrap(),
            wire(),
        )
        .unwrap();
    loop {
        let event = bounded_liveness(stream.next()).await.unwrap().unwrap();
        if event.get("type") == Some(&"model_request_started".into()) {
            break;
        }
    }
    bounded(registry.reload()).await.unwrap();
    assert!(old.lease_count() > 0);
    session.abort(&"cancel probe".into()).unwrap();
    let mut terminal = None;
    while let Some(event) = bounded_liveness(stream.next()).await.unwrap() {
        if event.get("type") == Some(&"turn_completed".into()) {
            terminal = Some(event);
        }
    }
    drop(stream);
    bounded(session.dispose()).await.unwrap();
    bounded(registry.dispose()).await.unwrap();
    let terminal = terminal.unwrap();
    assert_eq!(
        terminal.get("result").and_then(|result| result.get("type")),
        Some(&"aborted".into()),
        "full SOP cancel terminal: {terminal:?}"
    );
    assert_peer_reaped(&pid).await;
    assert_eq!(old.lease_count(), 0);
    let read = crate::storage::read_transcript(recorder.writer().path()).unwrap();
    assert_eq!(
        read.entries
            .iter()
            .filter(|entry| entry.get("type") == Some(&"accepted_input".into()))
            .count(),
        1
    );
    assert!(
        read.entries
            .iter()
            .any(|entry| entry.get("type") == Some(&"turn_result".into())
                && entry.get("result").and_then(|result| result.get("type"))
                    == Some(&"aborted".into()))
    );
}

#[tokio::test]
async fn sop_unresponsive_cancel_preserves_unknown_progress_and_reaps_child() {
    let scratch = Scratch::new();
    let registry = registry(&scratch);
    let recorder = test_recorder();
    let pid = scratch.0.join("sop-unknown.pid");
    let session = factory(registry.clone(), peer("hold", Some(&pid)), None)
        .with_loop_controller(Arc::new(SopProbeController {
            isolated: false,
            rounds: 1,
            outcomes: Arc::default(),
        }))
        .into_live_session(recorder.clone(), Vec::new);
    let mut stream = session
        .submit_turn_for_test(
            parse_json(r#"{"type":"text","text":"cancel unknown node"}"#).unwrap(),
            wire(),
        )
        .unwrap();
    loop {
        let event = bounded_liveness(stream.next()).await.unwrap().unwrap();
        if event.get("type") == Some(&"model_request_started".into()) {
            break;
        }
    }
    session.abort(&"cancel probe".into()).unwrap();
    let mut terminal = None;
    while let Some(event) = bounded_liveness(stream.next()).await.unwrap() {
        if event.get("type") == Some(&"turn_completed".into()) {
            terminal = Some(event);
        }
    }
    drop(stream);
    bounded(session.dispose()).await.unwrap();
    bounded(registry.dispose()).await.unwrap();
    let terminal = terminal.unwrap();
    assert_eq!(
        terminal.get("result").and_then(|result| result.get("type")),
        Some(&"error".into()),
        "{terminal:?}"
    );
    let errors = terminal
        .get("result")
        .and_then(|result| result.get("errors"));
    let Some(JsValue::Array(errors)) = errors else {
        panic!("missing actual errors: {terminal:?}");
    };
    assert_eq!(errors[0].get("code"), Some(&"agent_invalid_state".into()));
    assert_eq!(errors[0].get("message"), Some(&"Host execution failed: Sidecar execute did not reach a terminal within the post-cancel exit deadline; progress is unknown.".into()));
    assert_peer_reaped(&pid).await;
}

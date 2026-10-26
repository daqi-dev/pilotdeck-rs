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
// dispose signals cancellation; a detached handler disposer may still own the
// last lease after the stream ends. Observe release with a deadline rather than
// treating either stream EOF or child reaping as a lease barrier (#842).
async fn wait_for_lease_release(
    runtime: &ProjectRuntime,
) -> Result<(), tokio::time::error::Elapsed> {
    timeout(Duration::from_secs(10), async {
        while runtime.lease_count() != 0 {
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
        Limits::default(),
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
    let signal = Cancellation::new();
    SessionPorts {
        model: RouterModelPortOptions::main_agent(),
        recorder: None,
        tool_results_dir: std::env::temp_dir().join("pdrs-host-factory-tools"),
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
        Limits::default(),
        Some(JsString::from("fixture")),
        move |_, shared| {
            assert!(
                Arc::ptr_eq(&shared, &expected_recorder),
                "ports factory must receive the session Recorder"
            );
            observed_calls.fetch_add(1, Ordering::SeqCst);
            ports(None)
        },
    )
    .into_live_session(recorder.clone(), move || {
        observed_history.fetch_add(1, Ordering::SeqCst);
        current_history.lock().unwrap().clone()
    });
    assert_eq!(port_calls.load(Ordering::SeqCst), 0);
    assert_eq!(history_calls.load(Ordering::SeqCst), 0);
    for index in 0..2 {
        let previous = parse_json(&format!(
            r#"{{"role":"assistant","content":"history-{index}"}}"#
        ))
        .unwrap();
        *history.lock().unwrap() = vec![previous.clone()];
        let mut turn = session
            .submit(parse_json(r#"{"type":"text","text":"hello"}"#).unwrap(), {
                let mut options = wire().as_object().unwrap().clone();
                options.insert(
                    "turnId".into(),
                    JsValue::String(format!("history-{index}").into()),
                );
                JsValue::Object(options)
            })
            .unwrap();
        let mut completed = None;
        while let Some(event) = bounded(turn.next()).await.unwrap() {
            if event.get("type") == Some(&"turn_completed".into()) {
                completed = Some(event);
            }
        }
        let completed = completed.expect("peer completed the turn");
        assert_eq!(
            completed
                .get("result")
                .and_then(|result| result.get("type")),
            Some(&"success".into()),
            "every history turn must execute successfully in its own sidecar"
        );
        assert_eq!(
            completed.get("messages"),
            Some(&JsValue::Array(vec![
                previous,
                parse_json(r#"{"role":"user","content":[{"type":"text","text":"hello"}]}"#)
                    .unwrap(),
            ])),
            "fresh history must reach the sidecar on every submit"
        );
        let bytes = std::fs::read_to_string(recorder.writer().path()).unwrap();
        assert_eq!(
            bytes
                .lines()
                .map(|line| parse_json(line).unwrap())
                .filter(|row| row.get("type") == Some(&"accepted_input".into()))
                .count(),
            index + 1,
            "each factory turn must persist accepted_input"
        );
        assert_eq!(port_calls.load(Ordering::SeqCst), index + 1);
        assert_eq!(history_calls.load(Ordering::SeqCst), index + 1);
        assert_peer_reaped(&pid_file).await;
    }
    session.dispose().await.unwrap();
    bounded(registry.dispose()).await.unwrap();
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
            .submit(parse_json(r#"{"type":"text","text":"hello"}"#).unwrap(), {
                let mut options = wire().as_object().unwrap().clone();
                options.insert(
                    "turnId".into(),
                    JsValue::String(format!("reload-{index}").into()),
                );
                JsValue::Object(options)
            })
            .unwrap();
        let mut completed = None;
        while let Some(event) = bounded(turn.next()).await.unwrap() {
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
    bounded(registry.dispose()).await.unwrap();
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
        .submit(
            parse_json(r#"{"type":"text","text":"hello"}"#).unwrap(),
            wire(),
        )
        .unwrap();
    assert_eq!(
        bounded(turn.next()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    bounded(turn.next()).await.unwrap().unwrap(); // host accepted
    drive_to_peer(turn.as_mut(), &pid_file, true).await;
    assert_eq!(old.lease_count(), 1);
    bounded(registry.reload()).await.unwrap();
    assert_eq!(old.state(), crate::runtime::project::RuntimeState::Retired);
    assert_eq!(old.lease_count(), 1);
    assert_ts_lease("retire-with-second-lease", 1, 0, "retired");
    drop(turn);
    // The dropped stream releases the execute future; the turn's cleanup
    // releases the lease, and only then may the old generation dispose.
    bounded(old.wait_disposed()).await.unwrap();
    assert_peer_reaped(&pid_file).await;
    assert_eq!(old.lease_count(), 0);
    assert_eq!(old.state(), crate::runtime::project::RuntimeState::Disposed);
    assert_ts_lease("after-last-release", 0, 1, "disposed");
    bounded(registry.dispose()).await.unwrap();
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
        .submit(
            parse_json(r#"{"type":"text","text":"hello"}"#).unwrap(),
            wire(),
        )
        .unwrap();
    assert_eq!(
        bounded(turn.next()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    bounded(turn.next()).await.unwrap().unwrap(); // host accepted
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
    bounded(runtime.wait_disposed()).await.unwrap();
    assert_peer_reaped(&pid_file).await;
    assert_eq!(runtime.lease_count(), 0);
    assert_eq!(
        runtime.state(),
        crate::runtime::project::RuntimeState::Disposed
    );
    bounded(registry.dispose()).await.unwrap();
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
        .submit(
            parse_json(r#"{"type":"text","text":"hello"}"#).unwrap(),
            wire(),
        )
        .unwrap();
    bounded(turn.next()).await.unwrap().unwrap(); // host started
    bounded(turn.next()).await.unwrap().unwrap(); // host accepted
    drive_to_peer(turn.as_mut(), &pid_file, false).await;
    let mut pending = Box::pin(turn.next());
    session.dispose().await.unwrap();
    // The prepared handler still owns the lease until cancellation is polled.
    assert_eq!(runtime.lease_count(), 1);
    let event = bounded(pending.as_mut()).await.unwrap().unwrap();
    assert_eq!(
        event.get("result").unwrap().get("type"),
        Some(&"aborted".into())
    );
    drop(pending);
    assert!(bounded(turn.next()).await.unwrap().is_none());
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
            .submit(
                parse_json(r#"{"type":"text","text":"hello"}"#).unwrap(),
                wire(),
            )
            .unwrap();
        assert_eq!(
            bounded(turn.next()).await.unwrap().unwrap().get("type"),
            Some(&"turn_started".into())
        );
        bounded(turn.next()).await.unwrap().unwrap(); // host accepted
        drive_to_peer(turn.as_mut(), &pid_file, true).await;
        session.dispose().await.unwrap();
        drop(turn);
        bounded(entered.notified()).await;
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
    bounded(async {
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

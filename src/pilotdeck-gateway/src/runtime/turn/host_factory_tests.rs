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
            ProjectResourcesOptions, ProjectRuntimeRegistry, ResourceFamilySource, RuntimeProfile,
            RuntimeProfileOverrides, RuntimeResult, StageContext, StageSink,
            interaction_permission::PermissionTurnOptions,
        },
        session::LiveSession,
        steer::MutationRecorder,
        turn::interaction::InteractionTurn,
    },
};
use pilotdeck_protocol::{
    JsObject, JsString, JsValue, admission::ExecutionIdentity, parse_json, stringify_json,
};
use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};
use tokio::time::timeout;

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

fn config(root: &Path) {
    std::fs::create_dir_all(root.join(".pilotdeck")).unwrap();
    std::fs::write(
        root.join("pilotdeck.yaml"),
        "schemaVersion: 1\nagent: {model: test/mock}\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock\n      models:\n        mock: {}\n",
    )
    .unwrap();
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
    let root = scratch.0.join("project");
    let home = scratch.0.join("home");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&home).unwrap();
    config(&home);
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
    resolve_deployment(&env, Path::new("/tmp")).unwrap()
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
        move |_| ports(context_runtime.clone()),
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
async fn reload_between_turns_uses_new_generation() {
    let scratch = Scratch::new();
    let registry = registry(&scratch);
    let old = registry.resolve(None).unwrap();
    let pid_file = scratch.0.join("between.pid");
    let session =
        factory(registry.clone(), peer("success", Some(&pid_file)), None).into_live_session();
    for (index, expected) in [(0, false), (1, true)] {
        if index == 1 {
            bounded(registry.reload()).await.unwrap();
        }
        let mut turn = session
            .submit(parse_json(r#"{"messages":[]}"#).unwrap(), wire())
            .unwrap();
        while bounded(turn.next()).await.unwrap().is_some() {}
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_reaped(pid).await;
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
    let session =
        factory(registry.clone(), peer("hold", Some(&pid_file)), None).into_live_session();
    let mut turn = session
        .submit(parse_json(r#"{"messages":[]}"#).unwrap(), wire())
        .unwrap();
    assert_eq!(
        bounded(turn.next()).await.unwrap().unwrap().get("type"),
        Some(&"turn_started".into())
    );
    assert_eq!(old.lease_count(), 1);
    bounded(registry.reload()).await.unwrap();
    assert_eq!(old.state(), crate::runtime::project::RuntimeState::Retired);
    assert_eq!(old.lease_count(), 1);
    assert_ts_lease("retire-with-second-lease", 1, 0, "retired");
    drop(turn);
    // The dropped stream releases the execute future; the turn's cleanup
    // releases the lease, and only then may the old generation dispose.
    bounded(old.wait_disposed()).await.unwrap();
    let pid: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_reaped(pid).await;
    assert_eq!(old.lease_count(), 0);
    assert_eq!(old.state(), crate::runtime::project::RuntimeState::Disposed);
    assert_ts_lease("after-last-release", 0, 1, "disposed");
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
        let error = factory.create(&identity()).await.err().unwrap();
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
    let error = factory.create(&identity()).await.err().unwrap();
    assert!(
        error.message.contains("hello") || error.message.contains("handshake"),
        "{error:?}"
    );
    assert_eq!(runtime.lease_count(), 0);
    let pid: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_reaped(pid).await;
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
    .into_live_session();
    let mut turn = session
        .submit(parse_json(r#"{"messages":[]}"#).unwrap(), wire())
        .unwrap();
    let mut pending = Box::pin(turn.next());
    bounded(async {
        tokio::select! {
            result = &mut pending => panic!("host creation resolved before cancel: {result:?}"),
            _ = async {
                while !pid_file.exists() {
                    tokio::task::yield_now().await;
                }
            } => {}
        }
    })
    .await;
    session.dispose().await.unwrap();
    let event = bounded(pending.as_mut()).await.unwrap().unwrap();
    assert_eq!(
        event.get("result").unwrap().get("type"),
        Some(&"aborted".into())
    );
    drop(pending);
    assert!(bounded(turn.next()).await.unwrap().is_none());
    assert_eq!(runtime.lease_count(), 0);
    let pid: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_reaped(pid).await;
    bounded(registry.dispose()).await.unwrap();
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

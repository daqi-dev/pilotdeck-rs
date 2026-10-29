//! #748 production Rust submission through the fixed real TS sidecar.
#[allow(dead_code)]
#[path = "common/baseline.rs"]
mod baseline;
#[path = "../src/mutants.rs"]
mod mutants;
#[allow(dead_code)]
#[path = "common/negative_receipt.rs"]
mod test_negative_receipt;
#[allow(dead_code)]
#[path = "../src/test_process.rs"]
mod test_process;
#[path = "common/scratch.rs"]
mod test_scratch;
use pilotdeck_gateway::{
    host::{
        HostError, HostFuture,
        modules::{
            CompositionSwitches, capability, model_port::RouterModelPortOptions,
            permission::PermissionDecision, tool_context::*,
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
        replacement::ReplacementCoordinator,
        session::{
            SessionContext, SessionOwner, SessionOwnerOptions,
            factory::{
                FactoryOptions, FactoryStorage, PreparedResources, PreparedSession,
                ProjectSessionFactory, SessionBinding,
            },
        },
        steer::MutationRecorder,
        tools::{
            ExecutableTool, SchedulerMode, ToolError, ToolFuture, ToolOutput, ToolRegistry,
            ToolRunContext, ToolRuntime, builtin::read_file::ReadFileTool,
        },
        turn::{
            admission::{GatewaySubmitAdmission, SubmitAdmissionOptions},
            coordinator::TurnEventCoordinator,
            host_factory::{Deployment, SessionPorts, TurnHostFactory},
            interaction::InteractionTurnInput,
            projector::{
                GatewayAgentEventProjector, GatewayAgentEventProjectorOptions,
                GatewayToolResultArtifactStore,
            },
            publish::{FileRegistry, TurnRunRegistry},
            runner::tail::TailConfig,
            submit::{
                SubmitError, SubmitFuture, SubmitSnapshots, TurnSubmitter, WorkspaceAdmissions,
            },
            submit_host::{
                GatewaySubmitHost, NativeSubmitReplacement, SubmitAttachmentLease,
                SubmitCwdService, SubmitHostServices,
            },
        },
    },
    storage::{FileRunRegistry, JsonlTranscriptStore, RunRef, TranscriptKey},
};
use pilotdeck_protocol::{
    JsObject, JsString, JsValue, admission::ExecutionIdentity, parse_json, stringify_json,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime},
};
use tokio::time::{sleep, timeout};
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/live-e2e");
const SESSION: &str = "live-e2e-session";
fn js(value: Value) -> JsValue {
    parse_json(&value.to_string()).expect("#748 JSON input")
}
fn val(value: &JsValue) -> Value {
    serde_json::from_str(
        &stringify_json(value)
            .expect("#748 serialize JS")
            .expect("#748 defined JSON"),
    )
    .expect("#748 JSON output")
}
fn obj(value: Value) -> JsObject {
    js(value).as_object().expect("#748 object").clone()
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("#748 read evidence"))
        .expect("#748 parse evidence")
}
fn publish(path: &Path, value: &str) {
    let temp = path.with_extension("tmp");
    fs::write(&temp, value).expect("#748 atomic evidence write");
    fs::rename(temp, path).expect("#748 atomic evidence publish");
}
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
/// The TS process has its own group, so HostClient Drop cannot reap it alone.
struct Descendants(PathBuf);
impl Drop for Descendants {
    fn drop(&mut self) {
        use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
        let Some(identity) = fs::read(self.0.join("process.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        else {
            return;
        };
        let groups: Vec<_> = ["mitmPid", "childPid"]
            .into_iter()
            .filter_map(|key| {
                identity[key]
                    .as_i64()
                    .and_then(|pid| i32::try_from(pid).ok())
                    .filter(|pid| *pid > 1 && *pid as u32 != std::process::id())
                    .and_then(Pid::from_raw)
            })
            .collect();
        for signal in [Signal::TERM, Signal::KILL] {
            for pid in &groups {
                if test_kill_process_group(*pid).is_ok() {
                    let _ = kill_process_group(*pid, signal);
                }
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while groups
                .iter()
                .any(|pid| test_kill_process_group(*pid).is_ok())
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
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
struct Environment;
impl TurnEnvironment for Environment {
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
struct Observer;
impl capability::CapabilityResultObserver for Observer {
    fn on_capability_results(&self, _: Vec<JsObject>) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
}
struct Recorder;
impl MutationRecorder for Recorder {
    fn record(&self, _: JsString, _: JsObject) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
}
struct AllowRead;
impl PermissionDecision for AllowRead {
    fn decide(
        &self,
        tool: Arc<dyn capability::ToolDefinition>,
        _: JsValue,
        _: capability::RuntimeContext,
        _: JsString,
    ) -> HostFuture<JsValue> {
        assert_eq!(
            tool.fields().get("name"),
            Some(&"read_file".into()),
            "#748 only read permission"
        );
        Box::pin(async {
            Ok(js(
                json!({"type":"allow","reason":{"type":"runtime","message":"fixture"}}),
            ))
        })
    }
}
/// Gate inside ToolRuntime.execute, delegating validation and real file reads.
struct GatedRead {
    inner: ReadFileTool,
    directory: PathBuf,
    gate: bool,
}
impl capability::ToolDefinition for GatedRead {
    fn fields(&self) -> &JsObject {
        self.inner.fields()
    }
    fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
        self.inner.is_read_only(input)
    }
    fn is_concurrency_safe(&self, input: &JsValue) -> Result<JsValue, HostError> {
        self.inner.is_concurrency_safe(input)
    }
}
impl ExecutableTool for GatedRead {
    fn validate_input(
        &self,
        input: JsValue,
        context: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        self.inner.validate_input(input, context)
    }
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let directory = self.directory.clone();
        let gate = self.gate;
        let execution = self.inner.execute(input, context);
        Box::pin(async move {
            if gate {
                publish(&directory.join("tool-entered.json"), "{\"entered\":true}");
                // The predecessor deliberately never finishes. Host cancellation owns this future.
                if timeout(Duration::from_secs(10), std::future::pending::<()>())
                    .await
                    .is_err()
                {
                    return Err(ToolError::failed("#748 tool gate did not cancel"));
                }
            }
            execution.await
        })
    }
}
type SharedRecorder = Arc<Mutex<Option<Arc<pilotdeck_gateway::storage::recorder::Recorder>>>>;
struct Services {
    recorder: SharedRecorder,
}
impl SubmitHostServices for Services {
    fn workspace_id(&self, _: &JsValue) -> Result<Option<JsString>, SubmitError> {
        Ok(None)
    }
    fn snapshots(&self) -> Option<Arc<dyn SubmitSnapshots>> {
        None
    }
    fn snapshots_required(&self) -> bool {
        false
    }
    fn cwd(&self) -> Option<Arc<dyn SubmitCwdService>> {
        None
    }
    fn refresh(&self) -> SubmitFuture<'_, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
    }
    fn run_policy(&self) -> Option<JsValue> {
        None
    }
    fn resolve_attachments<'a>(
        &'a self,
        _: &'a JsValue,
    ) -> SubmitFuture<'a, Result<SubmitAttachmentLease, SubmitError>> {
        Box::pin(async { Err(SubmitError::new("gateway_submit_failed", "unreachable")) })
    }
    fn prepare_attachment_turn<'a>(
        &'a self,
        message: &'a JsValue,
        _: Vec<JsValue>,
        _: Option<&JsString>,
        _: Option<&JsString>,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        let message = message.clone();
        Box::pin(async move { Ok(js(json!({"type":"text","text":val(&message)}))) })
    }
    fn resolve_model_selection<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        let _ = input;
        Box::pin(async { Ok(js(json!({"source":"default"}))) })
    }
    fn record_failure<'a>(
        &'a self,
        run: &'a RunRef,
        status: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        let recorder = self
            .recorder
            .lock()
            .expect("#748 shared writer lock")
            .clone()
            .expect("#748 owned session writer");
        let run = run.clone();
        let status = status.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                recorder.writer().record_agent_status_message(
                    &run.session_key,
                    &run.run_id,
                    &status,
                )
            })
            .await
            .expect("#748 status writer task")
            .map_err(|error| SubmitError::new("gateway_submit_failed", error.to_string()))
        })
    }
    fn after_turn(&self, _: &RunRef) -> Result<(), SubmitError> {
        Ok(())
    }
    fn now_ms(&self) -> f64 {
        0.0
    }
    fn abort_turn_timeout_ms(&self) -> u64 {
        5000
    }
}
fn registry(directory: &Path, port: u64) -> Arc<ProjectRuntimeRegistry> {
    let home = directory.join("home");
    let sandbox = directory.join("sandbox");
    fs::create_dir_all(home.join(".pilotdeck")).expect("#748 home");
    fs::write(home.join("pilotdeck.yaml"),format!("schemaVersion: 1\nagent: {{model: live-e2e/mock-model}}\nmodel:\n  providers:\n    live-e2e:\n      protocol: openai\n      url: http://127.0.0.1:{port}/v1\n      apiKey: mock-only\n      models:\n        mock-model: {{}}\n")).expect("#748 local provider config");
    let mut env = HashMap::new();
    env.insert("PILOT_HOME".into(), home.to_string_lossy().into_owned());
    let factory = ProjectResourcesFactory::new(ProjectResourcesOptions {
        pilot_home: home.clone(),
        cwd: sandbox.clone(),
        env,
        homedir: Some(home.clone()),
        builtin_skills_root: None,
        now: Arc::new(|| SystemTime::UNIX_EPOCH + Duration::from_millis(1_792_972_800_000)),
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
        .with_plugins(pilotdeck_gateway::runtime::plugins::PluginRuntimeOptions {
            project_root: sandbox.clone(),
            pilot_home: home,
            builtin_skills_root: None,
            builtin_plugins: vec![],
            builtin_plugins_enabled: HashMap::new(),
        })
        .with_interaction(),
    });
    Arc::new(ProjectRuntimeRegistry::new(sandbox, Arc::new(factory)))
}
fn ports(
    identity: &ExecutionIdentity,
    binding: &SessionBinding,
    directory: &Path,
    gate: bool,
) -> SessionPorts {
    let cwd = directory.join("sandbox").to_string_lossy().into_owned();
    let signal = Cancellation::new();
    let tools = ToolRegistry::new(None, None);
    tools
        .register(Arc::new(GatedRead {
            inner: ReadFileTool::new(),
            directory: directory.into(),
            gate,
        }))
        .expect("#748 real read registered");
    let runtime = Arc::new(ToolRuntime::new(
        tools,
        Arc::new(AllowRead),
        SchedulerMode::Sequential,
    ));
    let config = obj(
        json!({"cwd":cwd,"permissionMode":"default","permissionContext":{"mode":"default","canPrompt":false},"env":{}}),
    );
    let turn_binding = TurnBinding {
        config: config.clone(),
        input: obj(
            json!({"sessionId":String::from_utf16_lossy(identity.session_id.units()),"turnId":String::from_utf16_lossy(identity.turn_id.units())}),
        ),
        checkpoint: binding.checkpoint.clone(),
        abort_signal: Some(Arc::new(signal.clone())),
        environment: Arc::new(Environment),
    };
    let tool_ports = ToolContextPorts {
        clock: Some(Arc::new(|| "2026-10-26T00:00:00.000Z".into())),
        ..Default::default()
    };
    SessionPorts {
        model: RouterModelPortOptions::main_agent(),
        recorder: None,
        tool_results_dir: directory.join("tool-results"),
        context_runtime: None,
        context_identity: ContextRequestIdentity::new(turn_binding.clone(), tool_ports.clone()),
        context_config: config,
        tool_execution: runtime,
        tool_context: Arc::new(CapabilityContext {
            builder: ToolContextBuilder::new(turn_binding, tool_ports, true),
            plan_todo: None,
        }),
        tool_observer: Arc::new(Observer),
        plan_todo: None,
        turn: Default::default(),
        interaction: InteractionTurnInput {
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
                        cwd: cwd.into(),
                        can_prompt: Some(false),
                        ..Default::default()
                    },
                ),
                rules: pilotdeck_gateway::config::permission::RuleSetHandle::empty(),
                coordinator: Arc::default(),
                emit: Arc::new(|_| true),
                timeout: Duration::from_secs(5),
            },
            hooks: vec![],
            contribution: None,
            base_env: None,
            signal,
            steer_recorder: Some(Arc::new(Recorder)),
            record_message: Some(Arc::new(|_| Box::pin(async { Ok(()) }))),
            messages: vec![],
            switches: CompositionSwitches {
                plan_todo: false,
                elicitation_available: None,
            },
            turn_ports: Default::default(),
        },
    }
}
fn canonical(value: &mut Value, directory: &Path) {
    match value {
        Value::String(text) => {
            *text = text.replace(directory.to_string_lossy().as_ref(), "<CASE>");
        }
        Value::Array(items) => {
            for item in items {
                canonical(item, directory)
            }
        }
        Value::Object(fields) => {
            for value in fields.values_mut() {
                canonical(value, directory);
            }
        }
        _ => {}
    }
}
/// Compare all recorder payloads; native session metadata is checked independently.
fn durable_entries(transcript: &Value, native: bool) -> Value {
    let entries = transcript.as_array().expect("#748 transcript array");
    let mut parent = Value::Null;
    let mut identities = std::collections::HashSet::new();
    let mut selected = Vec::new();
    let mut metadata = Vec::new();
    let mut prompt = None;
    for (index, entry) in entries.iter().enumerate() {
        negative_assert_eq!(
            entry["sequence"],
            json!(index + 1),
            "#748 transcript sequence"
        );
        negative_assert_eq!(entry["parentEntryId"], parent, "#748 transcript parent");
        parent = entry["entryId"].clone();
        negative_assert!(
            parent
                .as_str()
                .is_some_and(|id| !id.is_empty() && identities.insert(id.to_owned())),
            "#748 transcript entry identity"
        );
        negative_assert_eq!(
            entry["sessionId"],
            json!(SESSION),
            "#748 transcript session identity"
        );
        let kind = entry["type"].as_str().expect("#748 transcript entry type");
        if kind == "accepted_input" {
            prompt = Some(entry["messages"][0]["content"][0]["text"].clone());
        }
        let mut primary = entry.as_object().expect("#748 transcript object").clone();
        for key in ["entryId", "parentEntryId", "sequence"] {
            primary.remove(key);
        }
        if kind == "session_metadata" {
            if !mutants::on("live-metadata") {
                metadata.push(Value::Object(primary));
            }
            continue;
        }
        negative_assert!(
            [
                "turn_started",
                "accepted_input",
                "assistant_message",
                "tool_result_message",
                "durable_message",
                "turn_result",
                "agent_status_message",
                "model_request",
                "model_stream_event",
                "step_started",
                "step_completed"
            ]
            .contains(&kind),
            "#748 known primary transcript kind"
        );
        selected.push(Value::Object(primary));
    }
    if native {
        let prompt = prompt.expect("#748 session metadata follows accepted input");
        let make = |snapshot| {
            let mut fields = json!({"firstPrompt":prompt,"lastPrompt":prompt,"updatedAt":"2026-10-26T00:00:00.000Z"});
            if snapshot {
                fields["isSnapshot"] = json!(true);
            }
            json!({"createdAt":"2026-10-26T00:00:00.000Z","sessionId":SESSION,"turnId":entries[0]["turnId"],"type":"session_metadata","metadata":fields})
        };
        negative_assert_eq!(
            metadata,
            vec![make(false), make(true)],
            "#748 native session metadata payloads"
        );
    }
    Value::Array(selected)
}
fn mask_timestamp(value: &mut Value) {
    let valid = value.as_str().is_some_and(|text| {
        text.len() == 24
            && text.ends_with('Z')
            && pilotdeck_transport::system_clock::parse_node22_date(&text.into())
                .is_ok_and(|parsed| parsed.is_some())
    });
    negative_assert!(valid, "#748 raw timestamp shape");
    *value = json!("2026-10-26T00:00:00.000Z");
}
fn normalize_observation(observed: &mut Value, raw: &[JsValue], directory: &Path, port: u64) {
    use sha2::{Digest, Sha256};
    let record = &mut observed["record"];
    negative_assert_eq!(
        record["ownerPid"],
        json!(std::process::id()),
        "#748 raw owner pid"
    );
    record["ownerPid"] = json!("<PID>");
    mask_timestamp(&mut record["acceptedAt"]);
    mask_timestamp(&mut record["updatedAt"]);
    let mut requests = HashMap::new();
    for (index, entry) in observed["transcript"]
        .as_array_mut()
        .expect("#748 transcript to normalize")
        .iter_mut()
        .enumerate()
    {
        mask_timestamp(&mut entry["createdAt"]);
        match entry["type"].as_str().expect("#748 durable type") {
            "session_metadata" => mask_timestamp(&mut entry["metadata"]["updatedAt"]),
            "turn_result" if entry["result"]["type"] == "error" => {
                mask_timestamp(&mut entry["result"]["startedAt"]);
                mask_timestamp(&mut entry["result"]["completedAt"]);
            }
            "model_request" => {
                let mut request = raw[index]
                    .get("request")
                    .and_then(JsValue::as_object)
                    .expect("#748 raw model request")
                    .clone();
                request.insert("stream".into(), JsValue::Bool(true));
                requests.insert(entry["step"].as_u64().expect("#748 model step"), request);
            }
            "model_stream_event" if entry["event"]["type"] == "request_started" => {
                let request = requests
                    .get(&entry["step"].as_u64().expect("#748 stream step"))
                    .expect("#748 request before fingerprint");
                let bytes = stringify_json(&JsValue::Object(request.clone()))
                    .expect("#748 original request JSON")
                    .expect("#748 defined original request");
                negative_assert_eq!(
                    entry["event"]["requestFingerprint"],
                    json!(format!("{:x}", Sha256::digest(bytes))),
                    "#748 original request fingerprint"
                );
                entry["event"]["requestFingerprint"] = json!("<REQUEST-SHA256-VERIFIED>");
                negative_assert_eq!(
                    entry["event"]["providerBaseUrl"],
                    json!(format!("http://127.0.0.1:{port}/v1")),
                    "#748 loopback provider identity"
                );
                entry["event"]["providerBaseUrl"] = json!("http://127.0.0.1:<PORT>/v1");
            }
            _ => {}
        }
    }
    canonical(observed, directory);
}
fn conversation(transcript: &Value) -> Value {
    let mut messages = Vec::new();
    for entry in transcript.as_array().expect("#748 transcript conversation") {
        match entry["type"].as_str() {
            Some("accepted_input") => messages.extend(
                entry["messages"]
                    .as_array()
                    .expect("#748 accepted messages")
                    .iter()
                    .cloned(),
            ),
            Some("assistant_message" | "tool_result_message" | "durable_message") => {
                messages.push(entry["message"].clone())
            }
            _ => {}
        }
    }
    Value::Array(messages)
}
fn expectation(scenario: &str) -> Value {
    if scenario != "early-final" {
        return read(&Path::new(FIXTURE).join(format!("expected/{scenario}.json")));
    }
    // Explicit A7.2-TURN-EARLYFINAL-001-R2 policy; the TS success golden is retained.
    let baseline = read(&Path::new(FIXTURE).join("expected/early-final.json"));
    negative_assert_eq!(
        baseline["events"]
            .as_array()
            .expect("#748 TS early-final events")
            .last()
            .expect("#748 TS terminal")["finishReason"],
        json!("mitm_premature_final"),
        "#748 TS early-final observed difference"
    );
    let mut reference = fs::read_to_string(Path::new(FIXTURE).join("expected/duplicate.json"))
        .expect("#748 error projection reference");
    if mutants::on("live-derived-run-count") {
        reference = reference.replacen("run-duplicate", "missing-run-anchor", 1);
    }
    if mutants::on("live-derived-error-count") {
        reference = reference.replacen(
            "Sidecar event sequence mismatch: expected 1, got 0.",
            "missing-error-anchor",
            1,
        );
    }
    negative_assert_eq!(
        reference.matches("run-duplicate").count(),
        19,
        "#748 early-final run identity replacement count"
    );
    negative_assert_eq!(
        reference
            .matches("Sidecar event sequence mismatch: expected 1, got 0.")
            .count(),
        7,
        "#748 early-final error replacement count"
    );
    let mut expected: Value = serde_json::from_str(
        &reference
            .replace("run-duplicate", "run-early-final")
            .replace(
                "Sidecar event sequence mismatch: expected 1, got 0.",
                "Sidecar success final arrived before loop completion.",
            ),
    )
    .expect("#748 registered early-final expectation");
    if mutants::on("live-derived-event-shape") {
        expected["events"]
            .as_array_mut()
            .expect("#748 derived event mutation")
            .remove(0);
    }
    if mutants::on("live-derived-transcript-shape") {
        expected["transcript"]
            .as_array_mut()
            .expect("#748 derived transcript mutation")
            .remove(0);
    }
    negative_assert_eq!(
        expected["events"]
            .as_array()
            .expect("#748 early-final reference events")
            .iter()
            .map(|row| row["type"]
                .as_str()
                .expect("#748 early-final reference event type"))
            .collect::<Vec<_>>(),
        [
            "turn_started",
            "input_accepted",
            "model_request_started",
            "agent_status",
            "error",
            "turn_completed"
        ],
        "#748 early-final explicit error event structure"
    );
    negative_assert_eq!(
        expected["transcript"]
            .as_array()
            .expect("#748 early-final reference transcript")
            .iter()
            .map(|row| row["type"]
                .as_str()
                .expect("#748 early-final reference row type"))
            .collect::<Vec<_>>(),
        [
            "turn_started",
            "accepted_input",
            "turn_result",
            "agent_status_message"
        ],
        "#748 early-final explicit error transcript structure"
    );
    expected
}
fn verify_artifacts() {
    use sha2::{Digest, Sha256};
    let mut oracle = read(&Path::new(FIXTURE).join("oracle.json"));
    if mutants::on("live-mitm-hash") {
        oracle["apparatusSha256"]["stdio_mitm.py"] = json!("0".repeat(64));
    }
    if mutants::on("live-provenance-list") {
        oracle["apparatusSha256"]
            .as_object_mut()
            .expect("#748 mutable apparatus manifest")
            .remove("stdio_mitm.py");
    }
    let names = |field: &str| {
        oracle[field]
            .as_object()
            .expect("#748 provenance map")
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
    };
    negative_assert_eq!(
        names("apparatusSha256"),
        [
            "stdio_mitm.py",
            "io_deadline.py",
            "driver.mts",
            "mock_provider.mjs",
            "determinism.mjs",
            "capture.py"
        ]
        .map(str::to_string)
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>(),
        "#748 exact provenance inventory"
    );
    negative_assert_eq!(
        names("expectedSha256"),
        [
            "text",
            "tool",
            "cancel-stream",
            "cancel-tool",
            "drop",
            "duplicate",
            "early-final"
        ]
        .map(str::to_string)
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>(),
        "#748 exact provenance inventory"
    );
    for (name, digest) in oracle["apparatusSha256"]
        .as_object()
        .expect("#748 apparatus hashes")
    {
        let bytes = fs::read(Path::new(FIXTURE).join(name)).expect("#748 apparatus source bytes");
        negative_assert_eq!(
            json!(format!("{:x}", Sha256::digest(bytes))),
            *digest,
            "#748 fixed apparatus hash"
        );
    }
    for (name, digest) in oracle["expectedSha256"]
        .as_object()
        .expect("#748 golden hashes")
    {
        let mut bytes = fs::read(Path::new(FIXTURE).join(format!("expected/{name}.json")))
            .expect("#748 golden bytes");
        if mutants::on("live-golden-hash") {
            bytes.push(b' ');
        }
        negative_assert_eq!(
            json!(format!("{:x}", Sha256::digest(bytes))),
            *digest,
            "#748 golden byte hash"
        );
    }
}
#[test]
fn live_artifact_provenance() {
    verify_artifacts();
}

#[test]
#[ignore = "requires fixed PDRS_BASELINE and Node v22.22.0"]
fn live_ts_client_payload() {
    verify_artifacts();
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../_tmp/20261026-issue-748")
        .join(format!("ts-client-{}", uuid::Uuid::new_v4()));
    let mut command = Command::new("python3");
    command
        .args(["-B", &format!("{FIXTURE}/capture.py"), "text"])
        .arg(&directory)
        .env_remove("LIVE_E2E_MUTANT");
    if mutants::on("live-client-payload") {
        command.env("LIVE_E2E_MUTANT", "client-payload");
    }
    let output = command
        .output()
        .expect("#748 independent TS client capture");
    assert!(
        output.status.success(),
        "#748 TS client capture failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let actual = read(&directory.join("result.json"));
    let expected = read(&Path::new(FIXTURE).join("expected/text.json"));
    negative_assert_eq!(
        serde_json::to_vec(&actual["events"]).expect("#748 client bytes"),
        serde_json::to_vec(&expected["events"]).expect("#748 golden client bytes"),
        "#748 TS client payload bytes"
    );
}
async fn run_case(scenario: &str) {
    verify_artifacts();
    assert!(
        std::env::var_os("LIVE_E2E_MITM").is_none(),
        "#748 fixed MITM path"
    );
    let baseline_path =
        PathBuf::from(std::env::var_os("PDRS_BASELINE").expect("#748 set PDRS_BASELINE"));
    static VERIFIED: OnceLock<()> = OnceLock::new();
    VERIFIED.get_or_init(|| {
        baseline::oracle(include_str!("fixtures/live-e2e/oracle.json"));
        let version = Command::new("node")
            .arg("--version")
            .output()
            .expect("#748 Node version");
        assert_eq!(version.stdout, b"v22.22.0\n", "#748 pinned Node runtime");
    });
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../_tmp/20261026-issue-748")
        .join(format!("rust-{scenario}-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).expect("#748 exclusive case directory");
    let directory = fs::canonicalize(directory).expect("#748 canonical case directory");
    let _descendants = Descendants(directory.clone());
    fs::create_dir(directory.join("sandbox")).expect("#748 sandbox");
    fs::create_dir(directory.join("home")).expect("#748 home directory");
    fs::write(
        directory.join("sandbox/tool-target.txt"),
        "live-e2e read tool payload\n",
    )
    .expect("#748 tool target");
    let mock_err =
        fs::File::create_new(directory.join("mock-stderr.log")).expect("#748 mock stderr");
    let mut mock = ChildGuard(
        Command::new("node")
            .arg(format!("{FIXTURE}/mock_provider.mjs"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").expect("#748 PATH"))
            .env("HOME", directory.join("home"))
            .env("LIVE_E2E_MOCK_PORT", "0")
            .env("LIVE_E2E_MOCK_LOG", directory.join("mock.jsonl"))
            .env("LIVE_E2E_MOCK_READY", directory.join("mock-ready.json"))
            .env("LIVE_E2E_INJECTION", scenario)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(mock_err)
            .spawn()
            .expect("#748 start local mock"),
    );
    timeout(Duration::from_secs(5), async {
        while !directory.join("mock-ready.json").exists() {
            assert!(
                mock.0.try_wait().expect("#748 mock status").is_none(),
                "#748 mock unexpectedly exited"
            );
            sleep(Duration::from_millis(10)).await
        }
    })
    .await
    .expect("#748 mock startup deadline");
    let port = read(&directory.join("mock-ready.json"))["port"]
        .as_u64()
        .expect("#748 mock assigned port");
    let project = registry(&directory, port);
    let mut args = vec![
        format!("{FIXTURE}/stdio_mitm.py"),
        "--log".into(),
        directory
            .join("frames.jsonl")
            .to_string_lossy()
            .into_owned(),
        "--summary".into(),
        directory
            .join("mitm-summary.json")
            .to_string_lossy()
            .into_owned(),
    ];
    if ["drop", "duplicate", "early-final"].contains(&scenario) && !mutants::on("live-injection") {
        fs::write(directory.join("plan.json"),json!({"injections":[{"action":if scenario=="early-final"{"early_final"}else{scenario},"direction":"s2h","kind":"event","final":false,"nth":if scenario=="duplicate"{1}else{2}}]}).to_string()).expect("#748 injection plan");
        args.extend([
            "--plan".into(),
            directory.join("plan.json").to_string_lossy().into_owned(),
        ]);
    }
    args.extend([
        "--".into(),
        "node".into(),
        "--import".into(),
        format!("{FIXTURE}/determinism.mjs"),
        "--import".into(),
        baseline_path
            .join("node_modules/tsx/dist/loader.mjs")
            .to_string_lossy()
            .into_owned(),
        baseline_path
            .join("src/cli/pilotdeck-agent-loop-sidecar.ts")
            .to_string_lossy()
            .into_owned(),
    ]);
    let deployment = Deployment::Stdio {
        command: "python3".into(),
        args,
        cwd: directory.join("sandbox"),
        env: HashMap::from([
            (
                "PATH".into(),
                std::env::var("PATH").expect("#748 PATH text"),
            ),
            (
                "HOME".into(),
                directory.join("home").to_string_lossy().into_owned(),
            ),
        ]),
    };
    let shared_recorder: SharedRecorder = Arc::default();
    let prepare_recorder = shared_recorder.clone();
    let prepare_project = project.clone();
    let prepare_dir = directory.clone();
    let storage_dir = directory.clone();
    let gate = scenario == "cancel-tool";
    let factory = Arc::new(ProjectSessionFactory::new(FactoryOptions {
        writer_busy_wait_timeout: Duration::from_secs(5),
        prepare: Arc::new(move |_, _| {
            let project = prepare_project.clone();
            let directory = prepare_dir.clone();
            let deployment = deployment.clone();
            let shared_recorder = prepare_recorder.clone();
            Box::pin(async move {
                let config = obj(
                    json!({"provider":"live-e2e","model":"mock-model","cwd":directory.join("sandbox"),"permissionMode":"default"}),
                );
                Ok(PreparedSession {
                    resources: PreparedResources::default(),
                    host: Box::new(move |binding| {
                        *shared_recorder.lock().expect("#748 retain owned writer") =
                            Some(binding.recorder.clone());
                        Ok(TurnHostFactory::new(
                            project,
                            deployment,
                            pilotdeck_gateway::host::Limits {
                                close_timeout: Duration::from_secs(5),
                                ..Default::default()
                            },
                            Some("pilotdeck-agent-loop".into()),
                            move |identity, _| ports(identity, &binding, &directory, gate),
                        )
                        .with_agent_config(config))
                    }),
                    tail: TailConfig {
                        collect_file_artifacts: false,
                        ..Default::default()
                    },
                })
            })
        }),
        create_storage: Arc::new(move |context| {
            let directory = storage_dir.clone();
            Box::pin(async move {
                Ok(FactoryStorage::new(
                    Arc::new(JsonlTranscriptStore::new()),
                    TranscriptKey {
                        project_root: directory.join("sandbox"),
                        pilot_home: directory.join("home"),
                        transcript_path: directory.join("transcript.jsonl"),
                        session_id: context.session_key,
                    },
                ))
            })
        }),
        admission: None,
        diagnostic: Arc::new(|_| {}),
    }));
    let mut options = SessionOwnerOptions::new(factory.clone());
    options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(options);
    let replacement = Arc::new(ReplacementCoordinator::new(
        Default::default(),
        Arc::new(owner.clone()),
        5000,
        None,
    ));
    let runs = Arc::new(
        FileRegistry::new(
            FileRunRegistry::open(directory.join("registry.json")).expect("#748 registry open"),
        )
        .expect("#748 registry worker"),
    );
    let submitter = Arc::new(TurnSubmitter {
        owner: owner.clone(),
        coordinator: Arc::new(TurnEventCoordinator::new()),
        registry: Some(runs.clone()),
        workspace_admissions: WorkspaceAdmissions::default(),
        host: Arc::new(GatewaySubmitHost::new(
            Arc::new(Services {
                recorder: shared_recorder,
            }),
            Arc::new(NativeSubmitReplacement((*replacement).clone())),
            Arc::new(GatewaySubmitAdmission::new(SubmitAdmissionOptions::new(
                replacement,
            ))),
        )),
        projector: Arc::new(
            GatewayAgentEventProjector::new(GatewayAgentEventProjectorOptions {
                tool_result_artifacts: Some(Arc::new(
                    GatewayToolResultArtifactStore::new(directory.join("artifacts"), 4096)
                        .expect("#748 artifact root"),
                )),
                ..Default::default()
            })
            .expect("#748 projector"),
        ),
    });
    let run_id = format!("run-{scenario}");
    let target = directory.join("sandbox/tool-target.txt");
    let prompt = match scenario {
        "tool" => format!("LIVE_E2E_TOOL {}", target.display()),
        "cancel-tool" => format!("LIVE_E2E_CANCEL_TOOL {}", target.display()),
        "cancel-stream" => "LIVE_E2E_HANG".into(),
        _ => "LIVE_E2E_TEXT".into(),
    };
    let mut stream = submitter.submit(js(
        json!({"sessionKey":SESSION,"runId":run_id,"channelKey":"test","message":prompt}),
    ));
    let cancel_owner = owner.clone();
    let cancel_run: JsString = run_id.clone().into();
    let cancel_dir = directory.clone();
    let mut abort = if gate {
        Some(tokio::spawn(async move {
            timeout(Duration::from_secs(15), async {
                while !cancel_dir.join("tool-entered.json").exists() {
                    sleep(Duration::from_millis(5)).await
                }
            })
            .await
            .expect("#748 tool execution window");
            cancel_owner
                .abort_turn(
                    &SESSION.into(),
                    Some(&cancel_run),
                    Some(&"live_e2e_cancel".into()),
                )
                .await
                .expect("#748 tool cancellation")
        }))
    } else {
        None
    };
    let mut events = Vec::new();
    timeout(Duration::from_secs(45), async {
        while let Some(event) = stream.next().await.expect("#748 submission stream") {
            let value = val(&event);
            if scenario == "cancel-stream"
                && value["type"] == "assistant_text_delta"
                && abort.is_none()
                && !mutants::on("live-cancel")
            {
                let owner = owner.clone();
                let run: JsString = run_id.clone().into();
                abort = Some(tokio::spawn(async move {
                    owner
                        .abort_turn(&SESSION.into(), Some(&run), Some(&"live_e2e_cancel".into()))
                        .await
                        .expect("#748 streaming cancellation")
                }));
            }
            if scenario == "duplicate" && value["type"] == "model_request_started" {
                publish(
                    &directory.join("event-visible.json"),
                    "{\"type\":\"model_request_started\"}",
                );
            }
            events.push(value);
        }
    })
    .await
    .expect("#748 bounded submission EOF");
    if let Some(abort) = abort {
        abort.await.expect("#748 cancellation task");
    }
    let run = RunRef {
        session_key: SESSION.into(),
        project_key: None,
        run_id: run_id.into(),
    };
    let persisted = runs.events(&run).await.expect("#748 registry drain");
    let active = owner.has_active_turn(&SESSION.into());
    owner
        .close(&SESSION.into())
        .await
        .expect("#748 close completed session");
    let restored = owner
        .get_or_create(SessionContext {
            session_key: SESSION.into(),
            project_key: None,
            channel_key: "test".into(),
            allowed_tools: None,
            disallowed_tools: None,
        })
        .await
        .expect("#748 reopen completed session");
    let restored_messages = restored
        .reload_snapshot()
        .expect("#748 restore durable state")
        .state
        .messages;
    owner.shutdown().await.expect("#748 session shutdown");
    factory.dispose().await.expect("#748 factory dispose");
    project.dispose().await.expect("#748 project dispose");
    let record = FileRunRegistry::open(directory.join("registry.json"))
        .expect("#748 reopen registry")
        .get(&run)
        .expect("#748 read registry")
        .expect("#748 accepted run");
    let transcript_bytes =
        fs::read_to_string(directory.join("transcript.jsonl")).expect("#748 durable transcript");
    let raw: Vec<JsValue> = transcript_bytes
        .lines()
        .map(|line| parse_json(line).expect("#748 ordered transcript JSONL"))
        .collect();
    let transcript: Vec<Value> = raw.iter().map(val).collect();
    let mut observed = json!({"events":events,"transcript":transcript,"record":val(record.as_json()),"registryEvents":persisted.iter().map(val).collect::<Vec<_>>(),"restoredMessages":restored_messages.iter().map(val).collect::<Vec<_>>(),"active":active});
    if mutants::on("live-event-payload") {
        for event in observed["events"].as_array_mut().expect("#748 event list") {
            if event["type"] == "assistant_text_delta" {
                event["text"] = json!("corrupted host payload");
            }
        }
    }
    if mutants::on("live-registry") {
        observed["record"]["state"] = json!("running");
    }
    if mutants::on("live-transcript") {
        for entry in observed["transcript"]
            .as_array_mut()
            .expect("#748 transcript list")
        {
            if entry["type"] == "assistant_message" {
                entry["message"]["content"][0]["text"] = json!("corrupted durable output");
            }
        }
    }
    if mutants::on("live-time-shape") {
        observed["record"]["acceptedAt"] = Value::Null;
    }
    if mutants::on("live-owner-pid") {
        observed["record"]["ownerPid"] = Value::Null;
    }
    if mutants::on("live-fingerprint") {
        for entry in observed["transcript"]
            .as_array_mut()
            .expect("#748 fingerprint mutation rows")
        {
            if entry["type"] == "model_stream_event" && entry["event"]["type"] == "request_started"
            {
                entry["event"]["requestFingerprint"] = json!("0".repeat(64));
            }
        }
    }
    normalize_observation(&mut observed, &raw, &directory, port);
    fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(&observed).expect("#748 encode observations"),
    )
    .expect("#748 preserve observations");
    let expected = expectation(scenario);
    let summary = read(&directory.join("mitm-summary.json"));
    let injected = ["drop", "duplicate", "early-final"].contains(&scenario);
    negative_assert_eq!(
        summary["injections"]
            .as_array()
            .expect("#748 injection list")
            .len(),
        usize::from(injected),
        "#748 injection executed"
    );
    negative_assert!(
        summary["injections"]
            .as_array()
            .expect("#748 injection list")
            .iter()
            .all(|item| item["fired"] == true),
        "#748 injection executed"
    );
    let mut reaped = read(&directory.join("reaped.json"));
    if mutants::on("live-reap") {
        reaped["groupAlive"] = json!(true);
    }
    negative_assert_eq!(
        reaped,
        json!({"childReaped":true,"groupAlive":false}),
        "#748 child group reaped"
    );
    for pid in read(&directory.join("process.json"))
        .as_object()
        .expect("#748 process identity")
        .values()
    {
        let absent = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("#748 process liveness probe");
        negative_assert!(!absent.success(), "#748 child group reaped");
    }
    if scenario == "cancel-stream" {
        let rows: Vec<Value> = fs::read_to_string(directory.join("mock.jsonl"))
            .expect("#748 mock connections")
            .lines()
            .map(|line| serde_json::from_str(line).expect("#748 mock log JSON"))
            .filter(|row: &Value| row.get("connection").is_some())
            .collect();
        negative_assert_eq!(rows.len(), 1, "#748 provider interrupted");
        negative_assert_eq!(
            rows[0]["connection"],
            json!("aborted"),
            "#748 provider interrupted"
        );
    }
    if gate {
        negative_assert!(
            directory.join("tool-entered.json").exists(),
            "#748 read cancellation window"
        );
    }
    // Stop/reap the mock as well, before assertion failures can unwind this test.
    mock.0.kill().expect("#748 stop mock");
    mock.0.wait().expect("#748 reap mock");
    negative_assert_eq!(observed["active"], json!(false), "#748 admission released");
    negative_assert_eq!(
        serde_json::to_vec(&observed["events"]).expect("#748 event bytes"),
        serde_json::to_vec(&expected["events"]).expect("#748 golden event bytes"),
        "#748 GatewayEvent payload bytes"
    );
    negative_assert_eq!(
        observed["registryEvents"],
        observed["events"],
        "#748 registry event payloads"
    );
    negative_assert_eq!(
        observed["record"],
        expected["record"],
        "#748 registry terminal record"
    );
    negative_assert_eq!(
        durable_entries(&observed["transcript"], true),
        durable_entries(&expected["transcript"], false),
        "#748 durable transcript payloads"
    );
    negative_assert_eq!(
        observed["restoredMessages"],
        conversation(&expected["transcript"]),
        "#748 reopened conversation"
    );
}
macro_rules! cell {
    ($name:ident,$scenario:literal) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        #[ignore = "requires fixed PDRS_BASELINE and Node v22.22.0"]
        async fn $name() {
            run_case($scenario).await;
        }
    };
}
cell!(live_text, "text");
cell!(live_tool, "tool");
cell!(live_cancel_stream, "cancel-stream");
cell!(live_cancel_tool, "cancel-tool");
cell!(live_drop, "drop");
cell!(live_duplicate, "duplicate");
cell!(live_early_final, "early-final");

//! A5.3 native host probe. This is a Module entry, not the production WS/HTTP
//! gateway. The independent driver records provider HTTP and reports missing
//! production entry adapters as NOT_RUN.
use pilotdeck_gateway::{
    host::{
        Cancellation, ExecuteRequest, ExecuteResult, HostClient, HostError, HostErrorKind,
        HostFuture, HostHandler, Limits,
        modules::{ModuleRouter, capability, manifest, model::TurnContext, model_port},
    },
    runtime::model::{
        http::HyperModelTransport,
        provider_registry::NativeModelInvocationProvider,
        router::{RouterConfig, RouterRuntime},
    },
};
use pilotdeck_protocol::{
    JsObject, JsValue, admission::ExecutionIdentity, parse_json, stringify_json,
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::process::Command;

fn record(kind: &str, value: &JsValue) {
    // NDJSON is only the native host view. The parent records HTTP separately.
    let mut row = JsObject::new();
    row.insert("kind".into(), kind.into());
    row.insert("value".into(), value.clone());
    println!(
        "{}",
        stringify_json(&JsValue::Object(row)).unwrap().unwrap()
    );
}

struct ObservedHost(ModuleRouter);
impl HostHandler for ObservedHost {
    fn module_call(&self, call: JsValue) -> HostFuture<JsObject> {
        record("module_call", &call);
        self.0.module_call(call)
    }
    fn event(&self, event: JsValue) -> HostFuture<()> {
        record("event", &event);
        Box::pin(async { Ok(()) })
    }
    fn dispose(&self) -> HostFuture<()> {
        self.0.dispose()
    }
}

// This fixture has an empty tool catalog. It never simulates a successful
// capability: any unexpected tool/context/observer call fails the probe.
struct NoTools;
fn unexpected<T: Send + 'static>() -> HostFuture<T> {
    Box::pin(async {
        Err(HostError::new(
            HostErrorKind::Callback,
            "unexpected tool call in model-only probe",
        ))
    })
}
impl capability::ToolCatalog for NoTools {
    fn list(&self) -> HostFuture<Vec<Arc<dyn capability::ToolDefinition>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}
impl capability::ToolExecution for NoTools {
    fn execute_all(
        &self,
        _: Vec<capability::ToolCall>,
        _: capability::ToolInvocationContext,
    ) -> HostFuture<Vec<JsObject>> {
        unexpected()
    }
}
impl capability::ToolContext for NoTools {
    fn prepare(&self, _: JsValue) -> HostFuture<capability::ToolInvocationContext> {
        unexpected()
    }
}
impl capability::CapabilityResultObserver for NoTools {
    fn on_capability_results(&self, _: Vec<JsObject>) -> HostFuture<()> {
        unexpected()
    }
}

fn field<'a>(value: &'a JsValue, key: &str) -> Result<&'a JsValue, Box<dyn std::error::Error>> {
    value
        .get(key)
        .ok_or_else(|| format!("missing {key}").into())
}
fn text(value: &JsValue, key: &str) -> Result<String, Box<dyn std::error::Error>> {
    Ok(field(value, key)?
        .as_string()
        .ok_or("expected string")?
        .to_utf8()?)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: a53_native_host CONFIG.json")?;
    let config = parse_json(&std::fs::read_to_string(path)?)?;
    let isolation = PathBuf::from(text(&config, "isolation")?).canonicalize()?;
    let mut command = match text(&config, "topology")?.as_str() {
        "Rust-sidecar-ts" => {
            let baseline = PathBuf::from(text(&config, "baseline")?).canonicalize()?;
            let mut command = Command::new(text(&config, "node")?);
            command
                .arg("--import")
                .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
                .arg(baseline.join("src/cli/pilotdeck-agent-loop-sidecar.ts"));
            command
        }
        "Rust-sidecar-native" => {
            let mut command = Command::new(text(&config, "sidecar")?);
            command.arg("--stdio");
            command
        }
        _ => return Err("unknown native topology (no proxy fallback)".into()),
    };
    command
        .current_dir(&isolation)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &isolation)
        .env("PILOT_HOME", &isolation)
        .env("TMPDIR", &isolation);
    let provider = field(&config, "provider")?.clone();
    let invocation = Arc::new(NativeModelInvocationProvider::new(
        provider.clone(),
        Arc::new(HyperModelTransport::new()),
    ));
    let router = Arc::new(RouterRuntime::new(
        RouterConfig::from_snapshot(field(&config, "snapshot")?),
        invocation,
    )?);
    let mut providers = JsObject::new();
    providers.insert("fixture".into(), provider);
    let mut model_config = JsObject::new();
    model_config.insert("providers".into(), JsValue::Object(providers));
    let tools = Arc::new(NoTools);
    let modules = ModuleRouter {
        model: model_port::compose(
            router.clone(),
            JsValue::Object(model_config),
            None,
            Default::default(),
            TurnContext {
                session_id: "a53-session".into(),
                turn_id: "a53-turn".into(),
                ..Default::default()
            },
        ),
        capability: capability::Handler::new(tools.clone(), tools.clone(), tools),
        ..Default::default()
    };
    let manifest = modules
        .turn_manifest(
            Vec::new(),
            manifest::PermissionSnapshot {
                cwd: isolation
                    .to_str()
                    .ok_or("isolation path must be UTF-8")?
                    .into(),
                permission_mode: "default".into(),
                permission_context: JsObject::new(),
                turn_can_prompt: Some(JsValue::Bool(false)),
            },
            &Default::default(),
        )
        .map_err(|missing| format!("missing handlers: {missing:?}"))?;
    let mut payload = field(&config, "input")?
        .as_object()
        .ok_or("input must be an object")?
        .clone();
    for (key, value) in manifest.entries() {
        payload.insert(key.clone(), value.clone());
    }
    let host = Arc::new(ObservedHost(modules));
    // Install before spawning, including the handshake cancellation window.
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let expected_module = "pilotdeck-agent-loop".into();
    let startup = HostClient::spawn(
        &mut command,
        Some(&expected_module),
        Limits {
            execution_timeout: Some(Duration::from_secs(30)),
            ..Limits::default()
        },
    );
    let client = tokio::select! {
        result = startup => result?,
        _ = interrupt.recv() => return Err("probe interrupted during handshake".into()),
    };
    println!(
        "{}",
        serde_json::json!({"kind":"launch","pid":client.child_id(),
            "binding":{"moduleId":client.binding().module_id.to_utf8()?,
                       "moduleInstanceId":client.binding().module_instance_id.to_utf8()?,
                       "connectionGeneration":client.binding().connection_generation.to_utf8()?},
            "program":command.as_std().get_program().to_str().ok_or("program path must be UTF-8")?,
            "argv":command.as_std().get_args().map(|arg| arg.to_str().ok_or("argument must be UTF-8"))
                .collect::<Result<Vec<_>, _>>()?})
    );
    let cancellation = Cancellation::default();
    let signal_cancellation = cancellation.clone();
    let signal = tokio::spawn(async move {
        if interrupt.recv().await.is_some() {
            signal_cancellation.cancel(Some("probe_interrupted".into()));
        }
    });
    let result = client
        .execute_cancellable(
            ExecuteRequest {
                identity: ExecutionIdentity {
                    run_id: "a53-run".into(),
                    operation_id: "a53-operation".into(),
                    request_id: "a53-request".into(),
                    session_id: "a53-session".into(),
                    turn_id: "a53-turn".into(),
                },
                payload,
                idempotency_key: None,
                operation_deadline: None,
            },
            host,
            cancellation,
        )
        .await;
    signal.abort();
    router.shutdown();
    match result? {
        ExecuteResult::Terminal(terminal) => record("terminal", &terminal),
        other => return Err(format!("unexpected execution result: {other:?}").into()),
    }
    Ok(())
}

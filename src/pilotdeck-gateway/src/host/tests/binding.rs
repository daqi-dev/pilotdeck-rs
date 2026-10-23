//! Per-turn handler binding replay (A6.9 pr2b).
//!
//! The committed oracle (tests/fixtures/host-module-binding) was produced by
//! the real TS dispatcher and turn composition: for every configuration
//! combination it records (a) the manifest advertisement
//! (`dispatcher.manifest.hostModules`, sidecarDefaultModuleDispatcher.ts:394-437),
//! (b) the actually callable module handlers (Object.keys of the registry,
//! dispatcher:66-202), and (c) probe module_call responses that distinguish a
//! registered module from an unregistered one. The Rust `ModuleRouter` derives
//! its advertisement from the very handlers it dispatches to, so this replay is
//! the item-by-item check that every advertised capability has a real handler.
use super::*;
use crate::host::{
    execution::module_response,
    modules::{
        CompositionSwitches, ModuleRouter, budget, capability, context, event, lifecycle, manifest,
        model, permission, tool_context, turn,
    },
};
use pilotdeck_protocol::{parse_json, stringify_json};

const ORACLE: &str = include_str!("../../../tests/fixtures/host-module-binding/oracle.json");
static UNDEFINED: JsValue = JsValue::Undefined;

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn text(value: &JsValue) -> String {
    value.as_string().unwrap().to_utf8().unwrap()
}
fn array(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(items) = value {
        items
    } else {
        panic!("not array: {value:?}")
    }
}
fn yes(value: &JsValue, key: &str) -> bool {
    value.get(key).and_then(JsValue::as_bool) == Some(true)
}
fn object_yes(object: &JsObject, key: &str) -> bool {
    object.get(key).and_then(JsValue::as_bool) == Some(true)
}
fn encoded(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap()
}

// ---- stand-ins for the ports composition would bind; probes never reach them ----

struct PanicModel;
impl model::ModelExecution for PanicModel {
    fn prepare(
        &self,
        _input: model::PrepareInput,
    ) -> model::ModelFuture<model::PreparedInvocation> {
        Box::pin(async { panic!("binding probes must not reach model execution") })
    }
    fn stream(
        &self,
        _input: model::StreamInput,
    ) -> model::ModelFuture<Box<dyn model::ModelStream>> {
        Box::pin(async { panic!("binding probes must not reach model execution") })
    }
}
impl model::RequestMaterializer for PanicModel {
    fn materialize(
        &self,
        _prepared: model::PreparedInvocation,
        _candidate: JsObject,
    ) -> model::ModelFuture<JsValue> {
        Box::pin(async { panic!("binding probes must not reach the materializer") })
    }
}
/// The fixed TS dispatcher accepts absent lookups; the trait defaults model
/// every lookup as absent, which is exactly the fixture's metadata port.
struct NoMetadata;
impl model::ModelMetadata for NoMetadata {}

struct PanicBudget;
impl budget::RequestInputEstimator for PanicBudget {
    fn estimate_request_input(&self, _request: JsObject) -> HostFuture<f64> {
        Box::pin(async { panic!("binding probes must not reach budget ports") })
    }
}
impl budget::RequestBudgetEvaluator for PanicBudget {
    fn evaluate_request_budget(
        &self,
        _request: JsObject,
        _options: budget::EvaluationOptions,
    ) -> HostFuture<JsValue> {
        Box::pin(async { panic!("binding probes must not reach budget ports") })
    }
}
impl budget::UsageCostEstimator for PanicBudget {
    fn estimate_usage_cost(
        &self,
        _usage: Option<JsObject>,
        _provider: JsString,
        _model: JsString,
    ) -> HostFuture<Option<f64>> {
        Box::pin(async { panic!("binding probes must not reach budget ports") })
    }
}

/// The fixture's capability module: an execution port whose catalog is empty.
/// The unknown-operation probe builds context before failing, so `prepare`
/// must succeed; execution and observation must never be reached.
struct ToolPort;
impl capability::ToolCatalog for ToolPort {
    fn list(&self) -> HostFuture<Vec<Arc<dyn capability::ToolDefinition>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}
impl capability::ToolContext for ToolPort {
    fn prepare(&self, _call: JsValue) -> HostFuture<capability::ToolInvocationContext> {
        Box::pin(async {
            Ok(capability::ToolInvocationContext {
                runtime: Arc::new(()),
                execution: Arc::new(()),
            })
        })
    }
}
impl capability::ToolExecution for ToolPort {
    fn execute_all(
        &self,
        _calls: Vec<capability::ToolCall>,
        _context: capability::ToolInvocationContext,
    ) -> HostFuture<Vec<JsObject>> {
        Box::pin(async { panic!("binding probes must fail validation before execution") })
    }
}
impl capability::CapabilityResultObserver for ToolPort {
    fn on_capability_results(&self, _results: Vec<JsObject>) -> HostFuture<()> {
        Box::pin(async { panic!("binding probes must not reach the observer") })
    }
}

struct PanicPermission;
impl capability::ToolCatalog for PanicPermission {
    fn list(&self) -> HostFuture<Vec<Arc<dyn capability::ToolDefinition>>> {
        Box::pin(async { panic!("binding probes must fail before the permission catalog") })
    }
}
impl permission::PermissionContext for PanicPermission {
    fn prepare(&self, _value: JsValue) -> HostFuture<capability::RuntimeContext> {
        Box::pin(async { panic!("binding probes must fail before the permission context") })
    }
}
impl permission::PermissionDecision for PanicPermission {
    fn decide(
        &self,
        _tool: Arc<dyn capability::ToolDefinition>,
        _input: JsValue,
        _context: capability::RuntimeContext,
        _tool_call_id: JsString,
    ) -> HostFuture<JsValue> {
        Box::pin(async { panic!("binding probes must fail before the permission decision") })
    }
}

struct PanicEnvironment;
impl tool_context::TurnEnvironment for PanicEnvironment {
    fn build(
        &self,
        _base_env: Option<&JsValue>,
        _cwd: &JsString,
        _session_id: &JsString,
        _turn_id: &JsString,
    ) -> Result<JsObject, HostError> {
        panic!("binding probes must fail before environment construction")
    }
}

/// The fixture's context runtime: prepare is mandatory, the optional operations
/// follow the recorded spec, and no probe may reach an actual invocation.
struct SpecRuntime {
    apply_tool_results: bool,
    recover_from_model_error: bool,
    capture_turn: bool,
    try_auto_compact: bool,
}
impl context::ContextRuntime for SpecRuntime {
    fn supports(&self, operation: context::Operation) -> bool {
        match operation {
            context::Operation::PrepareForModel => true,
            context::Operation::ApplyToolResults => self.apply_tool_results,
            context::Operation::RecoverFromModelError => self.recover_from_model_error,
            context::Operation::CaptureTurn => self.capture_turn,
            context::Operation::TryAutoCompact => self.try_auto_compact,
        }
    }
    fn prepare_for_model(&self, _input: context::ContextInput) -> HostFuture<JsValue> {
        Box::pin(async { panic!("binding probes must not reach the context runtime") })
    }
}

struct PanicSink;
impl event::EventSink for PanicSink {
    fn emit(&self, _event: JsObject) -> Result<(), HostError> {
        panic!("binding probes must fail validation before the event sink")
    }
}

/// Mirrors the TS `createHostPlanTodoModuleHandler`
/// (hostPlanTodoModuleHandler.ts:14-45): the state handle is resolved before
/// any payload validation, so an unresolved composition answers every call
/// with the missing-capability error even for malformed payloads.
struct FakePlanTodo {
    resolving: bool,
}
impl capability::PlanTodoHandler for FakePlanTodo {
    fn call(&self, call: JsValue) -> HostFuture<JsObject> {
        let resolving = self.resolving;
        Box::pin(async move {
            if !resolving {
                return Err(
                    ModuleError::new("Host did not provide a plan/todo capability.").into(),
                );
            }
            let payload = call.get("payload").and_then(JsValue::as_object);
            let required = |name: &str| -> Result<JsString, HostError> {
                payload
                    .and_then(|payload| payload.get(name))
                    .and_then(JsValue::as_string)
                    .filter(|value| !value.is_empty())
                    .cloned()
                    .ok_or_else(|| ModuleError::new(format!("Missing {name}.")).into())
            };
            let session_id = required("sessionId")?;
            let turn_id = required("turnId")?;
            if !session_id.equals_str("session") || !turn_id.equals_str("turn") {
                return Err(ModuleError::new(
                    "Plan/todo capability identity does not match the active host turn.",
                )
                .into());
            }
            let method = required("method")?;
            if method.equals_str("read") {
                let mut snapshot = JsObject::new();
                snapshot.insert("items".into(), JsValue::Array(Vec::new()));
                let mut response = JsObject::new();
                response.insert("snapshot".into(), JsValue::Object(snapshot));
                return Ok(response);
            }
            // wire::named_error is module-private; build the same UTF-16
            // message inline so lone surrogates survive like the wire errors.
            let units = "Unsupported sidecar plan/todo method: "
                .encode_utf16()
                .chain(method.units().iter().copied())
                .collect();
            Err(ModuleError::new(JsString::from_utf16(units)).into())
        })
    }
}

fn identity() -> tool_context::ContextRequestIdentity {
    let mut input = JsObject::new();
    input.insert("sessionId".into(), "session".into());
    input.insert("turnId".into(), "turn".into());
    tool_context::ContextRequestIdentity::new(
        tool_context::TurnBinding {
            config: JsObject::new(),
            input,
            checkpoint: tool_context::HostToolCheckpoint::default(),
            abort_signal: None,
            environment: Arc::new(PanicEnvironment),
        },
        tool_context::ToolContextPorts::default(),
    )
}

/// Build the router the recorded composition would have produced: one handler
/// per installed module, bound exactly as the TS turn composition binds them.
fn router_for(case: &JsValue) -> ModuleRouter {
    let modules = field(case, "modules");
    let turn_spec = field(case, "turn");

    let model_spec = field(modules, "model");
    let mut model = model::Handler::with_ports(
        Arc::new(PanicModel),
        if yes(model_spec, "metadata") {
            Some(Arc::new(NoMetadata) as Arc<dyn model::ModelMetadata>)
        } else {
            None
        },
        model::TurnContext::default(),
    );
    if yes(model_spec, "materializeRequest") {
        model.materializer = Some(Arc::new(PanicModel));
    }

    let budget_ports = field(modules, "budget")
        .as_object()
        .map(|spec| budget::Ports {
            estimate_request_input: object_yes(spec, "estimateRequestInput")
                .then(|| Arc::new(PanicBudget) as Arc<dyn budget::RequestInputEstimator>),
            evaluate_request_budget: object_yes(spec, "evaluateRequestBudget")
                .then(|| Arc::new(PanicBudget) as Arc<dyn budget::RequestBudgetEvaluator>),
            estimate_usage_cost: object_yes(spec, "estimateUsageCost")
                .then(|| Arc::new(PanicBudget) as Arc<dyn budget::UsageCostEstimator>),
        });

    let tool = Arc::new(ToolPort);
    // The turn composition binds the plan/todo handler for every turn
    // (sidecarTurnComposition.ts:83-87); only the advertisement follows
    // modules.planTodo, and the fixture mirrors the resolved handle's shape.
    let resolving = field(modules, "planTodo").as_string().is_some_and(|plan| {
        assert!(
            plan.equals_str("unresolved") || plan.equals_str("resolving"),
            "unknown planTodo spec"
        );
        plan.equals_str("resolving")
    });
    let capability = capability::Handler::new(tool.clone(), tool.clone(), tool.clone())
        .with_plan_todo(Arc::new(FakePlanTodo { resolving }));

    let permission_port = Arc::new(PanicPermission);
    let permission = if yes(modules, "permission") {
        permission::Handler::new(
            permission_port.clone(),
            permission_port.clone(),
            permission_port,
        )
    } else {
        permission::Handler::default()
    };

    let context = match field(modules, "context") {
        JsValue::Null | JsValue::Undefined => context::Handler::default(),
        spec @ JsValue::Object(_) => {
            let spec = spec.as_object().unwrap();
            let runtime = Arc::new(SpecRuntime {
                apply_tool_results: object_yes(spec, "applyToolResults"),
                recover_from_model_error: object_yes(spec, "recoverFromModelError"),
                capture_turn: object_yes(spec, "captureTurn"),
                try_auto_compact: object_yes(spec, "tryAutoCompact"),
            });
            context::Handler::new(runtime, identity(), JsObject::new(), None)
        }
        spec => {
            assert!(
                spec.as_string().is_some_and(|s| s.equals_str("no-runtime")),
                "unknown context spec"
            );
            context::Handler::without_runtime(identity(), JsObject::new(), None)
        }
    };

    let turn = turn::Handler {
        ports: turn::Ports {
            drain_steer: yes(turn_spec, "drainSteer").then(|| {
                Arc::new(|| Box::pin(async { Ok(Vec::new()) }) as HostFuture<Vec<JsObject>>)
                    as turn::DrainSteer
            }),
            drain_or_close_steer: yes(turn_spec, "drainOrClose").then(|| {
                Arc::new(|| Box::pin(async { Ok(JsObject::new()) }) as HostFuture<JsObject>)
                    as turn::DrainOrCloseSteer
            }),
            persist_compaction: yes(turn_spec, "persistCompaction").then(|| {
                Arc::new(|_boundary: JsObject, _messages: Vec<JsValue>| {
                    Box::pin(async { Ok(()) }) as HostFuture<()>
                }) as turn::PersistCompaction
            }),
        },
        authorizations: None,
    };

    let lifecycle = if yes(modules, "lifecycle") {
        lifecycle::Handler::null(lifecycle::Binding {
            session_id: "session".into(),
            turn_id: "turn".into(),
            cwd: "/fixture/project".into(),
            permission_mode: None,
            base_env: None,
            signal: None,
        })
    } else {
        lifecycle::Handler::default()
    };

    let event = if yes(modules, "event") {
        event::Handler::new(
            event::TurnIdentity::new("session", "turn"),
            Arc::new(PanicSink),
        )
    } else {
        event::Handler::default()
    };

    // A handler assembled before budget wiring is an installed, empty budget
    // module (X0); an absent budget module answers as unregistered instead.
    ModuleRouter {
        model,
        budget: budget::Handler {
            ports: budget_ports,
            signal: None,
        },
        capability,
        permission,
        context,
        turn,
        lifecycle,
        event,
    }
}

fn switches_for(case: &JsValue) -> CompositionSwitches {
    CompositionSwitches {
        plan_todo: !matches!(
            field(field(case, "modules"), "planTodo"),
            JsValue::Null | JsValue::Undefined
        ),
        // The fixture composes no interaction module; elicitation is not a
        // host module and never a handler-derived switch.
        elicitation_available: None,
    }
}

/// The manifest needs a permission projection even though only hostModules is
/// compared; the fixture's config is the generator's fixed config.
fn permission_snapshot() -> manifest::PermissionSnapshot {
    manifest::PermissionSnapshot {
        cwd: "/fixture/project".into(),
        permission_mode: "default".into(),
        permission_context: JsObject::new(),
        turn_can_prompt: None,
    }
}

/// Byte-for-byte replay against the dispatcher registry and manifest the real
/// TS composition produced (fixtures/host-module-binding/oracle.json,
/// generated twice and compared).
#[tokio::test]
async fn real_ts_handler_binding_replay() {
    let oracle = crate::test_baseline::oracle(ORACLE);
    assert_eq!(array(field(&oracle, "cases")).len(), 27);
    let mut probes = 0;
    for case in array(field(&oracle, "cases")) {
        let name = text(field(case, "name"));
        let router = router_for(case);
        let switches = switches_for(case);

        // Item-by-item: nothing is advertised without the handler that takes
        // its calls, and the actually callable set matches the TS registry
        // (dispatcher:66-202 order) exactly.
        let inventory = router.module_inventory(&switches);
        let missing = router.missing_handlers(&inventory);
        assert!(
            missing.is_empty(),
            "{name}: advertised without a bound handler: {missing:?}"
        );
        let expected_handlers = array(field(case, "handlers"));
        let callable: Vec<JsString> = router
            .callable_modules()
            .into_iter()
            .map(JsString::from)
            .collect();
        let expected: Vec<JsString> = expected_handlers
            .iter()
            .map(|value| value.as_string().unwrap().clone())
            .collect();
        assert_eq!(callable, expected, "{name}: callable modules");

        // The advertised method surface matches dispatcher.manifest.hostModules.
        let manifest = router
            .turn_manifest(Vec::new(), permission_snapshot(), &switches)
            .unwrap_or_else(|missing| panic!("{name}: manifest refused: {missing:?}"));
        assert_eq!(
            encoded(manifest.get("hostModules").unwrap()),
            encoded(field(case, "hostModules")),
            "{name}: hostModules"
        );

        // Probe responses distinguish registered modules from unregistered ones
        // and pin the plan/todo switches' independence.
        for probe in array(field(case, "probes")) {
            probes += 1;
            let call = field(probe, "call");
            let response = module_response(call, router.module_call(call.clone()).await);
            assert_eq!(
                encoded(&response),
                encoded(field(probe, "response")),
                "{name}/{}",
                text(field(probe, "probe"))
            );
        }
        router.dispose().await.unwrap();
    }
    assert_eq!(probes, 324, "27 cases x 12 probes");
}

/// The TS-side invariant the generator already asserted, replayed here: every
/// advertised module has a registered handler in the golden itself.
#[test]
fn golden_advertises_only_registered_modules() {
    let oracle = crate::test_baseline::oracle(ORACLE);
    for case in array(field(&oracle, "cases")) {
        let handlers = array(field(case, "handlers"));
        let advertised = field(case, "hostModules").as_object().unwrap();
        for (module, _) in advertised.entries() {
            assert!(
                handlers
                    .iter()
                    .any(|handler| handler.as_string() == Some(module)),
                "{}: golden advertises {} without a handler",
                text(field(case, "name")),
                module.to_utf8().unwrap()
            );
        }
    }
}

/// A router assembled without its TS-mandatory ports must refuse the manifest
/// instead of advertising methods no handler would take.
#[test]
fn advertised_without_bound_handler_fails_closed() {
    let router = ModuleRouter::default();
    let switches = CompositionSwitches::default();
    let inventory = router.module_inventory(&switches);
    assert_eq!(
        router.missing_handlers(&inventory),
        vec!["model", "capability"],
        "TS-mandatory modules without ports fail closed"
    );
    let missing = router
        .turn_manifest(Vec::new(), permission_snapshot(), &switches)
        .unwrap_err();
    assert_eq!(missing, vec!["model", "capability"]);
}

/// The two plan/todo switches are independent (#570 scope note): with the
/// advertisement off, the per-turn handler is still bound and callable.
#[tokio::test]
async fn plan_todo_handler_is_callable_without_advertisement() {
    let tool = Arc::new(ToolPort);
    let router = ModuleRouter {
        model: model::Handler::with_ports(
            Arc::new(PanicModel),
            None,
            model::TurnContext::default(),
        ),
        capability: capability::Handler::new(tool.clone(), tool.clone(), tool.clone())
            .with_plan_todo(Arc::new(FakePlanTodo { resolving: true })),
        ..ModuleRouter::default()
    };
    let switches = CompositionSwitches {
        plan_todo: false,
        elicitation_available: None,
    };
    let manifest = router
        .turn_manifest(Vec::new(), permission_snapshot(), &switches)
        .unwrap();
    let advertised = encoded(manifest.get("hostModules").unwrap());
    assert!(!advertised.contains("plan_todo"));
    let call = parse_json(
        r#"{"kind":"request","method":"module_call","messageId":"call-1","requestId":"callback-request","runId":"run","operationId":"operation","module":"capability","payload":{"operation":"plan_todo","sessionId":"session","turnId":"turn","method":"read"}}"#,
    )
    .unwrap();
    let response = module_response(&call, router.module_call(call.clone()).await);
    assert_eq!(
        encoded(&response),
        encoded(&parse_json(r#"{"kind":"response","messageId":"host-module-call-1","inReplyTo":"call-1","requestId":"callback-request","ok":true,"payload":{"snapshot":{"items":[]}}}"#).unwrap()),
        "the bound handler answers even while unadvertised"
    );
}

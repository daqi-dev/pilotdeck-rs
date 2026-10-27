//! Pure host-module manifest synthesis (TS sidecarDefaultModuleDispatcher:216-224,
//! 394-437, 440 and sidecarToolContext.ts:187-195). The manifest advertises the
//! host's registered capabilities to the sidecar before the first module call;
//! it is computed once per turn from a config snapshot and the module
//! inventory, never from sidecar authority. Per-turn handler binding and
//! registration stay with the ModuleRouter (A6.9 pr2b).
use super::capability::{ToolDefinition, serialize_tool_descriptor};
use crate::host::HostError;
use pilotdeck_protocol::{JsObject, JsString, JsValue};

/// One registered tool at composition time: the manifest view of
/// `capability.execution.list()` (TS serializeToolDescriptor:440).
pub struct ToolSnapshot {
    /// name, description, kind and inputSchema exactly as the tool exposes them.
    pub fields: JsObject,
    /// Pre-evaluated `isReadOnly({})` with TS safely() applied: a predicate
    /// that threw (or is absent, like a missing method) serializes as false,
    /// while an undefined or null return value passes through verbatim.
    pub read_only: JsValue,
    /// Pre-evaluated `isConcurrencySafe({})` with TS safely() applied.
    pub concurrency_safe: JsValue,
    /// Pre-evaluated `requiresUserInteraction?.({}) ?? false` with TS safely()
    /// applied: only non-nullish results survive, everything else is false.
    pub requires_user_interaction: JsValue,
    /// `requiredRuntimeCapabilities` verbatim; None omits the field entirely.
    pub required_runtime_capabilities: Option<Vec<JsString>>,
}
impl ToolSnapshot {
    /// Capture live predicates once with the descriptor's TS safely() rules.
    /// The native definition remains in the catalog for permission/execution.
    pub fn from_definition(tool: &dyn ToolDefinition) -> Self {
        let fields = tool.fields().clone();
        let empty = JsValue::Object(JsObject::new());
        let read_only = tool.is_read_only(&empty).unwrap_or(JsValue::Bool(false));
        let concurrency_safe = tool
            .is_concurrency_safe(&empty)
            .unwrap_or(JsValue::Bool(false));
        let interaction = tool
            .requires_user_interaction(&empty)
            .unwrap_or(JsValue::Bool(false));
        Self {
            fields,
            read_only,
            concurrency_safe,
            requires_user_interaction: match interaction {
                JsValue::Undefined | JsValue::Null => JsValue::Bool(false),
                value => value,
            },
            required_runtime_capabilities: tool.required_runtime_capabilities(),
        }
    }
}
impl ToolDefinition for ToolSnapshot {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(self.read_only.clone())
    }
    fn is_concurrency_safe(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(self.concurrency_safe.clone())
    }
    fn requires_user_interaction(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(self.requires_user_interaction.clone())
    }
    fn required_runtime_capabilities(&self) -> Option<Vec<JsString>> {
        self.required_runtime_capabilities.clone()
    }
}

/// Permission context inputs (AgentRuntimeConfig projection plus the turn's
/// canPrompt): TS effectivePermissionContext (sidecarToolContext.ts:187-195).
pub struct PermissionSnapshot {
    /// config.cwd, overriding any cwd recorded in the context.
    pub cwd: JsValue,
    /// config.permissionMode, overriding any mode recorded in the context.
    pub permission_mode: JsValue,
    /// config.permissionContext, spread first so its own key order and any
    /// extra fields survive verbatim.
    pub permission_context: JsObject,
    /// input.canPrompt, overriding context.canPrompt unless null or undefined.
    pub turn_can_prompt: Option<JsValue>,
}

/// Turn-scoped callback availability (AgentLoopInput). The turn module is only
/// advertised while at least one callback exists.
pub struct TurnCallbacks {
    pub drain_steer_messages: bool,
    pub drain_or_close_steer_mailbox: bool,
    pub on_compact_persisted: bool,
}

/// Which optional methods the composed model and budget modules expose.
pub struct ModelInventory {
    /// modules.model.materializeRequest -> materialize_prepared_request.
    pub materialize_request: bool,
    /// modules.model.metadata -> get_metadata.
    pub metadata: bool,
}
pub struct BudgetInventory {
    pub estimate_request_input: bool,
    pub evaluate_request_budget: bool,
    pub estimate_usage_cost: bool,
}
/// The optional methods of an otherwise mandatory AgentTurnContextPort.
pub struct ContextInventory {
    pub apply_tool_results: bool,
    pub recover_from_model_error: bool,
    pub capture_turn: bool,
    pub try_auto_compact: bool,
}

/// The registered module inventory (SidecarModuleComposition projection).
/// model and capability are mandatory in every composition, so they carry no
/// Option; every other module is advertised only when present.
pub struct ModuleInventory {
    pub model: ModelInventory,
    /// None = no budget module, not even an empty method list.
    pub budget: Option<BudgetInventory>,
    /// modules.planTodo, advertised as plan_todo on the capability module.
    pub plan_todo: bool,
    /// None = no context module.
    pub context: Option<ContextInventory>,
    pub permission: bool,
    pub lifecycle: bool,
    pub event: bool,
    /// modules.interaction?.elicitationAvailable; None = no interaction module.
    pub elicitation_available: Option<bool>,
}

/// Pure manifest input: a config snapshot plus the registered module
/// inventory. It deliberately excludes live ports, turns and futures.
pub struct ManifestInput {
    /// capability.execution.list() at composition time.
    pub tools: Vec<ToolSnapshot>,
    pub permission: PermissionSnapshot,
    pub modules: ModuleInventory,
    pub turn: TurnCallbacks,
}

/// Synthesize the sidecar host manifest: tools, permissionContext,
/// hostModules and interactionCapabilities, in TS property order
/// (sidecarDefaultModuleDispatcher.ts:216-224).
pub fn compose_manifest(input: &ManifestInput) -> JsObject {
    let mut manifest = JsObject::new();
    let tools = input
        .tools
        .iter()
        .map(|tool| JsValue::Object(serialize_tool_descriptor(tool)))
        .collect();
    manifest.insert("tools".into(), JsValue::Array(tools));
    manifest.insert(
        "permissionContext".into(),
        JsValue::Object(effective_permission_context(&input.permission)),
    );
    manifest.insert(
        "hostModules".into(),
        JsValue::Object(host_module_capabilities(&input.modules, &input.turn)),
    );
    let mut interaction = JsObject::new();
    interaction.insert(
        "elicitationAvailable".into(),
        JsValue::Bool(input.modules.elicitation_available == Some(true)),
    );
    manifest.insert(
        "interactionCapabilities".into(),
        JsValue::Object(interaction),
    );
    manifest
}

/// Host permission projection: spread the config context, then overwrite cwd,
/// mode, canPrompt and rules in place (sidecarToolContext.ts:187-195). Keys
/// the context never had are appended in that order, like the TS object
/// literal; overwritten keys keep their original position.
fn effective_permission_context(permission: &PermissionSnapshot) -> JsObject {
    let context = &permission.permission_context;
    let mut result = context.clone();
    result.insert("cwd".into(), permission.cwd.clone());
    result.insert("mode".into(), permission.permission_mode.clone());
    result.insert(
        "canPrompt".into(),
        permission
            .turn_can_prompt
            .clone()
            .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
            .unwrap_or_else(|| {
                context
                    .get("canPrompt")
                    .cloned()
                    .unwrap_or(JsValue::Undefined)
            }),
    );
    result.insert(
        "rules".into(),
        context.get("rules").cloned().unwrap_or(JsValue::Undefined),
    );
    result
}

/// Advertise the host module method surface (TS hostModuleCapabilities,
/// sidecarDefaultModuleDispatcher.ts:394-437). Entry order is fixed:
/// model, budget, turn, capability, context, permission, lifecycle, event.
fn host_module_capabilities(modules: &ModuleInventory, turn: &TurnCallbacks) -> JsObject {
    let mut capabilities = JsObject::new();
    let mut model = vec![JsValue::from("prepare")];
    if modules.model.materialize_request {
        model.push(JsValue::from("materialize_prepared_request"));
    }
    model.push(JsValue::from("stream"));
    model.push(JsValue::from("stream_next"));
    model.push(JsValue::from("close_stream"));
    if modules.model.metadata {
        model.push(JsValue::from("get_metadata"));
    }
    capabilities.insert("model".into(), module_methods(model));
    if let Some(budget) = &modules.budget {
        let mut methods = Vec::new();
        if budget.estimate_request_input {
            methods.push(JsValue::from("estimate_request_input"));
        }
        if budget.evaluate_request_budget {
            methods.push(JsValue::from("evaluate_request_budget"));
        }
        if budget.estimate_usage_cost {
            methods.push(JsValue::from("estimate_usage_cost"));
        }
        capabilities.insert("budget".into(), module_methods(methods));
    }
    if turn.drain_steer_messages || turn.drain_or_close_steer_mailbox || turn.on_compact_persisted {
        let mut methods = Vec::new();
        if turn.drain_steer_messages {
            methods.push(JsValue::from("drain_steer"));
        }
        if turn.drain_or_close_steer_mailbox {
            methods.push(JsValue::from("drain_or_close_steer"));
        }
        if turn.on_compact_persisted {
            methods.push(JsValue::from("persist_compaction"));
        }
        capabilities.insert("turn".into(), module_methods(methods));
    }
    let mut capability = vec![
        JsValue::from("execute"),
        JsValue::from("execute_batch"),
        JsValue::from("list_tools"),
    ];
    if modules.plan_todo {
        capability.push(JsValue::from("plan_todo"));
    }
    capabilities.insert("capability".into(), module_methods(capability));
    if let Some(context) = &modules.context {
        let mut methods = vec![JsValue::from("prepare_for_model")];
        if context.apply_tool_results {
            methods.push(JsValue::from("apply_tool_results"));
        }
        if context.recover_from_model_error {
            methods.push(JsValue::from("recover_from_model_error"));
        }
        if context.capture_turn {
            methods.push(JsValue::from("capture_turn"));
        }
        if context.try_auto_compact {
            methods.push(JsValue::from("try_auto_compact"));
        }
        capabilities.insert("context".into(), module_methods(methods));
    }
    if modules.permission {
        capabilities.insert(
            "permission".into(),
            module_methods(vec![JsValue::from("decide")]),
        );
    }
    if modules.lifecycle {
        capabilities.insert(
            "lifecycle".into(),
            module_methods(vec![JsValue::from("dispatch")]),
        );
    }
    if modules.event {
        capabilities.insert("event".into(), module_methods(vec![JsValue::from("emit")]));
    }
    capabilities
}

fn module_methods(methods: Vec<JsValue>) -> JsValue {
    let mut module = JsObject::new();
    module.insert("methods".into(), JsValue::Array(methods));
    JsValue::Object(module)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::{parse_json, stringify_json};

    const BASELINE_COMMIT: &str = "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85";

    fn field(value: &JsValue, key: &str) -> JsValue {
        value.get(key).cloned().unwrap_or(JsValue::Undefined)
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
    fn decode(value: &JsValue) -> JsValue {
        match value {
            JsValue::Array(items) => JsValue::Array(items.iter().map(decode).collect()),
            JsValue::Object(object) => {
                if object.contains_key("$undefined") {
                    return JsValue::Undefined;
                }
                let mut out = JsObject::new();
                for (key, value) in object.entries() {
                    out.insert(key.clone(), decode(value));
                }
                JsValue::Object(out)
            }
            other => other.clone(),
        }
    }

    /// Tool predicate setups recorded by the generator. 'absent' and 'throw'
    /// both reach the manifest as false through TS safely(); 'undefined' and
    /// 'null' pass the raw return value through.
    fn predicate(spec: &JsValue, key: &str) -> JsValue {
        match spec.get(key).map(text).as_deref() {
            Some("true") => JsValue::Bool(true),
            Some("undefined") => JsValue::Undefined,
            Some("null") => JsValue::Null,
            _ => JsValue::Bool(false),
        }
    }

    fn tool_snapshot(spec: &JsValue) -> ToolSnapshot {
        let mut fields = JsObject::new();
        for key in ["name", "description", "kind", "inputSchema"] {
            if let Some(value) = spec.get(key) {
                fields.insert(key.into(), decode(value));
            }
        }
        ToolSnapshot {
            fields,
            read_only: predicate(spec, "readOnly"),
            concurrency_safe: predicate(spec, "concurrencySafe"),
            // `requiresUserInteraction?.({}) ?? false` collapses every
            // non-boolean result to false before safely() runs.
            requires_user_interaction: JsValue::Bool(matches!(
                spec.get("requiresUserInteraction").map(text).as_deref(),
                Some("true")
            )),
            required_runtime_capabilities: spec.get("requiredRuntimeCapabilities").map(|value| {
                array(value)
                    .iter()
                    .map(|item| item.as_string().unwrap().clone())
                    .collect()
            }),
        }
    }

    fn manifest_input(spec: &JsValue) -> ManifestInput {
        let config = field(spec, "config");
        let input = field(spec, "input");
        let modules = field(spec, "modules");
        let model = field(&modules, "model");
        ManifestInput {
            tools: array(&field(spec, "tools"))
                .iter()
                .map(tool_snapshot)
                .collect(),
            permission: PermissionSnapshot {
                cwd: field(&config, "cwd"),
                permission_mode: field(&config, "permissionMode"),
                permission_context: field(&config, "permissionContext")
                    .as_object()
                    .expect("config permission context")
                    .clone(),
                turn_can_prompt: input.get("canPrompt").cloned(),
            },
            modules: ModuleInventory {
                model: ModelInventory {
                    materialize_request: yes(&model, "materializeRequest"),
                    metadata: yes(&model, "metadata"),
                },
                budget: field(&modules, "budget")
                    .as_object()
                    .map(|budget| BudgetInventory {
                        estimate_request_input: object_yes(budget, "estimateRequestInput"),
                        evaluate_request_budget: object_yes(budget, "evaluateRequestBudget"),
                        estimate_usage_cost: object_yes(budget, "estimateUsageCost"),
                    }),
                plan_todo: yes(&modules, "planTodo"),
                context: field(&modules, "context")
                    .as_object()
                    .map(|context| ContextInventory {
                        apply_tool_results: object_yes(context, "applyToolResults"),
                        recover_from_model_error: object_yes(context, "recoverFromModelError"),
                        capture_turn: object_yes(context, "captureTurn"),
                        try_auto_compact: object_yes(context, "tryAutoCompact"),
                    }),
                permission: yes(&modules, "permission"),
                lifecycle: yes(&modules, "lifecycle"),
                event: yes(&modules, "event"),
                elicitation_available: field(&modules, "interaction").as_bool(),
            },
            turn: TurnCallbacks {
                drain_steer_messages: yes(&input, "drainSteer"),
                drain_or_close_steer_mailbox: yes(&input, "drainOrClose"),
                on_compact_persisted: yes(&input, "persistCompaction"),
            },
        }
    }

    /// Byte-for-byte replay against manifests the real TS dispatcher produced
    /// (fixtures/host-manifest/oracle.json, generated twice and compared).
    #[test]
    fn replay_real_ts_manifest_goldens() {
        let oracle = parse_json(include_str!(
            "../../../tests/fixtures/host-manifest/oracle.json"
        ))
        .expect("oracle.json must be valid JSON");
        assert_eq!(text(&field(&oracle, "commit")), BASELINE_COMMIT);
        let hashes = field(&oracle, "sourceSha256");
        let hashes = hashes.as_object().expect("sourceSha256");
        assert_eq!(hashes.len(), 650, "#303 exact import closure source count");
        let cases = field(&oracle, "cases");
        assert_eq!(array(&cases).len(), 48);
        for spec in array(&cases) {
            let name = text(&field(spec, "name"));
            let manifest = compose_manifest(&manifest_input(spec));
            let actual = stringify_json(&JsValue::Object(manifest))
                .expect("manifest encodes")
                .expect("manifest is an object");
            let expected = stringify_json(&field(spec, "expected"))
                .expect("expected encodes")
                .expect("expected is an object");
            assert_eq!(actual, expected, "case {name}");
        }
    }

    /// Two compositions from the same snapshot must agree byte-for-byte.
    #[test]
    fn composition_is_deterministic() {
        let oracle = parse_json(include_str!(
            "../../../tests/fixtures/host-manifest/oracle.json"
        ))
        .expect("oracle.json must be valid JSON");
        let cases = field(&oracle, "cases");
        for spec in array(&cases) {
            let first = compose_manifest(&manifest_input(spec));
            let second = compose_manifest(&manifest_input(spec));
            assert_eq!(
                stringify_json(&JsValue::Object(first)).unwrap().unwrap(),
                stringify_json(&JsValue::Object(second)).unwrap().unwrap(),
                "case {}",
                text(&field(spec, "name"))
            );
        }
    }
    #[test]
    fn live_tool_snapshot_preserves_safely_semantics_and_evaluates_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct LiveTool {
            fields: JsObject,
            value: Result<JsValue, HostError>,
            calls: AtomicUsize,
        }
        impl LiveTool {
            fn predicate(&self, input: &JsValue) -> Result<JsValue, HostError> {
                assert_eq!(input, &JsValue::Object(JsObject::new()));
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.value.clone()
            }
        }
        impl ToolDefinition for LiveTool {
            fn fields(&self) -> &JsObject {
                &self.fields
            }
            fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
                self.predicate(input)
            }
            fn is_concurrency_safe(&self, input: &JsValue) -> Result<JsValue, HostError> {
                self.predicate(input)
            }
            fn requires_user_interaction(&self, input: &JsValue) -> Result<JsValue, HostError> {
                self.predicate(input)
            }
            fn required_runtime_capabilities(&self) -> Option<Vec<JsString>> {
                Some(vec!["test-capability".into()])
            }
        }
        for value in [
            Ok(JsValue::Bool(false)),
            Ok(JsValue::Bool(true)),
            Ok(JsValue::Null),
            Ok(JsValue::Undefined),
            Ok("non-boolean".into()),
            Err(crate::host::ModuleError::new("predicate failed").into()),
        ] {
            let tool = LiveTool {
                fields: parse_json(
                    r#"{"name":"live","inputSchema":{},"description":"test","kind":"tool"}"#,
                )
                .unwrap()
                .as_object()
                .unwrap()
                .clone(),
                value,
                calls: AtomicUsize::new(0),
            };
            let snapshot = ToolSnapshot::from_definition(&tool);
            let captured = serialize_tool_descriptor(&snapshot);
            assert_eq!(tool.calls.load(Ordering::SeqCst), 3);
            assert_eq!(captured, serialize_tool_descriptor(&tool));
        }
    }
}

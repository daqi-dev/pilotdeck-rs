//! Gateway-side encoder for the Module 2.0 `execute` payload.
//!
//! Mirrors `agentLoopSidecarClient.executeRequest` (cc7c4e6 :741-790) and
//! `serializeAgentConfig` (:1290-1310); the inverse of the transport decoder
//! (`pilotdeck-transport/src/factory_payload.rs:768`). Field presence is the
//! contract: the decoder treats absent keys as "no override", so an encoder
//! that always emits keys would change turn behaviour.
use pilotdeck_protocol::{JsObject, JsString, JsValue};

/// One turn's worth of already-resolved host state.
pub struct ExecutePayloadInput<'a> {
    pub run_id: &'a JsString,
    pub operation_id: &'a JsString,
    pub request_id: &'a JsString,
    pub session_id: &'a JsString,
    pub turn_id: &'a JsString,
    /// Serialized `AgentRuntimeConfig` (`serializeAgentConfig` order).
    pub agent: JsValue,
    pub messages: Vec<JsValue>,
    pub max_turns: Option<f64>,
    pub max_budget_usd: Option<f64>,
    pub task_budget_usd: Option<f64>,
    pub initial_task_budget_spent_usd: Option<f64>,
    pub can_elicit: Option<bool>,
    pub model_override: Option<JsValue>,
    pub base_permission_mode: Option<&'a str>,
    pub allow_plan_mode_tools: Option<bool>,
    /// Already-serialized tool descriptors (`tool_descriptor` output).
    pub tools: Vec<JsValue>,
    pub permission_context: JsValue,
    pub host_modules: JsValue,
    pub interaction_capabilities: JsValue,
    pub seed_state: Option<JsValue>,
    pub model_state: Option<JsValue>,
}

/// `executeRequest()` (:741-790). `messageId` is the caller's; the transport
/// owns `kind`/envelope framing, so only the body is produced here.
pub fn encode_execute(input: &ExecutePayloadInput<'_>) -> JsObject {
    let mut request = JsObject::new();
    request.insert("messageId".into(), JsValue::from("execute"));
    request.insert("method".into(), JsValue::from("execute"));
    request.insert("runId".into(), JsValue::String(input.run_id.clone()));
    request.insert(
        "operationId".into(),
        JsValue::String(input.operation_id.clone()),
    );
    request.insert(
        "requestId".into(),
        JsValue::String(input.request_id.clone()),
    );
    request.insert(
        "sessionId".into(),
        JsValue::String(input.session_id.clone()),
    );
    request.insert("turnId".into(), JsValue::String(input.turn_id.clone()));

    let mut payload = JsObject::new();
    payload.insert("agent".into(), input.agent.clone());
    payload.insert("messages".into(), JsValue::Array(input.messages.clone()));
    // Presence checks match the decoder's `!== undefined` / truthy rules.
    for (key, value) in [
        ("maxTurns", input.max_turns),
        ("maxBudgetUsd", input.max_budget_usd),
        ("taskBudgetUsd", input.task_budget_usd),
        (
            "initialTaskBudgetSpentUsd",
            input.initial_task_budget_spent_usd,
        ),
    ] {
        if let Some(value) = value.filter(|value| value.is_finite()) {
            payload.insert(key.into(), JsValue::Number(value));
        }
    }
    if let Some(value) = input.can_elicit {
        payload.insert("canElicit".into(), JsValue::Bool(value));
    }
    if let Some(value) = &input.model_override {
        payload.insert("modelOverride".into(), value.clone());
    }
    if let Some(value) = input.base_permission_mode {
        payload.insert("basePermissionMode".into(), JsValue::from(value));
    }
    if let Some(value) = input.allow_plan_mode_tools {
        payload.insert("allowPlanModeTools".into(), JsValue::Bool(value));
    }
    payload.insert("tools".into(), JsValue::Array(input.tools.clone()));
    payload.insert("permissionContext".into(), input.permission_context.clone());
    payload.insert("hostModules".into(), input.host_modules.clone());
    payload.insert(
        "interactionCapabilities".into(),
        input.interaction_capabilities.clone(),
    );
    if let Some(value) = &input.seed_state {
        payload.insert("seedState".into(), value.clone());
    }
    if let Some(value) = &input.model_state {
        payload.insert("modelState".into(), value.clone());
    }
    request.insert("payload".into(), JsValue::Object(payload));
    request
}

/// `serializeToolDescriptor` (:440). Key order matches the decoder's readers.
#[allow(clippy::too_many_arguments)]
pub fn tool_descriptor(
    name: &JsString,
    description: &JsString,
    kind: &JsString,
    input_schema: &JsObject,
    read_only: bool,
    concurrency_safe: bool,
    requires_user_interaction: bool,
    required_runtime_capabilities: Option<&[JsString]>,
) -> JsValue {
    let mut object = JsObject::new();
    object.insert("name".into(), JsValue::String(name.clone()));
    object.insert("description".into(), JsValue::String(description.clone()));
    object.insert("kind".into(), JsValue::String(kind.clone()));
    object.insert("inputSchema".into(), JsValue::Object(input_schema.clone()));
    object.insert("readOnly".into(), JsValue::Bool(read_only));
    object.insert("concurrencySafe".into(), JsValue::Bool(concurrency_safe));
    object.insert(
        "requiresUserInteraction".into(),
        JsValue::Bool(requires_user_interaction),
    );
    if let Some(capabilities) = required_runtime_capabilities {
        object.insert(
            "requiredRuntimeCapabilities".into(),
            JsValue::Array(
                capabilities
                    .iter()
                    .map(|value| JsValue::String(value.clone()))
                    .collect(),
            ),
        );
    }
    JsValue::Object(object)
}

/// `serializeAgentConfig` (:1290-1310). Optional keys are omitted when the
/// source omits them; the decoder distinguishes `undefined` from `null`.
pub fn serialize_agent_config(config: &AgentConfig<'_>) -> JsValue {
    let mut agent = JsObject::new();
    agent.insert("provider".into(), JsValue::String(config.provider.clone()));
    agent.insert("model".into(), JsValue::String(config.model.clone()));
    agent.insert("cwd".into(), JsValue::String(config.cwd.clone()));
    for (key, value) in [
        ("systemPrompt", config.system_prompt),
        ("appendSystemPrompt", config.append_system_prompt),
        ("planModeInstructions", config.plan_mode_instructions),
    ] {
        if let Some(value) = value {
            agent.insert(key.into(), JsValue::String(value.clone()));
        }
    }
    if let Some(value) = config.runtime_context_surface {
        agent.insert("runtimeContextSurface".into(), JsValue::from(value));
    }
    for (key, value) in [
        ("maxOutputTokens", config.max_output_tokens),
        ("maxContextTokens", config.max_context_tokens),
        ("maxContextMessages", config.max_context_messages),
    ] {
        if let Some(value) = value.filter(|value| *value >= 1.0) {
            agent.insert(key.into(), JsValue::Number(value));
        }
    }
    for (key, value) in [
        ("thinking", config.thinking),
        ("toolChoice", config.tool_choice),
        ("metadata", config.metadata),
        ("subagentModel", config.subagent_model),
    ] {
        if let Some(value) = value {
            agent.insert(key.into(), value.clone());
        }
    }
    for (key, value) in [
        ("stopOnStructuredOutput", config.stop_on_structured_output),
        ("jsonSelfCorrect", config.json_self_correct),
        ("isSubagent", config.is_subagent),
    ] {
        if let Some(value) = value {
            agent.insert(key.into(), JsValue::Bool(value));
        }
    }
    if let Some(value) = config.run_mode {
        agent.insert("runMode".into(), JsValue::from(value));
    }
    if let Some(value) = config.permission_mode_before_plan {
        agent.insert("permissionModeBeforePlan".into(), JsValue::from(value));
    }
    JsValue::Object(agent)
}

/// The subset of `AgentRuntimeConfig` this encoder serializes.
pub struct AgentConfig<'a> {
    pub provider: &'a JsString,
    pub model: &'a JsString,
    pub cwd: &'a JsString,
    pub system_prompt: Option<&'a JsString>,
    pub append_system_prompt: Option<&'a JsString>,
    pub plan_mode_instructions: Option<&'a JsString>,
    pub runtime_context_surface: Option<&'a str>,
    pub max_output_tokens: Option<f64>,
    pub max_context_tokens: Option<f64>,
    pub max_context_messages: Option<f64>,
    pub thinking: Option<&'a JsValue>,
    pub tool_choice: Option<&'a JsValue>,
    pub metadata: Option<&'a JsValue>,
    pub subagent_model: Option<&'a JsValue>,
    pub stop_on_structured_output: Option<bool>,
    pub json_self_correct: Option<bool>,
    pub is_subagent: Option<bool>,
    pub run_mode: Option<&'a str>,
    pub permission_mode_before_plan: Option<&'a str>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_transport::execute::{FactoryPayloadDecoder, OriginalFactoryPayloadDecoder};

    fn js(text: &str) -> JsString {
        text.into()
    }

    /// The encoder must be the exact inverse of the transport decoder: every
    /// field the decoder reads has to survive a round trip unchanged. This is
    /// what makes the gateway able to drive a Rust sidecar at all.
    #[test]
    fn encode_round_trips_through_the_transport_decoder() {
        let provider = js("anthropic");
        let model = js("claude-x");
        let cwd = js("/tmp/project");
        let agent = serialize_agent_config(&AgentConfig {
            provider: &provider,
            model: &model,
            cwd: &cwd,
            system_prompt: Some(&js("be brief")),
            append_system_prompt: None,
            plan_mode_instructions: None,
            runtime_context_surface: Some("user_message"),
            max_output_tokens: Some(4096.0),
            max_context_tokens: Some(200000.0),
            max_context_messages: None,
            thinking: None,
            tool_choice: None,
            metadata: None,
            subagent_model: None,
            stop_on_structured_output: None,
            json_self_correct: None,
            is_subagent: Some(false),
            run_mode: Some("agent"),
            permission_mode_before_plan: None,
        });
        let request = encode_execute(&ExecutePayloadInput {
            run_id: &js("run-1"),
            operation_id: &js("op-1"),
            request_id: &js("req-1"),
            session_id: &js("session-1"),
            turn_id: &js("turn-1"),
            agent,
            messages: vec![
                pilotdeck_protocol::parse_json(r#"{"role":"user","content":"hello"}"#).unwrap(),
            ],
            max_turns: Some(12.0),
            max_budget_usd: None,
            task_budget_usd: Some(1.5),
            initial_task_budget_spent_usd: Some(0.25),
            can_elicit: Some(true),
            model_override: None,
            base_permission_mode: Some("default"),
            allow_plan_mode_tools: Some(false),
            tools: vec![tool_descriptor(
                &js("read_file"),
                &js("read"),
                &js("filesystem"),
                &JsObject::new(),
                true,
                true,
                false,
                None,
            )],
            permission_context: pilotdeck_protocol::parse_json(r#"{"mode":"default"}"#).unwrap(),
            host_modules: pilotdeck_protocol::parse_json(r#"{"model":{"methods":["stream"]}}"#)
                .unwrap(),
            interaction_capabilities: pilotdeck_protocol::parse_json(
                r#"{"elicitationAvailable":true}"#,
            )
            .unwrap(),
            seed_state: None,
            model_state: None,
        });
        let decoded = OriginalFactoryPayloadDecoder
            .decode(&JsValue::Object(request), &js("/process"))
            .expect("gateway-encoded payload must decode");
        assert_eq!(
            decoded.config.route.provider.to_utf8().ok().as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            decoded.config.route.model.to_utf8().ok().as_deref(),
            Some("claude-x")
        );
        assert_eq!(
            decoded.config.cwd.to_utf8().ok().as_deref(),
            Some("/tmp/project")
        );
        assert_eq!(decoded.config.max_output_tokens, Some(4096.0));
        assert_eq!(
            decoded.input.session_id.to_utf8().ok().as_deref(),
            Some("session-1")
        );
        assert_eq!(
            decoded.input.turn_id.to_utf8().ok().as_deref(),
            Some("turn-1")
        );
        assert_eq!(decoded.input.max_turns, Some(12.0));
        assert_eq!(decoded.input.task_budget_usd, Some(1.5));
        assert_eq!(decoded.input.initial_task_budget_spent_usd, Some(0.25));
        assert!(decoded.input.can_elicit);
        assert_eq!(decoded.tools.len(), 1);
        assert_eq!(
            decoded.tools[0].name.to_utf8().ok().as_deref(),
            Some("read_file")
        );
        assert!(decoded.tools[0].read_only);
        assert!(decoded.elicitation_available);
        assert_eq!(decoded.input.messages.len(), 1);
    }

    /// An absent optional key must stay absent, not become an empty default:
    /// the decoder treats "missing" and "present but nullish" differently.
    #[test]
    fn omitted_optionals_stay_absent() {
        let provider = js("p");
        let model = js("m");
        let cwd = js("/c");
        let agent = serialize_agent_config(&AgentConfig {
            provider: &provider,
            model: &model,
            cwd: &cwd,
            system_prompt: None,
            append_system_prompt: None,
            plan_mode_instructions: None,
            runtime_context_surface: None,
            max_output_tokens: None,
            max_context_tokens: None,
            max_context_messages: None,
            thinking: None,
            tool_choice: None,
            metadata: None,
            subagent_model: None,
            stop_on_structured_output: None,
            json_self_correct: None,
            is_subagent: None,
            run_mode: None,
            permission_mode_before_plan: None,
        });
        let object = agent.as_object().expect("agent config object");
        assert_eq!(object.len(), 3, "only provider/model/cwd are mandatory");
        assert!(!object.contains_key("systemPrompt"));
        assert!(!object.contains_key("maxOutputTokens"));
    }
}

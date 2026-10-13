//! Bounded RemoteHost → issued-owner HostExchange captures, not formal stdio/runtime.
use pilotdeck_agent::ports::*;
use pilotdeck_protocol::admission::CallEffect;
use pilotdeck_protocol::model::{BoundarySnapshot, ModelProtocol, ToolSchema};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use pilotdeck_transport::*;
fn owner() -> ExecutionIdentity {
    ExecutionIdentity {
        session_id: "s".into(),
        turn_id: "t".into(),
        run_id: "r".into(),
        operation_id: "o".into(),
        request_id: "issued-context".into(),
        idempotency_key: None,
        module_instance_id: "bound".into(),
        connection_generation: "g".into(),
    }
}
#[derive(Default)]
struct CaptureExchange {
    calls: Vec<(&'static str, &'static str, CallEffect, JsObject)>,
}
impl HostExchange for CaptureExchange {
    fn exchange(
        &mut self,
        control: &CallControl,
        module: &'static str,
        operation: &'static str,
        effect: CallEffect,
        payload: JsObject,
    ) -> PortResult<HostExchangeResponse> {
        assert!(control.cancellation.cause().is_none());
        self.calls.push((module, operation, effect, payload));
        Ok(HostExchangeResponse { identity:owner(),cancelled_before_response:false,boundary:BoundarySnapshot::capture(&parse_json(r#"{"kind":"response","messageId":"response","inReplyTo":"call","ok":true,"payload":{"result":{"messages":[],"systemPromptParts":[],"tools":[],"diagnostics":[],"boundaries":[]}}}"#).unwrap()) })
    }
}
fn preparation() -> ContextPrepare {
    ContextPrepare {
        preview_only: None,
        session_id: "s".into(),
        turn_id: "t".into(),
        step_id: None,
        cwd: "/never-read".into(),
        runtime_context_surface: None,
        route: Route {
            provider: "p".into(),
            model: "m".into(),
        },
        protocol: None,
        supports_prompt_cache: None,
        permission_mode: "default".into(),
        run_mode: Some("agent".into()),
        additional_working_directories: vec![],
        messages: vec![],
        tools: vec![],
        custom_system_prompt: None,
        append_system_prompt: None,
        max_messages: None,
    }
}
fn captured(v: ContextPrepare) -> JsValue {
    let mut host = RemoteHost::new(
        CaptureExchange::default(),
        AdvertisedMethods {
            modules: vec![("context".into(), vec!["prepare_for_model".into()])],
        },
    );
    assert!(matches!(
        host.call(
            &CallControl {
                cancellation: Cancellation::default(),
                deadline: None
            },
            HostCall::PrepareContext(Box::new(v))
        )
        .unwrap(),
        HostReply::Context(_)
    ));
    let exchange = host.into_exchange();
    assert_eq!(exchange.calls.len(), 1);
    let (module, operation, effect, payload) = &exchange.calls[0];
    assert_eq!((*module, *operation), ("context", "prepare_for_model"));
    assert_eq!(*effect, CallEffect::SideEffect);
    assert!(
        payload
            .get("operation")
            .unwrap()
            .as_string()
            .unwrap()
            .equals_str("prepare_for_model")
    );
    payload.get("input").unwrap().clone()
}
fn keys(v: &JsValue) -> Vec<String> {
    v.as_object()
        .unwrap()
        .entries()
        .iter()
        .map(|(key, _)| key.to_utf8().unwrap())
        .collect()
}
const SOURCE_KEYS: [&str; 17] = [
    "sessionId",
    "turnId",
    "cwd",
    "runtimeContextSurface",
    "provider",
    "model",
    "protocol",
    "supportsPromptCache",
    "permissionMode",
    "runMode",
    "additionalWorkingDirectories",
    "messages",
    "tools",
    "maxMessages",
    "customSystemPrompt",
    "appendSystemPrompt",
    "previewOnly",
];
#[test]
fn normal_source_raw_order_and_undefined_survive_until_json_encoding() {
    let input = captured(preparation());
    assert_eq!(keys(&input), SOURCE_KEYS);
    for key in [
        "runtimeContextSurface",
        "protocol",
        "supportsPromptCache",
        "maxMessages",
        "customSystemPrompt",
        "appendSystemPrompt",
        "previewOnly",
    ] {
        assert!(matches!(input.get(key), Some(JsValue::Undefined)), "{key}");
    }
    assert!(input.get("abortSignal").is_none());
    assert!(input.get("stepId").is_none());
    assert_eq!(
        stringify_json(&input).unwrap().unwrap(),
        r#"{"sessionId":"s","turnId":"t","cwd":"/never-read","provider":"p","model":"m","permissionMode":"default","runMode":"agent","additionalWorkingDirectories":[],"messages":[],"tools":[]}"#
    );
    assert_eq!(keys(&input), SOURCE_KEYS); // Encoder does not remove own properties in the raw boundary.
}
#[test]
fn all_canonical_optionals_preserve_source_positions_and_false_empty_zero() {
    let mut v = preparation();
    v.runtime_context_surface = Some(RuntimeContextSurface::UserMessage);
    v.protocol = Some(ModelProtocol::Anthropic);
    v.supports_prompt_cache = Some(false);
    v.preview_only = Some(false);
    v.max_messages = Some(0.0);
    v.custom_system_prompt = Some("".into());
    v.append_system_prompt = Some("addon".into());
    v.run_mode = Some("ask".into());
    v.additional_working_directories = vec!["/also-never-read".into()];
    let input = captured(v);
    assert_eq!(keys(&input), SOURCE_KEYS);
    assert_eq!(
        stringify_json(&input).unwrap().unwrap(),
        r#"{"sessionId":"s","turnId":"t","cwd":"/never-read","runtimeContextSurface":"user_message","provider":"p","model":"m","protocol":"anthropic","supportsPromptCache":false,"permissionMode":"default","runMode":"ask","additionalWorkingDirectories":["/also-never-read"],"messages":[],"tools":[],"maxMessages":0,"customSystemPrompt":"","appendSystemPrompt":"addon","previewOnly":false}"#
    );
}
#[test]
fn preview_true_and_durable_step_are_appended_without_native_cancellation_authority() {
    let mut v = preparation();
    v.preview_only = Some(true);
    v.step_id = Some(-0.0);
    let input = captured(v);
    let mut expected = SOURCE_KEYS.to_vec();
    expected.push("stepId");
    assert_eq!(keys(&input), expected);
    assert!(matches!(
        input.get("previewOnly"),
        Some(JsValue::Bool(true))
    ));
    assert_eq!(
        input.get("stepId").unwrap().as_number().unwrap().to_bits(),
        (-0.0f64).to_bits()
    );
    let encoded = stringify_json(&input).unwrap().unwrap();
    assert!(encoded.ends_with(r#""previewOnly":true,"stepId":0}"#));
    assert!(!encoded.contains("abortSignal"));
}
#[test]
fn tool_schema_has_original_name_description_input_schema_order_and_undefined_description() {
    let mut v = preparation();
    let mut schema = JsObject::new();
    schema.insert("type".into(), "object".into());
    v.tools = vec![
        ToolSchema {
            name: "read".into(),
            description: Some("".into()),
            input_schema: schema.clone(),
        },
        ToolSchema {
            name: JsString::from_utf16(vec![0xd800]),
            description: None,
            input_schema: schema,
        },
    ];
    let input = captured(v);
    let JsValue::Array(tools) = input.get("tools").unwrap() else {
        panic!()
    };
    for tool in tools {
        assert_eq!(keys(tool), ["name", "description", "inputSchema"]);
    }
    assert!(
        tools[0]
            .get("description")
            .unwrap()
            .as_string()
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        tools[1].get("description"),
        Some(JsValue::Undefined)
    ));
    let encoded = stringify_json(input.get("tools").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        encoded,
        r#"[{"name":"read","description":"","inputSchema":{"type":"object"}},{"name":"\ud800","inputSchema":{"type":"object"}}]"#
    );
}
#[test]
fn protocol_labels_and_run_mode_own_undefined_are_not_defaulted_by_transport() {
    for (protocol, label) in [
        (ModelProtocol::OpenAi, "openai"),
        (ModelProtocol::OpenAiResponses, "openai-responses"),
        (ModelProtocol::Google, "google"),
    ] {
        let mut v = preparation();
        v.protocol = Some(protocol);
        v.run_mode = None;
        let input = captured(v);
        assert!(
            input
                .get("protocol")
                .unwrap()
                .as_string()
                .unwrap()
                .equals_str(label)
        );
        assert!(matches!(input.get("runMode"), Some(JsValue::Undefined)));
    }
}

#[test]
fn budget_projection_omits_only_unsupplied_preview_slot_while_normal_keeps_it() {
    for preview in [None, Some(false), Some(true)] {
        let mut v = preparation();
        v.preview_only = preview;
        let mut host = RemoteHost::new(
            CaptureExchange::default(),
            AdvertisedMethods {
                modules: vec![("context".into(), vec!["try_auto_compact".into()])],
            },
        );
        let _reply = host
            .call(
                &CallControl {
                    cancellation: Cancellation::default(),
                    deadline: None,
                },
                HostCall::AutoCompact(Box::new(CompactionIntent {
                    session_id: Some("s".into()),
                    turn_id: Some("t".into()),
                    stage: BudgetStage::PreRoute,
                    trigger: CompactionTrigger::Auto,
                    messages: vec![],
                    max_context_tokens: None,
                    reserved_output_tokens: None,
                    manual_force: None,
                    allow_fallback_on_failure: None,
                    budget_request: None,
                    preparation: Some(v),
                    prepared_handle: None,
                    calibration: None,
                })),
            )
            .unwrap();
        let exchange = host.into_exchange();
        let payload = &exchange.calls[0].3;
        let budget = payload
            .get("input")
            .unwrap()
            .get("budgetPreparation")
            .unwrap();
        let mut expected = SOURCE_KEYS[..16].to_vec();
        if preview.is_some() {
            expected.push("previewOnly");
        }
        assert_eq!(keys(budget), expected);
        match preview {
            None => assert!(budget.get("previewOnly").is_none()),
            Some(value) => assert_eq!(budget.get("previewOnly").unwrap().as_bool(), Some(value)),
        };
    }
    assert!(matches!(
        captured(preparation()).get("previewOnly"),
        Some(JsValue::Undefined)
    ));
}

//! Source-derived pure factory cases. Original exported differential execution
//! and live default-factory/AgentLoop acceptance remain separate gates.
use pilotdeck_agent::ports::{PermissionMode, RuntimeContextSurface};
use pilotdeck_protocol::model::{ThinkingMode, ToolChoice};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use pilotdeck_transport::{AdvertisedMethods, execute};

#[path = "../src/factory_payload.rs"]
mod factory_payload;

fn obj(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in fields {
        out.insert(key.into(), value);
    }
    JsValue::Object(out)
}
fn arr(values: impl IntoIterator<Item = JsValue>) -> JsValue {
    JsValue::Array(values.into_iter().collect())
}
fn array(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(values) = value {
        values
    } else {
        panic!("expected canonical array")
    }
}
fn n(value: f64) -> JsValue {
    JsValue::Number(value)
}
fn b(value: bool) -> JsValue {
    JsValue::Bool(value)
}
fn decode(payload: JsValue) -> execute::FactoryPayload {
    factory_payload::decode(
        &obj([("operationId", "op".into()), ("payload", payload)]),
        &"/controlled/cwd".into(),
    )
    .unwrap()
}
fn text(value: JsValue) -> JsValue {
    obj([("type", "text".into()), ("text", value)])
}
fn message(content: JsValue) -> JsValue {
    obj([("content", content)])
}
fn content(payload: &execute::FactoryPayload, index: usize) -> &JsValue {
    payload.input.messages[index]
        .value()
        .get("content")
        .unwrap()
}
fn error(payload: JsValue) -> execute::FactoryPayloadError {
    factory_payload::decode(&obj([("payload", payload)]), &"/controlled/cwd".into()).unwrap_err()
}
fn subagent_error(value: JsValue, expected: &str) {
    assert_eq!(
        error(obj([("subagentModel", value)])),
        execute::FactoryPayloadError::InvalidSubagentModel {
            message: expected.into()
        }
    );
}

#[test]
fn defaults_use_supplied_cwd_and_json_task_fallback() {
    let result = decode(obj([]));
    assert_eq!(result.config.cwd, JsString::from("/controlled/cwd"));
    assert_eq!(result.config.route.provider, JsString::from("default"));
    assert_eq!(result.config.route.model, JsString::from("default"));
    assert_eq!(result.input.session_id, JsString::from("op"));
    assert_eq!(result.input.turn_id, JsString::from("op"));
    assert_eq!(content(&result, 0), &arr([text("{}".into())]));
    assert!(result.config.metadata.is_none());
    assert!(result.config.permission.rules.is_none());
    assert!(
        result
            .methods
            .modules
            .iter()
            .all(|(_, methods)| methods.is_empty())
    );
}

#[test]
fn explicit_override_messages_own_undefined_null_and_empty_never_fall_back() {
    for value in [JsValue::Undefined, JsValue::Null, arr([]), n(5.0)] {
        let result = decode(obj([
            ("task", obj([("prompt", "fallback".into())])),
            ("messages", arr([message("payload".into())])),
            ("contextOverride", obj([("messages", value)])),
        ]));
        assert!(result.input.messages.is_empty());
    }
    let result = decode(obj([
        ("task", obj([("prompt", "fallback".into())])),
        ("messages", arr([])),
    ]));
    assert_eq!(content(&result, 0), &arr([text("fallback".into())]));
}

#[test]
fn nullish_precedence_does_not_skip_invalid_or_empty_present_values() {
    let result = decode(obj([
        ("provider", "payload".into()),
        ("maxOutputTokens", n(9.0)),
        ("runMode", "plan".into()),
        ("systemPrompt", "payload system".into()),
        (
            "task",
            obj([
                ("prompt", "".into()),
                ("instruction", "not selected".into()),
            ]),
        ),
        (
            "agent",
            obj([
                ("provider", "".into()),
                ("maxOutputTokens", n(0.0)),
                ("runMode", "invalid".into()),
                ("systemPrompt", "agent system".into()),
            ]),
        ),
        ("contextOverride", obj([("systemPrompt", "".into())])),
    ]));
    assert!(result.config.route.provider.is_empty());
    assert_eq!(result.config.max_output_tokens, None);
    assert_eq!(result.config.run_mode, None);
    assert_eq!(result.config.system_prompt, Some("".into()));
    assert_eq!(
        content(&result, 0),
        &arr([text(
            "{\"prompt\":\"\",\"instruction\":\"not selected\"}".into()
        )])
    );
}

#[test]
fn js_string_coercion_keeps_utf16_arrays_and_nonfinite_number_names() {
    let surrogate = JsString::from_utf16(vec![0xd800, 0x0061]);
    let input = obj([
        (
            "sessionId",
            arr([
                JsValue::Null,
                JsValue::String(surrogate.clone()),
                arr([n(2.0), JsValue::Undefined]),
            ]),
        ),
        ("turnId", n(f64::NAN)),
        (
            "payload",
            obj([
                ("provider", n(f64::INFINITY)),
                ("model", n(f64::NEG_INFINITY)),
                ("cwd", b(false)),
            ]),
        ),
    ]);
    let result = factory_payload::decode(&input, &"unused".into()).unwrap();
    assert_eq!(
        result.input.session_id.units(),
        &[0x002c, 0xd800, 0x0061, 0x002c, 0x0032, 0x002c]
    );
    assert_eq!(result.input.turn_id, JsString::from("NaN"));
    assert_eq!(result.config.route.provider, JsString::from("Infinity"));
    assert_eq!(result.config.route.model, JsString::from("-Infinity"));
    assert_eq!(result.config.cwd, JsString::from("false"));
}

#[test]
fn final_null_operation_id_and_noncallable_to_string_are_distinct() {
    let result =
        factory_payload::decode(&obj([("operationId", JsValue::Null)]), &"x".into()).unwrap();
    assert_eq!(result.input.session_id, JsString::from("null"));
    assert_eq!(result.input.turn_id, JsString::from("null"));
    assert_eq!(
        error(obj([("provider", obj([("toString", JsValue::Null)]))])),
        execute::FactoryPayloadError::SourceTypeError {
            message: "Cannot convert object to primitive value".into()
        }
    );
    for receiver in [JsValue::Null, JsValue::Undefined] {
        assert!(matches!(
            factory_payload::decode(&receiver, &"x".into()),
            Err(execute::FactoryPayloadError::SourceTypeError { .. })
        ));
    }
}

#[test]
fn subagent_validation_order_and_undefined_null_distinction() {
    subagent_error(JsValue::Null, "agent.subagentModel must be an object.");
    subagent_error(
        obj([("provider", "".into()), ("model", n(2.0))]),
        "agent.subagentModel.provider must be a non-empty string.",
    );
    subagent_error(
        obj([
            ("provider", "p".into()),
            ("model", "".into()),
            ("maxContextTokens", JsValue::Null),
        ]),
        "agent.subagentModel.model must be a non-empty string.",
    );
    subagent_error(
        obj([
            ("provider", "p".into()),
            ("model", "m".into()),
            ("maxContextTokens", JsValue::Null),
            ("maxOutputTokens", n(0.0)),
        ]),
        "agent.subagentModel.maxContextTokens must be a positive integer.",
    );
    subagent_error(
        obj([
            ("provider", "p".into()),
            ("model", "m".into()),
            ("maxContextTokens", JsValue::Undefined),
            ("maxOutputTokens", n(0.5)),
        ]),
        "agent.subagentModel.maxOutputTokens must be a positive integer.",
    );
    assert!(
        decode(obj([("subagentModel", JsValue::Undefined)]))
            .config
            .subagent_model
            .is_none()
    );
}

#[test]
fn positive_integer_factory_fields_allow_unsafe_integers_but_not_nan() {
    let unsafe_integer = 9_007_199_254_740_992.0;
    let result = decode(obj([
        ("maxTurns", n(unsafe_integer)),
        ("maxOutputTokens", n(unsafe_integer)),
        ("maxContextMessages", n(f64::INFINITY)),
        ("maxBudgetUsd", n(0.5)),
        ("taskBudgetUsd", n(f64::NAN)),
        ("initialTaskBudgetSpentUsd", n(-0.0)),
        (
            "subagentModel",
            obj([
                ("provider", "p".into()),
                ("model", "m".into()),
                ("maxContextTokens", n(unsafe_integer)),
            ]),
        ),
    ]));
    assert_eq!(result.input.max_turns, Some(unsafe_integer));
    assert_eq!(result.config.max_output_tokens, Some(unsafe_integer));
    assert_eq!(result.config.max_context_messages, None);
    assert_eq!(result.input.max_budget_usd, Some(0.5));
    assert_eq!(result.input.task_budget_usd, None);
    assert_eq!(
        result
            .input
            .initial_task_budget_spent_usd
            .unwrap()
            .to_bits(),
        (-0.0f64).to_bits()
    );
    assert_eq!(
        result.config.subagent_model.unwrap().max_context_tokens,
        Some(unsafe_integer)
    );
}

#[test]
fn thinking_and_tool_choice_preserve_false_and_strict_names() {
    let result = decode(obj([
        (
            "thinking",
            obj([
                ("enabled", b(false)),
                ("mode", "max".into()),
                ("budgetTokens", n(3.0)),
                ("preserve", b(false)),
                ("splitReasoning", b(true)),
            ]),
        ),
        (
            "toolChoice",
            obj([("type", "tool".into()), ("name", "tool".into())]),
        ),
        ("jsonSelfCorrect", b(false)),
        ("stopOnStructuredOutput", b(false)),
    ]));
    let thinking = result.config.thinking.unwrap();
    assert!(!thinking.enabled);
    assert_eq!(thinking.mode, Some(ThinkingMode::Max));
    assert_eq!(thinking.budget_tokens, Some(3.0));
    assert_eq!(thinking.preserve, Some(false));
    assert_eq!(thinking.split_reasoning, Some(true));
    assert_eq!(
        result.config.tool_choice,
        Some(ToolChoice::Tool {
            name: "tool".into()
        })
    );
    assert_eq!(result.config.json_self_correct, Some(false));
    assert_eq!(result.config.stop_on_structured_output, Some(false));
    let invalid = decode(obj([
        ("thinking", obj([("enabled", n(1.0))])),
        (
            "toolChoice",
            obj([("type", "tool".into()), ("name", "".into())]),
        ),
    ]));
    assert!(invalid.config.thinking.is_none());
    assert!(invalid.config.tool_choice.is_none());
}

#[test]
fn permission_opaque_rules_and_all_or_nothing_directories_are_not_grants() {
    let result = decode(obj([
        (
            "agent",
            obj([
                ("permissionMode", "plan".into()),
                ("canPrompt", b(true)),
                ("runtimeContextSurface", "user_message".into()),
            ]),
        ),
        (
            "permissionContext",
            obj([
                ("mode", "invalid".into()),
                ("canPrompt", n(1.0)),
                ("bypassAvailable", b(true)),
                (
                    "additionalWorkingDirectories",
                    arr(["".into(), "a".into(), "a".into()]),
                ),
                (
                    "rules",
                    obj([
                        ("allow", arr([JsValue::Undefined, JsValue::Null, n(42.0)])),
                        ("deny", n(1.0)),
                        ("ask", arr([])),
                    ]),
                ),
            ]),
        ),
    ]));
    assert_eq!(result.config.permission.mode, PermissionMode::Plan);
    assert!(!result.config.permission.can_prompt);
    assert!(result.config.permission.bypass_available);
    assert_eq!(
        result.config.runtime_context_surface,
        RuntimeContextSurface::UserMessage
    );
    assert_eq!(
        result.config.permission.additional_working_directories,
        vec![JsString::from(""), "a".into(), "a".into()]
    );
    let rules = result.config.permission.rules.unwrap();
    assert_eq!(
        rules.allow,
        Some(vec![JsValue::Undefined, JsValue::Null, n(42.0)])
    );
    assert!(rules.deny.is_none());
    assert_eq!(rules.ask, Some(vec![]));
    let invalid = decode(obj([
        ("runtimeContextSurface", "user_message".into()),
        (
            "permissionContext",
            obj([(
                "additionalWorkingDirectories",
                arr(["valid".into(), n(1.0)]),
            )]),
        ),
    ]));
    assert!(
        invalid
            .config
            .permission
            .additional_working_directories
            .is_empty()
    );
    assert_eq!(
        invalid.config.runtime_context_surface,
        RuntimeContextSurface::SystemPrompt
    );
}

#[test]
fn metadata_shallow_order_overwrite_and_own_undefined_are_detached() {
    let result = decode(obj([
        (
            "agent",
            obj([(
                "metadata",
                obj([
                    ("a", obj([("nested", "old".into())])),
                    ("b", "first".into()),
                ]),
            )]),
        ),
        (
            "executionContext",
            obj([("b", JsValue::Undefined), ("c", JsValue::Null)]),
        ),
        (
            "contextOverride",
            obj([("metadata", obj([("a", "last".into()), ("d", b(false))]))]),
        ),
    ]));
    let metadata = result.config.metadata.unwrap();
    assert_eq!(
        metadata
            .entries()
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>(),
        vec![JsString::from("a"), "b".into(), "c".into(), "d".into()]
    );
    assert_eq!(metadata.get("a"), Some(&JsValue::from("last")));
    assert_eq!(metadata.get("b"), Some(&JsValue::Undefined));
    assert_eq!(metadata.get("c"), Some(&JsValue::Null));
    assert!(
        decode(obj([("executionContext", obj([]))]))
            .config
            .metadata
            .unwrap()
            .is_empty()
    );
}

#[test]
fn proto_metadata_requires_separate_native_graph_projection() {
    for key in ["agent", "executionContext", "contextOverride"] {
        for value in [JsValue::Null, obj([("inherited", b(true))]), arr([])] {
            let metadata = obj([("__proto__", value)]);
            let source = if key == "executionContext" {
                metadata
            } else {
                obj([("metadata", metadata)])
            };
            assert!(matches!(
                error(obj([(key, source)])),
                execute::FactoryPayloadError::NativeGraphRequired { .. }
            ));
        }
    }
    for value in [
        JsValue::Undefined,
        "ignored".into(),
        n(0.0),
        n(f64::NAN),
        b(false),
    ] {
        let result = decode(obj([
            (
                "agent",
                obj([("metadata", obj([("first", n(1.0)), ("__proto__", value)]))]),
            ),
            ("executionContext", obj([("second", n(2.0))])),
        ]));
        let metadata = result.config.metadata.unwrap();
        assert!(!metadata.contains_key("__proto__"));
        assert_eq!(
            metadata,
            obj([("first", n(1.0)), ("second", n(2.0))])
                .as_object()
                .unwrap()
                .clone()
        );
    }
}

#[test]
fn host_methods_filter_unknown_values_without_dedup_or_defaults() {
    let result = decode(obj([(
        "hostModules",
        obj([
            (
                "model",
                obj([(
                    "methods",
                    arr([
                        "stream_next".into(),
                        "prepare".into(),
                        "prepare".into(),
                        "unknown".into(),
                        n(2.0),
                    ]),
                )]),
            ),
            (
                "context",
                obj([(
                    "methods",
                    arr(["capture_turn".into(), "prepare_for_model".into()]),
                )]),
            ),
            (
                "capability",
                obj([(
                    "methods",
                    arr(["execute_batch".into(), "plan_todo".into(), "execute".into()]),
                )]),
            ),
        ]),
    )]));
    assert_eq!(
        result.methods.modules[0].1,
        vec![
            JsString::from("stream_next"),
            "prepare".into(),
            "prepare".into()
        ]
    );
    assert_eq!(
        result.methods.modules[3].1,
        vec![JsString::from("capture_turn"), "prepare_for_model".into()]
    );
    assert_eq!(
        result.methods.modules[6].1,
        vec![
            JsString::from("execute_batch"),
            "plan_todo".into(),
            "execute".into()
        ]
    );
}

#[test]
fn descriptor_caps_use_set_order_but_duplicate_tool_names_remain() {
    let descriptor = obj([
        ("name", "tool".into()),
        ("description", "".into()),
        ("kind", "agent".into()),
        ("inputSchema", obj([("type", "invalid".into())])),
        (
            "input_schema",
            obj([("type", "object".into()), ("notSelected", b(true))]),
        ),
        ("readOnly", n(1.0)),
        ("concurrencySafe", b(true)),
        ("requiresUserInteraction", b(true)),
        (
            "requiredRuntimeCapabilities",
            arr([
                "user_interaction".into(),
                "plan_workflow".into(),
                "plan_workflow".into(),
                "invalid".into(),
            ]),
        ),
    ]);
    let result = decode(obj([(
        "tools",
        obj([(
            "available",
            arr([
                descriptor.clone(),
                descriptor,
                obj([("name", "".into())]),
                JsValue::Null,
            ]),
        )]),
    )]));
    assert_eq!(result.tools.len(), 2);
    assert_eq!(
        result.tools[0].description,
        JsString::from("Host capability")
    );
    assert!(!result.tools[0].read_only);
    assert!(result.tools[0].concurrency_safe);
    assert_eq!(
        result.tools[0].input_schema,
        obj([("type", "object".into())])
            .as_object()
            .unwrap()
            .clone()
    );
    assert_eq!(
        result.tools[0].required_runtime_capabilities,
        Some(vec![
            JsString::from("user_interaction"),
            "plan_workflow".into(),
            "subagent_fork".into()
        ])
    );
    let overridden = decode(obj([
        ("tools", arr([obj([("name", "tool".into())])])),
        ("contextOverride", obj([("tools", JsValue::Null)])),
    ]));
    assert!(overridden.tools.is_empty());
    let undefined = decode(obj([
        ("tools", arr([obj([("name", "tool".into())])])),
        ("contextOverride", obj([("tools", JsValue::Undefined)])),
    ]));
    assert_eq!(undefined.tools.len(), 1);
}

#[test]
fn canonical_message_metadata_keeps_false_empty_and_fork_subset() {
    let result = decode(obj([(
        "messages",
        arr([
            obj([
                ("role", "system".into()),
                ("content", "text".into()),
                (
                    "metadata",
                    obj([
                        ("model", "".into()),
                        ("synthetic", b(false)),
                        ("transient", b(true)),
                        ("purpose", "".into()),
                        ("unknown", b(true)),
                        (
                            "forkCarryover",
                            obj([
                                ("sourceSessionId", "".into()),
                                ("sourceTurnId", JsValue::Null),
                                ("unknown", b(true)),
                            ]),
                        ),
                    ]),
                ),
            ]),
            obj([("role", "assistant".into()), ("content", "second".into())]),
        ]),
    )]));
    assert_eq!(
        result.input.messages[0].value().get("role"),
        Some(&JsValue::from("user"))
    );
    assert_eq!(
        result.input.messages[1].value().get("role"),
        Some(&JsValue::from("assistant"))
    );
    assert_eq!(
        result.input.messages[0].value().get("metadata"),
        Some(&obj([
            ("model", "".into()),
            ("synthetic", b(false)),
            ("transient", b(true)),
            ("purpose", "".into()),
            ("forkCarryover", obj([("sourceSessionId", "".into())]))
        ]))
    );
}

#[test]
fn block_raw_null_inputs_and_nested_result_fifo_are_canonicalized() {
    let tool = obj([
        ("type", "tool_call".into()),
        ("id", "".into()),
        ("name", "".into()),
        ("input", JsValue::Null),
        ("raw", JsValue::Null),
        ("unknown", b(true)),
    ]);
    let nested = obj([
        ("type", "tool_result".into()),
        ("toolCallId", "id".into()),
        (
            "content",
            arr([
                text("first".into()),
                obj([
                    ("type", "tool_result".into()),
                    ("toolCallId", "child".into()),
                    ("content", "".into()),
                    ("raw", JsValue::Undefined),
                ]),
                text("last".into()),
            ]),
        ),
        ("raw", JsValue::Null),
        ("isError", b(false)),
    ]);
    let result = decode(obj([("messages", arr([message(arr([tool, nested]))]))]));
    assert_eq!(
        content(&result, 0),
        &arr([
            obj([
                ("type", "tool_call".into()),
                ("id", "".into()),
                ("name", "".into()),
                ("input", obj([])),
                ("raw", JsValue::Null)
            ]),
            obj([
                ("type", "tool_result".into()),
                ("toolCallId", "id".into()),
                (
                    "content",
                    arr([
                        text("first".into()),
                        obj([
                            ("type", "tool_result".into()),
                            ("toolCallId", "child".into()),
                            ("content", arr([text("".into())]))
                        ]),
                        text("last".into())
                    ])
                ),
                ("raw", JsValue::Null)
            ])
        ])
    );
}

#[test]
fn timeline_accepts_negative_unsafe_integer_and_empty_ids_without_extensions() {
    let timeline = obj([
        ("version", n(1.0)),
        ("turnId", "".into()),
        ("id", "".into()),
        ("previousId", "".into()),
        ("order", n(-3.0)),
        ("revision", n(9_007_199_254_740_992.0)),
        ("offset", n(-2.0)),
        ("unknown", b(true)),
    ]);
    let first = obj([
        ("type", "text".into()),
        ("text", JsValue::String(JsString::from_utf16(vec![0xdc00]))),
        ("timeline", timeline),
    ]);
    let second = obj([
        ("type", "text".into()),
        ("text", "invalid timeline".into()),
        (
            "timeline",
            obj([
                ("version", n(1.0)),
                ("turnId", "t".into()),
                ("id", "b".into()),
                ("order", n(0.5)),
                ("revision", n(1.0)),
            ]),
        ),
    ]);
    let result = decode(obj([("messages", arr([message(arr([first, second]))]))]));
    let blocks = array(content(&result, 0));
    assert_eq!(
        blocks[0].get("text").unwrap().as_string().unwrap().units(),
        &[0xdc00]
    );
    assert_eq!(
        blocks[0].get("timeline"),
        Some(&obj([
            ("version", n(1.0)),
            ("turnId", "".into()),
            ("id", "".into()),
            ("previousId", "".into()),
            ("order", n(-3.0)),
            ("revision", n(9_007_199_254_740_992.0)),
            ("offset", n(-2.0))
        ]))
    );
    assert!(blocks[1].get("timeline").is_none());
}

#[test]
fn media_projection_preserves_fractional_sizes_and_filters_bad_pdf() {
    let result = decode(obj([(
        "messages",
        arr([obj([
            ("content", "start".into()),
            (
                "images",
                arr([
                    obj([
                        ("type", "image".into()),
                        ("source", "url".into()),
                        ("data", "url".into()),
                        ("mimeType", "".into()),
                        ("bytes", n(0.5)),
                        ("detail", "low".into()),
                    ]),
                    obj([
                        ("type", "pdf".into()),
                        ("source", "base64".into()),
                        ("data", "pdf".into()),
                        ("mimeType", "application/pdf".into()),
                        ("bytes", n(0.0)),
                        ("pages", n(2.0)),
                    ]),
                    obj([
                        ("type", "pdf".into()),
                        ("source", "base64".into()),
                        ("data", "invalid".into()),
                        ("mimeType", "application/pdf".into()),
                    ]),
                    obj([
                        ("type", "audio".into()),
                        ("source", "base64".into()),
                        ("data", "audio".into()),
                        ("mimeType", "audio/wav".into()),
                        ("durationSeconds", n(0.25)),
                        ("bytes", n(f64::NAN)),
                    ]),
                ]),
            ),
        ])]),
    )]));
    let blocks = array(content(&result, 0));
    assert_eq!(blocks.len(), 4);
    assert_eq!(blocks[1].get("bytes"), Some(&n(0.5)));
    assert_eq!(blocks[2].get("pages"), Some(&n(2.0)));
    assert_eq!(blocks[3].get("durationSeconds"), Some(&n(0.25)));
    assert!(blocks[3].get("bytes").is_none());
}

#[test]
fn data_url_regex_is_case_sensitive_nonempty_and_not_base64_validation() {
    let urls = [
        "data:image/png;base64,not validated!",
        "DATA:image/png;base64,abc",
        "data:;base64,abc",
        "data:image/png;base64,",
        "data:image/png;base64,abc\n",
        "data:image/png;base64,a\u{2028}b",
        "https://example.invalid/image",
        "data:a\nb;base64,x",
    ];
    let result = decode(obj([(
        "messages",
        arr([message(arr(urls.into_iter().map(|url| {
            obj([
                ("type", "image_url".into()),
                ("image_url", obj([("url", url.into())])),
                (
                    "timeline",
                    obj([
                        ("version", n(1.0)),
                        ("turnId", "t".into()),
                        ("id", "id".into()),
                        ("order", n(1.0)),
                        ("revision", n(0.0)),
                    ]),
                ),
            ])
        })))]),
    )]));
    assert_eq!(
        content(&result, 0),
        &arr([
            obj([
                ("type", "image".into()),
                ("source", "base64".into()),
                ("mimeType", "image/png".into()),
                ("data", "not validated!".into())
            ]),
            obj([
                ("type", "image".into()),
                ("source", "base64".into()),
                ("mimeType", "a\nb".into()),
                ("data", "x".into())
            ])
        ])
    );
}

#[test]
fn model_override_route_requirements_and_finite_negative_parameters() {
    let result = decode(obj([
        (
            "modelOverride",
            obj([
                ("provider", "p".into()),
                ("model", "m".into()),
                ("temperature", n(-2.0)),
                ("speed", n(-0.0)),
                (
                    "thinking",
                    obj([("enabled", b(true)), ("mode", "unknown".into())]),
                ),
            ]),
        ),
        (
            "interactionCapabilities",
            obj([("elicitationAvailable", b(true))]),
        ),
        ("allowPlanModeTools", b(true)),
        ("canElicit", b(true)),
    ]));
    let model = result.input.model_override.unwrap();
    assert_eq!(model.temperature, Some(-2.0));
    assert_eq!(model.speed.unwrap().to_bits(), (-0.0f64).to_bits());
    assert!(model.thinking.unwrap().mode.is_none());
    assert!(result.elicitation_available);
    assert!(result.input.allow_plan_mode_tools);
    assert!(result.input.can_elicit);
    assert!(
        decode(obj([(
            "modelOverride",
            obj([("provider", "".into()), ("model", "m".into())])
        )]))
        .input
        .model_override
        .is_none()
    );
}

#[test]
fn thinking_and_reference_blocks_select_only_original_fields() {
    let result = decode(obj([(
        "messages",
        arr([message(arr([
            obj([
                ("type", "thinking".into()),
                ("text", "".into()),
                ("blockId", "".into()),
                ("signature", "sig".into()),
                ("reasoningContent", "reason".into()),
                ("private", b(true)),
            ]),
            obj([
                ("type", "tool_result_reference".into()),
                ("toolCallId", "".into()),
                ("path", "".into()),
                ("originalBytes", n(0.25)),
                ("preview", "".into()),
                ("hasMore", b(false)),
                ("isError", b(true)),
                ("readFilePath", "read".into()),
                ("mimeType", JsValue::Null),
                ("reason", "".into()),
            ]),
            obj([
                ("type", "media_reference".into()),
                ("path", "media".into()),
                ("originalBytes", n(0.0)),
                ("preview", "preview".into()),
                ("hasMore", b(true)),
                ("mimeType", "image/png".into()),
                ("mediaType", "image".into()),
                ("toolCallId", "".into()),
                ("pages", n(0.5)),
                ("detail", "high".into()),
                ("reason", "source".into()),
            ]),
            obj([
                ("type", "media_reference".into()),
                ("path", "bad".into()),
                ("originalBytes", n(0.0)),
                ("preview", "".into()),
                ("hasMore", b(true)),
                ("mimeType", "x".into()),
                ("mediaType", "video".into()),
            ]),
        ]))]),
    )]));
    assert_eq!(
        content(&result, 0),
        &arr([
            obj([
                ("type", "thinking".into()),
                ("text", "".into()),
                ("blockId", "".into()),
                ("signature", "sig".into()),
                ("reasoningContent", "reason".into())
            ]),
            obj([
                ("type", "tool_result_reference".into()),
                ("toolCallId", "".into()),
                ("path", "".into()),
                ("originalBytes", n(0.25)),
                ("preview", "".into()),
                ("hasMore", b(false)),
                ("isError", b(true)),
                ("readFilePath", "read".into()),
                ("reason", "".into())
            ]),
            obj([
                ("type", "media_reference".into()),
                ("path", "media".into()),
                ("originalBytes", n(0.0)),
                ("preview", "preview".into()),
                ("hasMore", b(true)),
                ("mimeType", "image/png".into()),
                ("mediaType", "image".into()),
                ("toolCallId", "".into()),
                ("detail", "high".into()),
                ("reason", "source".into())
            ]),
        ])
    );
}

#[test]
fn retained_seed_model_and_raw_message_boundaries_are_deep_detached() {
    let payload = obj([
        (
            "seedState",
            obj([(
                "unknown",
                arr([
                    JsValue::Undefined,
                    JsValue::Null,
                    JsValue::String(JsString::from_utf16(vec![0xd800])),
                ]),
            )]),
        ),
        ("modelState", JsValue::Undefined),
        (
            "messages",
            arr([message(arr([obj([
                ("type", "tool_call".into()),
                ("id", "id".into()),
                ("name", "name".into()),
                ("input", obj([("nested", arr([JsValue::Undefined]))])),
                ("raw", obj([("unknown", JsValue::Null)])),
            ])]))]),
        ),
    ]);
    let mut request = obj([("payload", payload)]);
    let result = factory_payload::decode(&request, &"controlled".into()).unwrap();
    let before = result.seed_state.as_ref().unwrap().value().clone();
    if let JsValue::Object(object) = &mut request {
        object.insert("payload".into(), JsValue::Null);
    }
    assert_eq!(result.seed_state.unwrap().value(), &before);
    assert_eq!(result.model_state.unwrap().value(), &JsValue::Undefined);
    assert_eq!(
        content(
            &decode(obj([(
                "messages",
                arr([message(arr([obj([("type", "unknown".into())])]))])
            )])),
            0
        ),
        &arr([text("{}".into())])
    );
}

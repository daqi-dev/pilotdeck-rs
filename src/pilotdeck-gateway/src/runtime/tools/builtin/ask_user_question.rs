//! Host-mediated multiple-choice question tool.
use super::{object, schema, string, text};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    native_process::elicitation::Elicitation,
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::HashSet;

pub struct AskUserQuestion {
    fields: JsObject,
}
impl Default for AskUserQuestion {
    fn default() -> Self {
        Self {
            fields: object([
                ("name", string("ask_user_question")),
                ("aliases", JsValue::Array(vec![string("AskUserQuestion")])),
                (
                    "description",
                    string("Ask the user one to four multiple-choice questions."),
                ),
                ("kind", string("session")),
                (
                    "requiredRuntimeCapabilities",
                    JsValue::Array(vec![string("user_interaction")]),
                ),
                ("shouldDefer", JsValue::Bool(true)),
                ("maxResultBytes", JsValue::Number(100_000.0)),
                (
                    "inputSchema",
                    JsValue::Object(schema(
                        r#"{"type":"object","required":["questions"],"additionalProperties":false,"properties":{"questions":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","required":["question","header","options"],"additionalProperties":false,"properties":{"question":{"type":"string"},"header":{"type":"string","maxLength":12},"options":{"type":"array","minItems":2,"maxItems":4,"items":{"type":"object","required":["label","description"],"additionalProperties":false,"properties":{"label":{"type":"string"},"description":{"type":"string"},"preview":{"type":"string"}}}},"multiSelect":{"type":"boolean"}}}},"answers":{"type":"object"},"annotations":{"type":"object"},"metadata":{"type":"object","additionalProperties":false,"properties":{"source":{"type":"string"}}}}}"#,
                    )),
                ),
            ]),
        }
    }
}
impl ToolDefinition for AskUserQuestion {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn requires_user_interaction(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn required_runtime_capabilities(&self) -> Option<Vec<JsString>> {
        Some(vec!["user_interaction".into()])
    }
}
fn issue(path: &str, message: String) -> Option<Vec<JsObject>> {
    Some(vec![object([
        ("path", string(path)),
        ("code", string("invalid_schema")),
        ("message", string(message)),
    ])])
}
fn validation(input: &JsValue) -> Option<Vec<JsObject>> {
    let full = regex::Regex::new(r"(?i)<\s*(html|body|!doctype)\b").unwrap();
    let danger = regex::Regex::new(r"(?i)<\s*(script|style)\b").unwrap();
    let tag = regex::Regex::new(r"(?i)<[a-z][^>]*>").unwrap();
    let Some(JsValue::Array(questions)) = input.get("questions") else {
        return issue("questions", "Provide 1-4 questions".into());
    };
    if questions.is_empty() {
        return issue("questions", "Provide 1-4 questions".into());
    }
    if questions.len() > 4 {
        return issue("questions", "At most 4 questions allowed".into());
    }
    for q in questions {
        let question = q
            .get("question")
            .and_then(JsValue::as_string)
            .map(text)
            .unwrap_or_default();
        if !matches!(q.get("options"), Some(JsValue::Array(a)) if (2..=4).contains(&a.len())) {
            return issue(
                "questions[].options",
                format!("Question \"{question}\" must have 2-4 options"),
            );
        }
    }
    let mut seen = HashSet::new();
    for q in questions {
        let Some(question) = q.get("question").and_then(JsValue::as_string) else {
            continue;
        };
        if !seen.insert(question.clone()) {
            return issue(
                "questions",
                format!("Question texts must be unique: \"{}\"", text(question)),
            );
        }
        if q.get("header")
            .and_then(JsValue::as_string)
            .is_some_and(|v| v.units().len() > 12)
        {
            return issue(
                "questions[].header",
                format!("header for \"{}\" exceeds 12 chars", text(question)),
            );
        }
        let Some(JsValue::Array(options)) = q.get("options") else {
            continue;
        };
        let mut labels = HashSet::new();
        for option in options {
            if let Some(label) = option.get("label").and_then(JsValue::as_string) {
                if !labels.insert(label.clone()) {
                    return issue(
                        "questions[].options",
                        format!(
                            "Option labels must be unique within question \"{}\"",
                            text(question)
                        ),
                    );
                }
                if let Some(preview) = option.get("preview").and_then(JsValue::as_string) {
                    let value = text(preview);
                    let err = if full.is_match(&value) {
                        Some(
                            "preview must be an HTML fragment, not a full document (no <html>, <body>, or <!DOCTYPE>)",
                        )
                    } else if danger.is_match(&value) {
                        Some(
                            "preview must not contain <script> or <style> tags. Use inline styles via the style attribute if needed.",
                        )
                    } else if !tag.is_match(&value) {
                        Some(
                            "preview must contain HTML (previewFormat is set to \"html\"). Wrap content in a tag like <div> or <pre>.",
                        )
                    } else {
                        None
                    };
                    if let Some(err) = err {
                        return issue(
                            "questions[].options[].preview",
                            format!(
                                "Option \"{}\" in question \"{}\": {err}",
                                text(label),
                                text(question)
                            ),
                        );
                    }
                }
            }
        }
    }
    None
}
fn js_truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(v) => *v,
        JsValue::Number(v) => *v != 0.0 && !v.is_nan(),
        JsValue::String(v) => !v.is_empty(),
        _ => true,
    }
}
fn append_js(units: &mut Vec<u16>, value: &JsValue) -> Result<(), ToolError> {
    units.extend(
        crate::js_value::to_string_units_checked(value).map_err(|_| {
            ToolError::new("tool_execution_failed", "Cannot convert answer to string.")
        })?,
    );
    Ok(())
}
impl ExecutableTool for AskUserQuestion {
    fn validate_input(
        &self,
        input: JsValue,
        _: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async move { Ok(validation(&input)) })
    }
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move {
            let channel = context
                .host
                .elicitation
                .as_ref()
                .and_then(|value| value.downcast_ref::<Elicitation>())
                .ok_or_else(|| {
                    ToolError::new(
                        "unsupported_tool",
                        "ask_user_question requires a host elicitation channel (none registered).",
                    )
                })?;
            let questions = input
                .get("questions")
                .cloned()
                .unwrap_or(JsValue::Undefined);
            let answers = input
                .get("answers")
                .and_then(JsValue::as_object)
                .filter(|a| !a.own_entries().is_empty());
            let (answers, annotations) = if let Some(answers) = answers {
                (
                    JsValue::Object(answers.clone()),
                    input.get("annotations").cloned(),
                )
            } else {
                let turn = context
                    .fields
                    .get("turnId")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default();
                let answer = channel
                    .ask(
                        turn,
                        questions.clone(),
                        input.get("metadata").cloned(),
                        &context,
                    )
                    .await?;
                if matches!(answer, JsValue::Null | JsValue::Undefined) {
                    let message = if matches!(answer, JsValue::Null) {
                        "Cannot read properties of null (reading 'type')"
                    } else {
                        "Cannot read properties of undefined (reading 'type')"
                    };
                    return Err(ToolError::new("tool_execution_failed", message));
                }
                if answer
                    .get("type")
                    .and_then(JsValue::as_string)
                    .is_some_and(|s| s.equals_str("cancelled"))
                {
                    let reason = answer
                        .get("reason")
                        .and_then(JsValue::as_string)
                        .map(text)
                        .filter(|v| !v.is_empty())
                        .map(|v| format!(" ({v})"))
                        .unwrap_or_default();
                    return Err(ToolError::new(
                        "unsupported_tool",
                        format!("User declined to answer questions{reason}").as_str(),
                    ));
                }
                (
                    answer.get("answers").cloned().unwrap_or(JsValue::Undefined),
                    answer.get("annotations").cloned(),
                )
            };
            let mut data = object([("questions", questions), ("answers", answers.clone())]);
            if let Some(value) = annotations.clone() {
                data.insert("annotations".into(), value);
            }
            let mut units: Vec<u16> = "User has answered your questions: "
                .encode_utf16()
                .collect();
            let entries: Vec<(JsString, JsValue)> = match &answers {
                JsValue::Null | JsValue::Undefined => {
                    return Err(ToolError::new(
                        "tool_execution_failed",
                        "Cannot convert undefined or null to object",
                    ));
                }
                JsValue::Object(map) => map
                    .own_entries()
                    .into_iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
                JsValue::Array(values) => values
                    .iter()
                    .enumerate()
                    .map(|(i, value)| (i.to_string().into(), value.clone()))
                    .collect(),
                JsValue::String(value) => value
                    .units()
                    .iter()
                    .enumerate()
                    .map(|(i, unit)| {
                        (
                            i.to_string().into(),
                            JsValue::String(JsString::from_utf16(vec![*unit])),
                        )
                    })
                    .collect(),
                _ => Vec::new(),
            };
            for (index, (key, value)) in entries.iter().enumerate() {
                if index > 0 {
                    units.extend(", ".encode_utf16());
                }
                units.push(34);
                units.extend_from_slice(key.units());
                units.extend("\"=\"".encode_utf16());
                match value {
                    JsValue::Array(values) => {
                        for (i, value) in values.iter().enumerate() {
                            if i > 0 {
                                units.extend(", ".encode_utf16());
                            }
                            if !matches!(value, JsValue::Null | JsValue::Undefined) {
                                append_js(&mut units, value)?;
                            }
                        }
                    }
                    value => append_js(&mut units, value)?,
                }
                units.push(34);
                if let Some(annotation) = annotations
                    .as_ref()
                    .and_then(JsValue::as_object)
                    .and_then(|a| a.get_key(key))
                {
                    for (name, prefix) in [
                        ("preview", " selected preview:\n"),
                        ("notes", " user notes: "),
                    ] {
                        if let Some(value) = annotation.get(name).filter(|value| js_truthy(value)) {
                            units.extend(prefix.encode_utf16());
                            append_js(&mut units, value)?;
                        }
                    }
                }
            }
            units.extend(". You can now continue with the user's answers in mind.".encode_utf16());
            Ok(ToolOutput {
                content: vec![JsValue::Object(object([
                    ("type", string("text")),
                    ("text", JsValue::String(JsString::from_utf16(units))),
                ]))],
                data: Some(JsValue::Object(data)),
                ..Default::default()
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_ts_validation_oracle() {
        let oracle = pilotdeck_protocol::parse_json(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/elicitation/oracle.json"
        )))
        .unwrap();
        let Some(JsValue::Array(cases)) = oracle.get("cases") else {
            panic!("missing cases")
        };
        let mut count = 0;
        for case in cases {
            let Some(expected) = case.get("validation") else {
                continue;
            };
            let actual = validation(case.get("input").unwrap());
            if expected.get("ok") == Some(&JsValue::Bool(true)) {
                assert!(actual.is_none());
            } else {
                assert_eq!(
                    JsValue::Array(actual.unwrap().into_iter().map(JsValue::Object).collect()),
                    expected.get("issues").unwrap().clone()
                );
            }
            count += 1;
        }
        assert_eq!(count, 7);
    }
}

#[cfg(test)]
mod execution_tests {
    use super::*;
    use crate::{
        host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext},
        runtime::{interaction::GatewayInteractionCoordinator, network::Cancellation},
    };
    use std::{sync::Arc, time::Duration};
    fn context(channel: Elicitation) -> ToolRunContext {
        let checkpoint = HostToolCheckpoint::new(None, &[]).unwrap();
        let host = Arc::new(ToolRuntimeContext {
            fields: object([("turnId", string("turn-1"))]),
            files: checkpoint.tool_context_state(),
            abort_signal: Some(Arc::new(Cancellation::new())),
            model: None,
            elicitation: Some(Arc::new(channel)),
            user_dialog: None,
            audit_recorder: None,
            file_history: None,
            file_update_notifier: None,
            now: None,
            progress: None,
            subagent: None,
            plan_todo: None,
            goal: None,
            plan_directory: None,
        });
        ToolRunContext::new(host)
    }
    #[tokio::test]
    async fn real_oracle_answer_and_prefilled_outputs() {
        let oracle = pilotdeck_protocol::parse_json(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/elicitation/oracle.json"
        )))
        .unwrap();
        let Some(JsValue::Array(cases)) = oracle.get("cases") else {
            panic!("missing cases")
        };
        for name in ["answer", "prefilled"] {
            let case = cases
                .iter()
                .find(|c| {
                    c.get("name")
                        .and_then(JsValue::as_string)
                        .is_some_and(|s| s.equals_str(name))
                })
                .unwrap();
            let coordinator = Arc::new(GatewayInteractionCoordinator::default());
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let channel = Elicitation {
                profile: crate::native_process::elicitation::test_profile(),
                coordinator: coordinator.clone(),
                session: "fixture".into(),
                can_prompt: true,
                emit: Arc::new(move |event| tx.send(event).is_ok()),
                timeout: Duration::from_secs(2),
            };
            let input = case.get("input").unwrap().clone();
            let task = tokio::spawn(async move {
                AskUserQuestion::default()
                    .execute(input, context(channel))
                    .await
            });
            if name == "answer" {
                let event = rx.recv().await.unwrap();
                let request = event.get("requestId").unwrap().as_string().unwrap();
                let answer = pilotdeck_protocol::parse_json(r#"{"type":"answered","answers":{"Pick?":"A"},"annotations":{"Pick?":{"notes":"note"}}}"#).unwrap();
                assert!(coordinator.respond_elicitation(&"fixture".into(), request, answer, None));
            }
            let output = tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let actual = JsValue::Object(object([
                ("content", JsValue::Array(output.content)),
                ("data", output.data.unwrap()),
            ]));
            assert_eq!(
                &actual,
                case.get("result").unwrap().get("value").unwrap(),
                "{name}"
            );
            if name == "prefilled" {
                assert!(rx.try_recv().is_err());
            }
            assert_eq!(
                coordinator
                    .elicitation_bus()
                    .pending_count(&"fixture".into()),
                0
            );
        }
    }
    #[tokio::test]
    async fn fixed_ts_answer_coercion_oracle() {
        let oracle = pilotdeck_protocol::parse_json(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/elicitation/oracle-coercion.json"
        )))
        .unwrap();
        let Some(JsValue::Array(cases)) = oracle.get("results") else {
            panic!("missing cases")
        };
        assert_eq!(cases.len(), 6);
        for case in cases {
            let channel = Elicitation {
                profile: crate::native_process::elicitation::test_profile(),
                coordinator: Arc::new(GatewayInteractionCoordinator::default()),
                session: "fixture".into(),
                can_prompt: true,
                emit: Arc::new(|_| panic!("prefilled answer must not prompt")),
                timeout: Duration::from_secs(2),
            };
            let output = AskUserQuestion::default()
                .execute(case.get("input").unwrap().clone(), context(channel))
                .await
                .unwrap();
            let actual = JsValue::Object(object([
                ("content", JsValue::Array(output.content)),
                ("data", output.data.unwrap()),
            ]));
            assert_eq!(&actual, case.get("result").unwrap().get("value").unwrap());
        }
    }

    #[tokio::test]
    async fn raw_gateway_answers_follow_fixed_ts_oracle() {
        let oracle = pilotdeck_protocol::parse_json(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/elicitation/oracle-malformed.json"
        )))
        .unwrap();
        let Some(JsValue::Array(cases)) = oracle.get("results") else {
            panic!("missing cases")
        };
        let input = pilotdeck_protocol::parse_json(r#"{"questions":[{"question":"Pick?","header":"Choice","options":[{"label":"A","description":"a"},{"label":"B","description":"b"}]}]}"#).unwrap();
        for case in cases {
            let coordinator = Arc::new(GatewayInteractionCoordinator::default());
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let channel = Elicitation {
                profile: crate::native_process::elicitation::test_profile(),
                coordinator: coordinator.clone(),
                session: "fixture".into(),
                can_prompt: true,
                emit: Arc::new(move |event| tx.send(event).is_ok()),
                timeout: Duration::from_secs(2),
            };
            let input = input.clone();
            let task = tokio::spawn(async move {
                AskUserQuestion::default()
                    .execute(input, context(channel))
                    .await
            });
            let event = rx.recv().await.unwrap();
            let request = event.get("requestId").unwrap().as_string().unwrap();
            let answer = case.get("answer").cloned().unwrap_or(JsValue::Undefined);
            assert!(coordinator.respond_elicitation(&"fixture".into(), request, answer, None));
            let result = task.await.unwrap();
            let expected = case.get("result").unwrap();
            if let Some(error) = expected.get("error") {
                let actual = result.unwrap_err();
                assert_eq!(Some(&JsValue::String(actual.message)), error.get("message"));
            } else {
                let output = result.unwrap();
                let actual = JsValue::Object(object([
                    ("content", JsValue::Array(output.content)),
                    ("data", output.data.unwrap()),
                ]));
                assert_eq!(&actual, expected.get("value").unwrap());
            }
        }
    }
}

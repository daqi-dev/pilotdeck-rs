//! Multiple-choice elicitation through the original connection-bound bus.
use super::{object, schema, string, text};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::{
        interaction::channel::QuestionChannel,
        tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
    },
};
use pilotdeck_protocol::{JsObject, JsValue};

pub struct AskUserQuestionTool {
    fields: JsObject,
}
impl AskUserQuestionTool {
    pub fn new() -> Self {
        Self {
            fields: object([
                ("name", string("ask_user_question")),
                ("aliases", JsValue::Array(vec![string("AskUserQuestion")])),
                (
                    "description",
                    string(
                        "Ask the user 1-4 multiple-choice questions, each with 2-4 options. Use the answers to continue execution; do not use this for plan approval.",
                    ),
                ),
                ("kind", string("session")),
                ("shouldDefer", JsValue::Bool(true)),
                ("maxResultBytes", JsValue::Number(100_000.0)),
                (
                    "inputSchema",
                    JsValue::Object(schema(
                        r#"{"type":"object","required":["questions"],"additionalProperties":false,"properties":{"questions":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","required":["question","header","options"],"additionalProperties":false,"properties":{"question":{"type":"string"},"header":{"type":"string","maxLength":12},"multiSelect":{"type":"boolean"},"options":{"type":"array","minItems":2,"maxItems":4,"items":{"type":"object","required":["label","description"],"additionalProperties":false,"properties":{"label":{"type":"string"},"description":{"type":"string"},"preview":{"type":"string"}}}}}}},"answers":{"type":"object"},"annotations":{"type":"object"},"metadata":{"type":"object","additionalProperties":false,"properties":{"source":{"type":"string"}}}}}"#,
                    )),
                ),
            ]),
        }
    }
}
impl Default for AskUserQuestionTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDefinition for AskUserQuestionTool {
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
    fn required_runtime_capabilities(&self) -> Option<Vec<pilotdeck_protocol::JsString>> {
        Some(vec!["user_interaction".into()])
    }
}
fn issue(path: &str, message: String) -> Vec<JsObject> {
    vec![object([
        ("path", string(path)),
        ("code", string("invalid_schema")),
        ("message", string(message)),
    ])]
}
fn validate(input: &JsValue) -> Option<Vec<JsObject>> {
    let preview_pattern_0 = regex::Regex::new(r"(?i)<\s*(html|body|!doctype)\b").unwrap();
    let preview_pattern_1 = regex::Regex::new(r"(?i)<\s*(script|style)\b").unwrap();
    let preview_pattern_2 = regex::Regex::new(r"(?i)<[a-z][^>]*>").unwrap();
    let Some(JsValue::Array(questions)) = input.get("questions") else {
        return Some(issue("questions", "Provide 1-4 questions".into()));
    };
    if questions.is_empty() {
        return Some(issue("questions", "Provide 1-4 questions".into()));
    }
    if questions.len() > 4 {
        return Some(issue("questions", "At most 4 questions allowed".into()));
    }
    let mut seen = Vec::new();
    for q in questions {
        let question = q.get("question").and_then(JsValue::as_string)?;
        let Some(JsValue::Array(options)) = q.get("options") else {
            return Some(issue(
                "questions[].options",
                format!("Question \"{}\" must have 2-4 options", text(question)),
            ));
        };
        if !(2..=4).contains(&options.len()) {
            return Some(issue(
                "questions[].options",
                format!("Question \"{}\" must have 2-4 options", text(question)),
            ));
        }
        if seen.contains(question) {
            return Some(issue(
                "questions",
                format!("Question texts must be unique: \"{}\"", text(question)),
            ));
        }
        seen.push(question.clone());
        if q.get("header")
            .and_then(JsValue::as_string)
            .is_some_and(|s| s.units().len() > 12)
        {
            return Some(issue(
                "questions[].header",
                format!("header for \"{}\" exceeds 12 chars", text(question)),
            ));
        }
        let mut labels = Vec::new();
        for option in options {
            let label = option.get("label").and_then(JsValue::as_string)?;
            if labels.contains(label) {
                return Some(issue(
                    "questions[].options",
                    format!(
                        "Option labels must be unique within question \"{}\"",
                        text(question)
                    ),
                ));
            }
            labels.push(label.clone());
            if let Some(preview) = option.get("preview").and_then(JsValue::as_string) {
                let preview = text(preview);
                let error = if preview_pattern_0.is_match(&preview) {
                    Some(
                        "preview must be an HTML fragment, not a full document (no <html>, <body>, or <!DOCTYPE>)",
                    )
                } else if preview_pattern_1.is_match(&preview) {
                    Some(
                        "preview must not contain <script> or <style> tags. Use inline styles via the style attribute if needed.",
                    )
                } else if !preview_pattern_2.is_match(&preview) {
                    Some(
                        "preview must contain HTML (previewFormat is set to \"html\"). Wrap content in a tag like <div> or <pre>.",
                    )
                } else {
                    None
                };
                if let Some(error) = error {
                    return Some(issue(
                        "questions[].options[].preview",
                        format!(
                            "Option \"{}\" in question \"{}\": {error}",
                            text(label),
                            text(question)
                        ),
                    ));
                }
            }
        }
    }
    None
}
fn output(input: &JsValue, answer: &JsValue) -> ToolOutput {
    let answers = answer
        .get("answers")
        .cloned()
        .unwrap_or(JsValue::Object(JsObject::new()));
    let annotations = answer.get("annotations");
    let mut entries = Vec::new();
    if let Some(values) = answers.as_object() {
        for (question, value) in values.entries() {
            let display = match value {
                JsValue::Array(v) => v
                    .iter()
                    .map(|x| x.as_string().map(text).unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => value.as_string().map(text).unwrap_or_default(),
            };
            let mut entry = format!("\"{}\"=\"{display}\"", text(question));
            for (key, prefix) in [
                ("preview", "selected preview:\n"),
                ("notes", "user notes: "),
            ] {
                if let Some(value) = annotations
                    .and_then(|a| a.as_object())
                    .and_then(|a| a.get_key(question))
                    .and_then(|a| a.get(key))
                    .and_then(JsValue::as_string)
                    .filter(|s| !s.is_empty())
                {
                    entry.push(' ');
                    entry.push_str(prefix);
                    entry.push_str(&text(value));
                }
            }
            entries.push(entry);
        }
    }
    let mut data = object([
        (
            "questions",
            input
                .get("questions")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        ),
        ("answers", answers),
    ]);
    if let Some(annotations) = annotations {
        data.insert("annotations".into(), annotations.clone());
    }
    ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            (
                "text",
                string(format!(
                    "User has answered your questions: {}. You can now continue with the user's answers in mind.",
                    entries.join(", ")
                )),
            ),
        ]))],
        data: Some(JsValue::Object(data)),
        ..Default::default()
    }
}
impl ExecutableTool for AskUserQuestionTool {
    fn validate_input(
        &self,
        input: JsValue,
        _: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async move { Ok(validate(&input)) })
    }
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move {
            let channel = context
                .host
                .elicitation
                .as_ref()
                .and_then(|v| v.downcast_ref::<QuestionChannel>())
                .ok_or_else(|| {
                    ToolError::new(
                        "unsupported_tool",
                        "ask_user_question requires a host elicitation channel (none registered).",
                    )
                })?;
            if input
                .get("answers")
                .and_then(JsValue::as_object)
                .is_some_and(|a| !a.entries().is_empty())
            {
                return Ok(output(&input, &input));
            }
            let answer = channel.ask(&input, &context).await?;
            if answer
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|s| s.equals_str("cancelled"))
            {
                let reason = answer
                    .get("reason")
                    .and_then(JsValue::as_string)
                    .filter(|s| !s.is_empty())
                    .map(|s| format!(" ({})", text(s)))
                    .unwrap_or_default();
                return Err(ToolError::new(
                    "unsupported_tool",
                    format!("User declined to answer questions{reason}"),
                ));
            }
            Ok(output(&input, &answer))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    #[test]
    fn question_validation_and_answer_format_preserve_contract() {
        let input = parse_json(r#"{"questions":[{"question":"Continue?","header":"Next","options":[{"label":"Yes","description":"Continue"},{"label":"No","description":"Stop"}]}],"answers":{"Continue?":"Yes"}}"#).unwrap();
        assert!(validate(&input).is_none());
        let result = output(&input, &input);
        assert_eq!(
            result.content[0].get("text"),
            Some(&string(
                "User has answered your questions: \"Continue?\"=\"Yes\". You can now continue with the user's answers in mind."
            ))
        );
        assert!(validate(&parse_json(r#"{"questions":[]}"#).unwrap()).is_some());
        let duplicate = parse_json(r#"{"questions":[{"question":"Q","header":"H","options":[{"label":"A","description":"A"},{"label":"A","description":"B"}]}]}"#).unwrap();
        assert!(validate(&duplicate).is_some());
    }
}

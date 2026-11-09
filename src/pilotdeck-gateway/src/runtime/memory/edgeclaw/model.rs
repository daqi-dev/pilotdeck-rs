//! Production model adapter for the native EdgeClaw memory runtime.
//!
//! The fixed TS baseline selects `memory.model || agentModel`, then reuses the
//! matching configured provider's URL, credentials and protocol
//! (`createEdgeClawMemoryProviderFromConfig.ts:72-99`). This adapter keeps the
//! same selection boundary and sends the request through PilotDeck's existing
//! provider transport, so OpenAI, Responses, Anthropic and Google retain their
//! normal request and error semantics.

use super::{NativeEdgeClawLlm, NativeEdgeClawLlmFuture};
use crate::runtime::model::http::{
    HyperModelTransport, ModelHttpTransport, StreamOptions, StreamOutput, stream_model,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::sync::Arc;

pub fn native_llm_from_snapshot(snapshot: &JsValue) -> Option<Arc<dyn NativeEdgeClawLlm>> {
    native_llm_from_snapshot_with_transport(snapshot, Arc::new(HyperModelTransport::new()))
}

fn native_llm_from_snapshot_with_transport(
    snapshot: &JsValue,
    transport: Arc<dyn ModelHttpTransport>,
) -> Option<Arc<dyn NativeEdgeClawLlm>> {
    let config = snapshot.get("config")?;
    let model_ref = config
        .get("memory")
        .and_then(|memory| memory.get("model"))
        .and_then(JsValue::as_string)
        .filter(|value| !text(value).trim().is_empty())
        .cloned()
        .or_else(|| {
            config
                .get("agent")
                .and_then(|agent| agent.get("model"))
                .and_then(|model| model.get("id"))
                .and_then(JsValue::as_string)
                .filter(|value| !text(value).trim().is_empty())
                .cloned()
        })?;
    let model_ref = text(&model_ref);
    let (provider, model) = model_ref.split_once('/')?;
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    let model_config = config.get("model")?.clone();
    Some(Arc::new(NativeMemoryLlm {
        provider: provider.into(),
        model: model.into(),
        model_config,
        transport,
    }))
}

struct NativeMemoryLlm {
    provider: JsString,
    model: JsString,
    model_config: JsValue,
    transport: Arc<dyn ModelHttpTransport>,
}

impl NativeEdgeClawLlm for NativeMemoryLlm {
    fn execute(&self, operation: &'static str, input: JsValue) -> NativeEdgeClawLlmFuture {
        let provider = self.provider.clone();
        let model = self.model.clone();
        let model_config = self.model_config.clone();
        let transport = self.transport.clone();
        Box::pin(async move {
            let request = request(operation, input, provider, model)?;
            let mut text = String::new();
            let mut provider_error = None;
            let mut sink = |output| {
                let StreamOutput::Event(event) = output else {
                    return;
                };
                match event.get("type").and_then(JsValue::as_string) {
                    Some(kind) if kind.equals_str("text_delta") => {
                        if let Some(delta) = event.get("text").and_then(JsValue::as_string) {
                            text.push_str(&String::from_utf16_lossy(delta.units()));
                        }
                    }
                    Some(kind) if kind.equals_str("error") => {
                        provider_error =
                            Some(event.get("error").cloned().unwrap_or(JsValue::Undefined));
                    }
                    _ => {}
                }
            };
            stream_model(
                transport.as_ref(),
                &request,
                &model_config,
                &StreamOptions::default(),
                &mut sink,
            )
            .await
            .map_err(|error| String::from_utf16_lossy(error.message().units()))?;
            if let Some(error) = provider_error {
                return Err(provider_error_message(&error));
            }
            parse_structured_response(&text)
        })
    }
}

fn stringify_js_value_pretty(value: &JsValue, indent: usize) -> String {
    let spaces = "  ".repeat(indent);
    match value {
        JsValue::Null | JsValue::Undefined => "null".to_string(),
        JsValue::Bool(b) => {
            if *b {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        JsValue::Number(n) => {
            if n.fract() == 0.0 && n.is_finite() && *n >= i64::MIN as f64 && *n <= i64::MAX as f64 {
                format!("{}", *n as i64)
            } else {
                format!("{n}")
            }
        }
        JsValue::String(s) => serde_json::to_string(&s.to_utf8().unwrap_or_default())
            .unwrap_or_else(|_| "\"\"".to_string()),
        JsValue::Array(arr) => {
            if arr.is_empty() {
                return "[]".to_string();
            }
            let next_spaces = "  ".repeat(indent + 1);
            let mut parts = Vec::new();
            for item in arr {
                parts.push(format!(
                    "{next_spaces}{}",
                    stringify_js_value_pretty(item, indent + 1)
                ));
            }
            format!("[\n{}\n{spaces}]", parts.join(",\n"))
        }
        JsValue::Object(obj) => {
            let entries: Vec<_> = obj
                .entries()
                .iter()
                .filter(|(_, val)| !matches!(val, JsValue::Undefined))
                .collect();
            if entries.is_empty() {
                return "{}".to_string();
            }
            let next_spaces = "  ".repeat(indent + 1);
            let mut parts = Vec::new();
            for (key, val) in entries {
                let key_str = serde_json::to_string(&key.to_utf8().unwrap_or_default())
                    .unwrap_or_else(|_| format!("\"{}\"", key.to_utf8().unwrap_or_default()));
                let val_str = stringify_js_value_pretty(val, indent + 1);
                parts.push(format!("{next_spaces}{key_str}: {val_str}"));
            }
            format!("{{\n{}\n{spaces}}}", parts.join(",\n"))
        }
    }
}

fn stringify_json_pretty(value: &JsValue) -> Result<String, String> {
    Ok(stringify_js_value_pretty(value, 0))
}

fn request(
    operation: &'static str,
    input: JsValue,
    provider: JsString,
    model: JsString,
) -> Result<JsValue, String> {
    let (system_prompt, user_content) = match operation {
        "decide_file_memory_route" | "memory_retrieval_route" => {
            let sys = [
                "You decide whether the current query should trigger long-term memory recall.",
                "Return JSON only with a single field route.",
                "Valid route values: none, user, project, mix.",
                "Use none unless the query clearly needs long-term memory.",
                "Use user only when the query is asking about stable personal identity/background facts about who the user is, such as name, profession, long-term role context, life background, or durable relationships.",
                "Do not use user for reply preferences, language choices, formatting rules, style guidance, file/tool boundaries, or delivery rules; those belong to project.",
                "Use project when the query only needs current project memory, including project facts, collaboration rules, delivery style, file boundaries, or project status.",
                "Use mix only when the query genuinely needs both current project memory and the user's stable identity/background at the same time.",
                "Do not use mix just because both could be helpful; choose mix only when both are actually necessary to answer well.",
            ].join("\n");
            let input_str = stringify_json_pretty(&input)?;
            (sys, input_str)
        }
        "select_file_manifest_entries" | "memory_manifest_select" => {
            let sys = [
                "You select a small number of memory files from a compact manifest.",
                "Return JSON only with selected_ids.",
                "Select at most 5 ids and prefer recent items that are directly useful for the query.",
            ].join("\n");
            let input_str = stringify_json_pretty(&input)?;
            (sys, input_str)
        }
        "select_recall_project" | "memory_recall_project" => {
            let sys = [
                "You choose the most relevant existing formal project for long-term memory recall only when one clearly matches the current query.",
                "Return JSON only with selected_project_id and reason.",
                "Select at most one project from the provided shortlist.",
                "Use the current query first, then recent user messages only for continuation/disambiguation.",
                "Do not infer a project from assistant wording.",
                "Similar project names are distinct by default; shared domain, shared workflow, or shared feedback do not make them the same project.",
                "If the query explicitly names one shortlist project, prefer that exact project instead of broadening to a nearby or umbrella project.",
                "If the current query introduces or switches to a new project that is not represented in the shortlist, return an empty selected_project_id.",
                "If no shortlist project is clearly relevant, return an empty selected_project_id.",
                "If multiple shortlist projects are plausible but evidence is not decisive, return an empty selected_project_id.",
                "When relevance is comparable, prefer general_local over workspace_external.",
                "Use empty selected_project_id to skip project-scoped recall for a new or unrelated project; do not force unrelated memory into an existing project.",
            ].join("\n");
            let input_str = stringify_json_pretty(&input)?;
            (sys, input_str)
        }
        _ => {
            let contract = match operation {
                "memory_retrieval_plan" => {
                    "Return one JSON object with route (none, user, project, or mix) and selectedIds (an array of manifest relativePath strings)."
                }
                "memory_index_project" => {
                    "Return one JSON object with decision, selected_project_id, and reason. decision must be attach_existing or create_new. Attach only to one General-local project from the supplied shortlist; otherwise choose create_new and return an empty selected_project_id."
                }
                "memory_classify_turn" => {
                    "Classify only the supplied focus user turn. Return one JSON object with should_store and labels. labels is an array whose type is user, project, or feedback and whose entries include reason and evidence. Assistant text is context only."
                }
                "memory_create_user" | "memory_create_project" | "memory_create_feedback" => {
                    "Create at most one durable memory note for the supplied classification. Return one JSON object with skip, name, description, and body. Set skip=true when the focus user turn does not justify a durable note."
                }
                "memory_dream_cluster_plan" => {
                    "Return one JSON object with summary and clusters. Each cluster has member_relative_paths and a concrete overlap/conflict reason, contains 2-8 files from the supplied category, and does not overlap another cluster."
                }
                "memory_dream_project_rewrite" | "memory_dream_feedback_rewrite" => {
                    "Return one JSON object with summary and file. file is either null or one rewritten durable note with name, description, and markdown for the supplied cluster."
                }
                "memory_dream_user_profile_merge" => {
                    "Return one JSON object with skip and candidate. candidate must contain name, description, and profile or body for the rewritten global user profile."
                }
                "memory_dream_general_project_merge_plan" => {
                    "Return one JSON object with summary and merge_groups. Each group has keeper_project_id, duplicate_project_ids, and reason. Use only supplied General-local project ids, never reuse an id across groups, and never list the keeper as a duplicate."
                }
                "memory_dream_project_meta_review" => {
                    "Return one JSON object with should_update, reason, and project_meta. project_meta contains project_name, description, and status derived from the supplied recent project and feedback records."
                }
                _ => {
                    return Err(format!(
                        "Unsupported native memory model operation: {operation}"
                    ));
                }
            };
            let input_str = stringify_json(&input)
                .map_err(|error| format!("Memory model input is not serializable: {error}"))?
                .unwrap_or_else(|| "null".into());
            (
                format!(
                    "You are PilotDeck's structured memory engine. {contract} The response must be valid JSON."
                ),
                format!("Operation: {operation}\nInput JSON:\n{input_str}"),
            )
        }
    };
    Ok(object([
        ("provider", JsValue::String(provider)),
        ("model", JsValue::String(model)),
        (
            "messages",
            JsValue::Array(vec![
                message("system", &system_prompt),
                message("user", &user_content),
            ]),
        ),
        ("stream", JsValue::Bool(true)),
    ]))
}

fn parse_structured_response(value: &str) -> Result<JsValue, String> {
    let trimmed = value.trim();
    let json = if let Some(fenced) = trimmed.strip_prefix("```json") {
        fenced.strip_suffix("```").map(str::trim).unwrap_or(fenced)
    } else if let Some(fenced) = trimmed.strip_prefix("```") {
        fenced.strip_suffix("```").map(str::trim).unwrap_or(fenced)
    } else {
        trimmed
    };
    let parsed =
        parse_json(json).map_err(|error| format!("Memory model returned invalid JSON: {error}"))?;
    if parsed.as_object().is_none() {
        return Err("Memory model response must be a JSON object.".into());
    }
    Ok(parsed)
}

fn provider_error_message(error: &JsValue) -> String {
    error
        .get("message")
        .and_then(JsValue::as_string)
        .map(text)
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| "Memory model provider failed.".into())
}

fn message(role: &str, content: &str) -> JsValue {
    object([
        ("role", role.into()),
        (
            "content",
            JsValue::Array(vec![object([
                ("type", "text".into()),
                ("text", content.into()),
            ])]),
        ),
    ])
}

fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    let mut value = JsObject::new();
    for (key, field) in fields {
        value.insert(key.into(), field);
    }
    JsValue::Object(value)
}

fn text(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::{Request, Response};
    use std::sync::Mutex;

    struct ScriptedTransport {
        request: Mutex<Option<Request<Full<Bytes>>>>,
        response: &'static str,
    }

    impl ModelHttpTransport for ScriptedTransport {
        fn send(
            &self,
            request: Request<Full<Bytes>>,
        ) -> crate::runtime::model::http::TransportFuture {
            *self.request.lock().unwrap() = Some(request);
            let body = Full::new(Bytes::from_static(self.response.as_bytes()))
                .map_err(|never| match never {})
                .boxed();
            crate::runtime::model::http::TransportFuture::ready(Ok(Response::builder()
                .status(200)
                .header("content-type", "text/event-stream")
                .body(body)
                .unwrap()))
        }
    }

    fn snapshot(memory_model: Option<&str>) -> JsValue {
        let memory_model = memory_model
            .map(|model| format!(r#", "model":"{model}""#))
            .unwrap_or_default();
        parse_json(&format!(
            r#"{{"config":{{"agent":{{"model":{{"id":"agent/base","provider":"agent","model":"base"}}}},"memory":{{"enabled":true,"provider":"edgeclaw"{memory_model}}},"model":{{"providers":{{"memory":{{"id":"memory","protocol":"openai","url":"http://memory.invalid/v1","apiKey":"fixture","headers":{{}},"models":{{"recall":{{"id":"recall","capabilities":{{"supportsStreaming":true,"maxOutputTokens":2048}},"multimodal":{{"input":["text"]}}}}}}}},"agent":{{"id":"agent","protocol":"openai","url":"http://agent.invalid/v1","apiKey":"fixture","headers":{{}},"models":{{"base":{{"id":"base","capabilities":{{"supportsStreaming":true,"maxOutputTokens":2048}},"multimodal":{{"input":["text"]}}}}}}}}}}}}}}}}"#
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn memory_model_override_uses_existing_stream_transport_and_parses_json() {
        let transport = Arc::new(ScriptedTransport {
            request: Mutex::new(None),
            response: "data: {\"id\":\"memory\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"{\\\"route\\\":\\\"project\\\",\\\"selectedIds\\\":[\\\"facts.md\\\"]}\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        });
        let llm = native_llm_from_snapshot_with_transport(
            &snapshot(Some("memory/recall")),
            transport.clone(),
        )
        .unwrap();
        let result = llm
            .execute(
                "memory_retrieval_plan",
                object([("query", "remember this".into())]),
            )
            .await
            .unwrap();
        assert_eq!(result.get("route"), Some(&"project".into()));
        assert_eq!(
            result.get("selectedIds"),
            Some(&JsValue::Array(vec!["facts.md".into()]))
        );
        let request = transport.request.lock().unwrap().take().unwrap();
        assert_eq!(
            request.uri().to_string(),
            "http://memory.invalid/v1/chat/completions"
        );
        let body = request.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["model"], "recall");
        assert!(
            body["messages"][1]["content"]
                .as_str()
                .unwrap()
                .contains("memory_retrieval_plan")
        );
    }

    #[test]
    fn missing_memory_model_falls_back_to_agent_model_and_invalid_refs_disable_llm() {
        let transport: Arc<dyn ModelHttpTransport> = Arc::new(ScriptedTransport {
            request: Mutex::new(None),
            response: "",
        });
        assert!(native_llm_from_snapshot_with_transport(&snapshot(None), transport).is_some());

        let invalid = parse_json(
            r#"{"config":{"agent":{"model":{"id":"invalid"}},"memory":{"enabled":true},"model":{"providers":{}}}}"#,
        )
        .unwrap();
        assert!(native_llm_from_snapshot(&invalid).is_none());
    }

    #[test]
    fn structured_response_accepts_json_fences_but_rejects_non_objects() {
        assert_eq!(
            parse_structured_response("```json\n{\"candidates\":[]}\n```").unwrap(),
            parse_json(r#"{"candidates":[]}"#).unwrap()
        );
        assert!(
            parse_structured_response("[]")
                .unwrap_err()
                .contains("JSON object")
        );
    }

    #[test]
    fn general_recall_project_operation_requires_an_optional_shortlist_selection() {
        let request = request(
            "memory_recall_project",
            object([("shortlist", JsValue::Array(Vec::new()))]),
            "memory".into(),
            "recall".into(),
        )
        .unwrap();
        let prompt = request
            .get("messages")
            .and_then(|messages| match messages {
                JsValue::Array(messages) => messages.first(),
                _ => None,
            })
            .and_then(|message| message.get("content"))
            .and_then(|content| match content {
                JsValue::Array(content) => content.first(),
                _ => None,
            })
            .and_then(|block| block.get("text"))
            .and_then(JsValue::as_string)
            .map(|value| String::from_utf16_lossy(value.units()))
            .unwrap();
        assert!(prompt.contains("selected_project_id"));
        assert!(prompt.contains("empty selected_project_id"));
    }

    #[test]
    fn general_index_project_operation_requires_attach_or_create_decision() {
        let request = request(
            "memory_index_project",
            object([
                ("candidate", object([("name", "release-plan".into())])),
                ("shortlist", JsValue::Array(Vec::new())),
            ]),
            "fixture".into(),
            "model".into(),
        )
        .unwrap();
        let encoded = stringify_json(&request).unwrap().unwrap();
        assert!(encoded.contains("attach_existing"));
        assert!(encoded.contains("create_new"));
        assert!(encoded.contains("General-local"));
    }
}

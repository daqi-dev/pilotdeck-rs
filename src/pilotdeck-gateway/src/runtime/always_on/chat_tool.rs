//! Discovery-only chat history tool over the existing bounded transcript reader.
use std::{collections::HashMap, future::Future, pin::Pin, sync::{Arc, OnceLock}};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use crate::runtime::{history::HistoryFiles, tools::{ToolError, ToolFuture, ToolOutput, ToolRunContext}};
use super::{storage, tools::PhaseToolHandler};
static CHAT_SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

pub(crate) trait ChatCatalog: Send + Sync {
    fn summary<'a>(&'a self, session: &'a JsString)
        -> Pin<Box<dyn Future<Output = Result<Option<JsString>, ToolError>> + Send + 'a>>;
}
pub(crate) struct ChatHandler {
    pub run_id: JsString,
    pub files: HistoryFiles,
    pub aliases: HashMap<JsString, JsString>,
    pub catalog: Arc<dyn ChatCatalog>,
    /// Host supplies the stricter of its sidecar and gateway frame limits.
    pub max_frame_bytes: usize,
}
fn whitespace(c: u16) -> bool {
    matches!(c, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a
        | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn trimmed(v: &[u16]) -> &[u16] {
    let a = v.iter().position(|c| !whitespace(*c)).unwrap_or(v.len());
    let b = v.iter().rposition(|c| !whitespace(*c)).map_or(a, |i| i + 1);
    &v[a..b]
}
fn error_for(id: &JsString, reason: &str) -> ToolError {
    let mut text = "Could not read session ".encode_utf16().collect::<Vec<_>>();
    text.extend_from_slice(id.units()); text.extend(": ".encode_utf16()); text.extend(reason.encode_utf16());
    ToolError::failed(JsString::from_utf16(text))
}
impl PhaseToolHandler for ChatHandler {
    fn execute(&self, name: &'static str, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        if name != "always_on_read_chat_history" {
            return Box::pin(async move { Err(ToolError::failed(format!("{name} is not available in this Always-On phase").as_str())) });
        }
        let Some(requested) = input.get("sessionId").and_then(JsValue::as_string).cloned() else {
            return Box::pin(async { Err(ToolError::new("invalid_input", "sessionId must be a string")) });
        };
        let real = self.aliases.get(&requested).cloned().unwrap_or_else(|| requested.clone());
        let files = self.files.clone(); let catalog = self.catalog.clone(); let run = self.run_id.clone();
        let frame_limit = self.max_frame_bytes;
        Box::pin(async move {
            if context.is_aborted() { return Err(ToolError::aborted()); }
            if frame_limit <= 4096 { return Err(ToolError::failed("Chat history output exceeds the sidecar frame budget")); }
            let permit = CHAT_SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4))).clone()
                .acquire_owned().await.map_err(|_| ToolError::failed("Chat history is unavailable"))?;
            let permit = Arc::new(permit);
            let snapshot = files.snapshot(&real).await.map_err(|e| error_for(&requested, &e))?;
            if snapshot.read.entries.is_empty() {
                let reason = snapshot.read.diagnostics.first().map(|d| d.message.as_str()).unwrap_or("No transcript entries found.");
                return Err(error_for(&requested, reason));
            }
            let entries = snapshot.read.entries;
            let projection_permit = permit.clone();
            let replay = storage::run(move || {
                let _permit = projection_permit;
                crate::runtime::session::project_transcript(&entries)
                    .map_err(|e| std::io::Error::other(e.to_string()))
            }).await
                .map_err(|e| ToolError::failed(e.to_string().as_str()))?;
            let catalog_title = catalog.summary(&real).await?;
            let title = ["title", "aiTitle"].into_iter().find_map(|k| {
                replay.metadata.get(k).filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                    .and_then(JsValue::as_string).cloned()
            }).or(catalog_title).unwrap_or_else(|| real.clone());
            // Two JSON copies; any UTF-16 unit may need a six-byte escape.
            // Leave room for the surrounding sidecar protocol frame.
            let mut remaining = frame_limit.saturating_sub(4096) / 12;
            let header = title.len_utf16().saturating_add(real.len_utf16()).saturating_add(run.len_utf16());
            remaining = remaining.checked_sub(header).ok_or_else(|| ToolError::failed("Chat history output exceeds the sidecar frame budget"))?;
            let mut conversation = Vec::new();
            for message in &replay.messages {
                let Some(role) = message.get("role").and_then(JsValue::as_string) else { continue; };
                if !role.equals_str("user") && !role.equals_str("assistant") { continue; }
                let Some(JsValue::Array(blocks)) = message.get("content") else { continue; };
                let mut text = Vec::new();
                for block in blocks {
                    if !block.get("type").and_then(JsValue::as_string).is_some_and(|t| t.equals_str("text")) { continue; }
                    let Some(value) = block.get("text").and_then(JsValue::as_string) else { continue; };
                    let item = trimmed(value.units()); if item.is_empty() { continue; }
                    let separator = if text.is_empty() { 0 } else { 2 };
                    let cap = if role.equals_str("assistant") { 301 } else { remaining };
                    if !role.equals_str("assistant") && text.len().saturating_add(separator).saturating_add(item.len()) > cap {
                        return Err(ToolError::failed("Chat history output exceeds the sidecar frame budget"));
                    }
                    if separator > 0 && text.len() < cap { text.extend([10,10].into_iter().take(cap-text.len())); }
                    let available = cap.saturating_sub(text.len());
                    text.extend_from_slice(&item[..item.len().min(available)]);
                }
                if text.is_empty() { continue; }
                if role.equals_str("assistant") && text.len() > 300 { text.truncate(300); text.extend("...".encode_utf16()); }
                remaining = remaining.checked_sub(text.len().saturating_add(64))
                    .ok_or_else(|| ToolError::failed("Chat history output exceeds the sidecar frame budget"))?;
                let mut entry = JsObject::new(); entry.insert("role".into(), JsValue::String(role.clone()));
                entry.insert("text".into(), JsValue::String(JsString::from_utf16(text)));
                entry.insert("createdAt".into(), "".into()); conversation.push(JsValue::Object(entry));
            }
            let count = conversation.len(); let mut data = JsObject::new();
            data.insert("sessionId".into(), JsValue::String(real)); data.insert("title".into(), JsValue::String(title.clone()));
            data.insert("messageCount".into(), JsValue::Number(count as f64));
            data.insert("conversation".into(), JsValue::Array(conversation));
            let mut text = "Session \"".encode_utf16().collect::<Vec<_>>(); text.extend_from_slice(title.units());
            text.extend(format!("\" — {count} messages.").encode_utf16());
            let mut text_block = JsObject::new(); text_block.insert("type".into(), "text".into());
            text_block.insert("text".into(), JsValue::String(JsString::from_utf16(text)));
            let mut json = JsObject::new(); json.insert("type".into(), "json".into()); json.insert("value".into(), JsValue::Object(data.clone()));
            let mut metadata = JsObject::new(); metadata.insert("runId".into(), JsValue::String(run));
            Ok(ToolOutput { content: vec![JsValue::Object(text_block), JsValue::Object(json)], data: Some(JsValue::Object(data)),
                metadata: Some(metadata), ..Default::default() })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext};
    struct Catalog;
    impl ChatCatalog for Catalog {
        fn summary<'a>(&'a self, _: &'a JsString) -> Pin<Box<dyn Future<Output = Result<Option<JsString>, ToolError>> + Send + 'a>> {
            Box::pin(async { Ok(Some("catalog title".into())) })
        }
    }
    fn context() -> ToolRunContext {
        ToolRunContext::new(Arc::new(ToolRuntimeContext {
            fields: JsObject::new(), files: HostToolCheckpoint::default().tool_context_state(),
            abort_signal: None, model: None, elicitation: None, user_dialog: None,
            audit_recorder: None, file_history: None, file_update_notifier: None,
            now: None, progress: None, subagent: None, plan_todo: None,
            goal: None, plan_directory: None,
        }))
    }
    #[tokio::test]
    async fn actual_bounded_transcript_replay_matches_typescript_chat_tool() {
        let root = std::env::temp_dir().join(format!("pdrs-always-on-chat-{}", uuid::Uuid::new_v4()));
        let home = root.join("home"); let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let metadata = crate::metadata::Metadata { home: home.clone(), cwd: Default::default(), files: crate::metadata::NativeFiles };
        let chat = metadata.process_chat_dir(&project.to_string_lossy()).unwrap();
        std::fs::create_dir_all(&chat).unwrap();
        let fixture = pilotdeck_protocol::parse_json(include_str!("../../../tests/fixtures/always-on-cli/chat-oracle.json")).unwrap();
        let JsValue::Array(cases) = fixture.get("records").unwrap() else { panic!("cases") };
        let JsValue::Array(entries) = cases[0].get("entries").unwrap() else { panic!("entries") };
        let mut lines = String::new();
        for (index, item) in entries.iter().enumerate() {
            let mut entry = item.as_object().unwrap().clone();
            entry.insert("sequence".into(), JsValue::Number((index + 1) as f64));
            entry.insert("entryId".into(), format!("e-{index}").as_str().into());
            entry.insert("createdAt".into(), "2026-01-02T03:04:05.678Z".into());
            lines.push_str(&pilotdeck_protocol::stringify_json(&JsValue::Object(entry)).unwrap().unwrap());
            lines.push('\n');
        }
        std::fs::write(chat.join(format!("{}.jsonl", crate::storage::sanitize_session_id(&"real".into()))), lines).unwrap();
        let handler = ChatHandler { run_id: "run".into(), files: HistoryFiles { home, project },
            aliases: HashMap::from([("alias".into(), "real".into())]), catalog: Arc::new(Catalog),
            max_frame_bytes: 16 * 1024 * 1024 };
        let mut input = JsObject::new(); input.insert("sessionId".into(), "alias".into());
        let result = handler.execute("always_on_read_chat_history", JsValue::Object(input), context()).await.unwrap();
        assert_eq!(result.data.as_ref(), cases[3].get("result").and_then(|v| v.get("data")));
        let small = ChatHandler { max_frame_bytes: 4096, ..handler };
        let mut input = JsObject::new(); input.insert("sessionId".into(), "alias".into());
        assert!(small.execute("always_on_read_chat_history", JsValue::Object(input), context()).await.is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}

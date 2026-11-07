//! Rebuildable product history/index cache. Kernel history is the sole source;
//! these records never assert execution acceptance, terminal outcome or resume.
use super::factory::SessionProjection;
use crate::{runtime::project::RuntimeError, storage::recorder::Recorder};
use pilotdeck_protocol::{JsValue, parse_json};
use pilotdeck_transport::{server::ExecutionClock, system_clock::SystemExecutionClock};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

pub(crate) struct KernelProjection {
    recorder: Arc<Recorder>,
    projection: Arc<SessionProjection>,
    session: String,
    kernel: String,
    context: String,
    serial: Mutex<()>,
}
fn error(e: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::new(format!("Kernel product projection unavailable: {e}"))
}
fn to_json(v: &JsValue) -> Result<Value, RuntimeError> {
    super::kernel::json_value(v)
}
fn digest(v: &Value) -> String {
    format!("{:x}", Sha256::digest(v.to_string().as_bytes()))
}
fn normalize(message: &Value) -> Value {
    let mut message = message.clone();
    if let Some(text) = message["content"].as_str() {
        message["content"] = json!([{"type":"text","text":text}]);
    }
    message
}
impl KernelProjection {
    pub(crate) fn new(
        recorder: Arc<Recorder>,
        projection: Arc<SessionProjection>,
        session: String,
        kernel: String,
        context: String,
    ) -> Self {
        Self {
            recorder,
            projection,
            session,
            kernel,
            context,
            serial: Mutex::new(()),
        }
    }
    pub(crate) fn sync(&self, snapshot: &Value) -> Result<(), RuntimeError> {
        let _guard = self.serial.lock().map_err(error)?;
        let revision = snapshot["revision"]
            .as_u64()
            .ok_or_else(|| error("history revision is missing"))?;
        let history = match &snapshot["history"] {
            Value::Array(messages) => messages.clone(),
            Value::Null => Vec::new(),
            _ => return Err(error("history must be canonical messages")),
        };
        let sha = digest(&json!(history));
        let start = self.recorder.writer().snapshot_state().sequence;
        let read = crate::storage::read_transcript(self.recorder.writer().path()).map_err(error)?;
        let previous = read
            .entries
            .iter()
            .rev()
            .find_map(|entry| entry.get("kernelProjection"))
            .map(to_json)
            .transpose()?;
        if let Some(previous) = previous {
            if previous["kernelSessionId"] != self.kernel {
                return Err(error("cache owner mismatch"));
            }
            if previous["revision"]
                .as_u64()
                .is_some_and(|old| old > revision)
            {
                return Err(error("Kernel history revision went backwards"));
            }
            if previous["revision"] == revision {
                if previous["sha256"] != sha {
                    return Err(error("same Kernel revision has different history"));
                }
                return Ok(());
            }
        }
        let prior = self.projection.snapshot().map_err(error)?;
        let mut metadata = to_json(&JsValue::Object(prior.metadata))?;
        metadata
            .as_object_mut()
            .ok_or_else(|| error("invalid UI metadata"))?;
        let marker = json!({"kernelSessionId":self.kernel,"contextRef":self.context,"revision":revision,"sha256":sha});
        metadata["kernelProjection"] = marker.clone();
        let prompts = history
            .iter()
            .filter(|message| message["role"] == "user" && message["metadata"]["synthetic"] != true)
            .map(normalize)
            .map(|message| content_text(&message["content"]))
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>();
        if let Some(first) = prompts.first() {
            metadata["firstPrompt"] = json!(first);
        }
        if let Some(last) = prompts.last() {
            metadata["lastPrompt"] = json!(last);
        }

        let created = read
            .entries
            .first()
            .and_then(|entry| entry.get("createdAt"))
            .and_then(JsValue::as_string)
            .map(|s| String::from_utf16_lossy(s.units()))
            .unwrap_or_else(|| String::from_utf16_lossy(SystemExecutionClock.now_iso().units()));
        let mut records = Vec::new();
        let mut parent = Value::Null;
        for (index, message) in history.iter().enumerate() {
            let message = normalize(message);
            let id = format!("kernel-cache:{revision}:{}:{index}", &sha[..16]);
            let mut record = json!({"type":if message["role"]=="user"{"accepted_input"}else{"durable_message"},"sessionId":self.session,"turnId":format!("kernel-history:{revision}"),"sequence":start+index as u64+1,"entryId":id,"parentEntryId":parent,"createdAt":created,"kernelProjection":marker});
            if message["role"] == "user" {
                record["messages"] = json!([message]);
            } else {
                record["message"] = message;
            }
            records.push(parse_json(&record.to_string()).map_err(error)?);
            parent = json!(id);
        }
        let record = json!({"type":"session_metadata","sessionId":self.session,"turnId":format!("kernel-history:{revision}"),"sequence":start+records.len() as u64+1,"entryId":format!("kernel-cache:{revision}:{}:metadata",&sha[..16]),"parentEntryId":parent,"createdAt":created,"kernelProjection":marker,"metadata":metadata});
        records.push(parse_json(&record.to_string()).map_err(error)?);
        self.recorder
            .writer()
            .replace_kernel_projection(&self.kernel, start, &records)
            .map_err(error)
    }
}

/// Backfill only display identity for pre-annotation native histories. An
/// assistant block must be observed under exactly one original root. A user
/// row is paired only through those block anchors plus that root's exact input.
/// Repeated equal prompts are never matched globally by text.
pub(crate) fn reconcile_identity(history: &Value, journal: &Value, roots: &[Value]) -> Value {
    let mut blocks: HashMap<String, Option<String>> = HashMap::new();
    fn ids(value: &Value, out: &mut HashSet<String>) {
        match value {
            Value::Object(object) => {
                if let Some(id) = object.get("blockId").and_then(Value::as_str) {
                    out.insert(id.into());
                }
                for child in object.values() {
                    ids(child, out);
                }
            }
            Value::Array(values) => {
                for child in values {
                    ids(child, out);
                }
            }
            _ => {}
        }
    }
    for event in journal.as_array().into_iter().flatten() {
        let Some(root) = roots.iter().find(|root| root["id"] == event["call_ref"]) else {
            continue;
        };
        let Some(run) = root["invocation"]["correlation_id"].as_str() else {
            continue;
        };
        let Some(agent) = event["payload"]["content"].get("agent_event") else {
            continue;
        };
        let mut found = HashSet::new();
        ids(agent, &mut found);
        for id in found {
            blocks
                .entry(id)
                .and_modify(|existing| {
                    if existing.as_deref() != Some(run) {
                        *existing = None;
                    }
                })
                .or_insert_with(|| Some(run.into()));
        }
    }
    let Some(original) = history.as_array() else {
        return history.clone();
    };
    let mut messages = original.clone();
    for message in &mut messages {
        if message["metadata"]["turnId"].is_string() {
            continue;
        }
        let mut found = HashSet::new();
        ids(&message["content"], &mut found);
        let runs = found
            .iter()
            .filter_map(|id| blocks.get(id).and_then(Option::as_ref))
            .collect::<HashSet<_>>();
        if runs.len() == 1 {
            let run = (*runs.iter().next().unwrap()).clone();
            if !message["metadata"].is_object() {
                message["metadata"] = json!({});
            }
            message["metadata"]["turnId"] = json!(run);
        }
    }
    let prompts = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| {
            message["role"] == "user"
                && message["content"].as_array().is_some_and(|content| {
                    content.iter().any(|block| block["type"] == "text")
                        && !content.iter().any(|block| block["type"] == "tool_result")
                })
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    for (ordinal, start) in prompts.iter().copied().enumerate() {
        if messages[start]["metadata"]["turnId"].is_string() {
            continue;
        }
        let end = prompts.get(ordinal + 1).copied().unwrap_or(messages.len());
        let runs = messages[start + 1..end]
            .iter()
            .filter_map(|message| message["metadata"]["turnId"].as_str())
            .collect::<HashSet<_>>();
        if runs.len() != 1 {
            continue;
        }
        let run = *runs.iter().next().unwrap();
        let text = content_text(&messages[start]["content"]);
        if roots.iter().any(|root| {
            root["invocation"]["correlation_id"] == run
                && root["invocation"]["payload"]["type"] == "text"
                && root["invocation"]["payload"]["text"] == text
        }) {
            let run = run.to_string();
            if !messages[start]["metadata"].is_object() {
                messages[start]["metadata"] = json!({});
            }
            messages[start]["metadata"]["turnId"] = json!(run);
        }
    }
    json!(messages)
}

/// The retained UI consumes WebMessage rows, not canonical model messages.
/// IDs are content-based and stable across cache deletion/reconstruction.
pub(crate) fn web_messages(
    history: &Value,
    session: &str,
    project: Option<&str>,
    params: &Value,
) -> Value {
    let mut rows = Vec::new();
    for (index, message) in history.as_array().into_iter().flatten().enumerate() {
        let message = normalize(message);
        let blocks = message["content"].as_array().cloned().unwrap_or_default();
        let role = message["role"].as_str().unwrap_or("system");
        let id = message["metadata"]["kernelMessageId"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("kernel-history:{index}:{}", &digest(&message)[..16]));
        let created = message["createdAt"]
            .as_str()
            .or_else(|| message["metadata"]["createdAt"].as_str())
            .unwrap_or("1970-01-01T00:00:00.000Z");
        for (part, block) in blocks.iter().enumerate() {
            let kind = block["type"].as_str().unwrap_or("unknown");
            let mut row = json!({"id":format!("{id}:{part}"),"sessionKey":session,"createdAt":created,"provider":"pilotdeck","role":role,"source":"history","sequence":rows.len()+1});
            if let Some(project) = project {
                row["projectKey"] = json!(project);
            }
            if let Some(run) = message["metadata"]["turnId"].as_str() {
                row["turnId"] = json!(run);
            }
            if block["timeline"].is_object() {
                row["timeline"] = block["timeline"].clone();
            }
            if let Some(model) = message["metadata"]["model"].as_str() {
                row["model"] = json!(model);
            }
            if let Some(block_id) = block
                .get("blockId")
                .or_else(|| block.get("id"))
                .and_then(Value::as_str)
                .filter(|_| kind != "tool_call" && kind != "tool_use")
            {
                row["blockId"] = json!(block_id);
            }
            match kind {
                "text" => {
                    row["kind"] = json!("text");
                    row["text"] = block["text"].clone();
                }
                "thinking" => {
                    row["kind"] = json!("thinking");
                    row["text"] = block
                        .get("text")
                        .or_else(|| block.get("reasoningContent"))
                        .or_else(|| block.get("thinking"))
                        .cloned()
                        .unwrap_or(json!(""));
                }
                "tool_call" | "tool_use" => {
                    row["kind"] = json!("tool_use");
                    row["toolCallId"] = block["id"].clone();
                    row["toolName"] = block["name"].clone();
                    row["payload"] = block["input"].clone();
                }
                "tool_result" => {
                    row["kind"] = json!("tool_result");
                    row["role"] = json!("tool");
                    row["toolCallId"] = block
                        .get("toolCallId")
                        .or_else(|| block.get("tool_use_id"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    row["ok"] = json!(
                        block
                            .get("isError")
                            .or_else(|| block.get("is_error"))
                            .and_then(Value::as_bool)
                            != Some(true)
                    );
                    row["payload"] = block["content"].clone();
                    row["text"] = json!(content_text(&block["content"]));
                }
                "image" => {
                    row["kind"] = json!("text");
                    row["text"] = json!("");
                    row["images"] = json!([{"data":block["data"],"mimeType":block["mimeType"]}]);
                }
                "pdf" | "audio" | "document" => {
                    row["kind"] = json!("status");
                    row["text"] = json!(format!("[{kind} attachment]"));
                    row["payload"] = block.clone();
                }
                _ => {
                    row["kind"] = json!("status");
                    row["text"] = json!(format!("[{kind}]"));
                    row["payload"] = block.clone();
                }
            }
            rows.push(row);
        }
    }
    let total = rows.len();
    let offset = params["cursor"]
        .as_str()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0)
        .min(total);
    let limit = params["limit"]
        .as_u64()
        .filter(|n| *n > 0)
        .map(|n| n as usize)
        .unwrap_or(total);
    let end = offset.saturating_add(limit).min(total);
    let mut result = json!({"messages":rows[offset..end],"total":total,"backend":"kernel"});
    if end < total {
        result["nextCursor"] = json!(end.to_string());
    }
    result
}
fn content_text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kernel_history_live_identity_uses_root_and_block_not_equal_text() {
        let history = json!([{"role":"user","content":[{"type":"text","text":"same"}]},{"role":"assistant","content":[{"type":"text","text":"same answer","blockId":"a","timeline":{"id":"a","turnId":"internal-a"}}]},{"role":"user","content":[{"type":"text","text":"same"}]},{"role":"assistant","content":[{"type":"text","text":"same answer","blockId":"b"}]}]);
        let roots = vec![
            json!({"id":"root-a","invocation":{"correlation_id":"ui-a","payload":{"type":"text","text":"same"}}}),
            json!({"id":"root-b","invocation":{"correlation_id":"ui-b","payload":{"type":"text","text":"same"}}}),
        ];
        let journal = json!([{"call_ref":"root-a","payload":{"content":{"agent_event":{"event":{"blockId":"a"}}}}},{"call_ref":"root-b","payload":{"content":{"agent_event":{"event":{"blockId":"b"}}}}}]);
        let display = reconcile_identity(&history, &journal, &roots);
        let rows = web_messages(&display, "s", None, &json!({}));
        assert_eq!(rows["messages"][0]["turnId"], "ui-a");
        assert_eq!(rows["messages"][1]["turnId"], "ui-a");
        assert_eq!(rows["messages"][1]["blockId"], "a");
        assert_eq!(rows["messages"][1]["timeline"]["turnId"], "internal-a");
        assert_eq!(rows["messages"][2]["turnId"], "ui-b");
        assert_eq!(rows["messages"][3]["turnId"], "ui-b");
        assert!(history[0]["metadata"].is_null());
        let annotated = json!([{"role":"user","metadata":{"turnId":"original","kernelMessageId":"native:call:0","createdAt":"2026-11-08T01:39:00.000Z"},"content":[{"type":"text","text":"same"}]}]);
        let rows = web_messages(&annotated, "s", None, &json!({}));
        assert_eq!(rows["messages"][0]["id"], "native:call:0:0");
        assert_eq!(rows["messages"][0]["createdAt"], "2026-11-08T01:39:00.000Z");
    }
    #[test]
    fn kernel_ui_cache_is_atomic_idempotent_rebuildable_and_has_no_execution_outcomes() {
        use crate::storage::{JsonlTranscriptStore, TranscriptKey};
        let root =
            std::env::temp_dir().join(format!("pd-kernel-projection-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("session.jsonl");
        std::fs::write(
            root.join("session.jsonl.backend.json"),
            json!({"backend":"kernel","kernel_session_id":"kernel"}).to_string(),
        )
        .unwrap();
        let key = TranscriptKey {
            project_root: root.clone(),
            pilot_home: root.clone(),
            session_id: "session".into(),
            transcript_path: path.clone(),
        };
        let recorder = Arc::new(Recorder::new(Arc::new(JsonlTranscriptStore::new()), key));
        let cache = KernelProjection::new(
            recorder,
            Arc::new(SessionProjection::default()),
            "session".into(),
            "kernel".into(),
            "default".into(),
        );
        let snapshot = json!({"revision":1,"history":[{"role":"user","content":[{"type":"text","text":"original prompt"}]},{"role":"assistant","content":[{"type":"text","text":"original answer"}]}]});
        cache.sync(&snapshot).unwrap();
        let first = std::fs::read(&path).unwrap();
        cache.sync(&snapshot).unwrap();
        assert_eq!(first, std::fs::read(&path).unwrap());
        let read = crate::storage::read_transcript(&path).unwrap();
        assert_eq!(
            super::super::factory::project(&read.entries)
                .unwrap()
                .messages
                .len(),
            2
        );
        for record in &read.entries {
            assert!(!["turn_started", "turn_result"].iter().any(|kind| {
                record
                    .get("type")
                    .and_then(JsValue::as_string)
                    .is_some_and(|s| s.equals_str(kind))
            }));
        }
        use crate::runtime::manager::{
            SessionCatalogListInput, SessionCatalogPort, TranscriptSessionCatalog,
        };
        let dir = root.clone();
        let adapter = Arc::new(crate::storage::AsyncTranscriptAdapter::new(
            Arc::new(JsonlTranscriptStore::new()),
            Arc::new(move |_| dir.clone()),
        ));
        let listed = TranscriptSessionCatalog::new(adapter)
            .list(&SessionCatalogListInput::new(root.clone(), root.clone()))
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0]
                .get("summary")
                .and_then(JsValue::as_string)
                .map(|s| String::from_utf16_lossy(s.units())),
            Some("original prompt".into())
        );
        std::fs::remove_file(&path).unwrap();
        cache.sync(&snapshot).unwrap();
        assert_eq!(
            crate::storage::read_transcript(&path)
                .unwrap()
                .entries
                .len(),
            3
        );
        assert!(cache.sync(&json!({"revision":1,"history":[]})).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn kernel_web_history_projects_canonical_tools_thinking_and_pagination() {
        let history = json!([{"role":"user","content":[{"type":"text","text":"hello"}]},{"role":"assistant","metadata":{"model":"actual"},"content":[{"type":"thinking","text":"reason"},{"type":"tool_call","id":"call","name":"Read","input":{"path":"x"}}]},{"role":"tool","content":[{"type":"tool_result","toolCallId":"call","content":[{"type":"text","text":"result"}]}]}]);
        let all = web_messages(&history, "s", Some("/p"), &json!({}));
        assert_eq!(all["total"], 4);
        assert_eq!(all["messages"][1]["kind"], "thinking");
        assert_eq!(all["messages"][2]["toolCallId"], "call");
        assert_eq!(all["messages"][3]["text"], "result");
        let page = web_messages(&history, "s", Some("/p"), &json!({"cursor":"1","limit":2}));
        assert_eq!(page["nextCursor"], "3");
        assert_eq!(page["messages"][0]["id"], all["messages"][1]["id"]);
    }
}

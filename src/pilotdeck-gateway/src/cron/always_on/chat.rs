//! Native catalog/digest read side. Reuses the gateway transcript validator.
use super::{AlwaysOnError, now};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

fn chat_dir(home: &Path, project: &Path) -> PathBuf {
    crate::session_chat_dir(home, project, &project.to_string_lossy())
}
fn clip(text: &str, limit: usize) -> String {
    if text.encode_utf16().count() <= limit {
        return text.into();
    }
    let mut units = 0;
    let prefix: String = text
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= limit
        })
        .collect();
    format!("{prefix}...")
}
fn text_blocks(message: &Value) -> Vec<&str> {
    message["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}
fn lite(path: &Path) -> std::io::Result<(String, String, fs::Metadata)> {
    let mut file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    let size = metadata.len().min(65_536) as usize;
    let mut head = vec![0; size];
    file.read_exact(&mut head)?;
    file.seek(SeekFrom::Start(metadata.len().saturating_sub(65_536)))?;
    let mut tail = vec![0; size];
    file.read_exact(&mut tail)?;
    Ok((
        String::from_utf8_lossy(&head).into_owned(),
        String::from_utf8_lossy(&tail).into_owned(),
        metadata,
    ))
}
fn entries(source: &str) -> impl Iterator<Item = Value> + '_ {
    source
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
}
fn merge_metadata(target: &mut Value, entry: &Value) {
    if entry["type"] != "session_metadata" {
        return;
    }
    for (key, value) in entry["metadata"].as_object().into_iter().flatten() {
        if key == "title" && value.is_null() {
            continue;
        }
        target[key] = value.clone();
    }
}
fn all_metadata(path: &Path) -> std::io::Result<Value> {
    let mut reader = BufReader::with_capacity(65_536, fs::File::open(path)?);
    let mut line = Vec::new();
    let mut oversized = false;
    let mut metadata = json!({});
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            if !oversized && let Ok(entry) = serde_json::from_slice::<Value>(&line) {
                merge_metadata(&mut metadata, &entry);
            }
            break;
        }
        let n = buffer
            .iter()
            .position(|b| *b == b'\n')
            .map_or(buffer.len(), |i| i + 1);
        let newline = buffer[n - 1] == b'\n';
        if !oversized {
            if line.len() + n <= 131_072 {
                line.extend_from_slice(&buffer[..n]);
            } else {
                oversized = true;
                line.clear();
            }
        }
        reader.consume(n);
        if newline {
            if !oversized && let Ok(entry) = serde_json::from_slice::<Value>(&line) {
                merge_metadata(&mut metadata, &entry);
            }
            line.clear();
            oversized = false;
        }
    }
    Ok(metadata)
}
fn title(metadata: &Value) -> Option<&str> {
    ["title", "aiTitle", "lastPrompt", "firstPrompt"]
        .into_iter()
        .filter_map(|key| metadata[key].as_str())
        .map(str::trim)
        .find(|s| !s.is_empty())
}

pub fn digest(home: &Path, project: &Path) -> Result<Value, AlwaysOnError> {
    let directory = chat_dir(home, project);
    let files = match fs::read_dir(&directory) {
        Ok(files) => files,
        Err(_) => return Ok(json!({"generatedAt":now(),"sessions":[],"aliasMap":{}})),
    };
    let mut catalog = Vec::new();
    for entry in files.flatten() {
        let path = entry.path();
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        if [
            "always-on-discovery:",
            "always-on-discovery-",
            "always-on-workspace:",
            "always-on-workspace-",
            "always-on-report:",
            "always-on-report-",
            "always-on-execute:",
        ]
        .iter()
        .any(|p| id.starts_with(p))
        {
            continue;
        }
        let Ok((head, tail, stat)) = lite(&path) else {
            continue;
        };
        if stat.len() == 0 {
            continue;
        }
        let source = format!("{head}\n{tail}");
        let mut metadata = json!({});
        for entry in entries(&source) {
            merge_metadata(&mut metadata, &entry);
        }
        if stat.len() > 65_536
            && let Ok(full) = all_metadata(&path)
        {
            for (k, v) in full.as_object().into_iter().flatten() {
                metadata[k] = v.clone();
            }
        }
        let prompts: Vec<String> = entries(&source)
            .filter(|e| e["type"] == "accepted_input")
            .flat_map(|e| e["messages"].as_array().cloned().unwrap_or_default())
            .flat_map(|m| {
                text_blocks(&m)
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect();
        let summary = title(&metadata)
            .map(str::to_owned)
            .or_else(|| prompts.last().cloned());
        let Some(summary) = summary else {
            continue;
        };
        let ms = stat
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as f64;
        catalog.push((ms, id.to_owned(), summary, prompts));
    }
    catalog.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut sessions = Vec::new();
    let mut aliases = json!({});
    for (ms, id, title, prompts) in catalog.into_iter().take(10) {
        let mut seen = HashSet::new();
        let prompts: Vec<String> = prompts
            .into_iter()
            .filter(|p| seen.insert(p.clone()))
            .take(8)
            .map(|p| clip(&p, 500))
            .collect();
        if prompts.is_empty() {
            continue;
        }
        let alias = format!("chat_{}", sessions.len() + 1);
        aliases[&alias] = json!(id);
        sessions.push(json!({"sessionId":id,"alias":alias,"title":title,"lastModified":crate::runtime::background::iso(ms).to_utf8().unwrap_or_default(),"userPrompts":prompts}));
    }
    Ok(json!({"generatedAt":now(),"sessions":sessions,"aliasMap":aliases}))
}

pub fn conversation(home: &Path, project: &Path, session: &str) -> Result<Value, AlwaysOnError> {
    let id = crate::storage::sanitize_session_id(&session.into());
    let path = chat_dir(home, project).join(format!("{id}.jsonl"));
    let read = crate::storage::read_transcript(&path)
        .map_err(|e| AlwaysOnError::new("tool_execution_failed", e.to_string()))?;
    if read.entries.is_empty() {
        return Err(AlwaysOnError::new(
            "tool_execution_failed",
            format!(
                "Could not read session {session}: {}",
                read.diagnostics
                    .first()
                    .map(|d| d.message.as_str())
                    .unwrap_or("No transcript entries found.")
            ),
        ));
    }
    let entries = read
        .entries
        .iter()
        .map(|e| {
            let encoded = pilotdeck_protocol::stringify_json(e)
                .map_err(|e| AlwaysOnError::new("storage_corrupt", e.to_string()))?
                .unwrap_or_default();
            serde_json::from_str::<Value>(&encoded).map_err(Into::into)
        })
        .collect::<Result<Vec<_>, AlwaysOnError>>()?;
    let messages = project_conversation(&entries);
    let mut metadata = json!({});
    for entry in &entries {
        merge_metadata(&mut metadata, entry);
    }
    let conversation:Vec<Value>=messages.iter().filter(|m|matches!(m["role"].as_str(),Some("user"|"assistant"))).filter_map(|m| {
        let text=text_blocks(m).join("\n\n");if text.is_empty() {return None;}
        Some(json!({"role":m["role"],"text":if m["role"]=="assistant" {clip(&text,300)} else {text},"createdAt":""}))
    }).collect();
    let fallback = entries
        .iter()
        .filter(|e| e["type"] == "accepted_input")
        .flat_map(|e| e["messages"].as_array().into_iter().flatten())
        .filter_map(|m| text_blocks(m).first().copied())
        .next_back()
        .unwrap_or(session);
    let title = metadata["title"]
        .as_str()
        .or_else(|| metadata["aiTitle"].as_str())
        .unwrap_or(fallback);
    Ok(
        json!({"sessionId":session,"title":title,"messageCount":conversation.len(),"conversation":conversation}),
    )
}

fn valid_snapshot(entry: &Value) -> Option<&Vec<Value>> {
    let b = &entry["boundary"];
    if b["kind"] != "compact" || b["subtype"] != "compact_boundary" || b["snapshot"]["version"] != 1
    {
        return None;
    }
    let messages = b["snapshot"]["messages"]
        .as_array()
        .filter(|m| !m.is_empty())?;
    let mut blocks = Vec::new();
    for message in messages {
        if !matches!(message["role"].as_str(), Some("user" | "assistant"))
            || message.get("metadata").is_some_and(|m| !m.is_object())
        {
            return None;
        }
        for block in message["content"].as_array()? {
            blocks.push((block, false));
        }
    }
    while let Some((block, nested)) = blocks.pop() {
        let kind = block["type"].as_str()?;
        if nested && !matches!(kind, "text" | "image" | "pdf") {
            return None;
        }
        let valid = match kind {
            "text" | "thinking" => block["text"].is_string(),
            "image" | "audio" => {
                matches!(block["source"].as_str(), Some("base64" | "url"))
                    && block["data"].is_string()
                    && block["mimeType"].is_string()
            }
            "pdf" => {
                block["source"] == "base64"
                    && block["data"].is_string()
                    && block["mimeType"] == "application/pdf"
                    && block["bytes"].is_number()
            }
            "tool_call" => block["id"].is_string() && block["name"].is_string(),
            "tool_result" => {
                if !block["toolCallId"].is_string() {
                    return None;
                }
                for child in block["content"].as_array()? {
                    blocks.push((child, true));
                }
                true
            }
            "tool_result_reference" | "media_reference" => {
                block["path"].is_string()
                    && block["originalBytes"].is_number()
                    && block["preview"].is_string()
                    && block["hasMore"].is_boolean()
                    && if kind == "tool_result_reference" {
                        block["toolCallId"].is_string()
                    } else {
                        block["mimeType"].is_string()
                            && matches!(
                                block["mediaType"].as_str(),
                                Some("image" | "pdf" | "audio")
                            )
                    }
            }
            _ => false,
        };
        if !valid {
            return None;
        }
    }
    Some(messages)
}
fn snapshot(entry: &Value) -> Option<&Vec<Value>> {
    let b = &entry["boundary"];
    if b["kind"] != "compact" || b["subtype"] != "compact_boundary" {
        return None;
    }
    valid_snapshot(entry).or_else(|| b["replacementMessages"].as_array())
}
fn real_user(message: &Value) -> bool {
    message["role"]=="user"&&message["metadata"]["synthetic"]!=true&&message["content"].as_array().into_iter().flatten().any(|block| {
        if matches!(block["type"].as_str(),Some("tool_result"|"tool_result_reference")) || (block["type"]=="media_reference"&&block["toolCallId"].is_string()) {return false;}
        if block["type"]!="text" {return true;}
        block["text"].as_str().is_some_and(|text| {
            let text=text.trim();!text.is_empty()&&text!="[system: the conversation above has been compacted. please continue with the current task.]"&&!["<compact-boundary","<snip-boundary","<memory-context>","<internal-compaction-control","<hook_context"].iter().any(|p|text.starts_with(p))
        })
    })
}
fn project_conversation(entries: &[Value]) -> Vec<Value> {
    let completed: HashSet<&str> = entries
        .iter()
        .filter(|e| e["type"] == "turn_result")
        .filter_map(|e| e["turnId"].as_str())
        .collect();
    let boundary = entries
        .iter()
        .enumerate()
        .rev()
        .find(|(_, e)| e["type"] == "control_boundary" && snapshot(e).is_some());
    let mut messages = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        if boundary.is_some_and(|(index, _)| i < index) {
            continue;
        }
        let complete = e["turnId"]
            .as_str()
            .is_some_and(|id| completed.contains(id));
        match e["type"].as_str() {
            Some("accepted_input") => {
                messages.extend(e["messages"].as_array().into_iter().flatten().cloned())
            }
            Some("assistant_message" | "tool_result_message" | "durable_message")
                if complete && e["message"]["metadata"]["compactReplacement"] != true =>
            {
                messages.push(e["message"].clone())
            }
            Some("context_snapshot") if complete => {
                let index = messages
                    .iter()
                    .rposition(real_user)
                    .unwrap_or(messages.len());
                messages.splice(
                    index..index,
                    e["runtimeContextMessages"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .cloned(),
                );
            }
            Some("control_boundary")
                if boundary.is_some_and(|(index, _)| i == index)
                    && (valid_snapshot(e).is_some() || complete) =>
            {
                messages.extend(snapshot(e).into_iter().flatten().cloned())
            }
            _ => {}
        }
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(kind: &str, sequence: u64, extra: Value) -> Value {
        let mut entry = json!({"type":kind,"sessionId":"user","turnId":"turn","createdAt":"2026-10-27T00:00:00.000Z","sequence":sequence});
        for (k, v) in extra.as_object().into_iter().flatten() {
            entry[k] = v.clone();
        }
        entry
    }
    #[test]
    fn native_digest_alias_and_full_history_use_durable_transcript() {
        let root = std::env::temp_dir().join(format!("pdrs-always-chat-{}", uuid::Uuid::new_v4()));
        let home = root.join("home");
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        let directory = chat_dir(&home, &project);
        fs::create_dir_all(&directory).unwrap();
        let entries = [
            entry(
                "session_metadata",
                1,
                json!({"metadata":{"title":"Real session"}}),
            ),
            entry(
                "accepted_input",
                2,
                json!({"messages":[{"role":"user","content":[{"type":"text","text":"Please fix file"}]}]}),
            ),
            entry(
                "assistant_message",
                3,
                json!({"message":{"role":"assistant","content":[{"type":"text","text":"x".repeat(350)}]}}),
            ),
            entry("turn_result", 4, json!({"result":{}})),
        ];
        let source = entries.iter().map(|e| format!("{e}\n")).collect::<String>();
        fs::write(directory.join("user.jsonl"), &source).unwrap();
        fs::write(directory.join("always-on-discovery-hidden.jsonl"), &source).unwrap();
        let digest = digest(&home, &project).unwrap();
        assert_eq!(digest["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(digest["aliasMap"]["chat_1"], "user");
        assert_eq!(
            digest["sessions"][0]["userPrompts"],
            json!(["Please fix file"])
        );
        let history = conversation(&home, &project, "user").unwrap();
        assert_eq!(history["title"], "Real session");
        assert_eq!(history["messageCount"], 2);
        assert_eq!(
            history["conversation"][1]["text"].as_str().unwrap().len(),
            303
        );
        assert!(conversation(&home, &project, "missing").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compact_snapshot_is_validated_and_incomplete_assistant_is_not_replayed() {
        let user = json!({"role":"user","content":[{"type":"text","text":"before compact"}]});
        let mut entries = vec![
            json!({"type":"accepted_input","messages":[user.clone()]}),
            json!({"type":"assistant_message","turnId":"unfinished","message":{"role":"assistant","content":[{"type":"text","text":"must not replay"}]}}),
        ];
        let invalid = json!({"type":"control_boundary","turnId":"compact","boundary":{"kind":"compact","subtype":"compact_boundary","snapshot":{"version":1,"messages":[{"role":"assistant","content":[{"type":"unknown"}]}]}}});
        entries.push(invalid);
        assert_eq!(project_conversation(&entries), vec![user]);
        entries.push(json!({"type":"control_boundary","turnId":"compact","boundary":{"kind":"compact","subtype":"compact_boundary","snapshot":{"version":1,"messages":[{"role":"assistant","content":[{"type":"text","text":"valid compact replacement"}]}]}}}));
        let messages = project_conversation(&entries);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0]["content"][0]["text"],
            "valid compact replacement"
        );
    }
}

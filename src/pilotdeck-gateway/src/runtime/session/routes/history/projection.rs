use super::*;
use std::time::UNIX_EPOCH;

pub(super) fn metadata(entries: &[JsValue]) -> JsValue {
    let mut meta = JsObject::new();
    for entry in entries.iter().filter(|e| is(e, "type", "session_metadata")) {
        if let Some(next) = field(entry, "metadata").as_object() {
            for (key, value) in next.entries() {
                if (key.equals_str("title") || key.equals_str("linkedPullRequest"))
                    && matches!(value, JsValue::Null | JsValue::Undefined)
                {
                    continue;
                }
                meta.insert(key.clone(), value.clone());
            }
        }
    }
    JsValue::Object(meta)
}
pub(super) fn internal(id: &str) -> bool {
    [
        "always-on-discovery",
        "always-on-workspace",
        "always-on-report",
    ]
    .iter()
    .any(|p| {
        id.strip_prefix(p)
            .is_some_and(|s| s.starts_with([':', '-']))
    })
}
pub(super) fn catalog_visible(entries: &[JsValue]) -> bool {
    let meta = metadata(entries);
    ["title", "aiTitle", "firstPrompt", "lastPrompt"]
        .iter()
        .any(|k| {
            !text(&meta, k)
                .trim_matches(crate::metadata::js_space)
                .is_empty()
        })
        || entries
            .iter()
            .filter(|e| is(e, "type", "accepted_input"))
            .any(|e| {
                array(field(e, "messages"))
                    .iter()
                    .flat_map(|m| array(field(m, "content")))
                    .any(|b| {
                        is(b, "type", "text")
                            && !text(b, "text")
                                .trim_matches(crate::metadata::js_space)
                                .is_empty()
                    })
            })
}
pub(super) fn session_info(
    entries: &[JsValue],
    id: &str,
    project: &Path,
    disk: Option<&fs::Metadata>,
) -> JsValue {
    let meta = metadata(entries);
    let prompts = entries
        .iter()
        .filter(|e| is(e, "type", "accepted_input"))
        .filter_map(|e| {
            array(field(e, "messages"))
                .iter()
                .flat_map(|m| array(field(m, "content")))
                .find(|b| is(b, "type", "text"))
                .map(|b| {
                    text(b, "text")
                        .trim_matches(crate::metadata::js_space)
                        .to_owned()
                })
                .filter(|s| !s.is_empty())
        })
        .collect::<Vec<_>>();
    let value = |key: &str| {
        meta.get(key)
            .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
            .cloned()
            .unwrap_or(JsValue::Undefined)
    };
    let first = if !value("firstPrompt").is_undefined() {
        value("firstPrompt")
    } else {
        prompts
            .first()
            .map(|s| js(s.as_str()))
            .unwrap_or(JsValue::Undefined)
    };
    let last = if !value("lastPrompt").is_undefined() {
        value("lastPrompt")
    } else {
        prompts
            .last()
            .map(|s| js(s.as_str()))
            .unwrap_or(JsValue::Undefined)
    };
    let summary = [value("title"), value("aiTitle"), last, first.clone()]
        .into_iter()
        .find(|v| v.as_string().is_some_and(|s| !s.is_empty()))
        .unwrap_or_else(|| id.into());
    let date = |entry: Option<&JsValue>| {
        entry
            .and_then(|e| field(e, "createdAt").as_string())
            .and_then(|s| {
                pilotdeck_transport::system_clock::parse_node22_date(s)
                    .ok()
                    .flatten()
            })
            .filter(|n| n.is_finite())
    };
    let modified = disk
        .and_then(|m| m.modified().ok()?.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64() * 1000.0)
        .or_else(|| date(entries.last()))
        .unwrap_or(0.0);
    object([
        ("sessionId", id.into()),
        ("summary", summary),
        ("lastModified", JsValue::Number(modified)),
        (
            "fileSize",
            disk.map(|m| JsValue::Number(m.len() as f64))
                .unwrap_or(JsValue::Undefined),
        ),
        ("customTitle", value("title")),
        ("aiTitle", value("aiTitle")),
        ("firstPrompt", first),
        ("cwd", project.to_string_lossy().as_ref().into()),
        ("tag", value("tag")),
        (
            "createdAt",
            date(entries.first())
                .map(JsValue::Number)
                .unwrap_or(JsValue::Undefined),
        ),
        ("parentSessionId", value("parentSessionId")),
        ("forkedFromTurnId", value("forkedFromTurnId")),
    ])
}

fn put(value: &mut JsValue, key: &str, new: JsValue) {
    let mut o = value.as_object().cloned().unwrap_or_default();
    o.insert(key.into(), new);
    *value = JsValue::Object(o);
}
fn base(
    key: &str,
    project: Option<&JsValue>,
    entry: &JsValue,
    role: &str,
    kind: &str,
    id: String,
    now: &str,
) -> JsValue {
    object([
        ("id", js(id)),
        ("sessionKey", key.into()),
        ("projectKey", project.cloned().unwrap_or(JsValue::Undefined)),
        (
            "createdAt",
            entry
                .get("createdAt")
                .cloned()
                .unwrap_or_else(|| now.into()),
        ),
        ("provider", "pilotdeck".into()),
        ("role", role.into()),
        ("kind", kind.into()),
        ("turnId", field(entry, "turnId").clone()),
        ("sequence", field(entry, "sequence").clone()),
        ("source", "history".into()),
    ])
}
fn copy(value: &mut JsValue, source: &JsValue, names: &[&str]) {
    for name in names {
        if let Some(v) = source.get(name) {
            put(value, name, v.clone());
        }
    }
}
struct TextBuffer {
    units: Vec<u16>,
    block_id: JsValue,
    timeline: JsValue,
    images: Vec<JsValue>,
}
impl TextBuffer {
    fn new() -> Self {
        Self {
            units: Vec::new(),
            block_id: JsValue::Undefined,
            timeline: JsValue::Undefined,
            images: Vec::new(),
        }
    }
    fn flush(
        &mut self,
        out: &mut Vec<JsValue>,
        message: &JsValue,
        entry: &JsValue,
        context: &Context<'_>,
    ) {
        if self.units.is_empty() && self.images.is_empty() {
            return;
        }
        let role = if is(message, "role", "user") {
            "user"
        } else {
            "assistant"
        };
        let mut web = base(
            context.key,
            context.project,
            entry,
            role,
            "text",
            format!("{}-msg-{}-{}", context.key, context.index, out.len()),
            context.now,
        );
        put(
            &mut web,
            "text",
            JsValue::String(JsString::from_utf16(std::mem::take(&mut self.units))),
        );
        if !self.block_id.is_undefined() {
            put(&mut web, "blockId", self.block_id.clone());
        }
        if !self.timeline.is_undefined() {
            put(&mut web, "timeline", self.timeline.clone());
        }
        if role == "assistant" {
            copy(&mut web, field(message, "metadata"), &["model"]);
        }
        if !self.images.is_empty() {
            put(
                &mut web,
                "images",
                JsValue::Array(std::mem::take(&mut self.images)),
            );
        }
        if context.unsupported {
            put(
                &mut web,
                "payload",
                object([
                    ("forkUnsupportedContent", JsValue::Bool(true)),
                    (
                        "forkUnsupportedReason",
                        "This turn contains attachments or media.".into(),
                    ),
                ]),
            );
        }
        copy(&mut web, entry, &["entryId"]);
        out.push(web);
        self.block_id = JsValue::Undefined;
        self.timeline = JsValue::Undefined;
    }
}
struct Context<'a> {
    key: &'a str,
    project: Option<&'a JsValue>,
    now: &'a str,
    index: usize,
    unsupported: bool,
}
fn image(block: &JsValue) -> JsValue {
    let data = if is(block, "source", "url") {
        field(block, "data").clone()
    } else {
        js(format!(
            "data:{};base64,{}",
            text(block, "mimeType"),
            text(block, "data")
        ))
    };
    object([
        ("data", data),
        ("mimeType", field(block, "mimeType").clone()),
    ])
}
fn flatten(message: &JsValue, entry: &JsValue, context: Context<'_>) -> Vec<JsValue> {
    let mut out = Vec::new();
    let mut buffer = TextBuffer::new();
    let role = if is(message, "role", "user") {
        "user"
    } else {
        "assistant"
    };
    let mut last_tool = None;
    for block in array(field(message, "content")) {
        if is(block, "type", "text") {
            if buffer.block_id != *field(block, "blockId") {
                buffer.flush(&mut out, message, entry, &context);
            }
            buffer.block_id = field(block, "blockId").clone();
            buffer.timeline = field(block, "timeline").clone();
            if let Some(text) = field(block, "text").as_string() {
                buffer.units.extend_from_slice(text.units());
            }
            last_tool = None;
            continue;
        }
        if is(block, "type", "image") && role == "user" {
            if let Some(index) = last_tool {
                let web: &mut JsValue = &mut out[index];
                let mut images = array(field(web, "images")).to_vec();
                images.push(image(block));
                put(web, "images", JsValue::Array(images));
            } else {
                buffer.images.push(image(block));
            }
            continue;
        }
        buffer.flush(&mut out, message, entry, &context);
        last_tool = None;
        let kind = text(block, "type");
        let id = match kind.as_str() {
            "thinking" => format!("{}-thinking-{}-{}", context.key, context.index, out.len()),
            "tool_call" => format!(
                "{}-tool-{}-{}",
                context.key,
                context.index,
                text(block, "id")
            ),
            "tool_result" => format!(
                "{}-tool-{}-{}-result",
                context.key,
                context.index,
                text(block, "toolCallId")
            ),
            "tool_result_reference" => format!(
                "{}-tool-{}-{}-result-ref",
                context.key,
                context.index,
                text(block, "toolCallId")
            ),
            "media_reference" => format!("{}-media-{}-{}", context.key, context.index, out.len()),
            _ => format!("{}-attachment-{}-{}", context.key, context.index, out.len()),
        };
        let (web_role, web_kind) = match kind.as_str() {
            "thinking" => ("assistant", "thinking"),
            "tool_call" => ("tool", "tool_use"),
            "tool_result" | "tool_result_reference" | "media_reference" => ("tool", "tool_result"),
            _ => (role, "status"),
        };
        let mut web = base(
            context.key,
            context.project,
            entry,
            web_role,
            web_kind,
            id,
            context.now,
        );
        match kind.as_str() {
            "thinking" => {
                copy(&mut web, block, &["text", "blockId"]);
                copy(&mut web, entry, &["entryId"]);
            }
            "tool_call" => {
                put(&mut web, "toolCallId", field(block, "id").clone());
                put(&mut web, "toolName", field(block, "name").clone());
                put(&mut web, "payload", field(block, "input").clone());
            }
            "tool_result" => {
                copy(&mut web, block, &["toolCallId"]);
                put(
                    &mut web,
                    "ok",
                    JsValue::Bool(!matches!(field(block, "isError"), JsValue::Bool(true))),
                );
                let mut chunks = Vec::new();
                flatten_result_text(block, &mut chunks);
                put(&mut web, "text", js(chunks.join("\n")));
                let raw = field(block, "raw");
                copy(&mut web, raw, &["toolName"]);
                if let Some(code) = field(raw, "error").get("code") {
                    put(&mut web, "errorCode", code.clone());
                }
                let data = field(raw, "data");
                if data.get("planFilePath").is_some() {
                    let mut payload = JsObject::new();
                    for name in ["planFilePath", "planTitle", "planSummary"] {
                        payload.insert(name.into(), field(data, name).clone());
                    }
                    put(&mut web, "payload", JsValue::Object(payload));
                } else if ["grep", "glob"].contains(&text(raw, "toolName").to_lowercase().as_str())
                    && data.as_object().is_some()
                {
                    put(&mut web, "payload", data.clone());
                }
                let images = array(field(block, "content"))
                    .iter()
                    .filter(|b| is(b, "type", "image"))
                    .map(image)
                    .collect::<Vec<_>>();
                if !images.is_empty() {
                    put(&mut web, "images", JsValue::Array(images));
                }
                last_tool = Some(out.len());
            }
            "tool_result_reference" | "media_reference" => {
                copy(&mut web, block, &["toolCallId"]);
                put(&mut web, "text", field(block, "preview").clone());
                put(
                    &mut web,
                    "ok",
                    JsValue::Bool(
                        kind == "media_reference"
                            || !matches!(field(block, "isError"), JsValue::Bool(true)),
                    ),
                );
                if kind == "tool_result_reference" {
                    put(&mut web, "resultPath", field(block, "path").clone());
                }
                let mut payload = JsObject::new();
                for name in ["path", "originalBytes", "hasMore", "mimeType", "reason"] {
                    payload.insert(name.into(), field(block, name).clone());
                }
                if kind == "media_reference" {
                    for name in ["mediaType", "pages", "detail"] {
                        payload.insert(name.into(), field(block, name).clone());
                    }
                }
                put(&mut web, "payload", JsValue::Object(payload));
            }
            "image" | "pdf" | "audio" => {
                put(&mut web, "text", js(format!("[{kind} attachment]")));
                put(
                    &mut web,
                    "payload",
                    object([
                        ("mimeType", field(block, "mimeType").clone()),
                        ("bytes", field(block, "bytes").clone()),
                    ]),
                );
            }
            _ => continue,
        }
        copy(&mut web, block, &["timeline"]);
        out.push(web);
    }
    buffer.flush(&mut out, message, entry, &context);
    for web in &mut out {
        for name in ["queueItemId", "moduleId"] {
            if field(field(message, "metadata"), name)
                .as_string()
                .is_some_and(|s| !s.is_empty())
            {
                put(web, name, field(field(message, "metadata"), name).clone());
            }
        }
    }
    out
}
fn flatten_result_text(block: &JsValue, out: &mut Vec<String>) {
    for block in array(field(block, "content")) {
        let mime = text(block, "mimeType");
        let length = field(block, "data")
            .as_string()
            .map_or(0, JsString::len_utf16);
        match text(block, "type").as_str() {
            "text" => out.push(text(block, "text")),
            "image" => out.push(if is(block, "source", "url") {
                format!("[Image: {mime}, {}]", text(block, "data"))
            } else {
                format!("[Image: {mime}, {length} base64 characters]")
            }),
            "pdf" => {
                let pages = text(block, "pages");
                let pages = if pages.is_empty() && number(block, "pages") != 0.0 {
                    pilotdeck_protocol::js_value::number_string(number(block, "pages"))
                } else {
                    pages
                };
                out.push(format!(
                    "[PDF: {mime}, {length} base64 characters{}]",
                    if pages.is_empty() {
                        String::new()
                    } else {
                        format!(", {pages} pages")
                    }
                ));
            }
            _ => {}
        }
    }
}

pub(super) fn messages(
    entries: &[JsValue],
    key: &str,
    project: Option<&JsValue>,
    now: &str,
    subagent: bool,
    artifacts: bool,
) -> Vec<JsValue> {
    let mut out = Vec::new();
    let mut index = 0;
    let mut saw_execution = false;
    let mut incomplete: Vec<JsValue> = Vec::new();
    for entry in entries {
        let kind = text(entry, "type");
        let durable = [
            "assistant_message",
            "tool_result_message",
            "durable_message",
        ]
        .contains(&kind.as_str());
        if durable && !incomplete.contains(field(entry, "turnId")) {
            incomplete.push(field(entry, "turnId").clone());
        }
        if kind == "accepted_input" || durable {
            if subagent && kind == "accepted_input" {
                continue;
            }
            if durable {
                saw_execution = true;
            }
            let values = if durable {
                vec![field(entry, "message")]
            } else {
                array(field(entry, "messages")).iter().collect()
            };
            let unsupported = !durable
                && values.iter().any(|m| {
                    array(field(m, "content"))
                        .iter()
                        .any(|b| !is(b, "type", "text"))
                });
            for message in values {
                if matches!(
                    field(field(message, "metadata"), "synthetic"),
                    JsValue::Bool(true)
                ) || matches!(
                    field(field(message, "metadata"), "compactReplacement"),
                    JsValue::Bool(true)
                ) {
                    continue;
                }
                out.extend(flatten(
                    message,
                    entry,
                    Context {
                        key,
                        project,
                        now,
                        index,
                        unsupported,
                    },
                ));
                index += 1;
            }
        }
        if kind == "control_boundary"
            && is(field(entry, "boundary"), "kind", "compact")
            && (!subagent || saw_execution)
        {
            let mut web = base(
                key,
                project,
                entry,
                "system",
                "compact_boundary",
                field(entry, "entryId")
                    .as_string()
                    .and_then(|s| s.to_utf8().ok())
                    .unwrap_or_else(|| {
                        format!(
                            "{key}-compact-{}-{}",
                            text(entry, "turnId"),
                            number(entry, "sequence")
                        )
                    }),
                now,
            );
            put(&mut web, "text", "Context compacted".into());
            let source = field(field(entry, "boundary"), "compactMetadata");
            let mut meta = JsObject::new();
            for name in [
                "timeline",
                "compactionId",
                "trigger",
                "preTokens",
                "postTokens",
                "messagesSummarized",
                "targetTokens",
                "summaryGenerated",
                "checkpointMerged",
                "finalRatio",
                "level",
                "stage",
                "stageLabel",
            ] {
                if let Some(v) = source.get(name) {
                    meta.insert(name.into(), v.clone());
                }
            }
            put(&mut web, "payload", JsValue::Object(meta));
            copy(&mut web, source, &["timeline"]);
            copy(&mut web, entry, &["entryId"]);
            out.push(web);
        }
    }
    if !subagent {
        for entry in entries {
            let kind = text(entry, "type");
            let id = text(entry, "entryId");
            if kind == "file_artifacts" && artifacts {
                let has_result = entries.iter().any(|e| {
                    field(e, "turnId") == field(entry, "turnId")
                        && array(field(field(e, "message"), "content"))
                            .iter()
                            .any(|b| {
                                is(b, "type", "tool_result")
                                    || is(b, "type", "tool_result_reference")
                            })
                });
                let files = array(field(entry, "artifacts"))
                    .iter()
                    .filter(|a| has_result || !is(a, "source", "workspace_diff"))
                    .cloned()
                    .collect::<Vec<_>>();
                if files.is_empty() {
                    continue;
                }
                let mut web = base(
                    key,
                    project,
                    entry,
                    "assistant",
                    "file_artifacts",
                    if id.is_empty() {
                        format!(
                            "{key}-file-artifacts-{}-{}",
                            text(entry, "turnId"),
                            number(entry, "sequence")
                        )
                    } else {
                        id
                    },
                    now,
                );
                put(&mut web, "artifacts", JsValue::Array(files));
                put(
                    &mut web,
                    "payload",
                    object([("turnId", field(entry, "turnId").clone())]),
                );
                copy(&mut web, entry, &["entryId"]);
                out.push(web);
            } else if kind == "agent_status_message" {
                let status = text(entry, "kind");
                let mut web = base(
                    key,
                    project,
                    entry,
                    if status == "error" { "error" } else { "system" },
                    &status,
                    if id.is_empty() {
                        format!(
                            "{key}-agent-status-{}-{}",
                            text(entry, "turnId"),
                            number(entry, "sequence")
                        )
                    } else {
                        id
                    },
                    now,
                );
                copy(&mut web, entry, &["text"]);
                for (from, to) in [
                    ("messageI18n", "contentI18n"),
                    ("userHintI18n", "userHintI18n"),
                ] {
                    if field(field(entry, "detail"), from)
                        .get("key")
                        .and_then(JsValue::as_string)
                        .is_some()
                    {
                        put(&mut web, to, field(field(entry, "detail"), from).clone());
                    }
                }
                let mut payload = object([("event", field(entry, "event").clone())]);
                copy(&mut payload, entry, &["detail"]);
                put(&mut web, "payload", payload);
                out.push(web);
            } else if kind == "turn_result" && is(field(entry, "result"), "type", "error") {
                if entries.iter().any(|s| {
                    is(s, "type", "agent_status_message")
                        && is(s, "kind", "error")
                        && field(s, "turnId") == field(entry, "turnId")
                        && !matches!(field(field(s, "detail"), "visible"), JsValue::Bool(false))
                }) {
                    continue;
                }
                let result = field(entry, "result");
                let errors = array(field(result, "errors"))
                    .iter()
                    .map(|e| text(e, "message"))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>();
                let mut web = base(
                    key,
                    project,
                    entry,
                    "error",
                    "error",
                    format!("{key}-turn-error-{}", text(entry, "turnId")),
                    now,
                );
                put(
                    &mut web,
                    "text",
                    js(if errors.is_empty() {
                        format!("Turn failed: {}", text(result, "stopReason"))
                    } else {
                        errors.join("\n")
                    }),
                );
                put(
                    &mut web,
                    "payload",
                    object([
                        ("code", field(result, "stopReason").clone()),
                        ("recoverable", JsValue::Bool(false)),
                    ]),
                );
                out.push(web);
            }
        }
        let started = entries
            .iter()
            .filter(|e| is(e, "type", "subagent_started"))
            .collect::<Vec<_>>();
        let mut assigned = Vec::new();
        let mut next = 0;
        for (at, web) in out.iter_mut().enumerate() {
            if next < started.len()
                && is(web, "kind", "tool_use")
                && ["agent", "task"].contains(&text(web, "toolName").to_lowercase().as_str())
            {
                put(
                    web,
                    "subagentId",
                    field(started[next], "subagentId").clone(),
                );
                assigned.push((field(started[next], "subagentId").clone(), at));
                next += 1;
            }
        }
        for completed in entries.iter().filter(|e| {
            is(e, "type", "subagent_completed")
                && !matches!(field(e, "errored"), JsValue::Bool(true))
        }) {
            let Some((_, at)) = assigned
                .iter()
                .find(|(id, _)| id == field(completed, "subagentId"))
            else {
                continue;
            };
            let tool = &out[*at];
            if field(tool, "toolCallId").as_string().is_none()
                || out.iter().any(|r| {
                    is(r, "kind", "tool_result")
                        && field(r, "toolCallId") == field(tool, "toolCallId")
                        && field(r, "turnId") == field(tool, "turnId")
                })
            {
                continue;
            }
            let mut web = base(
                key,
                project,
                completed,
                "tool",
                "tool_result",
                format!("{}-subagent-result", text(tool, "id")),
                now,
            );
            copy(&mut web, tool, &["turnId", "toolCallId", "toolName"]);
            put(&mut web, "ok", JsValue::Bool(true));
            put(&mut web, "text", field(completed, "summaryPreview").clone());
            copy(&mut web, completed, &["entryId"]);
            out.push(web);
        }
    }
    out.sort_by(|a, b| number(a, "sequence").total_cmp(&number(b, "sequence")));
    if !subagent {
        incomplete.retain(|id| {
            !entries
                .iter()
                .any(|e| is(e, "type", "turn_result") && field(e, "turnId") == id)
        });
        if !incomplete.is_empty() {
            let ids = incomplete
                .iter()
                .map(|v| {
                    v.as_string()
                        .and_then(|s| s.to_utf8().ok())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join("-");
            out.push(object([
                ("id", js(format!("{key}-incomplete-turn-status-{ids}"))),
                ("sessionKey", key.into()),
                ("projectKey", project.cloned().unwrap_or(JsValue::Undefined)),
                ("createdAt", now.into()),
                ("provider", "pilotdeck".into()),
                ("role", "system".into()),
                ("kind", "status".into()),
                (
                    "text",
                    "本轮记录尚未写入最终结果，已恢复当时产生的工具调用和输出。".into(),
                ),
                (
                    "payload",
                    object([("incompleteTurnIds", JsValue::Array(incomplete))]),
                ),
                ("source", "history".into()),
            ]));
        }
    }
    out
}

fn positive(value: &JsValue, key: &str) -> Option<f64> {
    field(value, key)
        .as_number()
        .filter(|n| n.is_finite() && *n > 0.0)
}
pub(super) fn token_usage(
    entries: &[JsValue],
    max_context: Option<f64>,
    max_output: Option<f64>,
) -> JsValue {
    let mut budget = None;
    let mut compact = None;
    let mut usage = None;
    for (index, entry) in entries.iter().enumerate() {
        if is(entry, "type", "agent_status_message") && is(entry, "event", "context_budget") {
            let detail = field(entry, "detail");
            if let (Some(used), Some(total)) = (
                positive(detail, "displayUsed").or_else(|| positive(detail, "used")),
                positive(detail, "total"),
            ) {
                let mut value = object([
                    ("used", JsValue::Number(used)),
                    ("total", JsValue::Number(total)),
                    (
                        "effectiveTotal",
                        JsValue::Number(positive(detail, "effectiveTotal").unwrap_or(total)),
                    ),
                    (
                        "reservedOutputTokens",
                        JsValue::Number(positive(detail, "reservedOutputTokens").unwrap_or(0.0)),
                    ),
                    ("source", "history".into()),
                    ("exact", JsValue::Bool(true)),
                ]);
                for name in ["displayUsed", "budgetUsed"] {
                    if let Some(n) = positive(detail, name) {
                        put(&mut value, name, JsValue::Number(n));
                    }
                }
                copy(&mut value, detail, &["state", "ratio"]);
                budget = Some((index, value));
            }
        }
        if is(entry, "type", "control_boundary")
            && is(field(entry, "boundary"), "kind", "compact")
            && is(field(entry, "boundary"), "subtype", "compact_boundary")
        {
            let meta = field(field(entry, "boundary"), "compactMetadata");
            if let Some(post) = positive(meta, "postTokens") {
                compact = Some((index, post, meta.clone()));
            }
        }
        if is(entry, "type", "turn_result") {
            let u = field(field(entry, "result"), "usage");
            if [
                "inputTokens",
                "outputTokens",
                "cacheReadTokens",
                "cacheWriteTokens",
                "totalTokens",
            ]
            .iter()
            .any(|k| positive(u, k).is_some())
            {
                usage = Some(u.clone());
            }
        }
    }
    let valid = |n: f64| n.is_finite() && n > 0.0;
    if let Some((_, post, meta)) =
        compact.filter(|(i, _, _)| budget.as_ref().is_none_or(|(b, _)| i > b))
    {
        let prior = budget.as_ref().map(|(_, v)| v);
        let total = prior
            .and_then(|v| positive(v, "total"))
            .or(max_context.filter(|n| valid(*n)))
            .unwrap_or(200_000.0);
        let reserved = prior
            .and_then(|v| positive(v, "reservedOutputTokens"))
            .or(max_output.filter(|n| valid(*n)))
            .unwrap_or(0.0);
        let effective = prior
            .and_then(|v| positive(v, "effectiveTotal"))
            .unwrap_or((total - reserved).max(1.0));
        let used = post.ceil();
        let ratio = used / effective;
        let mut value = object([
            ("used", JsValue::Number(used)),
            ("displayUsed", JsValue::Number(used)),
            ("budgetUsed", JsValue::Number(used)),
            ("total", JsValue::Number(total)),
            ("effectiveTotal", JsValue::Number(effective)),
            ("reservedOutputTokens", JsValue::Number(reserved)),
            ("ratio", JsValue::Number(ratio)),
            (
                "state",
                (if ratio >= 0.95 {
                    "blocking"
                } else if ratio >= 0.8 {
                    "warning"
                } else {
                    "ok"
                })
                .into(),
            ),
            ("source", "compact".into()),
            ("exact", JsValue::Bool(false)),
            ("compacted", JsValue::Bool(true)),
        ]);
        if let Some(n) = positive(&meta, "preTokens") {
            put(&mut value, "preCompactUsed", JsValue::Number(n));
        }
        if let Some(n) = positive(&meta, "messagesSummarized") {
            put(&mut value, "messagesSummarized", JsValue::Number(n));
        }
        return value;
    }
    if let Some((_, budget)) = budget {
        return budget;
    }
    let Some(usage) = usage else {
        return JsValue::Undefined;
    };
    let output = positive(&usage, "outputTokens").unwrap_or(0.0);
    let read = positive(&usage, "cacheReadTokens");
    let write = positive(&usage, "cacheWriteTokens");
    let input = positive(&usage, "inputTokens");
    let total_tokens = positive(&usage, "totalTokens");
    let used = if let Some(input) = input {
        (input + read.unwrap_or(0.0) + write.unwrap_or(0.0)).ceil()
    } else {
        total_tokens
            .map(|t| (t - output).ceil().max(0.0))
            .unwrap_or(0.0)
    };
    if used <= 0.0 {
        return JsValue::Undefined;
    }
    let total = max_context.filter(|n| valid(*n)).unwrap_or(200_000.0);
    let reserved = max_output.filter(|n| valid(*n)).unwrap_or(0.0);
    object([
        ("used", JsValue::Number(used)),
        ("total", JsValue::Number(total)),
        (
            "effectiveTotal",
            JsValue::Number((total - reserved).max(1.0)),
        ),
        ("reservedOutputTokens", JsValue::Number(reserved)),
        ("source", "history".into()),
        ("exact", JsValue::Bool(true)),
        (
            "breakdown",
            object([
                ("input", JsValue::Number(input.unwrap_or(0.0))),
                ("cacheRead", JsValue::Number(read.unwrap_or(0.0))),
                ("cacheWrite", JsValue::Number(write.unwrap_or(0.0))),
                ("output", JsValue::Number(output)),
                (
                    "total",
                    JsValue::Number(total_tokens.unwrap_or((used + output).ceil())),
                ),
            ]),
        ),
    ])
}

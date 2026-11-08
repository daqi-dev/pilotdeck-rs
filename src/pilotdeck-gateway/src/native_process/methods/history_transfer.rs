//! Explicit history transfer. The envelope contains verified messages, never
//! execution handles, approvals, checkpoints, leases, or resume instructions.
use super::super::requests::Requests;
use crate::{
    runtime::{
        connection::RequestError,
        session::{SessionContext, factory},
    },
    storage,
};
use base64::Engine;
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

fn err(message: impl Into<String>) -> RequestError {
    RequestError::new(message.into())
}
fn to_json(value: &JsValue) -> Result<Value, RequestError> {
    serde_json::from_str(
        &stringify_json(value)
            .map_err(|e| err(e.to_string()))?
            .unwrap_or_else(|| "null".into()),
    )
    .map_err(|e| err(e.to_string()))
}
fn to_js(value: &Value) -> Result<JsValue, RequestError> {
    parse_json(&value.to_string()).map_err(|e| err(e.to_string()))
}
fn digest(value: &Value) -> Result<String, RequestError> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).map_err(|e| err(e.to_string()))?)
    ))
}

/// Import uses canonical PilotDeck content. Provider-shaped historical blocks
/// are normalized to that content once during export; runtime metadata is not
/// imported. The model label is a read-only display annotation.
fn canonical_messages(messages: &Value) -> Result<Value, RequestError> {
    fn block(value: &Value) -> Result<Value, RequestError> {
        let mut value = value.clone();
        match value["type"].as_str() {
            Some("tool_use") => {
                value["type"] = json!("tool_call");
            }
            Some("thinking") if value.get("text").is_none() => {
                value["text"] = value["thinking"].clone();
                value.as_object_mut().unwrap().remove("thinking");
            }
            Some("document") => {
                value["type"] = json!("pdf");
            }
            _ => {}
        }
        if value["type"] == "tool_result" {
            if value.get("toolCallId").is_none() {
                value["toolCallId"] = value["tool_use_id"].clone();
                value.as_object_mut().unwrap().remove("tool_use_id");
            }
            if let Some(text) = value["content"].as_str() {
                value["content"] = json!([{"type":"text","text":text}]);
            }
            if let Some(content) = value["content"].as_array() {
                value["content"] =
                    Value::Array(content.iter().map(block).collect::<Result<_, _>>()?);
            }
        }
        if value["source"].is_object() && value["source"]["type"] == "base64" {
            let source = value["source"].clone();
            value["source"] = json!("base64");
            value["data"] = source["data"].clone();
            value["mimeType"] = source["media_type"].clone();
        }
        Ok(value)
    }
    let mut result = Vec::new();
    for message in messages
        .as_array()
        .ok_or_else(|| err("Transfer messages must be an array."))?
    {
        let content = if let Some(text) = message["content"].as_str() {
            json!([{"type":"text","text":text}])
        } else {
            Value::Array(
                message["content"]
                    .as_array()
                    .ok_or_else(|| err("Message content must be an array or text."))?
                    .iter()
                    .map(block)
                    .collect::<Result<_, _>>()?,
            )
        };
        let mut next = json!({"role":message["role"],"content":content});
        let mut display = serde_json::Map::new();
        for key in ["model", "turnId", "createdAt", "kernelMessageId"] {
            if let Some(value) = message["metadata"][key].as_str() {
                display.insert(key.into(), json!(value));
            }
        }
        if !display.is_empty() {
            next["metadata"] = Value::Object(display);
        }
        result.push(next);
    }
    Ok(Value::Array(result))
}
fn verify_media(
    block: &Value,
    path: Value,
    attachments: &mut Vec<Value>,
) -> Result<(), RequestError> {
    let kind = block["type"].as_str().unwrap_or("");
    if !matches!(kind, "image" | "pdf" | "audio") {
        return Err(err("Unknown inline attachment type."));
    }
    if kind != "pdf" && block["source"] != "base64" {
        return Err(err(
            "Attachments must contain inline base64 bytes; mutable paths/URLs cannot be imported.",
        ));
    }
    if kind != "pdf" && !block["mimeType"].is_string() {
        return Err(err("Canonical attachment mimeType is missing."));
    }
    let data = block["data"]
        .as_str()
        .ok_or_else(|| err("Attachment bytes missing."))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| err("Attachment base64 is invalid."))?;
    if let Some(size) = block["bytes"].as_u64()
        && size != bytes.len() as u64
    {
        return Err(err("Attachment byte count does not match content."));
    }
    let mut manifest = path;
    manifest["sha256"] = json!(format!("{:x}", Sha256::digest(bytes)));
    attachments.push(manifest);
    Ok(())
}
fn verify_messages(messages: &Value) -> Result<Vec<Value>, RequestError> {
    let mut tools = BTreeSet::new();
    let mut message_ids = BTreeSet::new();
    let mut block_ids = BTreeSet::new();
    let mut attachments = Vec::new();
    for (m, message) in messages
        .as_array()
        .ok_or_else(|| err("Transfer messages must be an array."))?
        .iter()
        .enumerate()
    {
        if !matches!(
            message["role"].as_str(),
            Some("user" | "assistant" | "system" | "tool")
        ) {
            return Err(err("Transfer message role is unsupported."));
        }
        if message.as_object().is_none_or(|o| {
            o.keys()
                .any(|k| !matches!(k.as_str(), "role" | "content" | "metadata"))
        }) {
            return Err(err("Transfer contains unverified message runtime state."));
        }
        if let Some(metadata) = message.get("metadata")
            && metadata.as_object().is_none_or(|o| {
                o.iter().any(|(key, value)| {
                    !matches!(
                        key.as_str(),
                        "model" | "turnId" | "createdAt" | "kernelMessageId"
                    ) || !value.is_string()
                })
            })
        {
            return Err(err(
                "Only read-only display identity/model annotations are imported from message metadata.",
            ));
        }
        for key in ["kernelMessageId", "turnId"] {
            if let Some(value) = message["metadata"].get(key)
                && value.as_str().is_none_or(|value| value.trim().is_empty())
            {
                return Err(err(format!("Invalid display {key}.")));
            }
        }
        if let Some(id) = message["metadata"]["kernelMessageId"].as_str()
            && !message_ids.insert(id.to_string())
        {
            return Err(err("Duplicate canonical display message identity."));
        }
        if let Some(created) = message["metadata"].get("createdAt") {
            let created = created
                .as_str()
                .ok_or_else(|| err("Invalid display createdAt."))?;
            if pilotdeck_transport::system_clock::parse_node22_date(&JsString::from(created))
                .map_err(|e| err(String::from_utf16_lossy(e.units())))?
                .is_none_or(|value| !value.is_finite())
            {
                return Err(err("Invalid display timestamp."));
            }
        }
        let blocks = message["content"]
            .as_array()
            .ok_or_else(|| err("Canonical message content must be an array."))?;
        for (b, block) in blocks.iter().enumerate() {
            if block.as_object().is_none_or(|o| {
                o.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "type"
                            | "text"
                            | "reasoningContent"
                            | "signature"
                            | "blockId"
                            | "timeline"
                            | "id"
                            | "name"
                            | "input"
                            | "raw"
                            | "toolCallId"
                            | "isError"
                            | "content"
                            | "source"
                            | "data"
                            | "mimeType"
                            | "bytes"
                            | "pages"
                            | "detail"
                            | "durationSeconds"
                    )
                })
            }) {
                return Err(err("History block contains unverified runtime state."));
            }
            let block_id = match block.get("blockId").filter(|value| !value.is_null()) {
                Some(value) => Some(
                    value
                        .as_str()
                        .filter(|value| !value.trim().is_empty())
                        .ok_or_else(|| err("Invalid display blockId."))?,
                ),
                None => None,
            };
            let timeline = block.get("timeline").filter(|value| !value.is_null());
            if let Some(timeline) = timeline {
                let object = timeline
                    .as_object()
                    .ok_or_else(|| err("Invalid display timeline."))?;
                if object.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "version"
                            | "turnId"
                            | "id"
                            | "previousId"
                            | "order"
                            | "revision"
                            | "offset"
                    )
                }) || timeline["version"] != 1
                    || ["turnId", "id"].iter().any(|key| {
                        timeline[*key]
                            .as_str()
                            .is_none_or(|value| value.trim().is_empty())
                    })
                    || ["order", "revision"].iter().any(|key| {
                        timeline[*key].as_f64().is_none_or(|value| {
                            value < 0.0 || value.fract() != 0.0 || value > 9_007_199_254_740_991.0
                        })
                    })
                    || timeline.get("offset").is_some_and(|value| {
                        value.as_f64().is_none_or(|value| {
                            value < 0.0 || value.fract() != 0.0 || value > 9_007_199_254_740_991.0
                        })
                    })
                    || timeline
                        .get("previousId")
                        .filter(|value| !value.is_null())
                        .is_some_and(|value| {
                            value.as_str().is_none_or(|value| value.trim().is_empty())
                                || value == &timeline["id"]
                        })
                {
                    return Err(err("Malformed canonical display timeline."));
                }
                if matches!(block["type"].as_str(), Some("text" | "thinking"))
                    && block_id.is_some_and(|id| timeline["id"] != id)
                {
                    return Err(err("Display blockId and timeline identity disagree."));
                }
            }
            let scope = message["metadata"]["turnId"]
                .as_str()
                .or_else(|| timeline.and_then(|timeline| timeline["turnId"].as_str()));
            let display_id = timeline
                .and_then(|timeline| timeline["id"].as_str())
                .or(block_id);
            if !matches!(block["type"].as_str(), Some("tool_call" | "tool_result"))
                && let (Some(scope), Some(id)) = (scope, display_id)
                && !block_ids.insert((scope.to_string(), id.to_string()))
            {
                return Err(err(
                    "Duplicate display block identity in one declared turn.",
                ));
            }
            let kind = block["type"]
                .as_str()
                .ok_or_else(|| err("Transfer block type missing."))?;
            match kind {
                "text" => {
                    if !block["text"].is_string() {
                        return Err(err("Invalid text block."));
                    }
                }
                "thinking" => {
                    if !block["text"].is_string()
                        || block
                            .get("reasoningContent")
                            .is_some_and(|v| !v.is_string())
                    {
                        return Err(err("Invalid canonical thinking block."));
                    }
                }
                "tool_call" => {
                    let id = block["id"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| err("Tool call identity missing."))?;
                    if message["role"] != "assistant"
                        || !block["name"].is_string()
                        || !tools.insert(id.to_string())
                    {
                        return Err(err("Duplicate or misplaced tool call."));
                    }
                }
                "tool_result" => {
                    let id = block["toolCallId"]
                        .as_str()
                        .ok_or_else(|| err("Canonical tool result identity missing."))?;
                    if !tools.remove(id) {
                        return Err(err("Tool result has no preceding verified call."));
                    }
                    for (r, part) in block["content"]
                        .as_array()
                        .ok_or_else(|| err("Canonical tool result content must be an array."))?
                        .iter()
                        .enumerate()
                    {
                        if part["type"] == "text" && part["text"].is_string() {
                            continue;
                        }
                        verify_media(
                            part,
                            json!({"message":m,"block":b,"resultItem":r}),
                            &mut attachments,
                        )?;
                    }
                }
                "image" | "pdf" | "audio" => {
                    verify_media(block, json!({"message":m,"block":b}), &mut attachments)?
                }
                _ => {
                    return Err(err(
                        "Unknown/unverified content or mutable result/media reference cannot be imported.",
                    ));
                }
            }
        }
    }
    if !tools.is_empty() {
        return Err(err(
            "History has unresolved tool effects; import cannot resume them.",
        ));
    }
    Ok(attachments)
}
fn envelope(source: Value, messages: Value) -> Result<Value, RequestError> {
    let messages = canonical_messages(&messages)?;
    let attachments = verify_messages(&messages)?;
    let body = json!({"format":"pilotdeck-history-import-v1","source":source,"messages":messages,"attachments":attachments});
    let hash = digest(&body)?;
    let mut out = body;
    out["sha256"] = json!(hash);
    Ok(out)
}
fn verify_envelope(value: &Value) -> Result<(), RequestError> {
    let mut body = value.clone();
    let hash = body
        .as_object_mut()
        .ok_or_else(|| err("Transfer envelope must be an object."))?
        .remove("sha256")
        .and_then(|v| v.as_str().map(str::to_string))
        .ok_or_else(|| err("Transfer hash missing."))?;
    if body["format"] != "pilotdeck-history-import-v1" || digest(&body)? != hash {
        return Err(err("Transfer envelope hash/format mismatch."));
    }
    if body["attachments"] != json!(verify_messages(&body["messages"])?) {
        return Err(err("Transfer attachment manifest mismatch."));
    }
    if body
        .as_object()
        .unwrap()
        .keys()
        .any(|k| !matches!(k.as_str(), "format" | "source" | "messages" | "attachments"))
    {
        return Err(err("Transfer envelope contains execution/resume state."));
    }
    Ok(())
}
struct LegacySourceProof {
    detached: bool,
    messages: Value,
    _fence: tokio::sync::OwnedMutexGuard<()>,
    _external: Option<storage::LedgerLock>,
    _activity: Option<crate::runtime::session::HandleActivity>,
}
fn require_legacy_stop_attestation(detached: bool, params: &JsValue) -> Result<(), RequestError> {
    if detached && params.get("sourceRuntimeStopped") != Some(&JsValue::Bool(true)) {
        return Err(err(
            "Detached legacy import requires explicit sourceRuntimeStopped:true caller attestation after stopping its old runtime. A free Rust ledger lock cannot prove an unrelated TypeScript runtime stopped.",
        ));
    }
    Ok(())
}
fn verified_legacy_messages(
    path: &std::path::Path,
    session: &JsString,
) -> Result<Value, RequestError> {
    let read = storage::read_transcript(path).map_err(|error| err(error.to_string()))?;
    read.ensure_writable()
        .map_err(|error| err(error.to_string()))?;
    if !read.diagnostics.is_empty() {
        return Err(err(format!(
            "History source has incomplete or malformed transcript diagnostics: {:?}",
            read.diagnostics
        )));
    }
    if read.entries.is_empty() {
        return Err(err("Source transcript does not exist or is empty."));
    }
    storage::verify_history_quiescent(session, &read.entries)
        .map_err(|error| err(error.to_string()))?;
    let projected = factory::project(&read.entries).map_err(|error| err(error.message))?;
    to_json(&JsValue::Array(projected.messages))
}
impl Requests {
    async fn legacy_source_proof(
        &self,
        session: &JsString,
    ) -> Result<LegacySourceProof, RequestError> {
        let path = super::super::assembly::transcript_path(&self.options, session);
        let path =
            storage::canonical_transcript_path(&path).map_err(|error| err(error.to_string()))?;
        let fence = crate::runtime::session::transcript_locks::acquire(&path)
            .await
            .map_err(|error| err(String::from_utf16_lossy(error.message.units())))?;
        let local = self.owner.current_handle(session);
        let activity = local
            .as_ref()
            .map(|handle| {
                handle
                    .begin_maintenance()
                    .map_err(|error| err(error.message))
            })
            .transpose()?;
        let external = if local.is_none() {
            Some(
                storage::LedgerLock::acquire(&path)
                    .map_err(|error| err(format!("Source writer is not quiescent: {error}")))?
                    .1,
            )
        } else {
            if local
                .as_ref()
                .is_some_and(|handle| handle.backend_kind() != "legacy")
            {
                return Err(err("History source backend ownership changed."));
            }
            let writer = self
                .recorders
                .lock()
                .unwrap()
                .get(session)
                .and_then(std::sync::Weak::upgrade)
                .ok_or_else(|| err("Source local writer ownership is unavailable."))?;
            if storage::canonical_transcript_path(writer.writer().path())
                .map_err(|error| err(error.to_string()))?
                != path
            {
                return Err(err(
                    "Source writer path does not match its canonical fence.",
                ));
            }
            None
        };
        let mut owner_path = path.as_os_str().to_os_string();
        owner_path.push(".backend.json");
        match std::fs::read(std::path::PathBuf::from(owner_path)) {
            Ok(bytes) => {
                let owner: Value =
                    serde_json::from_slice(&bytes).map_err(|error| err(error.to_string()))?;
                let project = std::fs::canonicalize(&self.options.project)
                    .map_err(|error| err(error.to_string()))?;
                if owner["backend"] != "legacy"
                    || owner["session"]
                        != session.to_utf8().map_err(|error| err(error.to_string()))?
                    || owner["project"] != project.to_string_lossy().as_ref()
                {
                    return Err(err("Source backend/project/session ownership mismatch."));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(err(error.to_string())),
        }
        let messages = verified_legacy_messages(&path, session)?;
        Ok(LegacySourceProof {
            detached: local.is_none(),
            messages,
            _fence: fence,
            _external: external,
            _activity: activity,
        })
    }
    async fn history_kernel_handle(
        &self,
        session: &JsString,
    ) -> Result<Option<crate::runtime::session::LiveSessionHandle>, RequestError> {
        if let Some(handle) = self.owner.current_handle(session) {
            return Ok((handle.backend_kind() == "kernel").then_some(handle));
        }
        let mut path =
            super::super::assembly::transcript_path(&self.options, session).into_os_string();
        path.push(".backend.json");
        let saved =
            crate::runtime::session::kernel::persisted_owner(&std::path::PathBuf::from(path))
                .map_err(RequestError::from_runtime)?;
        if saved.is_some_and(|s| s["backend"] == "kernel") {
            return self
                .owner
                .get_or_create(SessionContext {
                    session_key: session.clone(),
                    project_key: Some(self.options.project.to_string_lossy().into_owned().into()),
                    channel_key: "history".into(),
                    allowed_tools: None,
                    disallowed_tools: None,
                })
                .await
                .map(Some)
                .map_err(|e| err(e.message));
        }
        Ok(None)
    }
    async fn assert_kernel_quiescent(
        &self,
        session: &JsString,
        params: &JsValue,
    ) -> Result<Option<crate::runtime::session::LiveSessionHandle>, RequestError> {
        let handle = self.history_kernel_handle(session).await?;
        if let Some(handle) = &handle {
            let facts = handle
                .backend_request("active_turn_snapshot".into(), params.clone())
                .await
                .map_err(RequestError::from_runtime)?
                .ok_or_else(|| err("Kernel state unavailable."))?;
            let status = facts
                .get("status")
                .and_then(JsValue::as_string)
                .and_then(|v| v.to_utf8().ok())
                .unwrap_or_default();
            if !matches!(
                status.as_str(),
                "idle" | "completed" | "failed" | "cancelled"
            ) {
                return Err(err(
                    "Source session has active or unknown effects; history import cannot resume it.",
                ));
            }
        }
        Ok(handle)
    }
    pub(super) async fn read_history_messages(
        &self,
        params: &JsValue,
    ) -> Result<JsValue, RequestError> {
        let session = super::string(params, "sessionKey")?;
        if let Some(handle) = self.history_kernel_handle(&session).await? {
            return handle
                .backend_request("read_session_messages".into(), params.clone())
                .await
                .map_err(RequestError::from_runtime)?
                .ok_or_else(|| err("Kernel history unavailable."));
        }
        let path = super::super::assembly::transcript_path(&self.options, &session);
        let _fence = crate::runtime::session::transcript_locks::acquire(&path)
            .await
            .map_err(|e| err(String::from_utf16_lossy(e.message.units())))?;
        let read = storage::read_transcript(&path).map_err(|e| err(e.to_string()))?;
        let projected = factory::project(&read.entries).map_err(|e| err(e.message))?;
        let mut result =
            crate::metadata::object([("messages", JsValue::Array(projected.messages))]);
        if let JsValue::Object(ref mut result) = result {
            result.insert(
                "stream".into(),
                self.coordinator
                    .snapshot(&crate::runtime::turn::replay::ActiveTurnSnapshotInput::new(
                        session,
                    ))
                    .to_json(),
            );
        }
        Ok(result)
    }
    pub(super) async fn transfer_history(
        &self,
        method: &str,
        params: &JsValue,
    ) -> Result<JsValue, RequestError> {
        let session = super::string(params, "sessionKey")?;
        if method == "export_session_transcript" {
            let _reservation = self
                .owner
                .begin_maintenance(&session)
                .map_err(|e| err(e.message))?
                .ok_or_else(|| err("Source session must be quiescent before export."))?;
            if !self
                .interactions
                .permission_bus()
                .snapshot(&session)
                .is_empty()
                || !self
                    .interactions
                    .elicitation_bus()
                    .snapshot(&session)
                    .is_empty()
            {
                return Err(err("Source session still has pending interactions."));
            }
            let kernel_handle = self.assert_kernel_quiescent(&session, params).await?;
            let (backend, messages) = if let Some(handle) = kernel_handle {
                let history = handle
                    .backend_request("read_session_messages".into(), params.clone())
                    .await
                    .map_err(RequestError::from_runtime)?
                    .ok_or_else(|| err("Kernel history unavailable."))?;
                let history = to_json(&history)?;
                (
                    "kernel",
                    history
                        .get("canonicalMessages")
                        .or_else(|| history.get("messages"))
                        .or_else(|| history.get("history"))
                        .cloned()
                        .unwrap_or(json!([])),
                )
            } else {
                let proof = self.legacy_source_proof(&session).await?;
                ("legacy", proof.messages.clone())
            };
            let project =
                std::fs::canonicalize(&self.options.project).map_err(|e| err(e.to_string()))?;
            return to_js(&envelope(
                json!({"project":project,"sessionKey":session.to_utf8().map_err(|e|err(e.to_string()))?,"backend":backend}),
                messages,
            )?);
        }
        let params_json = to_json(params)?;
        let transfer = &params_json["transcript"];
        verify_envelope(transfer)?;
        let source = transfer["source"]["sessionKey"]
            .as_str()
            .ok_or_else(|| err("Transfer source session missing."))?;
        if session.equals_str(source) {
            return Err(err("Import requires a distinct new sessionKey."));
        }
        let project =
            std::fs::canonicalize(&self.options.project).map_err(|e| err(e.to_string()))?;
        if transfer["source"]["project"] != project.to_string_lossy().as_ref() {
            return Err(err(
                "Transfer source project does not match the canonical configured project.",
            ));
        }
        let source_key = JsString::from(source);
        let _source = self
            .owner
            .begin_maintenance(&source_key)
            .map_err(|e| err(e.message))?
            .ok_or_else(|| err("Source session must be quiescent before import."))?;
        if !self
            .interactions
            .permission_bus()
            .snapshot(&source_key)
            .is_empty()
            || !self
                .interactions
                .elicitation_bus()
                .snapshot(&source_key)
                .is_empty()
        {
            return Err(err(
                "Source has pending approval/question; import cannot resume it.",
            ));
        }
        let kernel_source = self.assert_kernel_quiescent(&source_key, params).await?;
        let _kernel_activity = kernel_source
            .as_ref()
            .map(|handle| {
                handle
                    .begin_maintenance()
                    .map_err(|error| err(error.message))
            })
            .transpose()?;
        let legacy_proof = if kernel_source.is_none() {
            Some(self.legacy_source_proof(&source_key).await?)
        } else {
            None
        };
        if let Some(proof) = &legacy_proof {
            require_legacy_stop_attestation(proof.detached, params)?;
        }
        let actual_backend = if kernel_source.is_some() {
            "kernel"
        } else {
            "legacy"
        };
        if transfer["source"]["backend"] != actual_backend {
            return Err(err(
                "Transfer source backend does not match its current immutable owner.",
            ));
        }
        let _target = self
            .owner
            .begin_maintenance(&session)
            .map_err(|e| err(e.message))?
            .ok_or_else(|| err("Target session is busy."))?;
        if self
            .env
            .get("PILOTDECK_SESSION_BACKEND")
            .map(String::as_str)
            != Some("kernel")
        {
            return Err(err("Explicit history import requires Kernel mode."));
        }
        let handle = self
            .owner
            .get_or_create(SessionContext {
                session_key: session.clone(),
                project_key: Some(self.options.project.to_string_lossy().into_owned().into()),
                channel_key: "history_import".into(),
                allowed_tools: None,
                disallowed_tools: None,
            })
            .await
            .map_err(|e| err(e.message))?;
        if handle.backend_kind() != "kernel" {
            return Err(err(
                "Import target must be a new Kernel session; existing legacy ownership is immutable.",
            ));
        }
        // A non-pristine target can only replay an existing exact Kernel
        // receipt; it can never newly import. Preserve lost-response retry even
        // when the stopped source has subsequently gained verified history.
        let replay = handle
            .backend_request("history_import_receipt".into(), params.clone())
            .await
            .map_err(RequestError::from_runtime)?
            .ok_or_else(|| err("Kernel import receipt observation unavailable."))?;
        if replay.get("found") == Some(&JsValue::Bool(true)) {
            return replay
                .get("import")
                .cloned()
                .ok_or_else(|| err("Kernel import receipt projection missing."));
        }
        let current_messages = if let Some(proof) = &legacy_proof {
            proof.messages.clone()
        } else {
            let history = kernel_source
                .as_ref()
                .unwrap()
                .backend_request(
                    "read_session_messages".into(),
                    crate::metadata::object([("sessionKey", JsValue::String(source_key.clone()))]),
                )
                .await
                .map_err(RequestError::from_runtime)?
                .ok_or_else(|| err("Kernel source history unavailable."))?;
            to_json(&history)?["canonicalMessages"].clone()
        };
        let current = envelope(
            json!({"project":project,"sessionKey":source,"backend":actual_backend}),
            current_messages,
        )?;
        if current["sha256"] != transfer["sha256"] {
            return Err(err(
                "Source history changed or transfer provenance does not match the current verified source. Export again before a first import.",
            ));
        }
        let result = handle
            .backend_request("restore_session_transcript".into(), params.clone())
            .await
            .map_err(RequestError::from_runtime)?
            .ok_or_else(|| err("Kernel import unavailable."))?;
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detached_legacy_import_requires_explicit_stop_attestation_without_fabricating_runtime_proof()
    {
        for params in [
            json!({}),
            json!({"sourceRuntimeStopped":false}),
            json!({"sourceRuntimeStopped":"true"}),
        ] {
            assert!(require_legacy_stop_attestation(true, &to_js(&params).unwrap()).is_err());
        }
        assert!(
            require_legacy_stop_attestation(
                true,
                &to_js(&json!({"sourceRuntimeStopped":true})).unwrap()
            )
            .is_ok()
        );
        assert!(require_legacy_stop_attestation(false, &to_js(&json!({})).unwrap()).is_ok());
    }
    fn legacy_fixture(root: &std::path::Path, complete: bool, unknown: bool) -> std::path::PathBuf {
        use std::sync::Arc;
        let path = root.join("old.jsonl");
        let recorder = storage::recorder::Recorder::new(
            Arc::new(storage::JsonlTranscriptStore::new()),
            storage::TranscriptKey {
                project_root: root.into(),
                pilot_home: root.into(),
                session_id: "old".into(),
                transcript_path: path.clone(),
            },
        );
        recorder.start_turn(&"old".into(), &"turn".into()).unwrap();
        recorder.writer().record_accepted_input(&"old".into(),&"turn".into(),vec![to_js(&json!({"role":"user","content":[{"type":"text","text":"verified historical user"}]})).unwrap()],None).unwrap();
        if complete {
            recorder.complete_turn(&"old".into(),&"turn".into(),to_js(&json!({"type":if unknown{"error"}else{"success"},"turnId":"turn","stopReason":if unknown{"result_unknown"}else{"completed"},"usage":{},"permissionDenials":[],"turns":1})).unwrap()).unwrap();
        }
        path
    }
    #[tokio::test]
    async fn legacy_transfer_rejects_partial_diagnostics_open_turn_unknown_and_keeps_bytes() {
        let root = std::env::temp_dir().join(format!("pd-legacy-proof-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = legacy_fixture(&root, true, false);
        let original = std::fs::read(&path).unwrap();
        let verified = verified_legacy_messages(&path, &"old".into()).unwrap();
        assert_eq!(verified.as_array().unwrap().len(), 1);
        assert!(verified_legacy_messages(&path, &"collision".into()).is_err());
        for suffix in [b"not-json\n".as_slice(), b"{\"type\":".as_slice()] {
            let mut corrupt = original.clone();
            corrupt.extend_from_slice(suffix);
            std::fs::write(&path, &corrupt).unwrap();
            assert!(verified_legacy_messages(&path, &"old".into()).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), corrupt);
        }
        std::fs::remove_file(&path).unwrap();
        legacy_fixture(&root, false, false);
        assert!(verified_legacy_messages(&path, &"old".into()).is_err());
        std::fs::remove_file(&path).unwrap();
        legacy_fixture(&root, true, true);
        assert!(verified_legacy_messages(&path, &"old".into()).is_err());
        std::fs::remove_file(&path).unwrap();
        legacy_fixture(&root, true, false);
        let final_bytes = std::fs::read(&path).unwrap();
        let fence = crate::runtime::session::transcript_locks::acquire(&path)
            .await
            .unwrap();
        let external = storage::LedgerLock::acquire(&path).unwrap().1;
        let proof = LegacySourceProof {
            detached: true,
            messages: verified_legacy_messages(&path, &"old".into()).unwrap(),
            _fence: fence,
            _external: Some(external),
            _activity: None,
        };
        assert!(storage::JsonlLedgerStore::open(&path, "old").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), final_bytes);
        drop(proof);
        assert!(storage::JsonlLedgerStore::open(&path, "old").is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_transfer_uses_original_operation_recovery_for_unresolved_effects() {
        let start = json!({"type":"agent_loop_operation_started","sessionId":"old","turnId":"turn","runId":"run","operationId":"operation","requestId":"request","binding":{"moduleInstanceId":"module","connectionGeneration":"generation"}});
        let mut unknown = start.clone();
        unknown["type"] = json!("agent_loop_operation_terminal");
        unknown["outcome"] = json!("result_unknown");
        unknown["streamId"] = json!("original-stream");
        unknown["lastAppliedSequence"] = Value::Null;
        let mut known = unknown.clone();
        known["outcome"] = json!("completed");
        known["lastAppliedSequence"] = json!(2);
        known["result"] = json!({"type":"success"});
        known["messages"] = json!([]);
        let entries = |values: Vec<Value>| {
            values
                .iter()
                .map(|value| to_js(value).unwrap())
                .collect::<Vec<_>>()
        };
        assert!(
            storage::verify_history_quiescent(&"old".into(), &entries(vec![start.clone()]))
                .is_err()
        );
        assert!(
            storage::verify_history_quiescent(
                &"old".into(),
                &entries(vec![start.clone(), unknown.clone()])
            )
            .is_err()
        );
        assert!(
            storage::verify_history_quiescent(&"old".into(), &entries(vec![start, unknown, known]))
                .is_ok()
        );
    }

    #[test]
    fn transfer_rejects_changed_hash_pending_tools_and_active_state() {
        let original = envelope(
            json!({"sessionKey":"old"}),
            json!([{"role":"user","content":[{"type":"text","text":"hello"}]}]),
        )
        .unwrap();
        verify_envelope(&original).unwrap();
        let mut changed = original.clone();
        changed["messages"][0]["content"][0]["text"] = json!("changed");
        assert!(verify_envelope(&changed).is_err());
        assert!(envelope(json!({}),json!([{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"write","input":{}}]}])).is_err());
        let mut active = original;
        active["resume"] = json!({"execution_id":"old"});
        let mut body = active.clone();
        body.as_object_mut().unwrap().remove("sha256");
        active["sha256"] = json!(digest(&body).unwrap());
        assert!(verify_envelope(&active).is_err());
    }
    #[test]
    fn verified_tool_pairs_and_attachment_bytes_roundtrip() {
        let value=envelope(json!({}),json!([
            {"role":"assistant","content":[{"type":"tool_use","id":"t","name":"read","input":{}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"result"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"aGVsbG8="}}]}
        ])).unwrap();
        verify_envelope(&value).unwrap();
        assert_eq!(value["attachments"].as_array().unwrap().len(), 1);
        let mut changed = value;
        changed["attachments"][0]["sha256"] = json!("bad");
        let mut body = changed.clone();
        body.as_object_mut().unwrap().remove("sha256");
        changed["sha256"] = json!(digest(&body).unwrap());
        assert!(verify_envelope(&changed).is_err());
    }
    #[test]
    fn kernel_history_transfer_rejects_ambiguous_modern_display_ids_and_malformed_timeline() {
        let base = json!([{"role":"user","metadata":{"kernelMessageId":"user","turnId":"root","createdAt":"2026-11-08T00:00:00Z"},"content":[{"type":"text","text":"same"}]},{"role":"assistant","metadata":{"kernelMessageId":"answer","turnId":"root"},"content":[{"type":"text","text":"answer","blockId":"block","timeline":{"version":1,"turnId":"internal-root","id":"block","order":0,"revision":4}}]}]);
        assert!(verify_messages(&base).is_ok());
        let tools = json!([{"role":"assistant","metadata":{"turnId":"root"},"content":[{"type":"tool_call","id":"call","name":"read","input":{},"timeline":{"version":1,"turnId":"internal","id":"tool:call","order":0,"revision":1}}]},{"role":"user","metadata":{"turnId":"root"},"content":[{"type":"tool_result","toolCallId":"call","content":[{"type":"text","text":"result"}],"timeline":{"version":1,"turnId":"internal","id":"tool:call","order":0,"revision":2}}]}]);
        assert!(verify_messages(&tools).is_ok());

        let mut duplicate = base.clone();
        duplicate[1]["metadata"]["kernelMessageId"] = json!("user");
        assert!(verify_messages(&duplicate).is_err());
        let mut blank = base.clone();
        blank[0]["metadata"]["kernelMessageId"] = json!(" ");
        assert!(verify_messages(&blank).is_err());
        let mut time = base.clone();
        time[0]["metadata"]["createdAt"] = json!("invalid");
        assert!(verify_messages(&time).is_err());
        let mut malformed = base.clone();
        malformed[1]["content"][0]["timeline"]["revision"] = json!(-1);
        assert!(verify_messages(&malformed).is_err());
        let mut duplicate_block = base.clone();
        let mut answer = base[1].clone();
        answer["metadata"]["kernelMessageId"] = json!("other-answer");
        duplicate_block.as_array_mut().unwrap().push(answer);
        assert!(verify_messages(&duplicate_block).is_err());
        duplicate_block[2]["metadata"]["turnId"] = json!("other-root");
        assert!(verify_messages(&duplicate_block).is_ok());
        let legacy = json!([{"role":"assistant","content":[{"type":"text","text":"one","blockId":"legacy-reused"}]},{"role":"assistant","content":[{"type":"text","text":"two","blockId":"legacy-reused"}]}]);
        assert!(verify_messages(&legacy).is_ok());
        #[cfg(unix)]
        {
            let rows = crate::runtime::session::kernel_projection::web_messages(
                &legacy,
                "new-target",
                None,
                &json!({}),
            );
            assert_ne!(
                rows["messages"][0]["blockId"],
                rows["messages"][1]["blockId"]
            );
            assert_eq!(legacy[0]["content"][0]["blockId"], "legacy-reused");
        }
    }
    #[test]
    fn canonical_native_assistant_metadata_thinking_tools_and_media_roundtrip() {
        let messages = json!([
            {"role":"assistant","metadata":{"model":"test-model"},"content":[{"type":"thinking","text":"reason","reasoningContent":"reason","signature":"sig"},{"type":"tool_call","id":"native-tool","name":"read_file","input":{"path":"safe"}}]},
            {"role":"user","content":[{"type":"tool_result","toolCallId":"native-tool","content":[{"type":"text","text":"read result"},{"type":"image","source":"base64","data":"aGVsbG8=","mimeType":"image/png","bytes":5},{"type":"pdf","data":"aGVsbG8=","bytes":5,"pages":1}]}]},
            {"role":"assistant","metadata":{"model":"test-model"},"content":[{"type":"text","text":"complete"}]}
        ]);
        let exported = envelope(json!({"sessionKey":"original"}), messages.clone()).unwrap();
        verify_envelope(&exported).unwrap();
        assert_eq!(exported["messages"], messages);
        assert_eq!(exported["attachments"].as_array().unwrap().len(), 2);
        let mut unsafe_history = exported;
        unsafe_history["messages"][0]["metadata"]["activeHandle"] = json!("must-not-resume");
        let mut body = unsafe_history.clone();
        body.as_object_mut().unwrap().remove("sha256");
        unsafe_history["sha256"] = json!(digest(&body).unwrap());
        assert!(verify_envelope(&unsafe_history).is_err());
    }
}

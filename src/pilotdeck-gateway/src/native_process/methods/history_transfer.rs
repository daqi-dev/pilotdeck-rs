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

/// Validates complete tool pairs and inline attachment bytes; unknown blocks
/// cannot smuggle mutable file references or active runtime state into history.
fn verify_messages(messages: &Value) -> Result<Vec<Value>, RequestError> {
    let messages = messages
        .as_array()
        .ok_or_else(|| err("Transfer messages must be an array."))?;
    let mut tools = BTreeSet::new();
    let mut attachments = Vec::new();
    for (m, message) in messages.iter().enumerate() {
        if !matches!(
            message["role"].as_str(),
            Some("user" | "assistant" | "system" | "tool")
        ) {
            return Err(err("Transfer message role is unsupported."));
        }
        if message.as_object().is_none_or(|o| {
            o.keys().any(|k| {
                !matches!(
                    k.as_str(),
                    "role" | "content" | "id" | "name" | "tool_call_id"
                )
            })
        }) {
            return Err(err(
                "Transfer message contains runtime or unverified metadata.",
            ));
        }
        if message["content"].is_string() {
            continue;
        }
        let blocks = message["content"]
            .as_array()
            .ok_or_else(|| err("Transfer content must be text or blocks."))?;
        for (b, block) in blocks.iter().enumerate() {
            match block["type"]
                .as_str()
                .ok_or_else(|| err("Transfer block type missing."))?
            {
                "text" => {
                    if !block["text"].is_string() {
                        return Err(err("Invalid text block."));
                    }
                }
                "thinking" => {
                    if !block["thinking"].is_string() {
                        return Err(err("Invalid thinking block."));
                    }
                }
                "tool_use" | "tool_call" => {
                    let id = block["id"]
                        .as_str()
                        .ok_or_else(|| err("Tool call identity missing."))?;
                    if message["role"] != "assistant" || !tools.insert(id.to_string()) {
                        return Err(err("Duplicate or misplaced tool call."));
                    }
                }
                "tool_result" => {
                    let id = block["tool_use_id"]
                        .as_str()
                        .or_else(|| block["toolCallId"].as_str())
                        .ok_or_else(|| err("Tool result identity missing."))?;
                    if !tools.remove(id) {
                        return Err(err("Tool result has no verified preceding call."));
                    }
                    if !block["content"].is_string()
                        && !block["content"].as_array().is_some_and(|parts| {
                            parts
                                .iter()
                                .all(|p| p["type"] == "text" && p["text"].is_string())
                        })
                    {
                        return Err(err(
                            "Only verified inline tool result text can be imported.",
                        ));
                    }
                }
                "image" | "document" => {
                    let source = &block["source"];
                    if source["type"] != "base64" {
                        return Err(err(
                            "Attachments must contain inline base64 bytes; mutable paths/URLs cannot be imported.",
                        ));
                    }
                    let data = source["data"]
                        .as_str()
                        .ok_or_else(|| err("Attachment bytes missing."))?;
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .map_err(|_| err("Attachment base64 is invalid."))?;
                    attachments.push(json!({"message":m,"block":b,"sha256":format!("{:x}",Sha256::digest(bytes))}));
                }
                _ => {
                    return Err(err(
                        "Unknown or unverified history block cannot be imported.",
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
impl Requests {
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
        let saved = match std::fs::read(std::path::PathBuf::from(path)) {
            Ok(bytes) => {
                Some(serde_json::from_slice::<Value>(&bytes).map_err(|e| err(e.to_string()))?)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(err(e.to_string())),
        };
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
                .map_err(|e| err(e.message))?
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
                .map_err(|e| err(e.message))?
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
                    .map_err(|e| err(e.message))?
                    .ok_or_else(|| err("Kernel history unavailable."))?;
                let history = to_json(&history)?;
                (
                    "kernel",
                    history
                        .get("messages")
                        .or_else(|| history.get("history"))
                        .cloned()
                        .unwrap_or(json!([])),
                )
            } else {
                let path = super::super::assembly::transcript_path(&self.options, &session);
                let _fence = crate::runtime::session::transcript_locks::acquire(&path)
                    .await
                    .map_err(|e| err(String::from_utf16_lossy(e.message.units())))?;
                let _external_lock = if self.owner.current_handle(&session).is_none() {
                    Some(
                        storage::LedgerLock::acquire(&path)
                            .map_err(|e| err(format!("Source writer is not quiescent: {e}")))?
                            .1,
                    )
                } else {
                    None
                };
                let read = storage::read_transcript(&path).map_err(|e| err(e.to_string()))?;
                read.ensure_writable().map_err(|e| err(e.to_string()))?;
                if read.entries.is_empty() {
                    return Err(err("Source transcript does not exist or is empty."));
                }
                let projected = factory::project(&read.entries).map_err(|e| err(e.message))?;
                ("legacy", to_json(&JsValue::Array(projected.messages))?)
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
        self.assert_kernel_quiescent(&source_key, params).await?;
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
        let result = handle
            .backend_request("restore_session_transcript".into(), params.clone())
            .await
            .map_err(|e| err(e.message))?
            .ok_or_else(|| err("Kernel import unavailable."))?;
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
}

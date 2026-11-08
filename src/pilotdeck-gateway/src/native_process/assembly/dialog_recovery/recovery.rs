//! Restart answers append through the same writer fence as live metadata.
use super::NativeDialogRecovery;
use crate::{metadata::object, native_process::requests::Requests,
    runtime::{connection::RequestError, session::transcript_locks},
    storage::{AsyncTranscriptAdapter, JsonlTranscriptStore, ProjectKey, SessionStorageInput},
    user_dialog::{DialogAnswer, GatewayUserDialogJournal}};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{path::{Path, PathBuf}, sync::Arc};

fn error(value: impl ToString) -> RequestError { RequestError::new(value.to_string()) }
fn journal(home: &Path, chat: &Path, session: &JsString) -> Result<GatewayUserDialogJournal, RequestError> {
    let path = chat.join(format!("{}.jsonl.dialogs.json", crate::storage::sanitize_session_id(session)));
    let journal = GatewayUserDialogJournal::new(path, Default::default());
    #[cfg(unix)]
    let journal = journal.with_native_home(home).map_err(error)?;
    Ok(journal)
}
fn request_id(dialog: &JsValue) -> Option<&JsString> {
    dialog.get("request").and_then(|v| v.get("requestId")).and_then(JsValue::as_string)
}
impl NativeDialogRecovery {
    async fn selected(&self, requests: &Requests, params: &JsValue, session: &JsString) -> Result<Option<PathBuf>, RequestError> {
        let home = self.home.clone();
        let root = params.get("projectKey").filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined)).cloned()
            .unwrap_or_else(|| requests.options.project.to_string_lossy().as_ref().into());
        let root = super::blocking(move || super::resolve_registered(&home, &root)).await.map_err(|e| RequestError { code: e.code, message: e.message.into(), details: None, validation: None })?;
        if self.sdk.registry.lock().await.is_ephemeral_session(session) { return Ok(None); }
        Ok(Some(root))
    }
    pub(crate) async fn list(&self, requests: &Requests, params: &JsValue, session: &JsString) -> Result<Vec<JsValue>, RequestError> {
        let Some(project) = self.selected(requests, params, session).await? else { return Ok(Vec::new()); };
        let (chat, permit) = crate::native_process::methods::session_dir::resolve_chat_dir(self.home.clone(), project.clone()).await?;
        let home = self.home.clone(); let session = session.clone(); let cache = self.cache.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut dialogs = journal(&home, &chat, &session)?.recover().map_err(error)?;
            let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
            let key = (project, session);
            if let Some(prior) = cache.get(&key) {
                for dialog in &mut dialogs {
                    if let Some(timestamp) = prior.iter().find(|p| request_id(p) == request_id(dialog))
                        .and_then(|p| p.get("terminatedAt")).cloned()
                        && let JsValue::Object(value) = dialog { value.insert("terminatedAt".into(), timestamp); }
                }
            }
            if dialogs.is_empty() { cache.remove(&key); } else { cache.insert(key, dialogs.clone()); }
            Ok(dialogs)
        }).await.map_err(error)?
    }
    pub(crate) async fn recover_answer(self: &Arc<Self>, requests: &Requests, params: &JsValue, session: &JsString,
        id: &JsString, answer: &DialogAnswer) -> Result<bool, RequestError> {
        let Some(project) = self.selected(requests, params, session).await? else { return Ok(false); };
        let key = (project.clone(), session.clone(), id.clone());
        let mut receiver = {
            let mut flights = self.flights.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = flights.get(&key) { existing.clone() } else {
                let (sender, receiver) = tokio::sync::watch::channel(None);
                flights.insert(key.clone(), receiver.clone());
                let owner = self.clone(); let requests = requests.clone();
                let session = session.clone(); let id = id.clone(); let answer = answer.clone();
                tokio::spawn(async move {
                    let result = owner.recover_answer_inner(&requests, project, &session, &id, &answer).await;
                    sender.send_replace(Some(result));
                    owner.flights.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
                });
                receiver
            }
        };
        loop {
            let result = receiver.borrow().clone();
            if let Some(result) = result { return result; }
            receiver.changed().await.map_err(|_| error("Dialog recovery owner stopped before completing its write."))?;
        }
    }
    async fn recover_answer_inner(&self, requests: &Requests, project: PathBuf, session: &JsString,
        id: &JsString, answer: &DialogAnswer) -> Result<bool, RequestError> {
        let (chat, permit) = crate::native_process::methods::session_dir::resolve_chat_dir(self.home.clone(), project.clone()).await?;
        let path = chat.join(format!("{}.jsonl", crate::storage::sanitize_session_id(session)));
        let fence = transcript_locks::acquire(&path).await?;
        let home = self.home.clone(); let session = session.clone(); let id = id.clone();
        let answer = answer.clone(); let recorders = requests.recorders.clone(); let cache = self.cache.clone();
        // The write, fence and IO permit survive a caller disconnect together.
        tokio::task::spawn_blocking(move || {
            let (_permit, _fence) = (permit, fence);
            let journal = journal(&home, &chat, &session)?;
            let dialogs = journal.recover().map_err(error)?;
            let Some(dialog) = dialogs.iter().find(|v| request_id(v) == Some(&id)) else { return Ok(false); };
            let request = dialog.get("request").expect("validated recovered request");
            if let DialogAnswer::Answered(value) = &answer && !accepts(request, value) {
                let kind = request.get("dialogKind").and_then(JsValue::as_string).map(|v| String::from_utf16_lossy(v.units())).unwrap_or_default();
                return Err(RequestError { code: "INVALID_USER_DIALOG_RESPONSE".into(),
                    message: format!("answered {kind} dialog result does not match the recovered dialog contract.").into(), details: None, validation: None });
            }
            let adapter = AsyncTranscriptAdapter::new(Arc::new(JsonlTranscriptStore::new_bounded()), Arc::new(move |_: &ProjectKey| chat.clone()));
            let storage = adapter.create_session_storage(SessionStorageInput {
                project_root: project.clone(), pilot_home: home, session_id: session.clone(), now: None,
            }).map_err(error)?;
            let live = crate::native_process::assembly::live_recorder(&recorders, &session);
            let cold = if live.is_none() { Some(transcript_locks::cold_write_lease(&path).ok_or_else(transcript_locks::writer_busy_error)?) } else { None };
            let _cold = cold;
            let entries = storage.read_transcript(Some(crate::storage::TRANSCRIPT_MAX_BYTES)).map_err(error)?.entries;
            let writer = if let Some((_, recorder)) = &live {
                transcript_locks::ensure_recorder_matches(&path, recorder)?;
                recorder.writer().as_ref()
            } else {
                storage.restore().map_err(error)?;
                &storage.transcript
            };
            let mut purpose = "gateway_user_dialog_recovery:".encode_utf16().collect::<Vec<_>>(); purpose.extend_from_slice(id.units());
            let purpose = JsString::from_utf16(purpose);
            let prior = entries.iter().find(|entry| {
                entry.get("type").and_then(JsValue::as_string).is_some_and(|v| v.equals_str("durable_message") || v.equals_str("tool_result_message"))
                    && entry.get("message").and_then(|v| v.get("metadata")).and_then(|v| v.get("purpose")) == Some(&JsValue::String(purpose.clone()))
            });
            let turn = prior.and_then(|v| v.get("turnId")).and_then(JsValue::as_string).cloned()
                .unwrap_or_else(|| format!("gateway:dialog-recovery:{}", uuid::Uuid::new_v4()).into());
            let now = pilotdeck_transport::system_clock::format_epoch_iso(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64);
            if prior.is_none() {
                let message = object([("role", "user".into()), ("content", JsValue::Array(vec![object([("type", "text".into()), ("text", JsValue::String(format_answer(request, &answer)))])])),
                    ("metadata", object([("synthetic", JsValue::Bool(true)), ("purpose", JsValue::String(purpose)),
                        ("toolCallId", request.get("toolCallId").cloned().unwrap_or(JsValue::Undefined))]))]);
                let mut draft = JsObject::new(); draft.insert("type".into(), "durable_message".into()); draft.insert("message".into(), message);
                writer.record_session_event_checked(&session, &turn, draft).map_err(error)?;
            }
            // If a previous attempt persisted the message but failed the second
            // append, finish its original recovery turn before acknowledging.
            if !entries.iter().any(|v| v.get("type") == Some(&"turn_result".into()) && v.get("turnId") == Some(&JsValue::String(turn.clone()))) {
                let result = object([("type", "aborted".into()), ("sessionId", JsValue::String(session.clone())), ("turnId", JsValue::String(turn.clone())),
                    ("stopReason", "aborted_tools".into()), ("usage", object([])), ("permissionDenials", JsValue::Array(vec![])),
                    ("turns", JsValue::Number(0.0)), ("startedAt", now.as_str().into()), ("completedAt", now.as_str().into())]);
                let mut draft = JsObject::new(); draft.insert("type".into(), "turn_result".into()); draft.insert("result".into(), result);
                writer.record_session_event_checked(&session, &turn, draft).map_err(error)?;
            }
            journal.remove(&id).map_err(error)?;
            cache.lock().unwrap_or_else(|e| e.into_inner()).remove(&(project, session));
            Ok(true)
        }).await.map_err(error)?
    }
}
fn accepts(request: &JsValue, value: &JsValue) -> bool {
    let kind = request.get("dialogKind").and_then(JsValue::as_string);
    if kind.is_some_and(|v| v.equals_str("input")) { return value.as_string().is_some(); }
    if kind.is_some_and(|v| v.equals_str("confirm")) { return matches!(value, JsValue::Bool(_)); }
    if kind.is_some_and(|v| v.equals_str("select")) {
        return value.as_string().is_some() && matches!(request.get("choices"), Some(JsValue::Array(choices)) if choices.iter().any(|choice| choice.get("value") == Some(value)));
    }
    value.as_object().is_some() && request.get("schema").is_some_and(|schema|
        crate::runtime::tools::builtin::form_schema::accepts_form_dialog_answer(schema, value))
}
fn format_answer(request: &JsValue, answer: &DialogAnswer) -> JsString {
    let mut out = "[Gateway restart dialog recovery]\nThe Gateway restarted while waiting for ".encode_utf16().collect::<Vec<_>>();
    append_value(&mut out, request.get("toolName"));out.extend(".\nQuestion: ".encode_utf16());append_value(&mut out, request.get("prompt"));out.push(10);
    match answer {
        DialogAnswer::Cancelled(reason) => { out.extend("The user cancelled the dialog".encode_utf16());
            if let Some(reason) = reason.as_ref().filter(|v| !v.is_empty()) { out.extend(": ".encode_utf16());out.extend_from_slice(reason.units()); } else { out.push(46); } }
        DialogAnswer::Answered(value) => {
            let kind = request.get("dialogKind").and_then(JsValue::as_string);
            if kind.is_some_and(|v| v.equals_str("confirm")) { out.extend(if value == &JsValue::Bool(true) { "The user confirmed: yes" } else { "The user confirmed: no" }.encode_utf16()); }
            else if kind.is_some_and(|v| v.equals_str("input") || v.equals_str("select")) {
                out.extend(if kind.unwrap().equals_str("input") { "The user answered: " } else { "The user selected: " }.encode_utf16());append_value(&mut out, Some(value));
            } else { out.extend("The user submitted: ".encode_utf16());
                out.extend(pilotdeck_protocol::stringify_json(value).ok().flatten().unwrap_or_else(|| "undefined".into()).encode_utf16()); }
        }
    }
    JsString::from_utf16(out)
}
fn append_value(out: &mut Vec<u16>, value: Option<&JsValue>) {
    if let Some(JsValue::String(value)) = value { out.extend_from_slice(value.units()); }
    else { out.extend("undefined".encode_utf16()); }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(kind: &str) -> JsValue {
        object([("type", "user_dialog_request".into()), ("requestId", "dialog".into()),
            ("dialogKind", kind.into()), ("toolName", "request_user_input".into()),
            ("toolCallId", "tool-call".into()), ("prompt", "Question?".into())])
    }
    async fn cleanup(requests: &Requests) {
        requests.owner.shutdown().await.unwrap(); requests.project.dispose().await.unwrap();
    }
    #[tokio::test]
    async fn native_dialog_restart_list_answer_and_idempotent_transcript_are_real() {
        let (_scratch, mut requests) = crate::native_process::management_tests::setup();
        let port = Arc::new(super::super::tests::recovery(&requests));
        requests.dialog_recovery = Some(port.clone());
        let session = JsString::from("recovery-native");
        let chat = crate::native_process::methods::session_dir::bounded_chat_dir(&requests.options.home, &requests.options.project).unwrap();
        journal(&requests.options.home, &chat, &session).unwrap().record(&request("input")).unwrap();
        let params = object([("sessionKey", JsValue::String(session.clone()))]);
        let first = requests.user_dialog_request("user_dialog_list".into(), params.clone()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let second = requests.user_dialog_request("user_dialog_list".into(), params.clone()).await.unwrap();
        assert_eq!(first, second, "restart timestamps stay stable across rereads");
        let answer = JsString::from_utf16(vec![0xd800, 0x4e2d]);
        let input = object([("sessionKey", JsValue::String(session.clone())), ("requestId", "dialog".into()),
            ("result", object([("behavior", "answered".into()), ("value", JsValue::String(answer.clone()))]))]);
        let delivered = requests.user_dialog_request("user_dialog_respond".into(), input.clone()).await.unwrap();
        assert_eq!(delivered, object([("delivered", JsValue::Bool(true)), ("recovered", JsValue::Bool(true)), ("reason", "gateway_restarted".into())]));
        assert_eq!(requests.user_dialog_request("user_dialog_list".into(), params).await.unwrap(), object([("dialogs", JsValue::Array(vec![]))]));
        assert_eq!(requests.user_dialog_request("user_dialog_respond".into(), input).await.unwrap(), object([("delivered", JsValue::Bool(false))]));
        let bytes = std::fs::read_to_string(chat.join("recovery-native.jsonl")).unwrap();
        let rows: Vec<_> = bytes.lines().map(|line| pilotdeck_protocol::parse_json(line).unwrap()).collect();
        assert_eq!(rows.len(), 2, "exactly one durable answer and its recovery turn result");
        assert_eq!(rows[0].get("sequence"), Some(&JsValue::Number(1.0))); assert_eq!(rows[1].get("sequence"), Some(&JsValue::Number(2.0)));
        let text = rows[0].get("message").unwrap().get("content").unwrap();
        let JsValue::Array(blocks) = text else { panic!("message blocks") };
        assert!(blocks[0].get("text").and_then(JsValue::as_string).unwrap().units().ends_with(answer.units()));
        assert_eq!(rows[1].get("result").unwrap().get("stopReason"), Some(&"aborted_tools".into()));
        cleanup(&requests).await;
    }
    #[tokio::test]
    async fn native_dialog_invalid_answer_and_busy_writer_preserve_recoverable_request() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let port = Arc::new(super::super::tests::recovery(&requests)); let session = JsString::from("blocked-recovery");
        let chat = crate::native_process::methods::session_dir::bounded_chat_dir(&requests.options.home, &requests.options.project).unwrap();
        let journal = journal(&requests.options.home, &chat, &session).unwrap();journal.record(&request("confirm")).unwrap();
        let params = object([]);
        let failure = port.recover_answer(&requests, &params, &session, &"dialog".into(), &DialogAnswer::Answered("yes".into())).await.unwrap_err();
        assert_eq!(failure.code, "INVALID_USER_DIALOG_RESPONSE"); assert_eq!(journal.recover().unwrap().len(), 1);
        let path = chat.join("blocked-recovery.jsonl");
        let lease = transcript_locks::cold_write_lease(&path).unwrap();
        let failure = port.recover_answer(&requests, &params, &session, &"dialog".into(), &DialogAnswer::Answered(JsValue::Bool(true))).await.unwrap_err();
        assert_eq!(failure.code, "SESSION_WRITER_BUSY"); assert_eq!(journal.recover().unwrap().len(), 1);
        drop(lease);
        assert!(port.recover_answer(&requests, &params, &session, &"dialog".into(), &DialogAnswer::Answered(JsValue::Bool(true))).await.unwrap());
        cleanup(&requests).await;
    }
    #[test]
    fn native_recovery_formats_cancel_choice_and_form_as_the_legacy_context() {
        let input = request("input");
        assert_eq!(format_answer(&input, &DialogAnswer::Cancelled(None)).to_utf8().unwrap(),
            "[Gateway restart dialog recovery]\nThe Gateway restarted while waiting for request_user_input.\nQuestion: Question?\nThe user cancelled the dialog.");
        assert!(format_answer(&request("select"), &DialogAnswer::Answered("choice".into())).to_utf8().unwrap().ends_with("The user selected: choice"));
        assert!(format_answer(&request("form"), &DialogAnswer::Answered(object([("value", JsValue::Bool(true))]))).to_utf8().unwrap().ends_with("The user submitted: {\"value\":true}"));
    }
}

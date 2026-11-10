//! Restart answers append through the same writer fence as live metadata.
use super::NativeDialogRecovery;
use crate::{
    metadata::object,
    native_process::requests::Requests,
    runtime::{connection::RequestError, session::transcript_locks},
    storage::{AsyncTranscriptAdapter, JsonlTranscriptStore, ProjectKey, SessionStorageInput},
    user_dialog::{DialogAnswer, GatewayUserDialogJournal},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub(crate) enum RecoveryResult {
    Recovered(bool),
    Live(Arc<crate::user_dialog::ResponseSnapshot>),
}
pub(super) struct RecoveryFlight {
    answer: Arc<DialogAnswer>,
    receiver: tokio::sync::watch::Receiver<Option<Result<RecoveryResult, RequestError>>>,
    #[cfg(test)]
    joined: usize,
}
// JSON-like equality with a bounded iterative walk. In particular an answer
// cannot make a duplicate flight hold the host executor or the flight mutex.
fn same_answer(a: &DialogAnswer, b: &DialogAnswer) -> bool {
    let (a, b) = match (a, b) {
        (DialogAnswer::Cancelled(a), DialogAnswer::Cancelled(b)) => return a == b,
        (DialogAnswer::Answered(a), DialogAnswer::Answered(b)) => (a, b),
        _ => return false,
    };
    let mut work = 0usize;
    let mut stack = vec![(a, b)];
    while let Some((a, b)) = stack.pop() {
        work = work.saturating_add(1);
        if work > 100_000 {
            return false;
        }
        match (a, b) {
            (JsValue::String(a), JsValue::String(b)) => {
                work = work
                    .saturating_add(a.len_utf16())
                    .saturating_add(b.len_utf16());
                if work > 8 * 1024 * 1024 || a != b {
                    return false;
                }
            }
            (JsValue::Array(a), JsValue::Array(b)) => {
                if a.len() != b.len() || a.len() > 100_000usize.saturating_sub(work) {
                    return false;
                }
                stack.extend(a.iter().zip(b.iter()));
            }
            (JsValue::Object(a), JsValue::Object(b)) => {
                let entries = a.own_entries();
                if entries.len() != b.own_entries().len()
                    || entries.len() > 100_000usize.saturating_sub(work)
                {
                    return false;
                }
                for (k, v) in entries {
                    work = work.saturating_add(k.len_utf16());
                    if work > 8 * 1024 * 1024 {
                        return false;
                    }
                    let Some(other) = b.get_key(k) else {
                        return false;
                    };
                    stack.push((v, other));
                }
            }
            _ if a == b => {}
            _ => return false,
        }
    }
    true
}
fn error(value: impl ToString) -> RequestError {
    RequestError::new(value.to_string())
}
fn journal(
    home: &Path,
    chat: &Path,
    session: &JsString,
) -> Result<GatewayUserDialogJournal, RequestError> {
    let path = chat.join(format!(
        "{}.jsonl.dialogs.json",
        crate::storage::sanitize_session_id(session)
    ));
    let journal = GatewayUserDialogJournal::new(path, Default::default());
    #[cfg(unix)]
    let journal = journal.with_native_home(home).map_err(error)?;
    Ok(journal)
}
fn request_id(dialog: &JsValue) -> Option<&JsString> {
    dialog
        .get("request")
        .and_then(|v| v.get("requestId"))
        .and_then(JsValue::as_string)
}
impl NativeDialogRecovery {
    async fn selected(
        &self,
        requests: &Requests,
        params: &JsValue,
        session: &JsString,
    ) -> Result<Option<PathBuf>, RequestError> {
        let home = self.home.clone();
        let root = params
            .get("projectKey")
            .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
            .cloned()
            .unwrap_or_else(|| requests.options.project.to_string_lossy().as_ref().into());
        let root = super::blocking(move || super::resolve_registered(&home, &root))
            .await
            .map_err(|e| RequestError {
                code: e.code,
                message: e.message.into(),
                details: None,
                validation: None,
            })?;
        if self.sdk.registry.lock().await.is_ephemeral_session(session) {
            return Ok(None);
        }
        Ok(Some(root))
    }
    pub(crate) async fn list(
        &self,
        requests: &Requests,
        params: &JsValue,
        session: &JsString,
    ) -> Result<Vec<JsValue>, RequestError> {
        let Some(project) = self.selected(requests, params, session).await? else {
            return Ok(Vec::new());
        };
        let (chat, permit) = crate::native_process::methods::session_dir::resolve_chat_dir(
            self.home.clone(),
            project.clone(),
        )
        .await?;
        let home = self.home.clone();
        let session = session.clone();
        let cache = self.cache.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut dialogs = journal(&home, &chat, &session)?.recover().map_err(error)?;
            let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
            let key = (project, session);
            if let Some(prior) = cache.get(&key) {
                for dialog in &mut dialogs {
                    if let Some(timestamp) = prior
                        .iter()
                        .find(|p| request_id(p) == request_id(dialog))
                        .and_then(|p| p.get("terminatedAt"))
                        .cloned()
                        && let JsValue::Object(value) = dialog
                    {
                        value.insert("terminatedAt".into(), timestamp);
                    }
                }
            }
            if dialogs.is_empty() {
                cache.remove(&key);
            } else {
                cache.insert(key, dialogs.clone());
            }
            Ok(dialogs)
        })
        .await
        .map_err(error)?
    }
    #[cfg(test)]
    pub(crate) async fn recover_answer(
        self: &Arc<Self>,
        requests: &Requests,
        params: &JsValue,
        session: &JsString,
        id: &JsString,
        answer: &DialogAnswer,
    ) -> Result<bool, RequestError> {
        Ok(matches!(
            self.recover_or_live(requests, params, session, id, answer)
                .await?,
            RecoveryResult::Recovered(true)
        ))
    }
    pub(crate) async fn recover_or_live(
        self: &Arc<Self>,
        requests: &Requests,
        params: &JsValue,
        session: &JsString,
        id: &JsString,
        answer: &DialogAnswer,
    ) -> Result<RecoveryResult, RequestError> {
        let Some(project) = self.selected(requests, params, session).await? else {
            return Ok(RecoveryResult::Recovered(false));
        };
        let key = (project.clone(), session.clone(), id.clone());
        let (mut receiver, existing) = {
            let mut flights = self.flights.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = flights.get_mut(&key) {
                #[cfg(test)]
                {
                    existing.joined += 1;
                }
                (existing.receiver.clone(), Some(existing.answer.clone()))
            } else {
                let (sender, receiver) = tokio::sync::watch::channel(None);
                let admitted = Arc::new(answer.clone());
                flights.insert(
                    key.clone(),
                    RecoveryFlight {
                        answer: admitted.clone(),
                        receiver: receiver.clone(),
                        #[cfg(test)]
                        joined: 1,
                    },
                );
                let owner = self.clone();
                let requests = requests.clone();
                let session = session.clone();
                let id = id.clone();
                tokio::spawn(async move {
                    let result = owner
                        .recover_answer_inner(&requests, project, &session, &id, &admitted)
                        .await;
                    sender.send_replace(Some(result));
                    owner
                        .flights
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&key);
                });
                (receiver, None)
            }
        };
        let same = if let Some(existing) = existing {
            let proposed = answer.clone();
            super::blocking(move || Ok(same_answer(&existing, &proposed)))
                .await
                .map_err(|error| RequestError {
                    code: error.code,
                    message: error.message.into(),
                    details: None,
                    validation: None,
                })?
        } else {
            true
        };
        loop {
            let result = receiver.borrow().clone();
            if let Some(result) = result {
                return match result? {
                    RecoveryResult::Live(snapshot) => Ok(RecoveryResult::Live(snapshot)),
                    RecoveryResult::Recovered(delivered) => {
                        Ok(RecoveryResult::Recovered(same && delivered))
                    }
                };
            }
            receiver
                .changed()
                .await
                .map_err(|_| error("Dialog recovery owner stopped before completing its write."))?;
        }
    }
    async fn recover_answer_inner(
        &self,
        requests: &Requests,
        project: PathBuf,
        session: &JsString,
        id: &JsString,
        answer: &DialogAnswer,
    ) -> Result<RecoveryResult, RequestError> {
        let gate = crate::runtime::tools::builtin::user_dialog::dialog_start_gate(session).await;
        let snapshot = requests
            .user_dialog_bus
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot_for_response(session, id);
        if let Some(snapshot) = snapshot {
            return Ok(RecoveryResult::Live(Arc::new(snapshot)));
        }
        let (chat, permit) = crate::native_process::methods::session_dir::resolve_chat_dir(
            self.home.clone(),
            project.clone(),
        )
        .await?;
        let path = chat.join(format!(
            "{}.jsonl",
            crate::storage::sanitize_session_id(session)
        ));
        let fence = transcript_locks::acquire(&path).await?;
        let home = self.home.clone();
        let session = session.clone();
        let id = id.clone();
        let answer = answer.clone();
        let recorders = requests.recorders.clone();
        let cache = self.cache.clone();
        // The write, fence and IO permit survive a caller disconnect together.
        tokio::task::spawn_blocking(move || {
            let (_permit, _fence, _gate) = (permit, fence, gate);
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
            let cold = if live.is_none() { Some(transcript_locks::cold_write_lease(&path)?.ok_or_else(transcript_locks::writer_busy_error)?) } else { None };
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
            // A previous partial attempt may have written the synthetic answer
            // but failed before terminal/journal cleanup. Never acknowledge a
            // different answer by finishing that earlier attempt.
            if let Some(prior) = prior {
                let text = prior.get("message").and_then(|v| v.get("content"))
                    .and_then(|content| match content { JsValue::Array(blocks) => blocks.first(), _ => None })
                    .and_then(|block| block.get("text")).and_then(JsValue::as_string);
                if text != Some(&format_answer(request, &answer)) { return Ok(false); }
            }
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
        }).await.map_err(error)?.map(RecoveryResult::Recovered)
    }
}
fn accepts(request: &JsValue, value: &JsValue) -> bool {
    let kind = request.get("dialogKind").and_then(JsValue::as_string);
    if kind.is_some_and(|v| v.equals_str("input")) {
        return value.as_string().is_some();
    }
    if kind.is_some_and(|v| v.equals_str("confirm")) {
        return matches!(value, JsValue::Bool(_));
    }
    if kind.is_some_and(|v| v.equals_str("select")) {
        return value.as_string().is_some()
            && matches!(request.get("choices"), Some(JsValue::Array(choices)) if choices.iter().any(|choice| choice.get("value") == Some(value)));
    }
    value.as_object().is_some()
        && request.get("schema").is_some_and(|schema| {
            crate::runtime::tools::builtin::form_schema::accepts_form_dialog_answer(schema, value)
        })
}
fn format_answer(request: &JsValue, answer: &DialogAnswer) -> JsString {
    let mut out = "[Gateway restart dialog recovery]\nThe Gateway restarted while waiting for "
        .encode_utf16()
        .collect::<Vec<_>>();
    append_value(&mut out, request.get("toolName"));
    out.extend(".\nQuestion: ".encode_utf16());
    append_value(&mut out, request.get("prompt"));
    out.push(10);
    match answer {
        DialogAnswer::Cancelled(reason) => {
            out.extend("The user cancelled the dialog".encode_utf16());
            if let Some(reason) = reason.as_ref().filter(|v| !v.is_empty()) {
                out.extend(": ".encode_utf16());
                out.extend_from_slice(reason.units());
            } else {
                out.push(46);
            }
        }
        DialogAnswer::Answered(value) => {
            let kind = request.get("dialogKind").and_then(JsValue::as_string);
            if kind.is_some_and(|v| v.equals_str("confirm")) {
                out.extend(
                    if value == &JsValue::Bool(true) {
                        "The user confirmed: yes"
                    } else {
                        "The user confirmed: no"
                    }
                    .encode_utf16(),
                );
            } else if kind.is_some_and(|v| v.equals_str("input") || v.equals_str("select")) {
                out.extend(
                    if kind.unwrap().equals_str("input") {
                        "The user answered: "
                    } else {
                        "The user selected: "
                    }
                    .encode_utf16(),
                );
                append_value(&mut out, Some(value));
            } else {
                out.extend("The user submitted: ".encode_utf16());
                out.extend(
                    pilotdeck_protocol::stringify_json(value)
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "undefined".into())
                        .encode_utf16(),
                );
            }
        }
    }
    JsString::from_utf16(out)
}
fn append_value(out: &mut Vec<u16>, value: Option<&JsValue>) {
    if let Some(JsValue::String(value)) = value {
        out.extend_from_slice(value.units());
    } else {
        out.extend("undefined".encode_utf16());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(kind: &str) -> JsValue {
        object([
            ("type", "user_dialog_request".into()),
            ("requestId", "dialog".into()),
            ("dialogKind", kind.into()),
            ("toolName", "request_user_input".into()),
            ("toolCallId", "tool-call".into()),
            ("prompt", "Question?".into()),
        ])
    }
    async fn cleanup(requests: &Requests) {
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
    #[tokio::test]
    async fn duplicate_recovery_acknowledges_only_the_answer_actually_recorded() {
        for same in [true, false] {
            let (_scratch, requests) = crate::native_process::management_tests::setup();
            let port = Arc::new(super::super::tests::recovery(&requests));
            let session: JsString = if same {
                "same-flight".into()
            } else {
                "different-flight".into()
            };
            let chat = crate::native_process::methods::session_dir::bounded_chat_dir(
                &requests.options.home,
                &requests.options.project,
            )
            .unwrap();
            journal(&requests.options.home, &chat, &session)
                .unwrap()
                .record(&request("input"))
                .unwrap();
            let path = chat.join(format!(
                "{}.jsonl",
                crate::storage::sanitize_session_id(&session)
            ));
            let fence = transcript_locks::acquire(&path).await.unwrap();
            let first = {
                let port = port.clone();
                let requests = requests.clone();
                let session = session.clone();
                tokio::spawn(async move {
                    port.recover_answer(
                        &requests,
                        &object([]),
                        &session,
                        &"dialog".into(),
                        &DialogAnswer::Answered("first".into()),
                    )
                    .await
                })
            };
            let joined = |n| {
                port.flights
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .values()
                    .any(|f| f.joined >= n)
            };
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while !joined(1) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let second = {
                let port = port.clone();
                let requests = requests.clone();
                let session = session.clone();
                tokio::spawn(async move {
                    let value = if same { "first" } else { "second" };
                    port.recover_answer(
                        &requests,
                        &object([]),
                        &session,
                        &"dialog".into(),
                        &DialogAnswer::Answered(value.into()),
                    )
                    .await
                })
            };
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while !joined(2) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            drop(fence);
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(3), first)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
            );
            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(3), second)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                same
            );
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains("first"));
            assert!(!text.contains("second"));
            assert_eq!(text.lines().count(), 2);
            assert!(
                journal(&requests.options.home, &chat, &session)
                    .unwrap()
                    .recover()
                    .unwrap()
                    .is_empty()
            );
            cleanup(&requests).await;
        }
    }
    #[test]
    fn duplicate_answer_comparison_is_order_insensitive_and_bounded() {
        let a = DialogAnswer::Answered(object([
            ("a", JsValue::Number(1.0)),
            ("b", JsValue::Bool(true)),
        ]));
        let b = DialogAnswer::Answered(object([
            ("b", JsValue::Bool(true)),
            ("a", JsValue::Number(1.0)),
        ]));
        assert!(same_answer(&a, &b));
        let huge = DialogAnswer::Answered(JsValue::Array(
            (0..100_001)
                .map(|n| JsValue::Number(f64::from(n)))
                .collect(),
        ));
        assert!(!same_answer(&huge, &huge));
    }
    #[tokio::test]
    async fn native_dialog_restart_list_answer_and_idempotent_transcript_are_real() {
        let (_scratch, mut requests) = crate::native_process::management_tests::setup();
        let port = Arc::new(super::super::tests::recovery(&requests));
        requests.dialog_recovery = Some(port.clone());
        let session = JsString::from("recovery-native");
        let chat = crate::native_process::methods::session_dir::bounded_chat_dir(
            &requests.options.home,
            &requests.options.project,
        )
        .unwrap();
        journal(&requests.options.home, &chat, &session)
            .unwrap()
            .record(&request("input"))
            .unwrap();
        let params = object([("sessionKey", JsValue::String(session.clone()))]);
        let first = requests
            .user_dialog_request("user_dialog_list".into(), params.clone())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let second = requests
            .user_dialog_request("user_dialog_list".into(), params.clone())
            .await
            .unwrap();
        assert_eq!(
            first, second,
            "restart timestamps stay stable across rereads"
        );
        let answer = JsString::from_utf16(vec![0xd800, 0x4e2d]);
        let input = object([
            ("sessionKey", JsValue::String(session.clone())),
            ("requestId", "dialog".into()),
            (
                "result",
                object([
                    ("behavior", "answered".into()),
                    ("value", JsValue::String(answer.clone())),
                ]),
            ),
        ]);
        let delivered = requests
            .user_dialog_request("user_dialog_respond".into(), input.clone())
            .await
            .unwrap();
        assert_eq!(
            delivered,
            object([
                ("delivered", JsValue::Bool(true)),
                ("recovered", JsValue::Bool(true)),
                ("reason", "gateway_restarted".into())
            ])
        );
        assert_eq!(
            requests
                .user_dialog_request("user_dialog_list".into(), params)
                .await
                .unwrap(),
            object([("dialogs", JsValue::Array(vec![]))])
        );
        assert_eq!(
            requests
                .user_dialog_request("user_dialog_respond".into(), input)
                .await
                .unwrap(),
            object([("delivered", JsValue::Bool(false))])
        );
        let bytes = std::fs::read_to_string(chat.join("recovery-native.jsonl")).unwrap();
        let rows: Vec<_> = bytes
            .lines()
            .map(|line| pilotdeck_protocol::parse_json(line).unwrap())
            .collect();
        assert_eq!(
            rows.len(),
            2,
            "exactly one durable answer and its recovery turn result"
        );
        assert_eq!(rows[0].get("sequence"), Some(&JsValue::Number(1.0)));
        assert_eq!(rows[1].get("sequence"), Some(&JsValue::Number(2.0)));
        let text = rows[0].get("message").unwrap().get("content").unwrap();
        let JsValue::Array(blocks) = text else {
            panic!("message blocks")
        };
        assert!(
            blocks[0]
                .get("text")
                .and_then(JsValue::as_string)
                .unwrap()
                .units()
                .ends_with(answer.units())
        );
        assert_eq!(
            rows[1].get("result").unwrap().get("stopReason"),
            Some(&"aborted_tools".into())
        );
        cleanup(&requests).await;
    }
    #[tokio::test]
    async fn native_dialog_invalid_answer_and_busy_writer_preserve_recoverable_request() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let port = Arc::new(super::super::tests::recovery(&requests));
        let session = JsString::from("blocked-recovery");
        let chat = crate::native_process::methods::session_dir::bounded_chat_dir(
            &requests.options.home,
            &requests.options.project,
        )
        .unwrap();
        let journal = journal(&requests.options.home, &chat, &session).unwrap();
        journal.record(&request("confirm")).unwrap();
        let params = object([]);
        let failure = port
            .recover_answer(
                &requests,
                &params,
                &session,
                &"dialog".into(),
                &DialogAnswer::Answered("yes".into()),
            )
            .await
            .unwrap_err();
        assert_eq!(failure.code, "INVALID_USER_DIALOG_RESPONSE");
        assert_eq!(journal.recover().unwrap().len(), 1);
        let path = chat.join("blocked-recovery.jsonl");
        let lease = transcript_locks::cold_write_lease(&path).unwrap().unwrap();
        let failure = port
            .recover_answer(
                &requests,
                &params,
                &session,
                &"dialog".into(),
                &DialogAnswer::Answered(JsValue::Bool(true)),
            )
            .await
            .unwrap_err();
        assert_eq!(failure.code, "SESSION_WRITER_BUSY");
        assert_eq!(journal.recover().unwrap().len(), 1);
        drop(lease);
        assert!(
            port.recover_answer(
                &requests,
                &params,
                &session,
                &"dialog".into(),
                &DialogAnswer::Answered(JsValue::Bool(true))
            )
            .await
            .unwrap()
        );
        cleanup(&requests).await;
    }
    #[tokio::test]
    async fn partial_recovery_only_finishes_the_previously_durable_answer() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let port = Arc::new(super::super::tests::recovery(&requests));
        let session: JsString = "partial-retry".into();
        let chat = crate::native_process::methods::session_dir::bounded_chat_dir(
            &requests.options.home,
            &requests.options.project,
        )
        .unwrap();
        let req = request("input");
        let journal = journal(&requests.options.home, &chat, &session).unwrap();
        journal.record(&req).unwrap();
        let adapter = AsyncTranscriptAdapter::new(
            Arc::new(JsonlTranscriptStore::new_bounded()),
            Arc::new({
                let chat = chat.clone();
                move |_: &ProjectKey| chat.clone()
            }),
        );
        let storage = adapter
            .create_session_storage(SessionStorageInput {
                project_root: requests.options.project.clone(),
                pilot_home: requests.options.home.clone(),
                session_id: session.clone(),
                now: None,
            })
            .unwrap();
        storage.restore().unwrap();
        let purpose: JsString = "gateway_user_dialog_recovery:dialog".into();
        let original = DialogAnswer::Answered("first".into());
        let msg = object([
            ("role", "user".into()),
            (
                "content",
                JsValue::Array(vec![object([
                    ("type", "text".into()),
                    ("text", JsValue::String(format_answer(&req, &original))),
                ])]),
            ),
            (
                "metadata",
                object([
                    ("synthetic", JsValue::Bool(true)),
                    ("purpose", JsValue::String(purpose)),
                    ("toolCallId", "tool-call".into()),
                ]),
            ),
        ]);
        let mut draft = JsObject::new();
        draft.insert("type".into(), "durable_message".into());
        draft.insert("message".into(), msg);
        storage
            .transcript
            .record_session_event_checked(&session, &"partial-turn".into(), draft)
            .unwrap();
        let path = chat.join("partial-retry.jsonl");
        let before = std::fs::read(&path).unwrap();
        assert!(
            !port
                .recover_answer(
                    &requests,
                    &object([]),
                    &session,
                    &"dialog".into(),
                    &DialogAnswer::Answered("different".into())
                )
                .await
                .unwrap()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(journal.recover().unwrap().len(), 1);
        assert!(
            port.recover_answer(
                &requests,
                &object([]),
                &session,
                &"dialog".into(),
                &original
            )
            .await
            .unwrap()
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("first"));
        assert!(!text.contains("different"));
        assert!(journal.recover().unwrap().is_empty());
        cleanup(&requests).await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_journal_start_and_recovery_are_serialized_and_route_to_the_live_dialog() {
        use crate::runtime::tools::builtin::user_dialog::NativeUserDialogPort;
        use crate::user_dialog::{
            ChannelOptions, GatewayUserDialogChannel, InputRequest, UserDialogJournal,
        };
        struct PausedJournal {
            inner: GatewayUserDialogJournal,
            started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
            release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl UserDialogJournal for PausedJournal {
            fn record(&self, event: &JsValue) -> crate::user_dialog::Result<()> {
                self.inner.record(event)?;
                if let Some(sender) = self.started.lock().unwrap().take() {
                    let _ = sender.send(());
                }
                self.release.lock().unwrap().recv().unwrap();
                Ok(())
            }
            fn remove(&self, id: &JsString) -> crate::user_dialog::Result<()> {
                self.inner.remove(id)
            }
        }
        let (_scratch, mut requests) = crate::native_process::management_tests::setup();
        let recovery = Arc::new(super::super::tests::recovery(&requests));
        requests.dialog_recovery = Some(recovery.clone());
        let session: JsString = "live-start-race".into();
        let chat = crate::native_process::methods::session_dir::bounded_chat_dir(
            &requests.options.home,
            &requests.options.project,
        )
        .unwrap();
        let journal = journal(&requests.options.home, &chat, &session).unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut options = ChannelOptions::new(
            session.clone(),
            requests.user_dialog_bus.clone(),
            Arc::new(|_| Ok(())),
        );
        options.uuid = Arc::new(|| "dialog".to_owned());
        options.journal = Some(Arc::new(PausedJournal {
            inner: journal,
            started: std::sync::Mutex::new(Some(started_tx)),
            release: std::sync::Mutex::new(release_rx),
        }));
        let channel = GatewayUserDialogChannel::new(options.clone());
        let port = NativeUserDialogPort::new(options);
        let begin = tokio::spawn(async move {
            port.begin(move || {
                channel.request_input(&InputRequest {
                    tool_call_id: "tool-call".into(),
                    tool_name: "request_user_input".into(),
                    prompt: "Question?".into(),
                    placeholder: None,
                    allow_empty: false,
                    abort: None,
                })
            })
            .await
            .unwrap()
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), started_rx)
            .await
            .unwrap()
            .unwrap();
        let input = object([
            ("sessionKey", JsValue::String(session.clone())),
            ("requestId", "dialog".into()),
            (
                "result",
                object([
                    ("behavior", "answered".into()),
                    ("value", "accepted".into()),
                ]),
            ),
        ]);
        let response = {
            let requests = requests.clone();
            tokio::spawn(async move {
                requests
                    .user_dialog_request("user_dialog_respond".into(), input)
                    .await
                    .unwrap()
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while recovery.flights.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            !response.is_finished(),
            "recovery must wait until journal and bus registration form one operation"
        );
        release_tx.send(()).unwrap();
        let mut request = tokio::time::timeout(std::time::Duration::from_secs(3), begin)
            .await
            .unwrap()
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), response)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.get("delivered"), Some(&JsValue::Bool(true)));
        assert!(
            result.get("recovered").is_none(),
            "response must use the live generation"
        );
        assert!(
            matches!(request.try_result(), Some(Ok(DialogAnswer::Answered(value))) if value == "accepted".into())
        );
        assert!(
            !chat.join("live-start-race.jsonl").exists(),
            "live answer must not create a synthetic recovery turn"
        );
        cleanup(&requests).await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_journal_start_is_not_listed_as_recovered() {
        use crate::runtime::tools::builtin::user_dialog::NativeUserDialogPort;
        use crate::user_dialog::{
            ChannelOptions, GatewayUserDialogChannel, InputRequest, UserDialogJournal,
        };
        struct PausedJournal {
            inner: GatewayUserDialogJournal,
            started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
            release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl UserDialogJournal for PausedJournal {
            fn record(&self, event: &JsValue) -> crate::user_dialog::Result<()> {
                self.inner.record(event)?;
                if let Some(sender) = self.started.lock().unwrap().take() {
                    let _ = sender.send(());
                }
                self.release.lock().unwrap().recv().unwrap();
                Ok(())
            }
            fn remove(&self, id: &JsString) -> crate::user_dialog::Result<()> {
                self.inner.remove(id)
            }
        }
        let (_scratch, mut requests) = crate::native_process::management_tests::setup();
        let recovery = Arc::new(super::super::tests::recovery(&requests));
        requests.dialog_recovery = Some(recovery.clone());
        let session: JsString = "live-list-race".into();
        let chat = crate::native_process::methods::session_dir::bounded_chat_dir(
            &requests.options.home,
            &requests.options.project,
        )
        .unwrap();
        let journal = journal(&requests.options.home, &chat, &session).unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut options = ChannelOptions::new(
            session.clone(),
            requests.user_dialog_bus.clone(),
            Arc::new(|_| Ok(())),
        );
        options.uuid = Arc::new(|| "dialog".to_owned());
        options.journal = Some(Arc::new(PausedJournal {
            inner: journal,
            started: std::sync::Mutex::new(Some(started_tx)),
            release: std::sync::Mutex::new(release_rx),
        }));
        let channel = GatewayUserDialogChannel::new(options.clone());
        let port = NativeUserDialogPort::new(options);
        let begin = tokio::spawn(async move {
            port.begin(move || {
                channel.request_input(&InputRequest {
                    tool_call_id: "tool-call".into(),
                    tool_name: "request_user_input".into(),
                    prompt: "Question?".into(),
                    placeholder: None,
                    allow_empty: false,
                    abort: None,
                })
            })
            .await
            .unwrap()
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), started_rx)
            .await
            .unwrap()
            .unwrap();
        let input = object([("sessionKey", JsValue::String(session.clone()))]);
        let mut listing = Box::pin(requests.user_dialog_request("user_dialog_list".into(), input));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(500), &mut listing)
                .await
                .is_err(),
            "list cannot observe the written journal before live registration"
        );
        release_tx.send(()).unwrap();
        let mut request = tokio::time::timeout(std::time::Duration::from_secs(3), begin)
            .await
            .unwrap()
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), listing)
            .await
            .unwrap()
            .unwrap();
        let Some(JsValue::Array(dialogs)) = result.get("dialogs") else {
            panic!("dialogs")
        };
        assert_eq!(dialogs.len(), 1);
        assert_eq!(dialogs[0].get("requestId"), Some(&"dialog".into()));
        assert_ne!(dialogs[0].get("reason"), Some(&"gateway_restarted".into()));
        // Settle through the actual live bus before shutting down the owner.
        let answer = object([
            ("sessionKey", JsValue::String(session)),
            ("requestId", "dialog".into()),
            (
                "result",
                object([
                    ("behavior", "answered".into()),
                    ("value", "accepted".into()),
                ]),
            ),
        ]);
        assert_eq!(
            requests
                .user_dialog_request("user_dialog_respond".into(), answer)
                .await
                .unwrap()
                .get("delivered"),
            Some(&JsValue::Bool(true))
        );
        assert!(request.try_result().is_some());
        cleanup(&requests).await;
    }
    #[test]
    fn native_recovery_formats_cancel_choice_and_form_as_the_legacy_context() {
        let input = request("input");
        assert_eq!(
            format_answer(&input, &DialogAnswer::Cancelled(None))
                .to_utf8()
                .unwrap(),
            "[Gateway restart dialog recovery]\nThe Gateway restarted while waiting for request_user_input.\nQuestion: Question?\nThe user cancelled the dialog."
        );
        assert!(
            format_answer(&request("select"), &DialogAnswer::Answered("choice".into()))
                .to_utf8()
                .unwrap()
                .ends_with("The user selected: choice")
        );
        assert!(
            format_answer(
                &request("form"),
                &DialogAnswer::Answered(object([("value", JsValue::Bool(true))]))
            )
            .to_utf8()
            .unwrap()
            .ends_with("The user submitted: {\"value\":true}")
        );
    }
}

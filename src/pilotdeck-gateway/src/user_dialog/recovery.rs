use super::*;
use crate::{runtime::connection::RequestError, storage::SessionStorage};

/// Resolves the host's existing session writer; caller-supplied paths never select storage.
pub type DialogRecoveryStorageResolver = Arc<
    dyn Fn(&DialogSession) -> std::result::Result<Arc<SessionStorage>, RequestError> + Send + Sync,
>;

pub(super) fn persist(
    storage: &SessionStorage,
    session: &JsString,
    request: &JsValue,
    answer: &DialogAnswer,
) -> std::result::Result<(), RequestError> {
    let failure = |error: crate::storage::AdapterError| RequestError::new(error.to_string());
    let transcript = storage.read_transcript(None).map_err(failure)?;
    if let Some(last) = transcript.entries.last() {
        let maximum = transcript
            .entries
            .iter()
            .filter_map(|entry| entry.get("sequence").and_then(JsValue::as_number))
            .fold(0.0, f64::max);
        storage
            .transcript
            .restore_state(maximum, last.get("entryId").unwrap_or(&JsValue::Null))
            .map_err(failure)?;
    }
    let id = request
        .get("requestId")
        .and_then(JsValue::as_string)
        .expect("recovered request id");
    let mut purpose = JsString::from("gateway_user_dialog_recovery:")
        .units()
        .to_vec();
    purpose.extend_from_slice(id.units());
    let purpose = JsValue::String(JsString::from_utf16(purpose));
    let prior = transcript.entries.iter().find(|entry| {
        matches!(entry.get("type").and_then(JsValue::as_string), Some(kind) if kind.equals_str("durable_message") || kind.equals_str("tool_result_message"))
            && entry.get("message").and_then(|message| message.get("metadata")).and_then(|metadata| metadata.get("purpose")) == Some(&purpose)
    });
    let turn: JsString = prior
        .and_then(|entry| entry.get("turnId"))
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_else(|| format!("gateway:dialog-recovery:{}", uuid::Uuid::new_v4()).into());
    if prior.is_some()
        && transcript.entries.iter().any(|entry| {
            entry.get("type") == Some(&"turn_result".into())
                && entry.get("turnId").and_then(JsValue::as_string) == Some(&turn)
        })
    {
        return Ok(());
    }
    let mut text = JsString::from(
        "[Gateway restart dialog recovery]\nThe Gateway restarted while waiting for ",
    )
    .units()
    .to_vec();
    let field = |name| {
        request
            .get(name)
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| "undefined".into())
    };
    text.extend_from_slice(field("toolName").units());
    text.extend_from_slice(JsString::from(".\nQuestion: ").units());
    text.extend_from_slice(field("prompt").units());
    let (prefix, value) = match answer {
        DialogAnswer::Cancelled(reason) => {
            if let Some(reason) = reason.as_ref().filter(|reason| !reason.is_empty()) {
                ("\nThe user cancelled the dialog: ", reason.clone())
            } else {
                ("\nThe user cancelled the dialog.", "".into())
            }
        }
        DialogAnswer::Answered(value) => {
            let kind = field("dialogKind");
            if kind.equals_str("input") {
                ("\nThe user answered: ", value.as_string().unwrap().clone())
            } else if kind.equals_str("select") {
                ("\nThe user selected: ", value.as_string().unwrap().clone())
            } else if kind.equals_str("confirm") {
                (
                    "\nThe user confirmed: ",
                    if value.as_bool() == Some(true) {
                        "yes".into()
                    } else {
                        "no".into()
                    },
                )
            } else {
                (
                    "\nThe user submitted: ",
                    stringify_json(value)
                        .map_err(|e| RequestError::new(e.to_string()))?
                        .unwrap_or_default()
                        .into(),
                )
            }
        }
    };
    text.extend_from_slice(JsString::from(prefix).units());
    text.extend_from_slice(value.units());
    let message = object([
        ("role", "user".into()),
        (
            "content",
            JsValue::Array(vec![object([
                ("type", "text".into()),
                ("text", JsValue::String(JsString::from_utf16(text))),
            ])]),
        ),
        (
            "metadata",
            object([
                ("synthetic", JsValue::Bool(true)),
                ("purpose", purpose),
                (
                    "toolCallId",
                    request
                        .get("toolCallId")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
            ]),
        ),
    ]);
    let mut draft = JsObject::new();
    draft.insert("type".into(), "durable_message".into());
    draft.insert("message".into(), message);
    if prior.is_none() {
        storage
            .transcript
            .record_session_event(session, &turn, draft)
            .map_err(failure)?;
    }
    let now = format_epoch_iso((DialogRuntime::default().now)());
    let result = object([
        ("type", "aborted".into()),
        ("sessionId", JsValue::String(session.clone())),
        ("turnId", JsValue::String(turn.clone())),
        ("stopReason", "aborted_tools".into()),
        ("usage", object([])),
        ("permissionDenials", JsValue::Array(vec![])),
        ("turns", JsValue::Number(0.0)),
        ("startedAt", now.as_str().into()),
        ("completedAt", now.as_str().into()),
    ]);
    storage
        .transcript
        .record_turn_result(session, &turn, result)
        .map_err(failure)
}

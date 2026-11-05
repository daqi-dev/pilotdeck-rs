//! S2 turn steering / cancellation / abort and U9 hook_async_result wiring.
//!
//! TS references:
//!  - `InProcessGateway.ts:1859-1893` (steerTurn, cancelSteer)
//!  - `InProcessGateway.ts:1896-1930` (submitAsyncHookResult)
//!  - `protocol/types.ts:504-550` (input/result types)
//!
//! `abort_turn` is connection-owned: `ConnectionTurnGateway::request`
//! (`runtime/turn/gateway.rs`) intercepts it (including the drain wait)
//! before the requests registry is consulted, so this family does not
//! register an `abort_turn` handler.
use super::super::requests::Requests;
use super::string;
use crate::{
    metadata::object,
    runtime::{
        connection::{Call, RequestError},
        steer::{SteerMessage, SteerReason},
    },
};
use pilotdeck_protocol::JsValue;

pub(super) const METHODS: &[&str] = &["steer_turn", "cancel_steer", "hook_async_result"];
pub(super) const HANDLER: super::Handler = super::Handler::AfterAdmission(Requests::steer_request);

fn steer_reason_str(reason: &SteerReason) -> &'static str {
    reason.as_str()
}

/// An attachments field carries nothing when it is absent, null/undefined,
/// or an empty array.
fn attachments_empty(value: &JsValue) -> bool {
    matches!(value, JsValue::Undefined | JsValue::Null)
        || matches!(value, JsValue::Array(items) if items.is_empty())
}

/// TS `steerTurn` merges `attachments`/`uploadedAttachments` into the content
/// array (InProcessGateway.ts:1864-1870). This native slice does not support
/// steering with attachments; a non-empty payload is rejected explicitly
/// instead of being silently dropped.
fn attachments_unsupported(field: &str) -> RequestError {
    RequestError {
        code: "STEER_ATTACHMENTS_UNSUPPORTED".into(),
        message: format!(
            "steer_turn does not support attachments in this native slice: `{field}` \
             must be absent, null, or an empty array."
        )
        .into(),
        details: None,
        validation: None,
    }
}

impl Requests {
    pub(in crate::native_process) fn steer_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            match method.as_str() {
                "steer_turn" => self.handle_steer_turn(params).await,
                "cancel_steer" => self.handle_cancel_steer(params).await,
                "hook_async_result" => self.handle_hook_async_result(params).await,
                _ => unreachable!("steer_request called for unknown method"),
            }
        })
    }

    /// `InProcessGateway.steerTurn`:
    /// check active run → build canonical message → enqueue via mailbox.
    async fn handle_steer_turn(&self, params: JsValue) -> Result<JsValue, RequestError> {
        let session_key = string(&params, "sessionKey")?;
        let run_id = string(&params, "runId")?;
        let item_id = string(&params, "itemId")?;
        let message_text = string(&params, "message")?;

        // Attachments are rejected before any state lookup: the caller must
        // learn the slice cannot carry them rather than see a fake accepted.
        for field in ["attachments", "uploadedAttachments"] {
            if params
                .get(field)
                .is_some_and(|value| !attachments_empty(value))
            {
                return Err(attachments_unsupported(field));
            }
        }

        // TS: no active run → no_active_turn; wrong run → turn_mismatch.
        let active_run = self.owner.active_turn_run_id(&session_key);
        let Some(active_run) = active_run else {
            return Ok(object([
                ("accepted", JsValue::Bool(false)),
                ("reason", "no_active_turn".into()),
            ]));
        };
        if active_run != run_id {
            return Ok(object([
                ("accepted", JsValue::Bool(false)),
                ("reason", "turn_mismatch".into()),
            ]));
        }

        let steer_info = self.current_steer_info(&session_key);
        let Some(steer_info) = steer_info else {
            return Ok(object([
                ("accepted", JsValue::Bool(false)),
                ("reason", "no_active_turn".into()),
            ]));
        };
        if steer_info.run_id != run_id {
            return Ok(object([
                ("accepted", JsValue::Bool(false)),
                ("reason", "turn_mismatch".into()),
            ]));
        }

        // Canonical TS steer message format (InProcessGateway.ts:1871-1883).
        let message = crate::metadata::object([
            ("role", "user".into()),
            (
                "content",
                JsValue::Array(vec![object([
                    ("type", "text".into()),
                    ("text", JsValue::String(message_text)),
                ])]),
            ),
            (
                "metadata",
                object([
                    ("purpose", "mid_turn_steer".into()),
                    ("queueItemId", JsValue::String(item_id.clone())),
                ]),
            ),
        ]);

        let result = steer_info
            .mailbox
            .enqueue(
                steer_info.turn_id,
                SteerMessage {
                    item_id,
                    message,
                    allowed_read_files: Some(Vec::new()),
                },
            )
            .await
            .map_err(|e| RequestError::new(e.message.clone()))?;

        if result.accepted {
            Ok(object([("accepted", JsValue::Bool(true))]))
        } else {
            let reason = result
                .reason
                .as_ref()
                .map(steer_reason_str)
                .unwrap_or("turn_closing");
            Ok(object([
                ("accepted", JsValue::Bool(false)),
                ("reason", reason.into()),
            ]))
        }
    }

    /// `InProcessGateway.cancelSteer`:
    /// cancel a queued steer item by `itemId`.
    async fn handle_cancel_steer(&self, params: JsValue) -> Result<JsValue, RequestError> {
        let session_key = string(&params, "sessionKey")?;
        let run_id = string(&params, "runId")?;
        let item_id = string(&params, "itemId")?;

        let active_run = self.owner.active_turn_run_id(&session_key);
        let Some(active_run) = active_run else {
            return Ok(object([
                ("cancelled", JsValue::Bool(false)),
                ("reason", "no_active_turn".into()),
            ]));
        };
        if active_run != run_id {
            return Ok(object([
                ("cancelled", JsValue::Bool(false)),
                ("reason", "turn_mismatch".into()),
            ]));
        }

        let steer_info = self.current_steer_info(&session_key);
        let Some(steer_info) = steer_info else {
            return Ok(object([
                ("cancelled", JsValue::Bool(false)),
                ("reason", "no_active_turn".into()),
            ]));
        };
        if steer_info.run_id != run_id {
            return Ok(object([
                ("cancelled", JsValue::Bool(false)),
                ("reason", "turn_mismatch".into()),
            ]));
        }

        let result = steer_info
            .mailbox
            .cancel(steer_info.turn_id, item_id)
            .await
            .map_err(|e| RequestError::new(e.message.clone()))?;

        if result.cancelled {
            Ok(object([("cancelled", JsValue::Bool(true))]))
        } else {
            let reason = result
                .reason
                .as_ref()
                .map(steer_reason_str)
                .unwrap_or("too_late");
            Ok(object([
                ("cancelled", JsValue::Bool(false)),
                ("reason", reason.into()),
            ]))
        }
    }

    /// U9 `hook_async_result` — deliver an async-hook output to its registry.
    async fn handle_hook_async_result(&self, params: JsValue) -> Result<JsValue, RequestError> {
        let session_key = string(&params, "sessionKey")?;
        let invocation_id = string(&params, "invocationId")?;

        let Some(registry) = &self.async_hook_registry else {
            // No InteractionSteer resource staged → no pending hooks possible.
            return Ok(object([
                ("invocationId", JsValue::String(invocation_id)),
                ("status", "unknown".into()),
            ]));
        };

        let output = params.get("output").cloned().unwrap_or(JsValue::Undefined);

        let result = registry
            .submit(session_key, invocation_id, output)
            .await
            .map_err(|e| {
                let code = e
                    .module_error
                    .as_deref()
                    .and_then(|m| m.code.as_ref())
                    .and_then(|c| c.to_utf8().ok())
                    .unwrap_or_else(|| "gateway_request_failed".to_owned());
                RequestError {
                    code,
                    message: e.message.into(),
                    details: None,
                    validation: None,
                }
            })?;

        Ok(object([
            ("invocationId", JsValue::String(result.invocation_id)),
            ("status", result.status.as_str().into()),
        ]))
    }

    /// Retrieve `ActiveSteerInfo` for the session's current live turn.
    ///
    /// Returns `None` if no steer resource is staged, no active turn, or
    /// the stored run ID no longer matches the admission slot. The
    /// resource fetch, active-turn lookup, and run-ID guard live in the
    /// shared `interaction_steer::active_turn_for`.
    fn current_steer_info(
        &self,
        session_key: &pilotdeck_protocol::JsString,
    ) -> Option<crate::runtime::project::interaction_steer::ActiveSteerInfo> {
        // Guard: the steer slot's run must still match the admission slot.
        let active_run = self.owner.active_turn_run_id(session_key);
        crate::runtime::project::interaction_steer::active_turn_for(
            &self.project,
            session_key,
            active_run.as_ref(),
        )
    }
}

//! S2 turn steering / cancellation / abort and U9 hook_async_result wiring.
//!
//! TS references:
//!  - `InProcessGateway.ts:1859-1893` (steerTurn, cancelSteer)
//!  - `InProcessGateway.ts:1896-1930` (submitAsyncHookResult)
//!  - `InProcessGateway.ts:2000-2020` (abortTurn explicit RPC variant)
//!  - `protocol/types.ts:504-550` (input/result types)
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

pub(super) const METHODS: &[&str] = &[
    "steer_turn",
    "cancel_steer",
    "abort_turn",
    "hook_async_result",
];
pub(super) const HANDLER: super::Handler = super::Handler::AfterAdmission(Requests::steer_request);

fn gateway_error(code: &'static str, message: impl Into<String>) -> RequestError {
    RequestError {
        code: code.into(),
        message: message.into().into(),
        details: None,
        validation: None,
    }
}

fn steer_reason_str(reason: &SteerReason) -> &'static str {
    reason.as_str()
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
                "abort_turn" => self.handle_abort_turn(params).await,
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

    /// `InProcessGateway.abortTurn` explicit RPC variant.
    ///
    /// The connection-owned socket-close abort lives in `gateway.rs`; this
    /// handler exposes the same path for the explicit RPC call that carries
    /// `runId` and `reason`.
    async fn handle_abort_turn(&self, params: JsValue) -> Result<JsValue, RequestError> {
        // TS: non-string runId means the caller cannot target a live run.
        if params
            .get("runId")
            .is_some_and(|v| !matches!(v, JsValue::String(_) | JsValue::Undefined))
        {
            return Ok(object([("ok", JsValue::Bool(true))]));
        }
        let Some(session_key) = params.get("sessionKey").and_then(JsValue::as_string) else {
            return Ok(object([("ok", JsValue::Bool(true))]));
        };
        let run_id = params.get("runId").and_then(JsValue::as_string);
        let reason = params.get("reason").and_then(JsValue::as_string);

        let allowed = self
            .owner
            .start_gateway_abort(session_key, run_id, reason)
            .map_err(|e| RequestError::new(e.to_string()))?;

        if allowed {
            // Match the gateway.rs abort boundary: yield before draining.
            tokio::task::yield_now().await;
        }
        Ok(object([("ok", JsValue::Bool(true))]))
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
            .map_err(|e| gateway_error("ASYNC_HOOK_ERROR", e.message.to_string()))?;

        Ok(object([
            ("invocationId", JsValue::String(result.invocation_id)),
            ("status", result.status.as_str().into()),
        ]))
    }

    /// Retrieve `ActiveSteerInfo` for the session's current live turn.
    ///
    /// Returns `None` if no steer resource is staged, no active turn, or
    /// the stored run ID no longer matches the admission slot.
    fn current_steer_info(
        &self,
        session_key: &pilotdeck_protocol::JsString,
    ) -> Option<crate::runtime::project::interaction_steer::ActiveSteerInfo> {
        use crate::runtime::project::{ProjectResources, Resource};
        let lease = self.project.acquire(None).ok()?;
        let context = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()?
            .context();
        let steer_resource = context
            .resource(Resource::InteractionSteer)?
            .as_any()
            .downcast_ref::<crate::runtime::project::interaction_steer::InteractionSteer>()?
            .clone();
        let info = steer_resource.active_steer_info(session_key)?;
        // Guard: admission slot must still agree with the steer slot.
        let active = self.owner.active_turn_run_id(session_key)?;
        if info.run_id != active {
            return None;
        }
        Some(info)
    }
}

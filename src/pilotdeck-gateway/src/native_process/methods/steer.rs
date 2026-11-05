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

/// TS `steerTurn` merges non-empty `attachments` into the content array via
/// `prepareAttachmentTurn` and never reads `uploadedAttachments` (only
/// `submitTurn` consumes uploads; InProcessGateway.ts:1864-1870). This native
/// slice cannot steer with attachments yet: a non-empty `attachments` payload
/// is rejected explicitly — at the TS attachment-preparation position, right
/// after the admission branches and before the resource gates — instead of
/// being silently dropped (ledger A7.2-STEER-ATTACH-001, corrected by
/// A7.2-STEER-ATTACH-001-R1); `uploadedAttachments` is accepted and ignored,
/// matching the TS baseline.
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
    /// check active run → prepare attachments → build canonical message →
    /// enqueue via the admission-locked mailbox path.
    async fn handle_steer_turn(&self, params: JsValue) -> Result<JsValue, RequestError> {
        let session_key = string(&params, "sessionKey")?;
        let run_id = string(&params, "runId")?;
        let item_id = string(&params, "itemId")?;
        let message_text = string(&params, "message")?;

        // TS: no active run → no_active_turn; wrong run → turn_mismatch
        // (InProcessGateway.ts:1860-1862).
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

        // Attachments are rejected immediately after the admission branches,
        // mirroring the TS order: steerTurn prepares attachments right after
        // the activeTurnRunId gates (InProcessGateway.ts:1864-1870), while the
        // session/run resource gates run later, inside router.steer
        // (SessionRouter.ts:307-314, AgentSession.ts:290-297). The caller
        // still learns no_active_turn / turn_mismatch before the slice's
        // explicit attachments error.
        if params
            .get("attachments")
            .is_some_and(|value| !attachments_empty(value))
        {
            return Err(attachments_unsupported("attachments"));
        }
        // `uploadedAttachments` is never read by TS `steerTurn` (only
        // `submitTurn` consumes uploads): a non-empty payload is accepted
        // and ignored here, matching the baseline.

        // Same shared lookup as the async-hook registry's turn resolution:
        // fetch the steer slot and guard it against the admission run ID
        // already checked above (`active_turn_for` performs the guard).
        let steer_info = crate::runtime::project::interaction_steer::active_turn_for(
            &self.project,
            &session_key,
            Some(&active_run),
        );
        let Some(steer_info) = steer_info else {
            return Ok(object([
                ("accepted", JsValue::Bool(false)),
                ("reason", "no_active_turn".into()),
            ]));
        };

        // Regression-test interleave point for the admission-vs-enqueue race:
        // by the time this handler resumes, the steer target may have closed
        // and restarted under the same textual turn ID. Inert unless armed.
        #[cfg(test)]
        race_hook::pause(&session_key).await;

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
            .enqueue(SteerMessage {
                item_id,
                message,
                allowed_read_files: Some(Vec::new()),
            })
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

        let steer_info = crate::runtime::project::interaction_steer::active_turn_for(
            &self.project,
            &session_key,
            Some(&active_run),
        );
        let Some(steer_info) = steer_info else {
            return Ok(object([
                ("cancelled", JsValue::Bool(false)),
                ("reason", "no_active_turn".into()),
            ]));
        };

        let result = steer_info
            .cancel(item_id)
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

        // TS validates unconditionally before any lookup
        // (InProcessGateway.ts:1896-1899): blank ids are rejected even when
        // no async-hook registry is staged, rather than reporting unknown.
        // Same blank semantics as `AsyncHookRegistry::submit`, the Rust side
        // of the same TS function.
        if crate::runtime::async_hooks::blank(&session_key)
            || crate::runtime::async_hooks::blank(&invocation_id)
        {
            return Err(RequestError {
                code: "INVALID_ASYNC_HOOK_RESULT".into(),
                message: "sessionKey and invocationId are required.".into(),
                details: None,
                validation: None,
            });
        }

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
}

/// Test-only interleave for the admission-vs-enqueue race (#1307 fix-r4).
/// `arm` stages a one-shot pause for one session key; the steer handler
/// suspends between its admission/steer-info gates and the locked enqueue,
/// so a test can close and restart the turn under the same textual turn ID
/// while the request is in flight. Inert for every other session and absent
/// from non-test builds.
#[cfg(test)]
pub(super) mod race_hook {
    use pilotdeck_protocol::JsString;
    use std::sync::Mutex;
    use tokio::sync::{oneshot, watch};

    struct Hook {
        session: JsString,
        reached: oneshot::Sender<()>,
        resume: watch::Sender<bool>,
    }
    static ARMED: Mutex<Option<Hook>> = Mutex::new(None);

    /// Arm the one-shot pause for `session`; the returned receiver fires when
    /// the handler has reached the pause, and the sender resumes it.
    pub(in crate::native_process::methods) fn arm(
        session: JsString,
    ) -> (oneshot::Receiver<()>, watch::Sender<bool>) {
        let (reached, reached_rx) = oneshot::channel();
        let (resume, _resume_rx) = watch::channel(false);
        *ARMED.lock().unwrap() = Some(Hook {
            session,
            reached,
            resume: resume.clone(),
        });
        (reached_rx, resume)
    }

    pub(in crate::native_process::methods) async fn pause(session: &JsString) {
        let hook = {
            let mut armed = ARMED.lock().unwrap();
            match armed.as_ref() {
                Some(hook) if hook.session == *session => armed.take(),
                _ => None,
            }
        };
        let Some(hook) = hook else {
            return;
        };
        // Subscribe before announcing `reached`: the test must never be able
        // to send the resume signal ahead of the subscription (a `watch`
        // sender with zero receivers drops the value).
        let mut resume = hook.resume.subscribe();
        let _ = hook.reached.send(());
        loop {
            if *resume.borrow() {
                return;
            }
            if resume.changed().await.is_err() {
                // The test dropped the release handle without resuming:
                // fail open rather than hanging the handler.
                return;
            }
        }
    }
}

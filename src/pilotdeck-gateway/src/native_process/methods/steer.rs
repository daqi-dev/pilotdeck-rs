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
use super::{input_property, require_text};
use crate::{
    js_value,
    metadata::object,
    runtime::{
        connection::{Call, RequestError},
        steer::{SteerMessage, SteerReason},
    },
};
use pilotdeck_protocol::{JsString, JsValue};

pub(super) const METHODS: &[&str] = &["steer_turn", "cancel_steer", "hook_async_result"];
pub(super) const HANDLER: super::Handler = super::Handler::AfterAdmission(Requests::steer_request);

fn steer_reason_str(reason: &SteerReason) -> &'static str {
    reason.as_str()
}

fn no_active_turn(field: &'static str) -> JsValue {
    object([
        (field, JsValue::Bool(false)),
        ("reason", "no_active_turn".into()),
    ])
}
fn turn_mismatch(field: &'static str) -> JsValue {
    object([
        (field, JsValue::Bool(false)),
        ("reason", "turn_mismatch".into()),
    ])
}

/// `String(value)` for the steer item id. TS `steerTurn` / `cancelSteer` embed
/// `input.itemId` verbatim (no guard, no coercion — probes
/// `_tmp/20261026-issue-1604/ts-probe/output.txt` s5/s8), but the mailbox's
/// item identity is a `JsString` through `SteerMessage` / `cancel`, so a
/// non-string id travels as its unchecked `String()` form: objects become
/// `[object Object]` instead of throwing the ToPrimitive TypeError TS never
/// raises here. The durable `queueItemId` therefore shows the coerced text —
/// ledger A7.2-STEER-INPUT-001. Because the coerced text also keys the
/// `seen` / `cancelled` sets, cross-type same-text interleavings alias in
/// Rust while TS keys by the raw wire value — the interleaved response
/// frames may differ (ledger §交错 A/B); a missing key folds to the literal
/// text "undefined" and an explicit `null` to "null" (probe
/// `_tmp/20261109-review-2382-r2-ds/ts-probe/output5.txt`), so each also
/// aliases a genuine string id of that spelling. The empty text (`[]` and
/// `""` both fold to `""`) aliases the same way (ledger 差异 7) and is the
/// one coerced id the durable claim gate skips, where TS's truthiness gate
/// claims the raw `[]` (ledger 差异 6, probe
/// `_tmp/20261109-review-2382-r5-ds/ts-probe/output6.txt`).
fn item_id_string(value: &JsValue) -> Result<JsString, RequestError> {
    js_value::to_string_units(value)
        .map(JsString::from_utf16)
        .map_err(|_| RequestError::new(js_value::StackOverflow::MESSAGE))
}

/// TS `steerTurn` merges `attachments` into the content array through
/// `prepareAttachmentTurn` and never reads `uploadedAttachments` (only
/// `submitTurn` consumes uploads; InProcessGateway.ts:1864-1870). The
/// field's non-array spellings keep the composer's own boundary
/// (GatewayAttachmentTurnComposer.ts:97/:177/:199 — each pass opens with
/// `!attachments || attachments.length === 0` and then iterates): every
/// falsy non-nullish value short-circuits to the no-attachment turn
/// (`""`/`0`/`false`, probe `_tmp/20261026-issue-1444/ts-probe/output.txt`
/// a4/a5/a6), a non-empty string iterates to single-char "attachments"
/// whose missing path/type/content every pass skips (a10), and only a
/// truthy non-iterable value reaches `for (const attachment of
/// attachments)` and throws the boundary TypeError (a7/a8/a9). The front
/// door reads the length off the raw value, so an object carrying the
/// number 0 folds the same way as the falsy spellings (`{length:0}` and
/// `{0:{…},length:0}` both compose the plain text turn — fix-r1 probe
/// b1/b2, `_tmp/20261026-issue-1444/fix-r1/ts-probe/output.txt`), while
/// every other object still reaches the `for...of` and throws
/// (`{length:2}` b3, `{length:"0"}` b4 — the check is `=== 0` on the raw
/// value, so a string "0" or a nonzero number does not fold).
fn steer_attachments(value: &JsValue) -> Result<Vec<JsValue>, RequestError> {
    match value {
        // `input.attachments ?? []` — only the nullish values default.
        JsValue::Undefined | JsValue::Null => Ok(Vec::new()),
        JsValue::Array(items) => Ok(items.clone()),
        JsValue::String(_) | JsValue::Bool(false) => Ok(Vec::new()),
        JsValue::Number(value) if *value == 0.0 || value.is_nan() => Ok(Vec::new()),
        // `attachments.length === 0` on the raw object: the number 0 (and
        // -0, matching JS `===`) short-circuits every composer pass.
        JsValue::Object(_) if value.get("length").and_then(JsValue::as_number) == Some(0.0) => {
            Ok(Vec::new())
        }
        JsValue::Bool(true) | JsValue::Number(_) | JsValue::Object(_) => {
            Err(RequestError::new("attachments is not iterable"))
        }
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
        // TS `steerTurn` has no input guard: `input.sessionKey` is read
        // straight into the `activeTurnRunId` lookup (InProcessGateway.ts:
        // 1860-1862). A nullish params throws on the property read; every
        // non-string key is a Map miss exactly like a blank or absent one
        // (#1604 probes, `_tmp/20261026-issue-1604/ts-probe/output.txt` s1-s3).
        let session_key = input_property(&params, "sessionKey")?;
        let Some(JsValue::String(session_key)) = session_key.as_ref() else {
            return Ok(no_active_turn("accepted"));
        };
        let active_run = self.owner.active_turn_run_id(session_key);
        let Some(active_run) = active_run else {
            return Ok(no_active_turn("accepted"));
        };
        // TS: `activeRunId !== input.runId` — strict equality against the
        // map's (always string) run id, so any non-string or absent runId
        // mismatches instead of erroring (probe s4/s10).
        if !matches!(input_property(&params, "runId")?, Some(JsValue::String(ref id)) if id == &active_run)
        {
            return Ok(turn_mismatch("accepted"));
        }

        // TS steerTurn prepares the attachment turn immediately after the
        // admission branches (InProcessGateway.ts:1864-1870), before the
        // session/run resource gates inside router.steer (SessionRouter.ts:
        // 307-314, AgentSession.ts:290-297): a non-iterable `attachments`
        // payload throws here (probe a7-a9) and the composed input is ready
        // before the steer slot is consulted. `uploadedAttachments` is never
        // read by TS `steerTurn` (only `submitTurn` consumes uploads): a
        // non-empty payload is accepted and ignored, matching the baseline.
        let message_text = input_property(&params, "message")?.unwrap_or(JsValue::Undefined);
        let attachments = steer_attachments(
            &input_property(&params, "attachments")?.unwrap_or(JsValue::Undefined),
        )?;
        let (content, allowed_read_files) = if attachments.is_empty() {
            // The composer's plain path (GatewayAttachmentTurnComposer.ts:
            // 67-72): the text block embeds the raw wire message verbatim
            // (probe s6/a11), exactly the shape this handler built before
            // the merge (#1444, ledger A7.2-STEER-ATTACH-001/-R1/-R2).
            (
                JsValue::Array(vec![object([
                    ("type", "text".into()),
                    ("text", message_text.clone()),
                ])]),
                Vec::new(),
            )
        } else {
            // The same submit-lane prepare (#1321): profile validation, the
            // string-message contract and the composition all run inside
            // `Services::prepare_attachment_turn`, so steer inherits them
            // (ledger A7.2-STEER-ATTACH-001-R2).
            let prepared = self
                .submit_services
                .prepare_attachment_turn(
                    &message_text,
                    attachments,
                    input_property(&params, "projectKey")?
                        .as_ref()
                        .and_then(JsValue::as_string),
                    Some(session_key),
                )
                .await
                .map_err(|error| RequestError {
                    code: error.code,
                    message: error.message.into(),
                    details: None,
                    validation: None,
                })?;
            // TS steerTurn :1873-1875: `agentInput.type === "text"` wraps
            // the text block, anything else takes the composed content
            // array verbatim.
            let content = if prepared.get("type") == Some(&JsValue::from("text")) {
                JsValue::Array(vec![object([
                    ("type", "text".into()),
                    (
                        "text",
                        prepared.get("text").cloned().unwrap_or(JsValue::Undefined),
                    ),
                ])])
            } else {
                prepared
                    .get("content")
                    .cloned()
                    .unwrap_or(JsValue::Undefined)
            };
            // The composer returns `allowedReadFiles` as an array (always
            // present in TS); the Rust compose attaches the key only when a
            // path registered, so a missing key is the empty list.
            let allowed_read_files = match prepared.get("allowedReadFiles") {
                Some(JsValue::Array(paths)) => paths
                    .iter()
                    .filter_map(|path| path.as_string().cloned())
                    .collect(),
                _ => Vec::new(),
            };
            (content, allowed_read_files)
        };

        // Same shared lookup as the async-hook registry's turn resolution:
        // fetch the steer slot and guard it against the admission run ID
        // already checked above (`active_turn_for` performs the guard).
        let steer_info = crate::runtime::project::interaction_steer::active_turn_for(
            &self.project,
            session_key,
            Some(&active_run),
        );
        let Some(steer_info) = steer_info else {
            return Ok(no_active_turn("accepted"));
        };

        // Regression-test interleave point for the admission-vs-enqueue race:
        // by the time this handler resumes, the steer target may have closed
        // and restarted under the same textual turn ID. Inert unless armed.
        #[cfg(test)]
        race_hook::pause(session_key).await;

        // Canonical TS steer message format (InProcessGateway.ts:1871-1883):
        // role + the prepared content + the steer metadata. `queueItemId`
        // carries the item id's coerced form (see `item_id_string`).
        let item_id =
            item_id_string(&input_property(&params, "itemId")?.unwrap_or(JsValue::Undefined))?;
        let message = crate::metadata::object([
            ("role", "user".into()),
            ("content", content),
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
                allowed_read_files: Some(allowed_read_files),
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
        // Same guard-free reads as `steerTurn` (InProcessGateway.ts:1887-1893):
        // the session key feeds the `activeTurnRunId` Map lookup, the run id
        // is compared with strict equality, and the item id is embedded
        // verbatim on the TS side (`cancel` of an unknown id — including a
        // non-string one — records the cancellation and answers
        // `{cancelled: true}`; probe m2).
        let session_key = input_property(&params, "sessionKey")?;
        let Some(JsValue::String(session_key)) = session_key.as_ref() else {
            return Ok(no_active_turn("cancelled"));
        };
        let active_run = self.owner.active_turn_run_id(session_key);
        let Some(active_run) = active_run else {
            return Ok(no_active_turn("cancelled"));
        };
        if !matches!(input_property(&params, "runId")?, Some(JsValue::String(ref id)) if id == &active_run)
        {
            return Ok(turn_mismatch("cancelled"));
        }

        let steer_info = crate::runtime::project::interaction_steer::active_turn_for(
            &self.project,
            session_key,
            Some(&active_run),
        );
        let Some(steer_info) = steer_info else {
            return Ok(no_active_turn("cancelled"));
        };

        let item_id =
            item_id_string(&input_property(&params, "itemId")?.unwrap_or(JsValue::Undefined))?;
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
        // TS validates unconditionally before any lookup
        // (InProcessGateway.ts:1896-1899): `!input.sessionKey?.trim() ||
        // !input.invocationId?.trim()`, sessionKey first. #1604: the same
        // three-layer guard every other method family uses — blank ids are
        // rejected with the domain error even when no async-hook registry is
        // staged, and a non-string id is the boundary's `?.trim is not a
        // function` TypeError instead of this file's old
        // "<field> must be a string.".
        let session_key = require_text(
            &params,
            "sessionKey",
            "INVALID_ASYNC_HOOK_RESULT",
            "sessionKey and invocationId are required.",
        )?;
        let invocation_id = require_text(
            &params,
            "invocationId",
            "INVALID_ASYNC_HOOK_RESULT",
            "sessionKey and invocationId are required.",
        )?;

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

//! Production SubmitAdmission over the gateway's admission coordinators.
//! InProcessGateway.ts:764-889 (normalize/route windows), :956-1013
//! (replacement claim preflight), :1116-1203 (model/budget/dialog-recovery
//! preflight) and :1816 (pending-turn rejection), wired by
//! createLocalGateway.ts:1413-1451.
//!
//! 7a ports the admission windows whose native coordinators exist
//! (interaction reconnect, replacement claims, command routing,
//! trusted-context authorization, model-selection folding,
//! GatewayManualCompactionCoordinator's projection). The 7b windows
//! (SDK session configuration, model assertion, task-budget accounting,
//! dialog recovery) are optional ports: an absent port reproduces the TS
//! "option not wired" behavior exactly, and 7b wires the native
//! implementations behind the same ports.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use pilotdeck_protocol::{JsObject, JsString, JsValue};

use super::submit::{
    SubmitError, SubmitFuture, aborted, error_event, field, is, set, string, truthy,
};
use crate::runtime::session::TurnCompletionHandle;
use crate::{
    metadata::object,
    runtime::{
        interaction::{GatewayInteractionCoordinator, InteractionBinding},
        replacement::{ReplacementCoordinator, SubmitClaim, js_whitespace},
    },
    storage::RunRef,
};

/// `options.trustedContextAuthorizer.authorize`
/// (gateway/protocol/types.ts: GatewayTrustedContextAuthorizer). Absent
/// reproduces TS: CAPABILITY_UNAVAILABLE for any non-empty trustedContext.
pub trait SubmitTrustedAuthorizer: Send + Sync {
    fn authorize<'a>(
        &'a self,
        request: &'a JsValue,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>>;
}

/// The coordinator's command deadline
/// (GatewayManualCompactionCoordinator.ts:30-38): a `timeout:<runId>` abort
/// visible to the router, cleared when the compaction result settles.
#[derive(Clone, Default)]
pub struct CompactionAbort {
    state: Arc<CompactionAbortState>,
}
#[derive(Default)]
struct CompactionAbortState {
    reason: Mutex<Option<JsString>>,
    notified: tokio::sync::Notify,
}
impl CompactionAbort {
    pub fn abort(&self, reason: JsString) {
        *self.state.reason.lock().unwrap() = Some(reason);
        self.state.notified.notify_waiters();
    }
    pub fn reason(&self) -> Option<JsString> {
        self.state.reason.lock().unwrap().clone()
    }
    /// Resolves with the abort reason; the abort may have fired before or
    /// while waiting.
    pub async fn aborted(&self) -> JsString {
        loop {
            let notified = self.state.notified.notified();
            if let Some(reason) = self.reason() {
                return reason;
            }
            notified.await;
        }
    }
}

/// `router.compact(sessionKey, {turnId, abortSignal})` behind
/// GatewayManualCompactionCoordinator.ts:8-13. Native session manual
/// compaction is not ported yet; the embedding supplies the router.
pub trait SubmitCompactionRouter: Send + Sync {
    fn compact<'a>(
        &'a self,
        session: &'a JsString,
        turn: &'a JsString,
        abort: &'a CompactionAbort,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>>;
}

/// `options.setSdkSessionConfig` (InProcessGateway.ts:1085-1115).
pub trait SubmitSdkSessionConfig: Send + Sync {
    fn set<'a>(
        &'a self,
        session: &'a JsString,
        config: &'a JsValue,
        project: Option<&'a JsString>,
        completion: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<bool, SubmitError>>;
}
/// `options.assertSdkModelAllowed` (InProcessGateway.ts:1119-1133).
pub trait SubmitModelAssertion: Send + Sync {
    fn assert(
        &self,
        session: &JsString,
        selection: Option<&JsValue>,
        project: Option<&JsString>,
    ) -> Result<(), SubmitError>;
}
/// `options.taskBudgetSnapshot` (InProcessGateway.ts:1135-1156) resolving
/// `{ totalUsd, spentUsd }`.
pub trait SubmitTaskBudgetSnapshot: Send + Sync {
    fn snapshot<'a>(
        &'a self,
        session: &'a JsString,
        project: Option<&'a JsString>,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>>;
}
/// `options.clearRecoveredUserDialogs` (InProcessGateway.ts:1183-1191).
pub trait SubmitDialogRecovery: Send + Sync {
    fn clear_recovered<'a>(
        &'a self,
        session: &'a JsString,
        project: Option<&'a JsString>,
    ) -> SubmitFuture<'a, Result<(), SubmitError>>;
}

/// The admission assembly: real native coordinators plus the embedding ports.
pub struct SubmitAdmissionOptions {
    pub interactions: Arc<GatewayInteractionCoordinator>,
    pub replacement: Arc<ReplacementCoordinator>,
    pub compaction: Option<Arc<dyn SubmitCompactionRouter>>,
    pub trusted_authorizer: Option<Arc<dyn SubmitTrustedAuthorizer>>,
    pub sdk_config: Option<Arc<dyn SubmitSdkSessionConfig>>,
    pub model_assertion: Option<Arc<dyn SubmitModelAssertion>>,
    pub task_budget: Option<Arc<dyn SubmitTaskBudgetSnapshot>>,
    pub dialog_recovery: Option<Arc<dyn SubmitDialogRecovery>>,
}
impl SubmitAdmissionOptions {
    pub fn new(replacement: Arc<ReplacementCoordinator>) -> Self {
        Self {
            interactions: Arc::new(GatewayInteractionCoordinator::default()),
            replacement,
            compaction: None,
            trusted_authorizer: None,
            sdk_config: None,
            model_assertion: None,
            task_budget: None,
            dialog_recovery: None,
        }
    }
}

pub struct GatewaySubmitAdmission {
    options: SubmitAdmissionOptions,
    /// `dialogProjectKeys` (InProcessGateway.ts:774); consumption is 7b.
    dialog_project_keys: Mutex<HashMap<JsString, JsValue>>,
}
impl GatewaySubmitAdmission {
    pub fn new(options: SubmitAdmissionOptions) -> Self {
        Self {
            options,
            dialog_project_keys: Mutex::new(HashMap::new()),
        }
    }
    pub fn dialog_project_key(&self, session: &JsString) -> Option<JsValue> {
        self.dialog_project_keys
            .lock()
            .unwrap()
            .get(session)
            .cloned()
    }

    /// modelCatalog.ts:92-100 `normalizeSessionModelSelection`: drop retired
    /// preference fields without relaxing validation.
    fn normalize_model_selection(selection: &JsValue) -> JsValue {
        if is(selection, "mode", "auto") {
            return object([("mode", "auto".into())]);
        }
        if !is(selection, "mode", "model") {
            return selection.clone();
        }
        let mut result = JsObject::new();
        result.insert("mode".into(), "model".into());
        result.insert("provider".into(), field(selection, "provider").clone());
        result.insert("model".into(), field(selection, "model").clone());
        for key in ["reasoning", "speed"] {
            if !matches!(field(selection, key), JsValue::Undefined) {
                result.insert(key.into(), field(selection, key).clone());
            }
        }
        JsValue::Object(result)
    }

    /// InProcessGateway.ts:820-833 plus GatewayManualCompactionCoordinator's
    /// whole projection. The compaction runId is independent of the turn's
    /// (TS :827 generates a fresh uuid when the input has none).
    async fn compact_command(&self, input: &JsValue) -> Result<Vec<JsValue>, SubmitError> {
        let run = field(input, "runId")
            .as_string()
            .cloned()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string().into());
        let session = string(field(input, "sessionKey"));
        let Some(router) = self.options.compaction.as_ref() else {
            return Err(SubmitError::new(
                "gateway_submit_failed",
                "router.compact is not configured. Wire the manual compaction router via createLocalGateway.",
            ));
        };
        let timeout = field(input, "timeoutMs")
            .as_number()
            .filter(|ms| ms.is_finite() && *ms > 0.0);
        let abort = CompactionAbort::default();
        let timer = timeout.map(|ms| {
            let abort = abort.clone();
            let mut reason: Vec<u16> = "timeout:".encode_utf16().collect();
            reason.extend_from_slice(run.units());
            let reason = JsString::from_utf16(reason);
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(ms as u64)).await;
                abort.abort(reason);
            })
        });
        let result = router.compact(&session, &run, &abort).await;
        if let Some(timer) = timer {
            timer.abort(); // finally { clearTimeout(timeoutHandle) }
        }
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                // GatewayManualCompactionCoordinator.ts:52-64: busy-shaped
                // failures become session_busy with fixed copy.
                let message = error.message;
                let busy = busy_error(&message);
                return Ok(vec![with_hint(
                    object([
                        ("type", "error".into()),
                        ("runId", JsValue::String(run)),
                        (
                            "code",
                            if busy {
                                "session_busy"
                            } else {
                                "manual_compaction_failed"
                            }
                            .into(),
                        ),
                        (
                            "message",
                            if busy {
                                "Compaction is unavailable because this session has an active turn or queued work.".into()
                            } else {
                                message.as_str().into()
                            },
                        ),
                        ("recoverable", JsValue::Bool(true)),
                    ]),
                    if busy {
                        "Wait for the current work to finish, then run /compact again."
                    } else {
                        "Retry /compact after checking the session state."
                    },
                )]);
            }
        };
        Ok(vec![
            object([
                ("type", "agent_status".into()),
                ("runId", JsValue::String(run.clone())),
                ("event", "manual_compaction".into()),
                (
                    "detail",
                    object([
                        ("outcome", field(&result, "type").clone()),
                        ("turnId", JsValue::String(run.clone())),
                    ]),
                ),
            ]),
            object([
                ("type", "assistant_text_delta".into()),
                ("text", JsValue::String(manual_compaction_text(&result))),
            ]),
            object([
                ("type", "turn_completed".into()),
                ("runId", JsValue::String(run)),
                ("usage", field(&result, "usage").clone()),
                ("finishReason", finish_reason(&result).into()),
            ]),
        ])
    }
}
/// `/active turn|not idle|not available/i` over the failure message.
fn busy_error(message: &str) -> bool {
    static BUSY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new("(?i)active turn|not idle|not available").expect("fixed busy regex")
    });
    BUSY.is_match(message)
}
fn manual_compaction_text(result: &JsValue) -> JsString {
    let text = if is(result, "type", "compacted") {
        format!(
            "Compacted {} history items (~{} tokens).",
            field(result, "messagesSummarized")
                .as_number()
                .unwrap_or(f64::NAN),
            field(result, "preTokens").as_number().unwrap_or(f64::NAN),
        )
    } else if is(result, "type", "skipped") {
        if is(result, "reason", "unavailable") {
            "Compaction is unavailable for this session.".to_string()
        } else {
            "No compactable history yet.".to_string()
        }
    } else if is(result, "type", "aborted") {
        "Compaction cancelled.".to_string()
    } else {
        format!(
            "Compaction failed: {}",
            field(result, "error")
                .as_string()
                .map(|error| String::from_utf16_lossy(error.units()))
                .unwrap_or_default()
        )
    };
    JsString::from_utf16(text.encode_utf16().collect())
}
fn finish_reason(result: &JsValue) -> &'static str {
    if is(result, "type", "aborted") {
        "aborted_streaming"
    } else if is(result, "type", "failed") {
        "model_error"
    } else {
        "completed"
    }
}
/// `/plan(?:\s+([\s\S]*))?` capture: everything after the leading whitespace
/// run, still needing the final trim (InProcessGateway.ts:3585-3595).
fn plan_task(trimmed: &[u16]) -> Option<Option<&[u16]>> {
    if !starts_with16(trimmed, "/plan") {
        return None;
    }
    if trimmed.len() == 5 {
        return Some(None);
    }
    if trimmed.len() > 5 && js_whitespace(trimmed[5]) {
        let start = trimmed[6..]
            .iter()
            .position(|unit| !js_whitespace(*unit))
            .map_or(trimmed.len(), |start| 6 + start);
        return Some(Some(&trimmed[start..]));
    }
    None
}

impl super::submit_host::SubmitAdmission for GatewaySubmitAdmission {
    /// InProcessGateway.ts:765-789: model-selection folding, dialog project
    /// recording and the per-turn interaction reconnect.
    fn normalize<'a>(
        &'a self,
        input: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async move {
            for key in ["modelSelection", "modelOverride"] {
                if truthy(field(input, key)) {
                    let folded = Self::normalize_model_selection(field(input, key));
                    set(input, key, folded);
                }
            }
            if truthy(field(input, "projectKey")) {
                self.dialog_project_keys.lock().unwrap().insert(
                    string(field(input, "sessionKey")),
                    field(input, "projectKey").clone(),
                );
            }
            if truthy(field(input, "interactionBinding")) {
                let session = string(field(input, "sessionKey"));
                let binding = field(input, "interactionBinding");
                // GatewayInteractionCoordinator.ts:79-82 compares the raw
                // connectionId before the port validates the binding, so a
                // differently-bound (even non-string) id is a stale binding,
                // not a validation failure.
                let mismatch =
                    self.options
                        .interactions
                        .get_binding(&session)
                        .is_some_and(|current| {
                            !matches!(binding.get("connectionId"),
                            Some(JsValue::String(id)) if id == &current.connection_id)
                        });
                if !mismatch {
                    let binding = InteractionBinding::from_value(binding).map_err(|message| {
                        SubmitError::new(
                            "gateway_submit_failed",
                            String::from_utf16_lossy(message.units()),
                        )
                    })?;
                    let reconnect = self
                        .options
                        .interactions
                        .reconnect_for_turn(&session, binding)
                        .map_err(|message| {
                            SubmitError::new(
                                "gateway_submit_failed",
                                String::from_utf16_lossy(message.units()),
                            )
                        })?;
                    if !is(&reconnect, "outcome", "stale_binding") {
                        return Ok(vec![]);
                    }
                }
                return Ok(vec![object([
                    ("type", "error".into()),
                    ("code", "interaction_reconnect_required".into()),
                    (
                        "message",
                        "This session has reconnectable interaction requests. Reconnect with the previous binding before submitting a new turn.".into(),
                    ),
                    ("recoverable", JsValue::Bool(true)),
                ])]);
            }
            Ok(vec![])
        })
    }
    /// InProcessGateway.ts:820-889: compact and plan command routing, then
    /// trusted-context authorization.
    fn route_input<'a>(
        &'a self,
        input: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async move {
            let message = field(input, "message")
                .as_string()
                .cloned()
                .ok_or_else(|| {
                    SubmitError::new("gateway_submit_failed", "message.trim is not a function")
                })?;
            let trimmed = trim16(message.units());
            // parseCompactCommand (InProcessGateway.ts:3597-3601):
            // /^\/compact(?:\s|$)/u marks the command, but only exactly
            // "/compact" is valid; any argument shows the usage instead.
            if starts_with16(trimmed, "/compact")
                && (trimmed.len() == 8 || js_whitespace(trimmed[8]))
            {
                if trimmed.len() != 8 {
                    return Ok(vec![
                        object([
                            ("type", "assistant_text_delta".into()),
                            ("text", "用法：/compact".into()),
                        ]),
                        object([
                            ("type", "turn_completed".into()),
                            ("usage", object([])),
                            ("finishReason", "completed".into()),
                        ]),
                    ]);
                }
                return self.compact_command(input).await;
            }
            // parsePlanCommand (InProcessGateway.ts:3585-3595)
            if let Some(task) = plan_task(trimmed) {
                if task.is_none_or(|task| trim16(task).is_empty()) {
                    return Ok(vec![
                        object([
                            ("type", "assistant_text_delta".into()),
                            (
                                "text",
                                "用法：/plan <任务>\n例如：/plan 设计一个新功能".into(),
                            ),
                        ]),
                        object([
                            ("type", "turn_completed".into()),
                            ("usage", object([])),
                            ("finishReason", "completed".into()),
                        ]),
                    ]);
                }
                // normalizePlanCommandInput (InProcessGateway.ts:3567-3583)
                let base = [field(input, "basePermissionMode"), field(input, "mode")]
                    .into_iter()
                    .find(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
                    .cloned()
                    .unwrap_or_else(|| "default".into());
                let task = JsString::from_utf16(trim16(task.unwrap()).to_vec());
                set(input, "message", JsValue::String(task));
                set(input, "runMode", "plan".into());
                set(input, "mode", "plan".into());
                set(input, "basePermissionMode", base);
                set(input, "allowPlanModeTools", JsValue::Bool(true));
            }
            // Trusted-context authorization (InProcessGateway.ts:849-889)
            if let JsValue::Array(contexts) = field(input, "trustedContext")
                && !contexts.is_empty()
            {
                let run = field(input, "runId").clone();
                let Some(authorizer) = self.options.trusted_authorizer.as_ref() else {
                    return Ok(vec![object([
                        ("type", "error".into()),
                        ("runId", run),
                        ("code", "CAPABILITY_UNAVAILABLE".into()),
                        (
                            "message",
                            "trustedContext requires a Gateway trusted-context authorizer.".into(),
                        ),
                        ("recoverable", JsValue::Bool(true)),
                    ])]);
                };
                for context in contexts.iter() {
                    let decision = authorizer
                        .authorize(&object([
                            ("sessionKey", field(input, "sessionKey").clone()),
                            ("projectKey", field(input, "projectKey").clone()),
                            ("channelKey", field(input, "channelKey").clone()),
                            ("context", context.clone()),
                        ]))
                        .await
                        .and_then(|decision| {
                            let valid = ["principal", "source"].iter().all(|key| {
                                decision
                                    .get(key)
                                    .and_then(JsValue::as_string)
                                    .is_some_and(|value| !trim16(value.units()).is_empty())
                            });
                            if valid {
                                Ok(decision)
                            } else {
                                Err(SubmitError::new(
                                    "INVALID_TRUSTED_CONTEXT_AUTHORIZATION",
                                    "Trusted context authorizer returned an invalid decision.",
                                ))
                            }
                        });
                    // TS catches both the invalid-decision throw and an
                    // authorizer rejection and yields one error event.
                    if let Err(error) = decision {
                        return Ok(vec![object([
                            ("type", "error".into()),
                            ("runId", run),
                            ("code", error.code.as_str().into()),
                            ("message", error.message.as_str().into()),
                            ("recoverable", JsValue::Bool(true)),
                        ])]);
                    }
                }
                // Authorized results are not written back to the input; their
                // consumption (synthetic messages) is 7b.
            }
            Ok(vec![])
        })
    }
    /// InProcessGateway.ts:956-1013: the replacement claim and the transcript
    /// write reservation. A conflict holds no claim, so the submitter's
    /// release is a no-op for it, exactly like TS releasePreflight.
    fn preflight<'a>(
        &'a self,
        _: &'a JsValue,
        run: &'a RunRef,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async move {
            let session = &run.session_key;
            if self
                .options
                .replacement
                .claim_for_submit(session, &run.run_id)
                == SubmitClaim::Conflict
            {
                return Ok(vec![with_hint(
                    error_event(
                        run,
                        "replace_turn_pending",
                        "This session is waiting for its edited replacement turn to be accepted.",
                        true,
                    ),
                    "Wait for the edited message transaction to finish, then try again.",
                )]);
            }
            if self
                .options
                .replacement
                .has_transcript_write_reservation(session)
            {
                return Ok(vec![with_hint(
                    error_event(
                        run,
                        "replace_turn_pending",
                        "This session transcript is currently being updated.",
                        true,
                    ),
                    "Wait for the session update to finish, then try again.",
                )]);
            }
            Ok(vec![])
        })
    }
    /// InProcessGateway.ts:1116-1203; the optional 7b ports. Port-absent
    /// behavior mirrors the TS option-absent branches.
    fn admitted_preflight<'a>(
        &'a self,
        input: &'a JsValue,
        run: &'a RunRef,
        completion: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async move {
            if let Some(assertion) = self.options.model_assertion.as_ref() {
                // explicitModel = modelSelection.mode==="model" ?
                // modelSelection : modelOverride (TS :1116-1118)
                let explicit = if is(field(input, "modelSelection"), "mode", "model") {
                    field(input, "modelSelection")
                } else {
                    field(input, "modelOverride")
                };
                let explicit = matches!(explicit, JsValue::Object(_)).then_some(explicit);
                if let Err(error) =
                    assertion.assert(&run.session_key, explicit, run.project_key.as_ref())
                {
                    return Ok(vec![error_event(run, &error.code, &error.message, true)]);
                }
            }
            if let Some(budget) = self.options.task_budget.as_ref() {
                let budget = budget
                    .snapshot(&run.session_key, run.project_key.as_ref())
                    .await?;
                if completion.is_cancelled() {
                    return Ok(vec![aborted(run)]);
                }
                let (total, spent) = (
                    field(&budget, "totalUsd").as_number().unwrap_or(f64::NAN),
                    field(&budget, "spentUsd").as_number().unwrap_or(f64::NAN),
                );
                if spent >= total {
                    return Ok(vec![
                        with_hint(
                            error_event(
                                run,
                                "agent_task_budget_reached",
                                &format!(
                                    "Reached Gateway-owned taskBudget.total (${total:.6}) after spending ${spent:.6}."
                                ),
                                false,
                            ),
                            "Increase taskBudget.total or start a new SDK session with a larger budget.",
                        ),
                        object([
                            ("type", "turn_completed".into()),
                            ("runId", JsValue::String(run.run_id.clone())),
                            ("usage", object([])),
                            ("finishReason", "task_budget".into()),
                        ]),
                    ]);
                }
            } else if truthy(field(field(input, "sdkSessionConfig"), "taskBudget")) {
                // TS :1171-1181: a taskBudget configuration without the
                // accounting option is an explicit capability refusal.
                return Ok(vec![error_event(
                    run,
                    "CAPABILITY_UNAVAILABLE",
                    "Gateway task-budget accounting is unavailable.",
                    true,
                )]);
            }
            if let Some(recovery) = self.options.dialog_recovery.as_ref() {
                if let Err(error) = recovery
                    .clear_recovered(&run.session_key, run.project_key.as_ref())
                    .await
                {
                    return Ok(vec![with_hint(
                        error_event(
                            run,
                            "gateway_dialog_recovery_cleanup_failed",
                            &error.message,
                            true,
                        ),
                        "Retry the turn after the Gateway storage is available.",
                    )]);
                }
                if completion.is_cancelled() {
                    return Ok(vec![aborted(run)]);
                }
            }
            Ok(vec![])
        })
    }
    /// Permission-mode migration, model telemetry and task-budget spend are
    /// 7b; without them there is nothing to observe before projection.
    fn observe_event<'a>(
        &'a self,
        _: &'a JsValue,
        _: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
    }
    fn has_sdk_config(&self) -> bool {
        self.options.sdk_config.is_some()
    }
    fn validate_sdk_config(&self, _: &JsValue) -> Result<(), SubmitError> {
        // The submitter only asks after has_sdk_config(); a wired port owns
        // its validation (7b wires the native validator).
        Ok(())
    }
    fn set_sdk_config<'a>(
        &'a self,
        input: &'a JsValue,
        completion: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<bool, SubmitError>> {
        let port = self.options.sdk_config.clone();
        Box::pin(async move {
            let Some(port) = port else {
                return Err(SubmitError::new(
                    "CAPABILITY_UNAVAILABLE",
                    "sdk_session_config is unavailable.",
                ));
            };
            port.set(
                &string(field(input, "sessionKey")),
                field(input, "sdkSessionConfig"),
                field(input, "projectKey").as_string(),
                completion,
            )
            .await
        })
    }
    /// InProcessGateway.ts:1816 rejectPendingTurn.
    fn reject_pending(&self, session: &JsString, reason: &str) -> Result<(), SubmitError> {
        self.options
            .interactions
            .reject_pending_turn(session, &reason.into());
        Ok(())
    }
}

fn with_hint(mut event: JsValue, hint: &str) -> JsValue {
    set(&mut event, "userHint", hint.into());
    event
}
fn starts_with16(units: &[u16], text: &str) -> bool {
    let bytes = text.as_bytes();
    units.len() >= bytes.len() && bytes.iter().zip(units).all(|(a, b)| u16::from(*a) == *b)
}
fn trim16(units: &[u16]) -> &[u16] {
    let start = units
        .iter()
        .position(|u| !js_whitespace(*u))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|u| !js_whitespace(*u))
        .map_or(start, |i| i + 1);
    &units[start..end]
}

#[cfg(test)]
#[path = "submit_admission_tests.rs"]
mod tests;

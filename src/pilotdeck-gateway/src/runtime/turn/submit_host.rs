//! Production SubmitHost over the embedding gateway's services.
//! InProcessGateway.ts:954,1269-1650,1762-1856 wired by createLocalGateway.ts:1021-1504.
//!
//! This host owns the embedding-service windows of [`SubmitHost`]: workspace
//! identity and snapshot pairing, run policy read after the turn-start refresh,
//! the attachment-lease/model-selection/config prepare window, accepted-input
//! commits, status recording, exactly-once lease release on pump finish and
//! after-turn. Admission (normalize/route/preflight/SDK config/pending
//! rejection) is delegated to the admission host; the WS adapter and final
//! assembly are separate packages. Snapshot capture stays behind the existing
//! [`SubmitSnapshots`] port (`NativeSubmitSnapshots` in production).
use std::{collections::HashMap, sync::Arc};

use pilotdeck_protocol::{JsString, JsValue};

use super::submit::{
    PreparedTurn, SubmitError, SubmitFuture, SubmitHost, SubmitSnapshots, SubmitWorkspace,
};
use crate::storage::RunRef;

/// `options.setSessionCwd`. Unlike TS, which silently skips an unconfigured
/// setSessionCwd (InProcessGateway.ts:1269), a requested cwd is mandatory host
/// registration: an absent service is an explicit capability refusal.
pub trait SubmitCwdService: Send + Sync {
    fn set(&self, session: &JsString, cwd: &JsString) -> Result<(), SubmitError>;
}

/// `resolveUploadedAttachments`: merged attachments plus a release that the
/// host runs exactly once per acquired lease (InProcessGateway.ts:1476-1478,
/// 1767-1773).
pub struct SubmitAttachmentLease {
    pub attachments: Vec<JsValue>,
    pub release: SubmitLeaseRelease,
}
pub type SubmitLeaseRelease =
    Box<dyn FnOnce() -> SubmitFuture<'static, Result<(), SubmitError>> + Send>;

/// The embedding's turn services, mirroring the TS gateway options:
/// workspace identity/snapshots/cwd, run policy and turn-start refresh,
/// attachment lease and turn composition, model selection, status recording,
/// after-turn, clock and abort budget.
pub trait SubmitHostServices: Send + Sync {
    /// `options.workspaceId ?? workspaceIdResolver(projectKey)`; errors refuse
    /// the turn before admission (InProcessGateway.ts:954).
    fn workspace_id(&self, input: &JsValue) -> Result<Option<JsString>, SubmitError>;
    /// `options.snapshotRecorder`; None when workspace snapshotting is
    /// unavailable. Required-but-unavailable is refused by the submitter.
    fn snapshots(&self) -> Option<Arc<dyn SubmitSnapshots>>;
    /// Deployment policy making the recorder mandatory for workspace turns.
    fn snapshots_required(&self) -> bool;
    /// The session cwd registration service, when the embedding has one.
    fn cwd(&self) -> Option<Arc<dyn SubmitCwdService>>;
    /// `refreshConfigBeforeTurn`; errors keep the previous config snapshot
    /// (the pump swallows them, InProcessGateway.ts:1336-1343).
    fn refresh(&self) -> SubmitFuture<'_, Result<(), SubmitError>>;
    /// `resolveRunPolicy`, read after refresh (InProcessGateway.ts:1346).
    fn run_policy(&self) -> Option<JsValue>;
    /// `resolveUploadedAttachments(input)`, resolved only when
    /// `input.uploadedAttachments` is a non-empty array.
    fn resolve_attachments<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> SubmitFuture<'a, Result<SubmitAttachmentLease, SubmitError>>;
    /// `prepareAttachmentTurn(message, attachments, projectKey, sessionKey)`
    /// (InProcessGateway.ts:1481-1486). Input attachments precede lease ones.
    fn prepare_attachment_turn<'a>(
        &'a self,
        message: &'a JsValue,
        attachments: Vec<JsValue>,
        project: Option<&JsString>,
        session: Option<&JsString>,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>>;
    /// `resolveTurnModelSelection(input)` → `{ source, selection? }`
    /// (InProcessGateway.ts:1509-1515). createLocalGateway always wires the
    /// resolver, so the TS no-resolver fallback chain is unreachable here.
    fn resolve_model_selection<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>>;
    /// `recordAgentStatusMessage`.
    fn record_failure<'a>(
        &'a self,
        run: &'a RunRef,
        status: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>>;
    /// `afterTurnCompleted` (InProcessGateway.ts:1850).
    fn after_turn(&self, run: &RunRef) -> Result<(), SubmitError>;
    /// The wall clock used for operation deadlines.
    fn now_ms(&self) -> f64;
    /// The hard timeout budget for a timed-out pump to reach quiescence.
    fn abort_turn_timeout_ms(&self) -> u64;
}

/// `turnReplacementCoordinator` as seen by a submission: the submit claim and
/// the accepted-input commit. The native coordinator's commit is best effort
/// (a backup-removal failure only warns), so the adapter never fails; the
/// port keeps the Result for embeddings that surface refusals.
pub trait SubmitReplacement: Send + Sync {
    fn commit_accepted_input<'a>(
        &'a self,
        session: &'a JsString,
        run: &'a JsString,
    ) -> SubmitFuture<'a, Result<(), SubmitError>>;
    fn release_claim(&self, session: &JsString, run: &JsString);
}
/// Adapter over the native [`ReplacementCoordinator`]; its
/// `commit_accepted_input` has already warned on failure internally.
pub struct NativeSubmitReplacement(pub crate::runtime::replacement::ReplacementCoordinator);
impl SubmitReplacement for NativeSubmitReplacement {
    fn commit_accepted_input<'a>(
        &'a self,
        session: &'a JsString,
        run: &'a JsString,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async move {
            self.0.commit_accepted_input(session, run).await;
            Ok(())
        })
    }
    fn release_claim(&self, session: &JsString, run: &JsString) {
        self.0.release_submit_claim(session, run)
    }
}

/// Admission windows assembled by the admission packages: session-model
/// normalization/interaction reconnect, command routing, replacement claims
/// and model/budget/dialog-recovery preflight, SDK session configuration and
/// pending-turn rejection. They return wire events (including usage/command
/// short-circuits); empty means admission may proceed.
pub trait SubmitAdmission: Send + Sync {
    fn normalize<'a>(
        &'a self,
        input: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>>;
    fn route_input<'a>(
        &'a self,
        input: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>>;
    fn preflight<'a>(
        &'a self,
        input: &'a JsValue,
        run: &'a RunRef,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>>;
    fn admitted_preflight<'a>(
        &'a self,
        input: &'a JsValue,
        run: &'a RunRef,
        completion: &'a crate::runtime::session::TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>>;
    fn observe_event<'a>(
        &'a self,
        input: &'a JsValue,
        event: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>>;
    fn has_sdk_config(&self) -> bool;
    fn validate_sdk_config(&self, config: &JsValue) -> Result<(), SubmitError>;
    fn set_sdk_config<'a>(
        &'a self,
        input: &'a JsValue,
        completion: &'a crate::runtime::session::TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<bool, SubmitError>>;
    fn reject_pending(&self, session: &JsString, reason: &str) -> Result<(), SubmitError>;
}

/// One release per acquired lease, keyed by (session, run): `finish_pump`
/// removes the entry before releasing, so a lease is released exactly once
/// even if the pump future is settled along a timeout or failure path.
type SubmitLeases = HashMap<(JsString, JsString), SubmitLeaseRelease>;

/// The production host: embedding services plus the admission host.
pub struct GatewaySubmitHost {
    services: Arc<dyn SubmitHostServices>,
    replacement: Arc<dyn SubmitReplacement>,
    admission: Arc<dyn SubmitAdmission>,
    leases: std::sync::Mutex<SubmitLeases>,
}
impl GatewaySubmitHost {
    pub fn new(
        services: Arc<dyn SubmitHostServices>,
        replacement: Arc<dyn SubmitReplacement>,
        admission: Arc<dyn SubmitAdmission>,
    ) -> Self {
        Self {
            services,
            replacement,
            admission,
            leases: std::sync::Mutex::new(HashMap::new()),
        }
    }
}
impl SubmitHost for GatewaySubmitHost {
    fn workspace(&self, input: &JsValue) -> Result<SubmitWorkspace, SubmitError> {
        Ok(SubmitWorkspace {
            id: self.services.workspace_id(input)?,
            snapshots_required: self.services.snapshots_required(),
            recorder: self.services.snapshots(),
        })
    }
    fn failure_guard_config(&self) -> Option<JsValue> {
        self.services
            .run_policy()
            .and_then(|policy| policy.get("failureGuard").cloned())
    }
    fn set_session_cwd(&self, session: &JsString, cwd: &JsString) -> Result<(), SubmitError> {
        match self.services.cwd() {
            Some(service) => service.set(session, cwd),
            None => Err(SubmitError::new(
                "CAPABILITY_UNAVAILABLE",
                "Session cwd host capability is unavailable.",
            )),
        }
    }
    fn now_ms(&self) -> f64 {
        self.services.now_ms()
    }
    fn abort_turn_timeout_ms(&self) -> u64 {
        self.services.abort_turn_timeout_ms()
    }
    fn refresh(&self) -> SubmitFuture<'_, Result<(), SubmitError>> {
        self.services.refresh()
    }
    fn prepare<'a>(
        &'a self,
        input: &'a JsValue,
        run: &'a RunRef,
    ) -> SubmitFuture<'a, Result<PreparedTurn, SubmitError>> {
        Box::pin(async move {
            // TS :1476 resolves the lease only for a non-empty array; an
            // empty/absent field keeps the turn attachment-free.
            let lease = match non_empty_array(field(input, "uploadedAttachments")) {
                true => Some(self.services.resolve_attachments(input).await?),
                false => None,
            };
            let mut attachments = match field(input, "attachments") {
                JsValue::Array(values) => values.to_vec(),
                _ => Vec::new(),
            };
            if let Some(lease) = &lease {
                attachments.extend(lease.attachments.iter().cloned());
            }
            // The lease is owned from here: a failure in any later prepare
            // step still reaches finish_pump, which releases it.
            if let Some(lease) = lease {
                // A poisoned lock must not strand the lease: the map itself
                // is never left half-updated (a panicking holder cannot
                // observe the insert), so recovery takes the data through.
                self.leases
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert((run.session_key.clone(), run.run_id.clone()), lease.release);
            }
            let agent_input = self
                .services
                .prepare_attachment_turn(
                    field(input, "message"),
                    attachments,
                    field(input, "projectKey").as_string(),
                    field(input, "sessionKey").as_string(),
                )
                .await?;
            let model_selection = self.services.resolve_model_selection(input).await?;
            Ok(PreparedTurn {
                model_selection,
                input: agent_input,
                options: crate::metadata::object([
                    ("turnId", JsValue::String(run.run_id.clone())),
                    (
                        "execution",
                        crate::metadata::object([
                            ("runId", JsValue::String(run.run_id.clone())),
                            ("operationId", JsValue::String(run.run_id.clone())),
                        ]),
                    ),
                ]),
            })
        })
    }
    fn commit_accepted_input<'a>(
        &'a self,
        run: &'a RunRef,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        self.replacement
            .commit_accepted_input(&run.session_key, &run.run_id)
    }
    fn release_claim(&self, session: &JsString, run: &JsString) {
        self.replacement.release_claim(session, run)
    }
    fn record_failure<'a>(
        &'a self,
        run: &'a RunRef,
        status: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        self.services.record_failure(run, status)
    }
    fn finish_pump<'a>(&'a self, run: &'a RunRef) -> SubmitFuture<'a, Result<(), SubmitError>> {
        // Removal before release is the exactly-once guard: repeated or
        // racing finish_pump calls find no entry for the second caller. The
        // lock is taken poison-tolerantly: a panic elsewhere must not block
        // the pump's tail from releasing an acquired lease.
        let release = self
            .leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&(run.session_key.clone(), run.run_id.clone()));
        Box::pin(async move {
            match release {
                Some(release) => release().await,
                None => Ok(()),
            }
        })
    }
    fn after_turn(&self, run: &RunRef) -> Result<(), SubmitError> {
        self.services.after_turn(run)
    }
    // Admission windows keep their existing ports.
    fn normalize<'a>(
        &'a self,
        input: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        self.admission.normalize(input)
    }
    fn route_input<'a>(
        &'a self,
        input: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        self.admission.route_input(input)
    }
    fn preflight<'a>(
        &'a self,
        input: &'a JsValue,
        run: &'a RunRef,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        self.admission.preflight(input, run)
    }
    fn admitted_preflight<'a>(
        &'a self,
        input: &'a JsValue,
        run: &'a RunRef,
        completion: &'a crate::runtime::session::TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        self.admission.admitted_preflight(input, run, completion)
    }
    fn observe_event<'a>(
        &'a self,
        input: &'a JsValue,
        event: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        self.admission.observe_event(input, event)
    }
    fn has_sdk_config(&self) -> bool {
        self.admission.has_sdk_config()
    }
    fn validate_sdk_config(&self, config: &JsValue) -> Result<(), SubmitError> {
        self.admission.validate_sdk_config(config)
    }
    fn set_sdk_config<'a>(
        &'a self,
        input: &'a JsValue,
        completion: &'a crate::runtime::session::TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<bool, SubmitError>> {
        self.admission.set_sdk_config(input, completion)
    }
    fn reject_pending(&self, session: &JsString, reason: &str) -> Result<(), SubmitError> {
        self.admission.reject_pending(session, reason)
    }
}

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or({
        static UNDEFINED: JsValue = JsValue::Undefined;
        &UNDEFINED
    })
}
fn non_empty_array(value: &JsValue) -> bool {
    matches!(value, JsValue::Array(values) if !values.is_empty())
}

#[cfg(test)]
#[path = "submit_host_tests.rs"]
mod tests;

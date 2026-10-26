//! Production-host window tests. The oracle rerun in `submit_tests.rs` covers
//! the TS-measured matrix cells (refresh failure, epoch change, commit×abort,
//! timeout×commit, lease release through a full turn); these tests pin the
//! production host's own composition: exactly-once lease release, the
//! attachment merge order measured by the TS probe, the post-refresh policy
//! read, capability refusals and the native replacement adapter.
use super::*;
use crate::{
    metadata::object,
    runtime::{
        replacement::{ReplacementCoordinator, ReplacementSession, ReplacementStorage},
        session::TurnCompletionHandle,
    },
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct FakeSnapshots;
impl SubmitSnapshots for FakeSnapshots {
    fn capture<'a>(
        &'a self,
        _: crate::storage::WorkspaceSnapshotInput,
        _: bool,
    ) -> SubmitFuture<'a, Result<crate::storage::WorkspaceSnapshotResult, SubmitError>> {
        Box::pin(async { unreachable!("capture is not part of these windows") })
    }
}
struct FakeCwd(Mutex<Vec<(String, String)>>);
impl SubmitCwdService for FakeCwd {
    fn set(&self, session: &JsString, cwd: &JsString) -> Result<(), SubmitError> {
        self.0
            .lock()
            .unwrap()
            .push((session.to_utf8().unwrap(), cwd.to_utf8().unwrap()));
        Ok(())
    }
}
/// Configurable services; defaults reproduce the oracle harness ports.
struct Fake {
    releases: Arc<AtomicUsize>,
    resolved: Arc<AtomicUsize>,
    attachments_seen: Mutex<Vec<Vec<String>>>,
    policy: Mutex<JsValue>,
    refresh_failure: bool,
    prepare_failure: bool,
    model_selection_failure: bool,
    workspace_id: Option<JsString>,
    workspace_error: bool,
    snapshots_required: bool,
    snapshots: bool,
    cwd: Option<Arc<FakeCwd>>,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            releases: Arc::new(AtomicUsize::new(0)),
            resolved: Arc::new(AtomicUsize::new(0)),
            attachments_seen: Mutex::new(Vec::new()),
            policy: Mutex::new(JsValue::Undefined),
            refresh_failure: false,
            prepare_failure: false,
            model_selection_failure: false,
            workspace_id: None,
            workspace_error: false,
            snapshots_required: false,
            snapshots: false,
            cwd: None,
        }
    }
}
impl Fake {
    fn input(&self, fields: &[(&'static str, JsValue)]) -> JsValue {
        let mut input = object([
            ("sessionKey", "s".into()),
            ("projectKey", "p".into()),
            ("runId", "r".into()),
            ("message", "hello".into()),
        ]);
        for (key, value) in fields {
            if let JsValue::Object(object) = &mut input {
                object.insert((*key).into(), value.clone());
            }
        }
        input
    }
    fn attachment(id: &str) -> JsValue {
        object([("uploadId", id.into())])
    }
}
impl SubmitHostServices for Fake {
    fn workspace_id(&self, _: &JsValue) -> Result<Option<JsString>, SubmitError> {
        if self.workspace_error {
            return Err(SubmitError::new("WORKSPACE", "injected resolver failure"));
        }
        Ok(self.workspace_id.clone())
    }
    fn snapshots(&self) -> Option<Arc<dyn SubmitSnapshots>> {
        self.snapshots
            .then(|| Arc::new(FakeSnapshots) as Arc<dyn SubmitSnapshots>)
    }
    fn snapshots_required(&self) -> bool {
        self.snapshots_required
    }
    fn cwd(&self) -> Option<Arc<dyn SubmitCwdService>> {
        self.cwd.clone().map(|cwd| cwd as Arc<dyn SubmitCwdService>)
    }
    fn refresh(&self) -> SubmitFuture<'_, Result<(), SubmitError>> {
        Box::pin(async {
            if self.refresh_failure {
                Err(SubmitError::new("CONFIG", "transient config error"))
            } else {
                Ok(())
            }
        })
    }
    fn run_policy(&self) -> Option<JsValue> {
        Some(object([(
            "failureGuard",
            self.policy.lock().unwrap().clone(),
        )]))
    }
    fn resolve_attachments<'a>(
        &'a self,
        _: &'a JsValue,
    ) -> SubmitFuture<'a, Result<SubmitAttachmentLease, SubmitError>> {
        self.resolved.fetch_add(1, Ordering::SeqCst);
        let releases = self.releases.clone();
        let lease = SubmitAttachmentLease {
            attachments: vec![Self::attachment("lease-b")],
            release: Box::new(move || {
                Box::pin(async move {
                    releases.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        };
        Box::pin(async move { Ok(lease) })
    }
    fn prepare_attachment_turn<'a>(
        &'a self,
        _: &'a JsValue,
        attachments: Vec<JsValue>,
        _: Option<&JsString>,
        _: Option<&JsString>,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        self.attachments_seen.lock().unwrap().push(
            attachments
                .iter()
                .map(|value| {
                    value
                        .get("uploadId")
                        .and_then(JsValue::as_string)
                        .unwrap()
                        .to_utf8()
                        .unwrap()
                })
                .collect(),
        );
        Box::pin(async {
            if self.prepare_failure {
                Err(SubmitError::new("HISTORY_EPOCH", "history epoch changed"))
            } else {
                Ok(object([("type", "text".into()), ("text", "hello".into())]))
            }
        })
    }
    fn resolve_model_selection<'a>(
        &'a self,
        _: &'a JsValue,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        Box::pin(std::future::ready(if self.model_selection_failure {
            Err(SubmitError::new(
                "MODEL_SELECTION",
                "injected selection failure",
            ))
        } else {
            Ok(object([("source", "default".into())]))
        }))
    }
    fn record_failure<'a>(
        &'a self,
        _: &'a RunRef,
        _: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(std::future::ready(Ok(())))
    }
    fn after_turn(&self, _: &RunRef) -> Result<(), SubmitError> {
        Ok(())
    }
    fn now_ms(&self) -> f64 {
        0.0
    }
    fn abort_turn_timeout_ms(&self) -> u64 {
        20
    }
}
struct NoopAdmission;
impl SubmitAdmission for NoopAdmission {
    fn normalize<'a>(
        &'a self,
        _: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(std::future::ready(Ok(vec![])))
    }
    fn route_input<'a>(
        &'a self,
        _: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(std::future::ready(Ok(vec![])))
    }
    fn preflight<'a>(
        &'a self,
        _: &'a JsValue,
        _: &'a RunRef,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(std::future::ready(Ok(vec![])))
    }
    fn admitted_preflight<'a>(
        &'a self,
        _: &'a JsValue,
        _: &'a RunRef,
        _: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(std::future::ready(Ok(vec![])))
    }
    fn observe_event<'a>(
        &'a self,
        _: &'a JsValue,
        _: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(std::future::ready(Ok(())))
    }
    fn has_sdk_config(&self) -> bool {
        false
    }
    fn validate_sdk_config(&self, _: &JsValue) -> Result<(), SubmitError> {
        Ok(())
    }
    fn set_sdk_config<'a>(
        &'a self,
        _: &'a JsValue,
        _: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<bool, SubmitError>> {
        Box::pin(std::future::ready(Ok(false)))
    }
    fn reject_pending(&self, _: &JsString, _: &str) -> Result<(), SubmitError> {
        Ok(())
    }
}
struct RecordingReplacement(Mutex<Vec<&'static str>>);
impl SubmitReplacement for RecordingReplacement {
    fn commit_accepted_input<'a>(
        &'a self,
        _: &'a JsString,
        _: &'a JsString,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        self.0.lock().unwrap().push("commit");
        Box::pin(std::future::ready(Ok(())))
    }
    fn release_claim(&self, _: &JsString, _: &JsString) {
        self.0.lock().unwrap().push("release");
    }
}
fn make_host(fake: &Arc<Fake>) -> Arc<GatewaySubmitHost> {
    Arc::new(GatewaySubmitHost::new(
        fake.clone(),
        Arc::new(RecordingReplacement(Mutex::new(Vec::new()))),
        Arc::new(NoopAdmission),
    ))
}
fn run() -> RunRef {
    RunRef {
        session_key: "s".into(),
        project_key: None,
        run_id: "r".into(),
    }
}

/// Matrix cell 5: the lease is released exactly once per acquired lease, even
/// across repeated finish_pump calls (removal precedes release). The release
/// counter is asserted after each call: a hypothetical double-release path
/// (the guard demoted from `remove` to a re-observable handle) cannot be
/// written against the `FnOnce` ownership type, so the repeated-call
/// assertion is the observable guard for it.
#[tokio::test]
async fn finish_pump_releases_a_lease_exactly_once() {
    let fake = Arc::new(Fake::default());
    let host = make_host(&fake);
    let input = fake.input(&[(
        "uploadedAttachments",
        JsValue::Array(vec![Fake::attachment("uploaded")]),
    )]);
    host.prepare(&input, &run()).await.unwrap();
    host.finish_pump(&run()).await.unwrap();
    assert_eq!(
        fake.releases.load(Ordering::SeqCst),
        1,
        "a prepared lease must be released exactly once"
    );
    host.finish_pump(&run()).await.unwrap();
    assert_eq!(
        fake.releases.load(Ordering::SeqCst),
        1,
        "a second finish_pump must not release again"
    );
    host.finish_pump(&run()).await.unwrap();
    assert_eq!(
        fake.releases.load(Ordering::SeqCst),
        1,
        "a third finish_pump must not release again"
    );
}

/// DeepSeek P1-1: a panic that poisons the leases mutex must not block a
/// later finish_pump from removing and releasing the acquired lease, nor a
/// later turn from registering its own lease.
#[tokio::test]
async fn finish_pump_releases_a_lease_after_the_leases_mutex_is_poisoned() {
    let fake = Arc::new(Fake::default());
    let host = make_host(&fake);
    let input = fake.input(&[(
        "uploadedAttachments",
        JsValue::Array(vec![Fake::attachment("uploaded")]),
    )]);
    host.prepare(&input, &run()).await.unwrap();
    let leases = &host.leases;
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = leases.lock().unwrap();
        panic!("poison the leases mutex mid-turn");
    }));
    assert!(poisoned.is_err(), "the mutex must actually be poisoned");
    assert!(host.leases.is_poisoned());
    host.finish_pump(&run()).await.unwrap();
    assert_eq!(fake.releases.load(Ordering::SeqCst), 1);
    // A second turn under the same poisoned host registers and releases its
    // own lease.
    host.prepare(&input, &run()).await.unwrap();
    host.finish_pump(&run()).await.unwrap();
    assert_eq!(fake.releases.load(Ordering::SeqCst), 2);
}

/// DeepSeek P2-2: the lease is owned from resolution, so a failure in the
/// last prepare step (resolve_model_selection) still reaches finish_pump.
#[tokio::test]
async fn lease_is_released_when_model_selection_fails() {
    let fake = Arc::new(Fake {
        model_selection_failure: true,
        ..Fake::default()
    });
    let host = make_host(&fake);
    let input = fake.input(&[(
        "uploadedAttachments",
        JsValue::Array(vec![Fake::attachment("uploaded")]),
    )]);
    let error = host.prepare(&input, &run()).await.err().unwrap();
    assert_eq!(error.code, "MODEL_SELECTION");
    assert_eq!(
        *fake.attachments_seen.lock().unwrap(),
        vec![vec!["lease-b".to_string()]],
        "prepare_attachment_turn ran with the merged attachments"
    );
    host.finish_pump(&run()).await.unwrap();
    assert_eq!(fake.releases.load(Ordering::SeqCst), 1);
}

/// TS probe ts-probe-attachment-merge.out: empty uploadedAttachments keeps the
/// turn attachment-free; absent input.attachments is an empty merge source.
#[tokio::test]
async fn lease_is_not_acquired_without_uploaded_attachments() {
    let fake = Arc::new(Fake::default());
    let host = make_host(&fake);
    for input in [
        fake.input(&[]),
        fake.input(&[("uploadedAttachments", JsValue::Array(vec![]))]),
    ] {
        host.prepare(&input, &run()).await.unwrap();
        host.finish_pump(&run()).await.unwrap();
    }
    assert_eq!(fake.resolved.load(Ordering::SeqCst), 0);
    assert_eq!(fake.releases.load(Ordering::SeqCst), 0);
    assert_eq!(
        *fake.attachments_seen.lock().unwrap(),
        vec![Vec::<String>::new(), Vec::<String>::new()]
    );
}

/// TS probe ts-probe-attachment-merge.out: prepareAttachmentTurn receives
/// input.attachments before lease attachments (["input-a","input-b","lease-b"]).
#[tokio::test]
async fn prepare_merges_input_attachments_before_lease_attachments() {
    let fake = Arc::new(Fake::default());
    let host = make_host(&fake);
    let input = fake.input(&[
        (
            "attachments",
            JsValue::Array(vec![
                Fake::attachment("input-a"),
                Fake::attachment("input-b"),
            ]),
        ),
        (
            "uploadedAttachments",
            JsValue::Array(vec![Fake::attachment("uploaded")]),
        ),
    ]);
    let prepared = host.prepare(&input, &run()).await.unwrap();
    assert_eq!(
        *fake.attachments_seen.lock().unwrap(),
        vec![vec![
            "input-a".to_string(),
            "input-b".to_string(),
            "lease-b".to_string()
        ]]
    );
    // The observable option surface of the oracle's fake session: turnId and
    // execution{runId, operationId} (durableCalls op:execution).
    assert_eq!(
        prepared.options.get("turnId").and_then(JsValue::as_string),
        Some(&"r".into())
    );
    let execution = prepared.options.get("execution").unwrap();
    assert_eq!(
        execution.get("runId").and_then(JsValue::as_string),
        Some(&"r".into())
    );
    assert_eq!(
        execution.get("operationId").and_then(JsValue::as_string),
        Some(&"r".into())
    );
    assert_eq!(
        prepared
            .model_selection
            .get("source")
            .and_then(JsValue::as_string),
        Some(&"default".into())
    );
}

/// The lease is owned from resolution: a failure in a later prepare step still
/// reaches finish_pump, which releases it once (TS pump finally, :1767-1773).
#[tokio::test]
async fn lease_is_released_when_a_later_prepare_step_fails() {
    let fake = Arc::new(Fake {
        prepare_failure: true,
        ..Fake::default()
    });
    let host = make_host(&fake);
    let input = fake.input(&[(
        "uploadedAttachments",
        JsValue::Array(vec![Fake::attachment("uploaded")]),
    )]);
    let error = host.prepare(&input, &run()).await.err().unwrap();
    assert_eq!(error.code, "HISTORY_EPOCH");
    host.finish_pump(&run()).await.unwrap();
    assert_eq!(fake.releases.load(Ordering::SeqCst), 1);
}

/// Matrix cell 6: the guard config is read from the run policy after the
/// turn-start refresh (InProcessGateway.ts:1336-1347), so a refresh failure
/// keeps the previous snapshot and a refreshed policy takes effect.
#[tokio::test]
async fn failure_guard_config_is_read_after_refresh() {
    let fake = Arc::new(Fake::default());
    let host = make_host(&fake);
    assert_eq!(host.failure_guard_config(), Some(JsValue::Undefined));
    let enabled = object([
        ("enabled", JsValue::Bool(true)),
        ("modelFailureLimit", JsValue::Number(2.0)),
    ]);
    *fake.policy.lock().unwrap() = enabled.clone();
    assert_eq!(host.failure_guard_config(), Some(enabled.clone()));
    let failed = Arc::new(Fake {
        refresh_failure: true,
        policy: Mutex::new(enabled.clone()),
        ..Fake::default()
    });
    let failed_host = make_host(&failed);
    assert!(
        failed_host.refresh().await.is_err(),
        "refresh errors surface to the pump, which swallows them"
    );
    assert_eq!(failed_host.failure_guard_config(), Some(enabled));
}

#[test]
fn set_session_cwd_refuses_without_a_service_and_delegates_with_one() {
    let fake = Arc::new(Fake::default());
    let host = make_host(&fake);
    let error = host
        .set_session_cwd(&"s".into(), &"/work".into())
        .unwrap_err();
    assert_eq!(error.code, "CAPABILITY_UNAVAILABLE");
    assert_eq!(error.message, "Session cwd host capability is unavailable.");
    let cwd = Arc::new(FakeCwd(Mutex::new(Vec::new())));
    let fake = Arc::new(Fake {
        cwd: Some(cwd.clone()),
        ..Fake::default()
    });
    let host = make_host(&fake);
    host.set_session_cwd(&"s".into(), &"/work".into()).unwrap();
    assert_eq!(
        *cwd.0.lock().unwrap(),
        vec![("s".to_string(), "/work".to_string())]
    );
}

#[test]
fn workspace_assembles_services_and_surfaces_resolver_errors() {
    let fake = Arc::new(Fake {
        workspace_id: Some("w".into()),
        snapshots_required: true,
        snapshots: false,
        ..Fake::default()
    });
    let host = make_host(&fake);
    let workspace = host.workspace(&fake.input(&[])).unwrap();
    assert_eq!(workspace.id, Some("w".into()));
    assert!(workspace.snapshots_required);
    assert!(workspace.recorder.is_none());
    let fake = Arc::new(Fake {
        workspace_error: true,
        ..Fake::default()
    });
    assert_eq!(
        make_host(&fake)
            .workspace(&fake.input(&[]))
            .err()
            .unwrap()
            .code,
        "WORKSPACE"
    );
}

/// The native adapter over runtime/replacement.rs:301. The coordinator's
/// commit is best effort (backup-removal failure only warns), so the host sees
/// Ok(()) either way; claims are released without an owned pending entry.
#[tokio::test]
async fn native_replacement_commit_is_best_effort() {
    #[derive(Default)]
    struct Port {
        finalize_calls: AtomicUsize,
        finalize_failure: std::sync::atomic::AtomicBool,
        warns: AtomicUsize,
        session_closes: AtomicUsize,
    }
    impl ReplacementSession for Arc<Port> {
        fn active_turn_run_id(&self, _: &JsString) -> Option<JsString> {
            None
        }
        fn has_active_turn(&self, _: &JsString) -> bool {
            false
        }
        fn abort_expected_turn(
            &self,
            _: &JsString,
            _: &JsString,
        ) -> crate::runtime::refresh::BoxFuture<
            Result<(), crate::runtime::replacement::ReplacementError>,
        > {
            Box::pin(std::future::ready(Ok(())))
        }
        fn close_session(
            &self,
            _: &JsString,
        ) -> crate::runtime::refresh::BoxFuture<
            Result<(), crate::runtime::replacement::ReplacementError>,
        > {
            self.session_closes.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(Ok(())))
        }
    }
    let port = Arc::new(Port::default());
    let finalize_port = port.clone();
    let warn_port = port.clone();
    let storage = ReplacementStorage {
        fork_session: None,
        replace_last_turn: Some(Arc::new(move |_| {
            Box::pin(async move { Ok(object([("transactionId", "tx-1".into())])) })
        })),
        finalize_last_turn_replacement: Some(Arc::new(move |_| {
            let port = finalize_port.clone();
            Box::pin(async move {
                port.finalize_calls.fetch_add(1, Ordering::SeqCst);
                if port.finalize_failure.load(Ordering::SeqCst) {
                    Err(crate::runtime::replacement::ReplacementError::new(
                        "injected finalize failure",
                    ))
                } else {
                    Ok(JsValue::Null)
                }
            })
        })),
    };
    let coordinator = ReplacementCoordinator::new(
        storage,
        Arc::new(port.clone()),
        // Wide enough that the expiry timer never fires mid-test; claiming
        // disables it anyway.
        60_000,
        Some(Arc::new(move |_, _| {
            warn_port.warns.fetch_add(1, Ordering::SeqCst);
        })),
    );
    let host = NativeSubmitReplacement(coordinator.clone());
    // No pending transaction: commit and release are settled no-ops.
    host.commit_accepted_input(&"s".into(), &"r".into())
        .await
        .unwrap();
    host.release_claim(&"s".into(), &"r".into());
    assert_eq!(port.finalize_calls.load(Ordering::SeqCst), 0);
    // A claimed pending transaction commits through the finalize port; a
    // finalize failure is warned, not surfaced to the submission.
    coordinator
        .replace_last_turn(object([
            ("sessionKey", "s".into()),
            ("replacementTurnId", "r".into()),
        ]))
        .await
        .unwrap();
    assert_eq!(
        coordinator.claim_for_submit(&"s".into(), &"r".into()),
        crate::runtime::replacement::SubmitClaim::Claimed
    );
    host.commit_accepted_input(&"s".into(), &"r".into())
        .await
        .unwrap();
    assert_eq!(port.finalize_calls.load(Ordering::SeqCst), 1);
    assert_eq!(port.warns.load(Ordering::SeqCst), 0);
    port.finalize_failure.store(true, Ordering::SeqCst);
    coordinator
        .replace_last_turn(object([
            ("sessionKey", "s".into()),
            ("replacementTurnId", "r2".into()),
        ]))
        .await
        .unwrap();
    coordinator.claim_for_submit(&"s".into(), &"r2".into());
    host.commit_accepted_input(&"s".into(), &"r2".into())
        .await
        .unwrap();
    assert_eq!(port.finalize_calls.load(Ordering::SeqCst), 2);
    assert_eq!(port.warns.load(Ordering::SeqCst), 1);
}

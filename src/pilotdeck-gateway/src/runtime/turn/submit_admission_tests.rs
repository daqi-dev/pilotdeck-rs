//! Per-cell replay of the turn-submit-admission oracle against the production
//! [`GatewaySubmitAdmission`], plus full-turn integration for the scenarios
//! whose whole submit path is native in this change. The oracle's controlled
//! ports (trusted authorizer, compaction router, replacement storage) are
//! mirrored by the recording stubs below.
use super::super::coordinator::TurnEventCoordinator;
use super::super::projector::{GatewayAgentEventProjector, GatewayAgentEventProjectorOptions};
use super::super::publish::FileRegistry;
use super::super::submit::{SubmitSnapshots, TurnSubmitter, WorkspaceAdmissions};
use super::super::submit_host::{
    GatewaySubmitHost, NativeSubmitReplacement, SubmitAdmission, SubmitAttachmentLease,
    SubmitCwdService, SubmitHostServices,
};
use super::*;
use crate::runtime::project::{DisposeFuture, RuntimeError, RuntimeResult};
use crate::runtime::refresh::BoxFuture;
use crate::runtime::replacement::{ReplacementSession, ReplacementStorage, StorageOperation};
use crate::runtime::session::{
    LiveSession, LiveSessionHandle, LiveTurn, SessionContext, SessionFactory, SessionNext,
    SessionOwner, SessionOwnerOptions,
};
use crate::storage::FileRunRegistry;
use pilotdeck_protocol::{parse_json, stringify_json};
use serde_json::{Value, json};
use std::time::Duration;

const ORACLE: &str = include_str!("../../../tests/fixtures/turn-submit-admission/oracle.json");
fn js(value: &Value) -> JsValue {
    parse_json(&value.to_string()).unwrap()
}
fn json(value: &JsValue) -> Value {
    serde_json::from_str(&stringify_json(value).unwrap().unwrap()).unwrap()
}
fn wire(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap()
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

// ---- controlled ports, mirroring generate.mts ----

enum AuthorizerMode {
    Invalid,
    Throw,
    Authorized,
}
struct RecordingAuthorizer {
    mode: AuthorizerMode,
    calls: Mutex<Vec<Value>>,
}
impl SubmitTrustedAuthorizer for RecordingAuthorizer {
    fn authorize<'a>(
        &'a self,
        request: &'a JsValue,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(json(request));
            match self.mode {
                AuthorizerMode::Invalid => Ok(js(&json!({"source": "src"}))),
                AuthorizerMode::Throw => {
                    Err(SubmitError::new("TRUSTED_CONTEXT_UNAUTHORIZED", "no way"))
                }
                AuthorizerMode::Authorized => {
                    Ok(js(&json!({"principal": "p1", "source": "authorized"})))
                }
            }
        })
    }
}

/// The probe's controlled SDK session config port (generate.mts
/// `setSdkSessionConfig`): validation mirrors the TS gateway's own
/// `validateSdkSessionConfig` checks exercised by the oracle cases
/// (InProcessGateway.ts:3216-3218, 3243-3249); the native validator is 7b.
struct RecordingSdkConfig {
    calls: Mutex<Vec<Value>>,
}
impl SubmitSdkSessionConfig for RecordingSdkConfig {
    fn validate(&self, config: &JsValue) -> Result<(), SubmitError> {
        if !matches!(
            field(config, "systemPrompt"),
            JsValue::Undefined | JsValue::String(_)
        ) {
            return Err(SubmitError::new(
                "INVALID_SDK_SESSION_CONFIG",
                "systemPrompt must be a string.",
            ));
        }
        // InProcessGateway.ts:3243-3249: a deny entry must be a non-empty
        // string; the oracle's invalid case uses [""].
        if matches!(field(config, "managedPermissions"), JsValue::Object(_))
            && matches!(field(config, "managedPermissions").get("deny"), Some(JsValue::Array(deny))
                if deny.iter().any(|entry| matches!(entry, JsValue::String(text) if trim16(text.units()).is_empty())))
        {
            return Err(SubmitError::new(
                "UNSUPPORTED_SDK_MANAGED_SETTING",
                "managedPermissions may contain deny/ask entries, defaultMode=plan, and canPrompt=false only.",
            ));
        }
        Ok(())
    }
    fn set<'a>(
        &'a self,
        session: &'a JsString,
        config: &'a JsValue,
        project: Option<&'a JsString>,
        _: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<bool, SubmitError>> {
        let call = json!({
            "sessionKey": text(session),
            "config": json(config),
            "projectKey": project.map(text),
            "aborted": false,
        });
        self.calls.lock().unwrap().push(call);
        Box::pin(async { Ok(true) })
    }
}

struct ScriptedRouter {
    setup: Value,
    calls: Mutex<Vec<Value>>,
    aborts: Mutex<Vec<String>>,
}
impl SubmitCompactionRouter for ScriptedRouter {
    fn compact<'a>(
        &'a self,
        session: &'a JsString,
        turn: &'a JsString,
        abort: &'a CompactionAbort,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push(json!({"sessionKey": text(session), "turnId": text(turn)}));
            if let Some(result) = self.setup["compactResult"].as_object() {
                return Ok(js(&Value::Object(result.clone())));
            }
            if self.setup["compactHang"] == true {
                let reason = abort.aborted().await;
                self.aborts.lock().unwrap().push(text(&reason));
                return Ok(js(&json!({
                    "type": "aborted", "turnId": text(turn), "error": text(&reason), "usage": {}
                })));
            }
            if let Some(error) = self.setup["compactError"].as_str() {
                return Err(SubmitError::new("manual_compaction_failed", error));
            }
            Ok(js(&json!({
                "type": "skipped", "turnId": text(turn),
                "reason": "no_compactable_history", "usage": {}
            })))
        })
    }
}

/// The probe's controlled replacement storage: `replaceLastTurn` returns a
/// fixed transaction, `finalizeLastTurnReplacement` records its input.
#[derive(Clone, Default)]
struct RecordingStorage {
    calls: Arc<Mutex<Vec<Value>>>,
}
impl RecordingStorage {
    fn operation(self, name: &'static str) -> StorageOperation {
        let calls = self.calls.clone();
        let replace = name == "replace";
        Arc::new(move |input| {
            calls
                .lock()
                .unwrap()
                .push(json!({"op": name, "input": json(&input)}));
            Box::pin(async move {
                Ok(if replace {
                    js(&json!({"transactionId": "tx-1"}))
                } else {
                    js(&json!(null))
                })
            })
        })
    }
}
struct SeededSession;
impl ReplacementSession for SeededSession {
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
    ) -> BoxFuture<Result<(), crate::runtime::replacement::ReplacementError>> {
        Box::pin(async { Ok(()) })
    }
    fn close_session(
        &self,
        _: &JsString,
    ) -> BoxFuture<Result<(), crate::runtime::replacement::ReplacementError>> {
        Box::pin(async { Ok(()) })
    }
}
fn replacement_coordinator(storage: &RecordingStorage) -> Arc<ReplacementCoordinator> {
    Arc::new(ReplacementCoordinator::new(
        ReplacementStorage {
            fork_session: None,
            replace_last_turn: Some(storage.clone().operation("replace")),
            finalize_last_turn_replacement: Some(storage.clone().operation("finalize")),
        },
        Arc::new(SeededSession),
        60_000,
        None,
    ))
}

struct Driven {
    admission: Arc<GatewaySubmitAdmission>,
    router: Option<Arc<ScriptedRouter>>,
    authorizer: Option<Arc<RecordingAuthorizer>>,
}
/// The oracle drops setup functions during JSON serialization, so the
/// authorizer mode is keyed by the case name (generate.mts:272-274).
async fn build(name: &str, setup: &Value) -> Driven {
    let interactions = Arc::new(GatewayInteractionCoordinator::default());
    let replacement = replacement_coordinator(&RecordingStorage::default());
    if let Some(pending) = setup["pendingReplacement"].as_str() {
        replacement
            .replace_last_turn(js(&json!({
                "sessionKey": "s", "projectKey": "p",
                "expectedTurnId": "prev", "replacementTurnId": pending
            })))
            .await
            .unwrap();
    }
    if setup["writeReservation"] == true {
        replacement
            .reserve_transcript_write(&"s".into(), "session_update")
            .unwrap();
    }
    if let Some(seed) = setup["seed"].as_object() {
        interactions
            .reconnect_port()
            .reconnect(
                &"s".into(),
                InteractionBinding {
                    connection_id: seed["connectionId"].as_str().unwrap().into(),
                    generation: seed["generation"].as_u64().unwrap(),
                },
                None,
            )
            .unwrap();
    }
    let mut options = SubmitAdmissionOptions::new(replacement);
    options.interactions = interactions;
    let mut router = None;
    if setup["compactResult"].is_object()
        || setup["compactError"].is_string()
        || setup["compactHang"] == true
    {
        let port = Arc::new(ScriptedRouter {
            setup: setup.clone(),
            calls: Mutex::default(),
            aborts: Mutex::default(),
        });
        router = Some(port.clone());
        options.compaction = Some(port);
    }
    let mut authorizer = None;
    if let Some(mode) = match name {
        "trusted-invalid-decision" => Some(AuthorizerMode::Invalid),
        "trusted-throws" => Some(AuthorizerMode::Throw),
        "trusted-authorized" => Some(AuthorizerMode::Authorized),
        _ => None,
    } {
        let port = Arc::new(RecordingAuthorizer {
            mode,
            calls: Mutex::default(),
        });
        authorizer = Some(port.clone());
        options.trusted_authorizer = Some(port);
    }
    if setup["wireSdkConfig"] == true {
        options.sdk_config = Some(Arc::new(RecordingSdkConfig {
            calls: Mutex::default(),
        }));
    }
    Driven {
        admission: Arc::new(GatewaySubmitAdmission::new(options)),
        router,
        authorizer,
    }
}

/// Drives the admission windows in the submitter's order (submit.rs run():
/// normalize → validate → route_input → preflight → sdkSessionConfig
/// capability/validation); input validation itself is pinned by the
/// turn-submit fixture.
async fn drive(case: &Value) -> (Driven, Vec<JsValue>, Option<String>, JsValue) {
    let driven = build(case["name"].as_str().unwrap(), &case["setup"]).await;
    let mut input = {
        let mut value = json!({"sessionKey":"s","projectKey":"p","runId":"r","message":"hello","channelKey":"test"});
        if let Some(fields) = case["setup"]["input"].as_object() {
            for (key, item) in fields {
                value[key.as_str()] = item.clone();
            }
        }
        js(&value)
    };
    let run = RunRef {
        session_key: "s".into(),
        project_key: Some("p".into()),
        run_id: "r".into(),
    };
    let outcome = async {
        let early = driven.admission.normalize(&mut input).await?;
        if !early.is_empty() {
            return Ok(early);
        }
        let routed = driven.admission.route_input(&mut input).await?;
        if !routed.is_empty() {
            return Ok(routed);
        }
        let early = driven.admission.preflight(&input, &run).await?;
        if !early.is_empty() {
            return Ok(early);
        }
        // submit.rs:370-392: the sdkSessionConfig window sits between the
        // replacement preflight and begin_turn — capability first, then the
        // port's validation (InProcessGateway.ts:1014-1034).
        if truthy(field(&input, "sdkSessionConfig")) {
            let error = if !driven.admission.has_sdk_config() {
                Some(SubmitError::new(
                    "CAPABILITY_UNAVAILABLE",
                    "sdk_session_config is unavailable.",
                ))
            } else {
                driven
                    .admission
                    .validate_sdk_config(field(&input, "sdkSessionConfig"))
                    .err()
            };
            if let Some(error) = error {
                return Ok(vec![error_event(&run, &error.code, &error.message, true)]);
            }
        }
        Ok::<_, SubmitError>(Vec::new())
    }
    .await;
    let (events, thrown) = match outcome {
        Ok(events) => (events, None),
        Err(error) => (Vec::new(), Some(error.message)),
    };
    (driven, events, thrown, input)
}

#[tokio::test]
async fn real_ts_admission_scenarios() {
    let oracle = json(&crate::test_baseline::oracle(ORACLE));
    let mut selected = 0;
    for case in oracle["cases"].as_array().unwrap() {
        if std::env::var("PDRS_SUBMIT_ADMISSION_CASE")
            .ok()
            .is_none_or(|name| name == case["name"].as_str().unwrap())
        {
            selected += 1;
            tokio::time::timeout(Duration::from_secs(20), replay_admission(case))
                .await
                .unwrap_or_else(|_| panic!("{} deadlocked", case["name"]));
        }
    }
    assert!(
        selected > 0,
        "PDRS_SUBMIT_ADMISSION_CASE matched no scenario"
    );
}

async fn replay_admission(case: &Value) {
    let name = case["name"].as_str().unwrap();
    let result = case["results"].as_array().unwrap().last().unwrap();
    let (driven, events, thrown, input) = drive(case).await;
    let full_turn = result["hostCalls"]
        .as_array()
        .unwrap()
        .iter()
        .any(|call| call == "submit");
    let wires: Vec<String> = events.iter().map(wire).collect();
    if full_turn {
        // The admission windows stayed open all the way to the pump.
        assert_eq!(wires, Vec::<String>::new(), "{name}: admission stayed open");
        assert_eq!(thrown, None, "{name}");
        match name {
            // InProcessGateway.ts:3567-3583 rewrite, measured through the
            // probe's session.submit.
            "plan-rewritten" => {
                let submit = &result["submits"][0];
                assert_eq!(
                    json(field(&input, "message")),
                    submit["agentInput"]["text"],
                    "{name}: rewritten message"
                );
                assert_eq!(json(field(&input, "runMode")), submit["options"]["runMode"]);
                assert_eq!(
                    json(field(&input, "mode")),
                    submit["options"]["permissionMode"]
                );
                assert_eq!(
                    json(field(&input, "basePermissionMode")),
                    submit["options"]["basePermissionMode"]
                );
                assert_eq!(
                    json(field(&input, "allowPlanModeTools")),
                    submit["options"]["allowPlanModeTools"]
                );
            }
            // modelCatalog.ts:92-100 folding, measured through session.submit.
            "model-selection-normalized" => {
                assert_eq!(
                    json(field(&input, "modelSelection")),
                    result["submits"][0]["options"]["modelSelection"],
                    "{name}: folded modelSelection"
                );
                // The override folds through the same helper
                // (InProcessGateway.ts:770-772); the submit-level golden is
                // pump-derived, so the folded input is pinned here.
                assert_eq!(
                    json(field(&input, "modelOverride")),
                    json!({"mode": "model", "provider": "ov", "model": "om"}),
                    "{name}: folded modelOverride"
                );
            }
            "trusted-authorized" => {
                let authorizer = driven.authorizer.as_ref().unwrap();
                assert_eq!(
                    *authorizer.calls.lock().unwrap(),
                    result["authorizerCalls"].as_array().unwrap().clone(),
                    "{name}: authorizer requests"
                );
            }
            _ => {}
        }
        return;
    }
    if let Some(thrown) = thrown {
        // The generator throws for a non-validatable binding (N3).
        assert_eq!(result["thrown"].as_str(), Some(thrown.as_str()), "{name}");
        assert!(events.is_empty(), "{name}");
        return;
    }
    assert_eq!(
        wires,
        result["wire"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        "{name}: admission events"
    );
    if let Some(router) = &driven.router {
        assert_eq!(
            *router.calls.lock().unwrap(),
            result["compactCalls"].as_array().unwrap().clone(),
            "{name}: router calls"
        );
        assert_eq!(
            *router.aborts.lock().unwrap(),
            result["compactAborts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect::<Vec<_>>(),
            "{name}: abort reasons"
        );
    }
}

// ---- full-turn integration through the production host and admission ----

struct ScriptedSession {
    native: Vec<JsValue>,
    coordinator: Arc<TurnEventCoordinator>,
    submits: Mutex<Vec<Value>>,
}
impl LiveSession for ScriptedSession {
    fn submit(&self, input: JsValue, options: JsValue) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        self.submits
            .lock()
            .unwrap()
            .push(json!({"agentInput": json(&input), "options": json(&options)}));
        Ok(Box::new(ScriptedTurn {
            native: self.native.clone(),
            coordinator: self.coordinator.clone(),
            index: 0,
        }))
    }
    fn snapshot(&self) -> JsValue {
        object([
            ("messages", JsValue::Array(vec![])),
            ("cwd", "/unused".into()),
        ])
    }
    fn abort(&self, _: &JsString) -> RuntimeResult {
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}
struct ScriptedTurn {
    native: Vec<JsValue>,
    coordinator: Arc<TurnEventCoordinator>,
    index: usize,
}
impl LiveTurn for ScriptedTurn {
    fn next(&mut self) -> SessionNext<'_> {
        Box::pin(async move {
            if let Some(event) = self.native.get(self.index) {
                self.index += 1;
                return Ok(Some(event.clone()));
            }
            self.coordinator.emit(
                &"s".into(),
                &object([
                    ("type", "agent_status".into()),
                    ("event", "SessionEnd".into()),
                ]),
            );
            Ok(None)
        })
    }
}
struct ScriptedFactory {
    session: Arc<ScriptedSession>,
}
impl SessionFactory for ScriptedFactory {
    fn create(
        &self,
        _: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        let session = self.session.clone();
        Box::pin(async move { Ok(LiveSessionHandle::new(session, None)) })
    }
}
struct Services;
impl SubmitHostServices for Services {
    fn workspace_id(&self, _: &JsValue) -> Result<Option<JsString>, SubmitError> {
        Ok(None)
    }
    fn snapshots(&self) -> Option<Arc<dyn SubmitSnapshots>> {
        None
    }
    fn snapshots_required(&self) -> bool {
        false
    }
    fn cwd(&self) -> Option<Arc<dyn SubmitCwdService>> {
        None
    }
    fn refresh(&self) -> SubmitFuture<'_, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
    }
    fn run_policy(&self) -> Option<JsValue> {
        None
    }
    fn resolve_attachments<'a>(
        &'a self,
        _: &'a JsValue,
    ) -> SubmitFuture<'a, Result<SubmitAttachmentLease, SubmitError>> {
        Box::pin(async { Err(SubmitError::new("gateway_submit_failed", "unreachable")) })
    }
    fn prepare_attachment_turn<'a>(
        &'a self,
        message: &'a JsValue,
        _: Vec<JsValue>,
        _: Option<&JsString>,
        _: Option<&JsString>,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        let message = message.clone();
        Box::pin(async move { Ok(object([("type", "text".into()), ("text", message)])) })
    }
    fn resolve_model_selection<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        // createLocalGateway wires the real sessionModels resolver
        // (GatewaySessionModelBundle.ts:92-100): a submitted model-mode
        // selection resolves as {selection, source: "turn"} after normalize's
        // folding; anything else keeps the default source.
        let selection = field(input, "modelSelection");
        let resolved = if is(selection, "mode", "model") {
            object([("source", "turn".into()), ("selection", selection.clone())])
        } else {
            object([("source", "default".into())])
        };
        Box::pin(async move { Ok(resolved) })
    }
    fn record_failure<'a>(
        &'a self,
        _: &'a RunRef,
        _: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
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

#[tokio::test]
async fn real_ts_admission_full_turn_scenarios() {
    let oracle = json(&crate::test_baseline::oracle(ORACLE));
    // fix-r1: the whole-submission scenarios on the real submitter/host pair,
    // covering every admission window that a native component observes: the
    // replacement claim outcomes, the compaction command (including the
    // timeout abort), trusted-context authorization, the plan rewrite, the
    // model-selection fold and the sdkSessionConfig validation window.
    for name in [
        "reconnect-fresh-turn",
        "replacement-claim-committed",
        "replacement-claim-conflict",
        "replacement-write-reservation",
        "compact-busy",
        "compact-timeout-abort",
        "trusted-authorized",
        "plan-rewritten",
        "model-selection-normalized",
        "sdk-config-valid-wired",
        "sdk-config-invalid-wired",
    ] {
        let case = oracle["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == name)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(20), replay_full_turn(case))
            .await
            .unwrap_or_else(|_| panic!("{name} deadlocked"));
    }
}

/// The full native path: TurnSubmitter + GatewaySubmitHost +
/// GatewaySubmitAdmission + NativeSubmitReplacement, with the probe's
/// scripted session events and recording replacement storage.
async fn replay_full_turn(case: &Value) {
    let name = case["name"].as_str().unwrap();
    let setup = &case["setup"];
    let storage = RecordingStorage::default();
    let replacement = replacement_coordinator(&storage);
    let interactions = Arc::new(GatewayInteractionCoordinator::default());
    if let Some(pending) = setup["pendingReplacement"].as_str() {
        replacement
            .replace_last_turn(js(&json!({
                "sessionKey": "s", "projectKey": "p",
                "expectedTurnId": "prev", "replacementTurnId": pending
            })))
            .await
            .unwrap();
    }
    if setup["writeReservation"] == true {
        replacement
            .reserve_transcript_write(&"s".into(), "session_update")
            .unwrap();
    }
    if let Some(seed) = setup["seed"].as_object() {
        interactions
            .reconnect_port()
            .reconnect(
                &"s".into(),
                InteractionBinding {
                    connection_id: seed["connectionId"].as_str().unwrap().into(),
                    generation: seed["generation"].as_u64().unwrap(),
                },
                None,
            )
            .unwrap();
    }
    let mut options = SubmitAdmissionOptions::new(replacement.clone());
    options.interactions = interactions;
    // The probe's controlled ports, wired exactly as the oracle's setup
    // describes (compaction router, trusted authorizer, SDK session config).
    let mut router = None;
    if setup["compactResult"].is_object()
        || setup["compactError"].is_string()
        || setup["compactHang"] == true
    {
        let port = Arc::new(ScriptedRouter {
            setup: setup.clone(),
            calls: Mutex::default(),
            aborts: Mutex::default(),
        });
        router = Some(port.clone());
        options.compaction = Some(port);
    }
    let mut authorizer = None;
    if name == "trusted-authorized" {
        let port = Arc::new(RecordingAuthorizer {
            mode: AuthorizerMode::Authorized,
            calls: Mutex::default(),
        });
        authorizer = Some(port.clone());
        options.trusted_authorizer = Some(port);
    }
    let mut sdk = None;
    if setup["wireSdkConfig"] == true {
        let port = Arc::new(RecordingSdkConfig {
            calls: Mutex::default(),
        });
        sdk = Some(port.clone());
        options.sdk_config = Some(port);
    }
    let admission = Arc::new(GatewaySubmitAdmission::new(options));
    let coordinator = Arc::new(TurnEventCoordinator::new());
    let session = Arc::new(ScriptedSession {
        native: case["native"].as_array().unwrap().iter().map(js).collect(),
        coordinator: coordinator.clone(),
        submits: Mutex::default(),
    });
    let mut owner_options = SessionOwnerOptions::new(Arc::new(ScriptedFactory {
        session: session.clone(),
    }));
    owner_options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(owner_options);
    let root = std::env::temp_dir().join(format!("pdrs-admission-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let registry = Arc::new(
        FileRegistry::new(FileRunRegistry::open(root.join("registry.json")).unwrap()).unwrap(),
    );
    let host = Arc::new(GatewaySubmitHost::new(
        Arc::new(Services),
        Arc::new(NativeSubmitReplacement((*replacement).clone())),
        admission,
    ));
    let submitter = Arc::new(TurnSubmitter {
        owner: owner.clone(),
        coordinator,
        registry: Some(registry.clone()),
        host: host.clone(),
        workspace_admissions: WorkspaceAdmissions::default(),
        projector: Arc::new(GatewayAgentEventProjector::new(
            GatewayAgentEventProjectorOptions::default(),
        )),
    });
    let mut input = json!({"sessionKey":"s","projectKey":"p","runId":"r","message":"hello","channelKey":"test"});
    if let Some(fields) = setup["input"].as_object() {
        for (key, value) in fields {
            input[key.as_str()] = value.clone();
        }
    }
    let mut stream = submitter.submit(js(&input));
    let mut wires = Vec::new();
    while let Some(event) = stream.next().await.unwrap() {
        wires.push(wire(&event));
    }
    let result = case["results"].as_array().unwrap().last().unwrap();
    assert_eq!(
        wires,
        result["wire"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        "{name}: visible wire"
    );
    assert_eq!(
        *storage.calls.lock().unwrap(),
        result["storageCalls"].as_array().unwrap().clone(),
        "{name}: storage calls"
    );
    if let Some(router) = &router {
        assert_eq!(
            *router.calls.lock().unwrap(),
            result["compactCalls"].as_array().unwrap().clone(),
            "{name}: router calls"
        );
        assert_eq!(
            *router.aborts.lock().unwrap(),
            result["compactAborts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect::<Vec<_>>(),
            "{name}: abort reasons"
        );
    }
    if let Some(authorizer) = &authorizer {
        assert_eq!(
            *authorizer.calls.lock().unwrap(),
            result["authorizerCalls"].as_array().unwrap().clone(),
            "{name}: authorizer requests"
        );
    }
    if let Some(sdk) = &sdk {
        assert_eq!(
            *sdk.calls.lock().unwrap(),
            result["sdkSetCalls"].as_array().unwrap().clone(),
            "{name}: sdk config set calls"
        );
    }
    assert!(
        !owner.has_active_turn(&"s".into()),
        "{name}: admission released"
    );
    // A refused submit never claims: its own claim stays settled, while a
    // conflicting pending replacement (a different runId, oracle setup
    // 'rx') keeps gating the session exactly as the TS releasePreflight
    // no-op leaves it.
    let expected_claim = match setup["pendingReplacement"].as_str() {
        Some(pending) if pending != "r" => SubmitClaim::Conflict,
        _ => SubmitClaim::None,
    };
    assert_eq!(
        replacement.claim_for_submit(&"s".into(), &"r".into()),
        expected_claim,
        "{name}: claim settled"
    );
    drop(submitter);
    drop(host);
    drop(registry);
    // Reopen after all FIFO appends have drained, to compare persisted entries
    // rather than a second in-memory FileRunRegistry cache.
    let reopened = FileRunRegistry::open(root.join("registry.json")).unwrap();
    let run = RunRef {
        session_key: "s".into(),
        project_key: Some("p".into()),
        run_id: "r".into(),
    };
    // Command turns (compact/plan usage) never reach run acceptance, so the
    // oracle carries no durable expectations for them: the run stays absent.
    let expected_events = case["registryEvents"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        reopened
            .events(&run, None, None)
            .unwrap()
            .iter()
            .map(json)
            .collect::<Vec<_>>(),
        expected_events,
        "{name}: durable events"
    );
    let record = reopened.get(&run).unwrap();
    let record = record
        .map(|record| {
            let record = json(record.as_json());
            json!({"state": record["state"], "lastSeq": record["lastSeq"], "revision": record["revision"]})
        })
        .unwrap_or(Value::Null);
    assert_eq!(record, case["record"], "{name}: persisted run state");
    owner.shutdown().await.unwrap();
    drop(reopened);
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn absent_ports_reproduce_unwired_option_behavior() {
    // S1 (InProcessGateway.ts:1014-1025) and the taskBudget refusal
    // (:1171-1181): a configured capability without its port is refused.
    let admission = Arc::new(GatewaySubmitAdmission::new(SubmitAdmissionOptions::new(
        replacement_coordinator(&RecordingStorage::default()),
    )));
    assert!(!admission.has_sdk_config());
    assert!(admission.validate_sdk_config(&js(&json!({}))).is_ok());
    let input = js(&json!({
        "sessionKey": "s", "projectKey": "p", "runId": "r", "message": "hello", "channelKey": "test",
        "sdkSessionConfig": {"taskBudget": {"total": 1}}
    }));
    let run = RunRef {
        session_key: "s".into(),
        project_key: Some("p".into()),
        run_id: "r".into(),
    };
    let completion = stub_completion();
    let events = admission
        .admitted_preflight(&input, &run, &completion)
        .await
        .unwrap();
    assert_eq!(
        json(&JsValue::Array(events)),
        json!([
            {"type": "error", "runId": "r", "code": "CAPABILITY_UNAVAILABLE",
             "message": "Gateway task-budget accounting is unavailable.",
             "recoverable": true}
        ]),
        "taskBudget without accounting"
    );
    let error = admission
        .set_sdk_config(&input, &completion)
        .await
        .unwrap_err();
    assert_eq!(error.code, "CAPABILITY_UNAVAILABLE");
    assert_eq!(error.message, "sdk_session_config is unavailable.");
}
/// fix-r1 P1: a wired port owns the sdkSessionConfig validation in the
/// preflight window (probe sdk-config-invalid-wired: the TS gateway rejects
/// INVALID_SDK_SESSION_CONFIG before beginTurn and before the set port).
/// The admission surface must surface the port's refusal, not defer it to
/// set_sdk_config.
#[test]
fn wired_sdk_config_rejects_invalid_config_at_validation() {
    let mut options =
        SubmitAdmissionOptions::new(replacement_coordinator(&RecordingStorage::default()));
    options.sdk_config = Some(Arc::new(RecordingSdkConfig {
        calls: Mutex::default(),
    }));
    let admission = Arc::new(GatewaySubmitAdmission::new(options));
    assert!(admission.has_sdk_config());
    let error = admission
        .validate_sdk_config(&js(&json!({"systemPrompt": 42})))
        .unwrap_err();
    assert_eq!(error.code, "INVALID_SDK_SESSION_CONFIG");
    assert_eq!(error.message, "systemPrompt must be a string.");
    assert!(
        admission
            .validate_sdk_config(&js(&json!({"systemPrompt": "ok"})))
            .is_ok()
    );
    let managed = admission
        .validate_sdk_config(&js(
            &json!({"managedPermissions": {"deny": [""], "ask": []}}),
        ))
        .unwrap_err();
    assert_eq!(managed.code, "UNSUPPORTED_SDK_MANAGED_SETTING");
    assert_eq!(
        managed.message,
        "managedPermissions may contain deny/ask entries, defaultMode=plan, and canPrompt=false only."
    );
}

/// fix-r1 P2: a turn dropped while /compact still awaits the router must
/// cancel the timeout timer with it (GatewayManualCompactionCoordinator's
/// clearTimeout finally extended to the dropped-future path): no orphaned
/// task survives to fire the abort after the turn is gone.
#[tokio::test]
async fn compact_timer_dies_with_dropped_turn() {
    struct HangingRouter {
        aborts: Mutex<Vec<CompactionAbort>>,
    }
    impl SubmitCompactionRouter for HangingRouter {
        fn compact<'a>(
            &'a self,
            _: &'a JsString,
            _: &'a JsString,
            abort: &'a CompactionAbort,
        ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
            let abort = abort.clone();
            self.aborts.lock().unwrap().push(abort.clone());
            Box::pin(async move {
                let _ = abort.aborted().await;
                Ok(js(&json!({"type": "aborted", "usage": {}})))
            })
        }
    }
    let router = Arc::new(HangingRouter {
        aborts: Mutex::default(),
    });
    let mut options =
        SubmitAdmissionOptions::new(replacement_coordinator(&RecordingStorage::default()));
    options.compaction = Some(router.clone());
    let admission = GatewaySubmitAdmission::new(options);
    let mut input = js(&json!({
        "sessionKey": "s", "projectKey": "p", "runId": "r", "channelKey": "test",
        "message": "/compact", "timeoutMs": 150
    }));
    // The turn is cancelled while the router still hangs.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), admission.route_input(&mut input))
            .await
            .is_err(),
        "the router must still be hanging"
    );
    // Well past the original timeoutMs: an orphaned timer would have fired.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        router.aborts.lock().unwrap()[0].reason().is_none(),
        "the dropped turn's timeout timer must not outlive it"
    );
}

fn stub_completion() -> TurnCompletionHandle {
    struct NoopFactory;
    impl SessionFactory for NoopFactory {
        fn create(
            &self,
            _: SessionContext,
            _: Option<LiveSessionHandle>,
        ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
            Box::pin(async { Err(RuntimeError::new("completion stub never creates sessions")) })
        }
    }
    let mut options = SessionOwnerOptions::new(Arc::new(NoopFactory));
    options.idle_sweep_interval_ms = 0;
    SessionOwner::new(options).begin_completion(&"s".into(), &"r".into())
}

/// Lone surrogates cannot cross a JSON golden (serde_json rejects them), so
/// the UTF-16 command paths are pinned directly: TS `String.prototype.trim`
/// and `/^\/plan(?:\s+([\s\S]*))?$/u` (InProcessGateway.ts:3585-3601).
#[tokio::test]
async fn utf16_command_parsing() {
    let admission = GatewaySubmitAdmission::new(SubmitAdmissionOptions::new(
        replacement_coordinator(&RecordingStorage::default()),
    ));
    let mut message = "/plan".encode_utf16().collect::<Vec<u16>>();
    message.push(0x3000); // ideographic space is \s, so the command parses
    message.push(0xd800); // lone surrogate
    message.push(b'x' as u16);
    message.push(0x2028); // LINE SEPARATOR trims away
    let mut input =
        js(&json!({"sessionKey": "s", "projectKey": "p", "runId": "r", "channelKey": "test"}));
    set(
        &mut input,
        "message",
        JsValue::String(JsString::from_utf16(message)),
    );
    admission.route_input(&mut input).await.unwrap();
    assert_eq!(
        field(&input, "message").as_string().unwrap().units(),
        &[0xd800, b'x' as u16][..],
        "the lone surrogate survives the plan rewrite"
    );
    // "/planx" is not a plan command; the message passes through untouched.
    let mut plain = js(&json!({
        "sessionKey": "s", "projectKey": "p", "runId": "r",
        "message": "/planx", "channelKey": "test"
    }));
    admission.route_input(&mut plain).await.unwrap();
    assert_eq!(json(field(&plain, "message")), json!("/planx"));
}

#[test]
#[ignore = "requires real fixed TS baseline and Node 22"]
fn real_ts_submit_admission_regenerates_identically() {
    crate::test_baseline::regenerates_identically("turn-submit-admission");
}

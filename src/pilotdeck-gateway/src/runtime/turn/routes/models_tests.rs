use super::*;
use crate::{
    config::Env,
    runtime::{
        plugins::{PluginRuntime, PluginRuntimeOptions},
        project::{DisposeFuture, RuntimeError, RuntimeResult},
        refresh::{BoxFuture, RefreshRouter},
        replacement::ReplacementStorage,
        sdk_session::{SdkRuntimeResolver, SdkSessionOptions},
        session::{
            LiveSession, LiveSessionHandle, SessionContext, SessionFactory, SessionOwnerOptions,
        },
    },
    storage::{JsonlTranscriptStore, read_transcript},
    test_scratch::Scratch,
};
use pilotdeck_protocol::{parse_json, stringify_json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Resolver {
    root: PathBuf,
    home: PathBuf,
    env: Env,
    plugins: Arc<PluginRuntime>,
}
impl SdkRuntimeResolver for Resolver {
    fn resolve(&self, _project: Option<&JsString>) -> Result<ResolvedSdkRuntime, SdkSessionError> {
        let snapshot = crate::config::load_snapshot(
            &self.home,
            &self.root,
            &self.env,
            Some(&self.root.to_string_lossy()),
            1.0,
            "",
        )
        .map_err(|failure| SdkSessionError::new(failure.code, "Fixture config load failed."))?;
        Ok(ResolvedSdkRuntime {
            project_root: self.root.clone(),
            snapshot,
            plugin_runtime: self.plugins.clone(),
        })
    }
}

struct FakeSession(Arc<AtomicUsize>);
impl LiveSession for FakeSession {
    fn snapshot(&self) -> JsValue {
        object([("messages", JsValue::Array(vec![]))])
    }
    fn abort(&self, _: &JsString) -> RuntimeResult {
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
struct Factory(Arc<AtomicUsize>);
impl SessionFactory for Factory {
    fn create(
        &self,
        _: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        let count = self.0.clone();
        Box::pin(async move { Ok(LiveSessionHandle::new(Arc::new(FakeSession(count)), None)) })
    }
}

struct Fixture {
    home: PathBuf,
    root: PathBuf,
    resolver: Arc<Resolver>,
    sdk: Mutex<SdkSessionRegistry>,
    owner: SessionOwner,
    replacement: ReplacementCoordinator,
    transcripts: AsyncTranscriptAdapter,
    disposed: Arc<AtomicUsize>,
    _scratch: Scratch,
}
impl Fixture {
    fn new() -> Self {
        let scratch = Scratch::new("turn-models");
        let home = scratch.path().join("home");
        let root = scratch.path().join("project");
        fs::create_dir_all(home.join("projects/test")).unwrap();
        fs::create_dir_all(&root).unwrap();
        fs::write(
            home.join("projects/test/.cwd"),
            root.to_string_lossy().as_bytes(),
        )
        .unwrap();
        // Public fixture credential only: no process environment or real secret is read.
        fs::write(
            home.join("pilotdeck.yaml"),
            concat!(
                "schemaVersion: 1\nagent: {model: parity/base}\nmodel:\n  providers:\n",
                "    parity:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n",
                "      apiKey: mock-only\n      models:\n",
                "        base: {}\n        sdk: {}\n        session: {}\n        turn: {}\n",
            ),
        )
        .unwrap();
        let env = Env::from([
            ("HOME".into(), scratch.path().to_string_lossy().into_owned()),
            ("PILOT_HOME".into(), home.to_string_lossy().into_owned()),
        ]);
        let resolver = Arc::new(Resolver {
            root: root.clone(),
            home: home.clone(),
            env,
            plugins: Arc::new(PluginRuntime::new(PluginRuntimeOptions {
                project_root: root.clone(),
                pilot_home: home.clone(),
                builtin_skills_root: None,
                builtin_plugins: vec![],
                builtin_plugins_enabled: Default::default(),
            })),
        });
        let sdk = Mutex::new(registry(&resolver));
        let disposed = Arc::new(AtomicUsize::new(0));
        let owner = SessionOwner::new(SessionOwnerOptions::new(Arc::new(Factory(
            disposed.clone(),
        ))));
        let replacement = ReplacementCoordinator::new(
            ReplacementStorage::default(),
            Arc::new(owner.clone()),
            1000,
            None,
        );
        let cwd = root.clone();
        let transcripts = AsyncTranscriptAdapter::new(
            Arc::new(JsonlTranscriptStore::new()),
            Arc::new(move |key| {
                crate::session_chat_dir(&key.pilot_home, &cwd, &key.project_root.to_string_lossy())
            }),
        );
        Self {
            home,
            root,
            resolver,
            sdk,
            owner,
            replacement,
            transcripts,
            disposed,
            _scratch: scratch,
        }
    }
    fn models(&self) -> Models<'_> {
        Models {
            sdk: &self.sdk,
            owner: &self.owner,
            replacement: &self.replacement,
            transcripts: &self.transcripts,
            pilot_home: &self.home,
        }
    }
    fn project(&self) -> JsString {
        self.root.to_string_lossy().into_owned().into()
    }
    fn params(&self, selection: Option<JsValue>) -> JsValue {
        let mut params = object([
            ("projectKey", JsValue::String(self.project())),
            ("sessionKey", "web:test".into()),
        ]);
        if let (Some(selection), JsValue::Object(params)) = (selection, &mut params) {
            params.insert("selection".into(), selection);
        }
        params
    }
    fn transcript_path(&self) -> PathBuf {
        crate::session_chat_dir(&self.home, &self.root, &self.root.to_string_lossy())
            .join("web:test.jsonl")
    }
    async fn prepare(&self, input: &JsValue) -> JsValue {
        resolve_for_turn(&self.models(), &"web:test".into(), &self.project(), input)
            .await
            .unwrap()
    }
}
fn registry(resolver: &Arc<Resolver>) -> SdkSessionRegistry {
    SdkSessionRegistry::new(
        SdkSessionOptions::new(
            &resolver.root,
            &resolver.root,
            resolver.env.clone(),
            resolver.clone(),
        )
        .with_homedir(resolver.home.parent().unwrap())
        .with_clock(Arc::new(|| 1_700_000_000_000.0)),
    )
}
fn selection(model: &str) -> JsValue {
    object([
        ("mode", "model".into()),
        ("provider", "parity".into()),
        ("model", model.into()),
    ])
}
fn assert_model(prepared: &JsValue, model: &str, source: &str) {
    assert!(is(
        field(field(prepared, "selection"), "provider"),
        "parity"
    ));
    assert!(is(field(field(prepared, "selection"), "model"), model));
    assert!(is(field(prepared, "source"), source));
}

#[tokio::test]
async fn set_changes_next_prepare_and_clear_restores_real_config_default() {
    let fixture = Fixture::new();
    assert_model(&fixture.prepare(&object([])).await, "base", "default");
    fixture
        .owner
        .get_or_create(SessionContext {
            session_key: "web:test".into(),
            project_key: Some(fixture.project()),
            channel_key: "web".into(),
            allowed_tools: None,
            disallowed_tools: None,
        })
        .await
        .unwrap();
    let mut chosen = selection("session");
    if let JsValue::Object(value) = &mut chosen {
        value.insert("temperature".into(), JsValue::Number(0.7));
    }
    let result = handle(
        &fixture.models(),
        "session_model_set",
        &fixture.params(Some(chosen)),
    )
    .await
    .unwrap();
    assert!(is(field(field(&result, "effective"), "model"), "session"));
    assert!(field(field(&result, "saved"), "temperature").is_undefined());
    assert_eq!(fixture.disposed.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.owner.cached_session_count(), 0);
    assert_model(&fixture.prepare(&object([])).await, "session", "session");
    // A fresh SDK registry has no preference cache: the next prepare replays disk.
    *fixture.sdk.lock().await = registry(&fixture.resolver);
    assert_model(&fixture.prepare(&object([])).await, "session", "session");
    let cleared = handle(
        &fixture.models(),
        "session_model_clear",
        &fixture.params(None),
    )
    .await
    .unwrap();
    assert!(cleared.is_undefined());
    assert_model(&fixture.prepare(&object([])).await, "base", "default");
    let entries = read_transcript(fixture.transcript_path()).unwrap().entries;
    assert_eq!(entries.len(), 2);
    assert_eq!(field(&entries[0], "sequence").as_number(), Some(1.0));
    assert_eq!(field(&entries[1], "sequence").as_number(), Some(2.0));
    assert_eq!(
        field(&entries[1], "parentEntryId"),
        field(&entries[0], "entryId")
    );
    assert!(is(
        field(field(&entries[0], "metadata"), "updatedAt"),
        "2023-11-14T22:13:20.000Z"
    ));
    assert!(matches!(
        field(field(&entries[1], "metadata"), "modelSelection"),
        JsValue::Null
    ));
}

#[tokio::test]
async fn sdk_then_session_then_turn_precedence_and_clear_restores_sdk_default() {
    let fixture = Fixture::new();
    fixture
        .sdk
        .lock()
        .await
        .set_session_config(
            &"web:test".into(),
            &parse_json(r#"{"settings":{"agent":{"model":"parity/sdk"}}}"#).unwrap(),
            Some(&fixture.project()),
            None,
        )
        .await
        .unwrap();
    assert_model(&fixture.prepare(&object([])).await, "sdk", "default");
    handle(
        &fixture.models(),
        "session_model_set",
        &fixture.params(Some(selection("session"))),
    )
    .await
    .unwrap();
    assert_model(&fixture.prepare(&object([])).await, "session", "session");
    assert_model(
        &fixture
            .prepare(&object([("modelSelection", selection("turn"))]))
            .await,
        "turn",
        "turn",
    );
    assert_model(
        &fixture
            .prepare(&object([("modelOverride", selection("turn"))]))
            .await,
        "turn",
        "turn",
    );
    handle(
        &fixture.models(),
        "session_model_clear",
        &fixture.params(None),
    )
    .await
    .unwrap();
    assert_model(&fixture.prepare(&object([])).await, "sdk", "default");
}

#[tokio::test]
async fn invalid_selection_or_active_turn_leaves_preferences_and_admission_unchanged() {
    let fixture = Fixture::new();
    let session = "web:test".into();
    let run = "r1".into();
    let turn = fixture.owner.begin_turn(&session, &run).unwrap();
    let failure = handle(
        &fixture.models(),
        "session_model_set",
        &fixture.params(Some(selection("session"))),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, "SESSION_BUSY");
    assert!(fixture.owner.has_active_turn(&session));
    assert!(
        !fixture
            .replacement
            .has_transcript_write_reservation(&session)
    );
    fixture.owner.end_turn(&session, Some(&run), Some(&turn));
    let failure = handle(
        &fixture.models(),
        "session_model_set",
        &fixture.params(Some(selection("missing"))),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, "INVALID_MODEL_OVERRIDE");
    assert!(!fixture.transcript_path().exists());
    assert!(
        !fixture
            .replacement
            .has_transcript_write_reservation(&session)
    );
    assert!(fixture.owner.begin_turn(&session, &run).is_some());
}

#[tokio::test]
async fn concurrent_transcript_update_and_invalid_project_are_rejected_without_writing() {
    let fixture = Fixture::new();
    let session = "web:test".into();
    fixture
        .replacement
        .reserve_transcript_write(&session, "replace")
        .unwrap();
    let failure = handle(
        &fixture.models(),
        "session_model_clear",
        &fixture.params(None),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, "SESSION_BUSY");
    fixture.replacement.release_transcript_write(&session);
    let mut params = fixture.params(Some(selection("session")));
    if let JsValue::Object(value) = &mut params {
        value.insert("projectKey".into(), "/unregistered-project".into());
    }
    let failure = handle(&fixture.models(), "session_model_set", &params)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "PROJECT_NOT_FOUND");
    assert!(!fixture.transcript_path().exists());
    assert!(
        !fixture
            .replacement
            .has_transcript_write_reservation(&session)
    );
}

#[tokio::test]
async fn failed_append_does_not_report_success_or_close_the_live_session() {
    let fixture = Fixture::new();
    fixture
        .owner
        .get_or_create(SessionContext {
            session_key: "web:test".into(),
            project_key: Some(fixture.project()),
            channel_key: "web".into(),
            allowed_tools: None,
            disallowed_tools: None,
        })
        .await
        .unwrap();
    fs::create_dir_all(fixture.transcript_path().parent().unwrap()).unwrap();
    fs::create_dir(fixture.transcript_path()).unwrap();
    let failure = handle(
        &fixture.models(),
        "session_model_set",
        &fixture.params(Some(selection("session"))),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, "gateway_request_failed");
    assert_eq!(fixture.disposed.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.owner.cached_session_count(), 1);
    assert!(
        !fixture
            .replacement
            .has_transcript_write_reservation(&"web:test".into())
    );
    assert!(
        fixture
            .owner
            .begin_turn(&"web:test".into(), &"r".into())
            .is_some()
    );
}

#[tokio::test]
async fn cancellation_releases_the_transcript_reservation_while_waiting_for_sdk() {
    let fixture = Fixture::new();
    let sdk_guard = fixture.sdk.lock().await;
    let models = fixture.models();
    let params = fixture.params(Some(selection("session")));
    let mut call = Box::pin(handle(&models, "session_model_set", &params));
    tokio::select! {
        _ = &mut call => panic!("SDK lock must keep the request pending"),
        _ = tokio::task::yield_now() => {},
    }
    assert!(
        fixture
            .replacement
            .has_transcript_write_reservation(&"web:test".into())
    );
    drop(call);
    assert!(
        !fixture
            .replacement
            .has_transcript_write_reservation(&"web:test".into())
    );
    drop(sdk_guard);
}

#[tokio::test]
async fn auto_requires_router_and_conflicting_turn_overrides_fail() {
    let fixture = Fixture::new();
    let failure = handle(
        &fixture.models(),
        "session_model_set",
        &fixture.params(Some(object([("mode", "auto".into())]))),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, "ROUTER_AUTO_UNAVAILABLE");
    let failure = resolve_for_turn(
        &fixture.models(),
        &"web:test".into(),
        &fixture.project(),
        &object([
            ("modelSelection", selection("turn")),
            ("modelOverride", selection("sdk")),
        ]),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, "INVALID_MODEL_OVERRIDE");
    assert!(!fixture.transcript_path().exists());
}

#[tokio::test]
async fn sdk_policy_checks_the_selected_session_or_turn_model() {
    let fixture = Fixture::new();
    fixture
        .sdk
        .lock()
        .await
        .set_session_config(
            &"web:test".into(),
            &parse_json(r#"{"managedModels":{"allow":[],"deny":["parity/session"]}}"#).unwrap(),
            Some(&fixture.project()),
            None,
        )
        .await
        .unwrap();
    let failure = handle(
        &fixture.models(),
        "session_model_set",
        &fixture.params(Some(selection("session"))),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, "SDK_MANAGED_MODEL_DENIED");
    assert!(!fixture.transcript_path().exists());
    let failure = resolve_for_turn(
        &fixture.models(),
        &"web:test".into(),
        &fixture.project(),
        &object([("modelSelection", selection("session"))]),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, "SDK_MANAGED_MODEL_DENIED");
    assert_model(&fixture.prepare(&object([])).await, "base", "default");
}

#[test]
fn parameters_are_validated_and_historical_reasoning_is_removed_without_losing_speed() {
    let catalog = object([(
        "items",
        JsValue::Array(vec![object([
            ("id", "parity/session".into()),
            ("provider", "parity".into()),
            ("model", "session".into()),
            ("available", JsValue::Bool(true)),
            ("capabilities", object([("speed", object([]))])),
        ])]),
    )]);
    let mut chosen = selection("session");
    if let JsValue::Object(value) = &mut chosen {
        value.insert("speed".into(), JsValue::Number(1.1));
    }
    assert_eq!(
        validate_explicit(&catalog, &chosen).unwrap_err().code,
        "UNSUPPORTED_MODEL_PARAMETER"
    );
    if let JsValue::Object(value) = &mut chosen {
        value.insert("speed".into(), JsValue::Number(1.0));
        value.insert("reasoning".into(), JsValue::Number(0.8));
        value.insert("temperature".into(), JsValue::Number(0.7));
    }
    assert_eq!(
        validate_explicit(&catalog, &chosen).unwrap_err().code,
        "UNSUPPORTED_MODEL_PARAMETER"
    );
    let restored = restore_selection(&catalog, chosen);
    assert!(field(&restored, "reasoning").is_undefined());
    assert!(field(&restored, "temperature").is_undefined());
    assert_eq!(field(&restored, "speed").as_number(), Some(1.0));
    validate_explicit(&catalog, &restored).unwrap();
    assert!(
        !stringify_json(&restored)
            .unwrap()
            .unwrap()
            .contains("temperature")
    );
}

#[test]
fn utf16_identity_and_timeclip_are_preserved() {
    let session = JsString::from_utf16(vec![0xD800]);
    require_session_key(&session).unwrap();
    assert_eq!(
        require_session_key(&" \u{feff} ".into()).unwrap_err().code,
        "INVALID_SESSION_KEY"
    );
    let project = JsString::from_utf16(vec![0xDFFF]);
    assert_eq!(
        *unknown_project(&project).message.units().last().unwrap(),
        0xDFFF
    );
    assert_eq!(iso_time(-1.0).unwrap(), "1969-12-31T23:59:59.999Z");
    assert!(iso_time(f64::NAN).is_err());
}

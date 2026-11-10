use super::*;
use crate::runtime::{
    project::{DisposeFuture, ProjectRuntimeRegistry, RuntimeError, RuntimeResult},
    refresh::{self, BoxFuture, NotificationServer, RefreshRegistry, RefreshRouter},
    session::{
        LiveSession, LiveSessionHandle, SessionContext, SessionFactory, SessionOwner,
        SessionOwnerOptions,
    },
    turn::gateway::GatewayRequests,
};
use crate::{negative_assert, negative_assert_eq};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const ORACLE: &str = include_str!("../../tests/fixtures/native-management/oracle.json");
fn json_value(value: &JsValue) -> Value {
    serde_json::from_str(
        &pilotdeck_protocol::stringify_json(value)
            .expect("#1231 stringify")
            .expect("#1231 serializable value"),
    )
    .expect("#1231 JSON output")
}
fn normalize(mut value: Value, root: &Path) -> Value {
    fn mtimes(value: &mut Value) {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    if key == "nextCursor" {
                        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
                        let bytes = URL_SAFE_NO_PAD
                            .decode(value.as_str().expect("#1231 cursor string"))
                            .expect("#1231 base64 cursor");
                        *value = serde_json::from_slice(&bytes).expect("#1231 cursor JSON");
                        let signature = URL_SAFE_NO_PAD
                            .decode(value["signature"].as_str().expect("#1231 signature string"))
                            .expect("#1231 base64 signature");
                        value["signature"] =
                            serde_json::from_slice(&signature).expect("#1231 signature JSON");
                    } else if key == "mtime" {
                        negative_assert!(value.is_number(), "#1231 skill mtime numeric");
                        *value = json!("<mtime>");
                    } else {
                        mtimes(value);
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    mtimes(value);
                }
            }
            _ => {}
        }
    }
    mtimes(&mut value);
    serde_json::from_str(
        &value
            .to_string()
            .replace(root.to_str().expect("#1231 root UTF8"), "<root>"),
    )
    .expect("#1231 normalized JSON")
}
fn expand(value: &Value, root: &Path) -> JsValue {
    pilotdeck_protocol::parse_json(
        &value
            .to_string()
            .replace("<root>", root.to_str().expect("#1231 root UTF8")),
    )
    .expect("#1231 expanded JSON")
}
pub(super) struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
/// #1457: the fixture session seeds read state through the same shared
/// algorithm the owned session uses (builtin::seed_agent_read_state), so the
/// seed_read_state tests exercise the real admission/classification path.
struct Session {
    checkpoint: crate::host::modules::tool_context::HostToolCheckpoint,
    cwd: PathBuf,
}
impl LiveSession for Session {
    fn snapshot(&self) -> JsValue {
        crate::metadata::object([("messages", JsValue::Array(vec![]))])
    }
    fn abort(&self, _: &JsString) -> RuntimeResult {
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn seed_read_state(&self, path: &JsString, mtime_ms: f64) -> Result<bool, RuntimeError> {
        crate::runtime::tools::builtin::seed_agent_read_state(
            &self.checkpoint,
            &JsString::from(self.cwd.to_string_lossy().into_owned()),
            &"default".into(),
            path,
            mtime_ms,
        )
        .map_err(|error| {
            RuntimeError::new(String::from_utf16_lossy(error.message.units())).with_js_error(
                crate::runtime::project::RuntimeJsError {
                    name: "Error".into(),
                    code: Some(error.code),
                    message: error.message,
                    requires_host: false,
                },
            )
        })
    }
}
struct Factory(PathBuf);
impl SessionFactory for Factory {
    fn create(
        &self,
        _: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        let cwd = self.0.clone();
        Box::pin(async move {
            Ok(LiveSessionHandle::new(
                Arc::new(Session {
                    checkpoint: Default::default(),
                    cwd,
                }),
                None,
            ))
        })
    }
    fn can_recreate(&self) -> bool {
        true
    }
}
fn context(project: &Path) -> SessionContext {
    SessionContext {
        session_key: "fixture".into(),
        project_key: Some(project.to_string_lossy().into_owned().into()),
        channel_key: "test".into(),
        allowed_tools: None,
        disallowed_tools: None,
    }
}
pub(super) fn setup() -> (Scratch, requests::Requests) {
    let root = std::env::temp_dir().join(format!("pdrs-1231-{}", uuid::Uuid::new_v4()));
    for dir in ["home", "project/.pilotdeck/commands"] {
        std::fs::create_dir_all(root.join(dir)).expect("#1231 scratch directory");
    }
    for file in ["token", "sidecar"] {
        std::fs::write(root.join(file), "fixture").expect("#1231 scratch file");
    }
    let root = root.canonicalize().expect("#1231 canonical scratch");
    std::fs::write(root.join("home/pilotdeck.yaml"),"schemaVersion: 1\nagent:\n  model: test/test\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:1\n      apiKey: test-only\n      models:\n        test: {}\n").expect("#1231 config");
    std::fs::write(
        root.join("project/.pilotdeck/commands/a8demo.md"),
        "# A8 command\n",
    )
    .expect("#1231 command file");
    std::fs::write(
        root.join("project/.pilotdeck/commands/replacement�.md"),
        "# Replacement command\n",
    )
    .expect("#1231 replacement command file");
    let options = Options::parse([
        "--home".into(),
        root.join("home").into_os_string(),
        "--project".into(),
        root.join("project").into_os_string(),
        "--listen".into(),
        "127.0.0.1:0".into(),
        "--token-file".into(),
        root.join("token").into_os_string(),
        "--sidecar".into(),
        root.join("sidecar").into_os_string(),
    ])
    .expect("#1231 isolated options");
    std::fs::create_dir_all(options.home.join("projects/a8-main"))
        .expect("#1231 stable project directory");
    std::fs::write(
        options.home.join("projects/a8-main/.cwd"),
        options.project.to_string_lossy().as_bytes(),
    )
    .expect("#1231 stable project marker");
    assembly::register_project(&options).expect("#1231 register project");
    let chat = crate::session_chat_dir(
        &options.home,
        &options.project,
        &options.project.to_string_lossy(),
    );
    std::fs::create_dir_all(&chat).expect("#1231 transcript directory");
    let session = chat.join("a8-session.jsonl");
    std::fs::write(
        &session,
        include_bytes!("../../tests/fixtures/native-management/session.jsonl"),
    )
    .expect("#1231 seeded transcript");
    std::fs::File::open(&session)
        .expect("#1231 transcript metadata handle")
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        )
        .expect("#1231 fixed transcript timestamp");
    let mut env = options.environment(crate::config::Env::new());
    env.insert(
        "PILOTDECK_STORAGE_ROOT".into(),
        root.join("home/storage").to_string_lossy().into_owned(),
    );
    let project = assembly::project_registry(&options, &env);
    let mut owner_options = SessionOwnerOptions::new(Arc::new(Factory(options.project.clone())));
    owner_options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(owner_options);
    let runs = Arc::new(
        crate::runtime::turn::publish::FileRegistry::new(
            crate::storage::FileRunRegistry::open(root.join("home/runs.json"))
                .expect("#1231 run file"),
        )
        .expect("#1231 run registry"),
    );
    (Scratch(root), {
        let replacement_owner = owner.clone();
        requests::Requests {
            options,
            env,
            snapshot_enabled: true,
            memory: Arc::new(crate::runtime::memory::gateway::MemoryGateway::disabled()),
            project,
            owner: owner.clone(),
            coordinator: Arc::default(),
            runs: Some(runs),
            replacement: Arc::new(crate::runtime::replacement::ReplacementCoordinator::new(
                Default::default(),
                Arc::new(replacement_owner),
                5000,
                None,
            )),
            recorders: Arc::default(),
            interactions: Arc::default(),
            mcp: requests::Requests::stub_mcp(&owner),
            async_hook_registry: None,
            // #1600: same state production assembles
            // (`native_process/assembly/user_dialog.rs`): connected, no
            // renderer observer. The legacy observer-coupled `new(runtime,
            // None)` made the shared fixture diverge from production, so any
            // case relying on "the bus is unavailable" here would test a state
            // that no longer exists. A case that genuinely needs the
            // unconnected state builds it explicitly.
            user_dialog_bus: Arc::new(std::sync::Mutex::new(
                crate::user_dialog::GatewayUserDialogBus::connected(
                    crate::user_dialog::DialogRuntime::default(),
                ),
            )),
            upload_gateway: Arc::new(crate::upload::gateway::UploadGateway::unavailable()),
            prepare_weixin_login: None,
            cron: None,
            always_on: None,
            settings: requests::Requests::stub_settings(&owner),
            session_overrides: Arc::default(),
            restoring_session_keys: Arc::default(),
        }
    })
}

/// #1600 negative control: the shared fixture must assemble the same
/// user-dialog bus state production does
/// (`native_process/assembly/user_dialog.rs`), so no case can silently assert
/// against the retired `connected = false` state. Reverting `setup()` to the
/// observer-coupled `GatewayUserDialogBus::new(runtime, None)` turns this red.
#[tokio::test]
async fn setup_assembles_a_connected_user_dialog_bus() {
    let (_scratch, requests) = setup();
    negative_assert!(
        requests
            .user_dialog_bus
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_connected(),
        "#1600 the shared fixture's user-dialog bus must be connected like production"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1600 fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1600 fixture project shutdown");
}

type Trace = Arc<Mutex<Vec<Value>>>;
struct Registry(Arc<ProjectRuntimeRegistry>, Trace);
impl RefreshRegistry for Registry {
    fn reload(&self) -> Result<BoxFuture<RuntimeResult>, RuntimeError> {
        let p = self.0.clone();
        Ok(Box::pin(async move { p.reload().await }))
    }
    fn invalidate(&self, key: Option<&JsString>) {
        let key = key.map(|k| k.to_utf8().expect("#1231 project key"));
        self.0.invalidate(key.as_deref().map(Path::new));
        self.1
            .lock()
            .expect("#1231 trace lock")
            .push(json!({"event":"invalidate","projectKey":key}));
    }
}
struct Router(SessionOwner, Trace);
impl RefreshRouter for Router {
    fn mark_project_dirty(&self, key: &JsString, reason: &str) -> usize {
        let count = self.0.mark_project_dirty(key, reason);
        self.1.lock().expect("#1231 trace lock").push(json!({"event":"dirty","projectKey":key.to_utf8().expect("#1231 project key"),"reason":reason,"count":count}));
        count
    }
    fn mark_all_dirty(&self, reason: &str) -> usize {
        self.0.mark_all_dirty(reason)
    }
    fn mark_session_dirty(&self, key: &JsString, reason: &str) -> bool {
        self.0.mark_session_dirty(key, reason)
    }
    fn cached_session_count(&self) -> usize {
        self.0.cached_session_count()
    }
    fn snapshot_session(&self, key: &JsString) -> Option<JsValue> {
        self.0.snapshot_session(key)
    }
}
struct Server(Trace);
impl NotificationServer for Server {
    fn broadcast_notification(&self, name: &str, payload: &JsValue) {
        self.0
            .lock()
            .expect("#1231 trace lock")
            .push(json!({"event":"notification","name":name,"payload":json_value(payload)}));
    }
}
struct Unused;
impl refresh::RefreshConfigStore for Unused {
    fn subscribe(&self, _: refresh::ConfigListener) -> refresh::Unsubscribe {
        Box::new(|| {})
    }
    fn reload(&self, _: &'static str) -> BoxFuture<refresh::ConfigReloadResult> {
        panic!("#1231 config reload not used")
    }
}
impl refresh::MemoryMaintenance for Unused {
    fn schedule(&self, _: &JsString) -> RuntimeResult {
        panic!("#1231 memory not used")
    }
}
impl refresh::RefreshLog for Unused {
    fn log(&self, _: &JsString) {}
    fn warn(&self, _: &str, _: refresh::WarnDetail<'_>) {
        panic!("#1231 unexpected warning")
    }
}
fn refresh(requests: &requests::Requests, trace: Trace) -> refresh::RefreshBundle {
    let router: Arc<dyn RefreshRouter> = Arc::new(Router(requests.owner.clone(), trace.clone()));
    let bundle = refresh::RefreshBundle::new(refresh::RefreshOptions {
        config_store: Arc::new(Unused),
        registry: Arc::new(Registry(requests.project.clone(), trace.clone())),
        memory_maintenance: Arc::new(Unused),
        router: Arc::new(move || Some(router.clone())),
        project_root: requests
            .options
            .project
            .to_string_lossy()
            .into_owned()
            .into(),
        memory_diagnostics: None,
        sdk: None,
        log: Arc::new(Unused),
    });
    bundle.bind_server(Arc::new(Server(trace)));
    bundle
}
async fn call(requests: &requests::Requests, method: &Value, params: &Value, root: &Path) -> Value {
    call_wire(requests, method, expand(params, root), root).await
}
async fn call_wire(
    requests: &requests::Requests,
    method: &Value,
    params: JsValue,
    root: &Path,
) -> Value {
    let result = requests
        .request(method.as_str().expect("#1231 method").into(), params)
        .await;
    normalize(json_value(&management::outcome(result)), root)
}

#[tokio::test]
async fn route_refresh_reread_matches_ts() {
    crate::test_baseline::oracle_bounded(ORACLE);
    let oracle: Value = serde_json::from_str(ORACLE).expect("#1231 oracle JSON");
    let (scratch, requests) = setup();
    let trace: Trace = Arc::default();
    let bundle = refresh(&requests, trace.clone());
    for row in oracle["cases"].as_array().expect("#1231 matrix") {
        let before = requests
            .project
            .acquire(None)
            .expect("#1231 before generation");
        let before_session = requests
            .owner
            .get_or_create(context(&requests.options.project))
            .await
            .expect("#1231 matrix cached session");
        let first = call(&requests, &row["method"], &row["params"], &scratch.0).await;
        negative_assert_eq!(first, row["first"], "#1231 routed first result");
        negative_assert_eq!(
            json!(*trace.lock().expect("#1231 trace lock")),
            row["directTrace"],
            "#1231 direct RPC has no notification"
        );
        let refreshed = bundle.reload_extensions(refresh::ExtensionsReload {
            project_key: Some(
                requests
                    .options
                    .project
                    .to_string_lossy()
                    .into_owned()
                    .into(),
            ),
            changed_paths: Some(vec![row["family"].as_str().expect("#1231 family").into()]),
        });
        negative_assert_eq!(
            json!({"reloaded":refreshed.reloaded,"changedPaths":refreshed.changed_paths.iter().map(|s|s.to_utf8().expect("#1231 changed path")).collect::<Vec<_>>()}),
            row["refreshed"],
            "#1231 refresh result"
        );
        let observed = std::mem::take(&mut *trace.lock().expect("#1231 trace lock"));
        negative_assert_eq!(
            normalize(json!(observed), &scratch.0),
            row["refreshTrace"],
            "#1231 invalidate dirty notification order"
        );
        negative_assert_eq!(
            call(&requests, &row["read"], &row["readParams"], &scratch.0).await,
            row["second"],
            "#1231 routed reread result"
        );
        let after = requests
            .project
            .acquire(None)
            .expect("#1231 after generation");
        let after_session = requests
            .owner
            .get_or_create(context(&requests.options.project))
            .await
            .expect("#1231 matrix refreshed session");
        negative_assert_eq!(
            !before_session.same_identity(&after_session),
            row["dirtyRecreated"].as_bool().expect("#1231 dirty flag"),
            "#1231 dirty flag recreates cached session"
        );
        negative_assert_eq!(
            !Arc::ptr_eq(before.runtime(), after.runtime()),
            row["generationChanged"]
                .as_bool()
                .expect("#1231 generation flag"),
            "#1231 generation replaced"
        );
    }
    for row in oracle["requests"].as_array().expect("#1231 requests") {
        let mut params = row["params"].clone();
        if let Some(form) = row["cursorForm"].as_str() {
            use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
            // Build the same real Buffer input with this run's isolated root.
            let signature = URL_SAFE_NO_PAD.encode(format!(
                "{{\"projectKey\":{},\"query\":{}}}",
                serde_json::to_string(&requests.options.project.to_string_lossy())
                    .expect("#1231 cursor project"),
                serde_json::to_string(&params["query"]).expect("#1231 cursor query")
            ));
            let bytes = format!("{{\"offset\":0,\"signature\":\"{signature}\"}}").into_bytes();
            let mut fields = serde_json::Map::new();
            for (i, n) in bytes.iter().enumerate() {
                fields.insert(i.to_string(), json!(n));
            }
            fields.insert("length".into(), json!(bytes.len()));
            let arraylike = Value::Object(fields.clone());
            params["cursor"] = match form {
                "valueOf-array" => {
                    fields.insert("valueOf".into(), json!([5]));
                    Value::Object(fields)
                }
                "high-base64" => json!(
                    URL_SAFE_NO_PAD
                        .encode(&bytes)
                        .chars()
                        .map(|c| char::from_u32(c as u32 + 256).expect("#1231 high cursor char"))
                        .collect::<String>()
                ),
                "empty-array" => json!([]),
                "empty-buffer" => json!({"type":"Buffer","data":[]}),
                "zero-length" => json!({"length":0}),
                "buffer-length" => json!({"type":"Buffer","data":bytes,"length":3}),
                "null-length-buffer" => json!({"type":"Buffer","data":bytes,"length":null}),
                "array" => json!(bytes),
                "buffer" => json!({"type":"Buffer","data":bytes}),
                "string-bytes" => json!(bytes.iter().map(|n| n.to_string()).collect::<Vec<_>>()),
                "wrapped-bytes" => json!(
                    bytes
                        .iter()
                        .map(|n| u32::from(*n) + 256)
                        .collect::<Vec<_>>()
                ),
                "fractional-length" => {
                    fields.insert("length".into(), json!(bytes.len() as f64 + 0.9));
                    Value::Object(fields)
                }
                "bad-valueOf" => {
                    fields.insert("valueOf".into(), json!(5));
                    Value::Object(fields)
                }
                "string-length" => {
                    fields.insert("length".into(), json!(bytes.len().to_string()));
                    Value::Object(fields)
                }
                "bad-buffer-data" => json!({"type":"Buffer","data":arraylike}),
                "arraylike" => arraylike,
                _ => panic!("#1231 unknown cursor form"),
            };
        }
        let mut input = expand(&params, &scratch.0);
        if let Some(units) = row["queryUnits"].as_array() {
            let JsValue::Object(ref mut fields) = input else {
                panic!("#1231 query wire object")
            };
            fields.insert(
                "query".into(),
                JsValue::String(JsString::from_utf16(
                    units
                        .iter()
                        .map(|n| {
                            u16::try_from(n.as_u64().expect("#1231 query unit integer"))
                                .expect("#1231 query unit range")
                        })
                        .collect::<Vec<_>>(),
                )),
            );
        }
        negative_assert_eq!(
            call_wire(&requests, &row["method"], input, &scratch.0).await,
            row["result"],
            "#1231 method wire result"
        );
    }
    bundle.dispose();
    requests
        .owner
        .shutdown()
        .await
        .expect("#1231 shutdown sessions");
    requests
        .project
        .dispose()
        .await
        .expect("#1231 dispose project");
}

#[tokio::test]
async fn dirty_refresh_recreates_cached_session() {
    let (_scratch, requests) = setup();
    let trace: Trace = Arc::default();
    let bundle = refresh(&requests, trace);
    let before = requests
        .owner
        .get_or_create(context(&requests.options.project))
        .await
        .expect("#1231 cached session");
    bundle.reload_extensions(refresh::ExtensionsReload {
        project_key: Some(
            requests
                .options
                .project
                .to_string_lossy()
                .into_owned()
                .into(),
        ),
        changed_paths: None,
    });
    let after = requests
        .owner
        .get_or_create(context(&requests.options.project))
        .await
        .expect("#1231 refreshed session");
    negative_assert!(
        !before.same_identity(&after),
        "#1231 dirty flag recreates cached session"
    );
    bundle.dispose();
    requests
        .owner
        .shutdown()
        .await
        .expect("#1231 shutdown sessions");
    requests
        .project
        .dispose()
        .await
        .expect("#1231 dispose project");
}

#[test]
#[ignore = "requires pinned Node and explicit PDRS_BASELINE"]
fn regenerates_identically() {
    crate::test_baseline::regenerates_identically("native-management");
}

#[derive(Default)]
struct Text(Mutex<Vec<Value>>);
impl crate::runtime::connection::GatewayTextConnection for Text {
    fn send_text(&self, text: String) {
        self.0
            .lock()
            .expect("#1231 frames lock")
            .push(serde_json::from_str(&text).expect("#1231 frame JSON"));
    }
    fn close(&self, _: u16, _: &str) {}
}

#[tokio::test]
async fn production_host_management_and_restart() {
    let (_scratch, requests) = setup();
    let options = requests.options.clone();
    requests
        .project
        .dispose()
        .await
        .expect("#1231 dispose unused test registry");
    requests
        .owner
        .shutdown()
        .await
        .expect("#1231 shutdown unused test owner");
    for restart in [false, true] {
        // Snapshot success requires an explicit isolated storage authority.
        let host = assembly::start(options.clone(), requests.env.clone())
            .await
            .expect("#1231 native management boot/restart");
        let text = Arc::new(Text::default());
        let connection = host.connect(text.clone(), "token".into(), "fixture".into());
        connection.receive(json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"fixture"}).to_string()).await.expect("#1231 hello");
        let mut calls = vec![
            ("snapshot_list", json!({"projectKey":options.project})),
            ("manager_sessions", json!({"projectKey":options.project})),
            (
                "commands_list",
                json!({"projectKey":options.project,"query":"a8demo"}),
            ),
        ];
        if !restart {
            calls.push(("skill_create",json!({"projectKey":options.project,"scope":"project","slug":"persisted","content":"# Persistent\n"})));
        }
        calls.push((
            "skill_read",
            json!({"projectKey":options.project,"scope":"project","slug":"persisted"}),
        ));
        for (method, params) in calls {
            connection
                .receive(
                    json!({"type":"request","id":method,"method":method,"params":params})
                        .to_string(),
                )
                .await
                .expect("#1231 route frame");
            let frames = text.0.lock().expect("#1231 frames lock");
            let response = frames
                .iter()
                .find(|f| f["id"] == method)
                .expect("#1231 route response");
            if method == "snapshot_list" {
                negative_assert_eq!(
                    response["ok"],
                    true,
                    "#1231 snapshot route with explicit storage"
                );
            } else {
                negative_assert_eq!(response["ok"], true, "#1231 production route success");
            }
        }
        connection
            .receive(
                json!({"type":"request","id":"describe","method":"describe_server"}).to_string(),
            )
            .await
            .expect("#1231 capability describe request");
        let description = {
            let frames = text.0.lock().expect("#1231 capability frames");
            let response = frames
                .iter()
                .find(|frame| frame["id"] == "describe")
                .expect("#1231 capability describe response");
            negative_assert_eq!(response["ok"], true, "#1231 capability describe success");
            response["result"].clone()
        };
        let mut capabilities: Vec<&str> = description["capabilities"]
            .as_array()
            .expect("#1231 capabilities")
            .iter()
            .map(|value| value.as_str().expect("#1231 capability string"))
            .collect();
        capabilities.sort_unstable();
        let expected = expected_capabilities(true);
        negative_assert_eq!(capabilities, expected, "#1231 exact wired capability set");
        // No `assert_ts_capability_subset` here on purpose: this table runs in
        // the default (snapshots-enabled) state, and exact equality above is
        // strictly stronger than any subset check. Comparing a
        // snapshots-enabled table against the two disabled contracts would
        // false-FAIL the day TS legitimately drops names from them (#1761
        // paired state to state for exactly this reason). The #1352 derived
        // expectation makes the enabled-state subset check a tautology anyway.
        negative_assert!(
            assembly::validate_profile(&options, &options.environment(crate::config::Env::new()))
                .await
                .is_err_and(|e| e
                    .to_string()
                    .contains("configured plugins, skills or hooks")),
            "#1231 skill management does not enable execution"
        );
        host.shutdown(options.drain_budget)
            .await
            .expect("#1231 native host shutdown");
    }
}

#[tokio::test]
async fn admission_and_unwired_families_stay_closed() {
    let (scratch, requests) = setup();
    // #1319 wired the MCP four-control family and #1322 wired memory here, so
    // no family stays unwired. The MCP methods live in their own registered
    // family; the memory pair enters `management_request` and is rejected by
    // the fixture's disabled gateway after parameter validation.
    for method in ["memory_list", "memory_wipe"] {
        negative_assert!(
            requests
                .management_request(method, &crate::metadata::object([]))
                .await
                .is_some(),
            "#1322 wired memory family routes through management"
        );
    }
    let target = scratch.0.join("outside");
    std::fs::create_dir_all(&target).expect("#1231 outside target");
    let result=requests.request("snapshot_restore".into(),expand(&json!({"projectKey":"<root>/project","snapshotId":"missing","targetProjectKey":target}),&scratch.0)).await;
    negative_assert_eq!(
        result.expect_err("#1231 foreign restore must fail").code,
        "CAPABILITY_UNAVAILABLE",
        "#1231 restore target project admission"
    );
    let units = format!(
        "{{\"projectKey\":\"{}\",\"snapshotId\":\"missing\",\"targetProjectKey\":\"{}/\\ud800/..\"}}",
        requests.options.project.display(),
        requests.options.home.display()
    );
    let malformed = pilotdeck_protocol::parse_json(&units).expect("#1231 surrogate target JSON");
    let result = requests.request("snapshot_restore".into(), malformed).await;
    negative_assert_eq!(
        result.expect_err("#1231 surrogate restore must fail").code,
        "CAPABILITY_UNAVAILABLE",
        "#1231 surrogate restore target admission"
    );
    let result = requests
        .request(
            "skill_list".into(),
            pilotdeck_protocol::parse_json(r#"{"projectKey":{"constructor":5}}"#)
                .expect("#1231 host boundary params"),
        )
        .await;
    negative_assert_eq!(
        result
            .expect_err("#1231 host-dependent skill must fail")
            .code,
        "CAPABILITY_UNAVAILABLE",
        "#1231 host-dependent skill rejected"
    );
    let entries = std::fs::read_dir(&target)
        .expect("#1231 target entries")
        .count();
    negative_assert_eq!(entries, 0, "#1231 rejected restore has no writes");
    requests
        .owner
        .shutdown()
        .await
        .expect("#1231 shutdown sessions");
    requests
        .project
        .dispose()
        .await
        .expect("#1231 dispose project");
}

#[tokio::test]
async fn pending_file_read_does_not_block_serial_host() {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let (scratch, requests) = setup();
    let options = requests.options.clone();
    requests
        .project
        .dispose()
        .await
        .expect("#1231 unused registry disposal");
    requests
        .owner
        .shutdown()
        .await
        .expect("#1231 unused owner shutdown");
    let dir = options.project.join(".pilotdeck/skills/pending");
    std::fs::create_dir_all(&dir).expect("#1231 pending skill directory");
    let fifo = dir.join("SKILL.md");
    negative_assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("#1231 mkfifo launch")
            .success(),
        "#1231 FIFO fixture"
    );
    let host = assembly::start(options.clone(), requests.env.clone())
        .await
        .expect("#1231 pending IO host");
    let text = Arc::new(Text::default());
    let connection = host.connect(text.clone(), "token".into(), "fixture".into());
    connection.receive(json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"fixture"}).to_string()).await.expect("#1231 pending IO hello");
    let pending=connection.enqueue(json!({"type":"request","id":"pending","method":"skill_read","params":{"projectKey":options.project,"scope":"project","slug":"pending"}}).to_string()).await.expect("#1231 pending IO admission");
    drop(pending); // Caller cancellation must not abandon the host-owned IO.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut writer = loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
        {
            Ok(writer) => break writer,
            Err(error)
                if error.raw_os_error() == Some(libc::ENXIO)
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(1)).await
            }
            Err(error) => panic!("#1231 pending read did not reach FIFO: {error}"),
        }
    };
    let independent=tokio::time::timeout(Duration::from_secs(3),connection.receive(json!({"type":"request","id":"independent","method":"snapshot_list","params":{"projectKey":options.project}}).to_string())).await;
    let read_pending = !text
        .0
        .lock()
        .expect("#1231 pending frames")
        .iter()
        .any(|frame| frame["id"] == "pending");
    // Release the physical IO before any bound assertion can panic. Replace
    // the path so SkillManager's second metadata read sees a regular file.
    std::fs::write(dir.join("next.md"), "# Released\n").expect("#1231 FIFO replacement");
    std::fs::rename(dir.join("next.md"), &fifo).expect("#1231 FIFO replacement publish");
    writer
        .write_all(b"# Released\n")
        .expect("#1231 release FIFO");
    drop(writer);
    tokio::time::timeout(Duration::from_secs(3), async {
        while !text
            .0
            .lock()
            .expect("#1231 completion frames")
            .iter()
            .any(|frame| frame["id"] == "pending")
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("#1231 cancelled waiter IO still completes");
    host.shutdown(options.drain_budget)
        .await
        .expect("#1231 pending host shutdown");
    negative_assert!(
        independent.is_ok_and(|result| result.is_ok()),
        "#1231 pending file IO must not block host"
    );
    let oracle: Value = serde_json::from_str(ORACLE).expect("#1231 pending IO oracle");
    negative_assert_eq!(
        read_pending,
        oracle["pendingIo"]["readPending"]
            .as_bool()
            .expect("#1231 pending flag"),
        "#1231 predecessor stays pending"
    );
    let frames = text.0.lock().expect("#1231 pending frames");
    for (id, key) in [("pending", "result"), ("independent", "independent")] {
        let frame = frames
            .iter()
            .find(|frame| frame["id"] == id)
            .expect("#1231 pending IO response");
        negative_assert_eq!(frame["ok"], true, "#1231 pending IO succeeds");
        negative_assert_eq!(
            normalize(json!({"ok":frame["result"]}), &scratch.0),
            oracle["pendingIo"][key],
            "#1231 pending IO matches TS"
        );
    }
}

#[test]
fn management_progress_with_one_available_blocking_thread() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
        .expect("#1231 limited runtime");
    let result = runtime.block_on(async {
        let (scratch, requests) = setup();
        let (release, gate) = std::sync::mpsc::channel();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let parked = tokio::task::spawn_blocking(move || {
            entered.send(()).expect("#1231 parked slot signal");
            gate.recv().expect("#1231 parked slot release");
        });
        ready.await.expect("#1231 occupied blocking slot");
        let mut passed = true;
        for method in [
            "commands_list",
            "snapshot_list",
            "manager_sessions",
            "skill_list",
        ] {
            let result = tokio::time::timeout(
                Duration::from_secs(3),
                requests.request(
                    method.into(),
                    expand(
                        &json!({"projectKey":"<root>/project","query":"a8demo","scope":"project"}),
                        &scratch.0,
                    ),
                ),
            )
            .await;
            if !result.is_ok_and(|r| r.is_ok()) {
                passed = false;
                break;
            }
        }
        // Release before asserting, including the nested-blocking mutant.
        release.send(()).expect("#1231 release reserved slot");
        parked.await.expect("#1231 reserved slot finished");
        requests
            .owner
            .shutdown()
            .await
            .expect("#1231 limited owner shutdown");
        tokio::time::timeout(Duration::from_secs(3), requests.project.dispose())
            .await
            .expect("#1231 limited registry shutdown deadline")
            .expect("#1231 limited registry shutdown");
        passed
    });
    runtime.shutdown_timeout(Duration::from_secs(3));
    negative_assert!(result, "#1231 management progress with one blocking slot");
}

fn seed_files(directory: &Path, files: &Value, root: &Path) {
    for file in files.as_array().expect("#1231 seed file list") {
        let path = directory.join(file["path"].as_str().expect("#1231 seed file path"));
        std::fs::create_dir_all(path.parent().expect("#1231 seed file parent"))
            .expect("#1231 seed directory");
        std::fs::write(
            path,
            file["text"]
                .as_str()
                .expect("#1231 seed file text")
                .replace("<root>", root.to_str().expect("#1231 seed root")),
        )
        .expect("#1231 seed file bytes");
    }
}

fn file_tree(directory: &Path) -> Value {
    fn walk(directory: &Path, relative: &Path, rows: &mut Vec<Value>) {
        let mut entries = std::fs::read_dir(directory.join(relative))
            .expect("#1231 observed file tree directory")
            .map(|entry| entry.expect("#1231 observed file tree entry"))
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = relative.join(entry.file_name());
            let kind = entry.file_type().expect("#1231 observed file type");
            if kind.is_dir() {
                walk(directory, &path, rows);
            } else {
                negative_assert!(kind.is_file(), "#1231 observed regular file");
                rows.push(json!({"path":path,"text":std::fs::read_to_string(entry.path()).expect("#1231 observed file bytes")}));
            }
        }
    }
    let mut rows = vec![];
    walk(directory, Path::new(""), &mut rows);
    json!(rows)
}

async fn assert_storage_disabled(storage: Option<&str>, fixture: &str) {
    let (scratch, requests) = setup();
    let options = requests.options.clone();
    let oracle: Value = serde_json::from_str(ORACLE).expect("#1231 disabled storage oracle");
    // Existing data must not implicitly enable the provider without configuration.
    seed_files(
        &options.home.join("storage"),
        &oracle["snapshotSuccess"]["storageFiles"],
        &scratch.0,
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1231 disabled storage unused owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1231 disabled storage unused registry disposal");
    let mut env = crate::config::Env::new();
    if let Some(storage) = storage {
        env.insert("PILOTDECK_STORAGE_ROOT".into(), storage.into());
    }
    validate_environment(&env).expect("#1231 disabled storage inherited admission");
    let mut assembled_env = options.environment(env);
    // options.environment() removes PILOTDECK_STORAGE_ROOT when storage_root is None.
    // Re-inject so assembly.rs can observe the original inherited value at its seams.
    if let Some(storage_value) = storage {
        assembled_env.insert("PILOTDECK_STORAGE_ROOT".into(), storage_value.into());
    }
    let host = assembly::start(options.clone(), assembled_env)
        .await
        .expect("#1231 disabled storage host startup");
    let text = Arc::new(Text::default());
    let connection = host.connect(text.clone(), "token".into(), "fixture".into());
    connection.receive(json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"fixture"}).to_string()).await.expect("#1231 disabled storage hello");
    for method in ["snapshot_list", "snapshot_get", "snapshot_restore"] {
        let row = oracle["contract"][fixture]["requests"]
            .as_array()
            .expect("#1231 disabled storage TS requests")
            .iter()
            .find(|row| row["method"] == method)
            .expect("#1231 disabled storage TS method");
        let params = json_value(&expand(&row["params"], &scratch.0));
        connection
            .receive(
                json!({"type":"request","id":method,"method":method,"params":params}).to_string(),
            )
            .await
            .expect("#1231 disabled storage request");
        let frames = text.0.lock().expect("#1231 disabled storage frames");
        let response = frames
            .iter()
            .find(|frame| frame["id"] == method)
            .expect("#1231 disabled storage response");
        negative_assert_eq!(
            json!({"ok":response["ok"],"error":response["error"]}),
            json!({"ok":false,"error":row["result"]["error"]}),
            "#1231 disabled storage snapshot result"
        );
    }
    connection
        .receive(json!({"type":"request","id":"describe","method":"describe_server"}).to_string())
        .await
        .expect("#1231 disabled storage describe request");
    {
        let frames = text
            .0
            .lock()
            .expect("#1231 disabled storage describe frames");
        let response = frames
            .iter()
            .find(|frame| frame["id"] == "describe")
            .expect("#1231 disabled storage describe response");
        negative_assert_eq!(
            response["ok"],
            true,
            "#1231 disabled storage describe success"
        );
        let mut capabilities: Vec<&str> = response["result"]["capabilities"]
            .as_array()
            .expect("#1231 disabled storage capabilities")
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .expect("#1231 disabled storage capability string")
            })
            .collect();
        capabilities.sort_unstable();
        let expected = expected_capabilities(false);
        negative_assert_eq!(
            capabilities,
            expected,
            "#1231 disabled storage exact capability set"
        );
        assert_ts_capability_subset(&expected);
    }
    host.shutdown(options.drain_budget)
        .await
        .expect("#1231 disabled storage host shutdown");
}

#[tokio::test]
async fn missing_storage_environment_disables_snapshots() {
    assert_storage_disabled(None, "missingStorage").await;
}

#[tokio::test]
async fn empty_storage_environment_disables_snapshots() {
    assert_storage_disabled(Some(""), "emptyStorage").await;
}

/// The parsed `oracle.json`. Held in a `OnceLock` so the capability names it
/// carries can be handed out as `&'static str`, which keeps
/// [`expected_capabilities`] and [`assert_ts_capability_subset`] on the plain
/// sorted-`Vec<&str>` comparison they already use.
fn oracle_json() -> &'static Value {
    static PARSED: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    PARSED.get_or_init(|| {
        serde_json::from_str(ORACLE).expect("#1231 TS capability oracle is parseable")
    })
}

/// The capability names the recorded TS baseline advertises in one fixture
/// config state (`contract`, `contract.emptyStorage`, `contract.missingStorage`).
fn oracle_capabilities(state: &str) -> &'static [Value] {
    let contract = &oracle_json()["contract"];
    let node = if state == "contract" {
        contract
    } else {
        &contract[state]
    };
    node["capabilities"]
        .as_array()
        .unwrap_or_else(|| panic!("#1231 TS capability array missing at contract.{state}"))
}

/// Providers the management fixture leaves unbound, so their names are absent
/// from every live advertisement here even though the TS fixture configures
/// them: `setup()` builds the `MemoryGateway` without a projection
/// (`MemoryGateway::is_enabled()` is false). The memory pair is exercised by
/// `memory_management_request_disabled_gateway_returns_unavailable`, so
/// filtering it here does not remove the only assertion covering it.
const PROVIDERS_UNBOUND_IN_FIXTURE: &[&str] = &["memory_list", "memory_wipe"];

/// The exact capability set `describe_server` must advertise (#1231).
///
/// Derived from what the TS baseline actually recorded rather than from a
/// second hand-kept list (#1352): `oracle.json` is the baseline's own
/// `describe_server` output for the three fixture config states, all three
/// carrying the same 50 names. The Rust set is
///
///   recorded − `capabilities::NOT_ADVERTISED` − providers this fixture does
///   not bind (memory pair, and the snapshot triplet without `--storage-root`)
///
/// so the only way to omit a name the baseline records is to declare it, with
/// its reason, in one of those two lists — and a name the table invents or
/// misspells fails here. See `capability_table_covers_every_recorded_name` for
/// the same invariant stated over the table itself rather than over a live host.
fn expected_capabilities(with_snapshots: bool) -> Vec<&'static str> {
    let mut expected: Vec<&'static str> = oracle_capabilities("contract")
        .iter()
        .map(|value| value.as_str().expect("#1231 TS capability string"))
        .filter(|name| {
            !super::capabilities::NOT_ADVERTISED
                .iter()
                .any(|(declared, _)| declared == name)
        })
        .filter(|name| !PROVIDERS_UNBOUND_IN_FIXTURE.contains(name))
        .filter(|name| {
            with_snapshots
                || !matches!(*name, "snapshot_list" | "snapshot_get" | "snapshot_restore")
        })
        // `tools::METHODS` registers the two background names only on unix, and
        // the table drops a name whose method is missing; the expectation has to
        // follow, or the exact-set test would demand a name the registry cannot
        // route on Windows.
        .filter(|name| cfg!(unix) || !matches!(*name, "background_task_stop" | "background_tasks"))
        .collect();
    expected.sort_unstable();
    expected
}

/// #1761/#1352: a **snapshots-disabled** table must be legal in both disabled
/// TS config states (`contract.emptyStorage` / `contract.missingStorage`).
///
/// Only the disabled-table call sites use this. They are not tautological: the
/// expectation is derived from `contract`, the recorded arrays compared against
/// are the two disabled ones. The enabled-table sites skip it — exact equality
/// there is strictly stronger, and comparing an enabled table against a
/// disabled contract would false-FAIL if TS ever legally diverged the three
/// arrays (#1761 paired state to state for that reason). #1352 removed the old
/// `include_disabled_states` flag because its `false` branch compared the
/// derived expectation against `contract` itself, which can never fail.
///
/// #1434 item 4: the anti-drift contract around the oracle-derived
/// expectation, so "main wires a new capability" cannot silently pass or
/// spuriously fail. Both directions fail loudly today:
///
/// - a **wired** capability the oracle does not record makes the exact-set
///   comparisons red (`live ⊋ oracle-derived`) — the false-pass side;
/// - an **oracle** name nothing wires or excuses makes them red the other way
///   (`oracle-derived ⊋ live`) — the false-failure side, which is what forces
///   `oracle.json` to be regenerated with the TS baseline rather than edited
///   by hand;
/// - a name present in `contract` but missing from the two disabled states
///   fails this subset check, and
///   `settings_capabilities_match_served_delegates` (#1869) pins the three
///   states' sets equal, so the subset check cannot hide behind a diverged
///   state either.
///
/// The old hand-kept expectation array (23 names at PR #1426, the shape that
/// issue's item was written against) is gone — `expected_capabilities`
/// derives from the oracle itself — which is what makes the equality checks
/// the sync mechanism instead of a second list to keep in step.
fn assert_ts_capability_subset(expected: &[&str]) {
    for state in ["emptyStorage", "missingStorage"] {
        let ts_capabilities = oracle_capabilities(state);
        negative_assert!(
            expected
                .iter()
                .all(|capability| ts_capabilities.contains(&json!(capability))),
            "#1231 wired capabilities are TS subset (contract.{state})"
        );
    }
}

/// #1352: `hello_ok.serverInfo` and the `describe_server` result must be the
/// same value. The connection already builds one from the other
/// (`runtime/connection/mod.rs:189` — `describe_server()` is awaited once and
/// inserted as `serverInfo`), so this test fails the moment someone composes a
/// second list for the hello path and the two advertisements drift apart.
#[tokio::test]
async fn hello_reports_the_same_capabilities_as_describe_server() {
    let (_scratch, requests) = setup();
    let options = requests.options.clone();
    requests
        .project
        .dispose()
        .await
        .expect("#1352 dispose unused test registry");
    requests
        .owner
        .shutdown()
        .await
        .expect("#1352 shutdown unused test owner");
    let host = assembly::start(options.clone(), requests.env.clone())
        .await
        .expect("#1352 native host boot");
    let text = Arc::new(Text::default());
    let connection = host.connect(text.clone(), "token".into(), "fixture".into());
    connection
        .receive(json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"fixture"}).to_string())
        .await
        .expect("#1352 hello");
    let server_info = {
        let frames = text.0.lock().expect("#1352 hello frames");
        frames
            .iter()
            .find(|frame| frame["type"] == "hello_ok")
            .expect("#1352 hello_ok frame")["serverInfo"]
            .clone()
    };
    connection
        .receive(json!({"type":"request","id":"describe","method":"describe_server"}).to_string())
        .await
        .expect("#1352 describe request");
    let described = {
        let frames = text.0.lock().expect("#1352 describe frames");
        let response = frames
            .iter()
            .find(|frame| frame["id"] == "describe")
            .expect("#1352 describe response");
        negative_assert_eq!(response["ok"], true, "#1352 describe success");
        response["result"].clone()
    };
    negative_assert_eq!(
        server_info,
        described,
        "#1352 hello_ok.serverInfo equals the describe_server result"
    );
    negative_assert!(
        server_info["capabilities"]
            .as_array()
            .is_some_and(|names| !names.is_empty()),
        "#1352 hello_ok carries a non-empty capability array"
    );
    host.shutdown(options.drain_budget)
        .await
        .expect("#1352 native host shutdown");
}

/// #1869 item 3: the advertised half used to assert only "not
/// `CAPABILITY_UNAVAILABLE`", which is also true for a renamed dispatch arm
/// (`gateway_request_failed`) or a delegate that answers a degenerate
/// payload. Each published method therefore has to answer its own success
/// shape; an unknown method is a failure, not a pass, so a new entry in
/// `METHODS` cannot slip through the guard with a vacuous assertion.
///
/// Only the `resolve_settings` arm is reachable today: the production
/// capability table publishes exactly that one settings method, so the other
/// three arms are fail-closed placeholders that switch on when a later change
/// (e.g. #1671) publishes them. They are assertions, not dead code — a wrong
/// key name stays silent only until the method is published, and then the
/// guard reds instead of passing hollowly.
fn settings_success_shape(method: &str, value: &JsValue) -> bool {
    let array = |key: &str| matches!(value.get(key), Some(JsValue::Array(_)));
    let non_empty_array =
        |key: &str| matches!(value.get(key), Some(JsValue::Array(items)) if !items.is_empty());
    let non_empty_string =
        |key: &str| matches!(value.get(key), Some(JsValue::String(text)) if !text.is_empty());
    let number = |key: &str| matches!(value.get(key), Some(JsValue::Number(_)));
    match method {
        // `toGatewayResolvedSettings`: the full redacted projection.
        "resolve_settings" => {
            number("schemaVersion")
                && number("version")
                && non_empty_string("loadedAt")
                && non_empty_string("contentHash")
                && matches!(value.get("config"), Some(JsValue::Object(_)))
                && non_empty_array("sources")
        }
        // `{applied, cleared}`.
        "apply_flag_settings" => array("applied") && array("cleared"),
        // `{applied, cleared, changedPaths}`.
        "update_settings" => array("applied") && array("cleared") && array("changedPaths"),
        // `{reloaded: true, changed}`.
        "reload_output_styles" => {
            matches!(value.get("reloaded"), Some(JsValue::Bool(true))) && array("changed")
        }
        _ => false,
    }
}

/// #1320「公布 ⟺ 真能服务」三口径，三者有意不同：
/// - `已注册`（`methods/settings.rs::METHODS` 六项）：路由表会分发的
///   wire 名，能力守卫的完整输入域。
/// - `已公布`（`describe_server` capabilities，须为 TS 子集）：TS 在每个
///   配置状态只公布四项（apply_flag_settings / update_settings /
///   resolve_settings / reload_output_styles，见 oracle.json 三个 contract
///   的 capabilities），本固件在六项里只公布 `resolve_settings`——TS 公布
///   的其余三项没有委托，公布即承诺无法交付。
/// - `真能服务`（六项中 `resolve_settings` / `reload_config` /
///   `reload_extensions` 三项有真委托）：两个 reload 有委托但 TS 从不
///   公布它们，公布会违反 TS 子集口径。
/// 守卫循环因此只跑 TS 公布的四项：交集必须经 `Requests::settings_request`
/// 真的可服务（#1869 起还要回答该方法的成功形状，见
/// `settings_success_shape`——只排 `CAPABILITY_UNAVAILABLE` 会被改名的分发臂
/// 或空壳委托蒙混过去），差集必须回答
/// CAPABILITY_UNAVAILABLE "<method> is unavailable."；两个 reload 在循环外
/// 显式断言「真能服务且不在能力表」，证明不公布是有意的。settings 网关换成
/// 生产组合（`services::native_settings_for_process`）——`stub_settings`
/// 连 resolve_settings 都不可用，用它自证毫无意义。
///
/// #1761：两个名单不再手抄，从单一来源 `runtime::settings::METHODS` 二分派生
/// （`METHODS ∩ TS 公布` = 四项，残留 = 两个 reload），并用 `METHODS.len() == 6`
/// 锚定「六项」口径；两个 reload 断言正向成功形状 `{reloaded: true,
/// changedPaths: [...]}` 而不是只排 `CAPABILITY_UNAVAILABLE`——委托缺失时
/// 网关回答的是 `Ok({reloaded: false, reason: "unsupported"})`，排它恒真。
#[tokio::test]
async fn settings_capabilities_match_served_delegates() {
    let (_scratch, mut requests) = setup();
    let native = services::native_settings_for_process(
        requests.owner.clone(),
        requests.project.clone(),
        &requests.options.home,
        &requests.options.project,
        &requests.env,
    )
    .expect("#1320 the production settings stack must compose over the fixture");
    requests.settings = native.gateway;
    let description = requests
        .describe_server()
        .await
        .expect("#1320 describe_server succeeds");
    let advertised: Vec<String> = match description.get("capabilities") {
        Some(JsValue::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_string()
                    .and_then(|item| item.to_utf8().ok())
                    .expect("#1320 capability entries are strings")
            })
            .collect(),
        _ => panic!("#1320 describe_server must carry a capabilities array"),
    };
    // #1761: one source of truth for both halves of the loop. `METHODS` is the
    // registry table the guard is about; the TS contract decides which of its
    // entries are published, so neither list below is hand-copied. A method
    // added to or dropped from `METHODS` therefore changes this test instead of
    // silently escaping it (the count anchor pins the "six" claim the doc
    // comment and #1320 report).
    // #1869 item 4: the partition input used to be only the default
    // `contract` state, while the claims it feeds ("TS publishes four of the
    // six in every config state", the reloads unpublished in all of them)
    // are stated over all three. Assert the premise instead of trusting it:
    // a state that publishes a different settings capability set must fail
    // here rather than silently escape the partition below (the empty-
    // storage/missing-storage ORACLE mutants are its standing witnesses).
    let ts_capabilities: Vec<&str> = oracle_capabilities("contract")
        .iter()
        .map(|capability| {
            capability
                .as_str()
                .expect("#1320 TS capability entries are strings")
        })
        .collect();
    for state in ["emptyStorage", "missingStorage"] {
        let state_capabilities: Vec<&str> = oracle_capabilities(state)
            .iter()
            .map(|capability| {
                capability
                    .as_str()
                    .expect("#1320 TS capability entries are strings")
            })
            .collect();
        negative_assert!(
            state_capabilities.len() == ts_capabilities.len()
                && ts_capabilities
                    .iter()
                    .all(|capability| state_capabilities.contains(capability)),
            "#1869 the {state} state must publish the same settings capability \
             set as the default contract state: {state_capabilities:?} vs {ts_capabilities:?}"
        );
    }
    let methods = crate::runtime::settings::METHODS;
    negative_assert_eq!(
        methods.len(),
        6,
        "#1761 the settings registry table must stay six methods"
    );
    let (published, reloads): (Vec<&str>, Vec<&str>) =
        methods.iter().copied().partition(|method| {
            ts_capabilities
                .iter()
                .any(|capability| capability == method)
        });
    // TS publishes four of the six in every config state; the residue is the
    // two reloads this guard is here to defend. If the split ever changes, the
    // assertion below fails instead of quietly shrinking the guard.
    negative_assert_eq!(
        reloads,
        vec!["reload_config", "reload_extensions"],
        "#1761 the TS-unpublished settings methods must be the two reloads"
    );
    for &method in &published {
        let result = requests
            .settings_request(method.to_owned(), JsValue::Undefined)
            .await;
        if advertised.iter().any(|name| name == method) {
            // #1869 item 3: a non-CAPABILITY_UNAVAILABLE answer is not proof
            // the method was served — a renamed dispatch arm answers
            // `gateway_request_failed`, and a hollow delegate answers an
            // empty payload. Require the method's own success shape instead.
            let value = match result {
                Ok(value) => value,
                Err(error) => panic!(
                    "#1869 advertised settings method {method} must answer its success \
                     shape, got {}: {}",
                    error.code,
                    String::from_utf16_lossy(error.message.units())
                ),
            };
            negative_assert!(
                settings_success_shape(method, &value),
                "#1869 advertised settings method {method} must answer its success shape, \
                 not just any non-CAPABILITY_UNAVAILABLE payload, got: {value:?}"
            );
        } else {
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!(
                    "#1320 unadvertised settings method {method} must answer \
                     CAPABILITY_UNAVAILABLE"
                ),
            };
            negative_assert_eq!(
                error.code,
                "CAPABILITY_UNAVAILABLE",
                "#1320 unadvertised settings method {method} code"
            );
            negative_assert_eq!(
                String::from_utf16_lossy(error.message.units()),
                format!("{method} is unavailable."),
                "#1320 unadvertised settings method {method} message"
            );
        }
    }
    // The two reloads close the three-way gap: registered (METHODS) and
    // really served (real refresh delegates in the production composition),
    // yet deliberately unadvertised — TS never publishes them in any config
    // state, and the advertised set must stay a TS subset.
    //
    // #1761: the assertion is the success SHAPE, not the absence of one error
    // code. A missing delegate answers `Ok({reloaded: false, reason:
    // "unsupported"})` (runtime/settings/gateway.rs unsupported()), and a
    // renamed dispatch arm answers `gateway_request_failed`; neither carries
    // CAPABILITY_UNAVAILABLE, so the old `!matches!(.. CAPABILITY_UNAVAILABLE)`
    // was true for both regressions.
    for &method in &reloads {
        let result = requests
            .settings_request(method.to_owned(), JsValue::Undefined)
            .await;
        let value = match result {
            Ok(value) => value,
            Err(error) => panic!(
                "#1761 registered settings method {method} has a real delegate and must \
                 answer the reload success shape, got {}: {}",
                error.code,
                String::from_utf16_lossy(error.message.units())
            ),
        };
        negative_assert!(
            matches!(value.get("reloaded"), Some(JsValue::Bool(true))),
            "#1761 settings method {method} must answer reloaded: true from its real \
             delegate over the production stack, got: {value:?}"
        );
        negative_assert!(
            matches!(value.get("changedPaths"), Some(JsValue::Array(_))),
            "#1761 settings method {method} must answer a changedPaths array, got: {value:?}"
        );
        negative_assert!(
            !advertised.iter().any(|name| name == method),
            "#1320 settings method {method} must stay unadvertised: TS never publishes it, \
             so advertising it would break the TS-subset capability rule"
        );
    }
}

#[tokio::test]
async fn production_entry_snapshot_capabilities_are_disabled() {
    let (_scratch, requests) = setup();
    // Follow run()'s CLI/environment path, rather than injecting storage into
    // assembly::start as the snapshot success fixtures intentionally do.
    let options = Options::parse([
        "--home".into(),
        requests.options.home.clone().into_os_string(),
        "--project".into(),
        requests.options.project.clone().into_os_string(),
        "--listen".into(),
        "127.0.0.1:0".into(),
        "--token-file".into(),
        requests.options.token_file.clone().into_os_string(),
        "--sidecar".into(),
        requests.options.sidecar.clone().into_os_string(),
    ])
    .expect("#1231 production entry parses supported CLI options");
    requests
        .owner
        .shutdown()
        .await
        .expect("#1231 production entry unused owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1231 production entry unused registry disposal");
    for key in ["PILOTDECK_STORAGE_ROOT", "PILOTDECK_SNAPSHOT_ROOT"] {
        for value in ["/explicit/storage", "   "] {
            let env = Env::from([(key.into(), value.into())]);
            let error = validate_environment(&env)
                .expect_err("#1231 production entry rejects inherited snapshot authority");
            negative_assert_eq!(
                error.to_string(),
                format!("native isolated mode does not accept inherited {key}"),
                "#1231 production entry rejects named inherited snapshot authority"
            );
        }
    }
    for storage in [None, Some("")] {
        for snapshot in [None, Some("")] {
            let mut inherited = Env::new();
            for (key, value) in [
                ("PILOTDECK_STORAGE_ROOT", storage),
                ("PILOTDECK_SNAPSHOT_ROOT", snapshot),
            ] {
                if let Some(value) = value {
                    inherited.insert(key.into(), value.into());
                }
            }
            validate_environment(&inherited)
                .expect("#1231 production entry accepts absent or empty snapshot authority");
            let host = assembly::start(options.clone(), options.environment(inherited))
                .await
                .expect("#1231 production entry starts without snapshots");
            let text = Arc::new(Text::default());
            let connection = host.connect(text.clone(), "token".into(), "fixture".into());
            connection.receive(json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"fixture"}).to_string()).await.expect("#1231 production entry hello");
            connection
                .receive(
                    json!({"type":"request","id":"describe","method":"describe_server"})
                        .to_string(),
                )
                .await
                .expect("#1231 production entry describe request");
            {
                let frames = text
                    .0
                    .lock()
                    .expect("#1231 production entry describe frames");
                let response = frames
                    .iter()
                    .find(|frame| frame["id"] == "describe")
                    .expect("#1231 production entry describe response");
                negative_assert_eq!(
                    response["ok"],
                    true,
                    "#1231 production entry describe success"
                );
                let mut capabilities: Vec<&str> = response["result"]["capabilities"]
                    .as_array()
                    .expect("#1231 production entry capabilities")
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .expect("#1231 production entry capability string")
                    })
                    .collect();
                capabilities.sort_unstable();
                let expected = expected_capabilities(false);
                negative_assert_eq!(
                    capabilities,
                    expected,
                    "#1231 production entry exact capability set"
                );
                assert_ts_capability_subset(&expected);
            }
            host.shutdown(options.drain_budget)
                .await
                .expect("#1231 production entry host shutdown");
        }
    }
}

/// #1323 boundary P2: `--storage-root` edge inputs. A non-UTF-8 value is a hard
/// parse error (never silently dropped); empty or whitespace-only values reach
/// `PILOTDECK_STORAGE_ROOT` but must still resolve to snapshots disabled.
fn storage_root_args(
    requests: &requests::Requests,
    root: std::ffi::OsString,
) -> Vec<std::ffi::OsString> {
    vec![
        "--home".into(),
        requests.options.home.clone().into_os_string(),
        "--project".into(),
        requests.options.project.clone().into_os_string(),
        "--listen".into(),
        "127.0.0.1:0".into(),
        "--token-file".into(),
        requests.options.token_file.clone().into_os_string(),
        "--sidecar".into(),
        requests.options.sidecar.clone().into_os_string(),
        "--storage-root".into(),
        root,
    ]
}

#[cfg(unix)]
#[tokio::test]
async fn storage_root_non_utf8_is_hard_parse_error() {
    use std::os::unix::ffi::OsStringExt;
    let (_scratch, requests) = setup();
    Options::parse(storage_root_args(&requests, "/ok/storage".into()))
        .expect("#1323 control: UTF-8 --storage-root parses");
    let error = Options::parse(storage_root_args(
        &requests,
        std::ffi::OsString::from_vec(vec![0xff, 0xfe]),
    ))
    .expect_err("#1323 non-UTF-8 --storage-root must be rejected");
    negative_assert_eq!(
        error.to_string(),
        "storage root is not UTF-8",
        "#1323 non-UTF-8 --storage-root is a hard parse error"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1323 non-utf8 owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1323 non-utf8 registry dispose");
}

#[tokio::test]
async fn storage_root_blank_values_keep_snapshots_disabled() {
    let (scratch, requests) = setup();
    let real = scratch.0.join("home/storage");
    std::fs::create_dir_all(&real).expect("#1323 blank control storage root");
    let real = real
        .to_str()
        .expect("#1323 blank control path UTF-8")
        .to_owned();
    for (value, enabled) in [("", false), ("   ", false), (real.as_str(), true)] {
        let options = Options::parse(storage_root_args(&requests, value.into()))
            .expect("#1323 blank --storage-root parses");
        negative_assert_eq!(
            options.storage_root.as_deref(),
            Some(value),
            "#1323 --storage-root value is stored verbatim"
        );
        let env = options.environment(crate::config::Env::new());
        let config = crate::storage::resolve_workspace_snapshot_config(|key| env.get(key).cloned())
            .expect("#1323 blank --storage-root resolves");
        negative_assert_eq!(
            config.is_some(),
            enabled,
            "#1323 --storage-root {value:?}: snapshot config enabled only for a real root"
        );
    }
    requests
        .owner
        .shutdown()
        .await
        .expect("#1323 blank owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1323 blank registry dispose");
}

#[tokio::test]
async fn storage_root_cli_arg_wires_snapshot_capabilities() {
    // Pins the CLI parse path: Options::parse(["--storage-root", path]) stores
    // storage_root as Some(string); environment() injects PILOTDECK_STORAGE_ROOT
    // via the Some branch. No manual env.insert is used; the test solely exercises
    // the --storage-root → Options::storage_root → environment() chain and asserts
    // that describe_server advertises the full snapshot-enabled capability set.
    let (scratch, requests) = setup();
    let storage_path = scratch.0.join("home/storage");
    std::fs::create_dir_all(&storage_path).expect("#1323 storage root directory");
    let options = Options::parse([
        "--home".into(),
        requests.options.home.clone().into_os_string(),
        "--project".into(),
        requests.options.project.clone().into_os_string(),
        "--listen".into(),
        "127.0.0.1:0".into(),
        "--token-file".into(),
        requests.options.token_file.clone().into_os_string(),
        "--sidecar".into(),
        requests.options.sidecar.clone().into_os_string(),
        "--storage-root".into(),
        storage_path.clone().into_os_string(),
    ])
    .expect("#1323 Options::parse with --storage-root");
    negative_assert_eq!(
        options.storage_root.as_deref(),
        Some(storage_path.to_str().expect("#1323 storage path UTF-8")),
        "#1323 --storage-root sets Options::storage_root"
    );
    let env = options.environment(crate::config::Env::new());
    negative_assert_eq!(
        env.get("PILOTDECK_STORAGE_ROOT").map(String::as_str),
        Some(storage_path.to_str().expect("#1323 storage path UTF-8 env")),
        "#1323 environment() injects PILOTDECK_STORAGE_ROOT from --storage-root"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1323 dispose unused owner");
    requests
        .project
        .dispose()
        .await
        .expect("#1323 dispose unused registry");
    let host = assembly::start(options.clone(), env)
        .await
        .expect("#1323 assembly::start with --storage-root");
    let text = Arc::new(Text::default());
    let connection = host.connect(text.clone(), "token".into(), "fixture".into());
    connection
        .receive(
            json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"fixture"})
                .to_string(),
        )
        .await
        .expect("#1323 hello frame");
    connection
        .receive(json!({"type":"request","id":"describe","method":"describe_server"}).to_string())
        .await
        .expect("#1323 describe_server request");
    let description = {
        let frames = text.0.lock().expect("#1323 frames lock");
        let response = frames
            .iter()
            .find(|frame| frame["id"] == "describe")
            .expect("#1323 describe_server response");
        negative_assert_eq!(response["ok"], true, "#1323 describe_server success");
        response["result"].clone()
    };
    let mut capabilities: Vec<&str> = description["capabilities"]
        .as_array()
        .expect("#1323 capabilities array")
        .iter()
        .map(|v| v.as_str().expect("#1323 capability string"))
        .collect();
    capabilities.sort_unstable();
    let expected = expected_capabilities(true);
    negative_assert_eq!(
        capabilities,
        expected,
        "#1323 --storage-root CLI path advertises snapshot-enabled capabilities"
    );
    // See the note at the other snapshots-enabled call site: exact equality is
    // the whole guard here, and the disabled contracts are not this state.
    host.shutdown(options.drain_budget)
        .await
        .expect("#1323 host shutdown");
}

/// #1431 P2-1/P2-4: the submit-side wiring (`Services::workspace_id` +
/// `Services::snapshots`) and the management `snapshot_list` route must agree
/// on one workspace id, and a `.cwd` marker spelled differently from the
/// process cwd must still select the registered id over the legacy path slug.
#[tokio::test]
async fn submit_snapshot_wiring_matches_management_workspace_id() {
    use crate::runtime::turn::submit_host::SubmitHostServices;
    let (scratch, requests) = setup();
    let storage = requests.options.home.join("storage");
    std::fs::create_dir_all(&storage).expect("#1431 storage root");
    // Same project, different spelling: `resolve` folds `..` and the trailing
    // slash, so the marker branch must still win over the legacy path slug.
    std::fs::write(
        requests.options.home.join("projects/a8-main/.cwd"),
        format!("{}/../project/", requests.options.project.display()),
    )
    .expect("#1431 respelled marker");
    let options = Options::parse(storage_root_args(
        &requests,
        storage.clone().into_os_string(),
    ))
    .expect("#1431 --storage-root parses");
    let env = options.environment(crate::config::Env::new());
    // setup() staged the project from `PILOTDECK_STORAGE_ROOT=<home>/storage`;
    // `--storage-root` must resolve to that same root, or this test would
    // capture into a tree the management gateway never reads.
    negative_assert_eq!(
        env.get("PILOTDECK_STORAGE_ROOT"),
        Some(&storage.to_string_lossy().into_owned()),
        "#1431 --storage-root selects the staged root"
    );
    // Take the recorder from the production staging helpers instead of
    // building one by hand: they acquire the staged project registry and
    // extract the provider from `PersistenceResources`, so a regression in
    // that extraction cannot leave this test green. (They do not re-resolve
    // `--storage-root` themselves — staging did that when `setup()` registered
    // the project, and the assertions above pin the root the CLI selects.)
    // Handing that recorder to `Services` inside `assembly` is *not* driven
    // here — this test composes the `Services` literal itself and never starts
    // a real submit turn; that last hop still has no end-to-end control
    // (#1431).
    let enabled =
        crate::native_process::assembly::snapshot::snapshot_enabled(&options, &requests.project)
            .await
            .expect("#1431 staged snapshot state");
    negative_assert!(enabled, "#1431 --storage-root enables snapshots");
    let recorder = crate::native_process::assembly::snapshot::snapshot_recorder(
        &options,
        &requests.project,
        enabled,
    )
    .await
    .expect("#1431 staged recorder")
    .expect("#1431 staged snapshot recorder");
    let services = crate::native_process::services::Services {
        options: options.clone(),
        project: requests.project.clone(),
        env,
        recorders: Arc::default(),
        owner: requests.owner.clone(),
        snapshot_recorder: Some(recorder),
        upload_store: Arc::new(crate::upload::UploadStore::new(
            crate::upload::UploadStoreOptions::new(
                |_| Ok(std::path::PathBuf::new()),
                || Ok(Vec::new()),
            ),
        )),
    };
    let project_key = options.project.to_string_lossy().into_owned();
    let workspace = services
        .workspace_id(&expand(
            &json!({"projectKey": project_key.clone()}),
            &scratch.0,
        ))
        .expect("#1431 submit workspace id")
        .expect("#1431 submit snapshots enabled");
    negative_assert_eq!(
        workspace.to_utf8().expect("#1431 workspace id UTF-8"),
        "a8-main",
        "#1431 marker spelling selects the registered workspace id"
    );
    // The legacy slug differs, so the assertion above only passes through the
    // marker branch.
    negative_assert!(
        crate::metadata::project_id(&crate::metadata::resolve(
            std::path::Path::new(""),
            &project_key
        )) != "a8-main",
        "#1431 legacy slug differs from the registered id"
    );
    let snapshots = services
        .snapshots()
        .expect("#1431 snapshots() exposes the recorder");
    let result = snapshots
        .capture(
            crate::storage::WorkspaceSnapshotInput {
                workspace_id: workspace.to_utf8().expect("#1431 capture workspace id"),
                session_id: "a8-session".into(),
                turn_id: "turn-1".into(),
                run_id: "run-1".into(),
                workspace_dir: project_key.clone(),
                workspace_stable: Some(true),
                failure_kind: None,
                failure_reason: None,
            },
            true,
        )
        .await
        .expect("#1431 pre_user capture");
    negative_assert_eq!(result.phase.as_str(), "pre_user", "#1431 pre_user phase");
    negative_assert_eq!(
        result.state,
        crate::storage::SnapshotState::Committed,
        "#1431 capture committed"
    );
    let manifest = std::path::PathBuf::from(
        result
            .manifest_path
            .as_deref()
            .expect("#1431 manifest path"),
    );
    let expected = storage
        .canonicalize()
        .expect("#1431 canonical storage")
        .join("workspaces/a8-main/sessions/a8-session/snapshots")
        .join(&result.snapshot_id)
        .join("manifest.json");
    negative_assert_eq!(
        manifest,
        expected,
        "#1431 manifest lands under the registered workspace id"
    );
    negative_assert!(expected.is_file(), "#1431 manifest exists on disk");
    let listed = json_value(
        &requests
            .request(
                "snapshot_list".into(),
                expand(&json!({"projectKey": project_key}), &scratch.0),
            )
            .await
            .expect("#1431 snapshot_list"),
    );
    negative_assert!(
        listed["items"]
            .as_array()
            .expect("#1431 snapshot items")
            .iter()
            .any(|item| item["snapshotId"] == json!(result.snapshot_id)),
        "#1431 snapshot_list returns the submit-captured snapshot"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1431 owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1431 registry dispose");
}

/// #1431 item 8, second half: the submit-side snapshot wiring end to end
/// through the real assembly — `--storage-root` → `assembly::start` → a real
/// `submit_turn` → the pre_user capture the production submit path itself
/// drives → the manifest on disk under the registered workspace id →
/// `snapshot_list` on the same booted host returns it. The wiring test above
/// composes the `Services` literal by hand, so `snapshots()` returning `None`
/// or the assembly forgetting to hand its recorder to `Services` left every
/// existing test green while production turns silently landed no snapshot.
#[tokio::test]
async fn a_real_submit_turn_lands_its_snapshot_under_the_storage_root() {
    let (_scratch, requests) = setup();
    let storage = requests.options.home.join("storage");
    std::fs::create_dir_all(&storage).expect("#1431 storage root");
    let options = Options::parse(storage_root_args(
        &requests,
        storage.clone().into_os_string(),
    ))
    .expect("#1431 --storage-root parses");
    let env = options.environment(crate::config::Env::new());
    requests
        .owner
        .shutdown()
        .await
        .expect("#1431 dispose unused fixture owner");
    requests
        .project
        .dispose()
        .await
        .expect("#1431 dispose unused fixture registry");
    let host = assembly::start(options.clone(), env)
        .await
        .expect("#1431 assembly::start with --storage-root");
    let text = Arc::new(Text::default());
    let connection = host.connect(text.clone(), "token".into(), "fixture".into());
    connection
        .receive(json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"fixture"}).to_string())
        .await
        .expect("#1431 hello");
    // A real turn. The fixture model URL refuses connections, so the turn
    // fails — but the gateway's own preflight emits `input_accepted` before
    // the model is reached, and that event is the production trigger of the
    // pre_user capture (runtime/turn/submit.rs). A recorder wiring regression
    // makes this turn land no snapshot at all, and the poll below fails
    // instead of the turn's own failure.
    tokio::time::timeout(
        Duration::from_secs(30),
        connection.receive(
            json!({"type":"request","id":"turn","method":"submit_turn","params":{"sessionKey":"session","runId":"run","message":"hello","channelKey":"test"}})
                .to_string(),
        ),
    )
    .await
    .expect("#1431 the submit_turn response settles (the turn may fail, the response may not hang)")
    .expect("#1431 submit_turn frame");
    let snapshots = storage
        .canonicalize()
        .expect("#1431 canonical storage")
        .join("workspaces/a8-main/sessions/session/snapshots");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let manifests: Vec<std::path::PathBuf> = loop {
        let found: Vec<_> = std::fs::read_dir(&snapshots)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path().join("manifest.json"))
                    .filter(|path| path.is_file())
                    .collect()
            })
            .unwrap_or_default();
        if !found.is_empty() {
            break found;
        }
        negative_assert!(
            tokio::time::Instant::now() < deadline,
            "#1431 a real submit turn must land its pre_user snapshot under the \
             --storage-root: {} stays empty — the assembly-to-Services recorder \
             wiring regressed",
            snapshots.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    // The manifest the turn wrote names the turn's own ids, so a hand-planted
    // manifest from another path cannot satisfy the poll above.
    for manifest_path in &manifests {
        let manifest: Value = serde_json::from_str(
            &std::fs::read_to_string(manifest_path).expect("#1431 manifest bytes"),
        )
        .expect("#1431 manifest JSON");
        negative_assert_eq!(
            manifest["workspaceId"].as_str(),
            Some("a8-main"),
            "#1431 the turn's manifest records the registered workspace id"
        );
        negative_assert_eq!(
            manifest["sessionId"].as_str(),
            Some("session"),
            "#1431 the turn's manifest records its own session key"
        );
        negative_assert_eq!(
            manifest["runId"].as_str(),
            Some("run"),
            "#1431 the turn's manifest records its own run id"
        );
    }
    // Reverse lookup through the management route of the same booted host.
    connection
        .receive(
            json!({"type":"request","id":"list","method":"snapshot_list","params":{"projectKey": options.project.to_string_lossy()}}).to_string(),
        )
        .await
        .expect("#1431 snapshot_list frame");
    let listed = {
        let frames = text.0.lock().expect("#1431 frames lock");
        let response = frames
            .iter()
            .find(|frame| frame["id"] == "list")
            .expect("#1431 snapshot_list response frame");
        negative_assert_eq!(response["ok"], true, "#1431 snapshot_list succeeds");
        response["result"].clone()
    };
    let listed_ids: Vec<&str> = listed["items"]
        .as_array()
        .expect("#1431 snapshot_list items array")
        .iter()
        .filter_map(|item| item["snapshotId"].as_str())
        .collect();
    for manifest_path in &manifests {
        let manifest: Value = serde_json::from_str(
            &std::fs::read_to_string(manifest_path).expect("#1431 manifest bytes"),
        )
        .expect("#1431 manifest JSON");
        let snapshot_id = manifest["snapshotId"].as_str().expect("#1431 snapshot id");
        negative_assert!(
            listed_ids.contains(&snapshot_id),
            "#1431 snapshot_list on the booted host returns the snapshot the real \
             submit turn landed: {} not in {listed_ids:?}",
            snapshot_id
        );
    }
    host.shutdown(options.drain_budget)
        .await
        .expect("#1431 host shutdown after the end-to-end turn");
}

#[tokio::test]
async fn snapshot_success_matches_ts() {
    let (scratch, requests) = setup();
    let oracle: Value = serde_json::from_str(ORACLE).expect("#1231 snapshot oracle");
    let fixture = &oracle["snapshotSuccess"];
    seed_files(
        &requests.options.home.join("storage"),
        &fixture["storageFiles"],
        &scratch.0,
    );
    for row in fixture["requests"]
        .as_array()
        .expect("#1231 snapshot requests")
    {
        negative_assert_eq!(
            call(&requests, &row["method"], &row["params"], &scratch.0).await,
            row["result"],
            "#1231 snapshot get success and workspace filter"
        );
    }
    let restore = &fixture["restore"];
    let mut result = call(
        &requests,
        &json!("snapshot_restore"),
        &restore["params"],
        &scratch.0,
    )
    .await;
    negative_assert_eq!(
        result["ok"]["restored"],
        true,
        "#1231 snapshot restore success"
    );
    let path = PathBuf::from(
        result["ok"]["workspaceKey"]
            .as_str()
            .expect("#1231 snapshot restored path")
            .replace("<root>", scratch.0.to_str().expect("#1231 snapshot root")),
    );
    negative_assert_eq!(
        path.parent(),
        Some(requests.options.project.as_path()),
        "#1231 snapshot restore target parent"
    );
    let name = path
        .file_name()
        .expect("#1231 snapshot restore name")
        .to_str()
        .expect("#1231 snapshot restore name UTF8");
    negative_assert!(
        name.strip_prefix(".pilotdeck-restore-")
            .is_some_and(|suffix| suffix.len() == 6
                && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())),
        "#1231 snapshot restore target name"
    );
    negative_assert_eq!(
        file_tree(&path),
        restore["files"],
        "#1231 snapshot restored file bytes"
    );
    result["ok"]["workspaceKey"] = json!("<root>/project/.pilotdeck-restore-<random>");
    negative_assert_eq!(
        result,
        restore["result"],
        "#1231 snapshot restore TS result"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1231 snapshot owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1231 snapshot registry disposal");
}

#[tokio::test]
async fn skill_import_success_conflict_and_overwrite_match_ts() {
    let (scratch, requests) = setup();
    let oracle: Value = serde_json::from_str(ORACLE).expect("#1231 import oracle");
    let fixture = &oracle["skillImport"];
    let source = scratch.0.join("imported-source");
    seed_files(&source, &fixture["sourceFiles"], &scratch.0);
    for step in fixture["steps"].as_array().expect("#1231 import steps") {
        if let Some(writes) = step.get("writes") {
            seed_files(&source, writes, &scratch.0);
        }
        negative_assert_eq!(
            call(&requests, &step["method"], &step["params"], &scratch.0).await,
            step["result"],
            "#1231 skill import read list scan and conflict results"
        );
        negative_assert_eq!(
            file_tree(&requests.options.project.join(".pilotdeck/skills/imported")),
            step["files"],
            "#1231 skill import copies files and preserves conflict target"
        );
    }
    requests
        .owner
        .shutdown()
        .await
        .expect("#1231 import owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1231 import registry disposal");
}

/// #1323 regression: `Services::workspace_id` must return `Some(id)` when
/// `snapshot_recorder` is wired (i.e. `--storage-root` was supplied), and
/// calling the recorder with that id must produce a committed `manifest.json`
/// on disk.  The mutation that broke this: returning `Ok(None)` from
/// `workspace_id()` regardless of `snapshot_recorder`.
///
/// To prove the test catches the bug, temporarily replace line
///   `Ok(Some(JsString::from(projects.workspace_id(&project).as_str())))`
/// with
///   `Ok(None)`
/// in `services.rs`; this test must then FAIL.  Restore the original and the
/// test passes.
#[tokio::test]
async fn snapshot_wiring_workspace_id_routes_to_recorder() {
    let (scratch, requests) = setup();
    let storage_root = scratch.0.join("snapshot-storage");
    std::fs::create_dir_all(&storage_root).expect("#1323 regression snapshot storage root");

    // Build a recorder backed by the temp directory.
    let recorder = std::sync::Arc::new(
        crate::storage::ContentAddressedWorkspaceSnapshotRecorder::new(
            crate::storage::WorkspaceSnapshotConfig {
                root: storage_root.to_string_lossy().into_owned(),
                snapshot_root: storage_root.to_string_lossy().into_owned(),
            },
        ),
    );

    // Build a Services with snapshot_recorder wired.
    let owner_options = crate::runtime::session::SessionOwnerOptions::new(std::sync::Arc::new(
        crate::native_process::management_tests::SnapshotFactory,
    ));
    let services = super::services::Services {
        options: requests.options.clone(),
        project: requests.project.clone(),
        env: requests.env.clone(),
        recorders: std::sync::Arc::default(),
        owner: crate::runtime::session::SessionOwner::new(owner_options),
        snapshot_recorder: Some(recorder.clone()),
        upload_store: std::sync::Arc::new(crate::upload::UploadStore::new(
            crate::upload::UploadStoreOptions::new(
                |_: &pilotdeck_protocol::JsString| {
                    Err(crate::upload::UploadError::dialog(
                        "PROJECT_NOT_FOUND",
                        "unavailable in test",
                    ))
                },
                || Ok(vec![]),
            ),
        )),
    };

    // workspace_id() MUST return Some when snapshot_recorder is Some.
    use crate::runtime::turn::submit_host::SubmitHostServices as _;
    let dummy_input = pilotdeck_protocol::parse_json("{}").expect("#1323 dummy input");
    let workspace_id = services
        .workspace_id(&dummy_input)
        .expect("#1323 workspace_id must succeed")
        .expect("#1323 workspace_id must be Some when snapshot_recorder is wired");

    // The returned id must be non-empty and must not be empty or all dashes.
    let id_str = workspace_id
        .to_utf8()
        .expect("#1323 workspace_id must be valid UTF-8");
    assert!(
        !id_str.is_empty(),
        "#1323 workspace_id string must not be empty"
    );
    assert_ne!(
        id_str.trim_matches('-'),
        "",
        "#1323 workspace_id must not be all dashes"
    );

    // Capture a pre-user snapshot using the id returned above.  The workspace
    // dir is the project directory which already exists.
    let workspace_dir = requests.options.project.to_string_lossy().into_owned();
    let input = crate::storage::WorkspaceSnapshotInput {
        workspace_id: id_str.clone(),
        session_id: "regression-session".into(),
        turn_id: "regression-turn".into(),
        run_id: "regression-run".into(),
        workspace_dir,
        workspace_stable: None,
        failure_kind: None,
        failure_reason: None,
    };
    let result = recorder
        .capture_pre_user(&input)
        .expect("#1323 capture_pre_user must succeed");
    assert_eq!(
        result.state,
        crate::storage::SnapshotState::Committed,
        "#1323 snapshot must be committed"
    );

    // The manifest file must exist on disk.
    let manifest_path = result
        .manifest_path
        .expect("#1323 committed snapshot must have a manifest_path");
    assert!(
        std::path::Path::new(&manifest_path).is_file(),
        "#1323 manifest.json must exist on disk: {manifest_path}"
    );

    // The partition directory must be under the storage root.
    assert!(
        manifest_path.starts_with(storage_root.to_str().expect("#1323 storage root UTF-8")),
        "#1323 manifest must be inside --storage-root: {manifest_path}"
    );

    // The workspace subtree must be keyed by the workspace_id we computed.
    assert!(
        manifest_path.contains(&format!("/workspaces/{id_str}/")),
        "#1323 manifest path must contain workspaces/<id>/: {manifest_path}"
    );
}

/// #1323 boundary P1: the turn-side workspace id must follow TS
/// `resolveProjectStorageId` (registered `.cwd` marker first, legacy slug
/// otherwise) and agree with the management snapshot route, so a turn-captured
/// snapshot is listed under the same id. Mutation caught: computing the legacy
/// slug unconditionally (the id would be `<slug>`, not `a8-main`).
#[tokio::test]
async fn snapshot_workspace_id_prefers_registered_marker() {
    use crate::runtime::snapshot::{SnapshotProjectResolver as _, SnapshotProjects};
    use crate::runtime::turn::submit_host::SubmitHostServices as _;
    let (scratch, requests) = setup();
    let storage_root = scratch.0.join("snapshot-storage");
    let recorder = std::sync::Arc::new(
        crate::storage::ContentAddressedWorkspaceSnapshotRecorder::new(
            crate::storage::WorkspaceSnapshotConfig {
                root: storage_root.to_string_lossy().into_owned(),
                snapshot_root: storage_root.to_string_lossy().into_owned(),
            },
        ),
    );
    let services = super::services::Services {
        options: requests.options.clone(),
        project: requests.project.clone(),
        env: requests.env.clone(),
        recorders: std::sync::Arc::default(),
        owner: crate::runtime::session::SessionOwner::new(
            crate::runtime::session::SessionOwnerOptions::new(std::sync::Arc::new(SnapshotFactory)),
        ),
        snapshot_recorder: Some(recorder),
        upload_store: std::sync::Arc::new(crate::upload::UploadStore::new(
            crate::upload::UploadStoreOptions::new(
                |_: &pilotdeck_protocol::JsString| {
                    Err(crate::upload::UploadError::dialog(
                        "PROJECT_NOT_FOUND",
                        "unavailable in test",
                    ))
                },
                || Ok(vec![]),
            ),
        )),
    };
    let project = requests.options.project.to_string_lossy().into_owned();
    let input = pilotdeck_protocol::parse_json("{}").expect("#1323 marker input");
    let id = |services: &super::services::Services| {
        services
            .workspace_id(&input)
            .expect("#1323 marker workspace_id")
            .expect("#1323 marker workspace_id is Some")
            .to_utf8()
            .expect("#1323 marker workspace_id UTF-8")
    };

    // `setup` registers this project as `a8-main` (home/projects/a8-main/.cwd).
    let resolver = SnapshotProjects::new(requests.options.home.clone(), std::path::PathBuf::new());
    let registered = id(&services);
    assert_eq!(registered, "a8-main", "#1323 registered marker wins");
    assert_eq!(
        registered,
        resolver.workspace_id(&project),
        "#1323 turn and management snapshot routes agree (registered)"
    );

    // Without the marker the id falls back to the legacy slug.
    std::fs::remove_file(requests.options.home.join("projects/a8-main/.cwd"))
        .expect("#1323 remove marker");
    let legacy = id(&services);
    assert_eq!(
        legacy,
        crate::metadata::project_id(&requests.options.project),
        "#1323 unregistered project uses the legacy slug"
    );
    assert_eq!(
        legacy,
        resolver.workspace_id(&project),
        "#1323 turn and management snapshot routes agree (legacy)"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1323 marker shutdown");
}

/// #2562: `Services::workspace_id`'s projectKey gate reads the key the way the
/// TS submit path does — `InProcessGateway.ts:954` hands `input.projectKey` to
/// `workspaceIdResolver` and the local resolver is
/// `resolveProjectStorageId(requestedProjectKey ?? fallbackProjectRoot, pilotHome)`
/// (createLocalGateway.ts:1026-1028), so an explicit `projectKey: null` is
/// "not provided": it falls back to the configured project on both recorder
/// states instead of the pre-#2562 `projectKey must be a valid path string.`
/// error. Node v22.22.0 probes:
/// `_tmp/20261026-issue-2562/ts-probe/probe-{resolver,submit}.out` (null
/// behaves byte-identically to an omitted key end-to-end; a non-string 7
/// throws ERR_INVALID_ARG_TYPE, so the two controls below pin that the
/// nullish fix did not widen the gate for non-strings).
#[tokio::test]
async fn submit_workspace_id_treats_an_explicit_null_project_key_as_not_provided() {
    use crate::runtime::turn::submit_host::SubmitHostServices as _;
    let (scratch, requests) = setup();
    let storage_root = scratch.0.join("snapshot-storage");
    let recorder = std::sync::Arc::new(
        crate::storage::ContentAddressedWorkspaceSnapshotRecorder::new(
            crate::storage::WorkspaceSnapshotConfig {
                root: storage_root.to_string_lossy().into_owned(),
                snapshot_root: storage_root.to_string_lossy().into_owned(),
            },
        ),
    );
    let services = |recorder: Option<
        std::sync::Arc<crate::storage::ContentAddressedWorkspaceSnapshotRecorder>,
    >| {
        super::services::Services {
            options: requests.options.clone(),
            project: requests.project.clone(),
            env: requests.env.clone(),
            recorders: std::sync::Arc::default(),
            owner: crate::runtime::session::SessionOwner::new(
                crate::runtime::session::SessionOwnerOptions::new(std::sync::Arc::new(
                    SnapshotFactory,
                )),
            ),
            snapshot_recorder: recorder,
            upload_store: std::sync::Arc::new(crate::upload::UploadStore::new(
                crate::upload::UploadStoreOptions::new(
                    |_: &pilotdeck_protocol::JsString| {
                        Err(crate::upload::UploadError::dialog(
                            "PROJECT_NOT_FOUND",
                            "unavailable in test",
                        ))
                    },
                    || Ok(vec![]),
                ),
            )),
        }
    };
    let null_input =
        pilotdeck_protocol::parse_json(r#"{"projectKey":null}"#).expect("#2562 null input JSON");
    let omitted_input = pilotdeck_protocol::parse_json("{}").expect("#2562 omitted input JSON");

    // (1) Recorder on: the explicit null answers the configured project's id,
    // exactly like an omitted key.
    let on = services(Some(recorder));
    let null_id = on
        .workspace_id(&null_input)
        .expect("#2562 explicit null is not an error with the recorder on")
        .expect("#2562 explicit null answers an id with the recorder on")
        .to_utf8()
        .expect("#2562 the null-key id is UTF-8");
    let omitted_id = on
        .workspace_id(&omitted_input)
        .expect("#2562 an omitted key is not an error with the recorder on")
        .expect("#2562 an omitted key answers an id with the recorder on")
        .to_utf8()
        .expect("#2562 the omitted-key id is UTF-8");
    negative_assert_eq!(
        null_id,
        omitted_id,
        "#2562 a null projectKey falls back to the configured project like an omitted one"
    );

    // (2) Recorder off: the explicit null answers `None` like an omitted key.
    let off = services(None);
    let null_answer = off
        .workspace_id(&null_input)
        .expect("#2562 explicit null is not an error with the recorder off");
    negative_assert!(
        null_answer.is_none(),
        "#2562 a null projectKey answers None with the recorder off"
    );

    // (3) Non-string control: the nullish fix must not admit a number — the
    // gate still answers the type error both sides of the TS chain reject.
    let seven_input =
        pilotdeck_protocol::parse_json(r#"{"projectKey":7}"#).expect("#2562 seven input JSON");
    let error = on
        .workspace_id(&seven_input)
        .expect_err("#2562 a non-string projectKey still fails the gate");
    negative_assert_eq!(
        error.code,
        "gateway_submit_failed",
        "#2562 a non-string projectKey keeps the submit-failure code"
    );
    negative_assert_eq!(
        error.message,
        "projectKey must be a valid path string.",
        "#2562 a non-string projectKey keeps the type message"
    );

    // (4) Unserved string control: a foreign string is still refused, so the
    // fallback in (1) is the configured project, not "anything goes".
    let unserved_input = pilotdeck_protocol::parse_json(
        &serde_json::json!({"projectKey": "/nonexistent/pdrs-2562"}).to_string(),
    )
    .expect("#2562 unserved input JSON");
    let error = on
        .workspace_id(&unserved_input)
        .expect_err("#2562 an unserved project string is refused");
    negative_assert_eq!(
        error.code,
        "CAPABILITY_UNAVAILABLE",
        "#2562 an unserved project string keeps the isolated-project code"
    );
    negative_assert_eq!(
        error.message,
        "The isolated native process serves only its configured project.",
        "#2562 an unserved project string keeps the isolated-project message"
    );

    requests
        .owner
        .shutdown()
        .await
        .expect("#2562 test owner shutdown");
}

/// #1323 boundary P1: TS `/^-+|-+$/g` trims both ends of the legacy slug.
#[test]
fn legacy_project_id_trims_leading_dashes() {
    for (path, expected) in [("/-foo", "foo"), ("-foo-", "foo"), ("C:-foo", "foo")] {
        assert_eq!(
            crate::metadata::project_id(std::path::Path::new(path)),
            expected,
            "#1323 legacy slug of {path:?}"
        );
    }
}

/// Minimal SessionFactory stub used only by the regression test above.
pub(super) struct SnapshotFactory;
impl crate::runtime::session::SessionFactory for SnapshotFactory {
    fn create(
        &self,
        _: crate::runtime::session::SessionContext,
        _: Option<crate::runtime::session::LiveSessionHandle>,
    ) -> crate::runtime::refresh::BoxFuture<
        Result<crate::runtime::session::LiveSessionHandle, crate::runtime::project::RuntimeError>,
    > {
        Box::pin(std::future::ready(Err(
            crate::runtime::project::RuntimeError::new("snapshot factory stub"),
        )))
    }
}

struct ParkedMemoryRegistry {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
    released: Arc<std::sync::atomic::AtomicBool>,
}
impl crate::runtime::memory::ProjectMemoryRegistry for ParkedMemoryRegistry {
    fn resolve_project_key(
        &self,
        key: &JsString,
    ) -> Result<JsString, crate::runtime::memory::MemoryProviderError> {
        Ok(key.clone())
    }
    fn memory_management(
        &self,
        _: &JsString,
    ) -> Result<
        Option<Arc<dyn crate::runtime::memory::MemoryManagementService>>,
        crate::runtime::memory::MemoryProviderError,
    > {
        if let Some(entered) = self.entered.lock().expect("#1322 entered").take() {
            let _ = entered.send(());
        }
        let got = self
            .release
            .lock()
            .expect("#1322 release")
            .recv_timeout(Duration::from_secs(3))
            .is_ok();
        self.released
            .store(got, std::sync::atomic::Ordering::SeqCst);
        Ok(None)
    }
}

#[tokio::test]
async fn memory_management_request_disabled_gateway_returns_unavailable() {
    // Exercises the spawn_blocking branch in management_request for memory
    // methods: setup() uses MemoryGateway::disabled(), so memory_dispatch
    // returns None → CAPABILITY_UNAVAILABLE. Without this test the branch
    // had zero coverage in the #1231 suite (#1322 fix-r2 P1).
    let (scratch, requests) = setup();
    let params = expand(&json!({"projectKey": "<root>/project"}), &scratch.0);
    let result = requests.management_request("memory_list", &params).await;
    requests
        .owner
        .shutdown()
        .await
        .expect("#1322 owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1322 registry disposal");
    // #1456: the TS wrapper throws and `GatewayWsConnection` answers
    // `{ok:false,error:{code,message}}` (measured in
    // `_tmp/20261107-issue-1456/ts-baseline/probe.log` row
    // `capability-unavailable`), so the disabled gateway is a request failure,
    // not a successful body that describes the failure.
    let error = result
        .expect("#1322 memory_list must be a management method")
        .expect_err("#1456 a failed memory_list must not be an ok body");
    negative_assert_eq!(
        error.code.as_str(),
        "CAPABILITY_UNAVAILABLE",
        "#1322 memory_list via management_request with disabled gateway"
    );
    negative_assert_eq!(
        String::from_utf16_lossy(error.message.units()),
        "Gateway memory provider is unavailable.",
        "#1456 the thrown DialogGatewayError message reaches the wire"
    );
}

#[tokio::test]
async fn memory_management_typeerror_takes_the_node_fallback_code() {
    // The TS wrapper reads `input.projectKey` off the raw params object, so a
    // null/undefined params value throws a code-less TypeError and the WS
    // connection falls back to `gateway_request_failed` (probe rows
    // `params-typeerror`). A structured error keeps its own code; a code-less
    // one must not invent DialogGatewayError semantics.
    let (_scratch, requests) = setup();
    let result = requests
        .management_request("memory_list", &pilotdeck_protocol::JsValue::Undefined)
        .await;
    requests
        .owner
        .shutdown()
        .await
        .expect("#1456 owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1456 registry disposal");
    let error = result
        .expect("#1456 memory_list must be a management method")
        .expect_err("#1456 a TypeError must be a request failure");
    negative_assert_eq!(
        error.code.as_str(),
        "gateway_request_failed",
        "#1456 code-less TypeError takes the Node fallback code"
    );
    negative_assert_eq!(
        String::from_utf16_lossy(error.message.units()),
        "Cannot read properties of undefined (reading 'projectKey')",
        "#1456 TypeError message reaches the wire unchanged"
    );
}

#[tokio::test]
async fn pending_memory_io_does_not_block_serial_host() {
    let (scratch, mut requests) = setup();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
    requests.memory = Arc::new(crate::runtime::memory::gateway::MemoryGateway::new(
        Arc::new(ParkedMemoryRegistry {
            entered: Mutex::new(Some(entered)),
            release: Mutex::new(gate),
            released: released.clone(),
        }),
        requests.options.project.clone(),
    ));
    let params = json!({"projectKey":"<root>/project"});
    let parked = tokio::spawn({
        let requests = requests.clone();
        let params = expand(&params, &scratch.0);
        async move { requests.request("memory_list".into(), params).await }
    });
    ready.await.expect("#1322 memory IO entered");
    let independent = tokio::time::timeout(
        Duration::from_secs(2),
        requests.request("snapshot_list".into(), expand(&params, &scratch.0)),
    )
    .await;
    // Release before any assertion so the parked thread never outlives the test.
    release.send(()).expect("#1322 release memory IO");
    let _ = parked.await;
    requests
        .owner
        .shutdown()
        .await
        .expect("#1322 owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1322 registry disposal");
    negative_assert!(
        independent.is_ok_and(|result| result.is_ok())
            && released.load(std::sync::atomic::Ordering::SeqCst),
        "#1322 pending memory IO must not block host"
    );
}

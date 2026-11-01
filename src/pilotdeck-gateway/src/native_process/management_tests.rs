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
const UNWIRED_METHODS: [&str; 7] = [
    "memory_list",
    "memory_wipe",
    "mcp_server_status",
    "set_mcp_servers",
    "mcp_server_reconnect",
    "mcp_server_toggle",
    "set_mcp_permission_mode_override",
];
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
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Session;
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
}
struct Factory;
impl SessionFactory for Factory {
    fn create(
        &self,
        _: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        Box::pin(async { Ok(LiveSessionHandle::new(Arc::new(Session), None)) })
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
fn setup() -> (Scratch, requests::Requests) {
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
    let mut owner_options = SessionOwnerOptions::new(Arc::new(Factory));
    owner_options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(owner_options);
    let runs = Arc::new(
        crate::runtime::turn::publish::FileRegistry::new(
            crate::storage::FileRunRegistry::open(root.join("home/runs.json"))
                .expect("#1231 run file"),
        )
        .expect("#1231 run registry"),
    );
    (
        Scratch(root),
        requests::Requests {
            options,
            env,
            project,
            owner,
            coordinator: Arc::default(),
            runs,
        },
    )
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
            negative_assert_eq!(response["ok"], true, "#1231 production route success");
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
        let mut expected = [
            "run_get",
            "run_events",
            "run_reattach",
            "project_files_list",
            "project_file_read",
            "model_catalog_list",
            "session_model_get",
            "supported_agents",
            "snapshot_list",
            "snapshot_get",
            "snapshot_restore",
            "manager_sessions",
            "commands_list",
        ];
        expected.sort_unstable();
        negative_assert_eq!(capabilities, expected, "#1231 exact wired capability set");
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
    for method in UNWIRED_METHODS {
        negative_assert!(
            requests
                .management_request(method, &crate::metadata::object([]))
                .await
                .is_none(),
            "#1231 unwired family bypasses management"
        );
        let result = requests
            .request(method.into(), crate::metadata::object([]))
            .await;
        negative_assert_eq!(
            result.expect_err("#1231 unwired method must fail").code,
            "CAPABILITY_UNAVAILABLE",
            "#1231 unwired family rejected"
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
    let host = assembly::start(options.clone(), options.environment(env))
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
        let mut expected = [
            "run_get",
            "run_events",
            "run_reattach",
            "project_files_list",
            "project_file_read",
            "model_catalog_list",
            "session_model_get",
            "supported_agents",
            "manager_sessions",
            "commands_list",
        ];
        expected.sort_unstable();
        negative_assert_eq!(
            capabilities,
            expected,
            "#1231 disabled storage exact capability set"
        );
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

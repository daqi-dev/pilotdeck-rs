//! #1320: replay of the real TS native-settings oracle
//! (`tests/fixtures/native-settings/generate.mts`) through the settings
//! gateway layer. Every oracle row (21 `cases`, the `concurrency` /
//! `reconnect` / `secondConnection` probes, and the 6-row production
//! `matrix`) is replayed through [`Requests::settings_request`] — the
//! AfterAdmission layer the #1305 registry hands the six U7 methods to,
//! NOT the full `GatewayRequests::request` route: the isolation admission
//! gate and the registry lookup sit in front of it and are covered by the
//! routing tests at the bottom of this file — over the same
//! layers the TS generator drives: the real [`ConfigStore`] over real
//! scratch YAML trees, the real #1011 session owner, and the real
//! [`RefreshBundle`] with recording collaborators, one of them the settling
//! registry (`reload()` resolves on the first poll) that reproduces the TS
//! delegate's `setImmediate` settlement so the golden's full chain — log →
//! sdk dispatch → registry.reload() → settle → markAllDirty → broadcast —
//! is pinned, with the broadcast frame landing AFTER the reload response
//! and BEFORE it for the synchronous branches (reload_extensions,
//! runPolicy-only changes).
//!
//! Every row is compared on six cells: the reconstructed WS response
//! (`{ok, result}` / `{ok, error:{code, message}}`), the notification
//! frames the connection received before and after that response (split at
//! the response frame's position in the connection buffer, like the
//! generator's `splitFrames`), the ordered event stream, a follow-up
//! `resolve_settings` re-read (read and write share one store generation),
//! and the cumulative post state. Values are compared as normalized JSON
//! (ISO timestamps → `<ts>`, the scratch root → `<root>`), structural
//! equality via serde. The global request-id counter mirrors the
//! generator's `r{next++}` so the reconnect probe's `wire` cell — which
//! hard-codes `r45`/`r46`/`r48` — reproduces.
//!
//! The two `divergenceCases` (`changedPaths:[1,true,null]` /
//! `projectKey:5`, the registered divergence
//! `U7-EXTENSIONS-RELOAD-STRING-COERCION-001`) are recorded by the TS
//! golden but deliberately NOT replayed here while the ledger entry
//! stands; the count is guarded so a drifted oracle cannot silently drop
//! them.
//!
//! The mirror harness (probe, recording collaborators, scratch trees) is
//! shared with the #1318 component replay and lives in
//! `crate::runtime::settings::test_support`.
use super::Options;
use super::requests::Requests;
use super::services::{NativeSettings, NativeSettingsOptions, native_settings};
use crate::config::Env;
use crate::config::store::{ConfigStore, ReloadEvent, StoreOptions};
use crate::runtime::connection::RequestError;
use crate::runtime::memory::gateway::MemoryGateway;
use crate::runtime::project::{
    DisposeFuture, ProjectRuntimeFactory, ProjectRuntimeRegistry, ResourceBundle, RuntimeResources,
    RuntimeResult,
};
use crate::runtime::refresh::{
    BoxFuture, NotificationServer, RefreshBundle, RefreshOptions, RefreshRegistry, RegistryRefresh,
};
use crate::runtime::replacement::ReplacementCoordinator;
use crate::runtime::session::SessionOwner;
use crate::runtime::settings::test_support::{
    BASE, BundleLog, BundleRouter, BundleSdk, BundleServer, OTHER_MODEL, Probe, as_serde, field,
    harness, harness_with, lock, scenario_keys, string,
};
use crate::runtime::settings::{SettingsGateway, SettingsGatewayOptions};
use crate::runtime::turn::gateway::GatewayRequests;
use crate::upload::gateway::UploadGateway;
use crate::user_dialog::{DialogRuntime, GatewayUserDialogBus};
use pilotdeck_protocol::{JsString, JsValue, stringify_json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ORACLE: &str = include_str!("../../tests/fixtures/native-settings/oracle.json");

// ───────────────────────────── the settling registry (#1320's TS delegate)

/// The generator's registry delegate: `reload()` records the call and
/// settles on a setImmediate macrotask — modelled here as a future that
/// resolves on its first poll, so the settlement (markAllDirty +
/// broadcast) runs on the next driver yield after the RPC response, the
/// exact ordering the golden's before/after split pins.
struct SettlingRegistry(Probe);
impl RefreshRegistry for SettlingRegistry {
    fn reload(&self) -> Result<BoxFuture<RuntimeResult>, crate::runtime::project::RuntimeError> {
        self.0.event("registry-reload");
        Ok(Box::pin(async { Ok(()) }))
    }
    fn invalidate(&self, project_key: Option<&JsString>) {
        self.0.event(match project_key {
            Some(key) => format!("invalidate:{}", String::from_utf16_lossy(key.units())),
            None => "invalidate:null".to_string(),
        });
    }
}

// ───────────────────────────────── the fake WS connection layer

/// A parsed frame in a connection's buffer: a response carries its request
/// id (the wire cell echoes it), a notification its name and payload.
#[derive(Clone)]
enum Frame {
    Response(JsString),
    Notification { name: String, payload: JsValue },
}

/// The generator's `fakeWs().incoming`: every frame the connection
/// received, in arrival order.
struct Connection(Mutex<Vec<Frame>>);
impl Connection {
    fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }
    fn push(&self, frame: Frame) {
        lock(&self.0).push(frame);
    }
    fn clear(&self) {
        lock(&self.0).clear();
    }
    fn frames(&self) -> Vec<Frame> {
        lock(&self.0).clone()
    }
    fn index_of_response(&self, id: &str) -> Option<usize> {
        lock(&self.0).iter().position(|frame| match frame {
            Frame::Response(frame_id) => frame_id.to_utf8().map(|text| text == id).unwrap_or(false),
            Frame::Notification { .. } => false,
        })
    }
}

/// The server's connection set: `GatewayServer.broadcastNotification`
/// iterates it; every live connection receives the frame.
#[derive(Default)]
struct Connections(Mutex<Vec<Arc<Connection>>>);
impl Connections {
    fn add(&self) -> Arc<Connection> {
        let connection = Arc::new(Connection::new());
        lock(&self.0).push(connection.clone());
        connection
    }
}

/// The generator's `bindServer` mirror: push the frame to every live
/// connection, then record the broadcast event — both bundles (the harness
/// delegate's and the native stack's) bind this so a broadcast reaches
/// every connection exactly like the TS server.
struct BroadcastServer {
    probe: Probe,
    connections: Arc<Connections>,
}
impl NotificationServer for BroadcastServer {
    fn broadcast_notification(&self, name: &str, payload: &JsValue) {
        for connection in lock(&self.connections.0).iter() {
            connection.push(Frame::Notification {
                name: name.to_owned(),
                payload: payload.clone(),
            });
        }
        let text = stringify_json(payload)
            .expect("#1320 the broadcast payload must be serializable")
            .expect("#1320 the broadcast payload must not be undefined");
        self.probe.event(format!("broadcast:{name}:{text}"));
    }
}

// ───────────────────────────────────────────────────── normalization

/// The generator's ISO pattern `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$`.
fn is_iso(text: &str) -> bool {
    const TEMPLATE: &[u8; 24] = b"0000-00-00T00:00:00.000Z";
    text.len() == 24
        && text
            .as_bytes()
            .iter()
            .zip(TEMPLATE)
            .all(|(byte, template)| {
                byte.is_ascii_digit() || byte == template && !template.is_ascii_digit()
            })
}

/// The generator's `normalize`: ISO strings become `<ts>`, scratch-root
/// prefixes become `<root>…`; arrays and objects recurse.
fn normalize_value(value: &mut serde_json::Value, root: &str) {
    match value {
        serde_json::Value::String(text) => {
            if is_iso(text) {
                *text = "<ts>".to_owned();
            } else if let Some(rest) = text.strip_prefix(root) {
                *text = format!("<root>{rest}");
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                normalize_value(item, root);
            }
        }
        serde_json::Value::Object(entries) => {
            for (_, item) in entries.iter_mut() {
                normalize_value(item, root);
            }
        }
        _ => {}
    }
}

fn normalize(value: &JsValue, root: &str) -> serde_json::Value {
    let mut value = as_serde(value);
    normalize_value(&mut value, root);
    value
}

/// The generator's `notificationFrame`: a response frame records as
/// `{type:"response"}` (the id stays in the buffer), a notification as
/// `{type, name, payload}` — the before/after cells are exactly these.
fn notification_frames(frames: &[Frame], root: &str) -> serde_json::Value {
    serde_json::Value::Array(
        frames
            .iter()
            .map(|frame| match frame {
                Frame::Response(_) => serde_json::json!({"type": "response"}),
                Frame::Notification { name, payload } => serde_json::json!({
                    "type": "notification",
                    "name": name,
                    "payload": normalize(payload, root),
                }),
            })
            .collect(),
    )
}

/// The reconnect probe's wire cell: `response:{id}` / `notification:{name}`.
fn wire(frames: &[Frame]) -> Vec<String> {
    frames
        .iter()
        .map(|frame| match frame {
            Frame::Response(id) => {
                format!("response:{}", id.to_utf8().expect("#1320 ids are ASCII"))
            }
            Frame::Notification { name, .. } => format!("notification:{name}"),
        })
        .collect()
}

type SettingsOutcome = Result<JsValue, RequestError>;

/// The generator's `responseFrame`: `{ok, result}` on success, the error
/// frame's `{code, message}` on failure, both normalized.
fn response_cell(result: &SettingsOutcome, root: &str) -> serde_json::Value {
    match result {
        Ok(value) => serde_json::json!({"ok": true, "result": normalize(value, root)}),
        Err(error) => {
            let mut code = serde_json::Value::String(error.code.clone());
            let mut message =
                serde_json::Value::String(String::from_utf16_lossy(error.message.units()));
            normalize_value(&mut code, root);
            normalize_value(&mut message, root);
            serde_json::json!({"ok": false, "error": {"code": code, "message": message}})
        }
    }
}

/// The reconnect probe's `{version, contentHash}` read cells. TS JSON keeps
/// integral versions as `1`, not `1.0`.
fn generation_cell(result: &SettingsOutcome, what: &str) -> serde_json::Value {
    match result {
        Ok(value) => {
            let number = field(value, "version")
                .as_number()
                .unwrap_or_else(|| panic!("#1320 {what} must carry a version"));
            let version = if number.fract() == 0.0 {
                serde_json::Value::from(number as i64)
            } else {
                serde_json::Value::from(number)
            };
            serde_json::json!({
                "version": version,
                "contentHash": string(field(value, "contentHash"), "contentHash"),
            })
        }
        Err(error) => panic!(
            "#1320 {what} must succeed, got {}: {}",
            error.code,
            String::from_utf16_lossy(error.message.units())
        ),
    }
}

/// The probe read helpers: a resolve result's version, or a panic.
fn resolved_version(result: &SettingsOutcome, what: &str) -> f64 {
    match result {
        Ok(value) => field(value, "version")
            .as_number()
            .unwrap_or_else(|| panic!("#1320 {what} must carry a version")),
        Err(error) => panic!(
            "#1320 {what} must succeed, got {}: {}",
            error.code,
            String::from_utf16_lossy(error.message.units())
        ),
    }
}

fn changed_paths(result: &SettingsOutcome, what: &str) -> Vec<String> {
    match result {
        Ok(value) => match field(value, "changedPaths") {
            JsValue::Array(items) => items
                .iter()
                .map(|item| string(item, "changedPaths entry"))
                .collect(),
            _ => panic!("#1320 {what} must carry changedPaths"),
        },
        Err(error) => panic!(
            "#1320 {what} must succeed, got {}: {}",
            error.code,
            String::from_utf16_lossy(error.message.units())
        ),
    }
}

// ─────────────────────────────────────────── the native request plumbing

/// A factory that must never run: nothing in the settings family resolves
/// a project runtime, and a staging attempt is a replay bug.
struct NoRuntimeFactory;
impl ProjectRuntimeFactory for NoRuntimeFactory {
    fn stage(&self, _: &Path, _: &mut ResourceBundle) -> RuntimeResult {
        panic!("#1320 the settings replay never stages a runtime")
    }
}

/// The base [`Requests`] the replay drives: only `settings` matters (each
/// call swaps in the gateway under test), the rest are the inert test
/// stubs the other native fixtures use.
fn base_requests(root: &Path, owner: &SessionOwner) -> Requests {
    let home = root.join("native-home");
    std::fs::create_dir_all(&home).expect("#1320 the native home must exist");
    let token = root.join("native-token");
    std::fs::write(&token, "fixture").expect("#1320 the native token file must exist");
    let sidecar = root.join("native-sidecar");
    std::fs::write(&sidecar, "fixture").expect("#1320 the native sidecar file must exist");
    let project = root.join("native-project");
    std::fs::create_dir_all(&project).expect("#1320 the native project must exist");
    let options = Options::parse([
        "--home".into(),
        home.into_os_string(),
        "--project".into(),
        project.into_os_string(),
        "--listen".into(),
        "127.0.0.1:0".into(),
        "--token-file".into(),
        token.into_os_string(),
        "--sidecar".into(),
        sidecar.into_os_string(),
    ])
    .expect("#1320 the native options must parse");
    Requests {
        env: options.environment(Env::new()),
        options: options.clone(),
        snapshot_enabled: false,
        memory: Arc::new(MemoryGateway::disabled()),
        coordinator: Arc::default(),
        runs: None,
        owner: owner.clone(),
        project: Arc::new(ProjectRuntimeRegistry::new(
            options.project.clone(),
            Arc::new(NoRuntimeFactory),
        )),
        replacement: Arc::new(ReplacementCoordinator::new(
            Default::default(),
            Arc::new(owner.clone()),
            5000,
            None,
        )),
        recorders: Arc::default(),
        interactions: Arc::default(),
        mcp: Requests::stub_mcp(owner),
        async_hook_registry: None,
        // #1600: production assembles a connected bus (assembly/user_dialog.rs);
        // this stub must match, not the retired observer-coupled state.
        user_dialog_bus: Arc::new(Mutex::new(GatewayUserDialogBus::connected(
            DialogRuntime::default(),
        ))),
        upload_gateway: Arc::new(UploadGateway::unavailable()),
        prepare_weixin_login: None,
        cron: None,
        always_on: None,
        settings: Requests::stub_settings(owner),
        session_overrides: Arc::default(),
        restoring_session_keys: Arc::default(),
    }
}

/// One request in flight: the id is allocated at send time (the
/// generator's `r{next++}`), the response frame is pushed to the
/// connection buffer the moment the native route resolves — the frame
/// ordering against the notifications is the replay's wire truth.
async fn call(
    requests: &Requests,
    connection: &Arc<Connection>,
    gateway: Arc<SettingsGateway>,
    id: usize,
    method: &str,
    params: JsValue,
) -> SettingsOutcome {
    let mut requests = requests.clone();
    requests.settings = gateway;
    let connection = connection.clone();
    let method = method.to_owned();
    let frame = JsString::from(format!("r{id}"));
    let result = requests.settings_request(method, params).await;
    connection.push(Frame::Response(frame));
    result
}

/// The generator's drain: one macrotask flush lets the spawned settlement
/// (scheduled during the store reload, before the response send) run its
/// markAllDirty + broadcast chain; nothing after it can still be pending.
/// The bound only guards a broken settlement, never orders work.
async fn drain() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

#[derive(Default)]
struct Counter(AtomicUsize);
impl Counter {
    fn next(&self) -> usize {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

/// Everything a replay needs: the shared-probe harness (its registry is
/// the settling one), the connection set the broadcasts fan out to, and
/// the base native requests.
struct Stack {
    harness: crate::runtime::settings::test_support::Harness,
    probe: Probe,
    settling: Arc<SettlingRegistry>,
    connections: Arc<Connections>,
    requests: Requests,
    root: String,
}

fn stack(keys: &[String]) -> Stack {
    let probe = Probe::new();
    let settling = Arc::new(SettlingRegistry(probe.clone()));
    let harness = harness_with(keys, probe.clone(), settling.clone());
    let connections = Arc::new(Connections::default());
    harness.bundle.bind_server(Arc::new(BroadcastServer {
        probe: probe.clone(),
        connections: connections.clone(),
    }));
    let requests = base_requests(&harness._scratch.0, &probe.owner());
    let root = harness._scratch.0.to_string_lossy().into_owned();
    Stack {
        harness,
        probe,
        settling,
        connections,
        requests,
        root,
    }
}

/// A fresh store over an own scratch home, subscribed to the probe: for
/// the native-stack tests the store must carry ONLY the native bundle's
/// subscription (the harness bundle follows the harness trees; a shared
/// tree would double the chain).
fn own_store(root: &Path, name: &str, probe: &Probe) -> (ConfigStore, PathBuf) {
    let home = root.join("scratch").join(name).join("home");
    std::fs::create_dir_all(&home).expect("#1320 the scratch home must exist");
    let config = home.join("pilotdeck.yaml");
    std::fs::write(&config, BASE).expect("#1320 the scratch config must exist");
    let home_text = home.to_string_lossy().into_owned();
    let env = Env::from([
        ("HOME".to_owned(), home_text.clone()),
        ("PILOT_HOME".to_owned(), home_text),
    ]);
    let mut options = StoreOptions::new(home.clone(), home, env);
    options.project = Some(config.to_string_lossy().into_owned());
    let store = ConfigStore::new(options).expect("#1320 the scratch store must load");
    let probe = probe.clone();
    store.subscribe(Arc::new(move |event: &ReloadEvent| {
        probe.event(format!(
            "store-event:{}",
            event
                .changed_paths
                .iter()
                .map(|path| String::from_utf16_lossy(path.units()))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }));
    (store, config)
}

/// The production native stack over an own store: the #1320 matrix
/// assembly (only `resolve_settings` / `reload_config` /
/// `reload_extensions` wired), bound to the shared broadcast server.
fn native_stack(store: ConfigStore, stack: &Stack) -> NativeSettings {
    let native = native_settings(NativeSettingsOptions {
        owner: stack.probe.owner(),
        router: Arc::new(BundleRouter(stack.probe.clone())),
        registry: stack.settling.clone(),
        store,
        project_root: JsString::from("/pdrs-fixture/project"),
        log: Arc::new(BundleLog(stack.probe.clone())),
    });
    native.bundle.bind_server(Arc::new(BroadcastServer {
        probe: stack.probe.clone(),
        connections: stack.connections.clone(),
    }));
    native
}

fn oracle_array(oracle: &JsValue, key: &str, count: usize, what: &str) -> Vec<JsValue> {
    let items = match field(oracle, key) {
        JsValue::Array(items) => items.clone(),
        _ => panic!("#1320 the oracle must carry its {key} list"),
    };
    assert_eq!(
        items.len(),
        count,
        "#1320 the oracle must carry the full {what}"
    );
    items
}

fn setup_steps(row: &JsValue, kind: &str) -> Vec<String> {
    match field(row, "setup") {
        JsValue::Array(items) => items
            .iter()
            .map(|step| string(step, "setup step"))
            .collect(),
        _ => panic!("#1320 {kind} must carry a setup list"),
    }
}

fn event_cell(row: &JsValue, kind: &str) -> Vec<String> {
    match field(row, "events") {
        JsValue::Array(items) => items
            .iter()
            .map(|event| string(event, "events entry"))
            .collect(),
        _ => panic!("#1320 {kind} must carry an events list"),
    }
}

/// The six-cell comparison for one replayed row; a divergent cell pushes a
/// failure instead of stopping, so one run reports the whole oracle (the
/// negative controls bind one marker per row, and all of them must be
/// reachable in the single panic payload).
#[allow(clippy::too_many_arguments)]
fn compare_cells(
    failures: &mut Vec<String>,
    kind: &str,
    id: &str,
    response: &SettingsOutcome,
    before: &serde_json::Value,
    after: &serde_json::Value,
    re_read: Option<&serde_json::Value>,
    post: &serde_json::Value,
    row: &JsValue,
    root: &str,
    events: &[String],
) {
    // The probe rows carry their responses in their own cells (`responses`,
    // the reconnect wire cells) — compared by the caller — while a case or
    // matrix row always carries exactly one `response`.
    if kind != "probe" || row.get("response").is_some() {
        let expected = as_serde(field(row, "response"));
        let actual = response_cell(response, root);
        if actual != expected {
            failures.push(format!(
                "#1320 {kind} {id}: the WS response must match TS\n  expected: {expected}\n  actual:   {actual}"
            ));
        }
    }
    let expected = as_serde(field(row, "before"));
    if *before != expected {
        failures.push(format!(
            "#1320 {kind} {id}: the frames before the response must match TS\n  expected: {expected}\n  actual:   {before}"
        ));
    }
    let expected = as_serde(field(row, "after"));
    if *after != expected {
        failures.push(format!(
            "#1320 {kind} {id}: the frames after the response must match TS\n  expected: {expected}\n  actual:   {after}"
        ));
    }
    let expected = event_cell(row, kind);
    if events != expected {
        failures.push(format!(
            "#1320 {kind} {id}: the event stream must match TS\n  expected: {expected:?}\n  actual:   {events:?}"
        ));
    }
    if let Some(re_read) = re_read {
        let expected = as_serde(field(row, "reRead"));
        if *re_read != expected {
            failures.push(format!(
                "#1320 {kind} {id}: the re-read must match TS\n  expected: {expected}\n  actual:   {re_read}"
            ));
        }
    }
    let expected = as_serde(field(row, "post"));
    if *post != expected {
        failures.push(format!(
            "#1320 {kind} {id}: the post state must match TS\n  expected: {expected}\n  actual:   {post}"
        ));
    }
}

fn row_id(row: &JsValue) -> String {
    string(field(row, "id"), "row id")
}

// ─────────────────────────────────────────────────── the S4 replay tests

/// The full #1320 S4 replay: 21 case rows, the concurrency / reconnect /
/// secondConnection probes, and the 6-row production matrix, all through
/// [`Requests::settings_request`] over the real layers, compared cell by
/// cell against the TS golden.
#[tokio::test]
async fn native_settings_replays_the_real_ts_oracle() {
    let oracle = crate::test_baseline::oracle(ORACLE);
    let keys = scenario_keys(&oracle);
    let cases = oracle_array(&oracle, "cases", 21, "21-row case matrix");
    let matrix = oracle_array(&oracle, "matrix", 6, "6-row production matrix");
    let stack = stack(&keys);
    let primary = stack.connections.add();
    let counter = Counter::default();
    let mut failures: Vec<String> = Vec::new();

    // Phase A: the fully configured assembly, one oracle row per step.
    // The generator's gateway comes with its bundle attached (the TS
    // `createLocalGateway` closure); the harness attaches lazily once the
    // first tree exists, so every later rebind follows the trees exactly
    // like the single production store.
    let mut attached = false;
    for case in &cases {
        let id = row_id(case);
        let method = string(field(case, "method"), "case.method");
        for step in setup_steps(case, "case") {
            stack.harness.run_setup(&step).await;
        }
        if !attached && !lock(&stack.harness.trees).is_empty() {
            stack.harness.bundle.attach();
            attached = true;
        }
        stack.probe.clear_events();
        primary.clear();
        let params = field(case, "params").clone();
        let request_id = counter.next();
        let response = call(
            &stack.requests,
            &primary,
            Arc::new(stack.harness.gateway()),
            request_id,
            &method,
            params,
        )
        .await;
        drain().await;
        let frames = primary.frames();
        let at = primary
            .index_of_response(&format!("r{request_id}"))
            .unwrap_or_else(|| panic!("#1320 case {id}: the response frame must be in the buffer"));
        let before = notification_frames(&frames[..at], &stack.root);
        let after = notification_frames(&frames[at + 1..], &stack.root);
        let re_read_id = counter.next();
        let re_read = call(
            &stack.requests,
            &primary,
            Arc::new(stack.harness.gateway()),
            re_read_id,
            "resolve_settings",
            JsValue::Undefined,
        )
        .await;
        let re_read = match &re_read {
            Ok(value) => Some(normalize(value, &stack.root)),
            Err(error) => {
                failures.push(format!(
                    "#1320 case {id}: the re-read must succeed, got {}: {}",
                    error.code,
                    String::from_utf16_lossy(error.message.units())
                ));
                None
            }
        };
        let post = normalize(&stack.harness.post_state(), &stack.root);
        compare_cells(
            &mut failures,
            "case",
            &id,
            &response,
            &before,
            &after,
            re_read.as_ref(),
            &post,
            case,
            &stack.root,
            &stack.probe.events(),
        );
    }

    // probe: two same-tick reload_config calls share the store's single
    // in-flight reload.
    {
        let probe_row = field(field(&oracle, "probes"), "concurrency");
        let id = row_id(probe_row);
        for step in setup_steps(probe_row, "probe") {
            stack.harness.run_setup(&step).await;
        }
        stack.probe.clear_events();
        primary.clear();
        let first_id = counter.next();
        let second_id = counter.next();
        let first_gateway = Arc::new(stack.harness.gateway());
        let second_gateway = Arc::new(stack.harness.gateway());
        let first = call(
            &stack.requests,
            &primary,
            first_gateway,
            first_id,
            "reload_config",
            JsValue::Undefined,
        );
        let second = call(
            &stack.requests,
            &primary,
            second_gateway,
            second_id,
            "reload_config",
            JsValue::Undefined,
        );
        // `biased;`: the first-sent request stays the first-polled one on
        // every wake (the unbiased join! rotates poll order, which would let
        // the second call's future execute the shared flight and answer
        // first — the TS wire pins the first-sent response first).
        let (first, second) = tokio::join!(biased; first, second);
        drain().await;
        let frames = primary.frames();
        let at = primary
            .index_of_response(&format!("r{second_id}"))
            .unwrap_or_else(|| {
                panic!("#1320 probe {id}: the second response frame must be in the buffer")
            });
        let before = notification_frames(&frames[..at], &stack.root);
        let after = notification_frames(&frames[at + 1..], &stack.root);
        let re_read_id = counter.next();
        let re_read = call(
            &stack.requests,
            &primary,
            Arc::new(stack.harness.gateway()),
            re_read_id,
            "resolve_settings",
            JsValue::Undefined,
        )
        .await;
        let re_read = match &re_read {
            Ok(value) => Some(normalize(value, &stack.root)),
            Err(error) => {
                failures.push(format!(
                    "#1320 probe {id}: the re-read must succeed, got {}",
                    error.code
                ));
                None
            }
        };
        let expected = as_serde(field(probe_row, "responses"));
        let actual = serde_json::json!([
            response_cell(&first, &stack.root),
            response_cell(&second, &stack.root),
        ]);
        if actual != expected {
            failures.push(format!(
                "#1320 probe {id}: the shared responses must match TS\n  expected: {expected}\n  actual:   {actual}"
            ));
        }
        let post = normalize(&stack.harness.post_state(), &stack.root);
        compare_cells(
            &mut failures,
            "probe",
            &id,
            &first,
            &before,
            &after,
            re_read.as_ref(),
            &post,
            probe_row,
            &stack.root,
            &stack.probe.events(),
        );
    }

    // probe: a same-tick resolve while a changed reload is in flight reads
    // the OLD generation; a brand-new connection reads the NEW one and the
    // store survives the reconnect.
    {
        let probe_row = field(field(&oracle, "probes"), "reconnect");
        let id = row_id(probe_row);
        for step in setup_steps(probe_row, "probe") {
            stack.harness.run_setup(&step).await;
        }
        stack.probe.clear_events();
        primary.clear();
        let reload_id = counter.next();
        let mid_id = counter.next();
        let reload_gateway = Arc::new(stack.harness.gateway());
        let mid_gateway = Arc::new(stack.harness.gateway());
        let reload = call(
            &stack.requests,
            &primary,
            reload_gateway,
            reload_id,
            "reload_config",
            JsValue::Undefined,
        );
        let mid = call(
            &stack.requests,
            &primary,
            mid_gateway,
            mid_id,
            "resolve_settings",
            JsValue::Undefined,
        );
        let (mid, reload) = tokio::join!(mid, reload);
        drain().await;
        let second = stack.connections.add();
        let second_read_id = counter.next();
        let second_read = call(
            &stack.requests,
            &second,
            Arc::new(stack.harness.gateway()),
            second_read_id,
            "resolve_settings",
            JsValue::Undefined,
        )
        .await;
        let continuity_reload_id = counter.next();
        let continuity_reload = call(
            &stack.requests,
            &primary,
            Arc::new(stack.harness.gateway()),
            continuity_reload_id,
            "reload_config",
            JsValue::Undefined,
        )
        .await;
        let continuity_read_id = counter.next();
        let continuity_read = call(
            &stack.requests,
            &second,
            Arc::new(stack.harness.gateway()),
            continuity_read_id,
            "resolve_settings",
            JsValue::Undefined,
        )
        .await;
        let frames = primary.frames();
        let at = primary
            .index_of_response(&format!("r{reload_id}"))
            .unwrap_or_else(|| {
                panic!("#1320 probe {id}: the reload response frame must be in the buffer")
            });
        let before = notification_frames(
            &frames[..at]
                .iter()
                .filter(|frame| matches!(frame, Frame::Notification { .. }))
                .cloned()
                .collect::<Vec<_>>(),
            &stack.root,
        );
        let after = notification_frames(
            &frames[at + 1..]
                .iter()
                .filter(|frame| matches!(frame, Frame::Notification { .. }))
                .cloned()
                .collect::<Vec<_>>(),
            &stack.root,
        );
        let expected = as_serde(field(probe_row, "wire"));
        let actual = serde_json::to_value(wire(&frames)).expect("#1320 wire must serialize");
        if actual != expected {
            failures.push(format!(
                "#1320 probe {id}: the wire order must match TS\n  expected: {expected}\n  actual:   {actual}"
            ));
        }
        let cells = [
            (
                "midFlightRead",
                generation_cell(&mid, "the mid-flight read"),
            ),
            ("reloadResponse", response_cell(&reload, &stack.root)),
            (
                "secondConnectionRead",
                generation_cell(&second_read, "the reconnected read"),
            ),
            (
                "continuityReload",
                response_cell(&continuity_reload, &stack.root),
            ),
            (
                "continuityRead",
                generation_cell(&continuity_read, "the continuity read"),
            ),
        ];
        for (key, actual) in cells {
            let expected = as_serde(field(probe_row, key));
            if actual != expected {
                failures.push(format!(
                    "#1320 probe {id}: {key} must match TS\n  expected: {expected}\n  actual:   {actual}"
                ));
            }
        }
        let post = normalize(&stack.harness.post_state(), &stack.root);
        compare_cells(
            &mut failures,
            "probe",
            &id,
            &reload,
            &before,
            &after,
            None,
            &post,
            probe_row,
            &stack.root,
            &stack.probe.events(),
        );
    }

    // probe: the broadcast reaches every live connection, not only the
    // one that made the call.
    {
        let probe_row = field(field(&oracle, "probes"), "secondConnection");
        let id = row_id(probe_row);
        for step in setup_steps(probe_row, "probe") {
            stack.harness.run_setup(&step).await;
        }
        let second = stack.connections.add();
        stack.probe.clear_events();
        primary.clear();
        second.clear();
        let response_id = counter.next();
        let response = call(
            &stack.requests,
            &primary,
            Arc::new(stack.harness.gateway()),
            response_id,
            "reload_extensions",
            JsValue::Undefined,
        )
        .await;
        drain().await;
        let frames = primary.frames();
        let at = primary
            .index_of_response(&format!("r{response_id}"))
            .unwrap_or_else(|| {
                panic!("#1320 probe {id}: the response frame must be in the buffer")
            });
        let before = notification_frames(&frames[..at], &stack.root);
        let after = notification_frames(&frames[at + 1..], &stack.root);
        let second_before = notification_frames(&second.frames(), &stack.root);
        let expected = as_serde(field(probe_row, "secondBefore"));
        if second_before != expected {
            failures.push(format!(
                "#1320 probe {id}: the broadcast must reach every connection\n  expected: {expected}\n  actual:   {second_before}"
            ));
        }
        let re_read_id = counter.next();
        let re_read = call(
            &stack.requests,
            &primary,
            Arc::new(stack.harness.gateway()),
            re_read_id,
            "resolve_settings",
            JsValue::Undefined,
        )
        .await;
        let re_read = match &re_read {
            Ok(value) => Some(normalize(value, &stack.root)),
            Err(error) => {
                failures.push(format!(
                    "#1320 probe {id}: the re-read must succeed, got {}",
                    error.code
                ));
                None
            }
        };
        let post = normalize(&stack.harness.post_state(), &stack.root);
        compare_cells(
            &mut failures,
            "probe",
            &id,
            &response,
            &before,
            &after,
            re_read.as_ref(),
            &post,
            probe_row,
            &stack.root,
            &stack.probe.events(),
        );
    }

    // Phase B: the #1320 production matrix assembly — only the three
    // delegates the isolated native process owns are wired
    // (`services::native_settings`); the other three methods answer the
    // TS unconfigured shapes through the component's own option handling.
    {
        let mut native: Option<NativeSettings> = None;
        let mut matrix_connection: Option<Arc<Connection>> = None;
        for row in &matrix {
            let id = row_id(row);
            let method = string(field(row, "method"), "matrix.method");
            for step in setup_steps(row, "matrix row") {
                stack.harness.run_setup(&step).await;
            }
            if native.is_none() {
                let store = lock(&stack.harness.trees)
                    .iter()
                    .find(|(name, _)| name == "pb1")
                    .map(|(_, tree)| tree.store.clone())
                    .unwrap_or_else(|| {
                        panic!("#1320 matrix {id}: the pb1 tree must exist before the matrix runs")
                    });
                native = Some(native_stack(store, &stack));
                matrix_connection = Some(stack.connections.add());
            }
            let native = native.as_ref().expect("#1320 the native stack must exist");
            let connection = matrix_connection
                .as_ref()
                .expect("#1320 the matrix connection must exist");
            stack.probe.clear_events();
            connection.clear();
            let params = field(row, "params").clone();
            let request_id = counter.next();
            let response = call(
                &stack.requests,
                connection,
                native.gateway.clone(),
                request_id,
                &method,
                params,
            )
            .await;
            drain().await;
            let frames = connection.frames();
            let at = connection
                .index_of_response(&format!("r{request_id}"))
                .unwrap_or_else(|| {
                    panic!("#1320 matrix {id}: the response frame must be in the buffer")
                });
            let before = notification_frames(&frames[..at], &stack.root);
            let after = notification_frames(&frames[at + 1..], &stack.root);
            let re_read_id = counter.next();
            let re_read = call(
                &stack.requests,
                connection,
                native.gateway.clone(),
                re_read_id,
                "resolve_settings",
                JsValue::Undefined,
            )
            .await;
            let re_read = match &re_read {
                Ok(value) => Some(normalize(value, &stack.root)),
                Err(error) => {
                    failures.push(format!(
                        "#1320 matrix {id}: the re-read must succeed, got {}",
                        error.code
                    ));
                    None
                }
            };
            let post = normalize(&stack.harness.post_state(), &stack.root);
            compare_cells(
                &mut failures,
                "matrix",
                &id,
                &response,
                &before,
                &after,
                re_read.as_ref(),
                &post,
                row,
                &stack.root,
                &stack.probe.events(),
            );
        }
    }

    // The registered divergence cases are recorded by the TS golden but
    // not replayed while the ledger entry stands; the count is guarded so
    // a drifted oracle cannot silently drop them.
    let divergence = oracle_array(
        &oracle,
        "divergenceCases",
        2,
        "registered divergence cases (U7-EXTENSIONS-RELOAD-STRING-COERCION-001)",
    );
    let divergence_ids: Vec<String> = divergence
        .iter()
        .map(|row| string(field(row, "id"), "divergence id"))
        .collect();
    assert_eq!(
        divergence_ids,
        [
            "extensions-changed-paths-numbers",
            "extensions-project-key-number"
        ],
        "#1320 the oracle must carry exactly the two registered divergence cases"
    );

    assert!(
        failures.is_empty(),
        "#1320 {} oracle row(s) diverged from the TS golden:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ──────────────────────────────────────────── the seven named #1320 tests

/// Two same-tick `reload_config` calls share the store's single in-flight
/// reload: both responses report the same change, the version bumps once
/// and the settlement chain (registry reload, markAllDirty, broadcast)
/// runs once.
#[tokio::test]
async fn reload_config_inflight_is_shared() {
    let stack = stack(&[]);
    stack.harness.run_setup("tree:inflight").await;
    stack.harness.bundle.attach();
    stack.harness.run_setup("write-yaml:inflight:other").await;
    stack.probe.clear_events();
    let primary = stack.connections.add();
    let counter = Counter::default();
    let first_gateway = Arc::new(stack.harness.gateway());
    let second_gateway = Arc::new(stack.harness.gateway());
    let first = call(
        &stack.requests,
        &primary,
        first_gateway,
        counter.next(),
        "reload_config",
        JsValue::Undefined,
    );
    let second = call(
        &stack.requests,
        &primary,
        second_gateway,
        counter.next(),
        "reload_config",
        JsValue::Undefined,
    );
    let (first, second) = tokio::join!(first, second);
    drain().await;
    let first_paths = changed_paths(&first, "#1320 inflight: the first reload");
    let second_paths = changed_paths(&second, "#1320 inflight: the second reload");
    assert_eq!(
        first_paths, second_paths,
        "#1320 inflight: both calls must report the shared flight's change"
    );
    assert_eq!(
        first_paths,
        ["agent.model.id", "agent.model.model"],
        "#1320 inflight: the shared reload must report the model change"
    );
    let events = stack.probe.events();
    assert_eq!(
        events,
        [
            "store-event:agent.model.id,agent.model.model",
            "log:[pilotdeck] Config reloaded, refreshing runtimes: agent.model.id, agent.model.model",
            "sdk-config-change:agent.model.id,agent.model.model|next-request",
            "registry-reload",
            "mark-all-dirty:config_changed",
            r#"broadcast:config_changed:{"changedPaths":["agent.model.id","agent.model.model"],"changeClasses":["next-request"]}"#,
        ],
        "#1320 inflight: one flight settles exactly one chain"
    );
    let read = call(
        &stack.requests,
        &primary,
        Arc::new(stack.harness.gateway()),
        counter.next(),
        "resolve_settings",
        JsValue::Undefined,
    )
    .await;
    assert_eq!(
        resolved_version(&read, "#1320 inflight: the post-flight read"),
        2.0,
        "#1320 inflight: the shared reload bumps the generation once"
    );
    assert_eq!(
        primary
            .frames()
            .iter()
            .filter(|frame| matches!(frame, Frame::Notification { .. }))
            .count(),
        1,
        "#1320 inflight: the shared settlement broadcasts one notification"
    );
}

/// While a changed `reload_config` is in flight, a same-tick
/// `resolve_settings` reads the OLD generation; after the settlement a
/// brand-new connection reads the NEW generation, and the store survives
/// the reconnect (the next reload bumps the version monotonically).
/// Driven through the production native stack
/// (`services::native_settings`): its bundle must be attached for the
/// reload chain to settle at all.
#[tokio::test]
async fn resolve_settings_generation_across_reconnect() {
    let stack = stack(&[]);
    let root = stack.harness._scratch.0.clone();
    let (store, config) = own_store(&root, "reconnect", &stack.probe);
    std::fs::write(&config, OTHER_MODEL)
        .expect("#1320 reconnect: the changed config must be written");
    let native = native_stack(store, &stack);
    stack.probe.clear_events();
    let primary = stack.connections.add();
    let counter = Counter::default();
    let reload_id = counter.next();
    let reload = call(
        &stack.requests,
        &primary,
        native.gateway.clone(),
        reload_id,
        "reload_config",
        JsValue::Undefined,
    );
    let mid_id = counter.next();
    let mid = call(
        &stack.requests,
        &primary,
        native.gateway.clone(),
        mid_id,
        "resolve_settings",
        JsValue::Undefined,
    );
    let (mid, reload) = tokio::join!(mid, reload);
    drain().await;
    let second = stack.connections.add();
    let second_read = call(
        &stack.requests,
        &second,
        native.gateway.clone(),
        counter.next(),
        "resolve_settings",
        JsValue::Undefined,
    )
    .await;
    let continuity_reload_id = counter.next();
    let continuity_reload = call(
        &stack.requests,
        &primary,
        native.gateway.clone(),
        continuity_reload_id,
        "reload_config",
        JsValue::Undefined,
    )
    .await;
    let continuity_read = call(
        &stack.requests,
        &second,
        native.gateway.clone(),
        counter.next(),
        "resolve_settings",
        JsValue::Undefined,
    )
    .await;
    assert_eq!(
        resolved_version(&mid, "#1320 reconnect: the mid-flight read"),
        1.0,
        "#1320 reconnect: the mid-flight read must see the old generation"
    );
    assert_eq!(
        changed_paths(&reload, "#1320 reconnect: the reload"),
        ["agent.model.id", "agent.model.model"],
        "#1320 reconnect: the in-flight reload reports its change"
    );
    let wire = wire(&primary.frames());
    assert_eq!(
        wire,
        [
            format!("response:r{mid_id}"),
            format!("response:r{reload_id}"),
            "notification:config_changed".to_owned(),
            format!("response:r{continuity_reload_id}"),
        ],
        "#1320 reconnect: the mid-flight read answers before the reload response"
    );
    let at = primary
        .index_of_response(&format!("r{reload_id}"))
        .expect("#1320 reconnect: the reload response must be in the buffer");
    let after = notification_frames(
        &primary.frames()[at + 1..]
            .iter()
            .filter(|frame| matches!(frame, Frame::Notification { .. }))
            .cloned()
            .collect::<Vec<_>>(),
        &stack.root,
    );
    assert_eq!(
        after,
        serde_json::json!([{
            "type": "notification",
            "name": "config_changed",
            "payload": {
                "changedPaths": ["agent.model.id", "agent.model.model"],
                "changeClasses": ["next-request"],
            },
        }]),
        "#1320 reconnect: the settled broadcast must land after the reload response"
    );
    assert_eq!(
        resolved_version(&second_read, "#1320 reconnect: the reconnected read"),
        2.0,
        "#1320 reconnect: a reconnected client reads the new generation"
    );
    assert_eq!(
        changed_paths(&continuity_reload, "#1320 reconnect: the continuity reload"),
        Vec::<String>::new(),
        "#1320 reconnect: the post-reconnect reload works"
    );
    assert_eq!(
        resolved_version(&continuity_read, "#1320 reconnect: the continuity read"),
        3.0,
        "#1320 reconnect: the store survives the reconnect (monotonic version)"
    );
    let events = stack.probe.events();
    let settled: Vec<&String> = events
        .iter()
        .filter(|event| {
            event.starts_with("mark-all-dirty")
                || event.starts_with("broadcast")
                || *event == "registry-reload"
        })
        .collect();
    assert_eq!(
        settled,
        [
            &"registry-reload".to_string(),
            &"mark-all-dirty:config_changed".to_string(),
            &r#"broadcast:config_changed:{"changedPaths":["agent.model.id","agent.model.model"],"changeClasses":["next-request"]}"#.to_string(),
        ],
        "#1320 reconnect: the native bundle's attached chain settles exactly once (no sdk dispatch on the native stack)"
    );
}

/// The `reload_config` settlement is ordered log → sdk dispatch →
/// registry.reload() → settle → markAllDirty → broadcast, and the
/// broadcast frame lands after the response (the settlement spawns during
/// the reload publication).
#[tokio::test]
async fn reload_config_dirty_then_broadcast_order() {
    let stack = stack(&[]);
    stack.harness.run_setup("tree:order").await;
    stack.harness.bundle.attach();
    stack.harness.run_setup("write-yaml:order:other").await;
    stack.probe.clear_events();
    let primary = stack.connections.add();
    let counter = Counter::default();
    let response = call(
        &stack.requests,
        &primary,
        Arc::new(stack.harness.gateway()),
        counter.next(),
        "reload_config",
        JsValue::Undefined,
    )
    .await;
    drain().await;
    assert_eq!(
        changed_paths(&response, "#1320 dirty-then-broadcast: the reload"),
        ["agent.model.id", "agent.model.model"],
        "#1320 dirty-then-broadcast: the reload must report the change"
    );
    assert_eq!(
        stack.probe.events(),
        [
            "store-event:agent.model.id,agent.model.model",
            "log:[pilotdeck] Config reloaded, refreshing runtimes: agent.model.id, agent.model.model",
            "sdk-config-change:agent.model.id,agent.model.model|next-request",
            "registry-reload",
            "mark-all-dirty:config_changed",
            r#"broadcast:config_changed:{"changedPaths":["agent.model.id","agent.model.model"],"changeClasses":["next-request"]}"#,
        ],
        "#1320 dirty-then-broadcast: markAllDirty must run before the broadcast"
    );
    let frames = primary.frames();
    let response_index = frames
        .iter()
        .position(|frame| matches!(frame, Frame::Response(_)))
        .expect("#1320 dirty-then-broadcast: the response must be in the buffer");
    let before = notification_frames(&frames[..response_index], &stack.root);
    let after = notification_frames(&frames[response_index + 1..], &stack.root);
    assert_eq!(
        before,
        serde_json::json!([]),
        "#1320 dirty-then-broadcast: the chain settles after the response"
    );
    assert_eq!(
        after,
        serde_json::json!([{
            "type": "notification",
            "name": "config_changed",
            "payload": {
                "changedPaths": ["agent.model.id", "agent.model.model"],
                "changeClasses": ["next-request"],
            },
        }]),
        "#1320 dirty-then-broadcast: the broadcast lands after the response"
    );
}

/// `reload_extensions` runs its whole chain synchronously (log →
/// invalidate → mark-dirty → broadcast, before the response): per project
/// when a truthy `projectKey` is given, all runtimes otherwise, and the
/// `changedPaths` are reported through.
#[tokio::test]
async fn reload_extensions_order_per_key() {
    let stack = stack(&[]);
    stack.harness.run_setup("tree:extensions").await;
    let primary = stack.connections.add();
    let counter = Counter::default();
    let project_params = pilotdeck_protocol::parse_json(r#"{"projectKey":"/p"}"#)
        .expect("#1320 extensions-order: the project params must parse");
    stack.probe.clear_events();
    primary.clear();
    let project_id = counter.next();
    let project = call(
        &stack.requests,
        &primary,
        Arc::new(stack.harness.gateway()),
        project_id,
        "reload_extensions",
        project_params,
    )
    .await;
    drain().await;
    assert_eq!(
        changed_paths(&project, "#1320 extensions-order: the project reload"),
        Vec::<String>::new(),
        "#1320 extensions-order: the project branch reports no changed paths"
    );
    assert_eq!(
        stack.probe.events(),
        [
            "log:[pilotdeck] Extensions reload requested for project /p: (manual)",
            "invalidate:/p",
            "mark-project-dirty:/p:extension_changed",
            r#"broadcast:config_changed:{"changedPaths":[],"changeClasses":["extension-changed"]}"#,
        ],
        "#1320 extensions-order: the project branch runs log → invalidate → mark-project-dirty → broadcast"
    );
    assert_eq!(
        wire(&primary.frames()),
        [
            "notification:config_changed".to_owned(),
            format!("response:r{project_id}"),
        ],
        "#1320 extensions-order: the project broadcast lands before the response"
    );
    stack.probe.clear_events();
    primary.clear();
    let all_runtimes = call(
        &stack.requests,
        &primary,
        Arc::new(stack.harness.gateway()),
        counter.next(),
        "reload_extensions",
        JsValue::Undefined,
    )
    .await;
    drain().await;
    assert_eq!(
        changed_paths(
            &all_runtimes,
            "#1320 extensions-order: the all-runtimes reload"
        ),
        Vec::<String>::new(),
        "#1320 extensions-order: the all-runtimes branch reports no changed paths"
    );
    assert_eq!(
        stack.probe.events(),
        [
            "log:[pilotdeck] Extensions reload requested for all runtimes: (manual)",
            "invalidate:null",
            "mark-all-dirty:extension_changed",
            r#"broadcast:config_changed:{"changedPaths":[],"changeClasses":["extension-changed"]}"#,
        ],
        "#1320 extensions-order: the all-runtimes branch runs log → invalidate → mark-all-dirty → broadcast"
    );
    let paths_params =
        pilotdeck_protocol::parse_json(r#"{"changedPaths":["ext/a.ts","ext/b.ts"]}"#)
            .expect("#1320 extensions-order: the changed paths params must parse");
    stack.probe.clear_events();
    primary.clear();
    let changed = call(
        &stack.requests,
        &primary,
        Arc::new(stack.harness.gateway()),
        counter.next(),
        "reload_extensions",
        paths_params,
    )
    .await;
    drain().await;
    assert_eq!(
        changed_paths(&changed, "#1320 extensions-order: the changed-paths reload"),
        ["ext/a.ts", "ext/b.ts"],
        "#1320 extensions-order: the changed paths are reported through"
    );
    assert_eq!(
        stack.probe.events(),
        [
            "log:[pilotdeck] Extensions reload requested for all runtimes: ext/a.ts, ext/b.ts",
            "invalidate:null",
            "mark-all-dirty:extension_changed",
            r#"broadcast:config_changed:{"changedPaths":["ext/a.ts","ext/b.ts"],"changeClasses":["extension-changed"]}"#,
        ],
        "#1320 extensions-order: the changed paths flow through the broadcast"
    );
}

/// The `reload_config` capture listener is detached `finally`-style,
/// including when the future is dropped at the store-flight await point (a
/// client disconnect alone does not drop it — the WS layer retains and
/// drains request jobs; it is the host's own teardown that aborts a draining
/// job past its budget, see `runtime/settings/gateway.rs`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reload_config_drop_detaches_capture() {
    let stack = stack(&[]);
    stack.harness.make_tree("t-drop", None).await;
    let store = lock(&stack.harness.trees)
        .iter()
        .find(|(name, _)| name == "t-drop")
        .map(|(_, tree)| tree.store.clone())
        .expect("#1320 drop-capture: the scratch tree must be registered");
    let before = store.listener_count();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Mutex::new(Some(release_rx));
    let blocker = store.subscribe(Arc::new(move |_: &ReloadEvent| {
        if let Some(receive) = lock(&release_rx).take() {
            let _ = entered_tx.send(());
            let _ = receive.recv_timeout(Duration::from_secs(30));
        }
    }));
    assert_eq!(
        store.listener_count(),
        before + 1,
        "#1320 drop-capture: the blocker must be the only new listener"
    );
    let first = tokio::spawn(store.reload("manual"));
    tokio::task::spawn_blocking(move || {
        entered_rx
            .recv()
            .expect("#1320 drop-capture: the blocker listener must run")
    })
    .await
    .expect("#1320 drop-capture: the gate join must succeed");
    let connection = Arc::new(Connection::new());
    let mut future = Box::pin(call(
        &stack.requests,
        &connection,
        Arc::new(stack.harness.gateway()),
        0,
        "reload_config",
        JsValue::Undefined,
    ));
    let parked = tokio::select! {
        biased;
        result = &mut future => panic!(
            "#1320 drop-capture: reload_config must park at the chained flight, got {result:?}"
        ),
        _ = tokio::task::yield_now() => true,
    };
    assert!(parked, "#1320 drop-capture: the parking branch must win");
    assert_eq!(
        store.listener_count(),
        before + 2,
        "#1320 drop-capture: the capture listener must be subscribed while the reload is in flight"
    );
    drop(future);
    assert_eq!(
        store.listener_count(),
        before + 1,
        "#1320 drop-capture: dropping the future must detach the capture listener"
    );
    release_tx
        .send(())
        .expect("#1320 drop-capture: the blocker must be released");
    blocker.unsubscribe();
    first
        .await
        .expect("#1320 drop-capture: the first reload must join")
        .expect("#1320 drop-capture: the first reload must succeed");
    assert_eq!(
        store.listener_count(),
        before,
        "#1320 drop-capture: cleanup must leave only the probe listener"
    );
}

/// The recording runtime factory for the ticket test: every registry
/// reload stages a fresh runtime, recorded as `stage:N` on the probe so
/// the ticket queue's ordering is observable in the event stream.
struct StageFactory {
    probe: Probe,
    calls: AtomicUsize,
}
struct TicketGroup;
impl RuntimeResources for TicketGroup {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}
impl ProjectRuntimeFactory for StageFactory {
    fn stage(&self, _: &Path, resources: &mut ResourceBundle) -> RuntimeResult {
        let id = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        resources.add(TicketGroup);
        self.probe.event(format!("stage:{id}"));
        Ok(())
    }
}

/// The registry ticket queue: the next queued reload starts only after the
/// previous ticket's settlement callbacks (markAllDirty + broadcast) ran —
/// the ticket is released observably, not just without hanging. The first
/// settlement parks inside its `markAllDirty` (a gated router) while the
/// second change's reload is already answered, proving the second reload
/// stays queued behind the first ticket until the release.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reload_ticket_releases_after_settlement() {
    let harness = harness(&[]);
    let probe = harness.probe.clone();
    let root = std::env::temp_dir().join(format!("pdrs-1320-ticket-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("#1320 ticket: the scratch root must exist");
    std::fs::write(root.join("pilotdeck.yaml"), BASE)
        .expect("#1320 ticket: the scratch config must exist");
    let (store, config) = own_store(&root, "ticket", &probe);
    let factory = Arc::new(StageFactory {
        probe: probe.clone(),
        calls: AtomicUsize::new(0),
    });
    let registry = Arc::new(ProjectRuntimeRegistry::new(
        root.clone(),
        factory as Arc<dyn ProjectRuntimeFactory>,
    ));
    registry
        .resolve(None)
        .expect("#1320 ticket: the initial runtime must stage");
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let router = Arc::new(GatedRouter {
        probe: probe.clone(),
        gate: Mutex::new(Some((entered_tx, release_rx))),
    });
    let bundle = Arc::new(RefreshBundle::new(RefreshOptions {
        config_store: Arc::new(store.clone()),
        registry: Arc::new(RegistryRefresh::new(registry.clone())),
        memory_maintenance: Arc::new(crate::runtime::refresh::NoMemoryMaintenance),
        router: {
            let router = router.clone();
            Arc::new(
                move || Some(router.clone() as Arc<dyn crate::runtime::refresh::RefreshRouter>),
            )
        },
        project_root: JsString::from("/pdrs-fixture/project"),
        memory_diagnostics: None,
        sdk: Some(Arc::new(BundleSdk(probe.clone()))),
        log: Arc::new(BundleLog(probe.clone())),
    }));
    bundle.attach();
    bundle.bind_server(Arc::new(BundleServer(probe.clone())));
    let gateway = Arc::new(SettingsGateway::new(SettingsGatewayOptions {
        owner: probe.owner(),
        apply_flag_settings: None,
        update_settings: None,
        resolve_settings: None,
        reload_output_styles: None,
        reload_config: Some(bundle.clone()),
        reload_extensions: None,
    }));
    let requests = base_requests(&root, &probe.owner());
    let connection = Arc::new(Connection::new());
    probe.clear_events();

    // First change: the response resolves while its settlement parks
    // inside markAllDirty, holding the first ticket.
    std::fs::write(&config, OTHER_MODEL).expect("#1320 ticket: the changed config must be written");
    let first = call(
        &requests,
        &connection,
        gateway.clone(),
        0,
        "reload_config",
        JsValue::Undefined,
    )
    .await;
    assert_eq!(
        changed_paths(&first, "#1320 ticket: the first reload"),
        ["agent.model.id", "agent.model.model"],
        "#1320 ticket: the first reload must report its change"
    );
    tokio::task::spawn_blocking(move || {
        entered_rx
            .recv()
            .expect("#1320 ticket: the gated markAllDirty must run")
    })
    .await
    .expect("#1320 ticket: the gate join must succeed");

    // Second change: answered while the first ticket is still held — the
    // second reload must stay queued (no stage, no markAllDirty).
    std::fs::write(&config, BASE).expect("#1320 ticket: the base config must be restored");
    let second = call(
        &requests,
        &connection,
        gateway.clone(),
        1,
        "reload_config",
        JsValue::Undefined,
    )
    .await;
    assert_eq!(
        changed_paths(&second, "#1320 ticket: the second reload"),
        ["agent.model.id", "agent.model.model"],
        "#1320 ticket: the second reload must report its change"
    );
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    let events = probe.events();
    assert!(
        !events.iter().any(|event| event == "stage:3"),
        "#1320 ticket: the second reload must stay queued behind the first ticket, got {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| *event == "mark-all-dirty:config_changed")
            .count(),
        0,
        "#1320 ticket: neither settlement may pass the gate yet, got {events:?}"
    );

    // Release: the first ticket's callbacks run, then — and only then —
    // the second ticket's reload stages and settles.
    release_tx
        .send(())
        .expect("#1320 ticket: the gate must be released");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let events = probe.events();
        if events
            .iter()
            .filter(|event| *event == "mark-all-dirty:config_changed")
            .count()
            == 2
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "#1320 ticket: both settlements must complete after the release, got {events:?}"
        );
        tokio::task::yield_now().await;
    }
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    let settled: Vec<String> = probe
        .events()
        .into_iter()
        .filter(|event| {
            event.starts_with("stage:")
                || event.starts_with("mark-all-dirty")
                || event.starts_with("broadcast")
        })
        .collect();
    let changed = r#"broadcast:config_changed:{"changedPaths":["agent.model.id","agent.model.model"],"changeClasses":["next-request"]}"#;
    assert_eq!(
        settled,
        [
            "stage:2".to_owned(),
            "mark-all-dirty:config_changed".to_owned(),
            changed.to_owned(),
            "stage:3".to_owned(),
            "mark-all-dirty:config_changed".to_owned(),
            changed.to_owned(),
        ],
        "#1320 ticket: the next ticket's reload must start only after the previous settlement's markAllDirty and broadcast ran"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// The `markAllDirty` gate: the first settlement parks inside it until
/// the test releases, so the second ticket's queue position is observed.
struct GatedRouter {
    probe: Probe,
    gate: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
}
impl crate::runtime::refresh::RefreshRouter for GatedRouter {
    fn mark_all_dirty(&self, reason: &str) -> usize {
        let gate = lock(&self.gate).take();
        if let Some((entered, release)) = gate {
            let _ = entered.send(());
            // The bound only limits how long a broken queue gets to
            // overtake; a correct queue cannot pass while this runs.
            let _ = release.recv_timeout(Duration::from_millis(300));
        }
        self.probe.event(format!("mark-all-dirty:{reason}"));
        2
    }
    fn mark_project_dirty(&self, project_key: &JsString, reason: &str) -> usize {
        self.probe.event(format!(
            "mark-project-dirty:{}:{reason}",
            String::from_utf16_lossy(project_key.units())
        ));
        1
    }
    fn mark_session_dirty(&self, _: &JsString, _: &str) -> bool {
        false
    }
    fn cached_session_count(&self) -> usize {
        0
    }
    fn snapshot_session(&self, _: &JsString) -> Option<JsValue> {
        None
    }
}

/// The SESSION_BUSY gate on the fully configured assembly versus the
/// #1320 production matrix: apply refuses during an active turn, update
/// does not gate (the TS truth), and the matrix assembly answers the three
/// registry-bound methods with CAPABILITY_UNAVAILABLE while resolve and
/// the two reloads stay live.
#[tokio::test]
async fn settings_busy_gate_matrix() {
    let stack = stack(&["a".to_string(), "b".to_string()]);
    stack.harness.run_setup("tree:busy").await;
    stack.harness.run_setup("ensure-session:b").await;
    stack.harness.run_setup("begin-turn:b:run-1").await;
    let primary = stack.connections.add();
    let counter = Counter::default();
    let apply_params =
        pilotdeck_protocol::parse_json(r#"{"sessionKey":"b","settings":{"effortLevel":"low"}}"#)
            .expect("#1320 busy-gate: the apply params must parse");
    let busy = call(
        &stack.requests,
        &primary,
        Arc::new(stack.harness.gateway()),
        counter.next(),
        "apply_flag_settings",
        apply_params,
    )
    .await;
    match busy {
        Err(error) => {
            assert_eq!(
                error.code, "SESSION_BUSY",
                "#1320 busy-gate: apply_flag_settings must refuse during an active turn"
            );
            assert_eq!(
                String::from_utf16_lossy(error.message.units()),
                "Cannot apply flag settings during an active turn.",
                "#1320 busy-gate: the SESSION_BUSY message must match TS"
            );
        }
        Ok(_) => panic!("#1320 busy-gate: apply_flag_settings must refuse during an active turn"),
    }
    // The TS updateSettings closure has no busy gate: an update during the
    // same active turn succeeds.
    let update_params = pilotdeck_protocol::parse_json(
        r#"{"source":"localSettings","settings":{"agent":{"maxContextTokens":100000}}}"#,
    )
    .expect("#1320 busy-gate: the update params must parse");
    let update = call(
        &stack.requests,
        &primary,
        Arc::new(stack.harness.gateway()),
        counter.next(),
        "update_settings",
        update_params,
    )
    .await;
    assert_eq!(
        changed_paths(&update, "#1320 busy-gate: the update"),
        ["agent.maxContextTokens"],
        "#1320 busy-gate: update_settings must not gate on the active turn (the TS closure has no busy gate)"
    );
    let reload = call(
        &stack.requests,
        &primary,
        Arc::new(stack.harness.gateway()),
        counter.next(),
        "reload_config",
        JsValue::Undefined,
    )
    .await;
    assert_eq!(
        changed_paths(&reload, "#1320 busy-gate: the reload"),
        Vec::<String>::new(),
        "#1320 busy-gate: reload_config must not gate on the active turn"
    );

    // The production matrix assembly: the three registry-bound methods
    // answer CAPABILITY_UNAVAILABLE, resolve and the two reloads stay
    // live over the real store.
    let root = stack.harness._scratch.0.clone();
    let (store, config) = own_store(&root, "matrix-busy", &stack.probe);
    let native = native_stack(store, &stack);
    let matrix_connection = stack.connections.add();
    for (method, params) in [
        (
            "apply_flag_settings",
            r#"{"sessionKey":"a","settings":{"effortLevel":"low"}}"#,
        ),
        (
            "update_settings",
            r#"{"source":"localSettings","settings":{}}"#,
        ),
        ("reload_output_styles", r#"{"projectKey":"/proj-A"}"#),
    ] {
        let params = pilotdeck_protocol::parse_json(params)
            .unwrap_or_else(|_| panic!("#1320 busy-gate: the {method} params must parse"));
        let response = call(
            &stack.requests,
            &matrix_connection,
            native.gateway.clone(),
            counter.next(),
            method,
            params,
        )
        .await;
        match response {
            Err(error) => {
                assert_eq!(
                    error.code, "CAPABILITY_UNAVAILABLE",
                    "#1320 busy-gate: the matrix assembly must refuse {method}"
                );
                assert_eq!(
                    String::from_utf16_lossy(error.message.units()),
                    format!("{method} is unavailable."),
                    "#1320 busy-gate: the matrix refusal message must match TS"
                );
            }
            Ok(_) => panic!("#1320 busy-gate: the matrix assembly must refuse {method}"),
        }
    }
    let resolve = call(
        &stack.requests,
        &matrix_connection,
        native.gateway.clone(),
        counter.next(),
        "resolve_settings",
        JsValue::Undefined,
    )
    .await;
    assert_eq!(
        resolved_version(&resolve, "#1320 busy-gate: the matrix resolve"),
        1.0,
        "#1320 busy-gate: the matrix assembly must resolve over the real store"
    );
    std::fs::write(&config, OTHER_MODEL)
        .expect("#1320 busy-gate: the changed config must be written");
    let matrix_reload = call(
        &stack.requests,
        &matrix_connection,
        native.gateway.clone(),
        counter.next(),
        "reload_config",
        JsValue::Undefined,
    )
    .await;
    assert_eq!(
        changed_paths(&matrix_reload, "#1320 busy-gate: the matrix reload"),
        ["agent.model.id", "agent.model.model"],
        "#1320 busy-gate: the matrix assembly must reload the real store"
    );
    let extensions = call(
        &stack.requests,
        &matrix_connection,
        native.gateway.clone(),
        counter.next(),
        "reload_extensions",
        JsValue::Undefined,
    )
    .await;
    assert_eq!(
        changed_paths(&extensions, "#1320 busy-gate: the matrix extensions"),
        Vec::<String>::new(),
        "#1320 busy-gate: the matrix assembly must serve reload_extensions"
    );
}

/// The isolation refusal the full route answers for a `projectKey` it does
/// not serve: code and message must match the admission gate verbatim.
fn assert_isolated(result: Result<JsValue, RequestError>) {
    match result {
        Err(error) => {
            assert_eq!(
                error.code, "gateway_request_failed",
                "#1320 routing: the isolation refusal code"
            );
            assert_eq!(
                String::from_utf16_lossy(error.message.units()),
                "The isolated native process serves only its configured project.",
                "#1320 routing: the isolation refusal message"
            );
        }
        Ok(_) => {
            panic!("#1320 routing: an unserved projectKey must be refused by the admission gate")
        }
    }
}

/// #1320 routing: the S4 replay above drives [`Requests::settings_request`]
/// — the gateway layer after the #1305 registry has already selected the
/// settings family — so it never crosses the isolation admission gate or
/// the registry lookup. Through the full `GatewayRequests::request` route:
/// an own-project `projectKey` reaches the family and answers exactly like
/// the gateway layer, while a foreign or non-string `projectKey` is
/// refused before the family runs — so the golden's foreign-key rows
/// (`styles-closes-matching` `"/proj-A"`, `extensions-project` `"/p"`) and
/// the registered `U7-EXTENSIONS-RELOAD-STRING-COERCION-001` divergence
/// (`projectKey:5`) hold at the gateway layer only.
#[tokio::test]
async fn settings_methods_route_through_the_native_request_gate() {
    let stack = stack(&[]);
    stack.harness.run_setup("tree:routing").await;
    let mut requests = stack.requests.clone();
    requests.settings = Arc::new(stack.harness.gateway());
    let own_project = requests.options.project.to_string_lossy().into_owned();

    // Own project: the route answers exactly like the gateway layer does.
    let resolve_params =
        pilotdeck_protocol::parse_json(&format!(r#"{{"projectKey":"{own_project}"}}"#))
            .expect("#1320 routing: the own-project resolve params must parse");
    let routed = requests
        .request(JsString::from("resolve_settings"), resolve_params.clone())
        .await;
    let gateway = requests
        .settings_request("resolve_settings".to_owned(), resolve_params)
        .await;
    match (&routed, &gateway) {
        (Ok(routed), Ok(gateway)) => assert_eq!(
            as_serde(routed),
            as_serde(gateway),
            "#1320 routing: the route must answer an own-project resolve_settings like the gateway layer"
        ),
        _ => panic!(
            "#1320 routing: an own-project resolve_settings must succeed on both layers, got {routed:?} and {gateway:?}"
        ),
    }
    let reload_params =
        pilotdeck_protocol::parse_json(&format!(r#"{{"projectKey":"{own_project}"}}"#))
            .expect("#1320 routing: the own-project reload params must parse");
    let routed = requests
        .request(JsString::from("reload_extensions"), reload_params)
        .await;
    assert!(
        changed_paths(&routed, "#1320 routing: the own-project reload_extensions").is_empty(),
        "#1320 routing: an own-project reload_extensions must succeed through the route"
    );

    // Foreign key: refused by the gate before the family runs.
    let foreign = pilotdeck_protocol::parse_json(r#"{"projectKey":"/proj-A"}"#)
        .expect("#1320 routing: the foreign params must parse");
    let refused = requests
        .request(JsString::from("resolve_settings"), foreign)
        .await;
    assert_isolated(refused);

    // Non-string key: refused by the gate before the `String()` coercion of
    // U7-EXTENSIONS-RELOAD-STRING-COERCION-001 can run on the native route.
    let numeric = pilotdeck_protocol::parse_json(r#"{"projectKey":5}"#)
        .expect("#1320 routing: the numeric params must parse");
    let refused = requests
        .request(JsString::from("reload_extensions"), numeric)
        .await;
    assert_isolated(refused);
}

/// Regeneration guard for the TS golden; run once with `PDRS_BASELINE` set.
#[test]
#[ignore = "regenerates the TS fixture; needs PDRS_BASELINE and Node"]
fn native_settings_fixture_regenerates_identically_from_ts() {
    crate::test_baseline::regenerates_identically("native-settings");
}

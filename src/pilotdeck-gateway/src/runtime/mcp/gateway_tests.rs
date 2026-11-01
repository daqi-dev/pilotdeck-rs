//! Replay of the real-TS MCP control gateway oracle
//! (`tests/fixtures/mcp-gateway/generate.mts`, #1215). Every matrix row is
//! one replay step: the same setup actions run against the same three Rust
//! layers (the #1011 session owner, the #347 `SdkMcpConfig` registry mirror,
//! and this module's gateway adapter), then the reconstructed WS response
//! frame, the event stream and the post state must match the TS bytes.
//!
//! The TS golden also carries `assemblyCases` (a gateway assembled without
//! the four options answers CAPABILITY_UNAVAILABLE; an unknown method throws
//! "Unknown gateway method …"). Both belong to the wiring layer this
//! component deliberately does not touch, so they are recorded, not
//! replayed; the dispatch contract they imply (`Some` for exactly the four
//! methods, `None` otherwise) has its own dedicated test below.
use super::gateway::{McpGateway, McpGatewayOptions};
use super::sdk::{
    SdkMcpCloseSession, SdkMcpConfig, SdkMcpConfigOptions, SdkMcpError, SdkMcpFuture,
    SdkMcpProject, SdkMcpProjectResolver, SdkMcpSessionPlugins,
};
use crate::config::permission::GatewaySessionPermissionRuleSetRegistry;
use crate::runtime::project::{DisposeFuture, RuntimeError, RuntimeResult};
use crate::runtime::refresh::BoxFuture;
use crate::runtime::session::{
    EvictionReason, LiveSession, LiveSessionHandle, SessionContext, SessionFactory, SessionOwner,
    SessionOwnerOptions,
};
use crate::runtime::{plugins, sdk_session};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::vec;

const ORACLE: &str = include_str!("../../../tests/fixtures/mcp-gateway/oracle.json");

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

static UNDEFINED: JsValue = JsValue::Undefined;
fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    value.get(name).unwrap_or(&UNDEFINED)
}

fn string(value: &JsValue, what: &str) -> String {
    value
        .as_string()
        .unwrap_or_else(|| panic!("#1215 {what} must be a string"))
        .to_utf8()
        .unwrap_or_else(|_| panic!("#1215 {what} must be valid UTF-8"))
}

fn as_serde(value: &JsValue) -> serde_json::Value {
    let text = stringify_json(value)
        .expect("#1215 replay value must be serializable")
        .expect("#1215 replay value must not be undefined");
    serde_json::from_str(&text).expect("#1215 replay value must be valid JSON")
}

/// The scenario keys of the TS golden, in `oracle.json.setupKeys` order.
fn scenario_keys(oracle: &JsValue) -> Vec<String> {
    match field(oracle, "setupKeys") {
        JsValue::Array(items) => items
            .iter()
            .map(|key| string(key, "setupKeys entry"))
            .collect(),
        _ => panic!("#1215 oracle must carry its setupKeys list"),
    }
}

// ─────────────────────────────────────────────────────────── fixture fakes

struct FakeSession {
    key: String,
    probe: Probe,
}
impl LiveSession for FakeSession {
    fn snapshot(&self) -> JsValue {
        parse_json(&format!(
            "{{\"sessionId\":\"sess-{}\",\"messages\":[]}}",
            self.key
        ))
        .expect("#1215 fake session snapshot must parse")
    }
    fn abort(&self, _reason: &JsString) -> RuntimeResult {
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        let key = self.key.clone();
        let fails = self.key == "f";
        self.probe.event(format!("dispose:{key}"));
        Box::pin(async move {
            // The TS fixture's session throws `Error('fixture dispose
            // failure')` for key `f`; the real #1011 `LiveSessionHandle`
            // aggregation (session/handle.rs, the AgentHandle.ts:290
            // mirror) turns it into the golden's `Failed to dispose agent
            // handle.` text. The fake feeds the same distinct underlying
            // error, so the close-fault rows are produced by the real
            // aggregation instead of echoing the expected bytes (fix-r1
            // P2-3).
            if fails {
                Err(RuntimeError::new("fixture dispose failure"))
            } else {
                Ok(())
            }
        })
    }
}

struct ProbeState {
    events: Mutex<Vec<String>>,
    now: AtomicI64,
    turn_on_next_refresh: Mutex<Option<(String, String)>>,
    owner: Mutex<Option<SessionOwner>>,
    plugins: Mutex<Vec<(String, Vec<Arc<plugins::LoadedPlugin>>)>>,
}
#[derive(Clone)]
struct Probe(Arc<ProbeState>);
impl Probe {
    fn new() -> Self {
        Self(Arc::new(ProbeState {
            events: Mutex::new(Vec::new()),
            now: AtomicI64::new(1),
            turn_on_next_refresh: Mutex::new(None),
            owner: Mutex::new(None),
            plugins: Mutex::new(Vec::new()),
        }))
    }
    fn event(&self, event: impl Into<String>) {
        lock(&self.0.events).push(event.into());
    }
    fn events(&self) -> Vec<String> {
        lock(&self.0.events).clone()
    }
    fn clear_events(&self) {
        lock(&self.0.events).clear();
    }
    fn owner(&self) -> SessionOwner {
        lock(&self.0.owner)
            .clone()
            .expect("#1215 probe owner must be installed before use")
    }
    fn install_owner(&self, owner: SessionOwner) {
        *lock(&self.0.owner) = Some(owner);
    }
    fn owner_options(&self) -> SessionOwnerOptions {
        let probe = self.clone();
        let evict = self.clone();
        SessionOwnerOptions {
            factory: Arc::new(self.clone()),
            now_ms: Arc::new(move || probe.0.now.load(Ordering::SeqCst)),
            idle_timeout_ms: 60 * 60 * 1000,
            idle_sweep_interval_ms: 0,
            on_session_idle_evict: None,
            on_evict: Some(Arc::new(move |key, reason| {
                let reason = match reason {
                    EvictionReason::Idle => "idle",
                    EvictionReason::Closed => "closed",
                    EvictionReason::DirtyRecreate => "dirty_recreate",
                    EvictionReason::Shutdown => "shutdown",
                };
                evict.event(format!("evict:{}:{reason}", key.to_utf8().unwrap()));
            })),
        }
    }
}
impl SessionFactory for Probe {
    fn create(
        &self,
        context: SessionContext,
        _previous: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        let probe = self.clone();
        let key = context
            .session_key
            .to_utf8()
            .unwrap_or_else(|_| panic!("#1215 session key must be valid UTF-8"))
            .to_string();
        Box::pin(async move {
            probe.event(format!("create:{key}"));
            Ok(LiveSessionHandle::new(
                Arc::new(FakeSession {
                    key: key.clone(),
                    probe: probe.clone(),
                }),
                None,
            ))
        })
    }
}

/// `runtime.pluginRuntime`: refresh records (and can begin a turn, the S3
/// cell), snapshot records and returns no plugins.
struct FakeProject {
    probe: Probe,
}
impl SdkMcpProject for FakeProject {
    fn project_root(&self) -> JsString {
        "/pdrs-fixture/project".into()
    }
    fn refresh<'a>(&'a self) -> SdkMcpFuture<'a, Result<(), SdkMcpError>> {
        let probe = self.probe.clone();
        Box::pin(async move {
            probe.event("refresh");
            let turn = lock(&probe.0.turn_on_next_refresh).take();
            if let Some((key, run)) = turn {
                probe.owner().begin_turn(&key.into(), &run.into());
            }
            Ok(())
        })
    }
    fn snapshot<'a>(&'a self) -> SdkMcpFuture<'a, Vec<Arc<plugins::LoadedPlugin>>> {
        let probe = self.probe.clone();
        Box::pin(async move {
            probe.event("snapshot");
            Vec::new()
        })
    }
}

struct FakeResolver {
    probe: Probe,
    project: Arc<FakeProject>,
}
impl SdkMcpProjectResolver for FakeResolver {
    fn resolve<'a>(
        &'a self,
        project_key: Option<&'a JsString>,
    ) -> SdkMcpFuture<'a, Result<Arc<dyn SdkMcpProject>, SdkMcpError>> {
        let probe = self.probe.clone();
        let recorded = project_key
            .map(|key| string(&JsValue::String(key.clone()), "projectKey"))
            .unwrap_or_else(|| "null".to_string());
        let project: Arc<dyn SdkMcpProject> = self.project.clone();
        Box::pin(async move {
            probe.event(format!("resolve:{recorded}"));
            Ok(project)
        })
    }
}

struct SessionPluginsSource {
    probe: Probe,
}
impl SdkMcpSessionPlugins for SessionPluginsSource {
    fn plugins(&self, session_key: &JsValue) -> Vec<Arc<plugins::LoadedPlugin>> {
        let Some(key) = session_key.as_string().and_then(|key| key.to_utf8().ok()) else {
            return Vec::new();
        };
        lock(&self.probe.0.plugins)
            .iter()
            .find(|(existing, _)| existing == &key)
            .map(|(_, plugins)| plugins.clone())
            .unwrap_or_default()
    }
}

struct FixtureEnvironment;
impl super::sdk::SdkMcpEnvironment for FixtureEnvironment {
    fn env(&self, _name: &JsString) -> Option<JsString> {
        None
    }
    fn home(&self) -> JsString {
        "/pdrs-fixture/home".into()
    }
}

// ─────────────────────────────────────────────────────────────── harness

struct Harness {
    gateway: McpGateway,
    probe: Probe,
    config: Arc<SdkMcpConfig>,
    leases: Vec<(String, crate::config::permission::Lease)>,
    keys: Vec<String>,
}

fn harness(keys: &[String]) -> Harness {
    let probe = Probe::new();
    let owner = SessionOwner::new(probe.owner_options());
    probe.install_owner(owner.clone());

    let permission_rule_sets = Arc::new(GatewaySessionPermissionRuleSetRegistry::new());
    let leases = keys
        .iter()
        .map(|key| {
            (
                key.clone(),
                permission_rule_sets.acquire(&JsString::from(key.as_str()), None),
            )
        })
        .collect();

    // InProcessGateway.closeSession: `await router.close(key)` first, then
    // the interaction coordinator's closeSession — the record only lands
    // when the router close succeeded, matching the TS event order.
    let close_owner = owner.clone();
    let close_probe = probe.clone();
    let close_session: SdkMcpCloseSession = Arc::new(move |session_key, reason| {
        let key = match &session_key {
            JsValue::String(key) => key.clone(),
            _ => JsString::from(""),
        };
        let reason = reason.to_string();
        let owner = close_owner.clone();
        let probe = close_probe.clone();
        Box::pin(async move {
            owner
                .close(&key)
                .await
                .map_err(|error| SdkMcpError::plain("Error", JsString::from(error.message)))?;
            probe.event(format!(
                "close-session:{}:{reason}",
                key.to_utf8()
                    .unwrap_or_else(|_| panic!("#1215 session key must be UTF-8"))
            ));
            Ok(())
        })
    });

    let config = Arc::new(SdkMcpConfig::new(SdkMcpConfigOptions {
        resolver: Arc::new(FakeResolver {
            probe: probe.clone(),
            project: Arc::new(FakeProject {
                probe: probe.clone(),
            }),
        }),
        close_session,
        session_plugins: Arc::new(SessionPluginsSource {
            probe: probe.clone(),
        }),
        permission_rule_sets,
        environment: Arc::new(FixtureEnvironment),
        projects: sdk_session::SdkSessionProjects::shared(),
    }));

    let gateway = McpGateway::new(McpGatewayOptions {
        owner,
        config: config.clone(),
    });
    Harness {
        gateway,
        probe,
        config,
        leases,
        keys: keys.to_vec(),
    }
}

impl Harness {
    /// The setup vocabulary of the golden; each step is executed exactly as
    /// `generate.mts` executes it on the TS side.
    async fn run_setup(&self, step: &str) {
        let parts: Vec<&str> = step.split(':').collect();
        let (action, key, arg) = match parts.as_slice() {
            [action, key] => (*action, (*key).to_string(), String::new()),
            [action, key, arg] => (*action, (*key).to_string(), (*arg).to_string()),
            _ => panic!("#1215 setup step must be action:key[:arg]: {step}"),
        };
        assert!(
            self.keys.iter().any(|existing| existing == &key),
            "#1215 setup key must be a scenario key: {step}"
        );
        let key = JsString::from(key.as_str());
        match action {
            "ensure-session" => {
                self.probe
                    .owner()
                    .get_or_create(SessionContext {
                        session_key: key.clone(),
                        project_key: Some("/p".into()),
                        channel_key: JsString::from(""),
                        allowed_tools: None,
                        disallowed_tools: None,
                    })
                    .await
                    .expect("#1215 ensure-session must create a session");
            }
            "begin-turn" => {
                let reservation = self
                    .probe
                    .owner()
                    .begin_turn(&key, &JsString::from(arg.as_str()));
                assert!(
                    reservation.is_some(),
                    "#1215 begin-turn must reserve: {step}"
                );
            }
            "seed-set" => {
                let mut servers = JsObject::new();
                servers.insert(
                    JsString::from(arg.as_str()),
                    parse_json(r#"{"type":"stdio","command":"node"}"#)
                        .expect("#1215 seed config must parse"),
                );
                let mut input = JsObject::new();
                input.insert("sessionKey".into(), JsValue::String(key.clone()));
                input.insert("servers".into(), JsValue::Object(servers));
                self.gateway
                    .set_mcp_servers(&JsValue::Object(input))
                    .await
                    .expect("#1215 seed-set must succeed");
            }
            "session-plugin" => {
                let mut mcp_servers = JsObject::new();
                mcp_servers.insert(
                    JsString::from(arg.as_str()),
                    parse_json(r#"{"type":"stdio","command":"node"}"#)
                        .expect("#1215 plugin server must parse"),
                );
                lock(&self.probe.0.plugins).push((
                    key.to_utf8()
                        .unwrap_or_else(|_| panic!("#1215 session key must be UTF-8"))
                        .to_string(),
                    vec![Arc::new(plugins::LoadedPlugin {
                        name: "fixture-plugin".to_string(),
                        path: PathBuf::from("/pdrs-fixture/plugin"),
                        source: plugins::PluginSource::Project,
                        commands: Vec::new(),
                        skills: Vec::new(),
                        output_styles: Vec::new(),
                        hooks_config: None,
                        mcp_servers: Some(JsValue::Object(mcp_servers)),
                        lsp_servers: None,
                        dispose: None,
                    })],
                ));
            }
            "refresh-begins-turn" => {
                *lock(&self.probe.0.turn_on_next_refresh) = Some((
                    key.to_utf8()
                        .unwrap_or_else(|_| panic!("#1215 session key must be UTF-8"))
                        .to_string(),
                    arg,
                ));
            }
            other => panic!("#1215 unknown setup action {other}"),
        }
    }

    /// The post-state projection of the golden: active turns, live sessions,
    /// the MCP server tables and the live ask rules per scenario key.
    fn post_state(&self) -> JsValue {
        let owner = self.probe.owner();
        let turns = JsValue::Array(
            self.keys
                .iter()
                .filter(|key| owner.has_active_turn(&JsString::from(key.as_str())))
                .map(|key| JsValue::String(JsString::from(key.as_str())))
                .collect(),
        );
        let mut sessions = JsObject::new();
        let mut servers = JsObject::new();
        let mut ask = JsObject::new();
        let state = self.config.snapshot_state();
        let state_servers = match field(&state, "servers") {
            JsValue::Array(rows) => rows.clone(),
            _ => panic!("#1215 config state must carry a servers table"),
        };
        for key in &self.keys {
            let js_key = JsString::from(key.as_str());
            sessions.insert(
                js_key.clone(),
                JsValue::Bool(owner.current_handle(&js_key).is_some()),
            );
            // snapshot_state's servers table is [[key, [[name, {config,
            // enabled}] …]] …]; a session without an entry stays empty.
            let entries = state_servers
                .iter()
                .find(|row| match row {
                    JsValue::Array(pair) => pair
                        .first()
                        .and_then(JsValue::as_string)
                        .is_some_and(|existing| existing == &js_key),
                    _ => false,
                })
                .and_then(|row| match row {
                    JsValue::Array(pair) => pair.get(1).cloned(),
                    _ => None,
                })
                .unwrap_or(JsValue::Array(Vec::new()));
            let pairs = match &entries {
                JsValue::Array(items) => items
                    .iter()
                    .map(|entry| match entry {
                        JsValue::Array(pair) => JsValue::Array(vec![
                            pair.first().cloned().unwrap_or(JsValue::Undefined),
                            field(
                                &pair.get(1).cloned().unwrap_or(JsValue::Undefined),
                                "enabled",
                            )
                            .clone(),
                        ]),
                        _ => JsValue::Undefined,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            servers.insert(js_key.clone(), JsValue::Array(pairs));
            let rules = self
                .leases
                .iter()
                .find(|(existing, _)| existing == key)
                .map(|(_, lease)| lease.rules().snapshot())
                .unwrap_or_default();
            ask.insert(
                js_key,
                JsValue::Array(rules.ask.iter().map(|rule| rule.to_value()).collect()),
            );
        }
        let mut object = JsObject::new();
        object.insert("turns".into(), turns);
        object.insert("sessions".into(), JsValue::Object(sessions));
        object.insert("servers".into(), JsValue::Object(servers));
        object.insert("ask".into(), JsValue::Object(ask));
        JsValue::Object(object)
    }
}

// ─────────────────────────────────────────────────────── the replay tests

#[tokio::test]
async fn mcp_gateway_replays_the_real_ts_oracle() {
    let oracle = crate::test_baseline::oracle(ORACLE);
    let keys = scenario_keys(&oracle);
    let cases = match field(&oracle, "cases") {
        JsValue::Array(items) => items.clone(),
        _ => panic!("#1215 oracle must carry its case rows"),
    };
    assert_eq!(
        cases.len(),
        44,
        "#1215 the oracle must carry the full 44-row matrix"
    );
    let harness = harness(&keys);
    let mut failures: Vec<String> = Vec::new();
    for case in &cases {
        let id = string(field(case, "id"), "case.id");
        let method = string(field(case, "method"), "case.method");
        for step in match field(case, "setup") {
            JsValue::Array(items) => items.clone(),
            _ => panic!("#1215 case {id} must carry a setup list"),
        } {
            harness.run_setup(&string(&step, "case.setup step")).await;
        }
        harness.probe.clear_events();
        let input = field(case, "params").clone();
        let dispatch = harness.gateway.dispatch(&method, &input).await;
        let result = dispatch.unwrap_or_else(|| {
            panic!("#1215 case {id}: dispatch must own the four control methods")
        });
        let envelope = match result {
            Ok(value) => {
                let mut object = JsObject::new();
                object.insert("type".into(), JsValue::String("response".into()));
                object.insert("ok".into(), JsValue::Bool(true));
                object.insert("result".into(), value);
                JsValue::Object(object)
            }
            Err(error) => {
                let mut error_object = JsObject::new();
                match error.code() {
                    Some(code) => {
                        error_object.insert("code".into(), JsValue::String(code.into()));
                    }
                    None => {
                        error_object.insert(
                            "code".into(),
                            JsValue::String("gateway_request_failed".into()),
                        );
                    }
                }
                error_object.insert("message".into(), JsValue::String(error.message().clone()));
                let mut object = JsObject::new();
                object.insert("type".into(), JsValue::String("response".into()));
                object.insert("ok".into(), JsValue::Bool(false));
                object.insert("error".into(), JsValue::Object(error_object));
                JsValue::Object(object)
            }
        };
        // A divergent row does not stop the replay: every row is compared so
        // one run reports the whole matrix (the negative controls bind one
        // bound marker per matrix cell, so all of them must be reachable in
        // the single panic payload).
        let expected = as_serde(field(case, "response"));
        let actual = as_serde(&envelope);
        if actual != expected {
            failures.push(format!(
                "#1215 case {id}: the reconstructed WS response must match TS\n  expected: {expected}\n  actual:   {actual}"
            ));
        }
        let expected_events = match field(case, "events") {
            JsValue::Array(items) => items
                .iter()
                .map(|event| string(event, "case.events entry"))
                .collect::<Vec<_>>(),
            _ => panic!("#1215 case {id} must carry an events list"),
        };
        let actual_events = harness.probe.events();
        if actual_events != expected_events {
            failures.push(format!(
                "#1215 case {id}: the event stream must match TS\n  expected: {expected_events:?}\n  actual:   {actual_events:?}"
            ));
        }
        let expected_post = as_serde(field(case, "post"));
        let actual_post = as_serde(&harness.post_state());
        if actual_post != expected_post {
            failures.push(format!(
                "#1215 case {id}: the post state must match TS\n  expected: {expected_post}\n  actual:   {actual_post}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "#1215 {} matrix row(s) diverged from the TS oracle:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The dispatch entry owns exactly the four control methods; everything else
/// is the caller's business (the WS layer's unknown-method error and the
/// CAPABILITY_UNAVAILABLE answers live in the wiring layer, recorded in the
/// golden's `assemblyCases`).
#[tokio::test]
async fn mcp_gateway_dispatch_returns_none_for_non_control_methods() {
    let keys = vec!["s".to_string()];
    let harness = harness(&keys);
    let input = parse_json(r#"{"sessionKey":"s"}"#).expect("#1215 dispatch input must parse");
    for method in [
        "mcp_server_status",
        "submit_turn",
        "mcp_server_pause",
        "set_mcp_servers_extra",
        "",
    ] {
        let dispatch = harness.gateway.dispatch(method, &input).await;
        assert!(
            dispatch.is_none(),
            "#1215 dispatch must not own non-control method {method}"
        );
    }
    for method in [
        "set_mcp_servers",
        "mcp_server_reconnect",
        "mcp_server_toggle",
        "set_mcp_permission_mode_override",
    ] {
        let dispatch = harness.gateway.dispatch(method, &input).await;
        assert!(
            dispatch.is_some(),
            "#1215 dispatch must own the control method {method}"
        );
    }
    // The two void controls answer the WS-layer `{ ok: true }` shape even
    // though the error paths of the same methods keep their own envelope.
    let unknown = parse_json(r#"{"sessionKey":"s","serverName":"missing"}"#)
        .expect("#1215 dispatch input must parse");
    let result = harness
        .gateway
        .dispatch("mcp_server_reconnect", &unknown)
        .await
        .expect("#1215 dispatch must own mcp_server_reconnect");
    let error = result.expect_err("#1215 reconnect of an unknown server must fail");
    assert_eq!(
        error.code(),
        Some("MCP_SERVER_NOT_FOUND"),
        "#1215 reconnect of an unknown server must report MCP_SERVER_NOT_FOUND"
    );
}

/// Regeneration guard for the TS golden; run once with `PDRS_BASELINE` set.
#[test]
#[ignore = "regenerates the TS fixture; needs PDRS_BASELINE and Node"]
fn mcp_gateway_fixture_regenerates_identically_from_ts() {
    crate::test_baseline::regenerates_identically("mcp-gateway");
}

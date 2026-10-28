//! Replay of the real-TS SDK MCP configuration oracle
//! (`tests/fixtures/sdk-mcp/generate.mts`). Each step compares the method
//! result (or the exact `{ name, code, message }` error), the internal state
//! projection, the live ask/allow arrays, and the cumulative
//! resolve/refresh/closeSession call sequence.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::{
    config::{
        Env,
        permission::{ConfiguredRuleSet, GatewaySessionPermissionRuleSetRegistry},
    },
    runtime::{
        mcp::sdk::{
            SdkMcpCloseSession, SdkMcpConfig, SdkMcpConfigOptions, SdkMcpEnvironment, SdkMcpError,
            SdkMcpFuture, SdkMcpProject, SdkMcpProjectResolver, SdkMcpSessionPlugins,
        },
        plugins::{LoadedPlugin, PluginRuntime, PluginRuntimeOptions, PluginSource},
        sdk_session::{
            ResolvedSdkRuntime, SdkRuntimeResolver, SdkSessionError, SdkSessionOptions,
            SdkSessionProjects, SdkSessionProjectsTable, SdkSessionRegistry, SdkTaskBudget,
        },
    },
    storage::TaskBudgetError,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

const ORACLE: &str = include_str!("fixtures/sdk-mcp/oracle.json");

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn text(value: &JsValue) -> String {
    stringify_json(value)
        .expect("serializable value")
        .expect("value is not undefined")
}

static UNDEFINED: JsValue = JsValue::Undefined;
fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    value.get(name).unwrap_or(&UNDEFINED)
}

fn string(value: &JsValue, what: &str) -> JsString {
    value
        .as_string()
        .cloned()
        .unwrap_or_else(|| panic!("{what} must be a string"))
}

fn array<'a>(value: &'a JsValue, what: &str) -> &'a [JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => panic!("{what} must be an array"),
    }
}

/// Fixture-only state encoding: the oracle marks `undefined` explicitly so a
/// missing `enabled` member is not confused with a JSON omission.
fn encode_undefined(value: &JsValue, depth: usize) -> JsValue {
    assert!(depth < 128, "fixture state is deeper than expected");
    match value {
        JsValue::Undefined => {
            let mut tag = JsObject::new();
            tag.insert("$".into(), JsValue::String("undefined".into()));
            JsValue::Object(tag)
        }
        JsValue::Array(items) => JsValue::Array(
            items
                .iter()
                .map(|item| encode_undefined(item, depth + 1))
                .collect(),
        ),
        JsValue::Object(object) => {
            let mut encoded = JsObject::new();
            for (name, value) in object.entries() {
                encoded.insert(name.clone(), encode_undefined(value, depth + 1));
            }
            JsValue::Object(encoded)
        }
        other => other.clone(),
    }
}

/// Fixture input encoding for explicit `undefined` (JSON cannot carry it).
fn decode_input(value: &JsValue, depth: usize) -> JsValue {
    assert!(depth < 128, "fixture input is deeper than expected");
    match value {
        JsValue::Array(items) => JsValue::Array(
            items
                .iter()
                .map(|item| decode_input(item, depth + 1))
                .collect(),
        ),
        JsValue::Object(object) => {
            if object.len() == 1
                && object
                    .get("$")
                    .and_then(JsValue::as_string)
                    .is_some_and(|tag| tag.equals_str("undefined"))
            {
                return JsValue::Undefined;
            }
            let mut decoded = JsObject::new();
            for (name, value) in object.entries() {
                decoded.insert(name.clone(), decode_input(value, depth + 1));
            }
            JsValue::Object(decoded)
        }
        other => other.clone(),
    }
}

fn plugin(value: &JsValue, what: &str) -> Arc<LoadedPlugin> {
    let name = string(field(value, "name"), &format!("{what}.name"))
        .to_utf8()
        .expect("plugin name");
    let mcp_servers = match field(value, "mcpServers") {
        JsValue::Undefined => None,
        other => Some(other.clone()),
    };
    Arc::new(LoadedPlugin {
        name,
        path: PathBuf::from("/pdrs-fixture/plugin"),
        source: PluginSource::Project,
        commands: Vec::new(),
        skills: Vec::new(),
        output_styles: Vec::new(),
        hooks_config: None,
        mcp_servers,
        lsp_servers: None,
        dispose: None,
    })
}

struct FakeProject {
    root: JsString,
    plugins: Vec<Arc<LoadedPlugin>>,
    calls: Arc<Mutex<Vec<JsValue>>>,
    refresh_error: Option<String>,
}
impl SdkMcpProject for FakeProject {
    fn project_root(&self) -> JsString {
        self.root.clone()
    }
    fn refresh<'a>(&'a self) -> SdkMcpFuture<'a, Result<(), SdkMcpError>> {
        lock(&self.calls).push(call("refresh", &[]));
        let error = self.refresh_error.clone();
        Box::pin(async move {
            match error {
                Some(message) => Err(SdkMcpError::plain("Error", message)),
                None => Ok(()),
            }
        })
    }
    fn snapshot<'a>(&'a self) -> SdkMcpFuture<'a, Vec<Arc<LoadedPlugin>>> {
        let plugins = self.plugins.clone();
        Box::pin(async move { plugins })
    }
}

struct FakeResolver {
    project: Arc<FakeProject>,
    calls: Arc<Mutex<Vec<JsValue>>>,
    error: Option<String>,
}
impl SdkMcpProjectResolver for FakeResolver {
    fn resolve<'a>(
        &'a self,
        project_key: Option<&'a JsString>,
    ) -> SdkMcpFuture<'a, Result<Arc<dyn SdkMcpProject>, SdkMcpError>> {
        let key = project_key.map_or(JsValue::Null, |key| JsValue::String(key.clone()));
        lock(&self.calls).push(call("resolve", &[key]));
        if let Some(message) = &self.error {
            let message = message.clone();
            return Box::pin(async move { Err(SdkMcpError::plain("Error", message)) });
        }
        let project: Arc<dyn SdkMcpProject> = self.project.clone();
        Box::pin(async move { Ok(project) })
    }
}

type SessionPluginMap = Vec<(JsString, Vec<Arc<LoadedPlugin>>)>;

struct SessionPluginsSource {
    entries: Arc<Mutex<SessionPluginMap>>,
}
impl SdkMcpSessionPlugins for SessionPluginsSource {
    fn plugins(&self, session_key: &JsValue) -> Vec<Arc<LoadedPlugin>> {
        let Some(session_key) = session_key.as_string() else {
            return Vec::new();
        };
        lock(&self.entries)
            .iter()
            .find(|(key, _)| key == session_key)
            .map(|(_, plugins)| plugins.clone())
            .unwrap_or_default()
    }
}

struct FixtureEnvironment;
impl SdkMcpEnvironment for FixtureEnvironment {
    fn env(&self, name: &JsString) -> Option<JsString> {
        match name.to_utf8().ok()?.as_str() {
            "PDRS_MCP_SPACE" => Some(" ".into()),
            _ => None,
        }
    }
    fn home(&self) -> JsString {
        // Matches generate.mts: the replacement's `$`` prefix pattern makes
        // `${userHome}` alone expand to the empty string.
        "$`".into()
    }
}

fn call(name: &str, args: &[JsValue]) -> JsValue {
    let mut items = vec![JsValue::String(name.into())];
    items.extend(args.iter().cloned());
    JsValue::Array(items)
}

fn close_call(session_key: &JsValue, reason: &str) -> JsValue {
    let mut input = JsObject::new();
    input.insert("sessionKey".into(), session_key.clone());
    input.insert("reason".into(), JsValue::String(reason.into()));
    call("closeSession", &[JsValue::Object(input)])
}

fn ask_value(lease: &pilotdeck_gateway::config::permission::Lease) -> JsValue {
    let rules = lease.rules().snapshot();
    let mut object = JsObject::new();
    object.insert(
        "ask".into(),
        JsValue::Array(rules.ask.iter().map(|rule| rule.to_value()).collect()),
    );
    object.insert(
        "allow".into(),
        JsValue::Array(rules.allow.iter().map(|rule| rule.to_value()).collect()),
    );
    JsValue::Object(object)
}

fn state_value(
    config: &SdkMcpConfig,
    lease: &pilotdeck_gateway::config::permission::Lease,
) -> JsValue {
    let state = encode_undefined(&config.snapshot_state(), 0);
    let ask = ask_value(lease);
    let mut object = JsObject::new();
    for name in ["servers", "overrides", "rules", "projects"] {
        object.insert(name.into(), field(&state, name).clone());
    }
    object.insert("ask".into(), field(&ask, "ask").clone());
    object.insert("allow".into(), field(&ask, "allow").clone());
    JsValue::Object(object)
}

async fn run_step(config: &SdkMcpConfig, step: &JsValue) -> JsValue {
    let input = &decode_input(field(step, "input"), 0);
    let result = match string(field(step, "op"), "step.op")
        .to_utf8()
        .unwrap()
        .as_str()
    {
        "set" => config
            .set_mcp_servers(input)
            .await
            .map(|value| ("set", value)),
        "status" => config
            .mcp_server_status(input)
            .await
            .map(|value| ("status", value)),
        "toggle" => config
            .toggle_mcp_server(input)
            .await
            .map(|()| ("void", JsValue::Null)),
        "reconnect" => config
            .reconnect_mcp_server(input)
            .await
            .map(|()| ("void", JsValue::Null)),
        "override" => config
            .set_mcp_permission_mode_override(input)
            .await
            .map(|value| ("override", value)),
        other => panic!("unknown step {other}"),
    };
    let mut object = JsObject::new();
    match result {
        Ok((kind, value)) => {
            object.insert(
                "ok".into(),
                if kind == "void" {
                    JsValue::Bool(true)
                } else {
                    value
                },
            );
        }
        Err(error) => {
            object.insert("error".into(), error.to_value());
        }
    }
    JsValue::Object(object)
}

fn test_config(
    environment: Arc<dyn SdkMcpEnvironment>,
) -> (SdkMcpConfig, Arc<Mutex<Vec<JsValue>>>) {
    test_config_with_projects(environment, SdkSessionProjects::shared())
}

fn test_config_with_projects(
    environment: Arc<dyn SdkMcpEnvironment>,
    projects: Arc<dyn SdkSessionProjectsTable>,
) -> (SdkMcpConfig, Arc<Mutex<Vec<JsValue>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let project = Arc::new(FakeProject {
        root: "/pdrs-fixture/project".into(),
        plugins: Vec::new(),
        calls: Arc::clone(&calls),
        refresh_error: None,
    });
    let resolver = Arc::new(FakeResolver {
        project,
        calls: Arc::clone(&calls),
        error: None,
    });
    let close_calls = Arc::clone(&calls);
    let close_session: SdkMcpCloseSession = Arc::new(move |session_key, reason| {
        lock(&close_calls).push(close_call(&session_key, reason));
        Box::pin(async { Ok(()) })
    });
    let config = SdkMcpConfig::new(SdkMcpConfigOptions {
        resolver,
        close_session,
        session_plugins: Arc::new(SessionPluginsSource {
            entries: Arc::new(Mutex::new(Vec::new())),
        }),
        permission_rule_sets: Arc::new(GatewaySessionPermissionRuleSetRegistry::new()),
        environment,
        projects,
    });
    (config, calls)
}

fn stdio_config(command: &str) -> JsValue {
    let mut config = JsObject::new();
    config.insert("type".into(), JsValue::String("stdio".into()));
    config.insert("command".into(), JsValue::String(command.into()));
    JsValue::Object(config)
}

fn set_input(session_key: &str, servers: &[(&str, JsValue)]) -> JsValue {
    let mut map = JsObject::new();
    for (name, value) in servers {
        map.insert((*name).into(), value.clone());
    }
    let mut input = JsObject::new();
    input.insert("sessionKey".into(), JsValue::String(session_key.into()));
    input.insert("servers".into(), JsValue::Object(map));
    JsValue::Object(input)
}

fn toggle_input(session_key: &str, server_name: &str, enabled: bool) -> JsValue {
    let mut input = JsObject::new();
    input.insert("sessionKey".into(), JsValue::String(session_key.into()));
    input.insert("serverName".into(), JsValue::String(server_name.into()));
    input.insert("enabled".into(), JsValue::Bool(enabled));
    JsValue::Object(input)
}

fn deep_stdio(depth: usize) -> JsValue {
    let mut nested = JsValue::Number(7.0);
    for _ in 0..depth {
        nested = JsValue::Array(vec![nested]);
    }
    let mut env = JsObject::new();
    env.insert("NESTED".into(), nested);
    let mut config = JsObject::new();
    config.insert("type".into(), JsValue::String("stdio".into()));
    config.insert("command".into(), JsValue::String("node".into()));
    config.insert("env".into(), JsValue::Object(env));
    JsValue::Object(config)
}

fn count_calls(calls: &Arc<Mutex<Vec<JsValue>>>, name: &str) -> usize {
    lock(calls)
        .iter()
        .filter(|call| match call {
            JsValue::Array(items) => items
                .first()
                .and_then(JsValue::as_string)
                .is_some_and(|value| value.equals_str(name)),
            _ => false,
        })
        .count()
}

fn enabled_for(config: &SdkMcpConfig, name: &str) -> Option<JsValue> {
    let state = config.snapshot_state();
    let sessions = match state.get("servers") {
        Some(JsValue::Array(items)) => items,
        _ => return None,
    };
    for session in sessions {
        let pair = match session {
            JsValue::Array(items) => items,
            _ => continue,
        };
        if pair.len() != 2 {
            continue;
        }
        let entries = match &pair[1] {
            JsValue::Array(items) => items,
            _ => continue,
        };
        for entry in entries {
            let pair = match entry {
                JsValue::Array(items) => items,
                _ => continue,
            };
            if pair.len() != 2 {
                continue;
            }
            let key = match &pair[0] {
                JsValue::String(key) => key,
                _ => continue,
            };
            if key.equals_str(name) {
                return pair[1].get("enabled").cloned();
            }
        }
    }
    None
}

#[derive(Default)]
struct Gate {
    entered: (Mutex<bool>, Condvar),
    release: (Mutex<bool>, Condvar),
}

struct GatedEnvironment {
    gate: Arc<Gate>,
}
impl SdkMcpEnvironment for GatedEnvironment {
    fn env(&self, name: &JsString) -> Option<JsString> {
        if !name.equals_str("GATE") {
            return None;
        }
        {
            let mut entered = lock(&self.gate.entered.0);
            *entered = true;
            self.gate.entered.1.notify_all();
        }
        let mut released = lock(&self.gate.release.0);
        while !*released {
            released = self.gate.release.1.wait(released).unwrap();
        }
        Some("node".into())
    }
    fn home(&self) -> JsString {
        "/pdrs-fixture/home".into()
    }
}

/// #347 fix-r1 (review P1-1): a shared-table stand-in that parks the armed
/// `set`/`delete` writes, so the clear × set interleaving is deterministic.
/// Reads pass through to the real table underneath.
#[derive(Default)]
struct GatedProjects {
    inner: SdkSessionProjects,
    armed: Mutex<bool>,
    set_gate: Gate,
    delete_gate: Gate,
}

impl SdkSessionProjectsTable for GatedProjects {
    fn project(&self, session_key: &JsValue) -> Option<JsString> {
        self.inner.project(session_key)
    }
    fn set(&self, session_key: &JsValue, project_root: JsString) {
        if *lock(&self.armed) {
            hold_at_gate(&self.set_gate, "shared-table set");
        }
        self.inner.set(session_key, project_root);
    }
    fn delete(&self, session_key: &JsValue) {
        if *lock(&self.armed) {
            hold_at_gate(&self.delete_gate, "shared-table delete");
        }
        self.inner.delete(session_key);
    }
    fn entries(&self) -> Vec<(JsValue, JsString)> {
        self.inner.entries()
    }
}

/// Park the calling thread inside a gated write until the coordinator
/// releases it; bounded so a lost release fails the test instead of hanging.
fn hold_at_gate(gate: &Gate, step: &str) {
    {
        let mut entered = lock(&gate.entered.0);
        *entered = true;
        gate.entered.1.notify_all();
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut released = lock(&gate.release.0);
    while !*released {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "#347 fix-r1 the gated {step} was never released"
        );
        let (guard, _timed_out) = gate
            .release
            .1
            .wait_timeout(released, remaining)
            .expect("gate condvar");
        released = guard;
    }
}

/// Bounded wait until a gated write has entered.
fn wait_entered(gate: &Gate, step: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut entered = lock(&gate.entered.0);
    while !*entered {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "#347 fix-r1 the gated {step} never ran"
        );
        let (guard, _timed_out) = gate
            .entered
            .1
            .wait_timeout(entered, remaining)
            .expect("gate condvar");
        entered = guard;
    }
}

fn release_gate(gate: &Gate) {
    *lock(&gate.release.0) = true;
    gate.release.1.notify_all();
}

#[tokio::test]
async fn deep_configs_are_rejected_without_state_or_close() {
    let (config, calls) = test_config(Arc::new(FixtureEnvironment));
    config
        .set_mcp_servers(&set_input("s", &[("a", stdio_config("node"))]))
        .await
        .unwrap();
    let before = text(&config.snapshot_state());
    lock(&calls).clear();
    // `structuredClone(config)` and `JSON.stringify(value)` both reject this
    // depth; the new-name path must not skip the serialization check. Neither
    // attempt may mutate state or call closeSession.
    for servers in [
        vec![("fresh", deep_stdio(2001))],
        vec![("a", deep_stdio(2001))],
    ] {
        let error = config
            .set_mcp_servers(&set_input("s", &servers))
            .await
            .unwrap_err();
        assert_eq!(error.name(), "RangeError");
        assert_eq!(error.code(), None);
        assert_eq!(
            String::from_utf16_lossy(error.message().units()),
            "Maximum call stack size exceeded"
        );
        assert_eq!(text(&config.snapshot_state()), before);
        assert_eq!(count_calls(&calls, "closeSession"), 0);
    }
}

// ──────────────────────────────────────────────────────── #347 follow-ups

/// `setMcpServersForSdk` input with an explicit (possibly non-string)
/// `projectKey`.
fn set_input_with_project_key(project_key: JsValue, servers: &[(&str, JsValue)]) -> JsValue {
    let mut object = JsObject::new();
    object.insert("sessionKey".into(), JsValue::String("s".into()));
    let mut map = JsObject::new();
    for (name, value) in servers {
        map.insert((*name).into(), value.clone());
    }
    object.insert("servers".into(), JsValue::Object(map));
    object.insert("projectKey".into(), project_key);
    JsValue::Object(object)
}

/// `mcpServerStatusForSdk` input with an explicit `projectKey`.
fn status_input_with_project_key(project_key: JsValue) -> JsValue {
    let mut input = JsObject::new();
    input.insert("projectKey".into(), project_key);
    JsValue::Object(input)
}

fn override_input(session_key: &str, server_name: &str, mode: JsValue) -> JsValue {
    let mut input = JsObject::new();
    input.insert("sessionKey".into(), JsValue::String(session_key.into()));
    input.insert("serverName".into(), JsValue::String(server_name.into()));
    input.insert("mode".into(), mode);
    JsValue::Object(input)
}

/// #347①: a non-string, non-nullish `projectKey` makes the baseline's
/// `this.resolve(projectKey)` (`node:path.resolve`) throw `TypeError` before
/// any state is read (Node v22.22.0 text, probe
/// `_tmp/20261026-issue-347/probe-347/project-key-type.out`).
#[tokio::test]
async fn non_string_project_key_throws_the_node_type_error() {
    let (config, calls) = test_config(Arc::new(FixtureEnvironment));
    config
        .set_mcp_servers(&set_input("s", &[("a", stdio_config("node"))]))
        .await
        .unwrap();
    let before = text(&config.snapshot_state());
    lock(&calls).clear();
    for (label, key, received) in [
        ("number", JsValue::Number(123.0), "type number (123)"),
        ("boolean", JsValue::Bool(true), "type boolean (true)"),
        (
            "object",
            JsValue::Object(JsObject::new()),
            "an instance of Object",
        ),
    ] {
        let expected =
            format!("The \"paths[0]\" argument must be of type string. Received {received}");
        let set_error = config
            .set_mcp_servers(&set_input_with_project_key(
                key.clone(),
                &[("a", stdio_config("node"))],
            ))
            .await
            .expect_err("#347 a non-string projectKey must reject setMcpServersForSdk");
        assert_eq!(
            set_error.name(),
            "TypeError",
            "#347 {label} projectKey must throw TypeError, not fall back"
        );
        assert_eq!(
            set_error.code(),
            Some("ERR_INVALID_ARG_TYPE"),
            "#347 fix-r1 {label} projectKey keeps Node's error code"
        );
        assert_eq!(
            String::from_utf16_lossy(set_error.message().units()),
            expected,
            "#347 {label} projectKey message"
        );
        let status_error = config
            .mcp_server_status(&status_input_with_project_key(key.clone()))
            .await
            .expect_err("#347 a non-string projectKey must reject mcpServerStatusForSdk");
        assert_eq!(
            String::from_utf16_lossy(status_error.message().units()),
            expected,
            "#347 {label} projectKey status message"
        );
        assert_eq!(
            status_error.name(),
            "TypeError",
            "#347 {label} projectKey status error name"
        );
        assert_eq!(
            status_error.code(),
            Some("ERR_INVALID_ARG_TYPE"),
            "#347 fix-r1 {label} status projectKey keeps Node's error code"
        );
    }
    assert_eq!(
        count_calls(&calls, "resolve"),
        0,
        "#347 the TypeError is thrown before this.resolve runs"
    );
    assert_eq!(
        count_calls(&calls, "closeSession"),
        0,
        "#347 a rejected projectKey must not close the session"
    );
    assert_eq!(
        text(&config.snapshot_state()),
        before,
        "#347 a rejected projectKey must not mutate state"
    );
}

/// #347 fix-r1 (review P1-3): the `constructor.name` shapes Node converts
/// with `String(name)` — null/undefined/""/0/NaN/false/{}/[]/[1,2]… — project
/// to `an instance of <String(name)>`, not the `[Object]` fallback; only a
/// missing `name` property, a nullish/array constructor, or the implicit
/// Object.prototype constructor keep their own texts. Node v22.22.0 matrix:
/// `_tmp/20261026-issue-347/fix-r1/probe/constructor-name{,-edge}.out`.
#[tokio::test]
async fn constructor_name_shapes_project_the_node_received_text() {
    let (config, calls) = test_config(Arc::new(FixtureEnvironment));
    config
        .set_mcp_servers(&set_input("s", &[("a", stdio_config("node"))]))
        .await
        .expect("#347 fix-r1 the setup set must succeed");
    let before = text(&config.snapshot_state());
    lock(&calls).clear();
    let named = |name: JsValue| {
        let mut constructor = JsObject::new();
        constructor.insert("name".into(), name);
        let mut key = JsObject::new();
        key.insert("constructor".into(), JsValue::Object(constructor));
        JsValue::Object(key)
    };
    for (label, key, received) in [
        (
            "name-object",
            named(JsValue::Object(JsObject::new())),
            "an instance of [object Object]",
        ),
        (
            "name-array",
            named(JsValue::Array(Vec::new())),
            "an instance of ",
        ),
        (
            "name-two-element-array",
            named(JsValue::Array(vec![
                JsValue::Number(1.0),
                JsValue::Number(2.0),
            ])),
            "an instance of 1,2",
        ),
        ("name-null", named(JsValue::Null), "an instance of null"),
        (
            "name-undefined",
            named(JsValue::Undefined),
            "an instance of undefined",
        ),
        (
            "name-empty-string",
            named(JsValue::String("".into())),
            "an instance of ",
        ),
        (
            "name-false",
            named(JsValue::Bool(false)),
            "an instance of false",
        ),
        ("name-zero", named(JsValue::Number(0.0)), "an instance of 0"),
        (
            "name-nan",
            named(JsValue::Number(f64::NAN)),
            "an instance of NaN",
        ),
        (
            "name-infinity",
            named(JsValue::Number(f64::INFINITY)),
            "an instance of Infinity",
        ),
        (
            "constructor-null",
            parse_json(r#"{"constructor":null}"#).unwrap(),
            "[Object]",
        ),
        (
            "constructor-array",
            parse_json(r#"{"constructor":[]}"#).unwrap(),
            "[Object]",
        ),
        (
            "constructor-without-name",
            parse_json(r#"{"constructor":{}}"#).unwrap(),
            "[Object]",
        ),
        (
            "constructor-missing",
            parse_json("{}").unwrap(),
            "an instance of Object",
        ),
    ] {
        let expected =
            format!("The \"paths[0]\" argument must be of type string. Received {received}");
        let set_error = config
            .set_mcp_servers(&set_input_with_project_key(
                key.clone(),
                &[("a", stdio_config("node"))],
            ))
            .await
            .expect_err("#347 fix-r1 a non-string projectKey must reject setMcpServersForSdk");
        assert_eq!(
            String::from_utf16_lossy(set_error.message().units()),
            expected,
            "#347 fix-r1 {label} set message must match Node's String(name) conversion"
        );
        assert_eq!(
            set_error.code(),
            Some("ERR_INVALID_ARG_TYPE"),
            "#347 fix-r1 {label} set keeps Node's error code"
        );
        let status_error = config
            .mcp_server_status(&status_input_with_project_key(key.clone()))
            .await
            .expect_err("#347 fix-r1 a non-string projectKey must reject mcpServerStatusForSdk");
        assert_eq!(
            String::from_utf16_lossy(status_error.message().units()),
            expected,
            "#347 fix-r1 {label} status message must match Node's String(name) conversion"
        );
        assert_eq!(
            status_error.code(),
            Some("ERR_INVALID_ARG_TYPE"),
            "#347 fix-r1 {label} status keeps Node's error code"
        );
    }
    assert_eq!(
        count_calls(&calls, "resolve"),
        0,
        "#347 fix-r1 the TypeError is thrown before this.resolve runs"
    );
    assert_eq!(
        count_calls(&calls, "closeSession"),
        0,
        "#347 fix-r1 a rejected projectKey must not close the session"
    );
    assert_eq!(
        text(&config.snapshot_state()),
        before,
        "#347 fix-r1 a rejected projectKey must not mutate state"
    );
}

/// #347① companion: `undefined`/`null`/missing `projectKey` keep the
/// fallback project (`projectKey ?? fallbackProjectRoot`), and the resolver
/// observes `null` exactly as the fake-registry fixture records it.
#[tokio::test]
async fn nullish_project_key_keeps_the_fallback_project() {
    let (config, calls) = test_config(Arc::new(FixtureEnvironment));
    for label in ["missing", "undefined", "null"] {
        let input = match label {
            "missing" => set_input("s", &[("a", stdio_config("node"))]),
            "undefined" => {
                set_input_with_project_key(JsValue::Undefined, &[("a", stdio_config("node"))])
            }
            _ => set_input_with_project_key(JsValue::Null, &[("a", stdio_config("node"))]),
        };
        config
            .set_mcp_servers(&input)
            .await
            .unwrap_or_else(|error| panic!("#347 {label} projectKey must fall back: {error}"));
        let status = match label {
            "missing" => JsValue::Object(JsObject::new()),
            "undefined" => status_input_with_project_key(JsValue::Undefined),
            _ => status_input_with_project_key(JsValue::Null),
        };
        config
            .mcp_server_status(&status)
            .await
            .unwrap_or_else(|error| {
                panic!("#347 {label} projectKey status must fall back: {error}")
            });
    }
    let resolves: Vec<String> = lock(&calls)
        .iter()
        .filter(|call| {
            matches!(call, JsValue::Array(items)
                if items.first().and_then(JsValue::as_string).is_some_and(|name| name.equals_str("resolve")))
        })
        .map(text)
        .collect();
    assert_eq!(
        resolves,
        vec![r#"["resolve",null]"#.to_owned(); 6],
        "#347 every nullish projectKey resolves the fallback (recorded as null)"
    );
}

/// #347②: `clearSdkSessionState`'s MCP fields drop one session's servers,
/// permission-mode overrides, generated rules and shared project entry — and
/// leave both the other sessions and (like the baseline) the live ask array
/// untouched.
#[tokio::test]
async fn clear_sdk_session_state_drops_only_the_given_session() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let project = Arc::new(FakeProject {
        root: "/pdrs-fixture/project".into(),
        plugins: Vec::new(),
        calls: Arc::clone(&calls),
        refresh_error: None,
    });
    let resolver = Arc::new(FakeResolver {
        project,
        calls: Arc::clone(&calls),
        error: None,
    });
    let close_calls = Arc::clone(&calls);
    let close_session: SdkMcpCloseSession = Arc::new(move |session_key, reason| {
        lock(&close_calls).push(close_call(&session_key, reason));
        Box::pin(async { Ok(()) })
    });
    let permission_rule_sets = Arc::new(GatewaySessionPermissionRuleSetRegistry::new());
    let lease = permission_rule_sets.acquire(&JsString::from("s"), None);
    let config = SdkMcpConfig::new(SdkMcpConfigOptions {
        resolver,
        close_session,
        session_plugins: Arc::new(SessionPluginsSource {
            entries: Arc::new(Mutex::new(Vec::new())),
        }),
        permission_rule_sets: Arc::clone(&permission_rule_sets),
        environment: Arc::new(FixtureEnvironment),
        projects: SdkSessionProjects::shared(),
    });
    config
        .set_mcp_servers(&set_input("s", &[("a", stdio_config("node"))]))
        .await
        .unwrap();
    config
        .set_mcp_servers(&set_input("t", &[("b", stdio_config("node"))]))
        .await
        .unwrap();
    config
        .set_mcp_permission_mode_override(&override_input("s", "a", JsValue::String("auto".into())))
        .await
        .unwrap();
    let ask_before = text(&ask_value(&lease));
    assert!(
        ask_before.contains("mcp__a__*"),
        "#347 the auto override generates the ask rule: {ask_before}"
    );

    config.clear_sdk_session_state(&JsValue::String("s".into()));
    let state = config.snapshot_state();
    assert_eq!(
        text(field(&state, "servers")),
        r#"[["t",[["b",{"config":{"type":"stdio","command":"node"},"enabled":true}]]]]"#,
        "#347 clear keeps session t's server table"
    );
    assert_eq!(
        text(field(&state, "overrides")),
        "[]",
        "#347 clear drops session s's permission-mode overrides"
    );
    assert_eq!(
        text(field(&state, "rules")),
        "[]",
        "#347 clear drops session s's generated rules"
    );
    assert_eq!(
        text(field(&state, "projects")),
        "[[\"t\",\"/pdrs-fixture/project\"]]",
        "#347 clear drops only session s's shared project entry"
    );
    assert_eq!(
        text(&ask_value(&lease)),
        ask_before,
        "#347 like the baseline, clear does not re-sync the live ask array"
    );
    // The cleared session starts over from an empty table.
    config
        .set_mcp_servers(&set_input("s", &[("c", stdio_config("node"))]))
        .await
        .unwrap();
    let names: Vec<String> = config
        .session_mcp_servers(&JsValue::String("s".into()))
        .iter()
        .map(|(name, _)| String::from_utf16_lossy(name.units()))
        .collect();
    assert_eq!(names, vec!["c".to_owned()], "#347 session s restarts empty");
}

/// #347②: the `sdkSessionMcpServers` read entry used by the merged plugin
/// view (ProjectRuntimeRegistry.ts:1002).
#[tokio::test]
async fn session_mcp_servers_reads_the_table_in_insertion_order() {
    let (config, _calls) = test_config(Arc::new(FixtureEnvironment));
    config
        .set_mcp_servers(&set_input("s", &[("a", stdio_config("node"))]))
        .await
        .unwrap();
    config
        .set_mcp_servers(&set_input("s", &[("a", stdio_config("node"))]))
        .await
        .unwrap();
    let mut second = stdio_config("node");
    if let JsValue::Object(object) = &mut second {
        object.insert("command".into(), JsValue::String("deno".into()));
    }
    config
        .set_mcp_servers(&set_input(
            "s",
            &[("b", second), ("a", stdio_config("node"))],
        ))
        .await
        .unwrap();
    let servers = config.session_mcp_servers(&JsValue::String("s".into()));
    let names: Vec<String> = servers
        .iter()
        .map(|(name, _)| String::from_utf16_lossy(name.units()))
        .collect();
    assert_eq!(
        names,
        vec!["b".to_owned(), "a".to_owned()],
        "#347 the read keeps the set call's insertion order"
    );
    assert_eq!(
        text(&servers[0].1),
        r#"{"config":{"type":"stdio","command":"deno"},"enabled":true}"#,
        "#347 the read projects the stored {{config, enabled}}"
    );
    assert!(
        config
            .session_mcp_servers(&JsValue::String("missing".into()))
            .is_empty(),
        "#347 an unknown session reads an empty table"
    );
}

/// A `SdkRuntimeResolver` that always resolves one fixed runtime; shared by
/// the registry-over-shared-table tests.
struct FixedResolver {
    root: PathBuf,
    snapshot: JsValue,
    plugin_runtime: Arc<PluginRuntime>,
}
impl SdkRuntimeResolver for FixedResolver {
    fn resolve(
        &self,
        _project_key: Option<&JsString>,
    ) -> Result<ResolvedSdkRuntime, SdkSessionError> {
        Ok(ResolvedSdkRuntime {
            project_root: self.root.clone(),
            snapshot: self.snapshot.clone(),
            plugin_runtime: Arc::clone(&self.plugin_runtime),
        })
    }
}

/// #347③: `SdkMcpConfig` and the A6.3-pr8 `SdkSessionRegistry` write the
/// one shared `sdkSessionProjects` table, so a session→project entry written
/// by one slice is exactly what the other reads.
#[tokio::test]
async fn session_to_project_table_is_shared_with_the_session_registry() {
    let case = scratch::Scratch::new("sdk-mcp-shared-projects");
    let dir = case.path().to_path_buf();
    let plugin_runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
        project_root: dir.clone(),
        pilot_home: dir.join("home"),
        builtin_skills_root: None,
        builtin_plugins: Vec::new(),
        builtin_plugins_enabled: std::collections::HashMap::new(),
    }));
    let mut env = Env::new();
    env.insert("HOME".into(), dir.to_string_lossy().into_owned());
    env.insert(
        "PILOT_HOME".into(),
        dir.join("home").to_string_lossy().into_owned(),
    );
    // An empty session config passes validation; only the recorded project
    // root is under observation here.
    let snapshot = JsValue::Object(JsObject::new());
    let projects = SdkSessionProjects::shared();
    let mut registry = SdkSessionRegistry::new(
        SdkSessionOptions::new(
            dir.join("fallback"),
            case.path(),
            env,
            Arc::new(FixedResolver {
                root: dir.clone(),
                snapshot: snapshot.clone(),
                plugin_runtime: Arc::clone(&plugin_runtime),
            }),
        )
        .with_projects(Arc::clone(&projects)),
    );
    let empty_config = JsValue::Object(JsObject::new());
    registry
        .set_session_config(&"s".into(), &empty_config, None, None)
        .await
        .expect("#347 the session config must record the project");
    let registry_root = registry
        .session_project(&"s".into())
        .expect("#347 setSdkSessionConfig records the session root");
    assert_eq!(
        registry_root,
        dir.to_string_lossy().into_owned().into(),
        "#347 the registry writes the shared table"
    );

    let calls = Arc::new(Mutex::new(Vec::new()));
    let project = Arc::new(FakeProject {
        root: "/pdrs-fixture/project".into(),
        plugins: Vec::new(),
        calls: Arc::clone(&calls),
        refresh_error: None,
    });
    let close_calls = Arc::clone(&calls);
    let close_session: SdkMcpCloseSession = Arc::new(move |session_key, reason| {
        lock(&close_calls).push(close_call(&session_key, reason));
        Box::pin(async { Ok(()) })
    });
    let config = SdkMcpConfig::new(SdkMcpConfigOptions {
        resolver: Arc::new(FakeResolver {
            project,
            calls: Arc::clone(&calls),
            error: None,
        }),
        close_session,
        session_plugins: Arc::new(SessionPluginsSource {
            entries: Arc::new(Mutex::new(Vec::new())),
        }),
        permission_rule_sets: Arc::new(GatewaySessionPermissionRuleSetRegistry::new()),
        environment: Arc::new(FixtureEnvironment),
        projects: projects.clone(),
    });
    // The entry the session registry wrote is the one the MCP slice reads,
    // and the MCP write for another session does not disturb it.
    config
        .set_mcp_servers(&set_input("t", &[("b", stdio_config("node"))]))
        .await
        .expect("#347 setMcpServersForSdk records the project");
    let state = config.snapshot_state();
    let entries = array(field(&state, "projects"), "#347 state projects");
    let recorded: Vec<String> = entries.iter().map(text).collect();
    assert_eq!(
        recorded,
        vec![
            format!(
                r#"["s",{}]"#,
                text(&JsValue::String(dir.to_string_lossy().into_owned().into()))
            ),
            r#"["t","/pdrs-fixture/project"]"#.to_owned(),
        ],
        "#347 both slices append to the one shared table in insertion order"
    );
    assert_eq!(
        registry.session_project(&"t".into()),
        Some("/pdrs-fixture/project".into()),
        "#347 the registry reads the MCP slice's write"
    );

    config.clear_sdk_session_state(&JsValue::String("s".into()));
    assert_eq!(
        registry.session_project(&"s".into()),
        None,
        "#347 the MCP clear drops the shared entry the registry wrote"
    );
    assert_eq!(
        registry.session_project(&"t".into()),
        Some("/pdrs-fixture/project".into()),
        "#347 the MCP clear leaves the other sessions"
    );
}

/// #347 fix-r1 (review P2-2): the composition-root order for the two clear
/// entries — registry first (its task-budget step reads the shared project
/// entry), MCP second (it drops that entry) — clears everything, mirroring
/// the single TS `clearSdkSessionState` (:776-789) where the budget clear
/// reads `sdkSessionProjects` before the same function deletes it.
#[tokio::test]
async fn registry_clear_then_mcp_clear_clears_everything_in_order() {
    struct RecordingBudget {
        cleared: Arc<Mutex<Vec<JsString>>>,
    }
    impl SdkTaskBudget for RecordingBudget {
        fn configure_project(
            &self,
            _project_root: &JsString,
            _session_key: &JsString,
            _total_usd: f64,
            _retention_ms: Option<f64>,
            _now_ms: f64,
        ) -> Result<(), TaskBudgetError> {
            Ok(())
        }
        fn clear_session(
            &self,
            project_root: &JsString,
            _session_key: &JsString,
        ) -> Result<(), TaskBudgetError> {
            lock(&self.cleared).push(project_root.clone());
            Ok(())
        }
    }
    let case = scratch::Scratch::new("sdk-mcp-paired-clear");
    let dir = case.path().to_path_buf();
    let plugin_runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
        project_root: dir.clone(),
        pilot_home: dir.join("home"),
        builtin_skills_root: None,
        builtin_plugins: Vec::new(),
        builtin_plugins_enabled: std::collections::HashMap::new(),
    }));
    let mut env = Env::new();
    env.insert("HOME".into(), dir.to_string_lossy().into_owned());
    env.insert(
        "PILOT_HOME".into(),
        dir.join("home").to_string_lossy().into_owned(),
    );
    let projects = SdkSessionProjects::shared();
    let cleared = Arc::new(Mutex::new(Vec::new()));
    let mut registry = SdkSessionRegistry::new(
        SdkSessionOptions::new(
            dir.join("fallback"),
            case.path(),
            env,
            Arc::new(FixedResolver {
                root: dir.clone(),
                snapshot: JsValue::Object(JsObject::new()),
                plugin_runtime: Arc::clone(&plugin_runtime),
            }),
        )
        .with_projects(Arc::clone(&projects))
        .with_task_budget(Arc::new(RecordingBudget {
            cleared: Arc::clone(&cleared),
        })),
    );
    registry
        .set_session_config(&"s".into(), &JsValue::Object(JsObject::new()), None, None)
        .await
        .expect("#347 fix-r1 the session config must record the project");
    let (config, _calls) =
        test_config_with_projects(Arc::new(FixtureEnvironment), projects.clone());
    config
        .set_mcp_servers(&set_input("s", &[("a", stdio_config("node"))]))
        .await
        .expect("#347 fix-r1 the MCP set must record the project");
    assert_eq!(
        projects.project(&JsValue::String("s".into())),
        // The MCP set replaces the registry's entry in place (JS Map
        // semantics): both slices recorded the session's project.
        Some("/pdrs-fixture/project".into()),
        "#347 fix-r1 both slices recorded the session's project"
    );

    // The composition-root order: registry clear (budget step reads the
    // entry), then the MCP clear (drops it).
    registry
        .clear_session_state(&"s".into())
        .expect("#347 fix-r1 the registry clear must succeed");
    config.clear_sdk_session_state(&JsValue::String("s".into()));
    assert_eq!(
        lock(&cleared).len(),
        1,
        "#347 fix-r1 the registry clear's budget step still sees the shared entry"
    );
    assert!(
        config
            .session_mcp_servers(&JsValue::String("s".into()))
            .is_empty(),
        "#347 fix-r1 the paired clears drop the MCP server table"
    );
    assert_eq!(
        registry.session_project(&"s".into()),
        None,
        "#347 fix-r1 the paired clears drop the shared project entry"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn set_and_toggle_do_not_lose_updates() {
    let gate = Arc::new(Gate::default());
    let (config, _calls) = test_config(Arc::new(GatedEnvironment {
        gate: Arc::clone(&gate),
    }));
    let config = Arc::new(config);
    config
        .set_mcp_servers(&set_input("s", &[("a", stdio_config("node"))]))
        .await
        .unwrap();

    let set_config = Arc::clone(&config);
    let set_task = tokio::spawn(async move {
        set_config
            .set_mcp_servers(&set_input("s", &[("a", stdio_config("${env:GATE}"))]))
            .await
    });
    {
        let mut entered = lock(&gate.entered.0);
        while !*entered {
            entered = gate.entered.1.wait(entered).unwrap();
        }
    }
    let toggle_config = Arc::clone(&config);
    let mut toggle_task = tokio::spawn(async move {
        toggle_config
            .toggle_mcp_server(&toggle_input("s", "a", false))
            .await
    });
    let completed_early = tokio::time::timeout(Duration::from_millis(1000), &mut toggle_task)
        .await
        .is_ok();
    // Release the setter before asserting so a failure cannot leave the
    // runtime blocked on the gate.
    *lock(&gate.release.0) = true;
    gate.release.1.notify_all();
    set_task.await.unwrap().unwrap();
    assert!(
        !completed_early,
        "toggle completed inside set's read-modify-write window"
    );
    toggle_task.await.unwrap().unwrap();
    assert_eq!(enabled_for(&config, "a"), Some(JsValue::Bool(false)));
}

/// #347 fix-r1 (review P1-1): a clear issued while a `setMcpServersForSdk`
/// for the same session is in flight cannot leave a half-cleared session —
/// `clear_sdk_session_state` holds the servers guard (the same critical
/// section the set commits under) across all four drops, so the set's commit
/// runs entirely after it. Matrix cell 3 in
/// `_tmp/20261026-issue-347/fix-r1/plan.md`; TS is synchronous across the
/// whole clear (`probe/clear-set-atomicity.out`), so no mixed state exists
/// there to begin with. #347 fix-r2 (review P1, same pattern as the cell-4
/// test): the window outcome is consumed and the delete gate is only opened
/// after the in-window completion is confirmed, so the two writes' order is
/// pinned by the gates instead of thread wakeup order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clear_and_set_commits_share_one_critical_section() {
    let projects = Arc::new(GatedProjects::default());
    let (config, _calls) =
        test_config_with_projects(Arc::new(FixtureEnvironment), projects.clone());
    let config = Arc::new(config);
    config
        .set_mcp_servers(&set_input("s", &[("a", stdio_config("node"))]))
        .await
        .expect("#347 fix-r1 the setup set must succeed");
    // Arm: the next shared-table set/delete parks instead of completing.
    *lock(&projects.armed) = true;

    let clear_config = Arc::clone(&config);
    let clear_key = JsValue::String("s".into());
    let clear =
        tokio::task::spawn_blocking(move || clear_config.clear_sdk_session_state(&clear_key));
    // The clear parks at its last drop, holding the shared commit guard.
    wait_entered(&projects.delete_gate, "shared-table delete");

    let set_config = Arc::clone(&config);
    let mut set_task = tokio::spawn(async move {
        set_config
            .set_mcp_servers(&set_input("s", &[("b", stdio_config("node"))]))
            .await
    });
    // Open the set's gate and hand the commit a bounded window: while the
    // clear is parked at its gated delete it holds the servers guard (the
    // fixed shape), so the commit cannot even acquire it and the window must
    // elapse; only a clear that already dropped the guard (the mutation under
    // test) lets the commit finish inside the window (#347 fix-r2: the
    // outcome is consumed, so the observed interleaving — not thread wakeup
    // order — picks the branch).
    release_gate(&projects.set_gate);
    let outcome = tokio::time::timeout(Duration::from_secs(2), &mut set_task).await;
    match outcome {
        // The commit finished while the clear was still parked: its
        // shared-table write has landed before the clear's delete even starts
        // (the gate is still closed), so the delete now wipes the entry the
        // server table keeps — exactly the half-cleared state the shared
        // critical section exists to prevent.
        Ok(result) => {
            release_gate(&projects.delete_gate);
            clear.await.expect("#347 fix-r2 the clear task must join");
            result
                .expect("#347 fix-r2 the set task must join")
                .expect("#347 fix-r2 setMcpServersForSdk must succeed after the clear");
        }
        // The commit spent the whole window blocked at the servers guard (the
        // fixed shape): the clear finishes its drops — releasing the guard —
        // and only then does the commit run in full, after the clear.
        Err(_elapsed) => {
            release_gate(&projects.delete_gate);
            set_task
                .await
                .expect("#347 fix-r2 the set task must join")
                .expect("#347 fix-r2 setMcpServersForSdk must succeed after the clear");
            clear.await.expect("#347 fix-r2 the clear task must join");
        }
    }
    let names: Vec<String> = config
        .session_mcp_servers(&JsValue::String("s".into()))
        .iter()
        .map(|(name, _)| String::from_utf16_lossy(name.units()))
        .collect();
    assert_eq!(
        names,
        vec!["b".to_owned()],
        "#347 fix-r1 the later set commits its server table"
    );
    assert_eq!(
        projects.project(&JsValue::String("s".into())),
        Some("/pdrs-fixture/project".into()),
        "#347 fix-r1 the shared session→project entry must not lag the server table"
    );
}

/// #347 fix-r1 (review P1-1), the mirrored cell: a clear issued while a
/// set's commit is in flight cannot see the server table without the
/// shared-table write — the set's commit (including `projects.set`) holds
/// the servers guard until both writes are done, so the clear runs entirely
/// after it. Matrix cell 4 in `fix-r1/plan.md`. #347 fix-r2 (review P1):
/// the window outcome is consumed and the other gate is only opened after
/// the in-window completion is confirmed, so the two writes' order is pinned
/// by the gates instead of thread wakeup order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn set_commit_cannot_outlive_the_shared_table_write() {
    let projects = Arc::new(GatedProjects::default());
    let (config, _calls) =
        test_config_with_projects(Arc::new(FixtureEnvironment), projects.clone());
    let config = Arc::new(config);
    *lock(&projects.armed) = true;

    let set_config = Arc::clone(&config);
    let set_task = tokio::spawn(async move {
        set_config
            .set_mcp_servers(&set_input("s", &[("b", stdio_config("node"))]))
            .await
    });
    // The set's commit parks at its shared-table write, holding the guard.
    wait_entered(&projects.set_gate, "shared-table set");

    let clear_config = Arc::clone(&config);
    let clear_key = JsValue::String("s".into());
    let mut clear =
        tokio::task::spawn_blocking(move || clear_config.clear_sdk_session_state(&clear_key));
    // Open the delete gate and hand the clear a bounded window: while the set
    // is parked at its gated write the servers guard stays held (the fixed
    // shape), so the clear cannot even acquire it and the window must elapse;
    // only a commit that already dropped the guard before its shared-table
    // write (the mutation under test) lets the clear run to completion inside
    // the window.
    release_gate(&projects.delete_gate);
    let outcome = tokio::time::timeout(Duration::from_secs(2), &mut clear).await;
    release_gate(&projects.set_gate);
    match outcome {
        // The clear finished while the set was still parked: its shared-table
        // delete has returned before the set's write even starts (the gate
        // was still closed), so the write landing now is exactly the stale
        // entry the shared critical section exists to prevent.
        Ok(joined) => {
            joined.expect("#347 fix-r2 the in-window clear must join");
            set_task
                .await
                .expect("#347 fix-r2 the set task must join")
                .expect("#347 fix-r2 setMcpServersForSdk must succeed");
        }
        // The clear spent the whole window blocked at the servers guard (the
        // fixed shape): the set's shared-table write lands inside the guard,
        // and only once it is done does the clear take the guard and delete.
        Err(_elapsed) => {
            set_task
                .await
                .expect("#347 fix-r2 the set task must join")
                .expect("#347 fix-r2 setMcpServersForSdk must succeed");
            clear.await.expect("#347 fix-r2 the clear task must join");
        }
    }
    assert!(
        config
            .session_mcp_servers(&JsValue::String("s".into()))
            .is_empty(),
        "#347 fix-r2 the clear drops the in-flight commit's server table"
    );
    assert_eq!(
        projects.project(&JsValue::String("s".into())),
        None,
        "#347 fix-r2 the clear must not leave the commit's shared-table entry behind"
    );
}

#[tokio::test]
async fn replays_every_ts_sdk_mcp_step() {
    let oracle = baseline::oracle(ORACLE);
    let scenarios = array(field(&oracle, "scenarios"), "scenarios");
    // Exact corpus size: an empty or truncated oracle must never pass.
    assert_eq!(scenarios.len(), 16, "oracle scenario count");
    let mut step_total = 0;

    for scenario in scenarios {
        let id = string(field(scenario, "id"), "scenario.id")
            .to_utf8()
            .unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let project_plugins = array(field(scenario, "projectPlugins"), "projectPlugins")
            .iter()
            .map(|value| plugin(value, "project plugin"))
            .collect();
        let refresh_error = field(scenario, "refreshError")
            .as_string()
            .map(|value| value.to_utf8().unwrap());
        let project = Arc::new(FakeProject {
            root: "/pdrs-fixture/project".into(),
            plugins: project_plugins,
            calls: Arc::clone(&calls),
            refresh_error,
        });
        let resolver = Arc::new(FakeResolver {
            project,
            calls: Arc::clone(&calls),
            error: field(scenario, "resolveError")
                .as_string()
                .map(|value| value.to_utf8().unwrap()),
        });
        let session_entries: SessionPluginMap =
            array(field(scenario, "sessionPlugins"), "sessionPlugins")
                .iter()
                .map(|entry| {
                    let entry = array(entry, "session plugin entry");
                    (
                        string(&entry[0], "session key"),
                        array(&entry[1], "session plugins")
                            .iter()
                            .map(|value| plugin(value, "session plugin"))
                            .collect(),
                    )
                })
                .collect();
        let session_entries = Arc::new(Mutex::new(session_entries));
        let gateway = field(scenario, "gateway").as_bool().unwrap_or(true);
        let close_error = field(scenario, "closeError")
            .as_string()
            .map(|value| value.to_utf8().unwrap());
        let close_calls = Arc::clone(&calls);
        let close_session: SdkMcpCloseSession = Arc::new(move |session_key, reason| {
            if gateway {
                lock(&close_calls).push(close_call(&session_key, reason));
            }
            let error = close_error.clone();
            Box::pin(async move {
                match error {
                    Some(message) => Err(SdkMcpError::plain("Error", message)),
                    None => Ok(()),
                }
            })
        });
        let permission_rule_sets = Arc::new(GatewaySessionPermissionRuleSetRegistry::new());
        let configured = match field(scenario, "configured") {
            JsValue::Null | JsValue::Undefined => None,
            value => Some(ConfiguredRuleSet::from_permission_rules(value)),
        };
        let lease = permission_rule_sets.acquire(&JsString::from("s"), configured.as_ref());
        // The order scenario also updates session "t". Each acquisition gets
        // its own configured arrays; the baseline registry would otherwise
        // share the caller's exact arrays across entries.
        let configured_t = match field(scenario, "configured") {
            JsValue::Null | JsValue::Undefined => None,
            value => Some(ConfiguredRuleSet::from_permission_rules(value)),
        };
        let _lease_t = permission_rule_sets.acquire(&JsString::from("t"), configured_t.as_ref());
        let config = SdkMcpConfig::new(SdkMcpConfigOptions {
            resolver,
            close_session,
            session_plugins: Arc::new(SessionPluginsSource {
                entries: Arc::clone(&session_entries),
            }),
            permission_rule_sets: Arc::clone(&permission_rule_sets),
            environment: Arc::new(FixtureEnvironment),
            projects: SdkSessionProjects::shared(),
        });

        let steps = array(field(scenario, "steps"), "steps");
        step_total += steps.len();
        for (index, step) in steps.iter().enumerate() {
            let command = field(step, "step");
            let op = string(field(command, "op"), "step.op").to_utf8().unwrap();
            let result = if op == "setSessionPlugins" {
                let input = decode_input(field(command, "input"), 0);
                let key = string(field(&input, "sessionKey"), "setSessionPlugins.sessionKey");
                let plugins = array(field(&input, "plugins"), "setSessionPlugins.plugins")
                    .iter()
                    .map(|value| plugin(value, "session plugin"))
                    .collect();
                let mut entries = lock(&session_entries);
                entries.retain(|(existing, _)| existing != &key);
                entries.push((key, plugins));
                let mut object = JsObject::new();
                object.insert("ok".into(), JsValue::Bool(true));
                JsValue::Object(object)
            } else {
                run_step(&config, command).await
            };
            assert_eq!(
                text(&result),
                text(field(step, "result")),
                "{id} step {index} ({op}) result"
            );
            assert_eq!(
                text(&state_value(&config, &lease)),
                text(field(step, "state")),
                "{id} step {index} ({op}) state"
            );
            assert_eq!(
                text(&JsValue::Array(lock(&calls).clone())),
                text(field(step, "calls")),
                "{id} step {index} ({op}) calls"
            );
        }
    }
    assert_eq!(step_total, 132, "oracle step count");
}

#[test]
fn oracle_is_json_and_provenance_checked() {
    // `baseline::oracle` already runs; keep an explicit parse so a malformed
    // fixture fails even when PDRS_BASELINE is unset.
    let parsed = parse_json(ORACLE).expect("oracle.json must be valid JSON");
    assert!(field(&parsed, "sourceSha256").as_object().is_some());
}

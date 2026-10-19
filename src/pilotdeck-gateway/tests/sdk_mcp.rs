//! Replay of the real-TS SDK MCP configuration oracle
//! (`tests/fixtures/sdk-mcp/generate.mts`). Each step compares the method
//! result (or the exact `{ name, code, message }` error), the internal state
//! projection, the live ask/allow arrays, and the cumulative
//! resolve/refresh/closeSession call sequence.
#[path = "common/baseline.rs"]
mod baseline;

use pilotdeck_gateway::{
    config::permission::{ConfiguredRuleSet, GatewaySessionPermissionRuleSetRegistry},
    runtime::{
        mcp::sdk::{
            SdkMcpCloseSession, SdkMcpConfig, SdkMcpConfigOptions, SdkMcpEnvironment, SdkMcpError,
            SdkMcpFuture, SdkMcpProject, SdkMcpProjectResolver, SdkMcpSessionPlugins,
        },
        plugins::{LoadedPlugin, PluginSource},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
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

fn json_value(value: &JsValue) -> serde_json::Value {
    serde_json::from_str(&text(value)).expect("JSON-compatible value")
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
        other => Some(json_value(other)),
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
        "/pdrs-fixture/home".into()
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

#[tokio::test]
async fn replays_every_ts_sdk_mcp_step() {
    let oracle = baseline::oracle(ORACLE);

    for scenario in array(field(&oracle, "scenarios"), "scenarios") {
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
        });

        for (index, step) in array(field(scenario, "steps"), "steps").iter().enumerate() {
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
}

#[test]
fn oracle_is_json_and_provenance_checked() {
    // `baseline::oracle` already runs; keep an explicit parse so a malformed
    // fixture fails even when PDRS_BASELINE is unset.
    let parsed = parse_json(ORACLE).expect("oracle.json must be valid JSON");
    assert!(field(&parsed, "sourceSha256").as_object().is_some());
}

//! SDK MCP configuration state, ported from the fixed TypeScript baseline's
//! `src/cli/ProjectRuntimeRegistry.ts:1184-1308`:
//!
//! - `setMcpServersForSdk`
//! - `setMcpPermissionModeOverrideForSdk`
//! - `syncSdkMcpPermissionRules`
//! - `toggleMcpServerForSdk`
//! - `reconnectMcpServerForSdk`
//! - `mcpServerStatusForSdk`
//!
//! This is state management only. The project runtime, plugin snapshot,
//! session plugin source and gateway `closeSession` are injected; nothing here
//! connects to an MCP server (that is A6.5-pr10).
//!
//! ## JS semantics
//!
//! `input.servers` is walked with `Object.entries` key order (array-index keys
//! first, ascending), `String(value)` and `JSON.stringify` go through
//! [`crate::js_value`] / the protocol's iterative ordered stringifier, and the
//! generated permission rule is the same object identity in the live ask array
//! and the internal generated-rule map (as in TS).

use super::super::plugins::{LoadedPlugin, PluginRuntime, PluginRuntimeError};
use super::super::project::ProjectRuntime;
use crate::config::permission::{GatewaySessionPermissionRuleSetRegistry, PermissionRule};
use crate::config::session_value::SessionValue;
use crate::js_value;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
};

/// The warning returned for `mode: "auto"`.
pub const AUTO_PERMISSION_WARNING: &str = "PilotDeck does not expose Claude's MCP safety classifier; auto uses conservative ask semantics for this server.";
const NO_TRANSPORT: &str = "no recognized transport (need command or url)";
const MISSING_SPEC: &str = "missing or non-object spec";
const RESERVED_NAME: &str = "This MCP server name is owned by project configuration or a plugin.";

/// Boxed future used by the injected project/plugin ports.
pub type SdkMcpFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// Boxed `closeSession` future. The session key is owned, so the closure never
/// borrows the caller's input.
pub type SdkMcpCloseFuture =
    Pin<Box<dyn Future<Output = Result<(), SdkMcpError>> + Send + 'static>>;
/// The injected `gateway?.closeSession({ sessionKey, reason })` call.
pub type SdkMcpCloseSession = Arc<dyn Fn(JsValue, &'static str) -> SdkMcpCloseFuture + Send + Sync>;

/// A `DialogGatewayError` / plain `Error` / `TypeError` in the baseline's
/// `{ name, code, message }` projection. Codes are fixed strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SdkMcpError {
    code: Option<&'static str>,
    name: &'static str,
    message: JsString,
}
impl SdkMcpError {
    pub fn gateway(code: &'static str, message: impl Into<JsString>) -> Self {
        Self {
            code: Some(code),
            name: "DialogGatewayError",
            message: message.into(),
        }
    }
    pub fn plain(name: &'static str, message: impl Into<JsString>) -> Self {
        Self {
            code: None,
            name,
            message: message.into(),
        }
    }
    pub fn type_error(message: impl Into<JsString>) -> Self {
        Self::plain("TypeError", message)
    }
    pub fn code(&self) -> Option<&'static str> {
        self.code
    }
    pub fn name(&self) -> &'static str {
        self.name
    }
    pub fn message(&self) -> &JsString {
        &self.message
    }
    /// `{ name, code: code ?? null, message }`.
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("name".into(), JsValue::String(self.name.into()));
        object.insert(
            "code".into(),
            self.code
                .map_or(JsValue::Null, |code| JsValue::String(code.into())),
        );
        object.insert("message".into(), JsValue::String(self.message.clone()));
        JsValue::Object(object)
    }
}
impl fmt::Display for SdkMcpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf16_lossy(self.message.units()))
    }
}
impl std::error::Error for SdkMcpError {}

/// `process.env` / `os.homedir()` for `expandMcpString`. Injected so tests pin
/// the environment without touching process globals.
pub trait SdkMcpEnvironment: Send + Sync {
    /// `process.env[name] ?? ""`; `None` means unset.
    fn env(&self, name: &JsString) -> Option<JsString>;
    /// `process.env.HOME ?? process.env.USERPROFILE ?? os.homedir()`.
    fn home(&self) -> JsString;
}

/// The product environment: real process variables.
#[derive(Default)]
pub struct SystemEnvironment;
impl SdkMcpEnvironment for SystemEnvironment {
    fn env(&self, name: &JsString) -> Option<JsString> {
        let name = name.to_utf8().ok()?;
        match std::env::var_os(&name) {
            Some(value) => Some(JsString::from(value.to_string_lossy().into_owned())),
            None => object_prototype_env(&JsString::from(name)),
        }
    }
    fn home(&self) -> JsString {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|value| JsString::from(value.to_string_lossy().into_owned()))
            .unwrap_or_default()
    }
}

/// `process.env[name]` inherits `Object.prototype`, so names such as
/// `toString` or `__proto__` read a non-empty builtin instead of `undefined`.
/// The member table is the one shared with configuration prototype lookup.
fn object_prototype_env(name: &JsString) -> Option<JsString> {
    if !crate::config::builtin_property(name) {
        return None;
    }
    let name = name.to_utf8().ok()?;
    Some(match name.as_str() {
        "__proto__" => JsString::from("[object Object]"),
        // Node 22's `process.env.constructor` is the anonymous Object function.
        "constructor" => JsString::from("function () { [native code] }"),
        _ => JsString::from(format!("function {name}() {{ [native code] }}")),
    })
}

/// A resolved project generation seen by the SDK MCP state machine.
pub trait SdkMcpProject: Send + Sync {
    /// `runtime.projectRoot`.
    fn project_root(&self) -> JsString;
    /// `await runtime.pluginRuntime.refresh()`.
    fn refresh<'a>(&'a self) -> SdkMcpFuture<'a, Result<(), SdkMcpError>>;
    /// `runtime.pluginRuntime.snapshot()`.
    fn snapshot<'a>(&'a self) -> SdkMcpFuture<'a, Vec<Arc<LoadedPlugin>>>;
}

/// Adapter over the merged A6.3-pr7 [`PluginRuntime`] and A6.3-pr4
/// [`ProjectRuntime`] types.
pub struct PluginProjectRuntime {
    project_root: JsString,
    plugin_runtime: Arc<PluginRuntime>,
}
impl PluginProjectRuntime {
    pub fn new(project_root: impl Into<JsString>, plugin_runtime: Arc<PluginRuntime>) -> Self {
        Self {
            project_root: project_root.into(),
            plugin_runtime,
        }
    }
    /// Reuse a live project generation's resolved root.
    pub fn from_generation(runtime: &ProjectRuntime, plugin_runtime: Arc<PluginRuntime>) -> Self {
        Self::new(
            JsString::from(runtime.project_root().to_string_lossy().into_owned()),
            plugin_runtime,
        )
    }
}
impl SdkMcpProject for PluginProjectRuntime {
    fn project_root(&self) -> JsString {
        self.project_root.clone()
    }
    fn refresh<'a>(&'a self) -> SdkMcpFuture<'a, Result<(), SdkMcpError>> {
        Box::pin(async move {
            self.plugin_runtime
                .refresh()
                .await
                .map(|_| ())
                .map_err(plugin_error)
        })
    }
    fn snapshot<'a>(&'a self) -> SdkMcpFuture<'a, Vec<Arc<LoadedPlugin>>> {
        Box::pin(async move { self.plugin_runtime.snapshot().await })
    }
}
fn plugin_error(error: PluginRuntimeError) -> SdkMcpError {
    let name = if error.is_aggregate() {
        "AggregateError"
    } else {
        "Error"
    };
    SdkMcpError::plain(name, JsString::from(error.message))
}

/// `this.resolve(input.projectKey)`.
pub trait SdkMcpProjectResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        project_key: Option<&'a JsString>,
    ) -> SdkMcpFuture<'a, Result<Arc<dyn SdkMcpProject>, SdkMcpError>>;
}

/// `this.sdkSessionPlugins.get(sessionKey) ?? []`.
pub trait SdkMcpSessionPlugins: Send + Sync {
    fn plugins(&self, session_key: &JsValue) -> Vec<Arc<LoadedPlugin>>;
}
impl<F> SdkMcpSessionPlugins for F
where
    F: Fn(&JsValue) -> Vec<Arc<LoadedPlugin>> + Send + Sync,
{
    fn plugins(&self, session_key: &JsValue) -> Vec<Arc<LoadedPlugin>> {
        self(session_key)
    }
}

/// Injected collaborators. The permission registry is the same live object
/// that owns session ask arrays (A6.3-pr6).
pub struct SdkMcpConfigOptions {
    pub resolver: Arc<dyn SdkMcpProjectResolver>,
    pub close_session: SdkMcpCloseSession,
    pub session_plugins: Arc<dyn SdkMcpSessionPlugins>,
    pub permission_rule_sets: Arc<GatewaySessionPermissionRuleSetRegistry>,
    pub environment: Arc<dyn SdkMcpEnvironment>,
}

#[derive(Clone, Debug)]
struct ServerEntry {
    config: JsValue,
    /// Stored verbatim: TS stores `input.enabled` without coercion, and both
    /// `?? true` (re-set) and truthiness (status) observe the raw value.
    enabled: JsValue,
}

type ServerTable = Vec<(JsValue, Vec<(JsString, ServerEntry)>)>;
type OverrideTable = Vec<(JsString, Vec<(JsString, JsString)>)>;
type RuleTable = Vec<(JsString, Vec<(JsString, SessionValue)>)>;

/// SDK-owned MCP state for all sessions.
pub struct SdkMcpConfig {
    options: SdkMcpConfigOptions,
    /// JS `Map<string, Map<...>>` insertion order.
    servers: Mutex<ServerTable>,
    overrides: Mutex<OverrideTable>,
    /// Generated `PermissionRule` objects, kept by identity with the ask array.
    rules: Mutex<RuleTable>,
    projects: Mutex<Vec<(JsValue, JsString)>>,
}

impl SdkMcpConfig {
    pub fn new(options: SdkMcpConfigOptions) -> Self {
        Self {
            options,
            servers: Mutex::new(Vec::new()),
            overrides: Mutex::new(Vec::new()),
            rules: Mutex::new(Vec::new()),
            projects: Mutex::new(Vec::new()),
        }
    }

    /// `setMcpServersForSdk`.
    pub async fn set_mcp_servers(&self, input: &JsValue) -> Result<JsValue, SdkMcpError> {
        let input = require_input(input, "projectKey")?;
        let project_key = input.get("projectKey").and_then(JsValue::as_string);
        let runtime = self.options.resolver.resolve(project_key).await?;
        runtime.refresh().await?;
        let project_plugins = runtime.snapshot().await;
        let session_key = input
            .get("sessionKey")
            .cloned()
            .unwrap_or(JsValue::Undefined);

        let mut reserved: Vec<JsString> = Vec::new();
        for plugin in &project_plugins {
            collect_plugin_mcp_names(plugin.mcp_servers.as_ref(), &mut reserved);
        }
        for plugin in self.options.session_plugins.plugins(&session_key) {
            collect_plugin_mcp_names(plugin.mcp_servers.as_ref(), &mut reserved);
        }

        let project_root = runtime.project_root();
        // The whole read-modify-write runs under one guard: TS has no await
        // between reading `current` and writing `next`, so a concurrent
        // toggle/set must not interleave with this snapshot on the Rust side.
        // The block ends the guard before `closeSession` is awaited.
        let added: Vec<JsString>;
        let removed: Vec<JsString>;
        {
            let mut servers = lock(&self.servers);
            let session_index = servers
                .iter()
                .position(|(key, _)| same_key(key, &session_key));
            let current: Vec<(JsString, ServerEntry)> = session_index
                .map(|index| servers[index].1.clone())
                .unwrap_or_default();
            let mut next: Vec<(JsString, ServerEntry)> = Vec::new();
            let mut errors: Vec<(JsString, &'static str)> = Vec::new();
            for (name, config) in own_entries(input.get("servers")) {
                if reserved.iter().any(|existing| existing == &name) {
                    errors.push((name, RESERVED_NAME));
                    continue;
                }
                match check_config(&name, &config, self.options.environment.as_ref())? {
                    ConfigCheck::Valid => {
                        // TS calls `structuredClone(config)` for every accepted
                        // entry; both it and the later `JSON.stringify(value)`
                        // go through the shared wire-depth bound, including
                        // new names.
                        json_text(&config)?;
                        let enabled = current
                            .iter()
                            .find(|(existing, _)| existing == &name)
                            .map(|(_, entry)| entry.enabled.clone())
                            .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
                            .unwrap_or(JsValue::Bool(true));
                        next.push((
                            name,
                            ServerEntry {
                                config: config.clone(),
                                enabled,
                            },
                        ));
                    }
                    ConfigCheck::Diagnostic(message) => errors.push((name, message)),
                }
            }
            if !errors.is_empty() {
                return Ok(set_result(&[], &[], &errors));
            }

            let mut added_names: Vec<JsString> = Vec::new();
            for (name, value) in &next {
                // TS serializes every next entry, so a new name cannot skip
                // the check that an existing name already performs.
                let value_text = json_text(&server_value(value))?;
                let changed = match current.iter().find(|(existing, _)| existing == name) {
                    None => true,
                    Some((_, previous)) => value_text != json_text(&server_value(previous))?,
                };
                if changed {
                    added_names.push(name.clone());
                }
            }
            added = added_names;
            removed = current
                .iter()
                .filter(|(name, _)| !next.iter().any(|(candidate, _)| candidate == name))
                .map(|(name, _)| name.clone())
                .collect();

            match (session_index, next.is_empty()) {
                (Some(index), true) => {
                    servers.remove(index);
                }
                (Some(index), false) => {
                    servers[index].1 = next;
                }
                (None, true) => {}
                (None, false) => servers.push((session_key.clone(), next)),
            }
            self.set_project(&session_key, project_root);
        }
        (self.options.close_session)(session_key, "sdk_mcp_servers_changed").await?;
        Ok(set_result(&added, &removed, &[]))
    }

    /// `setMcpPermissionModeOverrideForSdk`.
    pub async fn set_mcp_permission_mode_override(
        &self,
        input: &JsValue,
    ) -> Result<JsValue, SdkMcpError> {
        let input = require_input(input, "sessionKey")?;
        let session_key = required_trimmed(
            input.get("sessionKey"),
            "sessionKey",
            "INVALID_SESSION_KEY",
            "sessionKey is required.",
        )?;
        let server_name = required_trimmed(
            input.get("serverName"),
            "serverName",
            "INVALID_MCP_SERVER",
            "serverName is required.",
        )?;
        let mode = input.get("mode").cloned().unwrap_or(JsValue::Undefined);
        let auto = match &mode {
            JsValue::Null => {
                self.remove_override(&session_key, &server_name);
                false
            }
            JsValue::String(value) if value.equals_str("default") => {
                self.set_override(&session_key, &server_name, value.clone());
                false
            }
            JsValue::String(value) if value.equals_str("auto") => {
                self.set_override(&session_key, &server_name, value.clone());
                true
            }
            other => {
                let mut units = "Unsupported MCP permission mode: "
                    .encode_utf16()
                    .collect::<Vec<_>>();
                units.extend(js_value::to_string_units(other).map_err(range_error)?);
                return Err(SdkMcpError::gateway(
                    "INVALID_MCP_PERMISSION_MODE",
                    JsString::from_utf16(units),
                ));
            }
        };
        self.options
            .permission_rule_sets
            .update_ask_rules(&session_key, |ask| {
                self.sync_permission_rules(&session_key, ask)
            });
        if auto {
            let mut object = JsObject::new();
            object.insert(
                "warning".into(),
                JsValue::String(AUTO_PERMISSION_WARNING.into()),
            );
            Ok(JsValue::Object(object))
        } else {
            Ok(JsValue::Object(JsObject::new()))
        }
    }

    /// `toggleMcpServerForSdk`.
    pub async fn toggle_mcp_server(&self, input: &JsValue) -> Result<(), SdkMcpError> {
        let input = require_input(input, "sessionKey")?;
        let session_key = input
            .get("sessionKey")
            .cloned()
            .unwrap_or(JsValue::Undefined);
        let server_name = input
            .get("serverName")
            .cloned()
            .unwrap_or(JsValue::Undefined);
        let enabled = input.get("enabled").cloned().unwrap_or(JsValue::Undefined);
        {
            let mut servers = lock(&self.servers);
            let server = servers
                .iter_mut()
                .find(|(key, _)| same_key(key, &session_key))
                .and_then(|(_, entries)| {
                    entries
                        .iter_mut()
                        .find(|(name, _)| server_name.as_string() == Some(name))
                });
            let Some((_, entry)) = server else {
                return Err(not_found(&server_name, &session_key)?);
            };
            entry.enabled = enabled;
        }
        (self.options.close_session)(session_key, "sdk_mcp_server_toggled").await
    }

    /// `reconnectMcpServerForSdk`.
    pub async fn reconnect_mcp_server(&self, input: &JsValue) -> Result<(), SdkMcpError> {
        let input = require_input(input, "sessionKey")?;
        let session_key = input
            .get("sessionKey")
            .cloned()
            .unwrap_or(JsValue::Undefined);
        let server_name = input
            .get("serverName")
            .cloned()
            .unwrap_or(JsValue::Undefined);
        {
            let servers = lock(&self.servers);
            let entry = servers
                .iter()
                .find(|(key, _)| same_key(key, &session_key))
                .and_then(|(_, entries)| {
                    entries
                        .iter()
                        .find(|(name, _)| server_name.as_string() == Some(name))
                });
            let Some((_, entry)) = entry else {
                return Err(not_found(&server_name, &session_key)?);
            };
            if !js_truthy(&entry.enabled) {
                return Err(SdkMcpError::gateway(
                    "MCP_SERVER_DISABLED",
                    disabled_message(&server_name)?,
                ));
            }
        }
        (self.options.close_session)(session_key, "sdk_mcp_server_reconnect").await
    }

    /// `mcpServerStatusForSdk`.
    pub async fn mcp_server_status(&self, input: &JsValue) -> Result<JsValue, SdkMcpError> {
        let input = require_input(input, "projectKey")?;
        let project_key = input.get("projectKey").and_then(JsValue::as_string);
        let runtime = self.options.resolver.resolve(project_key).await?;
        runtime.refresh().await?;
        let mut servers: Vec<(JsString, bool)> = Vec::new();
        if let Some(session_key) = input.get("sessionKey").filter(|value| js_truthy(value)) {
            for plugin in self.options.session_plugins.plugins(session_key) {
                let mut names = Vec::new();
                collect_plugin_mcp_names(plugin.mcp_servers.as_ref(), &mut names);
                for name in names {
                    set_status(&mut servers, name, true);
                }
            }
            for (name, entry) in self.servers_for(session_key) {
                set_status(&mut servers, name, js_truthy(&entry.enabled));
            }
        }
        Ok(status_result(&servers))
    }

    /// Internal state projection for tests and future adapters. Map order is
    /// preserved exactly as JS `Map` does.
    pub fn snapshot_state(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert(
            "servers".into(),
            JsValue::Array(
                lock(&self.servers)
                    .iter()
                    .map(|(key, entries)| {
                        JsValue::Array(vec![
                            key.clone(),
                            JsValue::Array(
                                entries
                                    .iter()
                                    .map(|(name, entry)| {
                                        JsValue::Array(vec![
                                            JsValue::String(name.clone()),
                                            server_value(entry),
                                        ])
                                    })
                                    .collect(),
                            ),
                        ])
                    })
                    .collect(),
            ),
        );
        object.insert(
            "overrides".into(),
            JsValue::Array(
                lock(&self.overrides)
                    .iter()
                    .map(|(key, entries)| {
                        JsValue::Array(vec![
                            JsValue::String(key.clone()),
                            JsValue::Array(
                                entries
                                    .iter()
                                    .map(|(name, mode)| {
                                        JsValue::Array(vec![
                                            JsValue::String(name.clone()),
                                            JsValue::String(mode.clone()),
                                        ])
                                    })
                                    .collect(),
                            ),
                        ])
                    })
                    .collect(),
            ),
        );
        object.insert(
            "rules".into(),
            JsValue::Array(
                lock(&self.rules)
                    .iter()
                    .map(|(key, entries)| {
                        JsValue::Array(vec![
                            JsValue::String(key.clone()),
                            JsValue::Array(
                                entries
                                    .iter()
                                    .map(|(name, rule)| {
                                        JsValue::Array(vec![
                                            JsValue::String(name.clone()),
                                            rule.to_value(),
                                        ])
                                    })
                                    .collect(),
                            ),
                        ])
                    })
                    .collect(),
            ),
        );
        object.insert(
            "projects".into(),
            JsValue::Array(
                lock(&self.projects)
                    .iter()
                    .map(|(key, root)| {
                        JsValue::Array(vec![key.clone(), JsValue::String(root.clone())])
                    })
                    .collect(),
            ),
        );
        JsValue::Object(object)
    }

    fn servers_for(&self, session_key: &JsValue) -> Vec<(JsString, ServerEntry)> {
        lock(&self.servers)
            .iter()
            .find(|(key, _)| same_key(key, session_key))
            .map(|(_, entries)| entries.clone())
            .unwrap_or_default()
    }

    fn set_project(&self, session_key: &JsValue, project_root: JsString) {
        let mut projects = lock(&self.projects);
        if let Some((_, root)) = projects
            .iter_mut()
            .find(|(key, _)| same_key(key, session_key))
        {
            *root = project_root;
        } else {
            projects.push((session_key.clone(), project_root));
        }
    }

    fn set_override(&self, session_key: &JsString, server_name: &JsString, mode: JsString) {
        let mut overrides = lock(&self.overrides);
        let index = match overrides.iter().position(|(key, _)| key == session_key) {
            Some(index) => index,
            None => {
                overrides.push((session_key.clone(), Vec::new()));
                overrides.len() - 1
            }
        };
        let entries = &mut overrides[index].1;
        if let Some((_, current)) = entries.iter_mut().find(|(name, _)| name == server_name) {
            *current = mode;
        } else {
            entries.push((server_name.clone(), mode));
        }
    }

    fn remove_override(&self, session_key: &JsString, server_name: &JsString) {
        let mut overrides = lock(&self.overrides);
        let Some(index) = overrides.iter().position(|(key, _)| key == session_key) else {
            return;
        };
        overrides[index].1.retain(|(name, _)| name != server_name);
        if overrides[index].1.is_empty() {
            overrides.remove(index);
        }
    }

    /// `syncSdkMcpPermissionRules`, called with the live ask array.
    fn sync_permission_rules(&self, session_key: &JsString, ask_rules: &mut Vec<SessionValue>) {
        let active: Vec<(JsString, JsString)> = lock(&self.overrides)
            .iter()
            .find(|(key, _)| key == session_key)
            .map(|(_, entries)| entries.clone())
            .unwrap_or_default();
        let mut generated: Vec<(JsString, SessionValue)> = lock(&self.rules)
            .iter()
            .find(|(key, _)| key == session_key)
            .map(|(_, entries)| entries.clone())
            .unwrap_or_default();

        generated.retain(|(server_name, rule)| {
            if active.iter().any(|(name, _)| name == server_name) {
                return true;
            }
            if let Some(index) = ask_rules
                .iter()
                .position(|candidate| candidate.same_object(rule))
            {
                ask_rules.remove(index);
            }
            false
        });
        for (server_name, _) in &active {
            let rule = match generated.iter().find(|(name, _)| name == server_name) {
                Some((_, rule)) => rule.clone(),
                None => {
                    let rule = generated_rule(server_name);
                    generated.push((server_name.clone(), rule.clone()));
                    rule
                }
            };
            if !ask_rules
                .iter()
                .any(|candidate| candidate.same_object(&rule))
            {
                ask_rules.push(rule);
            }
        }
        let mut rules = lock(&self.rules);
        let index = rules.iter().position(|(key, _)| key == session_key);
        match (index, generated.is_empty()) {
            (Some(index), true) => {
                rules.remove(index);
            }
            (Some(index), false) => {
                rules[index].1 = generated;
            }
            (None, false) => rules.push((session_key.clone(), generated)),
            (None, true) => {}
        }
    }
}

// ------------------------------------------------------------------ helpers

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn require_input<'a>(input: &'a JsValue, field: &str) -> Result<&'a JsValue, SdkMcpError> {
    match input {
        JsValue::Undefined | JsValue::Null => Err(SdkMcpError::type_error(format!(
            "Cannot read properties of {} (reading '{}')",
            input_kind(input),
            field
        ))),
        _ => Ok(input),
    }
}

fn input_kind(value: &JsValue) -> &'static str {
    match value {
        JsValue::Undefined => "undefined",
        JsValue::Null => "null",
        JsValue::Bool(_) => "boolean",
        JsValue::Number(_) => "number",
        JsValue::String(_) => "string",
        JsValue::Array(_) => "object",
        JsValue::Object(_) => "object",
    }
}

fn required_trimmed(
    value: Option<&JsValue>,
    field: &str,
    code: &'static str,
    message: &'static str,
) -> Result<JsString, SdkMcpError> {
    match value {
        None | Some(JsValue::Undefined) | Some(JsValue::Null) => {
            Err(SdkMcpError::gateway(code, message))
        }
        Some(JsValue::String(value)) => {
            let text = String::from_utf16_lossy(value.units());
            if crate::metadata::js_trim(&text).is_empty() {
                Err(SdkMcpError::gateway(code, message))
            } else {
                Ok(value.clone())
            }
        }
        Some(_) => Err(SdkMcpError::type_error(format!(
            "input.{field}?.trim is not a function"
        ))),
    }
}

fn js_truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}

fn same_key(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => true,
        (JsValue::Bool(a), JsValue::Bool(b)) => a == b,
        (JsValue::Number(a), JsValue::Number(b)) => a == b || (a.is_nan() && b.is_nan()),
        (JsValue::String(a), JsValue::String(b)) => a == b,
        _ => false,
    }
}

fn json_text(value: &JsValue) -> Result<Option<String>, SdkMcpError> {
    js_value::json_stringify(value).map_err(range_error)
}

fn server_value(entry: &ServerEntry) -> JsValue {
    let mut object = JsObject::new();
    object.insert("config".into(), entry.config.clone());
    object.insert("enabled".into(), entry.enabled.clone());
    JsValue::Object(object)
}

fn set_result(
    added: &[JsString],
    removed: &[JsString],
    errors: &[(JsString, &'static str)],
) -> JsValue {
    let names =
        |names: &[JsString]| JsValue::Array(names.iter().cloned().map(JsValue::String).collect());
    let mut object = JsObject::new();
    object.insert("added".into(), names(added));
    object.insert("removed".into(), names(removed));
    object.insert(
        "errors".into(),
        JsValue::Array(
            errors
                .iter()
                .map(|(name, message)| {
                    let mut error = JsObject::new();
                    error.insert("name".into(), JsValue::String(name.clone()));
                    error.insert("error".into(), JsValue::String((*message).into()));
                    JsValue::Object(error)
                })
                .collect(),
        ),
    );
    JsValue::Object(object)
}

fn status_result(servers: &[(JsString, bool)]) -> JsValue {
    let mut object = JsObject::new();
    object.insert(
        "servers".into(),
        JsValue::Array(
            servers
                .iter()
                .map(|(name, configured)| {
                    let mut entry = JsObject::new();
                    entry.insert("name".into(), JsValue::String(name.clone()));
                    entry.insert(
                        "status".into(),
                        JsValue::String(
                            if *configured {
                                "configured"
                            } else {
                                "disabled"
                            }
                            .into(),
                        ),
                    );
                    JsValue::Object(entry)
                })
                .collect(),
        ),
    );
    JsValue::Object(object)
}

fn set_status(servers: &mut Vec<(JsString, bool)>, name: JsString, configured: bool) {
    if let Some((_, status)) = servers.iter_mut().find(|(existing, _)| existing == &name) {
        *status = configured;
    } else {
        servers.push((name, configured));
    }
}

/// `Object.entries(value ?? {})` with JS own-key order: array indices first in
/// ascending numeric order, then other keys in insertion order.
fn own_entries(value: Option<&JsValue>) -> Vec<(JsString, JsValue)> {
    match value {
        Some(JsValue::Object(object)) => {
            let mut indexed = Vec::new();
            let mut ordinary = Vec::new();
            for (name, value) in object.entries() {
                match name.array_index() {
                    Some(index) => indexed.push((index, name.clone(), value.clone())),
                    None => ordinary.push((name.clone(), value.clone())),
                }
            }
            indexed.sort_by_key(|(index, _, _)| *index);
            indexed
                .into_iter()
                .map(|(_, name, value)| (name, value))
                .chain(ordinary)
                .collect()
        }
        Some(JsValue::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(index, value)| (JsString::from(index.to_string()), value.clone()))
            .collect(),
        Some(JsValue::String(value)) => value
            .units()
            .iter()
            .enumerate()
            .map(|(index, unit)| {
                (
                    JsString::from(index.to_string()),
                    JsValue::String(JsString::from_utf16(vec![*unit])),
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `Object.keys(plugin.mcpServers ?? {})`.
///
/// `LoadedPlugin.mcpServers` is a `serde_json::Value`; its default object map
/// is sorted, so this preserves the order the merged plugin type can represent.
/// A loader that needs JS manifest insertion order must carry that order at
/// its own boundary (A6.3-pr7b).
fn collect_plugin_mcp_names(value: Option<&serde_json::Value>, names: &mut Vec<JsString>) {
    match value {
        Some(serde_json::Value::Object(object)) => {
            // `Object.keys` enumerates array-index keys first in ascending
            // numeric order, then ordinary keys in insertion order. The
            // serde map only carries the latter as sorted keys; at least put
            // the computable index keys in JS order.
            let mut indexed = Vec::new();
            let mut ordinary = Vec::new();
            for key in object.keys() {
                let name = JsString::from(key.as_str());
                match name.array_index() {
                    Some(index) => indexed.push((index, name)),
                    None => ordinary.push(name),
                }
            }
            indexed.sort_by_key(|(index, _)| *index);
            names.extend(indexed.into_iter().map(|(_, name)| name));
            names.extend(ordinary);
        }
        Some(serde_json::Value::Array(items)) => {
            names.extend((0..items.len()).map(|index| JsString::from(index.to_string())));
        }
        Some(serde_json::Value::String(value)) => {
            names.extend(
                value
                    .encode_utf16()
                    .map(|unit| JsString::from_utf16(vec![unit])),
            );
        }
        _ => {}
    }
}

enum ConfigCheck {
    Valid,
    Diagnostic(&'static str),
}

/// The validation side of `toPluginMcpConfig` + `parsePluginMcpServers`: only
/// whether exactly one server is produced and no diagnostic is emitted.
fn check_config(
    name: &JsString,
    config: &JsValue,
    environment: &dyn SdkMcpEnvironment,
) -> Result<ConfigCheck, SdkMcpError> {
    match config {
        JsValue::Null => {
            return Err(SdkMcpError::type_error(
                "Cannot read properties of null (reading 'type')",
            ));
        }
        JsValue::Undefined => {
            return Err(SdkMcpError::type_error(
                "Cannot read properties of undefined (reading 'type')",
            ));
        }
        _ => {}
    }
    let stdio = config
        .get("type")
        .and_then(JsValue::as_string)
        .is_some_and(|value| value.equals_str("stdio"));
    if stdio {
        // `{ ...(config.args ? { args: [...config.args] } : {}) }`
        if let Some(args) = config.get("args").filter(|value| js_truthy(value))
            && !matches!(args, JsValue::String(_) | JsValue::Array(_))
        {
            return Err(SdkMcpError::type_error("config.args is not iterable"));
        }
    }
    if name.is_empty() {
        return Ok(ConfigCheck::Diagnostic(MISSING_SPEC));
    }
    let transport = if stdio {
        config.get("command").and_then(JsValue::as_string)
    } else {
        config.get("url").and_then(JsValue::as_string)
    };
    Ok(match transport {
        Some(value) if !expand_mcp_string(value, environment).is_empty() => ConfigCheck::Valid,
        _ => ConfigCheck::Diagnostic(NO_TRANSPORT),
    })
}

/// `expandMcpString` for the two branches that decide validation.
fn expand_mcp_string(value: &JsString, environment: &dyn SdkMcpEnvironment) -> JsString {
    let mut units = replace_env(value.units(), environment);
    let home = environment.home();
    units = replace_user_home(&units, home.units());
    if starts_with(&units, "~/") || starts_with(&units, "~\\") {
        let mut expanded = home.units().to_vec();
        expanded.extend_from_slice(&units[1..]);
        JsString::from_utf16(expanded)
    } else if units == "~".encode_utf16().collect::<Vec<_>>() {
        home
    } else {
        JsString::from_utf16(units)
    }
}

fn starts_with(units: &[u16], prefix: &str) -> bool {
    units.starts_with(&prefix.encode_utf16().collect::<Vec<_>>())
}

fn replace_env(units: &[u16], environment: &dyn SdkMcpEnvironment) -> Vec<u16> {
    const PATTERN: &[u8] = b"${env:";
    let pattern: Vec<u16> = PATTERN.iter().map(|byte| u16::from(*byte)).collect();
    let mut output = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        if units[index..].starts_with(&pattern) {
            let body = index + pattern.len();
            match units[body..]
                .iter()
                .position(|unit| *unit == u16::from(b'}'))
            {
                Some(offset) if offset > 0 => {
                    let name = JsString::from_utf16(units[body..body + offset].to_vec());
                    // An injected environment stands in for `process.env`; the
                    // prototype fallback applies whether the port reports the
                    // name as unset or does not model the chain itself.
                    if let Some(value) = environment
                        .env(&name)
                        .or_else(|| object_prototype_env(&name))
                    {
                        output.extend_from_slice(value.units());
                    }
                    index = body + offset + 1;
                    continue;
                }
                _ => {}
            }
        }
        output.push(units[index]);
        index += 1;
    }
    output
}

/// `String.prototype.replace(/\$\{userHome\}/g, home)` with GetSubstitution
/// semantics: `$$`, `$&`, `` $` `` and `$'` are expanded, while `$1` and
/// `$<name>` stay literal because the pattern has no capture groups.
fn replace_user_home(input: &[u16], replacement: &[u16]) -> Vec<u16> {
    const NEEDLE: &str = "${userHome}";
    let needle: Vec<u16> = NEEDLE.encode_utf16().collect();
    let mut output = Vec::with_capacity(input.len());
    let mut copied = 0;
    let mut search = 0;
    while let Some(offset) = find_units(&input[search..], &needle) {
        let start = search + offset;
        let end = start + needle.len();
        output.extend_from_slice(&input[copied..start]);
        expand_replacement(&mut output, replacement, input, start, end);
        copied = end;
        search = end;
    }
    output.extend_from_slice(&input[copied..]);
    output
}

fn find_units(haystack: &[u16], needle: &[u16]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn expand_replacement(
    output: &mut Vec<u16>,
    replacement: &[u16],
    input: &[u16],
    start: usize,
    end: usize,
) {
    let dollar = u16::from(b'$');
    let mut index = 0;
    while index < replacement.len() {
        if replacement[index] != dollar || index + 1 == replacement.len() {
            output.push(replacement[index]);
            index += 1;
            continue;
        }
        match replacement[index + 1] {
            unit if unit == u16::from(b'$') => {
                output.push(dollar);
                index += 2;
            }
            unit if unit == u16::from(b'&') => {
                output.extend_from_slice(&input[start..end]);
                index += 2;
            }
            unit if unit == u16::from(b'`') => {
                output.extend_from_slice(&input[..start]);
                index += 2;
            }
            unit if unit == u16::from(b'\'') => {
                output.extend_from_slice(&input[end..]);
                index += 2;
            }
            _ => {
                output.push(dollar);
                index += 1;
            }
        }
    }
}

fn generated_rule(server_name: &JsString) -> SessionValue {
    let mut tool_name = Vec::new();
    tool_name.extend("mcp__".encode_utf16());
    tool_name.extend_from_slice(normalize_mcp_permission_segment(server_name).units());
    tool_name.extend("__*".encode_utf16());
    let rule =
        PermissionRule::new("session", "ask", JsString::from_utf16(tool_name)).with_force(true);
    SessionValue::from(rule.to_value())
}

/// `normalizeMcpPermissionSegment`: ASCII non-word to `_`, collapse `_`, trim
/// leading/trailing `_`; fall back to the original value when empty.
fn normalize_mcp_permission_segment(value: &JsString) -> JsString {
    let mut normalized = Vec::with_capacity(value.units().len());
    let mut previous_underscore = false;
    for unit in value.units() {
        let allowed = u16::from(b'A') <= *unit && *unit <= u16::from(b'Z')
            || u16::from(b'a') <= *unit && *unit <= u16::from(b'z')
            || u16::from(b'0') <= *unit && *unit <= u16::from(b'9')
            || *unit == u16::from(b'_');
        let unit = if allowed { *unit } else { u16::from(b'_') };
        if unit == u16::from(b'_') {
            if previous_underscore {
                continue;
            }
            previous_underscore = true;
        } else {
            previous_underscore = false;
        }
        normalized.push(unit);
    }
    while normalized.first() == Some(&u16::from(b'_')) {
        normalized.remove(0);
    }
    while normalized.last() == Some(&u16::from(b'_')) {
        normalized.pop();
    }
    if normalized.is_empty() {
        value.clone()
    } else {
        JsString::from_utf16(normalized)
    }
}

fn not_found(server_name: &JsValue, session_key: &JsValue) -> Result<SdkMcpError, SdkMcpError> {
    let mut units = Vec::new();
    units.extend("SDK-owned MCP server ".encode_utf16());
    units.extend(js_value::to_string_units(server_name).map_err(range_error)?);
    units.extend(" is not configured for session ".encode_utf16());
    units.extend(js_value::to_string_units(session_key).map_err(range_error)?);
    units.push(u16::from(b'.'));
    Ok(SdkMcpError::gateway(
        "MCP_SERVER_NOT_FOUND",
        JsString::from_utf16(units),
    ))
}

fn disabled_message(server_name: &JsValue) -> Result<JsString, SdkMcpError> {
    let mut units = Vec::new();
    units.extend("SDK-owned MCP server ".encode_utf16());
    units.extend(js_value::to_string_units(server_name).map_err(range_error)?);
    units.extend(" is disabled; enable it before reconnecting.".encode_utf16());
    Ok(JsString::from_utf16(units))
}

fn range_error(_: js_value::StackOverflow) -> SdkMcpError {
    SdkMcpError::plain("RangeError", js_value::StackOverflow::MESSAGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_segments_like_the_baseline() {
        for (input, expected) in [
            ("a b", "a_b"),
            ("a__b", "a_b"),
            ("_a_", "a"),
            ("a-b-c", "a_b_c"),
            ("---", "---"),
            ("😀", "😀"),
            ("a😀b", "a_b"),
            ("A_z9", "A_z9"),
        ] {
            assert_eq!(
                normalize_mcp_permission_segment(&JsString::from(input)),
                JsString::from(expected),
                "{input}"
            );
        }
    }

    #[test]
    fn expands_placeholders_and_home() {
        struct Env;
        impl SdkMcpEnvironment for Env {
            fn env(&self, name: &JsString) -> Option<JsString> {
                match name.to_utf8().ok()?.as_str() {
                    "A" => Some("one".into()),
                    "NESTED" => Some("${userHome}/x".into()),
                    _ => None,
                }
            }
            fn home(&self) -> JsString {
                "/home/u".into()
            }
        }
        for (input, expected) in [
            ("${env:A}", "one"),
            ("${env:NESTED}", "/home/u/x"),
            ("${env:UNSET}", ""),
            ("${env:toString}", "function toString() { [native code] }"),
            ("${env:constructor}", "function () { [native code] }"),
            ("${env:__proto__}", "[object Object]"),
            ("~/x", "/home/u/x"),
            ("~", "/home/u"),
            ("~\\x", "/home/u\\x"),
            ("${env:}", "${env:}"),
        ] {
            assert_eq!(
                String::from_utf16_lossy(expand_mcp_string(&JsString::from(input), &Env).units()),
                expected,
                "{input}"
            );
        }
    }

    #[test]
    fn expands_user_home_through_get_substitution() {
        struct Home(&'static str);
        impl SdkMcpEnvironment for Home {
            fn env(&self, _: &JsString) -> Option<JsString> {
                None
            }
            fn home(&self) -> JsString {
                JsString::from(self.0)
            }
        }
        // `String.prototype.replace(/\$\{userHome\}/g, home)` interprets `$`
        // patterns in the replacement with no capture groups.
        for (home, input, expected) in [
            ("$`", "${userHome}", ""),
            ("$$", "${userHome}", "$"),
            ("$&x", "${userHome}", "${userHome}x"),
            ("$'", "${userHome}", ""),
            ("a$&b", "${userHome}", "a${userHome}b"),
            ("$1", "${userHome}", "$1"),
            ("$", "${userHome}", "$"),
            ("$`", "x${userHome}y", "xxy"),
            ("$'", "x${userHome}y${userHome}z", "xy${userHome}zyzz"),
            ("${userHome}", "${userHome}", "${userHome}"),
        ] {
            assert_eq!(
                String::from_utf16_lossy(
                    expand_mcp_string(&JsString::from(input), &Home(home)).units()
                ),
                expected,
                "home {home:?} input {input:?}"
            );
        }
    }

    #[test]
    fn plugin_mcp_names_put_array_indices_first() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"b":{},"10":{},"2":{},"a":{}}"#).unwrap();
        let mut names = Vec::new();
        collect_plugin_mcp_names(Some(&value), &mut names);
        assert_eq!(
            names
                .iter()
                .map(|name| name.to_utf8().unwrap())
                .collect::<Vec<_>>(),
            ["2", "10", "a", "b"]
        );
    }

    #[test]
    fn own_entries_use_js_key_order() {
        let value = pilotdeck_protocol::parse_json(r#"{"b":1,"2":"two","1":"one","a":2}"#).unwrap();
        let entries = own_entries(Some(&value));
        assert_eq!(
            entries
                .iter()
                .map(|(name, _)| String::from_utf16_lossy(name.units()))
                .collect::<Vec<_>>(),
            ["1", "2", "b", "a"]
        );
    }
}

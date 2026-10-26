//! MCP server ownership, ported from the fixed TypeScript baseline:
//!
//! - `src/mcp/runtime/parsePluginMcpServers.ts` — raw `mcpServers` blobs to
//!   [`McpServerSpec`]s (invalid entries become diagnostics, never errors);
//! - `src/mcp/runtime/projectMcpSpec.ts` — the built-in `browser-use` and
//!   `funasr` marker substitution;
//! - `src/mcp/runtime/ProjectMcpRuntimeProvider.ts` — shared runtime
//!   generations per frozen plugin contribution, reference-counted by lease;
//! - `src/mcp/runtime/SessionMcpRuntimeRegistry.ts` — per-session runtimes
//!   registered by identity, drained on dispose.
//!
//! Lifecycles mirror the TS promises with `watch` channels: a generation's
//! startup and stop are each started once and awaited by every caller.
//! Intentional repair: stopping project generations cannot issue new leases;
//! disposal drains outstanding leases before stopping their runtimes.

use super::bridge::{
    CreateToolDefinitionsOptions, McpBridgeTool, create_mcp_tool_definitions_from_runtime,
};
use super::protocol::{
    McpEnvironment, McpServerSpec, McpStatus, McpTransportSpec, expand_mcp_string,
    load_mcp_server_config,
};
use super::runtime::{McpRuntimeFactory, McpRuntimePort, SessionMcpRuntime};
use crate::{js_value, runtime::model::http::ProxySettings, runtime::project::RuntimeError};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};
use tokio::sync::watch;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// `{ state: "active" | "draining" | "disposed" }` of both owners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpOwnerState {
    Active,
    Draining,
    Disposed,
}
impl McpOwnerState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Disposed => "disposed",
        }
    }
}

fn aggregate(message: &str, mut failures: Vec<RuntimeError>) -> Result<(), RuntimeError> {
    match failures.len() {
        0 => Ok(()),
        1 => Err(failures.remove(0)),
        _ => Err(RuntimeError {
            message: message.into(),
            causes: failures,
            js_error: None,
        }),
    }
}

// ---------------------------------------------------------------------------
// parsePluginMcpServers
// ---------------------------------------------------------------------------

/// `{ id, message }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerDiagnostic {
    pub id: String,
    pub message: String,
}

/// `ParsePluginMcpServersResult`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsePluginMcpServersResult {
    pub servers: Vec<McpServerSpec>,
    pub diagnostics: Vec<McpServerDiagnostic>,
}

fn utf8(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

fn expand(value: &JsString, environment: &dyn McpEnvironment) -> String {
    utf8(&expand_mcp_string(value, environment))
}

/// `Object.entries(value)` for an object or array.
fn entries(value: &JsValue) -> Vec<(JsString, &JsValue)> {
    match value {
        JsValue::Object(object) => object
            .own_entries()
            .into_iter()
            .map(|(key, value)| (key.clone(), value))
            .collect(),
        JsValue::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, value)| (JsString::from(index.to_string()), value))
            .collect(),
        _ => Vec::new(),
    }
}

/// `isStringRecord(v)` + `expandStringRecord(v)`.
fn string_record(
    value: Option<&JsValue>,
    environment: &dyn McpEnvironment,
) -> Option<Vec<(String, String)>> {
    let value = value?;
    if !matches!(value, JsValue::Object(_) | JsValue::Array(_)) {
        return None;
    }
    let pairs = entries(value);
    if !pairs.iter().all(|(_, v)| matches!(v, JsValue::String(_))) {
        return None;
    }
    Some(
        pairs
            .into_iter()
            .map(|(key, v)| {
                (
                    utf8(&key),
                    expand(v.as_string().expect("checked"), environment),
                )
            })
            .collect(),
    )
}

/// `parsePluginMcpServers(raw)`.
pub fn parse_plugin_mcp_servers(
    raw: Option<&JsObject>,
    environment: &dyn McpEnvironment,
) -> ParsePluginMcpServersResult {
    let mut result = ParsePluginMcpServersResult::default();
    let Some(raw) = raw else {
        return result;
    };
    for (id, value) in raw.own_entries() {
        let name = utf8(id);
        let object = match value {
            JsValue::Object(_) | JsValue::Array(_) if !id.is_empty() => value,
            _ => {
                result.diagnostics.push(McpServerDiagnostic {
                    id: name,
                    message: "missing or non-object spec".into(),
                });
                continue;
            }
        };
        let get = |key: &str| match object {
            JsValue::Object(object) => object.get(key),
            _ => None,
        };
        let string = |key: &str| get(key).and_then(JsValue::as_string);
        let command = string("command").map(|c| expand(c, environment));
        if let Some(command) = command.filter(|c| !c.is_empty()) {
            let args = match get("args") {
                Some(JsValue::Array(items)) => Some(
                    items
                        .iter()
                        .filter_map(JsValue::as_string)
                        .map(|a| expand(a, environment))
                        .collect(),
                ),
                _ => None,
            };
            result.servers.push(McpServerSpec {
                id: name,
                transport: McpTransportSpec::Stdio {
                    command,
                    args,
                    env: string_record(get("env"), environment),
                    cwd: string("cwd").map(|c| expand(c, environment)),
                    per_session: matches!(get("perSession"), Some(JsValue::Bool(true))),
                },
                call_timeout_ms: get("callTimeoutMs")
                    .and_then(JsValue::as_number)
                    .filter(|n| n.is_finite() && *n > 0.0),
            });
            continue;
        }
        let url = string("url")
            .or_else(|| string("httpUrl"))
            .map(|u| expand(u, environment));
        if let Some(url) = url.filter(|u| !u.is_empty()) {
            // The baseline expands the URL a second time.
            let url = expand(&JsString::from(url), environment);
            let headers = string_record(get("headers"), environment);
            let sse = string("transport").is_some_and(|t| t.equals_str("sse"));
            result.servers.push(McpServerSpec {
                id: name,
                transport: if sse {
                    McpTransportSpec::Sse { url, headers }
                } else {
                    McpTransportSpec::StreamableHttp { url, headers }
                },
                call_timeout_ms: None,
            });
            continue;
        }
        result.diagnostics.push(McpServerDiagnostic {
            id: name,
            message: "no recognized transport (need command or url)".into(),
        });
    }
    result
}

/// The TS spec object (`JSON.stringify` drops `undefined` keys).
pub fn server_spec_value(spec: &McpServerSpec) -> JsValue {
    let mut object = JsObject::new();
    let string = |value: &str| JsValue::String(value.into());
    let record = |pairs: &[(String, String)]| {
        let mut out = JsObject::new();
        for (key, value) in pairs {
            out.insert(key.as_str().into(), string(value));
        }
        JsValue::Object(out)
    };
    object.insert("id".into(), string(&spec.id));
    match &spec.transport {
        McpTransportSpec::Stdio {
            command,
            args,
            env,
            cwd,
            per_session,
        } => {
            object.insert("transport".into(), string("stdio"));
            object.insert("command".into(), string(command));
            if let Some(args) = args {
                object.insert(
                    "args".into(),
                    JsValue::Array(args.iter().map(|a| string(a)).collect()),
                );
            }
            if let Some(env) = env {
                object.insert("env".into(), record(env));
            }
            if let Some(cwd) = cwd {
                object.insert("cwd".into(), string(cwd));
            }
            if let Some(timeout) = spec.call_timeout_ms {
                object.insert("callTimeoutMs".into(), JsValue::Number(timeout));
            }
            if *per_session {
                object.insert("perSession".into(), JsValue::Bool(true));
            }
        }
        McpTransportSpec::StreamableHttp { url, headers }
        | McpTransportSpec::Sse { url, headers } => {
            let kind = if matches!(spec.transport, McpTransportSpec::Sse { .. }) {
                "sse"
            } else {
                "streamable_http"
            };
            object.insert("transport".into(), string(kind));
            object.insert("url".into(), string(url));
            if let Some(headers) = headers {
                object.insert("headers".into(), record(headers));
            }
            if let Some(timeout) = spec.call_timeout_ms {
                object.insert("callTimeoutMs".into(), JsValue::Number(timeout));
            }
        }
    }
    JsValue::Object(object)
}

// ---------------------------------------------------------------------------
// projectMcpSpec
// ---------------------------------------------------------------------------

pub const PILOTDECK_PROJECT_ROOT_MARKER: &str = "__PILOTDECK_PROJECT_ROOT__";
pub const PILOTDECK_NODE_EXECUTABLE_MARKER: &str = "__PILOTDECK_NODE_EXECUTABLE__";
pub const PILOTDECK_FUNASR_MCP_ENTRYPOINT_MARKER: &str = "__PILOTDECK_FUNASR_MCP_ENTRYPOINT__";
pub const PILOTDECK_FUNASR_RUNTIME_ROOT_MARKER: &str = "__PILOTDECK_FUNASR_RUNTIME_ROOT__";
pub const PILOTDECK_FUNASR_INSTALL_COMMAND_MARKER: &str = "__PILOTDECK_FUNASR_INSTALL_COMMAND__";
pub const PILOTDECK_PLAYWRIGHT_MCP_ENTRYPOINT_MARKER: &str =
    "__PILOTDECK_PLAYWRIGHT_MCP_ENTRYPOINT__";

/// The host paths the built-in markers resolve to. The TS module derives
/// them from `process.execPath` and its own install location; the Rust
/// gateway is not a Node process, so the host injects them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuiltinMcpPaths {
    /// `process.execPath`.
    pub node_executable: String,
    /// `playwrightMcpEntrypoint()`.
    pub playwright_mcp_entrypoint: String,
    /// `funasrEntrypoint()`.
    pub funasr_mcp_entrypoint: String,
    /// `getPilotDeckInstallCommand()`.
    pub install_command: String,
}

/// `path.resolve(p)` (POSIX, lexical).
pub(crate) fn resolve_path(path: &str) -> String {
    crate::metadata::resolve_process_path(Path::new(""), Path::new(path))
        .to_string_lossy()
        .into_owned()
}

/// `value.replaceAll(marker, replacement)` with GetSubstitution.
fn replace_all(value: &str, marker: &str, replacement: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = 0;
    for (at, _) in value.match_indices(marker) {
        out.push_str(&value[rest..at]);
        out.push_str(&js_value::expand_replacement(
            replacement,
            marker,
            &value[..at],
            &value[at + marker.len()..],
        ));
        rest = at + marker.len();
    }
    out.push_str(&value[rest..]);
    out
}

/// `patchProjectScopedMcpSpec(spec, projectRoot, pilotHome)`.
pub fn patch_project_scoped_mcp_spec(
    spec: McpServerSpec,
    project_root: &str,
    pilot_home: &str,
    paths: &BuiltinMcpPaths,
) -> McpServerSpec {
    let McpTransportSpec::Stdio {
        command,
        args,
        env,
        per_session,
        ..
    } = &spec.transport
    else {
        return spec;
    };
    let replacements: Vec<(&str, String)> = match spec.id.as_str() {
        "browser-use" => vec![
            (
                PILOTDECK_NODE_EXECUTABLE_MARKER,
                paths.node_executable.clone(),
            ),
            (
                PILOTDECK_PLAYWRIGHT_MCP_ENTRYPOINT_MARKER,
                paths.playwright_mcp_entrypoint.clone(),
            ),
        ],
        "funasr" => vec![
            (PILOTDECK_PROJECT_ROOT_MARKER, resolve_path(project_root)),
            (
                PILOTDECK_NODE_EXECUTABLE_MARKER,
                paths.node_executable.clone(),
            ),
            (
                PILOTDECK_FUNASR_MCP_ENTRYPOINT_MARKER,
                paths.funasr_mcp_entrypoint.clone(),
            ),
            (
                PILOTDECK_FUNASR_RUNTIME_ROOT_MARKER,
                format!("{}/funasr", resolve_path(pilot_home).trim_end_matches('/')),
            ),
            (
                PILOTDECK_FUNASR_INSTALL_COMMAND_MARKER,
                paths.install_command.clone(),
            ),
        ],
        _ => return spec,
    };
    let patch = |value: &str| {
        replacements
            .iter()
            .fold(value.to_owned(), |out, (marker, replacement)| {
                replace_all(&out, marker, replacement)
            })
    };
    McpServerSpec {
        transport: McpTransportSpec::Stdio {
            command: patch(command),
            args: args
                .as_ref()
                .map(|args| args.iter().map(|a| patch(a)).collect()),
            env: env.clone(),
            cwd: Some(resolve_path(project_root)),
            per_session: *per_session,
        },
        ..spec
    }
}

// ---------------------------------------------------------------------------
// SessionMcpRuntimeRegistry
// ---------------------------------------------------------------------------

type Settle = watch::Receiver<Option<Result<(), RuntimeError>>>;

async fn settled(mut receiver: Settle) -> Result<(), RuntimeError> {
    match receiver.wait_for(Option::is_some).await {
        Ok(value) => value.clone().expect("checked by wait_for"),
        Err(_) => Err(RuntimeError::new("MCP lifecycle task ended unexpectedly")),
    }
}

/// Runs `future` once on a task; every caller awaits the same outcome.
fn shared<F>(future: F) -> Settle
where
    F: std::future::Future<Output = Result<(), RuntimeError>> + Send + 'static,
{
    shared_on(&tokio::runtime::Handle::current(), future)
}

fn shared_on<F>(executor: &tokio::runtime::Handle, future: F) -> Settle
where
    F: std::future::Future<Output = Result<(), RuntimeError>> + Send + 'static,
{
    let (sender, receiver) = watch::channel(None);
    // Keep the completion sender outside the task polling the external future.
    // A panic must settle the same shared result as an ordinary port error.
    let task = executor.spawn(future);
    executor.spawn(async move {
        let outcome = task.await.unwrap_or_else(|error| {
            let payload = error.try_into_panic().ok();
            let message = payload
                .as_ref()
                .and_then(|payload| {
                    payload
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                })
                .unwrap_or("MCP lifecycle task ended unexpectedly");
            Err(RuntimeError::new(message))
        });
        sender.send_replace(Some(outcome));
    });
    receiver
}

struct SessionEntry {
    id: u64,
    runtime: Arc<dyn SessionMcpRuntime>,
    stop: Mutex<Option<Settle>>,
}
impl SessionEntry {
    fn stop(&self) -> Settle {
        lock(&self.stop)
            .get_or_insert_with(|| {
                let runtime = self.runtime.clone();
                shared(async move { runtime.stop().await })
            })
            .clone()
    }
}

#[derive(Default)]
struct RegistryState {
    by_session: Vec<(String, Vec<Arc<SessionEntry>>)>,
    state: Option<McpOwnerState>,
    next_id: u64,
    dispose: Option<Settle>,
}

/// `SessionMcpRuntimeRegistry`: per-session runtimes by identity, so an old
/// and a replacement handle under the same key each release only their own.
#[derive(Clone, Default)]
pub struct SessionMcpRuntimeRegistry {
    inner: Arc<Mutex<RegistryState>>,
}

/// `SessionMcpRuntimeRegistration`: dispose once releases the entry.
pub struct SessionMcpRuntimeRegistration {
    registry: SessionMcpRuntimeRegistry,
    session_key: String,
    entry: u64,
    released: Mutex<bool>,
}
impl SessionMcpRuntimeRegistration {
    pub async fn dispose(&self) -> Result<(), RuntimeError> {
        if std::mem::replace(&mut *lock(&self.released), true) {
            return Ok(());
        }
        self.registry.release(&self.session_key, self.entry).await
    }
}

impl SessionMcpRuntimeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> McpOwnerState {
        lock(&self.inner).state.unwrap_or(McpOwnerState::Active)
    }

    pub fn size(&self) -> usize {
        lock(&self.inner)
            .by_session
            .iter()
            .map(|(_, entries)| entries.len())
            .sum()
    }

    pub fn register(
        &self,
        session_key: &str,
        runtime: Arc<dyn SessionMcpRuntime>,
    ) -> Result<SessionMcpRuntimeRegistration, RuntimeError> {
        let mut state = lock(&self.inner);
        let current = state.state.unwrap_or(McpOwnerState::Active);
        if current != McpOwnerState::Active {
            return Err(RuntimeError::new(format!(
                "Cannot register per-session MCP runtime; registry is {}.",
                current.as_str()
            )));
        }
        state.next_id += 1;
        let id = state.next_id;
        let entry = Arc::new(SessionEntry {
            id,
            runtime,
            stop: Mutex::new(None),
        });
        match state
            .by_session
            .iter_mut()
            .find(|(key, _)| key == session_key)
        {
            Some((_, entries)) => entries.push(entry),
            None => state.by_session.push((session_key.to_owned(), vec![entry])),
        }
        Ok(SessionMcpRuntimeRegistration {
            registry: self.clone(),
            session_key: session_key.to_owned(),
            entry: id,
            released: Mutex::new(false),
        })
    }

    /// The synchronous half of `release`: unlink the entry and start its
    /// stop. `None` when it was already released.
    fn take(state: &mut RegistryState, session_key: &str, id: u64) -> Option<Settle> {
        let index = state
            .by_session
            .iter()
            .position(|(key, _)| key == session_key)?;
        let entries = &mut state.by_session[index].1;
        let at = entries.iter().position(|entry| entry.id == id)?;
        let entry = entries.remove(at);
        if entries.is_empty() {
            state.by_session.remove(index);
        }
        Some(entry.stop())
    }

    async fn release(&self, session_key: &str, id: u64) -> Result<(), RuntimeError> {
        let stop = Self::take(&mut lock(&self.inner), session_key, id);
        match stop {
            Some(stop) => settled(stop).await,
            None => Ok(()),
        }
    }

    /// Drain every entry; memoized.
    pub async fn dispose(&self) -> Result<(), RuntimeError> {
        let receiver = {
            let mut state = lock(&self.inner);
            if let Some(dispose) = &state.dispose {
                dispose.clone()
            } else {
                state.state = Some(McpOwnerState::Draining);
                // Every entry is unlinked and its stop started now, like the
                // synchronous part of the TS `release` calls.
                let entries: Vec<(String, u64)> = state
                    .by_session
                    .iter()
                    .flat_map(|(key, entries)| entries.iter().map(move |e| (key.clone(), e.id)))
                    .collect();
                let stops: Vec<Settle> = entries
                    .iter()
                    .filter_map(|(key, id)| Self::take(&mut state, key, *id))
                    .collect();
                let registry = self.clone();
                let receiver = shared(async move {
                    let mut failures = Vec::new();
                    for stop in stops {
                        if let Err(error) = settled(stop).await {
                            failures.push(error);
                        }
                    }
                    lock(&registry.inner).state = Some(McpOwnerState::Disposed);
                    aggregate("Failed to dispose per-session MCP runtimes.", failures)
                });
                state.dispose = Some(receiver.clone());
                receiver
            }
        };
        settled(receiver).await
    }
}

// ---------------------------------------------------------------------------
// ProjectMcpRuntimeProvider
// ---------------------------------------------------------------------------

/// Diagnostic sink (`onDiagnostic(message, error?)`).
pub type McpDiagnosticSink = Arc<dyn Fn(String, Option<RuntimeError>) + Send + Sync>;

/// `ProjectMcpRuntimeProviderOptions`.
#[derive(Clone)]
pub struct ProjectMcpRuntimeProviderOptions {
    pub project_root: String,
    pub pilot_home: String,
    pub create_runtime: Option<McpRuntimeFactory>,
    /// `pilotdeck.yaml` `proxy` source for the default runtime factory.
    pub proxy: Option<ProxySettings>,
    pub on_diagnostic: Option<McpDiagnosticSink>,
    pub environment: Arc<dyn McpEnvironment>,
    pub builtin_paths: BuiltinMcpPaths,
}

struct SharedEntry {
    signature: String,
    runtime: Arc<dyn McpRuntimePort>,
    // A lease may be dropped by a plain thread with no current Tokio runtime.
    executor: tokio::runtime::Handle,
    /// Written by the startup task through its own strong handle, so the
    /// tools land whether or not the entry is still referenced (DeepSeek
    /// P1-1: a `Weak` written during `Arc::new_cyclic` could not upgrade).
    tools: Arc<Mutex<Vec<Arc<McpBridgeTool>>>>,
    // Changed only while holding provider.state; watch lets stop await drain.
    leases: watch::Sender<usize>,
    ready: Settle,
    stop: Mutex<Option<Settle>>,
}

#[derive(Default)]
struct ProviderState {
    state: Option<McpOwnerState>,
    entries: Vec<Arc<SharedEntry>>,
    configured: Option<JsObject>,
    dispose: Option<Settle>,
}

/// `ProjectMcpRuntimeProvider`.
#[derive(Clone)]
pub struct ProjectMcpRuntimeProvider {
    options: Arc<ProjectMcpRuntimeProviderOptions>,
    state: Arc<Mutex<ProviderState>>,
}

/// `ProjectMcpRuntimeLease`: the tools of one retained generation.
pub struct ProjectMcpRuntimeLease {
    tools: Vec<Arc<McpBridgeTool>>,
    entry: Option<(ProjectMcpRuntimeProvider, Arc<SharedEntry>)>,
    released: Mutex<bool>,
}
impl ProjectMcpRuntimeLease {
    fn empty() -> Self {
        Self {
            tools: Vec::new(),
            entry: None,
            released: Mutex::new(false),
        }
    }
    pub fn tools(&self) -> &[Arc<McpBridgeTool>] {
        &self.tools
    }
    pub async fn release(&self) -> Result<(), RuntimeError> {
        if std::mem::replace(&mut *lock(&self.released), true) {
            return Ok(());
        }
        match &self.entry {
            Some((provider, entry)) => provider.release_entry(entry).await,
            None => Ok(()),
        }
    }
}

impl Drop for ProjectMcpRuntimeLease {
    fn drop(&mut self) {
        if !std::mem::replace(&mut *lock(&self.released), true)
            && let Some((provider, entry)) = &self.entry
        {
            // Synchronously return ownership; the originating executor owns
            // any asynchronous stop even if this task has been cancelled.
            let _ = provider.release_entry_now(entry);
        }
    }
}

/// `stableServerSignature`: specs sorted by id, keys sorted, `undefined`
/// dropped. Only equality matters, so code-unit order stands in for
/// `localeCompare` (both are total and deterministic on distinct ids).
fn signature(servers: &[McpServerSpec]) -> String {
    fn sorted(value: &JsValue) -> JsValue {
        match value {
            JsValue::Object(object) => {
                let mut pairs = object.own_entries();
                pairs.sort_by(|a, b| a.0.units().cmp(b.0.units()));
                let mut out = JsObject::new();
                for (key, item) in pairs {
                    out.insert(key.clone(), sorted(item));
                }
                JsValue::Object(out)
            }
            JsValue::Array(items) => JsValue::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    let mut specs: Vec<&McpServerSpec> = servers.iter().collect();
    specs.sort_by(|a, b| a.id.cmp(&b.id));
    let value = JsValue::Array(
        specs
            .into_iter()
            .map(|s| sorted(&server_spec_value(s)))
            .collect(),
    );
    js_value::json_stringify(&value)
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn is_shared(spec: &McpServerSpec) -> bool {
    !matches!(
        spec.transport,
        McpTransportSpec::Stdio {
            per_session: true,
            ..
        }
    )
}

impl ProjectMcpRuntimeProvider {
    pub fn new(options: ProjectMcpRuntimeProviderOptions) -> Self {
        Self {
            options: Arc::new(options),
            state: Arc::new(Mutex::new(ProviderState::default())),
        }
    }

    pub fn state(&self) -> McpOwnerState {
        lock(&self.state).state.unwrap_or(McpOwnerState::Active)
    }

    fn diagnostic(&self, message: String, error: Option<RuntimeError>) {
        if let Some(sink) = &self.options.on_diagnostic {
            sink(message, error);
        }
    }

    /// `getConfiguredServers()`: loaded once.
    fn configured_servers(&self) -> JsObject {
        if let Some(configured) = &lock(&self.state).configured {
            return configured.clone();
        }
        let loaded = load_mcp_server_config(
            Path::new(&self.options.project_root),
            Path::new(&self.options.pilot_home),
            self.options.environment.as_ref(),
        );
        let servers = match loaded {
            Ok(result) => {
                for diagnostic in &result.diagnostics {
                    self.diagnostic(
                        format!(
                            "Ignoring invalid MCP config {}: {}",
                            diagnostic.path, diagnostic.message
                        ),
                        None,
                    );
                }
                result.servers
            }
            Err(error) => {
                self.diagnostic(format!("Ignoring invalid MCP config: {error}"), None);
                JsObject::new()
            }
        };
        lock(&self.state).configured = Some(servers.clone());
        servers
    }

    /// `materializeServers(pluginServers)`: `{ ...plugin, ...configured }`,
    /// parsed and patched.
    fn materialize(&self, plugin_servers: &JsObject) -> Vec<McpServerSpec> {
        let mut raw = JsObject::new();
        for (key, value) in plugin_servers.own_entries() {
            raw.insert(key.clone(), value.clone());
        }
        for (key, value) in self.configured_servers().own_entries() {
            raw.insert(key.clone(), value.clone());
        }
        let parsed = parse_plugin_mcp_servers(Some(&raw), self.options.environment.as_ref());
        for diagnostic in parsed.diagnostics {
            self.diagnostic(
                format!(
                    "Ignoring invalid MCP server {}: {}",
                    diagnostic.id, diagnostic.message
                ),
                None,
            );
        }
        parsed
            .servers
            .into_iter()
            .map(|spec| {
                patch_project_scoped_mcp_spec(
                    spec,
                    &self.options.project_root,
                    &self.options.pilot_home,
                    &self.options.builtin_paths,
                )
            })
            .collect()
    }

    /// `getPerSessionServerSpecs(pluginServers, forcePerSession)`.
    pub fn per_session_server_specs(
        &self,
        plugin_servers: &JsObject,
        force_per_session: bool,
    ) -> Option<Vec<McpServerSpec>> {
        let servers: Vec<McpServerSpec> = self
            .materialize(plugin_servers)
            .into_iter()
            .filter(|spec| force_per_session || !is_shared(spec))
            .map(|mut spec| {
                if force_per_session
                    && let McpTransportSpec::Stdio { per_session, .. } = &mut spec.transport
                {
                    *per_session = true;
                }
                spec
            })
            .collect();
        (!servers.is_empty()).then_some(servers)
    }

    fn assert_active(&self, action: &str) -> Result<(), RuntimeError> {
        let state = self.state();
        if state != McpOwnerState::Active {
            return Err(RuntimeError::new(format!(
                "Cannot {action}; MCP provider is {}.",
                state.as_str()
            )));
        }
        Ok(())
    }

    /// `acquireSharedRuntime(pluginServers)`.
    pub async fn acquire_shared_runtime(
        &self,
        plugin_servers: &JsObject,
    ) -> Result<ProjectMcpRuntimeLease, RuntimeError> {
        self.assert_active("acquire a shared MCP runtime")?;
        let servers: Vec<McpServerSpec> = self
            .materialize(plugin_servers)
            .into_iter()
            .filter(is_shared)
            .collect();
        if servers.is_empty() {
            self.assert_active("acquire a shared MCP runtime")?;
            return Ok(ProjectMcpRuntimeLease::empty());
        }
        let key = signature(&servers);
        loop {
            let entry = {
                let mut state = lock(&self.state);
                Self::check_active(&state)?;
                match state
                    .entries
                    .iter()
                    .find(|e| e.signature == key && lock(&e.stop).is_none())
                {
                    Some(entry) => entry.clone(),
                    None => {
                        let proxy = self.options.proxy.clone();
                        let factory = self.options.create_runtime.clone().unwrap_or_else(|| {
                            super::runtime::native_mcp_runtime_factory_with_proxy(proxy)
                        });
                        let runtime = factory(servers.clone());
                        let tools = Arc::new(Mutex::new(Vec::new()));
                        let provider = self.clone();
                        let runtime_for_start = runtime.clone();
                        let tools_for_start = tools.clone();
                        let ready = shared(async move {
                            provider
                                .start_entry(runtime_for_start, tools_for_start)
                                .await
                        });
                        let entry = Arc::new(SharedEntry {
                            signature: key.clone(),
                            runtime,
                            executor: tokio::runtime::Handle::current(),
                            tools,
                            leases: watch::channel(0).0,
                            ready,
                            stop: Mutex::new(None),
                        });
                        state.entries.push(entry.clone());
                        entry
                    }
                }
            };
            if let Err(error) = settled(entry.ready.clone()).await {
                if let Err(release) = self.release_entry(&entry).await {
                    self.diagnostic(
                        "Failed to clean up unavailable MCP runtime".into(),
                        Some(release),
                    );
                }
                self.diagnostic("MCP runtime startup partial-failed".into(), Some(error));
                return Ok(ProjectMcpRuntimeLease::empty());
            }
            // Selecting an entry reserves no lease: cancelling while waiting
            // for ready must not pin it. Recheck under the same lock as release
            // and dispose, since another caller may have retired it meanwhile.
            {
                let state = lock(&self.state);
                Self::check_active(&state)?;
                if lock(&entry.stop).is_some() {
                    continue;
                }
                entry.leases.send_modify(|count| *count += 1);
            }
            let tools = lock(&entry.tools).clone();
            return Ok(ProjectMcpRuntimeLease {
                tools,
                entry: Some((self.clone(), entry)),
                released: Mutex::new(false),
            });
        }
    }

    fn check_active(state: &ProviderState) -> Result<(), RuntimeError> {
        let owner = state.state.unwrap_or(McpOwnerState::Active);
        if owner == McpOwnerState::Active {
            Ok(())
        } else {
            Err(RuntimeError::new(format!(
                "Cannot acquire a shared MCP runtime; provider is {}.",
                owner.as_str()
            )))
        }
    }

    async fn start_entry(
        &self,
        runtime: Arc<dyn McpRuntimePort>,
        slot: Arc<Mutex<Vec<Arc<McpBridgeTool>>>>,
    ) -> Result<(), RuntimeError> {
        let statuses = runtime.start().await?;
        for status in statuses {
            if status.status == McpStatus::Error {
                let label = if status.server_id == "funasr" {
                    "ASR unavailable"
                } else {
                    "MCP server unavailable"
                };
                self.diagnostic(
                    format!(
                        "{label} (server={}): {}",
                        status.server_id,
                        status.error.as_deref().unwrap_or("unknown error")
                    ),
                    None,
                );
            }
        }
        let tools = create_mcp_tool_definitions_from_runtime(
            runtime,
            CreateToolDefinitionsOptions::default(),
        )
        .await?;
        *lock(&slot) = tools;
        Ok(())
    }

    async fn release_entry(&self, entry: &Arc<SharedEntry>) -> Result<(), RuntimeError> {
        match self.release_entry_now(entry) {
            Some(stop) => settled(stop).await,
            None => Ok(()),
        }
    }

    fn release_entry_now(&self, entry: &Arc<SharedEntry>) -> Option<Settle> {
        let mut state = lock(&self.state);
        entry
            .leases
            .send_modify(|count| *count = count.saturating_sub(1));
        let drained = *entry.leases.borrow() == 0;
        drained.then(|| self.stop_entry_locked(&mut state, entry))
    }

    /// Mark stopping under the same state lock as lease admission. Keep the
    /// entry tracked until stop finishes, so dispose also waits for stops
    /// already begun by the last release. No new leases can enter afterwards.
    fn stop_entry_locked(&self, _state: &mut ProviderState, entry: &Arc<SharedEntry>) -> Settle {
        lock(&entry.stop)
            .get_or_insert_with(|| {
                let entry = entry.clone();
                let provider = self.clone();
                let executor = entry.executor.clone();
                shared_on(&executor, async move {
                    let _ = settled(entry.ready.clone()).await;
                    let mut leases = entry.leases.subscribe();
                    leases
                        .wait_for(|count| *count == 0)
                        .await
                        .map_err(|_| RuntimeError::new("MCP lease channel ended unexpectedly"))?;
                    let result = entry.runtime.stop().await;
                    let mut state = lock(&provider.state);
                    if *entry.leases.borrow() == 0
                        && let Some(at) = state.entries.iter().position(|e| Arc::ptr_eq(e, &entry))
                    {
                        state.entries.remove(at);
                    }
                    result
                })
            })
            .clone()
    }

    /// `dispose()`: reject new leases, drain existing leases, then stop every
    /// generation; memoized. Unlike the TS baseline, disposal respects leases.
    /// Ledger: specs/gateway-parity/accepted-diffs.md A6.5-MCP-DISPOSE-001 (unapproved).
    pub async fn dispose(&self) -> Result<(), RuntimeError> {
        let receiver = {
            let mut state = lock(&self.state);
            if let Some(dispose) = &state.dispose {
                dispose.clone()
            } else {
                state.state = Some(McpOwnerState::Draining);
                let entries = state.entries.clone();
                let provider = self.clone();
                let stops: Vec<_> = entries
                    .iter()
                    .map(|entry| provider.stop_entry_locked(&mut state, entry))
                    .collect();
                let receiver = shared(async move {
                    let mut failures = Vec::new();
                    for stop in stops {
                        if let Err(error) = settled(stop).await {
                            failures.push(error);
                        }
                    }
                    lock(&provider.state).state = Some(McpOwnerState::Disposed);
                    aggregate("Failed to dispose project MCP runtime provider.", failures)
                });
                state.dispose = Some(receiver.clone());
                receiver
            }
        };
        settled(receiver).await
    }

    /// Live shared generations (for diagnostics and tests).
    pub fn shared_generation_count(&self) -> usize {
        lock(&self.state).entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn panic_deadline<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(std::time::Duration::from_secs(2), future)
            .await
            .expect("spawn completion deadline")
    }

    struct PanicStop(
        Mutex<Option<crate::runtime::mcp::runtime::McpFuture<'static, Result<(), RuntimeError>>>>,
    );
    impl SessionMcpRuntime for PanicStop {
        fn stop(&self) -> crate::runtime::mcp::runtime::McpFuture<'_, Result<(), RuntimeError>> {
            self.0.lock().unwrap().take().expect("stop called once")
        }
    }

    #[tokio::test]
    async fn shared_spawn_panic_matrix() {
        use std::{
            future::Future,
            task::{Context, Waker},
        };
        let oracle = include_str!("../../../tests/fixtures/runtime-spawn-panic/oracle.json");
        crate::test_baseline::oracle(oracle);
        let oracle: serde_json::Value = serde_json::from_str(oracle).unwrap();
        let mut ids = std::collections::HashSet::new();
        for case in oracle["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["id"].as_str().unwrap().starts_with("shared-"))
        {
            let id = case["id"].as_str().unwrap();
            assert!(ids.insert(id));
            let (release, gate) = tokio::sync::oneshot::channel();
            let (started, entered) = tokio::sync::oneshot::channel();
            let after = id.contains("-P1-");
            let owner = SessionMcpRuntimeRegistry::new();
            owner
                .register(
                    "s",
                    Arc::new(PanicStop(Mutex::new(Some(Box::pin(async move {
                        started.send(()).unwrap();
                        if after {
                            gate.await.unwrap();
                        }
                        panic!("injected panic");
                    }))))),
                )
                .unwrap();
            let n = case["outcomes"].as_array().unwrap().len();
            let mut waiters: Vec<_> = (0..n).map(|_| Box::pin(owner.dispose())).collect();
            let mut cx = Context::from_waker(Waker::noop());
            for waiter in &mut waiters {
                assert!(waiter.as_mut().poll(&mut cx).is_pending());
            }
            if id.ends_with("-C") {
                drop(waiters);
                waiters = (0..n).map(|_| Box::pin(owner.dispose())).collect();
            }
            panic_deadline(entered).await.unwrap();
            if after {
                assert!(
                    waiters[0].as_mut().poll(&mut cx).is_pending(),
                    "{id}: gate remains pending"
                );
                release.send(()).unwrap();
            }
            let mut outcomes = vec![];
            for waiter in waiters {
                let error = panic_deadline(waiter).await.unwrap_err();
                outcomes.push(error.message);
            }
            assert_eq!(
                serde_json::json!(outcomes),
                case["outcomes"],
                "shared failure: {id}"
            );
            assert_eq!(owner.state(), McpOwnerState::Disposed);
            assert_eq!(owner.size(), 0);
            let receiver = lock(&owner.inner).dispose.clone().unwrap();
            assert!(
                receiver.borrow().as_ref().unwrap().is_err(),
                "shared result retained"
            );
            assert_eq!(
                panic_deadline(owner.dispose()).await.unwrap_err().message,
                "injected panic",
                "late observer"
            );
        }
        assert_eq!(ids.len(), 8, "complete shared matrix");
    }

    #[tokio::test]
    async fn shared_spawn_outcomes_and_cancelled_gate() {
        for (payload, expected) in [
            (Some("owned panic".to_string()), "owned panic"),
            (None, "MCP lifecycle task ended unexpectedly"),
        ] {
            let result = shared(async move {
                if let Some(payload) = payload {
                    std::panic::panic_any(payload);
                }
                std::panic::panic_any(7_u8);
            });
            assert_eq!(
                panic_deadline(settled(result)).await.unwrap_err().message,
                expected,
                "panic payload"
            );
        }
        let (cancel, gate) = tokio::sync::oneshot::channel::<()>();
        let receiver =
            shared(async move { gate.await.map_err(|_| RuntimeError::new("cancelled gate")) });
        assert!(receiver.borrow().is_none());
        drop(cancel);
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/runtime-spawn-panic/oracle.json"
        ))
        .unwrap();
        let case = oracle["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "predecessor-shared")
            .unwrap();
        assert_eq!(
            serde_json::json!([panic_deadline(settled(receiver)).await.unwrap_err().message]),
            case["outcomes"]
        );
        assert_eq!(
            panic_deadline(settled(shared(async { Ok(()) }))).await,
            Ok(())
        );
    }

    #[test]
    #[ignore = "requires the fixed TS baseline and tsx"]
    fn spawn_panic_regenerates_identically() {
        crate::test_baseline::regenerates_identically("runtime-spawn-panic");
    }

    #[test]
    fn registry_paths_use_shared_lexical_resolution() {
        assert_eq!(resolve_path("/a/../../图/./"), "/图");
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(resolve_path("a/../b"), cwd.join("b").to_string_lossy());
        assert_eq!(resolve_path(""), cwd.to_string_lossy());
    }

    #[test]
    fn replace_all_uses_get_substitution() {
        assert_eq!(replace_all("a_M_b_M", "_M", "[$&]"), "a[_M]_b[_M]");
        assert_eq!(replace_all("xMy", "M", "$`$'"), "xxyy");
        assert_eq!(replace_all("M", "M", "$$1"), "$1");
    }

    #[test]
    fn resolve_is_lexical() {
        assert_eq!(resolve_path("/a/./b/../c/"), "/a/c");
        assert_eq!(resolve_path("/"), "/");
    }
}

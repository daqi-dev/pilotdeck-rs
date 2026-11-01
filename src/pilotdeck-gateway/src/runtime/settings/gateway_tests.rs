//! Replay of the real TS settings gateway oracle
//! (`tests/fixtures/settings-gateway/generate.mts`, #1318). Every matrix row
//! is one replay step: the same setup actions run against the same Rust
//! layers (the #1011 session owner, the real [`ConfigStore`] over a real
//! scratch YAML tree, the real [`RefreshBundle`] with recording
//! collaborators), then the reconstructed WS response frame, the event stream
//! and the post state must match the TS bytes.
//!
//! The TS golden's `assemblyCases` (a gateway assembled without the five
//! options answers CAPABILITY_UNAVAILABLE / the unsupported projection; an
//! unknown method throws "Unknown gateway method …") belong to the wiring
//! layer this component deliberately does not touch, so the capability half
//! is replayed through a bare component (the contract is the component's own
//! option handling) while the unknown-method row is recorded only; the
//! dispatch contract (`Some` for exactly the five methods, `None`
//! otherwise) has its own dedicated test below.
//!
//! The registry's `applyFlagSettingsForSdk` / `reloadOutputStylesForSdk` are
//! mirrored over the probe's map state (the TS bodies are synchronous Map
//! mutation; the mirror keeps the exact hasOwnProperty semantics), and
//! `updatePilotLocalSettings` is mirrored by [`MirrorWriter`]: the same
//! allowlist validation, the same YAML document edit (flow maps re-emitted
//! in the canonical spaced form, blocks kept verbatim) and the same atomic
//! write, composed with the real store reload inside the adapter exactly
//! like the createLocalGateway closure.
use super::gateway::{
    ApplyFlagSettings, FlagSettings, LocalSettingsUpdate, LocalSettingsWriter, OutputStylesReload,
    ReloadOutputStyles, SettingsError, SettingsGateway, SettingsGatewayOptions, UpdateSettings,
};
use crate::config::Env;
use crate::config::store::{
    ConfigStore, Listener as StoreListener, ReloadEvent, StoreOptions,
    Subscription as StoreSubscription,
};
use crate::runtime::project::{DisposeFuture, RuntimeError, RuntimeResult};
use crate::runtime::refresh::{
    BoxFuture, ConfigChangeNotice, ConfigListener, ConfigReloadResult, MemoryMaintenance,
    NotificationServer, RefreshBundle, RefreshConfigStore, RefreshLog, RefreshOptions,
    RefreshRegistry, RefreshRouter, SdkConfigChange, Unsubscribe, WarnDetail,
};
use crate::runtime::session::{
    EvictionReason, LiveSession, LiveSessionHandle, SessionContext, SessionFactory, SessionOwner,
    SessionOwnerOptions,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

const ORACLE: &str = include_str!("../../../tests/fixtures/settings-gateway/oracle.json");

// The generator's `BASE` / `OTHER_MODEL` / `BAD_SCHEMA` / `BROKEN_YAML`
// texts, byte for byte: the store tree, the writer edits and the break
// setups all read and write these exact bytes.
const BASE: &str = "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n        other: {}\n";
const OTHER_MODEL: &str = "schemaVersion: 1\nagent: {model: parity/other}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n        other: {}\n";
const BAD_SCHEMA: &str = "schemaVersion: 2\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n        other: {}\n";
const BROKEN_YAML: &str = "agent: {model: parity/deterministic\nmodel: [unclosed";
/// The yaml package's parse-error text for the broken fixture tree; the
/// writer wraps it in its LOCAL_SETTINGS_YAML_INVALID message.
const YAML_FLOW_MAP_ERROR: &str =
    "Flow map in block collection must be sufficiently indented and end with a }";

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
        .unwrap_or_else(|| panic!("#1318 {what} must be a string"))
        .to_utf8()
        .unwrap_or_else(|_| panic!("#1318 {what} must be valid UTF-8"))
}

fn as_serde(value: &JsValue) -> serde_json::Value {
    let text = stringify_json(value)
        .expect("#1318 replay value must be serializable")
        .expect("#1318 replay value must not be undefined");
    serde_json::from_str(&text).expect("#1318 replay value must be valid JSON")
}

/// The scenario keys of the TS golden, in `oracle.json.setupKeys` order.
fn scenario_keys(oracle: &JsValue) -> Vec<String> {
    match field(oracle, "setupKeys") {
        JsValue::Array(items) => items
            .iter()
            .map(|key| string(key, "setupKeys entry"))
            .collect(),
        _ => panic!("#1318 oracle must carry its setupKeys list"),
    }
}

/// `Object.entries` with JS own-key order: array indices first in ascending
/// numeric order, then other keys in insertion order.
fn own_entries(object: &JsObject) -> Vec<(JsString, JsValue)> {
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

fn number_text(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

// ─────────────────────────────────────────────────── the writer's YAML mirror

/// The document model of `updateLocalSettings`' YAML editor: top-level
/// scalars, top-level flow maps (re-emitted in the canonical spaced form on
/// any edit, exactly like the yaml package re-serializes the whole document)
/// and top-level blocks (kept verbatim). Only the shapes the golden's trees
/// use are representable; anything else is a bug in this fixture, not a
/// silent rewrite.
enum Entry {
    Scalar {
        key: String,
        value: String,
    },
    FlowMap {
        key: String,
        pairs: Vec<(String, String)>,
    },
    Block {
        key: String,
        raw: String,
    },
}

#[derive(Default)]
struct Document {
    entries: Vec<Entry>,
}

impl Document {
    fn parse(text: &str) -> Result<Self, String> {
        let lines: Vec<&str> = text.split('\n').collect();
        let mut entries = Vec::new();
        let mut index = 0;
        while index < lines.len() {
            let line = lines[index];
            index += 1;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(key) = line.strip_suffix(':') {
                let mut raw_lines: Vec<&str> = Vec::new();
                while index < lines.len() && lines[index].starts_with(' ') {
                    raw_lines.push(lines[index]);
                    index += 1;
                }
                let raw = if raw_lines.is_empty() {
                    String::new()
                } else {
                    format!("{}\n", raw_lines.join("\n"))
                };
                entries.push(Entry::Block {
                    key: key.to_string(),
                    raw,
                });
                continue;
            }
            let Some((key, value)) = line.split_once(": ") else {
                return Err(YAML_FLOW_MAP_ERROR.to_string());
            };
            if (value.starts_with('{') && !value.ends_with('}'))
                || (value.starts_with('[') && !value.ends_with(']'))
            {
                return Err(YAML_FLOW_MAP_ERROR.to_string());
            }
            if value.starts_with('{') {
                // The yaml package parses the flow map's entries as plain
                // scalars, so the canonical spaced form this emitter writes
                // re-parses to the same pairs: trim the inner text or every
                // rewrite accumulates the padding as part of the keys.
                let inner = value[1..value.len() - 1].trim();
                let mut pairs = Vec::new();
                if !inner.is_empty() {
                    for pair in inner.split(", ") {
                        let Some((pair_key, pair_value)) = pair.split_once(": ") else {
                            return Err(YAML_FLOW_MAP_ERROR.to_string());
                        };
                        pairs.push((pair_key.to_string(), pair_value.to_string()));
                    }
                }
                entries.push(Entry::FlowMap {
                    key: key.to_string(),
                    pairs,
                });
            } else {
                entries.push(Entry::Scalar {
                    key: key.to_string(),
                    value: value.to_string(),
                });
            }
        }
        Ok(Self { entries })
    }

    /// The document's YAML text. (`render`, not an inherent `to_string`:
    /// the emitter owns the exact byte shape, and the inherent method is the
    /// linted shape.)
    fn render(&self) -> String {
        let mut out = String::new();
        for entry in &self.entries {
            match entry {
                Entry::Scalar { key, value } => {
                    out.push_str(key);
                    out.push_str(": ");
                    out.push_str(value);
                    out.push('\n');
                }
                Entry::FlowMap { key, pairs } => {
                    out.push_str(key);
                    out.push_str(": {");
                    if !pairs.is_empty() {
                        out.push(' ');
                        for (position, (pair_key, pair_value)) in pairs.iter().enumerate() {
                            if position > 0 {
                                out.push_str(", ");
                            }
                            out.push_str(pair_key);
                            out.push_str(": ");
                            out.push_str(pair_value);
                        }
                        out.push(' ');
                    }
                    out.push_str("}\n");
                }
                Entry::Block { key, raw } => {
                    out.push_str(key);
                    out.push_str(":\n");
                    out.push_str(raw);
                }
            }
        }
        out
    }

    fn set(&mut self, path: &str, value: &str) {
        let (root, leaf) = path
            .split_once('.')
            .unwrap_or_else(|| panic!("#1318 a writer path is dotted: {path}"));
        match self
            .entries
            .iter_mut()
            .find(|entry| entry_key(entry) == root)
        {
            Some(Entry::FlowMap { pairs, .. }) => {
                if let Some(pair) = pairs.iter_mut().find(|(key, _)| key == leaf) {
                    pair.1 = value.to_string();
                } else {
                    pairs.push((leaf.to_string(), value.to_string()));
                }
            }
            Some(_) => panic!("#1318 out-of-golden-surface writer edit on {root}"),
            None => self.entries.push(Entry::Block {
                key: root.to_string(),
                raw: format!("  {leaf}: {value}\n"),
            }),
        }
    }

    fn delete(&mut self, path: &str) {
        let (root, leaf) = path
            .split_once('.')
            .unwrap_or_else(|| panic!("#1318 a writer path is dotted: {path}"));
        match self
            .entries
            .iter_mut()
            .find(|entry| entry_key(entry) == root)
        {
            Some(Entry::FlowMap { pairs, .. }) => pairs.retain(|(key, _)| key != leaf),
            Some(_) => panic!("#1318 out-of-golden-surface writer delete on {root}"),
            None => panic!("#1318 out-of-golden-surface writer delete of {root}"),
        }
    }
}

fn entry_key(entry: &Entry) -> &str {
    match entry {
        Entry::Scalar { key, .. } | Entry::FlowMap { key, .. } | Entry::Block { key, .. } => key,
    }
}

/// `normalizeSettings`: the allowlist walk over the requested update. Only
/// the golden's surface (agent.maxContextTokens/maxOutputTokens,
/// extension.includeHookEvents) is implemented; other allowlisted leaves
/// are a fixture bug, not a silent pass-through.
enum Action {
    Set { path: String, value: String },
    Delete { path: String },
}

fn normalize_settings(settings: &JsValue) -> Result<Vec<Action>, SettingsError> {
    let object = settings
        .as_object()
        .expect("#1318 the wrapper checked settings is an object");
    let mut actions = Vec::new();
    for (root, value) in own_entries(object) {
        let root_text = String::from_utf16_lossy(root.units());
        match root_text.as_str() {
            "agent" => {
                let agent = value
                    .as_object()
                    .unwrap_or_else(|| panic!("#1318 out-of-golden-surface agent value"));
                for (key, leaf) in own_entries(agent) {
                    let key_text = String::from_utf16_lossy(key.units());
                    match key_text.as_str() {
                        "maxContextTokens" | "maxOutputTokens" => {
                            let path = format!("agent.{key_text}");
                            match &leaf {
                                JsValue::Null => actions.push(Action::Delete { path }),
                                JsValue::Number(number)
                                    if number.fract() == 0.0 && *number > 0.0 =>
                                {
                                    actions.push(Action::Set {
                                        path,
                                        value: number_text(*number),
                                    })
                                }
                                _ => {
                                    return Err(SettingsError::local_settings(
                                        "INVALID_LOCAL_SETTINGS",
                                        format!(
                                            "localSettings.{path} must be a positive integer or null."
                                        ),
                                    ));
                                }
                            }
                        }
                        "thinking" | "subagents" => {
                            panic!("#1318 out-of-golden-surface agent key {key_text}")
                        }
                        other => {
                            return Err(SettingsError::local_settings(
                                "UNSUPPORTED_LOCAL_SETTING",
                                format!(
                                    "localSettings.agent.{other} is not supported by PilotDeck localSettings."
                                ),
                            ));
                        }
                    }
                }
            }
            "extension" => {
                let extension = value
                    .as_object()
                    .unwrap_or_else(|| panic!("#1318 out-of-golden-surface extension value"));
                for (key, leaf) in own_entries(extension) {
                    let key_text = String::from_utf16_lossy(key.units());
                    let path = format!("extension.{key_text}");
                    match key_text.as_str() {
                        "includeHookEvents" => match &leaf {
                            JsValue::Null => actions.push(Action::Delete { path }),
                            JsValue::Bool(flag) => actions.push(Action::Set {
                                path,
                                value: flag.to_string(),
                            }),
                            _ => {
                                return Err(SettingsError::local_settings(
                                    "INVALID_LOCAL_SETTINGS",
                                    format!("localSettings.{path} must be a boolean or null."),
                                ));
                            }
                        },
                        other => {
                            return Err(SettingsError::local_settings(
                                "UNSUPPORTED_LOCAL_SETTING",
                                format!(
                                    "localSettings.extension.{other} is not supported by PilotDeck localSettings."
                                ),
                            ));
                        }
                    }
                }
            }
            "tools" => panic!("#1318 out-of-golden-surface tools root"),
            _ => {
                return Err(SettingsError::local_settings(
                    "UNSUPPORTED_LOCAL_SETTING",
                    format!(
                        "localSettings.{root_text} is not supported by PilotDeck localSettings."
                    ),
                ));
            }
        }
    }
    Ok(actions)
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
        .expect("#1318 fake session snapshot must parse")
    }
    fn abort(&self, _reason: &JsString) -> RuntimeResult {
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        let key = self.key.clone();
        let fails = self.key == "f";
        let probe = self.probe.clone();
        probe.event(format!("dispose:{key}"));
        Box::pin(async move {
            // The TS fixture's session throws `Error('fixture dispose
            // failure')` for key `f`; the real #1011 `LiveSessionHandle`
            // aggregation turns it into the golden's `Failed to dispose agent
            // handle.` text, so the close-fault row is produced by the real
            // aggregation instead of echoing the expected bytes.
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
    owner: Mutex<Option<SessionOwner>>,
    /// `local.organizationPolicy?.settings?.canUpdateLocalSettings === false`.
    org_denied: Mutex<bool>,
    /// `local`'s current tree binding: the writer path and the store the
    /// updateSettings closure and the bundle's delegate read at call time.
    current: Mutex<Option<Tree>>,
    /// `registry.sdkSessionProjects`, insertion-ordered.
    styles_projects: Mutex<Vec<(String, String)>>,
    /// `registry.sdkSessionConfigs` keys.
    styles_configs: Mutex<Vec<String>>,
    /// `nextChanged`, the fake pluginRuntime reload's changed list.
    styles_changed: Mutex<Vec<JsString>>,
    /// `registry.sdkThinkingOverrides` (`{enabled, mode}` → the mode text).
    effort_overrides: Mutex<HashMap<String, String>>,
    /// `registry._sessionOverrides`' `permissionMode`.
    mode_overrides: Mutex<HashMap<String, String>>,
}
#[derive(Clone)]
struct Probe(Arc<ProbeState>);
impl Probe {
    fn new() -> Self {
        Self(Arc::new(ProbeState {
            events: Mutex::new(Vec::new()),
            now: AtomicI64::new(1),
            owner: Mutex::new(None),
            org_denied: Mutex::new(false),
            current: Mutex::new(None),
            styles_projects: Mutex::new(Vec::new()),
            styles_configs: Mutex::new(Vec::new()),
            styles_changed: Mutex::new(vec![JsString::from("style-a")]),
            effort_overrides: Mutex::new(HashMap::new()),
            mode_overrides: Mutex::new(HashMap::new()),
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
            .expect("#1318 probe owner must be installed before use")
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
            .unwrap_or_else(|_| panic!("#1318 session key must be valid UTF-8"))
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

/// `registry.applyFlagSettingsForSdk` (ProjectRuntimeRegistry.ts:914-954)
/// over the probe's map state, with the exact `hasOwnProperty` semantics:
/// the wrapper validated the shapes already, so this is pure mutation.
struct FakeApply {
    probe: Probe,
}
impl ApplyFlagSettings for FakeApply {
    fn apply(&self, input: &JsValue) -> Result<FlagSettings, SettingsError> {
        let session_key = string(field(input, "sessionKey"), "sessionKey");
        let settings = field(input, "settings")
            .as_object()
            .expect("#1318 the wrapper checked settings is an object");
        let mut applied = Vec::new();
        let mut cleared = Vec::new();
        if let Some(value) = settings.get("effortLevel") {
            match value {
                JsValue::Null => {
                    lock(&self.probe.0.effort_overrides).remove(&session_key);
                    cleared.push(JsString::from("effortLevel"));
                }
                mode => {
                    lock(&self.probe.0.effort_overrides)
                        .insert(session_key.clone(), string(mode, "effortLevel"));
                    applied.push(JsString::from("effortLevel"));
                }
            }
        }
        if let Some(permissions) = settings.get("permissions") {
            let default_mode = match permissions {
                JsValue::Null => None,
                JsValue::Object(object) => match object.get("defaultMode") {
                    None | Some(JsValue::Undefined | JsValue::Null) => None,
                    Some(value) => Some(string(value, "permissions.defaultMode")),
                },
                _ => panic!("#1318 the wrapper validated permissions' shape"),
            };
            match default_mode {
                None => {
                    lock(&self.probe.0.mode_overrides).remove(&session_key);
                    cleared.push(JsString::from("permissions.defaultMode"));
                }
                Some(mode) => {
                    lock(&self.probe.0.mode_overrides).insert(session_key.clone(), mode);
                    applied.push(JsString::from("permissions.defaultMode"));
                }
            }
        }
        Ok(FlagSettings { applied, cleared })
    }
}

/// `registry.reloadOutputStylesForSdk` (ProjectRuntimeRegistry.ts:1111-1123):
/// resolve (the projectKey verbatim, `null` for a nullish key), the
/// style-only reload, then every SDK session with a config whose project
/// root matches the requested key closes with
/// `sdk_output_styles_reloaded`. A nullish params throws the raw
/// property-read TypeError, exactly like the TS `input.projectKey` read.
struct FakeStyles {
    probe: Probe,
}
impl ReloadOutputStyles for FakeStyles {
    fn reload(
        &self,
        params: &JsValue,
    ) -> super::gateway::SettingsFuture<Result<OutputStylesReload, SettingsError>> {
        let probe = self.probe.clone();
        let params = params.clone();
        Box::pin(async move {
            let project_key = match &params {
                JsValue::Null => {
                    return Err(SettingsError::plain(
                        "TypeError",
                        "Cannot read properties of null (reading 'projectKey')",
                    ));
                }
                JsValue::Object(object) => match object.get("projectKey") {
                    None | Some(JsValue::Undefined | JsValue::Null) => None,
                    Some(JsValue::String(key)) => Some(
                        key.to_utf8()
                            .unwrap_or_else(|_| panic!("#1318 projectKey must be UTF-8"))
                            .to_string(),
                    ),
                    _ => panic!("#1318 out-of-golden-surface projectKey"),
                },
                _ => None,
            };
            probe.event(format!(
                "resolve:{}",
                project_key.as_deref().unwrap_or("null")
            ));
            probe.event("styles-reload");
            let changed = lock(&probe.0.styles_changed).clone();
            let projects = lock(&probe.0.styles_projects).clone();
            let configs = lock(&probe.0.styles_configs).clone();
            for (session_key, session_root) in projects {
                // `if (input.projectKey && projectRoot !== runtime.projectRoot)
                // continue` — resolve echoes the key, so the filter is a root
                // equality test against a truthy key.
                if let Some(key) = &project_key
                    && !key.is_empty()
                    && &session_root != key
                {
                    continue;
                }
                if !configs.contains(&session_key) {
                    continue;
                }
                let key = JsString::from(session_key.as_str());
                probe.owner().close(&key).await.map_err(|error| {
                    SettingsError::plain("Error", JsString::from(error.message))
                })?;
                probe.event(format!(
                    "close-session:{session_key}:sdk_output_styles_reloaded"
                ));
            }
            Ok(OutputStylesReload { changed })
        })
    }
}

/// The `updatePilotLocalSettings` mirror: the allowlist walk, the document
/// edit and the write, with the same changedPaths contract (the requested
/// sets and deletes; the store's event diff replaces them in the closure
/// above). The TS atomic-write and temp-home details are the production
/// writer's own; this mirror keeps the editing semantics.
struct MirrorWriter {
    probe: Probe,
}
impl LocalSettingsWriter for MirrorWriter {
    fn update(&self, settings: &JsValue) -> Result<LocalSettingsUpdate, SettingsError> {
        let actions = normalize_settings(settings)?;
        if actions.is_empty() {
            return Ok(LocalSettingsUpdate::default());
        }
        let tree = lock(&self.probe.0.current)
            .clone()
            .expect("#1318 a writer call requires a live tree");
        let text = std::fs::read_to_string(&tree.writer_path).map_err(|_| {
            SettingsError::local_settings(
                "LOCAL_SETTINGS_READ_FAILED",
                "Unable to update local settings: the file cannot be read.",
            )
        })?;
        let mut document = Document::parse(&text).map_err(|detail| {
            SettingsError::local_settings(
                "LOCAL_SETTINGS_YAML_INVALID",
                format!("Unable to update local settings: {detail}"),
            )
        })?;
        let mut applied = Vec::new();
        let mut cleared = Vec::new();
        for action in actions {
            match action {
                Action::Set { path, value } => {
                    document.set(&path, &value);
                    applied.push(JsString::from(path.as_str()));
                }
                Action::Delete { path } => {
                    document.delete(&path);
                    cleared.push(JsString::from(path.as_str()));
                }
            }
        }
        std::fs::write(&tree.writer_path, document.render()).map_err(|_| {
            SettingsError::local_settings(
                "LOCAL_SETTINGS_WRITE_FAILED",
                "Unable to update local settings: the write failed.",
            )
        })?;
        let mut changed_paths = applied.clone();
        changed_paths.extend(cleared.iter().cloned());
        Ok(LocalSettingsUpdate {
            applied,
            cleared,
            changed_paths,
        })
    }
}

// ─────────────────────────────────── the refresh bundle's recording fakes

struct BundleLog(Probe);
impl RefreshLog for BundleLog {
    fn log(&self, message: &JsString) {
        self.0
            .event(format!("log:{}", String::from_utf16_lossy(message.units())));
    }
    fn warn(&self, message: &str, detail: WarnDetail<'_>) {
        let detail = match detail {
            WarnDetail::Text(text) => String::from_utf16_lossy(text.units()),
            WarnDetail::Error(error) => error.message.clone(),
        };
        self.0.event(format!("warn:{message} {detail}"));
    }
}

/// The TS fake registry's reload promise never settles, so only
/// onConfigChange's synchronous prefix (the reload *call*) is pinned; the
/// `.then` ordering versus the WS response would be microtask-race noise.
struct BundleRegistry(Probe);
impl RefreshRegistry for BundleRegistry {
    fn reload(&self) -> Result<BoxFuture<RuntimeResult>, RuntimeError> {
        self.0.event("registry-reload");
        Ok(Box::pin(std::future::pending()))
    }
    fn invalidate(&self, project_key: Option<&JsString>) {
        self.0.event(match project_key {
            Some(key) => format!("invalidate:{}", String::from_utf16_lossy(key.units())),
            None => "invalidate:null".to_string(),
        });
    }
}

struct BundleRouter(Probe);
impl RefreshRouter for BundleRouter {
    fn mark_all_dirty(&self, reason: &str) -> usize {
        self.0.event(format!("mark-all-dirty:{reason}"));
        2
    }
    fn mark_project_dirty(&self, project_key: &JsString, reason: &str) -> usize {
        self.0.event(format!(
            "mark-project-dirty:{}:{reason}",
            String::from_utf16_lossy(project_key.units())
        ));
        1
    }
    fn mark_session_dirty(&self, _session_key: &JsString, _reason: &str) -> bool {
        false
    }
    fn cached_session_count(&self) -> usize {
        0
    }
    fn snapshot_session(&self, _session_key: &JsString) -> Option<JsValue> {
        None
    }
}

struct BundleServer(Probe);
impl NotificationServer for BundleServer {
    fn broadcast_notification(&self, name: &str, payload: &JsValue) {
        let text = stringify_json(payload)
            .expect("#1318 the broadcast payload must be serializable")
            .expect("#1318 the broadcast payload must not be undefined");
        self.0.event(format!("broadcast:{name}:{text}"));
    }
}

struct BundleSdk(Probe);
impl SdkConfigChange for BundleSdk {
    fn dispatch_sdk_config_change(&self, change: &ConfigChangeNotice) {
        let paths = change
            .changed_paths
            .iter()
            .map(|path| String::from_utf16_lossy(path.units()))
            .collect::<Vec<_>>()
            .join(",");
        let classes = change
            .change_classes
            .iter()
            .map(|class| String::from_utf16_lossy(class.units()))
            .collect::<Vec<_>>()
            .join(",");
        self.0.event(format!("sdk-config-change:{paths}|{classes}"));
    }
}

struct BundleMaintenance;
impl MemoryMaintenance for BundleMaintenance {
    fn schedule(&self, _project_key: &JsString) -> RuntimeResult {
        Ok(())
    }
}

// ─────────────────────────────────── the bundle's re-binding store delegate

/// The generator's `storeDelegate`: every live subscription (the bundle's
/// own attach listener, `reloadConfig`'s transient capture listener) is
/// re-pointed at the current scenario tree's store, so the real bundle
/// follows the trees the way production's single store feeds it. The probe
/// listener subscribes directly at `make_tree` time, so its event always
/// precedes the bundle's `onConfigChange` prefix.
struct RebindingStore(Arc<RebindingInner>);
struct RebindingInner {
    current: Mutex<Option<ConfigStore>>,
    tracked: Mutex<Vec<Tracked>>,
}
struct Tracked {
    key: usize,
    listener: ConfigListener,
    unsubscribe: StoreSubscription,
}

fn wrap_listener(listener: ConfigListener) -> StoreListener {
    Arc::new(move |event: &ReloadEvent| listener(&event.changed_paths, &event.change_classes))
}

impl RebindingStore {
    fn new() -> Self {
        Self(Arc::new(RebindingInner {
            current: Mutex::new(None),
            tracked: Mutex::new(Vec::new()),
        }))
    }
    fn rebind(&self, store: ConfigStore) {
        for tracked in lock(&self.0.tracked).iter_mut() {
            tracked.unsubscribe.unsubscribe();
            tracked.unsubscribe = store.subscribe(wrap_listener(tracked.listener.clone()));
        }
        *lock(&self.0.current) = Some(store);
    }
}

impl RefreshConfigStore for RebindingStore {
    fn subscribe(&self, listener: ConfigListener) -> Unsubscribe {
        let key = Arc::as_ptr(&listener) as *const u8 as usize;
        let store = lock(&self.0.current)
            .clone()
            .expect("#1318 the bundle store delegate requires a live tree");
        let unsubscribe = store.subscribe(wrap_listener(listener.clone()));
        lock(&self.0.tracked).push(Tracked {
            key,
            listener,
            unsubscribe,
        });
        let inner = Arc::clone(&self.0);
        Box::new(move || {
            let mut tracked = lock(&inner.tracked);
            if let Some(position) = tracked.iter().position(|tracked| tracked.key == key) {
                let tracked = tracked.remove(position);
                tracked.unsubscribe.unsubscribe();
            }
        })
    }
    fn reload(&self, reason: &'static str) -> BoxFuture<ConfigReloadResult> {
        let store = lock(&self.0.current)
            .clone()
            .expect("#1318 the bundle store delegate requires a live tree");
        let reload = ConfigStore::reload(&store, reason);
        Box::pin(async move { reload.await.map(|_| ()) })
    }
}

// ─────────────────────────────────────────────────────────────── harness

/// One scenario tree: a real store over a real scratch home, plus the two
/// paths the post state reads (`storePath` is the writer path unless the
/// tree was split for the reload-failure row).
#[derive(Clone)]
struct Tree {
    store: ConfigStore,
    writer_path: PathBuf,
    store_path: PathBuf,
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("pdrs-settings-gateway-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("#1318 the scratch root must be created");
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Harness {
    probe: Probe,
    keys: Vec<String>,
    _scratch: Scratch,
    trees: Mutex<Vec<(String, Tree)>>,
    bundle: Arc<RefreshBundle>,
    delegate: Arc<RebindingStore>,
}

fn harness(keys: &[String]) -> Harness {
    let probe = Probe::new();
    let owner = SessionOwner::new(probe.owner_options());
    probe.install_owner(owner);
    let delegate = Arc::new(RebindingStore::new());
    let router: Arc<dyn RefreshRouter> = Arc::new(BundleRouter(probe.clone()));
    let options = RefreshOptions {
        config_store: delegate.clone(),
        registry: Arc::new(BundleRegistry(probe.clone())),
        memory_maintenance: Arc::new(BundleMaintenance),
        router: Arc::new(move || Some(router.clone())),
        project_root: JsString::from("/pdrs-fixture/project"),
        memory_diagnostics: None,
        sdk: Some(Arc::new(BundleSdk(probe.clone()))),
        log: Arc::new(BundleLog(probe.clone())),
    };
    let bundle = Arc::new(RefreshBundle::new(options));
    bundle.bind_server(Arc::new(BundleServer(probe.clone())));
    Harness {
        probe,
        keys: keys.to_vec(),
        _scratch: Scratch::new(),
        trees: Mutex::new(Vec::new()),
        bundle,
        delegate,
    }
}

impl Harness {
    /// The gateway is reassembled per dispatch over the shared ports, the
    /// exact call-time binding of the TS closure's mutable `local`
    /// (organization policy, store, writer path).
    fn gateway(&self) -> SettingsGateway {
        let probe = self.probe.clone();
        let current = lock(&probe.0.current).clone();
        SettingsGateway::new(SettingsGatewayOptions {
            owner: probe.owner(),
            apply_flag_settings: Some(Arc::new(FakeApply {
                probe: probe.clone(),
            })),
            update_settings: current.map(|tree| UpdateSettings {
                organization_denied: *lock(&probe.0.org_denied),
                writer: Arc::new(MirrorWriter {
                    probe: probe.clone(),
                }),
                store: tree.store,
            }),
            reload_output_styles: Some(Arc::new(FakeStyles {
                probe: probe.clone(),
            })),
            reload_config: Some(self.bundle.clone()),
            reload_extensions: Some(self.bundle.clone()),
        })
    }

    async fn get_or_create(&self, key: &str) {
        self.probe
            .owner()
            .get_or_create(SessionContext {
                session_key: JsString::from(key),
                project_key: Some("/p".into()),
                channel_key: JsString::from(""),
                allowed_tools: None,
                disallowed_tools: None,
            })
            .await
            .expect("#1318 the fixture session must be created");
    }

    async fn make_tree(&self, name: &str, config_path_override: Option<PathBuf>) {
        let root = self._scratch.0.join("scratch").join(name);
        let home = root.join("home");
        let project = root.join("project");
        tokio::fs::create_dir_all(&home)
            .await
            .expect("#1318 the scratch home must be created");
        tokio::fs::create_dir_all(&project)
            .await
            .expect("#1318 the scratch project must be created");
        let writer_path = home.join("pilotdeck.yaml");
        tokio::fs::write(&writer_path, BASE)
            .await
            .expect("#1318 the scratch tree's config must be written");
        let home_text = home.to_string_lossy().into_owned();
        let mut env = Env::from([
            ("HOME".to_owned(), home_text.clone()),
            ("PILOT_HOME".to_owned(), home_text),
        ]);
        if let Some(path) = &config_path_override {
            env.insert(
                "PILOTDECK_CONFIG_PATH".to_owned(),
                path.to_string_lossy().into_owned(),
            );
        }
        let mut options = StoreOptions::new(home.clone(), home, env);
        options.project = Some(project.to_string_lossy().into_owned());
        let store =
            ConfigStore::new(options).expect("#1318 the scratch store must load its snapshot");
        let probe = self.probe.clone();
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
        let store_path = config_path_override.unwrap_or_else(|| writer_path.clone());
        let tree = Tree {
            store: store.clone(),
            writer_path,
            store_path,
        };
        *lock(&self.probe.0.current) = Some(tree.clone());
        self.delegate.rebind(store);
        lock(&self.trees).push((name.to_string(), tree));
    }

    fn rewrite_tree_file(&self, name: &str, content: &str) {
        let tree = lock(&self.trees)
            .iter()
            .find(|(existing, _)| existing == name)
            .map(|(_, tree)| tree.clone())
            .unwrap_or_else(|| panic!("#1318 setup needs tree {name}"));
        std::fs::write(&tree.store_path, content)
            .expect("#1318 the scratch config write must succeed");
    }

    /// The setup vocabulary of the golden; each step is executed exactly as
    /// `generate.mts` executes it on the TS side.
    async fn run_setup(&self, step: &str) {
        let parts: Vec<&str> = step.split(':').collect();
        match parts.as_slice() {
            ["ensure-session", key] => {
                self.get_or_create(key).await;
            }
            ["begin-turn", key, run] => {
                let reservation = self
                    .probe
                    .owner()
                    .begin_turn(&JsString::from(*key), &JsString::from(*run));
                assert!(
                    reservation.is_some(),
                    "#1318 begin-turn must reserve: {step}"
                );
            }
            ["styles-session", key, root] => {
                lock(&self.probe.0.styles_projects).push((key.to_string(), root.to_string()));
                self.get_or_create(key).await;
            }
            ["styles-config", key] => {
                lock(&self.probe.0.styles_configs).push(key.to_string());
            }
            ["styles-changed", csv] => {
                *lock(&self.probe.0.styles_changed) = csv.split(',').map(JsString::from).collect();
            }
            ["tree", name] => {
                self.make_tree(name, None).await;
            }
            ["tree-split", name] => {
                // The store reads a dedicated config file the writer never
                // touches: the only single-threaded composition where the
                // write succeeds and the reload still fails.
                let root = self._scratch.0.join("scratch").join(*name);
                let home = root.join("home");
                let project = root.join("project");
                tokio::fs::create_dir_all(&home)
                    .await
                    .expect("#1318 the scratch home must be created");
                tokio::fs::create_dir_all(&project)
                    .await
                    .expect("#1318 the scratch project must be created");
                let alt = root.join("alt-config.yaml");
                tokio::fs::write(&alt, BASE)
                    .await
                    .expect("#1318 the split tree's config must be written");
                self.make_tree(name, Some(alt)).await;
            }
            ["break-yaml", name] => self.rewrite_tree_file(name, BROKEN_YAML),
            ["break-store-yaml", name] => self.rewrite_tree_file(name, BAD_SCHEMA),
            ["write-yaml", name, variant] => {
                self.rewrite_tree_file(
                    name,
                    if *variant == "other" {
                        OTHER_MODEL
                    } else {
                        BASE
                    },
                );
            }
            ["deny-org"] => {
                *lock(&self.probe.0.org_denied) = true;
            }
            ["allow-org"] => {
                *lock(&self.probe.0.org_denied) = false;
            }
            ["attach-bundle"] => {
                self.bundle.attach();
            }
            _ => panic!("#1318 unknown setup step {step}"),
        }
    }

    /// The post-state projection of the golden: active turns, live sessions,
    /// the registry override tables and every tree's store version and both
    /// YAML files.
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
        let mut overrides = JsObject::new();
        for key in &self.keys {
            let js_key = JsString::from(key.as_str());
            sessions.insert(
                js_key.clone(),
                JsValue::Bool(owner.current_handle(&js_key).is_some()),
            );
            let mut record = JsObject::new();
            record.insert(
                "effort".into(),
                match lock(&self.probe.0.effort_overrides).get(key) {
                    Some(mode) => JsValue::String(JsString::from(mode.as_str())),
                    None => JsValue::Null,
                },
            );
            record.insert(
                "mode".into(),
                match lock(&self.probe.0.mode_overrides).get(key) {
                    Some(mode) => JsValue::String(JsString::from(mode.as_str())),
                    None => JsValue::Null,
                },
            );
            overrides.insert(js_key, JsValue::Object(record));
        }
        let mut trees = JsObject::new();
        for (name, tree) in lock(&self.trees).iter() {
            let mut record = JsObject::new();
            record.insert(
                "version".into(),
                JsValue::Number(
                    tree.store
                        .snapshot()
                        .get("version")
                        .and_then(JsValue::as_number)
                        .unwrap_or_default(),
                ),
            );
            record.insert("storeYaml".into(), read_yaml(&tree.store_path));
            record.insert("writerYaml".into(), read_yaml(&tree.writer_path));
            trees.insert(JsString::from(name.as_str()), JsValue::Object(record));
        }
        let mut object = JsObject::new();
        object.insert("turns".into(), turns);
        object.insert("sessions".into(), JsValue::Object(sessions));
        object.insert("overrides".into(), JsValue::Object(overrides));
        object.insert("trees".into(), JsValue::Object(trees));
        JsValue::Object(object)
    }
}

fn read_yaml(path: &std::path::Path) -> JsValue {
    match std::fs::read_to_string(path) {
        Ok(text) => JsValue::String(JsString::from(text)),
        Err(_) => JsValue::Null,
    }
}

/// The reconstructed WS response frame: `ok` carries the result, an error
/// carries `{code, message}` with the raw-throw fallback
/// `gateway_request_failed`, exactly like the WS layer's error frame.
fn envelope(result: Result<JsValue, SettingsError>) -> JsValue {
    match result {
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
                    error_object.insert("code".into(), JsValue::String(code.clone()));
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
    }
}

// ─────────────────────────────────────────────────────── the replay tests

#[tokio::test]
async fn settings_gateway_replays_the_real_ts_oracle() {
    let oracle = crate::test_baseline::oracle(ORACLE);
    let keys = scenario_keys(&oracle);
    let cases = match field(&oracle, "cases") {
        JsValue::Array(items) => items.clone(),
        _ => panic!("#1318 oracle must carry its case rows"),
    };
    assert_eq!(
        cases.len(),
        49,
        "#1318 the oracle must carry the full 49-row matrix"
    );
    let harness = harness(&keys);
    let mut failures: Vec<String> = Vec::new();
    for case in &cases {
        let id = string(field(case, "id"), "case.id");
        let method = string(field(case, "method"), "case.method");
        for step in match field(case, "setup") {
            JsValue::Array(items) => items.clone(),
            _ => panic!("#1318 case {id} must carry a setup list"),
        } {
            harness.run_setup(&string(&step, "case.setup step")).await;
        }
        harness.probe.clear_events();
        let input = field(case, "params").clone();
        let dispatch = harness.gateway().dispatch(&method, &input).await;
        let result = dispatch.unwrap_or_else(|| {
            panic!("#1318 case {id}: dispatch must own the five settings methods")
        });
        // A divergent row does not stop the replay: every row is compared so
        // one run reports the whole matrix (the negative controls bind one
        // bound marker per matrix cell, so all of them must be reachable in
        // the single panic payload).
        let expected = as_serde(field(case, "response"));
        let actual = as_serde(&envelope(result));
        if actual != expected {
            failures.push(format!(
                "#1318 case {id}: the reconstructed WS response must match TS\n  expected: {expected}\n  actual:   {actual}"
            ));
        }
        let expected_events = match field(case, "events") {
            JsValue::Array(items) => items
                .iter()
                .map(|event| string(event, "case.events entry"))
                .collect::<Vec<_>>(),
            _ => panic!("#1318 case {id} must carry an events list"),
        };
        let actual_events = harness.probe.events();
        if actual_events != expected_events {
            failures.push(format!(
                "#1318 case {id}: the event stream must match TS\n  expected: {expected_events:?}\n  actual:   {actual_events:?}"
            ));
        }
        let expected_post = as_serde(field(case, "post"));
        let actual_post = as_serde(&harness.post_state());
        if actual_post != expected_post {
            failures.push(format!(
                "#1318 case {id}: the post state must match TS\n  expected: {expected_post}\n  actual:   {actual_post}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "#1318 {} matrix row(s) diverged from the TS oracle:\n{}",
        failures.len(),
        failures.join("\n")
    );

    // `assemblyCases.capability`: a gateway assembled without the five
    // options answers CAPABILITY_UNAVAILABLE for apply/update/styles and
    // the `{reloaded:false,reason:"unsupported"}` projection for the two
    // reloads — the component's own option handling, replayed through a
    // bare instance.
    let assembly = field(&oracle, "assemblyCases");
    let capability = match field(assembly, "capability") {
        JsValue::Array(items) => items.clone(),
        _ => panic!("#1318 oracle must carry its assemblyCases.capability list"),
    };
    let bare = SettingsGateway::new(SettingsGatewayOptions {
        owner: harness.probe.owner(),
        apply_flag_settings: None,
        update_settings: None,
        reload_output_styles: None,
        reload_config: None,
        reload_extensions: None,
    });
    let input = parse_json(r#"{"sessionKey":"s"}"#)
        .expect("#1318 the capability assembly input must parse");
    for case in &capability {
        let method = string(field(case, "method"), "assemblyCases.capability.method");
        let result = bare
            .dispatch(&method, &input)
            .await
            .unwrap_or_else(|| panic!("#1318 the bare gateway still owns {method}"));
        let expected = as_serde(field(case, "response"));
        let actual = as_serde(&envelope(result));
        assert_eq!(
            actual, expected,
            "#1318 the bare assembly must answer {method} exactly like the TS WS layer"
        );
    }
    // The unknown-method row is the wiring layer's `Unknown gateway method`
    // error; for this component it must simply not be one of the five.
    let unknown = bare.dispatch("settings_probe", &input).await;
    assert!(
        unknown.is_none(),
        "#1318 dispatch must not own settings_probe; the WS layer answers Unknown gateway method"
    );
}

/// The dispatch entry owns exactly the five U7 methods; everything else is
/// the caller's business (the WS layer's unknown-method error and the
/// CAPABILITY_UNAVAILABLE answers live in the wiring layer, recorded in the
/// golden's `assemblyCases`).
#[tokio::test]
async fn settings_gateway_dispatch_returns_none_for_non_settings_methods() {
    let keys = vec!["s".to_string()];
    let harness = harness(&keys);
    // The owned methods run for real through the bundle's store delegate, so
    // the harness needs a live tree exactly like the TS assembly.
    harness.make_tree("t1", None).await;
    let input = parse_json(r#"{"sessionKey":"s"}"#).expect("#1318 dispatch input must parse");
    for method in [
        "settings_probe",
        "submit_turn",
        "set_mcp_servers",
        "apply_flag_settings_extra",
        "",
    ] {
        let dispatch = harness.gateway().dispatch(method, &input).await;
        assert!(
            dispatch.is_none(),
            "#1318 dispatch must not own non-settings method {method}"
        );
    }
    for method in [
        "apply_flag_settings",
        "update_settings",
        "reload_output_styles",
        "reload_config",
        "reload_extensions",
    ] {
        let dispatch = harness.gateway().dispatch(method, &input).await;
        assert!(
            dispatch.is_some(),
            "#1318 dispatch must own the settings method {method}"
        );
    }
}

/// Regeneration guard for the TS golden; run once with `PDRS_BASELINE` set.
#[test]
#[ignore = "regenerates the TS fixture; needs PDRS_BASELINE and Node"]
fn settings_gateway_fixture_regenerates_identically_from_ts() {
    crate::test_baseline::regenerates_identically("settings-gateway");
}

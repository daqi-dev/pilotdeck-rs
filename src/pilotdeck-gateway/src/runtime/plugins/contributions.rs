//! Contribution snapshot builders, ported from `PluginRuntime.ts` lines 620-900+.
//!
//! All sort operations use `str::localeCompare` semantics (en-US ICU collation)
//! via `node_collator`.
use super::loader::{LoadedPlugin, PluginSource};
use crate::hooks::protocol::HooksSettings;
use crate::project_files::{node_collator, node_locale};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

// ─────────────────────────────────────────────────────────── contribution types

/// `PluginCommandContribution`.
#[derive(Clone, Debug)]
pub struct PluginCommandContribution {
    pub name: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    pub content: String,
    pub namespace: String,
}

/// `PluginSkillContribution`.
#[derive(Clone, Debug)]
pub struct PluginSkillContribution {
    pub name: String,
    pub description: Option<String>,
    pub path: std::path::PathBuf,
    pub content: String,
    pub namespace: String,
}

/// `PluginMcpInstruction` (may have `None` for `instructions`).
#[derive(Clone, Debug)]
pub struct PluginMcpInstruction {
    pub server_name: String,
    pub instructions: Option<String>,
}

/// `PilotDeckMcpInstructionEntry` (always has `instructions`).
#[derive(Clone, Debug)]
pub struct PluginMcpInstructionEntry {
    pub server_name: String,
    pub instructions: String,
}

/// `PluginToolContribution` (programmatic; from `toolContributions`).
#[derive(Clone, Debug)]
pub struct PluginToolContribution {
    pub namespace: String,
    pub tool: JsValue,
}

/// `PluginRouterContribution` (programmatic).
#[derive(Clone, Debug)]
pub struct PluginRouterContribution {
    pub namespace: String,
    pub id: String,
}

/// `OutputStyleContribution` from `PluginRuntime.listOutputStyles`.
#[derive(Clone, Debug)]
pub struct PluginOutputStyleContribution {
    pub name: String,
    pub description: Option<String>,
    pub content: String,
    pub path: std::path::PathBuf,
    pub plugin: String,
    pub source: PluginSource,
}

// ─────────────────────────────────────────────────────────── snapshots

/// `PluginCommandCatalogSnapshot` (frozen in TS).
#[derive(Clone, Debug)]
pub struct PluginCommandCatalogSnapshot {
    pub generation: u64,
    pub commands: Vec<PluginCommandContribution>,
}

/// `PluginSessionContributionSnapshot` (frozen in TS).
#[derive(Clone, Debug)]
pub struct PluginSessionContributionSnapshot {
    pub generation: u64,
    pub tools: Vec<PluginToolContribution>,
    pub routers: Vec<PluginRouterContribution>,
    pub commands: Vec<PluginCommandContribution>,
    pub skills: Vec<PluginSkillContribution>,
    pub prompts: Vec<String>,
    pub hooks: HooksSettings,
    pub mcp_servers: JsObject,
    pub mcp_instructions: Vec<PluginMcpInstructionEntry>,
}

/// `PluginContributionSnapshot` (deprecated).
#[derive(Clone, Debug)]
pub struct PluginContributionSnapshot {
    pub generation: u64,
    pub plugins: Vec<Arc<LoadedPlugin>>,
    pub tools: Vec<PluginToolContribution>,
    pub routers: Vec<PluginRouterContribution>,
    pub commands: Vec<PluginCommandContribution>,
    pub skills: Vec<PluginSkillContribution>,
    pub hooks: HooksSettings,
    pub mcp_servers: JsObject,
    pub lsp_servers: JsObject,
    pub mcp_instructions: Vec<PluginMcpInstruction>,
}

// ─────────────────────────────────────────────────────────── lease types

use super::{PluginRuntimeError, PluginRuntimeState, release_lease_plugins};
use tokio::sync::{Mutex as TokioMutex, Notify};

// Own the decrement/disposal wait independently of a caller's cancellation,
// and memoize the result so repeated release observes the same TS promise.
fn start_lease_release(
    plugins: &[Arc<LoadedPlugin>],
    state: &Arc<TokioMutex<PluginRuntimeState>>,
    waiters: &Arc<Notify>,
) -> super::DisposalReceiver {
    let (tx, rx) = tokio::sync::watch::channel(None);
    let plugins = plugins.to_vec();
    let state = Arc::clone(state);
    let waiters = Arc::clone(waiters);
    tokio::spawn(async move {
        let result = release_lease_plugins(&plugins, &state, &waiters).await;
        let _ = tx.send(Some(result));
    });
    rx
}

/// A held lease on one plugin generation (command-catalog flavor).
pub struct PluginCommandCatalogLease {
    pub generation: u64,
    pub contributions: PluginCommandCatalogSnapshot,
    pub(super) state: std::sync::Arc<TokioMutex<PluginRuntimeState>>,
    pub(crate) plugins: Vec<Arc<LoadedPlugin>>,
    pub(crate) released: bool,
    pub(super) release_receiver: Option<super::DisposalReceiver>,
    pub(crate) waiters: std::sync::Arc<Notify>,
}

impl PluginCommandCatalogLease {
    pub async fn release(&mut self) -> Result<(), PluginRuntimeError> {
        let rx = if let Some(rx) = &self.release_receiver {
            rx.clone()
        } else {
            self.released = true;
            let rx = start_lease_release(&self.plugins, &self.state, &self.waiters);
            self.release_receiver = Some(rx.clone());
            rx
        };
        super::wait_watch(rx).await
    }
}

impl Drop for PluginCommandCatalogLease {
    fn drop(&mut self) {
        if !self.released {
            self.released = true;
            // Best-effort synchronous decrement; disposal callbacks are async
            // and will run when the waiters are notified at the next poll point.
            let state_clone = std::sync::Arc::clone(&self.state);
            let waiters_clone = std::sync::Arc::clone(&self.waiters);
            let plugins_clone = self.plugins.clone();
            tokio::spawn(async move {
                let _ = release_lease_plugins(&plugins_clone, &state_clone, &waiters_clone).await;
            });
        }
    }
}

/// A held lease on one plugin generation (session-contributions flavor).
pub struct PluginSessionContributionLease {
    pub generation: u64,
    pub contributions: PluginSessionContributionSnapshot,
    pub(super) state: std::sync::Arc<TokioMutex<PluginRuntimeState>>,
    pub(crate) plugins: Vec<Arc<LoadedPlugin>>,
    pub(crate) released: bool,
    pub(super) release_receiver: Option<super::DisposalReceiver>,
    pub(crate) waiters: std::sync::Arc<Notify>,
}

impl PluginSessionContributionLease {
    pub async fn release(&mut self) -> Result<(), PluginRuntimeError> {
        let rx = if let Some(rx) = &self.release_receiver {
            rx.clone()
        } else {
            self.released = true;
            let rx = start_lease_release(&self.plugins, &self.state, &self.waiters);
            self.release_receiver = Some(rx.clone());
            rx
        };
        super::wait_watch(rx).await
    }
}

impl Drop for PluginSessionContributionLease {
    fn drop(&mut self) {
        if !self.released {
            self.released = true;
            let state_clone = std::sync::Arc::clone(&self.state);
            let waiters_clone = std::sync::Arc::clone(&self.waiters);
            let plugins_clone = self.plugins.clone();
            tokio::spawn(async move {
                let _ = release_lease_plugins(&plugins_clone, &state_clone, &waiters_clone).await;
            });
        }
    }
}

/// A held lease on one plugin generation (deprecated contribution snapshot).
pub struct PluginContributionLease {
    pub generation: u64,
    pub contributions: PluginContributionSnapshot,
    pub(super) state: std::sync::Arc<TokioMutex<PluginRuntimeState>>,
    pub(crate) plugins: Vec<Arc<LoadedPlugin>>,
    pub(crate) released: bool,
    pub(super) release_receiver: Option<super::DisposalReceiver>,
    pub(crate) waiters: std::sync::Arc<Notify>,
}

impl PluginContributionLease {
    pub async fn release(&mut self) -> Result<(), PluginRuntimeError> {
        let rx = if let Some(rx) = &self.release_receiver {
            rx.clone()
        } else {
            self.released = true;
            let rx = start_lease_release(&self.plugins, &self.state, &self.waiters);
            self.release_receiver = Some(rx.clone());
            rx
        };
        super::wait_watch(rx).await
    }
}

impl Drop for PluginContributionLease {
    fn drop(&mut self) {
        if !self.released {
            self.released = true;
            let state_clone = std::sync::Arc::clone(&self.state);
            let waiters_clone = std::sync::Arc::clone(&self.waiters);
            let plugins_clone = self.plugins.clone();
            tokio::spawn(async move {
                let _ = release_lease_plugins(&plugins_clone, &state_clone, &waiters_clone).await;
            });
        }
    }
}

// ─────────────────────────────────────────────────────────── PluginRuntimeView

/// `PluginRuntimeView` (read-only, no lease).
pub struct PluginRuntimeView {
    plugins: Vec<Arc<LoadedPlugin>>,
}

impl PluginRuntimeView {
    pub(super) fn new(plugins: Vec<Arc<LoadedPlugin>>) -> Self {
        Self { plugins }
    }
    pub fn snapshot(&self) -> Vec<Arc<LoadedPlugin>> {
        self.plugins.clone()
    }
    pub fn all_commands(&self) -> Vec<PluginCommandContribution> {
        collect_command_contributions(&self.plugins)
    }
    pub fn all_skills(&self) -> Vec<PluginSkillContribution> {
        collect_skill_contributions(&self.plugins)
    }
    pub fn mcp_servers(&self) -> JsObject {
        collect_mcp_servers(&self.plugins)
    }
    pub fn all_mcp_instructions(&self) -> Vec<PluginMcpInstructionEntry> {
        collect_mcp_instructions(&self.plugins)
    }
    pub fn list_output_styles(&self) -> Vec<PluginOutputStyleContribution> {
        collect_output_styles(&self.plugins)
    }
    pub fn snapshot_contributions(&self) -> PluginContributionSnapshot {
        create_contribution_snapshot(0, &self.plugins)
    }
}

// ─────────────────────────────────────────────────────────── snapshot builders

pub(super) fn create_command_catalog_snapshot(
    generation: u64,
    plugins: &[Arc<LoadedPlugin>],
) -> PluginCommandCatalogSnapshot {
    PluginCommandCatalogSnapshot {
        generation,
        commands: collect_command_contributions(plugins),
    }
}

pub(super) fn create_session_contribution_snapshot(
    generation: u64,
    plugins: &[Arc<LoadedPlugin>],
) -> PluginSessionContributionSnapshot {
    PluginSessionContributionSnapshot {
        generation,
        tools: collect_tool_contributions(plugins),
        routers: collect_router_contributions(plugins),
        commands: collect_command_contributions(plugins),
        skills: collect_skill_contributions(plugins),
        prompts: collect_prompt_contributions(plugins),
        hooks: collect_hooks(plugins),
        mcp_servers: collect_mcp_servers(plugins),
        mcp_instructions: collect_mcp_instructions(plugins),
    }
}

pub(super) fn create_contribution_snapshot(
    generation: u64,
    plugins: &[Arc<LoadedPlugin>],
) -> PluginContributionSnapshot {
    PluginContributionSnapshot {
        generation,
        plugins: plugins.to_vec(),
        tools: collect_tool_contributions(plugins),
        routers: collect_router_contributions(plugins),
        commands: collect_command_contributions(plugins),
        skills: collect_skill_contributions(plugins),
        hooks: collect_hooks(plugins),
        mcp_servers: collect_mcp_servers(plugins),
        lsp_servers: collect_lsp_servers(plugins),
        mcp_instructions: collect_mcp_instructions_raw(plugins),
    }
}

// ─────────────────────────────────────────────────────────── collectors

fn source_priority(source: PluginSource) -> u8 {
    match source {
        PluginSource::Project => 2,
        PluginSource::Global => 1,
        PluginSource::Builtin => 0,
    }
}

pub(super) fn collect_command_contributions(
    plugins: &[Arc<LoadedPlugin>],
) -> Vec<PluginCommandContribution> {
    // project >= global >= builtin; ties: higher or equal priority wins (TS uses >=).
    let mut selected: std::collections::HashMap<String, (PluginCommandContribution, u8)> =
        std::collections::HashMap::new();
    for plugin in plugins {
        let priority = source_priority(plugin.source);
        for command in &plugin.commands {
            let contribution = to_command_contribution(plugin, command);
            let name = contribution.name.clone();
            let entry = selected.get(&name);
            if entry.is_none_or(|(_, p)| priority >= *p) {
                selected.insert(name, (contribution, priority));
            }
        }
    }
    let mut result: Vec<_> = selected.into_values().map(|(c, _)| c).collect();
    locale_sort_by(&mut result, |c| &c.name);
    result
}

pub(super) fn collect_skill_contributions(
    plugins: &[Arc<LoadedPlugin>],
) -> Vec<PluginSkillContribution> {
    let mut selected: std::collections::HashMap<String, (PluginSkillContribution, u8)> =
        std::collections::HashMap::new();
    for plugin in plugins {
        let priority = source_priority(plugin.source);
        for skill in &plugin.skills {
            let contribution = to_skill_contribution(plugin, skill);
            let name = contribution.name.clone();
            let entry = selected.get(&name);
            if entry.is_none_or(|(_, p)| priority >= *p) {
                selected.insert(name, (contribution, priority));
            }
        }
    }
    selected.into_values().map(|(c, _)| c).collect()
}

fn collect_output_styles(plugins: &[Arc<LoadedPlugin>]) -> Vec<PluginOutputStyleContribution> {
    let mut styles: std::collections::HashMap<String, PluginOutputStyleContribution> =
        std::collections::HashMap::new();
    for plugin in plugins {
        for style in &plugin.output_styles {
            styles
                .entry(style.name.clone())
                .or_insert_with(|| to_output_style(plugin, style));
        }
    }
    let mut result: Vec<_> = styles.into_values().collect();
    locale_sort_by(&mut result, |s| &s.name);
    result
}

fn collect_tool_contributions(_plugins: &[Arc<LoadedPlugin>]) -> Vec<PluginToolContribution> {
    // Programmatic tools only (disk plugins have no toolContributions).
    // Programmatic tool contributions require their own registration seam.
    Vec::new()
}

fn collect_router_contributions(_plugins: &[Arc<LoadedPlugin>]) -> Vec<PluginRouterContribution> {
    Vec::new()
}

fn collect_prompt_contributions(_plugins: &[Arc<LoadedPlugin>]) -> Vec<String> {
    Vec::new()
}

fn collect_hooks(plugins: &[Arc<LoadedPlugin>]) -> HooksSettings {
    let mut combined: HooksSettings = Vec::new();
    for plugin in plugins {
        let sources = [plugin.hooks_config.as_ref()];
        for source in sources.into_iter().flatten() {
            for (event, matchers) in source {
                let mut tagged: Vec<_> = matchers
                    .iter()
                    .map(|m| {
                        let mut m2 = m.clone();
                        m2.plugin_name = Some(JsString::from(plugin.name.as_str()));
                        m2.plugin_id = Some(JsString::from(
                            format!("{}@{}", plugin.name, plugin.source.as_str()).as_str(),
                        ));
                        m2.plugin_root =
                            Some(JsString::from(plugin.path.to_string_lossy().as_ref()));
                        m2
                    })
                    .collect();
                if let Some(existing) = combined.iter_mut().find(|(e, _)| e == event) {
                    existing.1.append(&mut tagged);
                } else {
                    combined.push((event.clone(), tagged));
                }
            }
        }
    }
    combined
}

pub(super) fn collect_mcp_servers(plugins: &[Arc<LoadedPlugin>]) -> JsObject {
    let mut out = JsObject::new();
    for plugin in plugins {
        if let Some(obj) = plugin.mcp_servers.as_ref().and_then(|v| v.as_object()) {
            for (key, value) in obj.own_entries() {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    out
}

pub(super) fn collect_lsp_servers(plugins: &[Arc<LoadedPlugin>]) -> JsObject {
    let mut out = JsObject::new();
    for plugin in plugins {
        if let Some(obj) = plugin.lsp_servers.as_ref().and_then(|v| v.as_object()) {
            for (key, value) in obj.own_entries() {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    out
}

/// MCP instructions as in `getAllMcpInstructions` / `PluginRuntime.ts:289-311`.
pub(super) fn collect_mcp_instructions(
    plugins: &[Arc<LoadedPlugin>],
) -> Vec<PluginMcpInstructionEntry> {
    const MAX_LEN: usize = 2048;
    let mut entries: Vec<PluginMcpInstructionEntry> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for plugin in plugins {
        let Some(servers) = &plugin.mcp_servers else {
            continue;
        };
        let Some(obj) = servers.as_object() else {
            continue;
        };
        for (server_name, raw) in obj.own_entries() {
            let server_name = String::from_utf16_lossy(server_name.units());
            if seen.contains(server_name.as_str()) {
                continue;
            }
            let Some(raw_obj) = raw.as_object() else {
                continue;
            };
            let Some(candidate) = raw_obj.get("instructions") else {
                continue;
            };
            let Some(str_val) = candidate.as_string() else {
                continue;
            };
            let str_val = String::from_utf16_lossy(str_val.units());
            let trimmed = str_val.trim();
            if trimmed.is_empty() {
                continue;
            }
            seen.insert(server_name.to_string());
            let instructions = if trimmed.len() <= MAX_LEN {
                trimmed.to_owned()
            } else {
                format!("{}… [truncated]", &trimmed[..MAX_LEN])
            };
            entries.push(PluginMcpInstructionEntry {
                server_name: server_name.to_string(),
                instructions,
            });
        }
    }
    locale_sort_by(&mut entries, |e| &e.server_name);
    entries
}

/// Raw instructions including `None` (legacy snapshot).
fn collect_mcp_instructions_raw(plugins: &[Arc<LoadedPlugin>]) -> Vec<PluginMcpInstruction> {
    collect_mcp_instructions(plugins)
        .into_iter()
        .map(|e| PluginMcpInstruction {
            server_name: e.server_name,
            instructions: Some(e.instructions),
        })
        .collect()
}

// ─────────────────────────────────────────────────────────── helpers

fn to_command_contribution(
    plugin: &LoadedPlugin,
    command: &super::loader::LoadedPluginCommand,
) -> PluginCommandContribution {
    let description = command
        .frontmatter
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let argument_hint = command
        .frontmatter
        .get("argument-hint")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    PluginCommandContribution {
        name: command.name.clone(),
        description,
        argument_hint,
        content: command.content.clone(),
        namespace: plugin.name.clone(),
    }
}

fn to_skill_contribution(
    plugin: &LoadedPlugin,
    skill: &super::loader::LoadedPluginCommand,
) -> PluginSkillContribution {
    let description = skill
        .frontmatter
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    PluginSkillContribution {
        name: skill.name.clone(),
        description,
        path: skill.path.clone(),
        content: skill.content.clone(),
        namespace: plugin.name.clone(),
    }
}

fn to_output_style(
    plugin: &LoadedPlugin,
    style: &super::loader::LoadedPluginCommand,
) -> PluginOutputStyleContribution {
    let description = style
        .frontmatter
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    PluginOutputStyleContribution {
        name: style.name.clone(),
        description,
        content: style.content.clone(),
        path: style.path.clone(),
        plugin: plugin.name.clone(),
        source: plugin.source,
    }
}

/// Sort with Node's ICU en-US `localeCompare` (stable, like JS `Array#sort`).
fn locale_sort_by<T, F: Fn(&T) -> &str>(items: &mut [T], key: F) {
    let locale = node_locale(|k| std::env::var(k).ok());
    if let Ok(collator) = node_collator(&locale) {
        items.sort_by(|a, b| {
            let ka: Vec<u16> = key(a).encode_utf16().collect();
            let kb: Vec<u16> = key(b).encode_utf16().collect();
            collator.compare_utf16(&ka, &kb)
        });
    } else {
        items.sort_by(|a, b| key(a).cmp(key(b)));
    }
}

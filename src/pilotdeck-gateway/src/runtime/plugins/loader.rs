//! Disk-loading bridge for plugins and skills (A6.3-pr7b scope).
//!
//! This module defines the stable interface that `PluginRuntime::perform_refresh`
//! calls. The implementation of `load_from_disk` is a placeholder; it must be
//! filled in by **A6.3-pr7b**, which will port:
//!
//! - `discoverPluginPaths` / `discoverSkillPaths` (discoverLocalPlugins.ts)
//! - `loadPluginFromPath` / `loadSkillFromPath` (PluginLoader.ts)
//! - `loadPluginCommands` / `loadStandaloneSkill` (PluginCommandLoader.ts)
//! - `parsePluginManifest` (config/parsePluginManifest.ts)
//! - `loadPluginHooks` (loading/PluginHookLoader.ts)
//!
//! Until then, `load_from_disk` returns empty results so the lifecycle tests
//! pass without I/O.
use crate::hooks::protocol::HooksSettings;
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};
/// Dispose callback type alias to keep the struct field readable.
pub type DisposeFn = Box<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'static>> + Send + Sync,
>;

use super::options::{PilotExtensionPaths, PluginRuntimeOptions};

/// A loaded plugin, matching `PilotDeckLoadedPlugin`.
pub struct LoadedPlugin {
    pub name: String,
    pub path: PathBuf,
    pub source: PluginSource,
    pub commands: Vec<LoadedPluginCommand>,
    pub skills: Vec<LoadedPluginCommand>,
    pub output_styles: Vec<LoadedPluginCommand>,
    pub hooks_config: Option<HooksSettings>,
    pub mcp_servers: Option<serde_json::Value>,
    pub lsp_servers: Option<serde_json::Value>,
    /// Optional async teardown for programmatic (builtin/test-injected) plugins.
    /// `Ok(())` = success; `Err(message)` = failure text forwarded to callers.
    pub dispose: Option<DisposeFn>,
}

impl std::fmt::Debug for LoadedPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedPlugin")
            .field("name", &self.name)
            .field("path", &self.path)
            .field("source", &self.source)
            .field("commands", &self.commands.len())
            .field("skills", &self.skills.len())
            .field("output_styles", &self.output_styles.len())
            .field("has_hooks", &self.hooks_config.is_some())
            .field("has_mcp", &self.mcp_servers.is_some())
            .field("has_lsp", &self.lsp_servers.is_some())
            .field("has_dispose", &self.dispose.is_some())
            .finish()
    }
}

/// `PilotDeckPluginSourceKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PluginSource {
    Builtin,
    Global,
    Project,
}

impl PluginSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

/// A loaded markdown contribution (command, skill, or output style).
pub struct LoadedPluginCommand {
    pub name: String,
    pub path: PathBuf,
    pub content: String,
    pub frontmatter: std::collections::HashMap<String, serde_json::Value>,
    pub is_skill: bool,
}

/// A candidate that failed to load during staging.
#[derive(Clone, Debug)]
pub struct PluginRefreshFailure {
    pub path: PathBuf,
    pub source: PluginSource,
    pub kind: PluginKind,
}

/// Whether the failure came from a plugin or a standalone skill directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginKind {
    Plugin,
    Skill,
}

/// The result returned to `perform_refresh`.
pub struct LoadFromDiskResult {
    pub plugins: Vec<Arc<LoadedPlugin>>,
    pub staged_failures: Vec<PluginRefreshFailure>,
}

/// Stable output of one complete `PluginRuntime::refresh_with_report` call.
#[derive(Clone, Debug)]
pub struct PluginRefreshResult {
    pub generation: u64,
    pub previous: Vec<Arc<LoadedPlugin>>,
    pub next: Vec<Arc<LoadedPlugin>>,
    pub added: Vec<Arc<LoadedPlugin>>,
    pub removed: Vec<Arc<LoadedPlugin>>,
    pub staged_failures: Vec<PluginRefreshFailure>,
}

/// Load all plugins and skills from disk for `options`.
///
/// **A6.3-pr7b split point**: this function's body is `todo!()` and will be
/// filled in by the next PR. The function signature and the types above are
/// final; callers must not change the return type.
///
/// Implementation outline (see TS sources):
/// 1. Call `getPilotExtensionPaths(projectRoot, pilotHome)`.
/// 2. `discoverPluginPaths` over `[globalPluginsDir/"global", projectPluginsDir/"project"]`.
/// 3. `discoverSkillPaths` over `[builtinSkillsRoot/"builtin"?, globalSkillsDir/"global", projectSkillsDir/"project"]`.
/// 4. Load each discovered path with `loadPluginFromPath` / `loadSkillFromPath`, collecting failures.
/// 5. Load builtin plugins from `options.builtin_plugins` via `loadPluginFromPath` (falling back to the original on error).
/// 6. Filter builtins by `builtinPluginsEnabled`.
/// 7. Return `LoadFromDiskResult { plugins, staged_failures }`.
pub async fn load_from_disk(options: &PluginRuntimeOptions) -> LoadFromDiskResult {
    // A6.3-pr7b: implement full disk loading here.
    // For now, pass through the builtin plugins directly (no disk discovery).
    use crate::runtime::plugins::loader::LoadedPlugin;
    let _ = PilotExtensionPaths::resolve(&options.project_root, &options.pilot_home);

    // Pass through builtin Arcs filtered by builtinPluginsEnabled.
    // Disk discovery is A6.3-pr7b scope.
    let plugins: Vec<Arc<LoadedPlugin>> = options
        .builtin_plugins
        .iter()
        .filter(|p| {
            if p.source != PluginSource::Builtin {
                return true;
            }
            options
                .builtin_plugins_enabled
                .get(&p.name)
                .copied()
                .unwrap_or(true)
        })
        .cloned()
        .collect();

    // A6.3-pr7b: add disk discovery and loading here before the builtins.
    LoadFromDiskResult {
        plugins,
        staged_failures: Vec::new(),
    }
}

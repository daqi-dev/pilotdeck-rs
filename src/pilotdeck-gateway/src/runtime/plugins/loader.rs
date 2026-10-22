//! Disk discovery and JSON/Markdown plugin loading. Runtime generations and leases
//! remain owned by PluginRuntime; this module only stages candidates.
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
    pub mcp_servers: Option<pilotdeck_protocol::JsValue>,
    pub lsp_servers: Option<pilotdeck_protocol::JsValue>,
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

mod config;
mod markdown;
mod server_value;
use super::PluginRuntimeError;
pub use config::{
    parse_plugin_manifest, resolve_marketplace_reference, validate_marketplace_name,
    validate_plugin_source_path,
};
pub use markdown::{get_plugin_command_name, load_plugin_commands, load_standalone_skill};
use pilotdeck_protocol::{JsValue, json_syntax, parse_json};
use std::{fs, path::Path};

pub(super) fn resolve_directory(base: &Path, child: &str) -> PathBuf {
    config::resolve_path(&config::join_path(base, child))
}

/// Ordered discovery result. Symlinks are followed by stat, not canonicalized.
#[derive(Clone, Debug)]
pub struct DiscoveredPluginPath {
    pub path: PathBuf,
    pub source: PluginSource,
}

pub fn discover_plugin_paths(directories: &[(PathBuf, PluginSource)]) -> Vec<DiscoveredPluginPath> {
    discover_paths(directories, false)
}

pub fn discover_skill_paths(directories: &[(PathBuf, PluginSource)]) -> Vec<DiscoveredPluginPath> {
    discover_paths(directories, true)
}

fn discover_paths(
    directories: &[(PathBuf, PluginSource)],
    skills: bool,
) -> Vec<DiscoveredPluginPath> {
    let mut output = Vec::new();
    for (directory, source) in directories {
        for path in sorted_entries(directory).unwrap_or_default() {
            if !fs::metadata(&path).is_ok_and(|s| s.is_dir()) {
                continue;
            }
            if skills
                && !sorted_entries(&path).is_ok_and(|files| {
                    files.iter().any(|p| {
                        p.file_name()
                            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("skill.md"))
                    })
                })
            {
                continue;
            }
            output.push(DiscoveredPluginPath {
                path,
                source: *source,
            });
        }
    }
    output
}

fn sorted_entries(directory: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut names = fs::read_dir(directory)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    // Node fs.readdir uses uv_fs_scandir (Unix fs.c: uv__fs_scandir_sort),
    // whose strcmp comparator sorts encoded filenames, not JS strings.
    names.sort();
    Ok(names
        .into_iter()
        .map(|name| directory.join(name.to_string_lossy().as_ref()))
        .collect())
}

fn io_error(operation: &str, path: &Path, error: &std::io::Error) -> PluginRuntimeError {
    let path = path.to_string_lossy();
    let diagnostic = if operation == "open" {
        crate::node_io::read_file(error, &path)
    } else {
        crate::node_io::diagnostic(error, operation, Some(&path))
    };
    PluginRuntimeError::new(diagnostic.map_or_else(|| error.to_string(), |(_, message)| message))
}

fn read_text(path: &Path) -> Result<String, PluginRuntimeError> {
    fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .map_err(|e| io_error("open", path, &e))
}

fn read_json(path: &Path) -> Result<JsValue, PluginRuntimeError> {
    let text = read_text(path)?;
    parse_json(&text)
        .map_err(|e| PluginRuntimeError::new(json_syntax::parse_error(&text).unwrap_or(e.message)))
}

fn string(value: &JsValue) -> Option<String> {
    value
        .as_string()
        .map(|s| String::from_utf16_lossy(s.units()))
}

fn basename(path: &Path) -> String {
    path.to_string_lossy()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

pub fn load_skill_from_path(
    path: &Path,
    source: PluginSource,
) -> Result<LoadedPlugin, PluginRuntimeError> {
    let name = basename(path);
    let skill = load_standalone_skill(&name, path)?;
    Ok(LoadedPlugin {
        name,
        path: path.into(),
        source,
        commands: vec![],
        skills: vec![skill],
        output_styles: vec![],
        hooks_config: None,
        mcp_servers: None,
        lsp_servers: None,
        dispose: None,
    })
}

pub fn load_plugin_from_path(
    path: &Path,
    source: PluginSource,
) -> Result<LoadedPlugin, PluginRuntimeError> {
    let manifest = parse_plugin_manifest(&read_json(&path.join("plugin.json"))?)?;
    let manifest = manifest.as_object().expect("validated manifest");
    let undefined = JsValue::Undefined;
    let get = |key| manifest.get_property(key).unwrap_or(&undefined);
    let hooks = get("hooks");
    let hooks_config = if hooks.as_object().is_some() {
        Some(crate::hooks::config::parse_hooks_config(hooks).settings)
    } else {
        let hook_path = string(hooks).unwrap_or_else(|| "hooks/hooks.json".into());
        read_json(&config::join_path(path, &hook_path))
            .ok()
            .map(|value| crate::hooks::config::parse_hooks_config(&value).settings)
    };
    let plugin_name = basename(path);
    let commands = load_configured_markdown(path, &plugin_name, get("commands"), "commands");
    let skills = load_configured_markdown(path, &plugin_name, get("skills"), "skills");
    let output_styles =
        load_configured_markdown(path, &plugin_name, get("outputStyles"), "output-styles");
    // The existing runtime represents server definitions as serde JSON. Keep
    // this conversion at the bridge; hooks and manifest validation use JsValue.
    let servers = |key| -> Result<Option<pilotdeck_protocol::JsValue>, PluginRuntimeError> {
        if get(key).as_object().is_none() {
            return Ok(None);
        }
        server_value::server_value(get(key)).map(Some)
    };
    Ok(LoadedPlugin {
        name: string(get("name")).expect("validated name"),
        path: path.into(),
        source,
        commands,
        skills,
        output_styles,
        hooks_config,
        mcp_servers: servers("mcpServers")?,
        lsp_servers: servers("lspServers")?,
        dispose: None,
    })
}

/// The style-only loader never reads hooks or unrelated markdown directories.
pub struct LoadedPluginOutputStyles {
    pub name: String,
    pub path: PathBuf,
    pub source: PluginSource,
    pub output_styles: Vec<LoadedPluginCommand>,
}

pub fn load_plugin_output_styles_from_path(
    path: &Path,
    source: PluginSource,
) -> Result<LoadedPluginOutputStyles, PluginRuntimeError> {
    let manifest = parse_plugin_manifest(&read_json(&path.join("plugin.json"))?)?;
    let manifest = manifest.as_object().expect("validated manifest");
    let configured = manifest
        .get_property("outputStyles")
        .unwrap_or(&JsValue::Undefined)
        .clone();
    Ok(LoadedPluginOutputStyles {
        name: string(manifest.get_property("name").expect("validated name"))
            .expect("validated name"),
        path: path.into(),
        source,
        output_styles: load_configured_markdown(
            path,
            &basename(path),
            &configured,
            "output-styles",
        ),
    })
}

fn load_configured_markdown(
    path: &Path,
    name: &str,
    configured: &JsValue,
    fallback: &str,
) -> Vec<LoadedPluginCommand> {
    let dirs = match configured {
        JsValue::String(_) => vec![string(configured).unwrap()],
        JsValue::Array(items) => items.iter().filter_map(string).collect(),
        _ => vec![fallback.into()],
    };
    dirs.into_iter()
        .flat_map(|dir| {
            load_plugin_commands(name, &config::join_path(path, &dir)).unwrap_or_default()
        })
        .filter(|c| fallback != "skills" || c.is_skill)
        .collect()
}

/// Stage disk I/O off the async executor; publication/retirement stays in the
/// existing single-flight refresh transaction (PluginRuntime::perform_refresh).
pub async fn load_from_disk(options: &PluginRuntimeOptions) -> LoadFromDiskResult {
    let options = PluginRuntimeOptions {
        project_root: options.project_root.clone(),
        pilot_home: options.pilot_home.clone(),
        builtin_skills_root: options.builtin_skills_root.clone(),
        builtin_plugins: options.builtin_plugins.clone(),
        builtin_plugins_enabled: options.builtin_plugins_enabled.clone(),
    };
    tokio::task::spawn_blocking(move || stage_from_disk(&options))
        .await
        .expect("plugin disk staging task")
}

fn stage_from_disk(options: &PluginRuntimeOptions) -> LoadFromDiskResult {
    let paths = PilotExtensionPaths::resolve(&options.project_root, &options.pilot_home);
    let discovered = discover_plugin_paths(&[
        (paths.global_plugins_dir, PluginSource::Global),
        (paths.project_plugins_dir, PluginSource::Project),
    ]);
    let mut skill_dirs = Vec::new();
    if let Some(path) = &options.builtin_skills_root {
        skill_dirs.push((path.clone(), PluginSource::Builtin));
    }
    skill_dirs.extend([
        (paths.global_skills_dir, PluginSource::Global),
        (paths.project_skills_dir, PluginSource::Project),
    ]);
    let discovered_skills = discover_skill_paths(&skill_dirs);
    // TS tries builtin disk loading before testing the enabled flag, and falls
    // back to the original object (including its source/dispose) on any error.
    let mut plugins: Vec<_> = options
        .builtin_plugins
        .iter()
        .map(|p| {
            load_plugin_from_path(&p.path, PluginSource::Builtin)
                .map(Arc::new)
                .unwrap_or_else(|_| Arc::clone(p))
        })
        .filter(|p| {
            p.source != PluginSource::Builtin
                || options
                    .builtin_plugins_enabled
                    .get(&p.name)
                    .copied()
                    .unwrap_or(true)
        })
        .collect();
    let mut failures = Vec::new();
    let mut successes = std::collections::HashSet::new();
    for (candidates, kind) in [
        (discovered, PluginKind::Plugin),
        (discovered_skills, PluginKind::Skill),
    ] {
        for candidate in candidates {
            let result = if kind == PluginKind::Plugin {
                load_plugin_from_path(&candidate.path, candidate.source)
            } else {
                load_skill_from_path(&candidate.path, candidate.source)
            };
            match result {
                Ok(plugin) => {
                    successes.insert((candidate.path, candidate.source));
                    plugins.push(Arc::new(plugin));
                }
                Err(_) => failures.push(PluginRefreshFailure {
                    path: candidate.path,
                    source: candidate.source,
                    kind,
                }),
            }
        }
    }
    failures.retain(|f| !successes.contains(&(f.path.clone(), f.source)));
    LoadFromDiskResult {
        plugins,
        staged_failures: failures,
    }
}

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
mod native;
mod server_value;
use super::PluginRuntimeError;
pub use config::{
    parse_plugin_manifest, resolve_marketplace_reference, validate_marketplace_name,
    validate_plugin_source_path,
};
pub use markdown::{get_plugin_command_name, load_plugin_commands, load_standalone_skill};
use pilotdeck_protocol::{JsValue, json_syntax, parse_json};
use std::{fs, path::Path};

pub(super) fn join_directory(base: &Path, child: &str) -> PathBuf {
    config::join_path(base, child)
}

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

/// Request-wide resource budget for the public style-only reload path.
pub(crate) struct StyleBudget {
    remaining_bytes: u64,
    remaining_entries: usize,
}
impl StyleBudget {
    fn native_skills() -> Self {
        Self {
            remaining_bytes: 8 * 1024 * 1024,
            remaining_entries: 2_048,
        }
    }
    pub(crate) fn new() -> Self {
        Self {
            remaining_bytes: 64 * 1024 * 1024,
            remaining_entries: 10_000,
        }
    }
    pub(super) fn entry(&mut self) -> Result<(), PluginRuntimeError> {
        self.remaining_entries = self
            .remaining_entries
            .checked_sub(1)
            .ok_or_else(style_limit)?;
        Ok(())
    }
    pub(crate) fn reserve(&mut self, bytes: usize) -> Result<(), PluginRuntimeError> {
        self.remaining_bytes = self
            .remaining_bytes
            .checked_sub(bytes as u64)
            .ok_or_else(style_limit)?;
        Ok(())
    }
    pub(super) fn read(&mut self, file: &mut std::fs::File) -> Result<String, PluginRuntimeError> {
        use std::io::Read;
        let mut bytes = Vec::new();
        file.take(self.remaining_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| PluginRuntimeError::new(e.to_string()))?;
        if bytes.len() as u64 > self.remaining_bytes {
            return Err(style_limit());
        }
        self.remaining_bytes -= bytes.len() as u64;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}
fn style_limit() -> PluginRuntimeError {
    PluginRuntimeError::gateway(
        "STYLE_RELOAD_LIMIT",
        "Output style reload resource limit exceeded.",
    )
}
fn bounded_style_json(
    path: &Path,
    budget: &mut StyleBudget,
) -> Result<JsValue, PluginRuntimeError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| io_error("open", path, &e))?;
        if !file
            .metadata()
            .map_err(|e| io_error("open", path, &e))?
            .is_file()
        {
            return Err(PluginRuntimeError::new(
                "Output style manifest is not a regular file.",
            ));
        }
        let text = budget.read(&mut file)?;
        parse_json(&text).map_err(|e| {
            PluginRuntimeError::new(json_syntax::parse_error(&text).unwrap_or(e.message))
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (path, budget);
        Err(PluginRuntimeError::new(
            "Bounded output style loading requires Unix.",
        ))
    }
}
pub(crate) fn discover_style_paths(
    directories: &[(PathBuf, PluginSource)],
    budget: &mut StyleBudget,
) -> Result<Vec<DiscoveredPluginPath>, PluginRuntimeError> {
    let mut output = Vec::new();
    for (directory, source) in directories {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        let mut names = Vec::new();
        for entry in entries {
            budget.entry()?;
            match entry {
                Ok(entry) => names.push(entry.file_name()),
                Err(_) => {
                    names.clear();
                    break;
                }
            }
        }
        names.sort();
        for name in names {
            let path = directory.join(name.to_string_lossy().as_ref());
            if fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
                output.push(DiscoveredPluginPath {
                    path,
                    source: *source,
                });
            }
        }
    }
    Ok(output)
}
/// One discovery candidate and its single parsed manifest, retained through
/// this staging transaction. Ordinary failures are handled per discovered path.
pub(crate) struct NativePluginManifest {
    pub(crate) path: PathBuf,
    pub(crate) source: PluginSource,
    manifest: Result<JsValue, PluginRuntimeError>,
}

pub(crate) fn validate_native_style_paths(
    directories: &[(PathBuf, PluginSource)],
    budget: &mut StyleBudget,
) -> Result<Vec<NativePluginManifest>, PluginRuntimeError> {
    let mut found = Vec::new();
    for (directory, source) in directories {
        // TS discovery treats readdir/stat failures as absent candidates.
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        let mut paths = Vec::new();
        for entry in entries {
            budget.entry()?;
            let Ok(entry) = entry else {
                paths.clear();
                break;
            };
            let Ok(metadata) = fs::metadata(entry.path()) else {
                continue;
            };
            if metadata.is_dir() {
                paths.push(entry.path());
            }
        }
        paths.sort();
        for path in paths {
            let manifest =
                native::read_manifest(&path, budget).and_then(|raw| parse_plugin_manifest(&raw));
            if let Err(error) = &manifest
                && native::fatal(error)
            {
                return Err(error.clone());
            }
            found.push(NativePluginManifest {
                path,
                source: *source,
                manifest,
            });
        }
    }
    Ok(found)
}

/// Load one explicitly selected SDK-local declaration with the same
/// descriptor-relative IO and aggregate budget as native project plugins.
pub(crate) fn load_native_session_plugin(
    path: &Path,
    budget: &mut StyleBudget,
) -> Result<LoadedPlugin, PluginRuntimeError> {
    budget.entry()?;
    let raw = native::read_manifest(path, budget)?;
    let manifest = parse_plugin_manifest(&raw)?;
    native::load_from_manifest(path, PluginSource::Project, &manifest, budget)
}

/// Test entrypoint for the same manifest and hook loading used by native staging.
#[cfg(test)]
fn load_native_plugin_hooks(
    path: &Path,
    budget: &mut StyleBudget,
) -> Result<Option<HooksSettings>, PluginRuntimeError> {
    let raw = native::read_manifest(path, budget)?;
    let manifest = parse_plugin_manifest(&raw)?;
    load_native_plugin_hooks_from_manifest(path, &manifest, budget)
}

fn load_native_plugin_hooks_from_manifest(
    path: &Path,
    manifest: &JsValue,
    budget: &mut StyleBudget,
) -> Result<Option<HooksSettings>, PluginRuntimeError> {
    let object = manifest.as_object().expect("validated plugin manifest");
    if let Some(value @ JsValue::Object(_)) = object.get("hooks") {
        return Ok(Some(
            crate::hooks::config::parse_hooks_config(value).settings,
        ));
    }
    let relative = object
        .get("hooks")
        .and_then(string)
        .unwrap_or_else(|| "hooks/hooks.json".to_owned());
    #[cfg(unix)]
    {
        // The manifest explicitly chooses this data file. TS accepts `..`
        // and static aliases here; ROOT-001 applies only to Markdown families.
        // Resolve once, then pin its physical parent and require a regular fd.
        let target = match config::join_path(path, &relative).canonicalize() {
            Ok(target) => target,
            Err(_) => return Ok(None),
        };
        let parent = target
            .parent()
            .ok_or_else(|| PluginRuntimeError::new("Invalid hook path."))?;
        let mut file = match crate::project_files::open_beneath(parent, &target) {
            Ok(file) => file,
            Err(error) if crate::project_files::nofollow_loop(&error) => {
                return Err(PluginRuntimeError::gateway(
                    "NATIVE_PLUGIN_UNSAFE",
                    error.to_string(),
                ));
            }
            Err(_) => return Ok(None),
        };
        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(_) => return Ok(None),
        };
        if metadata.is_dir() {
            return Ok(None);
        }
        if !metadata.is_file() {
            return Err(PluginRuntimeError::gateway(
                "NATIVE_PLUGIN_UNSAFE",
                "Hook config is not a regular file.",
            ));
        }
        let text = match budget.read(&mut file) {
            Ok(text) => text,
            Err(error) if error.code == Some("STYLE_RELOAD_LIMIT") => return Err(error),
            Err(_) => return Ok(None),
        };
        native::check_manifest_tokens(&text, budget)?;
        let value = match parse_json(&text) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        Ok(Some(
            crate::hooks::config::parse_hooks_config(&value).settings,
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, relative, budget);
        Err(PluginRuntimeError::new(
            "Bounded hook loading requires Unix.",
        ))
    }
}

/// Stage bounded disk declarations through the existing generation transaction.
/// Programmatic plugins remain outside this loader; LSP data does not start a provider.
pub(crate) fn stage_native_style_plugins(
    options: &PluginRuntimeOptions,
) -> Result<LoadFromDiskResult, PluginRuntimeError> {
    if !options.builtin_plugins.is_empty() {
        return Err(PluginRuntimeError::new(
            "Native restricted plugin family cannot load programmatic builtins.",
        ));
    }
    let paths = PilotExtensionPaths::resolve(&options.project_root, &options.pilot_home);
    let mut budget = StyleBudget::new();
    let paths = validate_native_style_paths(
        &[
            (paths.global_plugins_dir, PluginSource::Global),
            (paths.project_plugins_dir, PluginSource::Project),
        ],
        &mut budget,
    )?;
    let mut skill_budget = StyleBudget::native_skills();
    let builtins = stage_native_builtin_skills(options, &mut skill_budget)?;
    let mut plugins = builtins.plugins;
    let mut failures = builtins.staged_failures;
    for entry in paths {
        let attempt = entry.manifest.and_then(|manifest| {
            native::load_from_manifest(&entry.path, entry.source, &manifest, &mut budget)
        });
        match attempt {
            Ok(plugin) => plugins.push(Arc::new(plugin)),
            Err(error) if native::fatal(&error) => return Err(error),
            Err(_) => failures.push(PluginRefreshFailure {
                path: entry.path,
                source: entry.source,
                kind: PluginKind::Plugin,
            }),
        }
    }
    let skills = stage_native_standalone_skills(options, &mut skill_budget)?;
    plugins.extend(skills.plugins);
    failures.extend(skills.staged_failures);
    Ok(LoadFromDiskResult {
        plugins,
        staged_failures: failures,
    })
}

/// First-party builtin skill declaration. This is a real bounded Markdown
/// contribution, not a catalogue sort rule or an identity-only placeholder.
/// The broader browser plugin's MCP declaration is outside this skill profile.
/// TS loads builtin plugins before disk candidates; a later same-name/source
/// standalone replaces content while PluginRegistry retains its original slot.
fn stage_native_builtin_skills(
    options: &PluginRuntimeOptions,
    budget: &mut StyleBudget,
) -> Result<LoadFromDiskResult, PluginRuntimeError> {
    use crate::runtime::mcp::native_builtins;
    let mut result = LoadFromDiskResult {
        plugins: Vec::new(),
        staged_failures: Vec::new(),
    };
    let executable = std::env::current_exe()
        .map_err(|e| PluginRuntimeError::gateway("NATIVE_MCP_PATH", e.to_string()))?;
    let executable = fs::canonicalize(&executable).unwrap_or(executable);
    let fallback = crate::native_process::builtin_skills_root_at(
        &options.project_root,
        &crate::config::Env::new(),
        executable.parent(),
    );
    let root = options
        .builtin_skills_root
        .as_deref()
        .or(fallback.as_deref())
        .ok_or_else(|| {
            PluginRuntimeError::gateway(
                "NATIVE_SKILL_PATH",
                "Builtin skills root could not be resolved.",
            )
        })?;
    // Only these inert first-party declarations may contribute builtin MCP.
    // No executable plugin entry/callback or arbitrary on-disk manifest loads.
    for (name, directory, skill_relative) in [
        (
            "browser-use",
            ".builtin-plugins/browser-use",
            Some("skills/browser-use-install"),
        ),
        ("funasr", ".builtin-plugins/funasr", None),
        (
            "windows-skills",
            ".builtin-plugins/windows-skills",
            Some("skills/powershell-safe-invocation"),
        ),
    ] {
        if options.builtin_plugins_enabled.get(name) == Some(&false) {
            continue;
        }
        let path = root.join(directory);
        let mut skills = Vec::new();
        if let Some(relative) = skill_relative {
            let skill_path = path.join(relative);
            if fs::metadata(&skill_path).is_ok_and(|m| m.is_dir()) {
                match markdown::load_standalone_skill_bounded(
                    &basename(&skill_path),
                    &skill_path,
                    budget,
                ) {
                    Ok(mut skill) => {
                        skill.name = markdown::get_plugin_command_name(
                            name,
                            &skill.path,
                            &path.join("skills"),
                        );
                        skills.push(skill);
                    }
                    Err(error) if error.code == Some("STYLE_RELOAD_LIMIT") => {
                        return Err(PluginRuntimeError::gateway(
                            "NATIVE_SKILL_LIMIT",
                            "Native skill source budget exceeded.",
                        ));
                    }
                    Err(_) => result.staged_failures.push(PluginRefreshFailure {
                        path: path.clone(),
                        source: PluginSource::Builtin,
                        kind: PluginKind::Skill,
                    }),
                }
            } else if name == "windows-skills" {
                continue;
            }
        }
        let mcp_servers = match name {
            "browser-use" => Some(native_builtins::browser_servers()),
            "funasr" => Some(
                native_builtins::funasr_servers(&executable)
                    .map_err(|e| PluginRuntimeError::gateway("NATIVE_MCP_PATH", e.to_string()))?,
            ),
            _ => None,
        };
        result.plugins.push(Arc::new(LoadedPlugin {
            name: name.to_owned(),
            path,
            source: PluginSource::Builtin,
            skills,
            commands: Vec::new(),
            output_styles: Vec::new(),
            hooks_config: None,
            mcp_servers,
            lsp_servers: None,
            dispose: None,
        }));
    }
    Ok(result)
}

/// Standalone skills are inert markdown: no plugin manifests, hooks, MCP,
/// command directories, scripts, or assets are read by this selective loader.
/// Native-only bound: 8 MiB source bytes and 2,048 directory entries total.
fn stage_native_standalone_skills(
    options: &PluginRuntimeOptions,
    budget: &mut StyleBudget,
) -> Result<LoadFromDiskResult, PluginRuntimeError> {
    let paths = PilotExtensionPaths::resolve(&options.project_root, &options.pilot_home);
    let mut directories = Vec::new();
    if let Some(root) = &options.builtin_skills_root {
        directories.push((root.clone(), PluginSource::Builtin));
    }
    directories.extend([
        (paths.global_skills_dir, PluginSource::Global),
        (paths.project_skills_dir, PluginSource::Project),
    ]);
    let mut plugins = Vec::new();
    let mut staged_failures = Vec::new();
    for (directory, source) in directories {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_error("scandir", &directory, &error)),
        };
        let mut names = Vec::new();
        for entry in entries {
            budget.entry()?;
            names.push(
                entry
                    .map_err(|error| io_error("scandir", &directory, &error))?
                    .file_name(),
            );
        }
        names.sort();
        for name in names {
            if source == PluginSource::Builtin
                && name.as_os_str() == std::ffi::OsStr::new(".builtin-plugins")
            {
                continue; // private builtin assets are not standalone skills
            }
            let path = directory.join(name);
            if !fs::metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                continue;
            }
            match markdown::load_standalone_skill_bounded(&basename(&path), &path, budget) {
                Ok(skill) => plugins.push(Arc::new(LoadedPlugin {
                    name: basename(&path),
                    path,
                    source,
                    skills: vec![skill],
                    commands: vec![],
                    output_styles: vec![],
                    hooks_config: None,
                    mcp_servers: None,
                    lsp_servers: None,
                    dispose: None,
                })),
                Err(error) if error.code == Some("STYLE_RELOAD_LIMIT") => {
                    return Err(PluginRuntimeError::gateway(
                        "NATIVE_SKILL_LIMIT",
                        "Native skill source budget exceeded.",
                    ));
                }
                Err(_) => staged_failures.push(PluginRefreshFailure {
                    path,
                    source,
                    kind: PluginKind::Skill,
                }),
            }
        }
    }
    Ok(LoadFromDiskResult {
        plugins,
        staged_failures,
    })
}

pub(crate) fn load_style_bounded(
    path: &Path,
    source: PluginSource,
    budget: &mut StyleBudget,
) -> Result<LoadedPluginOutputStyles, PluginRuntimeError> {
    let parsed = bounded_style_json(&path.join("plugin.json"), budget)?;
    let manifest = parse_plugin_manifest(&parsed)?;
    let manifest = manifest.as_object().expect("validated manifest");
    let undefined = JsValue::Undefined;
    let configured = manifest.get_property("outputStyles").unwrap_or(&undefined);
    let dirs = match configured {
        JsValue::String(_) => vec![string(configured).unwrap()],
        JsValue::Array(items) => items.iter().filter_map(string).collect(),
        _ => vec!["output-styles".into()],
    };
    let mut output_styles = Vec::new();
    for dir in dirs {
        let base = config::join_path(path, &dir);
        match markdown::load_plugin_commands_bounded(&basename(path), &base, path, budget) {
            Ok(styles) => output_styles.extend(styles),
            Err(error) if error.code == Some("STYLE_RELOAD_LIMIT") => return Err(error),
            Err(_) => {}
        }
    }
    Ok(LoadedPluginOutputStyles {
        name: string(manifest.get_property("name").expect("validated name"))
            .expect("validated name"),
        path: path.into(),
        source,
        output_styles,
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

fn configured_markdown_directories(configured: &JsValue, fallback: &str) -> Vec<String> {
    match configured {
        JsValue::String(_) => vec![string(configured).unwrap()],
        JsValue::Array(items) => items.iter().filter_map(string).collect(),
        _ => vec![fallback.into()],
    }
}

fn load_configured_markdown(
    path: &Path,
    name: &str,
    configured: &JsValue,
    fallback: &str,
) -> Vec<LoadedPluginCommand> {
    configured_markdown_directories(configured, fallback)
        .into_iter()
        .flat_map(|dir| {
            let base = config::join_path(path, &dir);
            markdown::load_plugin_commands_with_root(name, &base, path).unwrap_or_default()
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

#[cfg(all(test, unix))]
mod native_hook_loading_tests {
    use super::*;
    use pilotdeck_protocol::JsObject;
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn fixed_ts_inline_hook_and_markdown_declarations_are_accepted() {
        let root = Scratch(
            std::env::temp_dir().join(format!("pdrs-plugin-hook-{}", uuid::Uuid::new_v4())),
        );
        let plugin = root.0.join("plugins/fixture");
        std::fs::create_dir_all(&plugin).unwrap();
        let oracle = pilotdeck_protocol::parse_json(include_str!(
            "../../../tests/fixtures/native-hooks/oracle.json"
        ))
        .unwrap();
        let hooks = oracle.get("hookConfig").cloned().unwrap();
        let mut manifest = JsObject::new();
        manifest.insert("name".into(), "fixture".into());
        manifest.insert("version".into(), "1.0.0".into());
        manifest.insert("hooks".into(), hooks.clone());
        std::fs::write(
            plugin.join("plugin.json"),
            pilotdeck_protocol::stringify_json(&JsValue::Object(manifest.clone()))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let dirs = vec![(root.0.join("plugins"), PluginSource::Project)];
        let mut budget = StyleBudget::new();
        assert_eq!(
            validate_native_style_paths(&dirs, &mut budget)
                .unwrap()
                .len(),
            1
        );
        let settings = load_native_plugin_hooks(&plugin, &mut budget)
            .unwrap()
            .unwrap();
        let mut actual = JsObject::new();
        for (event, matchers) in settings {
            actual.insert(
                event,
                JsValue::Array(matchers.iter().map(|matcher| matcher.to_value()).collect()),
            );
        }
        assert_eq!(JsValue::Object(actual), hooks);
        manifest.insert("commands".into(), "commands".into());
        std::fs::write(
            plugin.join("plugin.json"),
            pilotdeck_protocol::stringify_json(&JsValue::Object(manifest))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            validate_native_style_paths(&dirs, &mut StyleBudget::new())
                .unwrap()
                .len(),
            1
        );
    }
}

#[cfg(test)]
#[path = "builtin_priority_tests.rs"]
mod builtin_priority_tests;

#[cfg(test)]
mod native_builtin_root_tests {
    use super::*;
    #[test]
    fn installed_builtin_root_uses_the_same_two_generation_layouts_as_startup() {
        let root =
            std::env::temp_dir().join(format!("pdrs-native-builtin-root-{}", uuid::Uuid::new_v4()));
        let exe = root.join("generations/current/bin");
        let first = root.join("generations/current/skills");
        let second = root.join("generations/skills");
        fs::create_dir_all(&exe).unwrap();
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let project = root.join("project");
        fs::create_dir(&project).unwrap();
        let resolve = || {
            crate::native_process::builtin_skills_root_at(
                &project,
                &crate::config::Env::new(),
                Some(&exe),
            )
            .unwrap()
        };
        assert_eq!(resolve(), first);
        fs::remove_dir(&first).unwrap();
        assert_eq!(resolve(), second);
        fs::remove_dir_all(root).unwrap();
    }
}

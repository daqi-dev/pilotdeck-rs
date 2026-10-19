//! `PluginRuntimeOptions` and the resolved extension paths.
use std::path::PathBuf;

/// Mirrors `PluginRuntimeOptions` from `PluginRuntime.ts:44-51`.
pub struct PluginRuntimeOptions {
    pub project_root: PathBuf,
    pub pilot_home: PathBuf,
    /// Read-only skills shipped with the active PilotDeck build.
    pub builtin_skills_root: Option<PathBuf>,
    /// Pre-constructed builtin plugin objects; stored as Arc so they can be
    /// handed to multiple refresh generations without cloning a non-Clone type.
    pub builtin_plugins: Vec<std::sync::Arc<super::loader::LoadedPlugin>>,
    /// `builtinPluginsEnabled`: `false` disables the named builtin.
    pub builtin_plugins_enabled: std::collections::HashMap<String, bool>,
}

/// Paths resolved by `getPilotExtensionPaths` (pilot/paths.ts).
pub struct PilotExtensionPaths {
    pub global_plugins_dir: PathBuf,
    pub global_skills_dir: PathBuf,
    pub project_plugins_dir: PathBuf,
    pub project_skills_dir: PathBuf,
}

impl PilotExtensionPaths {
    pub fn resolve(project_root: &std::path::Path, pilot_home: &std::path::Path) -> Self {
        let pilotdeck = ".pilotdeck";
        Self {
            global_plugins_dir: pilot_home.join("plugins"),
            global_skills_dir: pilot_home.join("skills"),
            project_plugins_dir: project_root.join(pilotdeck).join("plugins"),
            project_skills_dir: project_root.join(pilotdeck).join("skills"),
        }
    }
}

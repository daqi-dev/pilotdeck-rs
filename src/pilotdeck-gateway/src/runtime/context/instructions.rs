//! PILOTDECK.md instruction discovery (TS `context/instructions/InstructionDiscovery.ts`).
//!
//! Discovery is intentionally storage-agnostic at the boundary: missing files,
//! directories and permission failures are all "absent layer" as in Node, while
//! file contents are decoded with replacement like `readFile(path, "utf8")`.
use crate::metadata::{js_trim, resolve};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Managed,
    User,
    Project,
    ProjectRules,
    Local,
}
impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::User => "user",
            Self::Project => "project",
            Self::ProjectRules => "project-rules",
            Self::Local => "local",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Managed => " (managed instructions, set by administrator)",
            Self::User => " (user's global instructions for all projects)",
            Self::Project => " (project instructions, checked into the codebase)",
            Self::ProjectRules => " (project rule, checked into the codebase)",
            Self::Local => " (user's private project instructions, not checked in)",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layer {
    pub scope: Scope,
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug)]
pub struct InstructionDiscovery {
    project_root: PathBuf,
    cwd: PathBuf,
    pilot_home: PathBuf,
    managed_dir: ManagedDirectory,
}
#[derive(Clone, Debug)]
enum ManagedDirectory {
    Fixed(Option<PathBuf>),
    Environment,
}
impl InstructionDiscovery {
    pub fn new(
        project_root: impl Into<PathBuf>,
        cwd: impl Into<PathBuf>,
        pilot_home: impl Into<PathBuf>,
        managed_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            project_root: project_root.into(),
            cwd: cwd.into(),
            pilot_home: pilot_home.into(),
            managed_dir: ManagedDirectory::Fixed(managed_dir),
        }
    }
    /// Default managed directory from the host environment.
    pub fn from_environment(
        project_root: impl Into<PathBuf>,
        cwd: impl Into<PathBuf>,
        pilot_home: impl Into<PathBuf>,
    ) -> Self {
        Self {
            project_root: project_root.into(),
            cwd: cwd.into(),
            pilot_home: pilot_home.into(),
            managed_dir: ManagedDirectory::Environment,
        }
    }

    /// Synchronous storage pass; call from `spawn_blocking` in async callers.
    pub fn discover(&self) -> Result<Vec<Layer>, String> {
        self.discover_with_environment(|| {
            std::env::var_os("PILOTDECK_MANAGED_CONFIG")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
    }

    fn discover_with_environment(
        &self,
        managed_environment: impl FnOnce() -> Option<PathBuf>,
    ) -> Result<Vec<Layer>, String> {
        let mut layers = Vec::new();
        let mut seen = HashSet::new();
        let managed_dir = match &self.managed_dir {
            ManagedDirectory::Fixed(dir) => dir.clone(),
            ManagedDirectory::Environment => managed_environment(),
        };
        if let Some(dir) = &managed_dir {
            self.try_add(
                &mut layers,
                &mut seen,
                Scope::Managed,
                absolute(dir)?.join("PILOTDECK.md"),
            )?;
        }
        let home = absolute(&self.pilot_home)?;
        self.try_add(
            &mut layers,
            &mut seen,
            Scope::User,
            home.join("PILOTDECK.md"),
        )?;
        self.try_add_rules_dir(&mut layers, &mut seen, Scope::User, home.join("rules"))?;
        for dir in self.collect_directory_chain()? {
            self.try_add(
                &mut layers,
                &mut seen,
                Scope::Project,
                dir.join("PILOTDECK.md"),
            )?;
            self.try_add(
                &mut layers,
                &mut seen,
                Scope::Project,
                dir.join(".pilotdeck").join("PILOTDECK.md"),
            )?;
            self.try_add_rules_dir(
                &mut layers,
                &mut seen,
                Scope::ProjectRules,
                dir.join(".pilotdeck").join("rules"),
            )?;
            self.try_add(
                &mut layers,
                &mut seen,
                Scope::Local,
                dir.join("PILOTDECK.local.md"),
            )?;
        }
        Ok(layers)
    }

    /// Root-first chain from `project_root` down to `cwd`, matching TS even when
    /// cwd is outside the project (the loop then runs up to `/`).
    fn collect_directory_chain(&self) -> Result<Vec<PathBuf>, String> {
        let root = absolute(&self.project_root)?;
        let current = absolute(&self.cwd)?;
        if root == current {
            return Ok(vec![root]);
        }
        let mut dirs = Vec::new();
        let mut dir = current;
        loop {
            let parent = dir.parent().map_or_else(|| dir.clone(), Path::to_path_buf);
            if dir == root || dir == parent {
                break;
            }
            dirs.push(dir);
            dir = parent;
        }
        dirs.push(root);
        dirs.reverse();
        Ok(dirs)
    }

    fn try_add(
        &self,
        layers: &mut Vec<Layer>,
        seen: &mut HashSet<PathBuf>,
        scope: Scope,
        file_path: PathBuf,
    ) -> Result<(), String> {
        let resolved = absolute(&file_path)?;
        if !seen.insert(resolved.clone()) {
            return Ok(());
        }
        let Ok(bytes) = fs::read(&resolved) else {
            return Ok(());
        };
        // Node's UTF-8 decoder replaces ill-formed sequences and never throws.
        let content = String::from_utf8_lossy(&bytes);
        let trimmed = js_trim(&content);
        if !trimmed.is_empty() {
            layers.push(Layer {
                scope,
                path: resolved.to_string_lossy().into_owned(),
                content: trimmed.to_owned(),
            });
        }
        Ok(())
    }

    fn try_add_rules_dir(
        &self,
        layers: &mut Vec<Layer>,
        seen: &mut HashSet<PathBuf>,
        scope: Scope,
        dir_path: PathBuf,
    ) -> Result<(), String> {
        let dir_path = absolute(&dir_path)?;
        let Ok(entries) = fs::read_dir(&dir_path) else {
            return Ok(());
        };
        let mut names = Vec::new();
        for entry in entries.flatten() {
            // Dirent.isFile() is false for symlinks: file_type() is lstat.
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".md") && name != "SKILL.md" {
                names.push(name);
            }
        }
        // Array.prototype.sort default: UTF-16 code-unit order, not bytes.
        names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        for name in names {
            self.try_add(layers, seen, scope, dir_path.join(name))?;
        }
        Ok(())
    }
}

fn absolute(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(resolve(Path::new("/"), &path.to_string_lossy()));
    }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    Ok(resolve(&cwd, &path.to_string_lossy()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn managed_environment_is_read_on_each_discovery_like_real_ts() {
        let raw = include_str!("../../../tests/fixtures/diff-backfill/oracle.json");
        crate::test_baseline::oracle(raw);
        let oracle: Value = serde_json::from_str(raw).unwrap();
        let root = std::env::temp_dir().join(format!("pdrs-502-{}", uuid::Uuid::new_v4()));
        for name in ["project", "home", "a", "b"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        for name in ["a", "b"] {
            std::fs::write(
                root.join(name).join("PILOTDECK.md"),
                format!("  managed {name}\n"),
            )
            .unwrap();
        }
        let discovery = InstructionDiscovery::from_environment(
            root.join("project"),
            root.join("project"),
            root.join("home"),
        );
        let fixed = InstructionDiscovery::new(
            root.join("project"),
            root.join("project"),
            root.join("home"),
            Some(root.join("a")),
        );
        let cases = oracle["managed"].as_array().unwrap();
        assert_eq!(cases.len(), 5);
        for case in cases {
            let value = case["value"].as_str().filter(|value| !value.is_empty());
            // Inject the per-call environment read instead of mutating process
            // env (unsafe with parallel Rust tests). TS changes its real env.
            let layers = discovery
                .discover_with_environment(|| value.map(|value| root.join(value)))
                .unwrap();
            let actual: Vec<_> = layers
                .iter()
                .map(|layer| {
                    json!({
                        "scope": layer.scope.as_str(), "content": layer.content,
                        "path": layer.path.replace(root.to_str().unwrap(), "<tree>"),
                    })
                })
                .collect();
            assert_eq!(json!(actual), case["layers"], "{case}");
            let fixed_layers = fixed
                .discover_with_environment(|| {
                    panic!("explicit directory must not read environment")
                })
                .unwrap();
            assert_eq!(
                fixed_layers[0].content,
                cases[0]["layers"][0]["content"].as_str().unwrap()
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

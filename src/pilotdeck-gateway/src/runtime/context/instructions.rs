//! PILOTDECK.md instruction discovery (TS `context/instructions/InstructionDiscovery.ts`).
//!
//! Discovery is intentionally storage-agnostic at the boundary: missing files,
//! directories and permission failures are all "absent layer" as in Node, while
//! file contents are decoded with replacement like `readFile(path, "utf8")`.
use crate::metadata::resolve;
use std::{
    fs,
    path::{Path, PathBuf},
};

use pilotdeck_context::instructions::{DirectoryEntry, Discovery, EntryKind, Storage};
pub use pilotdeck_context::instructions::{Layer, Scope};
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
        let managed_dir = match &self.managed_dir {
            ManagedDirectory::Fixed(dir) => dir.clone(),
            ManagedDirectory::Environment => managed_environment(),
        };
        let managed_dir = managed_dir.as_deref().map(absolute).transpose()?;
        Discovery::new(
            absolute(&self.project_root)?,
            absolute(&self.cwd)?,
            absolute(&self.pilot_home)?,
            managed_dir.as_deref(),
        )?
        .discover(&Filesystem)
    }
}

struct Filesystem;
impl Storage for Filesystem {
    fn read(&self, path: &Path) -> Result<Option<Vec<u8>>, String> {
        Ok(fs::read(path).ok())
    }
    fn list(&self, path: &Path) -> Result<Option<Vec<DirectoryEntry>>, String> {
        let Ok(entries) = fs::read_dir(path) else {
            return Ok(None);
        };
        Ok(Some(
            entries
                .flatten()
                .filter_map(|entry| {
                    let kind = entry.file_type().ok()?;
                    Some(DirectoryEntry {
                        name: entry.file_name().to_string_lossy().into_owned(),
                        kind: if kind.is_file() {
                            EntryKind::File
                        } else if kind.is_dir() {
                            EntryKind::Directory
                        } else if kind.is_symlink() {
                            EntryKind::Symlink
                        } else {
                            EntryKind::Other
                        },
                    })
                })
                .collect(),
        ))
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
    #[cfg(unix)]
    #[test]
    fn real_filesystem_rules_preserve_comma_names_utf16_order_and_exclude_symlinks() {
        let tree =
            std::env::temp_dir().join(format!("pdrs-instruction-port-{}", uuid::Uuid::new_v4()));
        let project = tree.join("project");
        let home = tree.join("home");
        let rules = project.join(".pilotdeck/rules");
        fs::create_dir_all(&rules).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(tree.join("target.txt"), "must not load rule symlink").unwrap();
        std::os::unix::fs::symlink(tree.join("target.txt"), rules.join("linked.md")).unwrap();
        for name in ["comma,name.md", "\u{10000}.md", "\u{e000}.md", "SKILL.md"] {
            fs::write(rules.join(name), name).unwrap();
        }
        let layers = InstructionDiscovery::new(&project, &project, &home, None)
            .discover()
            .unwrap();
        let names: Vec<_> = layers
            .iter()
            .map(|layer| {
                Path::new(&layer.path)
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
            })
            .collect();
        assert_eq!(names, vec!["comma,name.md", "\u{10000}.md", "\u{e000}.md"]);
        fs::remove_dir_all(tree).unwrap();
    }
}

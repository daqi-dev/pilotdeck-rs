//! Original PILOTDECK.md discovery over an explicitly supplied storage port.
//! No filesystem access, home discovery, environment reads or implicit cwd.
use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: EntryKind,
}
/// `None` is an actual absent/unreadable layer. Transport, authority and limit
/// failures are errors, so an unavailable configured port cannot look empty.
pub trait Storage {
    fn read(&self, path: &Path) -> Result<Option<Vec<u8>>, String>;
    fn list(&self, path: &Path) -> Result<Option<Vec<DirectoryEntry>>, String>;
}
#[derive(Clone, Debug)]
pub struct Discovery {
    project_root: PathBuf,
    cwd: PathBuf,
    user_root: PathBuf,
    managed_root: Option<PathBuf>,
}
/// Lexical Node path.resolve normalization for an explicitly absolute path.
/// This deliberately does not canonicalize symlinks or consult the filesystem.
fn absolute(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("instruction discovery requires explicit absolute roots".into());
    }
    let mut output = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                output.pop();
            }
            Component::CurDir => {}
            part => output.push(part.as_os_str()),
        }
    }
    Ok(output)
}
fn js_space(c: char) -> bool {
    matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')
}
impl Discovery {
    pub fn new(
        project_root: impl AsRef<Path>,
        cwd: impl AsRef<Path>,
        user_root: impl AsRef<Path>,
        managed_root: Option<&Path>,
    ) -> Result<Self, String> {
        Ok(Self {
            project_root: absolute(project_root.as_ref())?,
            cwd: absolute(cwd.as_ref())?,
            user_root: absolute(user_root.as_ref())?,
            managed_root: managed_root.map(absolute).transpose()?,
        })
    }
    /// Root-first chain, including the original cwd-outside-project behavior.
    pub fn directory_chain(&self) -> Vec<PathBuf> {
        if self.project_root == self.cwd {
            return vec![self.project_root.clone()];
        }
        let mut dirs = Vec::new();
        let mut current = self.cwd.clone();
        loop {
            let parent = current
                .parent()
                .map_or_else(|| current.clone(), Path::to_path_buf);
            if current == self.project_root || current == parent {
                break;
            }
            dirs.push(current);
            current = parent;
        }
        dirs.push(self.project_root.clone());
        dirs.reverse();
        dirs
    }
    pub fn discover(&self, storage: &dyn Storage) -> Result<Vec<Layer>, String> {
        let mut layers = Vec::new();
        let mut seen = HashSet::new();
        if let Some(dir) = &self.managed_root {
            Self::add(
                storage,
                &mut layers,
                &mut seen,
                Scope::Managed,
                dir.join("PILOTDECK.md"),
            )?;
        }
        Self::add(
            storage,
            &mut layers,
            &mut seen,
            Scope::User,
            self.user_root.join("PILOTDECK.md"),
        )?;
        Self::rules(
            storage,
            &mut layers,
            &mut seen,
            Scope::User,
            self.user_root.join("rules"),
        )?;
        for dir in self.directory_chain() {
            Self::add(
                storage,
                &mut layers,
                &mut seen,
                Scope::Project,
                dir.join("PILOTDECK.md"),
            )?;
            Self::add(
                storage,
                &mut layers,
                &mut seen,
                Scope::Project,
                dir.join(".pilotdeck/PILOTDECK.md"),
            )?;
            Self::rules(
                storage,
                &mut layers,
                &mut seen,
                Scope::ProjectRules,
                dir.join(".pilotdeck/rules"),
            )?;
            Self::add(
                storage,
                &mut layers,
                &mut seen,
                Scope::Local,
                dir.join("PILOTDECK.local.md"),
            )?;
        }
        Ok(layers)
    }
    fn add(
        storage: &dyn Storage,
        layers: &mut Vec<Layer>,
        seen: &mut HashSet<PathBuf>,
        scope: Scope,
        path: PathBuf,
    ) -> Result<(), String> {
        let path = absolute(&path)?;
        if !seen.insert(path.clone()) {
            return Ok(());
        }
        let Some(bytes) = storage.read(&path)? else {
            return Ok(());
        };
        let content = String::from_utf8_lossy(&bytes);
        let trimmed = content.trim_matches(js_space);
        if !trimmed.is_empty() {
            layers.push(Layer {
                scope,
                path: path.to_string_lossy().into_owned(),
                content: trimmed.into(),
            });
        }
        Ok(())
    }
    fn rules(
        storage: &dyn Storage,
        layers: &mut Vec<Layer>,
        seen: &mut HashSet<PathBuf>,
        scope: Scope,
        dir: PathBuf,
    ) -> Result<(), String> {
        let dir = absolute(&dir)?;
        let Some(entries) = storage.list(&dir)? else {
            return Ok(());
        };
        let mut names = Vec::new();
        for entry in entries {
            if entry.name.is_empty()
                || entry.name == "."
                || entry.name == ".."
                || entry.name.contains(['/', '\0'])
            {
                return Err("instruction directory port returned a non-basename entry".into());
            }
            if entry.kind == EntryKind::File
                && entry.name.ends_with(".md")
                && entry.name != "SKILL.md"
            {
                names.push(entry.name);
            }
        }
        names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        for name in names {
            Self::add(storage, layers, seen, scope, dir.join(name))?;
        }
        Ok(())
    }
}
pub fn format_layers(layers: &[Layer]) -> Option<String> {
    if layers.is_empty() {
        return None;
    }
    let blocks = layers
        .iter()
        .map(|layer| {
            format!(
                "Contents of {}{}:\n\n{}",
                layer.path,
                layer.scope.description(),
                layer.content
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    Some(format!(
        "<project-instructions>\nProject instructions are shown below. Adhere to these instructions. IMPORTANT: These instructions OVERRIDE any default behavior.\n\n{blocks}\n</project-instructions>"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::BTreeMap};
    #[derive(Default)]
    struct Memory {
        files: BTreeMap<PathBuf, Vec<u8>>,
        dirs: BTreeMap<PathBuf, Vec<DirectoryEntry>>,
        reads: RefCell<Vec<PathBuf>>,
        fail: bool,
    }
    impl Storage for Memory {
        fn read(&self, path: &Path) -> Result<Option<Vec<u8>>, String> {
            if self.fail {
                return Err("configured transport failed".into());
            }
            self.reads.borrow_mut().push(path.into());
            Ok(self.files.get(path).cloned())
        }
        fn list(&self, path: &Path) -> Result<Option<Vec<DirectoryEntry>>, String> {
            Ok(self.dirs.get(path).cloned())
        }
    }
    #[test]
    fn original_order_utf16_comma_names_lstat_kinds_and_utf8_trim_are_preserved() {
        let mut memory = Memory::default();
        for (path, content) in [
            ("/managed/PILOTDECK.md", "\u{feff} managed \n"),
            ("/project/PILOTDECK.md", "root"),
            ("/project/.pilotdeck/PILOTDECK.md", "hidden root"),
            ("/project/PILOTDECK.local.md", "local root"),
            ("/project/sub/PILOTDECK.md", "sub"),
            ("/project/sub/.pilotdeck/PILOTDECK.md", "hidden sub"),
            ("/project/sub/PILOTDECK.local.md", "local sub"),
        ] {
            memory
                .files
                .insert(path.into(), content.as_bytes().to_vec());
        }
        memory
            .files
            .insert("/user/PILOTDECK.md".into(), vec![b'a', 0xff, b'b']);
        let entries = vec![
            ("z.md", EntryKind::File),
            ("comma,name.md", EntryKind::File),
            ("SKILL.md", EntryKind::File),
            ("link.md", EntryKind::Symlink),
            ("dir.md", EntryKind::Directory),
            ("socket.md", EntryKind::Other),
            ("\u{e000}.md", EntryKind::File),
            ("\u{10000}.md", EntryKind::File),
        ];
        for (name, _) in &entries {
            memory.files.insert(
                Path::new("/user/rules").join(name),
                name.as_bytes().to_vec(),
            );
        }
        memory.dirs.insert(
            "/user/rules".into(),
            entries
                .into_iter()
                .map(|(name, kind)| DirectoryEntry {
                    name: name.into(),
                    kind,
                })
                .collect(),
        );
        let layers = Discovery::new(
            "/project",
            "/project/sub",
            "/user",
            Some(Path::new("/managed")),
        )
        .unwrap()
        .discover(&memory)
        .unwrap();
        let paths: Vec<_> = layers.iter().map(|layer| layer.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "/managed/PILOTDECK.md",
                "/user/PILOTDECK.md",
                "/user/rules/comma,name.md",
                "/user/rules/z.md",
                "/user/rules/\u{10000}.md",
                "/user/rules/\u{e000}.md",
                "/project/PILOTDECK.md",
                "/project/.pilotdeck/PILOTDECK.md",
                "/project/PILOTDECK.local.md",
                "/project/sub/PILOTDECK.md",
                "/project/sub/.pilotdeck/PILOTDECK.md",
                "/project/sub/PILOTDECK.local.md"
            ]
        );
        assert_eq!(layers[0].content, "managed");
        assert_eq!(layers[1].content, "a\u{fffd}b");
        assert!(
            !memory
                .reads
                .borrow()
                .iter()
                .any(|path| path.ends_with("link.md") || path.ends_with("SKILL.md"))
        );
    }
    #[test]
    fn lexical_dedup_absence_transport_failure_and_outside_chain_remain_distinct() {
        let mut memory = Memory::default();
        memory.files.insert(
            "/same/PILOTDECK.md".into(),
            "\u{85}kept\u{85}".as_bytes().to_vec(),
        );
        let discovery = Discovery::new(
            "/project",
            "/outside/deep",
            "/same/./",
            Some(Path::new("/same/a/..")),
        )
        .unwrap();
        assert_eq!(
            discovery.directory_chain(),
            vec![
                PathBuf::from("/project"),
                PathBuf::from("/outside"),
                PathBuf::from("/outside/deep")
            ]
        );
        let layers = discovery.discover(&memory).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].scope, Scope::Managed);
        assert_eq!(layers[0].content, "\u{85}kept\u{85}");
        assert_eq!(
            memory
                .reads
                .borrow()
                .iter()
                .filter(|path| path.as_path() == Path::new("/same/PILOTDECK.md"))
                .count(),
            1
        );
        assert!(Discovery::new("relative", "/cwd", "/user", None).is_err());
        assert!(
            discovery
                .discover(&Memory {
                    fail: true,
                    ..Default::default()
                })
                .is_err()
        );
        assert!(discovery.discover(&Memory::default()).unwrap().is_empty());
    }
    #[test]
    fn custom_prompt_still_receives_discovered_instructions_after_append_and_materializes_layers() {
        let fields=pilotdeck_protocol::parse_json(r#"{"cwd":"/project","provider":"p","model":"m","permissionMode":"default","messages":[],"tools":[],"customSystemPrompt":"CUSTOM","appendSystemPrompt":"APPEND"}"#).unwrap();
        let fields = fields.as_object().unwrap();
        let layers = vec![Layer {
            scope: Scope::Project,
            path: "/project/PILOTDECK.md".into(),
            content: "ACTUAL INSTRUCTION".into(),
        }];
        let result = crate::prepare::prepare_with_instructions(
            fields,
            crate::prepare::Environment {
                platform: "test",
                node_identity: "test",
                date: "2026-11-08",
            },
            Some(&layers),
        )
        .unwrap();
        let text = result
            .get("systemPrompt")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        assert!(text.starts_with("CUSTOM\n\n"));
        assert!(text.find("APPEND").unwrap() < text.find("<project-instructions>").unwrap());
        assert!(text.contains("Contents of /project/PILOTDECK.md (project instructions, checked into the codebase):\n\nACTUAL INSTRUCTION"));
        assert!(
            result
                .get("materialization")
                .unwrap()
                .get("instructionLayers")
                .is_some()
        );
    }
}

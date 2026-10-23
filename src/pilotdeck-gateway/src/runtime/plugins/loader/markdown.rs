//! PluginCommandLoader's deliberately small line-based frontmatter parser.
use super::{LoadedPluginCommand, sorted_entries};
use crate::{js_value::string_to_number, metadata::js_trim, runtime::plugins::PluginRuntimeError};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

pub(super) fn is_skill(path: &Path) -> bool {
    path.file_name().is_some_and(|n| {
        n.to_string_lossy()
            .replace('\u{212a}', "k")
            .replace('\u{17f}', "s")
            .eq_ignore_ascii_case("skill.md")
    })
}

pub fn get_plugin_command_name(plugin: &str, file: &Path, base: &Path) -> String {
    let dir = file.parent().unwrap_or(file);
    let skill = is_skill(file);
    let leaf = if skill {
        dir.file_name()
    } else {
        file.file_name()
    };
    let leaf = leaf.unwrap_or_default().to_string_lossy();
    let leaf = if !skill && leaf.to_ascii_lowercase().ends_with(".md") {
        &leaf[..leaf.len() - 3]
    } else {
        &leaf
    };
    let namespace_root = if skill && base != dir {
        dir.parent().unwrap_or(dir)
    } else {
        dir
    };
    let namespace = relative(base, namespace_root)
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(":");
    if namespace.is_empty() {
        format!("{plugin}:{leaf}")
    } else {
        format!("{plugin}:{namespace}:{leaf}")
    }
}

fn relative(base: &Path, target: &Path) -> String {
    let left: Vec<_> = base.components().collect();
    let right: Vec<_> = target.components().collect();
    let shared = left.iter().zip(&right).take_while(|(a, b)| a == b).count();
    let mut parts = vec!["..".to_owned(); left.len() - shared];
    parts.extend(
        right[shared..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

fn markdown_entries(directory: &Path) -> Vec<PathBuf> {
    // Like TS collectMarkdownFiles: unreadable directories contribute nothing.
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            return Vec::new();
        };
        names.push(entry.file_name());
    }
    names.sort();
    names
        .into_iter()
        .map(|name| {
            // Preserve TS's lexical contribution path, including a trailing slash.
            PathBuf::from(format!(
                "{}/{}",
                directory.display(),
                name.to_string_lossy()
            ))
        })
        .collect()
}

#[cfg(unix)]
fn directory_identity(metadata: &std::fs::Metadata, _canonical: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
fn directory_identity(_metadata: &std::fs::Metadata, canonical: &Path) -> PathBuf {
    canonical.to_path_buf()
}

#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
fn file_identity(_metadata: &std::fs::Metadata) -> () {
    ()
}

pub fn load_plugin_commands(
    plugin_name: &str,
    base: &Path,
) -> Result<Vec<LoadedPluginCommand>, PluginRuntimeError> {
    load_plugin_commands_with_root(plugin_name, base, base)
}

pub(super) fn load_plugin_commands_with_root(
    plugin_name: &str,
    base: &Path,
    plugin_root: &Path,
) -> Result<Vec<LoadedPluginCommand>, PluginRuntimeError> {
    let (Ok(root), Ok(canonical)) = (plugin_root.canonicalize(), base.canonicalize()) else {
        return Ok(Vec::new());
    };
    if !canonical.starts_with(&root) {
        return Ok(Vec::new());
    }
    let Ok(metadata) = std::fs::metadata(&canonical) else {
        return Ok(Vec::new());
    };
    let mut visited = HashSet::new();
    visited.insert(directory_identity(&metadata, &canonical));
    // Explicit DFS stack; each physical directory is expanded at most once.
    let mut pending: Vec<_> = markdown_entries(base).into_iter().rev().collect();
    let mut output = Vec::new();
    while let Some(path) = pending.pop() {
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        // Check files as well as directories, including multi-hop symlinks.
        if !canonical.starts_with(&root) {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&canonical) else {
            continue;
        };
        if metadata.is_dir() {
            // Global per traversal, not just the DFS ancestry: expanding each
            // inode once prevents factorial cliques and exponential alias DAGs.
            if !visited.insert(directory_identity(&metadata, &canonical)) {
                continue;
            }
            pending.extend(markdown_entries(&path).into_iter().rev());
        } else if path
            .file_name()
            .is_some_and(|s| s.to_string_lossy().to_ascii_lowercase().ends_with(".md"))
        {
            // The containment check above resolved `canonical`; read through a
            // re-verified handle so a leaf/ancestor replaced after that check
            // cannot redirect the read outside the plugin root (ROOT-001).
            let checked_identity = file_identity(&metadata);
            let Some(raw) = read_markdown(&root, &canonical, &canonical, checked_identity, false)?
            else {
                continue;
            };
            output.push(load_markdown_source(
                &raw,
                &path,
                get_plugin_command_name(plugin_name, &path, base),
            )?);
        }
    }
    Ok(output)
}

pub fn load_standalone_skill(
    name: &str,
    directory: &Path,
) -> Result<LoadedPluginCommand, PluginRuntimeError> {
    let file = sorted_entries(directory)
        .map_err(|e| super::io_error("scandir", directory, &e))?
        .into_iter()
        .find(|p| is_skill(p))
        .ok_or_else(|| {
            PluginRuntimeError::new(format!("Standalone skill '{name}' has no SKILL.md."))
        })?;
    // #594 / ROOT-001: read the single SKILL.md through the same re-verified
    // handle as plugin contributions, so a leaf swapped after discovery to an
    // out-of-root link, a different inode, or a special file cannot redirect the
    // read outside the skill directory. The lexical `file` is kept as the
    // command path, mirroring TS `join(skillDir, entry)`.
    load_markdown_source(&read_checked_skill(directory, &file)?, &file, name.into())
}

/// #594: resolve the discovered SKILL.md beneath the skill directory and read it
/// with `read_markdown` (shared with the plugin-command traversal). A standalone
/// skill is a single file, so there is nothing to skip past: a refused read
/// (out-of-root link, replaced inode, FIFO/socket) fails the whole skill, like
/// TS `loadStandaloneSkill` rejecting when `readFile` fails
/// (PluginCommandLoader.ts:37-42,:68). A concurrent deletion surfaces as the
/// real ENOENT from `read_markdown`, matching TS readFile.
#[cfg(unix)]
fn read_checked_skill(directory: &Path, file: &Path) -> Result<String, PluginRuntimeError> {
    let refused = || {
        super::io_error(
            "open",
            file,
            &std::io::Error::from_raw_os_error(libc::EACCES),
        )
    };
    // Resolve only for containment and the checked open. All diagnostics keep
    // the caller's lexical SKILL.md path, just like TS readFile.
    let error = |e| skill_io_error(file, e);
    let root = directory.canonicalize().map_err(error)?;
    let canonical = file.canonicalize().map_err(error)?;
    let metadata = std::fs::metadata(&canonical).map_err(error)?;
    // A SKILL.md that resolves to a directory (or a link to one) cannot leak
    // out-of-root bytes through a read, so keep TS readFile's EISDIR rather than
    // the generic refusal. FIFOs, sockets, raced-in links and replaced inodes
    // still take the refusal path through `read_markdown`.
    if metadata.is_dir() {
        return Err(super::io_error(
            "open",
            file,
            &std::io::Error::from_raw_os_error(libc::EISDIR),
        ));
    }
    read_markdown(&root, &canonical, file, file_identity(&metadata), true)?.ok_or_else(refused)
}

// Resolution failures are not all evidence of a refused link. In particular,
// a dangling link or concurrent deletion must retain readFile's ENOENT.
#[cfg(unix)]
fn skill_io_error(file: &Path, error: std::io::Error) -> PluginRuntimeError {
    let error = match error.kind() {
        std::io::ErrorKind::NotFound => error,
        _ if matches!(
            error.raw_os_error(),
            Some(libc::ELOOP | libc::EACCES | libc::EXDEV)
        ) =>
        {
            std::io::Error::from_raw_os_error(libc::EACCES)
        }
        _ => error,
    };
    super::io_error("open", file, &error)
}

/// Non-Unix keeps the pre-#564 path read: no openat + O_NOFOLLOW (unverified).
#[cfg(not(unix))]
fn read_checked_skill(_directory: &Path, file: &Path) -> Result<String, PluginRuntimeError> {
    super::read_text(file)
}

// Test-only race point between the pre-open lstat and the checked open (#564).
// Keyed by the canonical path it must fire on, so an earlier sibling read in
// the same traversal cannot consume it.
#[cfg(all(test, unix))]
type BeforeOpen = (PathBuf, Box<dyn FnOnce(&Path)>);
#[cfg(all(test, unix))]
thread_local!(static BEFORE_OPEN: std::cell::RefCell<Option<BeforeOpen>> = const { std::cell::RefCell::new(None) });
// Separate race window after the traversal's metadata identity was captured,
// before read_markdown observes the name with lstat. Keep BEFORE_OPEN for
// races after lstat so NOFOLLOW and the handle recheck stay independently tested.
#[cfg(all(test, unix))]
thread_local!(static BEFORE_LSTAT: std::cell::RefCell<Option<BeforeOpen>> = const { std::cell::RefCell::new(None) });

/// #564 / ROOT-001: never trust the checked path twice. Open it from `/` with
/// NOFOLLOW at every component (`project_files::open_beneath`, shared with the
/// #508 image sandbox), then require the handle to be the same regular file
/// (dev, ino) that was checked. A proven link or changed file identity is
/// skipped like any out-of-root entry; other open/read errors propagate like the
/// pre-#564 path read and TS `readFile` (PluginCommandLoader.ts:68).
#[cfg(unix)]
fn read_markdown(
    root: &Path,
    canonical: &Path,
    lexical: &Path,
    checked_identity: (u64, u64),
    require_single_link: bool,
) -> Result<Option<String>, PluginRuntimeError> {
    use std::{io::Read, os::unix::fs::MetadataExt};
    let error = |e: std::io::Error| super::io_error("open", lexical, &e);
    // Pre-open lstat: a FIFO/socket/device `.md` is skipped before any open.
    // If the checked entry no longer exists, propagate ENOENT like TS readFile
    // (PluginLoader.ts:93 `.catch(() => [])` fails the whole configured dir).
    // Other lstat errors also propagate: we only skip when it exists but isn't
    // a regular file (races are caught by identity recheck or NOFOLLOW open).
    #[cfg(test)]
    if let Some((_, hook)) = BEFORE_LSTAT.with(|h| {
        h.borrow_mut()
            .take_if(|(target, _)| target.as_path() == canonical)
    }) {
        hook(canonical);
    }
    let _before = match std::fs::symlink_metadata(canonical) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return Ok(None),       // exists but not a file: skip
        Err(e) => return Err(error(e)), // doesn't exist or other error: propagate
    };
    #[cfg(test)]
    if let Some((_, hook)) = BEFORE_OPEN.with(|h| {
        h.borrow_mut()
            .take_if(|(target, _)| target.as_path() == canonical)
    }) {
        hook(canonical);
    }
    let mut file = match crate::project_files::open_beneath(root, canonical) {
        Ok(file) => file,
        // ELOOP identifies a refused link. O_DIRECTORY can also report ENOTDIR
        // for either a link (Linux O_PATH) or an ordinary non-directory ancestor.
        // Skip ENOTDIR only if lstat now proves an ancestor is a symlink;
        // otherwise preserve the original error like TS readFile. This second
        // observation never opens or reads through the replacement.
        Err(e)
            if crate::project_files::nofollow_loop(&e)
                || (e.raw_os_error() == Some(libc::ENOTDIR) && has_symlink_ancestor(canonical)) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(error(e)),
    };
    let after = file.metadata().map_err(error)?;
    // A FIFO/device (opened NONBLOCK) or another inode renamed or hard-linked
    // over the checked name is not the checked regular file. Compare against
    // the identity captured at the root boundary check, not the pre-open lstat:
    // a hardlink raced in between the check and lstat has the same (dev, ino)
    // as the lstat, but differs from the original checked file.
    if !after.is_file() || (after.dev(), after.ino()) != checked_identity {
        return Ok(None);
    }
    // Hardlinks have no distinguished original name: canonicalization and a
    // directory-entry identity check cannot prove where the other names live.
    // Standalone skills conservatively refuse multiply linked inodes before
    // reading any bytes (including links whose other names are inside root).
    if require_single_link && after.nlink() > 1 {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(error)?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

// Inspect ancestors from / and stop at the first non-directory or failed
// lookup. Only an observed symlink is positive evidence; a file, deletion, or
// inaccessible path proves nothing. Further races can still change these names,
// but this check only classifies a failed open and never reads file contents.
#[cfg(unix)]
fn has_symlink_ancestor(path: &Path) -> bool {
    let ancestors: Vec<_> = path.ancestors().skip(1).collect();
    for ancestor in ancestors.into_iter().rev() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(metadata) => return metadata.file_type().is_symlink(),
            Err(_) => return false,
        }
    }
    false
}

/// Non-Unix keeps the pre-#564 path read: no openat + O_NOFOLLOW (unverified).
#[cfg(not(unix))]
fn read_markdown(
    _root: &Path,
    canonical: &Path,
    _lexical: &Path,
    _checked_identity: (),
    _require_single_link: bool,
) -> Result<Option<String>, PluginRuntimeError> {
    super::read_text(canonical).map(Some)
}

// Symlink/socket races and the openat path exist only on Unix.
#[cfg(all(test, unix))]
mod tests;

fn load_markdown_source(
    raw: &str,
    path: &Path,
    name: String,
) -> Result<LoadedPluginCommand, PluginRuntimeError> {
    let mut frontmatter = HashMap::new();
    let mut content = raw;
    if raw.starts_with("---\n")
        && let Some(end) = raw[4..].find("\n---\n").map(|i| i + 4)
    {
        for line in raw[4..end].split('\n') {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = js_trim(key);
            if key.is_empty() || key == "__proto__" {
                continue;
            }
            let value = js_trim(value);
            let number = string_to_number(value);
            let parsed = match value {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ if !value.is_empty() && number.is_finite() => serde_json::json!(number),
                _ => {
                    let value = value.strip_prefix(['\'', '"']).unwrap_or(value);
                    Value::String(value.strip_suffix(['\'', '"']).unwrap_or(value).into())
                }
            };
            frontmatter.insert(key.into(), parsed);
        }
        content = &raw[end + 5..];
    }
    Ok(LoadedPluginCommand {
        name,
        path: path.into(),
        content: content.into(),
        frontmatter,
        is_skill: is_skill(path),
    })
}

//! PluginCommandLoader's deliberately small line-based frontmatter parser.
use super::{LoadedPluginCommand, read_text, sorted_entries};
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
            let Some(raw) = read_markdown(&root, &canonical)? else {
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
    load_markdown(&file, name.into())
}

fn load_markdown(path: &Path, name: String) -> Result<LoadedPluginCommand, PluginRuntimeError> {
    load_markdown_source(&read_text(path)?, path, name)
}

// Test-only race point between the pre-open check and the checked open (#564).
#[cfg(test)]
type BeforeOpen = Box<dyn FnOnce(&Path)>;
#[cfg(test)]
thread_local!(static BEFORE_OPEN: std::cell::RefCell<Option<BeforeOpen>> = const { std::cell::RefCell::new(None) });

/// #564 / ROOT-001: never trust the checked path twice. Open it from `/` with
/// NOFOLLOW at every component (`project_files::open_beneath`, shared with the
/// #508 image sandbox), then require the handle to be the same regular file
/// (dev, ino) that was checked. A replacement after the check is skipped like
/// any out-of-root entry; I/O errors on the unchanged file still propagate.
#[cfg(unix)]
fn read_markdown(root: &Path, canonical: &Path) -> Result<Option<String>, PluginRuntimeError> {
    use std::{io::Read, os::unix::fs::MetadataExt};
    let error = |e: std::io::Error| super::io_error("open", canonical, &e);
    let Some(before) = std::fs::symlink_metadata(canonical)
        .ok()
        .filter(|m| m.is_file())
    else {
        return Ok(None);
    };
    #[cfg(test)]
    if let Some(hook) = BEFORE_OPEN.with(|h| h.borrow_mut().take()) {
        hook(canonical);
    }
    let mut file = match crate::project_files::open_beneath(root, canonical) {
        Ok(file) => file,
        // A raced-in link fails NOFOLLOW (ELOOP, or ENOTDIR for a Linux O_PATH
        // ancestor); only a path that still resolves to itself reports an error.
        Err(e) if canonical.canonicalize().is_ok_and(|now| now == canonical) => {
            return Err(error(e));
        }
        Err(_) => return Ok(None),
    };
    let after = file.metadata().map_err(error)?;
    if !after.is_file() || (before.dev(), before.ino()) != (after.dev(), after.ino()) {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(error)?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// Non-Unix keeps the pre-#564 path read: no openat + O_NOFOLLOW (unverified).
#[cfg(not(unix))]
fn read_markdown(_root: &Path, canonical: &Path) -> Result<Option<String>, PluginRuntimeError> {
    read_text(canonical).map(Some)
}

#[cfg(test)]
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

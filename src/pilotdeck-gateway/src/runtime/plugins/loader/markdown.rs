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

// Bound both directory enumeration allocation and traversal work. Limits apply
// to one configured contribution directory; exceeding either discards that
// directory through PluginLoader's existing error-to-empty boundary.
const MAX_MARKDOWN_ENTRIES: usize = 4096;
const MAX_MARKDOWN_DEPTH: usize = 64;

fn traversal_limit() -> PluginRuntimeError {
    PluginRuntimeError::new("Plugin markdown traversal limit exceeded.")
}

fn markdown_entries(
    directory: &Path,
    remaining: &mut usize,
) -> Result<Vec<PathBuf>, PluginRuntimeError> {
    // Like TS collectMarkdownFiles: unreadable directories contribute nothing.
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(Vec::new());
    };
    let mut names = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            return Ok(Vec::new());
        };
        if *remaining == 0 {
            return Err(traversal_limit());
        }
        *remaining -= 1;
        names.push(entry.file_name());
    }
    names.sort();
    Ok(names
        .into_iter()
        .map(|name| {
            // Preserve TS's lexical contribution path, including a trailing slash.
            PathBuf::from(format!(
                "{}/{}",
                directory.display(),
                name.to_string_lossy()
            ))
        })
        .collect())
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
    let mut remaining = MAX_MARKDOWN_ENTRIES;
    let mut pending: Vec<_> = markdown_entries(base, &mut remaining)?
        .into_iter()
        .rev()
        .map(|p| (p, 1))
        .collect();
    let mut output = Vec::new();
    while let Some((path, depth)) = pending.pop() {
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
            if depth > MAX_MARKDOWN_DEPTH {
                return Err(traversal_limit());
            }
            pending.extend(
                markdown_entries(&path, &mut remaining)?
                    .into_iter()
                    .rev()
                    .map(|p| (p, depth + 1)),
            );
        } else if path
            .file_name()
            .is_some_and(|s| s.to_string_lossy().to_ascii_lowercase().ends_with(".md"))
        {
            output.push(load_markdown_source(
                &canonical,
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
    load_markdown_source(path, path, name)
}

fn load_markdown_source(
    source: &Path,
    path: &Path,
    name: String,
) -> Result<LoadedPluginCommand, PluginRuntimeError> {
    let raw = read_text(source)?;
    let mut frontmatter = HashMap::new();
    let mut content = raw.as_str();
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

//! PluginCommandLoader's deliberately small line-based frontmatter parser.
use super::{LoadedPluginCommand, read_text, sorted_entries};
use crate::{js_value::string_to_number, metadata::js_trim, runtime::plugins::PluginRuntimeError};
use serde_json::Value;
use std::{collections::HashMap, path::Path};

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

fn markdown_entries(directory: &Path) -> Vec<std::path::PathBuf> {
    sorted_entries(directory)
        .unwrap_or_default()
        .into_iter()
        .map(|path| {
            // collectMarkdownFiles uses `${directory}/${entry}`, preserving a
            // configured trailing slash in the returned contribution path.
            std::path::PathBuf::from(format!(
                "{}/{}",
                directory.display(),
                path.file_name().unwrap().to_string_lossy()
            ))
        })
        .collect()
}

pub fn load_plugin_commands(
    plugin_name: &str,
    base: &Path,
) -> Result<Vec<LoadedPluginCommand>, PluginRuntimeError> {
    // Explicit DFS keeps the TS traversal order without growing the call stack.
    let mut pending = markdown_entries(base);
    pending.reverse();
    let mut output = Vec::new();
    while let Some(path) = pending.pop() {
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            let mut entries = markdown_entries(&path);
            entries.reverse();
            pending.extend(entries);
        } else if path
            .file_name()
            .is_some_and(|s| s.to_string_lossy().to_ascii_lowercase().ends_with(".md"))
        {
            output.push(load_markdown(
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
    let raw = read_text(path)?;
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

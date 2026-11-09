//! Bounded disk declaration loading. No programmatic plugin or module executes here.
use super::*;

pub(super) fn read_manifest(
    path: &Path,
    budget: &mut StyleBudget,
) -> Result<JsValue, PluginRuntimeError> {
    #[cfg(unix)]
    {
        // Follow a legitimate configured alias before opening its selected
        // physical file. NOFOLLOW/NONBLOCK still reject replacement races and
        // special files; the descriptor is the only source of read bytes.
        let target = path
            .join("plugin.json")
            .canonicalize()
            .map_err(|error| PluginRuntimeError::new(error.to_string()))?;
        let parent = target
            .parent()
            .ok_or_else(|| PluginRuntimeError::new("Invalid plugin path."))?;
        let mut file = crate::project_files::open_beneath(parent, &target).map_err(|error| {
            if crate::project_files::nofollow_loop(&error) {
                PluginRuntimeError::gateway("NATIVE_PLUGIN_UNSAFE", error.to_string())
            } else {
                PluginRuntimeError::new(error.to_string())
            }
        })?;
        let metadata = file
            .metadata()
            .map_err(|error| PluginRuntimeError::new(error.to_string()))?;
        if !metadata.is_file() {
            return Err(if metadata.is_dir() {
                PluginRuntimeError::new("Plugin manifest is not a regular file.")
            } else {
                PluginRuntimeError::gateway(
                    "NATIVE_PLUGIN_UNSAFE",
                    "Plugin manifest is not a regular file.",
                )
            });
        }
        let text = budget.read(&mut file)?;
        check_manifest_tokens(&text, budget)?;
        let value = parse_json(&text).map_err(|error| {
            PluginRuntimeError::new(json_syntax::parse_error(&text).unwrap_or(error.message))
        })?;
        Ok(value)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, budget);
        Err(PluginRuntimeError::new(
            "Bounded plugin loading requires Unix.",
        ))
    }
}

pub(super) fn fatal(error: &PluginRuntimeError) -> bool {
    matches!(
        error.code,
        Some("STYLE_RELOAD_LIMIT" | "NATIVE_PLUGIN_UNSAFE" | "NATIVE_PLUGIN_DEPTH")
    )
}

/// Count syntactic JSON values before materializing the JsValue tree. Keys do
/// not count; duplicate-key values do. This is a budget preflight, not a JSON
/// parser: the existing codec still validates syntax/strings and diagnostics.
pub(super) fn check_manifest_tokens(
    text: &str,
    budget: &mut StyleBudget,
) -> Result<(), PluginRuntimeError> {
    let bytes = text.as_bytes();
    let mut at = 0;
    let mut depth = 0usize;
    while at < bytes.len() {
        match bytes[at] {
            b'{' | b'[' => {
                budget.entry()?;
                depth += 1;
                if depth > crate::js_value::MAX_ARRAY_DEPTH {
                    return Err(PluginRuntimeError::gateway(
                        "NATIVE_PLUGIN_DEPTH",
                        "Maximum call stack size exceeded",
                    ));
                }
                at += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                at += 1;
            }
            b'"' => {
                at += 1;
                while at < bytes.len() {
                    match bytes[at] {
                        b'\\' => at = at.saturating_add(2),
                        b'"' => {
                            at += 1;
                            break;
                        }
                        _ => at += 1,
                    }
                }
                let mut next = at;
                while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                    next += 1;
                }
                if bytes.get(next) != Some(&b':') {
                    budget.entry()?;
                }
            }
            b'n' | b't' | b'f' | b'-' | b'0'..=b'9' => {
                budget.entry()?;
                at += 1;
                while at < bytes.len()
                    && !bytes[at].is_ascii_whitespace()
                    && !matches!(bytes[at], b',' | b':' | b'[' | b']' | b'{' | b'}' | b'"')
                {
                    at += 1;
                }
            }
            _ => at += 1,
        }
    }
    Ok(())
}

fn configured_markdown(
    path: &Path,
    name: &str,
    configured: &JsValue,
    fallback: &str,
    budget: &mut StyleBudget,
) -> Result<Vec<LoadedPluginCommand>, PluginRuntimeError> {
    let dirs = configured_markdown_directories(configured, fallback);
    let mut result = Vec::new();
    for dir in dirs {
        let base = config::join_path(path, &dir);
        match markdown::load_plugin_commands_bounded(name, &base, path, budget) {
            Ok(commands) => result.extend(commands),
            Err(error) if error.code == Some("STYLE_RELOAD_LIMIT") => return Err(error),
            // PluginLoader's configured Markdown catch preserves an empty
            // contribution for ordinary missing/unreadable/bad directories.
            Err(_) => {}
        }
    }
    if fallback == "skills" {
        result.retain(|command| command.is_skill);
    }
    Ok(result)
}

#[cfg(test)]
pub(super) fn load_plugin(
    path: &Path,
    source: PluginSource,
    budget: &mut StyleBudget,
) -> Result<LoadedPlugin, PluginRuntimeError> {
    let raw = read_manifest(path, budget)?;
    let parsed = parse_plugin_manifest(&raw)?;
    load_from_manifest(path, source, &parsed, budget)
}

pub(super) fn load_from_manifest(
    path: &Path,
    source: PluginSource,
    parsed: &JsValue,
    budget: &mut StyleBudget,
) -> Result<LoadedPlugin, PluginRuntimeError> {
    let manifest = parsed.as_object().expect("validated plugin manifest");
    let undefined = JsValue::Undefined;
    let get = |key| manifest.get_property(key).unwrap_or(&undefined);
    let namespace = basename(path);
    let commands = configured_markdown(path, &namespace, get("commands"), "commands", budget)?;
    let skills = configured_markdown(path, &namespace, get("skills"), "skills", budget)?;
    let output_styles = configured_markdown(
        path,
        &namespace,
        get("outputStyles"),
        "output-styles",
        budget,
    )?;
    let hooks_config = load_native_plugin_hooks_from_manifest(path, parsed, budget)?;
    let servers = |key| {
        if get(key).as_object().is_none() {
            Ok(None)
        } else {
            server_value::server_value(get(key)).map(Some)
        }
    };
    Ok(LoadedPlugin {
        name: string(get("name")).expect("validated plugin name"),
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

#[cfg(all(test, unix))]
mod tests;

//! Bounded disk declaration loading. No programmatic plugin or module executes here.
use super::*;

pub(super) fn read_manifest(
    path: &Path,
    budget: &mut StyleBudget,
) -> Result<JsValue, PluginRuntimeError> {
    #[cfg(unix)]
    {
        // Pin the discovery directory before opening the plugin/manifest
        // children, so replacing either child with a symlink cannot redirect I/O.
        let parent = path
            .parent()
            .ok_or_else(|| PluginRuntimeError::new("Invalid plugin path."))?
            .canonicalize()
            .map_err(|error| PluginRuntimeError::new(error.to_string()))?;
        let plugin = parent.join(
            path.file_name()
                .ok_or_else(|| PluginRuntimeError::new("Invalid plugin path."))?,
        );
        let mut file = crate::project_files::open_beneath(&parent, &plugin.join("plugin.json"))
            .map_err(|error| PluginRuntimeError::new(error.to_string()))?;
        if !file
            .metadata()
            .map_err(|error| PluginRuntimeError::new(error.to_string()))?
            .is_file()
        {
            return Err(PluginRuntimeError::new(
                "Plugin manifest is not a regular file.",
            ));
        }
        let text = budget.read(&mut file)?;
        let value = parse_json(&text).map_err(|error| {
            PluginRuntimeError::new(json_syntax::parse_error(&text).unwrap_or(error.message))
        })?;
        check_manifest_values(&value, budget)?;
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

fn check_manifest_values(
    value: &JsValue,
    budget: &mut StyleBudget,
) -> Result<(), PluginRuntimeError> {
    // Charge before enqueueing children: even an extremely wide object cannot
    // allocate an unbounded second traversal buffer before hitting the budget.
    budget.entry()?;
    let mut pending = vec![(value, 0usize)];
    while let Some((value, depth)) = pending.pop() {
        if matches!(value, JsValue::Array(_) | JsValue::Object(_))
            && depth >= crate::js_value::MAX_ARRAY_DEPTH
        {
            return Err(PluginRuntimeError::new("Maximum call stack size exceeded"));
        }
        match value {
            JsValue::Array(items) => {
                for child in items {
                    budget.entry()?;
                    pending.push((child, depth + 1));
                }
            }
            JsValue::Object(object) => {
                for (_, child) in object.entries() {
                    budget.entry()?;
                    pending.push((child, depth + 1));
                }
            }
            _ => {}
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

pub(super) fn load_plugin(
    path: &Path,
    source: PluginSource,
    budget: &mut StyleBudget,
) -> Result<LoadedPlugin, PluginRuntimeError> {
    let raw = read_manifest(path, budget)?;
    let parsed = parse_plugin_manifest(&raw)?;
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
    let hooks_config = load_native_plugin_hooks_from_manifest(path, &parsed, budget)?;
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

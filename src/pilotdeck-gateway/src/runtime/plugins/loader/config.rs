//! JSON manifest projection and local source validation from plugins/config.
use super::super::PluginRuntimeError;
use crate::metadata::js_trim;
use pilotdeck_protocol::{JsObject, JsValue};
use std::{
    path::{Component, Path, PathBuf},
    sync::LazyLock,
};

/// TS parsePluginManifest: only `name` is required; invalid optional fields
/// are omitted, rather than rejecting the plugin.
pub fn parse_plugin_manifest(raw: &JsValue) -> Result<JsValue, PluginRuntimeError> {
    let object = raw
        .as_object()
        .ok_or_else(|| PluginRuntimeError::new("Plugin manifest must be an object."))?;
    let undefined = JsValue::Undefined;
    let get = |key: &str| object.get_property(key).unwrap_or(&undefined);
    let name = get("name");
    if !name
        .as_string()
        .is_some_and(|s| !js_trim(&String::from_utf16_lossy(s.units())).is_empty())
    {
        return Err(PluginRuntimeError::new(
            "Plugin manifest must contain a name.",
        ));
    }
    let mut out = JsObject::new();
    out.insert("name".into(), name.clone());
    for key in ["version", "description"] {
        if get(key).as_string().is_some() {
            out.insert(key.into(), get(key).clone());
        }
    }
    for key in ["commands", "agents", "skills", "outputStyles"] {
        let value = get(key);
        if value.as_string().is_some()
            || matches!(value, JsValue::Array(a) if a.iter().all(|v| v.as_string().is_some()))
        {
            out.insert(key.into(), value.clone());
        }
    }
    let hooks = get("hooks");
    if hooks.as_object().is_some() || hooks.as_string().is_some() {
        out.insert("hooks".into(), hooks.clone());
    }
    for key in ["mcpServers", "lspServers", "settings"] {
        if get(key).as_object().is_some() {
            out.insert(key.into(), get(key).clone());
        }
    }
    if let Some(marketplace) = get("marketplace").as_object()
        && ["name", "plugin"].iter().all(|key| {
            marketplace
                .get_property(key)
                .and_then(JsValue::as_string)
                .is_some()
        })
    {
        let mut reference = JsObject::new();
        for key in ["name", "plugin", "version", "source", "url"] {
            if let Some(JsValue::String(value)) = marketplace.get_property(key)
                && (key != "source"
                    || ["marketplace", "git", "zip", "mcpb"]
                        .iter()
                        .any(|s| value.equals_str(s)))
            {
                reference.insert(key.into(), JsValue::String(value.clone()));
            }
        }
        out.insert("marketplace".into(), JsValue::Object(reference));
    }
    if let Some(value) = get("mcpb").as_string() {
        let text = String::from_utf16_lossy(value.units());
        if text.ends_with(".mcpb") || text.ends_with(".dxt") {
            out.insert("mcpb".into(), JsValue::String(value.clone()));
        }
    }
    Ok(JsValue::Object(out))
}

/// Lexical Node path.resolve (does not resolve symlinks).
pub(super) fn resolve_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .expect("current working directory")
            .join(path)
    };
    normalize_path(&absolute).components().collect()
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir if out.file_name().is_some_and(|n| n != "..") => {
                out.pop();
            }
            Component::ParentDir if path.is_absolute() => {}
            _ => out.push(part),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    if path.as_os_str().to_string_lossy().ends_with('/') {
        out.push("");
    }
    out
}

/// Node path.join keeps an absolute second operand beneath a nonempty first.
pub(super) fn join_path(base: &Path, part: &str) -> PathBuf {
    let joined = if base.as_os_str().is_empty() {
        PathBuf::from(part)
    } else if part.is_empty() {
        base.to_owned()
    } else {
        PathBuf::from(format!("{}/{}", base.display(), part))
    };
    normalize_path(&joined)
}

pub fn validate_plugin_source_path(plugin_path: &Path, allowed_root: &Path) -> bool {
    let plugin = resolve_path(plugin_path);
    let root = resolve_path(allowed_root);
    // TS explicitly appends '/', including for the filesystem root ('//').
    plugin == root
        || plugin
            .to_string_lossy()
            .starts_with(&format!("{}/", root.display()))
}

pub fn validate_marketplace_name(name: &str) -> Option<String> {
    if name.is_empty() || name.contains([' ', '/', '\\']) || name.contains("..") || name == "." {
        return Some("Marketplace name must be non-empty and must not contain spaces, path separators or traversal.".into());
    }
    let lower = name.to_ascii_lowercase();
    if lower == "inline" || lower == "builtin" {
        return Some(format!("Marketplace name {name} is reserved."));
    }
    static OFFICIAL: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?i)(?:official[^a-z0-9]*(pilotdeck|pilot)|(?:pilotdeck|pilot)[^a-z0-9]*official|^(?:pilotdeck|pilot)[^a-z0-9]*(marketplace|plugins|official))").unwrap()
    });
    if !name.bytes().all(|c| (0x20..=0x7e).contains(&c)) || OFFICIAL.is_match(name) {
        return Some("Marketplace name impersonates an official PilotDeck marketplace.".into());
    }
    None
}

pub fn resolve_marketplace_reference(reference: &JsValue) -> JsValue {
    let source = reference
        .as_object()
        .and_then(|o| o.get_property("source"))
        .and_then(JsValue::as_string);
    let deferred = source.filter(|s| ["git", "zip", "mcpb"].iter().any(|v| s.equals_str(v)));
    let mut out = JsObject::new();
    out.insert(
        "status".into(),
        if deferred.is_some() {
            "deferred"
        } else {
            "resolved"
        }
        .into(),
    );
    out.insert("reference".into(), reference.clone());
    if let Some(source) = deferred {
        out.insert(
            "reason".into(),
            format!(
                "{} installation is not implemented in the local runtime.",
                String::from_utf16_lossy(source.units())
            )
            .as_str()
            .into(),
        );
    }
    JsValue::Object(out)
}

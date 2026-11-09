//! Deployment-owned definitions, not a second SOP transition evaluator.
use super::common::*;
use crate::host::HostError;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};
static IO: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));
pub(crate) async fn load(path: PathBuf) -> Result<JsValue, HostError> {
    let permit = IO
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| error("SOP_DEFINITIONS_IO", "SOP definitions IO is unavailable."))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        read(&path)
    })
    .await
    .map_err(|e| error("SOP_DEFINITIONS_IO", e.to_string().as_str()))?
}
#[cfg(unix)]
fn read(path: &Path) -> Result<JsValue, HostError> {
    use std::{fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt};
    const LIMIT: u64 = 8 * 1024 * 1024;
    let label = path.to_string_lossy();
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(error(
                "SOP_DEFINITIONS_INVALID",
                format!("StaffDeck SOP definitions file does not exist: {label}").as_str(),
            ));
        }
        Err(e) => {
            return Err(error(
                "SOP_DEFINITIONS_IO",
                format!("Failed to read StaffDeck SOP definitions at {label}: {e}").as_str(),
            ));
        }
    };
    let metadata = file
        .metadata()
        .map_err(|e| error("SOP_DEFINITIONS_IO", e.to_string().as_str()))?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err(error(
            "SOP_DEFINITIONS_RESOURCE_LIMIT",
            "SOP definitions must be a regular file within the 8 MiB limit.",
        ));
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| error("SOP_DEFINITIONS_IO", e.to_string().as_str()))?;
    if bytes.len() as u64 > LIMIT {
        return Err(error(
            "SOP_DEFINITIONS_RESOURCE_LIMIT",
            "SOP definitions exceeded their 8 MiB limit.",
        ));
    }
    let doc = pilotdeck_yaml::parse_document(
        &String::from_utf8_lossy(&bytes).into_owned().into(),
        pilotdeck_yaml::Options {
            pretty_errors: false,
            ..Default::default()
        },
    );
    if !doc.errors.is_empty() {
        return Err(error(
            "SOP_DEFINITIONS_INVALID",
            format!(
                "Invalid StaffDeck SOP YAML at {label}: {}",
                doc.errors
                    .iter()
                    .map(|e| String::from_utf16_lossy(e.message.units()))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
            .as_str(),
        ));
    }
    let parsed = doc
        .to_json()
        .and_then(|g| g.to_js_value())
        .map_err(|e| error("SOP_DEFINITIONS_INVALID", e.message))?;
    normalize(parsed, &label)
}
#[cfg(not(unix))]
fn read(_: &Path) -> Result<JsValue, HostError> {
    Err(error(
        "SOP_DEFINITIONS_UNSUPPORTED",
        "Bounded SOP definitions loading requires Unix file descriptors.",
    ))
}
pub(crate) fn normalize(parsed: JsValue, path: &str) -> Result<JsValue, HostError> {
    let rows = match &parsed {
        JsValue::Array(a) => a.clone(),
        JsValue::Object(_) => {
            if let JsValue::Array(a) = field(&parsed, "sops") {
                a.clone()
            } else if let (Some(id), Some(version), Some(content)) = (
                text(field(&parsed, "skill_id")).or_else(|| text(field(&parsed, "id"))),
                text(field(&parsed, "version")),
                field(&parsed, "content").as_object(),
            ) {
                let mut row = JsObject::new();
                row.insert("id".into(), JsValue::String(id.clone()));
                row.insert("skill_id".into(), JsValue::String(id));
                row.insert("version".into(), JsValue::String(version));
                row.insert("content".into(), JsValue::Object(content.clone()));
                for k in ["name", "business_domain", "description"] {
                    if let Some(t) = text(field(&parsed, k)) {
                        row.insert(k.into(), JsValue::String(t));
                    }
                }
                vec![JsValue::Object(row)]
            } else {
                vec![]
            }
        }
        _ => vec![],
    };
    if rows.is_empty() {
        return Err(error(
            "SOP_DEFINITIONS_INVALID",
            format!("StaffDeck SOP definitions at {path} must contain a non-empty sops list.")
                .as_str(),
        ));
    }
    let mut ids = HashSet::new();
    for (i, row) in rows.iter().enumerate() {
        if row.as_object().is_none() {
            return Err(error(
                "SOP_DEFINITIONS_INVALID",
                format!("StaffDeck SOP definition {i} must be an object.").as_str(),
            ));
        }
        let id = id(row).ok_or_else(|| {
            error(
                "SOP_DEFINITIONS_INVALID",
                format!("StaffDeck SOP definition {i} must have an id.").as_str(),
            )
        })?;
        if !ids.insert(id.clone()) {
            return Err(error(
                "SOP_DEFINITIONS_INVALID",
                format!(
                    "StaffDeck SOP definitions contain duplicate id '{}'.",
                    String::from_utf16_lossy(id.units())
                )
                .as_str(),
            ));
        }
        for nodes in [field(row, "nodes"), field(field(row, "content"), "nodes")] {
            if let JsValue::Array(nodes) = nodes {
                for (index, node) in nodes.iter().enumerate() {
                    if node.as_object().is_none() {
                        continue;
                    }
                    let mode = field(node, "contextMode");
                    if !mode.is_undefined()
                        && !matches!(mode.as_string(),Some(s)if s.equals_str("new_session")||s.equals_str("inherit"))
                    {
                        return Err(error("SOP_DEFINITIONS_INVALID",format!("StaffDeck SOP definition '{}' node {index} has invalid contextMode '{}'.",String::from_utf16_lossy(id.units()),display(mode)).as_str()));
                    }
                    if !field(node, "model").is_undefined()
                        && parse_model(field(node, "model")).is_none()
                    {
                        return Err(error("SOP_DEFINITIONS_INVALID",format!("StaffDeck SOP definition '{}' node {index} has invalid model; expected 'provider/model'.",String::from_utf16_lossy(id.units())).as_str()));
                    }
                }
            }
        }
    }
    Ok(crate::metadata::object([("sops", JsValue::Array(rows))]))
}
pub(crate) fn id(row: &JsValue) -> Option<JsString> {
    text(field(row, "id")).or_else(|| text(field(row, "skill_id")))
}
pub(crate) fn contains(bundle: &JsValue, selected: &JsString) -> bool {
    matches!(field(bundle,"sops"),JsValue::Array(rows)if rows.iter().any(|r|id(r).as_ref()==Some(selected)))
}
pub(crate) fn parse_model(value: &JsValue) -> Option<JsObject> {
    let s = value.as_string()?;
    let units = s.units();
    if units
        .iter()
        .any(|u| char::from_u32(u32::from(*u)).is_some_and(crate::metadata::js_space))
    {
        return None;
    }
    let split = units.iter().position(|u| *u == 47)?;
    if split == 0 || split + 1 == units.len() || units[split + 1] == 47 {
        return None;
    }
    let mut o = JsObject::new();
    o.insert(
        "provider".into(),
        JsValue::String(JsString::from_utf16(units[..split].to_vec())),
    );
    o.insert(
        "model".into(),
        JsValue::String(JsString::from_utf16(units[split + 1..].to_vec())),
    );
    Some(o)
}
pub(super) fn node(bundle: &JsValue, sop: &JsString, node: &JsString) -> Option<JsValue> {
    let JsValue::Array(rows) = field(bundle, "sops") else {
        return None;
    };
    let row = rows.iter().find(|r| id(r).as_ref() == Some(sop))?;
    let content = if field(row, "content").as_object().is_some() {
        field(row, "content")
    } else {
        row
    };
    let JsValue::Array(nodes) = field(content, "nodes") else {
        return None;
    };
    nodes
        .iter()
        .find(|n| {
            n.as_object().is_some()
                && (field(n, "node_id").as_string() == Some(node)
                    || field(n, "nodeId").as_string() == Some(node))
        })
        .cloned()
}

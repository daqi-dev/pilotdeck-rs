//! The ledger seed projection preserves optional fields and duplicate paths.
//! The agent constructor's state projection has different normalization rules.
use super::{Result, invalid, ledger::truthy};
use pilotdeck_protocol::{JsObject, JsValue};

// TS serializeAgentLoopSeedStateProjection selects top-level fields, but does
// not normalize each Map value. Preserve that distinction for JSON.stringify
// identity: TS clone/restore parses the serialized projection. Rust retains the
// same wire/in-memory distinction but validates before append (timing: #201).
pub(super) fn serialize_seed(value: Option<&JsValue>) -> Result<Option<JsValue>> {
    let Some(value) = value.filter(|v| truthy(v)) else {
        return Ok(None);
    };
    let source = value
        .as_object()
        .ok_or_else(|| invalid("Invalid sidecar seedState: expected an object."))?;
    let mut result = JsObject::new();
    for key in ["allowedReadFiles", "readFileState", "writeSnapshots"] {
        if let Some(value) = source.get(key).filter(|v| truthy(v)) {
            result.insert(key.into(), value.clone());
        }
    }
    Ok(Some(JsValue::Object(result)))
}

pub(super) fn normalize_seed(value: Option<&JsValue>) -> Result<Option<JsValue>> {
    let Some(value) = value.filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined)) else {
        return Ok(None);
    };
    let source = value
        .as_object()
        .ok_or_else(|| invalid("Invalid sidecar seedState: expected an object."))?;
    let mut result = JsObject::new();
    if let Some(paths) = source.get("allowedReadFiles").filter(|v| !v.is_undefined()) {
        if !matches!(paths, JsValue::Array(items) if items.iter().all(|v| v.as_string().is_some()))
        {
            return Err(invalid("Invalid sidecar seedState.allowedReadFiles."));
        }
        result.insert("allowedReadFiles".into(), paths.clone());
    }
    for key in ["readFileState", "writeSnapshots"] {
        let Some(map) = source.get(key).filter(|v| !v.is_undefined()) else {
            continue;
        };
        let map = map
            .as_object()
            .ok_or_else(|| invalid(format!("Invalid sidecar seedState.{key}.")))?;
        let mut projected = JsObject::new();
        for (path, value) in map.entries() {
            let entry = value
                .as_object()
                .ok_or_else(|| invalid(format!("Invalid {key} entry.")))?;
            let mut fields = JsObject::new();
            let required: &[&str] = if key == "readFileState" {
                &["mtimeMs", "kind"]
            } else {
                &["absolutePath", "mtimeMs", "contentHash"]
            };
            for field in required {
                let value = entry
                    .get(field)
                    .ok_or_else(|| invalid(format!("Invalid {key} entry.")))?;
                let valid = match *field {
                    "mtimeMs" => value.as_number().is_some(),
                    "kind" => value.as_string().is_some_and(|s| {
                        ["text", "image", "pdf", "notebook"]
                            .iter()
                            .any(|k| s.equals_str(k))
                    }),
                    _ => value.as_string().is_some(),
                };
                if !valid {
                    return Err(invalid(format!("Invalid {key} entry.")));
                }
                fields.insert((*field).into(), value.clone());
            }
            for field in ["offset", "limit", "pages"] {
                if field == "pages" && key != "readFileState" {
                    continue;
                }
                if let Some(value) = entry.get(field)
                    && if field == "pages" {
                        value.as_string().is_some()
                    } else {
                        value.as_number().is_some()
                    }
                {
                    fields.insert(field.into(), value.clone());
                }
            }
            projected.insert(path.clone(), JsValue::Object(fields));
        }
        result.insert(key.into(), JsValue::Object(projected));
    }
    Ok(Some(JsValue::Object(result)))
}

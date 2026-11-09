//! TS `sopCapabilityAuthority.ts` validation and `SopAgentLoop.ts:744-746` stale checks.
use pilotdeck_protocol::{JsObject, JsString, JsValue};

const OPTIONAL_KNOWLEDGE_KEYS: &[&str] = &[
    "query",
    "knowledgeBaseIds",
    "knowledgeBaseVersionIds",
    "documentIds",
    "maxChunks",
];

pub fn validate_optional_knowledge_input(input: &JsValue) -> Result<(), JsString> {
    let Some(value) = input.as_object() else {
        return Err(JsString::from("SOP_AUTHORITY_INPUT_INVALID"));
    };
    for (key, _) in value.entries() {
        if !OPTIONAL_KNOWLEDGE_KEYS
            .iter()
            .any(|allowed| key.to_utf8().ok().is_some_and(|k| k == *allowed))
        {
            return Err(JsString::from("SOP_AUTHORITY_INPUT_INVALID"));
        }
    }
    let query = value.get("query").and_then(JsValue::as_string);
    if query.is_none_or(|q| q.to_utf8().map(|s| s.trim().is_empty()).unwrap_or(true)) {
        return Err(JsString::from("SOP_AUTHORITY_INPUT_INVALID"));
    }
    for key in ["knowledgeBaseIds", "knowledgeBaseVersionIds", "documentIds"] {
        if let Some(ids) = value.get(key) {
            let valid = match ids {
                JsValue::Array(items) => items.iter().all(|id| {
                    id.as_string()
                        .is_some_and(|s| s.to_utf8().map(|t| !t.trim().is_empty()).unwrap_or(false))
                }),
                _ => false,
            };
            if !valid {
                return Err(JsString::from("SOP_AUTHORITY_INPUT_INVALID"));
            }
        }
    }
    if let Some(max) = value.get("maxChunks").and_then(JsValue::as_number) {
        if max.fract() != 0.0 || max < 1.0 || max > 12.0 {
            return Err(JsString::from("SOP_AUTHORITY_INPUT_INVALID"));
        }
    }
    Ok(())
}

pub fn validate_sop_authority_projection(
    value: &JsValue,
    expected_context: &JsObject,
) -> Result<JsObject, JsString> {
    let Some(projection) = value.as_object().cloned() else {
        return Err(JsString::from("SOP_AUTHORITY_RESPONSE_INVALID"));
    };
    let context = projection
        .get("context")
        .and_then(JsValue::as_object)
        .ok_or_else(|| JsString::from("SOP_AUTHORITY_RESPONSE_INVALID"))?;
    if !context_matches(context, expected_context) {
        return Err(JsString::from("SOP_AUTHORITY_RESPONSE_INVALID"));
    }
    for key in ["sopId", "sopVersion", "nodeId", "snapshotId"] {
        if !nonempty_string(projection.get(key)) {
            return Err(JsString::from("SOP_AUTHORITY_RESPONSE_INVALID"));
        }
    }
    let generation = projection
        .get("registryGeneration")
        .and_then(JsValue::as_number)
        .filter(|v| v.fract() == 0.0 && *v >= 0.0)
        .ok_or_else(|| JsString::from("SOP_AUTHORITY_RESPONSE_INVALID"))?;
    let _ = generation;
    validate_optional_capabilities(projection.get("optionalCapabilities"))?;
    Ok(projection)
}

pub fn validate_projection_for_execute(projection: &JsObject) -> Result<(), JsString> {
    let context = projection
        .get("context")
        .and_then(JsValue::as_object)
        .ok_or_else(|| JsString::from("SOP_AUTHORITY_RESPONSE_INVALID"))?;
    for key in ["sessionKey", "projectKey", "requestId"] {
        if !nonempty_string(context.get(key)) {
            return Err(JsString::from("SOP_AUTHORITY_RESPONSE_INVALID"));
        }
    }
    if context.get("expectedRevision").and_then(JsValue::as_number).is_none() {
        return Err(JsString::from("SOP_AUTHORITY_RESPONSE_INVALID"));
    }
    for key in ["sopId", "sopVersion", "nodeId", "snapshotId"] {
        if !nonempty_string(projection.get(key)) {
            return Err(JsString::from("SOP_AUTHORITY_RESPONSE_INVALID"));
        }
    }
    projection
        .get("registryGeneration")
        .and_then(JsValue::as_number)
        .filter(|v| v.fract() == 0.0 && *v >= 0.0)
        .ok_or_else(|| JsString::from("SOP_AUTHORITY_RESPONSE_INVALID"))?;
    validate_optional_capabilities(projection.get("optionalCapabilities"))?;
    Ok(())
}

/// TS `SopAgentLoop.ts:744-746` stale projection check at executeAll.
pub fn projection_stale_vs_resolve(stored: &JsObject, fresh: &JsObject) -> bool {
    for key in [
        "snapshotId",
        "sopId",
        "sopVersion",
        "nodeId",
        "registryGeneration",
    ] {
        if stored.get(key) != fresh.get(key) {
            return true;
        }
    }
    !fresh
        .get("optionalCapabilities")
        .and_then(|v| match v {
            JsValue::Array(items) => Some(!items.is_empty()),
            _ => None,
        })
        .unwrap_or(false)
}

pub fn validate_pin_matches_step(
    projection: &JsObject,
    skill_id: &JsValue,
    sop_version: &JsValue,
    node_id: &JsValue,
) -> Result<(), JsString> {
    if projection.get("sopId") != Some(skill_id)
        || projection.get("sopVersion") != Some(sop_version)
        || projection.get("nodeId") != Some(node_id)
    {
        return Err(JsString::from("SOP_AUTHORITY_PIN_MISMATCH"));
    }
    Ok(())
}

fn validate_optional_capabilities(value: Option<&JsValue>) -> Result<(), JsString> {
    let Some(JsValue::Array(items)) = value else {
        return Err(JsString::from("SOP_AUTHORITY_STALE"));
    };
    if items.is_empty() {
        return Err(JsString::from("SOP_AUTHORITY_STALE"));
    }
    for cap in items {
        let Some(cap) = cap.as_object() else {
            return Err(JsString::from("SOP_AUTHORITY_RESPONSE_INVALID"));
        };
        if cap.get("operation").and_then(JsValue::as_string).is_none_or(|s| !s.equals_str("knowledge.search/v1"))
            || cap.get("resourceType").and_then(JsValue::as_string).is_none_or(|s| !s.equals_str("knowledge_base"))
            || !nonempty_string(cap.get("resourceId"))
            || cap.get("required") != Some(&JsValue::Bool(false))
            || cap.get("providerModuleId").and_then(JsValue::as_string).is_none_or(|s| !s.equals_str("knowledge.local"))
            || !nonempty_string(cap.get("providerVersion"))
            || cap.get("selectionMode").and_then(JsValue::as_string).is_none_or(|s| !s.equals_str("current"))
        {
            return Err(JsString::from("SOP_AUTHORITY_RESPONSE_INVALID"));
        }
    }
    Ok(())
}

fn context_matches(actual: &JsObject, expected: &JsObject) -> bool {
    for key in ["sessionKey", "projectKey", "expectedRevision", "requestId"] {
        if actual.get(key) != expected.get(key) {
            return false;
        }
    }
    true
}

fn nonempty_string(value: Option<&JsValue>) -> bool {
    value
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.to_utf8().map(|t| !t.trim().is_empty()).unwrap_or(false))
}

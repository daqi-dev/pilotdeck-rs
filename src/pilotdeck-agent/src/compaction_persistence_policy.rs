//! Pure fixed-Source compaction callback projection; no host calls or mutation.
//! Authority: b243 AgentLoop.ts 2662–2706 and 4032–4052.
use crate::compaction_persistence::{
    CompactionPersistenceCommit, CompactionPersistenceInput, CompactionPersistencePlan,
    CompactionSnapshotError,
};
use crate::ports::{CompactBoundary, PortError, PortResult};
use crate::tool_context::ContextMessageCollection;
use crate::{MessageProjection, ProjectedMessage, ProjectionGap};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue};

fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
fn source_error(message: String, compact: &JsValue) -> PortError {
    PortError::SourceException {
        message: message.into(),
        boundary: BoundarySnapshot::capture(compact),
    }
}
fn property(value: &JsValue, key: &str, compact: &JsValue) -> PortResult<JsValue> {
    match value {
        JsValue::Null | JsValue::Undefined => Err(source_error(
            format!(
                "Cannot read properties of {} (reading '{key}')",
                if matches!(value, JsValue::Null) {
                    "null"
                } else {
                    "undefined"
                }
            ),
            compact,
        )),
        // These field names are not indexed/string-length properties. Primitive
        // wrappers have no own values for any of the Source fields used here.
        _ => Ok(value.get(key).cloned().unwrap_or(JsValue::Undefined)),
    }
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Object(_) | JsValue::Array(_) => true,
    }
}
fn nullish(value: &JsValue) -> bool {
    matches!(value, JsValue::Undefined | JsValue::Null)
}
fn project(value: &JsValue) -> ProjectedMessage {
    ProjectedMessage {
        boundary: BoundarySnapshot::capture(value),
        normalized: MessageProjection::BoundaryOnly(vec![ProjectionGap::OtherBoundaryShape]),
    }
}
fn collection(value: &JsValue) -> ContextMessageCollection {
    match value {
        JsValue::Array(values) => {
            ContextMessageCollection::Items(values.iter().map(project).collect())
        }
        _ => ContextMessageCollection::BoundaryOnly(BoundarySnapshot::capture(value)),
    }
}
fn spread(value: &JsValue) -> JsObject {
    let mut out = JsObject::new();
    match value {
        JsValue::Object(value) => {
            // Object spread enumerates canonical integer indices first. The
            // insertion slot survives subsequent marker overwrite.
            let mut entries: Vec<_> = value.entries().iter().collect();
            entries.sort_by_key(|(key, _)| key.array_index().map_or((1, 0), |index| (0, index)));
            for (key, value) in entries {
                out.insert(key.clone(), value.clone());
            }
        }
        JsValue::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                out.insert(index.to_string().into(), value.clone());
            }
        }
        JsValue::String(value) => {
            // Spread boxes each UTF-16 code unit, including unpaired surrogates.
            for (index, unit) in value.units().iter().enumerate() {
                out.insert(
                    index.to_string().into(),
                    JsValue::String(JsString::from_utf16(vec![*unit])),
                );
            }
        }
        // Object spread skips nullish and primitive wrappers without enumerable
        // own fields; it does not coerce them to strings or reject them.
        _ => (),
    }
    out
}
fn marked_messages(
    messages: &JsValue,
    id: &JsValue,
    compact: &JsValue,
) -> PortResult<Vec<ProjectedMessage>> {
    let JsValue::Array(messages) = messages else {
        // Native custom callable .map is outside the immutable JSON profile.
        if nullish(messages) {
            property(messages, "map", compact)?;
        }
        return Err(source_error(
            "messages.map is not a function".into(),
            compact,
        ));
    };
    messages
        .iter()
        .map(|message| {
            // The spread runs before metadata access, but cannot throw for ordinary
            // JSON. Nullish message.metadata is the original failure point.
            let mut marked = spread(message);
            let metadata = property(message, "metadata", compact)?;
            let mut metadata = spread(&metadata);
            metadata.insert("compactReplacement".into(), JsValue::Bool(true));
            metadata.insert("compactSnapshotId".into(), id.clone());
            marked.insert("metadata".into(), JsValue::Object(metadata));
            Ok(project(&JsValue::Object(marked)))
        })
        .collect()
}

fn build_boundary(compact: &JsValue, result: &JsValue) -> PortResult<(CompactBoundary, JsValue)> {
    // Keep Source evaluation order: a null snapshot throws at postTokens before
    // target/summary evaluation or attempting messages.map.
    let mut metadata = JsObject::new();
    let id = property(result, "compactionId", compact)?;
    metadata.insert("compactionId".into(), id.clone());
    metadata.insert("trigger".into(), property(result, "trigger", compact)?);
    metadata.insert("preTokens".into(), property(result, "preTokens", compact)?);
    let snapshot = property(compact, "snapshot", compact)?;
    metadata.insert("postTokens".into(), property(&snapshot, "tokens", compact)?);
    metadata.insert(
        "messagesSummarized".into(),
        property(result, "messagesSummarized", compact)?,
    );
    let target = property(result, "targetPostTokens", compact)?;
    if !target.is_undefined() {
        metadata.insert("targetTokens".into(), target);
    }
    let summary = property(result, "summaryGenerated", compact)?;
    let summary = if nullish(&summary) {
        JsValue::Bool(!property(result, "summaryMessage", compact)?.is_undefined())
    } else {
        summary
    };
    metadata.insert("summaryGenerated".into(), summary);
    let merged = property(result, "checkpointMerged", compact)?;
    let merged = if nullish(&merged) {
        JsValue::Bool(matches!(
            property(result, "cacheReset", compact)?,
            JsValue::Bool(true)
        ))
    } else {
        merged
    };
    metadata.insert("checkpointMerged".into(), merged);
    metadata.insert("finalRatio".into(), property(&snapshot, "ratio", compact)?);
    let mut extra = JsObject::new();
    extra.insert("tier".into(), property(compact, "tier", compact)?);
    let succeeded = property(result, "error", compact)?.is_undefined()
        && !property(result, "summaryMessage", compact)?.is_undefined();
    extra.insert("summarySucceeded".into(), JsValue::Bool(succeeded));
    if truthy(&property(result, "cacheReset", compact)?) {
        extra.insert("cacheReset".into(), JsValue::Bool(true));
    }
    let error = property(compact, "error", compact)?;
    if truthy(&error) {
        extra.insert(
            "finalBudgetTokens".into(),
            property(&snapshot, "maxContextTokens", compact)?,
        );
        extra.insert(
            "finalUsedTokens".into(),
            property(&snapshot, "tokens", compact)?,
        );
        extra.insert(
            "finalBudgetRatio".into(),
            property(&snapshot, "ratio", compact)?,
        );
        extra.insert("error".into(), error);
    }
    metadata.insert("extra".into(), JsValue::Object(extra));
    let boundary = object([
        ("kind", "compact".into()),
        ("subtype", "compact_boundary".into()),
        ("compactMetadata", JsValue::Object(metadata)),
    ]);
    Ok((
        CompactBoundary {
            boundary: BoundarySnapshot::capture(&boundary),
        },
        id,
    ))
}

pub(crate) fn evaluate(
    input: CompactionPersistenceInput<'_>,
) -> PortResult<CompactionPersistencePlan> {
    let compact = input.compact.value();
    let messages = property(compact, "messages", compact)?;
    let replacement = collection(&messages);
    // Replacement installation precedes Source persistCompactSnapshot. Preserve
    // it even if later metadata/payload construction throws at distinct phases.
    let commit = if !input.persistence_advertised {
        Ok(None)
    } else {
        let result = property(compact, "result", compact)?;
        if !truthy(&result) {
            Ok(None)
        } else {
            match build_boundary(compact, &result) {
                Err(error) => Err(CompactionSnapshotError::BoundaryConstruction(error)),
                Ok((boundary, id)) => match marked_messages(&messages, &id, compact) {
                    Err(error) => Err(CompactionSnapshotError::PersistencePayload(error)),
                    Ok(messages) => Ok(Some(CompactionPersistenceCommit { boundary, messages })),
                },
            }
        }
    };
    Ok(CompactionPersistencePlan {
        replacement,
        commit,
    })
}

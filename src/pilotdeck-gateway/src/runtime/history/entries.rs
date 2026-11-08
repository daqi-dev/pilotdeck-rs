//! Projection of durable transcript entries in display order.
use super::project::{Context, ProjectionError, flatten_message_budgeted};
use pilotdeck_protocol::{JsString, JsValue};
fn field<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    static U: JsValue = JsValue::Undefined;
    v.get(k).unwrap_or(&U)
}
fn is(v: &JsValue, s: &str) -> bool {
    v.as_string().is_some_and(|value| value.equals_str(s))
}
fn visible(message: &JsValue) -> bool {
    let metadata = field(message, "metadata");
    !match field(metadata, "synthetic") {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.units().is_empty(),
        _ => true,
    }
        && field(metadata, "compactReplacement") != &JsValue::Bool(true)
}
fn non_text(message: &JsValue) -> Result<bool, ProjectionError> {
    let JsValue::Array(blocks) = field(message, "content") else {
        return Err(ProjectionError("History content must be an array".into()));
    };
    Ok(blocks.iter().any(|b| !is(field(b, "type"), "text")))
}
/// Build the display messages from the authoritative entries, including pre-compact records.
/// Unimplemented event families are explicit errors until their projection is added.
pub fn messages(
    entries: &[JsValue],
    session: &JsString,
    project: Option<&JsValue>,
    now: &JsValue,
) -> Result<Vec<JsValue>, ProjectionError> {
    messages_with_artifacts(entries, session, project, now, true)
}
pub(super) fn messages_with_artifacts(
    entries: &[JsValue],
    session: &JsString,
    project: Option<&JsValue>,
    now: &JsValue,
    include_artifacts: bool,
) -> Result<Vec<JsValue>, ProjectionError> {
    let mut out = Vec::new();
    let mut index = 0usize;
    let mut budget = super::budget::Budget::default();
    // WebHistory's durable-message reducer inspects content before visibility.
    for entry in entries {
        if [
            "assistant_message",
            "tool_result_message",
            "durable_message",
        ]
        .iter()
        .any(|t| is(field(entry, "type"), t))
        {
            match field(field(entry, "message"), "content") {
                JsValue::Null => {
                    return Err(ProjectionError(
                        "Cannot read properties of null (reading 'some')".into(),
                    ));
                }
                JsValue::Undefined => {
                    return Err(ProjectionError(
                        "Cannot read properties of undefined (reading 'some')".into(),
                    ));
                }
                JsValue::Array(_) => {}
                _ => {
                    return Err(ProjectionError(
                        "message.content.some is not a function".into(),
                    ));
                }
            }
        }
    }
    for entry in entries {
        let typ = field(entry, "type");
        let (records, fork) = if is(typ, "accepted_input") {
            let JsValue::Array(records) = field(entry, "messages") else {
                return Err(ProjectionError(
                    "History accepted_input messages must be an array".into(),
                ));
            };
            let fork = records
                .iter()
                .map(non_text)
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .any(|x| x);
            (records.iter().collect::<Vec<_>>(), fork)
        } else if [
            "assistant_message",
            "tool_result_message",
            "durable_message",
        ]
        .iter()
        .any(|t| is(typ, t))
        {
            (vec![field(entry, "message")], false)
        } else {
            continue;
        };
        for msg in records {
            if !visible(msg) {
                continue;
            }
            let created = entry
                .get("createdAt")
                .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                .unwrap_or(now);
            let ctx = Context {
                session_key: session,
                project_key: project,
                created_at: created,
                entry_id: entry.get("entryId"),
                turn_id: entry.get("turnId"),
                sequence: entry.get("sequence"),
                message_index: index,
                fork_unsupported: fork,
            };
            out.extend(flatten_message_budgeted(msg, &ctx, &mut budget)?);
            index += 1;
        }
    }
    super::events::inject(
        entries,
        &mut out,
        session,
        project,
        now,
        include_artifacts,
        &mut budget,
    )?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::{parse_json, stringify_json};
    #[test]
    fn ordinary_messages_match_real_ts_history() {
        let oracle = parse_json(include_str!(
            "../../../tests/fixtures/native-history/typical-r1.json"
        ))
        .unwrap();
        let Some(JsValue::Array(cases)) = oracle.get("cases") else {
            panic!("cases")
        };
        for id in [
            "text-basic",
            "legacy-text-merge",
            "block-id-timeline",
            "thinking-metadata",
            "tool-result",
            "incomplete-turn",
            "compact-boundary",
            "turn-error",
            "status-error-dedup",
            "file-artifacts",
            "subagent-recovery",
        ] {
            let case = cases.iter().find(|c| is(field(c, "id"), id)).unwrap();
            let JsValue::Array(entries) = field(case, "loadedEntries") else {
                panic!("entries")
            };
            let key: JsString = "history:fixture".into();
            let now: JsValue = "2026-01-02T03:04:05.678Z".into();
            let actual = JsValue::Array(messages(entries, &key, None, &now).unwrap());
            // stringify once to omit undefined object fields exactly as the WS does.
            let actual = parse_json(&stringify_json(&actual).unwrap().unwrap()).unwrap();
            assert_eq!(&actual, field(field(case, "result"), "messages"), "{id}");
        }
    }
}

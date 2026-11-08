//! Projection of durable transcript entries in display order.
use super::project::{Context, ProjectionError, flatten_message};
use pilotdeck_protocol::{JsString, JsValue};
fn field<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    static U: JsValue = JsValue::Undefined;
    v.get(k).unwrap_or(&U)
}
fn is(v: &JsValue, s: &str) -> bool {
    v.as_string().is_some_and(|s| s.equals_str(s))
}
fn visible(message: &JsValue) -> bool {
    let metadata = field(message, "metadata");
    field(metadata, "synthetic") != &JsValue::Bool(true)
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
    let mut out = Vec::new();
    let mut index = 0usize;
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
            if [
                "control_boundary",
                "agent_status_message",
                "file_artifacts",
                "subagent_started",
                "subagent_completed",
            ]
            .iter()
            .any(|t| is(typ, t))
            {
                return Err(ProjectionError(
                    "History transcript event requires additional projection".into(),
                ));
            }
            if is(typ, "turn_result") && is(field(field(entry, "result"), "type"), "error") {
                return Err(ProjectionError(
                    "History error result requires additional projection".into(),
                ));
            }
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
            out.extend(flatten_message(msg, &ctx)?);
            index += 1;
        }
    }
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

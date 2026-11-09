use super::project::ProjectionError;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
fn f<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    static U: JsValue = JsValue::Undefined;
    v.get(k).unwrap_or(&U)
}
fn is(v: &JsValue, s: &str) -> bool {
    v.as_string().is_some_and(|v| v.equals_str(s))
}
fn truth(v: &JsValue) -> bool {
    match v {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.units().is_empty(),
        _ => true,
    }
}
pub(super) fn inject(
    entries: &[JsValue],
    messages: &mut Vec<JsValue>,
    budget: &mut super::budget::Budget,
) -> Result<(), ProjectionError> {
    let references = crate::storage::project_subagent_references(entries)
        .map_err(|error| ProjectionError(error.to_string()))?;
    let mut attached: Vec<usize> = Vec::new();
    let mut queue = references.started.iter();
    for (index, message) in messages.iter_mut().enumerate() {
        let name = f(message, "toolName")
            .as_string()
            .and_then(|s| s.to_utf8().ok())
            .unwrap_or_default();
        if !is(f(message, "kind"), "tool_use")
            || !(name.eq_ignore_ascii_case("agent") || name.eq_ignore_ascii_case("task"))
        {
            continue;
        }
        let Some(start) = queue.next() else {
            break;
        };
        if let JsValue::Object(object) = message {
            budget.charge(f(start, "subagentId"))?;
            object.insert("subagentId".into(), f(start, "subagentId").clone());
            attached.push(index);
        }
    }
    for entry in &references.completed {
        if f(entry, "errored") == &JsValue::Bool(true) {
            continue;
        }
        let Some(&index) = attached
            .iter()
            .rev()
            .find(|&&index| f(&messages[index], "subagentId") == f(entry, "subagentId"))
        else {
            continue;
        };
        let tool = &messages[index];
        let call = f(tool, "toolCallId");
        if !truth(call) {
            continue;
        }
        let turn = if matches!(f(tool, "turnId"), JsValue::Null | JsValue::Undefined) {
            f(entry, "turnId")
        } else {
            f(tool, "turnId")
        };
        if messages.iter().any(|m| {
            is(f(m, "kind"), "tool_result") && f(m, "toolCallId") == call && f(m, "turnId") == turn
        }) {
            continue;
        }
        let Some(tool_id) = f(tool, "id").as_string() else {
            return Err(ProjectionError("Subagent tool id is not string".into()));
        };
        let mut id = tool_id.units().to_vec();
        id.extend("-subagent-result".encode_utf16());
        let mut o = JsObject::new();
        for (k, v) in [
            ("id", JsValue::String(JsString::from_utf16(id))),
            ("sessionKey", f(tool, "sessionKey").clone()),
            ("projectKey", f(tool, "projectKey").clone()),
            ("createdAt", f(entry, "createdAt").clone()),
            ("provider", "pilotdeck".into()),
            ("role", "tool".into()),
            ("kind", "tool_result".into()),
            ("turnId", turn.clone()),
            ("sequence", f(entry, "sequence").clone()),
            ("toolCallId", call.clone()),
            ("toolName", f(tool, "toolName").clone()),
            ("ok", JsValue::Bool(true)),
            ("text", f(entry, "summaryPreview").clone()),
            ("source", "history".into()),
        ] {
            o.insert(k.into(), v);
        }
        if truth(f(entry, "entryId")) {
            o.insert("entryId".into(), f(entry, "entryId").clone());
        }
        let inserted = super::events::insert_ordered(messages, JsValue::Object(o), budget)?;
        for index in &mut attached {
            if *index >= inserted {
                *index += 1;
            }
        }
    }
    Ok(())
}

//! Pure plan/todo checkpoint transitions. Caller supplies the observation time,
//! persists the returned checkpoint with CAS, and owns all authority.
use pilotdeck_protocol::{JsObject, JsString, JsValue};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanError(pub &'static str);
fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut o = JsObject::new();
    for (k, v) in fields {
        o.insert(k.into(), v);
    }
    JsValue::Object(o)
}
fn get<'a>(o: &'a JsObject, k: &str) -> Option<&'a JsValue> {
    o.get(k)
}
fn string<'a>(v: Option<&'a JsValue>) -> Result<&'a JsString, PlanError> {
    v.and_then(JsValue::as_string)
        .ok_or(PlanError("expected string"))
}
fn array(v: Option<&JsValue>) -> Result<&[JsValue], PlanError> {
    match v {
        Some(JsValue::Array(v)) => Ok(v),
        _ => Err(PlanError("expected array")),
    }
}
fn key(v: &JsValue) -> Result<JsString, PlanError> {
    string(
        v.get("id")
            .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
            .or_else(|| v.get("content")),
    )
    .cloned()
}
fn active(v: &JsValue) -> bool {
    v.get("status")
        .and_then(JsValue::as_string)
        .is_some_and(|v| v.equals_str("pending") || v.equals_str("in_progress"))
}
fn validate(v: &JsValue) -> Result<(), PlanError> {
    let o = v.as_object().ok_or(PlanError("todo must be object"))?;
    string(o.get("content"))?;
    let status = string(o.get("status"))?;
    if !["pending", "in_progress", "completed", "cancelled"]
        .iter()
        .any(|s| status.equals_str(s))
    {
        return Err(PlanError("invalid todo status"));
    }
    Ok(())
}
pub fn initial_checkpoint() -> JsValue {
    object([
        ("requiresInitialization", JsValue::Bool(true)),
        ("toolCallsSinceLastTodoWrite", JsValue::Number(0.0)),
        ("todos", JsValue::Array(vec![])),
        ("activeTodos", JsValue::Array(vec![])),
        ("todoHistory", JsValue::Array(vec![])),
        (
            "todoDiagnostics",
            object([
                ("writeCount", JsValue::Number(0.0)),
                ("todoCount", JsValue::Number(0.0)),
                ("activeCount", JsValue::Number(0.0)),
                ("completedCount", JsValue::Number(0.0)),
                ("cancelledCount", JsValue::Number(0.0)),
                ("largeRewriteCount", JsValue::Number(0.0)),
                ("deletedOpenItemCount", JsValue::Number(0.0)),
                ("completedWithoutActiveCount", JsValue::Number(0.0)),
            ]),
        ),
    ])
}
pub fn apply_checkpoint(
    checkpoint: &JsValue,
    mutation: &JsValue,
    observed_at: JsString,
) -> Result<JsValue, PlanError> {
    let mut state = checkpoint
        .as_object()
        .cloned()
        .ok_or(PlanError("checkpoint must be object"))?;
    let op = mutation
        .as_object()
        .ok_or(PlanError("mutation must be object"))?;
    let method = string(get(op, "method"))?;
    if method.equals_str("read") {
        return Ok(JsValue::Object(state));
    }
    if method.equals_str("mark_plan_approved") {
        state.insert(
            "approvedPlan".into(),
            JsValue::String(string(get(op, "plan"))?.clone()),
        );
        return Ok(JsValue::Object(state));
    }
    if method.equals_str("mark_tool_progress") {
        let name = string(get(op, "toolName"))?;
        if !["todo_write", "TodoWrite"]
            .iter()
            .any(|s| name.equals_str(s))
        {
            let count = state
                .get("toolCallsSinceLastTodoWrite")
                .and_then(JsValue::as_number)
                .ok_or(PlanError("invalid progress count"))?;
            state.insert(
                "toolCallsSinceLastTodoWrite".into(),
                JsValue::Number(count + 1.0),
            );
        }
        return Ok(JsValue::Object(state));
    }
    if !method.equals_str("write_todos") && !method.equals_str("record_todo_write") {
        return Err(PlanError("unknown plan/todo mutation"));
    }
    let updates = array(get(op, "todos"))?;
    let merge = get(op, "merge") == Some(&JsValue::Bool(true));
    let mut todos = if merge {
        array(state.get("todos"))?.to_vec()
    } else {
        vec![]
    };
    let previous = array(state.get("todos"))?.to_vec();
    for update in updates {
        let fields = update
            .as_object()
            .ok_or(PlanError("todo update must be object"))?;
        let id = key(update)?;
        if let Some(index) = todos.iter().position(|v| key(v).ok().as_ref() == Some(&id)) {
            let mut item = todos[index]
                .as_object()
                .cloned()
                .ok_or(PlanError("stored todo must be object"))?;
            for (k, v) in fields.entries() {
                if !v.is_undefined() {
                    item.insert(k.clone(), v.clone());
                }
            }
            let item = JsValue::Object(item);
            validate(&item)?;
            todos[index] = item;
        } else {
            validate(update)?;
            todos.push(update.clone());
        }
    }
    let mut history = array(state.get("todoHistory"))?.to_vec();
    let mut entry = JsObject::new();
    entry.insert("createdAt".into(), JsValue::String(observed_at));
    entry.insert(
        "mode".into(),
        if method.equals_str("record_todo_write") {
            "markdown".into()
        } else {
            "structured".into()
        },
    );
    entry.insert("merge".into(), JsValue::Bool(merge));
    entry.insert("todos".into(), JsValue::Array(todos.clone()));
    for k in ["reason", "markdown"] {
        if let Some(v) = get(op, k) {
            entry.insert(k.into(), v.clone());
        }
    }
    let deleted = previous
        .iter()
        .filter(|v| active(v) && !todos.iter().any(|next| key(next).ok() == key(v).ok()))
        .count();
    let active_count = todos.iter().filter(|v| active(v)).count();
    let count_status = |s: &str| {
        todos
            .iter()
            .filter(|v| {
                v.get("status")
                    .and_then(JsValue::as_string)
                    .is_some_and(|v| v.equals_str(s))
            })
            .count()
    };
    let mut diagnostics = state
        .get("todoDiagnostics")
        .and_then(JsValue::as_object)
        .cloned()
        .ok_or(PlanError("diagnostics must be object"))?;
    for (k, n) in [
        ("writeCount", history.len() + 1),
        ("todoCount", todos.len()),
        ("activeCount", active_count),
        ("completedCount", count_status("completed")),
        ("cancelledCount", count_status("cancelled")),
    ] {
        diagnostics.insert(k.into(), JsValue::Number(n as f64));
    }
    let prior = diagnostics
        .get("deletedOpenItemCount")
        .and_then(JsValue::as_number)
        .unwrap_or(0.0);
    diagnostics.insert(
        "deletedOpenItemCount".into(),
        JsValue::Number(prior + deleted as f64),
    );
    entry.insert("diagnostics".into(), JsValue::Object(diagnostics.clone()));
    history.push(JsValue::Object(entry));
    state.insert("requiresInitialization".into(), JsValue::Bool(false));
    state.insert("toolCallsSinceLastTodoWrite".into(), JsValue::Number(0.0));
    state.insert(
        "activeTodos".into(),
        JsValue::Array(todos.iter().filter(|v| active(v)).cloned().collect()),
    );
    state.insert("todos".into(), JsValue::Array(todos));
    state.insert("todoHistory".into(), JsValue::Array(history));
    state.insert("todoDiagnostics".into(), JsValue::Object(diagnostics));
    if let Some(v) = get(op, "markdown") {
        state.insert("lastMarkdown".into(), v.clone());
    }
    Ok(JsValue::Object(state))
}
#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    #[test]
    fn merge_preserves_old_fields_and_records_durable_history() {
        let first=parse_json(r#"{"method":"write_todos","todos":[{"id":"x","content":"ship","status":"pending","extension":null}]}"#).unwrap();
        let state = apply_checkpoint(&initial_checkpoint(), &first, "now".into()).unwrap();
        let update = parse_json(
            r#"{"method":"write_todos","merge":true,"todos":[{"id":"x","status":"completed"}]}"#,
        )
        .unwrap();
        let state = apply_checkpoint(&state, &update, "later".into()).unwrap();
        assert_eq!(array(state.get("todoHistory")).unwrap().len(), 2);
        let todo = &array(state.get("todos")).unwrap()[0];
        assert_eq!(todo.get("extension"), Some(&JsValue::Null));
        assert!(array(state.get("activeTodos")).unwrap().is_empty());
    }
    #[test]
    fn invalid_updates_never_mutate_original() {
        let state = initial_checkpoint();
        let before = state.clone();
        assert!(
            apply_checkpoint(
                &state,
                &parse_json(r#"{"method":"write_todos","todos":[{"content":"x","status":null}]}"#)
                    .unwrap(),
                "now".into()
            )
            .is_err()
        );
        assert_eq!(state, before);
    }
}

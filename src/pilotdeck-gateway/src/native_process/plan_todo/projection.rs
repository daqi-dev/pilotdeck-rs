//! Projection of durable plan/todo transcript events. No second state owner.
use crate::runtime::project::RuntimeError;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::{HashMap, HashSet};
fn object<const N: usize>(items: [(&str, JsValue); N]) -> JsObject {
    let mut o = JsObject::new();
    for (k, v) in items {
        o.insert(k.into(), v);
    }
    o
}
fn trim(s: &JsString) -> JsString {
    JsString::from_utf16(crate::runtime::memory::js_trim_units(s.units()).to_vec())
}
fn txt(v: Option<&JsValue>) -> Option<JsString> {
    v.and_then(JsValue::as_string)
        .map(trim)
        .filter(|s| !s.is_empty())
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Todo {
    pub id: JsString,
    pub content: JsString,
    pub status: JsString,
    pub priority: Option<JsString>,
}
impl Todo {
    pub(super) fn value(&self) -> JsValue {
        let mut o = object([
            ("id", JsValue::String(self.id.clone())),
            ("content", JsValue::String(self.content.clone())),
            ("status", JsValue::String(self.status.clone())),
        ]);
        if let Some(p) = &self.priority {
            o.insert("priority".into(), JsValue::String(p.clone()));
        }
        JsValue::Object(o)
    }
    fn active(&self) -> bool {
        self.status.equals_str("pending") || self.status.equals_str("in_progress")
    }
    fn normalize(v: &JsValue, index: usize) -> Self {
        let id = txt(v.get("id")).unwrap_or_else(|| format!("todo-{}", index + 1).into());
        let content = txt(v.get("content")).unwrap_or_else(|| "(no description)".into());
        let status = txt(v.get("status"))
            .filter(|s| {
                ["pending", "in_progress", "completed", "cancelled"]
                    .iter()
                    .any(|x| s.equals_str(x))
            })
            .unwrap_or_else(|| "pending".into());
        let priority = txt(v.get("priority"));
        Self {
            id,
            content,
            status,
            priority,
        }
    }
}
fn dedupe(input: &[JsValue]) -> Vec<&JsValue> {
    let mut last = HashMap::new();
    for (i, v) in input.iter().enumerate() {
        let id = txt(v.get("id")).unwrap_or_else(|| format!("todo-{}", i + 1).into());
        last.insert(id, i);
    }
    let mut indexes: Vec<usize> = last.into_values().collect();
    indexes.sort_unstable();
    indexes.into_iter().map(|i| &input[i]).collect()
}
pub(crate) fn replace(input: &[JsValue]) -> Vec<Todo> {
    dedupe(input)
        .into_iter()
        .enumerate()
        .map(|(i, v)| Todo::normalize(v, i))
        .collect()
}
pub(crate) fn merge(old: &[Todo], updates: &[JsValue]) -> Vec<Todo> {
    let mut by_id: HashMap<JsString, Todo> =
        old.iter().map(|v| (v.id.clone(), v.clone())).collect();
    let mut append = Vec::new();
    for update in dedupe(updates) {
        let id = txt(update.get("id"));
        if let Some(current) = id.as_ref().and_then(|id| by_id.get_mut(id)) {
            if let Some(v) = txt(update.get("content")) {
                current.content = v;
            }
            if let Some(v) = txt(update.get("status")).filter(|s| {
                ["pending", "in_progress", "completed", "cancelled"]
                    .iter()
                    .any(|x| s.equals_str(x))
            }) {
                current.status = v;
            }
            if let Some(v) = txt(update.get("priority")) {
                current.priority = Some(v);
            }
        } else {
            append.push(update)
        }
    }
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for old in old {
        let v = by_id.get(&old.id).cloned().unwrap_or_else(|| old.clone());
        if seen.insert(v.id.clone()) {
            result.push(v)
        }
    }
    let base = result.len();
    for (i, v) in append.into_iter().enumerate() {
        let v = Todo::normalize(v, base + i);
        if seen.insert(v.id.clone()) {
            result.push(v)
        }
    }
    result
}
#[derive(Clone, Default)]
pub(crate) struct Snapshot {
    plan: Option<JsString>,
    required: bool,
    progress: usize,
    todos: Vec<Todo>,
    history: Vec<JsValue>,
    last_markdown: Option<JsValue>,
    large: usize,
    deleted: usize,
    completed_without_active: usize,
    last_write: Option<JsValue>,
    budget: usize,
}
impl Snapshot {
    pub(crate) fn todos(&self) -> &[Todo] {
        &self.todos
    }
    pub(crate) fn should_record_progress(&self, name: &JsString) -> bool {
        self.plan.is_some() && !self.required && !name.equals_str("todo_write")
    }
    fn count(&self, status: &str) -> usize {
        self.todos
            .iter()
            .filter(|v| v.status.equals_str(status))
            .count()
    }
    fn diagnostics(&self) -> JsValue {
        let mut d = object([
            ("writeCount", JsValue::Number(self.history.len() as f64)),
            ("todoCount", JsValue::Number(self.todos.len() as f64)),
            (
                "activeCount",
                JsValue::Number(self.todos.iter().filter(|v| v.active()).count() as f64),
            ),
            (
                "completedCount",
                JsValue::Number(self.count("completed") as f64),
            ),
            (
                "cancelledCount",
                JsValue::Number(self.count("cancelled") as f64),
            ),
            ("largeRewriteCount", JsValue::Number(self.large as f64)),
            ("deletedOpenItemCount", JsValue::Number(self.deleted as f64)),
            (
                "completedWithoutActiveCount",
                JsValue::Number(self.completed_without_active as f64),
            ),
        ]);
        if let Some(v) = &self.last_write {
            d.insert("lastWrite".into(), v.clone());
        }
        JsValue::Object(d)
    }
    pub(crate) fn value(&self) -> JsValue {
        let mut o = JsObject::new();
        if !self.history.is_empty()
            && let Some(plan) = &self.plan
        {
            o.insert("approvedPlan".into(), JsValue::String(plan.clone()));
        }
        o.insert(
            "requiresInitialization".into(),
            JsValue::Bool(self.required),
        );
        o.insert(
            "toolCallsSinceLastTodoWrite".into(),
            JsValue::Number(self.progress as f64),
        );
        if let Some(v) = &self.last_markdown {
            o.insert("lastMarkdown".into(), v.clone());
        }
        o.insert(
            "todos".into(),
            JsValue::Array(self.todos.iter().map(Todo::value).collect()),
        );
        o.insert(
            "activeTodos".into(),
            JsValue::Array(
                self.todos
                    .iter()
                    .filter(|v| v.active())
                    .map(Todo::value)
                    .collect(),
            ),
        );
        o.insert("todoHistory".into(), JsValue::Array(self.history.clone()));
        o.insert("todoDiagnostics".into(), self.diagnostics());
        if self.history.is_empty()
            && let Some(plan) = &self.plan
        {
            o.insert("approvedPlan".into(), JsValue::String(plan.clone()));
        }
        JsValue::Object(o)
    }
    pub(crate) fn apply(&mut self, event: &JsValue) -> Result<(), RuntimeError> {
        let Some(kind) = event.get("type").and_then(JsValue::as_string) else {
            return Ok(());
        };
        if ![
            "plan_todo_plan_changed",
            "plan_todo_written",
            "plan_todo_progressed",
        ]
        .iter()
        .any(|value| kind.equals_str(value))
        {
            return Ok(());
        }
        let charge = if kind.equals_str("plan_todo_progressed") {
            0
        } else {
            measure(event)?.checked_mul(4).ok_or_else(limit)?
        };
        let total = if kind.equals_str("plan_todo_plan_changed") {
            charge
        } else {
            self.budget.checked_add(charge).ok_or_else(limit)?
        };
        if total > crate::native_ws::MAX_OUTPUT_PAYLOAD {
            return Err(limit());
        }
        self.budget = total;
        if kind.equals_str("plan_todo_plan_changed") {
            *self = Self::default();
            self.plan = txt(event.get("plan"));
            self.required = self.plan.is_some();
            self.budget = charge;
            return Ok(());
        }
        if kind.equals_str("plan_todo_progressed") {
            if self.plan.is_some()
                && !self.required
                && !event
                    .get("toolName")
                    .and_then(JsValue::as_string)
                    .is_some_and(|x| x.equals_str("todo_write"))
            {
                self.progress += 1;
            }
            return Ok(());
        }
        if !kind.equals_str("plan_todo_written") {
            return Ok(());
        }
        let Some(JsValue::Array(raw)) = event.get("todos") else {
            return Ok(());
        };
        let next: Vec<Todo> = raw
            .iter()
            .enumerate()
            .map(|(i, v)| Todo::normalize(v, i))
            .collect();
        let old_map: HashMap<_, _> = self.todos.iter().map(|v| (v.id.clone(), v)).collect();
        let new_ids: HashSet<_> = next.iter().map(|v| v.id.clone()).collect();
        let removed = self
            .todos
            .iter()
            .filter(|v| !new_ids.contains(&v.id))
            .count();
        let deleted = self
            .todos
            .iter()
            .filter(|v| !new_ids.contains(&v.id) && v.active())
            .count();
        let added = next.iter().filter(|v| !old_map.contains_key(&v.id)).count();
        let changed = next
            .iter()
            .filter(|v| old_map.get(&v.id).is_some_and(|old| *old != *v))
            .count();
        let preserved = next.len() - added;
        let large =
            !self.todos.is_empty() && !next.is_empty() && preserved < self.todos.len().div_ceil(2);
        let had_active = self.todos.iter().any(Todo::active);
        let all_complete = !next.is_empty()
            && next
                .iter()
                .all(|v| v.status.equals_str("completed") || v.status.equals_str("cancelled"));
        self.large += usize::from(large);
        self.deleted += deleted;
        self.completed_without_active += usize::from(all_complete && had_active);
        self.todos = next;
        self.required = false;
        self.progress = 0;
        self.last_markdown = event.get("markdown").cloned();
        let mut write = object([
            (
                "mode",
                event.get("mode").cloned().unwrap_or(JsValue::Undefined),
            ),
            (
                "merge",
                event.get("merge").cloned().unwrap_or(JsValue::Bool(false)),
            ),
        ]);
        if let Some(reason) = txt(event.get("reason")) {
            write.insert("reason".into(), JsValue::String(reason));
        }
        for (name, value) in [
            ("addedCount", JsValue::Number(added as f64)),
            ("removedCount", JsValue::Number(removed as f64)),
            ("changedCount", JsValue::Number(changed as f64)),
            ("deletedOpenItemCount", JsValue::Number(deleted as f64)),
            ("largeRewrite", JsValue::Bool(large)),
            ("allCompleted", JsValue::Bool(all_complete)),
        ] {
            write.insert(name.into(), value);
        }
        self.last_write = Some(JsValue::Object(write));
        let mut diagnostics = self.diagnostics();
        if let JsValue::Object(d) = &mut diagnostics {
            d.insert(
                "writeCount".into(),
                JsValue::Number((self.history.len() + 1) as f64),
            );
        }
        let mut item = object([
            (
                "createdAt",
                event
                    .get("createdAt")
                    .cloned()
                    .unwrap_or(JsValue::Undefined),
            ),
            (
                "mode",
                event.get("mode").cloned().unwrap_or(JsValue::Undefined),
            ),
            (
                "merge",
                event.get("merge").cloned().unwrap_or(JsValue::Bool(false)),
            ),
        ]);
        if let Some(reason) = txt(event.get("reason")) {
            item.insert("reason".into(), JsValue::String(reason));
        }
        if let Some(v) = self.last_markdown.clone() {
            item.insert("markdown".into(), v);
        }
        item.insert(
            "todos".into(),
            JsValue::Array(self.todos.iter().map(Todo::value).collect()),
        );
        item.insert("diagnostics".into(), diagnostics);
        self.history.push(JsValue::Object(item));
        Ok(())
    }
}

fn limit() -> RuntimeError {
    RuntimeError::new("Plan/todo projection exceeds Gateway resource limit.")
}
fn measure(value: &JsValue) -> Result<usize, RuntimeError> {
    let mut stack = vec![value];
    let mut bytes = 0usize;
    while let Some(value) = stack.pop() {
        bytes = bytes.checked_add(16).ok_or_else(limit)?;
        match value {
            JsValue::String(s) => {
                bytes = bytes
                    .checked_add(s.units().len().saturating_mul(6))
                    .ok_or_else(limit)?
            }
            JsValue::Array(items) => stack.extend(items),
            JsValue::Object(object) => {
                for (key, value) in object.entries() {
                    bytes = bytes
                        .checked_add(key.units().len().saturating_mul(6))
                        .ok_or_else(limit)?;
                    stack.push(value);
                }
            }
            _ => {}
        }
        if bytes > crate::native_ws::MAX_OUTPUT_PAYLOAD / 4 {
            return Err(limit());
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_ts_projection_and_merge_oracle() {
        let oracle = pilotdeck_protocol::parse_json(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/plan-todo/oracle.json"
        )))
        .unwrap();
        let Some(JsValue::Array(events)) = oracle.get("events") else {
            panic!("events")
        };
        let Some(JsValue::Array(rows)) = oracle.get("results") else {
            panic!("results")
        };
        assert_eq!(events.len(), 4);
        let mut state = Snapshot::default();
        for (index, event) in events.iter().enumerate() {
            if index == 2 {
                let updates = rows[7].get("input").unwrap().get("todos").unwrap();
                let JsValue::Array(updates) = updates else {
                    panic!("updates")
                };
                let merged = JsValue::Array(
                    merge(state.todos(), updates)
                        .iter()
                        .map(Todo::value)
                        .collect(),
                );
                assert_eq!(Some(&merged), rows[7].get("snapshot").unwrap().get("todos"));
            }
            state.apply(event).unwrap();
            let row = [4, 6, 7, 8][index];
            assert_eq!(
                &state.value(),
                rows[row].get("snapshot").unwrap(),
                "event {index}"
            );
        }
    }
    #[test]
    fn lone_surrogate_survives_normalization() {
        let input = pilotdeck_protocol::parse_json(
            r#"[{"id":"\ud800","content":"\ud800","status":"pending"}]"#,
        )
        .unwrap();
        let JsValue::Array(items) = &input else {
            panic!("array")
        };
        let todo = replace(&items);
        assert_eq!(todo[0].id.units(), &[0xd800]);
        assert_eq!(todo[0].content.units(), &[0xd800]);
    }
}

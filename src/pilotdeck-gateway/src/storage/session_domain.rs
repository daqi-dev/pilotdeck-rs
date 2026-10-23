//! SessionDomainValidation.ts state machine for both adapter and recorder.
use super::{
    ledger::field,
    recorder::{
        RecorderError, RecorderResult, call_id, compaction_subject, display, domain, safe_integer,
        same_id, truthy, validate_compaction_completed, validate_compaction_failed,
        validate_compaction_started,
    },
};
use pilotdeck_protocol::JsValue;
fn lossy(s: &pilotdeck_protocol::JsString) -> String {
    String::from_utf16_lossy(s.units())
}

/// TS SessionDomainValidation.ts. The shared writer publishes this candidate
/// only after durable append succeeds; restore validates the entire log first.
#[derive(Clone, Default)]
pub(super) struct DomainState {
    turns: Vec<(JsValue, DomainTurn)>,
    queue: Vec<(JsValue, JsValue)>, // insertion order is the FIFO contract
    used_items: Vec<JsValue>,
    used_turns: Vec<JsValue>,
    goal_revision: f64,
}
#[derive(Clone, Default)]
struct DomainTurn {
    closed: bool,
    last_step: u64,
    open_step: Option<u64>,
    context_recorded: bool,
    model_requested: bool,
    open_tool_calls: Vec<JsValue>,
    compactions: Vec<JsValue>,
    questions: Vec<JsValue>,
    permissions: Vec<JsValue>,
    inbox: Vec<(JsValue, &'static str)>,
}
fn contains(ids: &[JsValue], id: &JsValue) -> bool {
    ids.iter().any(|v| same_id(v, id))
}
fn remove(ids: &mut Vec<JsValue>, id: &JsValue) -> bool {
    if let Some(at) = ids.iter().position(|v| same_id(v, id)) {
        ids.remove(at);
        true
    } else {
        false
    }
}
fn text_is(value: &JsValue, text: &str) -> bool {
    value.as_string().is_some_and(|s| s.equals_str(text))
}
fn trimmed(value: &JsValue) -> bool {
    value
        .as_string()
        .is_some_and(|s| !super::transcript_adapter::js_trim_units(s.units()).is_empty())
}
fn nonblank(value: &JsValue) -> bool {
    trimmed(value)
}
fn length_zero(value: &JsValue) -> RecorderResult<bool> {
    call_id(value, "length")?;
    Ok(match value {
        JsValue::String(s) => s.is_empty(),
        JsValue::Array(v) => v.is_empty(),
        _ => field(value, "length").as_number() == Some(0.0),
    })
}
fn require_step(turn: &DomainTurn, entry: &JsValue, kind: &str) -> RecorderResult<u64> {
    if turn.open_step.is_none()
        || field(entry, "step").as_number() != turn.open_step.map(|n| n as f64)
    {
        return Err(domain(
            "step_not_open",
            format!(
                "{kind} requires open step {} in turn {}.",
                display(field(entry, "step"))?,
                display(field(entry, "turnId"))?
            ),
        ));
    }
    Ok(turn.open_step.unwrap())
}
impl DomainState {
    pub(super) fn next(&self, entry: &JsValue) -> RecorderResult<Self> {
        let kind = call_id(entry, "type")?
            .as_string()
            .map(lossy)
            .unwrap_or_default();
        let undefined = JsValue::Undefined;
        let id = field(entry, "turnId");
        let item = field(entry, "itemId");
        let mut next = self.clone();
        if kind == "agent_turn_enqueued" {
            let input = field(entry, "input");
            let valid_input = input.as_object().is_some()
                && ((text_is(field(input, "type"), "text")
                    && field(input, "text").as_string().is_some())
                    || (text_is(field(input, "type"), "blocks")
                        && matches!(field(input, "content"), JsValue::Array(_))));
            let subject = if truthy(item) {
                display(item)?
            } else {
                "(empty)".into()
            };
            if !truthy(item)
                || !truthy(id)
                || !valid_input
                || field(entry, "submitOptions").as_object().is_none()
            {
                return Err(domain(
                    "invalid_inbox_mutation",
                    format!("Queued turn {subject} has invalid input."),
                ));
            }
            if contains(&next.used_items, item) {
                return Err(domain(
                    "invalid_inbox_mutation",
                    format!("Queued turn item {} has already been used.", display(item)?),
                ));
            }
            if next.turns.iter().any(|(v, _)| same_id(v, id)) || contains(&next.used_turns, id) {
                return Err(domain(
                    "invalid_inbox_mutation",
                    format!("Queued turn id {} is already in use.", display(id)?),
                ));
            }
            next.queue.push((item.clone(), id.clone()));
            next.used_items.push(item.clone());
            next.used_turns.push(id.clone());
            return Ok(next);
        }
        if kind == "agent_turn_discarded" {
            let pending = next.queue.iter().position(|(i, _)| same_id(i, item));
            let queued_turn = pending.map(|at| &next.queue[at].1).unwrap_or(&undefined);
            if !truthy(item)
                || !(text_is(field(entry, "reason"), "cancelled")
                    || text_is(field(entry, "reason"), "agent_disposed"))
                || !same_id(queued_turn, id)
            {
                let subject = if truthy(item) {
                    display(item)?
                } else {
                    "(empty)".into()
                };
                return Err(domain(
                    "invalid_inbox_mutation",
                    format!("Queued turn {subject} is not pending."),
                ));
            }
            if let Some(at) = pending {
                next.queue.remove(at);
            }
            return Ok(next);
        }
        if matches!(
            kind.as_str(),
            "durable_message" | "assistant_message" | "tool_result_message"
        ) {
            let message = field(entry, "message");
            let metadata = call_id(message, "metadata")?;
            let claimed = field(metadata, "queueItemId");
            if claimed.as_string().is_some_and(|s| !s.is_empty())
                && let Some((_, turn)) = next.turns.iter_mut().find(|(v, _)| same_id(v, id))
            {
                let at = turn.inbox.iter().position(|(v, _)| same_id(v, claimed));
                if turn.closed || at.is_none_or(|at| turn.inbox[at].1 != "claimed") {
                    return Err(domain(
                        "invalid_inbox_mutation",
                        format!(
                            "Durable inbox message {} does not follow a claim in turn {}.",
                            display(claimed)?,
                            display(id)?
                        ),
                    ));
                }
                turn.inbox.remove(at.unwrap());
            }
            return Ok(next);
        }
        if !super::transcript_adapter::DOMAIN_TYPES.contains(&kind.as_str())
            && kind != "turn_result"
        {
            return Ok(next);
        }
        if kind == "turn_started" {
            if next.turns.iter().any(|(v, _)| same_id(v, id)) {
                return Err(domain(
                    "duplicate_turn_start",
                    format!("Turn {} has already started.", display(id)?),
                ));
            }
            let queued = next.queue.iter().position(|(_, v)| same_id(v, id));
            let claim = field(entry, "inboxItemId");
            if !claim.is_undefined() {
                let at = next.queue.iter().position(|(v, _)| same_id(v, claim));
                let queued_turn = at.map(|at| &next.queue[at].1).unwrap_or(&undefined);
                if !truthy(claim) || !same_id(queued_turn, id) {
                    let subject = if truthy(claim) {
                        display(claim)?
                    } else {
                        "(empty)".into()
                    };
                    return Err(domain(
                        "invalid_inbox_mutation",
                        format!("Turn {} cannot claim queued item {subject}.", display(id)?),
                    ));
                }
                let queued_item = queued.map(|at| &next.queue[at].0).unwrap_or(&undefined);
                if !same_id(queued_item, claim) {
                    return Err(domain(
                        "invalid_inbox_mutation",
                        format!(
                            "Turn {} does not own queued item {}.",
                            display(id)?,
                            display(claim)?
                        ),
                    ));
                }
                if at != Some(0) {
                    return Err(domain(
                        "invalid_inbox_mutation",
                        format!(
                            "Turn {} cannot bypass queued item {}.",
                            display(id)?,
                            display(&next.queue[0].0)?
                        ),
                    ));
                }
                next.queue.remove(at.unwrap());
            } else if let Some(at) = queued {
                return Err(domain(
                    "invalid_inbox_mutation",
                    format!(
                        "Turn {} must claim its queued item {}.",
                        display(id)?,
                        display(&next.queue[at].0)?
                    ),
                ));
            }
            next.turns.push((id.clone(), DomainTurn::default()));
            return Ok(next);
        }
        let Some(at) = next.turns.iter().position(|(v, _)| same_id(v, id)) else {
            if kind == "turn_result" {
                return Ok(next);
            }
            return Err(domain(
                "event_outside_turn",
                format!("{kind} requires an active turn {}.", display(id)?),
            ));
        };
        let turn = &mut next.turns[at].1;
        if turn.closed {
            return Err(domain(
                "event_outside_turn",
                format!("{kind} cannot follow the result for turn {}.", display(id)?),
            ));
        }
        match kind.as_str() {
            "compaction_started"
            | "compaction_completed"
            | "compaction_failed"
            | "question_started"
            | "question_completed"
            | "question_failed"
            | "permission_started"
            | "permission_completed"
            | "permission_failed" => {
                let (family, phase) = kind.split_once('_').unwrap();
                let operation = field(entry, "operationId");
                if family == "compaction" {
                    match phase {
                        "started" => validate_compaction_started(entry)?,
                        "completed" => validate_compaction_completed(entry)?,
                        _ => validate_compaction_failed(entry)?,
                    }
                } else {
                    let count = field(entry, "questionCount").as_number();
                    let step = field(entry, "step").as_number();
                    let mut invalid = !truthy(operation);
                    if family == "question" {
                        if phase != "failed" {
                            invalid |= !safe_integer(count) || count.is_some_and(|n| n <= 0.0);
                        }
                        if phase == "started" {
                            invalid |= !truthy(field(entry, "toolCallId"))
                                || !truthy(field(entry, "toolName"));
                        }
                    } else {
                        invalid |= !truthy(field(entry, "toolCallId"))
                            || !truthy(field(entry, "toolName"))
                            || !safe_integer(step)
                            || step.is_some_and(|n| n <= 0.0)
                            || !truthy(field(entry, "mode"));
                        if phase == "completed" {
                            invalid |= !truthy(field(entry, "decision"))
                                || !truthy(field(entry, "reason"));
                        }
                    }
                    if !invalid && phase == "failed" {
                        invalid = length_zero(field(entry, "error"))?;
                    }
                    if invalid {
                        let code = if family == "question" {
                            "invalid_question"
                        } else {
                            "invalid_permission"
                        };
                        let label = if family == "question" {
                            "Question"
                        } else {
                            "Permission"
                        };
                        let phase = match phase {
                            "started" => "start",
                            "completed" => "completion",
                            _ => "failure",
                        };
                        return Err(domain(
                            code,
                            format!(
                                "{label} {} has invalid {phase} metadata.",
                                compaction_subject(operation)?
                            ),
                        ));
                    }
                }
                if family == "permission" {
                    let step = require_step(turn, entry, "permission")?;
                    if !turn.model_requested {
                        return Err(domain(
                            "model_request_missing",
                            format!(
                                "Permission {} was recorded before the model request for step {step}.",
                                display(operation)?
                            ),
                        ));
                    }
                }
                let (ids, code, label) = match family {
                    "compaction" => (&mut turn.compactions, "invalid_compaction", "Compaction"),
                    "question" => (&mut turn.questions, "invalid_question", "Question"),
                    _ => (&mut turn.permissions, "invalid_permission", "Permission"),
                };
                if phase == "started" {
                    if contains(ids, operation) {
                        return Err(domain(
                            code,
                            format!("{label} {} already started.", display(operation)?),
                        ));
                    }
                    ids.push(operation.clone());
                } else if !remove(ids, operation) {
                    return Err(domain(
                        code,
                        format!(
                            "{label} {} has no open lifecycle bracket.",
                            display(operation)?
                        ),
                    ));
                }
            }
            "step_started" => {
                let step = field(entry, "step").as_number();
                if !safe_integer(step) || step.is_some_and(|n| n <= turn.last_step as f64) {
                    return Err(domain(
                        "invalid_step",
                        format!(
                            "Step {} must increase within turn {}.",
                            display(field(entry, "step"))?,
                            display(id)?
                        ),
                    ));
                }
                if let Some(open) = turn.open_step {
                    return Err(domain(
                        "step_already_open",
                        format!("Turn {} already has open step {open}.", display(id)?),
                    ));
                }
                turn.last_step = step.unwrap() as u64;
                turn.open_step = Some(turn.last_step);
                turn.context_recorded = false;
                turn.model_requested = false;
                turn.open_tool_calls.clear();
            }
            "context_snapshot" | "agent_instructions" | "model_request" | "model_stream_event"
            | "tool_call" | "tool_result" | "step_completed" => {
                let step = require_step(turn, entry, &kind)?;
                match kind.as_str() {
                    "context_snapshot" => {
                        if turn.context_recorded {
                            return Err(domain(
                                "duplicate_context_snapshot",
                                format!("Step {step} already has a context snapshot."),
                            ));
                        }
                        if turn.model_requested {
                            return Err(domain(
                                "context_after_model_request",
                                format!("Step {step} context must precede its model request."),
                            ));
                        }
                        turn.context_recorded = true;
                    }
                    "agent_instructions" if turn.model_requested => {
                        return Err(domain(
                            "instructions_after_model_request",
                            format!("Step {step} instructions must precede its model request."),
                        ));
                    }
                    "model_request" => {
                        if turn.model_requested {
                            return Err(domain(
                                "duplicate_model_request",
                                format!("Step {step} already has a model request."),
                            ));
                        }
                        turn.model_requested = true;
                    }
                    "model_stream_event" if !turn.model_requested => {
                        return Err(domain(
                            "model_request_missing",
                            format!("Step {step} emitted a model event before its request."),
                        ));
                    }
                    "tool_call" => {
                        let call = call_id(field(entry, "call"), "id")?;
                        if !turn.model_requested {
                            return Err(domain(
                                "model_request_missing",
                                format!(
                                    "Step {step} emitted tool call {} before its request.",
                                    display(call)?
                                ),
                            ));
                        }
                        if contains(&turn.open_tool_calls, call) {
                            return Err(domain(
                                "duplicate_tool_call",
                                format!("Tool call {} is already open.", display(call)?),
                            ));
                        }
                        turn.open_tool_calls.push(call.clone());
                    }
                    "tool_result" => {
                        let call = call_id(field(entry, "result"), "toolCallId")?;
                        if !remove(&mut turn.open_tool_calls, call) {
                            return Err(domain(
                                "tool_result_without_call",
                                format!(
                                    "Tool result {} has no open call in step {step}.",
                                    display(call)?
                                ),
                            ));
                        }
                    }
                    "step_completed" => {
                        if text_is(field(entry, "outcome"), "completed")
                            && !turn.open_tool_calls.is_empty()
                        {
                            return Err(domain(
                                "step_has_open_tool_calls",
                                format!("Step {step} completed with unresolved tool calls."),
                            ));
                        }
                        turn.open_step = None;
                    }
                    _ => {}
                }
            }
            "inbox_mutation" => {
                if !truthy(item) {
                    return Err(domain(
                        "invalid_inbox_mutation",
                        "Inbox mutation itemId must be non-empty.",
                    ));
                }
                let at = turn.inbox.iter().position(|(v, _)| same_id(v, item));
                let current = at.map(|at| turn.inbox[at].1);
                let mutation = field(entry, "mutation");
                let status = if text_is(mutation, "insert") {
                    if !truthy(field(entry, "message")) || current.is_some() {
                        return Err(domain(
                            "invalid_inbox_mutation",
                            format!(
                                "Inbox item {} cannot be inserted in its current state.",
                                display(item)?
                            ),
                        ));
                    }
                    Some("pending")
                } else if text_is(mutation, "cancel") {
                    if current == Some("claimed") {
                        return Err(domain(
                            "invalid_inbox_mutation",
                            format!("Claimed inbox item {} cannot be cancelled.", display(item)?),
                        ));
                    }
                    Some("cancelled")
                } else if text_is(mutation, "claim") {
                    if current != Some("pending") {
                        return Err(domain(
                            "invalid_inbox_mutation",
                            format!(
                                "Inbox item {} must be pending before claim.",
                                display(item)?
                            ),
                        ));
                    }
                    Some("claimed")
                } else if text_is(mutation, "discard") {
                    if !matches!(current, Some("pending" | "claimed")) {
                        return Err(domain(
                            "invalid_inbox_mutation",
                            format!(
                                "Inbox item {} cannot be discarded in its current state.",
                                display(item)?
                            ),
                        ));
                    }
                    turn.inbox.remove(at.unwrap());
                    None
                } else {
                    None
                };
                if let Some(status) = status {
                    if let Some(at) = at {
                        turn.inbox[at].1 = status;
                    } else {
                        turn.inbox.push((item.clone(), status));
                    }
                }
            }
            "plan_todo_plan_changed" => {
                let plan = field(entry, "plan");
                if !matches!(plan, JsValue::Null) && !nonblank(plan) {
                    return Err(domain(
                        "invalid_plan_todo",
                        "Plan/todo plan must be a non-empty string or null.",
                    ));
                }
            }
            "plan_todo_written" => validate_todos(entry)?,
            "plan_todo_progressed" => {
                let tool = field(entry, "toolName");
                if tool.as_string().is_none() {
                    call_id(tool, "trim")?;
                    return Err(RecorderError::Type(
                        "entry.toolName.trim is not a function".into(),
                    ));
                }
                if !trimmed(tool) {
                    return Err(domain(
                        "invalid_plan_todo",
                        "Plan/todo progress requires a tool name.",
                    ));
                }
            }
            "goal_changed" => validate_goal(&mut next.goal_revision, entry)?,
            "turn_result" => {
                if let Some(step) = turn.open_step {
                    return Err(domain(
                        "turn_has_open_step",
                        format!("Turn {} closed with step {step} open.", display(id)?),
                    ));
                }
                for (ids, code, label) in [
                    (
                        &turn.compactions,
                        "turn_has_pending_compaction",
                        "compaction",
                    ),
                    (&turn.questions, "turn_has_pending_question", "question"),
                    (
                        &turn.permissions,
                        "turn_has_pending_permission",
                        "permission",
                    ),
                ] {
                    if !ids.is_empty() {
                        let values = ids
                            .iter()
                            .map(display)
                            .collect::<RecorderResult<Vec<_>>>()?
                            .join(", ");
                        return Err(domain(
                            code,
                            format!(
                                "Turn {} closed with pending {label} {values}.",
                                display(id)?
                            ),
                        ));
                    }
                }
                if turn
                    .inbox
                    .iter()
                    .any(|(_, s)| matches!(*s, "pending" | "claimed"))
                {
                    return Err(domain(
                        "turn_has_pending_inbox",
                        format!("Turn {} closed with pending inbox items.", display(id)?),
                    ));
                }
                turn.closed = true;
            }
            _ => {}
        }
        Ok(next)
    }
}
fn validate_todos(entry: &JsValue) -> RecorderResult<()> {
    if !(text_is(field(entry, "mode"), "markdown") || text_is(field(entry, "mode"), "structured"))
        || !matches!(field(entry, "merge"), JsValue::Bool(_))
        || ["markdown", "reason"]
            .iter()
            .any(|k| !field(entry, k).is_undefined() && field(entry, k).as_string().is_none())
        || !matches!(field(entry, "todos"), JsValue::Array(_))
    {
        return Err(domain(
            "invalid_plan_todo",
            "Plan/todo write has invalid metadata.",
        ));
    }
    let JsValue::Array(todos) = field(entry, "todos") else {
        unreachable!()
    };
    let mut ids = Vec::new();
    for todo in todos {
        let id = call_id(todo, "id")?;
        if !nonblank(id)
            || !nonblank(field(todo, "content"))
            || !["pending", "in_progress", "completed", "cancelled"]
                .iter()
                .any(|s| text_is(field(todo, "status"), s))
            || (!field(todo, "priority").is_undefined()
                && field(todo, "priority").as_string().is_none())
            || contains(&ids, id)
        {
            return Err(domain(
                "invalid_plan_todo",
                "Plan/todo write contains an invalid todo item.",
            ));
        }
        ids.push(id.clone());
    }
    Ok(())
}
fn validate_goal(revision: &mut f64, entry: &JsValue) -> RecorderResult<()> {
    let number = field(entry, "revision").as_number();
    if !safe_integer(number) || number.is_some_and(|n| n < 1.0) {
        return Err(domain(
            "invalid_goal",
            "Goal revision must be a positive integer.",
        ));
    }
    if number != Some(*revision + 1.0) {
        return Err(domain(
            "invalid_goal",
            format!(
                "Goal revision must increase from {} to {}.",
                crate::js_value::number_string(*revision),
                crate::js_value::number_string(*revision + 1.0)
            ),
        ));
    }
    let goal = field(entry, "goal");
    if !matches!(goal, JsValue::Null) {
        let id = call_id(goal, "id")?;
        let max = field(goal, "maxGoalRounds");
        if !nonblank(id)
            || !nonblank(field(goal, "objective"))
            || field(goal, "revision").as_number() != number
            || !["active", "paused", "blocked", "complete"]
                .iter()
                .any(|s| text_is(field(goal, "phase"), s))
            || (!field(goal, "blockedReason").is_undefined()
                && field(goal, "blockedReason").as_string().is_none())
            || (!max.is_undefined()
                && (!safe_integer(max.as_number()) || max.as_number().is_some_and(|n| n < 1.0)))
        {
            return Err(domain("invalid_goal", "Goal snapshot is invalid."));
        }
        if text_is(field(goal, "phase"), "blocked") && !trimmed(field(goal, "blockedReason")) {
            return Err(domain("invalid_goal", "Blocked goals require a reason."));
        }
    }
    *revision = number.unwrap();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::{JsObject, stringify_json};
    fn outcome(result: RecorderResult<DomainState>) -> JsValue {
        let mut value = JsObject::new();
        match result {
            Ok(_) => {
                value.insert("ok".into(), JsValue::Bool(true));
            }
            Err(error) => {
                let mut e = JsObject::new();
                let (name, code, message) = match error {
                    RecorderError::Domain { code, message } => {
                        ("SessionDomainValidationError", Some(code), message)
                    }
                    RecorderError::Type(message) => ("TypeError", None, message),
                    RecorderError::Range(message) => ("RangeError", None, message),
                    other => panic!("unexpected error {other:?}"),
                };
                e.insert("name".into(), name.into());
                if let Some(code) = code {
                    e.insert("code".into(), code.into());
                }
                e.insert("message".into(), message.as_str().into());
                value.insert("error".into(), JsValue::Object(e));
            }
        }
        JsValue::Object(value)
    }
    #[test]
    fn real_ts_domain_logs_and_events_match_bytes() {
        let text = match std::env::var_os("PDRS_SESSION_DOMAIN_ORACLE") {
            Some(path) => std::fs::read_to_string(path).unwrap(),
            None => include_str!("../../tests/fixtures/session-domain/oracle.json").to_owned(),
        };
        let oracle = crate::test_baseline::oracle(&text);
        let JsValue::Array(cases) = field(&oracle, "scenarios") else {
            panic!("cases")
        };
        // Keep in sync with session-domain/generate.mts and its README.
        assert_eq!(cases.len(), 304, "domain oracle scenario count");
        for case in cases {
            let name = field(case, "name");
            let JsValue::Array(entries) = field(case, "entries") else {
                panic!("entries")
            };
            let JsValue::Array(expected) = field(case, "outcomes") else {
                panic!("outcomes")
            };
            assert_eq!(entries.len(), expected.len(), "{name:?}: outcome count");
            let mut state = DomainState::default();
            for (entry, expected) in entries.iter().zip(expected) {
                let next = state.next(entry);
                if let Ok(next) = &next {
                    state = next.clone();
                }
                assert_eq!(
                    stringify_json(&outcome(next)).unwrap(),
                    stringify_json(expected).unwrap(),
                    "{name:?}: {entry:?}"
                );
            }
            let log = entries
                .iter()
                .try_fold(DomainState::default(), |state, e| state.next(e));
            assert_eq!(
                stringify_json(&outcome(log)).unwrap(),
                stringify_json(field(case, "log")).unwrap(),
                "{name:?}: log"
            );
        }
    }
}

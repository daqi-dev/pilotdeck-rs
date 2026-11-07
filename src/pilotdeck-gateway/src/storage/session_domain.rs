//! SessionDomainValidation.ts state machine for both adapter and recorder.
use super::{
    ledger::field,
    recorder::{
        RecorderError, RecorderResult, call_id, compaction_subject, display, domain, safe_integer,
        same_id, truthy, validate_compaction_completed, validate_compaction_failed,
        validate_compaction_started,
    },
};
use pilotdeck_protocol::{JsString, JsValue};
use std::collections::{HashMap, HashSet};
fn lossy(s: &pilotdeck_protocol::JsString) -> String {
    String::from_utf16_lossy(s.units())
}

/// `same_id` keys: JS Set/Map SameValueZero over fresh JSON entries, so
/// objects and arrays never match a stored key and NaN never equals itself.
/// `TurnKey` projects a `JsValue` into exactly those equality classes —
/// undefined/null/bool/string/number, with `-0` folded onto `0` (f64 `==`
/// semantics). `None` is unindexable: the value can be stored but never
/// found, which is what `same_id` does for objects, arrays and NaN.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum TurnKey {
    Undefined,
    Null,
    Bool(bool),
    Number(u64),
    String(JsString),
}
fn key_of(value: &JsValue) -> Option<TurnKey> {
    match value {
        JsValue::Undefined => Some(TurnKey::Undefined),
        JsValue::Null => Some(TurnKey::Null),
        JsValue::Bool(b) => Some(TurnKey::Bool(*b)),
        JsValue::Number(n) if !n.is_nan() => {
            Some(TurnKey::Number(if *n == 0.0 { 0 } else { n.to_bits() }))
        }
        JsValue::String(s) => Some(TurnKey::String(s.clone())),
        _ => None,
    }
}
/// TS SessionDomainValidation.ts. The shared writer publishes this candidate
/// only after durable append succeeds; restore validates the entire log first.
///
/// #297 P2#2: the pre-fix `next` deep-cloned the whole state per event, and
/// turns are never removed, so every append paid for every turn the session
/// ever had, inside the commit mutex. The state now transitions in place
/// (`advance`) with an undo journal instead, and the turn/used sets are keyed
/// by `TurnKey`.
#[derive(Clone, Default)]
pub(super) struct DomainState {
    turns: Vec<(JsValue, DomainTurn)>,
    /// Points at the first slot whose key equals that `TurnKey` (the Vec
    /// `position`/`find` the pre-fix scans performed); unindexable keys are
    /// simply absent and therefore never found, like `same_id` never matches
    /// them.
    turn_index: HashMap<TurnKey, usize>,
    queue: Vec<(JsValue, JsValue)>, // insertion order is the FIFO contract
    used_items: HashSet<TurnKey>,
    used_turns: HashSet<TurnKey>,
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
/// Which per-turn lifecycle list an undo entry belongs to.
#[derive(Clone, Copy)]
pub(super) enum Lifecycle {
    Compaction,
    Question,
    Permission,
}
impl Lifecycle {
    fn vec_mut(self, turn: &mut DomainTurn) -> &mut Vec<JsValue> {
        match self {
            Lifecycle::Compaction => &mut turn.compactions,
            Lifecycle::Question => &mut turn.questions,
            Lifecycle::Permission => &mut turn.permissions,
        }
    }
}
/// One journaled state mutation; `rollback` replays the journal backwards.
#[derive(Clone)]
pub(super) enum Undo {
    QueuePush,
    QueueInsert {
        at: usize,
        pair: (JsValue, JsValue),
    },
    UsedItem(TurnKey),
    UsedTurn(TurnKey),
    TurnsPush,
    InboxRemove {
        turn: usize,
        at: usize,
        item: (JsValue, &'static str),
    },
    InboxStatus {
        turn: usize,
        at: usize,
        prev: &'static str,
    },
    InboxPush {
        turn: usize,
    },
    StepStarted {
        turn: usize,
        last_step: u64,
        open_step: Option<u64>,
        context_recorded: bool,
        model_requested: bool,
        open_tool_calls: Vec<JsValue>,
    },
    LifecyclePush {
        turn: usize,
        which: Lifecycle,
    },
    LifecycleRemove {
        turn: usize,
        which: Lifecycle,
        value: JsValue,
    },
    OpenCallPush {
        turn: usize,
    },
    OpenCallRemove {
        turn: usize,
        value: JsValue,
    },
    StepCompleted {
        turn: usize,
        prev: Option<u64>,
    },
    ContextFlag {
        turn: usize,
        prev: bool,
    },
    RequestFlag {
        turn: usize,
        prev: bool,
    },
    TurnClosed {
        turn: usize,
        prev: bool,
    },
    GoalRevision(f64),
}
fn contains(ids: &[JsValue], id: &JsValue) -> bool {
    ids.iter().any(|v| same_id(v, id))
}
fn text_is(value: &JsValue, text: &str) -> bool {
    value.as_string().is_some_and(|s| s.equals_str(text))
}
fn trimmed(value: &JsValue) -> bool {
    value
        .as_string()
        .is_some_and(|s| !crate::runtime::memory::js_trim_units(s.units()).is_empty())
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
        || (!crate::mutants::on("step-matching")
            && field(entry, "step").as_number() != turn.open_step.map(|n| n as f64))
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
    /// Oracle-harness fold entry: advance in place; an entry the validator
    /// rejects leaves the caller's state exactly where it was (the pre-fix
    /// implementation achieved the same by cloning and discarding).
    pub(super) fn next(&mut self, entry: &JsValue) -> RecorderResult<()> {
        let mut undo = Vec::new();
        let result = self.advance(entry, &mut undo);
        if result.is_err() {
            self.rollback(&mut undo);
        }
        result
    }
    /// The in-place transition every commit and restore fold runs. Statement
    /// order mirrors the pre-fix `next`: within each branch, every error
    /// return precedes that branch's first mutation, so the journal only has
    /// to replay when the *caller* refuses an already-validated entry — the
    /// store-append failure in `commit_with`.
    pub(super) fn advance(&mut self, entry: &JsValue, undo: &mut Vec<Undo>) -> RecorderResult<()> {
        let kind = (if crate::mutants::on("null-entry-property") {
            field(entry, "type")
        } else {
            call_id(entry, "type")?
        })
        .as_string()
        .map(lossy)
        .unwrap_or_default();
        let undefined = JsValue::Undefined;
        let id = field(entry, "turnId");
        let item = field(entry, "itemId");
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
            if !crate::mutants::on("queue-input")
                && (!truthy(item)
                    || !truthy(id)
                    || !valid_input
                    || field(entry, "submitOptions").as_object().is_none())
            {
                return Err(domain(
                    "invalid_inbox_mutation",
                    format!("Queued turn {subject} has invalid input."),
                ));
            }
            if !crate::mutants::on("queue-reused-item")
                && key_of(item).is_some_and(|key| self.used_items.contains(&key))
            {
                return Err(domain(
                    "invalid_inbox_mutation",
                    format!("Queued turn item {} has already been used.", display(item)?),
                ));
            }
            if !crate::mutants::on("queue-reused-turn")
                && (self.turn_slot(id).is_some()
                    || key_of(id).is_some_and(|key| self.used_turns.contains(&key)))
            {
                return Err(domain(
                    "invalid_inbox_mutation",
                    format!("Queued turn id {} is already in use.", display(id)?),
                ));
            }
            self.queue.push((item.clone(), id.clone()));
            undo.push(Undo::QueuePush);
            if let Some(key) = key_of(item)
                && self.used_items.insert(key.clone())
            {
                undo.push(Undo::UsedItem(key));
            }
            if let Some(key) = key_of(id)
                && self.used_turns.insert(key.clone())
            {
                undo.push(Undo::UsedTurn(key));
            }
            return Ok(());
        }
        if kind == "agent_turn_discarded" {
            let pending = self.queue.iter().position(|(i, _)| same_id(i, item));
            let queued_turn = pending.map(|at| &self.queue[at].1).unwrap_or(&undefined);
            let queued_mismatch = if crate::mutants::on("discard-undefined-lookup") {
                pending.is_none_or(|at| !same_id(&self.queue[at].1, id))
            } else {
                !same_id(queued_turn, id)
            };
            if !crate::mutants::on("queue-discard")
                && (!truthy(item)
                    || !(text_is(field(entry, "reason"), "cancelled")
                        || text_is(field(entry, "reason"), "agent_disposed"))
                    || queued_mismatch)
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
                let pair = self.queue.remove(at);
                undo.push(Undo::QueueInsert { at, pair });
            }
            return Ok(());
        }
        if matches!(
            kind.as_str(),
            "durable_message" | "assistant_message" | "tool_result_message"
        ) {
            let message = field(entry, "message");
            let metadata = call_id(message, "metadata")?;
            let claimed = field(metadata, "queueItemId");
            if claimed.as_string().is_some_and(|s| !s.is_empty())
                && let Some(turn_at) = self.turn_slot(id)
            {
                let turn = &mut self.turns[turn_at].1;
                let at = turn.inbox.iter().position(|(v, _)| same_id(v, claimed));
                if !crate::mutants::on("durable-inbox-claim")
                    && (turn.closed || at.is_none_or(|at| turn.inbox[at].1 != "claimed"))
                {
                    return Err(domain(
                        "invalid_inbox_mutation",
                        format!(
                            "Durable inbox message {} does not follow a claim in turn {}.",
                            display(claimed)?,
                            display(id)?
                        ),
                    ));
                }
                let removed = turn.inbox.remove(at.unwrap());
                undo.push(Undo::InboxRemove {
                    turn: turn_at,
                    at: at.unwrap(),
                    item: removed,
                });
            }
            return Ok(());
        }
        if !super::transcript_adapter::DOMAIN_TYPES.contains(&kind.as_str())
            && kind != "turn_result"
        {
            return Ok(());
        }
        if kind == "turn_started" {
            if !crate::mutants::on("duplicate-turn") && self.turn_slot(id).is_some() {
                return Err(domain(
                    "duplicate_turn_start",
                    format!("Turn {} has already started.", display(id)?),
                ));
            }
            let queued = self.queue.iter().position(|(_, v)| same_id(v, id));
            let claim = field(entry, "inboxItemId");
            let must_claim = if crate::mutants::on("queue-must-claim") {
                None
            } else {
                queued
            };
            if !claim.is_undefined() {
                let at = self.queue.iter().position(|(v, _)| same_id(v, claim));
                let queued_turn = at.map(|at| &self.queue[at].1).unwrap_or(&undefined);
                let claim_mismatch = if crate::mutants::on("claim-undefined-lookup") {
                    at.is_none_or(|at| !same_id(&self.queue[at].1, id))
                } else {
                    !same_id(queued_turn, id)
                };
                if !crate::mutants::on("queue-claim") && (!truthy(claim) || claim_mismatch) {
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
                let queued_item = queued.map(|at| &self.queue[at].0).unwrap_or(&undefined);
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
                if !crate::mutants::on("queue-fifo") && at != Some(0) {
                    return Err(domain(
                        "invalid_inbox_mutation",
                        format!(
                            "Turn {} cannot bypass queued item {}.",
                            display(id)?,
                            display(&self.queue[0].0)?
                        ),
                    ));
                }
                let pair = self.queue.remove(at.unwrap());
                undo.push(Undo::QueueInsert {
                    at: at.unwrap(),
                    pair,
                });
            } else if let Some(at) = must_claim {
                return Err(domain(
                    "invalid_inbox_mutation",
                    format!(
                        "Turn {} must claim its queued item {}.",
                        display(id)?,
                        display(&self.queue[at].0)?
                    ),
                ));
            }
            let slot = self.turns.len();
            self.turns.push((id.clone(), DomainTurn::default()));
            if let Some(key) = key_of(id) {
                self.turn_index.entry(key).or_insert(slot);
            }
            undo.push(Undo::TurnsPush);
            return Ok(());
        }
        let Some(at) = self.turn_slot(id) else {
            if kind == "turn_result" {
                return Ok(());
            }
            return Err(domain(
                "event_outside_turn",
                format!("{kind} requires an active turn {}.", display(id)?),
            ));
        };
        let turn = &mut self.turns[at].1;
        if !crate::mutants::on("closed-turn") && turn.closed {
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
                    if !crate::mutants::on("lifecycle-metadata") && invalid {
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
                    if !crate::mutants::on("permission-request") && !turn.model_requested {
                        return Err(domain(
                            "model_request_missing",
                            format!(
                                "Permission {} was recorded before the model request for step {step}.",
                                display(operation)?
                            ),
                        ));
                    }
                }
                let (which, code, label) = match family {
                    "compaction" => (Lifecycle::Compaction, "invalid_compaction", "Compaction"),
                    "question" => (Lifecycle::Question, "invalid_question", "Question"),
                    _ => (Lifecycle::Permission, "invalid_permission", "Permission"),
                };
                if phase == "started" {
                    if !crate::mutants::on("lifecycle-duplicate")
                        && contains(which.vec_mut(turn), operation)
                    {
                        return Err(domain(
                            code,
                            format!("{label} {} already started.", display(operation)?),
                        ));
                    }
                    which.vec_mut(turn).push(operation.clone());
                    undo.push(Undo::LifecyclePush { turn: at, which });
                } else if !crate::mutants::on("lifecycle-terminal") {
                    let Some(pos) = which
                        .vec_mut(turn)
                        .iter()
                        .position(|v| same_id(v, operation))
                    else {
                        return Err(domain(
                            code,
                            format!(
                                "{label} {} has no open lifecycle bracket.",
                                display(operation)?
                            ),
                        ));
                    };
                    let value = which.vec_mut(turn).remove(pos);
                    undo.push(Undo::LifecycleRemove {
                        turn: at,
                        which,
                        value,
                    });
                }
            }
            "step_started" => {
                let step = field(entry, "step").as_number();
                if !crate::mutants::on("step-increase")
                    && (!safe_integer(step) || step.is_some_and(|n| n <= turn.last_step as f64))
                {
                    return Err(domain(
                        "invalid_step",
                        format!(
                            "Step {} must increase within turn {}.",
                            display(field(entry, "step"))?,
                            display(id)?
                        ),
                    ));
                }
                let already_open = if crate::mutants::on("step-already-open") {
                    None
                } else {
                    turn.open_step
                };
                if let Some(open) = already_open {
                    return Err(domain(
                        "step_already_open",
                        format!("Turn {} already has open step {open}.", display(id)?),
                    ));
                }
                undo.push(Undo::StepStarted {
                    turn: at,
                    last_step: turn.last_step,
                    open_step: turn.open_step,
                    context_recorded: turn.context_recorded,
                    model_requested: turn.model_requested,
                    open_tool_calls: std::mem::take(&mut turn.open_tool_calls),
                });
                turn.last_step = step.unwrap() as u64;
                turn.open_step = Some(turn.last_step);
                turn.context_recorded = false;
                turn.model_requested = false;
            }
            "context_snapshot" | "agent_instructions" | "model_request" | "model_stream_event"
            | "tool_call" | "tool_result" | "step_completed" => {
                let step = require_step(turn, entry, &kind)?;
                match kind.as_str() {
                    "context_snapshot" => {
                        if !crate::mutants::on("context-duplicate") && turn.context_recorded {
                            return Err(domain(
                                "duplicate_context_snapshot",
                                format!("Step {step} already has a context snapshot."),
                            ));
                        }
                        if !crate::mutants::on("context-order") && turn.model_requested {
                            return Err(domain(
                                "context_after_model_request",
                                format!("Step {step} context must precede its model request."),
                            ));
                        }
                        undo.push(Undo::ContextFlag {
                            turn: at,
                            prev: turn.context_recorded,
                        });
                        turn.context_recorded = !crate::mutants::on("context-state");
                    }
                    "agent_instructions"
                        if !crate::mutants::on("instructions-order") && turn.model_requested =>
                    {
                        return Err(domain(
                            "instructions_after_model_request",
                            format!("Step {step} instructions must precede its model request."),
                        ));
                    }
                    "model_request" => {
                        if !crate::mutants::on("request-duplicate") && turn.model_requested {
                            return Err(domain(
                                "duplicate_model_request",
                                format!("Step {step} already has a model request."),
                            ));
                        }
                        undo.push(Undo::RequestFlag {
                            turn: at,
                            prev: turn.model_requested,
                        });
                        turn.model_requested = !crate::mutants::on("request-state");
                    }
                    "model_stream_event"
                        if !crate::mutants::on("model-event-order") && !turn.model_requested =>
                    {
                        return Err(domain(
                            "model_request_missing",
                            format!("Step {step} emitted a model event before its request."),
                        ));
                    }
                    "tool_call" => {
                        let call = call_id(field(entry, "call"), "id")?;
                        if !crate::mutants::on("tool-request") && !turn.model_requested {
                            return Err(domain(
                                "model_request_missing",
                                format!(
                                    "Step {step} emitted tool call {} before its request.",
                                    display(call)?
                                ),
                            ));
                        }
                        if !crate::mutants::on("tool-duplicate")
                            && contains(&turn.open_tool_calls, call)
                        {
                            return Err(domain(
                                "duplicate_tool_call",
                                format!("Tool call {} is already open.", display(call)?),
                            ));
                        }
                        turn.open_tool_calls.push(call.clone());
                        undo.push(Undo::OpenCallPush { turn: at });
                    }
                    "tool_result" => {
                        let call = call_id(field(entry, "result"), "toolCallId")?;
                        if !crate::mutants::on("tool-result-pair") {
                            let Some(pos) =
                                turn.open_tool_calls.iter().position(|v| same_id(v, call))
                            else {
                                return Err(domain(
                                    "tool_result_without_call",
                                    format!(
                                        "Tool result {} has no open call in step {step}.",
                                        display(call)?
                                    ),
                                ));
                            };
                            let value = turn.open_tool_calls.remove(pos);
                            undo.push(Undo::OpenCallRemove { turn: at, value });
                        }
                    }
                    "step_completed" => {
                        if !crate::mutants::on("open-tool-on-step-close")
                            && (text_is(field(entry, "outcome"), "completed")
                                && !turn.open_tool_calls.is_empty())
                        {
                            return Err(domain(
                                "step_has_open_tool_calls",
                                format!("Step {step} completed with unresolved tool calls."),
                            ));
                        }
                        undo.push(Undo::StepCompleted {
                            turn: at,
                            prev: turn.open_step,
                        });
                        turn.open_step = None;
                    }
                    _ => {}
                }
            }
            "inbox_mutation" => {
                if !crate::mutants::on("inbox-empty-item") && !truthy(item) {
                    return Err(domain(
                        "invalid_inbox_mutation",
                        "Inbox mutation itemId must be non-empty.",
                    ));
                }
                let slot = turn.inbox.iter().position(|(v, _)| same_id(v, item));
                let current = slot.map(|at| turn.inbox[at].1);
                let mutation = field(entry, "mutation");
                let status = if text_is(mutation, "insert") {
                    if !crate::mutants::on("inbox-insert")
                        && (!truthy(field(entry, "message")) || current.is_some())
                    {
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
                    if !crate::mutants::on("inbox-cancel") && current == Some("claimed") {
                        return Err(domain(
                            "invalid_inbox_mutation",
                            format!("Claimed inbox item {} cannot be cancelled.", display(item)?),
                        ));
                    }
                    Some("cancelled")
                } else if text_is(mutation, "claim") {
                    if !crate::mutants::on("inbox-claim") && current != Some("pending") {
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
                    let discardable = matches!(current, Some("pending"))
                        || matches!(current, Some("claimed"))
                            && !crate::mutants::on("inbox-discard");
                    if !discardable {
                        return Err(domain(
                            "invalid_inbox_mutation",
                            format!(
                                "Inbox item {} cannot be discarded in its current state.",
                                display(item)?
                            ),
                        ));
                    }
                    let removed = turn.inbox.remove(slot.unwrap());
                    undo.push(Undo::InboxRemove {
                        turn: at,
                        at: slot.unwrap(),
                        item: removed,
                    });
                    None
                } else {
                    None
                };
                if let Some(status) = status {
                    if let Some(slot) = slot {
                        undo.push(Undo::InboxStatus {
                            turn: at,
                            at: slot,
                            prev: turn.inbox[slot].1,
                        });
                        turn.inbox[slot].1 = status;
                    } else {
                        turn.inbox.push((item.clone(), status));
                        undo.push(Undo::InboxPush { turn: at });
                    }
                }
            }
            "plan_todo_plan_changed" => {
                let plan = field(entry, "plan");
                if !crate::mutants::on("plan-value")
                    && (!matches!(plan, JsValue::Null) && !nonblank(plan))
                {
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
                if !crate::mutants::on("plan-progress") && !trimmed(tool) {
                    return Err(domain(
                        "invalid_plan_todo",
                        "Plan/todo progress requires a tool name.",
                    ));
                }
            }
            "goal_changed" => validate_goal(&mut self.goal_revision, entry, undo)?,
            "turn_result" => {
                let open_step = if crate::mutants::on("turn-open-step") {
                    None
                } else {
                    turn.open_step
                };
                if let Some(step) = open_step {
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
                    if !crate::mutants::on("turn-pending-lifecycles") && !ids.is_empty() {
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
                if !crate::mutants::on("turn-pending-inbox")
                    && turn
                        .inbox
                        .iter()
                        .any(|(_, s)| matches!(*s, "pending" | "claimed"))
                {
                    return Err(domain(
                        "turn_has_pending_inbox",
                        format!("Turn {} closed with pending inbox items.", display(id)?),
                    ));
                }
                undo.push(Undo::TurnClosed {
                    turn: at,
                    prev: turn.closed,
                });
                turn.closed = !crate::mutants::on("closed-state");
            }
            _ => {}
        }
        Ok(())
    }
    /// First turn slot whose key equals `id` under `same_id` semantics —
    /// exactly the `position`/`find` the pre-fix scans performed, in O(1).
    fn turn_slot(&self, id: &JsValue) -> Option<usize> {
        key_of(id).and_then(|key| self.turn_index.get(&key).copied())
    }
    /// Reverts every journaled mutation, newest first. See `advance`.
    pub(super) fn rollback(&mut self, undo: &mut Vec<Undo>) {
        while let Some(entry) = undo.pop() {
            match entry {
                Undo::QueuePush => {
                    self.queue.pop();
                }
                Undo::QueueInsert { at, pair } => {
                    self.queue.insert(at, pair);
                }
                Undo::UsedItem(key) => {
                    self.used_items.remove(&key);
                }
                Undo::UsedTurn(key) => {
                    self.used_turns.remove(&key);
                }
                Undo::TurnsPush => {
                    if let Some((key, _)) = self.turns.pop() {
                        let slot = self.turns.len();
                        if let Some(key) = key_of(&key)
                            && self.turn_index.get(&key) == Some(&slot)
                        {
                            self.turn_index.remove(&key);
                        }
                    }
                }
                Undo::InboxRemove { turn, at, item } => {
                    self.turns[turn].1.inbox.insert(at, item);
                }
                Undo::InboxStatus { turn, at, prev } => {
                    self.turns[turn].1.inbox[at].1 = prev;
                }
                Undo::InboxPush { turn } => {
                    self.turns[turn].1.inbox.pop();
                }
                Undo::StepStarted {
                    turn,
                    last_step,
                    open_step,
                    context_recorded,
                    model_requested,
                    open_tool_calls,
                } => {
                    let state = &mut self.turns[turn].1;
                    state.last_step = last_step;
                    state.open_step = open_step;
                    state.context_recorded = context_recorded;
                    state.model_requested = model_requested;
                    state.open_tool_calls = open_tool_calls;
                }
                Undo::LifecyclePush { turn, which } => {
                    which.vec_mut(&mut self.turns[turn].1).pop();
                }
                Undo::LifecycleRemove { turn, which, value } => {
                    which.vec_mut(&mut self.turns[turn].1).push(value);
                }
                Undo::OpenCallPush { turn } => {
                    self.turns[turn].1.open_tool_calls.pop();
                }
                Undo::OpenCallRemove { turn, value } => {
                    self.turns[turn].1.open_tool_calls.push(value);
                }
                Undo::StepCompleted { turn, prev } => {
                    self.turns[turn].1.open_step = prev;
                }
                Undo::ContextFlag { turn, prev } => {
                    self.turns[turn].1.context_recorded = prev;
                }
                Undo::RequestFlag { turn, prev } => {
                    self.turns[turn].1.model_requested = prev;
                }
                Undo::TurnClosed { turn, prev } => {
                    self.turns[turn].1.closed = prev;
                }
                Undo::GoalRevision(prev) => {
                    self.goal_revision = prev;
                }
            }
        }
    }
}
fn validate_todos(entry: &JsValue) -> RecorderResult<()> {
    if !crate::mutants::on("todos-metadata")
        && (!(text_is(field(entry, "mode"), "markdown")
            || text_is(field(entry, "mode"), "structured"))
            || !matches!(field(entry, "merge"), JsValue::Bool(_))
            || ["markdown", "reason"]
                .iter()
                .any(|k| !field(entry, k).is_undefined() && field(entry, k).as_string().is_none())
            || !matches!(field(entry, "todos"), JsValue::Array(_)))
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
        if !crate::mutants::on("todo-item")
            && (!nonblank(id)
                || !nonblank(field(todo, "content"))
                || !["pending", "in_progress", "completed", "cancelled"]
                    .iter()
                    .any(|s| text_is(field(todo, "status"), s))
                || (!field(todo, "priority").is_undefined()
                    && field(todo, "priority").as_string().is_none())
                || contains(&ids, id))
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
fn validate_goal(revision: &mut f64, entry: &JsValue, undo: &mut Vec<Undo>) -> RecorderResult<()> {
    let number = field(entry, "revision").as_number();
    if !crate::mutants::on("goal-revision-integer")
        && (!safe_integer(number) || number.is_some_and(|n| n < 1.0))
    {
        return Err(domain(
            "invalid_goal",
            "Goal revision must be a positive integer.",
        ));
    }
    if !crate::mutants::on("goal-revision-sequence") && number != Some(*revision + 1.0) {
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
        if !crate::mutants::on("goal-snapshot")
            && (!nonblank(id)
                || !nonblank(field(goal, "objective"))
                || field(goal, "revision").as_number() != number
                || !["active", "paused", "blocked", "complete"]
                    .iter()
                    .any(|s| text_is(field(goal, "phase"), s))
                || (!field(goal, "blockedReason").is_undefined()
                    && field(goal, "blockedReason").as_string().is_none())
                || (!max.is_undefined()
                    && (!safe_integer(max.as_number())
                        || max.as_number().is_some_and(|n| n < 1.0))))
        {
            return Err(domain("invalid_goal", "Goal snapshot is invalid."));
        }
        if !crate::mutants::on("goal-blocked-reason")
            && (text_is(field(goal, "phase"), "blocked") && !trimmed(field(goal, "blockedReason")))
        {
            return Err(domain("invalid_goal", "Blocked goals require a reason."));
        }
    }
    undo.push(Undo::GoalRevision(*revision));
    *revision = if crate::mutants::on("goal-state") {
        0.0
    } else {
        number.unwrap()
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::{JsObject, stringify_json};
    fn outcome(result: RecorderResult<()>) -> JsValue {
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
            Some(path) => std::fs::read_to_string(path).expect("#743 domain fixture prerequisite"),
            None => include_str!("../../tests/fixtures/session-domain/oracle.json").to_owned(),
        };
        let oracle = crate::test_baseline::oracle(&text);
        let JsValue::Array(cases) = field(&oracle, "scenarios") else {
            panic!("#743 domain cases")
        };
        // Keep in sync with session-domain/generate.mts and its README.
        assert_eq!(cases.len(), 304, "domain oracle scenario count");
        for case in cases {
            let name = field(case, "name");
            let JsValue::Array(entries) = field(case, "entries") else {
                panic!("#743 domain entries")
            };
            let JsValue::Array(expected) = field(case, "outcomes") else {
                panic!("#743 domain outcomes")
            };
            assert_eq!(entries.len(), expected.len(), "{name:?}: outcome count");
            let mut state = DomainState::default();
            for (entry, expected) in entries.iter().zip(expected) {
                let next = state.next(entry);
                assert_eq!(
                    stringify_json(&outcome(next)).expect("#743 domain fixture prerequisite"),
                    stringify_json(expected).expect("#743 domain fixture prerequisite"),
                    "#743 domain transition outcome {name:?}: {entry:?}"
                );
            }
            let mut log_state = DomainState::default();
            let log = entries.iter().try_for_each(|e| log_state.next(e));
            assert_eq!(
                stringify_json(&outcome(log)).expect("#743 domain fixture prerequisite"),
                stringify_json(field(case, "log")).expect("#743 domain fixture prerequisite"),
                "#743 domain log outcome {name:?}"
            );
        }
    }
    /// #297 P2#2: a commit must not deep-copy the whole DomainState per event.
    /// Turns are never removed, so the cost of the per-event copy grows with
    /// the cumulative turn count; the fix keeps per-event work bounded while
    /// the TS baseline itself still pays the same cloneState cost per event
    /// (measured on the pinned baseline: see the round's probe-clone-cost).
    /// Trend criterion, not absolute time: the same open-step stream must cost
    /// the same per event whether 16 or 1024 turns came before it.
    #[test]
    fn advance_cost_stays_flat_as_turns_accumulate() {
        let event = |kind: &str, turn: &str, step: Option<u64>| {
            let mut object = JsObject::new();
            object.insert("type".into(), kind.into());
            object.insert("turnId".into(), turn.into());
            if let Some(step) = step {
                object.insert("step".into(), JsValue::Number(step as f64));
            }
            JsValue::Object(object)
        };
        let build = |turns: usize| -> RecorderResult<DomainState> {
            let mut state = DomainState::default();
            for t in 0..turns {
                let turn = format!("t-{t}");
                state.next(&event("turn_started", &turn, None))?;
                state.next(&event("step_started", &turn, Some(1)))?;
                state.next(&event("model_request", &turn, Some(1)))?;
                state.next(&event("step_completed", &turn, Some(1)))?;
                state.next(&event("turn_result", &turn, None))?;
            }
            state.next(&event("turn_started", "t-open", None))?;
            state.next(&event("step_started", "t-open", Some(1)))?;
            state.next(&event("model_request", "t-open", Some(1)))?;
            Ok(state)
        };
        let stream = event("model_stream_event", "t-open", Some(1));
        let time = |turns: usize| {
            (0..3)
                .map(|_| {
                    let mut state = build(turns).expect("#297 trend fixture state");
                    let started = std::time::Instant::now();
                    for _ in 0..10_000 {
                        state
                            .next(std::hint::black_box(&stream))
                            .expect("#297 stream event advances");
                    }
                    std::hint::black_box(&state);
                    started.elapsed().as_secs_f64()
                })
                .fold(f64::INFINITY, f64::min)
        };
        let small = time(16);
        let large = time(1024);
        assert!(
            large / small < 8.0,
            "#297 per-event domain cost grew {}x from 16 to 1024 cumulative turns; \
             a commit must not deep-copy the whole DomainState per event",
            large / small,
        );
    }
}

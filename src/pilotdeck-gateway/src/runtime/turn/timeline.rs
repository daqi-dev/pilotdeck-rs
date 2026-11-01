//! #1220: host-event timeline / streamBoundary projection.
//!
//! Port of the TS sidecar runner's coordinate allocator (TurnTimeline.ts,
//! cc7c4e6) and the projector around it (SidecarHostEventProjector,
//! agentLoopSidecarClient.ts:1493): the runner owns the externally visible
//! timeline for both child and host events, strips child-local stream marks,
//! stamps the bound module, and routes every position through one allocator
//! so blocks keep stable coordinates across the stream and the terminal.
//!
//! Shape deviations from a throwing TS read (missing `message`, non-array
//! `content`, non-string `blockId`/`text`) are skipped instead: the wire
//! decoder and StreamIntegrity reject malformed payloads before projection,
//! so the difference is not observable on the validated stream (ledger
//! A7.2-INTERACTION-CONSUME-001-R6).

use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::{HashMap, HashSet};

pub(crate) struct TurnTimeline {
    turn_id: JsString,
    last_model_position: Option<Position>,
    previous_id: Option<JsString>,
    next_order: u64,
    revision: u64,
    positions: HashMap<JsString, Position>,
    provisional_tools: HashSet<JsString>,
    offsets: HashMap<JsString, u64>,
}

#[derive(Clone)]
struct Position {
    id: JsString,
    previous_id: Option<JsString>,
    order: u64,
    revision: u64,
}

/// `prefix + id` in UTF-16 units; an absent field joins like TS string
/// coercion would (`"tool:" + undefined`).
fn joined(prefix: &str, suffix: Option<&JsString>) -> JsString {
    let mut units: Vec<u16> = prefix.encode_utf16().collect();
    match suffix {
        Some(suffix) => units.extend_from_slice(suffix.units()),
        None => units.extend("undefined".encode_utf16()),
    }
    JsString::from_utf16(units)
}

fn text_of(value: Option<&JsValue>) -> Option<&JsString> {
    value.and_then(JsValue::as_string)
}

fn is_kind(object: &JsObject, key: &str, kind: &str) -> bool {
    object
        .get(key)
        .and_then(JsValue::as_string)
        .is_some_and(|found| found.equals_str(kind))
}

/// TS nullish (`??=`) check: absent, undefined or null.
fn nullish(value: Option<&JsValue>) -> bool {
    matches!(value, None | Some(JsValue::Undefined) | Some(JsValue::Null))
}

fn without_key(object: &JsObject, key: &str) -> JsObject {
    let mut rebuilt = JsObject::new();
    for (name, value) in object.entries() {
        if name.equals_str(key) {
            continue;
        }
        rebuilt.insert(name.clone(), value.clone());
    }
    rebuilt
}

/// timelineContentId: the allocator key of one content block. Composed ids
/// are always truthy strings, like TS; text/thinking need a truthy blockId.
fn timeline_content_id(block: &JsObject) -> Option<JsString> {
    let kind = block.get("type").and_then(JsValue::as_string)?;
    if kind.equals_str("text") || kind.equals_str("thinking") {
        return text_of(block.get("blockId"))
            .filter(|id| !id.is_empty())
            .cloned();
    }
    if kind.equals_str("tool_call") {
        return Some(joined("tool:", text_of(block.get("id"))));
    }
    if kind.equals_str("tool_result") || kind.equals_str("tool_result_reference") {
        return Some(joined("result:", text_of(block.get("toolCallId"))));
    }
    None
}

fn queue_item_id(message: &JsObject) -> Option<JsString> {
    text_of(
        message
            .get("metadata")
            .and_then(|metadata| metadata.get("queueItemId")),
    )
    .filter(|id| !id.is_empty())
    .cloned()
}

impl TurnTimeline {
    pub(crate) fn new(turn_id: JsString) -> Self {
        Self {
            turn_id,
            last_model_position: None,
            previous_id: None,
            next_order: 0,
            revision: 0,
            positions: HashMap::new(),
            provisional_tools: HashSet::new(),
            offsets: HashMap::new(),
        }
    }

    fn position(&mut self, id: &JsString, provisional: bool) -> Position {
        let mut position = match self.positions.get(id) {
            Some(position) => position.clone(),
            None => {
                let position = Position {
                    id: id.clone(),
                    previous_id: self.previous_id.clone(),
                    order: if crate::mutants::on("timeline-order-frozen") {
                        0
                    } else {
                        self.next_order
                    },
                    revision: self.revision + 1,
                };
                self.next_order += 1;
                self.revision += 1;
                self.positions.insert(id.clone(), position.clone());
                if provisional {
                    self.provisional_tools.insert(id.clone());
                } else {
                    self.previous_id = Some(id.clone());
                }
                position
            }
        };
        if !provisional && self.provisional_tools.remove(id) {
            // Only published tools enter the predecessor chain. Receiving
            // complete arguments does not mean AgentLoop will retain it.
            let previous = self
                .positions
                .values()
                .filter(|candidate| {
                    candidate.order < position.order
                        && !self.provisional_tools.contains(&candidate.id)
                })
                .max_by_key(|candidate| candidate.order)
                .map(|candidate| candidate.id.clone());
            position.previous_id = previous;
            self.positions.insert(id.clone(), position.clone());
            let outdated = match &self.previous_id {
                None => true,
                Some(previous) => self
                    .positions
                    .get(previous)
                    .is_none_or(|current| current.order < position.order),
            };
            if outdated {
                self.previous_id = Some(id.clone());
            }
        }
        position
    }

    fn update(&mut self, id: &JsString) -> Position {
        let base = self.position(id, false);
        self.revision += 1;
        let next = Position {
            revision: self.revision,
            ..base
        };
        self.positions.insert(id.clone(), next.clone());
        next
    }

    fn position_object(&self, position: &Position, offset: Option<u64>) -> JsObject {
        let mut object = JsObject::new();
        object.insert("version".into(), JsValue::Number(1.0));
        object.insert("turnId".into(), JsValue::String(self.turn_id.clone()));
        object.insert("id".into(), JsValue::String(position.id.clone()));
        if let Some(previous) = &position.previous_id {
            object.insert("previousId".into(), JsValue::String(previous.clone()));
        }
        object.insert("order".into(), JsValue::Number(position.order as f64));
        object.insert("revision".into(), JsValue::Number(position.revision as f64));
        if let Some(offset) = offset {
            object.insert("offset".into(), JsValue::Number(offset as f64));
        }
        object
    }

    /// `TurnTimeline.message`: assign coordinates to a message's blocks.
    /// Located blocks use `update`; queue-item steering uses `position`.
    fn message(&mut self, message: &mut JsObject) {
        let queue_item_id = queue_item_id(message);
        let Some(JsValue::Array(blocks)) = message.get("content") else {
            return;
        };
        let mut blocks = blocks.clone();
        for block in &mut blocks {
            let Some(fields) = block.as_object() else {
                continue;
            };
            let located = timeline_content_id(fields);
            let id = located.clone().or_else(|| {
                queue_item_id
                    .as_ref()
                    .map(|item| joined("steer:", Some(item)))
            });
            let Some(id) = id else {
                continue;
            };
            if nullish(fields.get("timeline")) {
                let position = if located.is_some() {
                    self.update(&id)
                } else {
                    self.position(&id, false)
                };
                let mut fields = fields.clone();
                fields.insert(
                    "timeline".into(),
                    JsValue::Object(self.position_object(&position, None)),
                );
                *block = JsValue::Object(fields);
            }
        }
        message.insert("content".into(), JsValue::Array(blocks));
    }

    /// `TurnTimeline.event`: sequential branches, not else-if — a
    /// `tool_call_start` reserves its slot and may also carry a boundary.
    pub(crate) fn event(&mut self, event: &mut JsObject) {
        let kind = event.get("type").and_then(JsValue::as_string).cloned();
        let model_event = kind.as_ref().is_some_and(|k| k.equals_str("model_event"));
        if kind
            .as_ref()
            .is_some_and(|k| k.equals_str("model_request_started"))
        {
            for id in std::mem::take(&mut self.provisional_tools) {
                self.positions.remove(&id);
            }
        }
        let inner = event.get("event").and_then(JsValue::as_object).cloned();
        if model_event
            && let Some(inner) = &inner
            && let Some(inner_kind) = inner.get("type").and_then(JsValue::as_string)
        {
            // Reserve the tool's slot before any subsequent text gets a
            // position; some adapters emit only a complete tool call.
            if inner_kind.equals_str("tool_call_start") {
                self.position(&joined("tool:", text_of(inner.get("id"))), true);
            }
            if inner_kind.equals_str("tool_call_end")
                && let Some(tool_call) = inner.get("toolCall").and_then(JsValue::as_object)
            {
                self.position(&joined("tool:", text_of(tool_call.get("id"))), true);
            }
        }
        if model_event
            && text_of(event.get("blockId")).is_some_and(|id| !id.is_empty())
            && let Some(inner) = &inner
            && inner
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|kind| {
                    kind.equals_str("text_delta") || kind.equals_str("thinking_delta")
                })
        {
            let block_id = text_of(event.get("blockId")).expect("#1220 delta blockId");
            let offset = self.offsets.get(block_id).copied().unwrap_or(0);
            let length = text_of(inner.get("text")).map_or(0, JsString::len_utf16);
            self.offsets
                .insert(block_id.clone(), offset + length as u64);
            let position = self.update(block_id);
            self.last_model_position = Some(position.clone());
            event.insert(
                "timeline".into(),
                JsValue::Object(self.position_object(&position, Some(offset))),
            );
            return;
        }
        if model_event
            && !crate::mutants::on("timeline-boundary-omitted")
            && let Some(last) = &self.last_model_position
            && let Some(inner) = &inner
            && inner
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|kind| {
                    kind.equals_str("message_end")
                        || kind.equals_str("tool_call_start")
                        || kind.equals_str("error")
                })
        {
            let through = last.order;
            self.revision += 1;
            let mut boundary = JsObject::new();
            boundary.insert("turnId".into(), JsValue::String(self.turn_id.clone()));
            boundary.insert("through".into(), JsValue::Number(through as f64));
            boundary.insert("revision".into(), JsValue::Number(self.revision as f64));
            event.insert("streamBoundary".into(), JsValue::Object(boundary));
            return;
        }
        if kind.as_ref().is_some_and(|k| {
            k.equals_str("assistant_message")
                || k.equals_str("tool_results_projected")
                || k.equals_str("steer_applied")
        }) && let Some(JsValue::Object(message)) = event.get("message")
        {
            let mut message = message.clone();
            self.message(&mut message);
            event.insert("message".into(), JsValue::Object(message));
        }
        if kind
            .as_ref()
            .is_some_and(|k| k.equals_str("tool_calls_detected"))
            && let Some(JsValue::Array(calls)) = event.get("calls")
        {
            let mut calls = calls.clone();
            for call in &mut calls {
                let Some(fields) = call.as_object() else {
                    continue;
                };
                let id = joined("tool:", text_of(fields.get("id")));
                let position = self.position(&id, false);
                let mut fields = fields.clone();
                fields.insert(
                    "timeline".into(),
                    JsValue::Object(self.position_object(&position, None)),
                );
                *call = JsValue::Object(fields);
            }
            event.insert("calls".into(), JsValue::Array(calls));
        }
        if kind
            .as_ref()
            .is_some_and(|k| k.equals_str("compact_started") || k.equals_str("compact_completed"))
        {
            let compaction = text_of(event.get("compactionId"));
            let position = self.update(&joined("compact:", compaction));
            event.insert(
                "timeline".into(),
                JsValue::Object(self.position_object(&position, None)),
            );
            return;
        }
        if kind.as_ref().is_some_and(|k| k.equals_str("tool_result"))
            && let Some(result) = event.get("result").and_then(JsValue::as_object)
        {
            let tool_call = text_of(result.get("toolCallId"));
            let position = self.position(&joined("result:", tool_call), false);
            event.insert(
                "timeline".into(),
                JsValue::Object(self.position_object(&position, None)),
            );
        }
    }
}

/// stripCanonicalContentTimeline: strip one content block's child stream
/// marks; tool_result blocks also strip the marks of nested content items.
fn strip_content_block(block: &JsObject) -> JsObject {
    let mut stripped = without_key(block, "timeline");
    if is_kind(block, "type", "tool_result")
        && let Some(JsValue::Array(content)) = block.get("content")
    {
        let content: Vec<JsValue> = content
            .iter()
            .map(|item| {
                item.as_object()
                    .map(|item| without_key(item, "timeline"))
                    .map(JsValue::Object)
                    .unwrap_or_else(|| item.clone())
            })
            .collect();
        stripped.insert("content".into(), JsValue::Array(content));
    }
    stripped
}

/// stripMessageTimeline: strip every content block of one message.
fn strip_message(message: &JsObject) -> JsObject {
    let mut stripped = message.clone();
    if let Some(JsValue::Array(content)) = message.get("content") {
        let content: Vec<JsValue> = content
            .iter()
            .map(|block| {
                block
                    .as_object()
                    .map(strip_content_block)
                    .map(JsValue::Object)
                    .unwrap_or_else(|| block.clone())
            })
            .collect();
        stripped.insert("content".into(), JsValue::Array(content));
    }
    stripped
}

/// stripChildTimeline: strip the nested child stream marks of one event.
fn strip_child_timeline(event: &JsObject) -> JsObject {
    let mut event = event.clone();
    let kind = event.get("type").and_then(JsValue::as_string).cloned();
    let message_kind = kind.as_ref().is_some_and(|k| {
        k.equals_str("assistant_message")
            || k.equals_str("tool_results_projected")
            || k.equals_str("steer_applied")
    });
    if message_kind && let Some(JsValue::Object(message)) = event.get("message") {
        event.insert("message".into(), JsValue::Object(strip_message(message)));
    }
    if kind
        .as_ref()
        .is_some_and(|k| k.equals_str("tool_calls_detected"))
        && let Some(JsValue::Array(calls)) = event.get("calls")
    {
        let calls: Vec<JsValue> = calls
            .iter()
            .map(|call| {
                call.as_object()
                    .map(|call| without_key(call, "timeline"))
                    .map(JsValue::Object)
                    .unwrap_or_else(|| call.clone())
            })
            .collect();
        event.insert("calls".into(), JsValue::Array(calls));
    }
    if model_event_tool_call_end(&event)
        && let Some(mut inner) = event.get("event").cloned()
        && let JsValue::Object(inner_fields) = &mut inner
        && let Some(tool_call) = inner_fields
            .get("toolCall")
            .and_then(JsValue::as_object)
            .cloned()
    {
        inner_fields.insert(
            "toolCall".into(),
            JsValue::Object(without_key(&tool_call, "timeline")),
        );
        event.insert("event".into(), inner);
    }
    event
}

fn model_event_tool_call_end(event: &JsObject) -> bool {
    is_kind(event, "type", "model_event")
        && event
            .get("event")
            .and_then(JsValue::as_object)
            .is_some_and(|inner| is_kind(inner, "type", "tool_call_end"))
}

/// projectTerminalMessage: strip child marks, then position every block with
/// direct assignment (stream positions are reused, never re-revised). The
/// module-owned marking stays with the caller's existing
/// `mark_module_message`, which is its TS equivalent.
pub(crate) fn project_terminal_message(timeline: &mut TurnTimeline, message: &mut JsObject) {
    let mut projected = strip_message(message);
    let queue_item_id = queue_item_id(&projected);
    if let Some(JsValue::Array(blocks)) = projected.get("content") {
        let mut blocks = blocks.clone();
        for block in &mut blocks {
            let Some(fields) = block.as_object() else {
                continue;
            };
            let id = timeline_content_id(fields).or_else(|| {
                queue_item_id
                    .as_ref()
                    .map(|item| joined("steer:", Some(item)))
            });
            let Some(id) = id else {
                continue;
            };
            let position = timeline.position(&id, false);
            let mut fields = fields.clone();
            fields.insert(
                "timeline".into(),
                JsValue::Object(timeline.position_object(&position, None)),
            );
            *block = JsValue::Object(fields);
        }
        projected.insert("content".into(), JsValue::Array(blocks));
    }
    *message = projected;
}

/// SidecarHostEventProjector.project: a subagent child stream keeps its own
/// allocator intact; every other host-visible event is stripped of child
/// marks, stamped with the bound module, then projected through the timeline.
pub(crate) fn project(
    timeline: &mut TurnTimeline,
    event: &mut JsObject,
    module: Option<&JsString>,
) {
    if is_kind(event, "type", "subagent_model_event") {
        return;
    }
    let mut owned = without_key(
        &without_key(&strip_child_timeline(event), "timeline"),
        "streamBoundary",
    );
    if let Some(module) = module {
        // TS markModuleOwnedEvent only interprets the two message families;
        // a shape its classifier cannot read stays unmarked here and keeps
        // failing closed at delivery (record_event).
        if (is_kind(&owned, "type", "assistant_message")
            || is_kind(&owned, "type", "tool_results_projected"))
            && let Some(JsValue::Object(message)) = owned.get("message")
            && super::runner::classify_durable_message(&JsValue::Object(message.clone()))
                .is_ok_and(|classified| classified != "durable_message")
        {
            let mut message = message.clone();
            let mut metadata = message
                .get("metadata")
                .and_then(JsValue::as_object)
                .cloned()
                .unwrap_or_default();
            metadata.insert("moduleId".into(), JsValue::String(module.clone()));
            message.insert("metadata".into(), JsValue::Object(metadata));
            owned.insert("message".into(), JsValue::Object(message));
        }
        owned.insert("moduleId".into(), JsValue::String(module.clone()));
    }
    timeline.event(&mut owned);
    *event = owned;
}

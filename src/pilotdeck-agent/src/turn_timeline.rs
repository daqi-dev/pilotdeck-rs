//! Typed per-turn coordinates; JSON values below are immutable annotation echoes only.
use crate::{
    SourceTimelineKey, SourceTimelineNumber, SourceTimelineOffset, SourceTimelinePosition,
    SourceTimelinePrevious, TimelineError, TimelineErrorKind, TurnTimeline,
};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};

pub(crate) fn new(turn_id: JsString) -> TurnTimeline {
    TurnTimeline {
        turn_id,
        last_model_position: None,
        previous_id: None,
        next_order: 0.0,
        revision: 0.0,
        positions: Default::default(),
        provisional_tools: Default::default(),
        offsets: Default::default(),
    }
}

pub(crate) fn position(
    t: &mut TurnTimeline,
    id: SourceTimelineKey,
    provisional: bool,
) -> SourceTimelinePosition {
    if !t.positions.contains_key(&id) {
        t.revision += 1.0;
        let p = SourceTimelinePosition {
            turn_id: t.turn_id.clone(),
            id: id.clone(),
            previous_id: t
                .previous_id
                .as_ref()
                .filter(|p| key_truthy(p))
                .cloned()
                .map(SourceTimelinePrevious::Present)
                .unwrap_or(SourceTimelinePrevious::Missing),
            previous_id_appended: false,
            order: t.next_order,
            revision: t.revision,
            offset_utf16: None,
        };
        t.next_order += 1.0;
        t.positions.insert(id.clone(), p);
        if provisional {
            t.provisional_tools.insert(id.clone());
        } else {
            t.previous_id = Some(id.clone());
        }
    }
    if !provisional && t.provisional_tools.remove(&id) {
        let order = t.positions[&id].order;
        let previous = t
            .positions
            .values()
            .filter(|p| p.order < order && !t.provisional_tools.contains(&p.id))
            .max_by(|a, b| a.order.total_cmp(&b.order))
            .map(|p| p.id.clone());
        let p = t.positions.get_mut(&id).expect("allocated position");
        if matches!(p.previous_id, SourceTimelinePrevious::Missing) {
            p.previous_id_appended = true;
        }
        p.previous_id = previous
            .map(SourceTimelinePrevious::Present)
            .unwrap_or(SourceTimelinePrevious::Undefined);
        if t.previous_id
            .as_ref()
            .is_none_or(|p| !key_truthy(p) || t.positions[p].order < order)
        {
            t.previous_id = Some(id.clone());
        }
    }
    t.positions[&id].clone()
}

pub(crate) fn update(t: &mut TurnTimeline, id: SourceTimelineKey) -> SourceTimelinePosition {
    let mut p = position(t, id.clone(), false);
    t.revision += 1.0;
    p.revision = t.revision;
    t.positions.insert(id, p.clone());
    p
}

fn error(message: &str) -> TimelineError {
    TimelineError {
        kind: TimelineErrorKind::TypeError,
        message: message.into(),
        partial_input: None,
    }
}
fn native(message: &str) -> TimelineError {
    let mut e = error(message);
    e.kind = TimelineErrorKind::NativeGraphRequired;
    e
}
fn read_error(value: Option<&JsValue>, property: &str) -> TimelineError {
    let base = if matches!(value, Some(JsValue::Null)) {
        "null"
    } else {
        "undefined"
    };
    error(&format!(
        "Cannot read properties of {base} (reading '{property}')"
    ))
}
fn with_partial(mut error: TimelineError, value: &JsValue) -> TimelineError {
    error.partial_input = Some(BoundarySnapshot::capture(value));
    error
}
fn assignment_error(value: &JsValue) -> TimelineError {
    let kind = match value {
        JsValue::Number(_) => "number",
        JsValue::Bool(_) => "boolean",
        JsValue::String(_) => "string",
        _ => return native("Array non-index timeline property requires native graph admission"),
    };
    let text = string(Some(value)).expect("primitive string conversion");
    let mut message: Vec<u16> = format!("Cannot create property 'timeline' on {kind} '")
        .encode_utf16()
        .collect();
    message.extend_from_slice(text.units());
    message.push(u16::from(b'\''));
    TimelineError {
        kind: TimelineErrorKind::TypeError,
        message: JsString::from_utf16(message),
        partial_input: None,
    }
}
fn object(value: &JsValue) -> Result<JsObject, TimelineError> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| error("Timeline annotation requires an admitted object"))
}
fn is_type(o: &JsObject, kind: &str) -> bool {
    o.get("type")
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.equals_str(kind))
}
fn truthy(value: Option<&JsValue>) -> bool {
    match value {
        None | Some(JsValue::Undefined | JsValue::Null | JsValue::Bool(false)) => false,
        Some(JsValue::Number(n)) => *n != 0.0 && !n.is_nan(),
        Some(JsValue::String(s)) => !s.is_empty(),
        _ => true,
    }
}
fn nullish(value: Option<&JsValue>) -> bool {
    matches!(value, None | Some(JsValue::Undefined | JsValue::Null))
}
fn key_value(key: &SourceTimelineKey) -> JsValue {
    match key {
        SourceTimelineKey::String(s) => JsValue::String(s.clone()),
        SourceTimelineKey::Number(n) => JsValue::Number(n.value),
        SourceTimelineKey::Bool(b) => JsValue::Bool(*b),
        SourceTimelineKey::Null => JsValue::Null,
        SourceTimelineKey::Undefined => JsValue::Undefined,
    }
}
fn key_truthy(key: &SourceTimelineKey) -> bool {
    truthy(Some(&key_value(key)))
}
fn map_id(value: &JsValue) -> Result<SourceTimelineKey, TimelineError> {
    Ok(match value {
        JsValue::String(s) => SourceTimelineKey::String(s.clone()),
        JsValue::Number(n) => SourceTimelineKey::Number(SourceTimelineNumber { value: *n }),
        JsValue::Bool(b) => SourceTimelineKey::Bool(*b),
        JsValue::Null => SourceTimelineKey::Null,
        JsValue::Undefined => SourceTimelineKey::Undefined,
        _ => {
            return Err(native(
                "Object identity timeline Map key requires native graph admission",
            ));
        }
    })
}
// Template IDs use JS primitive string conversion, never replacement-decoded UTF8.
// Callable/prototype/native graph conversion is outside BoundarySnapshot's domain.
fn string(value: Option<&JsValue>) -> Result<JsString, TimelineError> {
    let undefined = JsValue::Undefined;
    let mut out = Vec::new();
    let mut tasks = vec![Some((value.unwrap_or(&undefined), false))];
    while let Some(task) = tasks.pop() {
        let Some((v, array_item)) = task else {
            out.push(44);
            continue;
        };
        match v {
            JsValue::Undefined | JsValue::Null if array_item => {}
            JsValue::Undefined => out.extend("undefined".encode_utf16()),
            JsValue::Null => out.extend("null".encode_utf16()),
            JsValue::Bool(v) => out.extend(if *v { "true" } else { "false" }.encode_utf16()),
            JsValue::String(v) => out.extend_from_slice(v.units()),
            JsValue::Number(v) => {
                let text = if v.is_nan() {
                    "NaN".into()
                } else if *v == f64::INFINITY {
                    "Infinity".into()
                } else if *v == f64::NEG_INFINITY {
                    "-Infinity".into()
                } else {
                    stringify_json(&JsValue::Number(*v))
                        .expect("finite number codec invariant")
                        .expect("number is never undefined")
                };
                out.extend(text.encode_utf16());
            }
            JsValue::Object(o) => {
                if o.contains_key("toString") {
                    return Err(error("Cannot convert object to primitive value"));
                }
                out.extend("[object Object]".encode_utf16());
            }
            JsValue::Array(items) => {
                for (i, v) in items.iter().enumerate().rev() {
                    tasks.push(Some((v, true)));
                    if i > 0 {
                        tasks.push(None);
                    }
                }
            }
        }
    }
    Ok(JsString::from_utf16(out))
}
pub(crate) fn prefixed(prefix: &str, value: Option<&JsValue>) -> Result<JsString, TimelineError> {
    let mut units: Vec<_> = prefix.encode_utf16().collect();
    units.extend_from_slice(string(value)?.units());
    Ok(JsString::from_utf16(units))
}
pub(crate) fn wire_position(_t: &TurnTimeline, p: &SourceTimelinePosition) -> JsValue {
    let mut o = JsObject::new();
    o.insert("version".into(), JsValue::Number(1.0));
    o.insert("turnId".into(), JsValue::String(p.turn_id.clone()));
    o.insert("id".into(), key_value(&p.id));
    let previous = match &p.previous_id {
        SourceTimelinePrevious::Missing => None,
        SourceTimelinePrevious::Undefined => Some(JsValue::Undefined),
        SourceTimelinePrevious::Present(k) => Some(key_value(k)),
    };
    if !p.previous_id_appended
        && let Some(previous) = &previous
    {
        o.insert("previousId".into(), previous.clone());
    }
    o.insert("order".into(), JsValue::Number(p.order));
    o.insert("revision".into(), JsValue::Number(p.revision));
    if p.previous_id_appended
        && let Some(previous) = previous
    {
        o.insert("previousId".into(), previous);
    }
    if let Some(offset) = &p.offset_utf16 {
        o.insert("offset".into(), offset_value(offset));
    }
    JsValue::Object(o)
}
fn offset_value(offset: &SourceTimelineOffset) -> JsValue {
    match offset {
        SourceTimelineOffset::Number(n) => JsValue::Number(*n),
        SourceTimelineOffset::String(s) => JsValue::String(s.clone()),
    }
}
fn add_length(
    offset: &SourceTimelineOffset,
    length: Option<&JsValue>,
) -> Result<SourceTimelineOffset, TimelineError> {
    let undefined = JsValue::Undefined;
    let length = length.unwrap_or(&undefined);
    let primitive = match length {
        JsValue::Object(_) | JsValue::Array(_) => JsValue::String(string(Some(length))?),
        _ => length.clone(),
    };
    if matches!(offset, SourceTimelineOffset::String(_)) || matches!(primitive, JsValue::String(_))
    {
        let mut units = string(Some(&offset_value(offset)))?.units().to_vec();
        units.extend_from_slice(string(Some(&primitive))?.units());
        return Ok(SourceTimelineOffset::String(JsString::from_utf16(units)));
    }
    let n = match &primitive {
        JsValue::Number(n) => *n,
        JsValue::Undefined => f64::NAN,
        JsValue::Null => 0.0,
        JsValue::Bool(b) => f64::from(u8::from(*b)),
        _ => unreachable!("objects converted to string"),
    };
    let SourceTimelineOffset::Number(offset) = offset else {
        unreachable!("string handled")
    };
    Ok(SourceTimelineOffset::Number(offset + n))
}

pub(crate) fn message(
    t: &mut TurnTimeline,
    snapshot: &BoundarySnapshot,
) -> Result<BoundarySnapshot, TimelineError> {
    message_inner(t, snapshot).map_err(|e| {
        if e.partial_input.is_some() {
            e
        } else {
            with_partial(e, snapshot.value())
        }
    })
}
fn annotate_block(
    t: &mut TurnTimeline,
    block: &JsValue,
    queue: Option<&JsValue>,
) -> Result<JsValue, TimelineError> {
    if nullish(Some(block)) {
        return Err(read_error(Some(block), "type"));
    }
    if let Some(mut b) = block.as_object().cloned() {
        let direct = is_type(&b, "text") || is_type(&b, "thinking");
        let id = if direct {
            None
        } else if is_type(&b, "tool_call") {
            Some(prefixed("tool:", b.get("id"))?)
        } else if is_type(&b, "tool_result") || is_type(&b, "tool_result_reference") {
            Some(prefixed("result:", b.get("toolCallId"))?)
        } else {
            None
        };
        if nullish(b.get("timeline")) {
            let p = if direct && truthy(b.get("blockId")) {
                Some(update(
                    t,
                    map_id(b.get("blockId").expect("truthy blockId"))?,
                ))
            } else if let Some(id) = id {
                Some(update(t, id.into()))
            } else if truthy(queue) {
                Some(position(t, prefixed("steer:", queue)?.into(), false))
            } else {
                None
            };
            if let Some(p) = p {
                b.insert("timeline".into(), wire_position(t, &p));
            }
        }
        return Ok(JsValue::Object(b));
    }
    if truthy(queue) {
        // Strict property assignment checks the receiver only AFTER evaluating its RHS.
        position(t, prefixed("steer:", queue)?.into(), false);
        return Err(assignment_error(block));
    }
    Ok(block.clone())
}
fn message_inner(
    t: &mut TurnTimeline,
    snapshot: &BoundarySnapshot,
) -> Result<BoundarySnapshot, TimelineError> {
    if nullish(Some(snapshot.value())) {
        return Err(read_error(Some(snapshot.value()), "content"));
    }
    let Some(mut m) = snapshot.value().as_object().cloned() else {
        return Err(error("message.content is not iterable"));
    };
    let queue = m
        .get("metadata")
        .and_then(|v| v.get("queueItemId"))
        .cloned();
    let Some(content) = m.get("content") else {
        return Err(error("message.content is not iterable"));
    };
    let mut blocks = match content {
        JsValue::Array(v) => v.clone(),
        JsValue::String(s) => {
            // String iteration uses code points, while retaining unpaired UTF16 units.
            let mut chars = Vec::new();
            let mut index = 0;
            while index < s.units().len() {
                let start = index;
                index += 1;
                if (0xd800..=0xdbff).contains(&s.units()[start])
                    && index < s.units().len()
                    && (0xdc00..=0xdfff).contains(&s.units()[index])
                {
                    index += 1;
                }
                chars.push(JsValue::String(JsString::from_utf16(
                    s.units()[start..index].to_vec(),
                )));
            }
            for block in chars {
                annotate_block(t, &block, queue.as_ref())?;
            }
            return Ok(snapshot.clone());
        }
        _ => return Err(error("message.content is not iterable")),
    };
    for index in 0..blocks.len() {
        match annotate_block(t, &blocks[index], queue.as_ref()) {
            Ok(block) => blocks[index] = block,
            Err(e) => {
                m.insert("content".into(), JsValue::Array(blocks));
                return Err(with_partial(e, &JsValue::Object(m)));
            }
        }
    }
    m.insert("content".into(), JsValue::Array(blocks));
    Ok(BoundarySnapshot::capture(&JsValue::Object(m)))
}

fn event_inner(
    t: &mut TurnTimeline,
    snapshot: &BoundarySnapshot,
) -> Result<BoundarySnapshot, TimelineError> {
    let undefined = JsValue::Undefined;
    if snapshot.value().as_object().is_none() {
        if nullish(Some(snapshot.value())) {
            return Err(read_error(Some(snapshot.value()), "type"));
        }
        return Ok(snapshot.clone());
    }
    let mut e = object(snapshot.value())?;
    if is_type(&e, "model_request_started") {
        for id in t.provisional_tools.drain() {
            t.positions.remove(&id);
        }
    }
    if is_type(&e, "model_event") {
        let inner_value = e.get("event").unwrap_or(&undefined);
        if nullish(Some(inner_value)) {
            return Err(read_error(Some(inner_value), "type"));
        }
        let inner = inner_value.as_object().cloned().unwrap_or_default();
        if is_type(&inner, "tool_call_start") {
            position(t, prefixed("tool:", inner.get("id"))?.into(), true);
        }
        if is_type(&inner, "tool_call_end") {
            let call = inner.get("toolCall").unwrap_or(&undefined);
            if nullish(Some(call)) {
                return Err(read_error(Some(call), "id"));
            }
            position(t, prefixed("tool:", call.get("id"))?.into(), true);
        }
        if truthy(e.get("blockId"))
            && (is_type(&inner, "text_delta") || is_type(&inner, "thinking_delta"))
        {
            let id = map_id(e.get("blockId").expect("truthy blockId"))?;
            let offset = t
                .offsets
                .get(&id)
                .cloned()
                .unwrap_or(SourceTimelineOffset::Number(0.0));
            let text = inner.get("text");
            let length = match text {
                None | Some(JsValue::Undefined | JsValue::Null) => {
                    return Err(read_error(text, "length"));
                }
                Some(JsValue::String(s)) => JsValue::Number(s.len_utf16() as f64),
                Some(JsValue::Array(a)) => JsValue::Number(a.len() as f64),
                Some(JsValue::Object(o)) => o.get("length").cloned().unwrap_or(JsValue::Undefined),
                Some(_) => JsValue::Undefined,
            };
            t.offsets
                .insert(id.clone(), add_length(&offset, Some(&length))?);
            let mut p = update(t, id);
            t.last_model_position = Some(p.clone());
            p.offset_utf16 = Some(offset);
            e.insert("timeline".into(), wire_position(t, &p));
            return Ok(BoundarySnapshot::capture(&JsValue::Object(e)));
        }
        if (is_type(&inner, "message_end")
            || is_type(&inner, "tool_call_start")
            || is_type(&inner, "error"))
            && let Some(last) = &t.last_model_position
        {
            t.revision += 1.0;
            let mut boundary = JsObject::new();
            boundary.insert("turnId".into(), JsValue::String(t.turn_id.clone()));
            boundary.insert("through".into(), JsValue::Number(last.order));
            boundary.insert("revision".into(), JsValue::Number(t.revision));
            e.insert("streamBoundary".into(), JsValue::Object(boundary));
            return Ok(BoundarySnapshot::capture(&JsValue::Object(e)));
        }
    }
    if is_type(&e, "assistant_message")
        || is_type(&e, "tool_results_projected")
        || is_type(&e, "steer_applied")
    {
        let m = BoundarySnapshot::capture(e.get("message").unwrap_or(&undefined));
        let annotated = match message(t, &m) {
            Ok(m) => m,
            Err(mut error) => {
                if e.contains_key("message")
                    && let Some(partial) = &error.partial_input
                {
                    e.insert("message".into(), partial.value().clone());
                }
                error.partial_input = Some(BoundarySnapshot::capture(&JsValue::Object(e)));
                return Err(error);
            }
        };
        e.insert("message".into(), annotated.value().clone());
    }
    if is_type(&e, "tool_calls_detected") {
        let calls = match e.get("calls") {
            Some(JsValue::Array(a)) => a.clone(),
            Some(JsValue::String(s)) => {
                if s.is_empty() {
                    return Ok(snapshot.clone());
                }
                let units = s.units();
                let end = if units.len() > 1
                    && (0xd800..=0xdbff).contains(&units[0])
                    && (0xdc00..=0xdfff).contains(&units[1])
                {
                    2
                } else {
                    1
                };
                let call = JsValue::String(JsString::from_utf16(units[..end].to_vec()));
                position(t, "tool:undefined".into(), false);
                return Err(assignment_error(&call));
            }
            _ => return Err(error("event.calls is not iterable")),
        };
        let mut annotated = calls;
        for index in 0..annotated.len() {
            let attempt = (|| {
                let call = &annotated[index];
                if nullish(Some(call)) {
                    return Err(read_error(Some(call), "id"));
                }
                let p = position(t, prefixed("tool:", call.get("id"))?.into(), false);
                let Some(mut call) = call.as_object().cloned() else {
                    return Err(assignment_error(call));
                };
                call.insert("timeline".into(), wire_position(t, &p));
                Ok(JsValue::Object(call))
            })();
            match attempt {
                Ok(call) => annotated[index] = call,
                Err(error) => {
                    e.insert("calls".into(), JsValue::Array(annotated));
                    return Err(with_partial(error, &JsValue::Object(e)));
                }
            }
        }
        e.insert("calls".into(), JsValue::Array(annotated));
    }
    if is_type(&e, "compact_started") || is_type(&e, "compact_completed") {
        let p = update(t, prefixed("compact:", e.get("compactionId"))?.into());
        e.insert("timeline".into(), wire_position(t, &p));
    }
    if is_type(&e, "tool_result") {
        let result = e.get("result").unwrap_or(&undefined);
        if nullish(Some(result)) {
            return Err(read_error(Some(result), "toolCallId"));
        }
        let p = position(
            t,
            prefixed("result:", result.get("toolCallId"))?.into(),
            false,
        );
        e.insert("timeline".into(), wire_position(t, &p));
    }
    Ok(BoundarySnapshot::capture(&JsValue::Object(e)))
}

pub(crate) fn event(
    t: &mut TurnTimeline,
    snapshot: &BoundarySnapshot,
) -> Result<BoundarySnapshot, TimelineError> {
    event_inner(t, snapshot).map_err(|e| {
        if e.partial_input.is_some() {
            e
        } else {
            with_partial(e, snapshot.value())
        }
    })
}

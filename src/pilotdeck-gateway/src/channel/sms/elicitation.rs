//! `ImElicitationHelper.ts`: pending elicitation state for IM channels.
use std::collections::HashMap;

use pilotdeck_protocol::{JsObject, JsString, JsValue};

use super::{concat_units, join_lines, js_string_of, truthy};

/// `Array.isArray`-style access: a non-array (or missing) `options` /
/// `questions` field is the empty list, the only shapes the events carry.
pub(crate) fn as_array(value: Option<&JsValue>) -> &[JsValue] {
    match value {
        Some(JsValue::Array(items)) => items.as_slice(),
        _ => &[],
    }
}

pub struct PendingElicitation {
    pub session_key: JsString,
    /// #1339 fix-r1: a missing event `requestId` stays `None` — the response
    /// then records real `undefined` (the JSON record omits the key).
    pub request_id: Option<JsString>,
    /// The raw `questions` array from the elicitation event.
    pub questions: Vec<JsValue>,
}

#[derive(Default)]
pub struct ImElicitationHelper {
    pending: HashMap<JsString, PendingElicitation>,
}

/// The numbered-option text the channel sends when the event stream yields an
/// `elicitation_request`. `capture` uses descriptions, the render path does not.
pub(crate) fn push_question_lines(
    lines: &mut Vec<JsString>,
    questions: &[JsValue],
    describe: bool,
) {
    for question in questions {
        if let Some(header) = question.get("header").filter(|value| truthy(value)) {
            lines.push(concat_units(&[
                "**".encode_utf16().collect::<Vec<u16>>().as_slice(),
                js_string_of(header).units(),
                "**".encode_utf16().collect::<Vec<u16>>().as_slice(),
            ]));
        }
        if let Some(text) = question.get("question").filter(|value| truthy(value)) {
            lines.push(js_string_of(text));
        }
        let options = as_array(question.get("options"));
        for (index, option) in options.iter().enumerate() {
            let mut line = format!("{}. ", index + 1)
                .encode_utf16()
                .collect::<Vec<u16>>();
            line.extend_from_slice(
                js_string_of(option.get("label").unwrap_or(&JsValue::Undefined)).units(),
            );
            if describe
                && let Some(description) = option.get("description").filter(|value| truthy(value))
            {
                line.extend_from_slice(" — ".encode_utf16().collect::<Vec<u16>>().as_slice());
                line.extend_from_slice(js_string_of(description).units());
            }
            lines.push(JsString::from_utf16(line));
        }
    }
}

fn question_text(questions: &[JsValue]) -> JsString {
    let mut lines: Vec<JsString> = Vec::new();
    push_question_lines(&mut lines, questions, true);
    lines.push(JsString::from(""));
    lines.push(JsString::from(
        "回复数字选择（多选用逗号分隔），回复 0 取消。",
    ));
    JsString::from_utf16(join_lines(&lines))
}

impl ImElicitationHelper {
    /// Call when the event stream yields an `elicitation_request`; returns the
    /// formatted question text to send to the user.
    pub fn capture(
        &mut self,
        chat_id: &JsString,
        session_key: &JsString,
        event: &JsValue,
    ) -> JsString {
        let questions: Vec<JsValue> = as_array(event.get("questions")).to_vec();
        let text = question_text(&questions);
        // #1339 fix-r1: the event may carry no `requestId`; TS stores real
        // `undefined` and `JSON.stringify` then omits the key.
        let request_id = event
            .get("requestId")
            .and_then(super::permission::optional_string);
        let request_id = if crate::mutants::on("sms-elicit-request-id-empty") {
            // fix-r1 mutant: the pre-fix `js_string_of` coercion turned a
            // missing id into the string "undefined".
            Some(request_id.unwrap_or_else(|| js_string_of(&JsValue::Undefined)))
        } else {
            request_id
        };
        self.pending.insert(
            chat_id.clone(),
            PendingElicitation {
                session_key: session_key.clone(),
                request_id,
                questions,
            },
        );
        text
    }

    pub fn has_pending(&self, chat_id: &JsString) -> bool {
        self.pending.contains_key(chat_id)
    }

    /// `answer()`'s prologue: the pending entry is removed before the gateway
    /// call, so a failed `respondElicitation` still consumes it.
    pub fn take(&mut self, chat_id: &JsString) -> Option<PendingElicitation> {
        self.pending.remove(chat_id)
    }

    pub fn clear(&mut self, chat_id: &JsString) {
        self.pending.remove(chat_id);
    }
}

/// The `{"type":"cancelled","reason":"user cancelled"}` answer.
pub fn cancelled_response(entry: &PendingElicitation) -> super::gateway::ElicitationResponse {
    let mut answer = JsObject::new();
    answer.insert("type".into(), JsValue::String("cancelled".into()));
    answer.insert("reason".into(), JsValue::String("user cancelled".into()));
    super::gateway::ElicitationResponse {
        session_key: entry.session_key.clone(),
        request_id: entry.request_id.clone(),
        answer: JsValue::Object(answer),
    }
}

/// ECMAScript `parseInt(s, 10)`: optional whitespace and sign, then ASCII
/// digits; anything else stops the scan, no digits is NaN.
fn parse_int_radix_10(units: &[u16]) -> Option<f64> {
    let is_space = |unit: u16| {
        matches!(
            unit,
            0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f
                | 0x205f | 0x3000 | 0xfeff
        )
    };
    let mut index = 0;
    while index < units.len() && is_space(units[index]) {
        index += 1;
    }
    let mut negative = false;
    if index < units.len() && (units[index] == u16::from(b'-') || units[index] == u16::from(b'+')) {
        negative = units[index] == u16::from(b'-');
        index += 1;
    }
    let start = index;
    let mut value = 0f64;
    while index < units.len() && (u16::from(b'0')..=u16::from(b'9')).contains(&units[index]) {
        value = value * 10.0 + f64::from(u32::from(units[index] - u16::from(b'0')));
        index += 1;
    }
    if index == start {
        return None;
    }
    Some(if negative { -value } else { value })
}

/// `trimmed.split(/[,，\s]+/).map(parseInt).filter(!NaN)` — empty pieces at
/// the boundaries are kept by JS `split` but parse to NaN and drop out.
fn answer_indices(trimmed: &[u16]) -> Vec<f64> {
    let is_separator = |unit: u16| {
        unit == u16::from(b',') || unit == 0xff0c || {
            matches!(
                unit,
                0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f
                    | 0x205f | 0x3000 | 0xfeff
            )
        }
    };
    let mut indices = Vec::new();
    let mut piece: Vec<u16> = Vec::new();
    let flush = |piece: &mut Vec<u16>, indices: &mut Vec<f64>| {
        if let Some(value) = parse_int_radix_10(piece) {
            indices.push(value);
        }
        piece.clear();
    };
    for &unit in trimmed {
        if is_separator(unit) {
            // The `+` quantifier collapses runs; an empty piece still counts
            // as a split position for JS, but only in the middle of a run.
            if !piece.is_empty() {
                flush(&mut piece, &mut indices);
            }
        } else {
            piece.push(unit);
        }
    }
    flush(&mut piece, &mut indices);
    indices
}

/// The `{"type":"answered","answers":{...}}` answer for a non-"0" reply.
pub fn answered_response(
    entry: &PendingElicitation,
    trimmed: &JsString,
) -> super::gateway::ElicitationResponse {
    let mut answers = JsObject::new();
    for question in &entry.questions {
        let options = as_array(question.get("options"));
        let mut selected: Vec<JsValue> = Vec::new();
        for index in answer_indices(trimmed.units()) {
            if (1.0..=options.len() as f64).contains(&index) {
                selected.push(
                    options[index as usize - 1]
                        .get("label")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                );
            }
        }
        if selected.is_empty() {
            selected.push(JsValue::String(trimmed.clone()));
        }
        let multi_select = question.get("multiSelect").is_some_and(truthy);
        let value = if multi_select {
            JsValue::Array(selected)
        } else {
            selected
                .into_iter()
                .next()
                .unwrap_or(JsValue::String(trimmed.clone()))
        };
        answers.insert(
            js_string_of(question.get("question").unwrap_or(&JsValue::Undefined)),
            value,
        );
    }
    let mut answer = JsObject::new();
    answer.insert("type".into(), JsValue::String("answered".into()));
    answer.insert("answers".into(), JsValue::Object(answers));
    super::gateway::ElicitationResponse {
        session_key: entry.session_key.clone(),
        request_id: entry.request_id.clone(),
        answer: JsValue::Object(answer),
    }
}

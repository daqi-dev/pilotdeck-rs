//! `ImPermissionHelper.ts`: the pending-permission state machine. The async
//! `answerWithState` is split at its single await boundary (`permissionDecide`)
//! into `begin_answer` / `finish_answer` / `fail_answer`; each phase runs under
//! the channel's state lock exactly like a JS run-to-completion block.
use std::collections::{HashMap, HashSet};

use pilotdeck_protocol::{JsString, JsValue};

use super::gateway::PermissionDecisionOutcome;
use super::{concat_units, join_lines, js_string_of, trim_units};

#[derive(Clone)]
pub struct PendingPermission {
    pub session_key: JsString,
    pub request_id: Option<JsString>,
    pub tool_name: JsString,
    pub payload: JsValue,
}

#[derive(Clone)]
struct PendingPrompt {
    text: JsString,
    request_id: Option<JsString>,
}

/// `ImPermissionAnswerResult`.
#[derive(Clone, Debug)]
pub struct PermissionAnswer {
    pub text: Option<JsString>,
    pub can_advance: bool,
    pub retry_prompt: bool,
    pub answer_token: Option<u32>,
}

/// `answerWithState`'s phases. `Decide` carries everything `finish_answer` or
/// `fail_answer` needs to restore state.
pub enum BeginAnswer {
    /// TS `undefined`: nothing to do.
    None,
    /// A completed (non-advancing) status result.
    Status(PermissionAnswer),
    Decide {
        entry: PendingPermission,
        deny: bool,
        remember: bool,
        answer_token: u32,
        generation: u32,
        /// The `entries` array after `shift()`.
        shifted_rest: Vec<PendingPermission>,
    },
}

#[derive(Default)]
pub struct ImPermissionHelper {
    pending: HashMap<JsString, Vec<PendingPermission>>,
    next_prompts: HashMap<JsString, PendingPrompt>,
    prompt_delivering: HashSet<JsString>,
    retry_prompt_pending: HashSet<JsString>,
    initial_prompt_pending: HashSet<JsString>,
    answering: HashSet<JsString>,
    in_flight: HashSet<JsString>,
    generations: HashMap<JsString, u32>,
    answer_tokens: HashMap<JsString, u32>,
    deferred_clears: HashSet<JsString>,
    next_answer_token: u32,
}

/// What the reply text was, for `finish_answer`'s final confirmation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DecisionReply {
    Deny,
    AllowOnce,
    AllowSession,
}

impl DecisionReply {
    /// `None` is a reply the TS branch turns into the usage status text.
    fn from_trimmed(trimmed: &[u16]) -> Option<Self> {
        if trimmed == super::utf16("0").as_slice() {
            Some(Self::Deny)
        } else if trimmed == super::utf16("1").as_slice() {
            Some(Self::AllowOnce)
        } else if trimmed == super::utf16("2").as_slice() {
            Some(Self::AllowSession)
        } else {
            None
        }
    }
}

impl ImPermissionHelper {
    pub fn capture(
        &mut self,
        chat_id: &JsString,
        session_key: &JsString,
        event: &JsValue,
    ) -> Option<JsString> {
        let entry = PendingPermission {
            session_key: session_key.clone(),
            request_id: event.get("requestId").and_then(optional_string),
            tool_name: js_string_of(event.get("toolName").unwrap_or(&JsValue::Undefined)),
            payload: event.get("payload").cloned().unwrap_or(JsValue::Undefined),
        };
        let entries = self.pending.entry(chat_id.clone()).or_default();
        entries.push(entry);
        if entries.len() != 1 || self.answering.contains(chat_id) {
            return None;
        }
        let prompt = format_permission_prompt(&entries[0]);
        self.answering.insert(chat_id.clone());
        self.next_prompts.insert(
            chat_id.clone(),
            PendingPrompt {
                text: prompt.clone(),
                request_id: entries[0].request_id.clone(),
            },
        );
        self.initial_prompt_pending.insert(chat_id.clone());
        Some(prompt)
    }

    pub fn format_pending(&self, chat_id: &JsString) -> Option<JsString> {
        self.pending
            .get(chat_id)
            .filter(|entries| !entries.is_empty())
            .map(|entries| format_permission_prompt(&entries[0]))
    }

    pub fn has_pending(&self, chat_id: &JsString) -> bool {
        self.pending
            .get(chat_id)
            .is_some_and(|entries| !entries.is_empty())
            || self.answering.contains(chat_id)
    }

    pub fn is_answering(&self, chat_id: &JsString) -> bool {
        self.answering.contains(chat_id)
    }

    pub fn take_next_prompt(
        &mut self,
        chat_id: &JsString,
        answer_token: Option<u32>,
    ) -> Option<JsString> {
        // Status replies from answer() are non-advancing. Do not let an inbound
        // reply during initial delivery or an in-flight decision consume the
        // queued prompt and clear the lock.
        if !token_matches(&self.answer_tokens, chat_id, answer_token) {
            return None;
        }
        if self.prompt_delivering.contains(chat_id)
            || self.initial_prompt_pending.contains(chat_id)
            || self.in_flight.contains(chat_id)
        {
            return None;
        }
        if !self.next_prompts.contains_key(chat_id) {
            let entry = self
                .pending
                .get(chat_id)
                .and_then(|entries| entries.first())
                .cloned();
            let Some(entry) = entry else {
                self.confirm_answer(chat_id, answer_token);
                return None;
            };
            self.next_prompts.insert(
                chat_id.clone(),
                PendingPrompt {
                    text: format_permission_prompt(&entry),
                    request_id: entry.request_id.clone(),
                },
            );
        }
        self.prompt_delivering.insert(chat_id.clone());
        self.next_prompts
            .get(chat_id)
            .map(|prompt| prompt.text.clone())
    }

    pub fn get_prompt_request_id(
        &self,
        chat_id: &JsString,
        answer_token: Option<u32>,
    ) -> Option<JsString> {
        if !token_matches(&self.answer_tokens, chat_id, answer_token) {
            return None;
        }
        self.next_prompts
            .get(chat_id)
            .and_then(|prompt| prompt.request_id.clone())
    }

    pub fn confirm_initial_prompt(
        &mut self,
        chat_id: &JsString,
        delivered: bool,
        expected_request_id: Option<&JsString>,
    ) {
        if !self.answering.contains(chat_id) || !self.next_prompts.contains_key(chat_id) {
            return;
        }
        if !request_id_matches(&self.next_prompts, chat_id, expected_request_id) {
            return;
        }
        if !delivered {
            self.initial_prompt_pending.remove(chat_id);
            self.retry_prompt_pending.insert(chat_id.clone());
            return;
        }
        self.next_prompts.remove(chat_id);
        self.initial_prompt_pending.remove(chat_id);
        self.retry_prompt_pending.remove(chat_id);
        self.answering.remove(chat_id);
        self.finish_deferred_clear(chat_id);
    }

    pub fn confirm_next_prompt(
        &mut self,
        chat_id: &JsString,
        delivered: bool,
        expected_request_id: Option<&JsString>,
        answer_token: Option<u32>,
    ) {
        if !token_matches(&self.answer_tokens, chat_id, answer_token) {
            return;
        }
        if !self.prompt_delivering.contains(chat_id) {
            return;
        }
        if !request_id_matches(&self.next_prompts, chat_id, expected_request_id) {
            return;
        }
        self.prompt_delivering.remove(chat_id);
        if !delivered {
            self.retry_prompt_pending.insert(chat_id.clone());
            return;
        }
        self.next_prompts.remove(chat_id);
        self.retry_prompt_pending.remove(chat_id);
        self.answering.remove(chat_id);
        self.answer_tokens.remove(chat_id);
        self.finish_deferred_clear(chat_id);
    }

    /// Release a completed answer when the adapter cannot deliver its reply.
    pub fn release_answer(&mut self, chat_id: &JsString, answer_token: Option<u32>) {
        if !token_matches(&self.answer_tokens, chat_id, answer_token) {
            return;
        }
        // Keep the queued prompt and lock intact so a failed delivery cannot let
        // the next inbound message decide the unseen request.
        self.prompt_delivering.remove(chat_id);
        if self.next_prompts.contains_key(chat_id) {
            self.retry_prompt_pending.insert(chat_id.clone());
        } else {
            self.confirm_answer(chat_id, answer_token);
        }
    }

    /// Mark the confirmation message as delivered and release the final answer lock.
    pub fn confirm_answer(&mut self, chat_id: &JsString, answer_token: Option<u32>) {
        if !token_matches(&self.answer_tokens, chat_id, answer_token) {
            return;
        }
        if !self.answering.contains(chat_id)
            || self.in_flight.contains(chat_id)
            || self.initial_prompt_pending.contains(chat_id)
        {
            return;
        }
        if self.prompt_delivering.contains(chat_id) || self.next_prompts.contains_key(chat_id) {
            return;
        }
        if self
            .pending
            .get(chat_id)
            .is_some_and(|entries| !entries.is_empty())
        {
            return;
        }
        self.retry_prompt_pending.remove(chat_id);
        self.answering.remove(chat_id);
        self.answer_tokens.remove(chat_id);
        self.finish_deferred_clear(chat_id);
    }

    /// Clear state after a completed turn, without racing an answer delivery.
    pub fn clear_after_turn(&mut self, chat_id: &JsString) {
        let initial_prompt_sending = self.initial_prompt_pending.contains(chat_id);
        if self.in_flight.contains(chat_id)
            || self.prompt_delivering.contains(chat_id)
            || self.retry_prompt_pending.contains(chat_id)
            || (!initial_prompt_sending && self.answering.contains(chat_id))
        {
            self.deferred_clears.insert(chat_id.clone());
            return;
        }
        self.clear(chat_id);
    }

    pub fn clear(&mut self, chat_id: &JsString) {
        self.deferred_clears.remove(chat_id);
        self.clear_now(chat_id);
    }

    fn finish_deferred_clear(&mut self, chat_id: &JsString) {
        if !self.deferred_clears.contains(chat_id) {
            return;
        }
        if self
            .pending
            .get(chat_id)
            .is_some_and(|entries| !entries.is_empty())
            || self.answering.contains(chat_id)
            || self.in_flight.contains(chat_id)
            || self.prompt_delivering.contains(chat_id)
            || self.initial_prompt_pending.contains(chat_id)
            || self.next_prompts.contains_key(chat_id)
            || self.retry_prompt_pending.contains(chat_id)
        {
            return;
        }
        self.deferred_clears.remove(chat_id);
        self.clear_now(chat_id);
    }

    fn clear_now(&mut self, chat_id: &JsString) {
        if self.in_flight.contains(chat_id) {
            *self.generations.entry(chat_id.clone()).or_insert(0) += 1;
        } else {
            self.generations.remove(chat_id);
        }
        self.pending.remove(chat_id);
        self.next_prompts.remove(chat_id);
        self.prompt_delivering.remove(chat_id);
        self.retry_prompt_pending.remove(chat_id);
        self.initial_prompt_pending.remove(chat_id);
        self.in_flight.remove(chat_id);
        self.answer_tokens.remove(chat_id);
        self.answering.remove(chat_id);
    }

    /// The synchronous part of `answerWithState` up to `permissionDecide`.
    pub fn begin_answer(&mut self, chat_id: &JsString, text: &JsString) -> BeginAnswer {
        if self.answering.contains(chat_id) {
            // Keep ordinary messages inside the permission flow while the RPC or
            // prompt delivery is pending. The structured result prevents adapters
            // from treating these status messages as completed decisions.
            if self.in_flight.contains(chat_id) {
                let text = if crate::mutants::on("sms-permission-status-text") {
                    "权限决定处理中。"
                } else {
                    "权限决定处理中，请稍候。"
                };
                return BeginAnswer::Status(status_answer(text, false, false));
            }
            if self.initial_prompt_pending.contains(chat_id) {
                return BeginAnswer::Status(status_answer(
                    "权限提示发送中，请稍候。",
                    false,
                    false,
                ));
            }
            if self.retry_prompt_pending.contains(chat_id) {
                return BeginAnswer::Status(status_answer(
                    "上一条权限提示发送失败，正在重试。",
                    false,
                    true,
                ));
            }
            return BeginAnswer::None;
        }
        if self
            .pending
            .get(chat_id)
            .is_none_or(|entries| entries.is_empty())
        {
            return BeginAnswer::None;
        }
        let trimmed = trim_units(text.units());
        let Some(reply) = DecisionReply::from_trimmed(trimmed) else {
            return BeginAnswer::Status(status_answer(
                "请回复 1 允许一次，回复 2 允许本会话，回复 0 拒绝。",
                false,
                false,
            ));
        };

        self.answering.insert(chat_id.clone());
        self.in_flight.insert(chat_id.clone());
        self.next_answer_token += 1;
        let answer_token = self.next_answer_token;
        self.answer_tokens.insert(chat_id.clone(), answer_token);
        let generation = self.generations.get(chat_id).copied().unwrap_or(0) + 1;
        self.generations.insert(chat_id.clone(), generation);
        // `entries.shift()`; the map keeps the rest, the caller keeps a copy
        // as the `?? entries` fallback for the post-decide branches.
        let mut entries = self.pending.remove(chat_id).unwrap_or_default();
        let entry = entries.remove(0);
        let shifted_rest = entries.clone();
        if !entries.is_empty() {
            self.pending.insert(chat_id.clone(), entries);
        }
        BeginAnswer::Decide {
            deny: matches!(reply, DecisionReply::Deny),
            remember: matches!(reply, DecisionReply::AllowSession),
            entry,
            answer_token,
            generation,
            shifted_rest,
        }
    }

    /// `answerWithState`'s catch block plus finally, after `permissionDecide`
    /// rejected. Restores the entry and rethrows through the caller.
    pub fn fail_answer(
        &mut self,
        chat_id: &JsString,
        generation: u32,
        entry: PendingPermission,
        shifted_rest: Vec<PendingPermission>,
    ) {
        let generation_matches = self.generations.get(chat_id).copied() == Some(generation);
        if generation_matches {
            let mut restored = vec![entry];
            restored.extend(self.pending.get(chat_id).cloned().unwrap_or(shifted_rest));
            self.pending.insert(chat_id.clone(), restored);
            self.next_prompts.remove(chat_id);
            self.retry_prompt_pending.remove(chat_id);
            self.answering.remove(chat_id);
        }
        self.answer_finally(chat_id, generation);
    }

    /// `answerWithState` after a resolved `permissionDecide` (the `finally`
    /// runs on every path, mismatch included).
    pub fn finish_answer(
        &mut self,
        chat_id: &JsString,
        answer_token: u32,
        generation: u32,
        reply: DecisionReply,
        shifted_rest: Vec<PendingPermission>,
        decision: PermissionDecisionOutcome,
    ) -> Option<PermissionAnswer> {
        let mismatched = self.generations.get(chat_id).copied() != Some(generation);
        let result = if mismatched {
            None
        } else if !decision.delivered {
            let current_entries = self.pending.get(chat_id).cloned().unwrap_or(shifted_rest);
            if !current_entries.is_empty() {
                let first = format_permission_prompt(&current_entries[0]);
                let request_id = current_entries[0].request_id.clone();
                self.pending.insert(chat_id.clone(), current_entries);
                self.next_prompts.insert(
                    chat_id.clone(),
                    PendingPrompt {
                        text: first,
                        request_id,
                    },
                );
            } else {
                self.pending.remove(chat_id);
                self.next_prompts.remove(chat_id);
            }
            self.retry_prompt_pending.remove(chat_id);
            Some(PermissionAnswer {
                text: Some(JsString::from("权限请求已失效，已跳过，继续处理。")),
                can_advance: false,
                retry_prompt: true,
                answer_token: Some(answer_token),
            })
        } else {
            let remaining = self.pending.get(chat_id).cloned().unwrap_or(shifted_rest);
            if !remaining.is_empty() {
                let text = format_permission_prompt(&remaining[0]);
                let request_id = remaining[0].request_id.clone();
                self.next_prompts
                    .insert(chat_id.clone(), PendingPrompt { text, request_id });
            }
            let text = match reply {
                DecisionReply::Deny => {
                    if crate::mutants::on("sms-permission-confirmation-text") {
                        "已拒绝。"
                    } else {
                        "已拒绝，继续处理。"
                    }
                }
                DecisionReply::AllowSession => "已允许本会话，继续执行。",
                DecisionReply::AllowOnce => "已允许一次，继续执行。",
            };
            Some(PermissionAnswer {
                text: Some(JsString::from(text)),
                can_advance: true,
                retry_prompt: false,
                answer_token: Some(answer_token),
            })
        };
        self.answer_finally(chat_id, generation);
        result
    }

    /// The `finally` block: releases the in-flight marker for the owning
    /// generation, or the generation entry when nothing still holds it.
    fn answer_finally(&mut self, chat_id: &JsString, generation: u32) {
        if self.generations.get(chat_id).copied() == Some(generation) {
            self.in_flight.remove(chat_id);
            self.generations.remove(chat_id);
        } else if !self.pending.contains_key(chat_id)
            && !self.in_flight.contains(chat_id)
            && !self.answering.contains(chat_id)
        {
            self.generations.remove(chat_id);
        }
    }
}

fn status_answer(text: &str, can_advance: bool, retry_prompt: bool) -> PermissionAnswer {
    PermissionAnswer {
        text: Some(JsString::from(text)),
        can_advance,
        retry_prompt,
        answer_token: None,
    }
}

/// `String(x) ?? nothing` event fields: `undefined`/`null` stay absent.
/// #1339 fix-r1: shared with the elicitation capture (same event shape).
pub(crate) fn optional_string(value: &JsValue) -> Option<JsString> {
    match value {
        JsValue::Undefined | JsValue::Null => None,
        value => Some(js_string_of(value)),
    }
}

fn token_matches(
    tokens: &HashMap<JsString, u32>,
    chat_id: &JsString,
    answer_token: Option<u32>,
) -> bool {
    match answer_token {
        None => true,
        Some(token) => tokens.get(chat_id).copied() == Some(token),
    }
}

fn request_id_matches(
    prompts: &HashMap<JsString, PendingPrompt>,
    chat_id: &JsString,
    expected: Option<&JsString>,
) -> bool {
    match prompts.get(chat_id) {
        None => true,
        Some(prompt) => prompt.request_id.as_ref() == expected,
    }
}

/// `JSON.stringify(payload, null, 2)` — 2-space indentation, `None` for a
/// top-level `undefined`.
fn stringify_indented(payload: &JsValue) -> Option<String> {
    fn write(value: &JsValue, depth: usize, out: &mut String) -> Option<()> {
        let pad = "  ".repeat(depth);
        let pad_inner = "  ".repeat(depth + 1);
        match value {
            JsValue::Undefined => return None,
            JsValue::Null => out.push_str("null"),
            JsValue::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
            JsValue::Number(number) => out.push_str(&js_number(*number)),
            JsValue::String(text) => out.push_str(&pilotdeck_protocol::quote_json_string(text)),
            JsValue::Array(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return Some(());
                }
                out.push_str("[\n");
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(",\n");
                    }
                    out.push_str(&pad_inner);
                    if write(item, depth + 1, out).is_none() {
                        out.push_str("null");
                    }
                }
                out.push('\n');
                out.push_str(&pad);
                out.push(']');
            }
            JsValue::Object(object) => {
                let entries: Vec<_> = object
                    .own_entries()
                    .into_iter()
                    .filter(|(_, value)| !matches!(value, JsValue::Undefined))
                    .collect();
                if entries.is_empty() {
                    out.push_str("{}");
                    return Some(());
                }
                out.push_str("{\n");
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push_str(",\n");
                    }
                    out.push_str(&pad_inner);
                    out.push_str(&pilotdeck_protocol::quote_json_string(key));
                    out.push_str(": ");
                    write(value, depth + 1, out);
                }
                out.push('\n');
                out.push_str(&pad);
                out.push('}');
            }
        }
        Some(())
    }
    let mut out = String::new();
    write(payload, 0, &mut out)?;
    Some(out)
}

/// JSON number text; NaN and both infinities serialize as `null`.
fn js_number(value: f64) -> String {
    if value.is_nan() || value.is_infinite() {
        "null".to_owned()
    } else {
        crate::js_value::number_string(value)
    }
}

fn format_payload(payload: &JsValue) -> JsString {
    let text = stringify_indented(payload)
        .unwrap_or_else(|| String::from_utf16_lossy(js_string_of(payload).units()));
    let trimmed = crate::metadata::js_trim(&text);
    let units: Vec<u16> = trimmed.encode_utf16().collect();
    if units.is_empty() {
        return JsString::from("(空)");
    }
    if units.len() <= 800 {
        return JsString::from_utf16(units);
    }
    let mut sliced = String::from_utf16_lossy(&units[..800]);
    sliced.push_str("...");
    JsString::from(sliced)
}

fn format_permission_prompt(entry: &PendingPermission) -> JsString {
    let lines: Vec<JsString> = vec![
        concat_units(&[
            "工具 ".encode_utf16().collect::<Vec<u16>>().as_slice(),
            entry.tool_name.units(),
            " 需要权限才能继续执行。"
                .encode_utf16()
                .collect::<Vec<u16>>()
                .as_slice(),
        ]),
        JsString::from(""),
        JsString::from("请求内容："),
        format_payload(&entry.payload),
        JsString::from(""),
        JsString::from("回复 1 允许一次，回复 2 允许本会话，回复 0 拒绝。"),
    ];
    JsString::from_utf16(join_lines(&lines))
}

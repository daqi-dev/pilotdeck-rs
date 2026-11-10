//! ImPermissionHelper.ts + ImElicitationHelper.ts — the permission lock and
//! elicitation state machine for the Weixin live owner.
//!
//! TypeScript baseline (cc7c4e65):
//! - src/adapters/channel/protocol/ImPermissionHelper.ts
//! - src/adapters/channel/protocol/ImElicitationHelper.ts
//! - src/adapters/channel/weixin/WeixinChannel.ts:452-477 (answer dispatch)
//!   and :722-742 (elicitation dispatch).
//!
//! The RPCs (permission_decide / elicitation_respond) and the chat sends stay in
//! live.rs; this module owns every state transition so the lock can be tested
//! without a gateway.
use super::*;
use crate::metadata::js_trim;
use std::collections::HashSet;

/// ImPermissionHelper.ts:3-8.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PendingPermission {
    pub session_key: String,
    pub request_id: String,
    pub tool_name: String,
    pub payload: Value,
}

/// ImPermissionHelper.ts:10-13.
#[derive(Clone, Debug, PartialEq)]
struct PendingPrompt {
    text: String,
    request_id: String,
}

/// The entry plus token a pending decision must carry across the RPC.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BeginAnswer {
    pub entry: PendingPermission,
    pub token: u64,
    pub generation: u64,
    pub decision: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AnswerResult {
    pub text: String,
    pub can_advance: bool,
    pub retry_prompt: bool,
    pub answer_token: Option<u64>,
}

/// ImPermissionHelper.ts:181-183 — answerWithState returns text-or-undefined;
/// Silent is the undefined case where the message is consumed without a reply.
/// Only Decide authorizes permission_decide.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PermissionAnswer {
    Reply(AnswerResult),
    Decide(BeginAnswer),
    Silent,
}

/// ImPermissionHelper.ts:185-205 — the pre-RPC half of answerWithState.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AnswerStep {
    /// Send this text and consume the inbound message.
    Reply(String),
    /// ImPermissionHelper.ts:192-194 — re-send the queued prompt.
    Retry,
    /// Call permission_decide for this entry.
    Decide(BeginAnswer),
    /// The TS helper returns undefined; the message is consumed with no reply.
    Swallow,
    /// Not a permission answer.
    NotHandled,
}

pub(crate) const PERMISSION_DECIDING: &str = "权限决定处理中，请稍候。";
pub(crate) const PERMISSION_PROMPT_SENDING: &str = "权限提示发送中，请稍候。";
pub(crate) const PERMISSION_RETRY: &str = "上一条权限提示发送失败，正在重试。";
pub(crate) const PERMISSION_CHOICES: &str =
    "请回复 1 允许一次，回复 2 允许本会话，回复 0 拒绝。";
pub(crate) const PERMISSION_EXPIRED: &str = "权限请求已失效，已跳过，继续处理。";

/// ImPermissionHelper.ts:24-35.
#[derive(Debug, Default)]
pub(crate) struct PermissionState {
    pending: HashMap<String, VecDeque<PendingPermission>>,
    next_prompts: HashMap<String, PendingPrompt>,
    prompt_delivering: HashSet<String>,
    retry_prompt_pending: HashSet<String>,
    initial_prompt_pending: HashSet<String>,
    answering: HashSet<String>,
    in_flight: HashSet<String>,
    answer_tokens: HashMap<String, u64>,
    generations: HashMap<String, u64>,
    in_flight_generations: HashMap<String, u64>,
    decisions: HashMap<String, String>,
    deferred_clears: HashSet<String>,
    next_answer_token: u64,
}

impl PermissionState {
    pub(crate) fn new() -> Self {
        Self {
            next_answer_token: 1,
            ..Default::default()
        }
    }

    /// ImPermissionHelper.ts:37-53.
    pub(crate) fn capture(
        &mut self,
        chat: &str,
        session: &str,
        request_id: &str,
        tool_name: &str,
        payload: &Value,
    ) -> Option<String> {
        let entries = self.pending.entry(chat.into()).or_default();
        entries.push_back(PendingPermission {
            session_key: session.into(),
            request_id: request_id.into(),
            tool_name: tool_name.into(),
            payload: payload.clone(),
        });
        let first = entries.len() == 1;
        let entry = entries.front().cloned();
        if !first || self.answering.contains(chat) {
            return None;
        }
        let entry = entry?;
        let prompt = format_permission_prompt(&entry);
        self.answering.insert(chat.into());
        self.next_prompts.insert(
            chat.into(),
            PendingPrompt {
                text: prompt.clone(),
                request_id: entry.request_id.clone(),
            },
        );
        self.initial_prompt_pending.insert(chat.into());
        Some(prompt)
    }

    /// ImPermissionHelper.ts:55-58.
    pub(crate) fn format_pending(&self, chat: &str) -> Option<String> {
        let entry = self.pending.get(chat)?.front()?;
        Some(format_permission_prompt(entry))
    }

    /// ImPermissionHelper.ts:60-62.
    pub(crate) fn has_pending(&self, chat: &str) -> bool {
        self.pending
            .get(chat)
            .is_some_and(|entries| !entries.is_empty())
            || self.answering.contains(chat)
    }

    /// ImPermissionHelper.ts:64-66.
    pub(crate) fn is_answering(&self, chat: &str) -> bool {
        self.answering.contains(chat)
    }

    /// ImPermissionHelper.ts:68-86.
    pub(crate) fn take_next_prompt(&mut self, chat: &str, token: Option<u64>) -> Option<String> {
        if token.is_some_and(|token| self.answer_tokens.get(chat) != Some(&token)) {
            return None;
        }
        if self.prompt_delivering.contains(chat)
            || self.initial_prompt_pending.contains(chat)
            || self.in_flight.contains(chat)
        {
            return None;
        }
        let prompt = match self.next_prompts.get(chat).cloned() {
            Some(prompt) => prompt,
            None => {
                let Some(entry) = self.pending.get(chat).and_then(|entries| entries.front()) else {
                    self.confirm_answer(chat, token);
                    return None;
                };
                let prompt = PendingPrompt {
                    text: format_permission_prompt(entry),
                    request_id: entry.request_id.clone(),
                };
                self.next_prompts.insert(chat.into(), prompt.clone());
                prompt
            }
        };
        self.prompt_delivering.insert(chat.into());
        Some(prompt.text)
    }

    /// ImPermissionHelper.ts:88-91.
    pub(crate) fn get_prompt_request_id(&self, chat: &str, token: Option<u64>) -> Option<String> {
        if token.is_some_and(|token| self.answer_tokens.get(chat) != Some(&token)) {
            return None;
        }
        self.next_prompts
            .get(chat)
            .map(|prompt| prompt.request_id.clone())
    }

    /// ImPermissionHelper.ts:93-106.
    pub(crate) fn confirm_initial_prompt(
        &mut self,
        chat: &str,
        delivered: bool,
        expected: Option<&str>,
    ) {
        if !self.answering.contains(chat) || !self.next_prompts.contains_key(chat) {
            return;
        }
        if expected.is_some_and(|expected| {
            self.next_prompts
                .get(chat)
                .map(|prompt| prompt.request_id.as_str())
                != Some(expected)
        }) {
            return;
        }
        if !delivered {
            self.initial_prompt_pending.remove(chat);
            self.retry_prompt_pending.insert(chat.into());
            return;
        }
        self.next_prompts.remove(chat);
        self.initial_prompt_pending.remove(chat);
        self.retry_prompt_pending.remove(chat);
        self.answering.remove(chat);
        self.finish_deferred_clear(chat);
    }

    /// ImPermissionHelper.ts:108-127.
    pub(crate) fn confirm_next_prompt(
        &mut self,
        chat: &str,
        delivered: bool,
        expected: Option<&str>,
        token: Option<u64>,
    ) {
        if token.is_some_and(|token| self.answer_tokens.get(chat) != Some(&token)) {
            return;
        }
        if !self.prompt_delivering.contains(chat) {
            return;
        }
        if expected.is_some_and(|expected| {
            self.next_prompts
                .get(chat)
                .map(|prompt| prompt.request_id.as_str())
                != Some(expected)
        }) {
            return;
        }
        self.prompt_delivering.remove(chat);
        if !delivered {
            self.retry_prompt_pending.insert(chat.into());
            return;
        }
        self.next_prompts.remove(chat);
        self.retry_prompt_pending.remove(chat);
        self.answering.remove(chat);
        self.answer_tokens.remove(chat);
        self.finish_deferred_clear(chat);
    }

    /// ImPermissionHelper.ts:129-137 — a failed reply keeps the lock so the next
    /// inbound message retries instead of deciding an unseen request.
    pub(crate) fn release_answer(&mut self, chat: &str, token: Option<u64>) {
        if token.is_some_and(|token| self.answer_tokens.get(chat) != Some(&token)) {
            return;
        }
        self.prompt_delivering.remove(chat);
        if self.next_prompts.contains_key(chat) {
            self.retry_prompt_pending.insert(chat.into());
        } else {
            self.confirm_answer(chat, token);
        }
    }

    /// ImPermissionHelper.ts:139-149.
    pub(crate) fn confirm_answer(&mut self, chat: &str, token: Option<u64>) {
        if token.is_some_and(|token| self.answer_tokens.get(chat) != Some(&token)) {
            return;
        }
        if !self.answering.contains(chat)
            || self.in_flight.contains(chat)
            || self.initial_prompt_pending.contains(chat)
        {
            return;
        }
        if self.prompt_delivering.contains(chat) || self.next_prompts.contains_key(chat) {
            return;
        }
        if self
            .pending
            .get(chat)
            .is_some_and(|entries| !entries.is_empty())
        {
            return;
        }
        self.retry_prompt_pending.remove(chat);
        self.answering.remove(chat);
        self.answer_tokens.remove(chat);
        self.finish_deferred_clear(chat);
    }

    /// ImPermissionHelper.ts:151-164.
    pub(crate) fn clear_after_turn(&mut self, chat: &str) {
        let initial_prompt_sending = self.initial_prompt_pending.contains(chat);
        if self.in_flight.contains(chat)
            || self.prompt_delivering.contains(chat)
            || self.retry_prompt_pending.contains(chat)
            || (!initial_prompt_sending && self.answering.contains(chat))
        {
            self.deferred_clears.insert(chat.into());
            return;
        }
        self.clear(chat);
    }

    /// ImPermissionHelper.ts:166-179.
    fn finish_deferred_clear(&mut self, chat: &str) {
        if !self.deferred_clears.contains(chat) {
            return;
        }
        if self
            .pending
            .get(chat)
            .is_some_and(|entries| !entries.is_empty())
            || self.answering.contains(chat)
            || self.in_flight.contains(chat)
            || self.prompt_delivering.contains(chat)
            || self.initial_prompt_pending.contains(chat)
            || self.next_prompts.contains_key(chat)
            || self.retry_prompt_pending.contains(chat)
        {
            return;
        }
        self.deferred_clears.remove(chat);
        self.clear_now(chat);
    }

    /// ImPermissionHelper.ts:185-216 — the pre-RPC half of answerWithState.
    fn answer_step(&mut self, chat: &str, text: &str) -> AnswerStep {
        if self.answering.contains(chat) {
            if self.in_flight.contains(chat) {
                return AnswerStep::Reply(PERMISSION_DECIDING.into());
            }
            if self.initial_prompt_pending.contains(chat) {
                return AnswerStep::Reply(PERMISSION_PROMPT_SENDING.into());
            }
            if self.retry_prompt_pending.contains(chat) {
                return AnswerStep::Retry;
            }
            return AnswerStep::Swallow;
        }
        if self
            .pending
            .get(chat)
            .is_none_or(|entries| entries.is_empty())
        {
            return AnswerStep::NotHandled;
        }
        let trimmed = js_trim(text);
        if trimmed != "0" && trimmed != "1" && trimmed != "2" {
            return AnswerStep::Reply(PERMISSION_CHOICES.into());
        }
        let mut answer = self.begin_answer(chat);
        answer.decision = trimmed.into();
        self.decisions.insert(chat.into(), trimmed.into());
        AnswerStep::Decide(answer)
    }

    /// ImPermissionHelper.ts:204-216. Call only for an unlocked pending queue.
    pub(crate) fn begin_answer(&mut self, chat: &str) -> BeginAnswer {
        assert!(!self.is_answering(chat));
        assert!(self.pending.get(chat).is_some_and(|entries| !entries.is_empty()));
        self.answering.insert(chat.into());
        self.in_flight.insert(chat.into());
        let token = self.next_answer_token;
        self.next_answer_token += 1;
        self.answer_tokens.insert(chat.into(), token);
        let generation = self.generations.get(chat).copied().unwrap_or(0) + 1;
        self.generations.insert(chat.into(), generation);
        self.in_flight_generations.insert(chat.into(), generation);
        self.decisions.insert(chat.into(), "1".into());
        let entry = {
            let entries = self.pending.get_mut(chat).expect("pending checked above");
            let entry = entries.pop_front().expect("pending checked above");
            if entries.is_empty() {
                self.pending.remove(chat);
            }
            entry
        };
        BeginAnswer {
            entry,
            token,
            generation,
            decision: "1".into(),
        }
    }

    /// ImPermissionHelper.ts:185-196. Retry/status replies cannot authorize an RPC.
    pub(crate) fn answer_with_state(&mut self, chat: &str, input: &str) -> Option<PermissionAnswer> {
        let (text, retry_prompt) = match self.answer_step(chat, input) {
            AnswerStep::Reply(text) => (text, false),
            AnswerStep::Retry => (PERMISSION_RETRY.into(), true),
            AnswerStep::Decide(answer) => return Some(PermissionAnswer::Decide(answer)),
            AnswerStep::Swallow => return Some(PermissionAnswer::Silent),
            AnswerStep::NotHandled => return None,
        };
        Some(PermissionAnswer::Reply(AnswerResult {
            text, can_advance: false, retry_prompt, answer_token: None,
        }))
    }

    /// ImPermissionHelper.ts:219-225. Build the RPC payload from the staged input.
    pub(crate) fn decision_params(answer: &BeginAnswer) -> Value {
        let deny = answer.decision == "0";
        let mut params = json!({"sessionKey":answer.entry.session_key,
            "requestId":answer.entry.request_id,
            "decision":if deny { "deny" } else { "allow" }});
        if deny {
            params["reason"] = "User denied permission from IM channel.".into();
        } else {
            params["remember"] = (answer.decision == "2").into();
        }
        params
    }

    /// ImPermissionHelper.ts:219-256 — the post-RPC half of answerWithState.
    pub(crate) fn applied_answer(
        &mut self,
        chat: &str,
        token: u64,
        delivered: bool,
    ) -> Option<String> {
        if self.answer_tokens.get(chat) != Some(&token)
            || !self.in_flight.contains(chat)
            || self.generations.get(chat) != self.in_flight_generations.get(chat) {
            return None;
        }
        self.in_flight.remove(chat);
        self.in_flight_generations.remove(chat);
        self.generations.remove(chat);
        let decision = self.decisions.remove(chat).unwrap_or_else(|| "1".into());
        if !delivered {
            match self
                .pending
                .get(chat)
                .and_then(|entries| entries.front())
                .cloned()
            {
                Some(entry) => {
                    self.next_prompts.insert(
                        chat.into(),
                        PendingPrompt {
                            text: format_permission_prompt(&entry),
                            request_id: entry.request_id,
                        },
                    );
                }
                None => {
                    self.pending.remove(chat);
                    self.next_prompts.remove(chat);
                }
            }
            self.retry_prompt_pending.remove(chat);
            return Some(PERMISSION_EXPIRED.into());
        }
        if let Some(entry) = self
            .pending
            .get(chat)
            .and_then(|entries| entries.front())
            .cloned()
        {
            self.next_prompts.insert(
                chat.into(),
                PendingPrompt {
                    text: format_permission_prompt(&entry),
                    request_id: entry.request_id,
                },
            );
        }
        Some(
            match decision.as_str() {
                "0" => "已拒绝，继续处理。",
                "2" => "已允许本会话，继续执行。",
                _ => "已允许一次，继续执行。",
            }
            .into(),
        )
    }

    /// ImPermissionHelper.ts:257-273 — the RPC-failure rollback.
    pub(crate) fn abort_answer(&mut self, chat: &str, token: u64, entry: PendingPermission) {
        if self.answer_tokens.get(chat) != Some(&token)
            || !self.in_flight.contains(chat)
            || self.generations.get(chat) != self.in_flight_generations.get(chat) {
            return;
        }
        let entries = self.pending.entry(chat.into()).or_default();
        entries.push_front(entry);
        self.next_prompts.remove(chat);
        self.retry_prompt_pending.remove(chat);
        self.answering.remove(chat);
        self.in_flight.remove(chat);
        self.in_flight_generations.remove(chat);
        self.generations.remove(chat);
        self.decisions.remove(chat);
        // TS catch retains the token until the next decision or an explicit clear.
    }

    /// ImPermissionHelper.ts:227-256, including expired decisions' retryPrompt.
    pub(crate) fn complete_answer(&mut self, chat: &str, token: u64, delivered: bool) -> Option<AnswerResult> {
        Some(AnswerResult {
            text: self.applied_answer(chat, token, delivered)?,
            can_advance: delivered, retry_prompt: !delivered, answer_token: Some(token),
        })
    }

    /// ImPermissionHelper.ts:276-295.
    pub(crate) fn clear(&mut self, chat: &str) {
        self.deferred_clears.remove(chat);
        self.clear_now(chat);
    }

    fn clear_now(&mut self, chat: &str) {
        if self.in_flight.contains(chat) {
            let generation = self.generations.entry(chat.into()).or_default();
            *generation += 1;
        } else {
            self.generations.remove(chat);
        }
        self.pending.remove(chat);
        self.next_prompts.remove(chat);
        self.prompt_delivering.remove(chat);
        self.retry_prompt_pending.remove(chat);
        self.initial_prompt_pending.remove(chat);
        self.in_flight.remove(chat);
        self.answer_tokens.remove(chat);
        self.answering.remove(chat);
        self.in_flight_generations.remove(chat);
        self.decisions.remove(chat);
    }

    pub(crate) fn retry_pending(&self, chat: &str) -> bool {
        self.retry_prompt_pending.contains(chat)
    }

    /// Reproduces the pre-merge independent `prompt_retry` flag so the restored
    /// stale-turn assertion can prove a failed prompt stays queued.
    #[cfg(test)]
    pub(crate) fn test_mark_retry(&mut self, chat: &str) {
        self.retry_prompt_pending.insert(chat.into());
    }

    pub(crate) fn deferred_clear_pending(&self, chat: &str) -> bool {
        self.deferred_clears.contains(chat)
    }

    pub(crate) fn queued_permissions(&self, chat: &str) -> usize {
        self.pending.get(chat).map(VecDeque::len).unwrap_or(0)
    }

    /// ImPermissionHelper.ts:47-52 — the head request the prompt text came from.
    pub(crate) fn head_request_id(&self, chat: &str) -> Option<String> {
        self.pending
            .get(chat)
            .and_then(|entries| entries.front())
            .map(|entry| entry.request_id.clone())
    }

    /// ImPermissionHelper.ts:29 — the initial prompt send gate (answering
    /// before the first confirmInitialPrompt lands).
    pub(crate) fn initial_prompt_pending(&self, chat: &str) -> bool {
        self.initial_prompt_pending.contains(chat)
    }
}

/// ImPermissionHelper.ts:298-309 — payload rendering.
fn format_payload(payload: &Value) -> String {
    let text = serde_json::to_string_pretty(payload).unwrap_or_else(|_| payload.to_string());
    let trimmed = text.trim();
    let units: Vec<u16> = trimmed.encode_utf16().collect();
    if units.len() <= 800 {
        return if trimmed.is_empty() {
            "(空)".into()
        } else {
            trimmed.into()
        };
    }
    format!("{}...", String::from_utf16_lossy(&units[..800]))
}

/// ImPermissionHelper.ts:311-320.
pub(crate) fn format_permission_prompt(entry: &PendingPermission) -> String {
    format!(
        "工具 {} 需要权限才能继续执行。\n\n请求内容：\n{}\n\n{}",
        entry.tool_name,
        format_payload(&entry.payload),
        "回复 1 允许一次，回复 2 允许本会话，回复 0 拒绝。"
    )
}

pub(crate) const ELICITATION_CANCELLED: &str = "已取消。";

/// ImElicitationHelper.ts:17-26.
#[derive(Debug, Default)]
pub(crate) struct ElicitationState {
    pending: HashMap<String, (String, Value)>,
}

/// ImElicitationHelper.ts:64-105 — the gateway RPC and the confirmation send
/// stay in live.rs, which is what keeps a failed 「已取消。」 non-fatal.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ElicitationReply {
    pub answer: Value,
    pub confirmation: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Confirmation {
    pub session_key: String,
    pub request_id: String,
    pub answer: Value,
    pub confirmation: Option<String>,
}

/// Both outcomes continue the turn; confirmation delivery cannot produce an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfirmationDelivery {
    ContinueTurn,
    ContinueTurnAfterSendFailure,
}

impl Confirmation {
    /// WeixinChannel.ts:439-442. Pass sendReply's awaited result without `?`.
    pub(crate) fn confirmation_sent<E>(&self, result: std::result::Result<bool, E>) -> ConfirmationDelivery {
        if self.confirmation.is_none() || matches!(result, Ok(true)) {
            ConfirmationDelivery::ContinueTurn
        } else {
            ConfirmationDelivery::ContinueTurnAfterSendFailure
        }
    }
}

impl ElicitationState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// ImElicitationHelper.ts:35-53.
    pub(crate) fn capture(&mut self, chat: &str, session: &str, event: &Value) -> String {
        self.pending
            .insert(chat.into(), (session.into(), event.clone()));
        elicitation_prompt(event)
    }

    /// ImElicitationHelper.ts:55-57.
    pub(crate) fn has_pending(&self, chat: &str) -> bool {
        self.pending.contains_key(chat)
    }

    /// ImElicitationHelper.ts:36-40 — the request the pending question came from.
    pub(crate) fn pending_request_id(&self, chat: &str) -> Option<String> {
        self.pending
            .get(chat)
            .and_then(|(_, event)| event["requestId"].as_str())
            .map(str::to_owned)
    }

    /// ImElicitationHelper.ts:65-67. Consume before parsing or awaiting RPC.
    pub(crate) fn take(&mut self, chat: &str) -> Option<(String, Value)> {
        self.pending.remove(chat)
    }

    /// ImElicitationHelper.ts:64-105. Keeps the RPC binding with its parsed answer.
    pub(crate) fn answer(&mut self, chat: &str, input: &str) -> Option<Confirmation> {
        let (session_key, event) = self.take(chat)?;
        let reply = elicitation_reply(&event, input);
        Some(Confirmation {
            session_key,
            request_id: event["requestId"].as_str().unwrap_or("").into(),
            answer: reply.answer,
            confirmation: reply.confirmation,
        })
    }

    /// ImElicitationHelper.ts:107-109.
    pub(crate) fn clear(&mut self, chat: &str) {
        self.pending.remove(chat);
    }
}

/// ImElicitationHelper.ts:69-104 — pure reply mapping.
pub(crate) fn elicitation_reply(event: &Value, input: &str) -> ElicitationReply {
    let trimmed = js_trim(input);
    if trimmed == "0" {
        return ElicitationReply {
            answer: json!({"type":"cancelled","reason":"user cancelled"}),
            confirmation: Some(ELICITATION_CANCELLED.into()),
        };
    }
    let indices: Vec<usize> = trimmed
        .split(|c: char| c == ',' || c == '，' || crate::metadata::js_space(c))
        .filter_map(parse_decimal_index)
        .collect();
    let mut answers = serde_json::Map::new();
    for question in event["questions"].as_array().into_iter().flatten() {
        let options = question["options"].as_array().cloned().unwrap_or_default();
        let mut selected: Vec<Value> = Vec::new();
        for index in &indices {
            if *index >= 1 && *index <= options.len() {
                if let Some(label) = options[*index - 1].get("label") {
                    selected.push(label.clone());
                }
            }
        }
        if selected.is_empty() {
            selected.push(json!(trimmed));
        }
        let value = if question["multiSelect"] == true {
            Value::Array(selected)
        } else {
            selected[0].clone()
        };
        answers.insert(question["question"].as_str().unwrap_or("").into(), value);
    }
    ElicitationReply {
        answer: json!({"type":"answered","answers":answers}),
        confirmation: None,
    }
}

// Number.parseInt(s, 10) accepts a decimal prefix; negative indices are invalid.
fn parse_decimal_index(text: &str) -> Option<usize> {
    let text = text.strip_prefix('+').unwrap_or(text);
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// ImElicitationHelper.ts:42-52 — prompt text.
pub(crate) fn elicitation_prompt(event: &Value) -> String {
    let mut lines: Vec<String> = Vec::new();
    for question in event["questions"].as_array().into_iter().flatten() {
        if let Some(header) = text(question, "header").filter(|header| !header.is_empty()) {
            lines.push(format!("**{header}**"));
        }
        if let Some(question_text) = text(question, "question").filter(|text| !text.is_empty()) {
            lines.push(question_text.into());
        }
        for (index, option) in question["options"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let description = option["description"]
                .as_str()
                .filter(|description| !description.is_empty())
                .map(|description| format!(" — {description}"))
                .unwrap_or_default();
            lines.push(format!(
                "{}. {}{description}",
                index + 1,
                option["label"].as_str().unwrap_or("")
            ));
        }
    }
    lines.push(String::new());
    lines.push("回复数字选择（多选用逗号分隔），回复 0 取消。".into());
    lines.join("\n")
}

#[cfg(test)]
mod permission_tests {
    use super::*;

    fn capture(state: &mut PermissionState, chat: &str, request: &str) -> Option<String> {
        state.capture(
            chat,
            "session",
            request,
            "write_file",
            &json!({"path":"file"}),
        )
    }

    fn ready(state: &mut PermissionState, chat: &str, request: &str) {
        let prompt = capture(state, chat, request).expect("first permission prompts");
        assert!(prompt.contains("请求内容"));
        state.confirm_initial_prompt(chat, true, Some(request));
        assert!(!state.is_answering(chat));
    }

    // ImPermissionHelper.ts:37-53 — only the first request produces a prompt.
    #[test]
    fn capture_prompts_once_and_formats_the_ts_text() {
        let mut state = PermissionState::new();
        let prompt = capture(&mut state, "chat", "r1").unwrap();
        assert!(prompt.starts_with("工具 write_file 需要权限才能继续执行。\n\n请求内容：\n"));
        assert!(prompt.ends_with("\n\n回复 1 允许一次，回复 2 允许本会话，回复 0 拒绝。"));
        assert_eq!(capture(&mut state, "chat", "r2"), None);
        assert_eq!(state.queued_permissions("chat"), 2);
        assert!(state.is_answering("chat"));
        assert!(state.has_pending("chat"));
    }

    // ImPermissionHelper.ts:68-86 — the initial prompt gate and the in-flight gate
    // both keep takeNextPrompt from consuming a queued prompt.
    #[test]
    fn take_next_prompt_is_blocked_while_initial_or_in_flight() {
        let mut state = PermissionState::new();
        capture(&mut state, "chat", "r1");
        assert_eq!(state.take_next_prompt("chat", None), None);
        state.confirm_initial_prompt("chat", true, Some("r1"));
        capture(&mut state, "chat", "r2");
        let AnswerStep::Decide(answer) = state.answer_step("chat", "1") else {
            panic!("expected a decision");
        };
        assert_eq!(state.take_next_prompt("chat", Some(answer.token)), None);
        assert_eq!(state.take_next_prompt("chat", None), None);
    }

    // ImPermissionHelper.ts:129-137 + 68-86 (八项③) — a failed confirmation keeps
    // the answering lock, and the next inbound text retries instead of deciding r2.
    #[test]
    fn failed_confirmation_keeps_answering_and_retries_next_text() {
        let mut state = PermissionState::new();
        ready(&mut state, "chat", "r1");
        capture(&mut state, "chat", "r2");
        let AnswerStep::Decide(answer) = state.answer_step("chat", "1") else {
            panic!("expected decision");
        };
        let result = state.complete_answer("chat", answer.token, true).unwrap();
        assert_eq!(result.text, "已允许一次，继续执行。");
        assert!(result.can_advance);

        // The confirmation send failed: TS releaseAnswer keeps the lock.
        state.release_answer("chat", Some(answer.token));
        assert!(state.is_answering("chat"));
        assert!(state.retry_pending("chat"));

        // The next inbound message must retry, never decide r2.
        assert_eq!(state.answer_step("chat", "1"), AnswerStep::Retry);
        assert_eq!(state.queued_permissions("chat"), 1);
        assert_eq!(
            state.get_prompt_request_id("chat", None).as_deref(),
            Some("r2")
        );

        // Retrying the queued prompt and delivering it releases the lock.
        let prompt = state.take_next_prompt("chat", None).expect("queued prompt");
        assert!(prompt.contains("请求内容"));
        state.confirm_next_prompt("chat", true, Some("r2"), None);
        assert!(!state.is_answering("chat"));
        assert!(!state.retry_pending("chat"));
    }

    // ImPermissionHelper.ts:108-127 (八项③) — a failed next prompt keeps the lock.
    #[test]
    fn failed_next_prompt_keeps_answering_and_retries() {
        let mut state = PermissionState::new();
        ready(&mut state, "chat", "r1");
        capture(&mut state, "chat", "r2");
        let AnswerStep::Decide(answer) = state.answer_step("chat", "2") else {
            panic!("expected decision");
        };
        assert_eq!(
            state
                .complete_answer("chat", answer.token, true)
                .unwrap()
                .text,
            "已允许本会话，继续执行。"
        );
        let prompt = state.take_next_prompt("chat", None).expect("queued prompt");
        assert!(prompt.contains("请求内容"));
        let request_id = state.get_prompt_request_id("chat", None).unwrap();
        state.confirm_next_prompt("chat", false, Some(&request_id), None);
        assert!(state.is_answering("chat"));
        assert!(state.retry_pending("chat"));
        assert_eq!(state.answer_step("chat", "1"), AnswerStep::Retry);
    }

    // ImPermissionHelper.ts:108-127 — a mismatched request id must not release.
    #[test]
    fn confirm_next_prompt_requires_the_matching_request_id() {
        let mut state = PermissionState::new();
        ready(&mut state, "chat", "r1");
        capture(&mut state, "chat", "r2");
        let AnswerStep::Decide(answer) = state.answer_step("chat", "1") else {
            panic!("expected decision");
        };
        state.complete_answer("chat", answer.token, true).unwrap();
        let prompt = state.take_next_prompt("chat", None).expect("queued prompt");
        assert!(prompt.contains("请求内容"));
        state.confirm_next_prompt("chat", true, Some("wrong-request"), None);
        assert!(state.is_answering("chat"));
        assert_eq!(
            state.get_prompt_request_id("chat", None).as_deref(),
            Some("r2")
        );
    }

    // ImPermissionHelper.ts:139-149 — no queued prompt means the answer releases.
    #[test]
    fn release_answer_without_a_next_prompt_confirms_the_answer() {
        let mut state = PermissionState::new();
        ready(&mut state, "chat", "r1");
        let AnswerStep::Decide(answer) = state.answer_step("chat", "0") else {
            panic!("expected decision");
        };
        assert_eq!(
            state
                .complete_answer("chat", answer.token, true)
                .unwrap()
                .text,
            "已拒绝，继续处理。"
        );
        state.release_answer("chat", Some(answer.token));
        assert!(!state.is_answering("chat"));
        assert!(!state.has_pending("chat"));
    }

    // ImPermissionHelper.ts:151-179 — clearAfterTurn defers while a decision runs.
    #[test]
    fn clear_after_turn_defers_until_the_answer_delivers() {
        let mut state = PermissionState::new();
        ready(&mut state, "chat", "r1");
        let AnswerStep::Decide(answer) = state.answer_step("chat", "1") else {
            panic!("expected decision");
        };
        state.clear_after_turn("chat");
        assert!(state.deferred_clear_pending("chat"));
        state.complete_answer("chat", answer.token, true).unwrap();
        assert!(state.deferred_clear_pending("chat"));
        assert_eq!(state.take_next_prompt("chat", Some(answer.token)), None);
        assert!(!state.is_answering("chat"));
        assert!(!state.deferred_clear_pending("chat"));
    }

    // ImPermissionHelper.ts:199-202 — illegal input never mutates the queue.
    #[test]
    fn illegal_input_replies_with_the_choices_and_keeps_the_request() {
        let mut state = PermissionState::new();
        ready(&mut state, "chat", "r1");
        assert_eq!(
            state.answer_step("chat", "yes"),
            AnswerStep::Reply(PERMISSION_CHOICES.into())
        );
        assert_eq!(state.queued_permissions("chat"), 1);
        assert!(!state.is_answering("chat"));
    }

    // ImPermissionHelper.ts:190-191 — the prompt-sending status text.
    #[test]
    fn answering_while_the_initial_prompt_sends_reports_the_ts_status() {
        let mut state = PermissionState::new();
        capture(&mut state, "chat", "r1");
        assert_eq!(
            state.answer_step("chat", "1"),
            AnswerStep::Reply(PERMISSION_PROMPT_SENDING.into())
        );
        assert_eq!(state.queued_permissions("chat"), 1);
    }

    // ImPermissionHelper.ts:257-273 — the RPC failure puts the entry back.
    #[test]
    fn aborted_decision_restores_the_request_and_clears_the_lock() {
        let mut state = PermissionState::new();
        ready(&mut state, "chat", "r1");
        capture(&mut state, "chat", "r2");
        let AnswerStep::Decide(answer) = state.answer_step("chat", "1") else {
            panic!("expected decision");
        };
        let token = answer.token;
        let entry = answer.entry.clone();
        state.abort_answer("chat", token, entry);
        assert_eq!(state.queued_permissions("chat"), 2);
        assert!(state.format_pending("chat").unwrap().contains("请求内容"));
        assert!(!state.is_answering("chat"));
    }

    // ImPermissionHelper.ts:219-246 — an invalidated request rewrites the head and
    // reports retryPrompt without advancing the caller.
    #[test]
    fn invalidated_decision_reports_expired_and_repoints_the_prompt() {
        let mut state = PermissionState::new();
        ready(&mut state, "chat", "r1");
        capture(&mut state, "chat", "r2");
        let AnswerStep::Decide(answer) = state.answer_step("chat", "1") else {
            panic!("expected decision");
        };
        let result = state.complete_answer("chat", answer.token, false).unwrap();
        assert_eq!(result.text, PERMISSION_EXPIRED);
        assert!(!result.can_advance);
        assert!(result.retry_prompt);
        assert_eq!(state.queued_permissions("chat"), 1);
        assert_eq!(
            state
                .get_prompt_request_id("chat", Some(answer.token))
                .as_deref(),
            Some("r2")
        );
        assert!(!state.retry_pending("chat"));
    }

    // ImPermissionHelper.ts:219-225 — the RPC payload mirrors the staged decision.
    #[test]
    fn decision_params_mirror_the_staged_answer() {
        let mut state = PermissionState::new();
        ready(&mut state, "chat", "r1");
        let AnswerStep::Decide(answer) = state.answer_step("chat", "2") else {
            panic!("expected decision");
        };
        assert_eq!(
            PermissionState::decision_params(&answer),
            json!({"sessionKey":"session","requestId":"r1","decision":"allow","remember":true})
        );
        let mut state = PermissionState::new();
        ready(&mut state, "chat", "r1");
        let AnswerStep::Decide(answer) = state.answer_step("chat", "0") else {
            panic!("expected decision");
        };
        assert_eq!(
            PermissionState::decision_params(&answer),
            json!({"sessionKey":"session","requestId":"r1","decision":"deny","reason":"User denied permission from IM channel."})
        );
    }

    // ImElicitationHelper.ts:42-52 — prompt text matches the TS helper.
    #[test]
    fn elicitation_prompt_lists_headers_questions_and_options() {
        let event = json!({"requestId":"q","questions":[{"header":"Header","question":"Choose","options":[{"label":"A","description":"first"},{"label":"B"}],"multiSelect":true}]});
        assert_eq!(
            elicitation_prompt(&event),
            "**Header**\nChoose\n1. A — first\n2. B\n\n回复数字选择（多选用逗号分隔），回复 0 取消。"
        );
    }

    // ImElicitationHelper.ts:71-78 + WeixinChannel.ts:439-442 (八项④) — "0" cancels,
    // and a failed 已取消 send still continues the turn.
    #[test]
    fn elicitation_zero_cancels_and_a_failed_confirmation_does_not_error() {
        let mut state = ElicitationState::new();
        let event = json!({"requestId":"q","questions":[{"question":"Choose","options":[{"label":"A"}]}]});
        state.capture("chat", "session", &event);
        let confirmation = state.answer("chat", "0").unwrap();
        assert_eq!(confirmation.request_id, "q");
        assert_eq!(confirmation.session_key, "session");
        assert_eq!(
            confirmation.answer,
            json!({"type":"cancelled","reason":"user cancelled"})
        );
        assert_eq!(
            confirmation.confirmation.as_deref(),
            Some(ELICITATION_CANCELLED)
        );
        assert_eq!(
            confirmation.confirmation_sent(Ok::<bool, String>(false)),
            ConfirmationDelivery::ContinueTurnAfterSendFailure
        );
        assert_eq!(
            confirmation.confirmation_sent(Err::<bool, String>("send failed".into())),
            ConfirmationDelivery::ContinueTurnAfterSendFailure
        );
        assert!(!state.has_pending("chat"));
    }

    // ImElicitationHelper.ts:80-104 — multi-select, raw text and bad indices.
    #[test]
    fn elicitation_answers_map_indices_and_fall_back_to_raw_text() {
        let event = json!({"requestId":"q","questions":[{"question":"Choose","options":[{"label":"A"},{"label":"B"}],"multiSelect":true}]});
        assert_eq!(
            elicitation_reply(&event, "2，1").answer,
            json!({"type":"answered","answers":{"Choose":["B","A"]}})
        );
        assert_eq!(
            elicitation_reply(&event, "free text").answer,
            json!({"type":"answered","answers":{"Choose":["free text"]}})
        );
        assert_eq!(
            elicitation_reply(&event, "9").answer,
            json!({"type":"answered","answers":{"Choose":["9"]}})
        );
        let single = json!({"requestId":"q","questions":[{"question":"Pick","options":[{"label":"A"},{"label":"B"}]}]});
        assert_eq!(
            elicitation_reply(&single, "2").answer,
            json!({"type":"answered","answers":{"Pick":"B"}})
        );
        assert_eq!(
            elicitation_reply(&single, " 1 ").answer,
            json!({"type":"answered","answers":{"Pick":"A"}})
        );
    }

    // ImElicitationHelper.ts:107-109 — /new drops the pending question.
    #[test]
    fn elicitation_clear_drops_the_pending_question() {
        let mut state = ElicitationState::new();
        let event = json!({"requestId":"q","questions":[{"question":"Choose","options":[]}]});
        state.capture("chat", "session", &event);
        state.clear("chat");
        assert!(!state.has_pending("chat"));
        assert!(state.answer("chat", "1").is_none());
    }
}




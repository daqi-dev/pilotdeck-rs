// TS WeComChannel.ts:600-640, ImPermissionHelper and ImElicitationHelper.
use super::*;
use std::collections::VecDeque;

#[derive(Default)]
pub(super) struct Chat {
    pub elicitation: Option<(String, Value)>,
    pending: VecDeque<(String, Value)>,
    answering: bool,
    initial: bool,
    in_flight: bool,
    prompt_sending: bool,
    retry: Option<String>,
    next_prompt: Option<String>,
    deferred_clear: bool,
}
impl Chat {
    pub fn finish_turn(&mut self) {
        self.elicitation = None;
        if self.in_flight
            || self.prompt_sending
            || self.retry.is_some()
            || (self.answering && !self.initial)
        {
            self.deferred_clear = true;
        } else {
            self.clear();
        }
    }
    fn clear(&mut self) {
        self.pending.clear();
        self.answering = false;
        self.initial = false;
        self.retry = None;
        self.next_prompt = None;
        self.deferred_clear = false;
    }
    fn finish_answer(&mut self) {
        self.answering = false;
        self.retry = None;
        self.next_prompt = None;
        if self.deferred_clear && self.pending.is_empty() {
            self.clear();
        }
    }
}
impl WeComChannel {
    pub(super) async fn capture_permission(&self, chat: &str, session: &str, event: Value) {
        let state = self.chat(chat);
        let prompt = {
            let mut state = state.lock().await;
            state.pending.push_back((session.into(), event.clone()));
            if state.pending.len() != 1 || state.answering {
                None
            } else {
                state.answering = true;
                state.initial = true;
                let prompt = permission_prompt(&event);
                state.next_prompt = Some(prompt.clone());
                Some(prompt)
            }
        };
        if let Some(prompt) = prompt {
            let delivered = self.send_reply(chat, &prompt).await;
            let mut state = state.lock().await;
            if !state.initial {
                return;
            }
            state.initial = false;
            if delivered {
                state.finish_answer();
            } else {
                state.retry = Some(prompt);
            }
        }
    }
    pub(super) async fn answer(&self, chat: &str, input: &str) -> Result<bool> {
        let shared = self.chat(chat);
        let mut state = shared.lock().await;
        if let Some((session, event)) = state.elicitation.take() {
            drop(state);
            let answer = elicitation_answer(&event, input);
            let result = self
                .gateway
                .request(
                    "elicitation_respond",
                    json!({"sessionKey":session,"requestId":event["requestId"],"answer":answer}),
                )
                .await;
            if result.is_err() {
                return Ok(true);
            }
            if crate::metadata::js_trim(input) == "0" {
                self.send_reply(chat, "已取消。").await;
            }
            return Ok(true);
        }
        if state.answering {
            let status = if state.in_flight {
                Some("权限决定处理中，请稍候。")
            } else if state.initial {
                Some("权限提示发送中，请稍候。")
            } else if state.retry.is_some() {
                Some("上一条权限提示发送失败，正在重试。")
            } else {
                None
            };
            let retry = state.retry.is_some();
            drop(state);
            if let Some(status) = status {
                if self.send_reply(chat, status).await && retry {
                    self.next_prompt(chat).await;
                }
            }
            return Ok(true);
        }
        let Some((session, event)) = state.pending.pop_front() else {
            return Ok(false);
        };
        let input = crate::metadata::js_trim(input);
        if !matches!(input, "0" | "1" | "2") {
            state.pending.push_front((session, event));
            drop(state);
            self.send_reply(chat, "请回复 1 允许一次，回复 2 允许本会话，回复 0 拒绝。")
                .await;
            return Ok(true);
        }
        state.answering = true;
        state.in_flight = true;
        drop(state);
        let mut params = json!({"sessionKey":session,"requestId":event["requestId"],"decision":if input=="0"{"deny"}else{"allow"}});
        if input == "0" {
            params["reason"] = "User denied permission from IM channel.".into();
        } else {
            params["remember"] = json!(input == "2");
        }
        let result = self.gateway.request("permission_decide", params).await;
        let mut state = shared.lock().await;
        state.in_flight = false;
        let result = match result {
            Ok(result) => result,
            Err(_) => {
                state.pending.push_front((session, event));
                state.answering = false;
                state.retry = None;
                state.next_prompt = None;
                return Ok(true);
            }
        };
        state.next_prompt = state
            .pending
            .front()
            .map(|(_, event)| permission_prompt(event));
        drop(state);
        let confirmation = if result["delivered"] != true {
            "权限请求已失效，已跳过，继续处理。"
        } else {
            match input {
                "0" => "已拒绝，继续处理。",
                "2" => "已允许本会话，继续执行。",
                _ => "已允许一次，继续执行。",
            }
        };
        if self.send_reply(chat, confirmation).await {
            self.next_prompt(chat).await;
        } else {
            let mut state = shared.lock().await;
            if let Some(prompt) = state.next_prompt.clone() {
                state.retry = Some(prompt);
            } else {
                state.finish_answer();
            }
        }
        Ok(true)
    }
    async fn next_prompt(&self, chat: &str) {
        let shared = self.chat(chat);
        let prompt = {
            let mut state = shared.lock().await;
            if state.in_flight || state.initial || state.prompt_sending {
                return;
            }
            let prompt = state.next_prompt.clone().or_else(|| {
                state
                    .pending
                    .front()
                    .map(|(_, event)| permission_prompt(event))
            });
            if prompt.is_some() {
                state.prompt_sending = true;
            } else {
                state.finish_answer();
            }
            prompt
        };
        if let Some(prompt) = prompt {
            let delivered = self.send_reply(chat, &prompt).await;
            let mut state = shared.lock().await;
            state.prompt_sending = false;
            if delivered {
                state.finish_answer();
            } else {
                state.retry = Some(prompt);
            }
        }
    }
}
fn permission_prompt(event: &Value) -> String {
    let payload =
        serde_json::to_string_pretty(&event["payload"]).unwrap_or_else(|_| "undefined".into());
    let payload = payload.trim();
    let units = payload.encode_utf16().collect::<Vec<_>>();
    let payload = if units.len() > 800 {
        format!("{}...", String::from_utf16_lossy(&units[..800]))
    } else if units.is_empty() {
        "(空)".into()
    } else {
        payload.into()
    };
    format!(
        "工具 {} 需要权限才能继续执行。\n\n请求内容：\n{payload}\n\n回复 1 允许一次，回复 2 允许本会话，回复 0 拒绝。",
        event["toolName"].as_str().unwrap_or("")
    )
}
pub(super) fn elicitation_prompt(event: &Value) -> String {
    let mut lines = vec![];
    for q in event["questions"].as_array().into_iter().flatten() {
        if let Some(header) = q["header"].as_str().filter(|s| !s.is_empty()) {
            lines.push(format!("**{header}**"));
        }
        if let Some(question) = q["question"].as_str().filter(|s| !s.is_empty()) {
            lines.push(question.into());
        }
        for (i, option) in q["options"].as_array().into_iter().flatten().enumerate() {
            lines.push(format!(
                "{}. {}{}",
                i + 1,
                option["label"].as_str().unwrap_or(""),
                option["description"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(|s| format!(" — {s}"))
                    .unwrap_or_default()
            ));
        }
    }
    lines.extend([
        String::new(),
        "回复数字选择（多选用逗号分隔），回复 0 取消。".into(),
    ]);
    lines.join("\n")
}
fn elicitation_answer(event: &Value, input: &str) -> Value {
    let input = crate::metadata::js_trim(input);
    if input == "0" {
        return json!({"type":"cancelled","reason":"user cancelled"});
    }
    let mut answers = serde_json::Map::new();
    for q in event["questions"].as_array().into_iter().flatten() {
        let mut selected = input
            .split(|c: char| c == ',' || c == '，' || crate::metadata::js_space(c))
            .filter_map(|part| {
                let part = part.trim_start();
                if part.starts_with('-') {
                    return None;
                }
                let digits = part
                    .strip_prefix('+')
                    .unwrap_or(part)
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>();
                let index = digits.parse::<usize>().ok()?;
                if index == 0 {
                    return None;
                }
                q["options"].get(index - 1)?.get("label").cloned()
            })
            .collect::<Vec<_>>();
        if selected.is_empty() {
            selected.push(input.into());
        }
        answers.insert(
            q["question"].as_str().unwrap_or("").into(),
            if q["multiSelect"] == true {
                json!(selected)
            } else {
                selected[0].clone()
            },
        );
    }
    json!({"type":"answered","answers":answers})
}

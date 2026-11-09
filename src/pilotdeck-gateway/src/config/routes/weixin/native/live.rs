//! Live per-chat ownership: polling keeps accepting answers while a turn waits.
use super::*;
use crate::config::channel_commands::ChannelEventSink;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex as StdMutex;

#[derive(Default)]
struct Chat {
    generation: u64,
    active_session: Option<String>,
    context: Option<String>,
    permissions: VecDeque<(String, Value)>,
    elicitation: Option<(String, Value)>,
    changed: Arc<tokio::sync::Notify>,
    answering: bool,
    initial_prompt: bool,
    prompt_retry: Option<String>,
    deferred_clear: bool,
}
impl Chat {
    fn finish_answer(&mut self) {
        self.answering = false;
        if self.deferred_clear && self.permissions.is_empty() && self.prompt_retry.is_none() {
            self.deferred_clear = false;
            self.permissions.clear();
            self.initial_prompt = false;
        }
    }
    /// The generation fence and its state mutation form one locked transition.
    fn finish_turn(&mut self, generation: u64) -> bool {
        if self.generation != generation {
            return false;
        }
        self.active_session = None;
        self.elicitation = None;
        if (self.answering && !self.initial_prompt) || self.prompt_retry.is_some() {
            self.deferred_clear = true;
        } else {
            self.permissions.clear();
            self.answering = false;
            self.initial_prompt = false;
            self.prompt_retry = None;
        }
        true
    }
}
pub(super) struct LiveOwner {
    storage: Storage,
    http: Http,
    gateway: Arc<dyn ChannelGateway>,
    mapper: StdMutex<ChannelSessionState>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    queues: StdMutex<HashMap<String, Arc<Mutex<()>>>>,
    delivery: Arc<super::WeixinDelivery>,
}
impl LiveOwner {
    pub fn new(
        storage: Storage,
        http: Http,
        gateway: Arc<dyn ChannelGateway>,
        mapper: ChannelSessionState,
        delivery: Arc<super::WeixinDelivery>,
    ) -> Self {
        Self {
            storage,
            http,
            gateway,
            mapper: StdMutex::new(mapper),
            chats: Default::default(),
            queues: Default::default(),
            delivery,
        }
    }
    fn chat(&self, id: &str) -> Arc<Mutex<Chat>> {
        self.chats
            .lock()
            .unwrap()
            .entry(id.into())
            .or_default()
            .clone()
    }
    fn queue(&self, id: &str) -> Arc<Mutex<()>> {
        self.queues
            .lock()
            .unwrap()
            .entry(id.into())
            .or_default()
            .clone()
    }
    fn save_mapper(&self) -> Result<()> {
        self.storage
            .save_mapper(&self.mapper.lock().unwrap().snapshot())
    }
    fn receipt_path(&self, id: &str, index: usize) -> std::path::PathBuf {
        self.storage
            .home
            .join("channels/weixin-inbox")
            .join(format!("{id}-{index}.receipt"))
    }
    fn save_receipt(&self, path: &std::path::Path, value: &Value) -> Result<()> {
        super::super::storage::write(path, value)
    }
    pub(crate) async fn deliver_outbox(
        &self,
        credentials: &Value,
        chat: &str,
        path: &std::path::Path,
        receipt: &mut Value,
    ) -> Result<()> {
        let outbox = receipt["outbox"].as_array().cloned().unwrap_or_default();
        let start = receipt["sentIndex"].as_u64().unwrap_or(0) as usize;
        let mut sent = start;
        for (index, text) in outbox.iter().enumerate().skip(start) {
            let context = self.chat(chat).lock().await.context.clone();
            let body = text.as_str().unwrap_or("");
            match super::send_reply(
                &self.storage,
                &self.http,
                credentials,
                chat,
                context.as_deref(),
                body,
            )
            .await
            {
                Ok(()) => {
                    sent = index + 1;
                    receipt["sentIndex"] = json!(sent);
                    self.save_receipt(path, receipt)?;
                }
                Err(error) => {
                    self.delivery.queue_pending_reply(chat, body);
                    eprintln!("weixin: outbox reply queued after failure: {}", error.message);
                }
            }
        }
        if sent >= outbox.len() {
            receipt["complete"] = true.into();
        }
        self.save_receipt(path, receipt)
    }
    async fn reply(
        &self,
        credentials: &Value,
        chat: &str,
        text: &str,
        queue_on_failure: bool,
    ) -> Result<()> {
        let context = self.chat(chat).lock().await.context.clone();
        super::send_reply_with_queue(
            &self.storage,
            &self.http,
            &self.delivery,
            credentials,
            chat,
            context.as_deref(),
            text,
            queue_on_failure,
        )
        .await
    }
    pub async fn process_batch(
        self: &Arc<Self>,
        credentials: &Value,
        mut batch: Value,
    ) -> Result<()> {
        let id = text(&batch, "id")
            .ok_or_else(|| failure("Invalid Weixin batch"))?
            .to_owned();
        if id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(failure("Invalid Weixin inbox id"));
        }
        let path = self
            .storage
            .home
            .join("channels/weixin-inbox")
            .join(format!("{id}.json"));
        let messages = batch["messages"].as_array().cloned().unwrap_or_default();
        if let Some(saved) = batch.get("mapper") {
            let restored = ChannelSessionState::from_value(saved);
            let mut current = self.mapper.lock().unwrap();
            for message in &messages {
                if let Some(chat) = text(message, "from_user_id") {
                    if let Some(value) = restored.active_by_chat_id.get(chat) {
                        current
                            .active_by_chat_id
                            .entry(chat.into())
                            .or_insert_with(|| value.clone());
                    }
                    if let Some(value) = restored.project_by_chat_id.get(chat) {
                        current
                            .project_by_chat_id
                            .entry(chat.into())
                            .or_insert_with(|| value.clone());
                    }
                }
            }
        }
        let start = batch["nextIndex"].as_u64().unwrap_or(0) as usize;
        let mut workers = tokio::task::JoinSet::new();
        for (index, message) in messages.into_iter().enumerate().skip(start) {
            let owner = self.clone();
            let credentials = credentials.clone();
            let id = id.clone();
            workers.spawn(async move {
                let result = owner.message(&credentials, &id, index, &message).await;
                (index, result)
            });
            // Let an earlier ordinary message acquire its chat queue before
            // dispatching the next item. Answers/new do not enter that queue.
            tokio::task::yield_now().await;
        }
        let mut completed = std::collections::BTreeSet::new();
        let mut prefix = start;
        while let Some(result) = workers.join_next().await {
            let (index, result) = result
                .map_err(|error| failure(&format!("Weixin message worker failed: {error}")))?;
            result?;
            completed.insert(index);
            while completed.remove(&prefix) {
                prefix += 1;
            }
            batch["nextIndex"] = json!(prefix);
            super::super::storage::write(&path, &batch)?;
        }
        self.save_mapper()?;
        super::super::inbox::WeixinInbox::new(self.storage.home.clone()).acknowledge(&id, None)
    }
    async fn message(
        &self,
        credentials: &Value,
        id: &str,
        index: usize,
        message: &Value,
    ) -> Result<()> {
        if message["message_type"] != 1 {
            return Ok(());
        }
        let Some(chat_id) = text(message, "from_user_id") else {
            return Ok(());
        };
        let Some((mut input, mut attachments)) =
            crate::config::routes::weixin::native::weixin_inbound_text_and_attachments(message)
        else {
            return Ok(());
        };
        if !attachments.is_empty() {
            let message_id = message
                .get("message_id")
                .or_else(|| message.get("client_id"))
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .unwrap_or(id);
            let materialize_diags = super::super::attachments::materialize_inbound(
                &self.storage.home,
                chat_id,
                message_id,
                &mut attachments,
            )
            .await?;
            input = super::super::attachments::merge_attachment_diagnostics(input, &materialize_diags);
        }
        let chat = self.chat(chat_id);
        if let Some(context) = text(message, "context_token").filter(|context| !context.is_empty()) {
            chat.lock().await.context = Some(context.into());
            self.delivery
                .contexts
                .lock()
                .unwrap()
                .insert(chat_id.into(), context.into());
        }
        if input.is_empty() && attachments.is_empty() {
            return Ok(());
        }
        let receipt_path = self.receipt_path(id, index);
        let mut receipt =
            super::super::storage::read_checked(&receipt_path)?.unwrap_or_else(|| json!({}));
        if receipt["complete"] == true {
            return Ok(());
        }
        if receipt.get("outbox").is_some() {
            return self
                .deliver_outbox(credentials, chat_id, &receipt_path, &mut receipt)
                .await;
        }
        let is_new = input == "/new" || input.starts_with("/new ");
        let mut queued_generation = chat.lock().await.generation;
        if is_new {
            if let Some(saved) = receipt.get("resolvedNew").and_then(Value::as_str) {
                self.mapper
                    .lock()
                    .unwrap()
                    .active_by_chat_id
                    .insert(chat_id.into(), saved.into());
                self.save_mapper()?;
            } else {
                let previous = {
                    let mut state = chat.lock().await;
                    state.generation += 1;
                    state.permissions.clear();
                    state.elicitation = None;
                    state.answering = false;
                    state.initial_prompt = false;
                    state.prompt_retry = None;
                    state.deferred_clear = false;
                    state.changed.notify_waiters();
                    state.active_session.take()
                };
                self.delivery.clear_pending_replies(chat_id);
                let previous = previous
                    .unwrap_or_else(|| self.mapper.lock().unwrap().session("weixin", chat_id));
                let _ = self
                    .gateway
                    .request("abort_turn", json!({"sessionKey":previous}))
                    .await;
                self.mapper.lock().unwrap().reset("weixin", chat_id);
                self.save_mapper()?;
                receipt["resolvedNew"] = self
                    .mapper
                    .lock()
                    .unwrap()
                    .session("weixin", chat_id)
                    .into();
                self.save_receipt(&receipt_path, &receipt)?;
            }
            if input == "/new" {
                // `WeixinChannel.ts:431` — no queueOnFailure for session confirmation.
                let context = chat.lock().await.context.clone();
                let _ = super::send_reply(
                    &self.storage,
                    &self.http,
                    credentials,
                    chat_id,
                    context.as_deref(),
                    "已创建新会话。",
                )
                .await;
                receipt["outbox"] = json!(["已创建新会话。"]);
                receipt["sentIndex"] = json!(1);
                receipt["complete"] = true.into();
                self.save_receipt(&receipt_path, &receipt)?;
                return Ok(());
            }
            queued_generation = chat.lock().await.generation;
        } else if self
            .answer(credentials, chat_id, input.as_str(), queued_generation)
            .await?
        {
            return Ok(());
        }
        let queue = self.queue(chat_id);
        let _queue = loop {
            let changed = chat.lock().await.changed.clone();
            let notified = changed.notified();
            tokio::pin!(notified);
            // Enable before checking pending state to avoid losing a prompt
            // transition between answer() and waiting on the queue.
            notified.as_mut().enable();
            if chat.lock().await.generation != queued_generation {
                receipt["complete"] = true.into();
                self.save_receipt(&receipt_path, &receipt)?;
                return Ok(());
            }
            if !is_new
                && self
                    .answer(credentials, chat_id, input.as_str(), queued_generation)
                    .await?
            {
                return Ok(());
            }
            tokio::select! {
                queue=queue.lock()=>break queue,
                _=&mut notified=>{},
            }
        };
        if chat.lock().await.generation != queued_generation {
            receipt["complete"] = true.into();
            self.save_receipt(&receipt_path, &receipt)?;
            return Ok(());
        }
        let prompt = if is_new {
            input[4..].trim().to_owned()
        } else {
            input
        };
        let prompt = if prompt.is_empty() && !attachments.is_empty() {
            "请查看我发送的附件。".to_owned()
        } else {
            prompt
        };
        let capture = CapturedReply::default();
        let mut mapper = self.mapper.lock().unwrap().clone();
        let mut ctx = CommandContext {
            gateway: self.gateway.as_ref(),
            reply: &capture,
            state: &mut mapper,
            channel: "weixin",
            chat: chat_id,
            home: &self.storage.home,
        };
        if execute_channel_command(&prompt, &mut ctx).await? {
            // Only copy this chat's entries. Other chats may have changed while
            // the command's gateway RPC was in flight.
            {
                let mut current = self.mapper.lock().unwrap();
                if let Some(value) = mapper.active_by_chat_id.get(chat_id) {
                    current
                        .active_by_chat_id
                        .insert(chat_id.into(), value.clone());
                }
                if let Some(value) = mapper.project_by_chat_id.get(chat_id) {
                    current
                        .project_by_chat_id
                        .insert(chat_id.into(), value.clone());
                }
            }
            self.save_mapper()?;
            let replies = capture.0.lock().unwrap().clone();
            receipt["outbox"] = json!(replies);
            self.save_receipt(&receipt_path, &receipt)?;
            return self
                .deliver_outbox(credentials, chat_id, &receipt_path, &mut receipt)
                .await;
        }
        let (session, project) = {
            let mapper = self.mapper.lock().unwrap();
            (
                mapper.session("weixin", chat_id),
                mapper.project(chat_id).map(str::to_owned),
            )
        };
        let generation = {
            let mut state = chat.lock().await;
            state.active_session = Some(session.clone());
            state.generation
        };
        let mut params = json!({"sessionKey":session,"channelKey":"weixin","runId":format!("weixin:{id}:{index}"),"message":prompt,"allowPlanModeTools":false});
        if let Some(project) = project {
            params["projectKey"] = project.into();
        }
        if !attachments.is_empty() {
            params["attachments"] = serde_json::Value::Array(attachments);
        }
        let sink = LiveSink {
            owner: self,
            credentials,
            chat: chat_id,
            session: &session,
            generation,
            text: StdMutex::new(String::new()),
        };
        let result = self.gateway.stream_turn(params, &sink).await;
        let current = chat.lock().await.finish_turn(generation);
        if current {
            if result.is_err() {
                receipt["outbox"] =
                    json!(["Failed to process this message. Please retry."]);
                self.save_receipt(&receipt_path, &receipt)?;
                return self
                    .deliver_outbox(credentials, chat_id, &receipt_path, &mut receipt)
                    .await;
            }
            let text = sink.text.lock().unwrap().clone();
            receipt["outbox"] = if text.is_empty() {
                json!([])
            } else {
                json!([text])
            };
            self.save_receipt(&receipt_path, &receipt)?;
            self.deliver_outbox(credentials, chat_id, &receipt_path, &mut receipt)
                .await?;
        } else {
            receipt["complete"] = true.into();
            self.save_receipt(&receipt_path, &receipt)?;
        }
        result.map(|()| ())
    }
    async fn answer(
        &self,
        credentials: &Value,
        chat_id: &str,
        input: &str,
        generation: u64,
    ) -> Result<bool> {
        let chat = self.chat(chat_id);
        let mut state = chat.lock().await;
        // An input queued before /new must never answer the replacement turn.
        if state.generation != generation {
            return Ok(true);
        }
        if state.answering {
            let initial = state.initial_prompt;
            let retry = state.prompt_retry.take();
            drop(state);
            if let Some(prompt) = retry {
                let result = self
                    .reply(
                        credentials,
                        chat_id,
                        "上一条权限提示发送失败，正在重试。",
                        false,
                    )
                    .await;
                if result.is_ok()
                    && self
                        .reply(credentials, chat_id, &prompt, false)
                        .await
                        .is_ok()
                {
                    let mut state = chat.lock().await;
                    if state.generation == generation {
                        state.finish_answer();
                    }
                } else {
                    let mut state = chat.lock().await;
                    if state.generation == generation {
                        state.prompt_retry = Some(prompt);
                    }
                }
            } else {
                let _ = self
                    .reply(
                        credentials,
                        chat_id,
                        if initial {
                            "权限提示发送中，请稍候。"
                        } else {
                            "权限决定处理中，请稍候。"
                        },
                        false,
                    )
                    .await;
            }
            return Ok(true);
        }
        if let Some((session, event)) = state.elicitation.take() {
            drop(state);
            let answer = elicitation_answer(&event, input);
            self.gateway
                .request(
                    "elicitation_respond",
                    json!({"sessionKey":session,"requestId":event["requestId"],"answer":answer}),
                )
                .await?;
            if input == "0" {
                self.reply(credentials, chat_id, "已取消。", false).await?;
            }
            return Ok(true);
        }
        let Some((session, event)) = state.permissions.front().cloned() else {
            return Ok(false);
        };
        if !matches!(input, "0" | "1" | "2") {
            drop(state);
            self.reply(
                credentials,
                chat_id,
                "请回复 1 允许一次，回复 2 允许本会话，回复 0 拒绝。",
                false,
            )
            .await?;
            return Ok(true);
        }
        // The answer gate serializes duplicate inputs. Never retain the state
        // mutex across RPC: /new must be able to invalidate this generation
        // and abort the old turn while a permission response is blocked.
        let mut params = json!({"sessionKey":session,"requestId":event["requestId"],"decision":if input=="0"{"deny"}else{"allow"}});
        if input == "0" {
            params["reason"] = "User denied permission from IM channel.".into();
        } else {
            params["remember"] = json!(input == "2");
        }
        state.answering = true;
        let generation = state.generation;
        drop(state);
        let result = self.gateway.request("permission_decide", params).await;
        let mut state = chat.lock().await;
        if state.generation != generation {
            return Ok(true);
        }
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                state.answering = false;
                return Err(error);
            }
        };
        state.permissions.pop_front();
        let next = state
            .permissions
            .front()
            .map(|(_, event)| permission_prompt(event));
        drop(state);
        let confirmation = self
            .reply(
                credentials,
                chat_id,
                if result["delivered"] != true {
                    "权限请求已失效，已跳过，继续处理。"
                } else {
                    match input {
                        "0" => "已拒绝，继续处理。",
                        "2" => "已允许本会话，继续执行。",
                        _ => "已允许一次，继续执行。",
                    }
                },
                false,
            )
            .await;
        if confirmation.is_err() {
            let mut state = chat.lock().await;
            if state.generation == generation {
                state.answering = false;
                state.prompt_retry = next;
                if state.prompt_retry.is_none() {
                    state.finish_answer();
                }
            }
            return Ok(true);
        }
        if chat.lock().await.generation != generation {
            return Ok(true);
        }
        if let Some(next) = next {
            if self
                .reply(credentials, chat_id, &next, false)
                .await
                .is_err()
            {
                let mut state = chat.lock().await;
                if state.generation == generation {
                    state.answering = false;
                    state.prompt_retry = Some(next);
                }
                return Ok(true);
            }
        }
        {
            let mut state = chat.lock().await;
            if state.generation == generation {
                state.finish_answer();
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod generation_tests {
    use super::*;
    #[test]
    fn stale_turn_completion_cannot_clear_replacement_interactions() {
        let mut chat = Chat {
            generation: 2,
            active_session: Some("new-session".into()),
            answering: true,
            initial_prompt: true,
            prompt_retry: Some("new prompt".into()),
            permissions: VecDeque::from([(
                "new-session".into(),
                json!({"requestId":"new-permission"}),
            )]),
            elicitation: Some(("new-session".into(), json!({"requestId":"new-question"}))),
            ..Default::default()
        };
        assert!(!chat.finish_turn(1));
        assert_eq!(chat.active_session.as_deref(), Some("new-session"));
        assert_eq!(chat.permissions[0].1["requestId"], "new-permission");
        assert_eq!(
            chat.elicitation.as_ref().unwrap().1["requestId"],
            "new-question"
        );
        assert!(chat.answering && chat.initial_prompt);
        assert_eq!(chat.prompt_retry.as_deref(), Some("new prompt"));
    }
}
struct LiveSink<'a> {
    owner: &'a LiveOwner,
    credentials: &'a Value,
    chat: &'a str,
    session: &'a str,
    generation: u64,
    text: StdMutex<String>,
}
impl ChannelEventSink for LiveSink<'_> {
    fn event(&self, event: Value) -> ChannelCall<'_, ()> {
        Box::pin(async move {
            let chat = self.owner.chat(self.chat);
            let mut state = chat.lock().await;
            if state.generation != self.generation {
                return Ok(());
            }
            let prompt = match event["type"].as_str() {
                Some("permission_request") => {
                    let first = state.permissions.is_empty() && !state.answering;
                    state
                        .permissions
                        .push_back((self.session.into(), event.clone()));
                    state.changed.notify_waiters();
                    if first {
                        state.answering = true;
                        state.initial_prompt = true;
                    }
                    first.then(|| permission_prompt(&event))
                }
                Some("elicitation_request") => {
                    state.elicitation = Some((self.session.into(), event.clone()));
                    state.changed.notify_waiters();
                    Some(elicitation_prompt(&event))
                }
                Some("assistant_text_delta") => {
                    if let Some(text) = event["text"].as_str() {
                        self.text.lock().unwrap().push_str(text);
                    }
                    None
                }
                Some("assistant_attachment") => {
                    let attachment = event["attachment"].clone();
                    let Some(context) = state.context.clone() else {
                        drop(state);
                        eprintln!("weixin: attachment send skipped: missing context");
                        self.owner
                            .delivery
                            .queue_attachment_fallback(self.chat, &attachment);
                        return Ok(());
                    };
                    drop(state);
                    match super::super::attachments::send_outbound(
                        &self.owner.http,
                        self.credentials,
                        self.chat,
                        &context,
                        &attachment,
                    )
                    .await
                    {
                        Ok(true) => return Ok(()),
                        Ok(false) => {
                            eprintln!("weixin: attachment send failed: missing content");
                        }
                        Err(error) => {
                            eprintln!("weixin: attachment send failed: {}", error.message);
                        }
                    }
                    let fallback =
                        super::super::attachments::format_attachment_fallback(&attachment);
                    let _ = self
                        .owner
                        .reply(self.credentials, self.chat, &fallback, true)
                        .await;
                    return Ok(());
                }
                Some("error") => event["message"].as_str().map(str::to_owned),
                _ => None,
            };
            drop(state);
            if let Some(prompt) = prompt {
                let queue_on_failure = event["type"] != "permission_request"
                    && event["type"] != "elicitation_request";
                let result = self
                    .owner
                    .reply(self.credentials, self.chat, &prompt, queue_on_failure)
                    .await;
                if event["type"] == "permission_request" {
                    let mut state = chat.lock().await;
                    if state.generation == self.generation {
                        state.initial_prompt = false;
                        if result.is_ok() {
                            state.answering = false;
                        } else {
                            state.prompt_retry = Some(prompt);
                        }
                        state.changed.notify_waiters();
                    }
                    // Delivery failure retains the request and prevents an
                    // unseen permission from being decided by the next input.
                    return Ok(());
                }
                if event["type"] == "elicitation_request" && result.is_err() {
                    // `WeixinChannel.ts:722-726` — elicitation prompt failure does not fail the turn.
                    return Ok(());
                }
                result?;
            }
            Ok(())
        })
    }
}
fn permission_prompt(event: &Value) -> String {
    let payload = serde_json::to_string_pretty(&event["payload"]).unwrap_or_default();
    let units: Vec<_> = payload.trim().encode_utf16().collect();
    let payload = if units.len() > 800 {
        format!("{}...", String::from_utf16_lossy(&units[..800]))
    } else if units.is_empty() {
        "(空)".into()
    } else {
        payload.trim().into()
    };
    format!(
        "工具 {} 需要权限才能继续执行。\n\n请求内容：\n{payload}\n\n回复 1 允许一次，回复 2 允许本会话，回复 0 拒绝。",
        event["toolName"].as_str().unwrap_or("")
    )
}
fn elicitation_prompt(event: &Value) -> String {
    let mut lines = Vec::new();
    for q in event["questions"].as_array().into_iter().flatten() {
        if let Some(header) = text(q, "header") {
            lines.push(format!("**{header}**"));
        }
        if let Some(question) = text(q, "question") {
            lines.push(question.into());
        }
        for (i, option) in q["options"].as_array().into_iter().flatten().enumerate() {
            let description = text(option, "description")
                .map(|text| format!(" — {text}"))
                .unwrap_or_default();
            lines.push(format!(
                "{}. {}{description}",
                i + 1,
                option["label"].as_str().unwrap_or("")
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
    if input == "0" {
        return json!({"type":"cancelled","reason":"user cancelled"});
    }
    let mut answers = serde_json::Map::new();
    for q in event["questions"].as_array().into_iter().flatten() {
        let mut selected: Vec<_> = input
            .split(|ch: char| ch == ',' || ch == '，' || ch.is_whitespace())
            .filter_map(|token| {
                let token = token.trim_start();
                let sign = if token.starts_with('-') { -1 } else { 1 };
                let digits = token
                    .trim_start_matches(['+', '-'])
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>();
                let index = digits.parse::<usize>().ok()?;
                if sign < 0 || index == 0 {
                    return None;
                }
                q["options"].get(index - 1)?.get("label").cloned()
            })
            .collect();
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

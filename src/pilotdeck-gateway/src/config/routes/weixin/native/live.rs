//! Live per-chat ownership: polling keeps accepting answers while a turn waits.
//!
//! TS baseline (cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85):
//! - WeixinChannel.ts:403-545 (dispatchMessage, queuePendingTurn,
//!   resetChatInteractionState, runQueuedTurns)
//! - WeixinChannel.ts:655-800 (processMessage, createLiveReplyTransport,
//!   sendReply, sendTypingIfPossible)
//! - ImPermissionHelper.ts / ImElicitationHelper.ts / ImLiveReplyController.ts
//!   live in native/live/{permissions,reply}.rs; ImChatSessionState.ts lives in
//!   native/live/session.rs.
use super::*;
use crate::config::channel_commands::ChannelEventSink;
use crate::metadata::js_trim;
use reply::{ActivityKind, LiveReplyController, LiveReplyOptions, WeixinLiveTransport, WeixinReplyIo};
pub(crate) use std::collections::{HashMap, VecDeque};
pub(crate) use std::sync::Mutex as StdMutex;

mod permissions;
mod reply;
mod session;
use permissions::{Confirmation, ElicitationState, PermissionAnswer, PermissionState};
use session::{ActiveRun, ChatSessionState, QueuedTurn};

/// One chat's live state. ImChatSessionState.ts:14-68 owns the generation, the
/// bounded pending-turn queue and the active run; permissions and elicitation
/// are the two helper state machines.
struct Chat {
    state: ChatSessionState,
    context: Option<String>,
    permissions: PermissionState,
    elicitation: ElicitationState,
    changed: Arc<tokio::sync::Notify>,
    live: Option<Arc<Mutex<LiveReplyController>>>,
}
impl Default for Chat {
    fn default() -> Self {
        Self {
            state: ChatSessionState::default(),
            context: None,
            permissions: PermissionState::new(),
            elicitation: ElicitationState::new(),
            changed: Arc::new(tokio::sync::Notify::new()),
            live: None,
        }
    }
}
impl Chat {
    fn generation(&self, chat: &str) -> u64 {
        self.state.generation(chat)
    }
    /// WeixinChannel.ts:719-742 / ImPermissionHelper.ts:151-164 — a finished turn
    /// never clears a permission decision that is still in flight.
    fn finish_turn(&mut self, chat: &str, generation: u64) -> bool {
        if !self.state.is_current(chat, generation) {
            return false;
        }
        self.elicitation.clear(chat);
        self.permissions.clear_after_turn(chat);
        self.live = None;
        true
    }
}
pub(super) struct LiveOwner {
    storage: Storage,
    http: Http,
    gateway: Arc<dyn ChannelGateway>,
    mapper: StdMutex<ChannelSessionState>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
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
    async fn generation(&self, chat: &str) -> u64 {
        self.chat(chat).lock().await.generation(chat)
    }
    /// WeixinChannel.ts:524 — resetChatInteractionState drops pendingTurns.
    pub(crate) async fn clear_pending_turns(&self, chat: &str) -> Vec<QueuedTurn> {
        self.chat(chat)
            .lock()
            .await
            .state
            .clear_pending_turns(chat)
    }
    async fn enter_run(&self, chat: &str) -> bool {
        self.chat(chat).lock().await.state.enter_run(chat)
    }
    async fn leave_run(&self, chat: &str) {
        self.chat(chat).lock().await.state.leave_run(chat);
    }
    async fn queue_turn(&self, chat: &str, turn: QueuedTurn) -> Vec<QueuedTurn> {
        self.chat(chat).lock().await.state.queue_turn(chat, turn)
    }
    async fn shift_turn(&self, chat: &str) -> Option<QueuedTurn> {
        self.chat(chat).lock().await.state.shift_turn(chat)
    }
    async fn active_run(&self, chat: &str) -> Option<ActiveRun> {
        self.chat(chat).lock().await.state.active_run(chat)
    }
    async fn set_active_run(
        &self,
        chat: &str,
        session: &str,
        run_id: Option<String>,
        generation: u64,
    ) {
        self.chat(chat).lock().await.state.set_active_run(
            chat,
            ActiveRun {
                session_key: session.into(),
                run_id,
                generation,
            },
        );
    }
    /// ImChatSessionState.ts:64 — only the turn that still owns the generation
    /// clears the run (WeixinChannel.ts:777).
    async fn clear_active_run(&self, chat: &str, generation: u64) {
        let guarded = self.chat(chat);
        let mut state = guarded.lock().await;
        if state.state.is_current(chat, generation) {
            state.state.clear_active_run(chat);
        }
    }

    #[cfg(test)]
    pub(crate) async fn test_set_active_run(
        &self,
        chat: &str,
        session: &str,
        run_id: Option<&str>,
        generation: u64,
    ) {
        self.set_active_run(chat, session, run_id.map(str::to_owned), generation)
            .await;
    }

    #[cfg(test)]
    pub(crate) async fn test_clear_active_run(&self, chat: &str, generation: u64) {
        self.clear_active_run(chat, generation).await;
    }

    #[cfg(test)]
    pub(crate) async fn test_active_run(&self, chat: &str) -> Option<ActiveRun> {
        self.active_run(chat).await
    }

    #[cfg(test)]
    pub(crate) async fn test_reset_for_new_session(&self, chat: &str) {
        let guarded = self.chat(chat);
        guarded.lock().await.state.reset_for_new_session(chat);
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
    /// WeixinChannel.ts:927-947 — queued replies flush on the next successful
    /// poll; each failed segment stays at the head.
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
                    eprintln!(
                        "weixin: outbox reply queued after failure: {}",
                        super::super::media::format_weixin_error_message(&error.message)
                    );
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
    /// WeixinChannel.ts:880-889 — best effort. A missing client or context token
    /// is a silent no-op; a failed RPC only logs with formatWeixinError.
    async fn send_typing(&self, credentials: &Value, chat: &str) {
        let context = self.chat(chat).lock().await.context.clone();
        let Some(context) = context else {
            return;
        };
        if let Err(error) = self.typing_rpc(credentials, chat, &context).await {
            eprintln!(
                "weixin: sendTyping failed: {}",
                super::super::media::format_weixin_error_message(&error.message)
            );
        }
    }
    /// weixin-ilink api.js:68-75: getconfig supplies typing_ticket, then sendtyping.
    async fn typing_rpc(&self, credentials: &Value, chat: &str, context: &str) -> Result<()> {
        let base = text(credentials, "baseUrl").unwrap_or("");
        let token = text(credentials, "botToken");
        let config = self
            .http
            .json(
                base,
                "ilink/bot/getconfig",
                token,
                Some(json!({"ilink_user_id":chat,"context_token":context})),
            )
            .await?;
        let Some(ticket) = text(&config, "typing_ticket").filter(|ticket| !ticket.is_empty())
        else {
            return Ok(());
        };
        self.http
            .json(
                base,
                "ilink/bot/sendtyping",
                token,
                Some(json!({"ilink_user_id":chat,"typing_ticket":ticket,"status":1})),
            )
            .await
            .map(|_| ())
    }
    /// WeixinChannel.ts:707 — a void sendTypingIfPossible call: the turn must not
    /// wait for the typing round trip.
    fn typing_in_background(self: &Arc<Self>, credentials: &Value, chat: &str) {
        let owner = self.clone();
        let credentials = credentials.clone();
        let chat = chat.to_owned();
        tokio::spawn(async move {
            owner.send_typing(&credentials, &chat).await;
        });
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
            // An earlier ordinary message claims the chat run loop before the
            // next item dispatches. Answers and /new never enter that queue.
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
    /// WeixinChannel.ts:403-521 — dispatchMessage.
    async fn message(
        self: &Arc<Self>,
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
        let Some((input, mut attachments)) =
            crate::config::routes::weixin::native::weixin_inbound_text_and_attachments(message)
        else {
            return Ok(());
        };
        let mut input = input;
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
        // WeixinChannel.ts:417-419 — trim decides whether the message is empty;
        // the command router still receives the raw text.
        if js_trim(&input).is_empty() && attachments.is_empty() {
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
        let previous_session = self.mapper.lock().unwrap().session("weixin", chat_id);
        // WeixinSessionMapper.ts:23-27 — resolve trims first, then matches /new.
        let resolved_new = resolve_new(&input);
        if let Some(remainder) = resolved_new.as_ref() {
            if let Some(saved) = receipt.get("resolvedNew").and_then(Value::as_str) {
                self.mapper
                    .lock()
                    .unwrap()
                    .active_by_chat_id
                    .insert(chat_id.into(), saved.into());
                self.save_mapper()?;
            } else {
                let active = self.active_run(chat_id).await;
                let dropped = self.clear_pending_turns(chat_id).await;
                for turn in dropped {
                    self.finish_dropped_turn(&turn)?;
                }
                {
                    let guarded = self.chat(chat_id);
                    let mut state = guarded.lock().await;
                    state.state.reset_for_new_session(chat_id);
                    state.permissions.clear(chat_id);
                    state.elicitation.clear(chat_id);
                    state.live = None;
                    state.changed.notify_waiters();
                }
                self.delivery.clear_pending_replies(chat_id);
                let mut params = json!({
                    "sessionKey":active
                        .as_ref()
                        .map(|run| run.session_key.clone())
                        .unwrap_or(previous_session)
                });
                if let Some(run_id) = active.and_then(|run| run.run_id) {
                    params["runId"] = run_id.into();
                }
                // WeixinChannel.ts:416-419 — a failed abort only logs.
                if let Err(error) = self.gateway.request("abort_turn", params).await {
                    eprintln!(
                        "weixin: abort previous session on /new failed: {}",
                        super::super::media::format_weixin_error_message(&error.message)
                    );
                }
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
            if remainder.is_empty() {
                // WeixinChannel.ts:431 — the confirmation is not queued on failure
                // and a failed send still acks this message.
                if self
                    .reply(credentials, chat_id, "已创建新会话。", false)
                    .await
                    .is_ok()
                {
                    receipt["outbox"] = json!(["已创建新会话。"]);
                    receipt["sentIndex"] = json!(1);
                } else {
                    receipt["outbox"] = json!([]);
                    receipt["sentIndex"] = json!(0);
                }
                receipt["complete"] = true.into();
                self.save_receipt(&receipt_path, &receipt)?;
                return Ok(());
            }
            input = remainder.clone();
        }
        let generation = self.generation(chat_id).await;
        // WeixinChannel.ts:437-441 — elicitation and permission answers both
        // consume the inbound message.
        if self.answer(credentials, chat_id, &input, generation).await? {
            return Ok(());
        }
        // WeixinChannel.ts:457-471 — commands run on the raw text.
        if js_trim(&input).starts_with('/')
            && self
                .run_command(&input, chat_id, credentials, &receipt_path, &mut receipt)
                .await?
        {
            return Ok(());
        }
        // WeixinChannel.ts:421 — mapped.message is the trimmed text.
        let prompt = if js_trim(&input).is_empty() {
            "请查看我发送的附件。".to_owned()
        } else {
            js_trim(&input).to_owned()
        };
        let (session, project) = {
            let mapper = self.mapper.lock().unwrap();
            (
                mapper.session("weixin", chat_id),
                mapper.project(chat_id).map(str::to_owned),
            )
        };
        let turn = QueuedTurn {
            session_key: session,
            message: prompt,
            project_key: project,
            attachments,
            receipt_path: Some(receipt_path),
        };
        self.run_queued_turns(credentials, chat_id, generation, turn)
            .await
    }
    /// WeixinChannel.ts:473-490 — executeChannelCommand and its reply delivery.
    async fn run_command(
        &self,
        input: &str,
        chat_id: &str,
        credentials: &Value,
        receipt_path: &std::path::Path,
        receipt: &mut Value,
    ) -> Result<bool> {
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
        if !execute_channel_command(input, &mut ctx).await? {
            return Ok(false);
        }
        // Only copy this chat's entries. Other chats may have changed while the
        // command's gateway RPC was in flight.
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
        // WeixinChannel.ts:478-486 — command replies go through sendReply with the
        // default options, so a delivery failure is NOT queued.
        let replies = capture.0.lock().unwrap().clone();
        for reply in &replies {
            let _ = self.reply(credentials, chat_id, reply, false).await;
        }
        receipt["outbox"] = json!([]);
        receipt["sentIndex"] = json!(0);
        receipt["complete"] = true.into();
        self.save_receipt(receipt_path, receipt)?;
        Ok(true)
    }
    /// WeixinChannel.ts:520-546 — queuePendingTurn + runQueuedTurns.
    async fn run_queued_turns(
        self: &Arc<Self>,
        credentials: &Value,
        chat_id: &str,
        generation: u64,
        first: QueuedTurn,
    ) -> Result<()> {
        if !self.enter_run(chat_id).await {
            let dropped = self.queue_turn(chat_id, first).await;
            for turn in dropped {
                self.finish_dropped_turn(&turn)?;
            }
            return Ok(());
        }
        let mut next = Some(first);
        while let Some(turn) = next {
            self.process_message(credentials, chat_id, generation, turn)
                .await;
            // A /new reset empties the queue, so a stale turn simply finds none.
            next = self.shift_turn(chat_id).await;
        }
        self.leave_run(chat_id).await;
        Ok(())
    }
    /// The oldest turns dropped by the 20-turn cap are finished, not replayed.
    fn finish_dropped_turn(&self, turn: &QueuedTurn) -> Result<()> {
        let Some(path) = turn.receipt_path.as_ref() else {
            return Ok(());
        };
        let mut receipt = match super::super::storage::read_checked(path) {
            Ok(Some(saved)) => saved,
            Ok(None) => json!({}),
            Err(_) => json!({}),
        };
        receipt["outbox"] = json!([]);
        receipt["sentIndex"] = json!(0);
        receipt["complete"] = true.into();
        self.save_receipt(path, &receipt)
    }
    /// WeixinChannel.ts:655-800 — processMessage. A gateway failure becomes a
    /// visible status and never stops the queued turns.
    async fn process_message(
        self: &Arc<Self>,
        credentials: &Value,
        chat_id: &str,
        generation: u64,
        turn: QueuedTurn,
    ) {
        let chat = self.chat(chat_id);
        if !chat.lock().await.state.is_current(chat_id, generation) {
            let _ = self.finish_dropped_turn(&turn);
            return;
        }
        self.set_active_run(chat_id, &turn.session_key, None, generation)
            .await;
        struct ReplyIo {
            owner: Arc<LiveOwner>,
            credentials: Value,
            chat: String,
            failed: Arc<StdMutex<Vec<String>>>,
        }
        impl WeixinReplyIo for ReplyIo {
            fn typing_available(&self) -> bool {
                true
            }
            /// WeixinChannel.ts:806-829 — queueOnFailure decides whether a failed
            /// send joins the pending list; an activity pulse never queues.
            fn send_reply<'a>(
                &'a self,
                text: &'a str,
                queue_on_failure: bool,
            ) -> ChannelCall<'a, bool> {
                Box::pin(async move {
                    let owner = self.owner.clone();
                    let chat = self.chat.clone();
                    let context = owner.chat(&chat).lock().await.context.clone();
                    match super::send_reply(
                        &owner.storage,
                        &owner.http,
                        &self.credentials,
                        &chat,
                        context.as_deref(),
                        text,
                    )
                    .await
                    {
                        Ok(()) => Ok(true),
                        Err(error) => {
                            eprintln!(
                                "weixin: sendText failed: {}",
                                super::super::media::format_weixin_error_message(&error.message)
                            );
                            // WeixinChannel.ts:806-829 — queueOnFailure alone decides
                            // whether a failed send joins the pending list.
                            if queue_on_failure {
                                owner.delivery.queue_pending_reply(&chat, text);
                                self.failed.lock().unwrap().push(text.into());
                            }
                            Ok(false)
                        }
                    }
                })
            }
            fn send_typing_if_possible(&self) -> ChannelCall<'_, ()> {
                Box::pin(async move {
                    self.owner.send_typing(&self.credentials, &self.chat).await;
                    Ok(())
                })
            }
        }
        let failed = Arc::new(StdMutex::new(Vec::<String>::new()));
        let transport = Arc::new(WeixinLiveTransport::new(Arc::new(ReplyIo {
            owner: self.clone(),
            credentials: credentials.clone(),
            chat: chat_id.into(),
            failed: failed.clone(),
        })));
        // WeixinChannel.ts:666-676 — the Weixin options disable the controller's
        // own watchdog; turn_timeout arrives as an event instead.
        let live = Arc::new(Mutex::new(LiveReplyController::with_options(
            transport.clone(),
            LiveReplyOptions::weixin(),
        )));
        chat.lock().await.live = Some(live.clone());
        // WeixinChannel.ts:747-761 — submitTurn builds the run; no client runId.
        let mut params = json!({
            "sessionKey":turn.session_key,
            "channelKey":"weixin",
            "message":turn.message,
            "allowPlanModeTools":false
        });
        if let Some(project) = turn.project_key.clone() {
            params["projectKey"] = project.into();
        }
        if !turn.attachments.is_empty() {
            params["attachments"] = serde_json::Value::Array(turn.attachments);
        }
        let sink = LiveSink {
            owner: self.clone(),
            credentials,
            chat: chat_id,
            session: &turn.session_key,
            generation,
            live: live.clone(),
            settled: StdMutex::new(false),
        };
        // WeixinChannel.ts:707 / :880 — fire and forget.
        self.typing_in_background(credentials, chat_id);
        if let Err(error) = self.gateway.stream_turn(params, &sink).await {
            // WeixinChannel.ts:734-743 — submitTurn failure becomes a visible
            // agent_status and the turn still flushes its final text.
            eprintln!(
                "weixin: submitTurn error: {}",
                super::super::media::format_weixin_error_message(&error.message)
            );
            let _ = live
                .lock()
                .await
                .handle_event(&json!({
                    "type":"agent_status",
                    "event":"channel_submit_failed",
                    "detail":{"message":"Failed to process this message. Please retry.","visible":true,"severity":"error"}
                }))
                .await;
        }
        if chat.lock().await.finish_turn(chat_id, generation) {
            self.clear_active_run(chat_id, generation).await;
            let _ = live.lock().await.flush_final().await;
        }
        let Some(path) = turn.receipt_path.as_ref() else {
            return;
        };
        let mut receipt = match super::super::storage::read_checked(path) {
            Ok(Some(saved)) => saved,
            Ok(None) => json!({}),
            Err(_) => json!({}),
        };
        // WeixinChannel.ts:789-790 — a failed segment is staged in the receipt so
        // the next poll resends it without re-submitting the turn.
        let failed = failed.lock().unwrap().clone();
        receipt["outbox"] = json!(failed);
        receipt["sentIndex"] = json!(0);
        // WeixinChannel.ts:789-790 — the receipt is only complete when nothing is
        // staged; otherwise the next poll replays the staged segments.
        if receipt["outbox"].as_array().is_none_or(|values| values.is_empty()) {
            receipt["complete"] = true.into();
        }
        let _ = self.save_receipt(path, &receipt);
    }
    /// WeixinChannel.ts:437-513 — the elicitation and permission answer paths.
    /// Every failure is logged and consumes the message, exactly like TS: the
    /// batch must still advance.
    async fn answer(
        &self,
        credentials: &Value,
        chat_id: &str,
        input: &str,
        generation: u64,
    ) -> Result<bool> {
        let chat = self.chat(chat_id);
        let pending_elicitation = {
            let state = chat.lock().await;
            if !state.state.is_current(chat_id, generation) {
                return Ok(true);
            }
            state.elicitation.has_pending(chat_id)
        };
        if pending_elicitation {
            let pending = chat.lock().await.elicitation.answer(chat_id, input);
            let Some(pending): Option<Confirmation> = pending else {
                return Ok(true);
            };
            if let Err(error) = self
                .gateway
                .request(
                    "elicitation_respond",
                    json!({
                        "sessionKey":pending.session_key,
                        "requestId":pending.request_id,
                        "answer":pending.answer
                    }),
                )
                .await
            {
                // WeixinChannel.ts:438-441 — swallow, keep the batch moving.
                eprintln!(
                    "weixin: elicitation answer error: {}",
                    super::super::media::format_weixin_error_message(&error.message)
                );
                return Ok(true);
            }
            if let Some(confirmation) = pending.confirmation {
                let _ = self
                    .reply(credentials, chat_id, &confirmation, false)
                    .await;
            }
            return Ok(true);
        }
        let (pending, answering) = {
            let state = chat.lock().await;
            (
                state.permissions.has_pending(chat_id),
                state.permissions.is_answering(chat_id),
            )
        };
        if !pending && !answering {
            return Ok(false);
        }
        let answer = chat
            .lock()
            .await
            .permissions
            .answer_with_state(chat_id, input);
        match answer {
            None | Some(PermissionAnswer::Silent) => Ok(true),
            Some(PermissionAnswer::Reply(result)) => {
                if self
                    .reply(credentials, chat_id, &result.text, false)
                    .await
                    .is_err()
                {
                    chat.lock()
                        .await
                        .permissions
                        .release_answer(chat_id, result.answer_token);
                    return Ok(true);
                }
                if !result.can_advance && !result.retry_prompt {
                    return Ok(true);
                }
                self.deliver_next_prompt(credentials, chat_id, result.answer_token)
                    .await;
                Ok(true)
            }
            Some(PermissionAnswer::Decide(begin)) => {
                let params = PermissionState::decision_params(&begin);
                let token = begin.token;
                let entry = begin.entry.clone();
                // WeixinChannel.ts:505-513 — a failed RPC rolls the request back
                // and is logged, never replayed as a new turn.
                let response = match self.gateway.request("permission_decide", params).await {
                    Ok(response) => response,
                    Err(error) => {
                        chat.lock()
                            .await
                            .permissions
                            .abort_answer(chat_id, token, entry);
                        eprintln!(
                            "weixin: permission answer error: {}",
                            super::super::media::format_weixin_error_message(&error.message)
                        );
                        return Ok(true);
                    }
                };
                let delivered = response["delivered"] == true;
                let Some(result) = chat
                    .lock()
                    .await
                    .permissions
                    .complete_answer(chat_id, token, delivered)
                else {
                    return Ok(true);
                };
                if self
                    .reply(credentials, chat_id, &result.text, false)
                    .await
                    .is_err()
                {
                    // ImPermissionHelper.ts:129-137 — keep the lock so the next
                    // message retries; never decide an unseen request.
                    chat.lock()
                        .await
                        .permissions
                        .release_answer(chat_id, result.answer_token);
                    return Ok(true);
                }
                if result.can_advance || result.retry_prompt {
                    self.deliver_next_prompt(credentials, chat_id, result.answer_token)
                        .await;
                }
                if matches!(js_trim(input), "1" | "2") && result.text.starts_with("已允许") {
                    let live = chat.lock().await.live.clone();
                    let pending = chat.lock().await.permissions.has_pending(chat_id);
                    if let Some(live) = live.filter(|_| !pending) {
                        let _ = live
                            .lock()
                            .await
                            .resume_activity(ActivityKind::Tool, false)
                            .await;
                    }
                }
                Ok(true)
            }
        }
    }
    /// WeixinChannel.ts:462-477 — take the next queued prompt while the lock is
    /// still held, confirm it by requestId, and keep the lock on a failed send.
    async fn deliver_next_prompt(&self, credentials: &Value, chat_id: &str, token: Option<u64>) {
        let chat = self.chat(chat_id);
        let Some(prompt) = chat
            .lock()
            .await
            .permissions
            .take_next_prompt(chat_id, token)
        else {
            return;
        };
        let request_id = chat
            .lock()
            .await
            .permissions
            .get_prompt_request_id(chat_id, token);
        let delivered = self
            .reply(credentials, chat_id, &prompt, false)
            .await
            .is_ok();
        chat.lock().await.permissions.confirm_next_prompt(
            chat_id,
            delivered,
            request_id.as_deref(),
            token,
        );
    }
}

#[cfg(test)]
mod generation_tests {
    use super::*;
    // WeixinChannel.ts:777 + ImChatSessionState.ts:64 — a stale turn may not clear
    // the replacement session's interactions. Every field is asserted.
    #[test]
    fn stale_turn_completion_cannot_clear_replacement_interactions() {
        let mut chat = Chat {
            state: ChatSessionState::new(20),
            ..Default::default()
        };
        chat.state.set_active_run(
            "chat",
            ActiveRun {
                session_key: "new-session".into(),
                run_id: None,
                generation: 2,
            },
        );
        assert!(chat.state.enter_run("chat"));
        chat.permissions.capture(
            "chat",
            "new-session",
            "new-permission",
            "write_file",
            &json!({}),
        );
        chat.elicitation.capture(
            "chat",
            "new-session",
            &json!({"requestId":"new-question","questions":[]}),
        );
        chat.permissions.test_mark_retry("chat");
        assert!(!chat.finish_turn("chat", 1));
        assert_eq!(
            chat.state.active_run("chat").unwrap().session_key,
            "new-session"
        );
        assert_eq!(
            chat.permissions.head_request_id("chat").as_deref(),
            Some("new-permission")
        );
        assert_eq!(
            chat.elicitation.pending_request_id("chat").as_deref(),
            Some("new-question")
        );
        assert!(chat.permissions.is_answering("chat"));
        assert!(chat.permissions.initial_prompt_pending("chat"));
        assert!(chat.permissions.retry_pending("chat"));
        assert_eq!(
            chat.permissions.format_pending("chat").as_deref(),
            Some(
                "工具 write_file 需要权限才能继续执行。\n\n请求内容：\n{}\n\n回复 1 允许一次，回复 2 允许本会话，回复 0 拒绝。"
            )
        );
    }

    // ImPermissionHelper.ts:93-106 — the initial prompt gate is the field the
    // stale-turn test must keep proving, not just "there is pending work".
    #[test]
    fn initial_prompt_flag_tracks_capture_and_delivery() {
        let mut state = PermissionState::new();
        state.capture("chat", "session", "r1", "write_file", &json!({}));
        assert!(state.initial_prompt_pending("chat"));
        state.confirm_initial_prompt("chat", true, Some("r1"));
        assert!(!state.initial_prompt_pending("chat"));
        assert!(!state.is_answering("chat"));
    }

    // WeixinSessionMapper.ts:23-33 — resolve trims before matching /new, so a
    // leading-space "/new" still resets the session (r19 PASS → r20 FAIL).
    #[test]
    fn resolve_new_matches_after_js_trim() {
        assert_eq!(resolve_new("/new"), Some(String::new()));
        assert_eq!(resolve_new("  /new  "), Some(String::new()));
        assert_eq!(resolve_new("\u{feff}/new"), Some(String::new()));
        assert_eq!(resolve_new("/new next task"), Some("next task".into()));
        assert_eq!(resolve_new("   /new   next  "), Some("next".into()));
        assert_eq!(resolve_new("/newer"), None);
        assert_eq!(resolve_new("/news"), None);
        assert_eq!(resolve_new("hello"), None);
        assert_eq!(resolve_new(""), None);
    }
}
/// WeixinSessionMapper.ts:23-33 — resolve() trims the text, then treats an exact
/// `/new` or `/new <rest>` as a session reset. Returns the trimmed remainder.
fn resolve_new(input: &str) -> Option<String> {
    js_trim(input)
        .strip_prefix("/new")
        .filter(|rest| rest.is_empty() || rest.starts_with(' '))
        .map(|rest| js_trim(rest).to_owned())
}

struct LiveSink<'a> {
    owner: Arc<LiveOwner>,
    credentials: &'a Value,
    chat: &'a str,
    session: &'a str,
    generation: u64,
    live: Arc<Mutex<LiveReplyController>>,
    /// WeixinChannel.ts:686-692 — once a terminal text was posted (timeout), a
    /// later abort must not replace it.
    settled: StdMutex<bool>,
}
impl ChannelEventSink for LiveSink<'_> {
    fn event(&self, event: Value) -> ChannelCall<'_, ()> {
        Box::pin(async move {
            let chat = self.owner.chat(self.chat);
            let kind = text(&event, "type").unwrap_or("");
            {
                let state = chat.lock().await;
                if state.generation(self.chat) != self.generation {
                    return Ok(());
                }
            }
            match kind {
                "turn_started" => {
                    if let Some(run_id) = text(&event, "runId") {
                        self.owner
                            .set_active_run(
                                self.chat,
                                self.session,
                                Some(run_id.into()),
                                self.generation,
                            )
                            .await;
                    }
                }
                "permission_request" => {
                    // WeixinChannel.ts:729-738 — pause activity, then capture and
                    // deliver the prompt with confirmInitialPrompt's result.
                    self.live.lock().await.pause_activity().await?;
                    let prompt = {
                        let mut state = chat.lock().await;
                        state.permissions.capture(
                            self.chat,
                            self.session,
                            text(&event, "requestId").unwrap_or(""),
                            text(&event, "toolName").unwrap_or(""),
                            &event["payload"],
                        )
                    };
                    chat.lock().await.changed.notify_waiters();
                    if let Some(prompt) = prompt {
                        let delivered = self
                            .owner
                            .reply(self.credentials, self.chat, &prompt, false)
                            .await
                            .is_ok();
                        let mut state = chat.lock().await;
                        if state.generation(self.chat) == self.generation {
                            state.permissions.confirm_initial_prompt(
                                self.chat,
                                delivered,
                                text(&event, "requestId"),
                            );
                            state.changed.notify_waiters();
                        }
                    }
                    // A failed prompt keeps the request and the lock (八项③).
                    return Ok(());
                }
                "elicitation_request" => {
                    self.live.lock().await.pause_activity().await?;
                    let prompt = {
                        let mut state = chat.lock().await;
                        state.elicitation.capture(self.chat, self.session, &event)
                    };
                    chat.lock().await.changed.notify_waiters();
                    // WeixinChannel.ts:722-726 — a failed question never fails the turn.
                    let _ = self
                        .owner
                        .reply(self.credentials, self.chat, &prompt, false)
                        .await;
                    return Ok(());
                }
                "assistant_attachment" => return self.attachment(event).await,
                "error" if text(&event, "code") == Some("agent_aborted") => {
                    // WeixinChannel.ts:739 — after a timeout the terminal text is
                    // already posted; markAborted only applies to real aborts.
                    if *self.settled.lock().unwrap() {
                        return Ok(());
                    }
                    *self.settled.lock().unwrap() = true;
                    self.live.lock().await.mark_aborted().await?;
                    return Ok(());
                }
                "error" if text(&event, "code") == Some("turn_timeout") => {
                    *self.settled.lock().unwrap() = true;
                    self.live.lock().await.mark_timed_out().await?;
                    return Ok(());
                }
                _ => {}
            }
            self.live.lock().await.handle_event(&event).await
        })
    }
}
impl LiveSink<'_> {
    async fn attachment(&self, event: Value) -> Result<()> {
        let attachment = event["attachment"].clone();
        let chat = self.owner.chat(self.chat);
        let Some(context) = chat.lock().await.context.clone() else {
            eprintln!("weixin: attachment send skipped: missing context");
            self.owner
                .delivery
                .queue_attachment_fallback(self.chat, &attachment);
            return Ok(());
        };
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
            Ok(false) => eprintln!("weixin: attachment send failed: missing content"),
            Err(error) => eprintln!(
                "weixin: attachment send failed: {}",
                super::super::media::format_weixin_error_message(&error.message)
            ),
        }
        let fallback = super::super::attachments::format_attachment_fallback(&attachment);
        let _ = self
            .owner
            .reply(self.credentials, self.chat, &fallback, true)
            .await;
        Ok(())
    }
}





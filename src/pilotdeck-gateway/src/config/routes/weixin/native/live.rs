//! Live per-chat ownership: polling keeps accepting answers while a turn waits.
//!
//! TS baseline (cc7c4e65): WeixinChannel.ts:403-800 (dispatchMessage,
//! runQueuedTurns, processMessage, createLiveReplyTransport) plus the protocol
//! helpers in native/live/{reply,permissions,session}.rs.
use super::*;
use crate::config::channel_commands::ChannelEventSink;
use reply::{ActivityKind, LiveReplyController, LiveReplyOptions, WeixinLiveTransport, WeixinReplyIo};
pub(crate) use std::collections::{HashMap, HashSet, VecDeque};
pub(crate) use std::sync::Mutex as StdMutex;

mod permissions;
mod reply;
mod session;
use permissions::{AnswerStep, ElicitationState, PermissionState};
use session::{ActiveRun, ChatSessionState, QueuedTurn, WEIXIN_MAX_PENDING_TURNS_PER_CHAT};

struct Chat {
    generation: u64,
    context: Option<String>,
    permissions: PermissionState,
    elicitation: ElicitationState,
    changed: Arc<tokio::sync::Notify>,
    live: Option<Arc<Mutex<LiveReplyController>>>,
}
impl Default for Chat {
    fn default() -> Self {
        Self {
            generation: 0,
            context: None,
            permissions: PermissionState::new(),
            elicitation: ElicitationState::new(),
            changed: Arc::new(tokio::sync::Notify::new()),
            live: None,
        }
    }
}
impl Chat {
    /// WeixinChannel.ts:719-742 / ImPermissionHelper.ts:151-164 — a finished turn
    /// never clears a permission decision that is still in flight.
    fn finish_turn(&mut self, chat: &str, generation: u64) -> bool {
        if self.generation != generation {
            return false;
        }
        self.elicitation.clear(chat);
        self.permissions.clear_after_turn(chat);
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
    sessions: StdMutex<HashMap<String, ChatSessionState>>,
    pending: StdMutex<HashMap<String, VecDeque<PendingTurn>>>,
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
            sessions: Default::default(),
            pending: Default::default(),
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
    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, ChatSessionState>> {
        self.sessions.lock().unwrap()
    }
    fn session_running(&self, chat: &str) -> bool {
        self.sessions()
            .get(chat)
            .is_some_and(|session| session.is_running(chat))
    }
    fn enter_run(&self, chat: &str) -> bool {
        self.sessions()
            .entry(chat.into())
            .or_default()
            .enter_run(chat)
    }
    fn leave_run(&self, chat: &str) {
        if let Some(session) = self.sessions().get_mut(chat) {
            session.leave_run(chat);
        }
    }
    fn queue_turn(&self, chat: &str, turn: QueuedTurn) {
        self.sessions()
            .entry(chat.into())
            .or_default()
            .queue_turn(chat, turn);
    }
    fn shift_turn(&self, chat: &str) -> Option<QueuedTurn> {
        self.sessions().get_mut(chat).and_then(|s| s.shift_turn(chat))
    }
    fn pending_turns(&self, chat: &str) -> usize {
        self.sessions()
            .get(chat)
            .map(|s| s.pending_len(chat))
            .unwrap_or(0)
    }
    fn active_run(&self, chat: &str) -> Option<ActiveRun> {
        self.sessions().get(chat).and_then(|s| s.active_run(chat))
    }
    fn set_active_run(&self, chat: &str, session: &str, run_id: Option<String>, generation: u64) {
        self.sessions()
            .entry(chat.into())
            .or_default()
            .set_active_run(
                chat,
                ActiveRun {
                    session_key: session.into(),
                    run_id,
                    generation,
                },
            );
    }
    fn clear_active_run(&self, chat: &str) {
        if let Some(session) = self.sessions().get_mut(chat) {
            session.clear_active_run(chat);
        }
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
    /// WeixinChannel.ts:875-893 — best effort: a missing context or a failed RPC
    /// only logs.
    async fn send_typing(&self, credentials: &Value, chat: &str) {
        let context = self.chat(chat).lock().await.context.clone();
        if context.is_none() {
            return;
        }
        if let Err(error) = self.typing_rpc(credentials, chat, context.as_deref()).await {
            eprintln!("weixin: sendTyping failed: {}", error.message);
        }
    }
    /// weixin-ilink api.js:68-75: getconfig supplies typing_ticket, then sendtyping.
    async fn typing_rpc(
        &self,
        credentials: &Value,
        chat: &str,
        context: Option<&str>,
    ) -> Result<()> {
        let base = text(credentials, "baseUrl").unwrap_or("");
        let token = text(credentials, "botToken");
        let config = self
            .http
            .json(
                base,
                "ilink/bot/getconfig",
                token,
                Some(json!({"ilink_user_id":chat,"context_token":context.unwrap_or("")})),
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
    pub async fn process_batch(
        self: &Arc<Self>,
        credentials: &Value,
        mut batch: Value,
    ) -> Result<()> {        let id = text(&batch, "id")
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
    /// WeixinChannel.ts:403-519 — command, answer, and ordinary-turn dispatch.
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
        if is_new {
            if let Some(saved) = receipt.get("resolvedNew").and_then(Value::as_str) {
                self.mapper
                    .lock()
                    .unwrap()
                    .active_by_chat_id
                    .insert(chat_id.into(), saved.into());
                self.save_mapper()?;
            } else {
                let previous_generation = self.reset_chat(chat_id).await;
                let previous = previous_generation.0;
                self.delivery.clear_pending_replies(chat_id);
                let previous = previous
                    .unwrap_or_else(|| self.mapper.lock().unwrap().session("weixin", chat_id));
                let mut params = json!({"sessionKey":previous});
                if let Some(run_id) = previous_generation.1 {
                    params["runId"] = run_id.into();
                }
                let _ = self.gateway.request("abort_turn", params).await;
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
                // WeixinChannel.ts:431 — no queueOnFailure for session confirmation.
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
        }
        // WeixinChannel.ts:413 — dispatched inputs carry the generation they
        // were admitted under; /new above already bumped it.
        let generation = self.chat_generation(chat_id).await;
        if !is_new && self.answer(credentials, chat_id, input.as_str(), generation).await? {
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
        let turn = QueuedTurn {
            session_key: session,
            message: prompt,
            project_key: project,
            attachments,
        };
        // WeixinChannel.ts:453-477 — answer with the prompt gate still open;
        // otherwise hand the turn to the per-chat run loop (or its queue).
        if self
            .await_chat_slot(credentials, chat_id, &turn.message, generation)
            .await?
        {
            return Ok(());
        }
        if self.session_running(chat_id) {
            self.enqueue_turn(chat_id, turn, receipt_path, receipt)?;
            return Ok(());
        }
        self.dispatch_turn(credentials, chat_id, generation, turn, receipt_path, receipt)
            .await
    }
    /// WeixinChannel.ts:453-477 — the permission answer path runs before the
    /// active-chat queue. Only a reply that can be an answer to an in-flight
    /// interaction waits for it; ordinary text queues immediately.
    async fn await_chat_slot(
        &self,
        credentials: &Value,
        chat_id: &str,
        input: &str,
        generation: u64,
    ) -> Result<bool> {
        if !self.session_running(chat_id) {
            return Ok(false);
        }
        let interacting = matches!(input.trim(), "0" | "1" | "2") || {
            let chat = self.chat(chat_id);
            let state = chat.lock().await;
            state.permissions.has_pending(chat_id)
                || state.permissions.is_answering(chat_id)
                || state.elicitation.has_pending(chat_id)
        };
        if !interacting {
            return Ok(false);
        }
        let chat = self.chat(chat_id);
        loop {
            let changed = chat.lock().await.changed.clone();
            let notified = changed.notified();
            tokio::pin!(notified);
            // Enable before checking state so a prompt transition between
            // answer() and the wait is not lost.
            notified.as_mut().enable();
            if self.chat_generation(chat_id).await != generation {
                return Ok(true);
            }
            if !self.session_running(chat_id) {
                return Ok(false);
            }
            if self.answer(credentials, chat_id, input, generation).await? {
                return Ok(true);
            }
            if !self.session_running(chat_id) {
                return Ok(false);
            }
            notified.await;
        }
    }
    /// WeixinChannel.ts:520-546 — one run loop per chat. The first message runs
    /// inline; later messages land in the bounded pending-turn queue and are
    /// drained by that loop, each with its own staged receipt.
    async fn dispatch_turn(
        self: &Arc<Self>,
        credentials: &Value,
        chat_id: &str,
        generation: u64,
        turn: QueuedTurn,
        receipt_path: std::path::PathBuf,
        receipt: Value,
    ) -> Result<()> {
        if !self.enter_run(chat_id) {
            self.enqueue_turn(chat_id, turn, receipt_path, receipt)?;
            return Ok(());
        }
        let mut next = Some((turn, receipt_path, receipt));
        let mut outcome = Ok(());
        while let Some((turn, receipt_path, mut receipt)) = next {
            if self.chat_generation(chat_id).await != generation {
                // /new replaced the session: drop this turn and everything queued.
                receipt["complete"] = true.into();
                self.save_receipt(&receipt_path, &receipt)?;
                break;
            }
            // The old post-lock re-check: a queued reply may answer the prompt.
            if self
                .answer(credentials, chat_id, &turn.message, generation)
                .await?
            {
                next = self.dequeue_turn(chat_id);
                continue;
            }
            outcome = self
                .process_message(credentials, chat_id, generation, turn, &receipt_path, &mut receipt)
                .await;
            if outcome.is_err() {
                break;
            }
            next = self.dequeue_turn(chat_id);
        }
        self.leave_run(chat_id);
        outcome
    }
    /// ImChatSessionState.ts:38-45 — cap at 20 pending turns, dropping the oldest.
    fn enqueue_turn(
        &self,
        chat: &str,
        turn: QueuedTurn,
        receipt_path: std::path::PathBuf,
        receipt: Value,
    ) -> Result<()> {
        let dropped = {
            let mut pending = self.pending.lock().unwrap();
            let queue = pending.entry(chat.into()).or_default();
            queue.push_back(PendingTurn {
                turn,
                receipt_path,
                receipt,
            });
            let mut dropped = Vec::new();
            while queue.len() > WEIXIN_MAX_PENDING_TURNS_PER_CHAT {
                if let Some(entry) = queue.pop_front() {
                    dropped.push(entry);
                }
            }
            dropped
        };
        for mut entry in dropped {
            entry.receipt["complete"] = true.into();
            self.save_receipt(&entry.receipt_path, &entry.receipt)?;
        }
        if let Some(session) = self.sessions().get_mut(chat) {
            session.queue_turn(
                chat,
                QueuedTurn {
                    session_key: String::new(),
                    message: String::new(),
                    project_key: None,
                    attachments: vec![],
                },
            );
        }
        Ok(())
    }
    fn dequeue_turn(&self, chat: &str) -> Option<(QueuedTurn, std::path::PathBuf, Value)> {
        let entry = self
            .pending
            .lock()
            .unwrap()
            .get_mut(chat)
            .and_then(|queue| queue.pop_front())?;
        if let Some(session) = self.sessions().get_mut(chat) {
            session.shift_turn(chat);
        }
        Some((entry.turn, entry.receipt_path, entry.receipt))
    }
    /// /new drops every queued turn (REPORT 113/158/159); their receipts complete
    /// so the inbox does not replay them.
    fn clear_pending(&self, chat: &str) -> Result<()> {
        let dropped: Vec<PendingTurn> = self
            .pending
            .lock()
            .unwrap()
            .remove(chat)
            .map(|queue| queue.into_iter().collect())
            .unwrap_or_default();
        for mut entry in dropped {
            entry.receipt["complete"] = true.into();
            self.save_receipt(&entry.receipt_path, &entry.receipt)?;
        }
        if let Some(session) = self.sessions().get_mut(chat) {
            session.reset_for_new_session(chat);
        }
        Ok(())
    }
    async fn chat_generation(&self, chat: &str) -> u64 {
        self.chat(chat).lock().await.generation
    }
    /// WeixinChannel.ts:524-529 — generation++, clear answers, abort the active run.
    async fn reset_chat(&self, chat: &str) -> (Option<String>, Option<String>) {
        let active = self.active_run(chat);
        let _ = self.clear_pending(chat);
        let guarded = self.chat(chat);
        let mut state = guarded.lock().await;
        state.generation += 1;
        state.permissions.clear(chat);
        state.elicitation.clear(chat);
        state.live = None;
        state.changed.notify_waiters();
        (
            active.as_ref().map(|run| run.session_key.clone()),
            active.and_then(|run| run.run_id),
        )
    }
    /// WeixinChannel.ts:655-745 — one turn: live reply controller in, final flush out.
    async fn process_message(
        self: &Arc<Self>,
        credentials: &Value,
        chat_id: &str,
        generation: u64,
        turn: QueuedTurn,
        receipt_path: &std::path::Path,
        receipt: &mut Value,
    ) -> Result<()> {
        let chat = self.chat(chat_id);
        self.set_active_run(chat_id, &turn.session_key, None, generation);
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
            fn send_reply<'a>(&'a self, text: &'a str, queue_on_failure: bool) -> ChannelCall<'a, bool> {
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
                            eprintln!("weixin: live reply send failed: {}", error.message);
                            // WeixinChannel.ts:789-790 — queue the segment and let
                            // the durable receipt resend it on the next poll.
                            owner.delivery.queue_pending_reply(&chat, text);
                            self.failed.lock().unwrap().push(text.into());
                            Ok(queue_on_failure)
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
        let mut params = json!({
            "sessionKey":turn.session_key,
            "channelKey":"weixin",
            "runId":format!("weixin:{chat_id}:{}", uuid::Uuid::new_v4()),
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
            live,
            timeout: StdMutex::new(None),
        };
        // WeixinChannel.ts:707 — typing before the turn starts.
        let _ = transport.send_typing_if_possible().await;
        let result = self.gateway.stream_turn(params, &sink).await;
        let current = chat.lock().await.finish_turn(chat_id, generation);
        self.clear_active_run(chat_id);
        if !current {
            receipt["complete"] = true.into();
            self.save_receipt(receipt_path, receipt)?;
            return Ok(());
        }
        if result.is_err() {
            // WeixinChannel.ts:734-743 — a submit failure becomes a visible status.
            sink.live
                .lock()
                .await
                .handle_event(&json!({
                    "type":"agent_status",
                    "event":"channel_submit_failed",
                    "detail":{"message":"Failed to process this message. Please retry.","visible":true,"severity":"error"}
                }))
                .await?;
        }
        let mut controller = sink.live.lock().await;
        controller.flush_final().await?;
        drop(controller);
        chat.lock().await.live = None;
        // WeixinChannel.ts:789 — a failed segment is staged in the receipt so the
        // next poll resends it without re-submitting the turn.
        let failed = failed.lock().unwrap().clone();
        if failed.is_empty() {
            receipt["outbox"] = json!([]);
            receipt["complete"] = true.into();
        } else {
            // WeixinChannel.ts:789-790 — every failed segment is retried separately.
            receipt["outbox"] = json!(failed);
            receipt["sentIndex"] = json!(0);
        }
        self.save_receipt(receipt_path, receipt)?;
        result
    }
    /// WeixinChannel.ts:452-477 + ImPermissionHelper.ts:185-274.
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
        if state.elicitation.has_pending(chat_id) {
            let pending = state.elicitation.answer(chat_id, input);
            drop(state);
            let Some(pending) = pending else {
                return Ok(true);
            };
            self.gateway
                .request(
                    "elicitation_respond",
                    json!({
                        "sessionKey":pending.session_key,
                        "requestId":pending.request_id,
                        "answer":pending.answer
                    }),
                )
                .await?;
            if let Some(confirmation) = pending.confirmation {
                let context = chat.lock().await.context.clone();
                // WeixinChannel.ts:439-442 — the confirmation is best effort and a
                // failed send never fails the turn (REPORT 132, 八项④).
                let _ = super::send_reply(
                    &self.storage,
                    &self.http,
                    credentials,
                    chat_id,
                    context.as_deref(),
                    &confirmation,
                )
                .await;
            }
            return Ok(true);
        }
        if !state.permissions.has_pending(chat_id) {
            return Ok(false);
        }
        let step = state.permissions.answer_step(chat_id, input);
        match step {
            AnswerStep::NotHandled => return Ok(false),
            AnswerStep::Reply(text) => {
                drop(state);
                self.reply(credentials, chat_id, &text, false).await?;
                return Ok(true);
            }
            AnswerStep::Swallow => return Ok(true),
            AnswerStep::Retry => {
                drop(state);
                // ImPermissionHelper.ts:192-194 — retry the queued prompt while the
                // lock stays held, then confirm it (REPORT 128/129, 八项③).
                let _ = self
                    .reply(credentials, chat_id, permissions::PERMISSION_RETRY, false)
                    .await;
                let prompt = {
                    let mut state = chat.lock().await;
                    state.permissions.take_next_prompt(chat_id, None)
                };
                if let Some(prompt) = prompt {
                    let delivered = self.reply(credentials, chat_id, &prompt, false).await;
                    let request_id = {
                        let state = chat.lock().await;
                        state.permissions.get_prompt_request_id(chat_id, None)
                    };
                    let mut state = chat.lock().await;
                    state.permissions.confirm_next_prompt(
                        chat_id,
                        delivered.is_ok(),
                        request_id.as_deref(),
                        None,
                    );
                }
                return Ok(true);
            }
            AnswerStep::Decide(answer) => {
                drop(state);
                let params = PermissionState::decision_params(&answer);
                let token = answer.token;
                let entry = answer.entry.clone();
                let result = self.gateway.request("permission_decide", params).await;
                if result.is_err() {
                    let mut state = chat.lock().await;
                    state.permissions.abort_answer(chat_id, token, entry);
                    return result.map(|_| true);
                }
                let delivered = result.unwrap_or_default()["delivered"] == true;
                let confirmation = {
                    let mut state = chat.lock().await;
                    state.permissions.complete_answer(chat_id, token, delivered)
                };
                let Some(confirmation) = confirmation else {
                    return Ok(true);
                };
                if self
                    .reply(credentials, chat_id, &confirmation.text, false)
                    .await
                    .is_err()
                {
                    // ImPermissionHelper.ts:129-137 — keep the lock so the next
                    // message retries; never let an unseen request be decided.
                    let mut state = chat.lock().await;
                    state.permissions.release_answer(chat_id, Some(token));
                    return Ok(true);
                }
                if confirmation.can_advance || confirmation.retry_prompt {
                    let prompt = {
                        let mut state = chat.lock().await;
                        state
                            .permissions
                            .take_next_prompt(chat_id, confirmation.answer_token)
                    };
                    if let Some(prompt) = prompt {
                        let request_id = {
                            let state = chat.lock().await;
                            state
                                .permissions
                                .get_prompt_request_id(chat_id, confirmation.answer_token)
                        };
                        let delivered =
                            self.reply(credentials, chat_id, &prompt, false).await.is_ok();
                        let mut state = chat.lock().await;
                        state.permissions.confirm_next_prompt(
                            chat_id,
                            delivered,
                            request_id.as_deref(),
                            confirmation.answer_token,
                        );
                    }
                }
                if (input.trim() == "1" || input.trim() == "2")
                    && confirmation.text.starts_with("已允许")
                {
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
}

#[cfg(test)]
mod generation_tests {
    use super::*;
    #[test]
    fn stale_turn_completion_cannot_clear_replacement_interactions() {
        let mut chat = Chat {
            generation: 2,
            ..Default::default()
        };
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
        assert!(!chat.finish_turn("chat", 1));
        assert!(chat.permissions.has_pending("chat"));
        assert!(chat.elicitation.has_pending("chat"));
        assert!(chat.finish_turn("chat", 2));
    }
}
struct PendingTurn {
    turn: QueuedTurn,
    receipt_path: std::path::PathBuf,
    receipt: Value,
}
struct LiveSink<'a> {
    owner: Arc<LiveOwner>,
    credentials: &'a Value,
    chat: &'a str,
    session: &'a str,
    generation: u64,
    live: Arc<Mutex<LiveReplyController>>,
    timeout: StdMutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Drop for LiveSink<'_> {
    fn drop(&mut self) {
        if let Ok(mut timeout) = self.timeout.lock() {
            if let Some(task) = timeout.take() {
                task.abort();
            }
        }
    }
}
impl ChannelEventSink for LiveSink<'_> {
    fn event(&self, event: Value) -> ChannelCall<'_, ()> {
        Box::pin(async move {
            let chat = self.owner.chat(self.chat);
            let kind = text(&event, "type").unwrap_or("");
            {
                let state = chat.lock().await;
                if state.generation != self.generation {
                    return Ok(());
                }
            }
            match kind {
                "turn_started" => {
                    if let Some(run_id) = text(&event, "runId") {
                        self.owner.set_active_run(
                            self.chat,
                            self.session,
                            Some(run_id.into()),
                            self.generation,
                        );
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
                        if state.generation == self.generation {
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
                    // WeixinChannel.ts:739 — a timeout notice already replaced the
                    // terminal text, so an abort after it is not appended twice.
                    if self.timeout.lock().unwrap().is_some() {
                        return Ok(());
                    }
                    self.live.lock().await.mark_aborted().await?;
                    return Ok(());
                }
                "error" if text(&event, "code") == Some("turn_timeout") => {
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
            Err(error) => eprintln!("weixin: attachment send failed: {}", error.message),
        }
        let fallback = super::super::attachments::format_attachment_fallback(&attachment);
        let _ = self
            .owner
            .reply(self.credentials, self.chat, &fallback, true)
            .await;
        Ok(())
    }
}
impl LiveSink<'_> {

}






//! WeCom websocket channel port under development; not registered for runtime.
mod admission;
mod batch;
mod command;
mod config;
mod delivery;
mod incoming;
mod interaction;
mod mapper;
mod media;
mod media_delivery;
mod policy;
mod project_commands;
mod render;
mod search_command;
mod transport;
mod turn;
mod wire;

use super::{
    im_interaction::InteractionPort,
    protocol::{
        BoxFuture, ChannelAdapter, ChannelControlPort, ChannelError, ChannelHandle, ChannelKey,
        ChannelSearchPort, ChannelStartDeps, ChannelStatusPort, ChannelUpdatePort,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::{Arc, Mutex};

/// Websocket WeCom channel. Later parity work must complete media, commands,
/// reconnect, batching and persistence before production registration.
pub struct WeComChannel {
    key: ChannelKey,
    config: config::Config,
    interaction: Arc<dyn InteractionPort>,
    control: Option<Arc<dyn ChannelControlPort>>,
    search: Option<Arc<dyn ChannelSearchPort>>,
    status: Arc<dyn ChannelStatusPort>,
    update: Arc<dyn ChannelUpdatePort>,
    pilot_home: Option<String>,
    state: Option<Arc<super::weixin::ChannelStatePersistence>>,
}
impl WeComChannel {
    #[expect(
        clippy::too_many_arguments,
        reason = "preserve the explicit channel dependency-injection API"
    )]
    pub fn new(
        options: &JsValue,
        interaction: Arc<dyn InteractionPort>,
        control: Option<Arc<dyn ChannelControlPort>>,
        search: Option<Arc<dyn ChannelSearchPort>>,
        status: Arc<dyn ChannelStatusPort>,
        update: Arc<dyn ChannelUpdatePort>,
        pilot_home: Option<String>,
        state: Option<Arc<super::weixin::ChannelStatePersistence>>,
    ) -> Self {
        Self {
            key: ChannelKey::new("wecom"),
            config: config::Config::from_options(options),
            interaction,
            control,
            search,
            status,
            update,
            pilot_home,
            state,
        }
    }
}
struct InteractionCleanup {
    port: Arc<dyn InteractionPort>,
    key: String,
}
impl Drop for InteractionCleanup {
    fn drop(&mut self) {
        self.port.elicitation_clear(&self.key);
        self.port.permission_clear_after_turn(&self.key);
    }
}
struct Handle {
    stop: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}
impl ChannelHandle for Handle {
    fn stop(&self, _: &str) -> BoxFuture<Result<(), ChannelError>> {
        if let Some(stop) = self.stop.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = stop.send(());
        }
        let task = self.task.clone();
        Box::pin(async move {
            let mut task = task.lock().await;
            if let Some(task) = task.take() {
                task.await.map_err(|e| ChannelError::error(e.to_string()))?;
            }
            Ok(())
        })
    }
}
async fn connect_authenticated(
    config: &config::Config,
    device: &str,
) -> Result<
    (
        transport::Transport,
        tokio::sync::mpsc::UnboundedReceiver<JsValue>,
    ),
    String,
> {
    let (transport, callbacks) = transport::Transport::connect(&config.websocket_url).await?;
    let id = format!("subscribe-{}", uuid::Uuid::new_v4().simple());
    let response = transport
        .request(
            id.clone().into(),
            wire::subscribe(
                id.into(),
                config.bot_id.as_str().into(),
                config.secret.as_str().into(),
                device.into(),
            ),
            std::time::Duration::from_secs(20),
        )
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            transport.stop().await;
            return Err(error);
        }
    };
    match wire::response_error(&response) {
        Ok(None) => Ok((transport, callbacks)),
        Ok(Some(error)) | Err(error) => {
            transport.stop().await;
            Err(error)
        }
    }
}
impl ChannelAdapter for WeComChannel {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }
    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        let config = self.config.clone();
        let interaction = self.interaction.clone();
        let commands = command::HostCommands {
            status: self.status.clone(),
            update: self.update.clone(),
        };
        let control = self.control.clone();
        let search = self.search.clone();
        let pilot_home = self.pilot_home.clone();
        let state = self.state.clone();
        Box::pin(async move {
            let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
            if config.bot_id.is_empty() || config.secret.is_empty() {
                if let Some(log) = &deps.log {
                    log("error", "wecom: botKey (bot_id) and secret are required");
                }
                let task = tokio::spawn(async {});
                return Ok(Arc::new(Handle {
                    stop: Mutex::new(Some(stop_tx)),
                    task: Arc::new(tokio::sync::Mutex::new(Some(task))),
                }) as Arc<dyn ChannelHandle>);
            }
            let device = uuid::Uuid::new_v4().simple().to_string();
            let (transport, mut callbacks) = match connect_authenticated(&config, &device).await {
                Ok(result) => result,
                Err(error) => {
                    if let Some(log) = &deps.log {
                        log("error", &format!("wecom: start failed: {error}"));
                    }
                    let task = tokio::spawn(async {});
                    return Ok(Arc::new(Handle {
                        stop: Mutex::new(Some(stop_tx)),
                        task: Arc::new(tokio::sync::Mutex::new(Some(task))),
                    }) as Arc<dyn ChannelHandle>);
                }
            };
            let task = tokio::spawn(async move {
                let transport = Arc::new(transport::ActiveTransport::new(transport));
                let active = admission::ActiveSessions::default();
                let media_client = Arc::new(media::MediaClient::default());
                let mut turns = tokio::task::JoinSet::new();
                let state_key: JsString = "wecom".into();
                let saved = match &state {
                    Some(state) => state.load(&state_key).await,
                    None => None,
                };
                let mut mapper = saved
                    .as_ref()
                    .map(mapper::WeComSessionMapper::from_state)
                    .unwrap_or_default();
                let mut dedupe = wire::Dedupe::default();
                let mut replies = delivery::ReplyHistory::default();
                let mut batches = batch::Batches::default();
                let mut batch_timers =
                    std::collections::HashMap::<String, tokio::task::JoinHandle<()>>::new();
                let (batch_tx, mut batch_rx) =
                    tokio::sync::mpsc::unbounded_channel::<(String, u64)>();
                let mut heartbeat = tokio::time::interval_at(
                    tokio::time::Instant::now() + std::time::Duration::from_secs(30),
                    std::time::Duration::from_secs(30),
                );
                let (reconnect_tx, mut reconnect_rx) = tokio::sync::mpsc::unbounded_channel();
                let mut reconnect_task: Option<tokio::task::JoinHandle<()>> = None;
                let mut connected = true;
                loop {
                    let (callback, from_batch) = tokio::select! {
                        _ = &mut stop_rx => break,
                        _ = turns.join_next(), if !turns.is_empty() => continue,
                        _ = heartbeat.tick() => {
                            let request_id = format!("ping-{}", uuid::Uuid::new_v4().simple());
                            let _ = transport.send(wire::ping(request_id.into())).await;
                            continue;
                        }
                        Some((key, generation)) = batch_rx.recv() => match batches.take(&key, generation) { Some(v) => { batch_timers.remove(&key); (v, true) }, None => continue },
                        Some((replacement, receiver)) = reconnect_rx.recv() => {
                            transport.replace(Some(replacement)).await;
                            callbacks = receiver;
                            connected = true;
                            reconnect_task.take();
                            heartbeat.reset();
                            if let Some(log) = &deps.log { log("info", "wecom: reconnected"); }
                            continue;
                        }
                        value = callbacks.recv(), if connected => match value {
                            Some(v) => (v, false),
                            None => {
                                connected = false;
                                transport.replace(None).await;
                                let retry_config = config.clone();
                                let retry_device = device.clone();
                                let retry_log = deps.log.clone();
                                let retry_tx = reconnect_tx.clone();
                                reconnect_task = Some(tokio::spawn(async move {
                                    let delays = [2_u64, 5, 10, 30, 60];
                                    let mut attempt = 0usize;
                                    loop {
                                        let delay = delays[attempt.min(delays.len() - 1)];
                                        attempt = attempt.saturating_add(1);
                                        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                                        match connect_authenticated(&retry_config, &retry_device).await {
                                            Ok(result) => { let _ = retry_tx.send(result); break; },
                                            Err(error) => if let Some(log) = &retry_log { log("warn", &format!("wecom: reconnect failed: {error}")); },
                                        }
                                    }
                                }));
                                continue;
                            }
                        }
                    };
                    let Some(body) = callback.get("body") else {
                        continue;
                    };
                    let incoming_request_id = delivery::request_id(&callback);
                    let text = |v: Option<&JsValue>| {
                        v.and_then(JsValue::as_string)
                            .map(|s| String::from_utf16_lossy(s.units()))
                            .unwrap_or_default()
                    };
                    let id = match body.get("msgid") {
                        Some(value) if !matches!(value, JsValue::Null | JsValue::Undefined) => {
                            text(Some(value))
                        }
                        _ => incoming_request_id
                            .as_ref()
                            .map(|s| String::from_utf16_lossy(s.units()))
                            .unwrap_or_default(),
                    }
                    .trim()
                    .to_string();
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    if !from_batch && dedupe.duplicate(id.into(), now) {
                        continue;
                    }
                    let sender = text(body.get("from").and_then(|v| v.get("userid")));
                    let chat = body
                        .get("chatid")
                        .map(|v| text(Some(v)))
                        .unwrap_or_else(|| sender.clone());
                    let group = text(body.get("chattype")).to_lowercase() == "group";
                    if if group {
                        !policy::group_allowed(
                            config.group_policy,
                            &config.group_allow_from,
                            &config.group_sender_allow(&chat),
                            &chat,
                            &sender,
                        )
                    } else {
                        !policy::dm_allowed(config.dm_policy, &config.allow_from, &sender)
                    } {
                        continue;
                    }
                    let request_id = replies.target(&chat, group, incoming_request_id);
                    let Ok(parts) = incoming::extract(body) else {
                        continue;
                    };
                    let mut attachments = Vec::new();
                    let mut cache_failed = false;
                    for (kind, data) in &parts.media {
                        match media_client.cache_inbound(kind, data).await {
                            Ok(Some(attachment)) => attachments.push(attachment),
                            Ok(None) => {}
                            Err(error) => {
                                if let Some(log) = &deps.log {
                                    log(
                                        "warn",
                                        &format!(
                                            "wecom: failed to cache inbound {kind}: {}",
                                            error.error.message()
                                        ),
                                    );
                                }
                                if error.is_storage() {
                                    cache_failed = true;
                                    break;
                                }
                            }
                        }
                    }
                    if cache_failed {
                        continue;
                    }
                    let message = if parts.text.units().is_empty() {
                        parts.reply_text.unwrap_or_default()
                    } else {
                        parts.text
                    };
                    let message = if group {
                        incoming::strip_leading_mention(&message)
                    } else {
                        message
                    };
                    let message = if message.units().is_empty() && !attachments.is_empty() {
                        JsString::from("用户发送了企业微信附件。")
                    } else {
                        message
                    };
                    if message.units().is_empty() {
                        continue;
                    }
                    let interaction_key =
                        if group && config.group_sessions_per_user && !sender.is_empty() {
                            format!("{chat}:{sender}")
                        } else {
                            chat.clone()
                        };
                    if !from_batch {
                        match interaction::answer_pending(
                            interaction.as_ref(),
                            &transport,
                            &interaction_key,
                            &String::from_utf16_lossy(message.units()),
                            &request_id,
                        )
                        .await
                        {
                            Ok(false) => {}
                            Ok(true) | Err(_) => continue,
                        }
                    }
                    if !from_batch
                        && attachments.is_empty()
                        && config.text_batch_delay_ms > 0
                        && !String::from_utf16_lossy(message.units())
                            .trim()
                            .starts_with("/")
                    {
                        let delay = if message.units().len() >= 3900 {
                            config.text_batch_split_delay_ms
                        } else {
                            config.text_batch_delay_ms
                        };
                        let generation =
                            batches.insert(interaction_key.clone(), callback.clone(), message);
                        let tx = batch_tx.clone();
                        let key = interaction_key;
                        if let Some(timer) = batch_timers.remove(&key) {
                            timer.abort();
                        }
                        let timer_key = key.clone();
                        let timer = tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                            let _ = tx.send((timer_key, generation));
                        });
                        batch_timers.insert(key, timer);
                        continue;
                    }
                    let command_text = String::from_utf16_lossy(message.units());
                    if deps.turn_gateway.is_some() && command::HostCommands::matches(&command_text)
                    {
                        let commands = commands.clone();
                        let transport = transport.clone();
                        turns.spawn(async move {
                            commands
                                .execute(&command_text, &transport, &request_id)
                                .await;
                        });
                        continue;
                    }
                    let mut input = JsObject::new();
                    input.insert("chatId".into(), JsValue::String(chat.into()));
                    input.insert("userId".into(), JsValue::String(sender.into()));
                    input.insert(
                        "chatType".into(),
                        if group { "group".into() } else { "dm".into() },
                    );
                    input.insert(
                        "groupSessionsPerUser".into(),
                        JsValue::Bool(config.group_sessions_per_user),
                    );
                    input.insert("text".into(), JsValue::String(message));
                    let scope = JsValue::Object(input);
                    let uuid: JsString = uuid::Uuid::new_v4().to_string().into();
                    let Ok(mapped) = mapper.resolve(&scope, &uuid) else {
                        continue;
                    };
                    if mapped
                        .get("command")
                        .and_then(JsValue::as_string)
                        .is_some_and(|s| s.equals_str("new"))
                        && let Some(state) = &state
                    {
                        state.save(&state_key, mapper.snapshot());
                    }
                    if deps.turn_gateway.is_some() {
                        if search_command::matches(&command_text) {
                            let project = mapper.get_project(&scope).ok().flatten();
                            let search = search.clone();
                            let pilot_home = pilot_home.clone();
                            let transport = transport.clone();
                            turns.spawn(async move {
                                search_command::execute(
                                    &command_text,
                                    project,
                                    pilot_home.as_deref(),
                                    search.as_deref(),
                                    &transport,
                                    &request_id,
                                )
                                .await;
                            });
                            continue;
                        }
                        if let Some(control) = &control
                            && project_commands::execute(
                                &command_text,
                                &scope,
                                &mut mapper,
                                control.as_ref(),
                                &transport,
                                &request_id,
                            )
                            .await
                        {
                            if let Some(state) = &state {
                                state.save(&state_key, mapper.snapshot());
                            }
                            continue;
                        }
                    }
                    let Some(session) = mapped
                        .get("sessionKey")
                        .and_then(JsValue::as_string)
                        .cloned()
                    else {
                        continue;
                    };
                    let Some(message) = mapped.get("message").and_then(JsValue::as_string).cloned()
                    else {
                        continue;
                    };
                    if message.units().is_empty() {
                        delivery::reply(&transport, request_id, "已创建新会话。".into()).await;
                        continue;
                    }
                    let Some(gateway) = deps.turn_gateway.clone() else {
                        continue;
                    };
                    let Some(guard) = active.try_enter(session.clone()) else {
                        continue;
                    };
                    let project = mapped
                        .get("projectKey")
                        .and_then(JsValue::as_string)
                        .cloned();
                    let transport = transport.clone();
                    let interaction = interaction.clone();
                    let media_client = media_client.clone();
                    let pilot_home = pilot_home.clone();
                    turns.spawn(async move {
                        let _guard = guard;
                        let _cleanup = InteractionCleanup { port: interaction.clone(), key: interaction_key.clone() };
                        let session_label = String::from_utf16_lossy(session.units());
                        let mut answer = turn::TurnText::default();
                        let result = turn::submit(gateway.as_ref(), session, project, message, attachments).await;
                        if let Ok(mut events) = result {
                            loop {
                                let event = match events.next().await {
                                    Ok(Some(event)) => event,
                                    Ok(None) => break,
                                    Err(_) => { answer = turn::TurnText::default(); let _ = answer.append(&pilotdeck_protocol::parse_json(r#"{"type":"assistant_text_delta","text":"处理消息时发生错误，请重试。"}"#).unwrap()); break; }
                                };
                                let event_type = event.get("type").and_then(JsValue::as_string);
                                if event_type.is_some_and(|t| t.equals_str("elicitation_request")) {
                                    let prompt = interaction.elicitation_capture(&interaction_key, &session_label, &event);
                                    delivery::reply(&transport, request_id.clone(), prompt.into()).await;
                                } else if event_type.is_some_and(|t| t.equals_str("permission_request")) {
                                    if let Some(prompt) = interaction.permission_capture(&interaction_key, &session_label, &event) {
                                        let delivered = delivery::reply(&transport, request_id.clone(), prompt.into()).await;
                                        let prompt_id = event.get("requestId").and_then(JsValue::as_string).map(|s| String::from_utf16_lossy(s.units()));
                                        interaction.permission_confirm_initial(&interaction_key, delivered, prompt_id.as_deref());
                                    }
                                } else {
                                    for image in media::tool_event_images(&event) {
                                        if !media_delivery::send_prepared(transport.clone(), &request_id, &image).await {
                                            delivery::reply(&transport, request_id.clone(), "图片结果生成成功，但发送到企业微信失败。".into()).await;
                                        }
                                    }
                                    let _ = answer.append(&event);
                                }
                            }
                        } else {
                            let _ = answer.append(&pilotdeck_protocol::parse_json(r#"{"type":"assistant_text_delta","text":"处理消息时发生错误，请重试。"}"#).unwrap());
                        }
                        let answer = answer.finish();
                        if !answer.units().is_empty() {
                            let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
                            let pilot_home = pilot_home.as_ref().map(std::path::PathBuf::from).unwrap_or_else(|| home.join(".pilotdeck"));
                            let extraction = media::extract_deliverables(&answer, &home, &pilot_home).await;
                            let mut visible = String::from_utf16_lossy(extraction.text.units()).trim().to_string();
                            for warning in extraction.warnings { if !visible.is_empty() { visible.push('\n'); } visible.push_str(warning.trim()); }
                            if !visible.is_empty() { delivery::reply(&transport, request_id.clone(), visible.into()).await; }
                            else if !extraction.deliverables.is_empty() { delivery::reply(&transport, request_id.clone(), "我已生成附件，正在发送。".into()).await; }
                            for item in extraction.deliverables {
                                if let Ok(mut prepared) = media_client.load_source(&item.path.to_string_lossy(), None).await {
                                    if item.media_type == "file" { prepared.final_type = "file".into(); prepared.detected_type = "file".into(); }
                                    media_delivery::send_prepared(transport.clone(), &request_id, &prepared).await;
                                }
                            }
                        }
                    });
                }
                if let Some(task) = reconnect_task {
                    task.abort();
                    let _ = task.await;
                }
                for (_, timer) in batch_timers.drain() {
                    timer.abort();
                }
                turns.abort_all();
                while turns.join_next().await.is_some() {}
                transport.stop().await;
            });
            Ok(Arc::new(Handle {
                stop: Mutex::new(Some(stop_tx)),
                task: Arc::new(tokio::sync::Mutex::new(Some(task))),
            }) as Arc<dyn ChannelHandle>)
        })
    }
}
#[cfg(test)]
mod tests;

//! Serialized native channel replacement (`pilotdeckServer.ts:154-231`).
use crate::config::routes::{ChannelCall, RouteError};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) trait Handle: Send + Sync {
    fn stop(&self) -> ChannelCall<'_, ()>;
}

type StartCall = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Box<dyn Handle>, RouteError>> + Send>,
>;
struct NativeAdapter {
    key: &'static str,
    start: Box<dyn Fn() -> StartCall + Send + Sync>,
    active: Box<dyn Fn(bool) + Send + Sync>,
}
impl Adapter for NativeAdapter {
    fn key(&self) -> &str {
        self.key
    }
    fn start(&self) -> ChannelCall<'_, Box<dyn Handle>> {
        (self.start)()
    }
    fn active(&self, active: bool) {
        (self.active)(active)
    }
}
macro_rules! native_handle {
    ($($module:ident :: $handle:ident),* $(,)?) => {$ (
        impl Handle for crate::config::routes::$module::$handle {
            fn stop(&self) -> ChannelCall<'_, ()> { Box::pin(async move { self.close().await; Ok(()) }) }
        }
    )*};
}
native_handle! {
    telegram::TelegramHandle, slack::SlackHandle, bluebubbles::BlueBubblesHandle,
    signal::SignalHandle, home_assistant::HomeAssistantHandle, matrix::MatrixHandle,
    mattermost::MattermostHandle, sms::SmsHandle, whatsapp::WhatsAppHandle,
    dingtalk::DingTalkHandle, discord::DiscordHandle, qq::QQHandle,
    wecom_callback::WeComCallbackHandle, email::EmailHandle, wecom::WeComHandle,
    feishu::FeishuHandle, channel_http::HttpChannelHandle,
}
struct WeixinAdapter {
    owner: Arc<crate::config::routes::WeixinChannelHost>,
    delivery: Arc<crate::config::routes::WeixinDelivery>,
    host: Arc<crate::cron::native::ProductionHost>,
}
struct WeixinHandle(Arc<crate::config::routes::WeixinChannelHost>);
impl Handle for WeixinHandle {
    fn stop(&self) -> ChannelCall<'_, ()> {
        Box::pin(self.0.stop_current())
    }
}
impl Adapter for WeixinAdapter {
    fn key(&self) -> &str {
        "weixin"
    }
    fn start(&self) -> ChannelCall<'_, Box<dyn Handle>> {
        use crate::config::routes::WeixinLoginHost;
        Box::pin(async {
            self.owner.hot_start().await?;
            Ok(Box::new(WeixinHandle(self.owner.clone())) as Box<dyn Handle>)
        })
    }
    fn active(&self, active: bool) {
        *self.host.weixin_delivery.lock().unwrap() = active.then(|| self.delivery.clone());
    }
    fn replaces_internally(&self) -> bool {
        true
    }
}

pub(crate) struct InstalledChannels {
    runtime: Arc<ChannelRuntime>,
    home: std::path::PathBuf,
    cwd: std::path::PathBuf,
    env: crate::config::Env,
    gateway: Arc<dyn crate::config::channel_commands::ChannelGateway>,
    host: Arc<crate::cron::native::ProductionHost>,
    config: crate::config::routes::native::SharedConfigRoutes,
    weixin: Arc<crate::config::routes::WeixinChannelHost>,
    weixin_delivery: Arc<crate::config::routes::WeixinDelivery>,
    feishu: Arc<Mutex<Option<Arc<crate::config::routes::feishu::FeishuChannel>>>>,
    channel_state: Arc<crate::config::routes::ChannelStatePersistence>,
    stopped: tokio::sync::watch::Sender<bool>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
const MANAGED: &[&str] = &[
    "feishu",
    "weixin",
    "qq",
    "wecom",
    "telegram",
    "discord",
    "slack",
    "matrix",
    "mattermost",
    "signal",
    "whatsapp",
    "bluebubbles",
    "dingtalk",
    "wecom-callback",
    "email",
    "sms",
    "home-assistant",
    "api-server",
    "webhook",
];

struct FeishuStateSaver(Arc<dyn Fn(Value) + Send + Sync>);
impl crate::config::routes::feishu::FeishuStateChange for FeishuStateSaver {
    fn changed(&self, state: Value) {
        (self.0)(state);
    }
}

impl InstalledChannels {
    fn session_mapper(
        &self,
        key: &'static str,
    ) -> (
        crate::config::channel_commands::ChannelSessionState,
        Option<Arc<dyn Fn(Value) + Send + Sync>>,
    ) {
        let saved = self.channel_state.load(key).unwrap_or_else(|| json!({}));
        (
            crate::config::channel_commands::ChannelSessionState::from_value(&saved),
            Some(self.channel_state.saver(key)),
        )
    }
    fn configured(&self, snapshot: &pilotdeck_protocol::JsValue) -> Vec<Arc<dyn Adapter>> {
        use crate::application::*;
        use crate::config::routes::*;
        let mut channels: Vec<Arc<dyn Adapter>> = Vec::new();
        macro_rules! add {
            ($key:literal, $channel:expr, $field:ident) => {{
                let channel = $channel;
                let active_channel = channel.clone();
                let host = self.host.clone();
                channels.push(Arc::new(NativeAdapter {
                    key: $key,
                    start: Box::new(move || {
                        let channel = channel.clone();
                        Box::pin(
                            async move { Ok(Box::new(channel.start().await?) as Box<dyn Handle>) },
                        )
                    }),
                    active: Box::new(move |active| {
                        *host.$field.lock().unwrap() = active.then(|| active_channel.clone());
                    }),
                }));
            }};
            ($key:literal, $channel:expr) => {{
                let channel = $channel;
                channels.push(Arc::new(NativeAdapter {
                    key: $key,
                    start: Box::new(move || {
                        let channel = channel.clone();
                        Box::pin(
                            async move { Ok(Box::new(channel.start().await?) as Box<dyn Handle>) },
                        )
                    }),
                    active: Box::new(|_| {}),
                }));
            }};
        }
        if let Some(options) = feishu_options(snapshot, &self.home) {
            let saved = self.channel_state.load("feishu").unwrap_or_else(|| json!({}));
            let channel = feishu::FeishuChannel::with_state(
                options,
                self.gateway.clone(),
                crate::config::channel_commands::ChannelSessionState::from_value(&saved),
                Some(Arc::new(FeishuStateSaver(
                    self.channel_state.saver("feishu"),
                ))),
            );
            let active_channel = channel.clone();
            let current = self.feishu.clone();
            let host = self.host.clone();
            channels.push(Arc::new(NativeAdapter {
                key: "feishu",
                start: Box::new(move || {
                    let channel = channel.clone();
                    Box::pin(async move { Ok(Box::new(channel.start().await?) as Box<dyn Handle>) })
                }),
                active: Box::new(move |active| {
                    *current.lock().unwrap() = active.then(|| active_channel.clone());
                    *host.feishu_delivery.lock().unwrap() = active.then(|| active_channel.clone());
                }),
            }));
        }
        if enabled_adapter(snapshot, "weixin").is_some() {
            channels.push(Arc::new(WeixinAdapter {
                owner: self.weixin.clone(),
                delivery: self.weixin_delivery.clone(),
                host: self.host.clone(),
            }));
        }
        if let Some(options) = qq_options(snapshot, &self.env, &self.channel_state) {
            add!(
                "qq",
                qq::QQChannel::new(options, self.gateway.clone())
            );
        }
        if let Some(options) = wecom_options(snapshot, &self.env, &self.home) {
            let saved = self
                .channel_state
                .load("wecom")
                .unwrap_or_else(|| json!({}));
            let (_, on_change) = self.session_mapper("wecom");
            add!(
                "wecom",
                wecom::WeComChannel::with_mapper(
                    options,
                    self.gateway.clone(),
                    saved,
                    on_change,
                )
            );
        }
        if let Some(options) = telegram_options(snapshot, &self.env) {
            let (mapper, on_change) = self.session_mapper("telegram");
            add!(
                "telegram",
                telegram::TelegramChannel::with_mapper(
                    options,
                    self.gateway.clone(),
                    mapper,
                    on_change,
                ),
                telegram_delivery
            );
        }
        if let Some(config) = enabled_adapter(snapshot, "discord") {
            let options = discord::DiscordOptions {
                token: channel_text(config.get("token"))
                    .or_else(|| self.env.get("DISCORD_BOT_TOKEN").cloned())
                    .unwrap_or_default(),
                ..Default::default()
            };
            let (mapper, on_change) = self.session_mapper("discord");
            add!(
                "discord",
                discord::DiscordChannel::with_mapper(
                    options,
                    self.gateway.clone(),
                    mapper,
                    on_change,
                ),
                discord_delivery
            );
        }
        if let Some(options) = slack_options(snapshot, &self.env) {
            let (mapper, on_change) = self.session_mapper("slack");
            add!(
                "slack",
                slack::SlackChannel::with_mapper(options, self.gateway.clone(), mapper, on_change)
            );
        }
        if let Some(options) = matrix_options(snapshot, &self.env, &self.home) {
            let (mapper, on_change) = self.session_mapper("matrix");
            add!(
                "matrix",
                matrix::MatrixChannel::with_mapper(options, self.gateway.clone(), mapper, on_change),
                matrix_delivery
            );
        }
        if let Some(options) = mattermost_options(snapshot, &self.env) {
            let (mapper, on_change) = self.session_mapper("mattermost");
            add!(
                "mattermost",
                mattermost::MattermostChannel::with_mapper(
                    options,
                    self.gateway.clone(),
                    mapper,
                    on_change,
                )
            );
        }
        if let Some(options) = signal_options(snapshot, &self.env) {
            let (mapper, on_change) = self.session_mapper("signal");
            add!(
                "signal",
                signal::SignalChannel::with_mapper(options, self.gateway.clone(), mapper, on_change),
                signal_delivery
            );
        }
        if let Some(options) = whatsapp_options(snapshot, &self.env) {
            let (mapper, on_change) = self.session_mapper("whatsapp");
            add!(
                "whatsapp",
                whatsapp::WhatsAppChannel::with_mapper(
                    options,
                    self.gateway.clone(),
                    mapper,
                    on_change,
                ),
                whatsapp_delivery
            );
        }
        if let Some(options) = bluebubbles_options(snapshot, &self.env) {
            let (mapper, on_change) = self.session_mapper("bluebubbles");
            add!(
                "bluebubbles",
                bluebubbles::BlueBubblesChannel::with_mapper(
                    options,
                    self.gateway.clone(),
                    mapper,
                    on_change,
                )
            );
        }
        if let Some(options) = dingtalk_options(snapshot, &self.env) {
            let (mapper, on_change) = self.session_mapper("dingtalk");
            add!(
                "dingtalk",
                dingtalk::DingTalkChannel::with_mapper(
                    options,
                    self.gateway.clone(),
                    mapper,
                    on_change,
                ),
                dingtalk_delivery
            );
        }
        if let Some(options) = wecom_callback_options(snapshot, &self.env) {
            let (mapper, on_change) = self.session_mapper("wecom-callback");
            add!(
                "wecom-callback",
                wecom_callback::WeComCallbackChannel::with_mapper(
                    options,
                    self.gateway.clone(),
                    mapper,
                    on_change,
                ),
                wecom_callback_delivery
            );
        }
        if let Some(options) = email_options(snapshot, &self.env) {
            let (mapper, on_change) = self.session_mapper("email");
            add!(
                "email",
                email::EmailChannel::with_mapper(options, self.gateway.clone(), mapper, on_change),
                email_delivery
            );
        }
        if let Some(options) = sms_options(snapshot, &self.env) {
            let (mapper, on_change) = self.session_mapper("sms");
            add!(
                "sms",
                sms::SmsChannel::with_mapper(options, self.gateway.clone(), mapper, on_change),
                sms_delivery
            );
        }
        if let Some(options) = home_assistant_options(snapshot, &self.env) {
            let (mapper, on_change) = self.session_mapper("home-assistant");
            add!(
                "home-assistant",
                home_assistant::HomeAssistantChannel::with_mapper(
                    options,
                    self.gateway.clone(),
                    mapper,
                    on_change,
                ),
                home_assistant_delivery
            );
        }
        let (api, webhook) = http_channel_options(snapshot, &self.env);
        if let Some(options) = api {
            add!(
                "api-server",
                channel_http::ApiServerChannel::new(options, self.gateway.clone())
            );
        }
        if let Some(options) = webhook {
            add!(
                "webhook",
                channel_http::WebhookChannel::new(options, self.gateway.clone()),
                webhook_delivery
            );
        }
        channels
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        home: std::path::PathBuf,
        cwd: std::path::PathBuf,
        env: crate::config::Env,
        gateway: Arc<dyn crate::config::channel_commands::ChannelGateway>,
        host: Arc<crate::cron::native::ProductionHost>,
        config: crate::config::routes::native::SharedConfigRoutes,
        weixin: Arc<crate::config::routes::WeixinChannelHost>,
        weixin_delivery: Arc<crate::config::routes::WeixinDelivery>,
    ) -> Arc<Self> {
        let channel_state = crate::config::routes::ChannelStatePersistence::new(
            home.join("channels"),
        );
        weixin.attach_mapper_persistence(channel_state.saver("weixin"));
        Arc::new(Self {
            runtime: Arc::default(),
            home,
            cwd,
            env,
            gateway,
            host,
            config,
            weixin,
            weixin_delivery,
            feishu: Arc::default(),
            channel_state,
            stopped: tokio::sync::watch::channel(false).0,
            task: Mutex::new(None),
        })
    }
    pub(crate) fn feishu_webhook(&self) -> crate::http::FeishuWebhook {
        use http_body_util::BodyExt;
        let current = self.feishu.clone();
        Arc::new(move |request| {
            let channel = current.lock().unwrap().clone();
            Box::pin(async move {
                let Some(channel) = channel else {
                    return Ok(None);
                };
                let (status, body) = channel.webhook(request.body()).await;
                Ok(Some(
                    hyper::Response::builder()
                        .status(status)
                        .header("content-type", "application/json; charset=utf-8")
                        .body(
                            http_body_util::Full::new(bytes::Bytes::from(body.to_string()))
                                .map_err(|never| match never {})
                                .boxed(),
                        )
                        .expect("Feishu response"),
                ))
            })
        })
    }
    pub(crate) fn start(self: &Arc<Self>) {
        let mut task = self.task.lock().unwrap();
        if task.is_some() {
            return;
        }
        // Subscribe before the initial snapshot to avoid losing a CONFIG update.
        let mut notices = crate::config::routes::native::subscribe();
        let mut stopped = self.stopped.subscribe();
        let owner = self.clone();
        *task = Some(tokio::spawn(async move {
            let snapshot = owner.config.lock().unwrap().store.snapshot();
            let mut starts = tokio::task::JoinSet::new();
            for adapter in owner.configured(&snapshot) {
                if *stopped.borrow() {
                    break;
                }
                let runtime = owner.runtime.clone();
                let (queued, ready) = tokio::sync::oneshot::channel();
                starts.spawn(async move {
                    if let Err(error) = runtime.replace_queued(adapter, Some(queued)).await {
                        eprintln!("channel startup failed: {}", error.message);
                    }
                });
                // Reserve the per-key slot before receiving reload notices.
                // Startup network work continues in the background.
                let _ = ready.await;
            }
            loop {
                tokio::select! { biased;
                    _ = stopped.changed() => break,
                    notice = notices.recv() => match notice {
                        Ok((home, cwd, notice)) if home == owner.home && cwd == owner.cwd && adapter_notice(&notice) => {
                            let snapshot = owner.config.lock().unwrap().store.snapshot();
                            if let Err(error) = owner.reconcile(&snapshot).await { eprintln!("channel hot-reload failed: {}", error.message); }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let snapshot = owner.config.lock().unwrap().store.snapshot();
                            if let Err(error) = owner.reconcile(&snapshot).await { eprintln!("channel reconciliation failed: {}", error.message); }
                        }
                        _ => {}
                    },
                    Some(_) = starts.join_next() => {}
                }
            }
            while starts.join_next().await.is_some() {}
        }));
    }
    pub(crate) async fn close(&self) -> Result<(), RouteError> {
        self.stopped.send_replace(true);
        self.runtime.closing.store(true, Ordering::SeqCst);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
        let result = self.runtime.close().await;
        self.channel_state.flush();
        // Login preparation can hot-start the same Weixin host outside the
        // configured adapter map. Shutdown must drain that operation too.
        let weixin = self.weixin.stop_current().await;
        *self.host.weixin_delivery.lock().unwrap() = None;
        result.and(weixin)
    }
    async fn reconcile(&self, snapshot: &pilotdeck_protocol::JsValue) -> Result<(), RouteError> {
        if crate::application::enabled_adapter(snapshot, "weixin").is_none() {
            // Also disable a channel created by prepare_weixin_login, which has
            // no ChannelRuntime entry while configuration remains disabled.
            self.weixin.stop_current().await?;
            *self.host.weixin_delivery.lock().unwrap() = None;
        }
        self.runtime
            .reconcile(self.configured(snapshot), MANAGED)
            .await
    }
}
fn adapter_notice(notice: &pilotdeck_protocol::JsValue) -> bool {
    if notice
        .get("name")
        .and_then(pilotdeck_protocol::JsValue::as_string)
        .is_none_or(|v| !v.equals_str("config_changed"))
    {
        return false;
    }
    let Some(pilotdeck_protocol::JsValue::Array(paths)) =
        notice.get("payload").and_then(|v| v.get("changedPaths"))
    else {
        return false;
    };
    paths
        .iter()
        .filter_map(pilotdeck_protocol::JsValue::as_string)
        .any(|value| {
            let path = String::from_utf16_lossy(value.units());
            path == "adapters" || path.starts_with("adapters.")
        })
}
pub(crate) trait Adapter: Send + Sync {
    fn key(&self) -> &str;
    fn start(&self) -> ChannelCall<'_, Box<dyn Handle>>;
    fn active(&self, active: bool);
    fn replaces_internally(&self) -> bool {
        false
    }
}
struct Running {
    adapter: Arc<dyn Adapter>,
    handle: Box<dyn Handle>,
}
type Entry = Arc<tokio::sync::Mutex<Option<Running>>>;
#[derive(Default)]
pub(crate) struct ChannelRuntime {
    closing: AtomicBool,
    entries: Mutex<HashMap<String, Entry>>,
}
fn closing() -> RouteError {
    RouteError::new("gateway_request_failed", "channel server is closing")
}
impl ChannelRuntime {
    fn entry(&self, key: &str) -> Entry {
        self.entries
            .lock()
            .unwrap()
            .entry(key.into())
            .or_default()
            .clone()
    }
    pub(crate) async fn replace(&self, next: Arc<dyn Adapter>) -> Result<(), RouteError> {
        self.replace_queued(next, None).await
    }
    async fn replace_queued(
        &self,
        next: Arc<dyn Adapter>,
        queued: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<(), RouteError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(closing());
        }
        let entry = self.entry(next.key());
        let mut slot = entry.lock().await;
        if let Some(queued) = queued {
            let _ = queued.send(());
        }
        if self.closing.load(Ordering::SeqCst) {
            return Err(closing());
        }
        let previous_running = slot.take();
        if !next.replaces_internally() {
            if let Some(previous) = &previous_running {
                if let Err(error) = previous.handle.stop().await {
                    let _ = previous;
                    *slot = previous_running;
                    return Err(error);
                }
                previous.adapter.active(false);
            }
        }
        match next.start().await {
            Ok(handle) => {
                if self.closing.load(Ordering::SeqCst) {
                    let _ = handle.stop().await;
                    next.active(false);
                    return Err(closing());
                }
                next.active(true);
                *slot = Some(Running {
                    adapter: next,
                    handle,
                });
                Ok(())
            }
            Err(error) => {
                if next.replaces_internally() {
                    *slot = previous_running;
                    return Err(error);
                }
                if self.closing.load(Ordering::SeqCst) {
                    return Err(closing());
                }
                if let Some(previous) = previous_running {
                    // Drop the old stopped handle before restarting its exact adapter:
                    // native handle Drop cancels the owner as well.
                    let adapter = previous.adapter.clone();
                    drop(previous);
                    match adapter.start().await {
                        Ok(handle) => {
                            adapter.active(true);
                            *slot = Some(Running { adapter, handle });
                        }
                        Err(restore) => {
                            return Err(RouteError::new(
                                "gateway_request_failed",
                                format!(
                                    "channel {} replacement and rollback failed: {}; {}",
                                    next.key(),
                                    error.message,
                                    restore.message
                                ),
                            ));
                        }
                    }
                }
                Err(error)
            }
        }
    }
    pub(crate) async fn stop(&self, key: &str) -> Result<bool, RouteError> {
        let entry = self.entry(key);
        let mut slot = entry.lock().await;
        let Some(running) = slot.take() else {
            return Ok(false);
        };
        if let Err(error) = running.handle.stop().await {
            *slot = Some(running);
            return Err(error);
        }
        running.adapter.active(false);
        Ok(true)
    }
    pub(crate) async fn reconcile(
        &self,
        desired: Vec<Arc<dyn Adapter>>,
        managed: &[&str],
    ) -> Result<(), RouteError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(closing());
        }
        let mut keys = HashSet::new();
        for adapter in &desired {
            if !keys.insert(adapter.key().to_owned()) {
                return Err(RouteError::new(
                    "gateway_request_failed",
                    format!(
                        "duplicate channel adapter in reconciliation: {}",
                        adapter.key()
                    ),
                ));
            }
        }
        for key in managed {
            if !keys.contains(*key) {
                self.stop(key).await?;
            }
        }
        for adapter in desired {
            self.replace(adapter).await?;
        }
        Ok(())
    }
    pub(crate) async fn close(&self) -> Result<(), RouteError> {
        self.closing.store(true, Ordering::SeqCst);
        let keys = self
            .entries
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut errors = Vec::new();
        for key in keys {
            if let Err(error) = self.stop(&key).await {
                errors.push(error.message);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(RouteError::new("gateway_request_failed", errors.join("; ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, net::SocketAddr};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::Notify,
    };

    struct SocketAdapter {
        key: &'static str,
        name: &'static str,
        address: SocketAddr,
        starts: Mutex<VecDeque<bool>>,
        events: Arc<Mutex<Vec<String>>>,
        entered: Option<Arc<Notify>>,
        release: Option<Arc<Notify>>,
    }
    struct SocketHandle {
        task: Mutex<Option<tokio::task::JoinHandle<()>>>,
        name: &'static str,
        events: Arc<Mutex<Vec<String>>>,
    }
    impl Handle for SocketHandle {
        fn stop(&self) -> ChannelCall<'_, ()> {
            Box::pin(async {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("stop:{}", self.name));
                let task = self.task.lock().unwrap().take();
                if let Some(task) = task {
                    task.abort();
                    let _ = task.await;
                }
                Ok(())
            })
        }
    }
    impl Drop for SocketHandle {
        fn drop(&mut self) {
            if let Some(task) = self.task.lock().unwrap().take() {
                task.abort();
            }
        }
    }
    impl Adapter for SocketAdapter {
        fn key(&self) -> &str {
            self.key
        }
        fn active(&self, active: bool) {
            self.events
                .lock()
                .unwrap()
                .push(format!("active:{}={active}", self.name));
        }
        fn start(&self) -> ChannelCall<'_, Box<dyn Handle>> {
            Box::pin(async {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("start:{}", self.name));
                if let Some(entered) = &self.entered {
                    entered.notify_one();
                }
                if let Some(release) = &self.release {
                    release.notified().await;
                }
                if !self.starts.lock().unwrap().pop_front().unwrap_or(true) {
                    return Err(RouteError::new("fixture_failure", "startup failed"));
                }
                let listener = TcpListener::bind(self.address)
                    .await
                    .map_err(|error| RouteError::new("listen_failed", error.to_string()))?;
                let name = self.name;
                let task = tokio::spawn(async move {
                    while let Ok((mut socket, _)) = listener.accept().await {
                        let _ = socket.write_all(name.as_bytes()).await;
                    }
                });
                Ok(Box::new(SocketHandle {
                    task: Mutex::new(Some(task)),
                    name,
                    events: self.events.clone(),
                }) as Box<dyn Handle>)
            })
        }
    }
    async fn address() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap()
    }
    fn adapter(
        key: &'static str,
        name: &'static str,
        address: SocketAddr,
        events: &Arc<Mutex<Vec<String>>>,
        starts: &[bool],
    ) -> Arc<SocketAdapter> {
        Arc::new(SocketAdapter {
            key,
            name,
            address,
            starts: Mutex::new(starts.iter().copied().collect()),
            events: events.clone(),
            entered: None,
            release: None,
        })
    }
    async fn read(address: SocketAddr) -> String {
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let mut text = String::new();
        socket.read_to_string(&mut text).await.unwrap();
        text
    }
    #[tokio::test]
    async fn real_listener_replacement_rolls_back_same_adapter_and_disables() {
        let runtime = ChannelRuntime::default();
        let address = address().await;
        let events: Arc<Mutex<Vec<String>>> = Arc::default();
        let first = adapter("test", "first", address, &events, &[true, true]);
        runtime.replace(first).await.unwrap();
        assert_eq!(read(address).await, "first");
        assert_eq!(
            runtime
                .replace(adapter("test", "bad", address, &events, &[false]))
                .await
                .unwrap_err()
                .message,
            "startup failed"
        );
        assert_eq!(read(address).await, "first");
        runtime
            .replace(adapter("test", "next", address, &events, &[true]))
            .await
            .unwrap();
        assert_eq!(read(address).await, "next");
        runtime.reconcile(vec![], &["test"]).await.unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
        assert_eq!(
            *events.lock().unwrap(),
            [
                "start:first",
                "active:first=true",
                "stop:first",
                "active:first=false",
                "start:bad",
                "start:first",
                "active:first=true",
                "stop:first",
                "active:first=false",
                "start:next",
                "active:next=true",
                "stop:next",
                "active:next=false"
            ]
        );
    }
    #[tokio::test]
    async fn rollback_failure_is_reported_and_duplicate_keys_have_no_effect() {
        let runtime = ChannelRuntime::default();
        let address = address().await;
        let events: Arc<Mutex<Vec<String>>> = Arc::default();
        runtime
            .replace(adapter("test", "first", address, &events, &[true, false]))
            .await
            .unwrap();
        let before = events.lock().unwrap().len();
        let error = runtime
            .reconcile(
                vec![
                    adapter("dup", "one", address, &events, &[]),
                    adapter("dup", "two", address, &events, &[]),
                ],
                &["test"],
            )
            .await
            .unwrap_err();
        assert!(error.message.contains("duplicate channel adapter"));
        assert_eq!(events.lock().unwrap().len(), before);
        assert_eq!(read(address).await, "first");
        let error = runtime
            .replace(adapter("test", "bad", address, &events, &[false]))
            .await
            .unwrap_err();
        assert!(error.message.contains("replacement and rollback failed"));
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
        runtime.close().await.unwrap();
    }
    #[tokio::test]
    async fn closing_drains_inflight_start_and_rejects_queued_same_key() {
        let runtime = Arc::new(ChannelRuntime::default());
        let address = address().await;
        let events: Arc<Mutex<Vec<String>>> = Arc::default();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let slow = Arc::new(SocketAdapter {
            key: "test",
            name: "slow",
            address,
            events: events.clone(),
            starts: Mutex::new(VecDeque::new()),
            entered: Some(entered.clone()),
            release: Some(release.clone()),
        });
        let start = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.replace(slow).await }
        });
        entered.notified().await;
        let queued = tokio::spawn({
            let runtime = runtime.clone();
            let next = adapter("test", "queued", address, &events, &[]);
            async move { runtime.replace(next).await }
        });
        let close = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.close().await }
        });
        while !runtime.closing.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        assert!(!close.is_finished());
        release.notify_one();
        assert_eq!(
            start.await.unwrap().unwrap_err().message,
            "channel server is closing"
        );
        assert_eq!(
            queued.await.unwrap().unwrap_err().message,
            "channel server is closing"
        );
        close.await.unwrap().unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
        assert!(!events.lock().unwrap().iter().any(|e| e == "start:queued"));
    }
    #[tokio::test]
    async fn independent_keys_start_while_one_key_is_waiting() {
        let runtime = Arc::new(ChannelRuntime::default());
        let one = address().await;
        let two = address().await;
        let events: Arc<Mutex<Vec<String>>> = Arc::default();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let slow = Arc::new(SocketAdapter {
            key: "one",
            name: "slow",
            address: one,
            events: events.clone(),
            starts: Mutex::new(VecDeque::new()),
            entered: Some(entered.clone()),
            release: Some(release.clone()),
        });
        let start = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.replace(slow).await }
        });
        entered.notified().await;
        runtime
            .replace(adapter("two", "other", two, &events, &[]))
            .await
            .unwrap();
        assert_eq!(read(two).await, "other");
        release.notify_one();
        start.await.unwrap().unwrap();
        runtime.close().await.unwrap();
    }
    #[test]
    fn config_notifications_only_reload_adapter_paths() {
        let notice = |paths| {
            pilotdeck_protocol::parse_json(&format!(
                r#"{{"name":"config_changed","payload":{{"changedPaths":{paths}}}}}"#
            ))
            .unwrap()
        };
        assert!(adapter_notice(&notice(r#"["adapters"]"#)));
        assert!(adapter_notice(&notice(
            r#"["agent.foo","adapters.telegram.token"]"#
        )));
        assert!(!adapter_notice(&notice(
            r#"["adaptersOther","agent.adapters"]"#
        )));
        assert!(!adapter_notice(
            &pilotdeck_protocol::parse_json(
                r#"{"name":"other","payload":{"changedPaths":["adapters"]}}"#
            )
            .unwrap()
        ));
    }

    struct Gateway;
    impl crate::config::channel_commands::ChannelGateway for Gateway {
        fn request<'a>(
            &'a self,
            _: &'a str,
            _: serde_json::Value,
        ) -> ChannelCall<'a, serde_json::Value> {
            Box::pin(async { Ok(serde_json::json!({})) })
        }
        fn turn(&self, _: serde_json::Value) -> ChannelCall<'_, Vec<serde_json::Value>> {
            Box::pin(async { Ok(vec![]) })
        }
    }
    struct LoginChannel(Arc<std::sync::atomic::AtomicUsize>);
    impl crate::config::routes::WeixinChannel for LoginChannel {
        fn start(&self) -> ChannelCall<'_, ()> {
            Box::pin(async {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
        fn stop(&self) -> ChannelCall<'_, ()> {
            Box::pin(async {
                self.0.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            })
        }
    }
    struct LoginFactory(Arc<std::sync::atomic::AtomicUsize>);
    impl crate::config::routes::WeixinChannelFactory for LoginFactory {
        fn load_and_create(
            &self,
        ) -> ChannelCall<'_, Arc<dyn crate::config::routes::WeixinChannel>> {
            Box::pin(async {
                Ok(Arc::new(LoginChannel(self.0.clone()))
                    as Arc<dyn crate::config::routes::WeixinChannel>)
            })
        }
    }
    async fn health(address: SocketAddr) -> String {
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        socket
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut text = String::new();
        socket.read_to_string(&mut text).await.unwrap();
        text
    }
    async fn wait_for(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(std::time::Duration::from_secs(4), async {
            while !condition() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn real_config_notification_rebinds_http_switches_feishu_and_disables_channels() {
        let root = std::env::current_dir()
            .unwrap()
            .parent()
            .unwrap()
            .join("_tmp/20261031-fill-gateway")
            .join(format!("channel-reload-{}", uuid::Uuid::new_v4()));
        let home = root.join("home");
        let cwd = root.join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let config_path = home.join("config.json");
        let one = address().await;
        let two = address().await;
        let write_config = |port: u16, enabled: bool, token: &str| {
            std::fs::write(&config_path, serde_json::json!({"schemaVersion":1,
                "agent":{"model":"parity/deterministic"},
                "model":{"providers":{"parity":{"protocol":"openai","url":"http://127.0.0.1:1/v1","apiKey":"fixture","models":{"deterministic":{}}}}},
                "adapters":{
                "apiServer":{"enabled":enabled,"extra":{"port":port}},
                "feishu":{"enabled":enabled,"appId":"fixture","appSecret":"fixture","verifyToken":token,"connectionMode":"webhook"}
            }}).to_string()).unwrap();
        };
        write_config(one.port(), true, "one");
        let env: crate::config::Env = [
            ("PILOT_HOME".into(), home.to_string_lossy().into_owned()),
            (
                "PILOTDECK_CONFIG_PATH".into(),
                config_path.to_string_lossy().into_owned(),
            ),
        ]
        .into_iter()
        .collect();
        let config = crate::config::routes::bootstrap::ConfigBootstrap::new(
            home.clone(),
            cwd.clone(),
            env.clone(),
        )
        .unwrap();
        let automation =
            crate::cron::Automation::new(home.clone(), cwd.clone(), root.join("unused-sidecar"))
                .await
                .unwrap();
        // The public login RPC starts an owner even while its configured
        // adapter is disabled. This fixture uses no QR server or account.
        let active_login = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let login = Arc::new(crate::config::routes::WeixinChannelHost::new(Arc::new(
            LoginFactory(active_login.clone()),
        )));
        config.state.lock().unwrap().weixin_login = Some(login.clone());
        crate::config::routes::native::call(
            config.state.clone(),
            "prepare_weixin_login".into(),
            pilotdeck_protocol::JsValue::Null,
        )
        .await
        .unwrap();
        assert_eq!(active_login.load(Ordering::SeqCst), 1);
        let owner = InstalledChannels::new(
            home.clone(),
            cwd.clone(),
            env,
            Arc::new(Gateway),
            automation.host.clone(),
            config.state.clone(),
            login.clone(),
            config.weixin_delivery.clone(),
        );
        let callback = owner.feishu_webhook();
        owner.start();
        wait_for(|| automation.host.feishu_delivery.lock().unwrap().is_some()).await;
        assert!(health(one).await.starts_with("HTTP/1.1 200"));
        let original = automation
            .host
            .feishu_delivery
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        let request = || {
            hyper::Request::builder()
                .body(r#"{"type":"url_verification","challenge":"live"}"#.to_owned())
                .unwrap()
        };
        assert_eq!(callback(request()).await.unwrap().unwrap().status(), 200);
        write_config(two.port(), true, "two");
        // A real CONFIG reload from another project must not replace this listener.
        let other_cwd = root.join("other-project");
        std::fs::create_dir_all(&other_cwd).unwrap();
        let other_env: crate::config::Env = [
            ("PILOT_HOME".into(), home.to_string_lossy().into_owned()),
            (
                "PILOTDECK_CONFIG_PATH".into(),
                config_path.to_string_lossy().into_owned(),
            ),
        ]
        .into_iter()
        .collect();
        let other = Arc::new(Mutex::new(
            crate::config::routes::ConfigRoutes::new(home.clone(), other_cwd, other_env).unwrap(),
        ));
        write_config(two.port(), true, "three");
        crate::config::routes::native::call(
            other.clone(),
            "reload_config".into(),
            pilotdeck_protocol::JsValue::Null,
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(Arc::ptr_eq(
            automation
                .host
                .feishu_delivery
                .lock()
                .unwrap()
                .as_ref()
                .unwrap(),
            &original
        ));
        crate::config::routes::native::call(
            config.state.clone(),
            "reload_config".into(),
            pilotdeck_protocol::JsValue::Null,
        )
        .await
        .unwrap();
        wait_for(|| {
            automation
                .host
                .feishu_delivery
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|current| !Arc::ptr_eq(current, &original))
        })
        .await;
        tokio::time::timeout(std::time::Duration::from_secs(4), async {
            while tokio::net::TcpStream::connect(two).await.is_err() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(health(two).await.starts_with("HTTP/1.1 200"));
        assert_eq!(active_login.load(Ordering::SeqCst), 0);
        assert!(tokio::net::TcpStream::connect(one).await.is_err());
        assert_eq!(callback(request()).await.unwrap().unwrap().status(), 200);
        write_config(two.port(), false, "two");
        crate::config::routes::native::call(
            config.state.clone(),
            "reload_config".into(),
            pilotdeck_protocol::JsValue::Null,
        )
        .await
        .unwrap();
        wait_for(|| automation.host.feishu_delivery.lock().unwrap().is_none()).await;
        assert!(callback(request()).await.unwrap().is_none());
        owner.close().await.unwrap();
        assert!(tokio::net::TcpStream::connect(two).await.is_err());
        // A second login after disable must also be drained by close, without
        // requiring a configured adapter entry.
        crate::config::routes::native::call(
            config.state.clone(),
            "prepare_weixin_login".into(),
            pilotdeck_protocol::JsValue::Null,
        )
        .await
        .unwrap();
        assert_eq!(active_login.load(Ordering::SeqCst), 1);
        owner.close().await.unwrap();
        assert_eq!(active_login.load(Ordering::SeqCst), 0);
        crate::config::routes::native::shutdown(other)
            .await
            .unwrap();
        config.close().await.unwrap();
        automation.stop().await.unwrap();
    }

    #[test]
    fn production_mapper_persistence_writes_telegram_state_json() {
        use crate::config::channel_commands::ChannelSessionState;

        let root =
            std::env::temp_dir().join(format!("pdrs-mapper-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("channels")).unwrap();
        let store = crate::config::routes::ChannelStatePersistence::new(root.join("channels"));
        let (mut mapper, on_change) = (
            ChannelSessionState::default(),
            Some(store.saver("telegram")),
        );
        mapper.reset("telegram", "42");
        ChannelSessionState::persist_channel_mapper(&mapper, &on_change);
        store.flush();
        assert!(store.file_path("telegram").exists());
        let saved: Value =
            serde_json::from_slice(&std::fs::read(store.file_path("telegram")).unwrap()).unwrap();
        assert!(
            saved["activeByChatId"]["42"]
                .as_str()
                .unwrap()
                .contains(":s_")
        );
        let _ = std::fs::remove_dir_all(root);
    }
}

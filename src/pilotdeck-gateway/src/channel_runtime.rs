//! Serialized channel replacement, sharing the existing native turn host.
use crate::config::{Env, channel_commands::NativeChannelGateway, routes::{ChannelCall, RouteError, Result, channel_http::{ApiServerChannel, ApiServerOptions, WebhookChannel, WebhookOptions, HttpChannelHandle}}};
use crate::runtime::turn::gateway::TurnGateway;
use pilotdeck_protocol::JsValue;
use std::{path::PathBuf, time::Duration};
use std::{collections::{HashMap, HashSet}, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}};

pub trait Handle: Send + Sync { fn stop(&self) -> ChannelCall<'_, ()>; }
pub trait Adapter: Send + Sync {
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
pub struct ChannelRuntime {
    closing: AtomicBool,
    entries: Mutex<HashMap<String, Entry>>,
}
fn closing() -> RouteError {
    RouteError::new("gateway_request_failed", "channel server is closing")
}
impl ChannelRuntime {
    pub fn new() -> Arc<Self> { Arc::new(Self::default()) }
    fn entry(&self, key: &str) -> Entry {
        self.entries
            .lock()
            .unwrap()
            .entry(key.into())
            .or_default()
            .clone()
    }
    pub async fn replace(&self, next: Arc<dyn Adapter>) -> std::result::Result<(), RouteError> {
        self.replace_queued(next, None).await
    }
    async fn replace_queued(
        &self,
        next: Arc<dyn Adapter>,
        queued: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> std::result::Result<(), RouteError> {
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
    pub async fn stop(&self, key: &str) -> std::result::Result<bool, RouteError> {
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
    pub async fn reconcile(
        &self,
        desired: Vec<Arc<dyn Adapter>>,
        managed: &[&str],
    ) -> std::result::Result<(), RouteError> {
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
    pub async fn close(&self) -> std::result::Result<(), RouteError> {
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

impl Handle for HttpChannelHandle {
    fn stop(&self) -> ChannelCall<'_, ()> {
        Box::pin(async move {
            self.close().await;
            Ok(())
        })
    }
}
enum HttpAdapter { Api(Arc<ApiServerChannel>), Webhook(Arc<WebhookChannel>) }
impl Adapter for HttpAdapter {
    fn key(&self) -> &str { match self { Self::Api(_) => "api-server", Self::Webhook(_) => "webhook" } }
    fn active(&self, _: bool) {}
    fn start(&self) -> ChannelCall<'_, Box<dyn Handle>> {
        Box::pin(async move {
            let handle = match self { Self::Api(channel) => channel.start().await?, Self::Webhook(channel) => channel.start().await? };
            Ok(Box::new(handle) as Box<dyn Handle>)
        })
    }
}
#[derive(Default)]
pub struct HttpChannelsOptions {
    pub api: Option<ApiServerOptions>,
    pub webhook: Option<WebhookOptions>,
}
impl HttpChannelsOptions {
    /// Only explicitly enabled adapters consume their options/credentials.
    pub fn from_snapshot(snapshot: &JsValue, env: &Env) -> Result<Self> {
        let adapters = snapshot.get("config").and_then(|v| v.get("adapters"));
        let enabled = |key| adapters.and_then(|v| v.get(key))
            .filter(|v| v.get("enabled") == Some(&JsValue::Bool(true)));
        let text = |v: Option<&JsValue>| v.and_then(JsValue::as_string).map(|s| String::from_utf16_lossy(s.units()));
        let port = |v: Option<&JsValue>, fallback: Option<&String>, default: u16| -> Result<u16> {
            match v {
                Some(v) => v.as_number().filter(|v| v.is_finite() && v.fract()==0.0 && (0.0..=65535.0).contains(v))
                    .map(|v| v as u16).ok_or_else(|| RouteError::new("CHANNEL_CONFIG_INVALID", "invalid channel port")),
                None => match fallback { Some(v) => v.parse().map_err(|_| RouteError::new("CHANNEL_CONFIG_INVALID", "invalid channel port")), None => Ok(default) }
            }
        };
        let api = enabled("apiServer").map(|config| -> Result<_> {
            let mut options = ApiServerOptions::default();
            options.port = port(config.get("extra").and_then(|v| v.get("port")), env.get("API_SERVER_PORT"), options.port)?;
            options.host = env.get("API_SERVER_HOST").cloned().unwrap_or(options.host);
            options.api_key = text(config.get("apiKey")).or_else(|| env.get("API_SERVER_KEY").cloned()).unwrap_or_default();
            options.model_name = env.get("API_SERVER_MODEL_NAME").cloned().unwrap_or(options.model_name);
            options.cors_origins = env.get("API_SERVER_CORS_ORIGINS").map(|v| v.split(',')
                .map(crate::metadata::js_trim).filter(|v| !v.is_empty()).map(str::to_owned).collect()).unwrap_or_default();
            Ok(options)
        }).transpose()?;
        let webhook = enabled("webhook").map(|config| -> Result<_> {
            let mut options = WebhookOptions::default();
            let extra = config.get("extra");
            options.port = port(extra.and_then(|v| v.get("port")), env.get("WEBHOOK_PORT"), options.port)?;
            options.host = text(extra.and_then(|v| v.get("host"))).or_else(|| env.get("WEBHOOK_HOST").cloned()).unwrap_or(options.host);
            options.secret = text(extra.and_then(|v| v.get("secret"))).unwrap_or_default();
            if let Some(routes) = extra.and_then(|v| v.get("routes")) {
                let source = pilotdeck_protocol::stringify_json(routes).map_err(|e| RouteError::new("CHANNEL_CONFIG_INVALID", e.to_string()))?
                    .ok_or_else(|| RouteError::new("CHANNEL_CONFIG_INVALID", "webhook routes must be an object"))?;
                options.routes = serde_json::from_str(&source).map_err(|_| RouteError::new("CHANNEL_CONFIG_INVALID", "webhook routes must be an object"))?;
            }
            Ok(options)
        }).transpose()?;
        Ok(Self { api, webhook })
    }
}
pub struct ChannelOwner {
    pub gateway: Arc<NativeChannelGateway>,
    runtime: Arc<ChannelRuntime>,
    store: Option<crate::config::store::ConfigStore>,
    subscription: Option<crate::config::store::Subscription>,
    watcher: Option<crate::config::store::WatchHandle>,
    reload: Option<tokio::task::JoinHandle<()>>,
}
impl ChannelOwner {
    pub async fn start(host: TurnGateway, project: PathBuf, options: HttpChannelsOptions) -> Result<Self> {
        let gateway = NativeChannelGateway::new(host, project);
        let runtime = ChannelRuntime::new();
        if let Err(error) = reconcile_http(&runtime, &gateway, options).await {
            let _ = runtime.close().await;
            let _ = gateway.close().await;
            return Err(error);
        }
        Ok(Self { gateway, runtime, store: None, subscription: None, watcher: None, reload: None })
    }
    pub async fn close(mut self) -> Result<()> {
        if let Some(subscription) = self.subscription.take() { subscription.unsubscribe(); }
        if let Some(watcher) = self.watcher.take() { watcher.stop().await; }
        if let Some(task) = self.reload.take() { task.abort(); let _ = task.await; }
        self.runtime.closing.store(true, Ordering::SeqCst);
        // Cancel bridges before closing HTTP waiters, so no active turn loses its owner.
        let gateway = self.gateway.close().await;
        let runtime = self.runtime.close().await;
        gateway.and(runtime)
    }
}
impl Drop for ChannelOwner {
    fn drop(&mut self) {
        if let Some(subscription) = self.subscription.take() { subscription.unsubscribe(); }
        self.watcher.take();
        if let Some(task) = self.reload.take() { task.abort(); }
        self.runtime.closing.store(true, Ordering::SeqCst);
        let runtime = self.runtime.clone();
        let gateway = self.gateway.clone();
        if let Ok(executor) = tokio::runtime::Handle::try_current() {
            executor.spawn(async move { let _ = gateway.close().await; let _ = runtime.close().await; });
        }
    }
}
async fn reconcile_http(runtime: &ChannelRuntime, gateway: &Arc<NativeChannelGateway>, options: HttpChannelsOptions) -> Result<()> {
    let mut desired: Vec<Arc<dyn Adapter>> = Vec::new();
    if let Some(options) = options.api { desired.push(Arc::new(HttpAdapter::Api(ApiServerChannel::new(options, gateway.clone())))); }
    if let Some(options) = options.webhook { desired.push(Arc::new(HttpAdapter::Webhook(WebhookChannel::new(options, gateway.clone())))); }
    runtime.reconcile(desired, &["api-server", "webhook"]).await
}
/// Embedding entry: explicit isolated home/project, the existing host, no token file.
pub async fn start_channels(home: PathBuf, project: PathBuf, env: Env, host: TurnGateway) -> Result<ChannelOwner> {
    let mut options = crate::config::store::StoreOptions::new(&home, &project, env.clone());
    options.project = Some(project.to_string_lossy().into_owned());
    let store = crate::config::store::ConfigStore::new(options)
        .map_err(|e| RouteError::new(e.code, String::from_utf16_lossy(e.message.units())))?;
    let (changed, mut changes) = tokio::sync::mpsc::unbounded_channel();
    let subscription = store.subscribe(Arc::new(move |event| {
        if event.changed_paths.iter().any(|s| { let s = String::from_utf16_lossy(s.units()); s == "adapters" || s.starts_with("adapters.") }) { let _ = changed.send(()); }
    }));
    let snapshot = store.snapshot();
    let mut owner = ChannelOwner::start(host, project, HttpChannelsOptions::from_snapshot(&snapshot, &env)?).await?;
    let watcher = match store.start_watching(crate::config::store::WatchOptions::default()) {
        Ok(watcher) => watcher, Err(error) => {
            owner.close().await?;
            return Err(RouteError::new(error.code, String::from_utf16_lossy(error.message.units())));
        }
    };
    let runtime = owner.runtime.clone();
    let gateway = owner.gateway.clone();
    let reload_store = store.clone();
    owner.reload = Some(tokio::spawn(async move {
        while changes.recv().await.is_some() {
            let snapshot = reload_store.snapshot();
            match HttpChannelsOptions::from_snapshot(&snapshot, &env) {
                Ok(options) => if let Err(error) = reconcile_http(&runtime, &gateway, options).await { eprintln!("channel reload failed: {}", error.code); },
                Err(error) => eprintln!("channel configuration rejected: {}", error.code),
            }
        }
    }));
    owner.watcher = Some(watcher);
    owner.subscription = Some(subscription);
    owner.store = Some(store);
    Ok(owner)
}

//! A8.3 `api-server` channel: the TS adapter owns its own HTTP listener.
//! Listener, method dispatch, CORS preflight, auth, error bodies and streaming
//! mirror `src/adapters/channel/api-server/ApiServerChannel.ts` (baseline
//! cc7c4e6). Inbound turns enter the shared A7 acceptor through
//! `channel::protocol::TurnGateway`; this module never creates a session owner.
pub mod error_body;
pub mod mapper;
pub mod render;
pub mod server;
#[cfg(test)]
mod tests;

use self::mapper::SessionMapper;
use super::protocol::*;
use pilotdeck_protocol::JsValue;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::watch, task::JoinHandle};

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: f64 = 8642.0;
pub const DEFAULT_MODEL_NAME: &str = "pilotdeck-gateway";
pub const MAX_REQUEST_BYTES: usize = 1_000_000;
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Constructor options of the TS adapter. `port`/`cors_origins` accept JS
/// values because `loadEnabledChannels` forwards `cfg.extra` unchanged; `env`
/// lets a host inject `API_SERVER_*` without touching process globals in tests.
#[derive(Clone, Default)]
pub struct ApiServerOptions {
    pub port: Option<JsValue>,
    pub api_key: Option<String>,
    pub host: Option<String>,
    pub model_name: Option<String>,
    pub cors_origins: Option<JsValue>,
    pub mapper: Option<Arc<SessionMapper>>,
    /// Rust test seam for the TS 300 s constants; production keeps the default.
    pub request_timeout: Option<Duration>,
    pub uuid: Option<Arc<dyn Fn() -> String + Send + Sync>>,
}

struct Listener {
    address: SocketAddr,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

pub(crate) struct Inner {
    key: ChannelKey,
    options: ApiServerOptions,
    env: HashMap<String, String>,
    uuid: Arc<dyn Fn() -> String + Send + Sync>,
    mapper: Arc<SessionMapper>,
    active: Arc<Mutex<std::collections::HashSet<String>>>,
    timeout: Duration,
    state: Mutex<Option<Listener>>,
    logger: Mutex<Option<ChannelLog>>,
    gateway: Mutex<Option<Arc<dyn TurnGateway>>>,
}

#[derive(Clone)]
pub struct ApiServerChannel(Arc<Inner>);

impl ApiServerChannel {
    /// Reads `API_SERVER_*` at construction, like the TS constructor.
    pub fn new(options: ApiServerOptions) -> Self {
        Self::with_env(options, std::env::vars().collect())
    }

    pub fn with_env(options: ApiServerOptions, env: HashMap<String, String>) -> Self {
        let mapper = options.mapper.clone().unwrap_or_else(|| {
            Arc::new(match &options.uuid {
                Some(uuid) => SessionMapper::with_uuid(uuid.clone()),
                None => SessionMapper::new(),
            })
        });
        let uuid = options.uuid.clone().unwrap_or_else(|| {
            Arc::new(|| uuid::Uuid::new_v4().to_string()) as Arc<dyn Fn() -> String + Send + Sync>
        });
        Self(Arc::new(Inner {
            key: ChannelKey::new("api_server"),
            timeout: options.request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT),
            options,
            env,
            uuid,
            mapper,
            active: Arc::default(),
            state: Mutex::new(None),
            logger: Mutex::new(None),
            gateway: Mutex::new(None),
        }))
    }

    /// The bound listener address, for hosts and tests that bind port 0.
    pub fn bound_addr(&self) -> Option<SocketAddr> {
        self.0
            .state
            .lock()
            .expect("api_server listener state")
            .as_ref()
            .map(|listener| listener.address)
    }

    fn log(&self, level: &str, message: &str) {
        if let Some(log) = self
            .0
            .logger
            .lock()
            .expect("api_server logger")
            .as_ref()
            .cloned()
        {
            log(level, message);
        }
    }

    fn resolve(&self, config: Option<&JsValue>) -> Resolved {
        let extra = config.and_then(|config| config.get("extra"));
        let config_port = extra.and_then(|extra| extra.get("port"));
        let config_key = config.and_then(|config| config.get("apiKey"));
        let port = self
            .0
            .options
            .port
            .as_ref()
            .filter(|value| !value.is_undefined() && !matches!(value, JsValue::Null))
            .or(config_port
                .filter(|value| !value.is_undefined() && !matches!(value, JsValue::Null)))
            .cloned()
            .or_else(|| {
                self.0
                    .env
                    .get("API_SERVER_PORT")
                    .map(|value| JsValue::from(value.as_str()))
            });
        let api_key = self
            .0
            .options
            .api_key
            .clone()
            .or_else(|| {
                config_key
                    .and_then(|value| value.as_string())
                    .map(js_string_value)
            })
            .or_else(|| self.0.env.get("API_SERVER_KEY").cloned())
            .unwrap_or_default();
        Resolved {
            host: self
                .0
                .options
                .host
                .clone()
                .or_else(|| self.0.env.get("API_SERVER_HOST").cloned())
                .unwrap_or_else(|| DEFAULT_HOST.to_owned()),
            port: port.map(|value| server::js_number(&value)),
            api_key,
            model_name: self
                .0
                .options
                .model_name
                .clone()
                .or_else(|| self.0.env.get("API_SERVER_MODEL_NAME").cloned())
                .unwrap_or_else(|| DEFAULT_MODEL_NAME.to_owned()),
            cors_origins: match &self.0.options.cors_origins {
                Some(value) => parse_cors_origins(value),
                None => match self.0.env.get("API_SERVER_CORS_ORIGINS") {
                    Some(value) => parse_cors_origins(&JsValue::from(value.as_str())),
                    None => Vec::new(),
                },
            },
        }
    }
}

pub(crate) struct Resolved {
    pub host: String,
    pub port: Option<f64>,
    pub api_key: String,
    pub model_name: String,
    pub cors_origins: Vec<String>,
}

fn js_string_value(value: &pilotdeck_protocol::JsString) -> String {
    value
        .to_utf8()
        .unwrap_or_else(|_| String::from_utf16_lossy(value.units()))
}

/// `parseCorsOrigins`: a string splits on commas, an array maps to strings;
/// empty entries are dropped. Nullish/false/0 force no CORS.
pub(crate) fn parse_cors_origins(value: &JsValue) -> Vec<String> {
    let entries: Vec<String> = match value {
        JsValue::Undefined | JsValue::Null | JsValue::Bool(false) => Vec::new(),
        JsValue::Number(number) if *number == 0.0 => Vec::new(),
        JsValue::String(text) => js_string_value(text)
            .split(',')
            .map(|part| crate::metadata::js_trim(part).to_owned())
            .filter(|part| !part.is_empty())
            .collect(),
        JsValue::Array(items) => items
            .iter()
            .map(|item| match item {
                JsValue::String(text) => js_string_value(text),
                JsValue::Undefined => "undefined".to_owned(),
                JsValue::Null => "null".to_owned(),
                JsValue::Bool(value) => value.to_string(),
                JsValue::Number(number) => crate::dispatch::error_number(*number),
                _ => "[object Object]".to_owned(),
            })
            .filter(|item| !item.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    entries
}

impl ChannelAdapter for ApiServerChannel {
    fn channel_key(&self) -> &ChannelKey {
        &self.0.key
    }

    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        let inner = self.0.clone();
        *inner.logger.lock().expect("api_server logger") = deps.log.clone();
        *inner.gateway.lock().expect("api_server gateway") = deps.turn_gateway.clone();
        Box::pin(async move {
            let channel = ApiServerChannel(inner.clone());
            let resolved = channel.resolve(deps.config.as_deref());
            let Some(port) = resolved.port else {
                return Ok(noop_handle());
            };
            if let Some(listener) = inner
                .state
                .lock()
                .expect("api_server listener state")
                .as_ref()
            {
                // TS's second start drops the old `this.server` reference and
                // fails bind; keeping the live listener is the safe equivalent.
                channel.log(
                    "error",
                    &format!(
                        "api-server: failed to start: server already listening on {}",
                        listener.address
                    ),
                );
                return Ok(noop_handle());
            }
            let address = match server::resolve_address(&resolved.host, port).await {
                Ok(address) => address,
                Err(error) => {
                    channel.log(
                        "error",
                        &format!("api-server: failed to start: Error: {error}"),
                    );
                    return Ok(noop_handle());
                }
            };
            let listener = match TcpListener::bind(address).await {
                Ok(listener) => listener,
                Err(error) => {
                    channel.log(
                        "error",
                        &format!("api-server: failed to start: Error: {error}"),
                    );
                    return Ok(noop_handle());
                }
            };
            let bound = match listener.local_addr() {
                Ok(address) => address,
                Err(error) => {
                    channel.log(
                        "error",
                        &format!("api-server: failed to start: Error: {error}"),
                    );
                    return Ok(noop_handle());
                }
            };
            let (stop, stop_rx) = watch::channel(false);
            let shared = Arc::new(server::ServerShared {
                host: resolved.host.clone(),
                port: port,
                api_key: resolved.api_key,
                model_name: resolved.model_name,
                cors_origins: resolved.cors_origins,
                gateway: deps.turn_gateway,
                uuid: inner.uuid.clone(),
                mapper: inner.mapper.clone(),
                active: inner.active.clone(),
                timeout: inner.timeout,
                log: deps.log.clone(),
            });
            let task = tokio::spawn(server::accept_loop(listener, shared, stop_rx));
            *inner.state.lock().expect("api_server listener state") = Some(Listener {
                address: bound,
                stop,
                task,
            });
            channel.log(
                "info",
                &format!(
                    "api-server: listening on http://{}:{}",
                    resolved.host,
                    server::format_port(port)
                ),
            );
            Ok(Arc::new(ApiServerHandle { inner }) as Arc<dyn ChannelHandle>)
        })
    }
}

struct ApiServerHandle {
    inner: Arc<Inner>,
}

impl ChannelHandle for ApiServerHandle {
    fn stop(&self, reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        let inner = self.inner.clone();
        let reason = reason.to_owned();
        Box::pin(async move {
            ApiServerChannel(inner.clone())
                .log("info", &format!("api-server: stopping ({reason})"));
            let listener = inner
                .state
                .lock()
                .expect("api_server listener state")
                .take();
            let Some(listener) = listener else {
                return Ok(());
            };
            listener.stop.send_replace(true);
            if let Err(error) = listener.task.await {
                return Err(ChannelError::error(format!(
                    "api-server: stop failed: {error}"
                )));
            }
            Ok(())
        })
    }
}

fn noop_handle() -> Arc<dyn ChannelHandle> {
    struct Noop;
    impl ChannelHandle for Noop {
        fn stop(&self, _reason: &str) -> BoxFuture<Result<(), ChannelError>> {
            Box::pin(async { Ok(()) })
        }
    }
    Arc::new(Noop)
}

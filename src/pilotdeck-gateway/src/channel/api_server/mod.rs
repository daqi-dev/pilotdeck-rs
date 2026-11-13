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
use pilotdeck_protocol::{JsObject, JsValue};
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
/// The *turn* deadline (`ApiServerChannel.ts` `DEFAULT_REQUEST_TIMEOUT`).
/// Same 300 s value as Node's `requestTimeout` default but a different
/// mechanism — do not reuse it for the body read (#1620).
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Node `http.createServer`'s default `requestTimeout` (v22.22.0: 300000 ms):
/// the bound on receiving the whole request. The channel applies it to the
/// body read only, like the webhook lane's `DEFAULT_REQUEST_TIMEOUT`.
pub const DEFAULT_BODY_READ_TIMEOUT: Duration = Duration::from_secs(300);

/// Constructor options of the TS adapter. `port`/`apiKey`/`cors_origins` accept
/// JS values because `loadEnabledChannels` forwards `cfg.extra` / `cfg.apiKey`
/// unchanged; `env` lets a host inject `API_SERVER_*` without touching process
/// globals in tests.
#[derive(Clone, Default)]
pub struct ApiServerOptions {
    pub port: Option<JsValue>,
    pub api_key: Option<JsValue>,
    pub host: Option<String>,
    pub model_name: Option<String>,
    pub cors_origins: Option<JsValue>,
    pub mapper: Option<Arc<SessionMapper>>,
    /// Rust test seam for the TS 300 s *turn* timeout; production keeps the
    /// default. Not the body-read deadline — that is
    /// [`Self::body_read_timeout`].
    pub request_timeout: Option<Duration>,
    /// Rust test seam for Node's `requestTimeout` body-read deadline
    /// ([`DEFAULT_BODY_READ_TIMEOUT`]). TS exposes no channel option for it
    /// (it is the server default), so this exists only for tests, like the
    /// webhook lane's `request_timeout`.
    pub body_read_timeout: Option<Duration>,
    /// Rust test seam (#2047): called once per connection the accept loop has
    /// taken out of the kernel backlog, before the handler task starts.
    /// `stop()` breaks the accept loop and drops the listener, and a socket
    /// still in the backlog at that moment is reset by the kernel — so a test
    /// that wants to observe the *body-read deadline* on its own connection
    /// has to know the connection was accepted first. Production leaves this
    /// `None`: no callback, no behavior change (the hook never touches the
    /// accept/stop paths, it only reports that `accept()` returned).
    pub accepted: Option<Arc<dyn Fn() + Send + Sync>>,
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
    body_read_timeout: Duration,
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
            body_read_timeout: options
                .body_read_timeout
                .unwrap_or(DEFAULT_BODY_READ_TIMEOUT),
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
        let config_key = config.and_then(|config| config.get("apiKey").cloned());
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
        // `options.apiKey ?? process.env.API_SERVER_KEY ?? ""`: only nullish
        // values fold — a present `false`/`0`/`NaN` is *kept* by `??` and then
        // fails checkAuth's `!this.apiKey` falsy gate, exactly like a `""` from
        // the last fallback.
        let api_key = self
            .0
            .options
            .api_key
            .clone()
            .filter(|value| !nullish(value))
            .or_else(|| config_key.filter(|value| !nullish(value)))
            .or_else(|| {
                self.0
                    .env
                    .get("API_SERVER_KEY")
                    .map(|value| JsValue::from(value.as_str()))
            })
            .map(|value| ApiKey::of(&value))
            .unwrap_or(ApiKey::Off);
        Resolved {
            host: self
                .0
                .options
                .host
                .clone()
                .or_else(|| self.0.env.get("API_SERVER_HOST").cloned())
                .unwrap_or_else(|| DEFAULT_HOST.to_owned()),
            // TS `Number(options.port ?? env.API_SERVER_PORT ?? DEFAULT_PORT)`:
            // the port is never absent, so an unconfigured adapter still binds
            // 8642. Without the last fallback the channel silently never
            // listens (and logs nothing), which no TS configuration produces.
            port: port
                .map(|value| server::js_number(&value))
                .unwrap_or(DEFAULT_PORT),
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
    /// Always present: TS falls back to `DEFAULT_PORT` when nothing configures one.
    pub port: f64,
    pub api_key: ApiKey,
    pub model_name: String,
    pub cors_origins: Vec<String>,
}

/// The TS constructor's `this.apiKey` after
/// `options.apiKey ?? process.env.API_SERVER_KEY ?? ""`, classified the way
/// `checkAuth` consumes it (ApiServerChannel.ts:52, 110-125): a falsy value
/// leaves the gate open; a truthy string is the UTF-8 bytes `Buffer.from`
/// holds; a truthy array is its per-element `ToUint8` bytes
/// (`Buffer.from([256, 257])` is `[0, 1]`, Node v22.22.0 probes in
/// `_tmp/20261026-issue-p2batch-f/`); any other truthy value makes
/// `Buffer.from(this.apiKey)` throw inside checkAuth's try, the catch
/// swallows it and every request answers 401 — never the pre-#2428 fold of
/// a non-string into the env/"" fallback that silently disabled auth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ApiKey {
    /// `!this.apiKey`: checkAuth answers ok without reading the request.
    Off,
    /// A truthy string or array: compared as bytes, length first, timing-safe.
    Key(Vec<u8>),
    /// A truthy value `Buffer.from` refuses (number, `true`, object): always 401.
    Never,
}

impl ApiKey {
    fn of(value: &JsValue) -> Self {
        match value {
            JsValue::Undefined | JsValue::Null | JsValue::Bool(false) => Self::Off,
            JsValue::Number(number) if *number == 0.0 || number.is_nan() => Self::Off,
            JsValue::String(text) if text.is_empty() => Self::Off,
            JsValue::String(text) => Self::Key(js_string_value(text).into_bytes()),
            JsValue::Array(items) => Self::Key(
                items
                    .iter()
                    .map(|item| to_uint8(server::js_number(item)))
                    .collect(),
            ),
            _ => Self::Never,
        }
    }

    /// checkAuth's `if (!this.apiKey) return { ok: true }` gate.
    fn is_off(&self) -> bool {
        matches!(self, Self::Off)
    }
}

/// `Buffer.from(array)`'s per-element conversion (Node v22.22.0 probes:
/// `Buffer.from([256, 257])` = `[0, 1]`, `[-1]` = `[255]`, `[1.7]` = `[1]`,
/// `["65", "a"]` = `[65, 0]`, `[{}]` = `[0]`): `ToNumber`, then ToUint8 —
/// NaN/±Infinity are 0, fractions truncate toward zero, negatives wrap
/// modulo 256.
fn to_uint8(number: f64) -> u8 {
    if !number.is_finite() {
        return 0;
    }
    number.trunc().rem_euclid(256.0) as u8
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
            .map(|item| {
                // `value.map(String)`: the shared joiner keeps a tagged
                // object's class (`[object Set]`), joins a nested array and
                // formats numbers like `String()` (`-0` is `"0"`), where this
                // arm used to be a hand-rolled copy that printed
                // `[object Object]` for every object and nested array (Node
                // v22.22.0 probe:
                // `_tmp/20261112-issue-1833/impl-r1/node-probe/c2-parse-cors-probe.out`).
                // `corsOrigins` is only constructed by tests today, so the
                // surface is pinned by unit tests. `String(item)` can also
                // throw (a too-deep join, an own `toString`), which the TS
                // constructor propagates; resolve is infallible, so those
                // entries drop out instead — the checked joiner drops an own
                // `toString` where the unchecked `display` used to keep it as
                // `[object Object]` (#1833 fix-r2).
                crate::js_value::to_string_units_checked(item)
                    .map(|units| String::from_utf16_lossy(&units))
                    .unwrap_or_default()
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
            let port = resolved.port;
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
                port,
                api_key: resolved.api_key,
                model_name: resolved.model_name,
                cors_origins: resolved.cors_origins,
                gateway: deps.turn_gateway,
                uuid: inner.uuid.clone(),
                mapper: inner.mapper.clone(),
                active: inner.active.clone(),
                timeout: inner.timeout,
                body_read_timeout: inner.body_read_timeout,
                accepted: inner.options.accepted.clone(),
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

/// Loader mount: builds the adapter from the `apiServer` loader option object
/// (`build_options` already forwarded `extra.port` / `apiKey`). Absent values
/// stay present-but-undefined like the TS literal's, and `resolve`'s nullish
/// filters fold them back into the env / `DEFAULT_PORT` chain — the TS
/// `Number(options.port ?? process.env.API_SERVER_PORT ?? DEFAULT_PORT)` /
/// `options.apiKey ?? process.env.API_SERVER_KEY ?? ""` order. `apiKey`
/// forwards raw (#2428): the TS literal is `apiKey: cfg.apiKey`, and folding a
/// non-string at either seam re-ordered the `??` chain into a silent
/// auth-off fail-open — a truthy non-string now classifies in `ApiKey`
/// exactly where TS's `Buffer.from(this.apiKey)` throws.
/// #2301 fix-r1: the TS constructor's `Number(options.port ?? …)` throws on a
/// failed toPrimitive, the loader's catch records the key and continues, and
/// the channel never enters the startup array — the Rust channel defers the
/// port conversion to `start`, so the mount applies the same check at
/// construction and surfaces the same TypeError through the same loader catch
/// (fix-r2: the shared coercion walk also covers arrays and the prototype
/// chain the config merge installs, matching `Number()`'s join behavior).
pub fn register(mounts: &mut super::loader::ChannelLoaderMounts) {
    mounts.register(
        "apiServer",
        Arc::new(|options: &JsObject| {
            if let Some(port) = options.get("port").filter(|value| !nullish(value))
                && let Some(error) = crate::js_value::number_conversion_error(port)
            {
                return Err(ChannelError::Plain(error));
            }
            Ok(Arc::new(ApiServerChannel::new(ApiServerOptions {
                port: options.get("port").cloned(),
                api_key: options.get("apiKey").cloned(),
                ..ApiServerOptions::default()
            })) as Arc<dyn ChannelAdapter>)
        }),
    );
}

/// An option absent vs nullish: only a present, non-nullish value reaches the
/// TS `??` chain's left side (the nullish arm falls back to the env / default
/// chain, which never throws and never disables auth on its own).
fn nullish(value: &JsValue) -> bool {
    matches!(value, JsValue::Undefined | JsValue::Null)
}

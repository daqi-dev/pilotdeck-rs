//! Parsed WebSocket proxy. TS owns authentication and methods outside the verified local whitelist.
mod acceptor;
pub mod channel_runtime;
pub mod application;
pub mod config;
pub mod cron;
pub mod http;
mod routes;
mod dispatch;
mod fetch_redirect;
mod frame;
pub mod hooks;
pub mod host;
pub mod http_proxy;
mod js_value;
mod metadata;
mod model_catalog;
pub mod model_cost;
mod module_host;
#[doc(hidden)]
pub mod mutants;
mod native_archive;
mod native_http;
pub mod native_process;
pub mod native_ws;
#[cfg(test)]
extern crate self as pilotdeck_gateway;
#[cfg(all(test, feature = "pdrs-mutants"))]
#[path = "../tests/edgeclaw_service_facade.rs"]
// The same integration driver includes its own common helpers in this test-only module.
#[allow(clippy::duplicate_mod)]
mod mutant_facade;
mod node_io;
mod node_locale;
mod pairing;
mod process_group;
#[cfg(all(test, unix))]
mod process_spawn;
mod project_files;
pub mod runtime;
mod session_model;
pub mod skills;
pub mod storage;
#[cfg(test)]
#[path = "../tests/common/baseline.rs"]
mod test_baseline;
// fix-r3 P1-1: the dream-tail race needs the `cfg(test)`-only
// `dream_tail_gate` seam, which an integration test cannot see. Compiled as a
// library test module instead of its own test target.
#[cfg(test)]
#[path = "../tests/common/dream_tail_race.rs"]
mod dream_tail_race;
#[cfg(test)]
#[path = "../tests/common/exact_receipt.rs"]
mod test_exact_receipt;
#[cfg(test)]
#[path = "../tests/common/native_ws_sample.rs"]
mod test_native_ws_sample;
#[cfg(test)]
#[path = "../tests/common/negative_receipt.rs"]
mod test_negative_receipt;
#[cfg(test)]
#[path = "../tests/common/node_provenance.rs"]
mod test_node_provenance;
#[cfg(test)]
#[path = "../tests/common/oracle_locale.rs"]
mod test_oracle_locale;
#[cfg(test)]
mod test_process;
#[cfg(test)]
#[path = "../tests/common/scratch.rs"]
mod test_scratch;
// fix-r3 review P2: `write_stage` hardcodes `/bin/sh` helpers and calls
// `rustix::process`, so it is unix-only like `test_scratch` above.
#[cfg(all(test, unix))]
#[path = "../tests/common/write_stage.rs"]
mod test_write_stage;
pub mod upload;
mod upstream;
pub mod user_dialog;
pub mod web_cli;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use hyper::body::Incoming;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use std::error::Error;

pub type ProxyError = Box<dyn Error + Send + Sync>;
pub type Body = BoxBody<Bytes, ProxyError>;
pub type HttpClient = Client<HttpConnector, Incoming>;

/// Node/libuv `Error.message` for a one-path syscall
/// (`code: description, syscall 'path'`), when the generated table knows the
/// errno. Descriptor reads and writes carry no path in Node (see
/// `node_io::read_file`: reading a directory is `EISDIR: illegal operation on
/// a directory, read`; node v22.22.0 under `ulimit -f` gives
/// `EFBIG: file too large, write` with `syscall: 'write'` and no path,
/// `_tmp/20261025-issue-621-takeover/xfsz-probe/`), while an
/// open/stat/mkdir/... error names its path — even an empty one (`fs.openSync('')`
/// on node v22.22.0 reports `ENOENT: no such file or directory, open ''`,
/// `_tmp/20261025-issue-656/fix-r2/empty-path-probe.out`). The CLI and the
/// MCP runtime share this formatter and the libuv table instead of keeping
/// their own copies.
pub fn node_fs_message(
    error: &std::io::Error,
    syscall: &str,
    path: Option<&str>,
) -> Option<String> {
    node_io::diagnostic(error, syscall, staged_path(syscall, path)).map(|(_, message)| message)
}

/// [`node_fs_message`] with libuv's `UV_UNKNOWN` fallback for errnos outside
/// the generated table: `UNKNOWN: unknown error` under the same stage and
/// path rules, formatted by the same `node_io` code — a descriptor read/write
/// error carries no path there either, and a staged error keeps its own
/// stage.
pub fn node_fs_message_or_unknown(
    error: &std::io::Error,
    syscall: &str,
    path: Option<&str>,
) -> String {
    node_io::diagnostic_or_unknown(error, syscall, staged_path(syscall, path)).1
}

/// Descriptor reads and writes carry no path in Node, whatever label the
/// caller uses; `node_io`'s stage override then restores a staged error's own
/// labels, so a `descriptor_error` stays pathless.
fn staged_path<'a>(syscall: &str, path: Option<&'a str>) -> Option<&'a str> {
    if matches!(syscall, "read" | "write") {
        None
    } else {
        path
    }
}

/// Read the original session bytes without transcript validation or sorting.
/// Callers retain physical line numbers and apply their own projection.
pub fn read_session_bytes(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use metadata::Files;
    metadata::NativeFiles.read(path)
}

/// The native Node-compatible directory listing used by session storage.
pub fn list_session_entries(path: &std::path::Path) -> std::io::Result<Vec<(String, bool)>> {
    use metadata::Files;
    metadata::NativeFiles.entries(path)
}

/// Resolve a chat directory through the existing project marker lookup.
/// `cwd` is the caller's explicit base (native assembly supplies the absolute
/// configured project); this API does not look up process cwd.
pub fn session_chat_dir(
    home: &std::path::Path,
    cwd: &std::path::Path,
    project: &str,
) -> std::path::PathBuf {
    metadata::Metadata {
        home: home.into(),
        cwd: cwd.into(),
        files: metadata::NativeFiles,
    }
    .chat_dir(project)
}

/// Native HTTP and authenticated WS entry point.
pub use routes::GatewayServices;

pub struct LocalServer {
    services: std::sync::Arc<GatewayServices>,
    proxy: std::sync::Arc<http_proxy::Proxy>,
    config_state: Option<config::routes::native::SharedConfigRoutes>,
    mcp: Option<std::sync::Arc<runtime::mcp::routes::McpRoutes>>,
    interactions: std::sync::Arc<runtime::interaction::GatewayInteractionCoordinator>,
    dialogs: std::sync::Arc<user_dialog::GatewayUserDialogs>,
    pub(crate) http: http::HttpState,
    automation: Option<std::sync::Arc<cron::Automation>>,
    allow_upstream_fallback: bool,
    pub(crate) memory_scheduler: Option<std::sync::Arc<runtime::memory::WebMemoryScheduler>>,
}
impl Drop for LocalServer {
    fn drop(&mut self) {
        if let Some(scheduler) = &self.memory_scheduler {
            scheduler.close_memory_services(|| ());
        }
        if let Some(state) = self.config_state.take() {
            config::routes::native::request_shutdown(state);
        }
    }
}

impl LocalServer {
    pub fn new(proxy: std::sync::Arc<http_proxy::Proxy>) -> Result<Self, ProxyError> {
        let token = Self::token_from_env()?;
        let mcp = metadata::Metadata::from_env().map(|metadata| {
            std::sync::Arc::new(runtime::mcp::routes::McpRoutes::native(
                metadata.home,
                metadata.cwd,
            ))
        });
        let mut server = Self::with_token(proxy, token)?;
        server.mcp = mcp;
        Ok(server)
    }

    pub fn from_env() -> Result<Self, ProxyError> {
        use hyper_util::{client::legacy::Client, rt::TokioExecutor};
        Self::new(std::sync::Arc::new(http_proxy::Proxy {
            upstream: "http://127.0.0.1:1".into(),
            client: Client::builder(TokioExecutor::new()).build_http(),
        }))
    }

    pub(crate) async fn run_websocket<S>(
        &self,
        stream: S,
        fallback: Option<runtime::session::routes::passthrough::Upgrade>,
    ) -> std::io::Result<()>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let metadata = self
            .config_state
            .as_ref()
            .map(|state| {
                let state = state.lock().unwrap();
                metadata::Metadata {
                    home: state.home.clone(),
                    cwd: state.cwd.clone(),
                    files: metadata::NativeFiles,
                }
            })
            .or_else(|| {
                self.http.home.as_ref().map(|home| metadata::Metadata {
                    home: home.clone(),
                    cwd: self.http.cwd.clone(),
                    files: metadata::NativeFiles,
                })
            })
            .or_else(metadata::Metadata::from_env);
        if let Some(gateway) = self.http.ws_gateway.clone() {
            pairing::run_local_with_gateway(stream, self.http.token.clone(), gateway).await
        } else {
            pairing::run_local_with_state(
                stream,
                self.http.token.clone(),
                pairing::LocalState {
                    metadata,
                    interactions: self.interactions.clone(),
                    dialogs: self.dialogs.clone(),
                    mcp: self.mcp.clone(),
                    services: self.services.clone(),
                    automation: self.automation.clone(),
                    fallback,
                },
            )
            .await
        }
    }

    /// Reuse G0 token resolution for the native runtime endpoint.
    pub fn token_from_env() -> Result<String, ProxyError> {
        let home = std::env::var_os("PILOT_HOME")
            .filter(|p| !p.is_empty())
            .map(std::path::PathBuf::from);
        let token = ["PILOTDECK_GATEWAY_TOKEN", "PILOTDECK_TOKEN"]
            .iter()
            .find_map(|key| std::env::var(key).ok().filter(|s| !s.trim().is_empty()))
            .map(Ok)
            .unwrap_or_else(|| {
                let home = home
                    .as_ref()
                    .ok_or("local gateway requires an explicit token or isolated PILOT_HOME")?;
                let bytes = std::fs::read_to_string(home.join("server-token"))?;
                let token = metadata::js_trim(&bytes).to_owned();
                if token.is_empty() {
                    return Err("gateway token file is empty".into());
                }
                Ok::<_, ProxyError>(token)
            })?;
        Ok(token)
    }
    pub fn with_token(
        proxy: std::sync::Arc<http_proxy::Proxy>,
        token: String,
    ) -> Result<Self, ProxyError> {
        if metadata::js_trim(&token).is_empty() {
            return Err("gateway token is empty".into());
        }
        Ok(Self {
            proxy,
            interactions: Default::default(),
            dialogs: Default::default(),
            services: std::sync::Arc::new(GatewayServices::from_env()),
            http: http::HttpState::new(
                token.clone(),
                std::env::var_os("PILOT_HOME")
                    .filter(|p| !p.is_empty())
                    .map(std::path::PathBuf::from),
                std::env::current_dir().unwrap_or_default(),
            ),
            automation: None,
            allow_upstream_fallback: true,
            config_state: None,
            mcp: None,
            memory_scheduler: None,
        })
    }

    /// Production listeners return native unknown-method errors. Legacy
    /// embedders may explicitly retain a migration proxy via with_token.
    pub fn without_upstream_fallback(mut self) -> Self {
        self.allow_upstream_fallback = false;
        self
    }

    /// Embed a gateway using the host's real admission and hook owners.
    pub fn with_runtime(
        proxy: std::sync::Arc<http_proxy::Proxy>,
        token: String,
        services: std::sync::Arc<GatewayServices>,
    ) -> Result<Self, ProxyError> {
        let mut server = Self::with_token(proxy, token)?;
        server.services = services;
        Ok(server)
    }

    /// Assemble CONFIG with the host's existing turn owner and channel lifecycle.
    /// Retain `config_state()` for turn-start snapshot/SDK override consumption.
    pub fn with_config_host(
        mut self,
        mut routes: config::routes::ConfigRoutes,
        host: config::routes::ConfigHost,
    ) -> Self {
        routes.bind_host(host);
        if let Some(scheduler) = &self.memory_scheduler {
            routes.memory_scheduler = Some(scheduler.clone());
        }
        let state = config::routes::native::install(routes);
        self.http.config = Some(state.clone());
        self.config_state = Some(state);
        self
    }
    /// Retain the bootstrap outside the listener for session admission/shutdown.
    pub fn with_native_config(
        mut self,
        bootstrap: &config::routes::bootstrap::ConfigBootstrap,
    ) -> Self {
        {
            let mut config = bootstrap.state.lock().unwrap();
            if let Some(automation) = &self.automation {
                config.session_owner = Some(automation.owner.clone());
                config.interactions = Some(automation.interactions.clone());
                config.shared_sdk = automation.sdk.clone();
            }
            if let Some(scheduler) = &self.memory_scheduler {
                config.memory_scheduler = Some(scheduler.clone());
            }
        }
        self.http.config = Some(bootstrap.state.clone());
        self.config_state = Some(bootstrap.state.clone());
        if let Some(home) = self.http.home.clone() {
            let cwd = self.http.cwd.clone();
            self = self.with_feishu_webhook(
                config::routes::channel_webhook::production_feishu_webhook(home.clone(), cwd.clone()),
            );
            self = self.with_wecom_webhook(
                config::routes::channel_webhook::production_wecom_webhook(home, cwd),
            );
        }
        self
    }
    pub fn config_state(&self) -> Option<config::routes::native::SharedConfigRoutes> {
        self.config_state.clone()
    }

    pub async fn shutdown(&self) -> Result<(), ProxyError> {
        if let Some(state) = self.config_state.clone() {
            config::routes::native::shutdown(state)
                .await
                .map_err(|e| std::io::Error::other(e.message))?;
        }
        Ok(())
    }

    /// Embed the gateway with an explicit token and shared MCP domain state.
    pub fn with_mcp(
        proxy: std::sync::Arc<http_proxy::Proxy>,
        token: String,
        mcp: std::sync::Arc<runtime::mcp::routes::McpRoutes>,
    ) -> Self {
        Self {
            proxy,
            mcp: Some(mcp),
            interactions: Default::default(),
            dialogs: Default::default(),
            services: std::sync::Arc::new(GatewayServices::from_env()),
            http: http::HttpState::new(
                token.clone(),
                std::env::var_os("PILOT_HOME")
                    .filter(|p| !p.is_empty())
                    .map(std::path::PathBuf::from),
                std::env::current_dir().unwrap_or_default(),
            ),
            automation: None,
            config_state: None,
            allow_upstream_fallback: true,
            memory_scheduler: None,
        }
    }

    /// Share the host's pending requests and session grants with every socket.
    /// Supply the same coordinator used by `GatewayServices::interactions`.
    pub fn with_interactions(
        mut self,
        interactions: std::sync::Arc<runtime::interaction::GatewayInteractionCoordinator>,
    ) -> Self {
        self.interactions = interactions;
        self
    }

    /// Share the channel's bus and persistent host with every socket.
    pub fn user_dialogs(&self) -> std::sync::Arc<user_dialog::GatewayUserDialogs> {
        self.dialogs.clone()
    }

    /// Share the channel's bus and persistent host with every socket.
    pub fn with_user_dialogs(
        mut self,
        dialogs: std::sync::Arc<user_dialog::GatewayUserDialogs>,
    ) -> Self {
        self.dialogs = dialogs;
        self
    }

    pub fn with_gateway(
        mut self,
        gateway: std::sync::Arc<dyn runtime::connection::ConnectionGateway>,
    ) -> Self {
        self.http.ws_gateway = Some(gateway);
        self
    }

    pub fn with_module_host(mut self, bindings: http::module_host::Bindings) -> Self {
        self.http.module_host = bindings;
        self
    }

    pub fn with_static_assets(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.http.static_root = Some(root.into());
        self
    }

    pub fn with_feishu_webhook(mut self, callback: http::FeishuWebhook) -> Self {
        self.http.feishu_webhook = Some(callback);
        self
    }

    pub fn with_wecom_webhook(mut self, callback: http::WecomWebhook) -> Self {
        self.http.wecom_webhook = Some(callback);
        self
    }

    pub fn authenticated(
        proxy: std::sync::Arc<http_proxy::Proxy>,
        token: String,
    ) -> Result<Self, ProxyError> {
        Self::with_token(proxy, token)
    }
    pub fn with_automation(mut self, automation: std::sync::Arc<cron::Automation>) -> Self {
        self.interactions = automation.interactions.clone();
        let mut services = GatewayServices::new(
            self.services.home.clone(),
            self.services.cwd.clone(),
            self.services.admission.clone(),
            automation.async_hooks.clone(),
        );
        services.browsers = self.services.browsers.clone();
        self.services = std::sync::Arc::new(services);
        self.automation = Some(automation);
        self
    }
    pub fn with_home(mut self, home: std::path::PathBuf, cwd: std::path::PathBuf) -> Self {
        self.http.home = Some(home.clone());
        self.http.cwd = cwd.clone();
        let mut services = GatewayServices::new(
            home.clone(),
            cwd.clone(),
            self.services.admission.clone(),
            self.services.async_hooks.clone(),
        );
        services.browsers = self.services.browsers.clone();
        self.services = std::sync::Arc::new(services);
        self.mcp = Some(std::sync::Arc::new(
            runtime::mcp::routes::McpRoutes::native(home.clone(), cwd),
        ));
        self.memory_scheduler = Some(std::sync::Arc::new(
            runtime::memory::WebMemoryScheduler::with_workspace_root(home.join("workspaces")),
        ));
        self
    }
    pub fn memory_scheduler(&self) -> Option<std::sync::Arc<runtime::memory::WebMemoryScheduler>> {
        self.memory_scheduler.clone()
    }
    pub fn resolve_memory_config_flags(
        &self,
    ) -> (
        bool,
        bool,
        Option<std::sync::Arc<dyn runtime::memory::edgeclaw::NativeEdgeClawLlm>>,
    ) {
        let snapshot = if let Some(state) = &self.config_state {
            let routes = state.lock().unwrap();
            Some(routes.store.snapshot())
        } else if let Some(home) = &self.http.home {
            crate::config::load_snapshot(
                home,
                &self.http.cwd,
                &std::env::vars().collect(),
                None,
                1.0,
                "",
            )
            .ok()
            .map(std::sync::Arc::new)
        } else {
            None
        };

        let enabled = snapshot
            .as_deref()
            .and_then(|s| s.get("config"))
            .and_then(|c| c.get("memory"))
            .and_then(|m| m.get("enabled"))
            .and_then(pilotdeck_protocol::JsValue::as_bool)
            .unwrap_or(true);
        let llm = snapshot
            .as_deref()
            .and_then(runtime::memory::edgeclaw::native_llm_from_snapshot);
        let has_model = llm.is_some();
        (enabled, has_model, llm)
    }

    /// Starts background memory scheduler for auto index/dream (aligns with `ui/server/index.js:3712`).
    pub fn start_memory_scheduler(&self) {
        if let Some(scheduler) = &self.memory_scheduler {
            let (enabled, has_model, llm) = self.resolve_memory_config_flags();
            if let Some(llm) = llm {
                scheduler.set_llm(llm);
            }
            scheduler.start_memory_scheduler(None, enabled, has_model);
        }
    }

    /// Closes all active web memory services on graceful shutdown (aligns with `ui/server/index.js:3740-3741`).
    pub fn close_memory_services(&self) {
        if let Some(scheduler) = &self.memory_scheduler {
            scheduler.close_memory_services(|| ());
        }
    }

    /// Reloads web memory scheduler on external config reload (aligns with `pilotdeckConfigReloader.js:17-20`).
    pub fn on_config_reload(&self, enabled: bool, has_model: bool) {
        if let Some(scheduler) = &self.memory_scheduler {
            let (_, _, llm) = self.resolve_memory_config_flags();
            if let Some(llm) = llm {
                scheduler.set_llm(llm);
            }
            scheduler.on_config_reload(|| (), None, enabled, has_model);
        }
    }
    pub async fn stop(&self) -> Result<(), ProxyError> {
        self.close_memory_services();
        if let Some(automation) = &self.automation {
            automation
                .stop()
                .await
                .map_err(|e| std::io::Error::other(format!("{}: {}", e.code, e.message)))?;
        }
        self.shutdown().await
    }
    pub async fn serve_connection<S>(
        self: std::sync::Arc<Self>,
        stream: S,
    ) -> Result<(), hyper::Error>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        http_proxy::serve_local_connection(self, stream).await
    }
}

/// Register this process's SIGXFSZ handler, aligning it with Node (#621).
/// Node's platform init ignores SIGXFSZ process-wide, so a write past
/// RLIMIT_FSIZE surfaces as an `EFBIG` error from the syscall; Rust std
/// ignores only SIGPIPE, so the same write instead kills the process
/// (coordinator probe, node v22.22.0 vs rustc 1.99.0 + tokio 1.48.0,
/// `ulimit -f 1` writing 4 KiB: unregistered the writing process dies with
/// exit 153; registered through tokio's safe signal API it observes
/// os error 27 and continues — evidence
/// `_tmp/20261025-issue-621-takeover/xfsz-tokio-probe/`). The gateway
/// binary calls this at startup, and the debug request dump registers
/// before its write (`runtime/model/http.rs`), so every host process of the
/// model runtime is covered — that runtime has no process entry of its own.
/// Exactly once per process (`OnceLock`); the returned `Signal` must stay
/// held so the disposition stays installed, so it lives in the lock.
/// Registration requires a tokio runtime with the signal driver enabled —
/// every runtime this crate builds in production is `enable_all` (the
/// gateway binary's `#[tokio::main]` included). Without a runtime on the
/// calling thread it degrades to a stderr note and the default disposition,
/// no worse than without this guard; inside a runtime that lacks the signal
/// driver, tokio's `signal()` panics instead of returning `Err`, so the
/// `Handle::try_current` guard in `register_sigxfsz` only catches the
/// no-runtime case.
#[cfg(unix)]
pub fn ensure_sigxfsz_ignored() {
    static SIGNAL: std::sync::OnceLock<Option<tokio::signal::unix::Signal>> =
        std::sync::OnceLock::new();
    SIGNAL.get_or_init(|| match register_sigxfsz() {
        Ok(signal) => Some(signal),
        // A failed attempt is recorded as `None`, so the registration
        // and this note both happen exactly once per process.
        Err(reason) => {
            eprintln!(
                "[model-debug] SIGXFSZ handler not registered ({reason}); \
                 writes past RLIMIT_FSIZE will terminate the process"
            );
            None
        }
    });
}

/// The registration attempt itself: safe tokio signal API, no unsafe. The
/// runtime check first keeps the library callable from plain (non-async)
/// threads, where tokio's `signal()` would panic instead of answering.
#[cfg(unix)]
fn register_sigxfsz() -> Result<tokio::signal::unix::Signal, String> {
    if tokio::runtime::Handle::try_current().is_err() {
        return Err("no tokio runtime on this thread".into());
    }
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::from_raw(libc::SIGXFSZ))
        .map_err(|error| format!("tokio signal registration failed: {error}"))
}

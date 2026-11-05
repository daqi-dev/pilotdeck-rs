//! A8.3 channel assembly for the native process (#1328): the weixin subset of
//! `cli/ChannelAdapterBundle.ts` (the `adapters.weixin.enabled` gate, durable
//! mapper state, `hotStartWeixin`, `reload`, `flush`) on the #1327
//! `ChannelServer` lifecycle host, plus the CLI `prepareWeixinLogin` callback
//! (`pilotdeck.ts:240-247`). The other bundle channels (feishu/qq/wecom and
//! the loader-table ones) land with their own A8.3-ch-* units; the native
//! process reconciles only the weixin key today, so `managedChannelKeys`
//! carries exactly that one key.
use crate::channel::{
    protocol::{
        ChannelAdapter, ChannelError, ChannelKey, ChannelLog, ChannelStartDeps, ChannelState,
        ChannelStatus, CloseHost, ReconcileInput, ReconcileResult, StatusReporter,
    },
    runtime_status::{
        ChannelRuntimeStatusReporter, ChannelRuntimeStatusUpdate,
        create_channel_runtime_status_reporter,
    },
    server::ChannelServer,
    weixin::{
        ChannelStatePersistence, WeixinChannel, WeixinChannelOptions, WeixinChannelStatus,
        WeixinGatewayPort, WeixinSessionMapper, WeixinStateChange,
    },
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use super::methods::PrepareWeixinLogin;

/// The channel key this assembly manages (TS `managedChannelKeys` weixin
/// entry; the sibling keys land with their own units).
fn weixin_key() -> ChannelKey {
    ChannelKey::new("weixin")
}

/// A satisfied close host: the native embedding drains its WS server before
/// the host cleanup closes the channel server, so there is no gateway-WS
/// close left to order behind the channel stops. The real WS shutdown
/// ordering is the #1328 pr2 follow-up noted in the plan.
pub fn noop_close_host() -> CloseHost {
    Arc::new(|| Box::pin(async { Ok(()) }))
}

/// TS `ChannelAdapterBundleOptions.createWeixin` — the constructor seam the
/// bundle tests use to swap in a controlled channel.
pub type WeixinChannelMaker =
    Arc<dyn Fn(WeixinChannelOptions) -> Arc<dyn ChannelAdapter> + Send + Sync>;
fn default_weixin_maker() -> WeixinChannelMaker {
    Arc::new(|options| Arc::new(WeixinChannel::new(options)))
}

pub struct NativeChannelsOptions {
    /// Pilot home: state dir `resolve(pilotHome, "channels")` holds the mapper
    /// state files and the runtime-status sink.
    pub pilot_home: PathBuf,
    /// The loaded config snapshot's `config` object (`baseDeps.config`,
    /// pilotdeckServer.ts:78). Drives the `adapters.weixin.enabled` gate.
    pub config: Arc<JsValue>,
    /// The weixin construction seams. Production passes the defaults (real
    /// auth host); tests point the QR login and ilink client at a loopback
    /// mock. `mapper`/`on_state_change` are replaced per construction, and
    /// `runtime_status` is wired to the shared status sink.
    pub weixin: WeixinChannelOptions,
    /// TS `createWeixin` seam: builds the adapter from its options. Tests
    /// swap in a controlled channel to observe the lifecycle.
    pub weixin_maker: Option<WeixinChannelMaker>,
    /// Test seam over the console channel log.
    pub log: Option<ChannelLog>,
    /// Test seam over the runtime-status sink (default: the
    /// `ChannelRuntimeStatus` file writer).
    pub report_status: Option<ChannelRuntimeStatusReporter>,
    /// The ChannelServer's close host (default: the no-op embedding host).
    pub close_host: Option<CloseHost>,
}

pub struct NativeChannels {
    server: ChannelServer,
    state: ChannelStatePersistence,
    weixin: WeixinChannelOptions,
    weixin_maker: WeixinChannelMaker,
    gateway: OnceLock<Arc<dyn WeixinGatewayPort>>,
    config: Arc<JsValue>,
}

impl NativeChannels {
    pub fn new(options: NativeChannelsOptions) -> Self {
        let runtime = options
            .report_status
            .unwrap_or_else(|| create_channel_runtime_status_reporter(options.pilot_home.clone()));
        // pilotdeckServer.ts:72-83: one status sink serves both the lifecycle
        // reporter and the adapter's own login-state reports.
        let lifecycle_runtime = runtime.clone();
        let report_status: StatusReporter = Arc::new(move |status| {
            lifecycle_runtime(status.key.as_js(), &lifecycle_update(&status));
        });
        let weixin_runtime = runtime.clone();
        let mut weixin = options.weixin;
        weixin.runtime_status = Some(Arc::new(move |status| {
            weixin_runtime(&weixin_key().as_js(), &weixin_update(&status));
        }));
        let state = ChannelStatePersistence::new(options.pilot_home.join("channels"));
        Self {
            server: ChannelServer::new(
                ChannelStartDeps {
                    config: Some(options.config.clone()),
                    report_status: Some(report_status),
                    log: options.log,
                },
                options.close_host.unwrap_or_else(noop_close_host),
            ),
            state,
            weixin,
            weixin_maker: options.weixin_maker.unwrap_or_else(default_weixin_maker),
            gateway: OnceLock::new(),
            config: options.config,
        }
    }

    pub fn server(&self) -> &ChannelServer {
        &self.server
    }

    /// Publish the gateway port the channel submits through. The assembly
    /// calls this once the native submitter exists; every weixin channel
    /// constructed afterwards (startup or hot start) shares it, like the TS
    /// `baseDeps.gateway` each `start()` receives.
    pub fn set_gateway_port(&self, port: Arc<dyn WeixinGatewayPort>) -> bool {
        self.gateway.set(port).is_ok()
    }

    /// ChannelAdapterBundle.makeWeixin: mapper from the persisted state (or a
    /// fresh one), `onStateChange` persisting back through the debounce.
    async fn make_weixin(&self) -> Arc<dyn ChannelAdapter> {
        let saved = self.state.load(&weixin_key().as_js()).await;
        let mapper = match saved {
            Some(saved) => Arc::new(WeixinSessionMapper::from_saved(&saved)),
            None => Arc::new(WeixinSessionMapper::new()),
        };
        let state = self.state.clone();
        let on_state_change: WeixinStateChange =
            Arc::new(move |value| state.save(&weixin_key().as_js(), value.clone()));
        let mut options = self.weixin.clone();
        options.mapper = mapper;
        options.on_state_change = Some(on_state_change);
        options.gateway = self.gateway.get().cloned();
        (self.weixin_maker)(options)
    }

    /// ChannelAdapterBundle.createWeixin: `adapters.weixin.enabled === true`
    /// gates the startup adapter (ChannelAdapterBundle.ts:145-152).
    pub async fn create_startup_weixin(&self) -> Option<Arc<dyn ChannelAdapter>> {
        if weixin_enabled(&self.config) {
            Some(self.make_weixin().await)
        } else {
            None
        }
    }

    /// ChannelAdapterBundle.hotStartWeixin: state.load → makeWeixin →
    /// hotStartChannel (the channel server replaces any previous instance).
    pub async fn hot_start_weixin(&self) -> Result<(), ChannelError> {
        let channel = self.make_weixin().await;
        self.server.hot_start_channel(channel).await
    }

    /// ChannelAdapterBundle.reload: reconcile the desired set against the
    /// managed keys (an enabled weixin replaces, a disabled one stops with
    /// `config-disabled`). No caller in the native process yet — the config
    /// hot-reload event source lands with the pr2 channel cluster — so the
    /// reconcile semantics are pinned by the assembly tests for now.
    #[allow(dead_code)]
    pub async fn reload(&self, config: &JsValue) -> Result<ReconcileResult, ChannelError> {
        let mut input = ReconcileInput {
            managed_channel_keys: vec![weixin_key()],
            channels: Vec::new(),
        };
        if weixin_enabled(config) {
            input.channels.push(self.make_weixin().await);
        }
        self.server.reconcile_channels(input).await
    }

    /// ChannelAdapterBundle.flush.
    pub async fn flush(&self) -> Result<(), String> {
        self.state.flush().await
    }

    /// pilotdeckServer.closeServer's channel half (stop every running channel
    /// through the lifecycle core, then run the close host).
    pub async fn close(&self) -> Result<(), ChannelError> {
        self.server.close().await
    }

    /// The CLI `prepareWeixinLogin` callback (pilotdeck.ts:240-247):
    /// `requestedAt` is captured before the hot start and returned with
    /// `requested: true` after it. The `!serverRef` unsupported branch is
    /// unreachable for the native process — the channel server is constructed
    /// with the gateway — so the no-callback shape stays the `Requests` field
    /// staying `None`.
    pub fn prepare_login_callback(self: &Arc<Self>) -> PrepareWeixinLogin {
        let channels = self.clone();
        Arc::new(move || {
            let channels = channels.clone();
            Box::pin(async move {
                let requested_at = now_iso();
                channels
                    .hot_start_weixin()
                    .await
                    .map_err(|error| {
                        crate::runtime::connection::RequestError::new(error.message())
                    })?;
                Ok(crate::metadata::object([
                    ("requested", JsValue::Bool(true)),
                    ("requestedAt", JsValue::String(requested_at)),
                ]))
            })
        })
    }
}

/// `config.adapters?.weixin?.enabled !== true` → disabled
/// (ChannelAdapterBundle.ts:146 — a strict `=== true` gate).
fn weixin_enabled(config: &JsValue) -> bool {
    config
        .get("adapters")
        .and_then(|adapters| adapters.get("weixin"))
        .and_then(|weixin| weixin.get("enabled"))
        == Some(&JsValue::Bool(true))
}

fn lifecycle_update(status: &ChannelStatus) -> ChannelRuntimeStatusUpdate {
    let state = match status.state {
        ChannelState::Starting => "starting",
        ChannelState::Connected => "connected",
        ChannelState::Stopped => "stopped",
        ChannelState::Failed => "failed",
    };
    ChannelRuntimeStatusUpdate {
        state: state.to_string(),
        message: Some(status.message.clone()),
        account_id: None,
        error: status.error.clone(),
        qr_url: None,
    }
}

fn weixin_update(status: &WeixinChannelStatus) -> ChannelRuntimeStatusUpdate {
    ChannelRuntimeStatusUpdate {
        state: status.state.as_str().to_string(),
        message: Some(status.message.clone()),
        account_id: status.account_id.clone(),
        error: status.error.clone(),
        qr_url: status.qr_url.clone(),
    }
}

fn now_iso() -> JsString {
    use pilotdeck_transport::{server::ExecutionClock, system_clock::SystemExecutionClock};
    SystemExecutionClock.now_iso()
}

//! A8.3 channel assembly for the native process (#1328): the weixin and
//! feishu subsets of `cli/ChannelAdapterBundle.ts` (the `adapters.<key>.enabled`
//! gate, durable mapper state, `hotStart<Key>`, `reload`, `flush`) on the
//! #1327 `ChannelServer` lifecycle host, plus the CLI `prepareWeixinLogin`
//! callback (`pilotdeck.ts:240-247`) and the `/feishu/webhook` slot the native
//! HTTP listener mounts. The other bundle channels (qq/wecom and the
//! loader-table ones) land with their own A8.3-ch-* units; the native process
//! reconciles only the keys it constructs, so `managedChannelKeys` carries
//! exactly those.
use crate::channel::{
    feishu::{FeishuChannel, FeishuOptions, FeishuWebhookSlot, no_interaction},
    protocol::{
        ChannelAdapter, ChannelError, ChannelKey, ChannelLog, ChannelStartDeps, ChannelState,
        ChannelStatus, CloseHost, ReconcileInput, ReconcileResult, StatusReporter, TurnGateway,
    },
    runtime_status::{
        ChannelRuntimeStatusReporter, ChannelRuntimeStatusUpdate,
        create_channel_runtime_status_reporter,
    },
    server::ChannelServer,
    turn_submitter::SubmitterTurnGateway,
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
use crate::runtime::turn::submit::TurnSubmitter;

/// The channel keys this assembly manages (TS `managedChannelKeys` subset).
fn weixin_key() -> ChannelKey {
    ChannelKey::new("weixin")
}
fn feishu_key() -> ChannelKey {
    ChannelKey::new("feishu")
}

/// The `ChannelAdapterBundle` keys this assembly actually constructs (#1694
/// §7). `crate::channel::loader::reject_unimplemented_bundle_channels` refuses
/// any *other* enabled bundle key instead of letting the native process start
/// with a channel that would never be built. An A8.3 bundle unit adds its key
/// here in the same change that adds its construction.
pub const IMPLEMENTED_BUNDLE_CHANNEL_KEYS: &[&str] = &["feishu", "weixin"];

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
    /// The feishu construction seams (app id/secret, encrypt key, verify
    /// token, connection mode, domain). Production passes the defaults; tests
    /// point `base_url` at a loopback Lark mock.
    pub feishu: FeishuOptions,
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
    feishu: FeishuOptions,
    /// `activeFeishu`: the `/feishu/webhook` slot. Published at construction
    /// by every feishu adapter this assembly builds.
    feishu_slot: Arc<FeishuWebhookSlot>,
    /// The A7 port each feishu adapter receives through `ChannelStartDeps`.
    /// The submitter only exists after the host builder ran, so the adapter
    /// gets this handle and waits for `set_turn_submitter`.
    feishu_gateway: Arc<FeishuTurnGateway>,
    config: Arc<JsValue>,
}

/// The `ChannelStartDeps.turn_gateway` the feishu adapter is constructed with.
/// It forwards to a [`SubmitterTurnGateway`] once the assembly publishes the
/// native submitter; until then `submit` fails with the same
/// "not started" shape as a missing gateway.
struct FeishuTurnGateway {
    submitter: OnceLock<Arc<TurnSubmitter>>,
    channel_key: ChannelKey,
}

impl FeishuTurnGateway {
    fn new() -> Self {
        Self {
            submitter: OnceLock::new(),
            channel_key: feishu_key(),
        }
    }

    fn set(&self, submitter: Arc<TurnSubmitter>) -> bool {
        self.submitter.set(submitter).is_ok()
    }

    fn inner(&self) -> Option<SubmitterTurnGateway> {
        self.submitter
            .get()
            .map(|submitter| SubmitterTurnGateway::new(submitter.clone(), self.channel_key.clone()))
    }
}

impl TurnGateway for FeishuTurnGateway {
    fn submit(
        &self,
        request: crate::channel::protocol::TurnRequest,
    ) -> crate::channel::protocol::BoxFuture<
        Result<
            Box<dyn crate::channel::protocol::TurnEventStream>,
            crate::channel::protocol::ChannelError,
        >,
    > {
        match self.inner() {
            Some(gateway) => gateway.submit(request),
            None => Box::pin(async {
                Err(crate::channel::protocol::ChannelError::error(
                    "Gateway not ready",
                ))
            }),
        }
    }
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
            weixin_runtime(weixin_key().as_js(), &weixin_update(&status));
        }));
        let state = ChannelStatePersistence::new(options.pilot_home.join("channels"));
        let feishu_gateway = Arc::new(FeishuTurnGateway::new());
        let turn_gateway: Arc<dyn TurnGateway> = feishu_gateway.clone();
        Self {
            server: ChannelServer::new(
                ChannelStartDeps {
                    config: Some(options.config.clone()),
                    report_status: Some(report_status),
                    log: options.log,
                    // weixin submits through its own `WeixinChannelOptions.gateway`
                    // port (`set_gateway_port`); feishu uses the shared
                    // channel-server port, which the `FeishuTurnGateway` handle
                    // fills in once the native submitter exists.
                    turn_gateway: Some(turn_gateway),
                },
                options.close_host.unwrap_or_else(noop_close_host),
            ),
            state,
            weixin,
            weixin_maker: options.weixin_maker.unwrap_or_else(default_weixin_maker),
            gateway: OnceLock::new(),
            feishu: options.feishu,
            feishu_slot: Arc::new(FeishuWebhookSlot::new()),
            feishu_gateway,
            config: options.config,
        }
    }

    pub fn server(&self) -> &ChannelServer {
        &self.server
    }

    /// The `activeFeishu` slot the native HTTP listener mounts at
    /// `/feishu/webhook`. Stable for the life of the assembly, so the listener
    /// can hold the resolver while the adapter comes and goes.
    pub fn feishu_webhook_slot(&self) -> Arc<FeishuWebhookSlot> {
        self.feishu_slot.clone()
    }

    /// Publish the A7 submitter the feishu adapter submits through.
    pub fn set_turn_submitter(&self, submitter: Arc<TurnSubmitter>) -> bool {
        self.feishu_gateway.set(submitter)
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
        let saved = self.state.load(weixin_key().as_js()).await;
        let mapper = match saved {
            Some(saved) => Arc::new(WeixinSessionMapper::from_saved(&saved)),
            None => Arc::new(WeixinSessionMapper::new()),
        };
        let state = self.state.clone();
        let on_state_change: WeixinStateChange =
            Arc::new(move |value| state.save(weixin_key().as_js(), value.clone()));
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

    /// ChannelAdapterBundle.makeFeishu: mapper from the persisted state,
    /// `onStateChange` persisting back through the debounce, and the active
    /// slot published at construction (TS assigns `activeFeishu` before
    /// `start`).
    async fn make_feishu(&self) -> Arc<dyn ChannelAdapter> {
        let saved = self.state.load(feishu_key().as_js()).await;
        let state = self.state.clone();
        let on_state_change =
            Arc::new(move |value: JsValue| state.save(feishu_key().as_js(), value));
        Arc::new(FeishuChannel::for_assembly(
            self.configured_feishu_options(),
            no_interaction(),
            self.feishu_slot.clone(),
            saved.as_ref(),
            Some(on_state_change),
        ))
    }

    /// `createFeishu`'s constructor argument: the `adapters.feishu` settings
    /// (an absent field stays `undefined` → the constructor's default). The
    /// assembly-level options supply the loopback `base_url` seam tests need;
    /// a real config keeps the origin its `domainName` selects.
    fn configured_feishu_options(&self) -> FeishuOptions {
        let Some(settings) = self
            .config
            .get("adapters")
            .and_then(|adapters| adapters.get("feishu"))
            .and_then(JsValue::as_object)
        else {
            return self.feishu.clone();
        };
        let mut options = FeishuOptions::from_object(settings);
        if self.feishu.base_url != crate::channel::feishu::FEISHU_BASE_URL
            && self.feishu.base_url != crate::channel::feishu::LARK_BASE_URL
        {
            options.base_url = self.feishu.base_url.clone();
        }
        options
    }

    /// ChannelAdapterBundle.createFeishu: `adapters.feishu.enabled === true`.
    pub async fn create_startup_feishu(&self) -> Option<Arc<dyn ChannelAdapter>> {
        if feishu_enabled(&self.config) {
            Some(self.make_feishu().await)
        } else {
            None
        }
    }

    /// ChannelAdapterBundle.hotStartFeishu. Pure reservation: the TS bundle
    /// only defines `hotStartWeixin` (the feishu login callback does not
    /// exist), and the native process has no feishu config hot-reload event
    /// source yet, so nothing calls this. Kept as the symmetric seam for the
    /// day a feishu hot start is wired.
    #[allow(dead_code)]
    pub async fn hot_start_feishu(&self) -> Result<(), ChannelError> {
        let channel = self.make_feishu().await;
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
            managed_channel_keys: vec![feishu_key(), weixin_key()],
            channels: Vec::new(),
        };
        if feishu_enabled(config) {
            input.channels.push(self.make_feishu().await);
        }
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
                channels.hot_start_weixin().await.map_err(|error| {
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

/// `config.adapters?.feishu?.enabled === true`.
fn feishu_enabled(config: &JsValue) -> bool {
    config
        .get("adapters")
        .and_then(|adapters| adapters.get("feishu"))
        .and_then(|feishu| feishu.get("enabled"))
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

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
    loader::{ChannelLoaderMounts, LoadEnabledChannelsOptions, load_enabled_channels},
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

/// TS `consoleLogger` (`pilotdeckServer.ts:67-72`): the channel log every
/// adapter's `start` receives — `info` to stdout, `warn` / `error` to stderr.
/// Until #2301 only the feishu/weixin startup existed and the assembly left
/// the log unset, so no channel line was observable; the loader-table keys
/// make that visible (the `api-server: listening on …` / `sms: twilio not
/// installed…` anchors the #1853 r1 probe recorded on the TS side).
pub fn console_channel_log() -> ChannelLog {
    Arc::new(|level: &str, message: &str| {
        write_console_line(
            level,
            message,
            &mut std::io::stdout(),
            &mut std::io::stderr(),
        )
    })
}

/// `consoleLogger`'s one line over an explicit sink pair: `info` to the out
/// sink, `warn` / `error` to the err sink. The production closure passes the
/// real stdout/stderr; the routing and failing-sink controls drive this same
/// entry (#2301 fix-r2 review P1 #5 — a control that only exercised the
/// helper could not see the production closure regress).
pub(crate) fn write_console_line(
    level: &str,
    message: &str,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) {
    match level {
        "info" => console_log_line(out, message),
        _ => console_log_line(err, message),
    }
}

/// One console line on an explicit sink. `console.log` / `console.error`
/// never fail the channel lifecycle, while `println!` panics when the write
/// fails (std behavior) — a closed stdout would abort an adapter's task after
/// it saved its listener state, stranding the listener (#2301 fix-r1 review
/// P0). The write error is dropped instead, like TS's best-effort console.
/// #2558: `pub` and re-exported one hop up (`native_process::console_log_line`)
/// so the A4 binary entry (`main.rs`) shares the same non-panicking writer as
/// the library call sites (`http_proxy`, `upstream`, `native_ws`, the memory
/// maintenance diagnostic) instead of keeping its own `eprintln!` copies.
pub fn console_log_line(sink: &mut dyn std::io::Write, message: &str) {
    let _ = writeln!(sink, "{message}");
}

/// The production loader-table mounts (#2301): one per implemented loader
/// key. Each channel module owns its own mount (the sms precedent);
/// wecomCallback's constructor takes the shared interaction port here, and
/// the `NoInteraction` default stays until the A7 boundary grows the respond
/// calls (the feishu/bluebubbles precedent, `A8.3-CH-FEISHU-008` /
/// `A8.3-CH-BLUEBUBBLES-002` / `A8.3-WECOM-CALLBACK-INTERACTION-001`).
/// `production_mounts_cover_every_non_rejected_loader_key` keeps this closed
/// against `CHANNEL_LOADER_KEYS`: a key that leaves the rejection table
/// without a mount here would return to the silent skip #2301 removed.
pub(crate) fn production_loader_mounts() -> ChannelLoaderMounts {
    let mut mounts = ChannelLoaderMounts::default();
    crate::channel::sms::register(&mut mounts);
    crate::channel::bluebubbles::register(&mut mounts);
    crate::channel::slack::register(&mut mounts);
    crate::channel::api_server::register(&mut mounts);
    crate::channel::webhook::register(&mut mounts);
    mounts.register(
        "wecomCallback",
        crate::channel::wecom_callback::WeComCallbackChannel::constructor(
            crate::channel::wecom_callback::no_interaction(),
        ),
    );
    mounts
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

/// The `ChannelStartDeps.turn_gateway` the whole channel server is
/// constructed with (feishu was the first consumer, hence the name). It
/// forwards to a [`SubmitterTurnGateway`] once the assembly publishes the
/// native submitter; until then `submit` fails with the same
/// "not started" shape as a missing gateway. Each request keeps its own
/// channel identity: every adapter submits its `TurnRequest.channel_key`,
/// and the gateway carries it through (#2301 fix-r1 — it used to overwrite
/// the requester's key with this handle's construction-time feishu one).
struct FeishuTurnGateway {
    submitter: OnceLock<Arc<TurnSubmitter>>,
}

impl FeishuTurnGateway {
    fn new() -> Self {
        Self {
            submitter: OnceLock::new(),
        }
    }

    fn set(&self, submitter: Arc<TurnSubmitter>) -> bool {
        self.submitter.set(submitter).is_ok()
    }

    fn inner(&self) -> Option<SubmitterTurnGateway> {
        self.submitter
            .get()
            .map(|submitter| SubmitterTurnGateway::new(submitter.clone()))
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
    /// The `ChannelStartDeps` this assembly's channel server (and through it
    /// every adapter it starts) is constructed with — TS's `baseDeps`
    /// (pilotdeckServer.ts:72-83) collected into the one explicit injection
    /// face instead of an inline literal: the config snapshot, the one status
    /// sink the lifecycle reporter and the adapters' own login reports share,
    /// the channel log, and the A7 turn-gateway handle. The weixin gateway
    /// port is the one startup dependency that cannot ride here: the native
    /// submitter only exists after the host builder ran, so weixin takes it
    /// through `WeixinChannelOptions.gateway` (`set_gateway_port`) — the same
    /// late-publish shape the `FeishuTurnGateway` handle below gives feishu.
    fn start_deps(
        config: Arc<JsValue>,
        report_status: StatusReporter,
        log: Option<ChannelLog>,
        turn_gateway: Arc<dyn TurnGateway>,
    ) -> ChannelStartDeps {
        ChannelStartDeps {
            config: Some(config),
            report_status: Some(report_status),
            log,
            turn_gateway: Some(turn_gateway),
        }
    }

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
                Self::start_deps(
                    options.config.clone(),
                    report_status,
                    options.log,
                    turn_gateway,
                ),
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

    /// `ChannelAdapterBundle.createStartupAdapters`'s configured half (#2301):
    /// `loadEnabledChannels(config.adapters)` over the production mounts —
    /// the six implemented loader-table keys construct in table order, a
    /// construction failure `console.error`s and continues (the
    /// error-then-continue loop, `loadEnabledChannels.ts:132-136`), and no
    /// options travel on this path: TS calls
    /// `this.loadConfiguredChannels(config.adapters)` with no second
    /// argument, so `pilotHome` stays `undefined` (only the matrix loader
    /// reads it, and matrix is not implemented). The error port is the same
    /// best-effort console write as the channel log: `assembly::start` calls
    /// this synchronously before any channel starts, outside every
    /// `catch_unwind`, so an `eprintln!` here (std panic on a failed write,
    /// #2301 fix-r2 review P0 #2) would abort the startup of every already
    /// constructed channel; TS's `console.error` never throws.
    pub fn create_startup_configured(&self) -> Vec<Arc<dyn ChannelAdapter>> {
        load_enabled_channels(
            self.config.get("adapters"),
            &LoadEnabledChannelsOptions { pilot_home: None },
            &production_loader_mounts(),
            &mut |message| console_log_line(&mut std::io::stderr(), &message),
        )
    }

    /// ChannelAdapterBundle.makeFeishu: mapper from the persisted state,
    /// `onStateChange` persisting back through the debounce, and the active
    /// slot published at construction (TS assigns `activeFeishu` before
    /// `start`).
    async fn make_feishu(&self) -> Arc<dyn ChannelAdapter> {
        self.make_feishu_with(&self.config).await
    }

    /// [`Self::make_feishu`] against an explicit config: `reload` must build
    /// from the config it was handed, not the one the assembly started with
    /// (`ChannelAdapterBundle.createStartupAdapters(config)` reads the passed
    /// snapshot, so a reloaded app secret / verify token / domain takes
    /// effect).
    async fn make_feishu_with(&self, config: &JsValue) -> Arc<dyn ChannelAdapter> {
        let saved = self.state.load(feishu_key().as_js()).await;
        let state = self.state.clone();
        let on_state_change =
            Arc::new(move |value: JsValue| state.save(feishu_key().as_js(), value));
        Arc::new(FeishuChannel::for_assembly(
            self.configured_feishu_options(config),
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
    fn configured_feishu_options(&self, config: &JsValue) -> FeishuOptions {
        let Some(settings) = config
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
    /// `config-disabled`). Not wired in the native process (#1725 item 10,
    /// caller inventory verified repo-wide): TS wires this through
    /// `configStore.subscribe` on `adapters.*` changes (pilotdeck.ts:148-172),
    /// and the native process has no config hot-reload event source yet — the
    /// same gap that leaves `ProjectAutomationBundle::reload` unwired — so
    /// wiring it here would be a one-off event source this closure round
    /// cannot feed. The reconcile semantics stay pinned by the callers below
    /// (channels_tests.rs and weixin replay_tests.rs), so the attribute is
    /// test-scoped like the automation sibling's, and any future wiring is a
    /// coordinator decision on the shared event source, not a weixin change.
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn reload(&self, config: &JsValue) -> Result<ReconcileResult, ChannelError> {
        let mut input = ReconcileInput {
            managed_channel_keys: vec![feishu_key(), weixin_key()],
            channels: Vec::new(),
        };
        if feishu_enabled(config) {
            input.channels.push(self.make_feishu_with(config).await);
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

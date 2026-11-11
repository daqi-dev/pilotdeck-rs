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
    feishu::{FeishuChannel, FeishuHostPorts, FeishuOptions, FeishuWebhookSlot},
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
    sync::{Arc, Mutex, OnceLock},
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
pub const IMPLEMENTED_BUNDLE_CHANNEL_KEYS: &[&str] = &["feishu", "weixin", "qq", "wecom"];

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
pub(crate) fn console_log_line(sink: &mut dyn std::io::Write, message: &str) {
    let _ = writeln!(sink, "{message}");
}

/// The production loader-table mounts (#2301): one per implemented loader
/// key. Each channel module owns its own mount (the sms precedent);
/// IM constructors receive independent helper state backed by the same live
/// GatewayInteractionCoordinator used by WS permission/elicitation responses.
/// The weak publication prevents a retired gateway being kept alive by adapters.
/// `production_mounts_cover_every_non_rejected_loader_key` keeps this closed
/// against `CHANNEL_LOADER_KEYS`: a key that leaves the rejection table
/// without a mount here would return to the silent skip #2301 removed.
#[cfg(test)]
pub(crate) fn production_loader_mounts() -> ChannelLoaderMounts {
    production_loader_mounts_with_interactions(&Arc::new(
        super::channel_interaction::NativeInteractionResponder::default(),
    ))
}
// Explicit acceptance-fixture transport seam. This never accepts DNS names or
// non-loopback addresses and cannot silently fall back to real services.
fn parse_test_channel_loopback_origin(value: &str) -> Result<String, &'static str> {
    let uri: hyper::Uri = value.parse().map_err(|_| "invalid URI")?;
    if uri.scheme_str() != Some("http") || uri.path_and_query().is_some_and(|p| p.as_str() != "/") {
        return Err("expected an HTTP origin without path or query");
    }
    let authority = uri.authority().ok_or("missing authority")?.as_str();
    let address: std::net::SocketAddrV4 = authority
        .parse()
        .map_err(|_| "expected IPv4 loopback and port")?;
    if *address.ip() != std::net::Ipv4Addr::LOCALHOST || address.port() == 0 {
        return Err("expected 127.0.0.1 and a nonzero port");
    }
    Ok(format!("http://{address}"))
}

fn test_channel_loopback_origin() -> Option<String> {
    match std::env::var("PDRS_TEST_CHANNEL_LOOPBACK_ORIGIN") {
        Ok(value) => Some(
            parse_test_channel_loopback_origin(&value).unwrap_or_else(|reason| {
                panic!("invalid PDRS_TEST_CHANNEL_LOOPBACK_ORIGIN: {reason}")
            }),
        ),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("invalid PDRS_TEST_CHANNEL_LOOPBACK_ORIGIN: non-UTF-8")
        }
    }
}

fn production_loader_mounts_with_interactions(
    responder: &Arc<super::channel_interaction::NativeInteractionResponder>,
) -> ChannelLoaderMounts {
    let mut mounts = ChannelLoaderMounts::default();
    let test_origin = test_channel_loopback_origin();
    let sms_factory = match test_origin.as_ref() {
        Some(origin) => {
            crate::channel::sms::sender::native_twilio_factory_with_base(origin.clone())
        }
        None => crate::channel::sms::sender::native_twilio_factory(),
    };
    crate::channel::sms::register_native(&mut mounts, responder.clone(), sms_factory);
    crate::channel::telegram::register_with_interaction(&mut mounts, responder.port());
    crate::channel::matrix::register_with_interaction(&mut mounts, responder.port());
    crate::channel::signal::register_with_interaction(&mut mounts, responder.port());
    crate::channel::mattermost::register_with_interaction(&mut mounts, responder.port());
    crate::channel::discord::register_with_interaction(&mut mounts, responder.port());
    let dingtalk_interaction = responder.port();
    mounts.register(
        "dingtalk",
        Arc::new(move |options| {
            Ok(Arc::new(crate::channel::dingtalk::DingTalkChannel::new(
                crate::channel::dingtalk::DingTalkOptions::from_object(options),
                dingtalk_interaction.clone(),
            )) as Arc<dyn ChannelAdapter>)
        }),
    );
    let email_interaction = responder.port();
    mounts.register(
        "email",
        Arc::new(move |options| {
            Ok(Arc::new(crate::channel::email::EmailChannel::new(
                crate::channel::email::EmailOptions::from_object(options),
                email_interaction.clone(),
            )) as Arc<dyn ChannelAdapter>)
        }),
    );
    let whatsapp_interaction = responder.port();
    mounts.register(
        "whatsapp",
        Arc::new(move |options| {
            Ok(Arc::new(crate::channel::whatsapp::WhatsAppChannel::new(
                crate::channel::whatsapp::WhatsAppOptions::from_object(options),
                whatsapp_interaction.clone(),
            )) as Arc<dyn ChannelAdapter>)
        }),
    );
    let home_assistant_interaction = responder.port();
    mounts.register(
        "homeassistant",
        Arc::new(move |options| {
            Ok(
                Arc::new(crate::channel::home_assistant::HomeAssistantChannel::new(
                    crate::channel::home_assistant::HomeAssistantOptions::from_object(options),
                    home_assistant_interaction.clone(),
                )) as Arc<dyn ChannelAdapter>,
            )
        }),
    );
    crate::channel::bluebubbles::register_with_interaction(&mut mounts, responder.port());
    crate::channel::slack::register_native(&mut mounts, responder.clone());
    crate::channel::api_server::register(&mut mounts);
    crate::channel::webhook::register(&mut mounts);
    mounts.register(
        "wecomCallback",
        match test_origin {
            Some(origin) => {
                crate::channel::wecom_callback::WeComCallbackChannel::constructor_with_base(
                    responder.port(),
                    format!("{origin}/cgi-bin"),
                )
            }
            None => {
                crate::channel::wecom_callback::WeComCallbackChannel::constructor(responder.port())
            }
        },
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

#[derive(Clone, Default)]
pub struct ChannelEndpointOverrides {
    /// Isolated executable used by actual channel command tests; unavailable in production.
    #[cfg(test)]
    pub channel_system_cli: Option<PathBuf>,
    pub sms_twilio_api_base: Option<String>,
    pub slack_api_base: Option<String>,
    pub wecom_callback_api_base: Option<String>,
    pub feishu_api_base: Option<String>,
    pub telegram_api_root: Option<String>,
    pub discord_api_root: Option<String>,
    pub qq_api_base: Option<String>,
    pub qq_token_url: Option<String>,
    pub dingtalk_stream_endpoint: Option<String>,
    pub dingtalk_webhook_route: Option<crate::channel::dingtalk::WebhookRoute>,
}

// The same constructor seams used by actual-Server tests are available to the
// production executable only behind the already validated explicit loopback env.
#[cfg(not(test))]
fn fixture_channel_endpoints() -> ChannelEndpointOverrides {
    let Some(origin) = test_channel_loopback_origin() else {
        return ChannelEndpointOverrides::default();
    };
    let dingtalk_send = format!("{origin}/dingtalk/send");
    ChannelEndpointOverrides {
        slack_api_base: Some(origin.clone()),
        feishu_api_base: Some(origin.clone()),
        telegram_api_root: Some(origin.clone()),
        discord_api_root: Some(origin.clone()),
        qq_api_base: Some(origin.clone()),
        qq_token_url: Some(format!("{origin}/qq/token")),
        dingtalk_stream_endpoint: Some(format!("{origin}/dingtalk/open")),
        dingtalk_webhook_route: Some(Arc::new(move |_| dingtalk_send.clone())),
        ..Default::default()
    }
}

pub struct NativeChannelsOptions {
    #[cfg(test)]
    pub endpoints: ChannelEndpointOverrides,
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
    control: Arc<super::channel_control::NativeChannelControl>,
    pub(super) search: OnceLock<Arc<super::channel_search::NativeChannelSearch>>,
    #[cfg(unix)]
    pub(super) system: OnceLock<Arc<super::channel_system::NativeChannelSystem>>,
    managed_keys: Mutex<Vec<ChannelKey>>,
    endpoints: ChannelEndpointOverrides,
    interaction: Arc<super::channel_interaction::NativeInteractionResponder>,
    pilot_home: PathBuf,
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
    fn submit_with_options(
        &self,
        request: crate::channel::protocol::TurnRequest,
        options: crate::channel::protocol::ChannelTurnOptions,
    ) -> crate::channel::protocol::BoxFuture<
        Result<Box<dyn crate::channel::protocol::TurnEventStream>, ChannelError>,
    > {
        match self.inner() {
            Some(gateway) => gateway.submit_with_options(request, options),
            None => Box::pin(async { Err(ChannelError::error("Gateway not ready")) }),
        }
    }

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
        #[cfg(test)]
        let endpoints = options.endpoints;
        #[cfg(not(test))]
        let endpoints = fixture_channel_endpoints();
        let feishu = options.feishu;
        let feishu = match &endpoints.feishu_api_base {
            Some(base) => FeishuOptions {
                base_url: base.clone(),
                ..feishu
            },
            None => feishu,
        };
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
        if let Some(origin) = test_channel_loopback_origin() {
            weixin.login = WeixinChannelOptions::with_login_base_url(origin).login;
        }
        weixin.runtime_status = Some(Arc::new(move |status| {
            weixin_runtime(weixin_key().as_js(), &weixin_update(&status));
        }));
        let state = ChannelStatePersistence::new(options.pilot_home.join("channels"));
        let feishu_gateway = Arc::new(FeishuTurnGateway::new());
        let turn_gateway: Arc<dyn TurnGateway> = feishu_gateway.clone();
        Self {
            interaction: Arc::default(),
            control: Arc::default(),
            search: OnceLock::new(),
            #[cfg(unix)]
            system: OnceLock::new(),
            managed_keys: Mutex::new(
                IMPLEMENTED_BUNDLE_CHANNEL_KEYS
                    .iter()
                    .map(|key| ChannelKey::new(*key))
                    .collect(),
            ),
            endpoints,
            pilot_home: options.pilot_home,
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
            feishu,
            feishu_slot: Arc::new(FeishuWebhookSlot::new()),
            feishu_gateway,
            config: options.config,
        }
    }

    pub(super) fn set_control(&self, requests: &Arc<super::requests::Requests>) -> bool {
        self.control.publish(requests)
    }

    pub fn set_interactions(
        &self,
        coordinator: &Arc<crate::runtime::interaction::GatewayInteractionCoordinator>,
    ) -> bool {
        self.interaction.publish(coordinator)
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
        options.host_ports = Some(self.host_ports());
        options.interaction = Some(self.interaction.port());
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
    /// TS uses its process pilot home when no loader override is supplied.
    /// Native embeddings pass their explicit isolated home, so a loader never
    /// falls back to the coordinating user's home. The error port is the same
    /// best-effort console write as the channel log: `assembly::start` calls
    /// this synchronously before any channel starts, outside every
    /// `catch_unwind`, so an `eprintln!` here (std panic on a failed write,
    /// #2301 fix-r2 review P0 #2) would abort the startup of every already
    /// constructed channel; TS's `console.error` never throws.
    pub fn create_startup_configured(&self) -> Vec<Arc<dyn ChannelAdapter>> {
        self.create_configured(&self.config)
    }

    fn create_configured(&self, config: &JsValue) -> Vec<Arc<dyn ChannelAdapter>> {
        let channels = load_enabled_channels(
            config.get("adapters"),
            &LoadEnabledChannelsOptions {
                pilot_home: Some(self.pilot_home.to_string_lossy().into_owned().into()),
            },
            &self.configured_mounts(),
            &mut |message| console_log_line(&mut std::io::stderr(), &message),
        );
        let mut managed = self.managed_keys.lock().expect("channel managed keys");
        for channel in &channels {
            if !managed.contains(channel.channel_key()) {
                managed.push(channel.channel_key().clone());
            }
        }
        channels
    }

    fn configured_mounts(&self) -> ChannelLoaderMounts {
        let mounts = production_loader_mounts_with_interactions(&self.interaction);
        self.with_endpoint_overrides(mounts)
    }

    fn with_endpoint_overrides(&self, mut mounts: ChannelLoaderMounts) -> ChannelLoaderMounts {
        if let Some(base) = self.endpoints.slack_api_base.clone() {
            crate::channel::slack::register_native_with_base(
                &mut mounts,
                self.interaction.clone(),
                base,
            );
        }
        if let Some(base) = self.endpoints.wecom_callback_api_base.clone() {
            mounts.register(
                "wecomCallback",
                crate::channel::wecom_callback::WeComCallbackChannel::constructor_with_base(
                    self.interaction.port(),
                    base,
                ),
            );
        }
        if let Some(base) = self.endpoints.sms_twilio_api_base.clone() {
            crate::channel::sms::register_native(
                &mut mounts,
                self.interaction.clone(),
                crate::channel::sms::sender::native_twilio_factory_with_base(base),
            );
        }
        if let Some(api_root) = self.endpoints.telegram_api_root.clone() {
            let interaction = self.interaction.port();
            mounts.register(
                "telegram",
                Arc::new(move |options| {
                    Ok(
                        Arc::new(crate::channel::telegram::TelegramChannel::with_interaction(
                            crate::channel::telegram::TelegramOptions {
                                api_root: Some(api_root.clone()),
                                token: options
                                    .get("token")
                                    .filter(|v| !crate::channel::sms::nullish(v))
                                    .map(crate::channel::sms::js_string_of),
                                webhook_url: options
                                    .get("webhookUrl")
                                    .filter(|v| !crate::channel::sms::nullish(v))
                                    .map(crate::channel::sms::js_string_of),
                                ..Default::default()
                            },
                            interaction.clone(),
                        )) as Arc<dyn ChannelAdapter>,
                    )
                }),
            );
        }
        if let Some(api_root) = self.endpoints.discord_api_root.clone() {
            let interaction = self.interaction.port();
            mounts.register(
                "discord",
                Arc::new(move |options| {
                    Ok(
                        Arc::new(crate::channel::discord::DiscordChannel::with_interaction(
                            crate::channel::discord::DiscordOptions {
                                api_root: Some(api_root.clone()),
                                token: options
                                    .get("token")
                                    .filter(|v| !crate::channel::sms::nullish(v))
                                    .map(crate::channel::sms::js_string_of),
                                ..Default::default()
                            },
                            interaction.clone(),
                        )) as Arc<dyn ChannelAdapter>,
                    )
                }),
            );
        }
        if self.endpoints.dingtalk_stream_endpoint.is_some()
            || self.endpoints.dingtalk_webhook_route.is_some()
        {
            let endpoints = self.endpoints.clone();
            let interaction = self.interaction.port();
            mounts.register(
                "dingtalk",
                Arc::new(move |options| {
                    let mut options =
                        crate::channel::dingtalk::DingTalkOptions::from_object(options);
                    options.stream_endpoint = endpoints.dingtalk_stream_endpoint.clone();
                    options.webhook_route = endpoints.dingtalk_webhook_route.clone();
                    Ok(Arc::new(crate::channel::dingtalk::DingTalkChannel::new(
                        options,
                        interaction.clone(),
                    )) as Arc<dyn ChannelAdapter>)
                }),
            );
        }
        mounts
    }

    async fn make_qq_with(&self, config: &JsValue) -> Arc<dyn ChannelAdapter> {
        use crate::channel::qq::{QQChannel, QQOptions, QQSessionMapper, QQSessionMapperState};
        let saved = self.state.load(&"qq".into()).await;
        let mapper = QQSessionMapper::new(
            saved
                .as_ref()
                .map(QQSessionMapperState::from_value)
                .unwrap_or_default(),
            Arc::new(|| uuid::Uuid::new_v4().to_string().into()),
        );
        let state = self.state.clone();
        let settings = config.get("adapters").and_then(|v| v.get("qq"));
        let string = |key| {
            settings
                .and_then(|s| s.get(key))
                .and_then(JsValue::as_string)
                .cloned()
        };
        let strings = |key| match settings.and_then(|s| s.get(key)) {
            Some(JsValue::Array(values)) => Some(
                values
                    .iter()
                    .filter_map(JsValue::as_string)
                    .cloned()
                    .collect(),
            ),
            _ => None,
        };
        let options = QQOptions {
            app_id: string("appId"),
            client_secret: string("clientSecret"),
            allow_groups: strings("allowGroups"),
            trigger_prefixes: strings("triggerPrefixes"),
            max_message_length: settings
                .and_then(|s| s.get("maxMessageLength"))
                .and_then(|v| {
                    if let JsValue::Number(n) = v {
                        Some(*n as usize)
                    } else {
                        None
                    }
                }),
            mapper: Some(mapper),
            on_state_change: Some(Arc::new(move |value| {
                state.save(&"qq".into(), value.to_value())
            })),
            ..Default::default()
        };
        let options = QQOptions {
            api_base: self.endpoints.qq_api_base.clone(),
            token_url: self.endpoints.qq_token_url.clone(),
            ..options
        };
        Arc::new(QQChannel::with_interaction(
            options,
            self.interaction.port(),
        ))
    }

    pub async fn create_startup_qq(&self) -> Option<Arc<dyn ChannelAdapter>> {
        if bundle_enabled(&self.config, "qq") {
            Some(self.make_qq_with(&self.config).await)
        } else {
            None
        }
    }

    async fn make_wecom_with(&self, config: &JsValue) -> Arc<dyn ChannelAdapter> {
        use crate::channel::protocol::{ChannelStatusPort, ChannelUpdatePort};
        let settings = config.get("adapters").and_then(|v| v.get("wecom"));
        let mut options = pilotdeck_protocol::JsObject::new();
        options.insert(
            "botKey".into(),
            settings
                .and_then(|s| s.get("token"))
                .cloned()
                .unwrap_or(JsValue::Undefined),
        );
        options.insert(
            "extra".into(),
            settings
                .and_then(|s| s.get("extra"))
                .cloned()
                .unwrap_or(JsValue::Undefined),
        );
        let unavailable = Arc::new(UnboundChannelSystem);
        let status: Arc<dyn ChannelStatusPort> = unavailable.clone();
        let update: Arc<dyn ChannelUpdatePort> = unavailable;
        #[cfg(unix)]
        let (status, update) = self
            .system
            .get()
            .map(|system| {
                (
                    system.clone() as Arc<dyn ChannelStatusPort>,
                    system.clone() as Arc<dyn ChannelUpdatePort>,
                )
            })
            .unwrap_or((status, update));
        Arc::new(crate::channel::wecom::WeComChannel::new(
            &JsValue::Object(options),
            self.interaction.port(),
            Some(self.control.clone()),
            self.search.get().map(|port| port.clone() as Arc<_>),
            status,
            update,
            Some(self.pilot_home.to_string_lossy().into_owned()),
            Some(Arc::new(self.state.clone())),
        ))
    }

    pub async fn create_startup_wecom(&self) -> Option<Arc<dyn ChannelAdapter>> {
        if bundle_enabled(&self.config, "wecom") {
            Some(self.make_wecom_with(&self.config).await)
        } else {
            None
        }
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
        Arc::new(
            FeishuChannel::for_assembly(
                self.configured_feishu_options(config),
                self.interaction.port(),
                self.feishu_slot.clone(),
                saved.as_ref(),
                Some(on_state_change),
            )
            .with_host_ports(self.host_ports()),
        )
    }

    fn host_ports(&self) -> FeishuHostPorts {
        let mut ports = FeishuHostPorts {
            control: Some(self.control.clone()),
            search: self.search.get().map(|port| port.clone() as Arc<_>),
            pilot_home: Some(self.pilot_home.clone()),
            ..FeishuHostPorts::default()
        };
        #[cfg(unix)]
        if let Some(system) = self.system.get() {
            ports.status = Some(system.clone());
            ports.update = Some(system.clone());
        }
        ports
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
    /// `config-disabled`). No caller in the native process yet — the config
    /// hot-reload event source lands with the pr2 channel cluster — so the
    /// reconcile semantics are pinned by the assembly tests for now.
    #[allow(dead_code)]
    pub async fn reload(&self, config: &JsValue) -> Result<ReconcileResult, ChannelError> {
        let mut input = ReconcileInput {
            managed_channel_keys: Vec::new(),
            channels: Vec::new(),
        };
        if feishu_enabled(config) {
            input.channels.push(self.make_feishu_with(config).await);
        }
        if weixin_enabled(config) {
            input.channels.push(self.make_weixin().await);
        }
        if bundle_enabled(config, "qq") {
            input.channels.push(self.make_qq_with(config).await);
        }
        if bundle_enabled(config, "wecom") {
            input.channels.push(self.make_wecom_with(config).await);
        }
        input.channels.extend(self.create_configured(config));
        input.managed_channel_keys = self
            .managed_keys
            .lock()
            .expect("channel managed keys")
            .clone();
        self.server.reconcile_channels(input).await
    }

    /// ChannelAdapterBundle.flush.
    pub async fn flush(&self) -> Result<(), String> {
        self.state.flush().await
    }

    /// pilotdeckServer.closeServer's channel half (stop every running channel
    /// through the lifecycle core, then run the close host).
    pub async fn close(&self) -> Result<(), ChannelError> {
        let channel_close = self.server.close();
        #[cfg(unix)]
        let system_result = if let Some(system) = self.system.get() {
            system.stop().await
        } else {
            Ok(())
        };
        let channel_result = channel_close.await;
        channel_result?;
        #[cfg(unix)]
        system_result?;
        Ok(())
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

fn bundle_enabled(config: &JsValue, key: &str) -> bool {
    config
        .get("adapters")
        .and_then(|v| v.get(key))
        .and_then(|v| v.get("enabled"))
        == Some(&JsValue::Bool(true))
}

// A test embedding can leave system commands unbound. Production Server installs
// NativeChannelSystem before constructing adapters and shares its owned jobs.
struct UnboundChannelSystem;
impl crate::channel::protocol::ChannelStatusPort for UnboundChannelSystem {
    fn read(
        &self,
    ) -> crate::channel::protocol::BoxFuture<
        Result<crate::channel::protocol::ChannelStatusSnapshot, ChannelError>,
    > {
        Box::pin(async { Err(ChannelError::error("Host status port is not configured")) })
    }
}
impl crate::channel::protocol::ChannelUpdatePort for UnboundChannelSystem {
    fn run(
        &self,
        _check: bool,
    ) -> crate::channel::protocol::BoxFuture<
        Result<crate::channel::protocol::ChannelUpdateResult, ChannelError>,
    > {
        Box::pin(async { Err(ChannelError::error("Host update port is not configured")) })
    }
}

#[cfg(test)]
mod loopback_fixture_origin_tests {
    use super::parse_test_channel_loopback_origin;
    #[test]
    fn only_plain_ipv4_loopback_origins_with_explicit_port_are_allowed() {
        assert_eq!(
            parse_test_channel_loopback_origin("http://127.0.0.1:4321/").unwrap(),
            "http://127.0.0.1:4321"
        );
        for input in [
            "https://127.0.0.1:4321",
            "http://localhost:4321",
            "http://127.0.0.2:4321",
            "http://0.0.0.0:4321",
            "http://192.0.2.1:4321",
            "http://127.0.0.1",
            "http://127.0.0.1:0",
            "http://user@127.0.0.1:4321",
            "http://127.0.0.1:4321/x",
            "http://127.0.0.1:4321/?x=1",
            "not a URL",
        ] {
            assert!(
                parse_test_channel_loopback_origin(input).is_err(),
                "accepted {input}"
            );
        }
    }
}

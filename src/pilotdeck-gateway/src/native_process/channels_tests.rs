//! #1328 A8.3 assembly tests: the weixin bundle (`NativeChannels`) on the
//! #1327 lifecycle core — the strict startup gate, the `prepareWeixinLogin`
//! hot start and its replacement, config reconcile, close ordering, durable
//! mapper state, and the shared runtime-status sink. The channel adapter is
//! the `weixin_maker` seam's controlled mock (the TS bundle tests'
//! `createWeixin`); no external service is ever contacted.
use super::channels::{NativeChannels, NativeChannelsOptions, WeixinChannelMaker};
use crate::channel::protocol::{
    BoxFuture, ChannelAdapter, ChannelError, ChannelHandle, ChannelKey, ChannelStartDeps,
    ReconcileResult,
};
use crate::channel::runtime_status::{channel_runtime_status_path, read_snapshot};
use crate::channel::weixin::{
    WeixinAbortFuture, WeixinChannelOptions, WeixinChannelState, WeixinChannelStatus,
    WeixinGatewayPort, WeixinRuntimeStatusReporter, WeixinSessionMapper, WeixinStateChange,
    WeixinTurnStream,
};
use pilotdeck_protocol::{JsString, JsValue, parse_json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

/// A temporary directory this test owns; removed on drop.
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "pdrs-1328-channels-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => Self(path),
            Err(error) => panic!("#1328 channels test root: {error}"),
        }
    }
    fn path(&self) -> PathBuf {
        self.0.clone()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The lifecycle events the mock adapters and handles record.
#[derive(Clone, Default)]
struct Lifecycle(Arc<Mutex<Vec<String>>>);
impl Lifecycle {
    fn push(&self, event: String) {
        self.0.lock().expect("#1328 lifecycle mutex").push(event);
    }
    fn events(&self) -> Vec<String> {
        self.0.lock().expect("#1328 lifecycle mutex").clone()
    }
    /// Waits until at least `count` events exist. Every wait states its
    /// termination condition: a 5-second bound on the count.
    async fn until(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let recorded = self.0.lock().expect("#1328 lifecycle mutex").len();
                if recorded >= count {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("#1328 lifecycle event count reached");
    }
}

struct MockWeixin {
    lifecycle: Lifecycle,
}
impl ChannelAdapter for MockWeixin {
    fn channel_key(&self) -> &ChannelKey {
        static KEY: std::sync::OnceLock<ChannelKey> = std::sync::OnceLock::new();
        KEY.get_or_init(|| ChannelKey::new("weixin"))
    }
    fn start(
        &self,
        _deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        self.lifecycle.push("start".into());
        let lifecycle = self.lifecycle.clone();
        Box::pin(async move {
            Ok(Arc::new(MockHandle { lifecycle }) as Arc<dyn ChannelHandle>)
        })
    }
}
struct MockHandle {
    lifecycle: Lifecycle,
}
impl ChannelHandle for MockHandle {
    fn stop(&self, reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        self.lifecycle.push(format!("stop:{reason}"));
        Box::pin(async { Ok(()) })
    }
}

/// What the maker observed on the options each construction passed through.
#[derive(Clone)]
struct CapturedOptions {
    mapper: Arc<WeixinSessionMapper>,
    on_state_change: Option<WeixinStateChange>,
    gateway: Option<Arc<dyn WeixinGatewayPort>>,
    runtime_status: Option<WeixinRuntimeStatusReporter>,
}

struct Harness {
    channels: Arc<NativeChannels>,
    lifecycle: Lifecycle,
    captured: Arc<Mutex<Vec<CapturedOptions>>>,
    home: Root,
}

impl Harness {
    fn latest(&self) -> CapturedOptions {
        self.captured
            .lock()
            .expect("#1328 captured options mutex")
            .last()
            .cloned()
            .expect("#1328 at least one construction")
    }
}

fn harness(config: &str) -> Harness {
    let home = Root::new();
    let lifecycle = Lifecycle::default();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let recording = captured.clone();
    let maker: WeixinChannelMaker = {
        let lifecycle = lifecycle.clone();
        Arc::new(move |options: WeixinChannelOptions| {
            recording
                .lock()
                .expect("#1328 captured options mutex")
                .push(CapturedOptions {
                    mapper: options.mapper.clone(),
                    on_state_change: options.on_state_change.clone(),
                    gateway: options.gateway.clone(),
                    runtime_status: options.runtime_status.clone(),
                });
            Arc::new(MockWeixin {
                lifecycle: lifecycle.clone(),
            }) as Arc<dyn ChannelAdapter>
        })
    };
    let channels = Arc::new(NativeChannels::new(NativeChannelsOptions {
        pilot_home: home.path(),
        config: Arc::new(parse_json(config).expect("#1328 harness config JSON")),
        weixin: WeixinChannelOptions::default(),
        weixin_maker: Some(maker),
        log: None,
        report_status: None,
        close_host: None,
    }));
    Harness {
        channels,
        lifecycle,
        captured,
        home,
    }
}

/// The CLI callback's `requestedAt` must be an ISO timestamp.
fn parse_iso(value: &JsValue) -> Option<f64> {
    use pilotdeck_transport::{server::ExecutionClock, system_clock::SystemExecutionClock};
    SystemExecutionClock
        .parse_date(value.as_string().expect("#1328 requestedAt is a string"))
        .expect("#1328 requestedAt must parse or report why")
}

fn field_text(value: &JsValue, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(JsValue::as_string)
        .map(|text| String::from_utf16_lossy(text.units()))
}

#[tokio::test]
async fn prepare_login_callback_hot_starts_and_replaces() {
    let harness = harness("{}");
    let callback = harness.channels.prepare_login_callback();
    let first = callback().await.expect("#1328 prepare resolves");
    let keys: Vec<String> = first
        .as_object()
        .expect("#1328 prepare result is an object")
        .entries()
        .iter()
        .map(|(key, _)| key.to_utf8().expect("#1328 key UTF-8").to_string())
        .collect();
    assert_eq!(
        keys, ["requested", "requestedAt"],
        "#1328 prepare result field order"
    );
    assert_eq!(
        first.get("requested"),
        Some(&JsValue::Bool(true)),
        "#1328 prepare reports requested"
    );
    assert!(
        parse_iso(first.get("requestedAt").expect("#1328 requestedAt present")).is_some(),
        "#1328 prepare requestedAt is an ISO timestamp"
    );
    harness.lifecycle.until(1).await;
    assert_eq!(
        harness.lifecycle.events(),
        ["start"],
        "#1328 prepare hot-started the channel"
    );
    // A repeat call replaces the previous instance (ChannelAdapterBundle's
    // hotStartWeixin through the lifecycle core).
    let second = callback().await.expect("#1328 repeat prepare resolves");
    assert_eq!(
        second.get("requested"),
        Some(&JsValue::Bool(true)),
        "#1328 repeat prepare reports requested"
    );
    assert!(
        parse_iso(
            second
                .get("requestedAt")
                .expect("#1328 repeat requestedAt present")
        )
        .is_some(),
        "#1328 repeat requestedAt is an ISO timestamp"
    );
    harness.lifecycle.until(3).await;
    assert_eq!(
        harness.lifecycle.events(),
        ["start", "stop:hot-reload", "start"],
        "#1328 repeat prepare replaces the old instance"
    );
    harness.channels.close().await.expect("#1328 test close");
}

#[tokio::test]
async fn startup_gate_requires_strict_boolean_true() {
    for (config, expected) in [
        (r#"{"adapters":{"weixin":{"enabled":true}}}"#, true),
        (r#"{"adapters":{"weixin":{"enabled":"true"}}}"#, false),
        (r#"{"adapters":{"weixin":{"enabled":1}}}"#, false),
        (r#"{"adapters":{"weixin":{}}}"#, false),
        (r#"{"adapters":{}}"#, false),
        (r#"{}"#, false),
    ] {
        let harness = harness(config);
        let channel = harness.channels.create_startup_weixin().await;
        assert_eq!(
            channel.is_some(),
            expected,
            "#1328 startup gate ({config})"
        );
        if let Some(channel) = channel {
            harness.channels.server().start_channels(vec![channel]);
            harness.lifecycle.until(1).await;
            assert_eq!(
                harness.lifecycle.events(),
                ["start"],
                "#1328 enabled startup starts the channel"
            );
            harness.channels.close().await.expect("#1328 gate close");
        }
    }
}

#[tokio::test]
async fn reload_reconciles_the_weixin_key() {
    let enabled = parse_json(r#"{"adapters":{"weixin":{"enabled":true}}}"#)
        .expect("#1328 enabled config JSON");
    let disabled = parse_json("{}").expect("#1328 disabled config JSON");
    let harness = harness("{}");
    let result = harness
        .channels
        .reload(&disabled)
        .await
        .expect("#1328 disabled reload resolves");
    assert_eq!(
        result,
        ReconcileResult::default(),
        "#1328 disabled reload stops nothing"
    );
    let result = harness
        .channels
        .reload(&enabled)
        .await
        .expect("#1328 enable reload resolves");
    assert_eq!(
        result.started,
        vec![ChannelKey::new("weixin")],
        "#1328 enable reload starts weixin"
    );
    assert!(result.stopped.is_empty(), "#1328 enable reload stops nothing");
    harness.lifecycle.until(1).await;
    let result = harness
        .channels
        .reload(&disabled)
        .await
        .expect("#1328 disable reload resolves");
    assert!(
        result.started.is_empty(),
        "#1328 disable reload starts nothing"
    );
    assert_eq!(
        result.stopped,
        vec![ChannelKey::new("weixin")],
        "#1328 disable reload stops weixin"
    );
    harness.lifecycle.until(2).await;
    assert_eq!(
        harness.lifecycle.events(),
        ["start", "stop:config-disabled"],
        "#1328 disable stop reason"
    );
    let result = harness
        .channels
        .reload(&disabled)
        .await
        .expect("#1328 repeated disabled reload resolves");
    assert_eq!(
        result,
        ReconcileResult::default(),
        "#1328 repeated disabled reload is a no-op"
    );
    let result = harness
        .channels
        .reload(&enabled)
        .await
        .expect("#1328 re-enable reload resolves");
    assert_eq!(
        result.started,
        vec![ChannelKey::new("weixin")],
        "#1328 re-enable reload starts weixin"
    );
    harness.channels.close().await.expect("#1328 reload close");
}

#[tokio::test]
async fn close_stops_running_channel_and_memoizes() {
    let empty = harness("{}");
    empty
        .channels
        .close()
        .await
        .expect("#1328 empty server closes cleanly");
    let running = harness("{}");
    running
        .channels
        .prepare_login_callback()
        ()
        .await
        .expect("#1328 running harness starts");
    running.lifecycle.until(1).await;
    running
        .channels
        .close()
        .await
        .expect("#1328 running server closes");
    running.lifecycle.until(2).await;
    assert_eq!(
        running.lifecycle.events(),
        ["start", "stop:server-shutdown"],
        "#1328 close stop reason"
    );
    running
        .channels
        .close()
        .await
        .expect("#1328 close memoizes its result");
}

struct ProbePort;
impl WeixinGatewayPort for ProbePort {
    fn submit_turn(&self, _input: JsValue) -> WeixinTurnStream {
        unreachable!("#1328 probe port never submits")
    }
    fn abort_turn<'a>(
        &'a self,
        _session_key: &'a JsString,
        _run_id: Option<&'a JsString>,
    ) -> WeixinAbortFuture<'a> {
        unreachable!("#1328 probe port never aborts")
    }
}

#[tokio::test]
async fn gateway_port_publishes_once_and_reaches_channels() {
    let published = harness("{}");
    let port: Arc<dyn WeixinGatewayPort> = Arc::new(ProbePort);
    assert!(
        published.channels.set_gateway_port(port.clone()),
        "#1328 first publish wins"
    );
    assert!(
        !published.channels.set_gateway_port(Arc::new(ProbePort)),
        "#1328 second publish is rejected"
    );
    published
        .channels
        .prepare_login_callback()
        ()
        .await
        .expect("#1328 published harness starts");
    published.lifecycle.until(1).await;
    let constructed = published.latest();
    let observed = constructed
        .gateway
        .expect("#1328 published port reaches the channel");
    assert!(
        Arc::ptr_eq(&observed, &port),
        "#1328 constructed channel keeps the published port identity"
    );
    published.channels.close().await.expect("#1328 port close");
    // Without a publish, the constructed channel has no gateway (the TS
    // baseDeps.gateway is only present once the gateway exists).
    let fresh = harness("{}");
    fresh
        .channels
        .prepare_login_callback()
        ()
        .await
        .expect("#1328 unpublished harness starts");
    fresh.lifecycle.until(1).await;
    assert!(
        fresh.latest().gateway.is_none(),
        "#1328 unpublished port stays absent"
    );
    fresh.channels.close().await.expect("#1328 fresh close");
}

#[tokio::test]
async fn hot_start_restores_mapper_state_and_persists_changes() {
    let harness = harness("{}");
    harness
        .channels
        .prepare_login_callback()
        ()
        .await
        .expect("#1328 mapper harness starts");
    harness.lifecycle.until(1).await;
    let chat = JsString::from("chat-1");
    let first = harness.latest();
    assert_eq!(
        String::from_utf16_lossy(first.mapper.get_session(&chat).units()),
        "weixin:chat=chat-1:general",
        "#1328 fresh mapper starts unmapped"
    );
    // The adapter's onStateChange persists through the debounced writer.
    let snapshot = parse_json(
        r#"{"activeByChatId":{"chat-1":"weixin:chat=chat-1:s_persisted"},"projectByChatId":{"chat-1":"proj-1"}}"#,
    )
    .expect("#1328 mapper snapshot JSON");
    first
        .on_state_change
        .expect("#1328 on_state_change wired")(&snapshot);
    harness
        .channels
        .flush()
        .await
        .expect("#1328 state flush resolves");
    let raw = std::fs::read_to_string(
        harness
            .home
            .path()
            .join("channels")
            .join("weixin.state.json"),
    )
    .expect("#1328 persisted mapper state file");
    let saved = parse_json(&raw).expect("#1328 persisted mapper state JSON");
    assert_eq!(
        field_text(&saved.get("activeByChatId").expect("#1328 active map"), "chat-1"),
        Some("weixin:chat=chat-1:s_persisted".into()),
        "#1328 flushed active session"
    );
    assert_eq!(
        field_text(&saved.get("projectByChatId").expect("#1328 project map"), "chat-1"),
        Some("proj-1".into()),
        "#1328 flushed project binding"
    );
    // A second hot start restores the mapper from the persisted state.
    harness
        .channels
        .prepare_login_callback()
        ()
        .await
        .expect("#1328 repeat hot start resolves");
    harness.lifecycle.until(3).await;
    let second = harness.latest();
    assert_eq!(
        String::from_utf16_lossy(second.mapper.get_session(&chat).units()),
        "weixin:chat=chat-1:s_persisted",
        "#1328 restored mapper keeps the persisted session"
    );
    assert_eq!(
        second.mapper.get_project(&chat).map(|key| String::from_utf16_lossy(key.units())),
        Some("proj-1".into()),
        "#1328 restored mapper keeps the persisted project"
    );
    harness.channels.close().await.expect("#1328 mapper close");
}

fn read_status_file(path: &PathBuf) -> (String, JsValue) {
    let raw = std::fs::read_to_string(path).expect("#1328 runtime status file");
    let parsed = parse_json(&raw).expect("#1328 runtime status JSON");
    (raw, parsed)
}

fn entry_keys(value: &JsValue) -> Vec<String> {
    value
        .as_object()
        .expect("#1328 runtime status entry object")
        .entries()
        .iter()
        .map(|(key, _)| key.to_utf8().expect("#1328 key UTF-8").to_string())
        .collect()
}

#[tokio::test]
async fn runtime_status_sink_serves_lifecycle_and_login_reports() {
    let harness = harness("{}");
    // An absent file reads as the epoch empty snapshot.
    let absent = JsValue::Object(read_snapshot(&harness.home.path()));
    assert_eq!(
        field_text(&absent, "updatedAt"),
        Some("1970-01-01T00:00:00.000Z".into()),
        "#1328 absent file reads epoch updatedAt"
    );
    assert!(
        absent
            .get("channels")
            .and_then(JsValue::as_object)
            .expect("#1328 absent snapshot channels map")
            .entries()
            .is_empty(),
        "#1328 absent file reads empty channels"
    );
    // An unparsable file also reads as the epoch empty snapshot.
    let path = channel_runtime_status_path(&harness.home.path());
    std::fs::create_dir_all(path.parent().expect("#1328 status parent dir"))
        .expect("#1328 status dir");
    std::fs::write(&path, "{oops").expect("#1328 invalid status file");
    let invalid = read_snapshot(&harness.home.path());
    assert_eq!(
        field_text(&JsValue::Object(invalid), "updatedAt"),
        Some("1970-01-01T00:00:00.000Z".into()),
        "#1328 invalid file reads epoch updatedAt"
    );
    // The lifecycle reporter writes the connected entry.
    harness
        .channels
        .prepare_login_callback()
        ()
        .await
        .expect("#1328 status harness starts");
    harness.lifecycle.until(1).await;
    let (raw, status) = read_status_file(&path);
    assert!(raw.ends_with('\n'), "#1328 runtime status file ends newline");
    assert_eq!(
        entry_keys(&status),
        ["updatedAt", "channels"],
        "#1328 runtime status top-level order"
    );
    let entry = status
        .get("channels")
        .and_then(|channels| channels.get("weixin"))
        .expect("#1328 lifecycle entry present");
    assert_eq!(
        entry_keys(entry),
        ["channelKey", "state", "updatedAt", "message"],
        "#1328 lifecycle entry field order"
    );
    assert_eq!(field_text(entry, "state"), Some("connected".into()));
    assert_eq!(field_text(entry, "message"), Some("weixin: started".into()));
    // The adapter's own login-state reports share the sink; empty optional
    // fields are omitted and the truthy ones carry through.
    let reporter = harness
        .latest()
        .runtime_status
        .expect("#1328 adapter runtime_status wired");
    reporter(WeixinChannelStatus {
        state: WeixinChannelState::WaitingForLogin,
        message: String::new(),
        account_id: Some(JsString::from("acc-1")),
        error: None,
        qr_url: Some(JsString::from("https://qr.example/login")),
    });
    let (_, status) = read_status_file(&path);
    let entry = status
        .get("channels")
        .and_then(|channels| channels.get("weixin"))
        .expect("#1328 login entry present");
    assert_eq!(
        entry_keys(entry),
        ["channelKey", "state", "updatedAt", "accountId", "qrUrl"],
        "#1328 login entry order with empty message omitted"
    );
    assert_eq!(
        field_text(entry, "state"),
        Some("waiting_for_login".into()),
        "#1328 login state carried through"
    );
    assert_eq!(field_text(entry, "accountId"), Some("acc-1".into()));
    assert_eq!(
        field_text(entry, "qrUrl"),
        Some("https://qr.example/login".into())
    );
    harness.channels.close().await.expect("#1328 status close");
    let (_, status) = read_status_file(&path);
    let entry = status
        .get("channels")
        .and_then(|channels| channels.get("weixin"))
        .expect("#1328 stopped entry present");
    assert_eq!(
        field_text(entry, "state"),
        Some("stopped".into()),
        "#1328 close reports the stopped state"
    );
}

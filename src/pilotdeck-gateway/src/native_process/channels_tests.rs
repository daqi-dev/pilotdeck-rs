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
use crate::channel::runtime_status::{
    ChannelRuntimeStatusUpdate, channel_runtime_status_path,
    create_channel_runtime_status_reporter_locked, read_snapshot,
};
use crate::channel::weixin::{
    WeixinAbortFuture, WeixinChannelOptions, WeixinChannelState, WeixinChannelStatus,
    WeixinGatewayPort, WeixinRuntimeStatusReporter, WeixinSessionMapper, WeixinStateChange,
    WeixinTurnStream,
};
use crate::runtime::project::RuntimeError;
use crate::runtime::session::{
    LiveSessionHandle, SessionContext, SessionFactory, SessionOwner, SessionOwnerOptions,
    TurnCompletionHandle,
};
use crate::runtime::turn::coordinator::TurnEventCoordinator;
use crate::runtime::turn::projector::{
    GatewayAgentEventProjector, GatewayAgentEventProjectorOptions,
};
use crate::runtime::turn::submit::{
    PreparedTurn, SubmitError, SubmitFuture, SubmitHost, SubmitWorkspace, TurnSubmitter,
    WorkspaceAdmissions,
};
use crate::storage::RunRef;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use std::{
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
        Box::pin(async move { Ok(Arc::new(MockHandle { lifecycle }) as Arc<dyn ChannelHandle>) })
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
        feishu: crate::channel::feishu::FeishuOptions::default(),
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
        keys,
        ["requested", "requestedAt"],
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
        assert_eq!(channel.is_some(), expected, "#1328 startup gate ({config})");
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
    assert!(
        result.stopped.is_empty(),
        "#1328 enable reload stops nothing"
    );
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
    running.channels.prepare_login_callback()()
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
        _reason: Option<&'a str>,
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
    published.channels.prepare_login_callback()()
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
    fresh.channels.prepare_login_callback()()
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
    harness.channels.prepare_login_callback()()
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
    first.on_state_change.expect("#1328 on_state_change wired")(&snapshot);
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
        field_text(
            saved.get("activeByChatId").expect("#1328 active map"),
            "chat-1"
        ),
        Some("weixin:chat=chat-1:s_persisted".into()),
        "#1328 flushed active session"
    );
    assert_eq!(
        field_text(
            saved.get("projectByChatId").expect("#1328 project map"),
            "chat-1"
        ),
        Some("proj-1".into()),
        "#1328 flushed project binding"
    );
    // A second hot start restores the mapper from the persisted state.
    harness.channels.prepare_login_callback()()
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
        second
            .mapper
            .get_project(&chat)
            .map(|key| String::from_utf16_lossy(key.units())),
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
    harness.channels.prepare_login_callback()()
        .await
        .expect("#1328 status harness starts");
    harness.lifecycle.until(1).await;
    let (raw, status) = read_status_file(&path);
    assert!(
        raw.ends_with('\n'),
        "#1328 runtime status file ends newline"
    );
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

/// #1725#1: the reporter's whole read-modify-write sits inside the shared
/// lock — the TS module is synchronous on one event loop, so two concurrent
/// reports cannot interleave their read-snapshot → merge → publish stretches
/// and lose an update. Holding the lock pins the reporter entirely; a report
/// blocked on the lock writes nothing, and a file rewrite that lands while it
/// is blocked is still there after it publishes (it re-reads *after*
/// acquiring the lock). The tmp+rename publish also never leaves its scratch
/// file behind.
#[test]
fn reporter_updates_serialize_through_the_shared_lock() {
    let home = Root::new();
    let lock = Arc::new(Mutex::new(()));
    let reporter = create_channel_runtime_status_reporter_locked(home.path(), lock.clone());
    let update = |state: &str| ChannelRuntimeStatusUpdate {
        state: state.into(),
        message: None,
        account_id: None,
        error: None,
        qr_url: None,
    };
    reporter(&JsString::from("weixin"), &update("connected"));
    let path = channel_runtime_status_path(&home.path());

    // Holding the shared lock pins the reporter's whole critical section: a
    // concurrent report cannot have run its read-modify-write.
    let guard = lock.lock().expect("#1725 shared lock");
    let writer = {
        let reporter = reporter.clone();
        std::thread::spawn(move || reporter(&JsString::from("feishu"), &update("connected")))
    };
    std::thread::sleep(Duration::from_millis(100));
    let raw = crate::test_negative_receipt::expect_ok(
        std::fs::read_to_string(&path),
        "#1725 runtime status file",
    );
    crate::negative_assert!(
        !raw.contains("feishu"),
        "#1725 a report blocked on the shared lock writes nothing"
    );

    // A rewrite that lands while the report is blocked — the update the
    // report would have lost if it had read its snapshot before the lock.
    std::fs::write(
        &path,
        "{\"updatedAt\":\"blocked\",\"channels\":{\"weixin\":{\"state\":\"connected\"},\"other\":{\"state\":\"connected\"}}}\n",
    )
    .expect("#1725 concurrent status rewrite");
    drop(guard);
    writer.join().expect("#1725 the blocked report finishes");

    let (_, status) = read_status_file(&path);
    for channel in ["weixin", "other", "feishu"] {
        crate::negative_assert!(
            status
                .get("channels")
                .and_then(|channels| channels.get(channel))
                .is_some(),
            "#1725 the blocked report re-reads under the lock and keeps the concurrent rewrite: {channel}"
        );
    }
    let leftovers = std::fs::read_dir(path.parent().expect("#1725 status parent dir"))
        .expect("#1725 status dir listing")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
        .count();
    crate::negative_assert!(
        leftovers == 0,
        "#1725 the atomic status publish leaves no tmp file behind"
    );
}

// #1763 / #1386 / #1302 B: the production native startup must refuse a config
// that enables a channel this process does not implement. These drive
// `assembly::start` (the `#[cfg(test)]` facade over the same `start_impl` that
// `native_process::run` reaches through `start_with_webhook`), so deleting the
// call site, downgrading it to a warning, or turning it into a silent skip
// fails here; the seam-level tests in `channel::tests` alone would stay green.
const UNMIGRATED_REJECTION_PHRASE: &str = crate::channel::loader::UNMIGRATED_REASON;

/// The base config `management_tests::setup` writes, plus the given
/// `adapters:` body (`""` leaves the section out entirely). Starts the real
/// native assembly over it and returns the raw startup error, if any.
async fn start_with_adapters(adapters_yaml: &str) -> Result<(), crate::ProxyError> {
    let (_scratch, requests) = super::management_tests::setup();
    crate::test_negative_receipt::expect_ok(
        requests.project.dispose().await,
        "#1763 dispose unused test registry",
    );
    crate::test_negative_receipt::expect_ok(
        requests.owner.shutdown().await,
        "#1763 shutdown unused test owner",
    );
    let config = requests.options.home.join("pilotdeck.yaml");
    let mut body = crate::test_negative_receipt::expect_ok(
        std::fs::read_to_string(&config),
        "#1763 base config readable",
    );
    if !adapters_yaml.is_empty() {
        body.push_str("adapters:\n");
        body.push_str(adapters_yaml);
    }
    crate::test_negative_receipt::expect_ok(
        std::fs::write(&config, body),
        "#1763 config with adapters",
    );
    match super::assembly::start(requests.options.clone(), requests.env.clone()).await {
        Ok(host) => {
            crate::test_negative_receipt::expect_ok(
                host.shutdown(requests.options.drain_budget).await,
                "#1763 started host stopped",
            );
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn startup_error(result: Result<(), crate::ProxyError>, assertion: &'static str) -> String {
    crate::test_negative_receipt::expect_err(result, assertion).to_string()
}

#[tokio::test]
async fn startup_refuses_every_unmigrated_channel() {
    for key in crate::channel::loader::UNMIGRATED_CHANNEL_KEYS {
        let error = startup_error(
            start_with_adapters(&format!("  {key}: {{enabled: true}}\n")).await,
            "#1763 enabled unmigrated channel must refuse native startup",
        );
        crate::negative_assert!(
            error.contains(&format!("channel \"{key}\"")),
            "#1763 startup error names the enabled channel key"
        );
        crate::negative_assert!(
            error.contains(UNMIGRATED_REJECTION_PHRASE),
            "#1763 startup error carries the unmigrated phrase"
        );
    }
}

// #1763 review P1: `main` is `async fn main() -> Result<(), ProxyError>`, and
// std's `Termination for Result` prints `Error: {e:?}` — the Debug form, not
// the Display form the seam returns. The ledger entries must describe what an
// operator actually sees, so the exact stderr line is pinned here (measured
// from a minimal `Termination` probe in the #1763 PR). This test does not
// itself write to process stderr: it replays the one line std prints for the
// `main` signature above (`main.rs` returns `Result<(), ProxyError>`, which
// `native_process::run` propagates unchanged). Changing that signature or
// wrapping the error before returning it would invalidate the pinned text.
#[tokio::test]
async fn startup_rejection_reaches_stderr_in_the_debug_form() {
    let error = crate::test_negative_receipt::expect_err(
        start_with_adapters("  telegram: {enabled: true}\n").await,
        "#1763 enabled unmigrated channel must refuse native startup",
    );
    let printed = format!("Error: {error:?}");
    crate::negative_assert_eq!(
        printed,
        "Error: Error(\"[adapters] channel \\\"telegram\\\" 未迁移到 Rust，需继续使用 TS gateway\")",
        "#1763 the process stderr line is the Debug form of ChannelError"
    );
}

// #1694 §7 / #1329: `feishu` used to be a bundle key outside the unmigrated
// table with no native implementation, so enabling it was rejected. The
// constructor landed with #1329, so the same config must now start (a
// credential-less channel logs a warning and stays inert). The gate itself
// still exists — a bundle key listing without a constructor fails
// `channel::tests::implemented_bundle_keys_are_exactly_the_assembled_bundle_keys`.
// #1694 §7: the declaration is closed in both directions — every bundle key
// has a constructor (an enabled key that is missing here is refused at
// startup) and no constructor exists for a key the loader does not own. This
// is the cell that kills dropping `feishu` from `BUNDLE_CHANNEL_KEYS`.
#[test]
fn bundle_keys_match_the_assembly() {
    let mut implemented = crate::native_process::channels::IMPLEMENTED_BUNDLE_CHANNEL_KEYS.to_vec();
    implemented.sort_unstable();
    let mut bundle = crate::channel::loader::BUNDLE_CHANNEL_KEYS.to_vec();
    bundle.sort_unstable();
    crate::negative_assert_eq!(
        implemented,
        bundle,
        "#1763 the assembly constructs exactly the bundle keys the loader owns"
    );
}

#[tokio::test]
async fn startup_builds_the_enabled_feishu_channel() {
    crate::test_negative_receipt::expect_ok(
        start_with_adapters("  feishu: {enabled: true}\n").await,
        "#1329 enabled feishu must start native assembly",
    );
}

// #1763 review P2: production feeds the parsed snapshot, where
// `config/snapshot.rs`'s `bool_field` has already folded `enabled` to a bool.
// Non-boolean spellings therefore read `false` here and do not reject. TS's
// own `parseGatewayConfig.booleanField` folds the same way (`channel/loader.rs`
// records that the two gates cannot be told apart on a real config file), so
// this is the shared snapshot behavior, not a Rust-side divergence — pinned
// so it cannot drift unnoticed (the seam-level truthiness contract over raw
// objects stays in `channel::tests`).
#[tokio::test]
async fn startup_folds_non_boolean_enabled_spellings() {
    for spelling in ["1", "\"true\"", "\"1\"", "[]", "{}"] {
        crate::test_negative_receipt::expect_ok(
            start_with_adapters(&format!("  telegram: {{enabled: {spelling}}}\n")).await,
            "#1763 non-boolean enabled folds to false in the config snapshot",
        );
    }
}

// The control grid: the refusal must not misfire on configs TS starts with.
#[tokio::test]
async fn startup_accepts_configs_without_an_unmigrated_channel() {
    crate::test_negative_receipt::expect_ok(
        start_with_adapters("").await,
        "#1763 a config with no adapters starts",
    );
    let mut disabled = String::new();
    for key in crate::channel::loader::UNMIGRATED_CHANNEL_KEYS {
        disabled.push_str(&format!("  {key}: {{enabled: false}}\n"));
    }
    crate::test_negative_receipt::expect_ok(
        start_with_adapters(&disabled).await,
        "#1763 disabled unmigrated channels start",
    );
    // `weixin` is implemented, so the gate must not reject it. The enabled
    // case is not driven through `start`: the real adapter would dial the QR
    // login endpoint (#1328 covers its construction through the
    // `weixin_maker` seam), and that is not something a unit test may do.
    crate::test_negative_receipt::expect_ok(
        start_with_adapters("  weixin: {enabled: false}\n").await,
        "#1763 a disabled implemented bundle channel starts",
    );
}

// --------------------------------------------------------------- #2301 loader

// #2301: the production wiring closes the #1853 silent skip. Every loader key
// that has left the unmigrated rejection table must have a mount in
// `production_loader_mounts`, or an enabled config for it would construct
// nothing — no rejection, no adapter, no TS fallback. The reverse direction
// is structural: `load_enabled_channels` iterates `CHANNEL_LOADER_KEYS`, so
// a mount for a key outside the table can never be reached, and the pin in
// `channel::tests::loader_keys_partition_rejected_and_implemented` keeps the
// implemented set identical to the non-rejected table tail.
#[test]
fn production_mounts_cover_every_non_rejected_loader_key() {
    let mounts = super::channels::production_loader_mounts();
    for key in crate::channel::loader::CHANNEL_LOADER_KEYS {
        if crate::channel::loader::UNMIGRATED_CHANNEL_KEYS.contains(&key) {
            continue;
        }
        crate::negative_assert!(
            mounts.is_registered(key),
            "#2301 loader key \"{key}\" is neither rejected nor mounted in production"
        );
    }
}

/// A started host whose channels stay alive: `start_channels` runs each
/// key's startup in the background, so the observable effects (runtime-status
/// entries, listener ports) land only while the host runs. The caller stops
/// the host through [`AliveAssembly::stop`], which also releases the scratch
/// directory.
struct AliveAssembly {
    /// Keeps the test registry directory until the host has shut down.
    _scratch: super::management_tests::Scratch,
    home: PathBuf,
    host: crate::runtime::turn::gateway::TurnGateway,
    drain_budget: Duration,
}

impl AliveAssembly {
    async fn stop(self) {
        crate::test_negative_receipt::expect_ok(
            self.host.shutdown(self.drain_budget).await,
            "#2301 started host stopped",
        );
    }
}

/// [`start_with_adapters`] with the started host kept alive and its home
/// directory handed back (the runtime-status sink's root).
async fn start_with_adapters_alive(adapters_yaml: &str) -> AliveAssembly {
    let (scratch, requests) = super::management_tests::setup();
    crate::test_negative_receipt::expect_ok(
        requests.project.dispose().await,
        "#2301 dispose unused test registry",
    );
    crate::test_negative_receipt::expect_ok(
        requests.owner.shutdown().await,
        "#2301 shutdown unused test owner",
    );
    let config = requests.options.home.join("pilotdeck.yaml");
    let mut body = crate::test_negative_receipt::expect_ok(
        std::fs::read_to_string(&config),
        "#2301 base config readable",
    );
    body.push_str("adapters:\n");
    body.push_str(adapters_yaml);
    crate::test_negative_receipt::expect_ok(
        std::fs::write(&config, body),
        "#2301 config with adapters",
    );
    let host = crate::test_negative_receipt::expect_ok(
        super::assembly::start(requests.options.clone(), requests.env.clone()).await,
        "#2301 assembly start",
    );
    AliveAssembly {
        _scratch: scratch,
        home: requests.options.home.clone(),
        host,
        drain_budget: requests.options.drain_budget,
    }
}

/// The closed-port judgment the fixed-port watcher and
/// [`assert_ports_closed_after_stop`] share (#2462): of everything a
/// connect can answer with, only `ConnectionRefused` proves the port has
/// no listener — the kernel's own refusal (Node v22.22.0's own connect
/// surfaces the same fact as `code: 'ECONNREFUSED', errno: -61`). Every
/// other outcome leaves the port in an unknown state and is carried to the
/// caller so it can fail — or keep waiting — by name: the old
/// `connect().is_err()` read every error kind as "closed", so an
/// fd-exhausted or timing-out probe would "prove" a live listener gone,
/// a false PASS.
enum FixedPortProbe {
    /// The kernel refused the connection: nothing is listening.
    Refused,
    /// A listener accepted the connection: the port is still open.
    Accepted,
    /// Any other error kind, carried verbatim (kind and raw errno) for the
    /// named failure.
    Unknown(std::io::Error),
}

/// Classify one connect outcome for a claimed fixed port. Constructed kinds
/// classify here too, which is how the unit control drives EMFILE and a
/// timed-out probe without exhausting real fds (#2462).
fn classify_fixed_port_probe(probe: Result<(), std::io::Error>) -> FixedPortProbe {
    match probe {
        Ok(()) => FixedPortProbe::Accepted,
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            FixedPortProbe::Refused
        }
        Err(error) => FixedPortProbe::Unknown(error),
    }
}

/// The named failure [`assert_ports_closed_after_stop`] reports for a probe
/// outcome that is not a refusal (#2462): a port that still accepts
/// connections, or a probe whose error kind proves nothing — with its kind
/// and raw errno in the message, never a pass.
fn fixed_port_close_failure(port: u16, probe: FixedPortProbe) -> Option<String> {
    match probe {
        FixedPortProbe::Refused => None,
        FixedPortProbe::Accepted => Some(format!(
            "#2462 the fixed-port listener on port {port} is still accepting connections after stop"
        )),
        FixedPortProbe::Unknown(error) => Some(format!(
            "#2462 the close probe on port {port} answered {error} (kind {:?}, raw os error \
             {:?}) — only ConnectionRefused proves a port closed, so this is a named failure, \
             not a pass",
            error.kind(),
            error.raw_os_error()
        )),
    }
}

/// One bounded probe of a claimed fixed port — the watcher's poll step
/// (#2462): the sync sibling of [`assert_ports_closed_after_stop`]'s probe,
/// through the same [`FixedPortProbe`] judgment.
fn probe_fixed_port(port: u16) -> FixedPortProbe {
    classify_fixed_port_probe(
        std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_millis(250),
        )
        .map(|_| ()),
    )
}

/// The cap the fixed-port watcher runs with in production (#2461): the
/// ledger's 120-second bound, stated once here so the control can inject a
/// short deadline for the cap arm.
const FIXED_PORT_WATCHER_CAP: Duration = Duration::from_secs(120);

/// The watcher body behind [`FixedPortRegion::drop`], factored out so the
/// cap arm is observable (#2461): poll the claimed ports until every one of
/// them refuses connections, then release the flock files. Reaching `cap`
/// with a port still not refusing connections used to release silently — the
/// exact arm #2461 exists for — so it reports itself through `emit` first
/// (the detached watcher writes stderr, best-effort; the control below
/// records the line), keeping "all closed or the 120s cap" attributable on
/// both arms. Every wait states its termination condition: the loop ends at
/// all-refused or at `cap`.
fn release_fixed_port_files(
    ports: &[u16],
    files: Vec<std::fs::File>,
    cap: Duration,
    emit: &mut dyn FnMut(&str),
) {
    let deadline = std::time::Instant::now() + cap;
    loop {
        let probes: Vec<(u16, FixedPortProbe)> = ports
            .iter()
            .map(|port| (*port, probe_fixed_port(*port)))
            .collect();
        if probes
            .iter()
            .all(|(_, probe)| matches!(probe, FixedPortProbe::Refused))
        {
            break;
        }
        if std::time::Instant::now() > deadline {
            emit(&fixed_port_timeout_diagnostic(&probes, cap));
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(files);
}

/// The named diagnostic the cap arm emits before releasing (#2461): the cap
/// itself (120s in production) and every claimed port that is not refusing
/// connections at the expiry, each with its probe outcome.
fn fixed_port_timeout_diagnostic(probes: &[(u16, FixedPortProbe)], cap: Duration) -> String {
    let not_refused: Vec<String> = probes
        .iter()
        .filter_map(|(port, probe)| match probe {
            FixedPortProbe::Refused => None,
            FixedPortProbe::Accepted => Some(format!("{port}: still accepting connections")),
            FixedPortProbe::Unknown(error) => Some(format!("{port}: probe not a refusal: {error}")),
        })
        .collect();
    format!(
        "#2461 the fixed-port watcher hit its {cap:?} cap with a port still not refusing \
         connections — releasing the flock now; not refused: [{}]",
        not_refused.join("; ")
    )
}

/// #2301 fix-r2 (review P0): an exclusive flock over the fixed ports a test
/// claims, held from before the pre-probe until the after-stop assertion
/// (the guard drops when the test does). `flock` semantics, not a mkdir+pid
/// lock: the kernel releases the lock when the holder dies, so a crashed run
/// never wedges the port, and a holder that stays alive without finishing
/// surfaces here as the named 120-second timeout. Conforming runs of this
/// suite serialize on the lock file instead of racing for the port between
/// the probe and the channel's bind; a squatter outside this suite still
/// fails the pre-probe by name.
///
/// #2301 fix-r3 (review P0 ② / DS concurrency#6): the release is ordered
/// after the listeners close, not merely after the test body returns. A panic
/// unwinds this guard before the tokio runtime drops the channel listeners,
/// which used to leave a window where the lock was free above an open
/// listener — a sibling run's pre-probe then failed by name (fail-closed, but
/// not the "held throughout" the ledger describes). [`Drop`] now hands the
/// flock files to a bounded watcher that releases them only once every
/// claimed port stopped accepting connections, and the two assembly tests
/// hold their regions inside a re-exec'd child process, where the kernel
/// closes the listeners and the lock in the same process teardown.
///
/// #2461 (codex P1#3 + DS boundary#2): the two silent arms of that watcher
/// are observable now. Reaching the 120-second cap with a port still not
/// refusing connections emits the named diagnostic first — best-effort
/// stderr, a failed write is dropped, never a panic — and only then
/// releases; and the watcher thread is a `Builder::spawn` whose failure
/// falls back to the synchronous release instead of panicking, because this
/// [`Drop`] also runs inside panic unwinds, where the bare
/// `std::thread::spawn`'s panic on failure would abort the whole test
/// binary.
struct FixedPortRegion {
    ports: Vec<u16>,
    files: Vec<std::fs::File>,
}

impl Drop for FixedPortRegion {
    fn drop(&mut self) {
        let files = std::mem::take(&mut self.files);
        if files.is_empty() {
            return;
        }
        let ports = self.ports.clone();
        // The flock files travel through a bounded hand-off (#2461) so the
        // failure arm below can still release them explicitly: handing them
        // straight to the closure would leave their fate to std dropping the
        // closure that a failed `spawn` never runs. The async channel's
        // `send` never blocks, so this adds no wait to an unwind.
        let (hand_off, take_over) = std::sync::mpsc::channel::<Vec<std::fs::File>>();
        // Bounded (120s) and detached: on the normal path the after-stop
        // assertions already proved the ports closed, so the first check
        // releases immediately. A `Builder`, not the bare
        // `std::thread::spawn` (#2461, DS concurrency P3#4): this Drop also
        // runs inside panic unwinds, where the bare spawn's panic on failure
        // would be a second panic and abort the whole test binary.
        let spawned = std::thread::Builder::new()
            .name("pdrs-2461-fixed-port-watch".to_owned())
            .spawn(move || {
                // `recv` fails only once the dropping side is gone — it
                // released the files itself on that path, so there is
                // nothing left to do here.
                if let Ok(files) = take_over.recv() {
                    release_fixed_port_files(&ports, files, FIXED_PORT_WATCHER_CAP, &mut |line| {
                        // Best-effort by contract (#2461): `eprintln!`
                        // panics on a failed write, which would take the
                        // watcher down mid-release; the line is dropped
                        // instead.
                        let _ = writeln!(std::io::stderr(), "{line}");
                    });
                }
            });
        match spawned {
            Ok(_watcher) => {
                // The watcher owns the release ordering from here. `send`
                // fails only if the watcher exited before receiving; the
                // files then drop with the returned error, stated out loud
                // instead of left to a discarded value.
                if let Err(returned) = hand_off.send(files) {
                    drop(returned.0);
                }
            }
            Err(_not_started) => {
                // #2461: no watcher thread (resource exhaustion is the
                // realistic cause, plausibly mid-panic-unwind) — release
                // synchronously here instead of panicking, and accept the
                // degraded ordering: the claim pre-probe keeps a sibling run
                // fail-closed on the freed-before-closed window.
                drop(files);
            }
        }
    }
}

/// Claims the fixed ports for a test: the exclusive flock first (conforming
/// runs of this suite serialize instead of racing for the port between the
/// probe and the channel's bind), then the bind pre-probe itself — the
/// A8.3-CH-SMS-012 convention: a port another run or a stray listener already
/// owns fails here by name instead of turning into a bind failure the
/// production channels report as a plain error (#2301 fix-r3 review P0 ③:
/// both steps of the old `lock_fixed_port_region` + inline probe pair
/// collapsed into this one entry point).
async fn claim_fixed_ports(ports: &[u16]) -> FixedPortRegion {
    let mut files = Vec::new();
    for port in ports {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(format!("/tmp/pdrs-2301-fixed-port-{port}.lock"))
            .unwrap_or_else(|error| panic!("#2301 port {port} lock file: {error}"));
        // Every wait states its termination condition: 120 seconds on the
        // region, polled non-blocking so the failure is a named one.
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            // `File::try_lock` is `flock(LOCK_EX | LOCK_NB)` on Unix — the
            // stable std surface of the same kernel lock the workspace's
            // `unsafe_code = "forbid"` boundary (#448) lets a test reach.
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(error) => panic!("#2301 port {port} flock: {error}"),
            }
            if std::time::Instant::now() > deadline {
                crate::negative_assert!(
                    false,
                    "#2301 the fixed-port region for port {port} is still held by \
                     another run after 120s — rerun when the shared host is idle"
                );
                unreachable!("negative_assert! panics");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        files.push(file);
    }
    for port in ports {
        let probe = tokio::net::TcpListener::bind(("127.0.0.1", *port)).await;
        crate::negative_assert!(
            probe.is_ok(),
            "#2301 fixed-port test port {port} is already occupied — another run of this \
             suite (or a stray listener) owns it; rerun when the shared host is idle"
        );
        drop(probe);
    }
    FixedPortRegion {
        ports: ports.to_vec(),
        files,
    }
}

/// The after-stop half of the fixed-port convention (#2301 fix-r3 review
/// P0 ③): every listener this run built must be gone once the host stopped,
/// or a listener that outlives the assembly would keep answering from another
/// run's channel. #2462: "gone" is a `ConnectionRefused` answer and nothing
/// else — the same [`FixedPortProbe`] judgment the watcher uses — and the
/// connect itself is bounded by the same 5 seconds `assert_health_platform`
/// uses, so a probe that never completes fails by name instead of hanging
/// the suite.
async fn assert_ports_closed_after_stop(ports: &[u16]) {
    for port in ports {
        let probe = match tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::TcpStream::connect(("127.0.0.1", *port)),
        )
        .await
        {
            Ok(connected) => connected.map(|_| ()),
            Err(_elapsed) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "#2462 the 5-second close-probe bound elapsed",
            )),
        };
        if let Some(failure) = fixed_port_close_failure(*port, classify_fixed_port_probe(probe)) {
            crate::negative_assert!(false, "{failure}");
        }
    }
}

/// #2461 control: the cap arm must be observable. A live listener plus an
/// injected short cap can only end the watcher's loop through the cap arm,
/// so the release must first emit exactly one named diagnostic — the cap and
/// the not-refused port — and really release (a fresh handle can take the
/// same flock). The listener is an ephemeral port, no fixture port claimed.
#[test]
fn fixed_port_watcher_cap_releases_through_the_named_diagnostic() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("#2461 the cap control's probe listener binds");
    let port = listener
        .local_addr()
        .expect("#2461 the cap control's listener address")
        .port();
    let path = std::env::temp_dir().join(format!(
        "pdrs-2461-cap-{}-{}.lock",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .expect("#2461 the cap control's lock file opens");
    file.try_lock()
        .expect("#2461 the cap control's lock file locks");
    let mut emitted = Vec::new();
    let cap = Duration::from_millis(300);
    release_fixed_port_files(&[port], vec![file], cap, &mut |line| {
        emitted.push(line.to_owned());
    });
    crate::negative_assert_eq!(
        emitted.len(),
        1,
        "#2461 the cap arm emits exactly one named diagnostic"
    );
    let diagnostic = &emitted[0];
    crate::negative_assert!(
        diagnostic.contains("#2461") && diagnostic.contains(&format!("{cap:?}")),
        "#2461 the cap diagnostic names the watcher cap"
    );
    crate::negative_assert!(
        diagnostic.contains(&port.to_string()),
        "#2461 the cap diagnostic names the not-refused port"
    );
    let reclaimed = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("#2461 the cap control's lock file reopens");
    let relocked = reclaimed.try_lock();
    let _ = std::fs::remove_file(&path);
    crate::negative_assert!(
        relocked.is_ok(),
        "#2461 the flock is released after the cap diagnostic"
    );
}

/// #2461 control: the all-refused arm stays silent — a region whose every
/// port refuses connections releases immediately, with no diagnostic at all
/// (the diagnostic exists only for the cap arm). The port is a
/// bind-then-drop ephemeral one, so nothing can be listening on it.
#[test]
fn fixed_port_watcher_releases_silently_once_every_port_refuses() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("#2461 the silence control's probe listener binds");
    let port = listener
        .local_addr()
        .expect("#2461 the silence control's listener address")
        .port();
    drop(listener);
    let mut emitted = Vec::new();
    let started = std::time::Instant::now();
    release_fixed_port_files(&[port], Vec::new(), Duration::from_secs(10), &mut |line| {
        emitted.push(line.to_owned());
    });
    crate::negative_assert!(
        emitted.is_empty() && started.elapsed() < Duration::from_secs(2),
        "#2461 the all-refused release is immediate and silent"
    );
}

/// #2461 control: the release ordering itself. Dropping a region whose port
/// still accepts connections must leave the flock held — only the watcher
/// holds it now, and its loop cannot release while the port answers — and
/// once the listener closes, the watcher must release within its poll
/// cadence. Every wait states its termination condition: 10 seconds, far
/// over the 250ms probe + 50ms cadence.
#[test]
fn fixed_port_region_releases_only_after_its_listener_closes() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("#2461 the ordering control's listener binds");
    let port = listener
        .local_addr()
        .expect("#2461 the ordering control's listener address")
        .port();
    let path = std::env::temp_dir().join(format!(
        "pdrs-2461-order-{}-{}.lock",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .expect("#2461 the ordering control's lock file opens");
    file.try_lock()
        .expect("#2461 the ordering control's lock file locks");
    drop(FixedPortRegion {
        ports: vec![port],
        files: vec![file],
    });
    let probe = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("#2461 the ordering control's lock file reopens");
    crate::negative_assert!(
        matches!(probe.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "#2461 the flock is still held while the listener accepts connections"
    );
    drop(probe);
    drop(listener);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let released = loop {
        let reclaimed = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("#2461 the ordering control's lock file reopens");
        if reclaimed.try_lock().is_ok() {
            break true;
        }
        if std::time::Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = std::fs::remove_file(&path);
    crate::negative_assert!(
        released,
        "#2461 the watcher releases the flock once the listener closes"
    );
}

/// #2462 control: only a refusal proves a port closed. Constructed error
/// kinds — an fd-exhaustion `EMFILE` (24), the 5-second bound's own
/// `TimedOut` — go through the same judgment the after-stop assertion and
/// the watcher use, without exhausting real fds; each must be a named
/// failure carrying its kind/errno, never a pass.
#[test]
fn only_a_refusal_proves_the_fixed_port_closed() {
    crate::negative_assert!(
        fixed_port_close_failure(
            28645,
            classify_fixed_port_probe(Err(std::io::Error::from(
                std::io::ErrorKind::ConnectionRefused
            )))
        )
        .is_none(),
        "#2462 a refused probe is the only shape that proves the port closed"
    );
    let accepted = crate::test_negative_receipt::expect_some(
        fixed_port_close_failure(28645, classify_fixed_port_probe(Ok(()))),
        "#2462 an accepted connect is a named failure",
    );
    crate::negative_assert!(
        accepted.contains("#2462") && accepted.contains("still accepting connections"),
        "#2462 an accepted connect names the still-open listener, got: {accepted}"
    );
    let exhausted = crate::test_negative_receipt::expect_some(
        fixed_port_close_failure(
            28645,
            classify_fixed_port_probe(Err(std::io::Error::from_raw_os_error(24))),
        ),
        "#2462 a non-refused error kind is a named failure",
    );
    crate::negative_assert!(
        exhausted.contains("#2462") && exhausted.contains("24"),
        "#2462 the fd-exhaustion shape carries its raw os error, got: {exhausted}"
    );
    let timed_out = crate::test_negative_receipt::expect_some(
        fixed_port_close_failure(
            28645,
            classify_fixed_port_probe(Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "#2462 the 5-second close-probe bound elapsed",
            ))),
        ),
        "#2462 a timed-out probe is a named failure",
    );
    crate::negative_assert!(
        timed_out.contains("#2462") && timed_out.contains("TimedOut"),
        "#2462 the timed-out shape carries its kind, got: {timed_out}"
    );
}

/// #2301 fix-r3 (review P2-a #7): a named pre-check for host environment
/// values that would divert a channel's auth or bind behavior away from the
/// assertions (`API_SERVER_KEY` 401s the chat completion before `normalize`
/// runs, `API_SERVER_HOST` moves the bind off 127.0.0.1 so the readiness
/// line never matches). Shared by the identity test and the startup test's
/// child body; `startup_skips_*` needs no pre-flight of its own — its
/// apiServer key throws inside the constructor before any env read.
fn require_env_unset(names: &[&str]) {
    for name in names {
        crate::negative_assert!(
            std::env::var_os(name).is_none(),
            "#2301 this test requires {name} unset — a host environment value \
             would divert the channel's auth/bind behavior away from the assertions"
        );
    }
}

/// The child-probe marker (#2301 fix-r3 review P1 #3): a probe parent passes
/// a fresh random token per spawn both as this env value (`<mode>:<token>`)
/// and as a positional libtest filter. A process takes the probe branch only
/// when both halves match — a stale marker left in some earlier run's
/// environment cannot match this run's argv, so the harness itself can never
/// be steered into a probe branch. A positional filter was chosen over a
/// flag because libtest rejects unknown `--flag`s outright (verified on this
/// toolchain), while an unknown positional filter merely matches no test.
const CHILD_PROBE_ENV: &str = "PDRS_2301_CHILD_PROBE";

/// The probe branch for `mode`, taken only by the re-exec'd child this run
/// spawned (both the env value and the argv nonce must name the same token).
fn child_probe_token(mode: &str) -> Option<String> {
    let value = std::env::var_os(CHILD_PROBE_ENV)?;
    let value = value.to_str()?;
    let (probe_mode, token) = value.split_once(':')?;
    if probe_mode != mode || token.is_empty() {
        return None;
    }
    std::env::args()
        .any(|argument| argument == token)
        .then(|| token.to_owned())
}

/// The environment a probe child inherits: everything the parent test ran
/// with — the receipt protocol included, so the child's named assertions
/// still write their receipts under this test's identity — plus this spawn's
/// token pair.
fn probe_child_env(mode: &str, token: &str) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    crate::test_negative_receipt::forward_to_worker(&mut env);
    env.retain(|(key, _)| key != CHILD_PROBE_ENV);
    env.push((CHILD_PROBE_ENV.to_owned(), format!("{mode}:{token}")));
    env
}

/// The last 4 KiB of a probe child's console, for embedding in the parent's
/// named failure messages — the child's own panic payload lands there, so
/// the fixture anchors stay killable through the parent's failure line.
/// #2463: the window is cut on byte offsets, and a probe console carries
/// multi-byte text (the readiness lines' own `— routes: …`), so the cut can
/// land inside a character. `get` returns `None` exactly there, and the
/// named fallback keeps the parent's message printable — the tail omitted
/// with this line — instead of panicking on a non-boundary index.
fn console_tail(console: &str) -> String {
    let start = console.len().saturating_sub(4096);
    match console.get(start..) {
        Some(tail) => tail.to_owned(),
        None => "#2463 the 4096-byte console tail cut landed inside a multi-byte \
                 character; the tail is omitted"
            .to_owned(),
    }
}

/// #2463 control: a console whose 4096-byte tail cut lands inside the
/// readiness line's em dash — the multi-byte text the probe consoles really
/// carry (`— routes: …`, U+2014, 3 bytes; Node v22.22.0 probe) — must not
/// panic the parent's message builder, and the fallback must be the named
/// diagnostic line. The sample's cut position is asserted before the call, so
/// a drifted sample cannot silently test a boundary-aligned cut.
#[test]
fn console_tail_cuts_inside_a_multibyte_character_without_panicking() {
    let readiness = "webhook: listening on http://127.0.0.1:28643 — routes: (none configured)\n";
    let dash = readiness
        .find('—')
        .expect("#2463 the readiness sample carries its multi-byte separator");
    // With the suffix sized below, the cut index lands one byte inside the
    // em dash's UTF-8 bytes — never a char boundary.
    let fill = 512;
    let suffix = 4096 + dash + 1 - readiness.len();
    let mut console = "x".repeat(fill);
    console.push_str(readiness);
    console.push_str(&"y".repeat(suffix));
    let start = console.len().saturating_sub(4096);
    crate::negative_assert!(
        !console.is_char_boundary(start),
        "#2463 the sample's 4 KiB cut lands inside the multi-byte separator"
    );
    let tail = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| console_tail(&console)));
    let tail = match tail {
        Ok(tail) => tail,
        Err(_) => {
            crate::negative_assert!(
                false,
                "#2463 console_tail must not panic when the 4 KiB cut lands inside a character"
            );
            unreachable!("negative_assert! panics");
        }
    };
    crate::negative_assert!(
        tail.contains("#2463"),
        "#2463 the uncuttable tail falls back to the named diagnostic line"
    );
}

/// #2463 control: the boundary-aligned shapes keep the exact contract — the
/// last 4096 bytes, and the whole console when it fits.
#[test]
fn console_tail_takes_the_last_4096_bytes() {
    let console = format!("head{}", "z".repeat(4096));
    crate::negative_assert_eq!(
        console_tail(&console),
        "z".repeat(4096),
        "#2463 the tail of a boundary-aligned console is its last 4096 bytes"
    );
    crate::negative_assert_eq!(
        console_tail("short console"),
        "short console".to_owned(),
        "#2463 a console under 4 KiB is returned whole"
    );
}

/// Parent half of a re-exec'd startup probe (#2301 fix-r3 review P0 #1): the
/// whole claim → probe → start → stop → after-stop stretch now runs inside
/// the child process, so its flock and its listeners are released by the
/// same kernel teardown, and the instance-binding proof moves to the child's
/// console — each channel's readiness line (`api-server: listening on …` and
/// siblings) is printed by the channel itself only after its bind succeeded,
/// so a squatter that grabs the port inside the probe → bind window (the
/// assembly reports the bind failure as a plain error line and answers
/// `/health` with its own platform body) can no longer turn the run green:
/// the missing readiness line fails by name. Every wait states its
/// termination condition, and a timed-out child is killed and reaped before
/// the named failure (P1 #4: a dropped tokio `Child` keeps running).
async fn run_startup_probe_child(test: &str, mode: &str, readiness: &[String]) {
    let token = format!("pdrs-2301-{}", uuid::Uuid::new_v4().simple());
    let mut child =
        tokio::process::Command::new(std::env::current_exe().expect("#2301 the probe binary path"))
            .args([
                "--exact",
                // `--exact` matches the full libtest path, not the bare name.
                test,
                // libtest's capture would route console writes into its in-memory
                // sink instead of the child's stdout, hiding the readiness lines
                // this test exists to observe; the token doubles as a positional
                // filter that matches no test.
                "--nocapture",
                "--test-threads=1",
                &token,
            ])
            .env_clear()
            .envs(probe_child_env(mode, &token))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("#2301 the startup probe child spawns");
    let mut stdout = child.stdout.take().expect("#2301 the child's stdout pipe");
    let mut stderr = child.stderr.take().expect("#2301 the child's stderr pipe");
    let out = tokio::spawn(async move {
        let mut buffer = Vec::new();
        stdout
            .read_to_end(&mut buffer)
            .await
            .expect("#2301 the child's stdout stays readable");
        buffer
    });
    let err = tokio::spawn(async move {
        let mut buffer = Vec::new();
        stderr
            .read_to_end(&mut buffer)
            .await
            .expect("#2301 the child's stderr stays readable");
        buffer
    });
    // Every wait states its termination condition: 240 seconds on the child
    // (a full assembly start plus its bounded polls), then kill + reap
    // before the named failure.
    let exited = tokio::time::timeout(Duration::from_secs(240), child.wait()).await;
    let (status, console) = match exited {
        Ok(status) => {
            let out_buffer = out.await.expect("#2301 the child's stdout drains");
            let err_buffer = err.await.expect("#2301 the child's stderr drains");
            let console = format!(
                "{}{}",
                String::from_utf8_lossy(&out_buffer),
                String::from_utf8_lossy(&err_buffer)
            );
            (
                status.expect("#2301 the startup probe child is waitable"),
                console,
            )
        }
        Err(_) => {
            child
                .kill()
                .await
                .expect("#2301 the timed-out startup probe child is killed");
            let _ = child.wait().await;
            let out_buffer = out.await.expect("#2301 the killed child's stdout drains");
            let err_buffer = err.await.expect("#2301 the killed child's stderr drains");
            let console = format!(
                "{}{}",
                String::from_utf8_lossy(&out_buffer),
                String::from_utf8_lossy(&err_buffer)
            );
            crate::negative_assert!(
                false,
                "#2301 the startup probe child did not finish within 240s; \
                 child output tail: {}",
                console_tail(&console)
            );
            unreachable!("negative_assert! panics");
        }
    };
    for line in readiness {
        crate::negative_assert!(
            console.contains(line.as_str()),
            "#2301 this assembly's readiness line {:?} never appeared on the \
             startup console — the channel either failed to bind its port or never \
             started; child exit {status}, child output tail: {}",
            line,
            console_tail(&console)
        );
    }
    crate::negative_assert!(
        status.success(),
        "#2301 the startup probe child must pass its own assembly assertions; \
         child exit {status}, child output tail: {}",
        console_tail(&console)
    );
}

// #2301: the production startup constructs the configured loader channels
// (pilotdeckServer.ts:288-296 through `loadEnabledChannels`): every enabled
// implemented key reaches the channel server and gets its lifecycle entry —
// including the keys whose real credentials are absent, whose `start` logs
// the visible failure line and returns the missing-config handle, the TS
// error-then-continue startup shape (`sms: twilio not installed…`,
// `slack: SLACK_BOT_TOKEN not set`). The two listener channels own their
// configured ports for real. The ports are fixed rather than the 8642 /
// 8770 defaults so a stray listener from the environment cannot make the
// assertions pass against something else.
//
// #2301 fix-r1 (review P0): a fixed port has no built-in ownership, and a
// bind failure reports the same `connected` entry as a real listener (the
// noop handle), so two parallel cargo runs of this suite could measure each
// other's listeners. The A8.3-CH-SMS-012 fixed-port convention applies: probe
// the port first and fail with a named error, then prove the listener is a
// PilotDeck channel of the right kind — `GET /health` must answer the
// channel's own `{"status":"ok","platform":…}` body (the
// `health-and-routing` oracle cell), not just accept a TCP connection. The
// residual window between probe and the channel's bind can still lose the
// port to another run; that turns into a named failure here or at the
// after-stop assertion, never a silent pass.
//
// #2301 fix-r2 (review P0): the `/health` platform body is byte-identical
// across runs, so the r1 proof only showed the listener is *a* PilotDeck
// channel of the right kind — another run's listener answers it just as
// well. The window itself is now closed for conforming runs: every
// fixed-port test holds an exclusive cross-process flock over its ports for
// the whole probe → start → stop → after-stop stretch (see
// [`claim_fixed_ports`]), so two parallel runs serialize instead of
// racing for the port. A squatter that is not this suite still either fails
// the pre-probe by name or (when it grabs the port inside the window) turns
// the `/health` probe into the named timeout below, never a silent pass.
//
// #2301 fix-r3 (review P0 #1): the `/health` body is byte-identical, and the
// after-stop check passes against any listener still on the port — so a
// squatter that lands inside the window makes the run *green* on someone
// else's listener (the r1/r2 residual: the named failures only cover
// conforming runs and squatters outside the window). The test is now driven
// through a re-exec'd child process (see [`run_startup_probe_child`]): the
// whole flock → probe → start → stop → after-stop stretch and the
// instance-binding proof live in the child, where the kernel closes the
// listeners and releases the lock in one teardown, and the parent adds the
// readiness lines over the child's console — the one observable a squatter
// cannot fake, because the channel prints it only after its own bind
// succeeded. The wecomCallback key pins an explicit port too (#2438 review
// P2-a #6): the interaction listener defaults to 8780, and without a fixed
// port of its own the startup could never prove which assembly owns the
// listener it asserts about.
const STARTUP_BUILDS_API_SERVER_PORT: u16 = 28642;
const STARTUP_BUILDS_WEBHOOK_PORT: u16 = 28643;
const STARTUP_BUILDS_WECOM_CALLBACK_PORT: u16 = 28646;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_builds_the_enabled_loader_channels() {
    if child_probe_token("startup-builds").is_some() {
        startup_builds_loader_channels_child().await;
        return;
    }
    run_startup_probe_child(
        "native_process::channels_tests::startup_builds_the_enabled_loader_channels",
        "startup-builds",
        &[
            format!("api-server: listening on http://127.0.0.1:{STARTUP_BUILDS_API_SERVER_PORT}"),
            format!(
                "webhook: listening on http://127.0.0.1:{STARTUP_BUILDS_WEBHOOK_PORT} \
                 — routes: (none configured)"
            ),
            format!("wecom_callback: listening on 0.0.0.0:{STARTUP_BUILDS_WECOM_CALLBACK_PORT}"),
        ],
    )
    .await;
}

/// The child half of [`startup_builds_the_enabled_loader_channels`]: the body
/// that claims the fixed ports, drives the real `assembly::start`, and stops
/// the host — everything the parent then observes through the child's
/// console (readiness lines) and exit status.
async fn startup_builds_loader_channels_child() {
    require_env_unset(&["API_SERVER_KEY", "API_SERVER_HOST"]);
    let _ports = claim_fixed_ports(&[
        STARTUP_BUILDS_API_SERVER_PORT,
        STARTUP_BUILDS_WEBHOOK_PORT,
        STARTUP_BUILDS_WECOM_CALLBACK_PORT,
    ])
    .await;
    let adapters = format!(
        // The wecomCallback key carries dummy credentials so its start passes
        // the config gate and really binds the pinned port (the gate only
        // checks the strings are present; the channel contacts the qyapi on
        // send, not at startup). `token` is a top-level field, the rest sit
        // in `extra`, mirroring the loader's option object.
        "  slack: {{enabled: true}}\n  bluebubbles: {{enabled: true}}\n  \
         wecomCallback: {{enabled: true, token: probe, extra: {{corpId: probe, \
         agentId: probe, secret: probe, encodingAesKey: probe, \
         port: {STARTUP_BUILDS_WECOM_CALLBACK_PORT}}}}}\n  \
         sms: {{enabled: true}}\n  \
         apiServer: {{enabled: true, extra: {{port: {STARTUP_BUILDS_API_SERVER_PORT}}}}}\n  \
         webhook: {{enabled: true, extra: {{port: {STARTUP_BUILDS_WEBHOOK_PORT}}}}}\n"
    );
    let alive = start_with_adapters_alive(&adapters).await;
    // The status-entry key is the adapter's channel key (loader camelCase
    // keys map to the module's own key, e.g. `apiServer` → `api_server`).
    let expected = [
        "slack",
        "bluebubbles",
        "wecom_callback",
        "sms",
        "api_server",
        "webhook",
    ];
    let polled = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = JsValue::Object(read_snapshot(&alive.home));
            let channels = snapshot.get("channels");
            let entries: Vec<(String, String)> = expected
                .iter()
                .filter_map(|key| {
                    let entry = channels.and_then(|channels| channels.get(key))?;
                    Some((
                        field_text(entry, "state").unwrap_or_default(),
                        field_text(entry, "message").unwrap_or_default(),
                    ))
                })
                .collect();
            if entries.len() == expected.len()
                && entries
                    .iter()
                    .all(|(state, message)| state == "connected" && message.ends_with(": started"))
            {
                return entries;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    crate::negative_assert!(
        polled.is_ok(),
        "#2301 all six loader channels report their lifecycle entry"
    );
    let started = polled.expect("#2301 all six loader channels report their lifecycle entry");
    for (state, message) in started {
        crate::negative_assert_eq!(
            state,
            "connected".to_owned(),
            "#2301 {message} — the entry is the connected lifecycle state"
        );
    }
    // The listener channels actually listen on their configured ports, and
    // each listener is this assembly's channel: `/health` answers the
    // channel's own platform string (the `health-and-routing` oracle body),
    // which a stray listener on the port does not.
    assert_health_platform(STARTUP_BUILDS_API_SERVER_PORT, "api-server").await;
    assert_health_platform(STARTUP_BUILDS_WEBHOOK_PORT, "webhook").await;
    alive.stop().await;
    // The wecomCallback interaction listener is this run's too: it was
    // pinned to a fixed port above, so it is claimed, observed (the
    // readiness line the parent requires) and confirmed gone here.
    assert_ports_closed_after_stop(&[
        STARTUP_BUILDS_API_SERVER_PORT,
        STARTUP_BUILDS_WEBHOOK_PORT,
        STARTUP_BUILDS_WECOM_CALLBACK_PORT,
    ])
    .await;
}

/// `GET /health` on one of the assembly's loader-built listeners: the
/// response must be the channel's own health body, `{"status":"ok",
/// "platform":"<platform>"}` (api-server and webhook share the route shape;
/// the platform string names the channel). A listener that is not this
/// assembly's channel — the fixed-port hazard the pre-probe guards — does
/// not answer with this body. The whole connect → send → read stretch is
/// bounded (#2301 fix-r2 review P1 #4): a listener that accepts the
/// connection but never answers (and never closes it) would park
/// `read_to_end` forever — the slow-timeout profile only reports, it never
/// terminates — so the probe fails by name after 5 seconds instead of
/// hanging the suite.
async fn assert_health_platform(port: u16, platform: &str) {
    let response = tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = crate::test_negative_receipt::expect_ok(
            tokio::net::TcpStream::connect(("127.0.0.1", port)).await,
            &format!("#2301 the {platform} listener accepts connections"),
        );
        crate::test_negative_receipt::expect_ok(
            stream
                .write_all(b"GET /health HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\n\r\n")
                .await,
            &format!("#2301 the {platform} health request is sent"),
        );
        let mut response = Vec::new();
        crate::test_negative_receipt::expect_ok(
            stream.read_to_end(&mut response).await,
            &format!("#2301 the {platform} health response is read"),
        );
        response
    })
    .await;
    let response = crate::test_negative_receipt::expect_ok(
        response,
        &format!(
            "#2301 the {platform} listener on port {port} answered /health within 5s — \
             a listener that accepts but never replies must fail by name, not hang the suite"
        ),
    );
    let text = String::from_utf8_lossy(&response);
    let expected = format!("\"platform\":\"{platform}\"");
    crate::negative_assert!(
        text.starts_with("HTTP/1.1 200")
            && text.contains(&expected)
            && text.contains("\"status\":\"ok\""),
        "#2301 the listener on port {port} is this assembly's {platform} channel, got: {text:?}"
    );
}

// ------------------------------------------------ #2301 fix-r1 review items

// Review P0 #1 (fix-r1): `console_channel_log` writes the adapter lifecycle
// lines, and the adapter-internal log calls (`api-server: listening on …`,
// `sms: twilio not installed…`) are not under `ChannelServer`'s `catch_unwind`
// — only the start task itself is. `println!` / `eprintln!` panic when the
// write fails (std's macro contract), so a closed stdout would abort the
// adapter's task after it saved its listener state, stranding the listener.
// TS's `console.log` / `console.error` never fail the channel; the write error
// is dropped instead (`console_log_line`).
//
// Review P0 #2 + P1 #5 (fix-r2): the fix-r1 control only drove the helper
// directly, so a regression of either *production* closure — the
// `console_channel_log` closure, or the loader's `console.error` closure in
// `create_startup_configured` (which `assembly::start` calls synchronously
// before any channel starts, outside every catch_unwind) — back to
// `println!`/`eprintln!` survived it. The control below drives both
// production closures themselves.
//
// The level → sink selection is TS `consoleLogger`'s own contract
// (`info` → stdout, `warn`/`error` → stderr, pilotdeckServer.ts:67-72); the
// failing-sink probe cannot observe routing (both its sinks fail), so this
// pins the shared entry's routing first.
#[derive(Default)]
struct RecordingSink {
    contents: String,
}
impl std::io::Write for RecordingSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.contents.push_str(&String::from_utf8_lossy(buf));
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn console_channel_log_routes_levels_to_the_console_sinks() {
    let mut out = RecordingSink::default();
    let mut err = RecordingSink::default();
    super::channels::write_console_line(
        "info",
        "api-server: listening on http://127.0.0.1:8642",
        &mut out,
        &mut err,
    );
    super::channels::write_console_line("warn", "weixin: waiting for login", &mut out, &mut err);
    super::channels::write_console_line(
        "error",
        "sms: twilio not installed; run npm install twilio",
        &mut out,
        &mut err,
    );
    crate::negative_assert_eq!(
        out.contents,
        "api-server: listening on http://127.0.0.1:8642\n".to_owned(),
        "#2301 info-level console lines go to stdout"
    );
    crate::negative_assert_eq!(
        err.contents,
        "weixin: waiting for login\nsms: twilio not installed; run npm install twilio\n".to_owned(),
        "#2301 warn/error console lines go to stderr"
    );
}

/// The child marker for [`console_ports_survive_a_failing_sink`]: when the
/// `failing-console` token pair matches (see [`CHILD_PROBE_ENV`]), this
/// process is the probe itself.
#[tokio::test]
async fn console_ports_survive_a_failing_sink() {
    if let Some(token) = child_probe_token("failing-console") {
        run_failing_console_probe(&token);
        return;
    }
    let token = format!("pdrs-2301-{}", uuid::Uuid::new_v4().simple());
    let mut child =
        tokio::process::Command::new(std::env::current_exe().expect("#2301 the probe binary path"))
            .args([
                "--exact",
                // `--exact` matches the full libtest path, not the bare name.
                "native_process::channels_tests::console_ports_survive_a_failing_sink",
                // libtest's capture would route a mutated `println!` into its
                // in-memory sink instead of the failing fd, so the probe must run
                // uncaptured to reach the real console. The token doubles as the
                // positional nonce the child checks (see [`child_probe_token`]).
                "--nocapture",
                "--test-threads=1",
                &token,
            ])
            .env_clear()
            .envs(probe_child_env("failing-console", &token))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("#2301 the failing-console probe process spawns");
    let stdin = child.stdin.take().expect("#2301 the probe's stdin pipe");
    let mut stdout = child.stdout.take().expect("#2301 the probe's stdout pipe");
    let stderr = child.stderr.take().expect("#2301 the probe's stderr pipe");
    // The handshake orders the probe (#2301 fix-r3 review P1 #3: the old
    // libtest-banner scan raced it — under `RUST_TEST_THREADS=1` or a
    // single-CPU host the banner could land after the pipes were already
    // closed, failing the control spuriously). The child now announces
    // itself with a marker line naming this run's token, and this side
    // drops the read ends (every later write on the child's fds 1/2 fails
    // with EPIPE, the closed-console hazard) only after the marker arrives.
    let marker = format!("PDRS-2301-CONSOLE-READY-{token}\n").into_bytes();
    let announced = tokio::time::timeout(Duration::from_secs(10), async {
        let mut seen = Vec::new();
        let mut chunk = [0u8; 64];
        loop {
            let read = stdout
                .read(&mut chunk)
                .await
                .expect("#2301 the probe's console stays readable");
            if read == 0 {
                return false;
            }
            seen.extend_from_slice(&chunk[..read]);
            if seen.windows(marker.len()).any(|window| window == marker) {
                return true;
            }
        }
    })
    .await;
    crate::negative_assert!(
        matches!(announced, Ok(true)),
        "#2301 the failing-console probe announced itself within 10s — its \
         ready marker never arrived on the child console"
    );
    drop(stdout);
    drop(stderr);
    drop(stdin);
    // Every wait states its termination condition: 30 seconds on the probe,
    // then it is killed and reaped before the named failure (P1 #4).
    let done = tokio::time::timeout(Duration::from_secs(30), child.wait()).await;
    let status = match done {
        Ok(status) => status.expect("#2301 the failing-console probe is waitable"),
        Err(_) => {
            child
                .kill()
                .await
                .expect("#2301 the timed-out console probe is killed");
            let _ = child.wait().await;
            crate::negative_assert!(
                false,
                "#2301 the failing-console probe did not finish within 30s"
            );
            unreachable!("negative_assert! panics");
        }
    };
    crate::negative_assert!(
        status.success(),
        "#2301 the production console ports must survive a failing console — \
         `println!`/`eprintln!` panic on a failed write (child exit {status})"
    );
}

/// The child half of [`console_ports_survive_a_failing_sink`]: this process is
/// the re-exec'd test binary, so its stdout and stderr are the pipes the
/// parent chose. The parent drops the pipes' read ends (both of the child's
/// console fds now fail every write with EPIPE) and then closes the child's
/// stdin — the blocking read below returns EOF only at that point, so the
/// failing console is already in place when the production closures run.
/// No fd surgery is needed (and none is possible under the workspace's
/// `unsafe_code = "forbid"`): the pipes were wired by the parent's `spawn`.
fn run_failing_console_probe(token: &str) {
    // Announce first (the marker names this run's token, so a stale process
    // from an earlier run cannot satisfy the parent's scan), then wait for
    // the parent to close the pipes and release the stdin EOF. The wait is
    // bounded (60s watchdog): a lost parent must not park this process
    // forever.
    {
        use std::io::Write as _;
        let mut stdout = std::io::stdout();
        writeln!(stdout, "PDRS-2301-CONSOLE-READY-{token}")
            .expect("#2301 the probe's ready marker write");
        stdout
            .flush()
            .expect("#2301 the probe's ready marker flush");
    }
    let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let armed = released.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(60));
        if !armed.load(std::sync::atomic::Ordering::Relaxed) {
            // The console may already be failing — write best-effort, never
            // panic, then exit by name.
            use std::io::Write as _;
            let _ =
                std::io::stderr().write_all(b"#2301 the probe's stdin never closed within 60s\n");
            std::process::exit(1);
        }
    });
    {
        use std::io::Read as _;
        let mut stdin = std::io::stdin();
        let mut byte = [0u8; 1];
        while stdin.read(&mut byte).unwrap_or(0) > 0 {}
    }
    released.store(true, std::sync::atomic::Ordering::Relaxed);
    // Production console port #1: the `console_channel_log` closure
    // `assembly::start` mounts on the channel server.
    let log = super::channels::console_channel_log();
    log("info", "api-server: listening on http://127.0.0.1:8642");
    log("warn", "weixin: waiting for login");
    log("error", "sms: twilio not installed; run npm install twilio");
    // Production console port #2: the loader's `console.error` closure, here
    // reached through the real `create_startup_configured` — the apiServer
    // key's `Number(port)` throws, so the error-then-continue loop reports
    // it while the (empty) rest of the startup constructs.
    let home = Root::new();
    let channels = Arc::new(NativeChannels::new(NativeChannelsOptions {
        pilot_home: home.path(),
        config: Arc::new(
            parse_json(
                r#"{"adapters": {"apiServer": {"enabled": true, "extra": {"port": {"toString": null}}}}}"#,
            )
            .expect("#2301 the probe's config parses"),
        ),
        weixin: crate::channel::weixin::WeixinChannelOptions::default(),
        weixin_maker: None,
        feishu: crate::channel::feishu::FeishuOptions::default(),
        log: None,
        report_status: None,
        close_host: None,
    }));
    let startup = channels.create_startup_configured();
    crate::negative_assert_eq!(
        startup.len(),
        0,
        "#2301 the probe's bad apiServer key constructs nothing"
    );
    drop(startup);
    drop(channels);
    // The probe's scratch directory is released before the exit (#2301
    // fix-r3 review P1: `process::exit` skips destructors, so a probe that
    // exits with its `Root` still alive leaks the temp directory).
    drop(home);
    std::process::exit(0);
}

/// Review P2-a #5 (fix-r3): the failing-sink probe above cannot observe
/// routing — both of its sinks fail, so whichever way the production
/// `console_channel_log` closure routes, the writes are dropped either way.
/// This control drives the real closure itself (the one `assembly::start`
/// mounts) with the child's fds pointed at files, pinning the level → sink
/// selection TS's `consoleLogger` owns (info → stdout, warn/error → stderr,
/// pilotdeckServer.ts:67-72). libtest's own output shares those files, so
/// the assertions are containment-shaped with an explicit "not in the other
/// sink" half.
#[tokio::test]
async fn console_channel_log_routes_through_the_real_closure() {
    if child_probe_token("sink-routing").is_some() {
        run_console_sink_probe();
        return;
    }
    let home = Root::new();
    let out_path = home.path().join("console-out");
    let err_path = home.path().join("console-err");
    let out_file = std::fs::File::create(&out_path)
        .unwrap_or_else(|error| panic!("#2301 the sink probe's stdout file: {error}"));
    let err_file = std::fs::File::create(&err_path)
        .unwrap_or_else(|error| panic!("#2301 the sink probe's stderr file: {error}"));
    let token = format!("pdrs-2301-{}", uuid::Uuid::new_v4().simple());
    let mut child = tokio::process::Command::new(
        std::env::current_exe().expect("#2301 the probe binary path"),
    )
    .args([
        "--exact",
        "native_process::channels_tests::console_channel_log_routes_through_the_real_closure",
        "--nocapture",
        "--test-threads=1",
        &token,
    ])
    .env_clear()
    .envs(probe_child_env("sink-routing", &token))
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::from(out_file))
    .stderr(std::process::Stdio::from(err_file))
    .spawn()
    .expect("#2301 the sink-routing probe child spawns");
    // Every wait states its termination condition: 30 seconds on the child,
    // then kill + reap before the named failure (P1 #4).
    let exited = tokio::time::timeout(Duration::from_secs(30), child.wait()).await;
    let status = match exited {
        Ok(status) => status.expect("#2301 the sink-routing probe child is waitable"),
        Err(_) => {
            child
                .kill()
                .await
                .expect("#2301 the timed-out sink-routing probe child is killed");
            let _ = child.wait().await;
            crate::negative_assert!(
                false,
                "#2301 the sink-routing probe child did not finish within 30s"
            );
            unreachable!("negative_assert! panics");
        }
    };
    crate::negative_assert!(
        status.success(),
        "#2301 the sink-routing probe child exited cleanly (exit {status})"
    );
    let out = std::fs::read_to_string(&out_path)
        .unwrap_or_else(|error| panic!("#2301 the sink probe's stdout file reads: {error}"));
    let err = std::fs::read_to_string(&err_path)
        .unwrap_or_else(|error| panic!("#2301 the sink probe's stderr file reads: {error}"));
    crate::negative_assert!(
        out.contains("sink-probe: info line\n") && !err.contains("sink-probe: info line\n"),
        "#2301 the real console_channel_log closure routes info to stdout \
         (stdout file: {out:?}, stderr file: {err:?})"
    );
    crate::negative_assert!(
        err.contains("sink-probe: warn line\n") && !out.contains("sink-probe: warn line\n"),
        "#2301 the real console_channel_log closure routes warn to stderr \
         (stdout file: {out:?}, stderr file: {err:?})"
    );
    crate::negative_assert!(
        err.contains("sink-probe: error line\n") && !out.contains("sink-probe: error line\n"),
        "#2301 the real console_channel_log closure routes error to stderr \
         (stdout file: {out:?}, stderr file: {err:?})"
    );
}

/// The child half of [`console_channel_log_routes_through_the_real_closure`]:
/// this process's stdout and stderr are the parent-chosen files, so the
/// production closure's routing lands in them directly; the child returns
/// normally (libtest reports the test ok) — the files are the evidence, no
/// assertion runs here.
fn run_console_sink_probe() {
    let log = super::channels::console_channel_log();
    log("info", "sink-probe: info line");
    log("warn", "sink-probe: warn line");
    log("error", "sink-probe: error line");
}

// Review P1 #4 (loader level; fix-r2 review P1 #3 + P2-a #6 widened it): TS
// constructs every enabled loader key, and a constructor throw —
// `Number(options.port)` failing toPrimitive with `TypeError: Cannot convert
// object to primitive value` (Node v22.22.0 probes: fix-r1 `probe1.out` for
// the own-`toString` shape, fix-r2 `probe1.out`/`probe2.out` for the array,
// nested-array and prototype-carried shapes) — is `console.error`ed while
// the loop continues (loadEnabledChannels.ts:132-136). The fix-r1 guard only
// checked an own `toString`, so an array-shaped or prototype-carried port
// slipped through and the channel constructed where TS records the key and
// skips it; the guard now walks the same coercion `String(value)` uses
// (arrays join, the prototype chain the config merge installs is visible),
// so every throwing shape propagates. The fixture pins one bad shape per
// mount — apiServer own `toString`, webhook nested array, wecomCallback
// prototype — plus the keys around them, so no mount can silently drop its
// guard again.
#[test]
fn loader_records_constructor_type_errors_and_continues() {
    let mut config = parse_json(
        r#"{"adapters": {
            "slack": {"enabled": true},
            "bluebubbles": {"enabled": true},
            "wecomCallback": {"enabled": true, "extra": {}},
            "sms": {"enabled": true},
            "apiServer": {"enabled": true, "extra": {"port": {"toString": null}}},
            "webhook": {"enabled": true, "extra": {"port": [[{"toString": null}]]}}
        }}"#,
    )
    .expect("#2301 loader fixture config parses");
    install_proto_to_string_port(&mut config);
    let mut errors = Vec::new();
    let channels = crate::channel::loader::load_enabled_channels(
        config.get("adapters"),
        &crate::channel::loader::LoadEnabledChannelsOptions { pilot_home: None },
        &super::channels::production_loader_mounts(),
        &mut |message| errors.push(message),
    );
    crate::negative_assert_eq!(
        errors,
        vec![
            "[adapters] Failed to load channel \"wecomCallback\": TypeError: Cannot convert object to primitive value"
                .to_owned(),
            "[adapters] Failed to load channel \"apiServer\": TypeError: Cannot convert object to primitive value"
                .to_owned(),
            "[adapters] Failed to load channel \"webhook\": TypeError: Cannot convert object to primitive value"
                .to_owned(),
        ],
        "#2301 every mount reports its Number() failure as the loader's console.error line"
    );
    let keys: Vec<String> = channels
        .iter()
        .map(|channel| String::from_utf16_lossy(channel.channel_key().as_js().units()))
        .collect();
    crate::negative_assert_eq!(
        keys,
        vec![
            "slack".to_owned(),
            "bluebubbles".to_owned(),
            "sms".to_owned()
        ],
        "#2301 the keys around the failed constructors still construct"
    );
}

/// The wecomCallback fixture's port value: the `__proto__` shape — a `__proto__`
/// key on a config object reaches `config::merge`'s prototype arm (mirroring
/// TS `mergeConfigSources`' `output[key] = value`, `config/mod.rs`), so the
/// merged object's property lookup walks the installed prototype. `parse_json`
/// keeps `__proto__` an own key, so the prototype is installed directly here
/// to pin the post-merge shape the mount's guard must reject.
fn install_proto_to_string_port(config: &mut JsValue) {
    let mut port = JsObject::new();
    port.set_prototype(parse_json(r#"{"toString": null}"#).expect("#2301 the proto shape parses"));
    let JsValue::Object(root) = config else {
        panic!("#2301 the fixture root is an object");
    };
    let mut adapters = match root.get("adapters") {
        Some(JsValue::Object(adapters)) => adapters.clone(),
        _ => panic!("#2301 the fixture adapters entry is an object"),
    };
    let mut wecom = match adapters.get("wecomCallback") {
        Some(JsValue::Object(wecom)) => wecom.clone(),
        _ => panic!("#2301 the fixture wecomCallback entry is an object"),
    };
    let mut extra = match wecom.get("extra") {
        Some(JsValue::Object(extra)) => extra.clone(),
        _ => panic!("#2301 the fixture extra entry is an object"),
    };
    extra.insert("port".into(), JsValue::Object(port));
    wecom.insert("extra".into(), JsValue::Object(extra));
    adapters.insert("wecomCallback".into(), JsValue::Object(wecom));
    root.insert("adapters".into(), JsValue::Object(adapters));
}

// Review P1 #4 (assembly level): the same construction failure through the
// real startup — `assembly::start` calls `create_startup_configured`, so the
// apiServer key with a `{toString: null}` port must never reach
// `ChannelServer` (no runtime-status entry) while the webhook key after it
// in table order still constructs and owns its port. #2301 fix-r3 (review
// P0 #1): the port ownership stretch moved into a re-exec'd child, with the
// webhook readiness line as the parent's instance-binding proof — the
// apiServer key needs no env pre-flight, its constructor throws before any
// environment read.
const STARTUP_SKIPS_WEBHOOK_PORT: u16 = 28644;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_skips_channels_whose_constructor_throws() {
    if child_probe_token("startup-skips").is_some() {
        startup_skips_channels_child().await;
        return;
    }
    run_startup_probe_child(
        "native_process::channels_tests::startup_skips_channels_whose_constructor_throws",
        "startup-skips",
        &[format!(
            "webhook: listening on http://127.0.0.1:{STARTUP_SKIPS_WEBHOOK_PORT} \
             — routes: (none configured)"
        )],
    )
    .await;
}

/// The child half of [`startup_skips_channels_whose_constructor_throws`]:
/// the body that claims the fixed port, drives the real `assembly::start`
/// with one bad key ahead of one good key, and stops the host.
async fn startup_skips_channels_child() {
    let _ports = claim_fixed_ports(&[STARTUP_SKIPS_WEBHOOK_PORT]).await;
    let adapters = format!(
        "  apiServer: {{enabled: true, extra: {{port: {{toString: null}}}}}}\n  \
         webhook: {{enabled: true, extra: {{port: {STARTUP_SKIPS_WEBHOOK_PORT}}}}}\n"
    );
    let alive = start_with_adapters_alive(&adapters).await;
    let polled = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = JsValue::Object(read_snapshot(&alive.home));
            let ready = snapshot
                .get("channels")
                .and_then(|channels| channels.get("webhook"))
                .map(|entry| {
                    field_text(entry, "state").as_deref() == Some("connected")
                        && field_text(entry, "message")
                            .is_some_and(|message| message.ends_with(": started"))
                })
                .unwrap_or(false);
            if ready {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    crate::negative_assert!(
        polled.is_ok(),
        "#2301 the key after the failed constructor still starts"
    );
    // The failed key never reached the channel server: no lifecycle entry,
    // no listener — the entry only exists once `ChannelServer` starts an
    // adapter, and construction failed before that.
    let snapshot = JsValue::Object(read_snapshot(&alive.home));
    let api_entry = snapshot
        .get("channels")
        .and_then(|channels| channels.get("api_server"));
    crate::negative_assert!(
        api_entry.is_none(),
        "#2301 a constructor failure leaves no runtime-status entry: {:?}",
        api_entry.map(|entry| field_text(entry, "state"))
    );
    assert_health_platform(STARTUP_SKIPS_WEBHOOK_PORT, "webhook").await;
    alive.stop().await;
    // The listener this run built is gone with the host (#2301 fix-r2 review
    // P0: without the after-stop check a listener that outlives the assembly
    // would keep answering `/health` from another run's channel).
    assert_ports_closed_after_stop(&[STARTUP_SKIPS_WEBHOOK_PORT]).await;
}

// Review P1 #3: the whole channel server shares one `TurnGateway`
// (`FeishuTurnGateway` → `SubmitterTurnGateway` once the submitter exists).
// It used to stamp every submit with the construction-time feishu identity,
// so an api-server turn entered A7 as `feishu`; TS's api-server submits carry
// `channelKey: "api_server"` (ApiServerChannel.ts:319/:376 — Node v22.22.0
// probe `probe3.out`: gateway observed `{"sessionKey":"api_server:chat=…",
// "channelKey":"api_server","message":"hello"}`). This test drives the real
// entry — the loader-built channel's own HTTP server, through the shared
// assembly gateway — and pins the identity A7's `normalize` sees first.
struct RecordingSubmitHost {
    seen: std::sync::mpsc::Sender<(String, String, String)>,
}

impl SubmitHost for RecordingSubmitHost {
    fn workspace(&self, _input: &JsValue) -> Result<SubmitWorkspace, SubmitError> {
        Ok(SubmitWorkspace {
            id: None,
            snapshots_required: false,
            recorder: None,
        })
    }
    fn failure_guard_config(&self) -> Option<JsValue> {
        None
    }
    fn set_session_cwd(&self, _session: &JsString, _cwd: &JsString) -> Result<(), SubmitError> {
        Ok(())
    }
    fn now_ms(&self) -> f64 {
        0.0
    }
    fn abort_turn_timeout_ms(&self) -> u64 {
        60_000
    }
    fn normalize<'a>(
        &'a self,
        input: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        let seen = (
            field_text(input, "channelKey").unwrap_or_default(),
            field_text(input, "sessionKey").unwrap_or_default(),
            field_text(input, "message").unwrap_or_default(),
        );
        let _ = self.seen.send(seen);
        // The identity is captured; short-circuit the turn so the test never
        // needs a session behind the stub factory.
        Box::pin(async { Err(SubmitError::new("test", "identity captured")) })
    }
    fn route_input<'a>(
        &'a self,
        _input: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn admitted_preflight<'a>(
        &'a self,
        _input: &'a JsValue,
        _run: &'a RunRef,
        _completion: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn observe_event<'a>(
        &'a self,
        _input: &'a JsValue,
        _event: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
    }
    fn preflight<'a>(
        &'a self,
        _input: &'a JsValue,
        _run: &'a RunRef,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn has_sdk_config(&self) -> bool {
        false
    }
    fn validate_sdk_config(&self, _config: &JsValue) -> Result<(), SubmitError> {
        Ok(())
    }
    fn set_sdk_config<'a>(
        &'a self,
        _input: &'a JsValue,
        _completion: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<bool, SubmitError>> {
        Box::pin(async { Ok(false) })
    }
    fn finish_pump<'a>(&'a self, _run: &'a RunRef) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
    }
    fn refresh(&self) -> SubmitFuture<'_, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
    }
    fn prepare<'a>(
        &'a self,
        input: &'a JsValue,
        _run: &'a RunRef,
    ) -> SubmitFuture<'a, Result<PreparedTurn, SubmitError>> {
        let input = input.clone();
        Box::pin(async {
            Ok(PreparedTurn {
                model_selection: JsValue::Undefined,
                input,
                options: JsValue::Undefined,
            })
        })
    }
    fn commit_accepted_input<'a>(
        &'a self,
        _run: &'a RunRef,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
    }
    fn record_failure<'a>(
        &'a self,
        _run: &'a RunRef,
        _status: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
    }
    fn reject_pending(&self, _session: &JsString, _reason: &str) -> Result<(), SubmitError> {
        Ok(())
    }
    fn release_claim(&self, _session: &JsString, _run: &JsString) {}
    fn after_turn(&self, _run: &RunRef) -> Result<(), SubmitError> {
        Ok(())
    }
}

/// The session factory behind the stub host. `normalize` short-circuits the
/// turn before any session is needed, so a factory that always fails is the
/// honest stub: reaching it would mean the identity test lost control.
struct UnreachedSessionFactory;
impl SessionFactory for UnreachedSessionFactory {
    fn create(
        &self,
        _context: SessionContext,
        _previous: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        Box::pin(async {
            Err(RuntimeError::new(
                "#2301 identity test: the session factory is never reached",
            ))
        })
    }
}

// #2301 fix-r3 (review P0 #1): this test keeps its assembly in-process (the
// recording submitter and its mpsc channel are the oracle), so its port
// ownership proof stays in the parent — the claim helper's ordered release
// (see [`FixedPortRegion`]) and the readiness log line remain the binding
// evidence, plus the named env pre-flight below.
const IDENTITY_API_SERVER_PORT: u16 = 28645;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loader_channels_submit_under_their_own_channel_identity() {
    // #2301 fix-r2 (review P2-a #8): the api-server channel falls back to
    // `API_SERVER_KEY` / `API_SERVER_HOST` from the environment
    // (`api_server/mod.rs`'s `options.apiKey ?? process.env.API_SERVER_KEY`
    // order): a host `API_SERVER_KEY` would 401 the chat completion (normalize
    // never runs — the poll below fails misleadingly ten seconds later) and a
    // host `API_SERVER_HOST` would move the bind off 127.0.0.1 (the readiness
    // log line never matches). Both are named pre-checks, like the port probe.
    require_env_unset(&["API_SERVER_KEY", "API_SERVER_HOST"]);
    // Fixed port with the pre-probe ownership convention (see
    // [`startup_builds_the_enabled_loader_channels`]): the log line prints the
    // configured port, not the bound one, so a port-0 listener cannot be
    // discovered from it and the test owns its port explicitly instead. The
    // flock region serializes parallel runs of this suite (fix-r2 review P0).
    const API_SERVER_PORT: u16 = IDENTITY_API_SERVER_PORT;
    let _ports = claim_fixed_ports(&[API_SERVER_PORT]).await;
    let home = Root::new();
    let log_lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = log_lines.clone();
    let log: crate::channel::protocol::ChannelLog = Arc::new(move |_level: &str, message: &str| {
        sink.lock()
            .expect("#2301 log sink mutex")
            .push(message.to_owned());
    });
    let channels = Arc::new(NativeChannels::new(NativeChannelsOptions {
        pilot_home: home.path(),
        config: Arc::new(
            parse_json(&format!(
                r#"{{"adapters": {{"apiServer": {{"enabled": true, "extra": {{"port": {}}}}}}}}}"#,
                API_SERVER_PORT
            ))
            .expect("#2301 identity test config parses"),
        ),
        weixin: crate::channel::weixin::WeixinChannelOptions::default(),
        weixin_maker: None,
        feishu: crate::channel::feishu::FeishuOptions::default(),
        log: Some(log),
        report_status: None,
        close_host: None,
    }));
    let (seen_tx, seen_rx) = std::sync::mpsc::channel::<(String, String, String)>();
    let submitter = Arc::new(TurnSubmitter {
        owner: SessionOwner::new(SessionOwnerOptions::new(Arc::new(UnreachedSessionFactory))),
        coordinator: Arc::new(TurnEventCoordinator::new()),
        projector: Arc::new(
            GatewayAgentEventProjector::new(GatewayAgentEventProjectorOptions::default())
                .expect("#2301 identity test projector"),
        ),
        registry: None,
        host: Arc::new(RecordingSubmitHost { seen: seen_tx }),
        workspace_admissions: WorkspaceAdmissions::default(),
    });
    crate::negative_assert!(
        channels.set_turn_submitter(submitter),
        "#2301 the identity test publishes its submitter before startup"
    );
    let startup = channels.create_startup_configured();
    crate::negative_assert_eq!(
        startup.len(),
        1,
        "#2301 the apiServer config constructs exactly one channel"
    );
    channels.server().start_channels(startup);
    // Readiness: the TS anchor startup line (`api-server: listening on …`,
    // pilotdeckServer.ts's `consoleLogger` shape) lands after the bind.
    let listening = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let seen = log_lines
                .lock()
                .expect("#2301 log sink mutex")
                .iter()
                .any(|message| {
                    *message
                        == format!("api-server: listening on http://127.0.0.1:{API_SERVER_PORT}")
                });
            if seen {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    crate::test_negative_receipt::expect_ok(
        listening,
        "#2301 the loader-built api-server listener logs its startup line",
    );
    // The real entry: one OpenAI-style chat completion against the channel's
    // own HTTP server. The response itself is not the oracle here — the
    // identity the submitter's `normalize` sees first is. The whole connect →
    // send → read stretch is bounded (#2301 fix-r2 review P1 #4, same shape
    // as [`assert_health_platform`]): a listener that accepts but never
    // replies must fail by name, not park `read_to_end` forever.
    let response = tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = crate::test_negative_receipt::expect_ok(
            tokio::net::TcpStream::connect(("127.0.0.1", API_SERVER_PORT)).await,
            "#2301 the loader-built api-server listener accepts connections",
        );
        let body = r#"{"model":"x","messages":[{"role":"user","content":"hello"}]}"#;
        crate::test_negative_receipt::expect_ok(
            stream
                .write_all(
                    format!(
                        "POST /v1/chat/completions HTTP/1.1\r\nhost: 127.0.0.1\r\n\
                         content-type: application/json\r\ncontent-length: {}\r\n\
                         connection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                )
                .await,
            "#2301 the chat completion request is sent",
        );
        let mut response = Vec::new();
        crate::test_negative_receipt::expect_ok(
            stream.read_to_end(&mut response).await,
            "#2301 the chat completion response is read",
        );
        response
    })
    .await;
    let _response = crate::test_negative_receipt::expect_ok(
        response,
        "#2301 the chat completion was answered within 5s — a listener that \
         accepts but never replies must fail by name, not hang the suite",
    );
    let seen = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(seen) = seen_rx.try_recv() {
                return seen;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let (channel_key, session_key, message) =
        crate::test_negative_receipt::expect_ok(seen, "#2301 the turn reached the submitter");
    crate::negative_assert_eq!(
        channel_key,
        "api_server".to_owned(),
        "#2301 the api-server turn enters A7 under its own channel key, not the gateway's"
    );
    crate::negative_assert!(
        session_key.starts_with("api_server:chat="),
        "#2301 the api-server turn carries the channel's session key, got: {session_key:?}"
    );
    crate::negative_assert_eq!(
        message,
        "hello".to_owned(),
        "#2301 the api-server turn carries the user message"
    );
    crate::test_negative_receipt::expect_ok(
        channels.close().await,
        "#2301 the identity test channels closed",
    );
    // The listener this run built is gone with the channels (#2301 fix-r2
    // review P0: the after-stop ownership check, same shape as the startup
    // test's).
    assert_ports_closed_after_stop(&[IDENTITY_API_SERVER_PORT]).await;
}

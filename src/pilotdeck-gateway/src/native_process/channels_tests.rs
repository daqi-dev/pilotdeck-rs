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
use pilotdeck_protocol::{JsString, JsValue, parse_json};
use std::{
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_builds_the_enabled_loader_channels() {
    const API_SERVER_PORT: u16 = 28642;
    const WEBHOOK_PORT: u16 = 28643;
    for port in [API_SERVER_PORT, WEBHOOK_PORT] {
        let probe = tokio::net::TcpListener::bind(("127.0.0.1", port)).await;
        crate::negative_assert!(
            probe.is_ok(),
            "#2301 loader test port {port} is already occupied — another run of this \
             suite (or a stray listener) owns it; rerun when the shared host is idle"
        );
    }
    let adapters = format!(
        "  slack: {{enabled: true}}\n  bluebubbles: {{enabled: true}}\n  \
         wecomCallback: {{enabled: true}}\n  sms: {{enabled: true}}\n  \
         apiServer: {{enabled: true, extra: {{port: {API_SERVER_PORT}}}}}\n  \
         webhook: {{enabled: true, extra: {{port: {WEBHOOK_PORT}}}}}\n"
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
    assert_health_platform(API_SERVER_PORT, "api-server").await;
    assert_health_platform(WEBHOOK_PORT, "webhook").await;
    alive.stop().await;
    for port in [API_SERVER_PORT, WEBHOOK_PORT] {
        crate::negative_assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err(),
            "#2301 the loader-built listeners are gone after stop"
        );
    }
}

/// `GET /health` on one of the assembly's loader-built listeners: the
/// response must be the channel's own health body, `{"status":"ok",
/// "platform":"<platform>"}` (api-server and webhook share the route shape;
/// the platform string names the channel). A listener that is not this
/// assembly's channel — the fixed-port hazard the pre-probe guards — does
/// not answer with this body.
async fn assert_health_platform(port: u16, platform: &str) {
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

// Review P0 #1: `console_channel_log` writes the adapter lifecycle lines, and
// the adapter-internal log calls (`api-server: listening on …`,
// `sms: twilio not installed…`) are not under `ChannelServer`'s `catch_unwind`
// — only the start task itself is. `println!` / `eprintln!` panic when the
// write fails (std's macro contract), so a closed stdout would abort the
// adapter's task after it saved its listener state, stranding the listener.
// TS's `console.log` / `console.error` never fail the channel; the write error
// is dropped instead (`console_log_line`).
struct FailingSink;
impl std::io::Write for FailingSink {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("sink closed"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("sink closed"))
    }
}

#[test]
fn console_channel_log_survives_a_failing_sink() {
    let outcome = std::panic::catch_unwind(|| {
        // The two lifecycle lines the startup test relies on, each written to
        // a sink that fails every write — the shape `console_channel_log`
        // hands the adapter's `start` to.
        super::channels::console_log_line(
            &mut FailingSink,
            "api-server: listening on http://127.0.0.1:8642",
        );
        super::channels::console_log_line(
            &mut FailingSink,
            "sms: twilio not installed; run npm install twilio",
        );
    });
    crate::negative_assert!(
        outcome.is_ok(),
        "#2301 a failing console sink must not panic the channel log"
    );
}

// Review P1 #4 (loader level): TS constructs every enabled loader key, and a
// constructor throw — `Number(options.port)` on `{toString: null}` fails
// toPrimitive with `TypeError: Cannot convert object to primitive value`
// (Node v22.22.0 probe: `_tmp/20261026-issue-2301/fix-r1/node-probe/
// probe1.out`) — is `console.error`ed while the loop continues
// (loadEnabledChannels.ts:132-136, `probe2.out`: apiServer bad port +
// webhook port 0 → one error line, `['webhook']`). The Rust mounts must
// propagate that construction failure instead of always returning Ok, or
// the key would enter the startup array and fail (or no-op) at start time
// with no error line at all.
#[test]
fn loader_records_constructor_type_errors_and_continues() {
    let config = parse_json(
        r#"{"adapters": {
            "apiServer": {"enabled": true, "extra": {"port": {"toString": null}}},
            "webhook": {"enabled": true, "extra": {"port": 0}}
        }}"#,
    )
    .expect("#2301 loader fixture config parses");
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
            "[adapters] Failed to load channel \"apiServer\": TypeError: Cannot convert object to primitive value"
                .to_owned()
        ],
        "#2301 the constructor's Number() failure is the loader's one console.error line"
    );
    let keys: Vec<String> = channels
        .iter()
        .map(|channel| String::from_utf16_lossy(channel.channel_key().as_js().units()))
        .collect();
    crate::negative_assert_eq!(
        keys,
        vec!["webhook".to_owned()],
        "#2301 the keys after the failed constructor still construct"
    );
}

// Review P1 #4 (assembly level): the same construction failure through the
// real startup — `assembly::start` calls `create_startup_configured`, so the
// apiServer key with a `{toString: null}` port must never reach
// `ChannelServer` (no runtime-status entry) while the webhook key after it
// in table order still constructs and owns its port.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_skips_channels_whose_constructor_throws() {
    const WEBHOOK_PORT: u16 = 28644;
    let probe = tokio::net::TcpListener::bind(("127.0.0.1", WEBHOOK_PORT)).await;
    crate::negative_assert!(
        probe.is_ok(),
        "#2301 loader skip test port {WEBHOOK_PORT} is already occupied — another run of this \
         suite (or a stray listener) owns it; rerun when the shared host is idle"
    );
    drop(probe);
    let adapters = format!(
        "  apiServer: {{enabled: true, extra: {{port: {{toString: null}}}}}}\n  \
         webhook: {{enabled: true, extra: {{port: {WEBHOOK_PORT}}}}}\n"
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
    assert_health_platform(WEBHOOK_PORT, "webhook").await;
    alive.stop().await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loader_channels_submit_under_their_own_channel_identity() {
    // Fixed port with the pre-probe ownership convention (see
    // [`startup_builds_the_enabled_loader_channels`]): the log line prints the
    // configured port, not the bound one, so a port-0 listener cannot be
    // discovered from it and the test owns its port explicitly instead.
    const API_SERVER_PORT: u16 = 28645;
    let probe = tokio::net::TcpListener::bind(("127.0.0.1", API_SERVER_PORT)).await;
    crate::negative_assert!(
        probe.is_ok(),
        "#2301 identity test port {API_SERVER_PORT} is already occupied — another run of this \
         suite (or a stray listener) owns it; rerun when the shared host is idle"
    );
    drop(probe);
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
    // identity the submitter's `normalize` sees first is.
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
}

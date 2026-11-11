//! Unit tests for the weixin channel pieces (mapper, persistence). The
//! channel flows against the TS golden and the mock ilink server live with
//! the fixture under `tests/fixtures/weixin-channel/`.
use super::*;
use pilotdeck_protocol::parse_json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn js(text: &str) -> JsString {
    JsString::from(text)
}

fn text_of(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

/// A temporary directory this test owns; removed on drop.
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("pdrs-weixin-unit-{}-{n}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("#1328 temp root: {e}"),
            }
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

#[test]
fn mapper_general_session_for_new_chat() {
    let mapper = WeixinSessionMapper::new();
    let result = mapper.resolve(&js("chat-1"), &js("hello"));
    assert_eq!(
        text_of(&result.session_key),
        "weixin:chat=chat-1:general",
        "#1328 unmapped chat falls back to :general"
    );
    assert_eq!(
        text_of(&result.message),
        "hello",
        "#1328 message passes through"
    );
    assert!(
        result.command.is_none(),
        "#1328 plain text is not a command"
    );
    assert!(result.project_key.is_none(), "#1328 no project bound");
}

#[test]
fn mapper_new_command_rotates_session() {
    let mapper = WeixinSessionMapper::with_uuid(Arc::new(|| js("fixed-uuid")));
    let first = mapper.resolve(&js("chat-1"), &js("hi"));
    assert_eq!(text_of(&first.session_key), "weixin:chat=chat-1:general");

    let rotated = mapper.resolve(&js("chat-1"), &js("/new"));
    assert_eq!(
        text_of(&rotated.session_key),
        "weixin:chat=chat-1:s_fixed-uuid",
        "#1328 /new allocates the s_ session"
    );
    assert!(matches!(rotated.command, Some(WeixinCommand::New)));
    assert!(
        rotated.message.units().is_empty(),
        "#1328 bare /new has no message"
    );

    let follow_up = mapper.resolve(&js("chat-1"), &js("next"));
    assert_eq!(
        text_of(&follow_up.session_key),
        "weixin:chat=chat-1:s_fixed-uuid",
        "#1328 follow-up text lands in the rotated session"
    );
}

#[test]
fn mapper_new_command_strips_prefix_from_message() {
    let mapper = WeixinSessionMapper::with_uuid(Arc::new(|| js("u")));
    let result = mapper.resolve(&js("c"), &js("  /new   hello world  "));
    assert!(
        matches!(result.command, Some(WeixinCommand::New)),
        "#1328 /new with leading whitespace still routes"
    );
    assert_eq!(
        text_of(&result.message),
        "hello world",
        "#1328 /new payload trims the command prefix"
    );
    // `new ` without the slash is plain text.
    let plain = mapper.resolve(&js("c2"), &js("new session"));
    assert!(plain.command.is_none(), "#1328 no slash, no command");
}

#[test]
fn mapper_state_round_trip_keeps_snapshot_order() {
    let mapper = WeixinSessionMapper::with_uuid(Arc::new(|| js("u1")));
    mapper.resolve(&js("chat-a"), &js("/new"));
    mapper.bind_project(&js("chat-a"), &js("proj"));
    let snapshot = mapper.snapshot();
    let restored = WeixinSessionMapper::from_saved_with_uuid(&snapshot, Arc::new(|| js("u2")));
    let after = restored.resolve(&js("chat-a"), &js("text"));
    assert_eq!(
        text_of(&after.session_key),
        "weixin:chat=chat-a:s_u1",
        "#1328 restored active session survives"
    );
    assert_eq!(
        after.project_key.as_ref().map(text_of),
        Some("proj".into()),
        "#1328 restored project binding survives"
    );
    let keys: Vec<String> = snapshot
        .as_object()
        .expect("#1328 snapshot is an object")
        .entries()
        .iter()
        .map(|(key, _)| text_of(key))
        .collect();
    assert_eq!(
        keys,
        vec!["activeByChatId".to_string(), "projectByChatId".to_string()],
        "#1328 snapshot field order"
    );
}

#[test]
fn pretty_json_matches_node_stringify() {
    let mut inner = JsObject::new();
    inner.insert("d".into(), JsValue::Null);
    let mut array = JsObject::new();
    array.insert("a".into(), JsValue::Number(1.0));
    array.insert("b".into(), JsValue::Array(vec![]));
    array.insert("c".into(), JsValue::Object(inner));
    let text = state::stringify_pretty(&JsValue::Object(array));
    assert_eq!(
        text, "{\n  \"a\": 1,\n  \"b\": [],\n  \"c\": {\n    \"d\": null\n  }\n}",
        "#1328 JSON.stringify(value, null, 2) exact shape"
    );
}

#[tokio::test]
async fn persistence_debounces_then_writes_atomically() {
    let root = Root::new();
    let persistence = ChannelStatePersistence::with_debounce(root.path(), 1000);
    let key = js("weixin");
    let mut state = JsObject::new();
    state.insert("activeByChatId".into(), JsValue::Object(JsObject::new()));
    persistence.save(&key, JsValue::Object(state));

    // Before the debounce elapses, only a flush lands the write.
    let file = root.path().join("weixin.state.json");
    assert!(!file.exists(), "#1328 debounced write has not landed yet");
    persistence.flush().await.expect("#1328 flush writes");
    let raw = std::fs::read_to_string(&file).expect("#1328 state file");
    assert_eq!(
        raw, "{\n  \"activeByChatId\": {}\n}",
        "#1328 persisted state matches the TS JSON shape"
    );
    let loaded = persistence
        .load(&key)
        .await
        .expect("#1328 load after flush");
    assert!(
        loaded.get("activeByChatId").is_some(),
        "#1328 loaded state round-trips"
    );
}

#[tokio::test]
async fn persistence_debounce_lands_without_flush() {
    let root = Root::new();
    let persistence = ChannelStatePersistence::with_debounce(root.path(), 5);
    let mut state = JsObject::new();
    state.insert("x".into(), JsValue::Number(2.0));
    persistence.save(&js("weixin"), JsValue::Object(state));
    let file = root.path().join("weixin.state.json");
    for _ in 0..100 {
        if file.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let raw = std::fs::read_to_string(&file).expect("#1328 debounced write landed");
    assert_eq!(raw, "{\n  \"x\": 2\n}", "#1328 debounced content");
}

#[tokio::test]
async fn persistence_flush_drains_dirty_keys() {
    let root = Root::new();
    let persistence = ChannelStatePersistence::with_debounce(root.path(), 60_000);
    let mut state = JsObject::new();
    state.insert("k".into(), JsValue::Number(1.0));
    persistence.save(&js("weixin"), JsValue::Object(state));
    // A short debounce replaced by an immediate flush must still write.
    persistence.flush().await.expect("#1328 flush drains dirty");
    let raw = std::fs::read_to_string(root.path().join("weixin.state.json"))
        .expect("#1328 flushed file exists");
    assert!(raw.contains("\"k\": 1"), "#1328 flushed content: {raw}");
}

fn state_with_value(value: f64) -> JsValue {
    let mut state = JsObject::new();
    state.insert("v".into(), JsValue::Number(value));
    JsValue::Object(state)
}

/// The assertions of the two `#2073` tests run on the libtest thread, after
/// `block_on`, because a receipt is keyed by the failing thread's name and a
/// multi-threaded runtime names its workers `tokio-runtime-worker`. The
/// observations are therefore collected inside the runtime and asserted
/// outside it.
fn multi_thread_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("#2073 multi-thread runtime")
}

fn read_state(file: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(file).ok()
}

/// #2073: how long a `flush` may take before the test calls it a wedge. Shared
/// by the orphaned-entry check, the "still persists" check and the retry in the
/// abandoned-drain test, so tuning one does not silently drift from the others.
const FLUSH_DEADLINE: Duration = Duration::from_secs(5);

/// #2073: how long the test waits for a debounce timer to reach its park point.
/// Must stay well above the debounce it is paired with.
const PARK_WAIT: Duration = Duration::from_secs(30);

/// #2073: wall-clock bound on waiting for a superseded timer to retire. An
/// iteration count is not a bound on elapsed time — every `sleep(1ms)` may
/// overshoot under load — and the observation window below must stay narrower
/// than the debounce for an early write to prove anything.
const SETTLE_WINDOW: Duration = Duration::from_secs(2);

/// #2073 P0: a debounce timer that has already woken up must retire itself
/// when a newer `save` supersedes it — and must not lose the newest state.
///
/// Reachable only on a multi-threaded runtime: `save` aborts the old
/// `JoinHandle`, tokio records the cancellation, but the task is parked
/// between `sleep` and the state lock — so it still walks the synchronous
/// section (deletes the *new* timer handle, registers `in_flight`, claims the
/// newest state in `drain_writes`) and is only cancelled at the first await
/// inside `write_to_disk`, after the state has left `dirty`.
#[test]
fn superseded_debounce_timer_must_not_drop_the_newest_state() {
    struct Observed {
        settled: bool,
        early_write: bool,
        flushed: bool,
        after_flush: Option<String>,
        later_flushed: bool,
        after_later: Option<String>,
    }
    let observed = multi_thread_runtime().block_on(async {
        let root = Root::new();
        // An early write can only be blamed on the superseded timer if the
        // *live* timer cannot fire before `early_write` is sampled below. That
        // sample happens right after the settle loop, which `SETTLE_WINDOW`
        // bounds by the wall clock — so a 5 s debounce keeps a 3 s margin no
        // matter how far each `sleep(1ms)` overshoots. The `flush` calls come
        // later and are not part of the window: `flush` retires live timers by
        // sequence in the lock, before its first await.
        let persistence = ChannelStatePersistence::with_debounce(root.path(), 5_000);
        let key = js("weixin");
        let file = root.path().join("weixin.state.json");

        // Arm the rendezvous, then start a timer that parks as soon as it wakes.
        let entered = persistence.park_next_timer();
        persistence.save(&key, state_with_value(1.0));
        tokio::task::spawn_blocking(move || {
            entered
                .recv_timeout(PARK_WAIT)
                .expect("#2073 the debounce timer must reach the park point")
        })
        .await
        .expect("#2073 park waiter");

        // Supersede the parked timer and let it settle.
        let exits = persistence.timer_exits();
        persistence.save(&key, state_with_value(2.0));
        persistence.release_parked_timer();
        let mut settled = persistence.timer_exits() > exits;
        let settle_deadline = std::time::Instant::now() + SETTLE_WINDOW;
        while !settled && std::time::Instant::now() < settle_deadline {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            settled = persistence.timer_exits() > exits;
        }

        // A superseded timer must not write: the live timer's debounce still has
        // seconds to go, and TS would not have written either.
        let early_write = read_state(&file).is_some();

        // The newest state must land; pre-fix it was claimed and then dropped,
        // and `flush` waited forever on the orphaned in-flight entry. Both the
        // deadline and the inner `Result` are checked: a flush that returns
        // `Err` quickly has not written either.
        let flushed = tokio::time::timeout(FLUSH_DEADLINE, persistence.flush())
            .await
            .is_ok_and(|result| result.is_ok());
        let after_flush = read_state(&file);

        // And the key must not be wedged: a later save still persists.
        persistence.save(&key, state_with_value(3.0));
        let later_flushed = tokio::time::timeout(FLUSH_DEADLINE, persistence.flush())
            .await
            .is_ok_and(|result| result.is_ok());
        Observed {
            settled,
            early_write,
            flushed,
            after_flush,
            later_flushed,
            after_later: read_state(&file),
        }
    });
    crate::negative_assert!(observed.settled, "#2073 the superseded timer must settle");
    crate::negative_assert!(
        !observed.early_write,
        "#2073 a superseded timer must not write before the debounce elapses"
    );
    crate::negative_assert_eq!(
        observed.after_flush.as_deref(),
        Some("{\n  \"v\": 2\n}"),
        "#2073 the newest state must reach disk"
    );
    crate::negative_assert!(
        observed.flushed,
        "#2073 flush must not wait on an orphaned in-flight entry"
    );
    crate::negative_assert!(
        observed.later_flushed,
        "#2073 flush must not hang after a superseded timer"
    );
    crate::negative_assert_eq!(
        observed.after_later.as_deref(),
        Some("{\n  \"v\": 3\n}"),
        "#2073 the key must keep persisting after a superseded timer"
    );
}

/// #2073 P0 (second half): a drain whose task is dropped mid-write must not
/// leave an in-flight entry that wedges the key. The dropped drain restores
/// the state it claimed, and the waiter that was subscribed to it starts a
/// fresh drain instead of reporting a success that never wrote (pre-fix the
/// entry stayed forever and every later `save`/`flush` waited on it).
#[test]
fn abandoned_drain_restores_the_claimed_state_and_waiters_retry() {
    struct Observed {
        waiters: usize,
        cancelled: bool,
        /// `None` means the waiting flush never returned (the wedge);
        /// `Some(false)` means it returned with an error or panicked — neither
        /// is the hang this test is about.
        second: Option<bool>,
        content: Option<String>,
    }
    let observed = multi_thread_runtime().block_on(async {
        let root = Root::new();
        let persistence = ChannelStatePersistence::with_debounce(root.path(), 60_000);
        let key = js("weixin");
        let file = root.path().join("weixin.state.json");
        persistence.save(&key, state_with_value(5.0));

        // The first flush parks inside the drain, holding the claimed state.
        let gate = persistence.park_next_drain();
        let first = tokio::spawn({
            let persistence = persistence.clone();
            async move { persistence.flush().await }
        });
        gate.entered.notified().await;

        // A second flush joins the in-flight entry instead of starting its own.
        let second = tokio::spawn({
            let persistence = persistence.clone();
            async move { persistence.flush().await }
        });
        let mut waiters = persistence.in_flight_waiters(&key);
        for _ in 0..2_000 {
            if waiters == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            waiters = persistence.in_flight_waiters(&key);
        }

        // Drop the drainer mid-write; the waiter must recover, not wedge.
        first.abort();
        let cancelled = matches!(first.await, Err(error) if error.is_cancelled());
        let second = match tokio::time::timeout(FLUSH_DEADLINE, second).await {
            // Only the timeout is the wedge. A panic (join error) and a flush
            // error are both `Some(false)`: the inner `Result` must be `Ok`, or
            // "the retry writes" would pass on a retry that wrote nothing.
            Ok(joined) => Some(joined.is_ok_and(|result| result.is_ok())),
            Err(_) => None,
        };
        Observed {
            waiters,
            cancelled,
            second,
            content: read_state(&file),
        }
    });
    crate::negative_assert_eq!(
        observed.waiters,
        1,
        "#2073 the second flush must wait on the in-flight drain"
    );
    crate::negative_assert!(
        observed.cancelled,
        "#2073 the parked drain must be cancelled, not completed"
    );
    crate::negative_assert!(
        observed.second.is_some(),
        "#2073 the waiting flush must not hang on an abandoned entry"
    );
    crate::negative_assert!(observed.second == Some(true), "#2073 the retry writes");
    crate::negative_assert_eq!(
        observed.content.as_deref(),
        Some("{\n  \"v\": 5\n}"),
        "#2073 the claimed state must not be lost with the dropped drain"
    );
}

#[test]
fn recoverable_poll_error_matches_ts_substrings() {
    for detail in [
        "TypeError: fetch failed",
        "Error: ECONNRESET",
        "Error: etimedout while polling",
        "Error: socket hang up",
        "Error: network changed",
    ] {
        assert!(
            is_recoverable_poll_error(detail),
            "#1328 recoverable: {detail}"
        );
    }
    for detail in ["Error: something else", "SyntaxError: bad json"] {
        assert!(
            !is_recoverable_poll_error(detail),
            "#1328 not recoverable: {detail}"
        );
    }
}

#[test]
fn credentials_defaults_follow_ts_path() {
    let path = default_credentials_path();
    let text = path.to_string_lossy().to_string();
    assert!(
        text.ends_with(".pilotdeck/weixin-credentials.json"),
        "#1328 default credentials path: {text}"
    );
}

// ── #1821 turn-timeout watchdog integration ────────────────────────────────

/// One recorded `abortTurn` call: `(sessionKey, runId, reason)`.
type AbortRecord = (String, Option<String>, Option<String>);

/// A gateway port whose `submitTurn` streams `turn_started` and then never
/// resolves: the only way the turn can end is the live-reply watchdog firing
/// on the paused clock, so the recorded `abortTurn` calls observe the wiring
/// alone (`WeixinChannel.ts:692-702`).
struct TimeoutPort {
    aborts: Mutex<Vec<AbortRecord>>,
    senders: Mutex<Vec<tokio::sync::mpsc::UnboundedSender<WeixinStreamItem>>>,
}

impl TimeoutPort {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            aborts: Mutex::new(Vec::new()),
            senders: Mutex::new(Vec::new()),
        })
    }

    fn aborts(&self) -> Vec<AbortRecord> {
        self.aborts.lock().expect("#1821 aborts lock").clone()
    }
}

impl WeixinGatewayPort for TimeoutPort {
    fn submit_turn(&self, _input: JsValue) -> WeixinTurnStream {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        sender
            .send(WeixinStreamItem::Event(
                parse_json(r#"{"type":"turn_started","runId":"run-9"}"#)
                    .expect("#1821 turn_started event json"),
            ))
            .expect("#1821 turn_started handoff");
        // The sender stays alive in `senders`, so the stream stays open and
        // `next()` never resolves after the first event.
        self.senders
            .lock()
            .expect("#1821 senders lock")
            .push(sender);
        WeixinTurnStream {
            receiver,
            receipt: None,
        }
    }

    fn abort_turn<'a>(
        &'a self,
        session_key: &'a JsString,
        run_id: Option<&'a JsString>,
        reason: Option<&'a str>,
    ) -> WeixinAbortFuture<'a> {
        let record = (
            text_of(session_key),
            run_id.map(text_of),
            reason.map(str::to_string),
        );
        self.aborts.lock().expect("#1821 aborts lock").push(record);
        Box::pin(async { Ok(()) })
    }
}

fn timeout_channel(port: Arc<TimeoutPort>, turn_timeout_ms: u64, root: &Root) -> WeixinChannel {
    let mut options = WeixinChannelOptions::with_login_base_url("http://127.0.0.1:9".into());
    options.credentials_path = root.path().join("weixin-credentials.json");
    options.credentials_dir = Some(root.path());
    options.gateway = Some(port);
    options.live_reply = LiveReplyOptions {
        turn_timeout_ms,
        ..weixin_live_reply_options()
    };
    WeixinChannel::new(options)
}

/// A pr1 text message from `chat-1` (message_type 1, one text item).
fn inbound_text_message(text: &str) -> JsValue {
    parse_json(&format!(
        r#"{{"from_user_id":"chat-1","context_token":"tok-1","message_type":1.0,
            "item_list":[{{"type":1.0,"text_item":{{"text":"{text}"}}}}]}}"#
    ))
    .expect("#1821 inbound message json")
}

/// A FILE item whose declared size is past the store limit, so `saveFromUrl`
/// rejects it before it opens a socket.
fn oversize_file_message(chat: &str, token: &str, name: &str, cdn: &str) -> JsValue {
    parse_json(&format!(
        r#"{{"from_user_id":"{chat}","context_token":"{token}","message_type":1.0,
            "item_list":[{{"type":4.0,"file_item":{{"file_name":"{name}",
              "file_size":52428801.0,"cdn_url":"http://{cdn}/{name}"}}}}]}}"#
    ))
    .expect("#1822 oversize file message json")
}

/// Give the spawned turn enough scheduler rounds to reach its parked select
/// (the paused runtime fires or skips the armed watchdog on its own once no
/// task is runnable, so the assertions below never depend on poll order).
async fn let_the_turn_settle() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn turn_timeout_watchdog_aborts_with_system_timeout_reason() {
    let root = Root::new();
    let port = TimeoutPort::new();
    let channel = timeout_channel(port.clone(), 20, &root);
    let Some(start) = channel.dispatch_sync_prefix(&inbound_text_message("hello")) else {
        panic!("#1821 the inbound message dispatches a turn");
    };
    channel.spawn_dispatch(start);
    let_the_turn_settle().await;
    tokio::time::advance(Duration::from_millis(20)).await;
    let_the_turn_settle().await;
    crate::negative_assert_eq!(
        port.aborts(),
        vec![(
            "weixin:chat=chat-1:general".to_string(),
            Some("run-9".to_string()),
            Some("system:timeout".to_string()),
        )],
        "#1821 weixin turn timeout watchdog aborts with the system timeout reason"
    );
    // `markTimedOut` sends the weixin timeout notice through the transport
    // `send` (queue-on-failure); with no client it parks in pendingReplies.
    let pending = channel
        .lock_inner()
        .pending_replies
        .get(&js("chat-1"))
        .expect("#1821 timeout notice queued")
        .clone();
    crate::negative_assert_eq!(
        pending.len(),
        1,
        "#1821 weixin turn timeout watchdog queues exactly one timeout notice"
    );
    crate::negative_assert_eq!(
        text_of(&pending[0]),
        WEIXIN_TIMEOUT_FINAL_TEXT.to_string(),
        "#1821 weixin turn timeout watchdog queues the weixin timeout notice"
    );
}

#[tokio::test(start_paused = true)]
async fn zero_turn_timeout_never_aborts_the_turn() {
    let root = Root::new();
    let port = TimeoutPort::new();
    let channel = timeout_channel(port.clone(), 0, &root);
    let Some(start) = channel.dispatch_sync_prefix(&inbound_text_message("hello")) else {
        panic!("#1821 the inbound message dispatches a turn");
    };
    channel.spawn_dispatch(start);
    let_the_turn_settle().await;
    tokio::time::advance(Duration::from_millis(60_000)).await;
    let_the_turn_settle().await;
    crate::negative_assert!(
        port.aborts().is_empty(),
        "#1821 weixin zero turn timeout never aborts the turn"
    );
}

/// #1780: the first `submit_turn` panics (standing in for any panic inside
/// `process_message` — poisoned lock recovery, a JS-value conversion, …); the
/// second call returns an empty stream so a re-admitted chat completes.
struct PanicOnceGateway {
    submits: AtomicUsize,
    panicked: Arc<tokio::sync::Notify>,
}
impl WeixinGatewayPort for PanicOnceGateway {
    fn submit_turn(&self, _input: JsValue) -> WeixinTurnStream {
        let call = self.submits.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            // The test waits on this before asserting the marker is gone; the
            // panic itself unwinds the spawned task.
            self.panicked.notify_one();
            panic!("#1780 injected panic inside process_message");
        }
        let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        WeixinTurnStream {
            receiver,
            receipt: None,
        }
    }
    fn abort_turn<'a>(
        &'a self,
        _session_key: &'a JsString,
        _run_id: Option<&'a JsString>,
        _reason: Option<&'a str>,
    ) -> WeixinAbortFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}

fn queued_turn(message: &str) -> QueuedWeixinTurn {
    QueuedWeixinTurn {
        session_key: js("weixin:chat=chat-panic:general"),
        message: js(message),
        project_key: None,
        attachments: Vec::new(),
    }
}

fn channel_with_gateway(root: &Root, gateway: Arc<dyn WeixinGatewayPort>) -> WeixinChannel {
    let mut options = WeixinChannelOptions::with_login_base_url("http://127.0.0.1:9".into());
    options.credentials_path = root.path().join("weixin-credentials.json");
    options.credentials_dir = Some(root.path().to_path_buf());
    options.gateway = Some(gateway);
    WeixinChannel::new(options)
}

async fn until<F: FnMut() -> bool>(what: &str, mut ready: F) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if ready() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("#1780 timed out waiting for {what}"));
}

/// `until` for a wait a mutation row is bound to: the deadline death must
/// carry the row's registered assertion (a plain `panic!` writes no nonce
/// receipt, and the host matrix would judge the kill INVALID) — `what` only
/// reaches stderr, same shape as feishu's `until`.
async fn until_bound(assertion: &str, what: &str, mut ready: impl FnMut() -> bool) {
    match tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if ready() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    {
        Ok(()) => {}
        Err(_) => {
            eprintln!("#1725 timeout waiting for {what}");
            crate::negative_assert!(false, "{assertion}");
        }
    }
}

#[tokio::test]
async fn panicking_turn_releases_active_chat_slot() {
    let root = Root::new();
    let panicked = Arc::new(tokio::sync::Notify::new());
    let gateway = Arc::new(PanicOnceGateway {
        submits: AtomicUsize::new(0),
        panicked: panicked.clone(),
    });
    let channel = channel_with_gateway(&root, gateway.clone());
    let user = js("chat-panic");

    let _ = channel.enqueue_or_run(user.clone(), queued_turn("first"));
    // Positive control: `enqueue_or_run` admits the chat synchronously, before
    // the task is spawned. Without this, a regression that drops the admission
    // insert would leave the checks below vacuously green.
    assert!(
        channel.lock_inner().active_chats.contains(&user),
        "#1780 the chat is admitted before the turn runs"
    );
    tokio::time::timeout(Duration::from_secs(5), panicked.notified())
        .await
        .expect("#1780 the injected process_message panic never fired");

    // (a) A panic that kills the spawned turn task must still release the
    // admission marker; otherwise the chat stays "active" with no consumer.
    until("active_chats to release the panicked chat", || {
        !channel.lock_inner().active_chats.contains(&user)
    })
    .await;

    // (b) The user's next message must be consumed immediately instead of
    // queueing behind the dead task forever.
    let _ = channel.enqueue_or_run(user.clone(), queued_turn("second"));
    until("the next message to reach submit_turn", || {
        gateway.submits.load(Ordering::SeqCst) >= 2
    })
    .await;
}

#[tokio::test]
async fn turn_cancelled_before_first_poll_releases_active_chat_slot() {
    let root = Root::new();
    let panicked = Arc::new(tokio::sync::Notify::new());
    let gateway = Arc::new(PanicOnceGateway {
        submits: AtomicUsize::new(0),
        panicked: panicked.clone(),
    });
    let channel = channel_with_gateway(&root, gateway.clone());
    let user = js("chat-cancel");

    let handle = channel
        .enqueue_or_run(user.clone(), queued_turn("first"))
        .expect("#1780 a fresh chat runs the turn immediately");
    // Positive control: the admission marker exists before the cancellation;
    // otherwise "released" below would be vacuous.
    assert!(
        channel.lock_inner().active_chats.contains(&user),
        "#1780 the chat is admitted before the task is cancelled"
    );
    // Nothing has been awaited since the spawn, so on the current-thread test
    // runtime the task has not been polled yet. Aborting here drops its future
    // un-polled — the same shape as a runtime shutdown cancelling a task that
    // is still queued. The admission marker was inserted by `enqueue_or_run`'s
    // prelude, before the spawn, so a guard built inside the async body would
    // never exist and the chat would stay active with no consumer.
    handle.abort();
    assert!(
        handle.await.unwrap_err().is_cancelled(),
        "#1780 the turn task is cancelled before its first poll"
    );
    assert_eq!(
        gateway.submits.load(Ordering::SeqCst),
        0,
        "#1780 the cancelled task never reached submit_turn"
    );
    assert!(
        !channel.lock_inner().active_chats.contains(&user),
        "#1780 a task cancelled before its first poll still releases the chat"
    );
}

#[tokio::test]
async fn guard_releases_the_chat_through_a_poisoned_lock() {
    let root = Root::new();
    let gateway = Arc::new(PanicOnceGateway {
        submits: AtomicUsize::new(0),
        panicked: Arc::new(tokio::sync::Notify::new()),
    });
    let channel = channel_with_gateway(&root, gateway);
    let user = js("chat-poison");

    channel.lock_inner().active_chats.insert(&user);
    // Poison the channel's `Inner` mutex the way a panic inside
    // `process_message` does: unwind while the guard is held. The channel-wide
    // `Drop` must then recover through `lock_inner` instead of aborting the
    // process on a second panic inside a destructor.
    let poison = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = channel.lock_inner();
        panic!("#1780 poison the channel lock");
    }));
    assert!(
        poison.is_err() && channel.0.inner.is_poisoned(),
        "#1780 the injected panic poisons the channel lock"
    );

    drop(ActiveChatSlot {
        channel: channel.clone(),
        user: user.clone(),
        armed: true,
    });
    assert!(
        !channel.lock_inner().active_chats.contains(&user),
        "#1780 the guard releases the chat even when the lock is poisoned"
    );
}

/// Records the message text of every `submit_turn` and ends the turn at once,
/// so a queued follow-up can run.
#[derive(Default)]
struct RecordingGateway {
    submits: std::sync::Mutex<Vec<String>>,
}

impl RecordingGateway {
    fn submits(&self) -> Vec<String> {
        self.submits.lock().expect("#1822 submits lock").clone()
    }
}

impl WeixinGatewayPort for RecordingGateway {
    fn submit_turn(&self, input: JsValue) -> WeixinTurnStream {
        let message = input
            .get("message")
            .and_then(JsValue::as_string)
            .map(text_of)
            .unwrap_or_default();
        self.submits
            .lock()
            .expect("#1822 submits lock")
            .push(message);
        let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        WeixinTurnStream {
            receiver,
            receipt: None,
        }
    }

    fn abort_turn<'a>(
        &'a self,
        _session_key: &'a JsString,
        _run_id: Option<&'a JsString>,
        _reason: Option<&'a str>,
    ) -> WeixinAbortFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}

/// `WeixinChannel.ts:379-383` walks the whole `msgs` array synchronously
/// (`for (const msg of msgs) void this.dispatchMessage(msg)`), and Node drains
/// no microtask until that loop returns. An attachment message that fails
/// without I/O (here: the declared-size guard) therefore cannot reach the
/// session ahead of a plain-text message later in the same batch — the text
/// one resolves first because its extraction never suspends. Starting the
/// downloads only after the batch has been walked keeps that order instead of
/// racing a spawned task against the loop.
///
/// Multi-threaded on purpose: a current-thread runtime cannot run a spawned
/// task while `start_poll_batch` is still walking the batch, so it would pass
/// even with the per-message `spawn` this test guards against.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_later_text_message_in_the_same_batch_reaches_the_session_first() {
    let root = Root::new();
    let gateway = Arc::new(RecordingGateway::default());
    let channel = channel_with_gateway(&root, gateway.clone());
    channel
        .start_poll_batch(vec![
            oversize_file_message("chat-1", "tok-1", "big.bin", "127.0.0.1:9"),
            inbound_text_message("after"),
        ])
        .await;
    until("both batch messages submit", || {
        gateway.submits().len() == 2
    })
    .await;
    let submits = gateway.submits();
    crate::negative_assert_eq!(
        submits[0],
        "after",
        "#1822 the later text message of a batch submits first"
    );
    crate::negative_assert!(
        submits[1].starts_with("[Attachment diagnostics]\n- 微信附件处理失败："),
        "#1822 the instantly failing attachment follows the text"
    );
}

/// Two attachments the store rejects before it opens a socket reach the same
/// chat in *batch* order, like two already-settled promises: Node queues their
/// continuations in the order `dispatchMessage` awaited them. Without the
/// pre-transport phase both would be spawned and their admission order would
/// be left to the scheduler.
///
/// Same chat on purpose: `enqueue_or_run` admits the first turn inline and
/// queues the second behind it, and a chat's marker is only released once its
/// turn *completed* (i.e. after its submit), so no interleaving lets the
/// second message reach the gateway first. Across two different chats the two
/// turns are independent and the gateway call order follows task start order,
/// which this port does not serialise — see the fixture README.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_instantly_failing_attachments_keep_their_batch_order() {
    let root = Root::new();
    let gateway = Arc::new(RecordingGateway::default());
    let channel = channel_with_gateway(&root, gateway.clone());
    channel
        .start_poll_batch(vec![
            oversize_file_message("chat-1", "tok-1", "first.bin", "127.0.0.1:9"),
            oversize_file_message("chat-1", "tok-1", "second.bin", "127.0.0.1:9"),
        ])
        .await;
    until("both failing attachments submit", || {
        gateway.submits().len() == 2
    })
    .await;
    let submits = gateway.submits();
    crate::negative_assert!(
        submits[0].contains("first.bin"),
        "#1822 the first instantly failing attachment submits first"
    );
    crate::negative_assert!(
        submits[1].contains("second.bin"),
        "#1822 the second instantly failing attachment submits second"
    );
}

/// `WeixinChannel.ts:561-564`: `if (item.text_item?.text)` is a JS truthiness
/// test and `textParts.join("")` stringifies whatever it holds, so a numeric
/// text item still becomes message text; only falsy values skip it.
#[test]
fn a_non_string_text_item_still_becomes_message_text() {
    let texts = |body: &str| -> Vec<String> {
        let message = parse_json(body).expect("#1822 text item json");
        plan_incoming_items(&message)
            .into_iter()
            .filter_map(|action| match action {
                IncomingAction::Text(text) => Some(String::from_utf16_lossy(text.units())),
                _ => None,
            })
            .collect()
    };
    crate::negative_assert_eq!(
        texts(r#"{"item_list":[{"type":1.0,"text_item":{"text":5}}]}"#),
        vec!["5".to_string()],
        "#1822 a numeric text item is stringified, not dropped"
    );
    crate::negative_assert_eq!(
        texts(r#"{"item_list":[{"type":1.0,"text_item":{"text":true}}]}"#),
        vec!["true".to_string()],
        "#1822 a boolean text item is stringified, not dropped"
    );
    for body in [
        r#"{"item_list":[{"type":1.0,"text_item":{"text":0}}]}"#,
        r#"{"item_list":[{"type":1.0,"text_item":{"text":""}}]}"#,
        r#"{"item_list":[{"type":1.0,"text_item":{"text":false}}]}"#,
        r#"{"item_list":[{"type":1.0,"text_item":{"text":null}}]}"#,
    ] {
        crate::negative_assert!(texts(body).is_empty(), "#1822 falsy text item is skipped");
    }
}

/// Only the declared-size guard settles without I/O; anything that feeds the
/// transport, and anything without a download, must take the spawned path.
#[test]
fn only_the_declared_size_guard_settles_without_io() {
    let max = crate::channel::im_attachment::DEFAULT_MAX_BYTES;
    let oversize = parse_json(
        r#"{"item_list":[{"type":4.0,"file_item":{"file_name":"big.bin",
            "file_size":52428801.0,"cdn_url":"http://127.0.0.1:9/big.bin"}}]}"#,
    )
    .expect("#1822 oversize file json");
    crate::negative_assert!(
        settles_without_io(&oversize, max),
        "#1822 an over-limit file is rejected before the transport"
    );
    for (tag, body) in [
        (
            "a file under the limit still downloads",
            r#"{"item_list":[{"type":4.0,"file_item":{"file_name":"ok.bin",
                "file_size":10.0,"cdn_url":"http://127.0.0.1:9/ok.bin"}}]}"#,
        ),
        (
            "an image has no declared size",
            r#"{"item_list":[{"type":2.0,"image_item":{"cdn_url":"http://127.0.0.1:9/a.jpg"}}]}"#,
        ),
        (
            "an over-limit file next to an image still downloads",
            r#"{"item_list":[{"type":2.0,"image_item":{"cdn_url":"http://127.0.0.1:9/a.jpg"}},
                {"type":4.0,"file_item":{"file_name":"big.bin","file_size":52428801.0,
                  "cdn_url":"http://127.0.0.1:9/big.bin"}}]}"#,
        ),
        (
            "a text-only message has no download at all",
            r#"{"item_list":[{"type":1.0,"text_item":{"text":"hi"}}]}"#,
        ),
    ] {
        let message = parse_json(body).expect("#1822 predicate json");
        crate::negative_assert!(!settles_without_io(&message, max), "#1822 {tag}");
    }
}

// ── #1725 S2：并发收口 pins ─────────────────────────────────────────────

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

/// W1: the drainer that empties its queue gives the marker back in the same
/// critical section as the final `shift_turn` (#1725#4; TS `runQueuedTurns`
/// :544-547, the P4 probe anchor — no await between shift and the `finally`
/// delete). The observable half on a single thread: a chat whose queue
/// emptied is re-admitted immediately — a marker left behind would queue the
/// next message with nobody to drain it. The atomicity itself is a
/// cross-thread window with no await on either side of the fix, so it has no
/// deterministic single-thread construction (the same class as the feishu
/// resolve-fusion limitation registered in A8.3-CH-FEISHU-004-R2); the
/// mutation rows pin what is reachable.
#[tokio::test]
async fn drained_chat_is_re_admitted_without_a_leftover_marker() {
    let root = Root::new();
    let gateway = Arc::new(RecordingGateway::default());
    let channel = channel_with_gateway(&root, gateway.clone());
    let user = js("chat-w1");

    let _ = channel.enqueue_or_run(user.clone(), queued_turn("first"));
    until("the first turn to reach submit_turn", || {
        !gateway.submits().is_empty()
    })
    .await;
    until_bound(
        "#1725 a drained queue leaves the chat immediately re-admittable",
        "the drained chat's marker to be released",
        || !channel.lock_inner().active_chats.contains(&user),
    )
    .await;

    // The marker is free, so the next message is admitted and drained at once.
    let _ = channel.enqueue_or_run(user.clone(), queued_turn("second"));
    until(
        "the re-admitted chat's second turn to reach submit_turn",
        || gateway.submits().len() >= 2,
    )
    .await;
    crate::negative_assert_eq!(
        gateway.submits(),
        vec!["first".to_owned(), "second".to_owned()],
        "#1725 a drained queue leaves the chat immediately re-admittable"
    );
}

/// A poll client whose long-poll never returns: the stand-in for the request
/// a `stop()` can only wait on (the real client's request is bounded by the
/// ilink 35 s timeout; the fake has no bound, so only the channel-level stop
/// bound can end the wait). `entered` flips when the first poll parks, so the
/// test can tell "parked inside the request" from "never polled" — only the
/// former leaves the abort nothing to interrupt.
struct HangingPollClient {
    entered: Arc<AtomicBool>,
}
impl WeixinIlinkClient for HangingPollClient {
    fn cursor(&self) -> String {
        String::new()
    }
    fn set_cursor(&self, _cursor: &str) {}
    fn poll(&self) -> BoxFuture<Result<JsValue, WeixinClientError>> {
        self.entered.store(true, Ordering::SeqCst);
        Box::pin(std::future::pending())
    }
    fn send_text_chunked(
        &self,
        _to_user_id: &JsString,
        _text: &JsString,
        _context_token: &JsString,
        _max_length: usize,
    ) -> BoxFuture<Result<usize, WeixinClientError>> {
        Box::pin(async { Ok(1) })
    }
    fn send_typing(
        &self,
        _user_id: &JsString,
        _context_token: &JsString,
    ) -> BoxFuture<Result<(), WeixinClientError>> {
        Box::pin(async { Ok(()) })
    }
}

fn poll_channel_with_log(
    root: &Root,
    logs: Arc<Mutex<Vec<String>>>,
    poll_entered: Arc<AtomicBool>,
) -> WeixinChannel {
    let mut options = WeixinChannelOptions::with_login_base_url("http://127.0.0.1:9".into());
    options.credentials_path = root.path().join("weixin-credentials.json");
    options.credentials_dir = Some(root.path().to_path_buf());
    options.client_factory = Arc::new(move |_credentials: &SavedCredentials| {
        Arc::new(HangingPollClient {
            entered: poll_entered.clone(),
        }) as Arc<dyn WeixinIlinkClient>
    });
    let channel = WeixinChannel::new(options);
    *channel.0.logger.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(Arc::new(move |level: &str, text: &str| {
            logs.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("{level}: {text}"));
        }));
    channel
}

fn saved_credentials() -> SavedCredentials {
    SavedCredentials {
        base_url: "http://127.0.0.1:9".into(),
        bot_token: "t-1".into(),
        account_id: "a-1".into(),
        cursor: None,
    }
}

/// W2/W3: after a `stop()` aborted the generation's cancellation, a late
/// `start_polling_with_credentials` (the recovery watcher's or a credentials
/// reload racing the stop) must not install a fresh poll behind the stopper's
/// back. TS cannot restart there — the stopper awaits the recovery promise,
/// and a settled `pollPromise` stays truthy until `stop` nulls it (the P1
/// probe: `polls total=1`, `after-stop pollPromise=null`).
#[tokio::test]
async fn stopped_channel_does_not_restart_the_poll_loop() {
    let root = Root::new();
    let channel = poll_channel_with_log(
        &root,
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(AtomicBool::new(false)),
    );
    channel.set_stop_wait_for_tests(Duration::from_millis(150));
    channel.start_polling_with_credentials(saved_credentials());
    crate::negative_assert!(
        channel.lock_inner().poll_task.is_some(),
        "#1725 the poll loop starts with credentials"
    );

    let stopped = channel.clone().stop("test").await;
    crate::negative_assert!(stopped.is_ok(), "#1725 the channel stops");
    crate::negative_assert!(
        channel.lock_inner().poll_task.is_none(),
        "#1725 stop takes the poll task with it"
    );

    // The late-recovery stand-in: the liveness checks it raced are all stale
    // now, so only the abort recheck inside the start can refuse.
    channel.start_polling_with_credentials(saved_credentials());
    crate::negative_assert!(
        channel.lock_inner().poll_task.is_none(),
        "#1725 a stopped channel does not restart its poll loop"
    );

    // The recheck refuses only the *aborted* generation: a deliberate restart
    // goes through `start()`, which installs a fresh cancellation before any
    // polling (mirrored here by replacing the cancellation directly).
    {
        let mut inner = channel.lock_inner();
        inner.abort = Some(Cancellation::new());
    }
    channel.start_polling_with_credentials(saved_credentials());
    crate::negative_assert!(
        channel.lock_inner().poll_task.is_some(),
        "#1725 a deliberate restart after a fresh cancellation still polls"
    );
    let _ = channel.stop("cleanup").await;
}

/// W6: `stop()`'s wait for the poll task is bounded (#1725#7). The fake poll
/// never returns, so only the channel-level bound can end the stop — and the
/// stopper must warn rather than hang.
#[tokio::test]
async fn stop_is_bounded_when_the_poll_never_returns() {
    let root = Root::new();
    let logs = Arc::new(Mutex::new(Vec::new()));
    let poll_entered = Arc::new(AtomicBool::new(false));
    let channel = poll_channel_with_log(&root, logs.clone(), poll_entered.clone());
    channel.set_stop_wait_for_tests(Duration::from_millis(150));
    channel.start_polling_with_credentials(saved_credentials());
    crate::negative_assert!(
        channel.lock_inner().poll_task.is_some(),
        "#1725 the hanging poll loop is running before the stop"
    );
    // Park the loop inside the request first: an un-polled task would see the
    // abort at its next poll and exit on its own, leaving nothing for the
    // stop bound to bound.
    until("the poll loop to park inside the hanging request", || {
        poll_entered.load(Ordering::SeqCst)
    })
    .await;

    // The outer bound is the test's own: a stop that lost its wait bound
    // would park on the hanging poll forever and die here, by name, instead
    // of hanging the suite.
    let stopped = crate::test_negative_receipt::expect_ok(
        tokio::time::timeout(Duration::from_secs(5), channel.stop("test")).await,
        "#1725 stop returns even when the poll loop never does",
    );
    crate::negative_assert!(stopped.is_ok(), "#1725 the bounded stop resolves");
    crate::negative_assert!(
        logs.lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|line| line.contains("stop timed out waiting for the poll loop")),
        "#1725 the bounded stop warns when it gives up waiting"
    );
}

/// W8: a turn admitted with no gateway attached is dropped with a warn, not
/// silently (TS `if (!this.gateway) return;` is silent; the log is the port's
/// addition so a misassembled channel surfaces).
#[tokio::test]
async fn turn_without_a_gateway_warns_and_drops() {
    let root = Root::new();
    let logs = Arc::new(Mutex::new(Vec::new()));
    let mut options = WeixinChannelOptions::with_login_base_url("http://127.0.0.1:9".into());
    options.credentials_path = root.path().join("weixin-credentials.json");
    options.credentials_dir = Some(root.path().to_path_buf());
    let channel = WeixinChannel::new(options);
    let sink = logs.clone();
    *channel.0.logger.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(Arc::new(move |level: &str, text: &str| {
            sink.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("{level}: {text}"));
        }));

    let _ = channel.enqueue_or_run(js("chat-nogw"), queued_turn("lost"));
    until("the gateway-less turn's warn to land", || {
        logs.lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|line| line.contains("no gateway attached, dropping turn"))
    })
    .await;
    // The admission marker is still given back — the drop is a turn loss, not
    // a dead chat.
    until("the gateway-less chat's marker to be released", || {
        !channel.lock_inner().active_chats.contains(&js("chat-nogw"))
    })
    .await;
}

/// W7 (rule): only the entry that is still *this* submission's receiver goes
/// when a task retires — a task that outlived a newer submission for the same
/// session key must leave the newer fence in place.
#[test]
fn retiring_an_in_flight_entry_spares_a_newer_submission() {
    let key = js("weixin:chat=c:s1:general");
    let mut in_flight = HashMap::new();
    let (_old_sender, old_rx) = watch::channel(false);
    let (_new_sender, new_rx) = watch::channel(false);
    in_flight.insert(key.clone(), new_rx.clone());

    retire_in_flight_entry(&mut in_flight, &key, &old_rx);
    crate::negative_assert_eq!(
        in_flight.len(),
        1,
        "#1725 the stale task's retirement spares the newer entry"
    );

    retire_in_flight_entry(&mut in_flight, &key, &new_rx);
    crate::negative_assert_eq!(
        in_flight.len(),
        0,
        "#1725 the current holder retires its own entry"
    );

    // Retiring into an absent key is a no-op, not a panic.
    retire_in_flight_entry(&mut in_flight, &key, &new_rx);
    crate::negative_assert_eq!(
        in_flight.len(),
        0,
        "#1725 retiring with no entry leaves the table untouched"
    );
}

/// The session factory behind the stub host: `normalize` short-circuits the
/// turn before any session is needed, so a factory that always fails is the
/// honest stub — reaching it would mean the port test lost control.
struct UnreachedFactory;
impl SessionFactory for UnreachedFactory {
    fn create(
        &self,
        _context: SessionContext,
        _previous: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        Box::pin(async {
            Err(RuntimeError::new(
                "#1725 port test: the session factory is never reached",
            ))
        })
    }
}

/// The stub host: `normalize` refuses every turn at once, so the submission
/// ends immediately and the port task reaches its in-flight retirement
/// without any session machinery behind it.
struct RefusingHost;
impl SubmitHost for RefusingHost {
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
        _input: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        Box::pin(async { Err(SubmitError::new("test", "refused at once")) })
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

/// W7 (wiring): the production port over a refusing submitter — the entry a
/// submission claimed must be retired when the submission ends, or every
/// session key pins its receiver forever (a `/new` rotates keys without
/// bound).
#[tokio::test]
async fn finished_submission_retires_its_in_flight_entry() {
    let port = NativeWeixinGatewayPort::new(Arc::new(TurnSubmitter {
        owner: SessionOwner::new(SessionOwnerOptions::new(Arc::new(UnreachedFactory))),
        coordinator: Arc::new(TurnEventCoordinator::new()),
        projector: Arc::new(
            GatewayAgentEventProjector::new(GatewayAgentEventProjectorOptions::default())
                .expect("#1725 port test projector"),
        ),
        registry: None,
        host: Arc::new(RefusingHost),
        workspace_admissions: WorkspaceAdmissions::default(),
    }));
    let input = parse_json(
        r#"{"sessionKey":"weixin:chat=c-w7:general","channelKey":"weixin","message":"hi"}"#,
    )
    .expect("#1725 port test input");

    let stream = port.submit_turn(input);
    crate::negative_assert_eq!(
        port.in_flight_len(),
        1,
        "#1725 the submission claims its in-flight entry"
    );
    let mut stream = stream;
    while let Some(_item) = stream.receiver.recv().await {}
    until_bound(
        "#1725 a finished submission stops occupying the in-flight table",
        "the finished submission's in-flight entry to retire",
        || port.in_flight_len() == 0,
    )
    .await;
    crate::negative_assert_eq!(
        port.in_flight_len(),
        0,
        "#1725 a finished submission stops occupying the in-flight table"
    );
}

// ── #1725#3：state.rs flush/waiter pins ──────────────────────────────────

/// A failing write target: the state file's path is a directory, so the
/// atomic rename in `write_to_disk` fails (`EISDIR`) — the same failure the
/// P3 probe manufactured in TS.
fn failing_key(root: &Root, key: &str) {
    std::fs::create_dir_all(root.path().join(format!("{key}.state.json")))
        .expect("#1725 failing key dir");
}

/// W9a: `flush` starts every key's drain before any rejection is observed —
/// TS `Promise.all(keys.map(ensureWrite))` (the P3 probe:
/// `flush started drains for: b, a — every key attempted before any
/// rejection: true`). A sequential `?` would return after the first failing
/// key and leave the later keys unwritten.
#[tokio::test]
async fn flush_attempts_every_dirty_key_before_reporting_the_first_failure() {
    let root = Root::new();
    let persistence = ChannelStatePersistence::with_debounce(root.path(), 60_000);
    failing_key(&root, "a");
    persistence.save(&js("a"), state_with_value(1.0));
    persistence.save(&js("b"), state_with_value(2.0));

    let outcome = persistence.flush().await;
    crate::negative_assert!(
        outcome.is_err(),
        "#1725 flush reports the failing key's write error"
    );
    crate::negative_assert!(
        read_state(&root.path().join("b.state.json")).is_some(),
        "#1725 the key after the failing one is still written"
    );
}

/// W9b: a waiter on an in-flight drain observes the drainer's failure — TS
/// hands the waiter the drainer's own promise, so both reject (the P3 probe:
/// `waiter rejected with the drainer's failure: true`). The drainer is parked
/// after it claimed the key (the `park_next_drain` seam), the waiter joins
/// while the entry is live, and only then does the write fail.
#[tokio::test]
async fn flush_waiter_observes_the_drainers_failure() {
    let root = Root::new();
    let persistence = ChannelStatePersistence::with_debounce(root.path(), 60_000);
    failing_key(&root, "a");
    persistence.save(&js("a"), state_with_value(1.0));

    let gate = persistence.park_next_drain();
    let drainer = persistence.clone();
    let drain_task = tokio::spawn(async move { drainer.flush().await });
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .expect("#1725 the drainer parked after claiming the key");
    // The entry is live and claimed; this flush joins it as a waiter.
    let waiter = persistence.clone();
    let wait_task = tokio::spawn(async move { waiter.flush().await });
    until("the waiter to join the parked drain", || {
        persistence.in_flight_waiters(&js("a")) >= 1
    })
    .await;

    gate.release.notify_one();
    let drainer_outcome = tokio::time::timeout(Duration::from_secs(5), drain_task)
        .await
        .expect("#1725 the drainer settles after the release")
        .expect("#1725 the drainer task joins");
    let waiter_outcome = tokio::time::timeout(Duration::from_secs(5), wait_task)
        .await
        .expect("#1725 the waiter settles after the drainer")
        .expect("#1725 the waiter task joins");
    crate::negative_assert!(
        drainer_outcome.is_err(),
        "#1725 the drainer reports the write failure"
    );
    crate::negative_assert!(
        waiter_outcome.is_err(),
        "#1725 the waiter observes the drainer's failure, not a success"
    );
}

/// A channel bound to a credentials file the test owns, with no polling —
/// `save_credentials`/`save_cursor` are synchronous fs and never touch the
/// client factory.
fn credentials_channel(root: &Root) -> WeixinChannel {
    let mut options = WeixinChannelOptions::with_login_base_url("http://127.0.0.1:9".into());
    options.credentials_path = root.path().join("weixin-credentials.json");
    options.credentials_dir = Some(root.path().to_path_buf());
    WeixinChannel::new(options)
}

/// #1725#2 (credentials half): the save holds the credentials lock for the
/// whole write (a save blocked on the lock writes nothing), the final bytes
/// are the TS `writeFileSync` shape byte for byte (same insertion-ordered
/// JSON.stringify(…, null, 2) object, no trailing newline — the tmp hop must
/// not change what lands), and the publish leaves no scratch file behind.
#[test]
fn save_credentials_takes_the_lock_and_publishes_atomically() {
    let root = Root::new();
    let channel = credentials_channel(&root);
    let path = root.path().join("weixin-credentials.json");

    // Holding the credentials lock pins the save's whole write: a concurrent
    // save cannot have written the file yet.
    let guard = channel
        .0
        .credentials_write
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let writer = {
        let channel = channel.clone();
        std::thread::spawn(move || channel.save_credentials(&saved_credentials()))
    };
    std::thread::sleep(Duration::from_millis(100));
    crate::negative_assert!(
        !path.exists(),
        "#1725 a credentials save blocked on the lock writes nothing"
    );
    drop(guard);
    writer
        .join()
        .expect("#1725 the blocked credentials save finishes");

    let raw = crate::test_negative_receipt::expect_ok(
        std::fs::read_to_string(&path),
        "#1725 credentials file",
    );
    assert_eq!(
        raw,
        "{\n  \"baseUrl\": \"http://127.0.0.1:9\",\n  \"botToken\": \"t-1\",\n  \"accountId\": \"a-1\"\n}",
        "#1725 the credentials bytes are the writeFileSync shape"
    );
    let leftovers: Vec<String> = std::fs::read_dir(root.path())
        .expect("#1725 credentials dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp"))
        .collect();
    crate::negative_assert!(
        leftovers.is_empty(),
        "#1725 the atomic credentials publish leaves no tmp file behind: {leftovers:?}"
    );
}

/// The fake client behind the cursor save: only `cursor` matters here — the
/// poll parks forever so nothing else in the channel moves while the test
/// drives the save by hand.
struct CursorClient(String);
impl WeixinIlinkClient for CursorClient {
    fn cursor(&self) -> String {
        self.0.clone()
    }
    fn set_cursor(&self, _cursor: &str) {}
    fn poll(&self) -> BoxFuture<Result<JsValue, WeixinClientError>> {
        Box::pin(std::future::pending())
    }
    fn send_text_chunked(
        &self,
        _to_user_id: &JsString,
        _text: &JsString,
        _context_token: &JsString,
        _max_length: usize,
    ) -> BoxFuture<Result<usize, WeixinClientError>> {
        Box::pin(async { Ok(1) })
    }
    fn send_typing(
        &self,
        _user_id: &JsString,
        _context_token: &JsString,
    ) -> BoxFuture<Result<(), WeixinClientError>> {
        Box::pin(async { Ok(()) })
    }
}

/// #1725#2 (cursor half): the cursor save's read-modify-write sits inside
/// the credentials lock — the same lock `save_credentials` holds, so the two
/// cannot interleave (one of the pair would be lost on any interleaving).
/// While the save is blocked on the lock, a concurrent full rewrite lands;
/// the save must re-read *after* acquiring the lock and publish the newer
/// credentials plus its cursor, not the snapshot it would have loaded before
/// the lock.
#[tokio::test]
async fn save_cursor_read_modify_write_holds_the_credentials_lock() {
    let root = Root::new();
    let mut options = WeixinChannelOptions::with_login_base_url("http://127.0.0.1:9".into());
    options.credentials_path = root.path().join("weixin-credentials.json");
    options.credentials_dir = Some(root.path().to_path_buf());
    options.client_factory = Arc::new(|_credentials: &SavedCredentials| {
        Arc::new(CursorClient("cur-1".into())) as Arc<dyn WeixinIlinkClient>
    });
    let channel = WeixinChannel::new(options);
    channel.set_stop_wait_for_tests(Duration::from_millis(150));
    channel.start_polling_with_credentials(saved_credentials());
    let path = root.path().join("weixin-credentials.json");
    std::fs::write(
        &path,
        "{\n  \"baseUrl\": \"http://127.0.0.1:9\",\n  \"botToken\": \"t-1\",\n  \"accountId\": \"a-1\"\n}",
    )
    .expect("#1725 seed credentials file");

    // Hold the credentials lock: the cursor save cannot run its
    // read-modify-write at all — the file still has t-1 and no cursor.
    let guard = channel
        .0
        .credentials_write
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let writer = {
        let channel = channel.clone();
        std::thread::spawn(move || channel.save_cursor())
    };
    std::thread::sleep(Duration::from_millis(100));
    let raw = std::fs::read_to_string(&path).expect("#1725 credentials file mid-save");
    crate::negative_assert!(
        !raw.contains("cursor"),
        "#1725 a cursor save blocked on the lock writes nothing"
    );
    crate::negative_assert!(
        raw.contains("t-1"),
        "#1725 the blocked save has not touched the file yet"
    );

    // The concurrent rewrite a `stop()`-side `save_credentials` would issue:
    // newer token, newer account, still no cursor.
    std::fs::write(
        &path,
        "{\n  \"baseUrl\": \"http://127.0.0.1:9\",\n  \"botToken\": \"t-2\",\n  \"accountId\": \"a-2\"\n}",
    )
    .expect("#1725 concurrent credentials rewrite");
    drop(guard);
    writer
        .join()
        .expect("#1725 the blocked cursor save finishes");

    let raw = std::fs::read_to_string(&path).expect("#1725 credentials file after save");
    crate::negative_assert!(
        raw.contains("t-2"),
        "#1725 the cursor save re-reads under the lock and keeps the newer credentials"
    );
    crate::negative_assert!(
        raw.contains("a-2"),
        "#1725 the cursor save preserves the concurrent rewrite's account"
    );
    crate::negative_assert!(
        raw.contains("cur-1"),
        "#1725 the cursor save still lands its own cursor"
    );
    crate::negative_assert!(
        !raw.contains("t-1"),
        "#1725 the stale pre-lock snapshot is not resurrected"
    );
    let _ = channel.stop("cleanup").await;
}

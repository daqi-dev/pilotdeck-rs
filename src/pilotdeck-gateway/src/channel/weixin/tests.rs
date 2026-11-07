//! Unit tests for the weixin channel pieces (mapper, persistence). The
//! channel flows against the TS golden and the mock ilink server live with
//! the fixture under `tests/fixtures/weixin-channel/`.
use super::*;
use pilotdeck_protocol::parse_json;
use std::sync::atomic::{AtomicUsize, Ordering};

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
        // Long enough that the *live* timer cannot fire while this test
        // observes, so an early write can only come from the superseded timer.
        let persistence = ChannelStatePersistence::with_debounce(root.path(), 500);
        let key = js("weixin");
        let file = root.path().join("weixin.state.json");

        // Arm the rendezvous, then start a timer that parks as soon as it wakes.
        let entered = persistence.park_next_timer();
        persistence.save(&key, state_with_value(1.0));
        tokio::task::spawn_blocking(move || {
            entered
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("#2073 the debounce timer must reach the park point")
        })
        .await
        .expect("#2073 park waiter");

        // Supersede the parked timer and let it settle.
        let exits = persistence.timer_exits();
        persistence.save(&key, state_with_value(2.0));
        persistence.release_parked_timer();
        let mut settled = persistence.timer_exits() > exits;
        for _ in 0..2_000 {
            if settled {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            settled = persistence.timer_exits() > exits;
        }

        // A superseded timer must not write: the debounce still has ~500 ms to
        // go, and TS would not have written either.
        let early_write = read_state(&file).is_some();

        // The newest state must land; pre-fix it was claimed and then dropped,
        // and `flush` waited forever on the orphaned in-flight entry.
        let flushed = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            persistence.flush(),
        )
        .await
        .is_ok();
        let after_flush = read_state(&file);

        // And the key must not be wedged: a later save still persists.
        persistence.save(&key, state_with_value(3.0));
        let later_flushed = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            persistence.flush(),
        )
        .await
        .is_ok();
        Observed {
            settled,
            early_write,
            flushed,
            after_flush,
            later_flushed,
            after_later: read_state(&file),
        }
    });
    crate::negative_assert!(
        observed.settled,
        "#2073 the superseded timer must settle"
    );
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
        /// `None` means the waiting flush never returned (the wedge).
        second: Option<Result<(), String>>,
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
        let second = match tokio::time::timeout(std::time::Duration::from_secs(5), second).await {
            Ok(joined) => joined.ok(),
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
    crate::negative_assert!(
        observed.second.as_ref().is_some_and(|result| result.is_ok()),
        "#2073 the retry writes"
    );
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
    });
    assert!(
        !channel.lock_inner().active_chats.contains(&user),
        "#1780 the guard releases the chat even when the lock is poisoned"
    );
}

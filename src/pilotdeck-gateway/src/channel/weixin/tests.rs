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

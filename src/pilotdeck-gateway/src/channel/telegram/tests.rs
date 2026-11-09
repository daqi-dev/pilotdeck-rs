use super::*;
use std::{collections::VecDeque, future::Future, pin::Pin};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn mapper_preserves_js_trim_new_session_and_snapshot() {
    let mut mapper = TelegramSessionMapper::new(
        TelegramSessionMapperState::default(),
        Box::new(|| "fixed-id".into()),
    );
    let chat = "a:b".into();
    let initial = mapper.resolve(&chat, &"  hello  ".into());
    assert_eq!(
        initial.session_key,
        JsString::from("telegram:chat=a:b:general")
    );
    assert_eq!(initial.message, JsString::from("hello"));
    let new = mapper.resolve(&chat, &"\u{feff}/new  next\u{a0}".into());
    assert!(new.command_new);
    assert_eq!(new.message, JsString::from("next"));
    assert_eq!(
        mapper.resolve(&chat, &"again".into()).session_key,
        new.session_key
    );
    let mut restored = TelegramSessionMapper::new(mapper.snapshot(), Box::new(|| "unused".into()));
    assert_eq!(
        restored.resolve(&chat, &"again".into()).session_key,
        new.session_key
    );
    assert!(!restored.resolve(&chat, &"/new\tkept".into()).command_new);
}

#[test]
fn chunks_use_utf16_newline_then_space_and_render_error() {
    let input = JsString::from("aaaa\nbbbb cccc");
    assert_eq!(
        render::chunk_text(&input, 8),
        vec![
            JsString::from("aaaa"),
            JsString::from("bbbb"),
            JsString::from(" cccc")
        ]
    );
    let input = JsString::from_utf16(vec![97, 0xd83d, 0xde00, 98]);
    assert_eq!(
        render::chunk_text(&input, 2),
        vec![
            JsString::from_utf16(vec![97, 0xd83d]),
            JsString::from_utf16(vec![0xde00, 98])
        ]
    );
    let event =
        parse_json(r#"{"type":"tool_call_finished","ok":false,"toolName":"Read"}"#).unwrap();
    assert_eq!(
        render::render_telegram_event(&event),
        Some("\n⚠️ Read failed\n".into())
    );
}

struct Stream(VecDeque<JsValue>);
impl TurnEventStream for Stream {
    fn next(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>> {
        Box::pin(async move { Ok(self.0.pop_front()) })
    }
}
#[derive(Default)]
struct Gateway {
    requests: Mutex<Vec<TurnRequest>>,
}
impl TurnGateway for Gateway {
    fn submit(
        &self,
        request: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        self.requests.lock().unwrap().push(request);
        Box::pin(async {
            Ok(Box::new(Stream(VecDeque::from([parse_json(
                r#"{"type":"assistant_text_delta","text":"answer"}"#,
            )
            .unwrap()]))) as Box<dyn TurnEventStream>)
        })
    }
}
struct Mock {
    root: String,
    calls: Arc<Mutex<Vec<(String, JsValue)>>>,
    abort: JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.abort.abort();
    }
}
impl Mock {
    async fn new(update: Option<JsValue>, reject_send: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let record = calls.clone();
        let update = Arc::new(Mutex::new(update));
        let abort = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (mut socket,_) = accepted.unwrap(); let calls = record.clone(); let update = update.clone();
                        connections.spawn(async move {
                            let mut head = Vec::new();
                            while !head.ends_with(b"\r\n\r\n") { let mut byte=[0]; if socket.read_exact(&mut byte).await.is_err() { return } head.push(byte[0]); assert!(head.len()<16384); }
                            let head = String::from_utf8(head).unwrap();
                            let method = head.lines().next().unwrap().split_whitespace().nth(1).unwrap().rsplit('/').next().unwrap().to_owned();
                            let len: usize = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|s| s.trim().parse().unwrap())).unwrap_or(0);
                            let mut body=vec![0;len]; socket.read_exact(&mut body).await.unwrap();
                            let value=parse_json(&String::from_utf8(body).unwrap()).unwrap(); calls.lock().unwrap().push((method.clone(),value));
                            let response=match method.as_str() {
                                "getMe" => r#"{"ok":true,"result":{"username":"fixture"}}"#.to_owned(),
                                "getUpdates" => {
                                    let row=update.lock().unwrap().take();
                                    tokio::time::sleep(Duration::from_millis(10)).await;
                                    stringify_json(&object([("ok",JsValue::Bool(true)),("result",JsValue::Array(row.into_iter().collect()))])).unwrap().unwrap()
                                },
                                "sendMessage" if reject_send => r#"{"ok":false,"description":"blocked"}"#.into(),
                                _ => r#"{"ok":true,"result":true}"#.into(),
                            };
                            let wire=format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response);
                            socket.write_all(wire.as_bytes()).await.unwrap();
                        });
                    },
                    Some(_) = connections.join_next(), if !connections.is_empty() => {},
                }
            }
        });
        Self { root, calls, abort }
    }
    async fn wait_method(&self, method: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if self.calls.lock().unwrap().iter().any(|(m, _)| m == method) {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("mock method observed");
    }
}
fn update(text: &str) -> JsValue {
    object([
        ("update_id", JsValue::Number(41.0)),
        (
            "message",
            object([
                ("text", text.into()),
                ("chat", object([("id", JsValue::Number(12.0))])),
            ]),
        ),
    ])
}

#[tokio::test]
async fn real_polling_enters_shared_gateway_and_stop_reaps_poll_and_turn_tasks() {
    let mock = Mock::new(Some(update(" hello ")), false).await;
    let gateway = Arc::new(Gateway::default());
    let channel = TelegramChannel::new(TelegramOptions {
        token: Some("fixture".into()),
        api_root: Some(mock.root.clone()),
        ..Default::default()
    });
    let handle = channel
        .start(ChannelStartDeps {
            turn_gateway: Some(gateway.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    mock.wait_method("sendMessage").await;
    handle.stop("test").await.unwrap();
    assert!(!channel.inner.running.load(Ordering::Acquire));
    assert!(channel.inner.active.lock().unwrap().is_empty());
    let requests = gateway.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].channel_key, ChannelKey::new("telegram"));
    assert_eq!(
        requests[0].session_key,
        JsString::from("telegram:chat=12:general")
    );
    assert_eq!(requests[0].message, JsString::from("hello"));
    let golden = parse_json(include_str!("fixtures/oracle-diagnostic.json")).unwrap();
    assert_eq!(
        golden.get("baseline"),
        Some(&JsValue::from("cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"))
    );
    let Some(JsValue::Array(expected)) = golden.get("requests") else {
        panic!("fixed TS requests")
    };
    assert_eq!(expected.len(), 1);
    assert_eq!(
        expected[0].get("message"),
        Some(&JsValue::String(requests[0].message.clone()))
    );
    assert_eq!(
        expected[0].get("channelKey"),
        Some(&JsValue::String(requests[0].channel_key.as_js().clone()))
    );
    assert_eq!(
        expected[0].get("sessionKey"),
        Some(&JsValue::String(requests[0].session_key.clone()))
    );

    let calls = mock.calls.lock().unwrap();
    assert_eq!(calls[0].0, "deleteWebhook");
    assert_eq!(calls[1].0, "getMe");
    assert!(
        calls
            .iter()
            .any(|(m, b)| m == "sendMessage" && b.get("text") == Some(&JsValue::from("answer")))
    );
    assert!(
        calls
            .iter()
            .any(|(m, b)| m == "getUpdates" && b.get("offset") == Some(&JsValue::Number(0.0)))
    );
}

#[tokio::test]
async fn webhook_new_command_and_cron_delivery_use_same_bot_and_report_send_failure() {
    let mock = Mock::new(None, true).await;
    let channel = TelegramChannel::new(TelegramOptions {
        token: Some("fixture".into()),
        webhook_url: Some("https://fixture.invalid/webhook".into()),
        api_root: Some(mock.root.clone()),
        ..Default::default()
    });
    let handle = channel.start(ChannelStartDeps::default()).await.unwrap();
    channel.accept_webhook_update(update("/new")).await.unwrap();
    mock.wait_method("sendMessage").await;
    let delivery = CronResultDelivery {
        task_id: "t".into(),
        run_id: "r".into(),
        session_key: "telegram:chat=12:general".into(),
        channel_key: ChannelKey::new("telegram"),
        origin_session_key: None,
        origin_channel_key: None,
        text: "cron".into(),
    };
    assert!(!channel.deliver_cron_result(&delivery).await);
    handle.stop("test").await.unwrap();
    assert!(channel.accept_webhook_update(update("late")).await.is_err());
    assert!(!channel.deliver_cron_result(&delivery).await);
    let replacement = channel.start(ChannelStartDeps::default()).await.unwrap();
    // An old lifecycle handle must never close the replacement's inbox.
    handle.stop("old retry").await.unwrap();
    drop(handle);
    channel
        .accept_webhook_update(update("replacement"))
        .await
        .unwrap();
    replacement.stop("replacement").await.unwrap();
    let calls = mock.calls.lock().unwrap();
    assert_eq!(calls[0].0, "setWebhook");
    assert!(!calls.iter().any(|(m, _)| m == "getUpdates"));
    assert!(calls.iter().any(
        |(m, b)| m == "sendMessage" && b.get("text") == Some(&JsValue::from("已创建新会话。"))
    ));
}

struct PendingStream(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for PendingStream {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl TurnEventStream for PendingStream {
    fn next(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}
struct PendingGateway {
    entered: Arc<tokio::sync::Notify>,
    dropped: Arc<std::sync::atomic::AtomicUsize>,
}
impl TurnGateway for PendingGateway {
    fn submit(&self, _: TurnRequest) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        let entered = self.entered.clone();
        let dropped = self.dropped.clone();
        Box::pin(async move {
            entered.notify_one();
            Ok(Box::new(PendingStream(dropped)) as Box<dyn TurnEventStream>)
        })
    }
}
#[tokio::test]
async fn stop_cancels_a_real_admitted_stream_and_releases_chat_before_returning() {
    let mock = Mock::new(None, false).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let gateway = Arc::new(PendingGateway {
        entered: entered.clone(),
        dropped: dropped.clone(),
    });
    let channel = TelegramChannel::new(TelegramOptions {
        token: Some("fixture".into()),
        webhook_url: Some("https://fixture.invalid/webhook".into()),
        api_root: Some(mock.root.clone()),
        ..Default::default()
    });
    let handle = channel
        .start(ChannelStartDeps {
            turn_gateway: Some(gateway),
            ..Default::default()
        })
        .await
        .unwrap();
    channel
        .accept_webhook_update(update("pending"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .expect("turn admitted");
    assert_eq!(channel.inner.active.lock().unwrap().len(), 1);
    tokio::time::timeout(Duration::from_secs(3), handle.stop("cancel pending"))
        .await
        .expect("actual drain")
        .unwrap();
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(channel.inner.active.lock().unwrap().is_empty());
}

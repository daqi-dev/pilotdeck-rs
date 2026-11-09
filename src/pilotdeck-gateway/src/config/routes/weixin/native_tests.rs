use super::*;
use std::sync::{
    Mutex as StdMutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
struct Gateway {
    turns: AtomicUsize,
    requests: StdMutex<Vec<Value>>,
}
impl ChannelGateway for Gateway {
    fn request<'a>(&'a self, method: &'a str, params: Value) -> ChannelCall<'a, Value> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(json!([method, params]));
            Ok(
                json!({"projects":[{"name":"demo","fullPath":"/isolated/project","projectKey":"/isolated/project"}]}),
            )
        })
    }
    fn turn(&self, params: Value) -> ChannelCall<'_, Vec<Value>> {
        Box::pin(async move {
            self.turns.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().unwrap().push(params);
            Ok(vec![
                json!({"type":"assistant_text_delta","text":"real turn reply"}),
                json!({"type":"turn_completed"}),
            ])
        })
    }
}
struct Server {
    base: String,
    requests: Arc<StdMutex<Vec<Value>>>,
    task: JoinHandle<()>,
    hold_confirmation: Arc<std::sync::atomic::AtomicBool>,
    confirmation_entered: Arc<tokio::sync::Notify>,
    confirmation_release: Arc<tokio::sync::Notify>,
    hold_prompt: Arc<std::sync::atomic::AtomicBool>,
    prompt_entered: Arc<tokio::sync::Notify>,
    prompt_release: Arc<tokio::sync::Notify>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn server(fail_first: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let requests = Arc::new(StdMutex::new(Vec::new()));
    let recorded = requests.clone();
    let hold_confirmation = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let confirmation_entered = Arc::new(tokio::sync::Notify::new());
    let confirmation_release = Arc::new(tokio::sync::Notify::new());
    let hold = hold_confirmation.clone();
    let entered = confirmation_entered.clone();
    let release = confirmation_release.clone();
    let hold_prompt = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let prompt_entered = Arc::new(tokio::sync::Notify::new());
    let prompt_release = Arc::new(tokio::sync::Notify::new());
    let prompt_hold = hold_prompt.clone();
    let prompt_in = prompt_entered.clone();
    let prompt_out = prompt_release.clone();
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let recorded = recorded.clone();
            let prompt_hold = prompt_hold.clone();
            let prompt_in = prompt_in.clone();
            let prompt_out = prompt_out.clone();
            let hold = hold.clone();
            let entered = entered.clone();
            let release = release.clone();
            connections.spawn(async move {
            let mut bytes = Vec::new();
            let mut buf = [0; 4096];
            let (header, length, path) = loop {
                let count = socket.read(&mut buf).await.unwrap();
                if count == 0 {
                    panic!("incomplete HTTP");
                }
                bytes.extend_from_slice(&buf[..count]);
                if let Some(end) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap();
                    assert!(header.contains("ilink_bot_token"));
                    let path = header.split_whitespace().nth(1).unwrap().to_owned();
                    break (end + 4, length, path);
                }
            };
            while bytes.len() < header + length {
                let count = socket.read(&mut buf).await.unwrap();
                bytes.extend_from_slice(&buf[..count]);
            }
            let value: Value = serde_json::from_slice(&bytes[header..header + length]).unwrap();
            if value["msg"]["item_list"][0]["text_item"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("需要权限才能继续执行。"))
                && prompt_hold.load(Ordering::SeqCst)
            {
                prompt_in.notify_one();
                prompt_out.notified().await;
            }
            if value["msg"]["item_list"][0]["text_item"]["text"] == "已允许一次，继续执行。"
                && hold.load(Ordering::SeqCst)
            {
                entered.notify_one();
                release.notified().await;
            }
            let first = recorded.lock().unwrap().is_empty();
            let body = if path.ends_with("getupdates") {
                r#"{"ret":0,"msgs":[]}"#
            } else {
                assert!(path.ends_with("sendmessage"));
                recorded.lock().unwrap().push(value);
                if fail_first && first {
                    r#"{"ret":7}"#
                } else {
                    r#"{"ret":0}"#
                }
            };
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            });
        }
    });
    Server {
        base,
        requests,
        task,
        hold_confirmation,
        confirmation_entered,
        confirmation_release,
        hold_prompt,
        prompt_entered,
        prompt_release,
    }
}
fn setup(base: &str, messages: Value) -> (Storage, Value, Value) {
    let home = std::env::temp_dir().join(format!("pdrs-weixin-native-{}", uuid::Uuid::new_v4()));
    let storage = Storage::new(home);
    let credentials = json!({"baseUrl":base,"botToken":"isolated-fixture","accountId":"fixture"});
    storage.save_login(credentials.clone()).unwrap();
    let id = "a".repeat(64);
    let batch = json!({"id":id,"accountId":"fixture","messages":messages});
    super::super::storage::write(
        &storage
            .home
            .join("channels/weixin-inbox")
            .join(format!("{id}.json")),
        &batch,
    )
    .unwrap();
    (storage, credentials, batch)
}
fn message(input: &str) -> Value {
    json!({"message_type":1,"from_user_id":"chat","context_token":"context-fixture","item_list":[{"type":1,"text_item":{"text":input}}]})
}
#[tokio::test]
async fn native_owner_executes_project_command_persists_binding_and_delivers_real_reply() {
    let server = server(false).await;
    let (storage, credentials, batch) = setup(
        &server.base,
        json!([message("/switch-project demo"), message("hello")]),
    );
    let gateway = Gateway {
        turns: AtomicUsize::new(0),
        requests: StdMutex::new(vec![]),
    };
    let mut mapper = ChannelSessionState::default();
    drain_batch(
        &storage,
        &Http::new(),
        &credentials,
        &batch,
        &mut mapper,
        &gateway,
    )
    .await
    .unwrap();
    assert_eq!(gateway.turns.load(Ordering::SeqCst), 1);
    let calls = gateway.requests.lock().unwrap();
    assert_eq!(calls[0][0], "list_projects");
    assert_eq!(calls[1]["projectKey"], "/isolated/project");
    assert!(
        calls[1]["sessionKey"]
            .as_str()
            .unwrap()
            .starts_with("weixin:chat=chat:s_")
    );
    assert_eq!(
        storage.load_mapper().unwrap()["projectByChatId"]["chat"],
        "/isolated/project"
    );
    let sent = server.requests.lock().unwrap();
    assert!(
        sent[0]["msg"]["item_list"][0]["text_item"]["text"]
            .as_str()
            .unwrap()
            .contains("已切换到项目：demo")
    );
    assert_eq!(
        sent[1]["msg"]["item_list"][0]["text_item"]["text"],
        "real turn reply"
    );
    assert_eq!(sent[1]["msg"]["context_token"], "context-fixture");
}
#[tokio::test]
async fn failed_delivery_retries_staged_outbox_without_resubmitting_turn() {
    let server = server(true).await;
    let (storage, credentials, batch) = setup(&server.base, json!([message("hello")]));
    let gateway = Gateway {
        turns: AtomicUsize::new(0),
        requests: StdMutex::new(vec![]),
    };
    let mut mapper = ChannelSessionState::default();
    assert!(
        drain_batch(
            &storage,
            &Http::new(),
            &credentials,
            &batch,
            &mut mapper,
            &gateway
        )
        .await
        .is_err()
    );
    let saved = super::super::inbox::WeixinInbox::new(storage.home.clone())
        .pending()
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(saved["outbox"][0], "real turn reply");
    drain_batch(
        &storage,
        &Http::new(),
        &credentials,
        &saved,
        &mut mapper,
        &gateway,
    )
    .await
    .unwrap();
    assert_eq!(gateway.turns.load(Ordering::SeqCst), 1);
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn new_message_aborts_previous_session_before_submitting_remainder() {
    let server = server(false).await;
    let (storage, credentials, batch) = setup(&server.base, json!([message("/new next")]));
    let gateway = Gateway {
        turns: AtomicUsize::new(0),
        requests: StdMutex::new(vec![]),
    };
    let mut mapper = ChannelSessionState::default();
    drain_batch(
        &storage,
        &Http::new(),
        &credentials,
        &batch,
        &mut mapper,
        &gateway,
    )
    .await
    .unwrap();
    let calls = gateway.requests.lock().unwrap();
    assert_eq!(
        calls[0],
        json!(["abort_turn",{"sessionKey":"weixin:chat=chat:general"}])
    );
    assert_eq!(calls[1]["message"], "next");
    assert!(
        calls[1]["sessionKey"]
            .as_str()
            .unwrap()
            .starts_with("weixin:chat=chat:s_")
    );
}

#[tokio::test]
async fn factory_with_gateway_drains_real_persisted_inbox_and_stops_polling_owner() {
    let server = server(false).await;
    let (storage, _credentials, _batch) = setup(&server.base, json!([message("/projects")]));
    let gateway = Arc::new(Gateway {
        turns: AtomicUsize::new(0),
        requests: StdMutex::new(vec![]),
    });
    let factory = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .with_gateway(gateway.clone());
    let channel = factory.load_and_create().await.unwrap();
    channel.start().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while server.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    channel.stop().await.unwrap();
    assert!(
        super::super::inbox::WeixinInbox::new(storage.home.clone())
            .pending()
            .unwrap()
            .is_empty()
    );
    assert_eq!(gateway.turns.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.requests.lock().unwrap()[0][0], "list_projects");
    let status: Value = serde_json::from_slice(
        &std::fs::read(storage.home.join("channels/runtime-status.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(status["channels"]["weixin"]["state"], "stopped");
}

struct InteractiveGateway {
    requested: tokio::sync::Notify,
    answered: tokio::sync::Notify,
    requests: StdMutex<Vec<Value>>,
    elicitation: bool,
}
impl ChannelGateway for InteractiveGateway {
    fn request<'a>(&'a self, method: &'a str, params: Value) -> ChannelCall<'a, Value> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(json!([method, params]));
            if matches!(
                method,
                "permission_decide" | "elicitation_respond" | "abort_turn"
            ) {
                self.answered.notify_one();
            }
            Ok(json!({"delivered":true}))
        })
    }
    fn turn(&self, _params: Value) -> ChannelCall<'_, Vec<Value>> {
        Box::pin(async { panic!("live owner must use streaming port") })
    }
    fn stream_turn<'a>(
        &'a self,
        _params: Value,
        sink: &'a dyn crate::config::channel_commands::ChannelEventSink,
    ) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            sink.event(if self.elicitation {json!({"type":"elicitation_request","requestId":"question","questions":[{"question":"Choose","header":"Header","options":[{"label":"A","description":"first"},{"label":"B","description":"second"}],"multiSelect":true}]})}else{json!({"type":"permission_request","requestId":"permission","toolName":"write_file","payload":{"path":"file"}})}).await?;
            self.requested.notify_one();
            self.answered.notified().await;
            sink.event(json!({"type":"assistant_text_delta","text":"after decision"}))
                .await?;
            Ok(())
        })
    }
}
fn second_batch(storage: &Storage, input: &str) -> Value {
    let id = "b".repeat(64);
    let batch = json!({"id":id,"accountId":"fixture","messages":[message(input)]});
    super::super::storage::write(
        &storage
            .home
            .join("channels/weixin-inbox")
            .join(format!("{id}.json")),
        &batch,
    )
    .unwrap();
    batch
}
#[tokio::test]
async fn live_permission_answer_reaches_waiting_stream_and_cron_reuses_context() {
    let server = server(false).await;
    let (storage, credentials, batch) = setup(&server.base, json!([message("start")]));
    let gateway = Arc::new(InteractiveGateway {
        requested: Default::default(),
        answered: Default::default(),
        requests: Default::default(),
        elicitation: false,
    });
    let factory =
        NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone())).unwrap();
    let delivery = factory.delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery.clone(),
    ));
    let task = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    gateway.requested.notified().await;
    owner
        .process_batch(&credentials, second_batch(&storage, "2"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let requests = gateway.requests.lock().unwrap();
    assert_eq!(requests[0][0], "permission_decide");
    assert_eq!(requests[0][1]["remember"], true);
    drop(requests);
    assert!(delivery.deliver(&json!({"originChannelKey":"weixin","originSessionKey":"weixin:chat=chat:general","text":"scheduled"})).await.unwrap());
    let sent = server.requests.lock().unwrap();
    assert!(sent.iter().any(|v| {
        v["msg"]["item_list"][0]["text_item"]["text"]
            .as_str()
            .unwrap()
            .contains("需要权限")
    }));
    assert!(
        sent.iter()
            .any(|v| v["msg"]["item_list"][0]["text_item"]["text"] == "scheduled")
    );
}
#[tokio::test]
async fn live_elicitation_maps_comma_choices_into_waiting_turn() {
    let server = server(false).await;
    let (storage, credentials, batch) = setup(&server.base, json!([message("start")]));
    let gateway = Arc::new(InteractiveGateway {
        requested: Default::default(),
        answered: Default::default(),
        requests: Default::default(),
        elicitation: true,
    });
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery,
    ));
    let task = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    gateway.requested.notified().await;
    owner
        .process_batch(&credentials, second_batch(&storage, "2，1"))
        .await
        .unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(
        gateway.requests.lock().unwrap()[0][1]["answer"]["answers"]["Choose"],
        json!(["B", "A"])
    );
}

#[tokio::test]
async fn live_new_aborts_active_turn_and_suppresses_stale_generation_reply() {
    let server = server(false).await;
    let (storage, credentials, batch) = setup(&server.base, json!([message("start")]));
    let gateway = Arc::new(InteractiveGateway {
        requested: Default::default(),
        answered: Default::default(),
        requests: Default::default(),
        elicitation: false,
    });
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery,
    ));
    let task = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    gateway.requested.notified().await;
    owner
        .process_batch(&credentials, second_batch(&storage, "/new"))
        .await
        .unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(
        gateway.requests.lock().unwrap()[0],
        json!(["abort_turn",{"sessionKey":"weixin:chat=chat:general"}])
    );
    assert!(
        storage.load_mapper().unwrap()["activeByChatId"]["chat"]
            .as_str()
            .unwrap()
            .starts_with("weixin:chat=chat:s_")
    );
    let sent = server.requests.lock().unwrap();
    assert!(
        sent.iter()
            .any(|v| v["msg"]["item_list"][0]["text_item"]["text"] == "已创建新会话。")
    );
    assert!(
        !sent
            .iter()
            .any(|v| v["msg"]["item_list"][0]["text_item"]["text"] == "after decision")
    );
}

#[tokio::test]
async fn same_poll_batch_early_answer_preserves_ts_prompt_delivery_gate() {
    let server = server(false).await;
    server.hold_prompt.store(true, Ordering::SeqCst);
    let (storage, credentials, batch) =
        setup(&server.base, json!([message("start"), message("1")]));
    let gateway = Arc::new(InteractiveGateway {
        requested: Default::default(),
        answered: Default::default(),
        requests: Default::default(),
        elicitation: false,
    });
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery,
    ));
    let turn = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    server.prompt_entered.notified().await;
    assert!(gateway.requests.lock().unwrap().is_empty());
    tokio::time::timeout(Duration::from_secs(3), async {
        while !server.requests.lock().unwrap().iter().any(|value| {
            value["msg"]["item_list"][0]["text_item"]["text"] == "权限提示发送中，请稍候。"
        }) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(gateway.requests.lock().unwrap().is_empty());
    server.hold_prompt.store(false, Ordering::SeqCst);
    server.prompt_release.notify_one();
    gateway.requested.notified().await;
    owner
        .process_batch(&credentials, second_batch(&storage, "1"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(gateway.requests.lock().unwrap()[0][0], "permission_decide");
    assert!(
        super::super::inbox::WeixinInbox::new(storage.home)
            .pending()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn production_live_outbox_retries_failed_send_without_new_session_or_second_turn() {
    let server = server(true).await;
    let (storage, credentials, batch) = setup(&server.base, json!([message("/new next")]));
    let gateway = Arc::new(Gateway {
        turns: AtomicUsize::new(0),
        requests: Default::default(),
    });
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery,
    ));
    assert!(
        owner
            .process_batch(&credentials, batch.clone())
            .await
            .is_err()
    );
    let first_session = storage.load_mapper().unwrap()["activeByChatId"]["chat"].clone();
    owner.process_batch(&credentials, batch).await.unwrap();
    assert_eq!(gateway.turns.load(Ordering::SeqCst), 1);
    assert_eq!(
        storage.load_mapper().unwrap()["activeByChatId"]["chat"],
        first_session
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn duplicate_permission_answer_cannot_decide_next_request_before_prompt_delivery() {
    struct Two {
        ready: tokio::sync::Notify,
        decisions: AtomicUsize,
        finished: tokio::sync::Notify,
    }
    impl ChannelGateway for Two {
        fn request<'a>(&'a self, method: &'a str, _params: Value) -> ChannelCall<'a, Value> {
            Box::pin(async move {
                assert_eq!(method, "permission_decide");
                self.decisions.fetch_add(1, Ordering::SeqCst);
                Ok(json!({"delivered":true}))
            })
        }
        fn turn(&self, _params: Value) -> ChannelCall<'_, Vec<Value>> {
            Box::pin(async { panic!() })
        }
        fn stream_turn<'a>(
            &'a self,
            _params: Value,
            sink: &'a dyn crate::config::channel_commands::ChannelEventSink,
        ) -> ChannelCall<'a, ()> {
            Box::pin(async move {
                for id in ["A", "B"] {
                    sink.event(json!({"type":"permission_request","requestId":id,"toolName":"write_file","payload":{}})).await?;
                }
                self.ready.notify_one();
                self.finished.notified().await;
                Ok(())
            })
        }
    }
    let server = server(false).await;
    server.hold_confirmation.store(true, Ordering::SeqCst);
    let (storage, credentials, batch) = setup(&server.base, json!([message("start")]));
    let gateway = Arc::new(Two {
        ready: Default::default(),
        decisions: Default::default(),
        finished: Default::default(),
    });
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery,
    ));
    let turn = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    gateway.ready.notified().await;
    let first = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        let batch = second_batch(&storage, "1");
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    server.confirmation_entered.notified().await;
    // A completed turn must defer clearing the answer lock while the HTTP
    // confirmation is still in flight.
    gateway.finished.notify_one();
    turn.await.unwrap().unwrap();
    let mut duplicate = second_batch(&storage, "1");
    duplicate["id"] = "c".repeat(64).into();
    super::super::storage::write(
        &storage
            .home
            .join("channels/weixin-inbox")
            .join(format!("{}.json", "c".repeat(64))),
        &duplicate,
    )
    .unwrap();
    let second = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        tokio::spawn(async move { owner.process_batch(&credentials, duplicate).await })
    };
    tokio::task::yield_now().await;
    assert_eq!(gateway.decisions.load(Ordering::SeqCst), 1);
    server.hold_confirmation.store(false, Ordering::SeqCst);
    server.confirmation_release.notify_one();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(gateway.decisions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn live_owner_ignores_non_user_system_and_echo_messages() {
    let server = server(false).await;
    let mut echo = message("must not execute");
    echo["message_type"] = 2.into();
    let (storage, credentials, batch) = setup(&server.base, json!([echo]));
    let gateway = Arc::new(Gateway {
        turns: Default::default(),
        requests: Default::default(),
    });
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage,
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery,
    ));
    owner.process_batch(&credentials, batch).await.unwrap();
    assert_eq!(gateway.turns.load(Ordering::SeqCst), 0);
    assert!(gateway.requests.lock().unwrap().is_empty());
    assert!(server.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn new_session_drops_ordinary_message_queued_under_previous_generation() {
    struct Hold {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
        turns: AtomicUsize,
    }
    impl ChannelGateway for Hold {
        fn request<'a>(&'a self, method: &'a str, _params: Value) -> ChannelCall<'a, Value> {
            Box::pin(async move {
                assert_eq!(method, "abort_turn");
                self.release.notify_one();
                Ok(json!({}))
            })
        }
        fn turn(&self, _params: Value) -> ChannelCall<'_, Vec<Value>> {
            Box::pin(async { panic!() })
        }
        fn stream_turn<'a>(
            &'a self,
            _params: Value,
            _sink: &'a dyn crate::config::channel_commands::ChannelEventSink,
        ) -> ChannelCall<'a, ()> {
            Box::pin(async move {
                self.turns.fetch_add(1, Ordering::SeqCst);
                self.entered.notify_one();
                self.release.notified().await;
                Ok(())
            })
        }
    }
    let server = server(false).await;
    let (storage, credentials, batch) = setup(&server.base, json!([message("active")]));
    let gateway = Arc::new(Hold {
        entered: Default::default(),
        release: Default::default(),
        turns: Default::default(),
    });
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery,
    ));
    let active = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    gateway.entered.notified().await;
    let queued = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        let batch = second_batch(&storage, "old queued");
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    let mut reset = second_batch(&storage, "/new");
    reset["id"] = "d".repeat(64).into();
    super::super::storage::write(
        &storage
            .home
            .join("channels/weixin-inbox")
            .join(format!("{}.json", "d".repeat(64))),
        &reset,
    )
    .unwrap();
    owner.process_batch(&credentials, reset).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), queued)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    active.await.unwrap().unwrap();
    assert_eq!(gateway.turns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn new_session_clears_initial_prompt_gate_before_accepting_ordinary_message() {
    struct Prompt {
        turns: AtomicUsize,
    }
    impl ChannelGateway for Prompt {
        fn request<'a>(&'a self, method: &'a str, _params: Value) -> ChannelCall<'a, Value> {
            Box::pin(async move {
                assert_eq!(method, "abort_turn");
                Ok(json!({}))
            })
        }
        fn turn(&self, _params: Value) -> ChannelCall<'_, Vec<Value>> {
            Box::pin(async { panic!() })
        }
        fn stream_turn<'a>(
            &'a self,
            _params: Value,
            sink: &'a dyn crate::config::channel_commands::ChannelEventSink,
        ) -> ChannelCall<'a, ()> {
            Box::pin(async move {
                if self.turns.fetch_add(1, Ordering::SeqCst) == 0 {
                    sink.event(json!({"type":"permission_request","requestId":"old","toolName":"write_file","payload":{}})).await?;
                }
                Ok(())
            })
        }
    }
    let server = server(false).await;
    server.hold_prompt.store(true, Ordering::SeqCst);
    let (storage, credentials, batch) = setup(&server.base, json!([message("active")]));
    let gateway = Arc::new(Prompt {
        turns: Default::default(),
    });
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery,
    ));
    let active = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    server.prompt_entered.notified().await;
    let reset = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        let batch = second_batch(&storage, "/new");
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    tokio::task::yield_now().await;
    server.hold_prompt.store(false, Ordering::SeqCst);
    server.prompt_release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), reset)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    active.await.unwrap().unwrap();
    let mut ordinary = second_batch(&storage, "after new");
    ordinary["id"] = "e".repeat(64).into();
    super::super::storage::write(
        &storage
            .home
            .join("channels/weixin-inbox")
            .join(format!("{}.json", "e".repeat(64))),
        &ordinary,
    )
    .unwrap();
    owner.process_batch(&credentials, ordinary).await.unwrap();
    assert_eq!(gateway.turns.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn new_session_preempts_blocked_permission_rpc_and_ignores_its_stale_reply() {
    struct Blocked {
        turns: AtomicUsize,
        ready: tokio::sync::Notify,
        deciding: tokio::sync::Notify,
        decision_release: tokio::sync::Notify,
        old_turn_release: tokio::sync::Notify,
        new_turn_release: tokio::sync::Notify,
        decisions: StdMutex<Vec<Value>>,
    }
    impl ChannelGateway for Blocked {
        fn request<'a>(&'a self, method: &'a str, params: Value) -> ChannelCall<'a, Value> {
            Box::pin(async move {
                match method {
                    "abort_turn" => self.old_turn_release.notify_one(),
                    "permission_decide" => {
                        self.decisions.lock().unwrap().push(params.clone());
                        if params["requestId"] == "old" {
                            self.deciding.notify_one();
                            self.decision_release.notified().await;
                        }
                    }
                    _ => panic!("unexpected RPC {method}"),
                }
                Ok(json!({"delivered":true}))
            })
        }
        fn turn(&self, _params: Value) -> ChannelCall<'_, Vec<Value>> {
            Box::pin(async { panic!() })
        }
        fn stream_turn<'a>(
            &'a self,
            _params: Value,
            sink: &'a dyn crate::config::channel_commands::ChannelEventSink,
        ) -> ChannelCall<'a, ()> {
            Box::pin(async move {
                let old = self.turns.fetch_add(1, Ordering::SeqCst) == 0;
                sink.event(json!({"type":"permission_request","requestId":if old {"old"}else{"new"},"toolName":"write_file","payload":{}})).await?;
                self.ready.notify_one();
                if old {
                    self.old_turn_release.notified().await;
                } else {
                    self.new_turn_release.notified().await;
                }
                Ok(())
            })
        }
    }
    fn batch(storage: &Storage, id: char, input: &str) -> Value {
        let mut batch = second_batch(storage, input);
        batch["id"] = id.to_string().repeat(64).into();
        super::super::storage::write(
            &storage
                .home
                .join("channels/weixin-inbox")
                .join(format!("{}.json", id.to_string().repeat(64))),
            &batch,
        )
        .unwrap();
        batch
    }
    let server = server(false).await;
    let (storage, credentials, first) = setup(&server.base, json!([message("start")]));
    let gateway = Arc::new(Blocked {
        turns: Default::default(),
        ready: Default::default(),
        deciding: Default::default(),
        decision_release: Default::default(),
        old_turn_release: Default::default(),
        new_turn_release: Default::default(),
        decisions: Default::default(),
    });
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery,
    ));
    let old_turn = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        tokio::spawn(async move { owner.process_batch(&credentials, first).await })
    };
    gateway.ready.notified().await;
    let old_answer = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        let answer = batch(&storage, 'b', "1");
        tokio::spawn(async move { owner.process_batch(&credentials, answer).await })
    };
    gateway.deciding.notified().await;
    tokio::time::timeout(
        Duration::from_secs(3),
        owner.process_batch(&credentials, batch(&storage, 'c', "/new")),
    )
    .await
    .unwrap()
    .unwrap();
    old_turn.await.unwrap().unwrap();
    let new_turn = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        let input = batch(&storage, 'd', "after new");
        tokio::spawn(async move { owner.process_batch(&credentials, input).await })
    };
    gateway.ready.notified().await;
    gateway.decision_release.notify_one();
    old_answer.await.unwrap().unwrap();
    assert!(
        !server.requests.lock().unwrap().iter().any(
            |request| request["msg"]["item_list"][0]["text_item"]["text"]
                == "已允许一次，继续执行。"
        )
    );
    owner
        .process_batch(&credentials, batch(&storage, 'e', "1"))
        .await
        .unwrap();
    assert_eq!(
        gateway
            .decisions
            .lock()
            .unwrap()
            .iter()
            .map(|value| value["requestId"].clone())
            .collect::<Vec<_>>(),
        vec![json!("old"), json!("new")]
    );
    gateway.new_turn_release.notify_one();
    new_turn.await.unwrap().unwrap();
}

#[tokio::test]
async fn production_weixin_cron_empty_origin_session_does_not_use_session_key() {
    let server = server(false).await;
    let (storage, _, _) = setup(&server.base, json!([]));
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    delivery.test_seed_context("chat", "ctx-token");
    assert!(
        !delivery
            .deliver(&json!({
                "originChannelKey":"weixin",
                "originSessionKey":"",
                "sessionKey":"weixin:chat=chat:general",
                "text":"must not send"
            }))
            .await
            .unwrap()
    );
    assert!(server.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn attachment_fallback_without_context_is_queued_not_err() {
    let server = server(false).await;
    let (storage, _, _) = setup(&server.base, json!([]));
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    delivery.queue_attachment_fallback(
        "chat",
        &json!({"name":"doc.pdf","path":"/tmp/doc.pdf"}),
    );
    assert_eq!(delivery.test_pending_len("chat"), 1);
    assert!(server.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn production_weixin_cron_send_failure_returns_false_and_queues() {
    let server = server(true).await;
    let (storage, credentials, _) = setup(&server.base, json!([]));
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    delivery.test_seed_context("chat", "ctx-token");
    assert!(
        !delivery
            .deliver(&json!({
                "originChannelKey":"weixin",
                "originSessionKey":"weixin:chat=chat:general",
                "text":"cron failed send"
            }))
            .await
            .unwrap()
    );
    assert_eq!(delivery.test_pending_len("chat"), 1);
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    delivery.flush_pending_replies(&credentials).await;
    assert_eq!(delivery.test_pending_len("chat"), 0);
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|v| v["msg"]["item_list"][0]["text_item"]["text"] == "cron failed send")
    );
}

#[tokio::test]
async fn production_weixin_cron_invalid_credentials_returns_ok_false_not_err() {
    let server = server(false).await;
    let home = std::env::temp_dir().join(format!("pdrs-wx-bad-cred-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("channels")).unwrap();
    std::fs::write(home.join("weixin-credentials.json"), b"not-json").unwrap();
    let delivery = NativeWeixinFactory::new(home, Some(server.base.clone()))
        .unwrap()
        .delivery();
    delivery.test_seed_context("chat", "ctx-token");
    let result = delivery
        .deliver(&json!({
            "originChannelKey":"weixin",
            "originSessionKey":"weixin:chat=chat:general",
            "text":"queued on bad creds"
        }))
        .await;
    assert!(result.is_ok());
    assert!(!result.unwrap());
    assert_eq!(delivery.test_pending_len("chat"), 1);
    assert!(server.requests.lock().unwrap().is_empty());
}

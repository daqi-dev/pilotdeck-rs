use super::*;
use std::sync::{
    Mutex as StdMutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
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
            } else if path.ends_with("getconfig") {
                // WeixinChannel.ts:880-888 — typing needs the ticket first.
                r#"{"typing_ticket":"fixture-ticket"}"#
            } else if path.ends_with("sendtyping") {
                r#"{"ret":0}"#
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

async fn server_reject_all_sends() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let requests = Arc::new(StdMutex::new(Vec::new()));
    let recorded = requests.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let recorded = recorded.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buf = [0u8; 4096];
                let (header, length, path) = loop {
                    let count = socket.read(&mut buf).await.unwrap();
                    bytes.extend_from_slice(&buf[..count]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let hdr = String::from_utf8_lossy(&bytes[..end]);
                        let length = hdr
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse().ok())
                            })
                            .unwrap_or(0);
                        let path = hdr.split_whitespace().nth(1).unwrap_or("").to_owned();
                        break (end + 4, length, path);
                    }
                };
                while bytes.len() < header + length {
                    let count = socket.read(&mut buf).await.unwrap();
                    bytes.extend_from_slice(&buf[..count]);
                }
                let body = if path.ends_with("getupdates") {
                    r#"{"ret":0,"msgs":[]}"#.to_owned()
                } else if path.ends_with("getconfig") {
                    r#"{"typing_ticket":"fixture-ticket"}"#.to_owned()
                } else if path.ends_with("sendtyping") {
                    r#"{"ret":0}"#.to_owned()
                } else {
                    if header + length <= bytes.len() && length > 0 {
                        let _ =
                            serde_json::from_slice::<Value>(&bytes[header..header + length]);
                    }
                    recorded.lock().unwrap().push(json!({"path": path}));
                    r#"{"ret":7}"#.to_owned()
                };
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            });
        }
    });
    Server {
        base,
        requests,
        task,
        hold_confirmation: Arc::new(AtomicBool::new(false)),
        confirmation_entered: Default::default(),
        confirmation_release: Default::default(),
        hold_prompt: Arc::new(AtomicBool::new(false)),
        prompt_entered: Default::default(),
        prompt_release: Default::default(),
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

#[path = "outbox.rs"]
mod outbox;


use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Notify, mpsc},
};

struct Server {
    base: String,
    updates: mpsc::UnboundedSender<Value>,
    requests: Arc<StdMutex<Vec<(String, Value)>>>,
    changed: Arc<Notify>,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn server() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let (updates, receiver) = mpsc::unbounded_channel::<Value>();
    let receiver = Arc::new(Mutex::new(receiver));
    let requests = Arc::new(StdMutex::new(Vec::new()));
    let recorded = requests.clone();
    let changed = Arc::new(Notify::new());
    let notify = changed.clone();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let receiver = receiver.clone();
            let recorded = recorded.clone();
            let notify = notify.clone();
            connections.spawn(async move {
                let mut bytes=Vec::new();let mut buffer=[0;4096];
                let(header,length,path)=loop {let count=socket.read(&mut buffer).await.unwrap();if count==0{return}bytes.extend_from_slice(&buffer[..count]);if let Some(end)=bytes.windows(4).position(|v|v==b"\r\n\r\n"){let header=String::from_utf8_lossy(&bytes[..end]);let length=header.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v|v.trim().parse::<usize>().ok())).unwrap();let path=header.split_whitespace().nth(1).unwrap().to_owned();break(end+4,length,path)}};
                while bytes.len()<header+length {let count=socket.read(&mut buffer).await.unwrap();if count==0{return}bytes.extend_from_slice(&buffer[..count]);}
                let params:Value=serde_json::from_slice(&bytes[header..header+length]).unwrap();let method=path.rsplit('/').next().unwrap().to_owned();assert!(path.starts_with("/botfixture-token/"));recorded.lock().unwrap().push((method.clone(),params));notify.notify_one();
                let result=match method.as_str(){"getUpdates"=>match receiver.lock().await.recv().await{Some(update)=>json!([update]),None=>json!([])},"getMe"=>json!({"id":1,"username":"fixture"}),_=>json!(true)};
                let body=json!({"ok":true,"result":result}).to_string();let _=socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
            });
            while connections.try_join_next().is_some() {}
        }
    });
    Server {
        base,
        updates,
        requests,
        changed,
        task,
    }
}
impl Server {
    async fn wait(&self, predicate: impl Fn(&[(String, Value)]) -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let notified = self.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if predicate(&self.requests.lock().unwrap()) {
                    return;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
    }
    fn send(&self, id: i64, text: &str) {
        self.updates.send(update(id, text)).unwrap();
    }
    fn texts(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method == "sendMessage")
            .map(|(_, params)| params["text"].as_str().unwrap().into())
            .collect()
    }
}
fn update(id: i64, text: &str) -> Value {
    json!({"update_id":id,"message":{"chat":{"id":42},"text":text}})
}
#[derive(Default)]
struct Gateway {
    turns: StdMutex<Vec<Value>>,
    requests: StdMutex<Vec<(String, Value)>>,
    ready: Notify,
    decision: Notify,
    finish: Notify,
}
impl ChannelGateway for Gateway {
    fn request<'a>(&'a self, method: &'a str, params: Value) -> ChannelCall<'a, Value> {
        Box::pin(async move {
            self.requests.lock().unwrap().push((method.into(), params));
            self.decision.notify_one();
            Ok(json!({"delivered":true}))
        })
    }
    fn turn(&self, _params: Value) -> ChannelCall<'_, Vec<Value>> {
        Box::pin(async { panic!("must use real stream port") })
    }
    fn stream_turn<'a>(
        &'a self,
        params: Value,
        sink: &'a dyn ChannelEventSink,
    ) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            let message = params["message"].as_str().unwrap().to_owned();
            self.turns.lock().unwrap().push(params);
            match message.as_str() {
                "permission" => {
                    sink.event(json!({"type":"permission_request","requestId":"p","toolName":"write_file","payload":{"path":"fixture"}})).await?;
                    self.ready.notify_one();
                    self.decision.notified().await;
                }
                "question" => {
                    sink.event(json!({"type":"elicitation_request","requestId":"q","questions":[{"question":"Pick","header":"Header","multiSelect":true,"options":[{"label":"A","description":"first"},{"label":"B","description":"second"}]}]})).await?;
                    self.ready.notify_one();
                    self.decision.notified().await;
                }
                "hold" => {
                    self.ready.notify_one();
                    self.finish.notified().await;
                }
                _ => {
                    self.ready.notify_one();
                }
            }
            sink.event(json!({"type":"assistant_text_delta","text":format!("reply {message}")}))
                .await?;
            sink.event(json!({"type":"tool_call_finished","ok":false,"toolName":"read_file"}))
                .await?;
            Ok(())
        })
    }
}
fn options(server: &Server) -> TelegramOptions {
    TelegramOptions {
        token: "fixture-token".into(),
        base_url: server.base.clone(),
        ..Default::default()
    }
}

#[tokio::test]
async fn longpoll_gateway_reply_offset_and_stop_use_real_http() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = TelegramChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    server.send(7, "hello");
    server
        .wait(|requests| {
            requests.iter().any(|(method, params)| {
                method == "sendMessage" && params["text"] == "reply hello\n⚠️ read_file failed"
            })
        })
        .await;
    server
        .wait(|requests| {
            requests
                .iter()
                .any(|(method, params)| method == "getUpdates" && params["offset"] == 8)
        })
        .await;
    server
        .wait(|requests| {
            requests
                .iter()
                .any(|(method, params)| method == "sendChatAction" && params["action"] == "typing")
        })
        .await;
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests[0].0, "deleteWebhook");
    assert_eq!(requests[1].0, "getMe");
    assert!(
        requests
            .iter()
            .any(|(method, params)| method == "sendChatAction" && params["action"] == "typing")
    );
    assert_eq!(
        gateway.turns.lock().unwrap()[0],
        json!({"sessionKey":"telegram:chat=42:general","channelKey":"telegram","message":"hello"})
    );
    assert!(
        channel
            .deliver(&json!({"originSessionKey":"telegram:chat=42:general","text":"cron"}))
            .await
            .unwrap()
    );
    assert!(server.texts().iter().any(|text| text == "cron"));
    handle.close().await;
    assert!(
        !channel
            .deliver(&json!({"originSessionKey":"telegram:chat=42:general","text":"after stop"}))
            .await
            .unwrap()
    );
    assert!(
        channel
            .handle_update(update(10, "after stop"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn busy_input_and_new_are_dropped_while_idle_new_resets_session() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let mut opts = options(&server);
    opts.webhook_url = Some("https://fixture.invalid/telegram".into());
    let channel = TelegramChannel::new(opts, gateway.clone());
    let handle = channel.start().await.unwrap();
    assert_eq!(server.requests.lock().unwrap()[0].0, "setWebhook");
    assert!(
        !server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(method, _)| method == "getUpdates")
    );
    let turn = {
        let channel = channel.clone();
        tokio::spawn(async move { channel.handle_update(update(1, "hold")).await })
    };
    gateway.ready.notified().await;
    channel.handle_update(update(2, "/new")).await.unwrap();
    channel
        .handle_update(update(3, "busy ordinary"))
        .await
        .unwrap();
    assert_eq!(gateway.turns.lock().unwrap().len(), 1);
    assert!(gateway.requests.lock().unwrap().is_empty());
    gateway.finish.notify_one();
    turn.await.unwrap().unwrap();
    channel.handle_update(update(4, "/new")).await.unwrap();
    assert!(server.texts().iter().any(|text| text == "已创建新会话。"));
    channel.handle_update(update(5, "/projects")).await.unwrap();
    let turns = gateway.turns.lock().unwrap();
    assert_eq!(turns[1]["message"], "/projects");
    assert!(
        turns[1]["sessionKey"]
            .as_str()
            .unwrap()
            .starts_with("telegram:chat=42:s_")
    );
    drop(turns);
    handle.close().await;
}

#[tokio::test]
async fn permission_precedes_new_and_responds_on_same_waiting_gateway() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = TelegramChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    server.send(1, "permission");
    gateway.ready.notified().await;
    server.send(2, "/new");
    server
        .wait(|requests| {
            requests.iter().any(|(method, params)| {
                method == "sendMessage"
                    && params["text"] == "请回复 1 允许一次，回复 2 允许本会话，回复 0 拒绝。"
            })
        })
        .await;
    assert!(gateway.requests.lock().unwrap().is_empty());
    assert!(!server.texts().iter().any(|text| text == "已创建新会话。"));
    server.send(3, "2");
    server
        .wait(|requests| {
            requests.iter().any(|(method, params)| {
                method == "sendMessage"
                    && params["text"]
                        .as_str()
                        .is_some_and(|text| text.starts_with("reply permission"))
            })
        })
        .await;
    let requests = gateway.requests.lock().unwrap();
    assert_eq!(requests[0].0, "permission_decide");
    assert_eq!(
        requests[0].1,
        json!({"sessionKey":"telegram:chat=42:general","requestId":"p","decision":"allow","remember":true})
    );
    drop(requests);
    handle.close().await;
}

#[tokio::test]
async fn elicitation_numeric_multiselect_and_cancellation_use_ts_contract() {
    for (answer, expected) in [
        (
            "2,1",
            json!({"type":"answered","answers":{"Pick":["B","A"]}}),
        ),
        ("0", json!({"type":"cancelled","reason":"user cancelled"})),
    ] {
        let server = server().await;
        let gateway = Arc::new(Gateway::default());
        let channel = TelegramChannel::new(options(&server), gateway.clone());
        let handle = channel.start().await.unwrap();
        server.send(1, "question");
        gateway.ready.notified().await;
        server.send(2, answer);
        server
            .wait(|requests| {
                requests.iter().any(|(method, params)| {
                    method == "sendMessage"
                        && params["text"]
                            .as_str()
                            .is_some_and(|text| text.starts_with("reply question"))
                })
            })
            .await;
        assert!(
            server
                .texts()
                .iter()
                .any(|text| text.contains("1. A — first"))
        );
        assert_eq!(gateway.requests.lock().unwrap()[0].0, "elicitation_respond");
        assert_eq!(gateway.requests.lock().unwrap()[0].1["answer"], expected);
        if answer == "0" {
            assert!(server.texts().iter().any(|text| text == "已取消。"));
        }
        handle.close().await;
    }
}

#[tokio::test]
async fn missing_token_fails_before_network_and_stop_cancels_active_stream() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let mut opts = options(&server);
    opts.token.clear();
    assert!(
        TelegramChannel::new(opts, gateway.clone())
            .start()
            .await
            .is_err()
    );
    assert!(server.requests.lock().unwrap().is_empty());
    let channel = TelegramChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    server.send(1, "hold");
    gateway.ready.notified().await;
    tokio::time::timeout(Duration::from_secs(3), handle.close())
        .await
        .unwrap();
    assert!(!channel.chat("42").lock().await.is_active());
}

#[test]
fn telegram_chunking_uses_utf16_limits_and_ts_split_preference() {
    assert_eq!(
        chunk_text("abc\ndef ghi", 6)
            .iter()
            .map(|s| s.to_utf8().unwrap())
            .collect::<Vec<_>>(),
        vec!["abc", "def", " ghi"]
    );
    assert_eq!(
        chunk_text("abc\n\ndef", 5)
            .iter()
            .map(|s| s.to_utf8().unwrap())
            .collect::<Vec<_>>(),
        vec!["abc\n", "def"]
    );
    assert_eq!(
        chunk_text("", 4096)
            .iter()
            .map(|s| s.to_utf8().unwrap())
            .collect::<Vec<_>>(),
        vec![""]
    );
    let chunks = chunk_text(&"x".repeat(8193), 4096);
    assert_eq!(
        chunks.iter().map(|s| s.len_utf16()).collect::<Vec<_>>(),
        vec![4096, 4096, 1]
    );
}

// TS TelegramChannel.ts:220-232 slices UTF-16 units before JSON.stringify. The wire must keep
// the unmatched high/low surrogate when an emoji crosses the fixed limit.
#[tokio::test]
async fn outgoing_chunks_preserve_split_surrogates_on_actual_http_wire() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let captured = tokio::spawn(async move {
        let mut bodies = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 8192];
            let (header, length) = loop {
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|s| s.trim().parse::<usize>().ok())
                        })
                        .unwrap();
                    break (end + 4, length);
                }
            };
            while bytes.len() < header + length {
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
            }
            bodies.push(String::from_utf8(bytes[header..header + length].to_vec()).unwrap());
            let body = r#"{"ok":true,"result":true}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        bodies
    });
    let channel = TelegramChannel::new(
        TelegramOptions {
            token: "fixture-token".into(),
            base_url: format!("http://{address}/"),
            ..Default::default()
        },
        Arc::new(Gateway::default()),
    );
    channel.running.store(true, Ordering::SeqCst);
    let text = format!("{}😀", "a".repeat(4096 - 1));
    assert!(channel.send_reply("42", &text).await);
    let bodies = captured.await.unwrap();
    let parsed = bodies
        .iter()
        .map(|s| pilotdeck_protocol::parse_json(s).unwrap())
        .collect::<Vec<_>>();
    let first = parsed[0].get("text").unwrap().as_string().unwrap();
    let second = parsed[1].get("text").unwrap().as_string().unwrap();
    assert_eq!(first.len_utf16(), 4096);
    assert_eq!(first.units().last(), Some(&0xd83d));
    assert_eq!(second.units(), &[0xde00]);
}

#[tokio::test]
async fn production_inbound_handle_update_submits_through_gateway_stream_turn() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = TelegramChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    server.send(11, "production inbound");
    gateway.ready.notified().await;
    assert_eq!(
        gateway.turns.lock().unwrap()[0],
        json!({"sessionKey":"telegram:chat=42:general","channelKey":"telegram","message":"production inbound"})
    );
    handle.close().await;
}

#[tokio::test]
async fn production_interaction_permission_decide_before_assistant_stream() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = TelegramChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    server.send(12, "permission");
    gateway.ready.notified().await;
    server.send(13, "1");
    server
        .wait(|requests| {
            requests.iter().any(|(method, _)| method == "sendMessage")
                && !gateway.requests.lock().unwrap().is_empty()
        })
        .await;
    assert_eq!(gateway.requests.lock().unwrap()[0].0, "permission_decide");
    handle.close().await;
}

#[tokio::test]
async fn production_cron_deliver_routes_through_telegram_owner() {
    let server = server().await;
    let channel = TelegramChannel::new(options(&server), Arc::new(Gateway::default()));
    let handle = channel.start().await.unwrap();
    assert!(
        channel
            .deliver(&json!({
                "originChannelKey":"telegram",
                "channelKey":"slack",
                "originSessionKey":"telegram:chat=42:general",
                "text":"production cron"
            }))
            .await
            .unwrap()
    );
    server
        .wait(|requests| {
            requests.iter().any(|(method, params)| {
                method == "sendMessage" && params["text"].as_str() == Some("production cron")
            })
        })
        .await;
    handle.close().await;
}

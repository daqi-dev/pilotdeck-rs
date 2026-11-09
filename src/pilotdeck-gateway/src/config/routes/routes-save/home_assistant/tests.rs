use super::*;
use pilotdeck_protocol::frame::{self, Decoder};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
};

pub(crate) struct Server {
    pub base: String,
    pub incoming: mpsc::UnboundedSender<Value>,
    pub requests: Arc<StdMutex<Vec<(String, Value)>>>,
    pub changed: Arc<Notify>,
    pub service_success: Arc<AtomicBool>,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    pub async fn wait(&self, predicate: impl Fn(&[(String, Value)]) -> bool) {
        tokio::time::timeout(Duration::from_secs(4), async {
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
    pub fn replies(&self) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(kind, _)| kind == "call_service" || kind == "post")
            .map(|(_, value)| value.clone())
            .collect()
    }
}
pub(crate) async fn server(mattermost: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/prefix", listener.local_addr().unwrap());
    let requests = Arc::new(StdMutex::new(vec![]));
    let recorded = requests.clone();
    let changed = Arc::new(Notify::new());
    let notify = changed.clone();
    let service_success = Arc::new(AtomicBool::new(true));
    let success = service_success.clone();
    let (incoming, receiver) = mpsc::unbounded_channel::<Value>();
    let receiver = Arc::new(Mutex::new(receiver));
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let recorded = recorded.clone();
            let notify = notify.clone();
            let receiver = receiver.clone();
            let success = success.clone();
            connections.spawn(async move {
                let mut bytes = Vec::new(); let mut buffer = [0;4096];
                let (header, headers, path) = loop {
                    let count = stream.read(&mut buffer).await.unwrap(); if count == 0 { return; } bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).into_owned(); let path = headers.split_whitespace().nth(1).unwrap().to_owned(); break (end+4, headers, path);
                    }
                };
                if headers.to_ascii_lowercase().contains("upgrade: websocket") {
                    if mattermost {
                        assert_eq!(path, "/prefix/api/v4/websocket?token=test%20token");
                        assert!(headers.contains("origin: http://127.0.0.1:"));
                    } else { assert_eq!(path, "/prefix/api/websocket"); }
                    recorded.lock().unwrap().push(("ws".into(), json!(path))); notify.notify_waiters();
                    let key = headers.lines().find_map(|line| line.split_once(':').filter(|(key,_)|key.eq_ignore_ascii_case("sec-websocket-key")).map(|(_,value)|value.trim())).unwrap();
                    let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
                    stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await.unwrap();
                    if !mattermost { stream.write_all(&frame::server_frame(1,b"{\"type\":\"auth_required\"}")).await.unwrap(); }
                    let mut decoder = Decoder::default(); decoder.buffer.extend_from_slice(&bytes[header..]);
                    loop {
                        while let Some(frame) = decoder.next().unwrap() {
                            if frame.opcode == 8 { return; }
                            if frame.opcode != 1 { continue; }
                            let value: Value = serde_json::from_slice(&frame.payload).unwrap();
                            let kind = value["type"].as_str().unwrap_or("json").to_owned();
                            recorded.lock().unwrap().push((kind.clone(),value.clone())); notify.notify_waiters();
                            let reply = if kind == "auth" { assert_eq!(value["access_token"],"test token"); Some(json!({"type":"auth_ok"})) }
                            else if kind == "call_service" { Some(json!({"type":"result","id":value["id"],"success":success.load(Ordering::SeqCst)})) }
                            else { None };
                            if let Some(reply)=reply { if stream.write_all(&frame::server_frame(1,reply.to_string().as_bytes())).await.is_err(){return;} }
                        }
                        tokio::select! {
                            count = stream.read_buf(&mut decoder.buffer) => { if count.unwrap_or(0)==0{return;} }
                            value = async { receiver.lock().await.recv().await } => {
                                let Some(value) = value else { return; };
                                if value["type"] == "__close" { let _ = stream.write_all(&frame::server_frame(8,&1000u16.to_be_bytes())).await; return; }
                                if stream.write_all(&frame::server_frame(1,value.to_string().as_bytes())).await.is_err(){return;}
                            }
                        }
                    }
                }
                assert!(mattermost);
                assert!(headers.to_ascii_lowercase().contains("authorization: bearer test token"));
                let body = if path == "/prefix/users/me" { recorded.lock().unwrap().push(("me".into(), Value::Null)); json!({"id":"bot"}) }
                else {
                    assert_eq!(path, "/prefix/posts");
                    let length = headers.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|value|value.trim().parse::<usize>().ok())).unwrap();
                    while bytes.len()<header+length { let count=stream.read(&mut buffer).await.unwrap(); if count==0{return;} bytes.extend_from_slice(&buffer[..count]); }
                    let body:Value=serde_json::from_slice(&bytes[header..header+length]).unwrap(); recorded.lock().unwrap().push(("post".into(),body)); json!({"id":"reply"})
                };
                notify.notify_waiters();
                let body=body.to_string(); let _=stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
            });
            while connections.try_join_next().is_some() {}
        }
    });
    Server {
        base,
        incoming,
        requests,
        changed,
        service_success,
        task,
    }
}
#[derive(Default)]
pub(crate) struct Gateway {
    pub turns: StdMutex<Vec<Value>>,
    pub requests: StdMutex<Vec<(String, Value)>>,
    pub ready: Notify,
    pub release: Notify,
    pub cancelled: AtomicBool,
}
struct Cancelled<'a>(&'a AtomicBool);
impl Drop for Cancelled<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
impl ChannelGateway for Gateway {
    fn request<'a>(&'a self, method: &'a str, params: Value) -> ChannelCall<'a, Value> {
        Box::pin(async move {
            self.requests.lock().unwrap().push((method.into(), params));
            self.release.notify_one();
            Ok(json!({"delivered":true}))
        })
    }
    fn turn(&self, _: Value) -> ChannelCall<'_, Vec<Value>> {
        Box::pin(async { panic!("must use streaming turn") })
    }
    fn stream_turn<'a>(
        &'a self,
        params: Value,
        sink: &'a dyn ChannelEventSink,
    ) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            let text = params["message"].as_str().unwrap().to_owned();
            self.turns.lock().unwrap().push(params);
            match text.as_str() {
                "hold" => {
                    let _cancelled = Cancelled(&self.cancelled);
                    self.ready.notify_one();
                    self.release.notified().await;
                }
                "permission" => {
                    sink.event(json!({"type":"permission_request","requestId":"P","toolName":"write_file","payload":{"file":"x"}})).await?;
                    self.ready.notify_one();
                    self.release.notified().await;
                }
                "question" => {
                    sink.event(json!({"type":"elicitation_request","requestId":"Q","questions":[{"header":"Header","question":"Pick","options":[{"label":"A","description":"first"},{"label":"B","description":"second"}]}]})).await?;
                    self.ready.notify_one();
                    self.release.notified().await;
                }
                _ => {}
            }
            sink.event(json!({"type":"assistant_thinking_delta","text":"secret"}))
                .await?;
            sink.event(json!({"type":"assistant_text_delta","text":format!("reply {text}")}))
                .await?;
            Ok(())
        })
    }
}
fn options(server: &Server) -> HomeAssistantOptions {
    HomeAssistantOptions {
        url: server.base.clone(),
        token: "test token".into(),
        ..Default::default()
    }
}
fn state(chat: &str, old: &str, new: &str) -> Value {
    json!({"type":"event","event":{"event_type":"state_changed","data":{"entity_id":chat,"old_state":{"state":old},"new_state":{"state":new}}}})
}
async fn idle(channel: &HomeAssistantChannel, chat: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if !channel.chat(chat).lock().await.is_active() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn websocket_auth_subscription_state_filter_stream_and_service_ack() {
    let server = server(false).await;
    let gateway = Arc::new(Gateway::default());
    let channel = HomeAssistantChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    server
        .incoming
        .send(state("sensor.x", "before", "ignore"))
        .unwrap();
    server
        .incoming
        .send(state("conversation.x", "same", "same"))
        .unwrap();
    server
        .incoming
        .send(state("conversation.x", "before", "你好"))
        .unwrap();
    server
        .wait(|requests| requests.iter().any(|(kind, _)| kind == "call_service"))
        .await;
    idle(&channel, "conversation.x").await;
    assert_eq!(
        gateway.turns.lock().unwrap().as_slice(),
        &[
            json!({"sessionKey":"homeassistant:chat=conversation.x:general","channelKey":"homeassistant","message":"你好"})
        ]
    );
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(kind, value)| kind == "subscribe_events"
                && value["event_type"] == "state_changed")
    );
    let reply = &server.replies()[0];
    assert_eq!(reply["domain"], "persistent_notification");
    assert_eq!(reply["service_data"]["title"], "Gateway · conversation.x");
    assert_eq!(reply["service_data"]["message"], "reply 你好");
    assert!(channel.deliver(&json!({"originChannelKey":"homeassistant","originSessionKey":"homeassistant:chat=conversation.x:general","text":"cron"})).await.unwrap());
    server.service_success.store(false, Ordering::SeqCst);
    assert!(
        !channel
            .deliver(
                &json!({"sessionKey":"homeassistant:chat=conversation.x:general","text":"failed"})
            )
            .await
            .unwrap()
    );
    assert!(!channel.deliver(&json!({"originChannelKey":"signal","sessionKey":"homeassistant:chat=conversation.x:general","text":"wrong"})).await.unwrap());
    handle.close().await;
    assert!(
        !channel
            .deliver(
                &json!({"sessionKey":"homeassistant:chat=conversation.x:general","text":"stopped"})
            )
            .await
            .unwrap()
    );
}
#[tokio::test]
async fn websocket_permission_and_elicitation_answers_resume_live_turn() {
    for (input, answer, method) in [
        ("permission", "2", "permission_decide"),
        ("question", "2", "elicitation_respond"),
    ] {
        let server = server(false).await;
        let gateway = Arc::new(Gateway::default());
        let channel = HomeAssistantChannel::new(options(&server), gateway.clone());
        let handle = channel.start().await.unwrap();
        server
            .incoming
            .send(state("conversation.x", "before", input))
            .unwrap();
        gateway.ready.notified().await;
        server
            .incoming
            .send(state("conversation.x", input, answer))
            .unwrap();
        server
            .wait(|requests| {
                requests
                    .iter()
                    .any(|(_, body)| body["service_data"]["message"] == format!("reply {input}"))
            })
            .await;
        let requests = gateway.requests.lock().unwrap();
        assert_eq!(requests[0].0, method);
        if method == "permission_decide" {
            assert_eq!(requests[0].1["remember"], true);
        } else {
            assert_eq!(
                requests[0].1["answer"],
                json!({"type":"answered","answers":{"Pick":"B"}})
            );
        }
        drop(requests);
        handle.close().await;
    }
}
#[tokio::test]
async fn new_session_busy_filter_and_close_cancel_owned_turn() {
    let server = server(false).await;
    let gateway = Arc::new(Gateway::default());
    let channel = HomeAssistantChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    channel.incoming("conversation.x", "/new").await.unwrap();
    server
        .incoming
        .send(state("conversation.x", "before", "hold"))
        .unwrap();
    gateway.ready.notified().await;
    channel
        .incoming("conversation.x", "/new ignored")
        .await
        .unwrap();
    assert_eq!(gateway.turns.lock().unwrap().len(), 1);
    assert!(
        gateway.turns.lock().unwrap()[0]["sessionKey"]
            .as_str()
            .unwrap()
            .starts_with("homeassistant:chat=conversation.x:s_")
    );
    tokio::time::timeout(Duration::from_secs(3), handle.close())
        .await
        .unwrap();
    assert!(gateway.cancelled.load(Ordering::SeqCst));
}
#[test]
fn websocket_url_paths_match_ts() {
    assert_eq!(
        websocket_url("https://example.test/sub/").unwrap(),
        "wss://example.test/sub/api/websocket"
    );
    assert_eq!(
        websocket_url("localhost:8123").unwrap(),
        "ws://localhost:8123/api/websocket"
    );
}

#[tokio::test]
async fn reconnect_reauthenticates_and_resubscribes_after_socket_close() {
    let server = server(false).await;
    let gateway = Arc::new(Gateway::default());
    let channel = HomeAssistantChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    // start() returns after writing the subscription. Observe receipt before
    // forcing close, otherwise the fixture can close before recording the
    // first subscription and the reconnect count never reaches two.
    server
        .wait(|requests| requests.iter().any(|(kind, _)| kind == "subscribe_events"))
        .await;
    server.incoming.send(json!({"type":"__close"})).unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if server
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(kind, _)| kind == "subscribe_events")
                .count()
                == 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    server
        .incoming
        .send(state("conversation.x", "old", "after reconnect"))
        .unwrap();
    server
        .wait(|requests| {
            requests
                .iter()
                .any(|(_, body)| body["service_data"]["message"] == "reply after reconnect")
        })
        .await;
    handle.close().await;
}
#[tokio::test]
async fn missing_token_fails_before_opening_a_socket() {
    let channel = HomeAssistantChannel::new(
        HomeAssistantOptions::default(),
        Arc::new(Gateway::default()),
    );
    assert!(channel.start().await.is_err());
    assert!(!channel.running.load(Ordering::SeqCst));
}

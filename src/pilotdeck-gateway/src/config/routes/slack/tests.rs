use super::*;
use pilotdeck_protocol::frame::{self, Decoder};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Notify, mpsc},
};

struct Server {
    base: String,
    events: mpsc::UnboundedSender<Value>,
    requests: Arc<StdMutex<Vec<(String, Value)>>>,
    acknowledgements: Arc<StdMutex<Vec<Value>>>,
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
    let address = listener.local_addr().unwrap();
    let base = format!("http://{address}/api/");
    let websocket = format!("ws://{address}/socket");
    let (events, receiver) = mpsc::unbounded_channel::<Value>();
    let receiver = Arc::new(Mutex::new(receiver));
    let requests = Arc::new(StdMutex::new(Vec::new()));
    let recorded = requests.clone();
    let acknowledgements = Arc::new(StdMutex::new(Vec::new()));
    let acks = acknowledgements.clone();
    let changed = Arc::new(Notify::new());
    let notify = changed.clone();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let receiver = receiver.clone();
            let recorded = recorded.clone();
            let acks = acks.clone();
            let notify = notify.clone();
            let websocket = websocket.clone();
            connections.spawn(async move {
                let mut bytes=Vec::new();let mut buffer=[0;4096];
                let(header,path,headers)=loop{let count=stream.read(&mut buffer).await.unwrap();if count==0{return}bytes.extend_from_slice(&buffer[..count]);if let Some(end)=bytes.windows(4).position(|s|s==b"\r\n\r\n"){let headers=String::from_utf8_lossy(&bytes[..end]).into_owned();let path=headers.split_whitespace().nth(1).unwrap().to_owned();break(end+4,path,headers)}};
                if path=="/socket" {
                    let key=headers.lines().find_map(|line|line.split_once(':').filter(|(key,_)|key.eq_ignore_ascii_case("sec-websocket-key")).map(|(_,value)|value.trim())).unwrap();let accept=tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await.unwrap();stream.write_all(&frame::server_frame(1,json!({"type":"hello"}).to_string().as_bytes())).await.unwrap();
                    let mut decoder=Decoder::default();decoder.buffer.extend_from_slice(&bytes[header..]);
                    loop {
                        tokio::select! {
                            event=async{receiver.lock().await.recv().await}=>{let Some(event)=event else{return};if stream.write_all(&frame::server_frame(1,event.to_string().as_bytes())).await.is_err(){return}},
                            incoming=read_frame(&mut stream,&mut decoder)=>{match incoming{Some(incoming) if incoming.opcode==8=>return,Some(incoming) if incoming.opcode==1=>{acks.lock().unwrap().push(serde_json::from_slice(&incoming.payload).unwrap());notify.notify_one();},Some(_)=>{},None=>return}}
                        }
                    }
                }
                let length=headers.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v|v.trim().parse::<usize>().ok())).unwrap();while bytes.len()<header+length{let count=stream.read(&mut buffer).await.unwrap();if count==0{return}bytes.extend_from_slice(&buffer[..count]);}
                let method=path.rsplit('/').next().unwrap().to_owned();let params:Value=serde_json::from_slice(&bytes[header..header+length]).unwrap();let token=if method=="apps.connections.open"{"app-fixture"}else{"bot-fixture"};assert!(headers.to_ascii_lowercase().contains(&format!("authorization: bearer {token}")));recorded.lock().unwrap().push((method.clone(),params));notify.notify_one();
                let result=match method.as_str(){"apps.connections.open"=>json!({"ok":true,"url":websocket}),"auth.test"=>json!({"ok":true,"user_id":"BOT","user":"fixture"}),_=>json!({"ok":true})};let body=result.to_string();let _=stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
            });
            while connections.try_join_next().is_some() {}
        }
    });
    Server {
        base,
        events,
        requests,
        acknowledgements,
        changed,
        task,
    }
}
async fn read_frame(stream: &mut TcpStream, decoder: &mut Decoder) -> Option<frame::Frame> {
    loop {
        if let Some(frame) = decoder.next().unwrap() {
            return Some(frame);
        }
        if stream.read_buf(&mut decoder.buffer).await.ok()? == 0 {
            return None;
        }
    }
}
impl Server {
    fn send(&self, id: &str, event: Value) {
        self.events
            .send(json!({"envelope_id":id,"type":"events_api","payload":{"event":event}}))
            .unwrap();
    }
    async fn wait(&self, predicate: impl Fn(&[(String, Value)]) -> bool) {
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
    async fn ack(&self, id: &str) {
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let notified = self.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self
                    .acknowledgements
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|event| event["envelope_id"] == id)
                {
                    return;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
    }
    fn replies(&self) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method == "chat.postMessage")
            .map(|(_, params)| params.clone())
            .collect()
    }
}
#[derive(Default)]
struct Gateway {
    turns: StdMutex<Vec<Value>>,
    requests: StdMutex<Vec<(String, Value)>>,
    ready: Notify,
    release: Notify,
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
        Box::pin(async { panic!("expected live stream") })
    }
    fn stream_turn<'a>(
        &'a self,
        params: Value,
        sink: &'a dyn ChannelEventSink,
    ) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            let input = params["message"].as_str().unwrap().to_owned();
            self.turns.lock().unwrap().push(params);
            match input.as_str() {
                "hold" => {
                    self.ready.notify_one();
                    self.release.notified().await;
                }
                "permission" => {
                    sink.event(json!({"type":"permission_request","requestId":"P","toolName":"write_file","payload":{}})).await?;
                    self.ready.notify_one();
                    self.release.notified().await;
                }
                "question" => {
                    sink.event(json!({"type":"elicitation_request","requestId":"Q","questions":[{"question":"Pick","header":"Header","options":[{"label":"A","description":"first"},{"label":"B","description":"second"}]}]})).await?;
                    self.ready.notify_one();
                    self.release.notified().await;
                }
                _ => {}
            }
            sink.event(json!({"type":"assistant_text_delta","text":format!("**reply** [link](https://example.invalid) {input}")})).await?;
            Ok(())
        })
    }
}
fn options(server: &Server) -> SlackOptions {
    SlackOptions {
        bot_token: "bot-fixture".into(),
        app_token: "app-fixture".into(),
        base_url: server.base.clone(),
    }
}
fn message(text: &str) -> Value {
    json!({"type":"message","channel":"C","user":"USER","text":text,"ts":"2"})
}

#[tokio::test]
async fn socket_mode_acks_real_events_and_posts_formatted_thread_reply() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = SlackChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    let mut event = message("<@BOT> hello");
    event["thread_ts"] = "1".into();
    server.send("one", event);
    server.ack("one").await;
    server
        .wait(|requests| {
            requests.iter().any(|(method, params)| {
                method == "chat.postMessage"
                    && params["text"] == "*reply* <https://example.invalid|link> hello"
            })
        })
        .await;
    assert_eq!(
        gateway.turns.lock().unwrap()[0],
        json!({"sessionKey":"slack:chat=C:1:general","channelKey":"slack","message":"hello"})
    );
    let replies = server.replies();
    assert_eq!(replies[0]["channel"], "C");
    assert_eq!(replies[0]["thread_ts"], "1");
    assert_eq!(replies[0]["mrkdwn"], true);
    handle.close().await;
}

#[tokio::test]
async fn socket_filters_bot_edits_and_busy_inputs_but_idle_new_maps_session() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = SlackChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    for (id, change) in [
        ("bot", json!({"bot_id":"B"})),
        ("edit", json!({"subtype":"message_changed"})),
        ("self", json!({"user":"BOT"})),
    ] {
        let mut event = message("ignored");
        event
            .as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        server.send(id, event);
        server.ack(id).await;
    }
    server.send("hold", message("hold"));
    gateway.ready.notified().await;
    server.send("busy", message("/new"));
    server.ack("busy").await;
    channel.message(message("busy other")).await.unwrap();
    assert_eq!(gateway.turns.lock().unwrap().len(), 1);
    assert!(gateway.requests.lock().unwrap().is_empty());
    gateway.release.notify_one();
    server
        .wait(|requests| {
            requests
                .iter()
                .any(|(method, _)| method == "chat.postMessage")
        })
        .await;
    // Direct delivery of the next update uses the same production handler and
    // waits for the previous reply so the active guard has been released.
    loop {
        if !channel.chat("C").lock().await.is_active() {
            break;
        }
        tokio::task::yield_now().await;
    }
    channel.message(message("/new")).await.unwrap();
    channel.message(message("/projects")).await.unwrap();
    let turns = gateway.turns.lock().unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1]["message"], "/projects");
    assert!(
        turns[1]["sessionKey"]
            .as_str()
            .unwrap()
            .starts_with("slack:chat=C:s_")
    );
    drop(turns);
    handle.close().await;
}

#[tokio::test]
async fn socket_permission_and_elicitation_replies_reach_live_turn() {
    for (input, answer, method) in [
        ("permission", "2", "permission_decide"),
        ("question", "2", "elicitation_respond"),
    ] {
        let server = server().await;
        let gateway = Arc::new(Gateway::default());
        let channel = SlackChannel::new(options(&server), gateway.clone());
        let handle = channel.start().await.unwrap();
        server.send("prompt", message(input));
        gateway.ready.notified().await;
        server.send("answer", message(answer));
        server.ack("answer").await;
        server
            .wait(|requests| {
                requests.iter().any(|(method, params)| {
                    method == "chat.postMessage"
                        && params["text"]
                            .as_str()
                            .is_some_and(|text| text.starts_with("*reply*"))
                })
            })
            .await;
        let requests = gateway.requests.lock().unwrap();
        assert_eq!(requests[0].0, method);
        assert_eq!(requests[0].1["sessionKey"], "slack:chat=C:general");
        if input == "permission" {
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
async fn socket_disconnect_reopens_connection_and_close_cancels_active_turn() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = SlackChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    server
        .events
        .send(json!({"type":"disconnect","reason":"refresh_requested"}))
        .unwrap();
    server
        .wait(|requests| {
            requests
                .iter()
                .filter(|(method, _)| method == "apps.connections.open")
                .count()
                >= 2
        })
        .await;
    server.send("hold", message("hold"));
    gateway.ready.notified().await;
    tokio::time::timeout(Duration::from_secs(4), handle.close())
        .await
        .unwrap();
    assert!(!channel.chat("C").lock().await.is_active());
}

#[tokio::test]
async fn missing_slack_credentials_fail_before_network() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let mut opts = options(&server);
    opts.bot_token.clear();
    assert!(
        SlackChannel::new(opts, gateway.clone())
            .start()
            .await
            .is_err()
    );
    let mut opts = options(&server);
    opts.app_token.clear();
    assert!(SlackChannel::new(opts, gateway).start().await.is_err());
    assert!(server.requests.lock().unwrap().is_empty());
}

#[test]
fn slack_formatting_and_chunking_match_ts() {
    assert_eq!(
        format_mrkdwn("**bold** [name](https://example.invalid)"),
        "*bold* <https://example.invalid|name>"
    );
    assert_eq!(format_mrkdwn("**line\nbreak**"), "**line\nbreak**");
    assert_eq!(
        chunk_text("abc\ndef ghi", 6)
            .iter()
            .map(|s| s.to_utf8().unwrap())
            .collect::<Vec<_>>(),
        vec!["abc", "def", " ghi"]
    );
    assert_eq!(
        chunk_text(&"x".repeat(39001), 39000)
            .iter()
            .map(|s| s.len_utf16())
            .collect::<Vec<_>>(),
        vec![39000, 1]
    );
}

// TS SlackChannel.ts:256-268 slices UTF-16 units before JSON.stringify. The wire must keep
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
            let body = r#"{"ok":true}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        bodies
    });
    let channel = SlackChannel::new(
        SlackOptions {
            bot_token: "bot-fixture".into(),
            app_token: "app-fixture".into(),
            base_url: format!("http://{address}/api/"),
        },
        Arc::new(Gateway::default()),
    );
    channel.running.store(true, Ordering::SeqCst);
    let text = format!("{}😀", "a".repeat(39000 - 1));
    assert!(
        channel
            .send_reply(
                &SendContext {
                    channel: "C".into(),
                    thread: Some("T".into())
                },
                &text
            )
            .await
    );
    let bodies = captured.await.unwrap();
    let parsed = bodies
        .iter()
        .map(|s| pilotdeck_protocol::parse_json(s).unwrap())
        .collect::<Vec<_>>();
    let first = parsed[0].get("text").unwrap().as_string().unwrap();
    let second = parsed[1].get("text").unwrap().as_string().unwrap();
    assert_eq!(first.len_utf16(), 39000);
    assert_eq!(first.units().last(), Some(&0xd83d));
    assert_eq!(second.units(), &[0xde00]);
}

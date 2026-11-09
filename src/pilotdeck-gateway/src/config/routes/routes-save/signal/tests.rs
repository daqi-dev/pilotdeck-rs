use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Notify, mpsc},
};
struct Server {
    base: String,
    lines: mpsc::UnboundedSender<Vec<u8>>,
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
    let base = format!("http://{}/base", listener.local_addr().unwrap());
    let (lines, receiver) = mpsc::unbounded_channel::<Vec<u8>>();
    let receiver = Arc::new(Mutex::new(receiver));
    let requests = Arc::new(StdMutex::new(Vec::new()));
    let recorded = requests.clone();
    let changed = Arc::new(Notify::new());
    let notify = changed.clone();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let receiver = receiver.clone();
            let recorded = recorded.clone();
            let notify = notify.clone();
            connections.spawn(async move{
        let mut bytes=Vec::new();let mut buffer=[0;4096];let(header,path,headers)=loop{let count=stream.read(&mut buffer).await.unwrap();if count==0{return}bytes.extend_from_slice(&buffer[..count]);if let Some(end)=bytes.windows(4).position(|s|s==b"\r\n\r\n"){let headers=String::from_utf8_lossy(&bytes[..end]).into_owned();let path=headers.split_whitespace().nth(1).unwrap().to_owned();break(end+4,path,headers)}};
        if path.starts_with("/base/v1/receive/"){assert_eq!(path,"/base/v1/receive/%2B123");assert!(headers.to_ascii_lowercase().contains("accept: text/event-stream, application/json, */*"));recorded.lock().unwrap().push(("receive".into(),Value::Null));notify.notify_one();if stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.is_err(){return}loop{let Some(bytes)=receiver.lock().await.recv().await else{return};if stream.write_all(format!("{:x}\r\n",bytes.len()).as_bytes()).await.is_err(){return}if stream.write_all(&bytes).await.is_err(){return}if stream.write_all(b"\r\n").await.is_err(){return}}
        }
        assert_eq!(path,"/base/v2/send");let length=headers.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|value|value.trim().parse::<usize>().ok())).unwrap();while bytes.len()<header+length{let count=stream.read(&mut buffer).await.unwrap();if count==0{return}bytes.extend_from_slice(&buffer[..count]);}let body:Value=serde_json::from_slice(&bytes[header..header+length]).unwrap();recorded.lock().unwrap().push(("send".into(),body));notify.notify_one();let _=stream.write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
    });
            while connections.try_join_next().is_some() {}
        }
    });
    Server {
        base,
        lines,
        requests,
        changed,
        task,
    }
}
impl Server {
    fn send(&self, value: Value) {
        self.lines
            .send(format!("data: {value}\r\n").into_bytes())
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
    fn replies(&self) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method == "send")
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
        Box::pin(async { panic!("requires live stream") })
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
            sink.event(json!({"type":"assistant_text_delta","text":format!("reply {input}")}))
                .await?;
            Ok(())
        })
    }
}
fn options(server: &Server) -> SignalOptions {
    SignalOptions {
        rest_url: server.base.clone(),
        account: "+123".into(),
    }
}
fn envelope(text: &str) -> Value {
    json!({"envelope":{"source":"+456","dataMessage":{"message":text,"timestamp":1}}})
}

#[tokio::test]
async fn real_sse_partial_utf8_lines_stream_to_authenticated_account_send() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = SignalChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    let line = format!("data: {}\r\n", envelope("你好"));
    let split = line
        .as_bytes()
        .iter()
        .position(|byte| *byte >= 0x80)
        .unwrap()
        + 1;
    server
        .lines
        .send(line.as_bytes()[..split].to_vec())
        .unwrap();
    server
        .lines
        .send(line.as_bytes()[split..].to_vec())
        .unwrap();
    server
        .wait(|requests| {
            requests
                .iter()
                .any(|(kind, body)| kind == "send" && body["message"] == "reply 你好")
        })
        .await;
    assert_eq!(
        gateway.turns.lock().unwrap()[0],
        json!({"sessionKey":"signal:chat=dm:+456:general","channelKey":"signal","message":"你好"})
    );
    let replies = server.replies();
    assert_eq!(replies[0]["number"], "+123");
    assert_eq!(replies[0]["recipients"], json!(["+456"]));
    assert!(channel.deliver(&json!({"originChannelKey":"signal","originSessionKey":"signal:chat=dm:+456:general","text":"cron"})).await.unwrap());
    handle.close().await;
}

#[tokio::test]
async fn group_session_keeps_source_recipient_and_sync_message_precedes_data() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = SignalChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    server.send(json!({"envelope":{"source":"+source","syncMessage":{"sentMessage":{"message":"sent text","groupInfo":{"groupId":"sent-group"}}},"dataMessage":{"message":"data text","groupInfo":{"groupId":"data-group"}}}}));
    server
        .wait(|requests| requests.iter().any(|(kind, _)| kind == "send"))
        .await;
    assert_eq!(
        gateway.turns.lock().unwrap()[0]["sessionKey"],
        "signal:chat=group:data-group:general"
    );
    assert_eq!(gateway.turns.lock().unwrap()[0]["message"], "sent text");
    assert_eq!(server.replies()[0]["recipients"], json!(["+source"]));
    handle.close().await;
}

#[tokio::test]
async fn sse_permission_and_elicitation_answers_reach_same_live_gateway() {
    for (input, answer, method) in [
        ("permission", "2", "permission_decide"),
        ("question", "2", "elicitation_respond"),
    ] {
        let server = server().await;
        let gateway = Arc::new(Gateway::default());
        let channel = SignalChannel::new(options(&server), gateway.clone());
        let handle = channel.start().await.unwrap();
        server.send(envelope(input));
        gateway.ready.notified().await;
        server.send(envelope(answer));
        server
            .wait(|requests| {
                requests.iter().any(|(kind, body)| {
                    kind == "send"
                        && body["message"]
                            .as_str()
                            .is_some_and(|text| text.starts_with("reply "))
                })
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
async fn idle_new_resets_but_busy_new_is_dropped_and_stop_cancels_stream() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = SignalChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    channel
        .parse_line(&envelope("/new").to_string())
        .await
        .unwrap();
    channel
        .parse_line(&envelope("/projects").to_string())
        .await
        .unwrap();
    assert_eq!(gateway.turns.lock().unwrap()[0]["message"], "/projects");
    assert!(
        gateway.turns.lock().unwrap()[0]["sessionKey"]
            .as_str()
            .unwrap()
            .starts_with("signal:chat=dm:+456:s_")
    );
    server.send(envelope("hold"));
    gateway.ready.notified().await;
    channel
        .parse_line(&envelope("/new").to_string())
        .await
        .unwrap();
    assert_eq!(gateway.turns.lock().unwrap().len(), 2);
    tokio::time::timeout(Duration::from_secs(3), handle.close())
        .await
        .unwrap();
    assert!(!channel.chat("dm:+456").lock().await.is_active());
    assert!(
        !channel
            .deliver(&json!({"originSessionKey":"signal:chat=dm:+456:general","text":"after stop"}))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn signal_empty_account_fails_and_sse_control_lines_do_not_submit_turn() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    assert!(
        SignalChannel::new(SignalOptions::default(), gateway.clone())
            .start()
            .await
            .is_err()
    );
    let channel = SignalChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    for line in ["data: [DONE]", ":ok", "event: message", "{broken}", "{}"] {
        channel.parse_line(line).await.unwrap();
    }
    assert!(gateway.turns.lock().unwrap().is_empty());
    handle.close().await;
}

#[test]
fn signal_chunking_and_prefix_rules_match_ts() {
    assert_eq!(
        chunk_text("abc\ndef ghi", 6)
            .iter()
            .map(|s| s.to_utf8().unwrap())
            .collect::<Vec<_>>(),
        vec!["abc", "def", " ghi"]
    );
    assert_eq!(strip_chat_prefix("group:g"), "g");
    assert_eq!(strip_chat_prefix("dm:+1"), "+1");
    assert_eq!(
        chunk_text(&"x".repeat(4001), 2000)
            .iter()
            .map(|part| part.len_utf16())
            .collect::<Vec<_>>(),
        vec![2000, 2000, 1]
    );
}

// TS SignalChannel.ts:331-343 slices UTF-16 units before JSON.stringify. The wire must keep
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
            let body = r#"{}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        bodies
    });
    let channel = SignalChannel::new(
        SignalOptions {
            account: "+123".into(),
            rest_url: format!("http://{address}"),
        },
        Arc::new(Gateway::default()),
    );
    channel.running.store(true, Ordering::SeqCst);
    let text = format!("{}😀", "a".repeat(2000 - 1));
    assert!(channel.send_reply("dm:+456", &text).await);
    let bodies = captured.await.unwrap();
    let parsed = bodies
        .iter()
        .map(|s| pilotdeck_protocol::parse_json(s).unwrap())
        .collect::<Vec<_>>();
    let first = parsed[0].get("message").unwrap().as_string().unwrap();
    let second = parsed[1].get("message").unwrap().as_string().unwrap();
    assert_eq!(first.len_utf16(), 2000);
    assert_eq!(first.units().last(), Some(&0xd83d));
    assert_eq!(second.units(), &[0xde00]);
}

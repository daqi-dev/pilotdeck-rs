use super::super::home_assistant::tests::Gateway;
use super::*;
use pilotdeck_protocol::frame::{self, Decoder};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Notify, mpsc},
};
struct Server {
    base: String,
    incoming: mpsc::UnboundedSender<Value>,
    requests: Arc<StdMutex<Vec<(String, Value)>>>,
    changed: Arc<Notify>,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
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
}
async fn server() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = format!("http://{address}");
    let requests = Arc::new(StdMutex::new(vec![]));
    let recorded = requests.clone();
    let changed = Arc::new(Notify::new());
    let notify = changed.clone();
    let (incoming, receiver) = mpsc::unbounded_channel::<Value>();
    let receiver = Arc::new(Mutex::new(receiver));
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let recorded = recorded.clone();
            let notify = notify.clone();
            let receiver = receiver.clone();
            connections.spawn(async move{
        let mut bytes=vec![];let mut buf=[0;4096];let(header,headers,path)=loop{let count=stream.read(&mut buf).await.unwrap();if count==0{return;}bytes.extend_from_slice(&buf[..count]);if let Some(end)=bytes.windows(4).position(|part|part==b"\r\n\r\n"){let headers=String::from_utf8_lossy(&bytes[..end]).into_owned();let path=headers.split_whitespace().nth(1).unwrap().to_owned();break(end+4,headers,path);}};
        if path=="/stream?ticket=test-ticket"{
            let key=headers.lines().find_map(|line|line.split_once(':').filter(|(key,_)|key.eq_ignore_ascii_case("sec-websocket-key")).map(|(_,value)|value.trim())).unwrap();let accept=tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await.unwrap();let mut decoder=Decoder::default();decoder.buffer.extend_from_slice(&bytes[header..]);
            loop{while let Some(frame)=decoder.next().unwrap(){if frame.opcode==8{return;}if frame.opcode==1{let value:Value=serde_json::from_slice(&frame.payload).unwrap();recorded.lock().unwrap().push(("ack".into(),value));notify.notify_waiters();}}
                tokio::select!{count=stream.read_buf(&mut decoder.buffer)=>if count.unwrap_or(0)==0{return;},value=async{receiver.lock().await.recv().await}=>{let Some(value)=value else{return;};if value["type"]=="__close"{let _=stream.write_all(&frame::server_frame(8,&1000u16.to_be_bytes())).await;return;}if stream.write_all(&frame::server_frame(1,value.to_string().as_bytes())).await.is_err(){return;}}}
            }
        }
        let length=headers.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|value|value.trim().parse::<usize>().ok())).unwrap();while bytes.len()<header+length{let count=stream.read(&mut buf).await.unwrap();if count==0{return;}bytes.extend_from_slice(&buf[..count]);}let body:Value=serde_json::from_slice(&bytes[header..header+length]).unwrap();let reply=if path=="/open"{assert_eq!(body,json!({"clientId":"client","clientSecret":"secret","ua":"","subscriptions":[{"type":"EVENT","topic":"*"}]}));json!({"endpoint":format!("ws://{address}/stream"),"ticket":"test-ticket"})}else{assert_eq!(path,"/robot/send?session=valid");assert_eq!(body["msgtype"],"markdown");json!({"errcode":0})};recorded.lock().unwrap().push((path,body));notify.notify_waiters();let reply=reply.to_string();let _=stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",reply.len()).as_bytes()).await;
    });
            while connections.try_join_next().is_some() {}
        }
    });
    Server {
        base,
        incoming,
        requests,
        changed,
        task,
    }
}
fn options(server: &Server) -> DingTalkOptions {
    DingTalkOptions {
        client_id: " client ".into(),
        client_secret: " secret ".into(),
        gateway_url: format!("{}/open", server.base),
        webhook_transport_origin: Some(server.base.clone()),
    }
}
fn event(id: &str, text: &str) -> Value {
    json!({"type":"EVENT","headers":{"topic":ROBOT_TOPIC,"messageId":id},"data":json!({"msgId":id,"conversationId":"chat","senderId":"sender","text":{"content":text},"sessionWebhook":"https://api.dingtalk.com/robot/send?session=valid"}).to_string()})
}
#[tokio::test]
async fn endpoint_ws_ack_dedup_topic_filter_reply_and_cron() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = DingTalkChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    let mut ignored = event("ignored", "wrong topic");
    ignored["headers"]["topic"] = "/other".into();
    server.incoming.send(ignored).unwrap();
    server.incoming.send(event("id1", "hello")).unwrap();
    server.incoming.send(event("id1", "duplicate")).unwrap();
    server
        .wait(|requests| {
            requests.iter().any(|(kind, body)| {
                kind.starts_with("/robot") && body["markdown"]["text"] == "reply hello"
            })
        })
        .await;
    assert_eq!(
        gateway.turns.lock().unwrap().as_slice(),
        &[
            json!({"sessionKey":"dingtalk:chat=chat:general","channelKey":"dingtalk","message":"hello"})
        ]
    );
    server
        .wait(|requests| requests.iter().filter(|(kind, _)| kind == "ack").count() == 3)
        .await;
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(kind, _)| kind == "ack")
            .all(|(_, ack)| ack["code"] == 200 && ack["data"] == "{\"status\":\"SUCCESS\"}")
    );
    assert!(
        channel
            .deliver(&json!({"sessionKey":"dingtalk:chat=chat:general","text":"cron"}))
            .await
            .unwrap()
    );
    assert!(
        !channel
            .deliver(
                &json!({"sessionKey":"dingtalk:chat=unknown:general","text":"missing webhook"})
            )
            .await
            .unwrap()
    );
    handle.close().await;
}
#[tokio::test]
async fn permission_and_question_answers_resume_live_stream() {
    for (input, answer, method) in [
        ("permission", "2", "permission_decide"),
        ("question", "2", "elicitation_respond"),
    ] {
        let server = server().await;
        let gateway = Arc::new(Gateway::default());
        let channel = DingTalkChannel::new(options(&server), gateway.clone());
        let handle = channel.start().await.unwrap();
        server.incoming.send(event("first", input)).unwrap();
        gateway.ready.notified().await;
        server.incoming.send(event("answer", answer)).unwrap();
        server
            .wait(|requests| {
                requests.iter().any(|(kind, body)| {
                    kind.starts_with("/robot")
                        && body["markdown"]["text"] == format!("reply {input}")
                })
            })
            .await;
        let requests = gateway.requests.lock().unwrap();
        assert_eq!(requests[0].0, method);
        assert_eq!(requests[0].1["sessionKey"], "dingtalk:chat=chat:general");
        drop(requests);
        handle.close().await;
    }
}
#[tokio::test]
async fn system_ping_disconnect_and_reconnect_obtain_new_ticket() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = DingTalkChannel::new(options(&server), gateway);
    let handle = channel.start().await.unwrap();
    let ping =
        json!({"type":"SYSTEM","headers":{"topic":"ping","messageId":"ping"},"data":"ping-data"});
    server.incoming.send(ping.clone()).unwrap();
    server
        .wait(|requests| {
            requests.iter().any(|(kind, body)| {
                kind == "ack"
                    && body["headers"]["messageId"] == "ping"
                    && body["data"] == "ping-data"
            })
        })
        .await;
    server.incoming.send(json!({"type":"SYSTEM","headers":{"topic":"disconnect","messageId":"disconnect"},"data":"bye"})).unwrap();
    server
        .wait(|requests| requests.iter().filter(|(kind, _)| kind == "/open").count() == 2)
        .await;
    handle.close().await;
}
#[tokio::test]
async fn session_reset_busy_drop_and_shutdown_abort_owned_turn() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = DingTalkChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    channel.remember_webhook("chat", "https://api.dingtalk.com/robot/send?session=valid");
    channel.incoming("chat", "/new").await.unwrap();
    server.incoming.send(event("hold", "hold")).unwrap();
    gateway.ready.notified().await;
    channel.incoming("chat", "/new ignored").await.unwrap();
    assert_eq!(gateway.turns.lock().unwrap().len(), 1);
    assert!(
        gateway.turns.lock().unwrap()[0]["sessionKey"]
            .as_str()
            .unwrap()
            .starts_with("dingtalk:chat=chat:s_")
    );
    tokio::time::timeout(Duration::from_secs(3), handle.close())
        .await
        .unwrap();
    assert!(gateway.cancelled.load(Ordering::SeqCst));
    assert!(channel.state.lock().unwrap().webhooks.is_empty());
}
#[test]
fn webhook_origin_text_extraction_and_ordered_cache_limits_match_ts() {
    assert!(valid_webhook("https://api.dingtalk.com/robot/send"));
    for bad in [
        "http://api.dingtalk.com/x",
        "https://api.dingtalk.com.evil/x",
        "https://evil/api.dingtalk.com/",
    ] {
        assert!(!valid_webhook(bad));
    }
    assert_eq!(
        extract_text(&json!({"richText":[{"text":"first"},{"type":"image"},{"text":"second"}]})),
        "first second"
    );
    assert_eq!(
        extract_text(&json!({"text":{"content":" hi "},"richText":[{"text":"ignored"}]})),
        "hi"
    );
    let channel = DingTalkChannel::new(DingTalkOptions::default(), Arc::new(Gateway::default()));
    for i in 0..500 {
        channel.remember_webhook(&format!("c{i}"), "https://api.dingtalk.com/x");
    }
    channel.remember_webhook("c499", "https://api.dingtalk.com/y");
    let state = channel.state.lock().unwrap();
    assert_eq!(state.webhooks.len(), 499);
    assert!(!state.webhooks.contains_key("c0"));
    assert_eq!(state.webhooks["c499"], "https://api.dingtalk.com/y");
}
#[tokio::test]
async fn missing_credentials_fail_before_network() {
    assert!(
        DingTalkChannel::new(DingTalkOptions::default(), Arc::new(Gateway::default()))
            .start()
            .await
            .is_err()
    );
}

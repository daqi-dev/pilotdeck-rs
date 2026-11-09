use super::*;
use pilotdeck_protocol::frame::{self, Decoder};
use std::sync::atomic::AtomicUsize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Notify, mpsc},
};

struct Server {
    base: String,
    incoming: mpsc::UnboundedSender<Value>,
    requests: Arc<StdMutex<Vec<(String, String)>>>,
    changed: Arc<Notify>,
    fail_send: Arc<AtomicUsize>,
    channel_type: Arc<AtomicUsize>,
    acknowledge: Arc<AtomicBool>,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn server() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api", listener.local_addr().unwrap());
    let endpoint = format!("ws://{}/gateway", listener.local_addr().unwrap());
    let (incoming, receiver) = mpsc::unbounded_channel::<Value>();
    let receiver = Arc::new(Mutex::new(receiver));
    let requests = Arc::new(StdMutex::new(Vec::new()));
    let changed = Arc::new(Notify::new());
    let fail_send = Arc::new(AtomicUsize::new(0));
    let channel_type = Arc::new(AtomicUsize::new(0));
    let acknowledge = Arc::new(AtomicBool::new(true));
    let task = {
        let requests = requests.clone();
        let changed = changed.clone();
        let fail_send = fail_send.clone();
        let channel_type = channel_type.clone();
        let acknowledge = acknowledge.clone();
        tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let receiver = receiver.clone();
                let requests = requests.clone();
                let changed = changed.clone();
                let fail_send = fail_send.clone();
                let channel_type = channel_type.clone();
                let acknowledge = acknowledge.clone();
                let endpoint = endpoint.clone();
                connections.spawn(async move{
                let mut bytes=Vec::new();let mut buffer=[0;4096];let(header,headers,path)=loop{let count=stream.read(&mut buffer).await.unwrap();if count==0{return;}bytes.extend_from_slice(&buffer[..count]);if let Some(end)=bytes.windows(4).position(|part|part==b"\r\n\r\n"){let headers=String::from_utf8_lossy(&bytes[..end]).into_owned();let path=headers.split_whitespace().nth(1).unwrap().to_owned();break(end+4,headers,path);}};
                if headers.to_ascii_lowercase().contains("upgrade: websocket"){
                    assert_eq!(path,"/gateway?v=10&encoding=json");let key=headers.lines().find_map(|line|line.split_once(':').filter(|(key,_)|key.eq_ignore_ascii_case("sec-websocket-key")).map(|(_,value)|value.trim())).unwrap();let accept=tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
                    stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await.unwrap();stream.write_all(&frame::server_frame(1,json!({"op":10,"d":{"heartbeat_interval":100}}).to_string().as_bytes())).await.unwrap();
                    let mut decoder=Decoder::default();decoder.buffer.extend_from_slice(&bytes[header..]);loop{
                        while let Some(frame)=decoder.next().unwrap(){
                            if frame.opcode==8{requests.lock().unwrap().push(("close".into(),String::new()));changed.notify_one();return;}
                            if frame.opcode!=1{continue;}
                            let value:Value=serde_json::from_slice(&frame.payload).unwrap();let op=value["op"].as_i64().unwrap();requests.lock().unwrap().push((format!("op:{op}"),value.to_string()));changed.notify_one();
                            let response=match op{2=>{assert_eq!(value["d"]["token"],"test-token");assert_eq!(value["d"]["intents"],37377);Some(json!({"op":0,"s":1,"t":"READY","d":{"user":{"id":"bot"},"session_id":"session-one","resume_gateway_url":endpoint}}))},6=>Some(json!({"op":0,"s":2,"t":"RESUMED","d":{}})),1 if acknowledge.load(Ordering::SeqCst)=>Some(json!({"op":11,"d":null})),_=>None};
                            if let Some(response)=response{if stream.write_all(&frame::server_frame(1,response.to_string().as_bytes())).await.is_err(){return;}}
                        }
                        tokio::select!{count=stream.read_buf(&mut decoder.buffer)=>if count.unwrap_or(0)==0{return;},value=async{receiver.lock().await.recv().await}=>{let Some(value)=value else{return;};if stream.write_all(&frame::server_frame(1,value.to_string().as_bytes())).await.is_err(){return;}}}
                    }
                }
                assert!(headers.to_ascii_lowercase().contains("authorization: bot test-token"));let method=headers.split_whitespace().next().unwrap();let length=headers.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|s|s.trim().parse::<usize>().ok())).unwrap_or(0);while bytes.len()<header+length{let count=stream.read(&mut buffer).await.unwrap();if count==0{return;}bytes.extend_from_slice(&buffer[..count]);}
                let body=String::from_utf8(bytes[header..header+length].to_vec()).unwrap();requests.lock().unwrap().push((format!("{method} {path}"),body));changed.notify_one();
                let(status,body)=if path=="/api/gateway/bot"{(200,json!({"url":endpoint}).to_string())}else if path.ends_with("/typing"){(204,String::new())}else if path.ends_with("/messages"){let failed=fail_send.try_update(Ordering::SeqCst,Ordering::SeqCst,|n|n.checked_sub(1)).is_ok();(if failed{503}else{200},json!({"id":"sent"}).to_string())}else{(200,json!({"id":"room","type":channel_type.load(Ordering::SeqCst)}).to_string())};
                let _=stream.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
            });
                while connections.try_join_next().is_some() {}
            }
        })
    };
    Server {
        base,
        incoming,
        requests,
        changed,
        fail_send,
        channel_type,
        acknowledge,
        task,
    }
}
impl Server {
    fn options(&self) -> DiscordOptions {
        DiscordOptions {
            token: "test-token".into(),
            base_url: self.base.clone(),
            gateway_url: None,
        }
    }
    async fn wait(&self, predicate: impl Fn(&[(String, String)]) -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
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
    fn event(&self, sequence: i64, text: &str) {
        self.incoming
            .send(json!({"op":0,"s":sequence,"t":"MESSAGE_CREATE","d":message(text)}))
            .unwrap();
    }
    fn replies(&self) -> Vec<pilotdeck_protocol::JsValue> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _)| path.ends_with("/messages"))
            .map(|(_, body)| pilotdeck_protocol::parse_json(body).unwrap())
            .collect()
    }
}
#[derive(Default)]
struct Gateway {
    turns: StdMutex<Vec<Value>>,
    requests: StdMutex<Vec<(String, Value)>>,
    ready: Notify,
    release: Notify,
    cancelled: AtomicUsize,
}
struct Turn<'a>(&'a AtomicUsize);
impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
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
        Box::pin(async { panic!("live stream required") })
    }
    fn stream_turn<'a>(
        &'a self,
        params: Value,
        sink: &'a dyn ChannelEventSink,
    ) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            let _turn = Turn(&self.cancelled);
            let text = params["message"].as_str().unwrap().to_owned();
            self.turns.lock().unwrap().push(params);
            match text.as_str() {
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
                "fail" => return Err(failure("injected")),
                _ => {}
            }
            sink.event(json!({"type":"assistant_text_delta","text":format!("reply {text}")}))
                .await?;
            Ok(())
        })
    }
}
fn message(text: &str) -> Value {
    json!({"id":"message","type":0,"author":{"id":"alice","bot":false},"channel_id":"room","content":text})
}

#[tokio::test]
async fn real_gateway_identify_ready_heartbeat_filters_and_rest_typing_send() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = DiscordChannel::new(server.options(), gateway.clone());
    let handle = channel.start().await.unwrap();
    for mut raw in [
        message("bot"),
        message("system"),
        message("self"),
        message("no channel"),
    ] {
        match raw["content"].as_str().unwrap() {
            "bot" => raw["author"]["bot"] = true.into(),
            "system" => raw["type"] = 6.into(),
            "self" => raw["author"]["id"] = "bot".into(),
            _ => raw["channel_id"] = "".into(),
        }
        channel.message(raw).await.unwrap();
    }
    server.event(2, " 你好 ");
    server
        .wait(|r| {
            r.iter()
                .any(|(path, body)| path.ends_with("/messages") && body.contains("reply 你好"))
                && r.iter().any(|(path, _)| path.ends_with("/typing"))
                && r.iter().any(|(kind, _)| kind == "op:1")
        })
        .await;
    assert_eq!(
        *gateway.turns.lock().unwrap(),
        vec![
            json!({"sessionKey":"discord:chat=room:general","channelKey":"discord","message":"你好"})
        ]
    );
    assert_eq!(
        server.replies()[0]
            .get("content")
            .unwrap()
            .as_string()
            .unwrap(),
        &JsString::from("reply 你好")
    );
    handle.close().await;
}

#[tokio::test]
async fn gateway_reconnect_resumes_exact_session_and_latest_sequence() {
    let server = server().await;
    let channel = DiscordChannel::new(server.options(), Arc::new(Gateway::default()));
    let handle = channel.start().await.unwrap();
    server
        .incoming
        .send(json!({"op":0,"s":77,"t":"SOME_EVENT","d":{}}))
        .unwrap();
    server.incoming.send(json!({"op":7,"d":null})).unwrap();
    server
        .wait(|r| r.iter().any(|(kind, _)| kind == "op:6"))
        .await;
    let resume = server
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|(kind, _)| kind == "op:6")
        .map(|(_, body)| serde_json::from_str::<Value>(body).unwrap())
        .unwrap();
    assert_eq!(
        resume["d"],
        json!({"token":"test-token","session_id":"session-one","seq":77})
    );
    handle.close().await;
}

#[tokio::test]
async fn gateway_answers_live_permissions_questions_and_cancellation() {
    for (input, answer, method) in [
        ("permission", "2", "permission_decide"),
        ("question", "2", "elicitation_respond"),
        ("question", "0", "elicitation_respond"),
    ] {
        let server = server().await;
        let gateway = Arc::new(Gateway::default());
        let channel = DiscordChannel::new(server.options(), gateway.clone());
        let handle = channel.start().await.unwrap();
        server.event(2, input);
        tokio::time::timeout(Duration::from_secs(3), gateway.ready.notified())
            .await
            .unwrap();
        server.event(3, answer);
        server
            .wait(|r| {
                r.iter().any(|(path, body)| {
                    path.ends_with("/messages") && body.contains(&format!("reply {input}"))
                })
            })
            .await;
        let requests = gateway.requests.lock().unwrap();
        assert_eq!(requests[0].0, method);
        assert_eq!(requests[0].1["sessionKey"], "discord:chat=room:general");
        if input == "permission" {
            assert_eq!(requests[0].1["remember"], true);
        } else if answer == "0" {
            assert_eq!(requests[0].1["answer"]["type"], "cancelled");
        } else {
            assert_eq!(requests[0].1["answer"]["answers"], json!({"Pick":"B"}));
        }
        drop(requests);
        handle.close().await;
    }
}

#[tokio::test]
async fn new_session_busy_drop_and_stop_cancel_live_gateway_turn() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = DiscordChannel::new(server.options(), gateway.clone());
    let handle = channel.start().await.unwrap();
    channel.message(message("/new")).await.unwrap();
    channel.message(message("/projects")).await.unwrap();
    let key = gateway.turns.lock().unwrap()[0]["sessionKey"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(key.starts_with("discord:chat=room:s_"));
    server.event(2, "hold");
    gateway.ready.notified().await;
    channel.message(message("/new skipped")).await.unwrap();
    assert_eq!(gateway.turns.lock().unwrap().len(), 2);
    assert_eq!(
        channel.mapper.lock().unwrap().session("discord", "room"),
        key
    );
    tokio::time::timeout(Duration::from_secs(2), handle.close())
        .await
        .unwrap();
    assert_eq!(gateway.cancelled.load(Ordering::SeqCst), 2);
    assert!(
        !channel
            .deliver(&json!({"sessionKey":key,"text":"stopped"}))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn cron_fetches_sendable_channel_splits_utf16_and_continues_after_failure() {
    let server = server().await;
    let channel = DiscordChannel::new(server.options(), Arc::new(Gateway::default()));
    let handle = channel.start().await.unwrap();
    server.fail_send.store(1, Ordering::SeqCst);
    let text = format!("{}😀", "a".repeat(1999));
    assert!(
        !channel
            .deliver(&json!({"originSessionKey":"discord:chat=room:general","text":text}))
            .await
            .unwrap()
    );
    let replies = server.replies();
    assert_eq!(replies.len(), 2);
    let first = replies[0].get("content").unwrap().as_string().unwrap();
    let second = replies[1].get("content").unwrap().as_string().unwrap();
    assert_eq!(first.len_utf16(), 2000);
    assert_eq!(first.units().last(), Some(&0xd83d));
    assert_eq!(second.units(), &[0xde00]);
    server.channel_type.store(4, Ordering::SeqCst);
    assert!(
        !channel
            .deliver(&json!({"sessionKey":"discord:chat=room:general","text":"not sendable"}))
            .await
            .unwrap()
    );
    assert_eq!(server.replies().len(), 2);
    assert!(!channel.deliver(&json!({"originChannelKey":"matrix","sessionKey":"discord:chat=room:general","text":"wrong"})).await.unwrap());
    handle.close().await;
}

#[tokio::test]
async fn missed_heartbeat_ack_reconnects_and_failed_permission_prompt_retries() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = DiscordChannel::new(server.options(), gateway.clone());
    let handle = channel.start().await.unwrap();
    server.acknowledge.store(false, Ordering::SeqCst);
    server
        .wait(|r| r.iter().any(|(kind, _)| kind == "op:6"))
        .await;
    server.acknowledge.store(true, Ordering::SeqCst);
    server.fail_send.store(1, Ordering::SeqCst);
    server.event(4, "permission");
    gateway.ready.notified().await;
    channel.message(message("1")).await.unwrap();
    assert!(gateway.requests.lock().unwrap().is_empty());
    channel.message(message("1")).await.unwrap();
    assert_eq!(gateway.requests.lock().unwrap().len(), 1);
    handle.close().await;
}

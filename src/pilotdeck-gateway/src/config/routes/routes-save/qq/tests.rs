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
    let endpoint = format!("ws://{}/qq", listener.local_addr().unwrap());
    let (incoming, receiver) = mpsc::unbounded_channel::<Value>();
    let receiver = Arc::new(Mutex::new(receiver));
    let requests = Arc::new(StdMutex::new(Vec::new()));
    let changed = Arc::new(Notify::new());
    let fail_send = Arc::new(AtomicUsize::new(0));
    let task = {
        let requests = requests.clone();
        let changed = changed.clone();
        let fail_send = fail_send.clone();
        tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let receiver = receiver.clone();
                let requests = requests.clone();
                let changed = changed.clone();
                let fail_send = fail_send.clone();
                let endpoint = endpoint.clone();
                connections.spawn(async move{
                let mut bytes=Vec::new();let mut buffer=[0;4096];let(header,headers,path)=loop{let count=stream.read(&mut buffer).await.unwrap();if count==0{return;}bytes.extend_from_slice(&buffer[..count]);if let Some(end)=bytes.windows(4).position(|part|part==b"\r\n\r\n"){let headers=String::from_utf8_lossy(&bytes[..end]).into_owned();let path=headers.split_whitespace().nth(1).unwrap().to_owned();break(end+4,headers,path);}};
                if headers.to_ascii_lowercase().contains("upgrade: websocket"){
                    assert_eq!(path,"/qq");let key=headers.lines().find_map(|line|line.split_once(':').filter(|(key,_)|key.eq_ignore_ascii_case("sec-websocket-key")).map(|(_,value)|value.trim())).unwrap();let accept=tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await.unwrap();stream.write_all(&frame::server_frame(1,json!({"op":10,"d":{"heartbeat_interval":100}}).to_string().as_bytes())).await.unwrap();
                    let mut decoder=Decoder::default();decoder.buffer.extend_from_slice(&bytes[header..]);loop{
                        while let Some(frame)=decoder.next().unwrap(){if frame.opcode==8{return;}if frame.opcode!=1{continue;}let value:Value=serde_json::from_slice(&frame.payload).unwrap();let op=value["op"].as_i64().unwrap();requests.lock().unwrap().push((format!("op:{op}"),value.to_string()));changed.notify_one();let response=match op{2=>{assert_eq!(value["d"]["token"],"QQBot test-token");assert_eq!(value["d"]["intents"],33554432);assert_eq!(value["d"]["shard"],json!([0,1]));Some(json!({"op":0,"s":1,"t":"READY","d":{"session_id":"qq-session"}}))},6=>Some(json!({"op":0,"s":2,"t":"RESUMED","d":{}})),1=>Some(json!({"op":11})),_=>None};if let Some(response)=response{if stream.write_all(&frame::server_frame(1,response.to_string().as_bytes())).await.is_err(){return;}}}
                        tokio::select!{count=stream.read_buf(&mut decoder.buffer)=>if count.unwrap_or(0)==0{return;},value=async{receiver.lock().await.recv().await}=>{let Some(value)=value else{return;};if stream.write_all(&frame::server_frame(1,value.to_string().as_bytes())).await.is_err(){return;}}}
                    }
                }
                let method=headers.split_whitespace().next().unwrap();let length=headers.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|s|s.trim().parse::<usize>().ok())).unwrap_or(0);while bytes.len()<header+length{let count=stream.read(&mut buffer).await.unwrap();if count==0{return;}bytes.extend_from_slice(&buffer[..count]);}let body=String::from_utf8(bytes[header..header+length].to_vec()).unwrap();requests.lock().unwrap().push((format!("{method} {path}"),body.clone()));changed.notify_one();
                let(status,value)=if path=="/token"{assert_eq!(serde_json::from_str::<Value>(&body).unwrap(),json!({"appId":"app","clientSecret":"secret"}));(200,json!({"access_token":"test-token","expires_in":"7200"}))}else{assert!(headers.to_ascii_lowercase().contains("authorization: qqbot test-token"));if path=="/api/gateway"{(200,json!({"url":endpoint}))}else{let failed=fail_send.try_update(Ordering::SeqCst,Ordering::SeqCst,|n|n.checked_sub(1)).is_ok();(if failed{503}else{200},json!({"id":"sent"}))}};let body=value.to_string();let _=stream.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
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
        task,
    }
}
impl Server {
    fn options(&self) -> QQOptions {
        QQOptions {
            app_id: "app".into(),
            client_secret: "secret".into(),
            api_base: self.base.clone(),
            token_url: self.base.replace("/api", "/token"),
            ..Default::default()
        }
    }
    async fn wait(&self, predicate: impl Fn(&[(String, String)]) -> bool) {
        tokio::time::timeout(Duration::from_secs(8), async {
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
    fn event(&self, direct: bool, seq: i64, text: &str) {
        self.incoming.send(json!({"op":0,"s":seq,"t":if direct{"C2C_MESSAGE_CREATE"}else{"GROUP_AT_MESSAGE_CREATE"},"d":event(direct,text)})).unwrap();
    }
    fn sends(&self) -> Vec<(String, pilotdeck_protocol::JsValue)> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _)| path.ends_with("/messages"))
            .map(|(path, body)| (path.clone(), pilotdeck_protocol::parse_json(body).unwrap()))
            .collect()
    }
}
fn event(direct: bool, text: &str) -> Value {
    if direct {
        json!({"id":"M","author":{"user_openid":"U"},"content":text})
    } else {
        json!({"id":"M","author":{"member_openid":"U"},"group_openid":"G","content":text})
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

#[tokio::test]
async fn real_token_gateway_identify_heartbeat_group_and_c2c_messages() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = QQChannel::new(server.options(), gateway.clone());
    let handle = channel.start().await.unwrap();
    server
        .wait(|r| r.iter().any(|(kind, _)| kind == "op:2"))
        .await;
    server.event(false, 2, "/ask hello");
    server
        .wait(|r| r.iter().any(|(_, body)| body.contains("reply hello")))
        .await;
    server.event(true, 3, "/new direct prompt");
    server
        .wait(|r| {
            r.iter()
                .any(|(_, body)| body.contains("reply /new direct prompt"))
                && r.iter().any(|(kind, _)| kind == "op:1")
        })
        .await;
    assert_eq!(
        *gateway.turns.lock().unwrap(),
        vec![
            json!({"sessionKey":"qq:group=G:user=U:general","channelKey":"qq","message":"hello"}),
            json!({"sessionKey":"qq:c2c:U:general","channelKey":"qq","message":"/new direct prompt"})
        ]
    );
    let sends = server.sends();
    assert_eq!(sends[0].0, "POST /api/v2/groups/G/messages");
    assert_eq!(sends[1].0, "POST /api/v2/users/U/messages");
    for (_, body) in sends {
        assert_eq!(
            body.get("msg_id").unwrap().as_string().unwrap(),
            &JsString::from("M")
        );
        assert_eq!(body.get("msg_seq").unwrap().as_number(), Some(1.0));
    }
    assert_eq!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _)| path == "POST /token")
            .count(),
        1
    );
    handle.close().await;
}

#[tokio::test]
async fn group_allow_prefix_new_state_and_busy_guard_are_per_group_user() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let changed = Arc::new(StdMutex::new(Vec::new()));
    let stored = changed.clone();
    let mut options = server.options();
    options.allow_groups = vec!["G".into()];
    options.initial_state = json!({"activeByChatKey":{"G:U":"qq:group=G:user=U:s_saved"}});
    options.on_state_change = Some(Arc::new(move |state| stored.lock().unwrap().push(state)));
    let channel = QQChannel::new(options, gateway.clone());
    let handle = channel.start().await.unwrap();
    let mut blocked = event(false, "hello");
    blocked["group_openid"] = "blocked".into();
    channel.message(false, blocked).await.unwrap();
    channel.message(false, event(false, "/ask")).await.unwrap();
    assert!(gateway.turns.lock().unwrap().is_empty());
    channel
        .message(false, event(false, "plain @bot message"))
        .await
        .unwrap();
    assert_eq!(
        gateway.turns.lock().unwrap()[0]["sessionKey"],
        "qq:group=G:user=U:s_saved"
    );
    channel.message(false, event(false, "/new")).await.unwrap();
    assert_eq!(changed.lock().unwrap().len(), 1);
    let key = channel.snapshot()["activeByChatKey"]["G:U"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(key.starts_with("qq:group=G:user=U:s_"));
    server.event(false, 2, "hold");
    gateway.ready.notified().await;
    channel
        .message(false, event(false, "/new skip"))
        .await
        .unwrap();
    assert_eq!(changed.lock().unwrap().len(), 1);
    assert_eq!(gateway.turns.lock().unwrap().len(), 2);
    tokio::time::timeout(Duration::from_secs(2), handle.close())
        .await
        .unwrap();
    assert_eq!(gateway.cancelled.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn group_and_c2c_permission_elicitation_answers_use_original_sessions() {
    for (direct, input, answer, method) in [
        (false, "permission", "2", "permission_decide"),
        (true, "permission", "0", "permission_decide"),
        (false, "question", "2", "elicitation_respond"),
        (true, "question", "0", "elicitation_respond"),
    ] {
        let server = server().await;
        let gateway = Arc::new(Gateway::default());
        let channel = QQChannel::new(server.options(), gateway.clone());
        let handle = channel.start().await.unwrap();
        server.event(direct, 2, input);
        tokio::time::timeout(Duration::from_secs(3), gateway.ready.notified())
            .await
            .unwrap();
        server.event(direct, 3, answer);
        server
            .wait(|r| {
                r.iter()
                    .any(|(_, body)| body.contains(&format!("reply {input}")))
            })
            .await;
        let requests = gateway.requests.lock().unwrap();
        assert_eq!(requests[0].0, method);
        assert_eq!(
            requests[0].1["sessionKey"],
            if direct {
                "qq:c2c:U:general"
            } else {
                "qq:group=G:user=U:general"
            }
        );
        drop(requests);
        let sends = server.sends();
        if input == "permission" || answer == "0" {
            assert!(
                sends.iter().any(|(_, body)| body.get("msg_id").is_none()),
                "confirmation sends have no passive msg_id"
            );
        }
        handle.close().await;
    }
}

#[tokio::test]
async fn reply_chunks_use_message_sequence_stop_on_failure_and_preserve_utf16() {
    let server = server().await;
    let channel = QQChannel::new(server.options(), Arc::new(Gateway::default()));
    let handle = channel.start().await.unwrap();
    channel.targets.lock().unwrap().insert(
        "G:U".into(),
        Target {
            direct: false,
            id: "G".into(),
            msg_id: Some("passive".into()),
        },
    );
    let text = format!("{}😀", "a".repeat(1999));
    let started = tokio::time::Instant::now();
    assert!(channel.send_initial_reply("G:U", &text).await);
    assert!(started.elapsed() >= Duration::from_millis(1000));
    let sends = server.sends();
    assert_eq!(sends.len(), 2);
    assert_eq!(
        sends[0]
            .1
            .get("content")
            .unwrap()
            .as_string()
            .unwrap()
            .units()
            .last(),
        Some(&0xd83d)
    );
    assert_eq!(
        sends[1]
            .1
            .get("content")
            .unwrap()
            .as_string()
            .unwrap()
            .units(),
        &[0xde00]
    );
    assert_eq!(sends[1].1.get("msg_seq").unwrap().as_number(), Some(2.0));
    server.fail_send.store(1, Ordering::SeqCst);
    assert!(!channel.send_initial_reply("G:U", &text).await);
    assert_eq!(server.sends().len(), 3);
    handle.close().await;
}

#[tokio::test]
async fn reconnect_resumes_session_and_invalid_session_reidentifies_after_two_seconds() {
    let server = server().await;
    let channel = QQChannel::new(server.options(), Arc::new(Gateway::default()));
    let handle = channel.start().await.unwrap();
    server
        .wait(|r| r.iter().any(|(kind, _)| kind == "op:1"))
        .await;
    server
        .incoming
        .send(json!({"op":0,"s":88,"t":"OTHER","d":{}}))
        .unwrap();
    server.incoming.send(json!({"op":7})).unwrap();
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
        json!({"token":"QQBot test-token","session_id":"qq-session","seq":88})
    );
    server.incoming.send(json!({"op":9})).unwrap();
    server
        .wait(|r| r.iter().filter(|(kind, _)| kind == "op:2").count() == 2)
        .await;
    handle.close().await;
}

#[tokio::test]
async fn permission_prompt_failure_retries_and_token_refresh_uses_expiry_margin() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = QQChannel::new(server.options(), gateway.clone());
    let handle = channel.start().await.unwrap();
    server.fail_send.store(1, Ordering::SeqCst);
    server.event(false, 2, "permission");
    gateway.ready.notified().await;
    channel.message(false, event(false, "1")).await.unwrap();
    assert!(gateway.requests.lock().unwrap().is_empty());
    channel.message(false, event(false, "1")).await.unwrap();
    assert_eq!(gateway.requests.lock().unwrap().len(), 1);
    channel.token.lock().await.as_mut().unwrap().expires_at = 0.0;
    channel.access_token().await.unwrap();
    assert_eq!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _)| path == "POST /token")
            .count(),
        2
    );
    handle.close().await;
}

#[tokio::test]
async fn production_cron_qq_has_no_ts_deliver_cron_result() {
    let server = server().await;
    let channel = QQChannel::new(server.options(), Arc::new(Gateway::default()));
    let handle = channel.start().await.unwrap();
    assert!(
        !channel
            .deliver(&json!({
                "originChannelKey":"qq",
                "originSessionKey":"qq:group=G:user=U:general",
                "text":"cron to qq"
            }))
            .await
            .unwrap()
    );
    handle.close().await;
}

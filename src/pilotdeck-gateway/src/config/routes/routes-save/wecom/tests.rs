use super::*;
use pilotdeck_protocol::frame::{self, Decoder};
use std::sync::atomic::AtomicUsize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
};
struct Server {
    url: String,
    http: String,
    incoming: mpsc::UnboundedSender<Value>,
    requests: Arc<StdMutex<Vec<String>>>,
    changed: Arc<Notify>,
    fail: Arc<StdMutex<Option<String>>>,
    connections: Arc<AtomicUsize>,
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
    let (incoming, receiver) = mpsc::unbounded_channel::<Value>();
    let receiver = Arc::new(Mutex::new(receiver));
    let requests = Arc::new(StdMutex::new(vec![]));
    let changed = Arc::new(Notify::new());
    let fail = Arc::new(StdMutex::new(None::<String>));
    let connections = Arc::new(AtomicUsize::new(0));
    let task = {
        let requests = requests.clone();
        let changed = changed.clone();
        let fail = fail.clone();
        let connections = connections.clone();
        tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let receiver = receiver.clone();
                let requests = requests.clone();
                let changed = changed.clone();
                let fail = fail.clone();
                let connections = connections.clone();
                tasks.spawn(async move{let mut bytes=vec![];let mut buffer=[0;4096];let(header,headers)=loop{let n=stream.read(&mut buffer).await.unwrap_or(0);if n==0{return;}bytes.extend_from_slice(&buffer[..n]);if let Some(end)=bytes.windows(4).position(|b|b==b"\r\n\r\n"){break(end+4,String::from_utf8_lossy(&bytes[..end]).into_owned());}};if !headers.to_ascii_lowercase().contains("upgrade: websocket"){assert!(headers.to_ascii_lowercase().contains("user-agent: pilotdeck/1.0"));assert!(headers.to_ascii_lowercase().contains("accept: */*"));let _=stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Disposition: filename=download.png\r\nContent-Length: 5\r\nConnection: close\r\n\r\nimage").await;return;}connections.fetch_add(1,Ordering::SeqCst);let key=headers.lines().find_map(|line|line.split_once(':').filter(|(k,_)|k.eq_ignore_ascii_case("sec-websocket-key")).map(|(_,v)|v.trim())).unwrap();let accept=tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await.unwrap();let mut decoder=Decoder::default();decoder.buffer.extend_from_slice(&bytes[header..]);loop{while let Some(frame)=decoder.next().unwrap(){if frame.opcode==8{return;}if frame.opcode!=1{continue;}let raw=String::from_utf8(frame.payload).unwrap();let parsed=pilotdeck_protocol::parse_json(&raw).unwrap();let cmd=parsed.get("cmd").unwrap().as_string().unwrap().to_utf8().unwrap();requests.lock().unwrap().push(raw.clone());changed.notify_one();let id=parsed.get("headers").unwrap().get("req_id").unwrap().as_string().unwrap().to_utf8().unwrap();if cmd=="ping"{continue;}let failed={let mut f=fail.lock().unwrap();if f.as_deref()==Some(cmd.as_str()){f.take();true}else{false}};let mut body=json!({"errcode":if failed{1}else{0},"errmsg":"injected"});if cmd=="aibot_upload_media_init"{body["upload_id"]="UP".into();}if cmd=="aibot_upload_media_finish"{body["media_id"]="MEDIA".into();}let response=json!({"cmd":cmd,"headers":{"req_id":id},"body":body});if stream.write_all(&frame::server_frame(1,response.to_string().as_bytes())).await.is_err(){return;}}
 tokio::select!{n=stream.read_buf(&mut decoder.buffer)=>if n.unwrap_or(0)==0{return;},value=async{receiver.lock().await.recv().await}=>{let Some(value)=value else{return;};if value["cmd"]=="__close"{return;}if stream.write_all(&frame::server_frame(1,value.to_string().as_bytes())).await.is_err(){return;}}}
 } });
                while tasks.try_join_next().is_some() {}
            }
        })
    };
    Server {
        url: format!("ws://{address}/wecom"),
        http: format!("http://{address}/media"),
        incoming,
        requests,
        changed,
        fail,
        connections,
        task,
    }
}
impl Server {
    fn options(&self) -> WeComOptions {
        WeComOptions {
            bot_key: "BOT".into(),
            bot_secret: "SECRET".into(),
            websocket_url: self.url.clone(),
            dm_policy: "open".into(),
            group_policy: "open".into(),
            text_batch_delay_ms: 0,
            ..Default::default()
        }
    }
    async fn wait(&self, predicate: impl Fn(&[String]) -> bool) {
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
    fn callback(&self, id: &str, chat: &str, sender: &str, group: bool, text: &str) {
        self.incoming
            .send(callback(id, chat, sender, group, text))
            .unwrap();
    }
    fn values(&self) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect()
    }
}
fn callback(id: &str, chat: &str, sender: &str, group: bool, text: &str) -> Value {
    json!({"cmd":"aibot_msg_callback","headers":{"req_id":format!("req-{id}")},"body":{"msgid":id,"chatid":chat,"from":{"userid":sender},"chattype":if group{"group"}else{"single"},"msgtype":"text","text":{"content":text}}})
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
    fn turn(&self, _: Value) -> ChannelCall<'_, Vec<Value>> {
        Box::pin(async { panic!("stream required") })
    }
    fn request<'a>(&'a self, method: &'a str, params: Value) -> ChannelCall<'a, Value> {
        Box::pin(async move {
            self.requests.lock().unwrap().push((method.into(), params));
            if method == "list_projects" {
                return Ok(
                    json!({"projects":[{"name":"Work","projectKey":"p","fullPath":"/work"}]}),
                );
            }
            self.release.notify_one();
            Ok(json!({"delivered":true}))
        })
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
                    sink.event(json!({"type":"elicitation_request","requestId":"Q","questions":[{"question":"Pick","options":[{"label":"A"},{"label":"B"}]}]})).await?;
                    self.ready.notify_one();
                    self.release.notified().await;
                }
                "image" => {
                    sink.event(json!({"type":"tool_call_finished","ok":true,"toolCallId":"T","images":[{"data":"aW1hZ2U=","mimeType":"image/png"}]})).await?;
                }
                "fail" => return Err(failure("injected")),
                _ => {}
            }
            sink.event(json!({"type":"assistant_text_delta","text":format!("reply {text}")}))
                .await
        })
    }
}
#[tokio::test]
async fn subscribe_callback_reply_and_utf16_truncation_use_real_socket() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = WeComChannel::new(server.options(), gateway.clone());
    let handle = channel.start().await.unwrap();
    server.callback("1", "U", "U", false, "hello");
    server
        .wait(|r| r.iter().any(|v| v.contains("reply hello")))
        .await;
    let values = server.values();
    assert_eq!(values[0]["cmd"], "aibot_subscribe");
    assert_eq!(values[0]["body"]["bot_id"], "BOT");
    assert_eq!(values[0]["body"]["secret"], "SECRET");
    assert_eq!(values[0]["body"]["device_id"].as_str().unwrap().len(), 32);
    let reply = values
        .iter()
        .find(|v| v["cmd"] == "aibot_respond_msg")
        .unwrap();
    assert_eq!(reply["headers"]["req_id"], "req-1");
    let turn = gateway.turns.lock().unwrap()[0].clone();
    assert_eq!(turn["sessionKey"], "wecom:dm=U:general");
    assert_eq!(turn["allowPlanModeTools"], false);
    assert_eq!(
        turn["syntheticMessages"][0]["purpose"],
        "wecom_deliverable_hint"
    );
    assert!(
        channel
            .send_text("other", &format!("{}😀", "a".repeat(3999)), None)
            .await
    );
    let raw = server.requests.lock().unwrap().last().unwrap().clone();
    let parsed = pilotdeck_protocol::parse_json(&raw).unwrap();
    let content = parsed
        .get("body")
        .unwrap()
        .get("markdown")
        .unwrap()
        .get("content")
        .unwrap()
        .as_string()
        .unwrap();
    assert_eq!(content.units().len(), 4000);
    assert_eq!(content.units().last(), Some(&0xd83d));
    handle.close().await;
    assert!(!channel.send_text("U", "stopped", None).await);
}
#[tokio::test]
async fn policy_dedup_batch_quote_and_group_scope_are_preserved() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let mut options = server.options();
    options.dm_policy = "allowlist".into();
    options.allow_from = vec!["wecom:user:U".into()];
    options.group_policy = "allowlist".into();
    options.group_allow_from = vec!["wecom:group:G".into()];
    options.groups = json!({"g":{"allowFrom":["U"]}});
    options.text_batch_delay_ms = 40;
    let channel = WeComChannel::new(options, gateway.clone());
    let handle = channel.start().await.unwrap();
    channel
        .message(callback("blocked", "X", "X", false, "no"))
        .await
        .unwrap();
    channel
        .message(callback("first", "G", "U", true, "@bot first"))
        .await
        .unwrap();
    channel
        .message(callback("first", "G", "U", true, "duplicate"))
        .await
        .unwrap();
    channel
        .message(callback("second", "G", "U", true, "second"))
        .await
        .unwrap();
    server
        .wait(|r| r.iter().any(|v| v.contains("reply first\\nsecond")))
        .await;
    assert_eq!(gateway.turns.lock().unwrap().len(), 1);
    assert_eq!(
        gateway.turns.lock().unwrap()[0]["sessionKey"],
        "wecom:group=G:user=U:general"
    );
    let reply = server
        .values()
        .into_iter()
        .find(|v| v["cmd"] == "aibot_respond_msg")
        .unwrap();
    assert_eq!(reply["headers"]["req_id"], "req-second");
    let mut quote = callback("quote", "U", "U", false, "");
    quote["body"]["quote"] = json!({"msgtype":"text","text":{"content":"quoted"}});
    channel.message(quote).await.unwrap();
    server
        .wait(|r| r.iter().any(|v| v.contains("reply quoted")))
        .await;
    handle.close().await;
}
#[tokio::test]
async fn permission_elicitation_answers_and_prompt_retry_use_original_session() {
    for (input, answer, method) in [
        ("permission", "2", "permission_decide"),
        ("question", "0", "elicitation_respond"),
    ] {
        let server = server().await;
        let gateway = Arc::new(Gateway::default());
        let channel = WeComChannel::new(server.options(), gateway.clone());
        let handle = channel.start().await.unwrap();
        server.callback("initial", "G", "U", true, input);
        tokio::time::timeout(Duration::from_secs(3), gateway.ready.notified())
            .await
            .unwrap();
        server.callback("answer", "G", "U", true, answer);
        server
            .wait(|r| r.iter().any(|v| v.contains(&format!("reply {input}"))))
            .await;
        let requests = gateway.requests.lock().unwrap();
        assert_eq!(requests[0].0, method);
        assert_eq!(requests[0].1["sessionKey"], "wecom:group=G:user=U:general");
        drop(requests);
        handle.close().await;
    }
}
#[tokio::test]
async fn new_commands_state_persistence_busy_sessions_and_stop_cancellation() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let changed = Arc::new(StdMutex::new(vec![]));
    let copy = changed.clone();
    let mut options = server.options();
    let saved =
        json!({"activeByChatId":{"wecom:dm=U":"saved"},"projectByScopeKey":{"wecom:dm=U":"old"}});
    let channel = WeComChannel::with_mapper(
        options,
        gateway.clone(),
        saved,
        Some(Arc::new(move |s| copy.lock().unwrap().push(s))),
    );
    let handle = channel.start().await.unwrap();
    channel
        .message(callback("switch", "U", "U", false, "/switch-project Work"))
        .await
        .unwrap();
    let snapshot = channel.snapshot();
    assert_eq!(snapshot["projectByScopeKey"]["wecom:dm=U"], "p");
    assert!(
        snapshot["activeByChatId"]["wecom:dm=U"]
            .as_str()
            .unwrap()
            .starts_with("wecom:dm=U:s_")
    );
    server.callback("hold", "U", "U", false, "hold");
    gateway.ready.notified().await;
    channel
        .message(callback("busy", "U", "U", false, "ignored"))
        .await
        .unwrap();
    assert_eq!(gateway.turns.lock().unwrap().len(), 1);
    channel
        .message(callback("new", "U", "U", false, "/new hello"))
        .await
        .unwrap();
    assert_eq!(gateway.turns.lock().unwrap().len(), 2);
    assert_eq!(gateway.turns.lock().unwrap()[1]["projectKey"], "p");
    assert!(changed.lock().unwrap().len() >= 2);
    tokio::time::timeout(Duration::from_secs(2), handle.close())
        .await
        .unwrap();
    assert_eq!(gateway.cancelled.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn remote_inbound_and_outbound_media_upload_ack_errors_and_tool_images() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = WeComChannel::new(server.options(), gateway.clone());
    let handle = channel.start().await.unwrap();
    let mut incoming = callback("attachment", "U", "U", false, "");
    incoming["body"]["msgtype"] = "image".into();
    incoming["body"]["image"] = json!({"url":server.http,"filename":"bad name.png"});
    channel.message(incoming).await.unwrap();
    let turn = gateway.turns.lock().unwrap()[0].clone();
    assert_eq!(turn["message"], "用户发送了企业微信附件。");
    assert_eq!(turn["attachments"][0]["name"], "bad_name.png");
    assert_eq!(turn["attachments"][0]["mimeType"], "image/png");
    assert_eq!(
        std::fs::read(turn["attachments"][0]["path"].as_str().unwrap()).unwrap(),
        b"image"
    );
    assert!(
        channel
            .send_image("U", &server.http, Some("caption"), None)
            .await
    );
    let values = server.values();
    let init = values
        .iter()
        .find(|v| v["cmd"] == "aibot_upload_media_init")
        .unwrap();
    assert_eq!(init["body"]["type"], "image");
    assert_eq!(init["body"]["md5"], "78805a221a988e79ef3f42d7c5bfd418");
    assert!(
        values
            .iter()
            .any(|v| v["cmd"] == "aibot_upload_media_chunk"
                && v["body"]["base64_data"] == "aW1hZ2U=")
    );
    assert!(
        values
            .iter()
            .any(|v| v["body"]["image"]["media_id"] == "MEDIA")
    );
    *server.fail.lock().unwrap() = Some("aibot_upload_media_chunk".into());
    assert!(
        !channel
            .send_document("U", &server.http, None, None, None)
            .await
    );
    server.callback("tool", "U", "U", false, "image");
    server
        .wait(|r| r.iter().any(|v| v.contains("reply image")))
        .await;
    handle.close().await;
}
#[tokio::test]
async fn reconnect_authenticates_again_and_stop_clears_pending_batches() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let mut options = server.options();
    options.text_batch_delay_ms = 5000;
    let channel = WeComChannel::new(options, gateway.clone());
    let handle = channel.start().await.unwrap();
    server.incoming.send(json!({"cmd":"__close"})).unwrap();
    server
        .wait(|r| r.iter().filter(|v| v.contains("aibot_subscribe")).count() == 2)
        .await;
    assert_eq!(server.connections.load(Ordering::SeqCst), 2);
    channel
        .message(callback("batch", "U", "U", false, "pending"))
        .await
        .unwrap();
    assert_eq!(channel.batches.lock().unwrap().len(), 1);
    handle.close().await;
    assert!(channel.batches.lock().unwrap().is_empty());
    assert!(gateway.turns.lock().unwrap().is_empty());
}

#[tokio::test]
async fn appmsg_attachments_preserve_file_before_image_order() {
    let server = server().await;
    let channel = WeComChannel::new(server.options(), Arc::new(Gateway::default()));
    let (text,attachments)=channel.parts(&json!({"msgtype":"appmsg","appmsg":{"title":"Documents","file":{"base64":"ZmlsZQ==","filename":"report.pdf"},"image":{"base64":"aW1hZ2U=","filename":"preview.png"}}})).await.unwrap();
    assert_eq!(text, "Documents");
    assert_eq!(attachments.len(), 2);
    assert_eq!(attachments[0]["name"], "report.pdf");
    assert_eq!(attachments[0]["metadata"]["mediaType"], "file");
    assert_eq!(attachments[1]["name"], "preview.png");
    assert_eq!(attachments[1]["metadata"]["mediaType"], "image");
}

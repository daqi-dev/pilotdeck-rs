use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
    task::JoinHandle,
};
struct Server {
    base: String,
    requests: Arc<StdMutex<Vec<(String, Value)>>>,
    changed: Arc<Notify>,
    invalid: Arc<AtomicBool>,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn wait(&self, predicate: impl Fn(&[(String, Value)]) -> bool) {
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
}
async fn server() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/open-apis", listener.local_addr().unwrap());
    let requests = Arc::new(StdMutex::new(vec![]));
    let recorded = requests.clone();
    let changed = Arc::new(Notify::new());
    let notify = changed.clone();
    let invalid = Arc::new(AtomicBool::new(false));
    let rejected = invalid.clone();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let recorded = recorded.clone();
            let notify = notify.clone();
            let rejected = rejected.clone();
            connections.spawn(async move{let mut bytes=vec![];let mut buf=[0;4096];let(header,headers,path)=loop{let count=stream.read(&mut buf).await.unwrap();if count==0{return;}bytes.extend_from_slice(&buf[..count]);if let Some(end)=bytes.windows(4).position(|part|part==b"\r\n\r\n"){let headers=String::from_utf8_lossy(&bytes[..end]).into_owned();let path=headers.split_whitespace().nth(1).unwrap().to_owned();break(end+4,headers,path);}};let length=headers.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|value|value.trim().parse::<usize>().ok())).unwrap_or(0);while bytes.len()<header+length{let count=stream.read(&mut buf).await.unwrap();if count==0{return;}bytes.extend_from_slice(&buf[..count]);}let body=serde_json::from_slice(&bytes[header..header+length]).unwrap_or(Value::Null);let reply=if path.ends_with("/tenant_access_token/internal"){assert_eq!(body,json!({"app_id":"app","app_secret":"secret"}));json!({"code":0,"tenant_access_token":"tenant token","expire":7200})}else{assert!(headers.to_ascii_lowercase().contains("authorization: bearer tenant token"));if path.ends_with("/reactions"){json!({"code":0,"data":{"reaction_id":"reaction"}})}else if headers.starts_with("DELETE"){json!({"code":0})}else{assert!(path=="/open-apis/im/v1/messages?receive_id_type=chat_id" || headers.starts_with("PUT /open-apis/im/v1/messages/reply "));json!({"code":if rejected.load(Ordering::SeqCst){99991663}else{0},"data":{"message_id":"reply"}})}};recorded.lock().unwrap().push((format!("{} {path}",headers.split_whitespace().next().unwrap()),body));notify.notify_waiters();let reply=reply.to_string();let _=stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",reply.len()).as_bytes()).await;});
            while connections.try_join_next().is_some() {}
        }
    });
    Server {
        base,
        requests,
        changed,
        invalid,
        task,
    }
}
#[derive(Default)]
struct Gateway {
    turns: StdMutex<Vec<Value>>,
    requests: StdMutex<Vec<(String, Value)>>,
    ready: Notify,
    release: Notify,
    cancelled: AtomicBool,
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
            if method == "list_projects" {
                return Ok(
                    json!({"projects":[{"name":"Demo","projectKey":"P","fullPath":"/project"}]}),
                );
            }
            if method == "permission_decide" || method == "elicitation_respond" {
                self.release.notify_one();
            }
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
            let text = params["message"].as_str().unwrap().to_owned();
            self.turns.lock().unwrap().push(params);
            sink.event(json!({"type":"turn_started","runId":"run"}))
                .await?;
            match text.as_str() {
                "hold" => {
                    let _cancelled = Cancelled(&self.cancelled);
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
                _ => {}
            }
            sink.event(json!({"type":"assistant_text_delta","text":format!("reply {text}")}))
                .await?;
            Ok(())
        })
    }
}
#[derive(Default)]
struct Persist(StdMutex<Vec<Value>>);
impl FeishuStateChange for Persist {
    fn changed(&self, state: Value) {
        self.0.lock().unwrap().push(state);
    }
}
fn options(server: &Server) -> FeishuOptions {
    FeishuOptions {
        app_id: "app".into(),
        app_secret: "secret".into(),
        connection_mode: "webhook".into(),
        api_base: server.base.clone(),
        ..Default::default()
    }
}
fn direct(id: &str, text: &str) -> String {
    json!({"chatId":"chat","text":text,"eventId":id}).to_string()
}
fn reply(body: &Value) -> Option<String> {
    body["content"]
        .as_str()
        .and_then(|content| serde_json::from_str::<Value>(content).ok())
        .and_then(|content| {
            content["text"]
                .as_str()
                .or_else(|| content["zh_cn"]["content"][0][0]["text"].as_str())
                .map(str::to_owned)
        })
}
#[tokio::test]
async fn real_token_rest_reply_dedup_and_cron() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = FeishuChannel::new(options(&server), gateway.clone());
    assert_eq!(channel.webhook("{}").await.0, 503);
    let handle = channel.start().await.unwrap();
    assert_eq!(
        channel.webhook(&direct("id", "你好")).await,
        (200, json!({"ok":true}))
    );
    assert_eq!(
        channel.webhook(&direct("id", "duplicate")).await.1,
        json!({"ok":true,"deduped":true})
    );
    server
        .wait(|requests| {
            requests
                .iter()
                .any(|(_, body)| reply(body).as_deref() == Some("reply 你好"))
        })
        .await;
    assert_eq!(gateway.turns.lock().unwrap()[0]["channelKey"], "feishu");
    assert_eq!(
        gateway.turns.lock().unwrap()[0]["allowPlanModeTools"],
        false
    );
    assert!(
        channel
            .deliver(&json!({"sessionKey":"feishu:chat=chat:general","text":"cron"}))
            .await
            .unwrap()
    );
    assert_eq!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _)| path.ends_with("/tenant_access_token/internal"))
            .count(),
        1
    );
    handle.close().await;
}
#[tokio::test]
async fn common_project_command_persists_mapper_and_forwards_project_mode() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let persist = Arc::new(Persist::default());
    let mut opts = options(&server);
    opts.permission_mode = Some("bypassPermissions".into());
    let channel = FeishuChannel::with_state(
        opts,
        gateway.clone(),
        Default::default(),
        Some(persist.clone()),
    );
    let handle = channel.start().await.unwrap();
    channel
        .incoming(Inbound {
            chat: "chat".into(),
            text: "/switch-project Demo".into(),
            event_id: "command".into(),
            message_id: None,
        })
        .await
        .unwrap();
    assert_eq!(channel.snapshot().await["projectByChatId"]["chat"], "P");
    assert_eq!(persist.0.lock().unwrap().len(), 1);
    channel.webhook(&direct("message", "hello")).await;
    server
        .wait(|requests| {
            requests
                .iter()
                .any(|(_, body)| reply(body).as_deref() == Some("reply hello"))
        })
        .await;
    assert_eq!(gateway.turns.lock().unwrap()[0]["projectKey"], "P");
    assert_eq!(
        gateway.turns.lock().unwrap()[0]["mode"],
        "bypassPermissions"
    );
    handle.close().await;
}
#[tokio::test]
async fn permission_and_question_use_same_live_stream() {
    for (input, answer, method) in [
        ("permission", "2", "permission_decide"),
        ("question", "2", "elicitation_respond"),
    ] {
        let server = server().await;
        let gateway = Arc::new(Gateway::default());
        let channel = FeishuChannel::new(options(&server), gateway.clone());
        let handle = channel.start().await.unwrap();
        channel.webhook(&direct("first", input)).await;
        gateway.ready.notified().await;
        channel.webhook(&direct("answer", answer)).await;
        server
            .wait(|requests| {
                requests
                    .iter()
                    .any(|(_, body)| reply(body).as_deref() == Some(&format!("reply {input}")))
            })
            .await;
        assert_eq!(gateway.requests.lock().unwrap()[0].0, method);
        handle.close().await;
    }
}
#[tokio::test]
async fn new_generation_aborts_old_run_clears_pending_and_suppresses_stale_reply() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let persist = Arc::new(Persist::default());
    let channel = FeishuChannel::with_state(
        options(&server),
        gateway.clone(),
        Default::default(),
        Some(persist.clone()),
    );
    let handle = channel.start().await.unwrap();
    channel.webhook(&direct("hold", "hold")).await;
    gateway.ready.notified().await;
    for i in 0..22 {
        channel
            .incoming(Inbound {
                chat: "chat".into(),
                text: format!("queued{i}"),
                event_id: format!("queued{i}"),
                message_id: None,
            })
            .await
            .unwrap();
    }
    assert_eq!(channel.queues.lock().unwrap()["chat"].pending.len(), 20);
    channel
        .incoming(Inbound {
            chat: "chat".into(),
            text: "/new fresh".into(),
            event_id: "new".into(),
            message_id: None,
        })
        .await
        .unwrap();
    server
        .wait(|requests| {
            requests
                .iter()
                .any(|(_, body)| reply(body).as_deref() == Some("reply fresh"))
        })
        .await;
    assert_eq!(gateway.turns.lock().unwrap().len(), 2);
    {
        let requests = gateway.requests.lock().unwrap();
        assert_eq!(requests[0].0, "abort_turn");
        assert_eq!(requests[0].1["runId"], "run");
    }
    assert!(gateway.cancelled.load(Ordering::SeqCst));
    assert!(
        persist.0.lock().unwrap()[0]["activeByChatId"]["chat"]
            .as_str()
            .unwrap()
            .contains(":s_")
    );
    assert!(
        !server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(_, body)| reply(body).as_deref() == Some("reply hold"))
    );
    handle.close().await;
}
#[tokio::test]
async fn auth_invalidation_refreshes_next_send_and_reaction_lifecycle() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = FeishuChannel::new(options(&server), gateway);
    let handle = channel.start().await.unwrap();
    let body = json!({"header":{"event_id":"V2","event_type":"im.message.receive_v1"},"event":{"message":{"chat_id":"chat","message_type":"text","message_id":"message","content":"{\"text\":\"hello\"}"}}});
    channel.webhook(&body.to_string()).await;
    server
        .wait(|requests| {
            requests.iter().any(|(path, _)| {
                path.starts_with("DELETE ") && path.ends_with("/reactions/reaction")
            })
        })
        .await;
    server.invalid.store(true, Ordering::SeqCst);
    assert!(!channel.send_reply("chat", "invalid").await);
    server.invalid.store(false, Ordering::SeqCst);
    assert!(channel.send_reply("chat", "renew").await);
    assert_eq!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _)| path.ends_with("/tenant_access_token/internal"))
            .count(),
        2
    );
    handle.close().await;
}
#[test]
fn challenge_token_and_v1_v2_contracts() {
    let mut options = FeishuOptions {
        verify_token: Some("expected".into()),
        ..Default::default()
    };
    assert!(
        matches!(inbound::parse(r#"{"type":"url_verification","challenge":"yes","token":"wrong"}"#,&options),Parsed::Challenge(value)if value=="yes")
    );
    assert!(matches!(
        inbound::parse(r#"{"chatId":"chat","text":"no","token":"wrong"}"#, &options),
        Parsed::Ignore
    ));
    assert!(matches!(
        inbound::parse(r#"{"chatId":"chat","text":"yes"}"#, &options),
        Parsed::Message(_)
    ));
    assert!(
        matches!(inbound::parse(r#"{"uuid":"v1","event":{"chat_id":"chat","text":"hello"}}"#,&options),Parsed::Message(value)if value.event_id=="v1")
    );
    options.encrypt_key = Some("secret".into());
    let body = r#"{"encrypt":"MDEyMzQ1Njc4OWFiY2RlZkCYEmkjlf4T8pvqs2JanX1QzzjfM769ymM3DKLh3fLiDRAER+uwsl5fXq63//R0Of+0LG2p0RSi19mkQErpPtPt+ZzxNNmYdwyCXwyAgqPh"}"#;
    assert!(matches!(inbound::parse(body,&options),Parsed::Challenge(value)if value=="encrypted"));
}

#[tokio::test]
async fn live_post_sends_then_edits_same_message_and_segments_utf16_text() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = FeishuChannel::new(options(&server), gateway);
    let handle = channel.start().await.unwrap();
    let mut live = live::Live::default();
    live.append(&channel, "chat", &"x".repeat(24)).await;
    live.append(&channel, "chat", "tail").await;
    live.finish(&channel, "chat").await;
    let requests = server.requests.lock().unwrap();
    let initial = requests
        .iter()
        .find(|(path, body)| path.starts_with("POST ") && body["msg_type"] == "post")
        .unwrap();
    assert_eq!(reply(&initial.1).unwrap(), "x".repeat(24));
    let edited = requests
        .iter()
        .find(|(path, _)| path.starts_with("PUT "))
        .unwrap();
    assert_eq!(reply(&edited.1).unwrap(), format!("{}tail", "x".repeat(24)));
    drop(requests);
    let mut long = live::Live::default();
    long.append(&channel, "chat", &"z".repeat(4001)).await;
    long.finish(&channel, "chat").await;
    let requests = server.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|(_, body)| reply(body).is_some_and(|text| text.len() == 3998))
    );
    assert!(
        requests
            .iter()
            .any(|(_, body)| reply(body).as_deref() == Some("zzz"))
    );
    drop(requests);
    handle.close().await;
}

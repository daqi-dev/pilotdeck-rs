use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Notify, mpsc},
};
struct Server {
    base: String,
    rows: mpsc::UnboundedSender<Value>,
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
    let base = format!("http://{}/ignored/base/", listener.local_addr().unwrap());
    let (rows, receiver) = mpsc::unbounded_channel::<Value>();
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
        let body=if path.starts_with("/api/v1/message?"){let url=url::Url::parse(&format!("http://localhost{path}")).unwrap();let params=url.query_pairs().map(|(key,value)|(key.into_owned(),Value::String(value.into_owned()))).collect::<serde_json::Map<_,_>>();assert_eq!(params["password"],"fixture-password");assert_eq!(params["limit"],"50");recorded.lock().unwrap().push(("poll".into(),Value::Object(params)));notify.notify_one();receiver.lock().await.recv().await.unwrap_or_else(||json!([]))}else{assert_eq!(path,"/api/v1/message/text");assert!(headers.to_lowercase().contains("authorization: bearer fixture-password"));let length=headers.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v|v.trim().parse::<usize>().ok())).unwrap();while bytes.len()<header+length{let count=stream.read(&mut buffer).await.unwrap();if count==0{return}bytes.extend_from_slice(&buffer[..count]);}let body:Value=serde_json::from_slice(&bytes[header..header+length]).unwrap();assert_eq!(body["password"],"fixture-password");assert!(uuid::Uuid::parse_str(body["tempGuid"].as_str().unwrap()).is_ok());recorded.lock().unwrap().push(("send".into(),body));notify.notify_one();json!({"status":200})};
        let body=body.to_string();let _=stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
    });
            while connections.try_join_next().is_some() {}
        }
    });
    Server {
        base,
        rows,
        requests,
        changed,
        task,
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
    fn messages(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(kind, _)| kind == "send")
            .map(|(_, body)| body["message"].as_str().unwrap().into())
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
fn options(server: &Server) -> BlueBubblesOptions {
    BlueBubblesOptions {
        server_url: server.base.clone(),
        password: "fixture-password".into(),
    }
}
fn row(guid: &str, text: &str, timestamp: f64) -> Value {
    json!({"guid":guid,"text":text,"chatGuid":"chat-guid","dateCreated":timestamp})
}

#[tokio::test]
async fn real_poll_updates_cursor_deduplicates_and_sends_authenticated_reply() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = BlueBubblesChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    let timestamp = now_seconds() + 100.0;
    let mut own = row("own", "must ignore", timestamp + 1.0);
    own["isFromMe"] = true.into();
    server
        .rows
        .send(json!({"data":[row("one","hello",timestamp),own]}))
        .unwrap();
    server
        .wait(|requests| {
            requests
                .iter()
                .any(|(kind, body)| kind == "send" && body["message"] == "reply hello")
        })
        .await;
    server
        .wait(|requests| requests.iter().filter(|(kind, _)| kind == "poll").count() >= 2)
        .await;
    server.rows.send(json!({"messages":[row("one","duplicate",timestamp),row("two","second",timestamp+2.0)]})).unwrap();
    server
        .wait(|requests| {
            requests
                .iter()
                .any(|(kind, body)| kind == "send" && body["message"] == "reply second")
        })
        .await;
    assert_eq!(gateway.turns.lock().unwrap().len(), 2);
    assert_eq!(
        gateway.turns.lock().unwrap()[0],
        json!({"sessionKey":"bluebubbles:chat=chat-guid:general","channelKey":"bluebubbles","message":"hello"})
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while channel.cursor.lock().unwrap().timestamp != timestamp + 2.0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let polls = server
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(kind, _)| kind == "poll")
        .map(|(_, body)| body.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        polls[1]["after"].as_str().unwrap().parse::<f64>().unwrap(),
        timestamp + 1.0
    );
    handle.close().await;
}

#[tokio::test]
async fn overlapping_polls_accept_permission_answer_while_old_batch_waits() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = BlueBubblesChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    let timestamp = now_seconds() + 100.0;
    server
        .rows
        .send(json!([row("prompt", "permission", timestamp)]))
        .unwrap();
    gateway.ready.notified().await;
    server
        .wait(|requests| requests.iter().filter(|(kind, _)| kind == "poll").count() >= 2)
        .await;
    server
        .rows
        .send(json!([row("answer", "2", timestamp + 1.0)]))
        .unwrap();
    server
        .wait(|requests| {
            requests
                .iter()
                .any(|(kind, body)| kind == "send" && body["message"] == "reply permission")
        })
        .await;
    let requests = gateway.requests.lock().unwrap();
    assert_eq!(requests[0].0, "permission_decide");
    assert_eq!(requests[0].1["remember"], true);
    drop(requests);
    handle.close().await;
}

#[tokio::test]
async fn busy_new_is_dropped_idle_new_maps_and_commands_forward_as_text() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = BlueBubblesChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    let turn = {
        let channel = channel.clone();
        tokio::spawn(async move { channel.dispatch(&row("hold", "hold", 1.0)).await })
    };
    gateway.ready.notified().await;
    channel.dispatch(&row("new", "/new", 2.0)).await.unwrap();
    assert!(server.messages().is_empty());
    gateway.release.notify_one();
    turn.await.unwrap().unwrap();
    channel.dispatch(&row("new2", "/new", 3.0)).await.unwrap();
    channel
        .dispatch(&row("command", "/projects", 4.0))
        .await
        .unwrap();
    assert!(
        server
            .messages()
            .iter()
            .any(|text| text == "已创建新会话。")
    );
    let turns = gateway.turns.lock().unwrap();
    assert_eq!(turns[1]["message"], "/projects");
    assert!(
        turns[1]["sessionKey"]
            .as_str()
            .unwrap()
            .starts_with("bluebubbles:chat=chat-guid:s_")
    );
    drop(turns);
    handle.close().await;
}

#[tokio::test]
async fn elicitation_alias_payload_maps_number_and_stop_cancels_poll_turn() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = BlueBubblesChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    server
        .rows
        .send(json!([row("question", "question", now_seconds())]))
        .unwrap();
    gateway.ready.notified().await;
    channel
        .dispatch(&json!({"body":"2","chat_guid":"chat-guid","is_from_me":false}))
        .await
        .unwrap();
    server
        .wait(|requests| {
            requests
                .iter()
                .any(|(kind, body)| kind == "send" && body["message"] == "reply question")
        })
        .await;
    assert_eq!(gateway.requests.lock().unwrap()[0].0, "elicitation_respond");
    assert_eq!(
        gateway.requests.lock().unwrap()[0].1["answer"],
        json!({"type":"answered","answers":{"Pick":"B"}})
    );
    handle.close().await;
    assert!(!channel.send_reply("chat-guid", "after stop").await);
}

#[tokio::test]
async fn missing_configuration_fails_without_network_and_stop_cancels_waiting_poll() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    assert!(
        BlueBubblesChannel::new(BlueBubblesOptions::default(), gateway.clone())
            .start()
            .await
            .is_err()
    );
    let channel = BlueBubblesChannel::new(options(&server), gateway);
    let handle = channel.start().await.unwrap();
    server
        .wait(|requests| requests.iter().any(|(kind, _)| kind == "poll"))
        .await;
    tokio::time::timeout(Duration::from_secs(3), handle.close())
        .await
        .unwrap();
}

#[test]
fn bluebubbles_numeric_timestamps_and_js_payload_forms_match_ts() {
    for (input, expected) in [
        (json!("0x10"), Some(16.0)),
        (json!("0b10"), Some(2.0)),
        (json!("0o10"), Some(8.0)),
        (json!(" 1.5 "), Some(1.5)),
        (json!(""), None),
        (json!(false), None),
        (json!("Infinity"), None),
    ] {
        assert_eq!(number(Some(&input)), expected)
    }
    assert!(truthy(Some(&json!("false"))));
    assert_eq!(js_string(Some(&json!({"guid":"x"}))), "[object Object]");
}

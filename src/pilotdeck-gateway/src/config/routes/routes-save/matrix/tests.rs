use super::*;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use std::{convert::Infallible, sync::atomic::AtomicUsize};
use tokio::{
    net::TcpListener,
    sync::{Notify, mpsc},
};

#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    path: String,
    authorization: String,
    body: String,
}
struct Server {
    base: String,
    root: PathBuf,
    sync: mpsc::UnboundedSender<Value>,
    requests: Arc<StdMutex<Vec<Recorded>>>,
    changed: Arc<Notify>,
    fail_send: Arc<AtomicUsize>,
    whoami_status: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
async fn server() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/base", listener.local_addr().unwrap());
    let root = std::env::temp_dir().join(format!("pdrs-matrix-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let (sync, receiver) = mpsc::unbounded_channel::<Value>();
    let receiver = Arc::new(Mutex::new(receiver));
    let requests = Arc::new(StdMutex::new(Vec::<Recorded>::new()));
    let changed = Arc::new(Notify::new());
    let fail_send = Arc::new(AtomicUsize::new(0));
    let whoami_status = Arc::new(AtomicUsize::new(200));
    let task = {
        let requests = requests.clone();
        let changed = changed.clone();
        let fail_send = fail_send.clone();
        let whoami_status = whoami_status.clone();
        tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let receiver = receiver.clone();
                let requests = requests.clone();
                let changed = changed.clone();
                let fail_send = fail_send.clone();
                let whoami_status = whoami_status.clone();
                connections.spawn(async move {
                    let service = service_fn(move |request: Request<Incoming>| {
                        let receiver = receiver.clone();
                        let requests = requests.clone();
                        let changed = changed.clone();
                        let fail_send = fail_send.clone();
                        let whoami_status = whoami_status.clone();
                        async move {
                            let path = request.uri().path_and_query().unwrap().as_str().to_owned();
                            let method = request.method().to_string();
                            let authorization = request
                                .headers()
                                .get("authorization")
                                .unwrap()
                                .to_str()
                                .unwrap()
                                .to_owned();
                            let body = String::from_utf8(
                                request
                                    .into_body()
                                    .collect()
                                    .await
                                    .unwrap()
                                    .to_bytes()
                                    .to_vec(),
                            )
                            .unwrap();
                            requests.lock().unwrap().push(Recorded {
                                method,
                                path: path.clone(),
                                authorization,
                                body,
                            });
                            changed.notify_one();
                            let (status, response) = if path
                                .starts_with("/base/_matrix/client/v3/sync?")
                            {
                                let value = receiver.lock().await.recv().await.unwrap_or(json!({}));
                                (200, value)
                            } else if path == "/base/_matrix/client/v3/account/whoami" {
                                (
                                    whoami_status.load(Ordering::SeqCst),
                                    json!({"user_id":"@bot:test"}),
                                )
                            } else if path.contains("/send/m.room.message/") {
                                let failed = fail_send
                                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                                        n.checked_sub(1)
                                    })
                                    .is_ok();
                                (if failed { 503 } else { 200 }, json!({"event_id":"$sent"}))
                            } else {
                                (200, json!({}))
                            };
                            Ok::<_, Infallible>(
                                Response::builder()
                                    .status(status as u16)
                                    .header("content-type", "application/json")
                                    .body(Full::new(Bytes::from(response.to_string())))
                                    .unwrap(),
                            )
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(socket), service)
                        .await;
                });
                while connections.try_join_next().is_some() {}
            }
        })
    };
    Server {
        base,
        root,
        sync,
        requests,
        changed,
        fail_send,
        whoami_status,
        task,
    }
}
impl Server {
    fn options(&self) -> MatrixOptions {
        MatrixOptions {
            homeserver: format!("{}/", self.base),
            access_token: "test-token".into(),
            user_id: Some("@fallback:test".into()),
            storage_path: self.root.join("matrix-bot-storage.json"),
        }
    }
    fn batch(&self, token: &str, room: &str, events: Vec<Value>) {
        self.sync
            .send(
                json!({"next_batch":token,"rooms":{"join":{room:{"timeline":{"events":events}}}}}),
            )
            .unwrap();
    }
    async fn wait(&self, condition: impl Fn(&[Recorded]) -> bool) {
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let notified = self.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if condition(&self.requests.lock().unwrap()) {
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
            .filter(|r| r.path.contains("/send/m.room.message/"))
            .filter_map(|r| serde_json::from_str(&r.body).ok())
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
        Box::pin(async { panic!("live streaming required") })
    }
    fn stream_turn<'a>(
        &'a self,
        params: Value,
        sink: &'a dyn ChannelEventSink,
    ) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            let _turn = Turn(&self.cancelled);
            let input = params["message"].as_str().unwrap().to_owned();
            self.turns.lock().unwrap().push(params);
            match input.as_str() {
                "hold" => {
                    self.ready.notify_one();
                    self.release.notified().await;
                }
                "permission" => {
                    sink.event(json!({"type":"permission_request","requestId":"P","toolName":"write_file","payload":{"path":"a"}})).await?;
                    self.ready.notify_one();
                    self.release.notified().await;
                }
                "question" => {
                    sink.event(json!({"type":"elicitation_request","requestId":"Q","questions":[{"question":"Pick","header":"Header","options":[{"label":"A","description":"first"},{"label":"B","description":"second"}]}]})).await?;
                    self.ready.notify_one();
                    self.release.notified().await;
                }
                "fail" => return Err(failure("injected turn failure")),
                "render" => {
                    sink.event(json!({"type":"assistant_thinking_delta","text":"hidden"}))
                        .await?;
                    sink.event(json!({"type":"tool_call_finished","toolCallId":"T","toolName":"read_file","ok":false})).await?;
                    sink.event(json!({"type":"error","message":"provider error"}))
                        .await?;
                }
                _ => {}
            }
            sink.event(json!({"type":"tool_call_started","name":"ignored"}))
                .await?;
            sink.event(json!({"type":"assistant_text_delta","text":format!("reply {input}")}))
                .await?;
            Ok(())
        })
    }
}
fn message(text: &str) -> Value {
    json!({"type":"m.room.message","sender":"@alice:test","content":{"msgtype":"m.text","body":text}})
}

#[tokio::test]
async fn real_sync_joins_invites_filters_events_sends_and_resumes_persisted_cursor() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let options = server.options();
    let channel = MatrixChannel::new(options.clone(), gateway.clone());
    let handle = channel.start().await.unwrap();
    server.sync.send(json!({"next_batch":"batch/+1","rooms":{"invite":{"!invite:test":{}},"join":{"!room:test":{"timeline":{"events":[
        {"type":"m.room.message","sender":"@bot:test","content":{"body":"self"}},
        {"type":"m.room.message","sender":"@alice:test","content":{"body":"edit","m.relates_to":{"rel_type":"m.replace"}}},
        {"type":"m.room.message","sender":"@alice:test","content":{"msgtype":"m.notice","body":"notice"}},
        {"type":"m.room.message","sender":"@alice:test","content":{"msgtype":"m.image","body":"image"}},
        {"type":"m.room.member","sender":"@alice:test","content":{"body":"member"}},
        {"type":"m.room.message","content":{"body":"no sender"}},message("  你好  ")
    ]}}}}})).unwrap();
    server
        .wait(|r| {
            r.iter().any(|r| r.path.contains("/send/m.room.message/"))
                && r.iter().any(|r| r.path.contains("/join/!invite%3Atest"))
                && r.iter().any(|r| r.path.contains("since=batch%2F%2B1"))
        })
        .await;
    assert_eq!(
        *gateway.turns.lock().unwrap(),
        vec![
            json!({"sessionKey":"matrix:chat=!room:test:general","channelKey":"matrix","message":"你好"})
        ]
    );
    assert_eq!(
        server.replies()[0],
        json!({"msgtype":"m.text","body":"reply 你好"})
    );
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.authorization == "Bearer test-token")
    );
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.method == "PUT" && r.path.contains("/rooms/!room%3Atest/send/"))
    );
    let stored: Value =
        serde_json::from_slice(&std::fs::read(&options.storage_path).unwrap()).unwrap();
    assert_eq!(stored["syncToken"], "batch/+1");
    handle.close().await;
    let before = server.requests.lock().unwrap().len();
    let restarted = MatrixChannel::new(options, gateway);
    let restarted_handle = restarted.start().await.unwrap();
    server
        .wait(|r| {
            r[before..]
                .iter()
                .any(|r| r.path.ends_with("sync?timeout=30000&since=batch%2F%2B1"))
        })
        .await;
    restarted_handle.close().await;
}

#[tokio::test]
async fn sync_answers_live_permission_and_elicitation_in_original_session() {
    for (input, answer, method) in [
        ("permission", "2", "permission_decide"),
        ("question", "2", "elicitation_respond"),
        ("question", "0", "elicitation_respond"),
    ] {
        let server = server().await;
        let gateway = Arc::new(Gateway::default());
        let channel = MatrixChannel::new(server.options(), gateway.clone());
        let handle = channel.start().await.unwrap();
        server.batch("one", "!room:test", vec![message(input)]);
        tokio::time::timeout(Duration::from_secs(4), gateway.ready.notified())
            .await
            .unwrap();
        server.batch("two", "!room:test", vec![message(answer)]);
        server
            .wait(|r| r.iter().any(|r| r.body.contains(&format!("reply {input}"))))
            .await;
        let requests = gateway.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, method);
        assert_eq!(
            requests[0].1["sessionKey"],
            "matrix:chat=!room:test:general"
        );
        if input == "permission" {
            assert_eq!(requests[0].1["remember"], true);
            assert_eq!(requests[0].1["decision"], "allow");
        } else if answer == "0" {
            assert_eq!(
                requests[0].1["answer"],
                json!({"type":"cancelled","reason":"user cancelled"})
            );
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
async fn new_maps_room_busy_messages_drop_and_close_cancels_inflight_turn() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    server.whoami_status.store(503, Ordering::SeqCst);
    let channel = MatrixChannel::new(server.options(), gateway.clone());
    let handle = channel.start().await.unwrap();
    let mut own = message("self");
    own["sender"] = "@fallback:test".into();
    channel.message("!room:test", own).await.unwrap();
    assert!(gateway.turns.lock().unwrap().is_empty());
    channel
        .message("!room:test", message("/new"))
        .await
        .unwrap();
    channel
        .message("!room:test", message("/projects"))
        .await
        .unwrap();
    let key = gateway.turns.lock().unwrap()[0]["sessionKey"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(key.starts_with("matrix:chat=!room:test:s_"));
    assert_eq!(gateway.turns.lock().unwrap()[0]["message"], "/projects");
    server.batch("one", "!room:test", vec![message("hold")]);
    gateway.ready.notified().await;
    channel
        .message("!room:test", message("/new reset"))
        .await
        .unwrap();
    assert_eq!(gateway.turns.lock().unwrap().len(), 2);
    assert_eq!(
        channel
            .mapper
            .lock()
            .unwrap()
            .session("matrix", "!room:test"),
        key
    );
    tokio::time::timeout(Duration::from_secs(2), handle.close())
        .await
        .unwrap();
    assert_eq!(gateway.cancelled.load(Ordering::SeqCst), 2);
    assert!(
        !channel
            .deliver(&json!({"sessionKey":key,"text":"after stop"}))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn cron_delivery_preserves_utf16_split_and_continues_after_send_failure() {
    let server = server().await;
    let channel = MatrixChannel::new(server.options(), Arc::new(Gateway::default()));
    let handle = channel.start().await.unwrap();
    server.fail_send.store(1, Ordering::SeqCst);
    let text = format!("{}😀", "a".repeat(3999));
    assert!(!channel.deliver(&json!({"originChannelKey":"matrix","originSessionKey":"matrix:chat=!room:test:general","text":text})).await.unwrap());
    let requests = server.requests.lock().unwrap();
    let sends = requests
        .iter()
        .filter(|r| r.path.contains("/send/"))
        .collect::<Vec<_>>();
    assert_eq!(sends.len(), 2);
    let first = pilotdeck_protocol::parse_json(&sends[0].body).unwrap();
    let second = pilotdeck_protocol::parse_json(&sends[1].body).unwrap();
    let first = first.get("body").unwrap().as_string().unwrap();
    let second = second.get("body").unwrap().as_string().unwrap();
    assert_eq!(first.len_utf16(), 4000);
    assert_eq!(first.units().last(), Some(&0xd83d));
    assert_eq!(second.units(), &[0xde00]);
    drop(requests);
    for delivery in [
        json!({"originChannelKey":"signal","sessionKey":"matrix:chat=!room:test:general","text":"no"}),
        json!({"sessionKey":"matrix:chat=!room:test:invalid","text":"no"}),
        json!({"originSessionKey":"","sessionKey":"matrix:chat=!room:test:general","text":"no"}),
    ] {
        assert!(!channel.deliver(&delivery).await.unwrap());
    }
    assert!(
        channel
            .deliver(&json!({"sessionKey":"matrix:chat=!room:test:general","text":"cron"}))
            .await
            .unwrap()
    );
    handle.close().await;
}

#[tokio::test]
async fn failed_initial_permission_prompt_retries_before_answer_can_reach_gateway() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = MatrixChannel::new(server.options(), gateway.clone());
    let handle = channel.start().await.unwrap();
    server.fail_send.store(1, Ordering::SeqCst);
    server.batch("one", "!room:test", vec![message("permission")]);
    gateway.ready.notified().await;
    channel.message("!room:test", message("1")).await.unwrap();
    assert!(gateway.requests.lock().unwrap().is_empty());
    let replies = server.replies();
    assert!(
        replies
            .iter()
            .any(|r| r["body"].as_str().is_some_and(|s| s.contains("正在重试")))
    );
    channel.message("!room:test", message("1")).await.unwrap();
    assert_eq!(gateway.requests.lock().unwrap().len(), 1);
    handle.close().await;
}

#[tokio::test]
async fn streamed_rendering_and_submit_failure_match_matrix_plain_text_contract() {
    let server = server().await;
    let channel = MatrixChannel::new(server.options(), Arc::new(Gateway::default()));
    let handle = channel.start().await.unwrap();
    channel
        .message("!room:test", message("render"))
        .await
        .unwrap();
    channel
        .message("!room:test", message("fail"))
        .await
        .unwrap();
    let replies = server.replies();
    assert_eq!(
        replies[0]["body"],
        "⚠️ read_file failed\n\n❌ provider error\nreply render"
    );
    assert_eq!(replies[1]["body"], "处理消息时发生错误，请重试。");
    handle.close().await;
}

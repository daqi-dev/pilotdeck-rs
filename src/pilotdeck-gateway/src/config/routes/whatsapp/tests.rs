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

struct Server {
    base: String,
    root: PathBuf,
    messages: mpsc::UnboundedSender<Value>,
    requests: Arc<StdMutex<Vec<(String, Value)>>>,
    changed: Arc<Notify>,
    fail_send: Arc<AtomicUsize>,
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
    let root = std::env::temp_dir().join(format!("pdrs-whatsapp-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    // A real owned node child exercises BRIDGE_PORT and process cleanup. The
    // bridge HTTP peer is this Rust loopback server, with deterministic data.
    std::fs::write(root.join("bridge.js"),"const fs=require('node:fs');const p=require('node:path');fs.writeFileSync(p.join(__dirname,'child.json'),JSON.stringify({pid:process.pid,port:process.env.BRIDGE_PORT}));setInterval(()=>{},1000);\n").unwrap();
    let (messages, receiver) = mpsc::unbounded_channel::<Value>();
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
            let readiness = Arc::new(AtomicBool::new(true));
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let receiver = receiver.clone();
                let requests = requests.clone();
                let changed = changed.clone();
                let fail_send = fail_send.clone();
                let readiness = readiness.clone();
                connections.spawn(async move {
                    let service = service_fn(move |request: Request<Incoming>| {
                        let receiver = receiver.clone();
                        let requests = requests.clone();
                        let changed = changed.clone();
                        let fail_send = fail_send.clone();
                        let readiness = readiness.clone();
                        async move {
                            let path = request.uri().path().to_owned();
                            let method = request.method().to_string();
                            let bytes = request.into_body().collect().await.unwrap().to_bytes();
                            let body = if bytes.is_empty() {
                                Value::Null
                            } else {
                                serde_json::from_slice(&bytes).unwrap()
                            };
                            requests
                                .lock()
                                .unwrap()
                                .push((format!("{method} {path}"), body));
                            changed.notify_one();
                            let (status, value) = if path == "/base/messages" {
                                if readiness.swap(false, Ordering::SeqCst) {
                                    (404, json!({}))
                                } else {
                                    (200, receiver.lock().await.recv().await.unwrap_or(json!([])))
                                }
                            } else {
                                assert_eq!(path, "/base/send");
                                assert_eq!(method, "POST");
                                let failed = fail_send
                                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                                        n.checked_sub(1)
                                    })
                                    .is_ok();
                                (if failed { 503 } else { 200 }, json!({}))
                            };
                            Ok::<_, Infallible>(
                                Response::builder()
                                    .status(status)
                                    .header("content-type", "application/json")
                                    .body(Full::new(Bytes::from(value.to_string())))
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
        messages,
        requests,
        changed,
        fail_send,
        task,
    }
}
impl Server {
    fn options(&self) -> WhatsAppOptions {
        WhatsAppOptions {
            bridge_url: format!("{}///", self.base),
            bridge_path: self.root.join("bridge.js"),
            node_executable: "node".into(),
        }
    }
    async fn wait(&self, condition: impl Fn(&[(String, Value)]) -> bool) {
        tokio::time::timeout(Duration::from_secs(6), async {
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
            .filter(|(path, _)| path == "POST /base/send")
            .map(|(_, body)| body.clone())
            .collect()
    }
    async fn child(&self) -> Value {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(bytes) = std::fs::read(self.root.join("child.json"))
                    && let Ok(value) = serde_json::from_slice(&bytes)
                {
                    return value;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
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
                "fail" => return Err(failure("injected")),
                _ => {}
            }
            sink.event(json!({"type":"assistant_text_delta","text":format!("reply {input}")}))
                .await?;
            Ok(())
        })
    }
}

#[tokio::test]
async fn owns_real_bridge_child_and_normalizes_deduplicated_poll_messages() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = WhatsAppChannel::new(server.options(), gateway.clone());
    let handle = channel.start().await.unwrap();
    let child = server.child().await;
    let port = url::Url::parse(&server.base)
        .unwrap()
        .port()
        .unwrap()
        .to_string();
    assert_eq!(child["port"], port);
    server.messages.send(json!({"data":[{"key":7,"chat_id":" room ","body":" 你好 "},{"id":"7","chatId":"other","text":"duplicate"},{"id":"empty","chatId":"room","text":""},{"id":"","chatId":"room","text":"invalid"}]})).unwrap();
    server
        .wait(|r| r.iter().any(|(_, body)| body["message"] == "reply 你好"))
        .await;
    assert_eq!(
        *gateway.turns.lock().unwrap(),
        vec![
            json!({"sessionKey":"whatsapp:chat=room:general","channelKey":"whatsapp","message":"你好"})
        ]
    );
    assert_eq!(
        server.replies()[0],
        json!({"chatId":"room","message":"reply 你好"})
    );
    server.messages.send(json!({"messages":[{"id":"7","chatId":"room","text":"still duplicate"},{"messageId":"8","from":"second","content":false}]})).unwrap();
    server
        .wait(|r| r.iter().any(|(_, body)| body["message"] == "reply false"))
        .await;
    assert_eq!(gateway.turns.lock().unwrap().len(), 2);
    handle.close().await;
    #[cfg(unix)]
    {
        let pid = rustix::process::Pid::from_raw(child["pid"].as_u64().unwrap() as i32).unwrap();
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
    }
}

#[tokio::test]
async fn polled_permission_and_elicitation_answers_use_the_original_live_gateway() {
    for (input, answer, method) in [
        ("permission", "0", "permission_decide"),
        ("question", "2", "elicitation_respond"),
        ("question", "0", "elicitation_respond"),
    ] {
        let server = server().await;
        let gateway = Arc::new(Gateway::default());
        let channel = WhatsAppChannel::new(server.options(), gateway.clone());
        let handle = channel.start().await.unwrap();
        server
            .messages
            .send(json!([{"id":"one","chatId":"room","text":input}]))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), gateway.ready.notified())
            .await
            .unwrap();
        server
            .messages
            .send(json!([{"id":"two","chatId":"room","text":answer}]))
            .unwrap();
        server
            .wait(|r| {
                r.iter()
                    .any(|(_, body)| body["message"] == format!("reply {input}"))
            })
            .await;
        let requests = gateway.requests.lock().unwrap();
        assert_eq!(requests[0].0, method);
        assert_eq!(requests[0].1["sessionKey"], "whatsapp:chat=room:general");
        if input == "permission" {
            assert_eq!(requests[0].1["decision"], "deny");
            assert_eq!(
                requests[0].1["reason"],
                "User denied permission from IM channel."
            );
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
async fn new_session_busy_drop_and_stop_cancel_polls_turn_and_child() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = WhatsAppChannel::new(server.options(), gateway.clone());
    let handle = channel.start().await.unwrap();
    channel.message("room", "/new").await.unwrap();
    channel.message("room", "/projects").await.unwrap();
    let key = gateway.turns.lock().unwrap()[0]["sessionKey"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(key.starts_with("whatsapp:chat=room:s_"));
    assert_eq!(gateway.turns.lock().unwrap()[0]["message"], "/projects");
    server.messages.send(json!([{"id":"hold","chatId":"room","text":"hold"},{"id":"skip","chatId":"room","text":"/new skipped"}])).unwrap();
    gateway.ready.notified().await;
    assert_eq!(gateway.turns.lock().unwrap().len(), 2);
    assert_eq!(
        channel.mapper.lock().unwrap().session("whatsapp", "room"),
        key
    );
    tokio::time::timeout(Duration::from_secs(2), handle.close())
        .await
        .unwrap();
    assert_eq!(gateway.cancelled.load(Ordering::SeqCst), 2);
    assert!(
        !channel
            .deliver(&json!({"sessionKey":key,"text":"after close"}))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn sends_one_full_cron_message_and_reports_http_failures() {
    let server = server().await;
    let channel = WhatsAppChannel::new(server.options(), Arc::new(Gateway::default()));
    let handle = channel.start().await.unwrap();
    let text = "x".repeat(9000);
    assert!(channel.deliver(&json!({"originChannelKey":"whatsapp","originSessionKey":"whatsapp:chat=room:general","text":text})).await.unwrap());
    assert_eq!(server.replies()[0]["message"].as_str().unwrap().len(), 9000);
    server.fail_send.store(1, Ordering::SeqCst);
    assert!(
        !channel
            .deliver(&json!({"sessionKey":"whatsapp:chat=room:general","text":"failed"}))
            .await
            .unwrap()
    );
    assert!(!channel.deliver(&json!({"originChannelKey":"matrix","sessionKey":"whatsapp:chat=room:general","text":"wrong"})).await.unwrap());
    assert!(!channel.deliver(&json!({"originSessionKey":"","sessionKey":"whatsapp:chat=room:general","text":"wrong"})).await.unwrap());
    channel.message("room", "fail").await.unwrap();
    assert_eq!(
        server.replies().last().unwrap()["message"],
        "处理消息时发生错误，请重试。"
    );
    handle.close().await;
}

#[tokio::test]
async fn initial_permission_send_failure_retries_prompt_before_decision() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = WhatsAppChannel::new(server.options(), gateway.clone());
    let handle = channel.start().await.unwrap();
    server.fail_send.store(1, Ordering::SeqCst);
    server
        .messages
        .send(json!([{"id":"p","chatId":"room","text":"permission"}]))
        .unwrap();
    gateway.ready.notified().await;
    channel.message("room", "1").await.unwrap();
    assert!(gateway.requests.lock().unwrap().is_empty());
    channel.message("room", "2").await.unwrap();
    assert_eq!(gateway.requests.lock().unwrap()[0].1["remember"], true);
    handle.close().await;
}

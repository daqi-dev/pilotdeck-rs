use super::*;
use futures_util::{SinkExt, StreamExt};
use std::collections::VecDeque;
use tokio::{net::TcpListener, sync::mpsc};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
fn js(text: &str) -> JsValue {
    parse_json(text).unwrap()
}
fn oracle() -> serde_json::Value {
    serde_json::from_str(include_str!("oracle.json")).unwrap()
}
fn json(value: &JsValue) -> serde_json::Value {
    serde_json::from_str(&stringify_json(value).unwrap().unwrap()).unwrap()
}
#[test]
fn mapper_and_renderer_match_real_fixed_ts_capture() {
    let gold = oracle();
    let mut mapper = DingTalkSessionMapper::new(
        Default::default(),
        Arc::new(|| "00000000-0000-0000-0000-000000000001".into()),
    );
    for case in gold["mapper"].as_array().unwrap() {
        let text: JsString = case["text"].as_str().unwrap().into();
        let mapped = mapper.resolve(&"room".into(), &text);
        let expected = &case["result"];
        assert_eq!(
            shown(&mapped.session_key),
            expected["sessionKey"].as_str().unwrap()
        );
        assert_eq!(
            shown(&mapped.message),
            expected["message"].as_str().unwrap()
        );
        assert_eq!(mapped.new_session, expected.get("command").is_some());
    }
    for case in gold["render"].as_array().unwrap() {
        let event = js(&case["event"].to_string());
        assert_eq!(
            render::render_dingtalk_event(&event).map(|s| shown(&s)),
            case["result"].as_str().map(str::to_owned)
        );
    }
}
struct MockGateway {
    calls: watch::Sender<Vec<TurnRequest>>,
    gate: watch::Sender<bool>,
}
struct Events {
    gate: watch::Receiver<bool>,
    events: VecDeque<JsValue>,
}
impl TurnEventStream for Events {
    fn next(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>,
    > {
        Box::pin(async move {
            self.gate
                .wait_for(|open| *open)
                .await
                .map_err(|_| ChannelError::error("mock gate closed"))?;
            Ok(self.events.pop_front())
        })
    }
}
impl TurnGateway for MockGateway {
    fn submit(
        &self,
        request: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        self.calls.send_modify(|rows| rows.push(request));
        let gate = self.gate.subscribe();
        Box::pin(async move {
            Ok(Box::new(Events {
                gate,
                events: VecDeque::from([
                    js(r#"{"type":"assistant_text_delta","text":" REPLY "}"#),
                    js(r#"{"type":"tool_call_finished","ok":false,"toolName":"bash"}"#),
                ]),
            }) as Box<dyn TurnEventStream>)
        })
    }
}
#[test]
fn extracts_text_and_rich_fallback() {
    assert_eq!(
        extract_text(&parse_json(r#"{"text":{"content":" hello "}}"#).unwrap()),
        JsString::from("hello")
    );
    assert_eq!(
        extract_text(&parse_json(r#"{"richText":[{"text":"a"},{"text":"b"}]}"#).unwrap()),
        JsString::from("a b")
    );
}
struct Mock {
    endpoint: String,
    route: String,
    inbound: mpsc::UnboundedSender<JsValue>,
    requests: watch::Sender<Vec<JsValue>>,
    acks: watch::Sender<Vec<JsValue>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
async fn until(mut f: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        while !f() {
            tokio::task::yield_now().await;
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
impl Mock {
    async fn start() -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", http.local_addr().unwrap());
        let ws = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_url = format!("ws://{}/stream", ws.local_addr().unwrap());
        let (requests, _) = watch::channel(Vec::<JsValue>::new());
        let recorded = requests.clone();
        let (acks, _) = watch::channel(Vec::<JsValue>::new());
        let ack_rows = acks.clone();
        let (inbound, mut receiver) = mpsc::unbounded_channel::<JsValue>();
        let http_task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = http.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0u8; 1024];

                loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if bytes.windows(4).any(|s| s == b"\r\n\r\n") {
                        break;
                    }
                }
                let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") else {
                    continue;
                };
                let header_end = end + 4;
                let headers = String::from_utf8_lossy(&bytes[..end]).into_owned();
                let path = headers
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_owned();
                let length = headers
                    .lines()
                    .find_map(|s| {
                        let (k, v) = s.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + length {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                }
                let body = parse_json(&String::from_utf8_lossy(
                    &bytes[header_end..header_end + length],
                ))
                .unwrap();
                recorded.send_modify(|v| {
                    v.push(obj([
                        ("path", JsValue::String(path.clone().into())),
                        ("body", body.clone()),
                    ]))
                });
                let (status, response) = if path == "/open" {
                    assert!(
                        headers
                            .to_ascii_lowercase()
                            .contains("accept: application/json")
                    );
                    (
                        200,
                        stringify_json(&obj([
                            ("endpoint", JsValue::String(ws_url.clone().into())),
                            ("ticket", "owned-ticket".into()),
                        ]))
                        .unwrap()
                        .unwrap(),
                    )
                } else if body
                    .get("markdown")
                    .and_then(|m| m.get("text"))
                    .and_then(JsValue::as_string)
                    .is_some_and(|s| s.equals_str("FAIL"))
                {
                    (500, "owned fail".into())
                } else {
                    (200, "ok".into())
                };
                let response = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{response}",
                    response.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let ws_task = tokio::spawn(async move {
            loop {
                let (s, _) = ws.accept().await.unwrap();
                let mut socket = accept_async(s).await.unwrap();
                loop {
                    tokio::select! {Some(event)=receiver.recv()=>{if socket.send(Message::Text(stringify_json(&event).unwrap().unwrap().into())).await.is_err(){break}},message=socket.next()=>{match message{Some(Ok(Message::Text(s)))=>ack_rows.send_modify(|v|v.push(parse_json(&s).unwrap())),Some(Ok(Message::Ping(b)))=>{let _=socket.send(Message::Pong(b)).await;},_=>break}}}
                }
            }
        });
        Self {
            endpoint: format!("{base}/open"),
            route: format!("{base}/reply"),
            inbound,
            requests,
            acks,
            tasks: vec![http_task, ws_task],
        }
    }
    fn send(&self, id: &str, text: JsValue, topic: &str) {
        let data = obj([
            ("msgId", id.into()),
            ("conversationId", "room".into()),
            ("text", text),
            (
                "sessionWebhook",
                "https://api.dingtalk.com/mock-hook".into(),
            ),
        ]);
        self.inbound
            .send(obj([
                ("specVersion", "1.0".into()),
                ("type", "EVENT".into()),
                (
                    "headers",
                    obj([("topic", topic.into()), ("messageId", id.into())]),
                ),
                (
                    "data",
                    JsValue::String(stringify_json(&data).unwrap().unwrap().into()),
                ),
            ]))
            .unwrap();
    }
}
#[tokio::test]
async fn native_http_ws_to_a7_and_reply_matches_fixed_ts_stop_contract() {
    let mock = Mock::start().await;
    let (calls, _) = watch::channel(Vec::<TurnRequest>::new());
    let (gate, _) = watch::channel(false);
    let route = mock.route.clone();
    let channel = DingTalkChannel::new(
        DingTalkOptions {
            client_id: Some(" owned-id ".into()),
            client_secret: Some(" owned-secret ".into()),
            mapper: DingTalkSessionMapper::new(
                Default::default(),
                Arc::new(|| "00000000-0000-0000-0000-000000000001".into()),
            ),
            stream_endpoint: Some(mock.endpoint.clone()),
            webhook_route: Some(Arc::new(move |_| route.clone())),
            ..Default::default()
        },
        Arc::new(super::super::im_interaction::NoInteraction),
    );
    let handle = channel
        .start(ChannelStartDeps {
            turn_gateway: Some(Arc::new(MockGateway {
                calls: calls.clone(),
                gate: gate.clone(),
            })),
            ..Default::default()
        })
        .await
        .unwrap();
    mock.send("ignored", "drop".into(), "other");
    mock.send("new", "/new".into(), stream::TOPIC_ROBOT);
    until(|| mock.requests.borrow().len() == 2).await;
    mock.send("first", "hello".into(), stream::TOPIC_ROBOT);
    until(|| calls.borrow().len() == 1).await;
    mock.send("first", "repeated".into(), stream::TOPIC_ROBOT);
    mock.send("busy", "busy".into(), stream::TOPIC_ROBOT);
    until(|| mock.acks.borrow().len() == 5).await;
    until(|| channel.active_tasks() == 1).await;
    gate.send_replace(true);
    until(|| mock.requests.borrow().len() == 3).await;
    channel.drain().await;
    let cron = CronResultDelivery {
        task_id: "t".into(),
        run_id: "r".into(),
        session_key: "dingtalk:chat=room:general".into(),
        channel_key: ChannelKey::new("dingtalk"),
        origin_session_key: None,
        origin_channel_key: None,
        text: "CRON".into(),
    };
    assert!(channel.deliver_cron_result(&cron).await);
    assert!(
        !channel
            .deliver_cron_result(&CronResultDelivery {
                text: "FAIL".into(),
                ..cron.clone()
            })
            .await
    );
    gate.send_replace(false);
    mock.send(
        "second",
        obj([("content", "inflight".into())]),
        stream::TOPIC_ROBOT,
    );
    until(|| calls.borrow().len() == 2).await;
    handle.stop("owned-stop").await.unwrap();
    assert_eq!(channel.active_tasks(), 1);
    gate.send_replace(true);
    channel.drain().await;
    assert!(!channel.deliver_cron_result(&cron).await);
    let gold = oracle();
    let requests: Vec<_> = mock.requests.borrow().iter().map(json).collect();
    assert_eq!(serde_json::json!(requests), gold["requests"]);
    let turns:Vec<_>=calls.borrow().iter().map(|r|serde_json::json!({"sessionKey":shown(&r.session_key),"channelKey":"dingtalk","message":shown(&r.message)})).collect();
    assert_eq!(serde_json::json!(turns), gold["calls"]);
}
#[tokio::test]
async fn missing_credentials_noop_and_untrusted_webhook_is_not_used() {
    let channel = DingTalkChannel::new(
        DingTalkOptions {
            env: Some(Arc::new(|_| None)),
            ..Default::default()
        },
        Arc::new(super::super::im_interaction::NoInteraction),
    );
    let h = channel.start(Default::default()).await.unwrap();
    h.stop("").await.unwrap();
    assert!(
        !channel
            .shared
            .send_reply(&"room".into(), "text".into())
            .await
    );
}
struct InteractiveGateway {
    permission: bool,
    decisions: watch::Sender<Vec<JsValue>>,
    waiting: watch::Sender<bool>,
    answered: watch::Sender<bool>,
}
struct InteractiveStream {
    phase: u8,
    permission: bool,
    waiting: watch::Sender<bool>,
    answered: watch::Receiver<bool>,
}
impl TurnEventStream for InteractiveStream {
    fn next(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>,
    > {
        Box::pin(async move {
            match self.phase {
                0 => {
                    self.phase = 1;
                    Ok(Some(if self.permission {
                        js(
                            r#"{"type":"permission_request","requestId":"ding-request","toolName":"bash","payload":{"command":"echo owned"}}"#,
                        )
                    } else {
                        js(
                            r#"{"type":"elicitation_request","requestId":"ding-request","questions":[{"header":"H","question":"Q","options":[{"label":"A","description":"a"},{"label":"B","description":"b"}]}]}"#,
                        )
                    }))
                }
                1 => {
                    self.waiting.send_replace(true);
                    self.answered
                        .wait_for(|answer| *answer)
                        .await
                        .map_err(|_| ChannelError::error("answer channel gone"))?;
                    self.phase = 2;
                    Ok(Some(js(
                        r#"{"type":"assistant_text_delta","text":"ANSWERED"}"#,
                    )))
                }
                _ => Ok(None),
            }
        })
    }
}
impl TurnGateway for InteractiveGateway {
    fn submit(&self, _: TurnRequest) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        let stream = InteractiveStream {
            phase: 0,
            permission: self.permission,
            waiting: self.waiting.clone(),
            answered: self.answered.subscribe(),
        };
        Box::pin(async move { Ok(Box::new(stream) as Box<dyn TurnEventStream>) })
    }
}
impl super::super::bluebubbles::InteractionResponder for InteractiveGateway {
    fn respond_elicitation(
        &self,
        response: super::super::sms::ElicitationResponse,
    ) -> BoxFuture<Result<(), ChannelError>> {
        self.decisions
            .send_modify(|rows| rows.push(JsValue::Object(response.to_js())));
        self.answered.send_replace(true);
        Box::pin(async { Ok(()) })
    }
    fn permission_decide(
        &self,
        decision: super::super::sms::PermissionDecision,
    ) -> BoxFuture<Result<super::super::sms::PermissionDecisionOutcome, ChannelError>> {
        self.decisions
            .send_modify(|rows| rows.push(JsValue::Object(decision.to_js())));
        self.answered.send_replace(true);
        Box::pin(async { Ok(super::super::sms::PermissionDecisionOutcome { delivered: true }) })
    }
}
#[tokio::test]
async fn actual_im_helpers_route_permission_and_elicitation_answers_on_same_gateway() {
    for permission in [true, false] {
        let mock = Mock::start().await;
        let (decisions, _) = watch::channel(Vec::new());
        let (waiting, _) = watch::channel(false);
        let (answered, _) = watch::channel(false);
        let gateway = Arc::new(InteractiveGateway {
            permission,
            decisions: decisions.clone(),
            waiting: waiting.clone(),
            answered,
        });
        let interaction = Arc::new(super::super::bluebubbles::HelperInteraction::new(
            gateway.clone(),
        ));
        let channel = DingTalkChannel::new(
            DingTalkOptions {
                client_id: Some("id".into()),
                client_secret: Some("secret".into()),
                stream_endpoint: Some(mock.endpoint.clone()),
                webhook_route: Some({
                    let route = mock.route.clone();
                    Arc::new(move |_| route.clone())
                }),
                ..Default::default()
            },
            interaction,
        );
        let handle = channel
            .start(ChannelStartDeps {
                turn_gateway: Some(gateway),
                ..Default::default()
            })
            .await
            .unwrap();
        mock.send("question", "question".into(), stream::TOPIC_ROBOT);
        until(|| mock.requests.borrow().len() == 2).await;
        // Barrier is after the actual notification receipt and helper confirmation,
        // not just after a mock server received the outbound prompt.
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            waiting.subscribe().wait_for(|value| *value),
        )
        .await
        .unwrap()
        .unwrap();
        mock.send("answer", "0".into(), stream::TOPIC_ROBOT);
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            decisions.subscribe().wait_for(|rows| rows.len() == 1),
        )
        .await
        .unwrap()
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), channel.drain())
            .await
            .unwrap();
        let answer = decisions.borrow()[0].clone();
        assert_eq!(
            answer.get("requestId"),
            Some(&JsValue::from("ding-request"))
        );
        if permission {
            assert_eq!(answer.get("decision"), Some(&JsValue::from("deny")));
        } else {
            assert!(is(answer.get("answer").unwrap(), "type", "cancelled"));
        }
        assert!(mock.requests.borrow().iter().any(|row| {
            row.get("body")
                .and_then(|b| b.get("markdown"))
                .and_then(|data| data.get("text"))
                .and_then(JsValue::as_string)
                .is_some_and(|text| {
                    text.units()
                        .windows(8)
                        .any(|chunk| chunk == "ANSWERED".encode_utf16().collect::<Vec<_>>())
                })
        }));
        handle.stop("interaction-stop").await.unwrap();
    }
}

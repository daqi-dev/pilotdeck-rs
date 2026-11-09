use super::*;
use std::collections::VecDeque;
use tokio::{net::TcpListener, sync::mpsc};
use tokio_tungstenite::accept_async;
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
    let mut mapper = HomeAssistantSessionMapper::new(
        Default::default(),
        Arc::new(|| "00000000-0000-0000-0000-000000000001".into()),
    );
    for case in gold["mapper"].as_array().unwrap() {
        let text: JsString = case["text"].as_str().unwrap().into();
        let mapped = mapper.resolve(&"conversation.room".into(), &text);
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
            render::render_home_assistant_event(&event).map(|s| shown(&s)),
            case["result"].as_str().map(str::to_owned)
        );
    }
}
#[test]
fn websocket_url_preserves_base_path_but_drops_query_fragment() {
    for (input, expected) in [
        ("localhost:8123/", "ws://localhost:8123/api/websocket"),
        (
            "https://example.com/base/?x=1#f",
            "wss://example.com/base/api/websocket",
        ),
        (
            "http://[::1]:8123/base",
            "ws://[::1]:8123/base/api/websocket",
        ),
    ] {
        assert_eq!(http_to_ws(input).unwrap(), expected);
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
                    js(r#"{"type":"assistant_text_delta","text":" HA_REPLY "}"#),
                    js(r#"{"type":"tool_call_finished","ok":false,"toolName":"bash"}"#),
                ]),
            }) as Box<dyn TurnEventStream>)
        })
    }
}
struct MockHa {
    url: String,
    inbound: mpsc::UnboundedSender<JsValue>,
    requests: watch::Sender<Vec<JsValue>>,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for MockHa {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        self.task.abort();
    }
}
impl MockHa {
    async fn start() -> Self {
        Self::with_auth("ok").await
    }
    async fn with_auth(auth: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, mut inbound) = mpsc::unbounded_channel::<JsValue>();
        let (requests, _) = watch::channel(Vec::new());
        let recorded = requests.clone();
        let (stop, mut stopped) = watch::channel(false);
        let task = tokio::spawn(async move {
            loop {
                let accept = tokio::select! {biased;_=async { drop(stopped.wait_for(|s|*s).await); }=>break,x=listener.accept()=>x};
                let (socket, _) = accept.unwrap();
                let mut socket = accept_async(socket).await.unwrap();
                socket
                    .send(Message::Text(r#"{"type":"auth_required"}"#.into()))
                    .await
                    .unwrap();
                loop {
                    tokio::select! {biased;_=async { drop(stopped.wait_for(|s|*s).await); }=>return,Some(event)=inbound.recv()=>{if event.get("__disconnect")==Some(&JsValue::Bool(true)) { let _=socket.close(None).await; break; }
                    socket.send(Message::Text(stringify_json(&event).unwrap().unwrap().into())).await.unwrap();},message=socket.next()=>{
                     let Some(Ok(Message::Text(raw)))=message else{break};let row=parse_json(&raw).unwrap();recorded.send_modify(|rows|rows.push(row.clone()));
                     if is(&row,"type","auth") { match auth { "ok" => socket.send(Message::Text(r#"{"type":"auth_ok"}"#.into())).await.unwrap(), "invalid" => socket.send(Message::Text(r#"{"type":"auth_invalid","message":"owned bad token"}"#.into())).await.unwrap(), _ => {} } }
                     if is(&row,"type","call_service")&&!row.get("service_data").and_then(|v|v.get("message")).and_then(JsValue::as_string).is_some_and(|s|s.equals_str("NO_ACK")){
                       let success=!row.get("service_data").and_then(|v|v.get("message")).and_then(JsValue::as_string).is_some_and(|s|s.equals_str("FAIL_ACK"));let reply=object([("type","result".into()),("id",row.get("id").unwrap().clone()),("success",JsValue::Bool(success))]);socket.send(Message::Text(stringify_json(&reply).unwrap().unwrap().into())).await.unwrap();
                     }
                    }}
                }
            }
        });
        Self {
            url,
            inbound: tx,
            requests,
            stop,
            task,
        }
    }
    fn state(&self, entity: &str, new: &str, old: &str) {
        self.inbound.send(js(&serde_json::json!({"type":"event","event":{"event_type":"state_changed","data":{"entity_id":entity,"old_state":{"state":old},"new_state":{"state":new}}}}).to_string())).unwrap();
    }
    async fn service_count(&self, count: usize) {
        tokio::time::timeout(
            Duration::from_secs(3),
            self.requests.subscribe().wait_for(|rows| {
                rows.iter()
                    .filter(|r| is(r, "type", "call_service"))
                    .count()
                    >= count
            }),
        )
        .await
        .unwrap()
        .unwrap();
    }
}
fn options(url: &str) -> HomeAssistantOptions {
    HomeAssistantOptions {
        url: Some(url.into()),
        token: Some("owned-ha-token".into()),
        env: Some(Arc::new(|_| None)),
        mapper: HomeAssistantSessionMapper::new(
            Default::default(),
            Arc::new(|| "00000000-0000-0000-0000-000000000001".into()),
        ),
        now_ms: Arc::new(|| 1000),
        ..Default::default()
    }
}
fn cron(text: &str) -> CronResultDelivery {
    CronResultDelivery {
        task_id: "t".into(),
        run_id: "r".into(),
        session_key: "homeassistant:chat=conversation.room:general".into(),
        channel_key: ChannelKey::new("homeassistant"),
        origin_session_key: None,
        origin_channel_key: None,
        text: text.into(),
    }
}
#[tokio::test]
async fn real_ws_auth_inbound_turn_reply_cron_stop_and_restart_contract() {
    let mock = MockHa::start().await;
    let (calls, _) = watch::channel(Vec::new());
    let (gate, _) = watch::channel(false);
    let gateway = Arc::new(MockGateway {
        calls: calls.clone(),
        gate: gate.clone(),
    });
    let channel = Arc::new(HomeAssistantChannel::new(
        options(&mock.url),
        no_interaction(),
    ));
    let deps = ChannelStartDeps {
        turn_gateway: Some(gateway),
        ..Default::default()
    };
    let handle = channel.start(deps.clone()).await.unwrap();
    mock.state("sensor.ignored", "drop", "prior");
    mock.state("conversation.room", "same", "same");
    mock.state("conversation.room", "/new", "prior");
    mock.service_count(1).await;
    mock.state("conversation.room", "hello", "prior");
    tokio::time::timeout(
        Duration::from_secs(2),
        calls.subscribe().wait_for(|r| r.len() == 1),
    )
    .await
    .unwrap()
    .unwrap();
    mock.state("conversation.room", "duplicate", "prior");
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(calls.borrow().len(), 1);
    gate.send_replace(true);
    mock.service_count(2).await;
    assert!(channel.deliver_cron_result(&cron("CRON")).await);
    assert!(!channel.deliver_cron_result(&cron("FAIL_ACK")).await);
    let pending = channel.deliver_cron_result(&cron("NO_ACK"));
    let pending = tokio::spawn(pending);
    mock.service_count(5).await;
    gate.send_replace(false);
    mock.state("conversation.room", "inflight", "prior");
    tokio::time::timeout(
        Duration::from_secs(2),
        calls.subscribe().wait_for(|r| r.len() == 2),
    )
    .await
    .unwrap()
    .unwrap();
    handle.stop("owned-stop").await.unwrap();
    assert!(!pending.await.unwrap());
    assert_eq!(
        channel.active_tasks(),
        1,
        "TS stop does not drain an in-flight Gateway turn"
    );
    gate.send_replace(true);
    tokio::time::timeout(Duration::from_secs(2), channel.drain())
        .await
        .unwrap();
    let actual: Vec<_> = mock.requests.borrow().iter().map(json).collect();
    assert_eq!(serde_json::Value::Array(actual), oracle()["requests"]);
    let actual_calls:Vec<_>=calls.borrow().iter().map(|r|serde_json::json!({"sessionKey":shown(&r.session_key),"channelKey":r.channel_key.to_string(),"message":shown(&r.message)})).collect();
    assert_eq!(serde_json::Value::Array(actual_calls), oracle()["calls"]);
    // Same object restart retains mapper state and sequence, never resubmits history.
    let restarted = channel.start(deps).await.unwrap();
    assert_eq!(calls.borrow().len(), 2);
    restarted.stop("restart-stop").await.unwrap();
    channel.drain().await;
}
#[tokio::test]
async fn missing_token_is_an_observable_noop_without_connecting() {
    let logs = Arc::new(Mutex::new(Vec::new()));
    let saved = logs.clone();
    let channel = HomeAssistantChannel::new(
        HomeAssistantOptions {
            env: Some(Arc::new(|_| None)),
            ..Default::default()
        },
        no_interaction(),
    );
    let handle = channel
        .start(ChannelStartDeps {
            log: Some(Arc::new(move |level, message| {
                lock(&saved).push((level.to_owned(), message.to_owned()))
            })),
            ..Default::default()
        })
        .await
        .unwrap();
    handle.stop("missing").await.unwrap();
    assert_eq!(
        *lock(&logs),
        vec![("error".into(), "homeassistant: HASS_TOKEN not set".into())]
    );
    assert!(!channel.deliver_cron_result(&cron("no socket")).await);
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
                            r#"{"type":"permission_request","requestId":"ha-request","toolName":"bash","payload":{"command":"echo owned"}}"#,
                        )
                    } else {
                        js(
                            r#"{"type":"elicitation_request","requestId":"ha-request","questions":[{"header":"H","question":"Q","options":[{"label":"A","description":"a"},{"label":"B","description":"b"}]}]}"#,
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
        let mock = MockHa::start().await;
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
        let channel = HomeAssistantChannel::new(options(&mock.url), interaction);
        let handle = channel
            .start(ChannelStartDeps {
                turn_gateway: Some(gateway),
                ..Default::default()
            })
            .await
            .unwrap();
        mock.state("conversation.room", "question", "prior");
        mock.service_count(1).await;
        // Barrier is after the actual notification receipt and helper confirmation,
        // not just after a mock server received the outbound prompt.
        tokio::time::timeout(
            Duration::from_secs(2),
            waiting.subscribe().wait_for(|value| *value),
        )
        .await
        .unwrap()
        .unwrap();
        mock.state("conversation.room", "0", "question");
        tokio::time::timeout(
            Duration::from_secs(2),
            decisions.subscribe().wait_for(|rows| rows.len() == 1),
        )
        .await
        .unwrap()
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), channel.drain())
            .await
            .unwrap();
        let answer = decisions.borrow()[0].clone();
        assert_eq!(answer.get("requestId"), Some(&JsValue::from("ha-request")));
        if permission {
            assert_eq!(answer.get("decision"), Some(&JsValue::from("deny")));
        } else {
            assert!(is(answer.get("answer").unwrap(), "type", "cancelled"));
        }
        assert!(mock.requests.borrow().iter().any(|row| {
            row.get("service_data")
                .and_then(|data| data.get("message"))
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
#[tokio::test]
async fn stopped_channel_no_longer_accepts_and_cron_is_origin_scoped() {
    let mock = MockHa::start().await;
    let (calls, _) = watch::channel(Vec::new());
    let (gate, _) = watch::channel(true);
    let channel = HomeAssistantChannel::new(options(&mock.url), no_interaction());
    let handle = channel
        .start(ChannelStartDeps {
            turn_gateway: Some(Arc::new(MockGateway {
                calls: calls.clone(),
                gate,
            })),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut foreign = cron("wrong");
    foreign.origin_channel_key = Some(ChannelKey::new("sms"));
    assert!(!channel.deliver_cron_result(&foreign).await);
    handle.stop("stop").await.unwrap();
    assert!(!channel.deliver_cron_result(&cron("after-stop")).await);
    assert_eq!(calls.borrow().len(), 0);
    channel.drain().await;
}

#[tokio::test]
async fn invalid_auth_is_noop_and_cancelled_start_releases_socket_reader() {
    for mode in ["invalid", "silent"] {
        let mock = MockHa::with_auth(mode).await;
        let logs = Arc::new(Mutex::new(Vec::new()));
        let saved = logs.clone();
        let channel = HomeAssistantChannel::new(options(&mock.url), no_interaction());
        let start = tokio::spawn(channel.start(ChannelStartDeps {
            log: Some(Arc::new(move |level, message| {
                lock(&saved).push((level.to_owned(), message.to_owned()))
            })),
            ..Default::default()
        }));
        tokio::time::timeout(
            Duration::from_secs(2),
            mock.requests
                .subscribe()
                .wait_for(|rows| rows.iter().any(|row| is(row, "type", "auth"))),
        )
        .await
        .unwrap()
        .unwrap();
        if mode == "invalid" {
            let handle = start.await.unwrap().unwrap();
            handle.stop("noop").await.unwrap();
            assert_eq!(
                *lock(&logs),
                vec![
                    (
                        "error".into(),
                        "homeassistant: auth invalid: owned bad token".into()
                    ),
                    (
                        "error".into(),
                        "homeassistant: WebSocket auth failed or timed out".into()
                    )
                ]
            );
        } else {
            start.abort();
            match start.await {
                Err(error) => assert!(error.is_cancelled()),
                Ok(_) => panic!("start waiter was not cancelled"),
            }
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while channel.shared.writer.lock().await.is_some() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(channel.shared.closed.load(Ordering::SeqCst));
        assert_eq!(channel.active_tasks(), 0);
    }
}
#[tokio::test]
async fn previously_authenticated_close_reconnects_and_resubscribes_with_next_id() {
    let mock = MockHa::start().await;
    let channel = HomeAssistantChannel::new(options(&mock.url), no_interaction());
    let handle = channel.start(Default::default()).await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        mock.requests
            .subscribe()
            .wait_for(|rows| rows.iter().any(|row| is(row, "type", "subscribe_events"))),
    )
    .await
    .unwrap()
    .unwrap();
    mock.inbound.send(js(r#"{"__disconnect":true}"#)).unwrap();
    tokio::time::timeout(
        Duration::from_secs(8),
        mock.requests.subscribe().wait_for(|rows| {
            rows.iter()
                .filter(|row| is(row, "type", "subscribe_events"))
                .count()
                == 2
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let ids: Vec<_> = mock
        .requests
        .borrow()
        .iter()
        .filter(|row| is(row, "type", "subscribe_events"))
        .map(|row| row.get("id").unwrap().as_number().unwrap())
        .collect();
    assert_eq!(ids, vec![1.0, 2.0]);
    handle.stop("reconnect-stop").await.unwrap();
    channel.drain().await;
}

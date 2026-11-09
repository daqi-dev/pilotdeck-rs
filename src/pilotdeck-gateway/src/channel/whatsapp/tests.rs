use super::*;
use std::collections::VecDeque;
use tokio::sync::watch;

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
    let mut mapper = WhatsAppSessionMapper::new(
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
            render::render_whatsapp_event(&event).map(|s| shown(&s)),
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
                events: VecDeque::from([js(r#"{"type":"assistant_text_delta","text":" REPLY "}"#)]),
            }) as Box<dyn TurnEventStream>)
        })
    }
}
#[test]
fn normalizes_envelopes_and_aliases() {
    let rows = normalize_messages(
        &parse_json(
            r#"{"messages":[{"key":" a ","from":" room ","body":" hi "},{"text":"drop"}]}"#,
        )
        .unwrap(),
    );
    assert_eq!(
        rows,
        vec![InboundMessage {
            id: "a".into(),
            chat_id: "room".into(),
            text: "hi".into()
        }]
    );
}
async fn until(mut f: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(4), async {
        while !f() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
async fn inspect(http: &HttpClient, base: &str) -> JsValue {
    let r = http
        .request(
            "GET",
            &format!("{base}/inspect"),
            None,
            "",
            Duration::from_secs(2),
        )
        .await
        .unwrap();
    parse_json(&String::from_utf8_lossy(&r.body)).unwrap()
}
async fn sent(http: &HttpClient, base: &str, n: usize) {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if matches!(inspect(http,base).await.get("sent"),Some(JsValue::Array(v))if v.len()==n) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
async fn inject(channel: &WhatsAppChannel, base: &str, id: &str, text: &str) {
    let body = stringify_json(&JsValue::Array(vec![obj([
        ("id", id.into()),
        ("chatId", "room".into()),
        ("text", text.into()),
    ])]))
    .unwrap()
    .unwrap();
    channel
        .shared
        .http
        .request(
            "POST",
            &format!("{base}/inject"),
            Some("application/json"),
            &body,
            Duration::from_secs(2),
        )
        .await
        .unwrap();
    channel.shared.clone().poll().await;
}
#[tokio::test]
async fn owned_external_mock_bridge_to_a7_and_stop_matches_fixed_ts() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let base = format!("http://127.0.0.1:{port}");
    let (calls, _) = watch::channel(Vec::<TurnRequest>::new());
    let (gate, _) = watch::channel(false);
    let channel = WhatsAppChannel::new(
        WhatsAppOptions {
            bridge_path: Some(
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/src/channel/whatsapp/mock-bridge.mjs"
                )
                .into(),
            ),
            bridge_url: Some(format!("{base}///").into()),
            mapper: WhatsAppSessionMapper::new(
                Default::default(),
                Arc::new(|| "00000000-0000-0000-0000-000000000001".into()),
            ),
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
    inject(&channel, &base, "new", "/new").await;
    sent(&channel.shared.http, &base, 1).await;
    channel.drain().await;
    inject(&channel, &base, "first", "hello").await;
    until(|| calls.borrow().len() == 1).await;
    channel.shared.clone().poll().await;
    inject(&channel, &base, "busy", "busy").await;
    channel.shared.clone().poll().await;
    until(|| channel.active_tasks() == 1).await;
    assert_eq!(calls.borrow().len(), 1);
    gate.send_replace(true);
    channel.drain().await;
    sent(&channel.shared.http, &base, 2).await;
    let cron = CronResultDelivery {
        task_id: "t".into(),
        run_id: "r".into(),
        session_key: "whatsapp:chat=room:general".into(),
        channel_key: ChannelKey::new("whatsapp"),
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
    inject(&channel, &base, "second", "inflight").await;
    until(|| calls.borrow().len() == 2).await;
    let evidence = inspect(&channel.shared.http, &base).await;
    handle.stop("owned-stop").await.unwrap();
    assert_eq!(channel.active_tasks(), 1);
    gate.send_replace(true);
    channel.drain().await;
    assert!(!channel.deliver_cron_result(&cron).await);
    assert!(
        channel
            .shared
            .http
            .request(
                "GET",
                &format!("{base}/messages"),
                None,
                "",
                Duration::from_secs(2)
            )
            .await
            .is_err()
    );
    let gold = oracle();
    assert_eq!(json(evidence.get("sent").unwrap()), gold["sent"]);
    let turns:Vec<_>=calls.borrow().iter().map(|r|serde_json::json!({"sessionKey":shown(&r.session_key),"channelKey":"whatsapp","message":shown(&r.message)})).collect();
    assert_eq!(serde_json::json!(turns), gold["calls"]);
}
#[tokio::test]
async fn absent_bridge_path_is_distinct_from_absent_node_or_bridge_failure() {
    let logs = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = logs.clone();
    let channel = WhatsAppChannel::new(
        WhatsAppOptions {
            env: Some(Arc::new(|_| None)),
            ..Default::default()
        },
        Arc::new(super::super::im_interaction::NoInteraction),
    );
    let handle = channel
        .start(ChannelStartDeps {
            log: Some(Arc::new(move |_, message| {
                lock(&observed).push(message.into())
            })),
            ..Default::default()
        })
        .await
        .unwrap();
    handle.stop("noop").await.unwrap();
    assert_eq!(
        *lock(&logs),
        vec!["whatsapp: bridgePath / WHATSAPP_BRIDGE_PATH is required"]
    );
    let broken = WhatsAppChannel::new(
        WhatsAppOptions {
            bridge_path: Some("owned-missing-script".into()),
            node_executable: Some(PathBuf::from("/owned/no-node-executable")),
            ..Default::default()
        },
        Arc::new(super::super::im_interaction::NoInteraction),
    );
    let handle = broken.start(Default::default()).await.unwrap();
    handle.stop("no-node").await.unwrap();
    assert!(broken.shared.closed.load(Ordering::SeqCst));
}
#[test]
fn message_normalization_matches_fixed_ts_oracle() {
    for c in oracle()["normalize"].as_array().unwrap() {
        let rows = normalize_messages(&parse_json(&c["input"].to_string()).unwrap());
        let normalized:Vec<_>=rows.iter().map(|r|serde_json::json!({"id":shown(&r.id),"chatId":shown(&r.chat_id),"text":shown(&r.text)})).collect();
        assert_eq!(serde_json::json!(normalized), c["result"]);
    }
}

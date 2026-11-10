use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Default)]
struct Gateway {
    calls: Mutex<Vec<Value>>,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    hold: AtomicUsize,
}
impl ChannelGateway for Gateway {
    fn request<'a>(&'a self, _method: &'a str, _params: Value) -> ChannelCall<'a, Value> {
        Box::pin(async { Ok(json!({})) })
    }
    fn turn(&self, _params: Value) -> ChannelCall<'_, Vec<Value>> {
        Box::pin(async { panic!("HTTP owner must stream through the gateway port") })
    }
    fn stream_turn<'a>(
        &'a self,
        params: Value,
        sink: &'a dyn ChannelEventSink,
    ) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(params);
            sink.event(json!({"type":"assistant_text_delta","text":"first"}))
                .await?;
            self.entered.notify_one();
            if self.hold.load(Ordering::SeqCst) > 0 {
                self.release.notified().await;
            }
            sink.event(json!({"type":"tool_call_started","name":"read_file"}))
                .await?;
            sink.event(json!({"type":"agent_status","event":"turn_failed","detail":{"severity":"error","visible":true,"message":"visible failure"}})).await?;
            sink.event(json!({"type":"error","message":"duplicate failure"}))
                .await?;
            sink.event(json!({"type":"turn_completed"})).await?;
            Ok(())
        })
    }
}
async fn request(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> String {
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    socket.read_to_end(&mut bytes).await.unwrap();
    String::from_utf8(bytes).unwrap()
}
fn body(response: &str) -> Value {
    serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
}
fn api_options() -> ApiServerOptions {
    ApiServerOptions {
        port: 0,
        api_key: "fixture-key".into(),
        cors_origins: vec!["https://fixture.local".into()],
        ..Default::default()
    }
}

#[tokio::test]
async fn api_http_auth_cors_content_and_session_reset_match_ts() {
    let gateway = Arc::new(Gateway::default());
    let channel = ApiServerChannel::new(api_options(), gateway.clone());
    let handle = channel.start().await.unwrap();
    let address = handle.local_addr();
    let health = request(address, "DELETE", "/health", &[], "").await;
    assert_eq!(
        body(&health),
        json!({"status":"ok","platform":"api-server"})
    );
    let rejected = request(
        address,
        "POST",
        "/v1/chat/completions",
        &[("Origin", "https://fixture.local")],
        "{}",
    )
    .await;
    assert!(rejected.starts_with("HTTP/1.1 401"));
    assert!(rejected.contains("access-control-allow-origin: https://fixture.local"));
    assert_eq!(
        body(&rejected)["agent_status"]["detail"]["source"],
        "api_server"
    );
    let preflight = request(
        address,
        "OPTIONS",
        "/unknown",
        &[("Origin", "https://fixture.local")],
        "",
    )
    .await;
    assert!(preflight.starts_with("HTTP/1.1 204"));
    let headers = [
        ("Authorization", "Bearer fixture-key"),
        ("X-Hermes-Session-Id", "chat"),
    ];
    let completion=request(address,"POST","/v1/chat/completions",&headers,r#"{"messages":[{"role":"user","content":[{"type":"text","text":"hello"},"world",{"type":"image_url"}]}]}"#).await;
    assert!(completion.contains("x-hermes-session-id: chat"));
    let value = body(&completion);
    assert_eq!(value["usage"]["total_tokens"], 0);
    assert_eq!(
        value["choices"][0]["message"]["content"],
        "first\n[read_file running]\n\nError: visible failure"
    );
    assert_eq!(
        gateway.calls.lock().unwrap()[0],
        json!({"sessionKey":"api_server:chat=chat:general","channelKey":"api_server","message":"hello\nworld"})
    );
    let ack = request(
        address,
        "POST",
        "/v1/chat/completions",
        &headers,
        r#"{"messages":[{"content":"/new"}]}"#,
    )
    .await;
    assert!(!ack.contains("x-hermes-session-id:"));
    assert_eq!(
        body(&ack)["choices"][0]["message"]["content"],
        "已创建新会话。"
    );
    request(
        address,
        "POST",
        "/v1/chat/completions",
        &headers,
        r#"{"messages":[{"content":"after"}]}"#,
    )
    .await;
    assert!(
        gateway.calls.lock().unwrap()[1]["sessionKey"]
            .as_str()
            .unwrap()
            .starts_with("api_server:chat=chat:s_")
    );
    handle.close().await;
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
}

#[tokio::test]
async fn api_sse_delivers_before_turn_completes_and_rejects_busy_session() {
    let gateway = Arc::new(Gateway::default());
    gateway.hold.store(1, Ordering::SeqCst);
    let channel = ApiServerChannel::new(api_options(), gateway.clone());
    let handle = channel.start().await.unwrap();
    let address = handle.local_addr();
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let payload = r#"{"stream":true,"messages":[{"content":"go"}]}"#;
    socket.write_all(format!("POST /v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAuthorization: Bearer fixture-key\r\nX-Hermes-Session-Id: live\r\nContent-Length: {}\r\n\r\n{payload}",payload.len()).as_bytes()).await.unwrap();
    let mut received = String::new();
    let mut bytes = [0u8; 4096];
    tokio::time::timeout(Duration::from_secs(3), async {
        while !received.contains("first") {
            let count = socket.read(&mut bytes).await.unwrap();
            assert!(count > 0);
            received.push_str(std::str::from_utf8(&bytes[..count]).unwrap());
        }
    })
    .await
    .unwrap();
    assert!(received.contains("text/event-stream"));
    assert!(!received.contains("[DONE]"));
    let busy = request(
        address,
        "POST",
        "/v1/chat/completions",
        &[
            ("Authorization", "Bearer fixture-key"),
            ("X-Hermes-Session-Id", "live"),
        ],
        r#"{"messages":[{"content":"second"}]}"#,
    )
    .await;
    assert_eq!(body(&busy)["error"]["code"], "session_busy");
    gateway.release.notify_one();
    let mut rest = Vec::new();
    socket.read_to_end(&mut rest).await.unwrap();
    received.push_str(std::str::from_utf8(&rest).unwrap());
    assert!(received.contains("data: [DONE]"));
    assert!(!received.contains("duplicate failure"));
    handle.close().await;
}

#[derive(Default)]
struct Log {
    messages: Mutex<Vec<String>>,
    changed: tokio::sync::Notify,
}
impl WebhookLogger for Log {
    fn info(&self, message: &str) {
        self.messages.lock().unwrap().push(message.into());
        self.changed.notify_one();
    }
}
fn webhook_options() -> WebhookOptions {
    WebhookOptions {
        port: 0,
        secret: "fixture-secret".into(),
        routes: HashMap::from([("one".into(), json!({})), ("two".into(), json!({}))]),
        ..Default::default()
    }
}
fn signature(payload: &str) -> String {
    format!(
        "sha256={}",
        webhook::hmac(payload, "fixture-secret")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

#[tokio::test]
async fn webhook_verifies_hmac_deduplicates_across_routes_and_logs_reply() {
    let gateway = Arc::new(Gateway::default());
    let logger = Arc::new(Log::default());
    let channel = WebhookChannel::with_logger(webhook_options(), gateway.clone(), logger.clone());
    let handle = channel.start().await.unwrap();
    let address = handle.local_addr();
    let payload = r#"{"text":"hello"}"#;
    let signature = signature(payload);
    let invalid = request(address, "POST", "/webhooks/one", &[], payload).await;
    assert_eq!(body(&invalid)["error"]["code"], "invalid_signature");
    let accepted = request(
        address,
        "POST",
        "/webhooks/one",
        &[
            ("x-hub-signature-256", &signature),
            ("x-delivery-id", "delivery"),
        ],
        payload,
    )
    .await;
    assert_eq!(body(&accepted)["status"], "accepted");
    tokio::time::timeout(Duration::from_secs(3), logger.changed.notified())
        .await
        .unwrap();
    assert!(logger.messages.lock().unwrap()[0].contains("⚠️ visible failure"));
    assert!(!logger.messages.lock().unwrap()[0].contains("duplicate failure"));
    assert_eq!(
        gateway.calls.lock().unwrap()[0]["sessionKey"],
        "webhook:chat=webhook:one:delivery:general"
    );
    let duplicate = request(
        address,
        "POST",
        "/webhooks/two",
        &[
            ("x-signature-256", &signature),
            ("x-delivery-id", "delivery"),
        ],
        payload,
    )
    .await;
    assert_eq!(body(&duplicate)["status"], "duplicate");
    assert_eq!(gateway.calls.lock().unwrap().len(), 1);
    assert!(channel.deliver(&json!({"originChannelKey":"webhook","originSessionKey":"webhook:chat=webhook:one:delivery:general","text":"cron result"})).await.unwrap());
    assert!(
        logger
            .messages
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .contains("cron result")
    );
    handle.close().await;
}

#[tokio::test]
async fn production_api_rejects_unauthorized_before_gateway_stream_turn() {
    let gateway = Arc::new(Gateway::default());
    let channel = ApiServerChannel::new(api_options(), gateway.clone());
    let handle = channel.start().await.unwrap();
    let rejected = request(
        handle.local_addr(),
        "POST",
        "/v1/chat/completions",
        &[],
        r#"{"messages":[{"role":"user","content":"hello"}]}"#,
    )
    .await;
    assert!(rejected.starts_with("HTTP/1.1 401"));
    assert!(gateway.calls.lock().unwrap().is_empty());
    handle.close().await;
}

#[tokio::test]
async fn webhook_rate_limit_precedes_auth_and_payload_can_override_delivery_type() {
    let gateway = Arc::new(Gateway::default());
    let logger = Arc::new(Log::default());
    let mut options = webhook_options();
    options.rate_limit = 1;
    let channel = WebhookChannel::with_logger(options, gateway.clone(), logger.clone());
    let handle = channel.start().await.unwrap();
    let address = handle.local_addr();
    let rejected = request(address, "POST", "/webhooks/one", &[], "{}").await;
    assert!(rejected.starts_with("HTTP/1.1 401"));
    let limited = request(address, "POST", "/webhooks/one", &[], "{}").await;
    assert!(limited.starts_with("HTTP/1.1 429"));
    let payload = r#"{"text":"hello","deliver":"unsupported"}"#;
    let signature = signature(payload);
    let accepted = request(
        address,
        "POST",
        "/webhooks/two",
        &[
            ("x-hub-signature-256", &signature),
            ("x-delivery-id", "override"),
        ],
        payload,
    )
    .await;
    assert_eq!(body(&accepted)["status"], "accepted");
    tokio::time::timeout(Duration::from_secs(3), logger.changed.notified())
        .await
        .unwrap();
    assert!(logger.messages.lock().unwrap()[0].contains("deliver type 'unsupported'"));
    assert!(!channel.deliver(&json!({"originSessionKey":"webhook:chat=webhook:two:override:general","text":"cron"})).await.unwrap());
    handle.close().await;
}

#[tokio::test]
async fn production_webhook_rate_limit_signature_and_dedup_chain() {
    let gateway = Arc::new(Gateway::default());
    let mut options = webhook_options();
    options.rate_limit = 1;
    let channel = WebhookChannel::new(options, gateway.clone());
    let handle = channel.start().await.unwrap();
    let address = handle.local_addr();
    let payload = r#"{"text":"production"}"#;
    let signature = signature(payload);
    assert!(
        request(address, "POST", "/webhooks/one", &[], payload)
            .await
            .starts_with("HTTP/1.1 401")
    );
    assert!(
        request(address, "POST", "/webhooks/one", &[], payload)
            .await
            .starts_with("HTTP/1.1 429")
    );
    handle.close().await;
    let mut fresh = webhook_options();
    fresh.port = 0;
    let channel = WebhookChannel::new(fresh, gateway.clone());
    let handle = channel.start().await.unwrap();
    let address = handle.local_addr();
    let accepted = request(
        address,
        "POST",
        "/webhooks/one",
        &[
            ("x-hub-signature-256", &signature),
            ("x-delivery-id", "production-dedup"),
        ],
        payload,
    )
    .await;
    assert_eq!(body(&accepted)["status"], "accepted");
    let duplicate = request(
        address,
        "POST",
        "/webhooks/one",
        &[
            ("x-hub-signature-256", &signature),
            ("x-delivery-id", "production-dedup"),
        ],
        payload,
    )
    .await;
    assert_eq!(body(&duplicate)["status"], "duplicate");
    assert_eq!(gateway.calls.lock().unwrap().len(), 1);
    handle.close().await;
}

#[test]
fn webhook_hmac_matches_rfc4231_and_node_hex_truncation() {
    let expected = "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";
    assert!(webhook::verify_hmac(
        "what do ya want for nothing?",
        "Jefe",
        expected
    ));
    assert!(webhook::verify_hmac(
        "what do ya want for nothing?",
        "Jefe",
        &format!("{expected}z")
    ));
    assert!(!webhook::verify_hmac("different", "Jefe", expected));
}

#[tokio::test]
async fn webhook_missing_secret_does_not_listen_and_empty_text_is_ignored() {
    let gateway = Arc::new(Gateway::default());
    let mut options = webhook_options();
    options.secret.clear();
    assert!(
        WebhookChannel::new(options, gateway.clone())
            .start()
            .await
            .is_err()
    );
    let channel = WebhookChannel::new(webhook_options(), gateway.clone());
    let handle = channel.start().await.unwrap();
    let payload = r#"{"text":"","message":"must not submit"}"#;
    let signature = signature(payload);
    let ignored = request(
        handle.local_addr(),
        "POST",
        "/webhooks/one",
        &[("x-hub-signature-256", &signature)],
        payload,
    )
    .await;
    assert_eq!(body(&ignored)["status"], "ignored");
    assert!(gateway.calls.lock().unwrap().is_empty());
    handle.close().await;
}

#[tokio::test]
async fn webhook_delivery_defaults_only_for_nullish_payload_values() {
    let logger = Arc::new(Log::default());
    let channel = WebhookChannel::with_logger(
        webhook_options(),
        Arc::new(Gateway::default()),
        logger.clone(),
    );
    let handle = channel.start().await.unwrap();
    for (index, value) in [
        json!(false),
        json!(0),
        json!({}),
        json!(""),
        Value::Null,
        json!("log"),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("delivery-{index}");
        let payload = json!({"text":"reply", "deliver":value}).to_string();
        let signature = signature(&payload);
        let accepted = request(
            handle.local_addr(),
            "POST",
            "/webhooks/one",
            &[("x-hub-signature-256", &signature), ("x-delivery-id", &id)],
            &payload,
        )
        .await;
        assert_eq!(body(&accepted)["status"], "accepted");
        tokio::time::timeout(Duration::from_secs(3), logger.changed.notified())
            .await
            .unwrap();
        assert_eq!(channel.deliver(&json!({"originSessionKey":format!("webhook:chat=webhook:one:{id}:general"), "text":"reply"})).await.unwrap(), index >= 4);
        // Consume the cron reply notification before the next delivery.
        logger.changed.notified().await;
    }
    let messages = logger.messages.lock().unwrap();
    assert!(messages[0].contains("deliver type 'false'"));
    assert!(messages[4].contains("deliver type '[object Object]'"));
    drop(messages);
    handle.close().await;
}

#[tokio::test]
async fn webhook_cron_session_parser_matches_ts_hex_hyphen_pattern() {
    let channel = WebhookChannel::new(webhook_options(), Arc::new(Gateway::default()));
    assert!(channel.deliver(&json!({"originSessionKey":format!("webhook:chat=room:s_{}", "-".repeat(36)), "text":"result"})).await.unwrap());
    for suffix in ["s_abc", "s_gggggggg-gggg-gggg-gggg-gggggggggggg"] {
        assert!(!channel.deliver(&json!({"originSessionKey":format!("webhook:chat=room:{suffix}"), "text":"result"})).await.unwrap());
    }
    assert!(
        !channel
            .deliver(
                &json!({"originSessionKey":"webhook:chat=room\nother:general", "text":"result"})
            )
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn webhook_actual_http_preserves_json_order_pr_coercion_and_empty_cron_channel() {
    let gateway = Arc::new(Gateway::default());
    let logger = Arc::new(Log::default());
    let channel = WebhookChannel::with_logger(webhook_options(), gateway.clone(), logger.clone());
    let handle = channel.start().await.unwrap();
    let cases = [
        (r#"{"z":1,"a":2}"#, r#"{"z":1,"a":2}"#),
        (
            r#"{"action":"opened","pull_request":{"title":"T","body":42}}"#,
            "GitHub PR opened: T\n42",
        ),
        (
            r#"{"action":"opened","pull_request":{"title":null,"body":{"content":"nested"}}}"#,
            "GitHub PR opened: null\n[object Object]",
        ),
        (
            r#"{"action":"opened","pull_request":{"body":null}}"#,
            "GitHub PR opened: undefined",
        ),
        (
            r#"{"action":"opened","issue":{"title":null,"body":"Issue"}}"#,
            "GitHub opened: null\nIssue",
        ),
        (
            r#"{"z":1,"10":"ten","2":"two","a":2}"#,
            r#"{"2":"two","10":"ten","z":1,"a":2}"#,
        ),
    ];
    for (index, (payload, expected)) in cases.iter().enumerate() {
        let signature = signature(payload);
        let delivery = format!("ordered-{index}");
        let response = request(
            handle.local_addr(),
            "POST",
            "/webhooks/one",
            &[
                ("x-hub-signature-256", &signature),
                ("x-delivery-id", &delivery),
            ],
            payload,
        )
        .await;
        assert_eq!(body(&response)["status"], "accepted");
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let notified = logger.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if gateway.calls.lock().unwrap().len() > index {
                    return;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
        let calls = gateway.calls.lock().unwrap();
        assert_eq!(calls[index]["message"], *expected);
        assert_eq!(
            calls[index]["sessionKey"],
            format!("webhook:chat=webhook:one:{delivery}:general")
        );
    }
    // TS ImCronDelivery.ts:22:
    // `if (delivery.originChannelKey && delivery.originChannelKey !== channelKey) return false;`
    // Empty originChannelKey is falsy so it does not filter out the channel;
    // delivery continues and arrives at the target chat.
    assert!(
        channel
            .deliver(&json!({"originChannelKey":"","originSessionKey":"webhook:chat=webhook:one:ordered-0:general","text":"empty origin cron"}))
            .await
            .unwrap()
    );
    let msgs = logger.messages.lock().unwrap();
    assert!(
        msgs.iter().any(|m| m.contains("webhook: response for webhook:one:ordered-0: empty origin cron")),
        "expected logged delivery for empty origin cron, got: {msgs:?}"
    );
    drop(msgs);
    handle.close().await;
}

#[tokio::test]
async fn webhook_preserves_raw_utf16_into_the_ordered_gateway_port() {
    use crate::config::channel_commands::ChannelOrderedEventSink;
    use pilotdeck_protocol::JsValue;
    #[derive(Default)]
    struct OrderedGateway {
        calls: Mutex<Vec<JsValue>>,
        changed: tokio::sync::Notify,
    }
    impl ChannelGateway for OrderedGateway {
        fn request<'a>(&'a self, _: &'a str, _: Value) -> ChannelCall<'a, Value> {
            Box::pin(async { panic!("unexpected RPC") })
        }
        fn turn(&self, _: Value) -> ChannelCall<'_, Vec<Value>> {
            Box::pin(async { panic!("lossy gateway port") })
        }
        fn stream_turn_ordered<'a>(
            &'a self,
            params: JsValue,
            sink: &'a dyn ChannelOrderedEventSink,
        ) -> ChannelCall<'a, ()> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(params);
                // A surrogate pair split over events must combine before the
                // logger's UTF-8 output, as JavaScript string concatenation does.
                for wire in [
                    r#"{"type":"assistant_text_delta","text":"\ud83d"}"#,
                    r#"{"type":"assistant_text_delta","text":"\ude00"}"#,
                ] {
                    sink.event_ordered(pilotdeck_protocol::parse_json(wire).unwrap())
                        .await?;
                }
                self.changed.notify_one();
                Ok(())
            })
        }
    }
    let gateway = Arc::new(OrderedGateway::default());
    let channel = WebhookChannel::new(
        WebhookOptions {
            port: 0,
            secret: "__INSECURE_NO_AUTH__".into(),
            routes: HashMap::from([("raw".into(), json!({}))]),
            ..Default::default()
        },
        gateway.clone(),
    );
    let handle = channel.start().await.unwrap();
    for (index, payload) in [
        r#"{"text":" \ud800x\udc00 "}"#,
        r#"{"action":"opened","issue":{"title":"\ud801","body":"\udc01"}}"#,
    ]
    .iter()
    .enumerate()
    {
        let response = request(
            handle.local_addr(),
            "POST",
            "/webhooks/raw",
            &[("x-delivery-id", &format!("raw-{index}"))],
            payload,
        )
        .await;
        assert_eq!(body(&response)["status"], "accepted");
        tokio::time::timeout(Duration::from_secs(3), gateway.changed.notified())
            .await
            .unwrap();
    }
    let calls = gateway.calls.lock().unwrap();
    assert_eq!(
        calls[0].get("message").unwrap(),
        &pilotdeck_protocol::parse_json(r#""\ud800x\udc00""#).unwrap()
    );
    assert_eq!(
        calls[1].get("message").unwrap(),
        &pilotdeck_protocol::parse_json(r#""GitHub opened: \ud801\n\udc01""#).unwrap()
    );
    drop(calls);
    handle.close().await;
}

#[tokio::test]
async fn webhook_ordered_errors_and_delivery_override_keep_surrogate_values() {
    use pilotdeck_protocol::parse_json;
    let mut state = RenderState::default();
    let error = parse_json(r#"{"type":"error","message":"\ud800"}"#).unwrap();
    assert_eq!(
        super::webhook::ordered_status_fragment(&mut state, &error),
        Some("\nError: ".encode_utf16().chain([0xd800, 10]).collect())
    );
    let status = parse_json(r#"{"type":"agent_status","event":"turn_failed","detail":{"severity":"error","message":"\ud801"}}"#).unwrap();
    assert!(
        super::webhook::ordered_status_fragment(&mut state, &status)
            .unwrap()
            .contains(&0xd801)
    );
    assert!(super::webhook::ordered_status_fragment(&mut state, &error).is_none());
    let gateway = Arc::new(Gateway::default());
    let channel = WebhookChannel::new(
        WebhookOptions {
            port: 0,
            secret: "__INSECURE_NO_AUTH__".into(),
            routes: HashMap::from([("raw".into(), json!({}))]),
            ..Default::default()
        },
        gateway,
    );
    let handle = channel.start().await.unwrap();
    let result = request(
        handle.local_addr(),
        "POST",
        "/webhooks/raw",
        &[("x-delivery-id", "delivery-raw")],
        r#"{"text":"\ud800","deliver":"https"}"#,
    )
    .await;
    assert_eq!(body(&result)["status"], "accepted");
    assert!(!channel.deliver(&json!({"sessionKey":"webhook:chat=webhook:raw:delivery-raw:general","text":"reply"})).await.unwrap());
    handle.close().await;
}

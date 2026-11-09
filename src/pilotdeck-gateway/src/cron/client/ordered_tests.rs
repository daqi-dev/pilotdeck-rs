use super::*;

fn js(raw: &str) -> JsValue {
    pilotdeck_protocol::parse_json(raw).unwrap()
}
fn text(value: &JsValue) -> String {
    pilotdeck_protocol::stringify_json(value).unwrap().unwrap()
}
async fn incoming<S: AsyncRead + Unpin>(stream: &mut S, decoder: &mut Decoder) -> JsValue {
    loop {
        if let Some(frame) = decoder.next().unwrap() {
            if frame.opcode == 8 {
                return JsValue::Null;
            }
            assert_eq!(frame.opcode, 1);
            return js(std::str::from_utf8(&frame.payload).unwrap());
        }
        assert!(stream.read_buf(&mut decoder.buffer).await.unwrap() > 0);
    }
}
async fn outgoing<S: AsyncWrite + Unpin>(stream: &mut S, value: JsValue) {
    stream
        .write_all(&frame::server_frame(1, text(&value).as_bytes()))
        .await
        .unwrap();
}
fn response(id: &JsValue, kind: &str, value: JsValue) -> JsValue {
    let mut object = JsObject::new();
    object.insert("type".into(), "response".into());
    object.insert("id".into(), id.clone());
    object.insert("ok".into(), JsValue::Bool(kind == "result"));
    object.insert(kind.into(), value);
    JsValue::Object(object)
}

#[tokio::test]
async fn actual_websocket_ordered_api_preserves_utf16_order_errors_and_shared_notifications() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        // An actual HTTP Upgrade and masked client WS frames exercise the same
        // connection path as SDK callers, rather than a serializer-only seam.
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(stream.read_u8().await.unwrap());
        }
        assert!(
            std::str::from_utf8(&headers)
                .unwrap()
                .starts_with("GET /ws HTTP/1.1")
        );
        stream.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n").await.unwrap();
        let mut decoder = Decoder::default();
        let hello = incoming(&mut stream, &mut decoder).await;
        assert_eq!(hello.get("type").unwrap(), &JsValue::from("hello"));
        outgoing(&mut stream, js(r#"{"type":"hello_ok","protocolVersion":"1.2","serverVersion":"test","serverInfo":{"z":"\ud800","a":"\udc00"},"interactionBinding":{"raw":"\ud800"}}"#)).await;

        let request = incoming(&mut stream, &mut decoder).await;
        let params = request.get("params").unwrap();
        assert_eq!(
            text(params),
            r#"{"z":"\ud800","a":"\udc00","nested":{"b":2,"a":1}}"#
        );
        outgoing(&mut stream, js(r#"{"type":"notification","name":"models_changed","payload":{"z":"\ud800","a":"\udc00"}}"#)).await;
        outgoing(
            &mut stream,
            response(request.get("id").unwrap(), "result", params.clone()),
        )
        .await;

        let denied = incoming(&mut stream, &mut decoder).await;
        outgoing(&mut stream, response(denied.get("id").unwrap(), "error", js(r#"{"code":"SESSION_BUSY","message":"denied \ud800","details":{"z":"\udc00","a":1},"validation":{"path":"\ud800"}}"#))).await;

        let request = incoming(&mut stream, &mut decoder).await;
        // Legacy integer values must retain their original serde representation.
        let id = text(request.get("id").unwrap());
        let raw = format!(r#"{{"type":"response","id":{id},"ok":true,"result":9007199254740993}}"#);
        stream
            .write_all(&frame::server_frame(1, raw.as_bytes()))
            .await
            .unwrap();

        let request = incoming(&mut stream, &mut decoder).await;
        let id = text(request.get("id").unwrap());
        assert_eq!(
            request.get("params").unwrap().get("message"),
            Some(&js(r#""\ud800""#))
        );
        for raw in [
            format!(
                r#"{{"type":"event","id":{id},"seq":0,"event":{{"z":"\ud800","a":"\udc00"}}}}"#
            ),
            format!(r#"{{"type":"event","id":{id},"seq":0,"event":{{"type":"duplicate"}}}}"#),
            format!(r#"{{"type":"event","id":{id},"seq":1,"event":"\ud800\udc00"}}"#),
            format!(r#"{{"type":"event","id":{id},"seq":2,"event":["\ud800",{{"z":1,"a":2}}]}}"#),
            format!(
                r#"{{"type":"event","id":{id},"seq":3,"final":true,"event":{{"type":"turn_completed","raw":"\ud800"}}}}"#
            ),
        ] {
            stream
                .write_all(&frame::server_frame(1, raw.as_bytes()))
                .await
                .unwrap();
        }
        assert_eq!(incoming(&mut stream, &mut decoder).await, JsValue::Null);
    });
    let client = GatewayClient::connect(&format!("http://{address}"), "fixture-token")
        .await
        .unwrap();
    assert_eq!(
        text(client.hello_ordered().get("serverInfo").unwrap()),
        r#"{"z":"\ud800","a":"\udc00"}"#
    );
    let mut old_notifications = client.subscribe_notifications();
    let mut exact_notifications = client.subscribe_ordered_notifications();
    let params = js(r#"{"z":"\ud800","a":"\udc00","nested":{"b":2,"a":1}}"#);
    assert_eq!(
        client
            .clone()
            .request_ordered("echo", &params)
            .await
            .unwrap(),
        params
    );
    let notice = exact_notifications.recv().await.unwrap();
    assert!(notice.name.equals_str("models_changed"));
    assert_eq!(
        text(notice.payload.as_ref().unwrap()),
        r#"{"z":"\ud800","a":"\udc00"}"#
    );
    assert_eq!(
        old_notifications.recv().await.unwrap().payload.unwrap()["z"],
        "\u{fffd}"
    );
    let error = client
        .request_ordered("denied", &JsValue::Null)
        .await
        .unwrap_err();
    assert_eq!(error.code, "conflict");
    assert!(!error.retryable);
    assert_eq!(
        text(error.ordered_error.as_ref().unwrap()),
        r#"{"code":"SESSION_BUSY","message":"denied \ud800","details":{"z":"\udc00","a":1},"validation":{"path":"\ud800"}}"#
    );
    assert_eq!(
        client.request("legacy", &json!({})).await.unwrap().as_u64(),
        Some(9_007_199_254_740_993)
    );
    let mut stream = client
        .stream_ordered("submit_turn", &js(r#"{"message":"\ud800"}"#))
        .await
        .unwrap();
    assert_eq!(
        text(&stream.next().await.unwrap().unwrap()),
        r#"{"z":"\ud800","a":"\udc00","sequence":0}"#
    );
    assert_eq!(
        text(&stream.next().await.unwrap().unwrap()),
        r#"{"0":"\ud800","1":"\udc00","sequence":1}"#
    );
    assert_eq!(
        text(&stream.next().await.unwrap().unwrap()),
        r#"{"0":"\ud800","1":{"z":1,"a":2},"sequence":2}"#
    );
    assert_eq!(
        text(&stream.next().await.unwrap().unwrap()),
        r#"{"type":"turn_completed","raw":"\ud800","sequence":3}"#
    );
    assert!(stream.next().await.is_none());
    client.close().await;
    server.await.unwrap();
}

#[tokio::test]
async fn in_process_ordered_and_serde_clients_share_auth_and_preserve_raw_requests_and_events() {
    let gateway = Arc::new(super::tests::EmbeddedProbe {
        hellos: Default::default(),
    });
    let client = GatewayClient::connect_in_process(
        GatewayInProcessEndpoint {
            gateway: gateway.clone(),
            token: "fixture-token".into(),
            server_version: "test".into(),
        },
        "fixture-token",
        GatewayClientOptions {
            client_name: "sdk".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        client.hello_ordered().get("serverInfo").unwrap().get("raw"),
        Some(&js(r#""\ud800""#))
    );
    let clone = client.clone();
    let params = js(r#"{"sessionKey":"s_\ud800","z":"\udc00","a":{"b":1,"a":2}}"#);
    assert_eq!(
        clone.request_ordered("echo", &params).await.unwrap(),
        params
    );
    assert_eq!(
        client.request("echo", &json!({"legacy":1})).await.unwrap(),
        json!({"legacy":1})
    );
    let error = clone
        .request_ordered("denied_ordered", &JsValue::Null)
        .await
        .unwrap_err();
    assert_eq!(
        text(
            error
                .ordered_error
                .as_ref()
                .unwrap()
                .get("details")
                .unwrap()
        ),
        r#"{"z":"\ud800","a":"\udc00"}"#
    );
    let mut stream = clone.stream_ordered("submit_turn", &params).await.unwrap();
    let first = stream.next().await.unwrap().unwrap();
    let mut expected = params.as_object().unwrap().clone();
    expected.insert(
        "interactionBinding".into(),
        client
            .hello_ordered()
            .get("interactionBinding")
            .unwrap()
            .clone(),
    );
    assert_eq!(first.get("input"), Some(&JsValue::Object(expected)));
    assert_eq!(first.get("sequence"), Some(&JsValue::Number(0.0)));
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    assert_eq!(gateway.hellos.load(std::sync::atomic::Ordering::SeqCst), 1);
    client.close().await;
    assert_eq!(
        clone
            .request_ordered("echo", &params)
            .await
            .unwrap_err()
            .code,
        "transport_error"
    );
}

#[tokio::test]
async fn ordered_request_deadline_and_close_fail_queued_operations_on_the_same_owner() {
    let (client, _server) = super::tests::backpressured_client(Duration::from_millis(25));
    let large = JsValue::from("x".repeat(32 * 1024).as_str());
    let error = client.request_ordered("blocked", &large).await.unwrap_err();
    assert_eq!(error.code, "timeout");
    assert!(error.request_id.is_some());
    let mut stream = client
        .stream_ordered("queued", &JsValue::Null)
        .await
        .unwrap();
    timeout(Duration::from_secs(1), client.close())
        .await
        .unwrap();
    assert_eq!(
        stream.next().await.unwrap().unwrap_err().code,
        "transport_error"
    );
    assert!(stream.next().await.is_none());
}

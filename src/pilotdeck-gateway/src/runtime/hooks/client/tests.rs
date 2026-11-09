use super::*;
use crate::runtime::hooks::{
    HostedHookCallback, HostedHookCallbackMatcher, HostedHookFuture, HostedHookMatcher,
};
use http_body_util::{BodyExt, Full};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use pilotdeck_protocol::{
    JsObject, JsString, JsValue,
    frame::{self, Decoder},
    parse_json, stringify_json,
};
use serde_json::json;
use std::{sync::Mutex, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Notify, oneshot},
};

struct Callback(Arc<Mutex<Vec<JsString>>>);
impl HostedHookHandler for Callback {
    fn call(
        &self,
        event: JsString,
        _: usize,
        _: JsObject,
        _: Cancellation,
        _: JsString,
    ) -> HostedHookFuture {
        self.0.lock().unwrap().push(event);
        Box::pin(async { Ok(JsValue::Object(JsObject::new())) })
    }
}
async fn receive(stream: &mut TcpStream, decoder: &mut Decoder) -> Value {
    loop {
        if let Some(frame) = decoder.next().unwrap() {
            return serde_json::from_slice(&frame.payload).unwrap();
        }
        assert!(stream.read_buf(&mut decoder.buffer).await.unwrap() > 0);
    }
}
async fn send(stream: &mut TcpStream, value: Value) {
    stream
        .write_all(&frame::server_frame(
            1,
            &serde_json::to_vec(&value).unwrap(),
        ))
        .await
        .unwrap();
}
async fn accepted(listener: TcpListener) -> (TcpStream, Decoder) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut headers = Vec::new();
    loop {
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        headers.push(byte[0]);
        if headers.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let headers = String::from_utf8(headers).unwrap();
    let key = headers
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| key.eq_ignore_ascii_case("sec-websocket-key"))
                .map(|(_, value)| value.trim())
        })
        .unwrap();
    let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
    stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await.unwrap();
    let mut decoder = Decoder::default();
    let hello = receive(&mut stream, &mut decoder).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["token"], "fixture");
    send(&mut stream, json!({"type":"hello_ok","protocolVersion":"1.2","serverVersion":"fixture","serverInfo":{"mode":"remote","capabilities":["sdk_session_defaults"]}})).await;
    (stream, decoder)
}
async fn callback(config: &Value, event: &str) -> Value {
    let client: Client<HttpConnector, Full<bytes::Bytes>> =
        Client::builder(hyper_util::rt::TokioExecutor::new()).build_http();
    let request = hyper::Request::builder().method(hyper::Method::POST)
        .uri(format!("{}?event={event}&matcher=0", config["url"].as_str().unwrap()))
        .header(hyper::header::AUTHORIZATION, config["headers"]["authorization"].as_str().unwrap())
        .body(Full::new(bytes::Bytes::from(serde_json::to_vec(&json!({"hookEventName":event,"sessionId":"s","transcriptPath":"","cwd":"/isolated"})).unwrap()))).unwrap();
    let response = client.request(request).await.unwrap();
    assert_eq!(response.status(), hyper::StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}
async fn assert_closed(config: &Value) {
    let endpoint = url::Url::parse(config["url"].as_str().unwrap()).unwrap();
    assert!(
        TcpStream::connect(("127.0.0.1", endpoint.port().unwrap()))
            .await
            .is_err()
    );
}
async fn session(origin: &str, calls: Arc<Mutex<Vec<JsString>>>) -> HostedHookSession {
    HostedHookSession::connect(
        origin,
        "fixture",
        Default::default(),
        ["SessionStart", "SessionEnd"]
            .into_iter()
            .map(|event| {
                (
                    event.into(),
                    vec![HostedHookMatcher {
                        matcher: None,
                        timeout_seconds: None,
                    }],
                )
            })
            .collect(),
        Arc::new(Callback(calls)),
        Default::default(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn sdk_submit_hosts_callbacks_preserves_settings_and_keeps_session_end_until_eof() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("ws://{}/ws", listener.local_addr().unwrap());
    let (config_send, config_recv) = oneshot::channel();
    let end = Arc::new(Notify::new());
    let finish = end.clone();
    let server = tokio::spawn(async move {
        let (mut stream, mut decoder) = accepted(listener).await;
        let submitted = receive(&mut stream, &mut decoder).await;
        assert_eq!(submitted["method"], "submit_turn");
        assert_eq!(
            submitted["params"]["sdkSessionConfig"]["includeHookEvents"],
            true
        );
        assert_eq!(
            submitted["params"]["sdkSessionConfig"]["systemPrompt"],
            "kept"
        );
        let config = submitted["params"]["sdkSessionConfig"]["hooks"].clone();
        callback(&config, "SessionStart").await;
        config_send.send(config.clone()).unwrap();
        send(&mut stream, json!({"type":"event","id":submitted["id"],"seq":0,"final":false,"event":{"type":"turn_completed"}})).await;
        finish.notified().await;
        callback(&config, "SessionEnd").await;
        send(
            &mut stream,
            json!({"type":"response","id":submitted["id"],"ok":true,"result":{}}),
        )
        .await;
        let mut buffer = [0; 1024];
        let _ = stream.read(&mut buffer).await;
    });
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut session = session(&origin, calls.clone()).await;
    session.submit_turn(json!({"sessionKey":"s","message":"fixture","sdkSessionConfig":{"includeHookEvents":true,"systemPrompt":"kept"}})).await.unwrap();
    let config = config_recv.await.unwrap();
    assert_eq!(
        session.next().await.unwrap().unwrap()["type"],
        "turn_completed"
    );
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[JsString::from("SessionStart")]
    );
    end.notify_one();
    assert!(session.next().await.is_none());
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[JsString::from("SessionStart"), JsString::from("SessionEnd")]
    );
    assert_closed(&config).await;
    session.close().await;
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn sdk_stream_error_and_active_close_release_callback_listener() {
    for explicit_close in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("ws://{}/ws", listener.local_addr().unwrap());
        let (sent, received) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, mut decoder) = accepted(listener).await;
            let submitted = receive(&mut stream, &mut decoder).await;
            let config = submitted["params"]["sdkSessionConfig"]["hooks"].clone();
            callback(&config, "SessionStart").await;
            sent.send(config).unwrap();
            if explicit_close {
                let mut buffer = [0; 1024];
                let _ = stream.read(&mut buffer).await;
            } else {
                send(&mut stream, json!({"type":"response","id":submitted["id"],"ok":false,"error":{"code":"FIXTURE_FAILED","message":"fixture failure"}})).await;
            }
        });
        let mut session = session(&origin, Arc::new(Mutex::new(Vec::new()))).await;
        session
            .submit_turn(json!({"sessionKey":"s","message":"fixture"}))
            .await
            .unwrap();
        let config = received.await.unwrap();
        if explicit_close {
            session.close().await;
        } else {
            assert_eq!(
                session.next().await.unwrap().unwrap_err().code,
                "fixture_failed"
            );
            session.close().await;
        }
        assert_closed(&config).await;
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn callback_owner_restarts_after_eof_and_runs_each_matcher_callback_in_order() {
    struct Ordered {
        number: usize,
        calls: Arc<Mutex<Vec<usize>>>,
    }
    impl HostedHookCallback for Ordered {
        fn call(
            &self,
            input: JsObject,
            _: Option<JsString>,
            _: Cancellation,
            _: JsString,
        ) -> HostedHookFuture {
            assert_eq!(input.get("session_id"), Some(&"s".into()));
            self.calls.lock().unwrap().push(self.number);
            let number = self.number;
            Box::pin(async move {
                let mut output = JsObject::new();
                output.insert("systemMessage".into(), number.to_string().as_str().into());
                Ok(JsValue::Object(output))
            })
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("ws://{}/ws", listener.local_addr().unwrap());
    let (sent, mut received) = tokio::sync::mpsc::channel(2);
    let server = tokio::spawn(async move {
        let (mut stream, mut decoder) = accepted(listener).await;
        for _ in 0..2 {
            let submitted = receive(&mut stream, &mut decoder).await;
            assert_eq!(submitted["method"], "submit_turn");
            let config = submitted["params"]["sdkSessionConfig"]["hooks"].clone();
            assert_eq!(config["events"]["PreToolUse"], json!([{"matcher":"Bash"}]));
            assert_eq!(
                callback(&config, "PreToolUse").await,
                json!({"systemMessage":"1\n2"})
            );
            sent.send(config).await.unwrap();
            send(
                &mut stream,
                json!({"type":"response","id":submitted["id"],"ok":true,"result":{}}),
            )
            .await;
        }
        let mut buffer = [0; 1024];
        let _ = stream.read(&mut buffer).await;
    });
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut session = HostedHookSession::connect_callbacks(
        &origin,
        "fixture",
        Default::default(),
        vec![(
            "PreToolUse".into(),
            vec![HostedHookCallbackMatcher {
                matcher: Some("Bash".into()),
                timeout_seconds: None,
                callbacks: (1..=2)
                    .map(|number| {
                        Arc::new(Ordered {
                            number,
                            calls: calls.clone(),
                        }) as Arc<dyn HostedHookCallback>
                    })
                    .collect(),
            }],
        )],
        Default::default(),
    )
    .await
    .unwrap();
    let mut endpoints = Vec::new();
    for _ in 0..2 {
        session
            .submit_turn(json!({"sessionKey":"s","message":"fixture"}))
            .await
            .unwrap();
        let config = received.recv().await.unwrap();
        assert!(session.next().await.is_none());
        assert_closed(&config).await;
        endpoints.push(config);
    }
    assert_ne!(endpoints[0]["headers"], endpoints[1]["headers"]);
    assert_eq!(*calls.lock().unwrap(), vec![1, 2, 1, 2]);
    session.close().await;
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn sdk_ordered_turn_preserves_matchers_messages_events_and_error_details() {
    async fn receive_ordered(stream: &mut TcpStream, decoder: &mut Decoder) -> JsValue {
        loop {
            if let Some(frame) = decoder.next().unwrap() {
                return parse_json(std::str::from_utf8(&frame.payload).unwrap()).unwrap();
            }
            assert!(stream.read_buf(&mut decoder.buffer).await.unwrap() > 0);
        }
    }
    async fn send_ordered(stream: &mut TcpStream, value: &JsValue) {
        stream
            .write_all(&frame::server_frame(
                1,
                stringify_json(value).unwrap().unwrap().as_bytes(),
            ))
            .await
            .unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("ws://{}/ws", listener.local_addr().unwrap());
    let (sent, received) = oneshot::channel();
    let expected = parse_json(
        r#"{"type":"assistant_text_delta","text":"\ud801","\ud802":"\ud803","z":1,"a":2}"#,
    )
    .unwrap();
    let expected_server = expected.clone();
    let server = tokio::spawn(async move {
        let (mut stream, mut decoder) = accepted(listener).await;
        let submitted = receive_ordered(&mut stream, &mut decoder).await;
        let params = submitted.get("params").unwrap();
        assert_eq!(
            params.get("message"),
            Some(&parse_json(r#""\ud804""#).unwrap())
        );
        let settings = params.get("sdkSessionConfig").unwrap();
        assert_eq!(
            settings.get("systemPrompt"),
            Some(&parse_json(r#""\ud805""#).unwrap())
        );
        let config = settings.get("hooks").unwrap();
        let JsValue::Array(matchers) = config.get("events").unwrap().get("SessionStart").unwrap()
        else {
            panic!("matcher array");
        };
        assert_eq!(
            matchers[0].get("matcher"),
            Some(&parse_json(r#""\ud800""#).unwrap())
        );
        let endpoint = config
            .get("url")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        let headers = config.get("headers").unwrap();
        let authorization = headers
            .get("authorization")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        let legacy_config = json!({"url":endpoint,"headers":{"authorization":authorization}});
        callback(&legacy_config, "SessionStart").await;
        sent.send(legacy_config).unwrap();
        let id = submitted.get("id").unwrap().clone();
        let mut event = JsObject::new();
        event.insert("type".into(), "event".into());
        event.insert("id".into(), id.clone());
        event.insert("seq".into(), JsValue::Number(0.0));
        event.insert("final".into(), JsValue::Bool(false));
        event.insert("event".into(), expected_server);
        send_ordered(&mut stream, &JsValue::Object(event)).await;
        let mut response = JsObject::new();
        response.insert("type".into(), "response".into());
        response.insert("id".into(), id);
        response.insert("ok".into(), JsValue::Bool(false));
        response.insert(
            "error".into(),
            parse_json(r#"{"code":"RAW_DENIED","message":"\ud806","details":{"\ud807":"\ud808"}}"#)
                .unwrap(),
        );
        send_ordered(&mut stream, &JsValue::Object(response)).await;
        let mut buffer = [0; 1024];
        let _ = stream.read(&mut buffer).await;
    });
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut session = HostedHookSession::connect(
        &origin,
        "fixture",
        Default::default(),
        vec![(
            "SessionStart".into(),
            vec![HostedHookMatcher {
                matcher: Some(JsString::from_utf16(vec![0xd800])),
                timeout_seconds: None,
            }],
        )],
        Arc::new(Callback(calls.clone())),
        Default::default(),
    )
    .await
    .unwrap();
    session
        .submit_turn_ordered(parse_json(r#"{"sessionKey":"s","message":"\ud804","sdkSessionConfig":{"systemPrompt":"\ud805"}}"#).unwrap())
        .await
        .unwrap();
    let config = received.await.unwrap();
    // SDK transport.ts:281 projects the frame sequence onto each event.
    let mut expected = expected.as_object().unwrap().clone();
    expected.insert("sequence".into(), JsValue::Number(0.0));
    assert_eq!(
        session.next_ordered().await.unwrap().unwrap(),
        JsValue::Object(expected)
    );
    let error = session.next_ordered().await.unwrap().unwrap_err();
    assert_eq!(error.code, "raw_denied");
    assert_eq!(
        error.ordered_error.as_deref(),
        Some(
            &parse_json(
                r#"{"code":"RAW_DENIED","message":"\ud806","details":{"\ud807":"\ud808"}}"#
            )
            .unwrap()
        )
    );
    assert_eq!(*calls.lock().unwrap(), vec![JsString::from("SessionStart")]);
    assert_closed(&config).await;
    session.close().await;
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

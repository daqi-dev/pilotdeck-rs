use futures_util::{SinkExt, StreamExt};
use pilotdeck_cli::tui::transport::Client;
use pilotdeck_protocol::{JsValue, parse_json};
use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    time::{Duration, timeout},
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

fn js(value: Value) -> JsValue {
    parse_json(&value.to_string()).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn stream_permission_abort_and_disconnect_share_one_socket() {
    timeout(Duration::from_secs(8), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws", listener.local_addr().unwrap());
        let server = async {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(socket).await.unwrap();
            let hello: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            let oracle: Value = serde_json::from_str(include_str!("fixtures/tui/wire.json")).unwrap();
            assert_eq!(hello, oracle["rows"][0]["frame"]);
            ws.send(Message::Text(json!({"type":"hello_ok"}).to_string().into())).await.unwrap();
            let submit: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            let recorded = &oracle["rows"][2]["frame"];
            assert_eq!(submit["method"], recorded["method"]); assert_eq!(submit["params"], recorded["params"]);
            let id = submit["id"].as_str().unwrap();
            ws.send(Message::Text(json!({"type":"event","id":id,"final":false,"event":{"type":"permission_request","requestId":"p1","toolName":"bash","payload":{"command":"git status"}}}).to_string().into())).await.unwrap();
            let permission: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            let recorded = &oracle["rows"][4]["frame"];
            assert_eq!(permission["method"], recorded["method"]); assert_eq!(permission["params"], recorded["params"]);
            ws.send(Message::Text(json!({"type":"response","id":permission["id"],"ok":true,"result":{"delivered":true}}).to_string().into())).await.unwrap();
            let abort: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            let recorded = &oracle["rows"][6]["frame"];
            assert_eq!(abort["method"], recorded["method"]); assert_eq!(abort["params"], recorded["params"]);
            ws.send(Message::Text(json!({"type":"response","id":abort["id"],"ok":true,"result":{"ok":true}}).to_string().into())).await.unwrap();
            ws.send(Message::Text(json!({"type":"event","id":id,"final":true,"event":{"type":"turn_completed"}}).to_string().into())).await.unwrap();
            let waiting: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(waiting["method"], "list_sessions");
            ws.close(None).await.unwrap();
        };
        let client = async {
            let client = Client::connect(&url, "fixture-token").await.unwrap();
            let mut stream = client.stream("submit_turn", js(json!({"sessionKey":"fixture-session","channelKey":"tui","projectKey":"/fixture-project","message":"hello","mode":"default"}))).await.unwrap();
            let event = stream.next().await.unwrap().unwrap();
            assert!(event.get("type").unwrap().as_string().unwrap().equals_str("permission_request"));
            let reply = client.request("permission_decide", js(json!({"sessionKey":"fixture-session","requestId":"p1","decision":"allow","remember":false}))).await.unwrap();
            assert_eq!(reply.get("delivered").unwrap().as_bool(), Some(true));
            client.request("abort_turn", js(json!({"sessionKey":"fixture-session"}))).await.unwrap();
            assert!(stream.next().await.is_none(), "final event is not delivered by TS stream");
            assert!(client.request("list_sessions", js(json!({}))).await.is_err());
        };
        tokio::join!(server, client);
    }).await.expect("bounded interleaving");
}

#[tokio::test(flavor = "current_thread")]
async fn reversed_responses_idle_ping_and_all_waiters_on_disconnect() {
    timeout(Duration::from_secs(8), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws", listener.local_addr().unwrap());
        let (pong_ready, pong_received) = tokio::sync::oneshot::channel();
        let server = async {
            let (socket, _) = listener.accept().await.unwrap(); let mut ws = accept_async(socket).await.unwrap();
            let _ = ws.next().await.unwrap().unwrap();
            ws.send(Message::Text(json!({"type":"hello_ok"}).to_string().into())).await.unwrap();
            ws.send(Message::Ping(vec![7].into())).await.unwrap();
            assert_eq!(ws.next().await.unwrap().unwrap(), Message::Pong(vec![7].into()));
            pong_ready.send(()).unwrap();
            let first: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            let second: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            for request in [&second, &first] {
                ws.send(Message::Text(json!({"type":"response","id":request["id"],"ok":true,"result":request["method"]}).to_string().into())).await.unwrap();
            }
            for _ in 0..2 { let _ = ws.next().await.unwrap().unwrap(); }
            ws.close(None).await.unwrap();
        };
        let client = async {
            let c = Client::connect(&url,"fixture-token").await.unwrap();
            pong_received.await.unwrap();
            let (a,b) = tokio::join!(c.request("a", js(json!({}))), c.request("b", js(json!({}))));
            assert!(a.unwrap().as_string().unwrap().equals_str("a")); assert!(b.unwrap().as_string().unwrap().equals_str("b"));
            let (a,b) = tokio::join!(c.request("pending-a",js(json!({}))), c.request("pending-b",js(json!({}))));
            assert!(a.is_err() && b.is_err());
        };
        tokio::join!(server,client);
    }).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn overflowing_stream_fails_explicitly_and_dropped_stream_does_not_poison_connection() {
    timeout(Duration::from_secs(8), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws", listener.local_addr().unwrap());
        let server = async {
            let (socket, _) = listener.accept().await.unwrap(); let mut ws = accept_async(socket).await.unwrap();
            let _ = ws.next().await.unwrap().unwrap(); ws.send(Message::Text(json!({"type":"hello_ok"}).to_string().into())).await.unwrap();
            let drop_request: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            let go: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            ws.send(Message::Text(json!({"type":"event","id":drop_request["id"],"final":false,"event":{"type":"ignored"}}).to_string().into())).await.unwrap();
            ws.send(Message::Text(json!({"type":"response","id":go["id"],"ok":true,"result":true}).to_string().into())).await.unwrap();
            let stream: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            for n in 0..65 {
                ws.send(Message::Text(json!({"type":"event","id":stream["id"],"final":false,"event":{"type":"permission_request","requestId":n}}).to_string().into())).await.unwrap();
            }
            // The read pump must fail rather than silently discard the 65th event.
            let _ = ws.next().await;
        };
        let client = async {
            let c = Client::connect(&url,"fixture-token").await.unwrap();
            drop(c.stream("submit_turn",js(json!({}))).await.unwrap());
            assert_eq!(c.request("barrier",js(json!({}))).await.unwrap().as_bool(),Some(true));
            let mut stream = c.stream("submit_turn",js(json!({}))).await.unwrap();
            tokio::time::sleep(Duration::from_millis(150)).await;
            let mut count = 0; loop { match stream.next().await {
                Some(Ok(_)) => count += 1,
                Some(Err(pilotdeck_cli::tui::transport::Error::ResourceLimit)) => break,
                _ => panic!("overflow must surface an explicit error"),
            }}
            assert_eq!(count,64);
        };
        tokio::join!(server,client);
    }).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn admission_error_ends_stream_and_cancelled_sent_requests_release_slots() {
    timeout(Duration::from_secs(15), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws", listener.local_addr().unwrap());
        let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel();
        let server = async {
            let (socket, _) = listener.accept().await.unwrap(); let mut ws = accept_async(socket).await.unwrap();
            let _ = ws.next().await.unwrap().unwrap(); ws.send(Message::Text(json!({"type":"hello_ok"}).to_string().into())).await.unwrap();
            let stream: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            ws.send(Message::Text(json!({"type":"response","id":stream["id"],"ok":false,"error":{"code":"invalid_request","message":"fixture admission denied"}}).to_string().into())).await.unwrap();
            for n in 0..70 {
                let request: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
                assert_eq!(request["method"], "never-reply");
                seen_tx.send(n).unwrap();
            }
            let barrier: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(barrier["method"], "barrier");
            ws.send(Message::Text(json!({"type":"response","id":barrier["id"],"ok":true,"result":true}).to_string().into())).await.unwrap();
            let _ = ws.next().await;
        };
        let client = async {
            let c = Client::connect(&url,"fixture-token").await.unwrap();
            let mut stream = c.stream("submit_turn",js(json!({}))).await.unwrap();
            let error = stream.next().await.unwrap().unwrap_err();
            match error { pilotdeck_cli::tui::transport::Error::Gateway(envelope) => assert!(envelope.get("message").unwrap().as_string().unwrap().equals_str("fixture admission denied")), _ => panic!("admission failure must surface gateway error") }
            assert!(stream.next().await.is_none());
            for n in 0..70 {
                let other = c.clone(); let task = tokio::spawn(async move { other.request("never-reply",js(json!({}))).await });
                assert_eq!(seen_rx.recv().await,Some(n)); task.abort(); let _ = task.await;
            }
            assert_eq!(c.request("barrier",js(json!({}))).await.unwrap().as_bool(),Some(true));
            c.close().await.unwrap();
        };
        tokio::join!(server,client);
    }).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn saturated_pending_rejects_only_new_work_and_admits_after_cancellation() {
    timeout(Duration::from_secs(8), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws", listener.local_addr().unwrap());
        let (seen, mut received) = tokio::sync::mpsc::channel(1);
        let server = async {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(socket).await.unwrap();
            let _ = ws.next().await.unwrap().unwrap();
            ws.send(Message::Text(json!({"type":"hello_ok"}).to_string().into()))
                .await
                .unwrap();
            let mut requests = Vec::new();
            for n in 0..64 {
                let request: Value =
                    serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap())
                        .unwrap();
                assert_eq!(request["method"], "held");
                requests.push(request);
                seen.send(n).await.unwrap();
            }
            // The rejected 65th request must never reach the peer or close it.
            let replacement: Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(replacement["method"], "replacement");
            ws.send(Message::Text(
                json!({"type":"response","id":replacement["id"],"ok":true,"result":true})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            for request in requests.iter().skip(1) {
                ws.send(Message::Text(
                    json!({"type":"response","id":request["id"],"ok":true,"result":true})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            }
            assert!(matches!(ws.next().await, Some(Ok(Message::Close(_)))));
        };
        let client = async {
            let c = Client::connect(&url, "fixture-token").await.unwrap();
            let mut held = Vec::new();
            for n in 0..64 {
                let other = c.clone();
                held.push(tokio::spawn(async move {
                    other.request("held", js(json!({}))).await
                }));
                assert_eq!(received.recv().await, Some(n));
            }
            assert!(matches!(
                c.request("rejected", js(json!({}))).await,
                Err(pilotdeck_cli::tui::transport::Error::ResourceLimit)
            ));
            let cancelled = held.remove(0);
            cancelled.abort();
            assert!(cancelled.await.unwrap_err().is_cancelled());
            assert_eq!(
                c.request("replacement", js(json!({})))
                    .await
                    .unwrap()
                    .as_bool(),
                Some(true)
            );
            for task in held {
                assert_eq!(task.await.unwrap().unwrap().as_bool(), Some(true));
            }
            c.close().await.unwrap();
        };
        tokio::join!(server, client);
    })
    .await
    .expect("bounded saturated admission");
}

#[tokio::test(flavor = "current_thread")]
async fn unconsumed_stream_errors_keep_their_shared_byte_budget() {
    timeout(Duration::from_secs(8), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws", listener.local_addr().unwrap());
        let server = async {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(socket).await.unwrap();
            let _ = ws.next().await.unwrap().unwrap();
            ws.send(Message::Text(json!({"type":"hello_ok"}).to_string().into())).await.unwrap();
            for n in 0..4 {
                let request: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
                assert_eq!(request["method"], "submit_turn");
                ws.send(Message::Text(json!({"type":"response","id":request["id"],"ok":false,"error":{"message":"x".repeat(1024*1024)}}).to_string().into())).await.unwrap();
                if n < 3 {
                    let barrier: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
                    assert_eq!(barrier["method"], "barrier");
                    ws.send(Message::Text(json!({"type":"response","id":barrier["id"],"ok":true,"result":true}).to_string().into())).await.unwrap();
                }
            }
            let _ = ws.next().await;
        };
        let client = async {
            let c = Client::connect(&url, "fixture-token").await.unwrap();
            let mut held = Vec::new();
            for _ in 0..3 {
                held.push(c.stream("submit_turn", js(json!({}))).await.unwrap());
                c.request("barrier", js(json!({}))).await.unwrap();
            }
            let mut overflow = c.stream("submit_turn", js(json!({}))).await.unwrap();
            assert!(matches!(overflow.next().await, Some(Err(pilotdeck_cli::tui::transport::Error::ResourceLimit))));
            for mut stream in held {
                assert!(matches!(stream.next().await, Some(Err(pilotdeck_cli::tui::transport::Error::Gateway(_)))));
                assert!(stream.next().await.is_none());
            }
        };
        tokio::join!(server, client);
    }).await.expect("bounded terminal error memory");
}

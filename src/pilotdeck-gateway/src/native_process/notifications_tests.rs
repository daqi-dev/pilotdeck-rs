//! Actual native host and TCP WebSocket notification contract.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn send(ws: &mut Socket, value: Value) {
    ws.send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}
async fn frame(ws: &mut Socket) -> Value {
    let item = tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(item.into_text().unwrap().as_ref()).unwrap()
}
async fn hello(ws: &mut Socket) {
    send(
        ws,
        json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,
        "token":"token","clientName":"web","clientVersion":"notification-test"}),
    )
    .await;
    let reply = frame(ws).await;
    assert_eq!(reply["type"], "hello_ok", "{reply:?}");
}

#[tokio::test]
async fn actual_server_settings_broadcast_precedes_response_and_skips_unauthenticated() {
    let (_scratch, requests) = super::management_tests::setup();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    let options = super::channel_lifecycle_tests::real_options(&requests.options);
    let host = super::assembly::start(options, requests.env.clone())
        .await
        .unwrap();
    let server = crate::native_ws::NativeWsServer::bind(
        "127.0.0.1:0".parse().unwrap(),
        host,
        "token".into(),
        "test".into(),
    )
    .await
    .unwrap();
    let address = server.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(server.run(
        async {
            let _ = stop_rx.await;
        },
        Duration::from_secs(3),
    ));
    let url = format!("ws://{address}/ws");
    let (mut a, _) = connect_async(&url).await.unwrap();
    let (mut b, _) = connect_async(&url).await.unwrap();
    let (mut unauthenticated, _) = connect_async(&url).await.unwrap();
    hello(&mut a).await;
    hello(&mut b).await;
    send(
        &mut a,
        json!({"type":"request","id":"first","method":"update_settings",
        "params":{"source":"localSettings","settings":{"agent":{"maxContextTokens":100000}}}}),
    )
    .await;
    let first = frame(&mut a).await;
    let response = frame(&mut a).await;
    let other = frame(&mut b).await;
    assert_eq!(first["type"], "notification", "{first:?}");
    assert_eq!(first["name"], "config_changed", "{first:?}");
    assert_eq!(
        first["payload"]["changedPaths"],
        json!(["agent.maxContextTokens"])
    );
    assert_eq!(response["id"], "first", "{response:?}");
    assert_eq!(response["ok"], true, "{response:?}");
    assert_eq!(other, first);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), unauthenticated.next())
            .await
            .is_err()
    );
    b.close(None).await.unwrap();
    // Wait until the native close handshake has reached the server.
    while let Some(item) = tokio::time::timeout(Duration::from_secs(3), b.next())
        .await
        .unwrap()
    {
        if item.unwrap().is_close() {
            break;
        }
    }
    send(
        &mut a,
        json!({"type":"request","id":"second","method":"update_settings",
        "params":{"source":"localSettings","settings":{"agent":{"maxContextTokens":110000}}}}),
    )
    .await;
    let change = frame(&mut a).await;
    let response = frame(&mut a).await;
    assert_eq!(change["name"], "config_changed", "{change:?}");
    assert_eq!(response["id"], "second", "{response:?}");
    assert_eq!(response["ok"], true, "{response:?}");
    assert!(
        tokio::time::timeout(Duration::from_millis(150), unauthenticated.next())
            .await
            .is_err()
    );
    a.close(None).await.unwrap();
    unauthenticated.close(None).await.unwrap();
    drop(a);
    drop(b);
    drop(unauthenticated);
    let _ = stop_tx.send(());
    task.await.unwrap().unwrap();
}

// The provider is a loopback HTTP server; requests and bodies are retained so
// the test proves that the real sidecar called the model and consumed an answer.
async fn mock_model() -> (
    String,
    std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = requests.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let observed = observed.clone();
            tokio::spawn(async move {
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if socket.read_exact(&mut byte).await.is_err() {
                        return;
                    }
                    header.push(byte[0]);
                    assert!(header.len() < 65536);
                }
                let header = String::from_utf8(header).unwrap();
                let length = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|s| s.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut raw = vec![0; length];
                if socket.read_exact(&mut raw).await.is_err() {
                    return;
                }
                let body: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
                observed.lock().unwrap().push(body.clone());
                let messages = body["messages"].to_string();
                let dialog = messages.contains("NOTIFICATION_DIALOG");
                let answered = messages.contains("\"role\":\"tool\"");
                let (mime, output) = if body["stream"] == true {
                    let delta = if dialog && !answered {
                        json!({"role":"assistant","tool_calls":[{"index":0,"id":"dialog-call","type":"function","function":{"name":"request_user_input","arguments":"{\"prompt\":\"Notification test?\"}"}}]})
                    } else {
                        json!({"role":"assistant","content":"MODEL_NOTIFICATION_DONE"})
                    };
                    let reason = if dialog && !answered {
                        "tool_calls"
                    } else {
                        "stop"
                    };
                    (
                        "text/event-stream",
                        format!(
                            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                            json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
                            json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]})
                        ),
                    )
                } else {
                    ("application/json", json!({"choices":[{"message":{"role":"assistant","content":"{\"title\":\"Notification\"}"},"finish_reason":"stop"}]}).to_string())
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{output}",
                    output.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    (origin, requests, task)
}

async fn until_named(ws: &mut Socket, name: &str) -> Value {
    loop {
        let value = frame(ws).await;
        if value["name"] == name {
            return value;
        }
    }
}
async fn until_response(ws: &mut Socket, id: &str) -> Value {
    loop {
        let value = frame(ws).await;
        if value["id"] == id && value["type"] == "response" {
            assert_eq!(value["ok"], true, "{value:?}");
            return value;
        }
    }
}

#[tokio::test]
async fn real_server_cron_model_emits_turn_notification() {
    let (origin, requests, provider_task) = mock_model().await;
    let (_scratch, setup) = super::management_tests::setup();
    setup.project.dispose().await.unwrap();
    setup.owner.shutdown().await.unwrap();
    let options = super::channel_lifecycle_tests::real_options(&setup.options);
    let config = json!({"schemaVersion":1,"agent":{"model":"test/test"},"model":{"providers":{"test":{"protocol":"openai","url":format!("{origin}/v1"),"apiKey":"test-only","models":{"test":{}}}}},"cron":{"enabled":true}});
    std::fs::write(options.home.join("pilotdeck.yaml"), config.to_string()).unwrap();
    let project = options.project.to_string_lossy().to_string();
    let host = super::assembly::start(options, setup.env.clone())
        .await
        .unwrap();
    let server = crate::native_ws::NativeWsServer::bind(
        "127.0.0.1:0".parse().unwrap(),
        host,
        "token".into(),
        "test".into(),
    )
    .await
    .unwrap();
    let url = format!("ws://{}/ws", server.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(server.run(
        async {
            let _ = stopped.await;
        },
        Duration::from_secs(3),
    ));
    let (mut client, _) = connect_async(&url).await.unwrap();
    let (mut observer, _) = connect_async(&url).await.unwrap();
    hello(&mut client).await;
    hello(&mut observer).await;
    send(&mut client, json!({"type":"request","id":"create","method":"cron_create","params":{"projectKey":project,"message":"NOTIFICATION_CRON","sessionKey":"web:notification-cron","channelKey":"web","mode":"default","schedule":{"type":"once","runAt":"2099-01-01T00:00:00.000Z"}}})).await;
    let created = until_response(&mut client, "create").await;
    let task_id = created["result"]["task"]["taskId"].as_str().unwrap();
    send(&mut client, json!({"type":"request","id":"run","method":"cron_run_now","params":{"projectKey":project,"taskId":task_id}})).await;
    let started = until_response(&mut client, "run").await;
    assert_eq!(started["result"]["started"], true);
    let run_task = started["result"]["taskId"].as_str().unwrap();
    let event = until_named(&mut observer, "always-on:turn-event").await;
    assert_eq!(event["payload"]["sessionKey"], format!("cron:{run_task}"));
    assert_eq!(event["payload"]["channelKey"], "cron");
    assert!(event["payload"]["event"].is_object(), "{event:?}");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if requests
                .lock()
                .unwrap()
                .iter()
                .any(|body| body.to_string().contains("NOTIFICATION_CRON"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cron must reach the model");
    client.close(None).await.unwrap();
    observer.close(None).await.unwrap();
    drop(client);
    drop(observer);
    let _ = stop.send(());
    task.await.unwrap().unwrap();
    provider_task.abort();
}

#[tokio::test]
async fn real_server_model_dialog_requested_and_answered_are_broadcast() {
    let (origin, requests, provider_task) = mock_model().await;
    let (_scratch, setup) = super::management_tests::setup();
    setup.project.dispose().await.unwrap();
    setup.owner.shutdown().await.unwrap();
    let options = super::channel_lifecycle_tests::real_options(&setup.options);
    let config = json!({"schemaVersion":1,"agent":{"model":"test/test"},"model":{"providers":{"test":{"protocol":"openai","url":format!("{origin}/v1"),"apiKey":"test-only","models":{"test":{}}}}}});
    std::fs::write(options.home.join("pilotdeck.yaml"), config.to_string()).unwrap();
    let project = options.project.to_string_lossy().to_string();
    let host = super::assembly::start(options, setup.env.clone())
        .await
        .unwrap();
    let server = crate::native_ws::NativeWsServer::bind(
        "127.0.0.1:0".parse().unwrap(),
        host,
        "token".into(),
        "test".into(),
    )
    .await
    .unwrap();
    let url = format!("ws://{}/ws", server.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(server.run(
        async {
            let _ = stopped.await;
        },
        Duration::from_secs(3),
    ));
    let (mut client, _) = connect_async(&url).await.unwrap();
    let (mut observer, _) = connect_async(&url).await.unwrap();
    hello(&mut client).await;
    hello(&mut observer).await;
    let session = "web:notification-dialog";
    send(&mut client, json!({"type":"request","id":"turn","method":"submit_turn","params":{"sessionKey":session,"runId":"notification-dialog-run","channelKey":"web","projectKey":project,"message":"NOTIFICATION_DIALOG","sdkSessionConfig":{"userDialogKinds":["input"]},"maxTurns":4}})).await;
    let requested = until_named(&mut observer, "user_dialog_changed").await;
    assert_eq!(requested["payload"]["type"], "requested", "{requested:?}");
    assert_eq!(requested["payload"]["sessionKey"], session);
    let request_id = requested["payload"]["request"]["requestId"]
        .as_str()
        .unwrap()
        .to_owned();
    send(&mut observer, json!({"type":"request","id":"answer","method":"user_dialog_respond","params":{"sessionKey":session,"requestId":request_id,"result":{"behavior":"answered","value":"notification-answer"}}})).await;
    let client_requested = until_named(&mut client, "user_dialog_changed").await;
    assert_eq!(
        client_requested["payload"]["type"], "requested",
        "{client_requested:?}"
    );
    assert_eq!(client_requested["payload"]["sessionKey"], session);
    assert_eq!(
        client_requested["payload"]["request"]["requestId"],
        request_id
    );
    let settled = until_named(&mut client, "user_dialog_changed").await;
    assert_eq!(settled["payload"]["type"], "settled", "{settled:?}");
    assert_eq!(settled["payload"]["requestId"], request_id);
    let answer = until_response(&mut observer, "answer").await;
    assert_eq!(answer["result"]["delivered"], true);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if requests
                .lock()
                .unwrap()
                .iter()
                .any(|body| body.to_string().contains("notification-answer"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the model must receive the actual dialog answer");
    client.close(None).await.unwrap();
    observer.close(None).await.unwrap();
    drop(client);
    drop(observer);
    let _ = stop.send(());
    task.await.unwrap().unwrap();
    provider_task.abort();
}

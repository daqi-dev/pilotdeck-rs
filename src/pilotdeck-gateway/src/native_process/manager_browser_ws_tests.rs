//! Real native Server assembly + TCP WS; only browser host provider is fake.
use crate::runtime::manager::{ManagerBrowserPort, ManagerError};
use futures_util::{SinkExt, StreamExt};
use pilotdeck_protocol::JsValue;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

struct Provider(Arc<Mutex<Vec<JsValue>>>);
impl ManagerBrowserPort for Provider {
    fn list(&self, input: &JsValue) -> Result<JsValue, ManagerError> {
        self.0.lock().unwrap().push(input.clone());
        Ok(
            pilotdeck_protocol::parse_json(r#"{"items":[{"id":"browser-1","extra":[1,null]}]}"#)
                .unwrap(),
        )
    }
}
type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
async fn send(ws: &mut Ws, value: Value) {
    ws.send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}
async fn frame(ws: &mut Ws) -> Value {
    serde_json::from_str(
        tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap()
            .as_str(),
    )
    .unwrap()
}
async fn exercise(configured: bool) {
    let (_scratch, requests) = super::management_tests::setup();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    let mut options = requests.options.clone();
    options.sidecar = std::env::var_os("PDRS_CHANNEL_SIDECAR")
        .map(Into::into)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("pilotdeck-agent-loop-sidecar")
        });
    assert!(std::fs::metadata(&options.sidecar).unwrap().len() > 1024);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider =
        configured.then(|| Arc::new(Provider(seen.clone())) as Arc<dyn ManagerBrowserPort>);
    let host = super::assembly::start_with_manager_browser(options, requests.env.clone(), provider)
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
    send(&mut client, json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"browser-test"})).await;
    let hello = frame(&mut client).await;
    assert_eq!(hello["type"], "hello_ok");
    let capabilities = hello["serverInfo"]["capabilities"].as_array().unwrap();
    assert_eq!(
        capabilities.iter().any(|x| x == "manager_browsers"),
        configured
    );
    let input = json!({"projectKey":"/outside","sessionKey":"s","other":[1,null]});
    for (id, params) in [("foreign", input.clone()), ("null", Value::Null)] {
        send(
            &mut client,
            json!({"type":"request","id":id,"method":"manager_browsers","params":params}),
        )
        .await;
        let response = frame(&mut client).await;
        assert_eq!(response["id"], id);
        if configured {
            assert_eq!(response["ok"], true, "{response:?}");
            assert_eq!(
                response["result"],
                json!({"items":[{"id":"browser-1","extra":[1,null]}]})
            );
        } else {
            assert_eq!(response["error"]["code"], "CAPABILITY_UNAVAILABLE");
            assert_eq!(
                response["error"]["message"],
                "Gateway manager browser provider is unavailable."
            );
        }
    }
    if configured {
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                pilotdeck_protocol::parse_json(&input.to_string()).unwrap(),
                JsValue::Null
            ]
        );
    } else {
        assert!(seen.lock().unwrap().is_empty());
    }
    client.close(None).await.unwrap();
    drop(client);
    let _ = stop.send(());
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn actual_server_configured_browser_preserves_raw_values_and_advertises() {
    exercise(true).await;
}
#[tokio::test]
async fn actual_server_unconfigured_browser_preserves_first_error_and_omits_capability() {
    exercise(false).await;
}

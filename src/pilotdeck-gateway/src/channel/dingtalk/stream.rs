//! Native subset of the official dingtalk Stream transport used by this adapter.
//! EVENT listener returns SUCCESS immediately; business turns run independently.
use super::{obj, shown};
use crate::channel::protocol::ChannelError;
use futures_util::{SinkExt, StreamExt};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use std::{sync::Arc, time::Duration};
use tokio::{net::TcpStream, sync::watch};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
pub const GATEWAY_URL: &str = "https://api.dingtalk.com/v1.0/gateway/connections/open";
pub const TOPIC_ROBOT: &str = "/v1.0/im/bot/messages/get";
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub(crate) async fn connect(url: &str, id: &str, secret: &str) -> Result<Socket, ChannelError> {
    let body = obj([
        ("clientId", id.into()),
        ("clientSecret", secret.into()),
        ("ua", "".into()),
        (
            "subscriptions",
            JsValue::Array(vec![obj([("type", "EVENT".into()), ("topic", "*".into())])]),
        ),
    ]);
    let body = stringify_json(&body)
        .map_err(|e| ChannelError::error(e.to_string()))?
        .ok_or_else(|| ChannelError::error("missing endpoint request"))?;
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::Request;
    use hyper_util::{client::legacy::Client, rt::TokioExecutor};
    let config = crate::runtime::model::http::webpki_client_config();
    let client: super::super::http_client::OutboundClient = Client::builder(TokioExecutor::new())
        .build(crate::runtime::model::http::https_connector(config));
    let request = Request::builder()
        .method("POST")
        .uri(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(Full::new(Bytes::from(body)))
        .map_err(|e| ChannelError::error(e.to_string()))?;
    let response = tokio::time::timeout(Duration::from_secs(10), async {
        let r = client
            .request(request)
            .await
            .map_err(|e| ChannelError::error(e.to_string()))?;
        if !r.status().is_success() {
            return Err(ChannelError::error(format!(
                "get endpoint HTTP {}",
                r.status()
            )));
        }
        r.into_body()
            .collect()
            .await
            .map(|b| b.to_bytes())
            .map_err(|e| ChannelError::error(e.to_string()))
    })
    .await
    .map_err(|_| ChannelError::error("endpoint request timed out"))??;
    let data = parse_json(&String::from_utf8_lossy(&response))
        .map_err(|e| ChannelError::error(e.to_string()))?;
    let endpoint = data
        .get("endpoint")
        .and_then(JsValue::as_string)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ChannelError::error("endpoint or ticket is null"))?;
    let ticket = data
        .get("ticket")
        .and_then(JsValue::as_string)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ChannelError::error("endpoint or ticket is null"))?;
    let url = format!("{}?ticket={}", shown(endpoint), shown(ticket));
    tokio::time::timeout(Duration::from_secs(10), connect_async(url))
        .await
        .map_err(|_| ChannelError::error("WebSocket connect timed out"))?
        .map(|r| r.0)
        .map_err(|e| ChannelError::error(e.to_string()))
}
pub(crate) async fn run(
    mut socket: Socket,
    endpoint: String,
    id: String,
    secret: String,
    mut stopped: watch::Receiver<bool>,
    receive: Arc<dyn Fn(JsValue) + Send + Sync>,
) {
    let mut attempt = 0u32;
    loop {
        loop {
            let frame = tokio::select! {biased;_=async { drop(stopped.wait_for(|s|*s).await); }=>{let _=tokio::time::timeout(Duration::from_secs(1),socket.close(None)).await;return},f=socket.next()=>f};
            let raw = match frame {
                Some(Ok(Message::Text(s))) => s.to_string(),
                Some(Ok(Message::Binary(s))) => String::from_utf8_lossy(&s).into_owned(),
                Some(Ok(Message::Ping(s))) => {
                    let _ = socket.send(Message::Pong(s)).await;
                    continue;
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => continue,
            };
            let Ok(v) = parse_json(&raw) else { continue };
            let kind = v.get("type").and_then(JsValue::as_string);
            if kind.is_some_and(|s| s.equals_str("EVENT")) {
                receive(v.clone());
                let ack = obj([
                    ("code", JsValue::Number(200.0)),
                    (
                        "headers",
                        obj([
                            ("contentType", "application/json".into()),
                            (
                                "messageId",
                                v.get("headers")
                                    .and_then(|h| h.get("messageId"))
                                    .cloned()
                                    .unwrap_or(JsValue::Undefined),
                            ),
                        ]),
                    ),
                    ("message", "OK".into()),
                    ("data", r#"{"status":"SUCCESS"}"#.into()),
                ]);
                if let Ok(Some(raw)) = stringify_json(&ack) {
                    if socket.send(Message::Text(raw.into())).await.is_err() {
                        break;
                    }
                }
            } else if kind.is_some_and(|s| s.equals_str("SYSTEM")) {
                let topic = v
                    .get("headers")
                    .and_then(|h| h.get("topic"))
                    .and_then(JsValue::as_string);
                if topic.is_some_and(|s| s.equals_str("ping") || s.equals_str("disconnect")) {
                    let ack = obj([
                        ("code", JsValue::Number(200.0)),
                        (
                            "headers",
                            v.get("headers").cloned().unwrap_or(JsValue::Undefined),
                        ),
                        ("message", "OK".into()),
                        ("data", v.get("data").cloned().unwrap_or(JsValue::Undefined)),
                    ]);
                    if let Ok(Some(raw)) = stringify_json(&ack) {
                        let _ = socket.send(Message::Text(raw.into())).await;
                    }
                    if topic.is_some_and(|s| s.equals_str("disconnect")) {
                        let _ = socket.close(None).await;
                        break;
                    }
                }
            }
        }
        loop {
            let jitter = (uuid::Uuid::new_v4().as_u128() % 1000) as u64;
            let delay = Duration::from_millis(
                (1000u64.saturating_mul(1u64 << attempt.min(6)) + jitter).min(60000),
            );
            tokio::select! {biased;_=async { drop(stopped.wait_for(|s|*s).await); }=>return,_=tokio::time::sleep(delay)=>{}}
            let next = tokio::select! {biased;_=async { drop(stopped.wait_for(|s|*s).await); }=>return,v=connect(&endpoint,&id,&secret)=>v};
            match next {
                Ok(next) => {
                    socket = next;
                    attempt = 0;
                    break;
                }
                Err(_) => attempt = attempt.saturating_add(1),
            }
        }
    }
}

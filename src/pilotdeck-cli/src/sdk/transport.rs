//! One owned connection for the CLI's ordinary RPCs; no reconnect loop or SDK
//! background tasks. Dropping this future also drops the socket and its timers.
use super::Connection;
use crate::Failure;
use futures_util::{SinkExt, StreamExt};
use pilotdeck_protocol::{JsValue, parse_json};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn error(message: &str) -> Failure {
    Failure::Error(message.into())
}

pub(super) async fn request(
    connection: &Connection,
    method: &str,
    params: Value,
) -> Result<JsValue, Failure> {
    // Node v22.22.0 uses the native WebSocket, not the ws package's 100 MiB
    // maxPayload. The SDK passes only the URL; its native client accepts even
    // 100 MiB + 1 byte (tests/fixtures/sdk-transport-limits).
    let config = WebSocketConfig::default()
        .max_frame_size(None)
        .max_message_size(None);
    let (mut socket, _) = timeout(
        CONNECT_TIMEOUT,
        connect_async_with_config(&connection.gateway_url, Some(config), false),
    )
    .await
    .map_err(|_| error("Gateway connection timed out."))?
    .map_err(|_| error("Gateway connection failed."))?;
    let result = async {
        timeout(CONNECT_TIMEOUT, async {
            send(
                &mut socket,
                json!({
                    "type": "hello", "protocolVersion": pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION, "clientName": "sdk",
                    "clientVersion": "pilotdeck-cli", "token": connection.auth_token,
                }),
            )
            .await?;
            loop {
                if string_field(&receive(&mut socket).await?, "type").as_deref() == Some("hello_ok")
                {
                    return Ok::<_, Failure>(());
                }
            }
        })
        .await
        .map_err(|_| error("Gateway hello timed out."))??;

        let id = uuid::Uuid::new_v4().to_string();
        // One deadline covers send and every unrelated frame; traffic cannot
        // keep an unanswered request alive by resetting the timer.
        timeout(REQUEST_TIMEOUT, async {
            send(
                &mut socket,
                json!({"type": "request", "id": id, "method": method, "params": params}),
            )
            .await?;
            loop {
                let frame = receive(&mut socket).await?;
                if string_field(&frame, "type").as_deref() != Some("response")
                    || string_field(&frame, "id").as_deref() != Some(&id)
                {
                    continue;
                }
                if frame.get("ok").and_then(JsValue::as_bool) == Some(true) {
                    return Ok(frame.get("result").cloned().unwrap_or(JsValue::Undefined));
                }
                let message = frame
                    .get("error")
                    .and_then(|value| string_field(value, "message"));
                return Err(error(
                    message.as_deref().unwrap_or("Gateway request failed."),
                ));
            }
        })
        .await
        .map_err(|_| error(&format!("Gateway request timed out: {method}")))?
    }
    .await;
    // TS client.close() starts an empty close handshake. Do not await peer
    // cooperation indefinitely, and never replace the RPC error with cleanup.
    let _ = timeout(CLOSE_TIMEOUT, SinkExt::close(&mut socket)).await;
    result
}

async fn send(socket: &mut Socket, frame: Value) -> Result<(), Failure> {
    socket
        .send(Message::Text(frame.to_string().into()))
        .await
        .map_err(|_| error("Gateway WebSocket is not connected."))
}

async fn receive(socket: &mut Socket) -> Result<JsValue, Failure> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(raw))) => {
                return parse_json(&raw).map_err(|_| error("Invalid Gateway JSON frame."));
            }
            Some(Ok(Message::Close(frame))) => {
                let reason = frame
                    .map(|frame| frame.reason.to_string())
                    .unwrap_or_default();
                return Err(error(if reason.is_empty() {
                    "Gateway WebSocket closed."
                } else {
                    &reason
                }));
            }
            Some(Ok(Message::Ping(_))) => {
                // tungstenite queues the matching pong while reading the ping.
                socket
                    .flush()
                    .await
                    .map_err(|_| error("Gateway WebSocket closed."))?;
            }
            Some(Ok(Message::Pong(_))) => {}
            Some(Ok(_)) => return Err(error("Invalid Gateway JSON frame.")),
            Some(Err(_)) | None => return Err(error("Gateway WebSocket closed.")),
        }
    }
}

fn string_field(value: &JsValue, name: &str) -> Option<String> {
    value
        .get(name)?
        .as_string()
        .map(|text| String::from_utf16_lossy(text.units()))
}

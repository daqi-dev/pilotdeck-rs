//! One owned connection for the CLI's ordinary RPCs; no reconnect loop or SDK
//! background tasks. Dropping this future also drops the socket and its timers.
use super::Connection;
use crate::Failure;
use futures_util::{SinkExt, StreamExt};
use pilotdeck_protocol::{JsValue, parse_json};
use serde::Serialize;
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
const MAX_RECEIVE_BYTES: usize = 128 * 1024 * 1024;
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn error(message: &str) -> Failure {
    Failure::Error(message.into())
}

pub(super) async fn request(
    connection: &Connection,
    method: &str,
    params: Value,
) -> Result<JsValue, Failure> {
    request_inner(connection, method, params, "pilotdeck-cli", false).await
}

/// Settings helpers use the SDK's default client version and lossless JSON
/// parameters; the sessions entry keeps its existing version and Value API.
pub(super) async fn request_with_version(
    connection: &Connection,
    method: &str,
    params: impl Serialize,
    client_version: &str,
) -> Result<JsValue, Failure> {
    request_inner(connection, method, params, client_version, true).await
}

async fn send_hello(
    socket: &mut Socket,
    connection: &Connection,
    client_version: &str,
    settings_order: bool,
) -> Result<(), Failure> {
    if settings_order {
        send(
            socket,
            SettingsHelloFrame {
                kind: "hello",
                protocol_version: pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,
                client_name: "sdk",
                client_version,
                token: &connection.auth_token,
            },
        )
        .await?;
    } else {
        send(
            socket,
            json!({
                "type": "hello", "protocolVersion": pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION, "clientName": "sdk",
                "clientVersion": client_version, "token": connection.auth_token,
            }),
        )
        .await?;
    }
    Ok(())
}

async fn request_inner(
    connection: &Connection,
    method: &str,
    params: impl Serialize,
    client_version: &str,
    settings_order: bool,
) -> Result<JsValue, Failure> {
    // Preserve the measured Node v22.22.0 successes through 100 MiB + 1.
    // A finite frame cap rejects hostile length headers before allocation;
    // the message cap also bounds accumulated fragments. Security exception:
    // specs/gateway-parity/diffs/D1-SDK-WS-RESOURCE-LIMIT-001.md.
    let config = WebSocketConfig::default()
        .max_frame_size(Some(MAX_RECEIVE_BYTES))
        .max_message_size(Some(MAX_RECEIVE_BYTES));
    let (mut socket, _) = timeout(
        CONNECT_TIMEOUT,
        connect_async_with_config(&connection.gateway_url, Some(config), false),
    )
    .await
    .map_err(|_| error("Gateway connection timed out."))?
    .map_err(|_| error("Gateway connection failed."))?;
    let result = async {
        timeout(CONNECT_TIMEOUT, async {
            send_hello(&mut socket, connection, client_version, settings_order).await?;
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
            if settings_order {
                send(
                    &mut socket,
                    SettingsRequestFrame {
                        kind: "request",
                        request_id: &id,
                        method,
                        params,
                    },
                )
                .await?;
            } else {
                send(
                    &mut socket,
                    RequestFrame {
                        kind: "request",
                        id: &id,
                        method,
                        params,
                    },
                )
                .await?;
            }
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

// Field declaration order is the SDK's JSON.stringify insertion order.
// Keep these envelopes separate from the legacy sessions serializer below.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsHelloFrame<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    protocol_version: &'a str,
    client_name: &'a str,
    client_version: &'a str,
    token: &'a str,
}

#[derive(Serialize)]
struct SettingsRequestFrame<'a, P> {
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(rename = "id")]
    request_id: &'a str,
    method: &'a str,
    params: P,
}

#[derive(Serialize)]
struct RequestFrame<'a, P> {
    // Preserve the old Value serializer's sorted envelope bytes for sessions.
    id: &'a str,
    method: &'a str,
    params: P,
    #[serde(rename = "type")]
    kind: &'a str,
}

async fn send(socket: &mut Socket, frame: impl Serialize) -> Result<(), Failure> {
    socket
        .send(Message::Text(
            serde_json::to_string(&frame)
                .map_err(|e| error(&e.to_string()))?
                .into(),
        ))
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

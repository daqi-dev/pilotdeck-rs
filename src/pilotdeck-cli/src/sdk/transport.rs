//! Owned CLI sockets for ordinary RPCs and the run control/query pair.
//! No reconnect loop or background tasks; dropping an owner drops its socket.
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

fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_frame_size(Some(MAX_RECEIVE_BYTES))
        .max_message_size(Some(MAX_RECEIVE_BYTES))
}

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
    let mut transport = Transport::connect(connection, client_version, settings_order).await?;
    let result = transport.request(method, params).await;
    transport.close().await;
    result
}

/// A run retains its control connection while the query uses a second socket.
/// Ordinary RPC callers still create and close exactly one owner per call.
pub(super) struct Transport {
    socket: Socket,
    settings_order: bool,
    stream_sequence: f64,
    pub(super) server_info: JsValue,
}

impl Transport {
    pub(super) async fn connect(
        connection: &Connection,
        client_version: &str,
        settings_order: bool,
    ) -> Result<Self, Failure> {
        // Retain P02's registered 128 MiB receive bounds on every connection.
        let config = websocket_config();
        let (mut socket, _) = timeout(
            CONNECT_TIMEOUT,
            connect_async_with_config(&connection.gateway_url, Some(config), false),
        )
        .await
        .map_err(|_| error("Gateway connection timed out."))?
        .map_err(|_| error("Gateway connection failed."))?;
        let hello = timeout(CONNECT_TIMEOUT, async {
            send_hello(&mut socket, connection, client_version, settings_order).await?;
            loop {
                let frame = receive(&mut socket).await?;
                if string_field(&frame, "type").as_deref() == Some("hello_ok") {
                    return Ok::<_, Failure>(frame);
                }
            }
        })
        .await
        .map_err(|_| error("Gateway hello timed out."))
        .and_then(|result| result);
        match hello {
            Ok(frame) => Ok(Self {
                socket,
                settings_order,
                stream_sequence: -1.0,
                server_info: frame
                    .get("serverInfo")
                    .cloned()
                    .unwrap_or(JsValue::Undefined),
            }),
            Err(error) => {
                close_socket(&mut socket).await;
                Err(error)
            }
        }
    }

    pub(super) async fn request(
        &mut self,
        method: &str,
        params: impl Serialize,
    ) -> Result<JsValue, Failure> {
        let id = uuid::Uuid::new_v4().to_string();
        // A single deadline includes send and all unrelated frames.
        timeout(REQUEST_TIMEOUT, async {
            self.send_request(&id, method, params).await?;
            loop {
                let frame = receive(&mut self.socket).await?;
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

    async fn send_request(
        &mut self,
        id: &str,
        method: &str,
        params: impl Serialize,
    ) -> Result<(), Failure> {
        let id = id.to_owned();
        // Preserve the ordinary RPC envelope and its lossless params. Bound
        // stream submission as well: a non-reading peer cannot block send forever.
        timeout(REQUEST_TIMEOUT, async {
            if self.settings_order {
                send(
                    &mut self.socket,
                    SettingsRequestFrame {
                        kind: "request",
                        request_id: &id,
                        method,
                        params,
                    },
                )
                .await
            } else {
                send(
                    &mut self.socket,
                    RequestFrame {
                        kind: "request",
                        id: &id,
                        method,
                        params,
                    },
                )
                .await
            }
        })
        .await
        .map_err(|_| error(&format!("Gateway request timed out: {method}")))?
    }

    pub(super) async fn start_stream(
        &mut self,
        method: &str,
        params: impl Serialize,
    ) -> Result<String, Failure> {
        let id = uuid::Uuid::new_v4().to_string();
        self.send_request(&id, method, params).await?;
        self.stream_sequence = -1.0;
        Ok(id)
    }

    /// Stream events have no ordinary-RPC timeout in the SDK. The `final` bit,
    /// socket closure, or transport error ends this wait; terminal payloads alone
    /// must not close a stream that can still carry lifecycle events.
    pub(super) async fn stream_event(
        &mut self,
        id: &str,
    ) -> Result<Option<(JsValue, bool)>, Failure> {
        loop {
            let frame = receive(&mut self.socket).await?;
            if string_field(&frame, "id").as_deref() != Some(id) {
                continue;
            }
            match string_field(&frame, "type").as_deref() {
                Some("event") => {
                    let sequence = frame
                        .get("seq")
                        .and_then(JsValue::as_number)
                        .filter(|seq| seq.is_finite() && *seq >= 0.0 && seq.fract() == 0.0)
                        .ok_or_else(|| {
                            error(&format!(
                                "Gateway event sequence must be a non-negative integer for {id}."
                            ))
                        })?;
                    if sequence <= self.stream_sequence {
                        continue;
                    }
                    if sequence != self.stream_sequence + 1.0 {
                        return Err(error(&format!(
                            "Gateway event sequence gap for {id}: expected {}, received {sequence}.",
                            self.stream_sequence + 1.0
                        )));
                    }
                    self.stream_sequence = sequence;
                    return Ok(Some((
                        frame.get("event").cloned().unwrap_or(JsValue::Undefined),
                        matches!(frame.get("final"), Some(JsValue::Bool(true))),
                    )));
                }
                Some("response") if matches!(frame.get("ok"), Some(JsValue::Bool(true))) => {
                    return Ok(None);
                }
                Some("response") if frame.get("ok").and_then(JsValue::as_bool) == Some(false) => {
                    let message = frame.get("error").and_then(|v| string_field(v, "message"));
                    return Err(error(
                        message.as_deref().unwrap_or("Gateway stream failed."),
                    ));
                }
                _ => {}
            }
        }
    }

    pub(super) async fn close(&mut self) {
        // Cleanup never replaces the original RPC/stream error.
        close_socket(&mut self.socket).await;
    }
}

async fn close_socket(mut socket: &mut Socket) {
    let _ = timeout(CLOSE_TIMEOUT, SinkExt::close(&mut socket)).await;
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

pub(super) fn string_field(value: &JsValue, name: &str) -> Option<String> {
    value
        .get(name)?
        .as_string()
        .map(|text| String::from_utf16_lossy(text.units()))
}

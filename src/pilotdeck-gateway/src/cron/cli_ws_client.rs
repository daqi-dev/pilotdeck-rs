//! Native CLI RPC client for the authenticated Gateway WebSocket protocol.
pub use super::always_on::AlwaysOnError;
use pilotdeck_protocol::frame::{self, Decoder};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{broadcast, mpsc, oneshot},
    time::{Duration, timeout},
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq)]
pub struct StreamEvent {
    pub sequence: u64,
    pub final_event: bool,
    pub event: Value,
}

pub struct EventStream {
    receiver: mpsc::Receiver<Result<StreamEvent, AlwaysOnError>>,
}

impl EventStream {
    pub async fn next(&mut self) -> Option<Result<StreamEvent, AlwaysOnError>> {
        self.receiver.recv().await
    }
}

enum PendingCall {
    Request(oneshot::Sender<Result<Value, AlwaysOnError>>),
    Stream {
        events: mpsc::Sender<Result<StreamEvent, AlwaysOnError>>,
        last_sequence: Option<u64>,
    },
}

enum Command {
    Request {
        id: String,
        method: String,
        params: Value,
        reply: oneshot::Sender<Result<Value, AlwaysOnError>>,
    },
    Stream {
        id: String,
        method: String,
        params: Value,
        events: mpsc::Sender<Result<StreamEvent, AlwaysOnError>>,
        reply: oneshot::Sender<Result<(), AlwaysOnError>>,
    },
    Cancel(String),
    Close(oneshot::Sender<()>),
}

/// One authenticated, multiplexed Gateway connection. Response/event ids keep
/// concurrent request results isolated.
#[derive(Clone)]
pub struct Client {
    commands: mpsc::Sender<Command>,
    notifications: broadcast::Sender<Value>,
    sequence: Arc<std::sync::atomic::AtomicU64>,
    server_info: Arc<Value>,
}

impl Client {
    pub async fn connect(endpoint: &str, token: &str) -> Result<Self, AlwaysOnError> {
        Self::connect_as(endpoint, token, "cli").await
    }

    pub async fn connect_as(
        endpoint: &str,
        token: &str,
        client_name: &str,
    ) -> Result<Self, AlwaysOnError> {
        let endpoint = websocket_endpoint(endpoint)?;
        let (upgraded, _) = timeout(
            Duration::from_secs(10),
            tokio_tungstenite::connect_async(endpoint),
        )
        .await
        .map_err(|_| AlwaysOnError::new("gateway_timeout", "Gateway connection timed out."))?
        .map_err(|error| AlwaysOnError::new("gateway_unavailable", error.to_string()))?;
        // The Gateway does not emit WebSocket data before receiving hello, so
        // no post-handshake bytes are buffered when ownership moves to the
        // protocol codec shared with the native gateway.
        let mut stream = upgraded.into_inner();
        let mut decoder = Decoder::default();
        send(
            &mut stream,
            &json!({
                "type":"hello",
                "protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,
                "clientName":client_name,
                "clientVersion":env!("CARGO_PKG_VERSION"),
                "token":token
            }),
        )
        .await?;
        let hello = loop {
            let hello = timeout(REQUEST_TIMEOUT, receive(&mut stream, &mut decoder))
                .await
                .map_err(|_| {
                    AlwaysOnError::new("gateway_timeout", "Gateway handshake timed out.")
                })??;
            match hello["type"].as_str() {
                Some("hello_ok") => break hello,
                Some("hello_error") => {
                    return Err(AlwaysOnError::new(
                        hello["error"]["code"]
                            .as_str()
                            .unwrap_or("gateway_auth_failed"),
                        hello["error"]["message"]
                            .as_str()
                            .unwrap_or("Gateway authentication failed."),
                    ));
                }
                _ => {}
            }
        };
        let (commands, receiver) = mpsc::channel(64);
        let (notifications, _) = broadcast::channel(64);
        tokio::spawn(actor(stream, decoder, receiver, notifications.clone()));
        Ok(Self {
            commands,
            notifications,
            sequence: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            server_info: Arc::new(hello["serverInfo"].clone()),
        })
    }

    pub fn server_info(&self) -> &Value {
        &self.server_info
    }

    pub fn has_capability(&self, capability: &str) -> bool {
        self.server_info["capabilities"]
            .as_array()
            .is_some_and(|capabilities| {
                capabilities
                    .iter()
                    .any(|item| item.as_str() == Some(capability))
            })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.notifications.subscribe()
    }

    pub async fn request(&self, method: &str, params: &Value) -> Result<Value, AlwaysOnError> {
        let id = self.next_id();
        let (reply, response) = oneshot::channel();
        self.commands
            .send(Command::Request {
                id: id.clone(),
                method: method.to_owned(),
                params: params.clone(),
                reply,
            })
            .await
            .map_err(|_| closed())?;
        match timeout(REQUEST_TIMEOUT, response).await {
            Ok(result) => result.map_err(|_| closed())?,
            Err(_) => {
                let _ = self.commands.send(Command::Cancel(id)).await;
                Err(AlwaysOnError::new(
                    "gateway_timeout",
                    format!("Gateway request timed out: {method}"),
                ))
            }
        }
    }

    pub async fn stream(&self, method: &str, params: &Value) -> Result<EventStream, AlwaysOnError> {
        let id = self.next_id();
        let (events, receiver) = mpsc::channel(64);
        let (reply, accepted) = oneshot::channel();
        self.commands
            .send(Command::Stream {
                id,
                method: method.to_owned(),
                params: params.clone(),
                events,
                reply,
            })
            .await
            .map_err(|_| closed())?;
        accepted.await.map_err(|_| closed())??;
        Ok(EventStream { receiver })
    }

    pub async fn close(&self) {
        let (reply, closed) = oneshot::channel();
        if self.commands.send(Command::Close(reply)).await.is_ok() {
            let _ = closed.await;
        }
    }

    fn next_id(&self) -> String {
        format!(
            "cli-{}",
            self.sequence
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
    }
}

fn websocket_endpoint(endpoint: &str) -> Result<String, AlwaysOnError> {
    let uri: hyper::Uri = endpoint
        .parse()
        .map_err(|_| AlwaysOnError::new("gateway_unavailable", "Invalid Gateway URL."))?;
    let authority = uri
        .authority()
        .ok_or_else(|| AlwaysOnError::new("gateway_unavailable", "Invalid Gateway URL."))?;
    let scheme = match uri.scheme_str() {
        Some("ws" | "http") => "ws",
        Some("wss" | "https") => "wss",
        _ => {
            return Err(AlwaysOnError::new(
                "gateway_unavailable",
                "Gateway URL must use ws:// or http://.",
            ));
        }
    };
    let path = uri
        .path_and_query()
        .map(|value| value.as_str().to_owned())
        .filter(|value| value != "/")
        .unwrap_or_else(|| "/ws".to_owned());
    Ok(format!("{scheme}://{authority}{path}"))
}

async fn actor<S>(
    mut stream: S,
    mut decoder: Decoder,
    mut commands: mpsc::Receiver<Command>,
    notifications: broadcast::Sender<Value>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut pending = HashMap::<String, PendingCall>::new();
    let failure = loop {
        tokio::select! {
            biased;
            command = commands.recv() => match command {
                Some(Command::Request { id, method, params, reply }) => {
                    let frame = json!({"type":"request","id":id,"method":method,"params":params});
                    if let Err(error) = send(&mut stream, &frame).await {
                        let _ = reply.send(Err(error.clone()));
                        break error;
                    }
                    pending.insert(id, PendingCall::Request(reply));
                }
                Some(Command::Stream { id, method, params, events, reply }) => {
                    let frame = json!({"type":"request","id":id,"method":method,"params":params});
                    if let Err(error) = send(&mut stream, &frame).await {
                        let _ = reply.send(Err(error.clone()));
                        break error;
                    }
                    pending.insert(id, PendingCall::Stream { events, last_sequence: None });
                    let _ = reply.send(Ok(()));
                }
                Some(Command::Cancel(id)) => {
                    pending.remove(&id);
                }
                Some(Command::Close(reply)) => {
                    let _ = stream.write_all(&frame::client_frame(0x88, 0, &[])).await;
                    let _ = reply.send(());
                    break closed();
                }
                None => break closed(),
            },
            incoming = receive(&mut stream, &mut decoder) => match incoming {
                Ok(value) => route(value, &mut pending, &notifications).await,
                Err(error) => break error,
            }
        }
    };
    fail_pending(pending, failure).await;
}

async fn route(
    value: Value,
    pending: &mut HashMap<String, PendingCall>,
    notifications: &broadcast::Sender<Value>,
) {
    if value["type"] == "notification" {
        let _ = notifications.send(value);
        return;
    }
    let Some(id) = value["id"].as_str().map(str::to_owned) else {
        return;
    };
    if value["type"] == "response" {
        let Some(item) = pending.remove(&id) else {
            return;
        };
        let response = if value["ok"] == true {
            Ok(value["result"].clone())
        } else {
            Err(remote_error(&value))
        };
        match item {
            PendingCall::Request(reply) => {
                let _ = reply.send(response);
            }
            PendingCall::Stream { events, .. } => {
                let _ = events
                    .send(response.map(|event| StreamEvent {
                        sequence: 0,
                        final_event: true,
                        event,
                    }))
                    .await;
            }
        }
        return;
    }
    if value["type"] != "event" {
        return;
    }
    let Some(sequence) = value["seq"].as_u64() else {
        fail_stream(
            pending,
            &id,
            AlwaysOnError::new(
                "validation_error",
                format!("Gateway event sequence must be a non-negative integer for {id}."),
            ),
        )
        .await;
        return;
    };
    let previous = match pending.get(&id) {
        Some(PendingCall::Stream { last_sequence, .. }) => *last_sequence,
        _ => return,
    };
    if previous.is_some_and(|previous| sequence <= previous) {
        return;
    }
    let expected = previous.map_or(0, |previous| previous + 1);
    if sequence != expected {
        fail_stream(
            pending,
            &id,
            AlwaysOnError::new(
                "validation_error",
                format!(
                    "Gateway event sequence gap for {id}: expected {expected}, received {sequence}."
                ),
            ),
        )
        .await;
        return;
    }
    let final_event = value["final"] == true;
    let event = StreamEvent {
        sequence,
        final_event,
        event: value["event"].clone(),
    };
    let sender = match pending.get_mut(&id) {
        Some(PendingCall::Stream {
            events,
            last_sequence,
        }) => {
            *last_sequence = Some(sequence);
            events.clone()
        }
        _ => return,
    };
    let _ = sender.send(Ok(event)).await;
    if final_event {
        pending.remove(&id);
    }
}

async fn fail_stream(pending: &mut HashMap<String, PendingCall>, id: &str, error: AlwaysOnError) {
    if let Some(PendingCall::Stream { events, .. }) = pending.remove(id) {
        let _ = events.send(Err(error)).await;
    }
}

async fn fail_pending(pending: HashMap<String, PendingCall>, error: AlwaysOnError) {
    for item in pending.into_values() {
        match item {
            PendingCall::Request(reply) => {
                let _ = reply.send(Err(error.clone()));
            }
            PendingCall::Stream { events, .. } => {
                let _ = events.send(Err(error.clone())).await;
            }
        }
    }
}

fn remote_error(value: &Value) -> AlwaysOnError {
    AlwaysOnError::new(
        value["error"]["code"]
            .as_str()
            .unwrap_or("gateway_request_failed"),
        value["error"]["message"]
            .as_str()
            .unwrap_or("Gateway request failed."),
    )
}

fn closed() -> AlwaysOnError {
    AlwaysOnError::new("gateway_closed", "Gateway closed the connection.")
}

/// Compatibility wrapper used by the cron CLI.
pub async fn request(
    endpoint: &str,
    token: &str,
    method: &str,
    params: &Value,
) -> Result<Value, AlwaysOnError> {
    let client = Client::connect(endpoint, token).await?;
    let result = client.request(method, params).await;
    client.close().await;
    result
}

async fn send<S: AsyncWrite + Unpin>(stream: &mut S, value: &Value) -> Result<(), AlwaysOnError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| AlwaysOnError::new("gateway_protocol", error.to_string()))?;
    let length = match bytes.len() {
        0..126 => bytes.len() as u8,
        126..=65535 => 126,
        _ => 127,
    };
    stream
        .write_all(&frame::client_frame(0x81, length, &bytes))
        .await
        .map_err(|error| AlwaysOnError::new("gateway_closed", error.to_string()))?;
    Ok(())
}

async fn receive<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    decoder: &mut Decoder,
) -> Result<Value, AlwaysOnError> {
    loop {
        if let Some(frame) = decoder
            .next_server()
            .map_err(|_| AlwaysOnError::new("gateway_protocol", "Invalid gateway frame."))?
        {
            match frame.opcode {
                1 => {
                    return serde_json::from_slice(&frame.payload).map_err(|error| {
                        AlwaysOnError::new("gateway_protocol", error.to_string())
                    });
                }
                8 => return Err(closed()),
                9 => stream
                    .write_all(&frame::client_frame(
                        0x8a,
                        frame.payload.len() as u8,
                        &frame.payload,
                    ))
                    .await
                    .map_err(|error| AlwaysOnError::new("gateway_closed", error.to_string()))?,
                _ => {}
            }
        } else if stream
            .read_buf(&mut decoder.buffer)
            .await
            .map_err(|error| AlwaysOnError::new("gateway_closed", error.to_string()))?
            == 0
        {
            return Err(AlwaysOnError::new(
                "gateway_closed",
                "Gateway ended the connection.",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use sha1::{Digest, Sha1};
    use tokio::net::TcpListener;

    #[test]
    fn endpoint_keeps_path_defaults_root_and_supports_tls() {
        assert_eq!(
            websocket_endpoint("ws://127.0.0.1:18789/runtime/ws?x=1").unwrap(),
            "ws://127.0.0.1:18789/runtime/ws?x=1"
        );
        assert_eq!(
            websocket_endpoint("http://localhost:9").unwrap(),
            "ws://localhost:9/ws"
        );
        assert_eq!(
            websocket_endpoint("https://example.invalid/runtime/ws").unwrap(),
            "wss://example.invalid/runtime/ws"
        );
    }

    #[tokio::test]
    async fn named_connect_sends_identity_and_preserves_server_capabilities() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let mut chunk = [0; 1024];
                let count = socket.read(&mut chunk).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&chunk[..count]);
            }
            let request = String::from_utf8(request).unwrap();
            let key = request
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("sec-websocket-key"))
                        .map(|(_, value)| value.trim())
                })
                .unwrap();
            let accept = STANDARD.encode(Sha1::digest(format!(
                "{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
            )));
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            let mut decoder = Decoder::default();
            let hello = loop {
                if let Some(frame) = decoder.next().unwrap() {
                    break serde_json::from_slice::<Value>(&frame.payload).unwrap();
                }
                assert_ne!(socket.read_buf(&mut decoder.buffer).await.unwrap(), 0);
            };
            assert_eq!(hello["clientName"], "sdk");
            socket
                .write_all(&frame::server_frame(
                    1,
                    br#"{"type":"hello_ok","serverInfo":{"mode":"local","capabilities":["sdk_session_defaults","other"]}}"#,
                ))
                .await
                .unwrap();

            loop {
                if let Some(frame) = decoder.next().unwrap() {
                    assert_eq!(frame.opcode, 8);
                    break;
                }
                assert_ne!(socket.read_buf(&mut decoder.buffer).await.unwrap(), 0);
            }
        });

        let client = Client::connect_as(&format!("ws://{address}/ws"), "token", "sdk")
            .await
            .unwrap();
        assert_eq!(client.server_info()["mode"], "local");
        assert!(client.has_capability("sdk_session_defaults"));
        assert!(!client.has_capability("missing"));
        client.close().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn routes_concurrent_responses_streams_and_notifications() {
        let (notifications, mut notices) = broadcast::channel(4);
        let (request_reply, request_result) = oneshot::channel();
        let (event_sender, mut events) = mpsc::channel(4);
        let mut pending = HashMap::from([
            ("r".into(), PendingCall::Request(request_reply)),
            (
                "s".into(),
                PendingCall::Stream {
                    events: event_sender,
                    last_sequence: None,
                },
            ),
        ]);
        route(
            json!({"type":"notification","name":"config_changed"}),
            &mut pending,
            &notifications,
        )
        .await;
        route(
            json!({"type":"response","id":"r","ok":true,"result":{"ok":1}}),
            &mut pending,
            &notifications,
        )
        .await;
        route(json!({"type":"event","id":"s","seq":0,"final":false,"event":{"type":"assistant_text_delta","text":"x"}}), &mut pending, &notifications).await;
        route(
            json!({"type":"event","id":"s","seq":1,"final":true,"event":{"type":"turn_completed"}}),
            &mut pending,
            &notifications,
        )
        .await;
        assert_eq!(notices.recv().await.unwrap()["name"], "config_changed");
        assert_eq!(request_result.await.unwrap().unwrap(), json!({"ok":1}));
        assert_eq!(events.recv().await.unwrap().unwrap().sequence, 0);
        assert!(events.recv().await.unwrap().unwrap().final_event);
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn response_error_settles_a_stream() {
        let (notifications, _) = broadcast::channel(1);
        let (sender, mut events) = mpsc::channel(1);
        let mut pending = HashMap::from([(
            "s".into(),
            PendingCall::Stream {
                events: sender,
                last_sequence: None,
            },
        )]);
        route(
            json!({"type":"response","id":"s","ok":false,"error":{"code":"bad","message":"no"}}),
            &mut pending,
            &notifications,
        )
        .await;
        let error = events.recv().await.unwrap().unwrap_err();
        assert_eq!((error.code.as_str(), error.message.as_str()), ("bad", "no"));
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn stream_deduplicates_and_rejects_sequence_gaps() {
        let (notifications, _) = broadcast::channel(1);
        let (sender, mut events) = mpsc::channel(4);
        let mut pending = HashMap::from([(
            "s".into(),
            PendingCall::Stream {
                events: sender,
                last_sequence: None,
            },
        )]);
        let event = |seq| json!({"type":"event","id":"s","seq":seq,"final":false,"event":{}});
        route(event(0), &mut pending, &notifications).await;
        route(event(0), &mut pending, &notifications).await;
        route(event(2), &mut pending, &notifications).await;
        assert_eq!(events.recv().await.unwrap().unwrap().sequence, 0);
        let error = events.recv().await.unwrap().unwrap_err();
        assert!(error.message.contains("expected 1, received 2"));
        assert!(pending.is_empty());
    }
}

//! Owned CLI sockets for ordinary RPCs and the run control/query pair.
//! No reconnect loop or background tasks; dropping an owner drops its socket.
use super::Connection;
use crate::Failure;
use futures_util::{SinkExt, StreamExt};
use pilotdeck_protocol::{JsValue, number::number_to_string, parse_json};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    net::TcpStream,
    time::{Instant, timeout, timeout_at},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
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

/// Check after each ready operation as well as waiting on the timer. A stream
/// of ready frames can keep the timer driver from getting another turn.
pub(super) async fn within_deadline<T>(
    deadline: Instant,
    request: impl std::future::Future<Output = Result<T, Failure>>,
    message: &str,
) -> Result<T, Failure> {
    match timeout_at(deadline, request).await {
        Ok(result) if Instant::now() < deadline => result,
        _ => Err(error(message)),
    }
}

/// Move an own property out of a frame that is being dropped anyway. The old
/// deep clone held the original tree and a full copy (plus the clone stack)
/// in memory while `response.result` carries a whole sessions projection
/// (#983); a missing key stays `Undefined`, like `frame.get(name)` would.
fn take_field(mut frame: JsValue, name: &str) -> JsValue {
    match &mut frame {
        JsValue::Object(object) => object
            .insert(name.into(), JsValue::Null)
            .unwrap_or(JsValue::Undefined),
        _ => JsValue::Undefined,
    }
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
    #[cfg(test)]
    receive_probe: Option<tests::ReceiveProbe>,
}

/// The query stream and its cancellation RPCs share one socket reader. A
/// response for an abort must never be discarded by the stream consumer.
pub(super) enum QueryFrame {
    Event(JsValue, bool),
    StreamDone,
    StreamFailed(Failure),
    AbortReply(Result<JsValue, Failure>),
}

impl Transport {
    pub(super) async fn connect(
        connection: &Connection,
        client_version: &str,
        settings_order: bool,
    ) -> Result<Self, Failure> {
        let mut transport = Self::open(connection, settings_order).await?;
        if let Err(error) = transport.hello(connection, client_version).await {
            transport.close().await;
            return Err(error);
        }
        Ok(transport)
    }

    /// The SDK query keeps its socket when initialization rejects. Separate
    /// opening from hello so the turn owner can preserve that observable
    /// lifetime, while ordinary `connect` callers retain their usual cleanup.
    pub(super) async fn open(
        connection: &Connection,
        settings_order: bool,
    ) -> Result<Self, Failure> {
        // Retain P02's registered 128 MiB receive bounds on every connection.
        let config = websocket_config();
        let (socket, _) = timeout(
            CONNECT_TIMEOUT,
            connect_async_with_config(&connection.gateway_url, Some(config), false),
        )
        .await
        .map_err(|_| error("Gateway connection timed out."))?
        .map_err(|_| error("Gateway connection failed."))?;
        Ok(Self {
            socket,
            settings_order,
            stream_sequence: -1.0,
            server_info: JsValue::Undefined,
            #[cfg(test)]
            receive_probe: None,
        })
    }

    pub(super) async fn hello(
        &mut self,
        connection: &Connection,
        client_version: &str,
    ) -> Result<(), Failure> {
        let deadline = self.deadline(CONNECT_TIMEOUT);
        let message = "Gateway hello timed out.";
        within_deadline(
            deadline,
            send_hello(
                &mut self.socket,
                connection,
                client_version,
                self.settings_order,
            ),
            message,
        )
        .await?;
        let hello = loop {
            let frame = self.receive(Some((deadline, message))).await?;
            if string_field(&frame, "type").as_deref() == Some("hello_ok") {
                break frame;
            }
        };
        self.server_info = take_field(hello, "serverInfo");
        Ok(())
    }

    pub(super) async fn request(
        &mut self,
        method: &str,
        params: impl Serialize,
    ) -> Result<JsValue, Failure> {
        let id = uuid::Uuid::new_v4().to_string();
        // A single deadline includes send and every unrelated/control frame.
        let deadline = self.deadline(REQUEST_TIMEOUT);
        let message = format!("Gateway request timed out: {method}");
        within_deadline(deadline, self.send_request(&id, method, params), &message).await?;
        loop {
            let frame = self.receive(Some((deadline, &message))).await?;
            if string_field(&frame, "type").as_deref() != Some("response")
                || string_field(&frame, "id").as_deref() != Some(&id)
            {
                continue;
            }
            if frame.get("ok").and_then(JsValue::as_bool) == Some(true) {
                return Ok(take_field(frame, "result"));
            }
            let message = frame
                .get("error")
                .and_then(|value| string_field(value, "message"));
            return Err(error(
                message.as_deref().unwrap_or("Gateway request failed."),
            ));
        }
    }

    fn deadline(&self, duration: Duration) -> Instant {
        #[cfg(test)]
        if let Some(probe) = &self.receive_probe {
            return Instant::now() + probe.timeout;
        }
        Instant::now() + duration
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
        let id = self.begin_request(method, params).await?;
        self.stream_sequence = -1.0;
        Ok(id)
    }

    /// Send an RPC without taking over the socket's receive loop.
    pub(super) async fn begin_request(
        &mut self,
        method: &str,
        params: impl Serialize,
    ) -> Result<String, Failure> {
        let id = uuid::Uuid::new_v4().to_string();
        self.send_request(&id, method, params).await?;
        Ok(id)
    }

    /// Stream events have no ordinary-RPC timeout in the SDK. The `final` bit,
    /// socket closure, or transport error ends this wait; terminal payloads alone
    /// must not close a stream that can still carry lifecycle events.
    pub(super) async fn query_frame(
        &mut self,
        id: Option<&str>,
        abort_ids: &[String],
    ) -> Result<QueryFrame, Failure> {
        loop {
            let frame = self.receive(None).await?;
            let frame_id = string_field(&frame, "id");
            if string_field(&frame, "type").as_deref() == Some("response")
                && frame_id.as_ref().is_some_and(|id| abort_ids.contains(id))
            {
                let result = if frame.get("ok").and_then(JsValue::as_bool) == Some(true) {
                    Ok(take_field(frame, "result"))
                } else {
                    let message = frame.get("error").and_then(|v| string_field(v, "message"));
                    Err(error(
                        message.as_deref().unwrap_or("Gateway request failed."),
                    ))
                };
                return Ok(QueryFrame::AbortReply(result));
            }
            // After the stream fails, its registration is gone but pending
            // abort RPCs still own this socket. Ignore every stream frame.
            let Some(id) = id else {
                continue;
            };
            if frame_id.as_deref() != Some(id) {
                continue;
            }
            match string_field(&frame, "type").as_deref() {
                Some("event") => {
                    let sequence = match next_sequence(&frame, id, self.stream_sequence) {
                        Ok(Some(sequence)) => sequence,
                        Ok(None) => continue,
                        Err(error) => return Ok(QueryFrame::StreamFailed(error)),
                    };
                    self.stream_sequence = sequence;
                    let final_frame = matches!(frame.get("final"), Some(JsValue::Bool(true)));
                    return Ok(QueryFrame::Event(take_field(frame, "event"), final_frame));
                }
                Some("response") if matches!(frame.get("ok"), Some(JsValue::Bool(true))) => {
                    return Ok(QueryFrame::StreamDone);
                }
                Some("response") if frame.get("ok").and_then(JsValue::as_bool) == Some(false) => {
                    let message = frame.get("error").and_then(|v| string_field(v, "message"));
                    return Ok(QueryFrame::StreamFailed(error(
                        message.as_deref().unwrap_or("Gateway stream failed."),
                    )));
                }
                _ => {}
            }
        }
    }

    /// Bound each raw frame, including Ping/Pong filtering and its flush.
    /// Untimed query streams retain their existing lifetime.
    async fn receive(&mut self, deadline: Option<(Instant, &str)>) -> Result<JsValue, Failure> {
        loop {
            let next = async {
                match self.next_message().await {
                    Some(Ok(Message::Text(raw))) => parse_json(&raw)
                        .map(Some)
                        .map_err(|_| error("Invalid Gateway JSON frame.")),
                    Some(Ok(Message::Close(frame))) => {
                        let reason = frame
                            .map(|frame| frame.reason.to_string())
                            .unwrap_or_default();
                        Err(error(if reason.is_empty() {
                            "Gateway WebSocket closed."
                        } else {
                            &reason
                        }))
                    }
                    Some(Ok(Message::Ping(_))) => {
                        self.socket
                            .flush()
                            .await
                            .map_err(|_| error("Gateway WebSocket closed."))?;
                        Ok(None)
                    }
                    Some(Ok(Message::Pong(_))) => Ok(None),
                    Some(Ok(_)) => Err(error("Invalid Gateway JSON frame.")),
                    Some(Err(_)) | None => Err(error("Gateway WebSocket closed.")),
                }
            };
            let frame = match deadline {
                Some((deadline, message)) => within_deadline(deadline, next, message).await?,
                None => next.await?,
            };
            if let Some(frame) = frame {
                return Ok(frame);
            }
        }
    }

    async fn next_message(
        &mut self,
    ) -> Option<Result<Message, tokio_tungstenite::tungstenite::Error>> {
        #[cfg(test)]
        if let Some(probe) = &mut self.receive_probe {
            return probe.next().await;
        }
        self.socket.next().await
    }

    pub(super) async fn close(&mut self) {
        // Cleanup never replaces the original RPC/stream error.
        close_socket(&mut self.socket).await;
    }

    pub(super) async fn flush_pending(&mut self) {
        // A received close queues an acknowledgement; flushing must not
        // initiate a close on an otherwise still-open exceptional query.
        let _ = timeout(CLOSE_TIMEOUT, self.socket.flush()).await;
    }

    /// An exceptional SDK query can outlive its CLI catch/finally. Do not turn
    /// that known lifetime into a successful early exit. The peer closing the
    /// socket (or the process receiving its default signal) ends this wait.
    pub(super) async fn wait_peer_closed(&mut self) {
        while let Some(frame) = self.socket.next().await {
            match frame {
                Ok(Message::Close(_)) => {
                    self.flush_pending().await;
                    break;
                }
                Err(_) => break,
                Ok(Message::Ping(_)) if self.socket.flush().await.is_err() => break,
                _ => {}
            }
        }
    }
}

fn next_sequence(frame: &JsValue, id: &str, previous: f64) -> Result<Option<f64>, Failure> {
    let sequence = frame
        .get("seq")
        .and_then(JsValue::as_number)
        .filter(|seq| seq.is_finite() && *seq >= 0.0 && seq.fract() == 0.0)
        .ok_or_else(|| {
            error(&format!(
                "Gateway event sequence must be a non-negative integer for {id}."
            ))
        })?;
    if sequence <= previous {
        return Ok(None);
    }
    if sequence != previous + 1.0 {
        return Err(error(&format!(
            "Gateway event sequence gap for {id}: expected {}, received {}.",
            number_to_string(previous + 1.0),
            number_to_string(sequence)
        )));
    }
    Ok(Some(sequence))
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

pub(super) fn string_field(value: &JsValue, name: &str) -> Option<String> {
    value
        .get(name)?
        .as_string()
        .map(|text| String::from_utf16_lossy(text.units()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_negative_receipt::verify;

    // Per-owner IO hook: every injected frame is Ready in the same poll. The
    // second read crosses D without yielding to the timer driver; after eight
    // frames it ends, so an old-loop mutant fails an assertion rather than hangs.
    pub(super) struct ReceiveProbe {
        pub(super) timeout: Duration,
        frame: Option<Message>,
        reads: usize,
    }

    impl ReceiveProbe {
        pub(super) async fn next(
            &mut self,
        ) -> Option<Result<Message, tokio_tungstenite::tungstenite::Error>> {
            self.reads += 1;
            let Some(frame) = &self.frame else {
                return std::future::pending().await;
            };
            if self.reads == 2 {
                std::thread::sleep(self.timeout + Duration::from_millis(1));
            }
            (self.reads <= 8).then(|| Ok(frame.clone()))
        }
    }

    fn deadline_row(operation: &str, mode: &str) -> Value {
        let oracle: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/sdk-signal-abort-state/oracle.json"
        ))
        .expect("#1145 transport deadline oracle JSON");
        oracle["cases"]
            .as_array()
            .expect("#1145 transport deadline cases")
            .iter()
            .find(|row| {
                row["kind"] == "transport-deadline"
                    && row["input"]["operation"] == operation
                    && row["input"]["mode"] == mode
            })
            .expect("#1145 transport deadline named TS cell exists")
            .clone()
    }

    async fn connected_pair() -> (Transport, WebSocketStream<TcpStream>, Connection) {
        timeout(Duration::from_secs(5), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("#1145 transport deadline bind owned mock");
            let address = listener
                .local_addr()
                .expect("#1145 transport deadline mock address");
            let connection = Connection {
                gateway_url: format!("ws://{address}/ws"),
                auth_token: "fixture".into(),
            };
            let accept = async {
                let (socket, _) = listener
                    .accept()
                    .await
                    .expect("#1145 transport deadline accept");
                tokio_tungstenite::accept_async(socket)
                    .await
                    .expect("#1145 transport deadline accept websocket")
            };
            let (transport, peer) = tokio::join!(Transport::open(&connection, false), accept);
            (
                transport.expect("#1145 transport deadline open"),
                peer,
                connection,
            )
        })
        .await
        .expect("#1145 transport deadline setup bounded")
    }

    async fn operation(
        transport: &mut Transport,
        connection: &Connection,
        kind: &str,
    ) -> Result<(), Failure> {
        if kind == "hello" {
            transport.hello(connection, "fixture").await
        } else {
            transport
                .request("deadline_probe", json!({}))
                .await
                .map(|_| ())
        }
    }

    async fn ready_flood(operation_name: &str, marker: &str) {
        let row = deadline_row(operation_name, "ready-flood");
        assert_eq!(
            row["output"]["beforeDeadlineSettled"], false,
            "#1145 transport TS no premature deadline"
        );
        assert_eq!(
            row["output"]["framesDelivered"], 192,
            "#1145 transport TS frames before and after deadline"
        );
        for frame in [
            Message::Text(
                r#"{"type":"response","id":"unrelated-rpc","ok":true,"result":{}}"#.into(),
            ),
            Message::Ping(vec![1].into()),
            Message::Pong(vec![1].into()),
        ] {
            let (mut transport, _peer, connection) = connected_pair().await;
            transport.receive_probe = Some(ReceiveProbe {
                timeout: Duration::from_millis(100),
                frame: Some(frame),
                reads: 0,
            });
            let result = operation(&mut transport, &connection, operation_name).await;
            let reads = transport
                .receive_probe
                .as_ref()
                .expect("#1145 transport ready probe retained")
                .reads;
            assert!(
                reads >= 2,
                "#1145 transport ready probe crossed deadline inside a poll"
            );
            let actual = match result {
                Err(Failure::Error(message)) => json!(message),
                other => json!(format!("{other:?}")),
            };
            assert_eq!(actual, row["output"]["message"], "{marker}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hello_ready_frames_cannot_starve_deadline() {
        ready_flood("hello", "#1145 hello fixed deadline wins over ready frames").await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_ready_frames_cannot_starve_deadline() {
        ready_flood(
            "request",
            "#1145 request fixed deadline wins over ready frames",
        )
        .await;
    }

    async fn idle_deadline(operation_name: &str) {
        let (mut transport, _peer, connection) = connected_pair().await;
        transport.receive_probe = Some(ReceiveProbe {
            timeout: Duration::from_millis(10),
            frame: None,
            reads: 0,
        });
        let result = operation(&mut transport, &connection, operation_name).await;
        let Err(Failure::Error(message)) = result else {
            panic!("#1145 transport idle must time out");
        };
        assert_eq!(
            json!(message),
            deadline_row(operation_name, "idle")["output"]["message"],
            "#1145 transport idle deadline matches TS"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hello_idle_deadline_matches_ts() {
        idle_deadline("hello").await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_idle_deadline_matches_ts() {
        idle_deadline("request").await;
    }

    async fn reply_before_deadline(operation_name: &str) {
        let (mut transport, mut peer, connection) = connected_pair().await;
        let reply = async {
            let request = peer
                .next()
                .await
                .expect("#1145 transport early reply request exists")
                .expect("#1145 transport early reply request websocket");
            let frame: Value = serde_json::from_str(
                request
                    .to_text()
                    .expect("#1145 transport early request text"),
            )
            .expect("#1145 transport early request JSON");
            let response = if operation_name == "hello" {
                json!({"type":"hello_ok", "serverInfo":{}})
            } else {
                json!({"type":"response", "id":frame["id"], "ok":true, "result":{}})
            };
            peer.send(Message::Text(response.to_string().into()))
                .await
                .expect("#1145 transport early reply send");
        };
        let (result, ()) = timeout(Duration::from_secs(5), async {
            tokio::join!(
                operation(&mut transport, &connection, operation_name),
                reply
            )
        })
        .await
        .expect("#1145 transport early reply bounded");
        assert_eq!(
            json!(result.is_ok()),
            deadline_row(operation_name, "reply-before-deadline")["output"]["resolved"],
            "#1145 transport early reply matches TS"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hello_reply_before_deadline_matches_ts() {
        reply_before_deadline("hello").await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_reply_before_deadline_matches_ts() {
        reply_before_deadline("request").await;
    }

    #[test]
    fn stream_sequence_matches_ts_boundaries() {
        let oracle: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/sdk-run-validation/oracle.json"
        ))
        .expect("#1058 sequence validation oracle");
        let rows = oracle["sequences"].as_array().expect("#1058 sequence rows");
        verify(
            rows.len() == 15,
            "#1058 registered sequence validation rows",
        );
        for case in rows {
            let input = &case["input"];
            let frame = parse_json(&input.to_string()).expect("#1058 sequence frame JSON");
            let result = next_sequence(
                &frame,
                "validation",
                input["previous"].as_f64().expect("#1058 previous sequence"),
            );
            let actual = match result {
                Ok(sequence) => json!({"sequence":sequence.map(|n| n as i64)}),
                Err(Failure::Error(message)) => json!({"error":message}),
                Err(other) => json!({"unexpected":format!("{other:?}")}),
            };
            if actual != case["output"] {
                eprintln!(
                    "#1058 sequence {}: {actual}, expected {}",
                    input["name"], case["output"]
                );
            }
            verify(actual == case["output"], "#1058 stream sequence validation");
        }
    }
}

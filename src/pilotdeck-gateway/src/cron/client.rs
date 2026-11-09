//! Shared authenticated Gateway WebSocket transport used by native CLI clients.
use super::always_on::AlwaysOnError as AutomationError;
use bytes::Bytes;
use http_body_util::Full;
use hyper_util::{
    client::legacy::Client,
    rt::{TokioExecutor, TokioIo},
};
use pilotdeck_protocol::frame::{self, Decoder};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{Mutex, broadcast, mpsc, oneshot, watch},
    task::JoinHandle,
    time::{Duration, sleep, timeout},
};

/// SDK error envelope. Keep structured business data for callers to inspect.
#[derive(Clone, Debug, PartialEq)]
pub struct GatewayClientError {
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
    pub validation: Option<Value>,
    pub request_id: Option<String>,
    pub retryable: bool,
    /// Original business error, including UTF-16 strings that Rust `String`
    /// cannot represent. The serde fields above retain their existing API.
    pub ordered_error: Option<Box<JsValue>>,
}
impl GatewayClientError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = code.into();
        Self {
            retryable: matches!(code.as_str(), "timeout" | "transport_error"),
            code,
            message: message.into(),
            details: None,
            validation: None,
            request_id: None,
            ordered_error: None,
        }
    }
}
impl std::fmt::Display for GatewayClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for GatewayClientError {}
impl From<std::io::Error> for GatewayClientError {
    fn from(error: std::io::Error) -> Self {
        Self::new("transport_error", error.to_string())
    }
}
impl From<serde_json::Error> for GatewayClientError {
    fn from(error: serde_json::Error) -> Self {
        Self::new("validation_error", error.to_string())
    }
}
impl From<GatewayClientError> for AutomationError {
    fn from(error: GatewayClientError) -> Self {
        Self::new(error.code, error.message)
    }
}
// Existing native call sites keep their spelling within this module.
type AlwaysOnError = GatewayClientError;

#[derive(Clone, Debug)]
pub struct GatewayClientOptions {
    pub client_name: String,
    pub client_version: String,
    pub protocol_version: String,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub max_attempts: usize,
    pub initial_retry_delay: Duration,
    pub max_retry_delay: Duration,
    pub retry_jitter: f64,
}

impl Default for GatewayClientOptions {
    fn default() -> Self {
        Self {
            client_name: "cli".into(),
            client_version: "0.1.0".into(),
            protocol_version: pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION.into(),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            max_attempts: 1,
            initial_retry_delay: Duration::from_millis(100),
            max_retry_delay: Duration::from_secs(2),
            retry_jitter: 0.2,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GatewayNotification {
    pub name: String,
    pub payload: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GatewayOrderedNotification {
    pub name: JsString,
    pub payload: Option<JsValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GatewayHello {
    pub protocol_version: String,
    pub server_version: String,
    pub server_info: Value,
    pub interaction_binding: Option<Value>,
}

/// Host-supplied authoritative dispatcher. Cloning the connected client shares
/// its one authenticated endpoint and never sends a second hello.
pub struct GatewayInProcessEndpoint {
    pub gateway: Arc<dyn crate::runtime::connection::ConnectionGateway>,
    pub token: String,
    pub server_version: String,
}

#[derive(Clone)]
pub struct GatewayClient {
    commands: mpsc::Sender<Command>,
    shutdown: watch::Sender<bool>,
    notifications: broadcast::Sender<GatewayNotification>,
    ordered_notifications: broadcast::Sender<GatewayOrderedNotification>,
    request_timeout: Duration,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
    hello: GatewayHello,
    ordered_hello: JsValue,
}

pub struct GatewayEventStream {
    receiver: mpsc::UnboundedReceiver<Result<Value, AlwaysOnError>>,
    // Retain the connection while its last client clone has been dropped.
    _commands: mpsc::Sender<Command>,
    _shutdown: watch::Sender<bool>,
}

impl GatewayEventStream {
    pub async fn next(&mut self) -> Option<Result<Value, AlwaysOnError>> {
        self.receiver.recv().await
    }
    /// Stop consuming locally. The wire protocol has no stream-cancel frame;
    /// callers cancel an active turn with its ordinary cancel_turn RPC.
    pub fn cancel(&mut self) {
        self.receiver.close();
    }
}
impl Drop for GatewayEventStream {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// The same authenticated stream owner as `GatewayEventStream`, with exact
/// JavaScript strings and object property order at the JSON boundary.
pub struct GatewayOrderedEventStream {
    receiver: mpsc::UnboundedReceiver<Result<JsValue, AlwaysOnError>>,
    _commands: mpsc::Sender<Command>,
    _shutdown: watch::Sender<bool>,
}
impl GatewayOrderedEventStream {
    pub async fn next(&mut self) -> Option<Result<JsValue, AlwaysOnError>> {
        self.receiver.recv().await
    }
    pub fn cancel(&mut self) {
        self.receiver.close();
    }
}
impl Drop for GatewayOrderedEventStream {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl GatewayClient {
    pub async fn connect_in_process(
        endpoint: GatewayInProcessEndpoint,
        token: &str,
        options: GatewayClientOptions,
    ) -> Result<Self, GatewayClientError> {
        let (client, server) = tokio::io::duplex(64 * 1024);
        tokio::spawn(run_in_process(endpoint, server));
        connect_io(client, token, &options).await
    }
    pub async fn connect(origin: &str, token: &str) -> Result<Self, AlwaysOnError> {
        Self::connect_with_options(origin, token, GatewayClientOptions::default()).await
    }

    pub async fn connect_with_options(
        origin: &str,
        token: &str,
        options: GatewayClientOptions,
    ) -> Result<Self, AlwaysOnError> {
        let attempts = options.max_attempts.max(1);
        let mut delay = options.initial_retry_delay;
        let max_delay = options.max_retry_delay.max(delay);
        let mut last_error = None;
        for attempt in 0..attempts {
            match connect_once(origin, token, &options).await {
                Ok(client) => return Ok(client),
                Err(error) if retryable_connect_error(&error) => last_error = Some(error),
                Err(error) => return Err(error),
            }
            if attempt + 1 < attempts {
                sleep(jittered(delay, options.retry_jitter)).await;
                delay = delay.saturating_mul(2).min(max_delay);
            }
        }
        Err(last_error.unwrap_or_else(|| {
            AlwaysOnError::new("gateway_unavailable", "Gateway connection failed.")
        }))
    }

    pub fn hello(&self) -> &GatewayHello {
        &self.hello
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<GatewayNotification> {
        self.notifications.subscribe()
    }

    pub fn hello_ordered(&self) -> &JsValue {
        &self.ordered_hello
    }

    pub fn subscribe_ordered_notifications(
        &self,
    ) -> broadcast::Receiver<GatewayOrderedNotification> {
        self.ordered_notifications.subscribe()
    }

    pub async fn request_ordered(
        &self,
        method: &str,
        params: &JsValue,
    ) -> Result<JsValue, AlwaysOnError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        let request = async {
            self.commands
                .send(Command::OrderedRequest {
                    id: id.clone(),
                    method: method.to_owned(),
                    params: params.clone(),
                    response: sender,
                })
                .await
                .map_err(|_| disconnected())?;
            receiver.await.map_err(|_| disconnected())?
        };
        match timeout(self.request_timeout, request).await {
            Ok(result) => result,
            Err(_) => {
                let mut error =
                    AlwaysOnError::new("timeout", format!("Gateway request timed out: {method}"));
                error.request_id = Some(id);
                Err(error)
            }
        }
    }

    pub async fn stream_ordered(
        &self,
        method: &str,
        params: &JsValue,
    ) -> Result<GatewayOrderedEventStream, AlwaysOnError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = mpsc::unbounded_channel();
        self.commands
            .send(Command::OrderedStream {
                id,
                method: method.to_owned(),
                params: params.clone(),
                events: sender,
            })
            .await
            .map_err(|_| disconnected())?;
        Ok(GatewayOrderedEventStream {
            receiver,
            _commands: self.commands.clone(),
            _shutdown: self.shutdown.clone(),
        })
    }

    pub async fn request(&self, method: &str, params: &Value) -> Result<Value, AlwaysOnError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        // The deadline includes queue admission. Cancellation drops the reply
        // receiver, so it never waits behind a blocked WebSocket write.
        let request = async {
            self.commands
                .send(Command::Request {
                    id: id.clone(),
                    method: method.to_owned(),
                    params: params.clone(),
                    response: sender,
                })
                .await
                .map_err(|_| disconnected())?;
            receiver.await.map_err(|_| disconnected())?
        };
        match timeout(self.request_timeout, request).await {
            Ok(result) => result,
            Err(_) => {
                let mut error =
                    AlwaysOnError::new("timeout", format!("Gateway request timed out: {method}"));
                error.request_id = Some(id);
                Err(error)
            }
        }
    }

    pub async fn stream(
        &self,
        method: &str,
        params: &Value,
    ) -> Result<GatewayEventStream, AlwaysOnError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = mpsc::unbounded_channel();
        self.commands
            .send(Command::Stream {
                id: id.clone(),
                method: method.to_owned(),
                params: params.clone(),
                events: sender,
            })
            .await
            .map_err(|_| disconnected())?;
        Ok(GatewayEventStream {
            receiver,
            _commands: self.commands.clone(),
            _shutdown: self.shutdown.clone(),
        })
    }

    pub async fn close(&self) {
        // Independent of the bounded data queue, and observed while writing.
        self.shutdown.send_replace(true);
        let mut task = self.task.lock().await;
        if let Some(task) = task.take() {
            let _ = task.await;
        }
    }
}

pub async fn request(
    origin: &str,
    token: &str,
    method: &str,
    params: &Value,
) -> Result<Value, AutomationError> {
    let client = GatewayClient::connect(origin, token).await?;
    let result = client.request(method, params).await;
    client.close().await;
    result.map_err(Into::into)
}

enum Command {
    Request {
        id: String,
        method: String,
        params: Value,
        response: oneshot::Sender<Result<Value, AlwaysOnError>>,
    },
    Stream {
        id: String,
        method: String,
        params: Value,
        events: mpsc::UnboundedSender<Result<Value, AlwaysOnError>>,
    },
    OrderedRequest {
        id: String,
        method: String,
        params: JsValue,
        response: oneshot::Sender<Result<JsValue, AlwaysOnError>>,
    },
    OrderedStream {
        id: String,
        method: String,
        params: JsValue,
        events: mpsc::UnboundedSender<Result<JsValue, AlwaysOnError>>,
    },
}

struct StreamState {
    events: EventSender,
    last_seq: i64,
}

enum EventSender {
    Legacy(mpsc::UnboundedSender<Result<Value, AlwaysOnError>>),
    Ordered(mpsc::UnboundedSender<Result<JsValue, AlwaysOnError>>),
}
impl EventSender {
    fn is_closed(&self) -> bool {
        match self {
            Self::Legacy(sender) => sender.is_closed(),
            Self::Ordered(sender) => sender.is_closed(),
        }
    }
    fn fail(&self, error: AlwaysOnError) {
        match self {
            Self::Legacy(sender) => {
                let _ = sender.send(Err(error));
            }
            Self::Ordered(sender) => {
                let _ = sender.send(Err(error));
            }
        }
    }
    fn event(&self, value: &WireValue, seq: i64) {
        match self {
            Self::Legacy(sender) => {
                let mut event = value.legacy["event"].clone();
                if !event.is_object() {
                    event = json!({});
                }
                event
                    .as_object_mut()
                    .unwrap()
                    .insert("sequence".into(), seq.into());
                let _ = sender.send(Ok(event));
            }
            Self::Ordered(sender) => {
                let mut event = ordered_event_spread(value.ordered.get("event"));
                event.insert("sequence".into(), JsValue::Number(seq as f64));
                let _ = sender.send(Ok(JsValue::Object(event)));
            }
        }
    }
}

fn ordered_event_spread(value: Option<&JsValue>) -> JsObject {
    match value {
        Some(JsValue::Object(object)) => object.clone(),
        Some(JsValue::Array(items)) => {
            let mut object = JsObject::new();
            for (index, item) in items.iter().enumerate() {
                object.insert(index.to_string().into(), item.clone());
            }
            object
        }
        Some(JsValue::String(value)) => {
            let mut object = JsObject::new();
            for (index, unit) in value.units().iter().enumerate() {
                object.insert(
                    index.to_string().into(),
                    JsValue::String(JsString::from_utf16(vec![*unit])),
                );
            }
            object
        }
        _ => JsObject::new(),
    }
}

enum ResponseSender {
    Legacy(oneshot::Sender<Result<Value, AlwaysOnError>>),
    Ordered(oneshot::Sender<Result<JsValue, AlwaysOnError>>),
}
impl ResponseSender {
    fn is_closed(&self) -> bool {
        match self {
            Self::Legacy(sender) => sender.is_closed(),
            Self::Ordered(sender) => sender.is_closed(),
        }
    }
    fn fail(self, error: AlwaysOnError) {
        match self {
            Self::Legacy(sender) => {
                let _ = sender.send(Err(error));
            }
            Self::Ordered(sender) => {
                let _ = sender.send(Err(error));
            }
        }
    }
    fn reply(self, value: &WireValue) {
        match self {
            Self::Legacy(sender) => {
                let _ = sender.send(
                    response_result(&value.legacy).map_err(|error| ordered_error(error, value)),
                );
            }
            Self::Ordered(sender) => {
                let result = if value.legacy["ok"] == true {
                    Ok(value
                        .ordered
                        .get("result")
                        .cloned()
                        .unwrap_or(JsValue::Undefined))
                } else {
                    Err(ordered_error(
                        response_result(&value.legacy).unwrap_err(),
                        value,
                    ))
                };
                let _ = sender.send(result);
            }
        }
    }
}

struct WireValue {
    ordered: JsValue,
    legacy: Value,
}
fn ordered_error(mut error: AlwaysOnError, value: &WireValue) -> AlwaysOnError {
    error.ordered_error = value.ordered.get("error").cloned().map(Box::new);
    error
}

async fn connect_once(
    origin: &str,
    token: &str,
    options: &GatewayClientOptions,
) -> Result<GatewayClient, AlwaysOnError> {
    let mut headers = hyper::HeaderMap::new();
    headers.insert(
        hyper::header::UPGRADE,
        hyper::header::HeaderValue::from_static("websocket"),
    );
    headers.insert(
        hyper::header::CONNECTION,
        hyper::header::HeaderValue::from_static("Upgrade"),
    );
    headers.insert(
        hyper::header::SEC_WEBSOCKET_KEY,
        hyper::header::HeaderValue::from_static("dGhlIHNhbXBsZSBub25jZQ=="),
    );
    headers.insert(
        hyper::header::SEC_WEBSOCKET_VERSION,
        hyper::header::HeaderValue::from_static("13"),
    );
    let accept = crate::acceptor::websocket_accept(&headers).expect("static key");
    let upgraded = timeout(options.connect_timeout, upgrade(origin, headers, &accept))
        .await
        .map_err(|_| AlwaysOnError::new("timeout", "Gateway connection timed out."))??;
    connect_io(TokioIo::new(upgraded), token, options).await
}

async fn connect_io<S>(
    mut stream: S,
    token: &str,
    options: &GatewayClientOptions,
) -> Result<GatewayClient, GatewayClientError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut decoder = Decoder::default();
    send(
        &mut stream,
        &json!({
            "type":"hello",
            "protocolVersion":options.protocol_version,
            "clientName":options.client_name,
            "clientVersion":options.client_version,
            "token":token
        }),
    )
    .await?;
    let hello = timeout(options.connect_timeout, async {
        loop {
            let value = receive_wire(&mut stream, &mut decoder).await?;
            match value.legacy["type"].as_str() {
                Some("hello_ok") => return Ok((parse_hello(&value.legacy)?, value.ordered)),
                Some("hello_error") => {
                    return Err(AlwaysOnError::new(
                        "authentication_error",
                        "Gateway authentication failed.",
                    ));
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| AlwaysOnError::new("timeout", "Gateway hello timed out."))??;
    let (hello, ordered_hello) = hello;
    let (commands, receiver) = mpsc::channel(64);
    let (shutdown, closing) = watch::channel(false);
    let (notifications, _) = broadcast::channel(64);
    let (ordered_notifications, _) = broadcast::channel(64);
    let task = Arc::new(Mutex::new(None));
    let join = tokio::spawn(run(
        stream,
        decoder,
        receiver,
        closing,
        notifications.clone(),
        ordered_notifications.clone(),
    ));
    *task.lock().await = Some(join);
    Ok(GatewayClient {
        commands,
        shutdown,
        notifications,
        ordered_notifications,
        request_timeout: options.request_timeout,
        task,
        hello,
        ordered_hello,
    })
}

struct InProcessText(mpsc::UnboundedSender<Vec<u8>>);
impl crate::runtime::connection::GatewayTextConnection for InProcessText {
    fn send_text(&self, text: String) {
        let _ = self.0.send(frame::server_frame(1, text.as_bytes()));
    }
    fn close(&self, code: u16, reason: &str) {
        let mut payload = code.to_be_bytes().to_vec();
        payload.extend_from_slice(reason.as_bytes());
        let _ = self.0.send(frame::server_frame(8, &payload));
    }
}

async fn run_in_process(endpoint: GatewayInProcessEndpoint, stream: tokio::io::DuplexStream) {
    use crate::runtime::connection::GatewayConnection;
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (send, mut outbound) = mpsc::unbounded_channel();
    let connection = Arc::new(GatewayConnection::new(
        Arc::new(InProcessText(send)),
        endpoint.gateway,
        endpoint.token,
        endpoint.server_version,
    ));
    let mut decoder = Decoder::default();
    let mut calls = tokio::task::JoinSet::new();
    let mut first_message = true;
    loop {
        tokio::select! {
            biased;
            next = outbound.recv() => {
                let Some(next) = next else { break };
                let close = next.first().is_some_and(|byte| byte & 0x0f == 8);
                if writer.write_all(&next).await.is_err() || close { break; }
            }
            read = reader.read_buf(&mut decoder.buffer) => {
                if !matches!(read, Ok(n) if n > 0) { break; }
                loop {
                    let incoming = match decoder.next() { Ok(Some(frame)) => frame, Ok(None) => break, Err(_) => { calls.abort_all(); let _ = connection.closed().await; return; } };
                    match incoming.opcode {
                        1 => {
                            let text = String::from_utf8_lossy(&incoming.payload).into_owned();
                            if std::mem::take(&mut first_message) {
                                // Authenticate before dispatching concurrent RPCs.
                                let _ = connection.receive(&text).await;
                            } else {
                                let connection = connection.clone();
                                calls.spawn(async move { let _ = connection.receive(&text).await; });
                            }
                        }
                        8 => { calls.abort_all(); let _ = connection.closed().await; return; }
                        9 => { let _ = writer.write_all(&frame::server_frame(10, &incoming.payload)).await; }
                        _ => {}
                    }
                }
            }
            Some(_) = calls.join_next(), if !calls.is_empty() => {}
        }
    }
    calls.abort_all();
    let _ = connection.closed().await;
}

fn parse_hello(value: &Value) -> Result<GatewayHello, AlwaysOnError> {
    let required = |name: &str| {
        value[name]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| AlwaysOnError::new("validation_error", "Invalid Gateway hello."))
    };
    Ok(GatewayHello {
        protocol_version: required("protocolVersion")?,
        server_version: required("serverVersion")?,
        server_info: value["serverInfo"].clone(),
        interaction_binding: value.get("interactionBinding").cloned(),
    })
}

async fn run<S>(
    stream: S,
    mut decoder: Decoder,
    mut commands: mpsc::Receiver<Command>,
    mut closing: watch::Receiver<bool>,
    notifications: broadcast::Sender<GatewayNotification>,
    ordered_notifications: broadcast::Sender<GatewayOrderedNotification>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut pending: HashMap<String, ResponseSender> = HashMap::new();
    let mut streams: HashMap<String, StreamState> = HashMap::new();
    let writing = std::sync::atomic::AtomicBool::new(false);
    let driver = async {
        loop {
            pending.retain(|_, response| !response.is_closed());
            streams.retain(|_, state| !state.events.is_closed());
            tokio::select! {
                command = commands.recv() => {
                    match command {
                        Some(Command::Request { id, method, params, response }) => {
                            if response.is_closed() { continue; }
                            let frame = json!({"type":"request","id":id,"method":method,"params":params});
                            pending.insert(id, ResponseSender::Legacy(response));
                            writing.store(true, std::sync::atomic::Ordering::SeqCst);
                            let result = send(&mut writer, &frame).await;
                            writing.store(false, std::sync::atomic::Ordering::SeqCst);
                            if let Err(error) = result { break error; }
                        }
                        Some(Command::Stream { id, method, params, events }) => {
                            if events.is_closed() { continue; }
                            let frame = json!({"type":"request","id":id,"method":method,"params":params});
                            streams.insert(id, StreamState { events: EventSender::Legacy(events), last_seq: -1 });
                            writing.store(true, std::sync::atomic::Ordering::SeqCst);
                            let result = send(&mut writer, &frame).await;
                            writing.store(false, std::sync::atomic::Ordering::SeqCst);
                            if let Err(error) = result { break error; }
                        }
                        Some(Command::OrderedRequest { id, method, params, response }) => {
                            if response.is_closed() { continue; }
                            let frame = ordered_request(&id, &method, params);
                            pending.insert(id, ResponseSender::Ordered(response));
                            writing.store(true, std::sync::atomic::Ordering::SeqCst);
                            let result = send_ordered(&mut writer, &frame).await;
                            writing.store(false, std::sync::atomic::Ordering::SeqCst);
                            if let Err(error) = result { break error; }
                        }
                        Some(Command::OrderedStream { id, method, params, events }) => {
                            if events.is_closed() { continue; }
                            let frame = ordered_request(&id, &method, params);
                            streams.insert(id, StreamState { events: EventSender::Ordered(events), last_seq: -1 });
                            writing.store(true, std::sync::atomic::Ordering::SeqCst);
                            let result = send_ordered(&mut writer, &frame).await;
                            writing.store(false, std::sync::atomic::Ordering::SeqCst);
                            if let Err(error) = result { break error; }
                        }
                        None => {
                            let close = frame::client_frame(0x88, 2, &1000u16.to_be_bytes());
                            let _ = timeout(Duration::from_millis(100), writer.write_all(&close)).await;
                            break AlwaysOnError::new("transport_error", "Gateway client closed.");
                        }
                    }
                }
                incoming = receive_read(&mut reader, &mut writer, &mut decoder, &writing) => {
                    let wire = match incoming { Ok(value) => value, Err(error) => break error };
                    let value = &wire.legacy;
                    match value["type"].as_str() {
                        Some("response") => {
                            let Some(id) = value["id"].as_str() else { continue };
                            if let Some(response) = pending.remove(id) {
                                response.reply(&wire);
                            } else if let Some(state) = streams.remove(id) {
                                if value["ok"] != true {
                                    state.events.fail(ordered_error(response_result_with_message(value, "Gateway stream failed.").unwrap_err(), &wire));
                                }
                            }
                        }
                        Some("event") => {
                            let Some(id) = value["id"].as_str() else { continue };
                            let Some(state) = streams.get_mut(id) else { continue };
                            let Some(seq) = value["seq"].as_f64().filter(|seq| seq.is_finite() && *seq >= 0.0 && seq.fract() == 0.0) else {
                                let state = streams.remove(id).expect("present");
                                state.events.fail(AlwaysOnError::new("validation_error", format!("Gateway event sequence must be a non-negative integer for {id}.")));
                                continue;
                            };
                            if seq <= state.last_seq as f64 { continue; }
                            if seq != (state.last_seq + 1) as f64 {
                                let expected = state.last_seq + 1;
                                let state = streams.remove(id).expect("present");
                                state.events.fail(AlwaysOnError::new("validation_error", format!("Gateway event sequence gap for {id}: expected {expected}, received {seq}.")));
                                continue;
                            }
                            state.last_seq = seq as i64;
                            let final_frame = value["final"] == true;
                            state.events.event(&wire, seq as i64);
                            if final_frame { streams.remove(id); }
                        }
                        Some("notification") => {
                            if let Some(name) = value["name"].as_str() {
                                let _ = notifications.send(GatewayNotification { name: name.to_owned(), payload: value.get("payload").cloned() });
                            }
                            if let Some(name) = wire.ordered.get("name").and_then(JsValue::as_string) {
                                let _ = ordered_notifications.send(GatewayOrderedNotification { name: name.clone(), payload: wire.ordered.get("payload").cloned() });
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    };
    // Dropping the driver cancels any in-progress write, including a pong.
    // Do not append a close frame after a partially written data frame.
    let failure = tokio::select! {
        biased;
        _ = closing.wait_for(|closing| *closing) => {
            AlwaysOnError::new("transport_error", "Gateway client closed.")
        }
        failure = driver => failure,
    };
    // An idle connection can perform the normal WS close handshake. A
    // cancelled partial write must terminate the transport without appending
    // a close frame in the middle of its data frame.
    if *closing.borrow() && !writing.load(std::sync::atomic::Ordering::SeqCst) {
        let close = frame::client_frame(0x88, 2, &1000u16.to_be_bytes());
        let _ = timeout(Duration::from_millis(100), writer.write_all(&close)).await;
    }
    commands.close();
    while let Ok(command) = commands.try_recv() {
        match command {
            Command::Request { response, .. } => {
                let _ = response.send(Err(failure.clone()));
            }
            Command::Stream { events, .. } => {
                let _ = events.send(Err(failure.clone()));
            }
            Command::OrderedRequest { response, .. } => {
                let _ = response.send(Err(failure.clone()));
            }
            Command::OrderedStream { events, .. } => {
                let _ = events.send(Err(failure.clone()));
            }
        }
    }
    for (_, response) in pending {
        response.fail(failure.clone());
    }
    for (_, stream) in streams {
        stream.events.fail(failure.clone());
    }
}

fn response_result(value: &Value) -> Result<Value, AlwaysOnError> {
    response_result_with_message(value, "Gateway request failed.")
}

fn response_result_with_message(value: &Value, fallback: &str) -> Result<Value, AlwaysOnError> {
    if value["ok"] == true {
        return Ok(value["result"].clone());
    }
    let mut error = AlwaysOnError::new(
        value["error"]["code"]
            .as_str()
            .map(normalize_code)
            .unwrap_or_else(|| "gateway_request_failed".into()),
        value["error"]["message"].as_str().unwrap_or(fallback),
    );
    error.details = value["error"].get("details").cloned();
    error.validation = value["error"].get("validation").cloned();
    // Business envelopes do not infer retryability from their code.
    error.retryable = false;
    Err(error)
}

fn normalize_code(code: &str) -> String {
    if code.is_empty() {
        return "gateway_request_failed".into();
    }
    match code.to_ascii_lowercase().as_str() {
        "capability_unavailable" => "unsupported_capability".into(),
        "session_not_found" | "project_not_found" => "not_found".into(),
        "session_busy" => "conflict".into(),
        "invalid_permission_mode" => "validation_error".into(),
        code if code.starts_with("invalid_") => "validation_error".into(),
        code => code.into(),
    }
}

fn disconnected() -> AlwaysOnError {
    AlwaysOnError::new("transport_error", "Gateway WebSocket closed.")
}

fn retryable_connect_error(error: &AlwaysOnError) -> bool {
    matches!(error.code.as_str(), "transport_error" | "timeout")
}

fn jittered(delay: Duration, jitter: f64) -> Duration {
    let jitter = jitter.clamp(0.0, 1.0);
    if jitter == 0.0 {
        return delay;
    }
    let random = f64::from(uuid::Uuid::new_v4().as_bytes()[0]) / f64::from(u8::MAX);
    Duration::from_millis(
        (delay.as_millis() as f64 * (1.0 + (random * 2.0 - 1.0) * jitter))
            .round()
            .max(0.0) as u64,
    )
}

async fn upgrade(
    origin: &str,
    headers: hyper::HeaderMap,
    expected_accept: &str,
) -> Result<hyper::upgrade::Upgraded, AlwaysOnError> {
    let origin = origin
        .strip_prefix("ws://")
        .map(|rest| format!("http://{rest}"))
        .or_else(|| {
            origin
                .strip_prefix("wss://")
                .map(|rest| format!("https://{rest}"))
        })
        .unwrap_or_else(|| origin.to_owned());
    let mut uri: hyper::Uri = origin
        .parse()
        .map_err(|error: hyper::http::uri::InvalidUri| {
            AlwaysOnError::new("transport_error", error.to_string())
        })?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.authority().is_none() {
        return Err(AlwaysOnError::new(
            "transport_error",
            "Gateway URL must use ws, wss, http, or https.",
        ));
    }
    if uri.path() == "/" {
        let mut parts = uri.into_parts();
        parts.path_and_query = Some(hyper::http::uri::PathAndQuery::from_static("/ws"));
        uri = hyper::Uri::from_parts(parts)
            .map_err(|error| AlwaysOnError::new("transport_error", error.to_string()))?;
    }
    let connector = crate::runtime::model::http::https_connector(
        crate::runtime::model::http::webpki_client_config(),
    );
    let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build(connector);
    let mut request = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri(uri)
        .body(Full::new(Bytes::new()))
        .map_err(|error| AlwaysOnError::new("transport_error", error.to_string()))?;
    *request.headers_mut() = headers;
    let mut response = client
        .request(request)
        .await
        .map_err(|error| AlwaysOnError::new("transport_error", error.to_string()))?;
    if response.status() != hyper::StatusCode::SWITCHING_PROTOCOLS
        || response
            .headers()
            .get(hyper::header::SEC_WEBSOCKET_ACCEPT)
            .is_none_or(|value| value.as_bytes() != expected_accept.as_bytes())
    {
        return Err(AlwaysOnError::new(
            "transport_error",
            "Gateway rejected WebSocket upgrade.",
        ));
    }
    hyper::upgrade::on(&mut response)
        .await
        .map_err(|error| AlwaysOnError::new("transport_error", error.to_string()))
}

async fn send<S: AsyncWrite + Unpin>(stream: &mut S, value: &Value) -> Result<(), AlwaysOnError> {
    let bytes = serde_json::to_vec(value)?;
    stream
        .write_all(&frame::client_frame(0x81, length_code(bytes.len()), &bytes))
        .await
        .map_err(|error| AlwaysOnError::new("transport_error", error.to_string()))
}

fn ordered_request(id: &str, method: &str, params: JsValue) -> JsValue {
    let mut object = JsObject::new();
    object.insert("type".into(), "request".into());
    object.insert("id".into(), id.into());
    object.insert("method".into(), method.into());
    object.insert("params".into(), params);
    JsValue::Object(object)
}

async fn send_ordered<S: AsyncWrite + Unpin>(
    stream: &mut S,
    value: &JsValue,
) -> Result<(), AlwaysOnError> {
    let text = pilotdeck_protocol::stringify_json(value)
        .map_err(|error| AlwaysOnError::new("validation_error", error.to_string()))?
        .ok_or_else(|| AlwaysOnError::new("validation_error", "Invalid Gateway request."))?;
    stream
        .write_all(&frame::client_frame(
            0x81,
            length_code(text.len()),
            text.as_bytes(),
        ))
        .await
        .map_err(|error| AlwaysOnError::new("transport_error", error.to_string()))
}

async fn receive_wire<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    decoder: &mut Decoder,
) -> Result<WireValue, AlwaysOnError> {
    loop {
        if let Some(value) = decode_next_tracked(decoder, stream, None).await? {
            return Ok(value);
        }
        if stream.read_buf(&mut decoder.buffer).await? == 0 {
            return Err(disconnected());
        }
    }
}

async fn receive_read<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
    decoder: &mut Decoder,
    writing: &std::sync::atomic::AtomicBool,
) -> Result<WireValue, AlwaysOnError> {
    loop {
        if let Some(value) = decode_next_tracked(decoder, writer, Some(writing)).await? {
            return Ok(value);
        }
        if reader.read_buf(&mut decoder.buffer).await? == 0 {
            return Err(disconnected());
        }
    }
}

async fn decode_next_tracked<W: AsyncWrite + Unpin>(
    decoder: &mut Decoder,
    writer: &mut W,
    writing: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Option<WireValue>, AlwaysOnError> {
    loop {
        let Some(incoming) = decoder
            .next_server()
            .map_err(|_| AlwaysOnError::new("validation_error", "Invalid gateway frame."))?
        else {
            return Ok(None);
        };
        match incoming.opcode {
            1 => {
                let raw = std::str::from_utf8(&incoming.payload).map_err(|_| {
                    AlwaysOnError::new("validation_error", "Invalid Gateway JSON frame.")
                })?;
                let ordered = pilotdeck_protocol::parse_json(raw).map_err(|_| {
                    AlwaysOnError::new("validation_error", "Invalid Gateway JSON frame.")
                })?;
                // The legacy API keeps serde's integer representations. Exact
                // consumers never pass through the lossy compatibility view.
                let legacy = serde_json::from_str(raw).unwrap_or_else(|_| js_json_lossy(&ordered));
                return Ok(Some(WireValue { ordered, legacy }));
            }
            8 => return Err(close_error(&incoming.payload)),
            9 => {
                if let Some(writing) = writing {
                    writing.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                let result = writer
                    .write_all(&frame::client_frame(
                        0x8a,
                        length_code(incoming.payload.len()),
                        &incoming.payload,
                    ))
                    .await
                    .map_err(|error| AlwaysOnError::new("transport_error", error.to_string()));
                if let Some(writing) = writing {
                    writing.store(false, std::sync::atomic::Ordering::SeqCst);
                }
                result?;
            }
            _ => {}
        }
    }
}

fn close_error(payload: &[u8]) -> AlwaysOnError {
    let code = payload
        .get(..2)
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
        .unwrap_or(0);
    let reason = String::from_utf8_lossy(payload.get(2..).unwrap_or_default());
    match (code, reason.as_ref()) {
        (4003, _) | (_, "auth_failed") => AlwaysOnError::new(
            "authentication_error",
            if reason.is_empty() {
                "Gateway WebSocket closed."
            } else {
                &reason
            },
        ),
        (_, "protocol_mismatch") => {
            AlwaysOnError::new("protocol_version_error", "protocol_mismatch")
        }
        _ => AlwaysOnError::new(
            "transport_error",
            if reason.is_empty() {
                "Gateway WebSocket closed."
            } else {
                &reason
            },
        ),
    }
}

fn js_json_lossy(value: &pilotdeck_protocol::JsValue) -> Value {
    use pilotdeck_protocol::JsValue;
    match value {
        JsValue::Undefined | JsValue::Null => Value::Null,
        JsValue::Bool(value) => Value::Bool(*value),
        JsValue::Number(value) => Value::from(*value),
        JsValue::String(value) => Value::String(String::from_utf16_lossy(value.units())),
        JsValue::Array(values) => Value::Array(values.iter().map(js_json_lossy).collect()),
        JsValue::Object(object) => Value::Object(
            object
                .own_entries()
                .into_iter()
                .filter(|(_, value)| !value.is_undefined())
                .map(|(key, value)| (String::from_utf16_lossy(key.units()), js_json_lossy(value)))
                .collect(),
        ),
    }
}

fn length_code(length: usize) -> u8 {
    match length {
        0..126 => length as u8,
        126..=65535 => 126,
        _ => 127,
    }
}

#[cfg(test)]
#[path = "client/ordered_tests.rs"]
mod ordered_tests;

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) struct EmbeddedProbe {
        pub(super) hellos: std::sync::atomic::AtomicUsize,
    }
    impl crate::runtime::connection::ConnectionGateway for EmbeddedProbe {
        fn describe_server(
            &self,
        ) -> crate::runtime::connection::Call<
            '_,
            Result<pilotdeck_protocol::JsValue, crate::runtime::connection::RequestError>,
        > {
            self.hellos
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async {
                Ok(pilotdeck_protocol::parse_json(
                    r#"{"mode":"in_process","capabilities":["turn"],"raw":"\ud800"}"#,
                )
                .unwrap())
            })
        }
        fn request(
            &self,
            method: pilotdeck_protocol::JsString,
            params: pilotdeck_protocol::JsValue,
        ) -> crate::runtime::connection::Call<
            '_,
            Result<pilotdeck_protocol::JsValue, crate::runtime::connection::RequestError>,
        > {
            Box::pin(async move {
                if method.equals_str("denied") || method.equals_str("denied_ordered") {
                    let mut error =
                        crate::runtime::connection::RequestError::new("denied by fixture");
                    error.code = "SESSION_BUSY".into();
                    let details = if method.equals_str("denied_ordered") {
                        r#"{"z":"\ud800","a":"\udc00"}"#
                    } else {
                        r#"{"owner":"other"}"#
                    };
                    error.details =
                        Some(Box::new(pilotdeck_protocol::parse_json(details).unwrap()));
                    return Err(error);
                }
                Ok(params)
            })
        }
        fn submit<'a>(
            &'a self,
            params: pilotdeck_protocol::JsValue,
            emit: &'a (dyn Fn(pilotdeck_protocol::JsValue) + Send + Sync),
        ) -> crate::runtime::connection::Call<
            'a,
            Result<(), crate::runtime::connection::RequestError>,
        > {
            Box::pin(async move {
                let mut event = JsObject::new();
                event.insert("type".into(), "input_accepted".into());
                event.insert("runId".into(), "r".into());
                event.insert("input".into(), params);
                emit(JsValue::Object(event));
                emit(
                    pilotdeck_protocol::parse_json(
                        r#"{"type":"turn_completed","runId":"r","finishReason":"stop"}"#,
                    )
                    .unwrap(),
                );
                Ok(())
            })
        }
        fn disconnect(
            &self,
            _: pilotdeck_protocol::JsValue,
            _: crate::runtime::connection::binding::WsBinding,
        ) -> crate::runtime::connection::Call<
            '_,
            Result<bool, crate::runtime::connection::RequestError>,
        > {
            Box::pin(async { Ok(false) })
        }
        fn abort_on_close(
            &self,
            _: pilotdeck_protocol::JsValue,
        ) -> crate::runtime::connection::Call<
            '_,
            Result<(), crate::runtime::connection::RequestError>,
        > {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn in_process_uses_authoritative_dispatcher_and_one_hello_for_client_clones() {
        let gateway = Arc::new(EmbeddedProbe {
            hellos: Default::default(),
        });
        let client = GatewayClient::connect_in_process(
            GatewayInProcessEndpoint {
                gateway: gateway.clone(),
                token: "fixture-token".into(),
                server_version: "test".into(),
            },
            "fixture-token",
            GatewayClientOptions {
                client_name: "sdk".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(client.hello().server_info["mode"], "in_process");
        assert!(client.hello().interaction_binding.is_some());
        let clone = client.clone();
        assert_eq!(
            clone.request("echo", &json!({"value":1})).await.unwrap(),
            json!({"value":1})
        );
        let error = clone.request("denied", &json!({})).await.unwrap_err();
        assert_eq!(error.code, "conflict");
        assert_eq!(error.details, Some(json!({"owner":"other"})));
        let mut stream = client
            .stream("submit_turn", &json!({"sessionKey":"s"}))
            .await
            .unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap()["sequence"], 0);
        let mut last = None;
        while let Some(event) = stream.next().await {
            last = Some(event.unwrap());
        }
        assert_eq!(last.unwrap()["type"], "turn_completed");
        assert_eq!(gateway.hellos.load(std::sync::atomic::Ordering::SeqCst), 1);
        client.close().await;
        assert_eq!(
            clone.request("echo", &json!({})).await.unwrap_err().code,
            "transport_error"
        );
    }

    #[tokio::test]
    async fn in_process_auth_and_protocol_failures_preserve_close_reason() {
        for (token, protocol, code, message) in [
            (
                "wrong",
                pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,
                "authentication_error",
                "auth_failed",
            ),
            (
                "fixture-token",
                "bad",
                "protocol_version_error",
                "protocol_mismatch",
            ),
        ] {
            let result = GatewayClient::connect_in_process(
                GatewayInProcessEndpoint {
                    gateway: Arc::new(EmbeddedProbe {
                        hellos: Default::default(),
                    }),
                    token: "fixture-token".into(),
                    server_version: "test".into(),
                },
                token,
                GatewayClientOptions {
                    protocol_version: protocol.into(),
                    ..Default::default()
                },
            )
            .await;
            let error = result.err().unwrap();
            assert_eq!(error.code, code);
            assert_eq!(error.message, message);
            assert!(!error.retryable);
        }
    }

    #[test]
    fn gateway_error_codes_match_sdk_normalization() {
        assert_eq!(normalize_code(""), "gateway_request_failed");
        assert_eq!(
            normalize_code("CAPABILITY_UNAVAILABLE"),
            "unsupported_capability"
        );
        assert_eq!(normalize_code("SESSION_NOT_FOUND"), "not_found");
        assert_eq!(normalize_code("SESSION_BUSY"), "conflict");
        assert_eq!(
            normalize_code("INVALID_PERMISSION_MODE"),
            "validation_error"
        );
        assert_eq!(normalize_code("custom"), "custom");
        assert!(retryable_connect_error(&AlwaysOnError::new(
            "transport_error",
            "closed"
        )));
        assert!(retryable_connect_error(&AlwaysOnError::new(
            "timeout", "late"
        )));
        assert!(!retryable_connect_error(&AlwaysOnError::new(
            "authentication_error",
            "denied"
        )));
        assert_eq!(
            jittered(Duration::from_millis(100), 0.0),
            Duration::from_millis(100)
        );
        assert_eq!(
            close_error(&[
                0x0f, 0xa3, b'a', b'u', b't', b'h', b'_', b'f', b'a', b'i', b'l', b'e', b'd'
            ])
            .code,
            "authentication_error"
        );
        assert_eq!(
            close_error(&[
                0x0f, 0xa1, b'p', b'r', b'o', b't', b'o', b'c', b'o', b'l', b'_', b'm', b'i', b's',
                b'm', b'a', b't', b'c', b'h'
            ])
            .code,
            "protocol_version_error"
        );
    }

    #[tokio::test]
    async fn connected_transport_correlates_requests_streams_notifications_and_sequence() {
        let (client_io, mut server_io) = tokio::io::duplex(16 * 1024);
        let (commands, receiver) = mpsc::channel(16);
        let (shutdown, closing) = watch::channel(false);
        let (notifications, _) = broadcast::channel(16);
        let (ordered_notifications, _) = broadcast::channel(16);
        let task = Arc::new(Mutex::new(Some(tokio::spawn(run(
            client_io,
            Decoder::default(),
            receiver,
            closing,
            notifications.clone(),
            ordered_notifications.clone(),
        )))));
        let client = GatewayClient {
            commands,
            shutdown,
            notifications,
            ordered_notifications,
            ordered_hello: JsValue::Undefined,
            request_timeout: Duration::from_secs(1),
            task,
            hello: GatewayHello {
                protocol_version: "1.2".into(),
                server_version: "test".into(),
                server_info: json!({}),
                interaction_binding: None,
            },
        };
        let mut notices = client.subscribe_notifications();
        let request_client = client.clone();
        let request =
            tokio::spawn(
                async move { request_client.request("describe_server", &json!({})).await },
            );
        let mut decoder = Decoder::default();
        let sent = loop {
            server_io.read_buf(&mut decoder.buffer).await.unwrap();
            if let Some(frame) = decoder.next().unwrap() {
                break serde_json::from_slice::<Value>(&frame.payload).unwrap();
            }
        };
        let request_id = sent["id"].as_str().unwrap();
        server_io.write_all(&frame::server_frame(1, &serde_json::to_vec(&json!({"type":"notification","name":"models_changed","payload":{"revision":2}})).unwrap())).await.unwrap();
        server_io
            .write_all(&frame::server_frame(
                1,
                &serde_json::to_vec(
                    &json!({"type":"response","id":request_id,"ok":true,"result":{"ok":true}}),
                )
                .unwrap(),
            ))
            .await
            .unwrap();
        assert_eq!(request.await.unwrap().unwrap(), json!({"ok":true}));
        assert_eq!(notices.recv().await.unwrap().name, "models_changed");

        let mut events = client.stream("submit_turn", &json!({})).await.unwrap();
        let sent = loop {
            server_io.read_buf(&mut decoder.buffer).await.unwrap();
            if let Some(frame) = decoder.next().unwrap() {
                break serde_json::from_slice::<Value>(&frame.payload).unwrap();
            }
        };
        let stream_id = sent["id"].as_str().unwrap();
        for frame_value in [
            json!({"type":"event","id":stream_id,"seq":0,"final":false,"event":{"type":"input_accepted"}}),
            json!({"type":"event","id":stream_id,"seq":2,"final":true,"event":{"type":"turn_completed"}}),
        ] {
            server_io
                .write_all(&frame::server_frame(
                    1,
                    &serde_json::to_vec(&frame_value).unwrap(),
                ))
                .await
                .unwrap();
        }
        assert_eq!(events.next().await.unwrap().unwrap()["sequence"], 0);
        assert_eq!(
            events.next().await.unwrap().unwrap_err().code,
            "validation_error"
        );
        client.close().await;
    }

    #[tokio::test]
    async fn connected_transport_correlates_multiple_out_of_order_requests() {
        let (client_io, mut server_io) = tokio::io::duplex(16 * 1024);
        let (commands, receiver) = mpsc::channel(16);
        let (shutdown, closing) = watch::channel(false);
        let (notifications, _) = broadcast::channel(16);
        let (ordered_notifications, _) = broadcast::channel(16);
        let task = Arc::new(Mutex::new(Some(tokio::spawn(run(
            client_io,
            Decoder::default(),
            receiver,
            closing,
            notifications.clone(),
            ordered_notifications.clone(),
        )))));
        let client = GatewayClient {
            commands,
            shutdown,
            notifications,
            ordered_notifications,
            ordered_hello: JsValue::Undefined,
            request_timeout: Duration::from_secs(1),
            task,
            hello: GatewayHello {
                protocol_version: "1.2".into(),
                server_version: "test".into(),
                server_info: json!({}),
                interaction_binding: None,
            },
        };
        let left_client = client.clone();
        let right_client = client.clone();
        let left = tokio::spawn(async move { left_client.request("left", &json!({})).await });
        let right = tokio::spawn(async move { right_client.request("right", &json!({})).await });
        let mut requests = HashMap::new();
        let mut decoder = Decoder::default();
        while requests.len() < 2 {
            server_io.read_buf(&mut decoder.buffer).await.unwrap();
            while let Some(frame) = decoder.next().unwrap() {
                let value: Value = serde_json::from_slice(&frame.payload).unwrap();
                requests.insert(
                    value["method"].as_str().unwrap().to_owned(),
                    value["id"].as_str().unwrap().to_owned(),
                );
            }
        }
        for (method, result) in [("right", 2), ("left", 1)] {
            server_io
                .write_all(&frame::server_frame(
                    1,
                    &serde_json::to_vec(&json!({
                        "type":"response",
                        "id":requests[method],
                        "ok":true,
                        "result":result
                    }))
                    .unwrap(),
                ))
                .await
                .unwrap();
        }
        assert_eq!(left.await.unwrap().unwrap(), 1);
        assert_eq!(right.await.unwrap().unwrap(), 2);
        client.close().await;
    }

    pub(super) fn backpressured_client(
        request_timeout: Duration,
    ) -> (GatewayClient, tokio::io::DuplexStream) {
        let (client_io, server_io) = tokio::io::duplex(1);
        let (commands, receiver) = mpsc::channel(64);
        let (shutdown, closing) = watch::channel(false);
        let (notifications, _) = broadcast::channel(16);
        let (ordered_notifications, _) = broadcast::channel(16);
        let task = Arc::new(Mutex::new(Some(tokio::spawn(run(
            client_io,
            Decoder::default(),
            receiver,
            closing,
            notifications.clone(),
            ordered_notifications.clone(),
        )))));
        (
            GatewayClient {
                commands,
                shutdown,
                notifications,
                ordered_notifications,
                ordered_hello: JsValue::Undefined,
                request_timeout,
                task,
                hello: GatewayHello {
                    protocol_version: "1.2".into(),
                    server_version: "test".into(),
                    server_info: json!({}),
                    interaction_binding: None,
                },
            },
            server_io,
        )
    }

    fn fill_request_queue(
        client: &GatewayClient,
        count: usize,
    ) -> Vec<oneshot::Receiver<Result<Value, AlwaysOnError>>> {
        (0..count)
            .map(|index| {
                let (response, receiver) = oneshot::channel();
                assert!(
                    client
                        .commands
                        .try_send(Command::Request {
                            id: format!("queued-{index}"),
                            method: "queued".into(),
                            params: json!({}),
                            response,
                        })
                        .is_ok()
                );
                receiver
            })
            .collect()
    }

    #[tokio::test]
    async fn request_deadline_survives_blocked_writer_and_full_command_queue() {
        let (client, mut server_io) = backpressured_client(Duration::from_millis(50));
        let request = {
            let client = client.clone();
            tokio::spawn(async move {
                client
                    .request("large", &json!({"body":"x".repeat(256 * 1024)}))
                    .await
            })
        };
        // Reading just one byte proves the large write started; duplex(1)
        // prevents it from completing while the peer stops reading.
        timeout(Duration::from_secs(1), server_io.read_exact(&mut [0u8; 1]))
            .await
            .unwrap()
            .unwrap();
        let _queued = fill_request_queue(&client, 64);
        assert_eq!(client.commands.capacity(), 0);
        let error = timeout(Duration::from_secs(1), request)
            .await
            .expect("a timeout must not enqueue a cancellation behind the blocked write")
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, "timeout");
        assert!(error.request_id.is_some());
        let error = timeout(
            Duration::from_secs(1),
            client.request("cannot-enter-queue", &json!({})),
        )
        .await
        .expect("queue admission must share the request deadline")
        .unwrap_err();
        assert_eq!(error.code, "timeout");
        assert_eq!(client.commands.capacity(), 0);
        timeout(Duration::from_secs(1), client.close())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn close_cancels_blocked_writer_and_fails_pending_and_queued_operations() {
        let (client, mut server_io) = backpressured_client(Duration::from_secs(60));
        let mut active_stream = client
            .stream("large", &json!({"body":"x".repeat(256 * 1024)}))
            .await
            .unwrap();
        timeout(Duration::from_secs(1), server_io.read_exact(&mut [0u8; 1]))
            .await
            .unwrap()
            .unwrap();
        let queued = fill_request_queue(&client, 63);
        let (events, mut queued_events) = mpsc::unbounded_channel();
        assert!(
            client
                .commands
                .try_send(Command::Stream {
                    id: "queued-stream".into(),
                    method: "queued".into(),
                    params: json!({}),
                    events,
                })
                .is_ok()
        );
        assert_eq!(client.commands.capacity(), 0);
        let waiting_request = {
            let client = client.clone();
            tokio::spawn(async move { client.request("waiting", &json!({})).await })
        };
        let waiting_stream = {
            let client = client.clone();
            tokio::spawn(async move { client.stream("waiting", &json!({})).await })
        };
        tokio::task::yield_now().await;
        timeout(Duration::from_secs(1), client.close())
            .await
            .expect("close must cancel the blocked writer without entering its full queue");
        assert_eq!(
            active_stream.next().await.unwrap().unwrap_err().code,
            "transport_error"
        );
        assert!(active_stream.next().await.is_none());
        for reply in queued {
            assert_eq!(reply.await.unwrap().unwrap_err().code, "transport_error");
        }
        assert_eq!(
            queued_events.recv().await.unwrap().unwrap_err().code,
            "transport_error"
        );
        assert!(queued_events.recv().await.is_none());
        assert_eq!(
            waiting_request.await.unwrap().unwrap_err().code,
            "transport_error"
        );
        assert_eq!(
            waiting_stream.await.unwrap().err().unwrap().code,
            "transport_error"
        );
    }
}

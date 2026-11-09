//! Transport-neutral channel port over the existing serial native TurnGateway.
//! Internal connections use private handshake credentials; no token/config file is read.
use super::routes::{ChannelCall, Result, RouteError};
use crate::runtime::{
    connection::GatewayTextConnection,
    turn::gateway::{HostedConnection, TurnGateway},
};
use pilotdeck_protocol::{GATEWAY_PROTOCOL_VERSION, JsObject, JsString, JsValue};
use serde_json::Value;
use std::{collections::HashMap, path::PathBuf, sync::{Arc, Mutex, Weak, atomic::{AtomicBool, Ordering}}};
use tokio::sync::{mpsc, watch};

pub trait ChannelEventSink: Send + Sync {
    fn event(&self, event: Value) -> ChannelCall<'_, ()>;
}
pub trait ChannelOrderedEventSink: Send + Sync {
    fn event_ordered(&self, event: JsValue) -> ChannelCall<'_, ()>;
}
pub trait ChannelGateway: Send + Sync {
    fn request<'a>(&'a self, method: &'a str, params: Value) -> ChannelCall<'a, Value>;
    fn turn(&self, params: Value) -> ChannelCall<'_, Vec<Value>>;
    fn stream_turn<'a>(&'a self, params: Value, sink: &'a dyn ChannelEventSink) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            for event in self.turn(params).await? { sink.event(event).await?; }
            Ok(())
        })
    }
    fn request_ordered<'a>(&'a self, method: &'a str, params: JsValue) -> ChannelCall<'a, JsValue> {
        Box::pin(async move { from_serde(&self.request(method, serde_view(&params)?).await?) })
    }
    fn stream_turn_ordered<'a>(&'a self, params: JsValue, sink: &'a dyn ChannelOrderedEventSink) -> ChannelCall<'a, ()> {
        Box::pin(async move { self.stream_turn(serde_view(&params)?, &LegacySink(sink)).await })
    }
}
fn serde_view(value: &JsValue) -> Result<Value> {
    let wire = wire(value)?;
    serde_json::from_str(&wire).map_err(|e| RouteError::new("CHANNEL_ORDERED_REQUIRED", e.to_string()))
}
fn from_serde(value: &Value) -> Result<JsValue> {
    pilotdeck_protocol::parse_json(&value.to_string())
        .map_err(|e| RouteError::new("CHANNEL_JSON_INVALID", e.to_string()))
}
fn wire(value: &JsValue) -> Result<String> {
    pilotdeck_protocol::stringify_json(value)
        .map_err(|e| RouteError::new("CHANNEL_JSON_INVALID", e.to_string()))
        .map(|value| value.unwrap_or_else(|| "null".into()))
}
struct LegacySink<'a>(&'a dyn ChannelOrderedEventSink);
impl ChannelEventSink for LegacySink<'_> {
    fn event(&self, event: Value) -> ChannelCall<'_, ()> {
        Box::pin(async move { self.0.event_ordered(from_serde(&event)?).await })
    }
}
struct SerdeSink<'a>(&'a dyn ChannelEventSink);
impl ChannelOrderedEventSink for SerdeSink<'_> {
    fn event_ordered(&self, event: JsValue) -> ChannelCall<'_, ()> {
        Box::pin(async move { self.0.event(serde_view(&event)?).await })
    }
}

type Frames = mpsc::UnboundedSender<JsValue>;
#[derive(Default)]
struct Output { pending: Mutex<HashMap<JsString, Frames>> }
impl GatewayTextConnection for Output {
    fn send_text(&self, text: String) {
        let Ok(frame) = pilotdeck_protocol::parse_json(&text) else {
            self.close(4002, "invalid_native_json"); return;
        };
        let id = if frame.get("type").and_then(JsValue::as_string)
            .is_some_and(|s| s.equals_str("hello_ok")) {
            JsString::from("hello")
        } else if let Some(id) = frame.get("id").and_then(JsValue::as_string) { id.clone() }
        else { return; }; // Connection notifications are not operation responses.
        if let Some(sender) = self.pending.lock().unwrap().get(&id) { let _ = sender.send(frame); }
    }
    fn close(&self, code: u16, reason: &str) {
        let mut error = JsObject::new();
        error.insert("code".into(), "CHANNEL_CONNECTION_CLOSED".into());
        error.insert("message".into(), format!("native channel connection closed ({code}): {reason}").as_str().into());
        let mut frame = JsObject::new();
        frame.insert("ok".into(), JsValue::Bool(false));
        frame.insert("error".into(), JsValue::Object(error));
        let frame = JsValue::Object(frame);
        for sender in self.pending.lock().unwrap().values() { let _ = sender.send(frame.clone()); }
    }
}
struct Pending { output: Arc<Output>, id: JsString }
impl Drop for Pending {
    fn drop(&mut self) { self.output.pending.lock().unwrap().remove(&self.id); }
}
struct Client {
    connection: HostedConnection,
    output: Arc<Output>,
    executor: tokio::runtime::Handle,
    closing: AtomicBool,
    closed: watch::Sender<Option<Result<()>>>,
}
impl Client {
    fn pending(self: &Arc<Self>, id: JsString) -> (Pending, mpsc::UnboundedReceiver<JsValue>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.output.pending.lock().unwrap().insert(id.clone(), sender);
        (Pending { output: self.output.clone(), id }, receiver)
    }
    fn begin_close(self: &Arc<Self>) {
        if self.closing.swap(true, Ordering::SeqCst) { return; }
        let client = self.clone();
        self.executor.spawn(async move {
            let result = client.connection.closed().await.map_err(RouteError::from);
            client.output.close(1000, "closed");
            client.closed.send_replace(Some(result));
        });
    }
    async fn close(self: &Arc<Self>) -> Result<()> {
        let mut done = self.closed.subscribe();
        self.begin_close();
        loop {
            if let Some(result) = done.borrow().clone() { return result; }
            done.changed().await.map_err(|_| RouteError::new("CHANNEL_CLOSE_FAILED", "channel cleanup stopped"))?;
        }
    }
    async fn call(self: &Arc<Self>, method: &str, params: JsValue, sink: Option<&dyn ChannelOrderedEventSink>) -> Result<JsValue> {
        if self.closing.load(Ordering::SeqCst) { return Err(closing()); }
        let id = JsString::from(uuid::Uuid::new_v4().to_string());
        let (_pending, mut frames) = self.pending(id.clone());
        let mut request = JsObject::new();
        request.insert("type".into(), "request".into());
        request.insert("id".into(), JsValue::String(id));
        request.insert("method".into(), method.into());
        request.insert("params".into(), params);
        let completion = self.connection.enqueue(wire(&JsValue::Object(request))?).await.map_err(RouteError::from)?;
        tokio::pin!(completion);
        let mut completed = false;
        let mut last_sequence = -1.0;
        loop {
            let frame = if completed {
                frames.try_recv().map_err(|_| RouteError::new("gateway_stream_ended_without_completion", "Gateway stream ended without a terminal result."))?
            } else {
                tokio::select! { biased;
                    frame = frames.recv() => frame.ok_or_else(|| RouteError::new("CHANNEL_CONNECTION_CLOSED", "native channel response channel closed"))?,
                    result = &mut completion => { result.map_err(RouteError::from)?; completed = true; continue; }
                }
            };
            if frame.get("ok") == Some(&JsValue::Bool(false)) {
                return Err(RouteError::from_ordered(frame.get("error").cloned().unwrap_or(JsValue::Undefined)));
            }
            if let Some(sink) = sink {
                let seq = frame.get("seq").and_then(JsValue::as_number)
                    .filter(|s| s.is_finite() && *s >= 0.0 && s.fract() == 0.0)
                    .ok_or_else(|| RouteError::new("validation_error", "Gateway event sequence must be a non-negative integer."))?;
                if seq <= last_sequence { continue; }
                if seq != last_sequence + 1.0 { return Err(RouteError::new("validation_error", "Gateway event sequence gap.")); }
                last_sequence = seq;
                let event = frame.get("event").cloned().unwrap_or(JsValue::Undefined);
                let mut event = event.as_object().cloned().unwrap_or_default();
                event.insert("sequence".into(), JsValue::Number(seq));
                let event = JsValue::Object(event);
                sink.event_ordered(event.clone()).await?;
                if frame.get("final") == Some(&JsValue::Bool(true)) { return Ok(event); }
            } else { return Ok(frame.get("result").cloned().unwrap_or(JsValue::Null)); }
        }
    }
}
fn closing() -> RouteError { RouteError::new("CHANNEL_HOST_CLOSED", "native channel host is closing") }
struct ClientGuard(Option<Arc<Client>>);
impl Drop for ClientGuard { fn drop(&mut self) { if let Some(client) = &self.0 { client.begin_close(); } } }
type Active = HashMap<JsString, (uuid::Uuid, Arc<Client>)>;
pub struct NativeChannelGateway {
    host: TurnGateway,
    project: PathBuf,
    active: Mutex<Active>,
    clients: Mutex<HashMap<uuid::Uuid, Weak<Client>>>,
    closing: AtomicBool,
}
impl NativeChannelGateway {
    pub fn new(host: TurnGateway, project: PathBuf) -> Arc<Self> {
        Arc::new(Self { host, project, active: Mutex::default(), clients: Mutex::default(), closing: AtomicBool::new(false) })
    }
    async fn open(&self) -> Result<Arc<Client>> {
        if self.closing.load(Ordering::SeqCst) { return Err(closing()); }
        let output = Arc::new(Output::default());
        let token = uuid::Uuid::new_v4().to_string();
        let client = Arc::new(Client {
            connection: self.host.connect(output.clone(), token.clone(), env!("CARGO_PKG_VERSION").into()),
            output, executor: tokio::runtime::Handle::current(), closing: AtomicBool::new(false),
            closed: watch::channel(None).0,
        });
        let mut guard = ClientGuard(Some(client.clone()));
        {
            let mut clients = self.clients.lock().unwrap();
            clients.retain(|_, client| client.strong_count() > 0);
            clients.insert(uuid::Uuid::new_v4(), Arc::downgrade(&client));
        }
        let (_pending, mut hello) = client.pending("hello".into());
        let mut request = JsObject::new();
        for (key, value) in [("type", "hello"), ("protocolVersion", GATEWAY_PROTOCOL_VERSION),
            ("clientName", "native-channel"), ("clientVersion", env!("CARGO_PKG_VERSION")), ("token", token.as_str())] { request.insert(key.into(), value.into()); }
        client.connection.receive(wire(&JsValue::Object(request))?).await.map_err(RouteError::from)?;
        let hello = hello.try_recv().map_err(|_| RouteError::new("CHANNEL_HELLO_FAILED", "native handshake did not produce hello_ok"))?;
        if hello.get("ok") == Some(&JsValue::Bool(false)) { return Err(RouteError::from_ordered(hello.get("error").cloned().unwrap_or(JsValue::Undefined))); }
        if self.closing.load(Ordering::SeqCst) { return Err(closing()); }
        guard.0.take(); // Ownership transfers to the request/stream guard.
        Ok(client)
    }
    fn scoped(&self, mut params: JsValue) -> Result<JsValue> {
        let JsValue::Object(ref mut params) = params else { return Err(RouteError::new("CHANNEL_PARAMS_INVALID", "channel params must be an object")); };
        if params.get("projectKey").is_none_or(|v| matches!(v, JsValue::Undefined | JsValue::Null)) {
            params.insert("projectKey".into(), self.project.to_string_lossy().as_ref().into());
        }
        Ok(JsValue::Object(params.clone()))
    }
    pub async fn close(&self) -> Result<()> {
        self.closing.store(true, Ordering::SeqCst);
        let clients: Vec<_> = self.clients.lock().unwrap().values().filter_map(Weak::upgrade).collect();
        let mut errors = Vec::new();
        for client in clients { if let Err(error) = client.close().await { errors.push(error.message); } }
        if errors.is_empty() { Ok(()) } else { Err(RouteError::new("CHANNEL_CLOSE_FAILED", errors.join("; "))) }
    }
}
struct ActiveGuard<'a> { gateway: &'a NativeChannelGateway, session: JsString, id: uuid::Uuid, client: Arc<Client> }
impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        let mut active = self.gateway.active.lock().unwrap();
        if active.get(&self.session).is_some_and(|(id, _)| *id == self.id) { active.remove(&self.session); }
        self.client.begin_close();
    }
}
impl ChannelGateway for NativeChannelGateway {
    fn request<'a>(&'a self, method: &'a str, params: Value) -> ChannelCall<'a, Value> {
        Box::pin(async move { serde_view(&self.request_ordered(method, from_serde(&params)?).await?) })
    }
    fn request_ordered<'a>(&'a self, method: &'a str, params: JsValue) -> ChannelCall<'a, JsValue> {
        Box::pin(async move {
            if self.closing.load(Ordering::SeqCst) { return Err(closing()); }
            let params = self.scoped(params)?;
            let active = params.get("sessionKey").and_then(JsValue::as_string)
                .and_then(|s| self.active.lock().unwrap().get(s).map(|(_, client)| client.clone()));
            if let Some(client) = active { return client.call(method, params, None).await; }
            let client = self.open().await?;
            let _guard = ClientGuard(Some(client.clone()));
            let result = client.call(method, params, None).await;
            let closed = client.close().await;
            match result { Ok(value) => { closed?; Ok(value) }, Err(error) => Err(error) }
        })
    }
    fn turn(&self, params: Value) -> ChannelCall<'_, Vec<Value>> {
        Box::pin(async move {
            let sink = Collect(Mutex::new(Vec::new()));
            self.stream_turn(params, &sink).await?;
            Ok(sink.0.into_inner().unwrap())
        })
    }
    fn stream_turn<'a>(&'a self, params: Value, sink: &'a dyn ChannelEventSink) -> ChannelCall<'a, ()> {
        Box::pin(async move { self.stream_turn_ordered(from_serde(&params)?, &SerdeSink(sink)).await })
    }
    fn stream_turn_ordered<'a>(&'a self, params: JsValue, sink: &'a dyn ChannelOrderedEventSink) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            let params = self.scoped(params)?;
            let session = params.get("sessionKey").and_then(JsValue::as_string).cloned()
                .filter(|s| !s.is_empty()).ok_or_else(|| RouteError::new("INVALID_SESSION_KEY", "sessionKey is required"))?;
            let client = self.open().await?;
            let _client_guard = ClientGuard(Some(client.clone()));
            let id = uuid::Uuid::new_v4();
            {
                let mut active = self.active.lock().unwrap();
                if active.contains_key(&session) { return Err(RouteError::new("session_busy", "Session already processing a request")); }
                active.insert(session.clone(), (id, client.clone()));
            }
            let _active = ActiveGuard { gateway: self, session, id, client: client.clone() };
            let result = client.call("submit_turn", params, Some(sink)).await;
            let closed = client.close().await;
            match result { Ok(_) => closed, Err(error) => Err(error) }
        })
    }
}
struct Collect(Mutex<Vec<Value>>);
impl ChannelEventSink for Collect {
    fn event(&self, event: Value) -> ChannelCall<'_, ()> {
        Box::pin(async move { self.0.lock().unwrap().push(event); Ok(()) })
    }
}

/// Shared IM mapper; HTTP transports retain their source-specific session rules.
#[derive(Clone, Default)]
pub struct ChannelSessionState {
    pub active_by_chat_id: HashMap<String, String>,
    pub project_by_chat_id: HashMap<String, String>,
}
impl ChannelSessionState {
    pub fn from_value(value: &Value) -> Self {
        let map = |key| value.get(key).and_then(Value::as_object).map(|v| v.iter()
            .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.into()))).collect()).unwrap_or_default();
        Self { active_by_chat_id: map("activeByChatId"), project_by_chat_id: map("projectByChatId") }
    }
    pub fn snapshot(&self) -> Value { serde_json::json!({"activeByChatId":self.active_by_chat_id,"projectByChatId":self.project_by_chat_id}) }
    pub fn session(&self, channel: &str, chat: &str) -> String {
        self.active_by_chat_id.get(chat).cloned().unwrap_or_else(|| format!("{channel}:chat={chat}:general"))
    }
    pub fn reset(&mut self, channel: &str, chat: &str) -> String {
        let session = format!("{channel}:chat={chat}:s_{}", uuid::Uuid::new_v4());
        self.active_by_chat_id.insert(chat.into(), session.clone()); session
    }
    pub fn project(&self, chat: &str) -> Option<&str> { self.project_by_chat_id.get(chat).map(String::as_str) }
    pub fn persist_channel_mapper(state: &Self, save: &Option<Arc<dyn Fn(Value) + Send + Sync>>) {
        if let Some(save) = save { save(state.snapshot()); }
    }
}

//! Native HTTP channel owners. TS: ApiServerChannel.ts and WebhookChannel.ts.
use super::{ChannelCall, Result, RouteError};
use crate::config::channel_commands::{ChannelEventSink, ChannelGateway};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::{Request, Response, body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::TcpListener,
    sync::watch,
    task::{JoinHandle, JoinSet},
};

mod api;
mod webhook;
pub use api::{ApiServerChannel, ApiServerOptions};
pub use webhook::{WebhookChannel, WebhookLogger, WebhookOptions};
#[cfg(test)]
mod tests;

type Body = UnsyncBoxBody<Bytes, Infallible>;
type Reply = Response<Body>;
type RequestCall = Pin<Box<dyn Future<Output = Reply> + Send>>;
trait Service: Send + Sync + 'static {
    fn request(self: Arc<Self>, request: Request<Incoming>) -> RequestCall;
}
#[derive(Default)]
struct Work(Mutex<JoinSet<()>>);
impl Work {
    fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        let mut work = self.0.lock().unwrap();
        while work.try_join_next().is_some() {}
        work.spawn(task);
    }
    fn abort(&self) {
        self.0.lock().unwrap().abort_all();
    }
    async fn close(&self) {
        let mut tasks = std::mem::take(&mut *self.0.lock().unwrap());
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
}
/// Owns the listener and every accepted connection. Closing also cancels channel
/// work; a dropped application cannot leave detached listeners or agent turns.
pub struct HttpChannelHandle {
    address: SocketAddr,
    shutdown: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<()>>>,
    work: Arc<Work>,
}
impl HttpChannelHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }
    pub async fn close(&self) {
        self.shutdown.send_replace(true);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
        self.work.close().await;
    }
}
impl Drop for HttpChannelHandle {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
        self.work.abort();
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
    }
}
async fn listen(
    host: &str,
    port: u16,
    service: Arc<dyn Service>,
    work: Arc<Work>,
) -> Result<HttpChannelHandle> {
    let listener = TcpListener::bind((host, port)).await.map_err(io_error)?;
    let address = listener.local_addr().map_err(io_error)?;
    let (shutdown, mut stopped) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                _ = stopped.changed() => break,
                accepted = listener.accept() => {
                    let Ok((socket, _)) = accepted else { break };
                    let service = service.clone();
                    let mut stopped = stopped.clone();
                    connections.spawn(async move {
                        let serve = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(socket), service_fn(move |request| {
                            let service = service.clone();
                            async move { Ok::<_, Infallible>(service.request(request).await) }
                        }));
                        tokio::pin!(serve);
                        tokio::select! { _ = stopped.changed() => {}, _ = &mut serve => {} }
                    });
                    while connections.try_join_next().is_some() {}
                }
            }
        }
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    });
    Ok(HttpChannelHandle {
        address,
        shutdown,
        task: Mutex::new(Some(task)),
        work,
    })
}
fn io_error(error: std::io::Error) -> RouteError {
    RouteError::new("CHANNEL_HTTP_FAILED", error.to_string())
}
fn response(status: u16, text: impl Into<Bytes>, content_type: &str) -> Reply {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Full::new(text.into()).boxed_unsync())
        .unwrap()
}
fn json_response(status: u16, value: Value) -> Reply {
    response(status, value.to_string(), "application/json")
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn header<'a>(request: &'a Request<Incoming>, name: &str) -> &'a str {
    request
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}
async fn read_body(mut body: Incoming, max: usize) -> std::result::Result<String, String> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| e.to_string())?;
        if let Some(data) = frame.data_ref() {
            if data.len() > max.saturating_sub(bytes.len()) {
                return Err("Error: payload too large".into());
            }
            bytes.extend_from_slice(data);
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
fn constant_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}
fn http_error(
    source: &str,
    status: u16,
    event: &str,
    code: &str,
    message: &str,
    hint: &str,
    scope: &str,
) -> Value {
    let kind = if status == 429 {
        "rate_limit_error"
    } else if status >= 500 {
        "server_error"
    } else {
        "invalid_request_error"
    };
    json!({"error":{"message":message,"type":kind,"code":code,"userHint":hint,"status":status,"scope":scope,"source":source,"event":event},"agent_status":{"type":"agent_status","event":event,"detail":{"message":message,"code":code,"severity":"error","visible":true,"userHint":hint,"scope":scope,"source":source,"status":status}}})
}
fn visible_failure(event: &Value) -> bool {
    event["type"] == "agent_status"
        && event["detail"]["visible"] != false
        && event["detail"]["severity"] == "error"
}
fn render(event: &Value, webhook: bool) -> Option<String> {
    let text = |name: &str| event[name].as_str().unwrap_or("").to_owned();
    match event["type"].as_str()? {
        "assistant_text_delta" => Some(text("text")),
        "assistant_thinking_delta" => Some(String::new()),
        "tool_call_started" => Some(format!("\n[{} running]\n", text("name"))),
        "tool_call_finished" => Some(format!(
            "\n[{} {}]\n",
            event["toolName"]
                .as_str()
                .or_else(|| event["toolCallId"].as_str())
                .unwrap_or(""),
            if event["ok"] == true {
                "done"
            } else {
                "failed"
            }
        )),
        "error" => Some(format!("\nError: {}\n", text("message"))),
        "agent_status"
            if event["detail"]["visible"] != false
                && (visible_failure(event)
                    || matches!(
                        event["event"].as_str(),
                        Some(
                            "model_empty_response_exhausted"
                                | "max_turns_reached"
                                | "max_output_recovery_exhausted"
                                | "model_request_failed"
                                | "tool_call_recovery_exhausted"
                                | "tool_error_loop"
                                | "lifecycle_blocked"
                                | "turn_failed"
                                | "turn_timeout"
                                | "gateway_submit_failed"
                                | "session_busy"
                                | "gateway_bridge_error"
                                | "gateway_stream_ended_without_completion"
                                | "channel_submit_failed"
                                | "subagent_failed"
                                | "content_filter_stop"
                                | "unknown_finish_reason"
                        )
                    )) =>
        {
            let message = event["detail"]["message"]
                .as_str()
                .filter(|v| !crate::metadata::js_trim(v).is_empty())
                .unwrap_or("Agent execution stopped before producing a complete response.");
            Some(format!(
                "\n{} {message}\n",
                if webhook { "⚠️" } else { "Error:" }
            ))
        }
        _ => None,
    }
}
#[derive(Default)]
struct RenderState {
    text: String,
    visible: bool,
}
impl RenderState {
    fn event(&mut self, event: &Value, webhook: bool) -> Option<String> {
        self.visible |= visible_failure(event);
        if event["type"] == "error" && self.visible {
            return None;
        }
        let fragment = render(event, webhook)?;
        self.text.push_str(&fragment);
        Some(fragment)
    }
}
struct BufferedSink {
    state: Mutex<RenderState>,
    webhook: bool,
}
impl ChannelEventSink for BufferedSink {
    fn event(&self, event: Value) -> ChannelCall<'_, ()> {
        Box::pin(async move {
            self.state.lock().unwrap().event(&event, self.webhook);
            Ok(())
        })
    }
}
#[derive(Default)]
struct Sessions(Mutex<HashMap<String, String>>);
impl Sessions {
    fn resolve_ordered(
        &self,
        channel: &str,
        chat: &str,
        input: &pilotdeck_protocol::JsString,
    ) -> (String, pilotdeck_protocol::JsString, bool) {
        let trim = |units: &[u16]| {
            let space = |u: &u16| {
                char::from_u32(u32::from(*u)).is_some_and(pilotdeck_protocol::js_value::js_space)
            };
            let start = units.iter().take_while(|u| space(u)).count();
            let end = units.len() - units[start..].iter().rev().take_while(|u| space(u)).count();
            units[start..end].to_vec()
        };
        let input = trim(input.units());
        let prefix: Vec<u16> = "/new".encode_utf16().collect();
        let new = input == prefix || (input.starts_with(&prefix) && input.get(4) == Some(&32));
        let mut sessions = self.0.lock().unwrap();
        if new {
            sessions.insert(
                chat.into(),
                format!("{channel}:chat={chat}:s_{}", uuid::Uuid::new_v4()),
            );
        }
        let session = sessions
            .get(chat)
            .cloned()
            .unwrap_or_else(|| format!("{channel}:chat={chat}:general"));
        let message = if new { trim(&input[4..]) } else { input };
        (
            session,
            pilotdeck_protocol::JsString::from_utf16(message),
            new,
        )
    }
    fn resolve(&self, channel: &str, chat: &str, input: &str) -> (String, String, bool) {
        let input = crate::metadata::js_trim(input);
        let new = input == "/new" || input.starts_with("/new ");
        let mut sessions = self.0.lock().unwrap();
        if new {
            sessions.insert(
                chat.into(),
                format!("{channel}:chat={chat}:s_{}", uuid::Uuid::new_v4()),
            );
        }
        (
            sessions
                .get(chat)
                .cloned()
                .unwrap_or_else(|| format!("{channel}:chat={chat}:general")),
            if new {
                crate::metadata::js_trim(&input[4..]).into()
            } else {
                input.into()
            },
            new,
        )
    }
}
struct ActiveGuard {
    chats: Arc<Mutex<HashSet<String>>>,
    chat: String,
}
impl ActiveGuard {
    fn acquire(chats: &Arc<Mutex<HashSet<String>>>, chat: &str) -> Option<Self> {
        chats.lock().unwrap().insert(chat.into()).then(|| Self {
            chats: chats.clone(),
            chat: chat.into(),
        })
    }
}
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.chats.lock().unwrap().remove(&self.chat);
    }
}

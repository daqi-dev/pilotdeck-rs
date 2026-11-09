//! Twilio SMS webhook/REST channel (SmsChannel.ts:24-341). Configuration is
//! supplied explicitly by the application; this module reads no environment.
use super::home_assistant::{
    interaction::{Chat, ImChannel, elicitation_prompt},
    render_event,
};
use super::{ChannelCall, Result, RouteError};
use crate::config::channel_commands::{ChannelEventSink, ChannelGateway, ChannelSessionState};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    convert::Infallible,
    net::SocketAddr,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, watch},
    task::{JoinHandle, JoinSet},
};
mod http;
#[cfg(test)]
mod tests;
use http::Api;
const TWIML_OK: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response></Response>";
#[derive(Clone, Debug)]
pub struct SmsOptions {
    pub account_sid: String,
    pub auth_token: String,
    pub phone_number: String,
    pub webhook_host: String,
    pub webhook_port: u16,
    pub webhook_path: String,
    pub public_url: String,
    /// Native equivalent of the Twilio SDK API origin. Explicitly injectable
    /// for loopback integration tests; the application uses the Twilio default.
    pub api_base: String,
}
impl Default for SmsOptions {
    fn default() -> Self {
        Self {
            account_sid: String::new(),
            auth_token: String::new(),
            phone_number: String::new(),
            webhook_host: "127.0.0.1".into(),
            webhook_port: 8790,
            webhook_path: "/sms/incoming".into(),
            public_url: String::new(),
            api_base: "https://api.twilio.com/2010-04-01".into(),
        }
    }
}
pub struct SmsChannel {
    options: SmsOptions,
    api: Api,
    gateway: Arc<dyn ChannelGateway>,
    mapper: StdMutex<ChannelSessionState>,
    on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    running: AtomicBool,
    work: StdMutex<JoinSet<()>>,
}
pub struct SmsHandle {
    pub address: SocketAddr,
    owner: Arc<SmsChannel>,
    shutdown: watch::Sender<bool>,
    task: StdMutex<Option<JoinHandle<()>>>,
}
impl SmsHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }
    pub async fn close(&self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
        let mut work = std::mem::take(&mut *self.owner.work.lock().unwrap());
        work.abort_all();
        while work.join_next().await.is_some() {}
        self.owner.chats.lock().unwrap().clear();
    }
}
impl Drop for SmsHandle {
    fn drop(&mut self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        self.owner.work.lock().unwrap().abort_all();
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
    }
}
impl SmsChannel {
    pub fn new(options: SmsOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Self::with_mapper(options, gateway, ChannelSessionState::default(), None)
    }
    pub fn with_mapper(
        options: SmsOptions, gateway: Arc<dyn ChannelGateway>,
        mapper: ChannelSessionState,
        on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    ) -> Arc<Self> {
        Arc::new(Self {

            options,
            api: Api::new(),
            gateway,
            mapper: StdMutex::new(mapper),
            on_state_change,
            chats: Default::default(),
            running: AtomicBool::new(false),
            work: StdMutex::new(JoinSet::new()),        })
    }
    fn persist_mapper(&self) {
        ChannelSessionState::persist_channel_mapper(
            &self.mapper.lock().unwrap(),
            &self.on_state_change,
        );
    }
    fn chat(&self, id: &str) -> Arc<Mutex<Chat>> {
        self.chats
            .lock()
            .unwrap()
            .entry(id.into())
            .or_default()
            .clone()
    }
    pub async fn start(self: &Arc<Self>) -> Result<SmsHandle> {
        if self.options.account_sid.is_empty()
            || self.options.auth_token.is_empty()
            || self.options.phone_number.is_empty()
        {
            return Err(failure(
                "Twilio account_sid, auth_token, and phone_number required",
            ));
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(failure("SMS channel already started"));
        }
        let listener = match TcpListener::bind((
            self.options.webhook_host.as_str(),
            self.options.webhook_port,
        ))
        .await
        {
            Ok(listener) => listener,
            Err(_) => {
                self.running.store(false, Ordering::SeqCst);
                return Err(failure("SMS webhook bind failed"));
            }
        };
        let address = listener
            .local_addr()
            .map_err(|_| failure("SMS webhook address unavailable"))?;
        let (shutdown, mut stopped) = watch::channel(false);
        let owner = self.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _=stopped.changed()=>break,
                    accepted=listener.accept()=>{
                        let Ok((stream,_))=accepted else{break;};let owner=owner.clone();let mut stopped=stopped.clone();
                        connections.spawn(async move{
                            let serve=hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream),service_fn(move|request|{let owner=owner.clone();async move{Ok::<_,Infallible>(owner.handle_http(request).await)}}));
                            tokio::pin!(serve);tokio::select!{_=stopped.changed()=>{},_= &mut serve=>{}}
                        });while connections.try_join_next().is_some(){}
                    }
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Ok(SmsHandle {
            address,
            owner: self.clone(),
            shutdown,
            task: StdMutex::new(Some(task)),
        })
    }
    async fn handle_http(self: &Arc<Self>, request: Request<Incoming>) -> Response<Full<Bytes>> {
        if request.method() != hyper::Method::POST
            || request.uri().path() != self.options.webhook_path
        {
            return response(404, "Not Found", "text/plain");
        }
        let json = request
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("application/json"));
        let signature = request
            .headers()
            .get("x-twilio-signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let mut body = request.into_body();
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            let Ok(frame) = frame else {
                return response(400, "Bad Request", "text/plain");
            };
            if let Some(data) = frame.data_ref() {
                if data.len() > 1_048_576usize.saturating_sub(bytes.len()) {
                    return response(400, "Bad Request: Error: payload too large", "text/plain");
                }
                bytes.extend_from_slice(data);
            }
        }
        let text = String::from_utf8_lossy(&bytes);
        let params = if json {
            match serde_json::from_str::<Value>(&text) {
                Ok(Value::Object(params)) => params
                    .into_iter()
                    .map(|(k, v)| (k, js_string(&v)))
                    .collect::<HashMap<_, _>>(),
                _ => return response(400, "Bad Request", "text/plain"),
            }
        } else {
            url::form_urlencoded::parse(text.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        };
        if !signature.is_empty()
            && !http::valid_signature(
                &self.signature_url(),
                &params,
                &self.options.auth_token,
                &signature,
            )
        {
            return response(401, "Unauthorized", "text/plain");
        }
        let from = params.get("From").cloned().unwrap_or_default();
        let body = params.get("Body").cloned().unwrap_or_default();
        if !from.is_empty() && !crate::metadata::js_trim(&body).is_empty() {
            let owner = self.clone();
            let mut work = self.work.lock().unwrap();
            while work.try_join_next().is_some() {}
            work.spawn(async move {
                let _ = owner.incoming(&from, &body).await;
            });
        }
        response(200, TWIML_OK, "text/xml")
    }
    fn signature_url(&self) -> String {
        if self.options.public_url.is_empty() {
            format!(
                "http://{}:{}{}",
                self.options.webhook_host, self.options.webhook_port, self.options.webhook_path
            )
        } else {
            format!(
                "{}{}",
                self.options
                    .public_url
                    .strip_suffix('/')
                    .unwrap_or(&self.options.public_url),
                self.options.webhook_path
            )
        }
    }
    async fn incoming(self: &Arc<Self>, chat: &str, text: &str) -> Result<()> {
        if (self.as_ref() as &dyn ImChannel).answer(chat, text).await? {
            return Ok(());
        }
        let shared = self.chat(chat);
        let mut state = shared.lock().await;
        if state.is_active() {
            return Ok(());
        }
        let text = crate::metadata::js_trim(text);
        let new = text == "/new" || text.starts_with("/new ");
        let (session, message) = {
            let mut mapper = self.mapper.lock().unwrap();
            if new {
                mapper.reset("sms", chat);
            }
            (
                mapper.session("sms", chat),
                if new {
                    crate::metadata::js_trim(&text[4..])
                } else {
                    text
                },
            )
        };
        if new {
            self.persist_mapper();
        }
        if new && message.is_empty() {
            drop(state);
            self.send_reply(chat, "已创建新会话。").await;
            return Ok(());
        }
        if message.is_empty() {
            return Ok(());
        }
        let flag = Arc::new(AtomicBool::new(true));
        state.active = Some(flag.clone());
        drop(state);
        let _active = Active(flag);
        let sink = Sink {
            owner: self.clone(),
            chat: chat.into(),
            session: session.clone(),
            text: StdMutex::new(String::new()),
        };
        let result = self
            .gateway
            .stream_turn(
                json!({"sessionKey":session,"channelKey":"sms","message":message}),
                &sink,
            )
            .await;
        self.chat(chat).lock().await.finish_turn();
        let text = if result.is_err() {
            "处理消息时发生错误，请重试。".into()
        } else {
            sink.text.lock().unwrap().clone()
        };
        if !crate::metadata::js_trim(&text).is_empty() {
            self.send_reply(chat, crate::metadata::js_trim(&text)).await;
        }
        Ok(())
    }
    async fn send_reply(&self, chat: &str, text: &str) -> bool {
        self.running.load(Ordering::SeqCst)
            && self.api.send(&self.options, chat, text).await.is_ok()
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use super::im_cron::origin_channel_matches;
        if !origin_channel_matches(delivery, "sms") {
            return Ok(false);
        }
        let session = delivery
            .get("originSessionKey")
            .filter(|v| !v.is_null())
            .unwrap_or(&delivery["sessionKey"]);
        let Some(chat) = session
            .as_str()
            .and_then(|session| session.strip_prefix("sms:chat="))
            .and_then(|suffix| suffix.rsplit_once(':'))
            .filter(|(chat, suffix)| {
                !chat.is_empty()
                    && !chat.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
                    && (*suffix == "general"
                        || suffix.strip_prefix("s_").is_some_and(|id| {
                            id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
                        }))
            })
            .map(|(chat, _)| chat)
        else {
            return Ok(false);
        };
        Ok(self
            .send_reply(chat, delivery["text"].as_str().unwrap_or(""))
            .await)
    }
}
impl ImChannel for SmsChannel {
    fn gateway(&self) -> &dyn ChannelGateway {
        self.gateway.as_ref()
    }
    fn chat(&self, chat: &str) -> Arc<Mutex<Chat>> {
        self.chat(chat)
    }
    fn send_reply<'a>(
        &'a self,
        chat: &'a str,
        text: &'a str,
    ) -> super::home_assistant::interaction::ReplyCall<'a> {
        Box::pin(self.send_reply(chat, text))
    }
}
struct Active(Arc<AtomicBool>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
struct Sink {
    owner: Arc<SmsChannel>,
    chat: String,
    session: String,
    text: StdMutex<String>,
}
impl ChannelEventSink for Sink {
    fn event(&self, event: Value) -> ChannelCall<'_, ()> {
        Box::pin(async move {
            match event["type"].as_str() {
                Some("permission_request") => {
                    (self.owner.as_ref() as &dyn ImChannel)
                        .capture_permission(&self.chat, &self.session, event)
                        .await
                }
                Some("elicitation_request") => {
                    self.owner.chat(&self.chat).lock().await.elicitation =
                        Some((self.session.clone(), event.clone()));
                    self.owner
                        .send_reply(&self.chat, &elicitation_prompt(&event))
                        .await;
                }
                _ => {
                    if let Some(fragment) = render_event(&event) {
                        self.text.lock().unwrap().push_str(&fragment);
                    }
                }
            }
            Ok(())
        })
    }
}
fn response(status: u16, text: &str, content_type: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Full::new(Bytes::copy_from_slice(text.as_bytes())))
        .unwrap()
}
fn js_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Object(_) => "[object Object]".into(),
        _ => value.to_string(),
    }
}
fn failure(message: &str) -> RouteError {
    RouteError::new("SMS_CHANNEL_FAILED", message)
}

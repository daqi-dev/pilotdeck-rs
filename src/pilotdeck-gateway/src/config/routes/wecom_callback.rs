//! WeCom encrypted callback channel (WeComCallbackChannel.ts:17-393).
//! Environment precedence is resolved by the embedding application.
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
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, watch},
    task::{JoinHandle, JoinSet},
};
mod crypto;
mod http;
#[cfg(test)]
mod tests;
use http::Api;
#[derive(Clone, Debug)]
pub struct WeComCallbackOptions {
    pub corp_id: String,
    pub agent_id: String,
    pub secret: String,
    pub token: String,
    pub encoding_aes_key: String,
    pub port: u16,
    /// Explicit bind injection; default matches TS 0.0.0.0.
    pub host: String,
    /// Explicit HTTP fixture injection; application uses official default.
    pub api_base: String,
}
impl Default for WeComCallbackOptions {
    fn default() -> Self {
        Self {
            corp_id: String::new(),
            agent_id: String::new(),
            secret: String::new(),
            token: String::new(),
            encoding_aes_key: String::new(),
            port: 8780,
            host: "0.0.0.0".into(),
            api_base: "https://qyapi.weixin.qq.com/cgi-bin".into(),
        }
    }
}
#[derive(Default)]
struct AccessToken {
    token: Option<String>,
    expires: f64,
}
pub struct WeComCallbackChannel {
    options: WeComCallbackOptions,
    api: Api,
    gateway: Arc<dyn ChannelGateway>,
    mapper: StdMutex<ChannelSessionState>,
    on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    access_token: StdMutex<AccessToken>,
    running: AtomicBool,
    work: StdMutex<JoinSet<()>>,
}
pub struct WeComCallbackHandle {
    pub address: SocketAddr,
    owner: Arc<WeComCallbackChannel>,
    shutdown: watch::Sender<bool>,
    task: StdMutex<Option<JoinHandle<()>>>,
}
impl WeComCallbackHandle {
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
impl Drop for WeComCallbackHandle {
    fn drop(&mut self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        self.owner.work.lock().unwrap().abort_all();
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
    }
}
impl WeComCallbackChannel {
    pub fn new(mut options: WeComCallbackOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Self::with_mapper(options, gateway, ChannelSessionState::default(), None)
    }
    pub fn with_mapper(
        mut options: WeComCallbackOptions,
        gateway: Arc<dyn ChannelGateway>,
        mapper: ChannelSessionState,
        on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    ) -> Arc<Self> {
        for value in [
            &mut options.corp_id,
            &mut options.agent_id,
            &mut options.secret,
            &mut options.token,
            &mut options.encoding_aes_key,
        ] {
            *value = crate::metadata::js_trim(value).into();
        }
        Arc::new(Self {
            options,
            api: Api::new(),
            gateway,
            mapper: StdMutex::new(mapper),
            on_state_change,
            chats: Default::default(),
            access_token: Default::default(),
            running: AtomicBool::new(false),
            work: StdMutex::new(JoinSet::new()),
        })
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
    pub async fn start(self: &Arc<Self>) -> Result<WeComCallbackHandle> {
        if self.options.corp_id.is_empty()
            || self.options.token.is_empty()
            || self.options.encoding_aes_key.is_empty()
            || self.options.secret.is_empty()
            || self.options.agent_id.is_empty()
        {
            return Err(failure(
                "WeCom corp_id, agent_id, secret, token and encoding_aes_key required",
            ));
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(failure("WeCom callback channel already started"));
        }
        let listener =
            match TcpListener::bind((self.options.host.as_str(), self.options.port)).await {
                Ok(listener) => listener,
                Err(_) => {
                    self.running.store(false, Ordering::SeqCst);
                    return Err(failure("WeCom callback bind failed"));
                }
            };
        let address = listener
            .local_addr()
            .map_err(|_| failure("WeCom callback address unavailable"))?;
        let (shutdown, mut stopped) = watch::channel(false);
        let owner = self.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {_=stopped.changed()=>break,accepted=listener.accept()=>{let Ok((stream,_))=accepted else{break;};let owner=owner.clone();let mut stopped=stopped.clone();connections.spawn(async move{let serve=hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream),service_fn(move|request|{let owner=owner.clone();async move{Ok::<_,Infallible>(owner.handle_http(request).await)}}));tokio::pin!(serve);tokio::select!{_=stopped.changed()=>{},_= &mut serve=>{}}});while connections.try_join_next().is_some(){}}}
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Ok(WeComCallbackHandle {
            address,
            owner: self.clone(),
            shutdown,
            task: StdMutex::new(Some(task)),
        })
    }
    async fn handle_http(self: &Arc<Self>, request: Request<Incoming>) -> Response<Full<Bytes>> {
        let query = url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
            .fold(HashMap::new(), |mut params, (key, value)| {
                params
                    .entry(key.into_owned())
                    .or_insert_with(|| value.into_owned());
                params
            });
        let query = |key: &str| query.get(key).map(String::as_str).unwrap_or("");
        let method = request.method().clone();
        if method != hyper::Method::GET && method != hyper::Method::POST {
            return response(405, "", None);
        }
        let encrypted = if method == hyper::Method::GET {
            if query("echostr").is_empty() {
                return response(400, "missing echostr", None);
            }
            query("echostr").to_owned()
        } else {
            let mut body = request.into_body();
            let mut bytes = vec![];
            while let Some(frame) = body.frame().await {
                let Ok(frame) = frame else {
                    return response(500, "error", None);
                };
                if let Some(data) = frame.data_ref() {
                    bytes.extend_from_slice(data);
                }
            }
            let text = String::from_utf8_lossy(&bytes);
            let Some(encrypted) = extract_encrypt(&text) else {
                return response(400, "no Encrypt", None);
            };
            encrypted
        };
        if crypto::signature(
            &self.options.token,
            query("timestamp"),
            query("nonce"),
            &encrypted,
        ) != query("msg_signature")
        {
            return response(403, "signature", None);
        }
        let plain = match crypto::decrypt(
            &self.options.encoding_aes_key,
            &encrypted,
            &self.options.corp_id,
        ) {
            Ok(plain) => plain,
            Err(_) => return response(500, "error", None),
        };
        if method == hyper::Method::GET {
            return response(200, &plain, Some("text/plain; charset=utf-8"));
        }
        let owner = self.clone();
        let mut work = self.work.lock().unwrap();
        while work.try_join_next().is_some() {}
        work.spawn(async move {
            let _ = owner.incoming_xml(&plain).await;
        });
        response(200, "success", Some("text/plain"))
    }
    async fn incoming_xml(self: &Arc<Self>, xml: &str) -> Result<()> {
        let kind = xml_tag(xml, "MsgType").unwrap_or_else(|| "text".into());
        let from = xml_tag(xml, "FromUserName").unwrap_or_default();
        if from.is_empty() || kind != "text" {
            return Ok(());
        }
        let text = xml_tag(xml, "Content").unwrap_or_default();
        if crate::metadata::js_trim(&text).is_empty() {
            return Ok(());
        }
        self.incoming(&from, &text).await
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
                mapper.reset("wecom_callback", chat);
            }
            (
                mapper.session("wecom_callback", chat),
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
                json!({"sessionKey":session,"channelKey":"wecom_callback","message":message}),
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
    async fn get_access_token(&self) -> Result<String> {
        {
            let cached = self.access_token.lock().unwrap();
            if cached.expires > now_ms() {
                if let Some(token) = &cached.token {
                    return Ok(token.clone());
                }
            }
        }
        let url = format!(
            "{}/gettoken?corpid={}&corpsecret={}",
            self.options.api_base,
            http::encode(&self.options.corp_id),
            http::encode(&self.options.secret)
        );
        let (ok, data) = self.api.request(&url, None, 20).await?;
        if !ok
            || data
                .get("errcode")
                .is_some_and(|code| !code.is_null() && code != 0)
        {
            return Err(failure("WeCom gettoken rejected"));
        }
        let token = data["access_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| failure("WeCom gettoken missing access_token"))?
            .to_owned();
        let seconds = data["expires_in"].as_f64().unwrap_or(7200.0);
        *self.access_token.lock().unwrap() = AccessToken {
            token: Some(token.clone()),
            expires: now_ms() + (seconds * 1000.0 - 60_000.0).min(7_000_000.0),
        };
        Ok(token)
    }
    async fn send_reply(&self, chat: &str, text: &str) -> bool {
        if !self.running.load(Ordering::SeqCst) {
            return false;
        }
        let Ok(token) = self.get_access_token().await else {
            return false;
        };
        let url = format!(
            "{}/message/send?access_token={}",
            self.options.api_base,
            http::encode(&token)
        );
        let text = String::from_utf16_lossy(&text.encode_utf16().take(2048).collect::<Vec<_>>());
        let agent = pilotdeck_protocol::js_value::string_to_number(&self.options.agent_id);
        let body = json!({"touser":chat,"msgtype":"text","agentid":agent,"text":{"content":text}});
        let Ok((ok, raw)) = self.api.request(&url, Some(body), 60).await else {
            return false;
        };
        if !ok
            || raw
                .get("errcode")
                .is_some_and(|code| !code.is_null() && code != 0)
        {
            if raw["errcode"] == 40014 || raw["errcode"] == 42001 {
                self.access_token.lock().unwrap().token = None;
            }
            return false;
        }
        true
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use super::im_cron::origin_channel_matches;
        if !origin_channel_matches(delivery, "wecom_callback") {
            return Ok(false);
        }
        let session = delivery
            .get("originSessionKey")
            .filter(|v| !v.is_null())
            .unwrap_or(&delivery["sessionKey"]);
        let Some(chat) = session
            .as_str()
            .and_then(|session| session.strip_prefix("wecom_callback:chat="))
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
impl ImChannel for WeComCallbackChannel {
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
    owner: Arc<WeComCallbackChannel>,
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
fn xml_tag(xml: &str, tag: &str) -> Option<String> {
    let re = regex::Regex::new(&format!(
        r"(?i)<{tag}><!\[CDATA\[([\s\S]*?)\]\]></{tag}>|<{tag}>([^<]*)</{tag}>"
    ))
    .ok()?;
    let matched = re.captures(xml)?;
    Some(crate::metadata::js_trim(matched.get(1).or_else(|| matched.get(2))?.as_str()).into())
}
fn extract_encrypt(xml: &str) -> Option<String> {
    let cdata = regex::Regex::new(r"(?i)<Encrypt><!\[CDATA\[([\s\S]*?)\]\]></Encrypt>")
        .ok()?
        .captures(xml)
        .and_then(|captures| {
            captures
                .get(1)
                .map(|m| crate::metadata::js_trim(m.as_str()).to_owned())
        });
    if let Some(text) = cdata.filter(|text| !text.is_empty()) {
        return Some(text);
    }
    regex::Regex::new(r"(?i)<Encrypt>([^<]+)</Encrypt>")
        .ok()?
        .captures(xml)
        .and_then(|captures| {
            captures
                .get(1)
                .map(|m| crate::metadata::js_trim(m.as_str()).into())
        })
}
fn response(status: u16, text: &str, content_type: Option<&str>) -> Response<Full<Bytes>> {
    let mut response = Response::builder().status(status);
    if let Some(content_type) = content_type {
        response = response.header("content-type", content_type);
    }
    response
        .body(Full::new(Bytes::copy_from_slice(text.as_bytes())))
        .unwrap()
}
fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0
}
fn failure(message: &str) -> RouteError {
    RouteError::new("WECOM_CALLBACK_FAILED", message)
}

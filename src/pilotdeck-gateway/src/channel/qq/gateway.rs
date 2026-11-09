//! First-party qqbot-gateway.ts rewritten in Rust: no Node or TS helper.
use super::object;
use crate::channel::{
    http_client::{OutboundClient, UNDICI_DEFAULT_TIMEOUT},
    protocol::{ChannelError, ChannelLog},
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{Mutex, mpsc, watch},
    time::Instant,
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
#[derive(Clone)]
pub struct GatewayOptions {
    pub app_id: JsString,
    pub client_secret: JsString,
    pub api_base: String,
    pub token_url: String,
}
impl Default for GatewayOptions {
    fn default() -> Self {
        Self {
            app_id: JsString::default(),
            client_secret: JsString::default(),
            api_base: "https://api.sgroup.qq.com".into(),
            token_url: "https://bots.qq.com/app/getAppAccessToken".into(),
        }
    }
}
#[derive(Default)]
struct TokenState {
    token: Option<JsString>,
    expires: Option<Instant>,
    refresh: Option<Instant>,
}
pub struct QQBotGateway {
    options: GatewayOptions,
    http: OutboundClient,
    token: Mutex<TokenState>,
    token_changed: tokio::sync::Notify,
}
impl QQBotGateway {
    pub fn new(options: GatewayOptions) -> Self {
        Self {
            options,
            http: Client::builder(TokioExecutor::new()).build(
                crate::runtime::model::http::https_connector(
                    crate::runtime::model::http::webpki_client_config(),
                ),
            ),
            token: Mutex::new(TokenState::default()),
            token_changed: tokio::sync::Notify::new(),
        }
    }
    async fn request(
        &self,
        method: &str,
        url: &str,
        body: Option<JsValue>,
        token: Option<&JsString>,
    ) -> Result<(u16, String), ChannelError> {
        let mut request = Request::builder().method(method).uri(url);
        if let Some(token) = token {
            request = request.header(
                "Authorization",
                format!(
                    "QQBot {}",
                    token
                        .to_utf8()
                        .map_err(|_| ChannelError::error("Invalid QQ token"))?
                ),
            );
        }
        let body = if let Some(body) = body {
            request = request.header("Content-Type", "application/json");
            stringify_json(&body)
                .map_err(|e| ChannelError::error(e.to_string()))?
                .unwrap_or_default()
        } else {
            String::new()
        };
        let request = request
            .body(Full::new(Bytes::from(body)))
            .map_err(|e| ChannelError::error(e.to_string()))?;
        tokio::time::timeout(UNDICI_DEFAULT_TIMEOUT, async {
            let response = self
                .http
                .request(request)
                .await
                .map_err(|e| ChannelError::error(e.to_string()))?;
            let status = response.status().as_u16();
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(|e| ChannelError::error(e.to_string()))?
                .to_bytes();
            Ok((status, String::from_utf8_lossy(&body).into_owned()))
        })
        .await
        .map_err(|_| ChannelError::error("QQ HTTP request timed out"))?
    }
    pub async fn ensure_access_token(&self) -> Result<JsString, ChannelError> {
        let mut state = self.token.lock().await;
        let now = Instant::now();
        if let (Some(token), Some(expires)) = (&state.token, state.expires)
            && expires.saturating_duration_since(now) > Duration::from_secs(120)
        {
            return Ok(token.clone());
        }
        let (status, text) = self
            .request(
                "POST",
                &self.options.token_url,
                Some(object([
                    ("appId", JsValue::String(self.options.app_id.clone())),
                    (
                        "clientSecret",
                        JsValue::String(self.options.client_secret.clone()),
                    ),
                ])),
                None,
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(ChannelError::error(format!(
                "getAppAccessToken failed: {status} {text}"
            )));
        }
        let data = parse_json(&text).map_err(|e| ChannelError::error(e.to_string()))?;
        let token = data
            .get("access_token")
            .and_then(JsValue::as_string)
            .cloned()
            .ok_or_else(|| ChannelError::error("QQ token response has no access_token"))?;
        let expires =
            crate::js_value::to_number(data.get("expires_in").unwrap_or(&JsValue::Undefined))
                .map_err(|_| ChannelError::error("Invalid QQ token expiry"))?;
        if !expires.is_finite() || expires < 0.0 || expires > 31_536_000.0 {
            return Err(ChannelError::error("Invalid QQ token expiry"));
        }
        let now = Instant::now();
        state.expires = now.checked_add(Duration::from_secs_f64(expires));
        state.refresh = now.checked_add(Duration::from_secs_f64((expires - 300.0).max(0.001)));
        state.token = Some(token.clone());
        self.token_changed.notify_waiters();
        Ok(token)
    }
    pub async fn prepare(&self) -> Result<String, ChannelError> {
        let token = self.ensure_access_token().await?;
        let (status, text) = self
            .request(
                "GET",
                &format!("{}/gateway", self.options.api_base),
                None,
                Some(&token),
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(ChannelError::error(format!("getGateway failed: {status}")));
        }
        parse_json(&text)
            .ok()
            .and_then(|v| {
                v.get("url")
                    .and_then(JsValue::as_string)
                    .and_then(|s| s.to_utf8().ok())
            })
            .ok_or_else(|| ChannelError::error("QQ gateway response has no URL"))
    }
    pub async fn send(
        &self,
        c2c: bool,
        recipient: &JsString,
        text: &JsString,
        msg_id: Option<&JsString>,
        msg_seq: Option<usize>,
    ) -> Result<(), ChannelError> {
        let token = self.ensure_access_token().await?;
        let mut body = super::object_map([
            ("content", JsValue::String(text.clone())),
            ("msg_type", JsValue::Number(0.0)),
        ]);
        if let Some(id) = msg_id.filter(|id| !id.is_empty()) {
            body.insert("msg_id".into(), JsValue::String(id.clone()));
            body.insert(
                "msg_seq".into(),
                JsValue::Number(msg_seq.unwrap_or(1) as f64),
            );
        }
        let kind = if c2c { "users" } else { "groups" };
        let recipient = String::from_utf16_lossy(recipient.units());
        let (status, text) = self
            .request(
                "POST",
                &format!("{}/v2/{kind}/{recipient}/messages", self.options.api_base),
                Some(JsValue::Object(body)),
                Some(&token),
            )
            .await?;
        if !(200..300).contains(&status) {
            let method = if c2c {
                "sendC2CMessage"
            } else {
                "sendGroupMessage"
            };
            return Err(ChannelError::error(format!(
                "{method} failed: {status} {text}"
            )));
        }
        Ok(())
    }
    async fn identify(
        &self,
        session: Option<&JsString>,
        seq: Option<f64>,
    ) -> Result<JsValue, ChannelError> {
        let state = self.token.lock().await;
        let token = state
            .token
            .as_ref()
            .ok_or_else(|| ChannelError::error("QQ token unavailable"))?;
        let token = JsString::from(format!("QQBot {}", String::from_utf16_lossy(token.units())));
        let (op, data) = if let Some(session) = session {
            (
                6.0,
                object([
                    ("token", JsValue::String(token)),
                    ("session_id", JsValue::String(session.clone())),
                    ("seq", seq.map_or(JsValue::Null, JsValue::Number)),
                ]),
            )
        } else {
            (
                2.0,
                object([
                    ("token", JsValue::String(token)),
                    ("intents", JsValue::Number(33_554_432.0)),
                    (
                        "shard",
                        JsValue::Array(vec![JsValue::Number(0.0), JsValue::Number(1.0)]),
                    ),
                    (
                        "properties",
                        object([
                            ("$os", "linux".into()),
                            ("$browser", "pilotdeck".into()),
                            ("$device", "pilotdeck".into()),
                        ]),
                    ),
                ]),
            )
        };
        Ok(object([("op", JsValue::Number(op)), ("d", data)]))
    }
    pub async fn refresh_loop(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        loop {
            let changed = self.token_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let refresh = self.token.lock().await.refresh;
            tokio::select! {biased;
                _=stop.changed()=>return,
                _=&mut changed=>continue,
                _=async{if let Some(at)=refresh{tokio::time::sleep_until(at).await;}else{std::future::pending::<()>().await;}}=>{
                    self.token.lock().await.refresh=None;
                    // Baseline's one-shot timer swallows refresh failures.
                    tokio::select!{biased;_=stop.changed()=>return,_=self.ensure_access_token()=>()}

                }
            }
        }
    }
    pub async fn run(
        self: Arc<Self>,
        initial: String,
        events: mpsc::Sender<JsValue>,
        mut stop: watch::Receiver<bool>,
        log: Option<ChannelLog>,
    ) {
        let emit = |level: &str, text: &str| {
            if let Some(log) = &log {
                log(level, text);
            }
        };
        let mut endpoint = initial;
        let mut session: Option<JsString> = None;
        let mut sequence: Option<f64> = None;
        loop {
            let connection = tokio::select! {biased;_=stop.changed()=>return,result=connect_async(endpoint.as_str())=>result};
            match connection {
                Ok((mut socket, _)) => {
                    emit("info", "qq: WebSocket connected to QQ Gateway");
                    let mut heartbeat: Option<tokio::time::Interval> = None;
                    let mut heartbeat_ms = 41250.0;
                    let mut identify_at: Option<Instant> = None;
                    loop {
                        tokio::select! {biased;
                         _=stop.changed()=>{let _=tokio::time::timeout(Duration::from_secs(2),socket.close(None)).await;return;},
                         _=async{if let Some(at)=identify_at{tokio::time::sleep_until(at).await;}else{std::future::pending::<()>().await;}}=>{
                          identify_at=None;if let Ok(value)=self.identify(None,None).await&& let Some(text)=stringify_json(&value).ok().flatten()&& socket.send(Message::Text(text.into())).await.is_err(){break;}
                         },
                         _=async{if let Some(timer)=heartbeat.as_mut(){timer.tick().await;}else{std::future::pending::<()>().await;}}=>{
                          let beat=object([("op",JsValue::Number(1.0)),("d",sequence.map_or(JsValue::Null,JsValue::Number))]);if let Some(text)=stringify_json(&beat).ok().flatten()&& socket.send(Message::Text(text.into())).await.is_err(){break;}
                         },
                         frame=socket.next()=>{
                          let raw=match frame{Some(Ok(Message::Text(text)))=>text.to_string(),Some(Ok(Message::Binary(bytes)))=>String::from_utf8_lossy(&bytes).into_owned(),Some(Ok(Message::Ping(bytes)))=>{if socket.send(Message::Pong(bytes)).await.is_err(){break;}continue;},Some(Ok(Message::Close(_)))|None=>break,Some(Err(e))=>{emit("error",&format!("qq: error: {e}"));break;},_=>continue};let Ok(value)=parse_json(&raw)else{continue;};
                          if let Some(JsValue::Number(seq))=value.get("s"){sequence=Some(*seq);}
                          match value.get("op"){
                           Some(JsValue::Number(op)) if *op==10.0=>{
                            if let Some(JsValue::Number(ms))=value.get("d").and_then(|d|d.get("heartbeat_interval"))&& ms.is_finite()&&*ms>=1.0&&*ms<=86_400_000.0{heartbeat_ms = *ms;}
                            if let Ok(identify)=self.identify(session.as_ref(),sequence).await{let text=stringify_json(&identify).ok().flatten().expect("identify");if socket.send(Message::Text(text.into())).await.is_err(){break;}}
                           },
                           Some(JsValue::Number(op)) if *op==0.0=>{
                            let kind=value.get("t").and_then(JsValue::as_string);let data=value.get("d").cloned().unwrap_or(JsValue::Undefined);
                            let raw=stringify_json(&data).ok().flatten().unwrap_or_else(||"undefined".into());let units=JsString::from(raw);let short=String::from_utf16_lossy(&units.units()[..units.units().len().min(200)]);emit("info",&format!("qq: [dispatch] type={} data={short}",kind.map(|s|String::from_utf16_lossy(s.units())).unwrap_or_else(||"undefined".into())));
                            if kind.is_some_and(|s|s.equals_str("READY")||s.equals_str("RESUMED")){
                             if kind.is_some_and(|s|s.equals_str("READY")){session=data.get("session_id").and_then(JsValue::as_string).cloned();emit("info","qq: authenticated, listening for group messages");}else{emit("info","qq: session resumed");}
                             let interval=Duration::from_secs_f64(heartbeat_ms/1000.0);heartbeat=Some(tokio::time::interval_at(Instant::now()+interval,interval));
                            }else if kind.is_some_and(|s|s.equals_str("GROUP_AT_MESSAGE_CREATE")||s.equals_str("C2C_MESSAGE_CREATE")){
                             let message=object([("c2c",JsValue::Bool(kind.is_some_and(|s|s.equals_str("C2C_MESSAGE_CREATE")))),("data",data)]);tokio::select!{biased;_=stop.changed()=>{let _=tokio::time::timeout(Duration::from_secs(2),socket.close(None)).await;return;},sent=events.send(message)=>if sent.is_err(){return;}}
                            }
                           },
                           Some(JsValue::Number(op)) if *op==7.0=>{let _=socket.close(None).await;break;},
                           Some(JsValue::Number(op)) if *op==9.0=>{session=None;sequence=None;identify_at=Some(Instant::now()+Duration::from_secs(2));},_=>(),
                          }
                         }
                        }
                    }
                }
                Err(e) => emit("error", &format!("qq: error: {e}")),
            }
            emit("warn", "qq: disconnected, will reconnect...");
            tokio::select! {biased;_=stop.changed()=>return,_=tokio::time::sleep(Duration::from_secs(5))=>{}};
            emit("info", "qq: reconnecting...");
            match self.prepare().await {
                Ok(url) => endpoint = url,
                Err(e) => {
                    emit("error", &format!("qq: error: {e}"));
                    return;
                }
            }
        }
    }
}

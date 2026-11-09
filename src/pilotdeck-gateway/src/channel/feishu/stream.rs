//! Lark pbbp2 binary websocket transport (SDK 1.65.0 wire contract).
//! Endpoint discovery, fragmented events, control heartbeat, correlated ACK and owned close.
use super::{FeishuOptions, Runtime};
use crate::channel::http_client::HttpClient;
use futures_util::{SinkExt, StreamExt};
use pilotdeck_protocol::{JsValue, parse_json};
use std::{
    collections::HashMap,
    sync::Weak,
    time::{Duration, Instant},
};
use tokio::{sync::watch, task::JoinHandle};
use tokio_tungstenite::{connect_async, tungstenite::Message};
#[derive(Clone, Debug, Default)]
pub(super) struct Frame {
    pub seq: u64,
    pub log: u64,
    pub service: u64,
    pub method: u64,
    pub headers: Vec<(String, String)>,
    pub encoding: Option<String>,
    pub kind: Option<String>,
    pub payload: Vec<u8>,
    pub log_new: Option<String>,
}
fn varint(bytes: &[u8], i: &mut usize) -> Result<u64, String> {
    let mut n = 0;
    for shift in (0..70).step_by(7) {
        let b = *bytes.get(*i).ok_or("truncated protobuf varint")?;
        *i += 1;
        if shift == 63 && b > 1 {
            return Err("protobuf varint overflow".into());
        }
        n |= ((b & 127) as u64) << shift;
        if b < 128 {
            return Ok(n);
        }
    }
    Err("protobuf varint overflow".into())
}
fn field(bytes: &[u8], i: &mut usize, wire: u64) -> Result<Vec<u8>, String> {
    let len = match wire {
        2 => usize::try_from(varint(bytes, i)?).map_err(|_| "protobuf length overflow")?,
        1 => 8,
        5 => 4,
        0 => {
            varint(bytes, i)?;
            return Ok(vec![]);
        }
        _ => return Err("unsupported protobuf wire type".into()),
    };
    let end = i
        .checked_add(len)
        .filter(|n| *n <= bytes.len())
        .ok_or("truncated protobuf field")?;
    let out = bytes[*i..end].to_vec();
    *i = end;
    Ok(out)
}
fn put(n: u64, out: &mut Vec<u8>) {
    let mut n = n;
    while n >= 128 {
        out.push(n as u8 | 128);
        n >>= 7;
    }
    out.push(n as u8);
}
fn string(number: u64, bytes: &[u8], out: &mut Vec<u8>) {
    put(number * 8 + 2, out);
    put(bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}
impl Frame {
    pub(super) fn decode(bytes: &[u8]) -> Result<Self, String> {
        let mut out = Self::default();
        let mut seen = 0;
        let mut i = 0;
        while i < bytes.len() {
            let tag = varint(bytes, &mut i)?;
            let number = tag >> 3;
            let wire = tag & 7;
            if number == 0 {
                return Err("invalid protobuf field zero".into());
            }
            match number {
                1..=4 if wire == 0 => {
                    let n = varint(bytes, &mut i)?;
                    seen |= 1 << (number - 1);
                    match number {
                        1 => out.seq = n,
                        2 => out.log = n,
                        3 => out.service = n,
                        _ => out.method = n,
                    }
                }
                5 if wire == 2 => {
                    let bytes = field(bytes, &mut i, wire)?;
                    let mut j = 0;
                    let (mut key, mut value) = (None, None);
                    while j < bytes.len() {
                        let tag = varint(&bytes, &mut j)?;
                        let raw = field(&bytes, &mut j, tag & 7)?;
                        if tag >> 3 == 1 {
                            key = Some(String::from_utf8_lossy(&raw).into_owned())
                        } else if tag >> 3 == 2 {
                            value = Some(String::from_utf8_lossy(&raw).into_owned())
                        }
                    }
                    out.headers.push((
                        key.ok_or("missing header key")?,
                        value.ok_or("missing header value")?,
                    ));
                }
                6..=9 if wire == 2 => {
                    let raw = field(bytes, &mut i, wire)?;
                    match number {
                        6 => out.encoding = Some(String::from_utf8_lossy(&raw).into_owned()),
                        7 => out.kind = Some(String::from_utf8_lossy(&raw).into_owned()),
                        8 => out.payload = raw,
                        _ => out.log_new = Some(String::from_utf8_lossy(&raw).into_owned()),
                    }
                }
                _ => {
                    field(bytes, &mut i, wire)?;
                }
            }
        }
        if seen != 15 {
            return Err("missing required protobuf frame field".into());
        }
        Ok(out)
    }
    pub(super) fn encode(&self) -> Vec<u8> {
        let mut out = vec![];
        for (number, value) in [
            (1, self.seq),
            (2, self.log),
            (3, self.service),
            (4, self.method),
        ] {
            put(number * 8, &mut out);
            put(value, &mut out);
        }
        for (key, value) in &self.headers {
            let mut header = vec![];
            string(1, key.as_bytes(), &mut header);
            string(2, value.as_bytes(), &mut header);
            string(5, &header, &mut out);
        }
        if let Some(s) = &self.encoding {
            string(6, s.as_bytes(), &mut out);
        }
        if let Some(s) = &self.kind {
            string(7, s.as_bytes(), &mut out);
        }
        if !self.payload.is_empty() {
            string(8, &self.payload, &mut out);
        }
        if let Some(s) = &self.log_new {
            string(9, s.as_bytes(), &mut out);
        }
        out
    }
    fn header(&self, key: &str) -> Option<&str> {
        self.headers
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}
fn valid_app_id(value: &str) -> bool {
    value
        .strip_prefix("cli_")
        .is_some_and(|tail| tail.len() == 16 && tail.bytes().all(|b| b.is_ascii_hexdigit()))
}
struct Config {
    url: String,
    service: u64,
    ping: Duration,
    reconnect: Duration,
    attempts: i64,
    nonce: Duration,
}
async fn discover(options: &FeishuOptions) -> Result<Config, String> {
    let body =
        serde_json::json!({"AppID":options.app_id,"AppSecret":options.app_secret}).to_string();
    let response = HttpClient::new()
        .request(
            "POST",
            &format!("{}/callback/ws/endpoint", options.base_url),
            Some("application/json"),
            &body,
            Duration::from_secs(15),
        )
        .await?;
    if !response.is_ok() {
        return Err(format!(
            "pullConnectConfig failed: HTTP {}",
            response.status
        ));
    }
    let value = parse_json(&String::from_utf8_lossy(&response.body)).map_err(|e| e.to_string())?;
    if value.get("code").and_then(JsValue::as_number) != Some(0.0) {
        return Err(format!(
            "pullConnectConfig failed: {}",
            String::from_utf16_lossy(crate::channel::sms::js_string_of(&value).units())
        ));
    }
    let data = value.get("data").ok_or("missing WS config data")?;
    let url = data
        .get("URL")
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok())
        .ok_or("missing WS URL")?;
    let parsed = url::Url::parse(&url).map_err(|e| e.to_string())?;
    let service = parsed
        .query_pairs()
        .find(|(k, _)| k == "service_id")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let config = data.get("ClientConfig");
    let number = |key, default| {
        config
            .and_then(|c| c.get(key))
            .and_then(JsValue::as_number)
            .filter(|n| n.is_finite())
            .unwrap_or(default)
    };
    let seconds =
        |n: f64| Duration::from_millis((n.max(0.001) * 1000.0).min(u64::MAX as f64) as u64);
    Ok(Config {
        url,
        service,
        ping: seconds(number("PingInterval", 120.0)),
        reconnect: seconds(number("ReconnectInterval", 2.0)),
        attempts: number("ReconnectCount", -1.0) as i64,
        nonce: seconds(number("ReconnectNonce", 0.001)),
    })
}
struct Parts {
    started: Instant,
    chunks: Vec<Option<Vec<u8>>>,
}
pub(super) struct StreamHandle {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}
impl StreamHandle {
    pub(super) async fn start(
        options: FeishuOptions,
        runtime: Weak<Runtime>,
    ) -> Result<Self, String> {
        // Actual installed SDK 1.65.0 validates before pullConnectConfig.
        // Invalid fixture/user credentials must never contact the cloud endpoint.
        if !valid_app_id(&options.app_id) {
            return Err(format!("invalid appId: {}", options.app_id));
        }
        let config = discover(&options).await?;
        let (socket, _) = tokio::time::timeout(Duration::from_secs(20), connect_async(&config.url))
            .await
            .map_err(|_| "Feishu WS connect timed out")?
            .map_err(|e| e.to_string())?;
        let (stop, mut stopped) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut socket = socket;
            let mut config = config;
            let mut cache: HashMap<String, Parts> = HashMap::new();
            loop {
                let mut heartbeat = tokio::time::interval(config.ping);
                heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {biased;
                    _=stopped.changed()=>{let _=tokio::time::timeout(Duration::from_secs(2),socket.close(None)).await;return;},
                    _=heartbeat.tick()=>{let ping=Frame{service:config.service,headers:vec![("type".into(),"ping".into())],..Default::default()};if socket.send(Message::Binary(ping.encode().into())).await.is_err(){break;}},
                    frame=socket.next()=>{
                    let bytes=match frame{Some(Ok(Message::Binary(bytes)))=>bytes,Some(Ok(Message::Ping(bytes)))=>{let _=socket.send(Message::Pong(bytes)).await;continue},Some(Ok(Message::Close(_)))|None|Some(Err(_))=>break,_=>continue};
                    let Ok(mut frame)=Frame::decode(&bytes)else{continue};
                    if frame.method==0{
                        if frame.headers.iter().find(|(key,_)|key=="type").is_some_and(|(_,value)|value=="pong")&&!frame.payload.is_empty()
                            && let Ok(value)=parse_json(&String::from_utf8_lossy(&frame.payload)){
                                if let Some(interval)=value.get("PingInterval").and_then(JsValue::as_number).filter(|n|n.is_finite()&&*n>=0.0){
                                    config.ping=Duration::from_millis(((interval*1000.0)as u64).max(1));heartbeat=tokio::time::interval(config.ping);heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                                }
                                if let Some(count)=value.get("ReconnectCount").and_then(JsValue::as_number).filter(|n|n.is_finite()){config.attempts=count as i64;}
                                if let Some(interval)=value.get("ReconnectInterval").and_then(JsValue::as_number).filter(|n|n.is_finite()&&*n>=0.0){config.reconnect=Duration::from_millis((interval*1000.0)as u64);}
                                if let Some(nonce)=value.get("ReconnectNonce").and_then(JsValue::as_number).filter(|n|n.is_finite()&&*n>=0.0){config.nonce=Duration::from_millis((nonce*1000.0)as u64);}
                            }continue;
                    }
                    if frame.method!=1||frame.header("type")!=Some("event"){continue;}
                    cache.retain(|_,p|p.started.elapsed()<Duration::from_secs(120));
                    let sum=frame.header("sum").and_then(|v|v.parse::<usize>().ok()).unwrap_or(1);
                    let seq=frame.header("seq").and_then(|v|v.parse::<usize>().ok()).unwrap_or(0);
                    if sum==0||sum>4096||seq>=sum{continue;}
                    let id=frame.header("message_id").unwrap_or("").to_owned();
                    if cache.len()>=1024&&!cache.contains_key(&id){continue;}
                    let parts=cache.entry(id.clone()).or_insert_with(||Parts{started:Instant::now(),chunks:vec![None;sum]});
                    if parts.chunks.len()!=sum{continue;}parts.chunks[seq]=Some(frame.payload.clone());
                    if parts.chunks.iter().any(Option::is_none){continue;}
                    let parts=cache.remove(&id).unwrap();let payload=parts.chunks.into_iter().flatten().flatten().collect::<Vec<_>>();
                    let start=Instant::now();
                    if let Some(runtime)=runtime.upgrade(){if let Ok(value)=parse_json(&String::from_utf8_lossy(&payload)){runtime.handle_stream(value);}}
                    else{return;}
                    let elapsed=start.elapsed().as_millis();
                    frame.headers.push(("biz_rt".into(),if elapsed==0{"0".into()}else{format!("-{elapsed}")}));
                    frame.payload=br#"{"code":200}"#.to_vec();
                    if socket.send(Message::Binary(frame.encode().into())).await.is_err(){break;}
                    }}
                }
                let mut attempts = 0;
                loop {
                    if config.attempts >= 0 && attempts >= config.attempts {
                        return;
                    }
                    attempts += 1;
                    let nonce = if attempts == 1 {
                        config.nonce
                    } else {
                        Duration::ZERO
                    };
                    tokio::select! {_=stopped.changed()=>return,_=tokio::time::sleep(config.reconnect+nonce)=>{}}
                    let next = tokio::select! {_=stopped.changed()=>return,value=discover(&options)=>value};
                    let Ok(next) = next else {
                        continue;
                    };
                    let connected = tokio::select! {_=stopped.changed()=>return,value=tokio::time::timeout(Duration::from_secs(20),connect_async(&next.url))=>value};
                    if let Ok(Ok((next_socket, _))) = connected {
                        socket = next_socket;
                        config = next;
                        break;
                    }
                }
            }
        });
        Ok(Self { stop, task })
    }
    pub(super) async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

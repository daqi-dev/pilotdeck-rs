//! SDK 1.65.0 WSClient protocol, fixed TS FeishuChannel.ts:237-303.
use super::*;
mod media;
mod proto;
mod socket;
pub(super) use media::MediaInput;
#[cfg(test)]
mod tests;
use proto::Frame;
use socket::Socket;
use tokio::time::{Instant, sleep, sleep_until};

#[derive(Clone)]
struct Config {
    endpoint: String,
    service: u64,
    ping: Duration,
    reconnect_count: i64,
    reconnect: Duration,
    nonce: Duration,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            service: 0,
            ping: Duration::from_secs(120),
            reconnect_count: -1,
            reconnect: Duration::from_secs(120),
            nonce: Duration::from_secs(30),
        }
    }
}
impl Config {
    fn update(&mut self, value: &Value) {
        let timer = |key| {
            Duration::from_millis(pilotdeck_protocol::timer_delay_ms(
                value[key].as_f64().unwrap_or(f64::NAN) * 1000.0,
            ))
        };
        self.ping = timer("PingInterval");
        self.reconnect = timer("ReconnectInterval");
        self.nonce = Duration::from_millis(
            value["ReconnectNonce"]
                .as_f64()
                .filter(|n| n.is_finite() && *n > 0.0)
                .map_or(0, |n| (n * 1000.0) as u64),
        );
        self.reconnect_count = value["ReconnectCount"].as_i64().unwrap_or(0);
    }
}
enum Pull {
    Ready(Config),
    Retry,
    Fatal,
}
async fn pull(owner: &FeishuChannel, previous: &Config) -> Pull {
    let mut base = match url::Url::parse(&owner.options.api_base) {
        Ok(base) => base,
        Err(_) => return Pull::Fatal,
    };
    base.set_path("/callback/ws/endpoint");
    if owner.options.domain_name == "lark" && base.host_str() == Some("open.feishu.cn") {
        let _ = base.set_host(Some("open.larksuite.com"));
    }
    base.set_query(None);
    let request = owner.api.call(
        "POST",
        base.as_str(),
        None,
        Some(json!({"AppID":owner.options.app_id,"AppSecret":owner.options.app_secret})),
    );
    let Ok(Ok((_, response))) = tokio::time::timeout(Duration::from_secs(15), request).await else {
        return Pull::Retry;
    };
    let code = response["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        return if code == 1_000_040_343 {
            Pull::Retry
        } else {
            Pull::Fatal
        };
    }
    let Some(endpoint) = response["data"]["URL"].as_str() else {
        return Pull::Retry;
    };
    let Ok(url) = url::Url::parse(endpoint) else {
        return Pull::Retry;
    };
    let mut config = previous.clone();
    config.endpoint = endpoint.into();
    config.service = url
        .query_pairs()
        .find(|(key, _)| key == "service_id")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    config.update(&response["data"]["ClientConfig"]);
    Pull::Ready(config)
}

struct Parts {
    created: Instant,
    parts: Vec<Option<Vec<u8>>>,
}
#[derive(Default)]
struct Cache(HashMap<String, Parts>);
impl Cache {
    fn merge(&mut self, frame: &Frame) -> Result<Option<Value>> {
        self.0
            .retain(|_, parts| parts.created.elapsed() <= Duration::from_secs(10));
        let Some(id) = frame.header("message_id") else {
            return Err(failure("Feishu event lacks message_id"));
        };
        let sum = frame
            .header("sum")
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| *n > 0 && *n <= 16_777_216)
            .ok_or_else(|| failure("Invalid Feishu event fragment count"))?;
        let seq = frame
            .header("seq")
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| *n < sum)
            .ok_or_else(|| failure("Invalid Feishu event fragment sequence"))?;
        let parts = self.0.entry(id.into()).or_insert_with(|| Parts {
            created: Instant::now(),
            parts: vec![None; sum],
        });
        if seq >= parts.parts.len() {
            return Err(failure("Invalid Feishu event fragment sequence"));
        }
        parts.parts[seq] = Some(frame.payload.clone().unwrap_or_default());
        if parts.parts.iter().any(Option::is_none) {
            return Ok(None);
        }
        let parts = self.0.remove(id).unwrap();
        let data: Vec<u8> = parts.parts.into_iter().flatten().flatten().collect();
        serde_json::from_slice(&data)
            .map(Some)
            .map_err(|_| failure("Invalid Feishu event JSON"))
    }
}

pub(super) async fn run(owner: Arc<FeishuChannel>, mut stopped: watch::Receiver<bool>) {
    // Exact SDK start validation. Invalid IDs leave webhook mode available.
    let app = &owner.options.app_id;
    if app.len() != 20
        || !app.starts_with("cli_")
        || !app[4..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        eprintln!("feishu: invalid stream appId");
        return;
    }
    let mut config = Config::default();
    let mut cache = Cache::default();
    let mut retry = false;
    let mut attempts = 0i64;
    loop {
        if *stopped.borrow() {
            return;
        }
        if retry {
            let delay = if attempts == 0 {
                let random = u16::from_be_bytes([
                    uuid::Uuid::new_v4().as_bytes()[0],
                    uuid::Uuid::new_v4().as_bytes()[1],
                ]);
                config.nonce.mul_f64(f64::from(random) / 65536.0)
            } else {
                config.reconnect
            };
            tokio::select! {biased;_ = stopping(&mut stopped)=>return,_=sleep(delay)=>{}}
            attempts += 1;
        }
        let pulled = tokio::select! {biased;_=stopping(&mut stopped)=>return,pulled=pull(&owner,&config)=>pulled};
        match pulled {
            Pull::Fatal => {
                eprintln!(
                    "feishu: stream endpoint rejected credentials/configuration; webhook remains available"
                );
                return;
            }
            Pull::Retry => {
                if retry && config.reconnect_count >= 0 && attempts >= config.reconnect_count {
                    return;
                }
                retry = true;
                continue;
            }
            Pull::Ready(next) => config = next,
        }
        let connected = tokio::select! {biased;_=stopping(&mut stopped)=>return,connected=Socket::connect(&config.endpoint)=>connected};
        let Ok(mut socket) = connected else {
            if retry && config.reconnect_count >= 0 && attempts >= config.reconnect_count {
                return;
            }
            retry = true;
            continue;
        };
        attempts = 0;
        let mut next_ping = Instant::now();
        loop {
            tokio::select! {biased;
                _=stopping(&mut stopped)=>{socket.close().await;return;},
                _=sleep_until(next_ping)=>{
                    let frame=Frame {service:config.service,headers:vec![("type".into(),"ping".into())],..Default::default()};
                    let wire=frame.encode();
                    let sent=tokio::select! {biased;_=stopping(&mut stopped)=>return,sent=socket.binary(&wire)=>sent};
                    if sent.is_err() {break;}
                    next_ping=Instant::now()+config.ping;
                },
                message=socket.next()=>{
                    let Ok(Some(message))=message else {break;};
                    let Ok(mut frame)=Frame::decode(&message) else {continue;};
                    if frame.method==0 {
                        if frame.header("type")==Some("pong") {
                            if let Some(payload)=frame.payload.as_ref().and_then(|payload|serde_json::from_slice::<Value>(payload).ok()) {config.update(&payload);}
                        }
                    } else if frame.method==1 && frame.header("type")==Some("event") {
                        let data=match cache.merge(&frame) {Ok(Some(data))=>data,Ok(None)=>continue,Err(_)=>continue};
                        let started=Instant::now();
                        accept_stream(&owner,&data);
                        frame.headers.push(("biz_rt".into(),format!("-{}",started.elapsed().as_millis())));
                        if frame.headers.last().is_some_and(|(_,value)|value=="-0") {frame.headers.last_mut().unwrap().1="0".into();}
                        frame.payload=Some(br#"{"code":200}"#.to_vec());
                        let wire=frame.encode();
                        let sent=tokio::select! {biased;_=stopping(&mut stopped)=>return,sent=socket.binary(&wire)=>sent};
                        if sent.is_err() {break;}
                    }
                }
            }
        }
        retry = true;
    }
}

fn accept_stream(owner: &Arc<FeishuChannel>, raw: &Value) {
    // EventDispatcher accepts v2 envelopes and passes event fields to the
    // callback; no webhook verification token check occurs on this transport.
    if raw["header"]["event_type"]
        .as_str()
        .is_some_and(|kind| kind != "im.message.receive_v1")
    {
        return;
    }
    let event = raw.get("event").unwrap_or(raw);
    let message = event
        .get("message")
        .or_else(|| raw.get("message"))
        .unwrap_or(&Value::Null);
    if message["message_type"] != "text" {
        if let Some(input) = MediaInput::from_raw(raw, true) {
            owner.accept_media(input);
        }
        return;
    }
    let Some(chat) = message["chat_id"].as_str().filter(|s| !s.is_empty()) else {
        return;
    };
    let Some(content) = message["content"].as_str() else {
        return;
    };
    let text = serde_json::from_str::<Value>(content)
        .map(|value| value["text"].as_str().unwrap_or("").to_owned())
        .unwrap_or_else(|_| content.into());
    let message_id = message["message_id"].as_str().map(str::to_owned);
    owner.accept(Inbound {
        chat: chat.into(),
        text,
        event_id: message_id
            .clone()
            .unwrap_or_else(|| format!("stream:{chat}:{}", now_ms())),
        message_id,
    });
}

pub(super) enum BatchInput {
    Text(Inbound),
    Media(MediaInput),
}
impl BatchInput {
    fn chat(&self) -> &str {
        match self {
            Self::Text(input) => &input.chat,
            Self::Media(input) => &input.chat,
        }
    }
}
#[derive(Default)]
pub(super) struct Batch {
    pub messages: VecDeque<BatchInput>,
    epoch: u64,
    draining: bool,
}
pub(super) fn enqueue(owner: &Arc<FeishuChannel>, input: BatchInput) {
    let owner = owner.clone();
    let mut work = owner.work.lock().unwrap();
    while work.try_join_next().is_some() {}
    let task_owner = owner.clone();
    work.spawn(async move {
        let text = match &input {
            BatchInput::Text(input) => input.text.as_str(),
            BatchInput::Media(_) => "",
        };
        // TS tryHandlePendingInteraction runs before batching. A reply cannot
        // wait behind a pending attachment download on this chat's drainer.
        if (task_owner.as_ref() as &dyn ImChannel)
            .answer(input.chat(), text)
            .await
            .unwrap_or(true)
        {
            return;
        }
        enqueue_unanswered(&task_owner, input);
    });
}
fn enqueue_unanswered(owner: &Arc<FeishuChannel>, input: BatchInput) {
    let delay = if matches!(input, BatchInput::Media(_)) {
        250
    } else {
        0
    };
    let chat = input.chat().to_owned();
    let epoch = {
        let mut batches = owner.batches.lock().unwrap();
        let batch = batches.entry(chat.clone()).or_default();
        batch.messages.push_back(input);
        batch.epoch = batch.epoch.wrapping_add(1);
        batch.epoch
    };
    let owner = owner.clone();
    let mut work = owner.work.lock().unwrap();
    while work.try_join_next().is_some() {}
    let task_owner = owner.clone();
    work.spawn(async move {
        sleep(Duration::from_millis(delay)).await;
        drain(task_owner, chat, epoch).await;
    });
}
async fn drain(owner: Arc<FeishuChannel>, chat: String, epoch: u64) {
    {
        let mut batches = owner.batches.lock().unwrap();
        let Some(batch) = batches.get_mut(&chat) else {
            return;
        };
        if batch.draining || batch.epoch != epoch {
            return;
        }
        batch.draining = true;
    }
    loop {
        let mut messages = {
            let mut batches = owner.batches.lock().unwrap();
            let Some(batch) = batches.get_mut(&chat) else {
                return;
            };
            std::mem::take(&mut batch.messages)
        };
        while let Some(input) = messages.pop_front() {
            if !owner.running.load(Ordering::SeqCst) {
                return;
            }
            match input {
                BatchInput::Text(input) => {
                    let _ = owner.incoming(input).await;
                }
                BatchInput::Media(input) => {
                    let mut group = vec![input];
                    while matches!(messages.front(), Some(BatchInput::Media(_))) {
                        if let Some(BatchInput::Media(input)) = messages.pop_front() {
                            group.push(input);
                        }
                    }
                    media::accept_batch(owner.clone(), group).await;
                }
            }
        }
        let mut batches = owner.batches.lock().unwrap();
        let Some(batch) = batches.get(&chat) else {
            return;
        };
        if batch.messages.is_empty() {
            batches.remove(&chat);
            return;
        }
    }
}
pub(super) async fn send_attachment(owner: &FeishuChannel, chat: &str, attachment: &Value) -> bool {
    media::send(owner, chat, attachment).await
}
pub(super) fn webhook_media(body: &str, options: &FeishuOptions) -> Option<MediaInput> {
    let mut raw: Value = serde_json::from_str(body).ok()?;
    if let (Some(encrypted), Some(key)) = (raw["encrypt"].as_str(), options.encrypt_key.as_deref())
    {
        let plain = decrypt_media(encrypted, key)?;
        raw = serde_json::from_slice(&plain).ok()?;
    }
    if let Some(token) = options.verify_token.as_deref().filter(|s| !s.is_empty()) {
        let provided = raw
            .get("token")
            .filter(|v| !v.is_null())
            .or_else(|| raw["header"].get("token"));
        if provided.is_some_and(|v| !v.as_str().unwrap_or("").is_empty() && v != token) {
            return None;
        }
    }
    MediaInput::from_raw(&raw, false)
}
fn decrypt_media(value: &str, key: &str) -> Option<Vec<u8>> {
    use aes::{
        Aes256,
        cipher::{BlockDecrypt, KeyInit},
    };
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .ok()?;
    if bytes.len() < 32 || bytes.len() % 16 != 0 {
        return None;
    }
    let cipher = Aes256::new_from_slice(&Sha256::digest(key.as_bytes())).ok()?;
    let mut previous = [0; 16];
    previous.copy_from_slice(&bytes[..16]);
    let mut out = Vec::new();
    for part in bytes[16..].chunks_exact(16) {
        let mut block = aes::Block::default();
        block.copy_from_slice(part);
        cipher.decrypt_block(&mut block);
        out.extend(block.iter().zip(previous).map(|(a, b)| a ^ b));
        previous.copy_from_slice(part);
    }
    let padding = usize::from(*out.last()?);
    if padding == 0
        || padding > 16
        || out.len() < padding
        || !out[out.len() - padding..]
            .iter()
            .all(|b| usize::from(*b) == padding)
    {
        return None;
    }
    out.truncate(out.len() - padding);
    Some(out)
}

async fn stopping(stopped: &mut watch::Receiver<bool>) {
    let _ = stopped.wait_for(|value| *value).await;
}

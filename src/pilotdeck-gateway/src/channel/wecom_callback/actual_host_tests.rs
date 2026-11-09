//! Actual native Server/A7/sidecar with loopback WeCom callback REST and model.
use super::channels::ChannelEndpointOverrides;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[derive(Default)]
struct Wire {
    generation: AtomicUsize,
    phase: AtomicUsize,
    sent: Mutex<Vec<Value>>,
    models: Mutex<Vec<Value>>,
}
impl Wire {
    fn contains(&self, s: &str) -> bool {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .any(|v| v.to_string().contains(s))
    }
}
struct Peer {
    origin: String,
    state: Arc<Wire>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Peer {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "WeCom callback sent={:#?}; models={:#?}",
                self.state.sent.lock().unwrap_or_else(|e| e.into_inner()),
                self.state.models.lock().unwrap_or_else(|e| e.into_inner())
            );
        }
        self.task.abort();
    }
}
async fn until(state: &Wire, label: &str, mut test: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(25), async {
        while !test() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "{label}; sent={:#?}; models={:#?}",
            state.sent.lock().unwrap_or_else(|e| e.into_inner()),
            state.models.lock().unwrap_or_else(|e| e.into_inner())
        )
    });
}
async fn peer(approved: std::path::PathBuf) -> Peer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(Wire::default());
    let shared = state.clone();
    let task = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                        incoming=listener.accept()=>{let(mut socket,_)=incoming.unwrap();let state=shared.clone();let approved=approved.clone();tasks.spawn(async move{
                        let mut raw=vec![];while !raw.ends_with(b"\r\n\r\n"){let mut byte=[0];if socket.read_exact(&mut byte).await.is_err(){return}raw.push(byte[0]);assert!(raw.len()<65536);}
                        let header=String::from_utf8(raw).unwrap();let path=header.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned();let size=header.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length:").map(|v|v.trim().parse::<usize>().unwrap())).unwrap_or(0);let mut raw=vec![0;size];if socket.read_exact(&mut raw).await.is_err(){return}
                        let (status,mime,text)=if path.starts_with("/cgi-bin/gettoken"){("200 OK","application/json",json!({"access_token":"mock-access-token","expires_in":7200}).to_string())
            }else if path.starts_with("/cgi-bin/message/send"){let body:Value=serde_json::from_slice(&raw).unwrap();state.sent.lock().unwrap().push(body);("200 OK","application/json",json!({"errcode":0,"errmsg":"ok"}).to_string())
            }else{let body:Value=serde_json::from_slice(&raw).unwrap_or(Value::Null);if body["stream"]==true{
                        state.models.lock().unwrap().push(body);let phase=state.phase.fetch_add(1,Ordering::SeqCst);let generation=state.generation.load(Ordering::SeqCst);
                        let(delta,finish)=match phase{
                        0=>(json!({"role":"assistant","tool_calls":[{"index":0,"id":"wecom-callback-permission","type":"function","function":{"name":"bash","arguments":json!({"command":format!("printf approved > '{}'",approved.display())}).to_string()}}]}),"tool_calls"),
                        2=>(json!({"role":"assistant","tool_calls":[{"index":0,"id":"wecom-callback-question","type":"function","function":{"name":"ask_user_question","arguments":json!({"questions":[{"header":"Choice","question":"WeCom callback choose mock option","options":[{"label":"One","description":"Mock one"},{"label":"Two","description":"Mock two"}],"multiSelect":false}]}).to_string()}}]}),"tool_calls"),
                        _=>(json!({"role":"assistant","content":format!("wecom-callback-actual-final-{generation}-{phase}")}),"stop")};
                        ("200 OK","text/event-stream",format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",json!({"id":"mock","choices":[{"index":0,"delta":delta,"finish_reason":null}]}),json!({"id":"mock","choices":[{"index":0,"delta":{},"finish_reason":finish}]})))
                        }else{("200 OK","application/json",json!({"choices":[{"message":{"role":"assistant","content":"{\"title\":\"WeCom callback actual\"}"},"finish_reason":"stop"}]}).to_string())}};
                        let reply=format!("HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",text.len());let _=socket.write_all(reply.as_bytes()).await;
                        });},Some(_)=tasks.join_next(),if !tasks.is_empty()=>{}
                        }
        }
    });
    Peer {
        origin,
        state,
        task,
    }
}
fn envelope(text: &str) -> (String, String) {
    use aes::Aes256;
    use base64::Engine;
    use cbc::cipher::{BlockEncryptMut, KeyIvInit, block_padding::NoPadding};
    use sha1::{Digest, Sha1};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let id = NEXT.fetch_add(1, Ordering::SeqCst);
    let key = [7u8; 32];
    let xml = format!(
        "<xml><ToUserName><![CDATA[mock-corp]]></ToUserName><FromUserName><![CDATA[alice]]></FromUserName><CreateTime>1700000000</CreateTime><MsgType><![CDATA[text]]></MsgType><Content><![CDATA[{text}]]></Content><MsgId>{id}</MsgId></xml>"
    );
    let mut bytes = vec![3u8; 16];
    bytes.extend_from_slice(&(xml.len() as u32).to_be_bytes());
    bytes.extend_from_slice(xml.as_bytes());
    bytes.extend_from_slice(b"mock-corp");
    let padding = 32 - bytes.len() % 32;
    bytes.extend(std::iter::repeat_n(padding as u8, padding));
    let len = bytes.len();
    let encrypted = cbc::Encryptor::<Aes256>::new_from_slices(&key, &key[..16])
        .unwrap()
        .encrypt_padded_mut::<NoPadding>(&mut bytes, len)
        .unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(encrypted);
    let mut parts = [
        "mock-callback-token",
        "1700000000",
        "mock-nonce",
        encoded.as_str(),
    ];
    parts.sort();
    let digest = Sha1::digest(parts.concat().as_bytes());
    let signature = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    (encoded, signature)
}
async fn incoming(port: u16, text: &str) -> Value {
    let (encoded, signature) = envelope(text);
    let body = format!("<xml><Encrypt><![CDATA[{encoded}]]></Encrypt></xml>");
    let request = format!(
        "POST /wecom/callback?msg_signature={signature}&timestamp=1700000000&nonce=mock-nonce HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match crate::channel::sms::raw_request(port, request.as_bytes(), false).await {
                Ok(v) => break v,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    })
    .await
    .expect("configured callback listener readiness")
}
#[derive(Default)]
struct Frames(Mutex<Vec<Value>>);
impl crate::runtime::connection::GatewayTextConnection for Frames {
    fn send_text(&self, text: String) {
        self.0
            .lock()
            .unwrap()
            .push(serde_json::from_str(&text).unwrap());
    }
    fn close(&self, _code: u16, _reason: &str) {}
}
#[tokio::test]
async fn wecom_callback_actual_server_a7_permission_elicitation_public_reload_shutdown() {
    let (_scratch, requests) = super::management_tests::setup();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    let approved = requests.options.project.join("wecom-callback-approved.txt");
    let peer = peer(approved.clone()).await;
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let path = requests.options.home.join("pilotdeck.yaml");
    let base = std::fs::read_to_string(&path)
        .unwrap()
        .replace("http://127.0.0.1:1", &format!("{}/v1", peer.origin));
    let aes_key = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .encode([7u8; 32])
            .trim_end_matches('=')
            .to_owned()
    };
    let config = |enabled| {
        format!(
            "{base}adapters:\n  wecomCallback:\n    enabled: {enabled}\n    token: mock-callback-token\n    extra:\n      corpId: mock-corp\n      agentId: '1000001'\n      secret: mock-corp-secret\n      encodingAesKey: {aes_key}\n      port: {port}\n"
        )
    };
    std::fs::write(&path, config(true)).unwrap();
    let mut options = requests.options.clone();
    options.sidecar = std::env::var_os("PDRS_CHANNEL_SIDECAR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("pilotdeck-agent-loop-sidecar")
        });
    let bytes = std::fs::read(&options.sidecar).expect("real sidecar required");
    assert!(bytes.len() > 1024);
    use sha2::Digest;
    eprintln!(
        "WeCom callback actual sidecar={} sha256={:x}; port reservation is a bounded bind/drop diagnostic seam",
        options.sidecar.display(),
        sha2::Sha256::digest(&bytes)
    );
    let (host, _channels) = super::assembly::start_with_channel_endpoints(
        options.clone(),
        requests.env.clone(),
        ChannelEndpointOverrides {
            wecom_callback_api_base: Some(format!("{}/cgi-bin", peer.origin)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(incoming(port, "permission-start").await["status"], 200);
    until(
        &peer.state,
        "actual A7 permission request -> WeCom callback",
        || peer.state.contains("回复 1 允许一次"),
    )
    .await;
    assert!(!approved.exists(), "permission gate must hold tool");
    assert_eq!(incoming(port, "1").await["status"], 200);
    until(
        &peer.state,
        "same A7 resumes from WeCom callback decision",
        || peer.state.contains("wecom-callback-actual-final-0-1"),
    )
    .await;
    assert_eq!(std::fs::read_to_string(&approved).unwrap(), "approved");
    assert!(peer.state.contains("已允许一次"));
    {
        let models = peer.state.models.lock().unwrap();
        assert!(
            models[1]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["role"] == "tool")
        );
    }
    assert_eq!(incoming(port, "elicitation-start").await["status"], 200);
    until(
        &peer.state,
        "actual ask_user_question -> WeCom callback",
        || peer.state.contains("WeCom callback choose mock option"),
    )
    .await;
    assert_eq!(incoming(port, "1").await["status"], 200);
    until(
        &peer.state,
        "same A7 resumes from WeCom callback elicitation",
        || peer.state.contains("wecom-callback-actual-final-0-3"),
    )
    .await;
    assert!(
        peer.state.models.lock().unwrap()[3]
            .to_string()
            .contains("One")
    );
    let frames = Arc::new(Frames::default());
    let connection = host.connect(frames.clone(), "token".into(), "fixture".into());
    connection.receive(json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"wecom-callback-lifecycle"}).to_string()).await.unwrap();
    std::fs::write(&path, config(false)).unwrap();
    connection
        .receive(
            json!({"type":"request","id":"disable-wecom-callback","method":"reload_config","params":{}})
                .to_string(),
        )
        .await
        .unwrap();
    until(&peer.state, "public reload response", || {
        frames
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|v| v["id"] == "disable-wecom-callback" && v["ok"] == true)
    })
    .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("disabled WeCom callback listener closes");
    let count = peer.state.sent.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(count, peer.state.sent.lock().unwrap().len());
    peer.state.generation.store(1, Ordering::SeqCst);
    std::fs::write(&path, config(true)).unwrap();
    connection
        .receive(
            json!({"type":"request","id":"enable-wecom-callback","method":"reload_config","params":{}})
                .to_string(),
        )
        .await
        .unwrap();
    assert_eq!(incoming(port, "reload-start").await["status"], 200);
    until(
        &peer.state,
        "fresh WeCom callback adapter still uses real A7",
        || peer.state.contains("wecom-callback-actual-final-1-4"),
    )
    .await;
    connection.closed().await.unwrap();
    host.shutdown(options.drain_budget).await.unwrap();
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
    );
    let count = peer.state.sent.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(count, peer.state.sent.lock().unwrap().len());
    let mut logs = String::new();
    fn collect(p: &std::path::Path, out: &mut String) {
        for row in std::fs::read_dir(p).unwrap() {
            let p = row.unwrap().path();
            if p.is_dir() {
                collect(&p, out)
            } else if p.extension().is_some_and(|e| e == "jsonl")
                && let Ok(s) = std::fs::read_to_string(p)
            {
                out.push_str(&s)
            }
        }
    }
    collect(&options.home.join("projects"), &mut logs);
    for marker in [
        "wecom-callback-actual-final-0-1",
        "wecom-callback-actual-final-0-3",
        "wecom-callback-actual-final-1-4",
    ] {
        assert!(logs.contains(marker), "durable transcript {marker}");
    }
}

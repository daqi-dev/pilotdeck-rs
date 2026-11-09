//! Real Server/A7 Weixin media: encrypted incoming CDN and actual tool -> AES upload -> ILink sendMedia.
use super::channels::ChannelEndpointOverrides;
use aes::{
    Aes128,
    cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray},
};
use base64::Engine;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
const FILE_IN: &str = "weixin-inbound-file-marker";
const FILE_OUT: &str = "weixin-outbound-file-marker";
#[derive(Default)]
struct Wire {
    incoming: Mutex<VecDeque<Value>>,
    sent: Mutex<Vec<Value>>,
    models: Mutex<Vec<Value>>,
    uploads: Mutex<Vec<Value>>,
    ciphertexts: Mutex<Vec<Vec<u8>>>,
    phase: AtomicUsize,
    generation: AtomicUsize,
    polls: AtomicUsize,
    hold_cdn: AtomicBool,
    active_cdn: AtomicUsize,
}
impl Wire {
    fn text(&self, s: &str) {
        self.incoming.lock().unwrap().push_back(json!({"from_user_id":"chat","context_token":"ctx","message_type":1,"item_list":[{"type":1,"text_item":{"text":s}}]}));
    }
    fn media(&self, origin: &str) {
        self.incoming.lock().unwrap().push_back(json!({"from_user_id":"chat","context_token":"ctx","message_type":1,"message_id":"incoming-media","item_list":[{"type":2,"image_item":{"cdn_url":format!("{origin}/incoming.png"),"aeskey":"33333333333333333333333333333333"}},{"type":4,"file_item":{"file_name":"inbound.txt","file_size":FILE_IN.len(),"cdn_url":format!("{origin}/incoming.txt"),"aeskey":"33333333333333333333333333333333"}}]}));
    }
    fn sent(&self, s: &str) -> bool {
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
                "Weixin media outbound={:#?}; uploadparams={:#?}; models={:#?}; active_cdn={}",
                self.state.sent.lock().unwrap_or_else(|e| e.into_inner()),
                self.state.uploads.lock().unwrap_or_else(|e| e.into_inner()),
                self.state.models.lock().unwrap_or_else(|e| e.into_inner()),
                self.state.active_cdn.load(Ordering::SeqCst)
            );
        }
        self.task.abort();
    }
}
fn encrypt(plain: &[u8], key: &[u8; 16]) -> Vec<u8> {
    let mut bytes = plain.to_vec();
    let pad = 16 - bytes.len() % 16;
    bytes.extend(std::iter::repeat_n(pad as u8, pad));
    let aes = Aes128::new(GenericArray::from_slice(key));
    for block in bytes.chunks_mut(16) {
        aes.encrypt_block(GenericArray::from_mut_slice(block));
    }
    bytes
}
fn decrypt(mut bytes: Vec<u8>, key: &[u8; 16]) -> Vec<u8> {
    let aes = Aes128::new(GenericArray::from_slice(key));
    for block in bytes.chunks_mut(16) {
        aes.decrypt_block(GenericArray::from_mut_slice(block));
    }
    let pad = *bytes.last().unwrap() as usize;
    assert!((1..=16).contains(&pad));
    assert!(
        bytes[bytes.len() - pad..]
            .iter()
            .all(|b| *b as usize == pad)
    );
    bytes.truncate(bytes.len() - pad);
    bytes
}
fn key(hex: &str) -> [u8; 16] {
    assert_eq!(hex.len(), 32);
    std::array::from_fn(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
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
            "{label}; sent={:#?}; uploads={:#?}; models={:#?}",
            state.sent.lock().unwrap_or_else(|e| e.into_inner()),
            state.uploads.lock().unwrap_or_else(|e| e.into_inner()),
            state.models.lock().unwrap_or_else(|e| e.into_inner())
        )
    });
}
async fn peer(file: std::path::PathBuf, image: std::path::PathBuf) -> Peer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let origin = format!("http://{address}");
    let state = Arc::new(Wire::default());
    let shared = state.clone();
    let task = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
            incoming=listener.accept()=>{let(mut socket,_)=incoming.unwrap();let state=shared.clone();let(file,image)=(file.clone(),image.clone());tasks.spawn(async move{
            let mut raw=vec![];while !raw.ends_with(b"\r\n\r\n"){let mut b=[0];if socket.read_exact(&mut b).await.is_err(){return}raw.push(b[0]);assert!(raw.len()<65536);}
            let header=String::from_utf8(raw).unwrap();let path=header.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned();let size=header.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length:").map(|v|v.trim().parse::<usize>().unwrap())).unwrap_or(0);let mut raw=vec![0;size];if socket.read_exact(&mut raw).await.is_err(){return}let body:Value=serde_json::from_slice(&raw).unwrap_or(Value::Null);
            let(status,mime,data,extra)=if path.ends_with("/getupdates"){state.polls.fetch_add(1,Ordering::SeqCst);let msg=state.incoming.lock().unwrap().pop_front();tokio::time::sleep(Duration::from_millis(50)).await;("200 OK","application/json",json!({"ret":0,"msgs":msg.into_iter().collect::<Vec<_>>(),"get_updates_buf":"cursor"}).to_string().into_bytes(),String::new())
            }else if path=="/incoming.png"{("200 OK","application/octet-stream",encrypt(&base64::engine::general_purpose::STANDARD.decode(PNG).unwrap(),&[0x33;16]),String::new())
            }else if path=="/incoming.txt"{("200 OK","application/octet-stream",encrypt(FILE_IN.as_bytes(),&[0x33;16]),String::new())
            }else if path=="/missing.png"{("404 Not Found","application/json",b"{}".to_vec(),String::new())
            }else if path.ends_with("/getuploadurl"){state.uploads.lock().unwrap().push(body);("200 OK","application/json",json!({"ret":0,"upload_full_url":format!("http://{address}/cdn-upload")}).to_string().into_bytes(),String::new())
            }else if path=="/cdn-upload"{
            state.ciphertexts.lock().unwrap().push(raw);if state.hold_cdn.load(Ordering::SeqCst){state.active_cdn.fetch_add(1,Ordering::SeqCst);let mut b=[0;1];let _=socket.read(&mut b).await;state.active_cdn.fetch_sub(1,Ordering::SeqCst);return;}
            ("200 OK","text/plain",vec![],"x-encrypted-param: download-param\r\n".to_owned())
            }else if path.ends_with("/sendmessage"){state.sent.lock().unwrap().push(body);("200 OK","application/json",json!({"ret":0}).to_string().into_bytes(),String::new())
            }else if path.ends_with("/getconfig")||path.ends_with("/sendtyping"){("200 OK","application/json",json!({"ret":0}).to_string().into_bytes(),String::new())
            }else if body["stream"]==true{
            state.models.lock().unwrap().push(body);let phase=state.phase.fetch_add(1,Ordering::SeqCst);let generation=state.generation.load(Ordering::SeqCst);
            let(delta,finish)=if phase==0{(json!({"role":"assistant","tool_calls":[{"index":0,"id":"weixin-send-file","type":"function","function":{"name":"send_attachment","arguments":json!({"file_path":file,"name":"reply.txt","mime_type":"text/plain"}).to_string()}},{"index":1,"id":"weixin-send-image","type":"function","function":{"name":"send_attachment","arguments":json!({"file_path":image,"name":"pixel.png","mime_type":"image/png"}).to_string()}}]}),"tool_calls")}else{(json!({"role":"assistant","content":format!("weixin-media-final-{generation}-{phase}")}),"stop")};
            ("200 OK","text/event-stream",format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",json!({"id":"mock","choices":[{"index":0,"delta":delta,"finish_reason":null}]}),json!({"id":"mock","choices":[{"index":0,"delta":{},"finish_reason":finish}]})).into_bytes(),String::new())
            }else{("200 OK","application/json",json!({"choices":[{"message":{"role":"assistant","content":"{\"title\":\"Weixin media\"}"},"finish_reason":"stop"}]}).to_string().into_bytes(),String::new())};
            let head=format!("HTTP/1.1 {status}\r\nContent-Type: {mime}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",data.len());let _=socket.write_all(head.as_bytes()).await;let _=socket.write_all(&data).await;
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
struct Rig {
    _scratch: super::management_tests::Scratch,
    requests: super::requests::Requests,
    host: crate::runtime::turn::gateway::TurnGateway,
    peer: Peer,
    base: String,
    path: std::path::PathBuf,
}
async fn rig() -> Rig {
    let (scratch, requests) = super::management_tests::setup();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    let file = requests.options.project.join("outgoing.txt");
    let image = requests.options.project.join("outgoing.png");
    std::fs::write(&file, FILE_OUT).unwrap();
    std::fs::write(
        &image,
        base64::engine::general_purpose::STANDARD
            .decode(PNG)
            .unwrap(),
    )
    .unwrap();
    let peer = peer(file, image).await;
    let path = requests.options.home.join("pilotdeck.yaml");
    let base = std::fs::read_to_string(&path)
        .unwrap()
        .replace("http://127.0.0.1:1", &format!("{}/v1", peer.origin));
    std::fs::write(
        &path,
        format!("{base}adapters:\n  weixin:\n    enabled: true\n"),
    )
    .unwrap();
    std::fs::write(
        requests.options.home.join("weixin-credentials.json"),
        json!({"baseUrl":peer.origin,"botToken":"mock-token","accountId":"mock-account"})
            .to_string(),
    )
    .unwrap();
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
        "Weixin media actual sidecar={} sha256={:x}",
        options.sidecar.display(),
        sha2::Sha256::digest(&bytes)
    );
    let (host, _channels) = super::assembly::start_with_channel_endpoints(
        options,
        requests.env.clone(),
        ChannelEndpointOverrides::default(),
    )
    .await
    .unwrap();
    Rig {
        _scratch: scratch,
        requests,
        host,
        peer,
        base,
        path,
    }
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
async fn weixin_actual_server_encrypted_inbound_media_a7_tool_upload_and_reload_shutdown() {
    let rig = rig().await;
    let state = &rig.peer.state;
    state.media(&rig.peer.origin);
    until(
        state,
        "real encrypted incoming media -> A7 -> final",
        || state.sent("weixin-media-final-0-1"),
    )
    .await;
    let models = state.models.lock().unwrap().clone();
    assert!(
        models[0].to_string().contains(PNG),
        "decrypted image bytes must reach actual model"
    );
    assert!(
        models[0].to_string().contains(FILE_IN),
        "decrypted file content must reach actual model"
    );
    let uploads = state.uploads.lock().unwrap().clone();
    let ciphertexts = state.ciphertexts.lock().unwrap().clone();
    assert_eq!(
        uploads.len(),
        2,
        "actual tool file and image each request upload URL"
    );
    assert_eq!(ciphertexts.len(), 2);
    for (params, ciphertext) in uploads.iter().zip(ciphertexts) {
        let plaintext = decrypt(ciphertext, &key(params["aeskey"].as_str().unwrap()));
        let expected = if params["media_type"] == 1 {
            base64::engine::general_purpose::STANDARD
                .decode(PNG)
                .unwrap()
        } else {
            FILE_OUT.as_bytes().to_vec()
        };
        assert_eq!(plaintext, expected);
        use md5::{Digest, Md5};
        assert_eq!(
            params["rawfilemd5"],
            format!("{:x}", Md5::digest(&expected))
        );
        assert_eq!(
            params["filesize"].as_u64().unwrap() as usize,
            (expected.len() / 16 + 1) * 16
        );
    }
    let sent = state.sent.lock().unwrap().clone();
    for typ in [2, 4] {
        let item = sent
            .iter()
            .flat_map(|v| v["msg"]["item_list"].as_array().unwrap())
            .find(|v| v["type"] == typ)
            .expect("actual native sendMedia envelope");
        let owner = if typ == 2 {
            &item["image_item"]
        } else {
            &item["file_item"]
        };
        assert_eq!(owner["media"]["encrypt_query_param"], "download-param");
        let key_hex = base64::engine::general_purpose::STANDARD
            .decode(owner["media"]["aes_key"].as_str().unwrap())
            .unwrap();
        assert!(
            uploads
                .iter()
                .any(|p| p["aeskey"] == String::from_utf8(key_hex.clone()).unwrap())
        );
    }
    let files = walk(&rig.requests.options.home.join("im-attachments"));
    assert!(files.iter().any(|p| {
        std::fs::read(p)
            .ok()
            .is_some_and(|v| v == FILE_IN.as_bytes())
    }));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in files {
            assert_eq!(
                std::fs::metadata(p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    state.incoming.lock().unwrap().push_back(json!({"from_user_id":"chat","context_token":"ctx","message_type":1,"item_list":[{"type":2,"image_item":{"cdn_url":format!("{}/missing.png",rig.peer.origin)}}]}));
    until(
        state,
        "failed incoming CDN remains a real diagnosable A7 turn",
        || state.sent("weixin-media-final-0-2"),
    )
    .await;
    assert!(
        state.models.lock().unwrap()[2]
            .to_string()
            .contains("微信附件处理失败")
    );
    let frames = Arc::new(Frames::default());
    let connection = rig
        .host
        .connect(frames.clone(), "token".into(), "fixture".into());
    connection.receive(json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"weixin-media"}).to_string()).await.unwrap();
    std::fs::write(
        &rig.path,
        format!("{}adapters:\n  weixin:\n    enabled: false\n", rig.base),
    )
    .unwrap();
    connection
        .receive(
            json!({"type":"request","id":"disable-media","method":"reload_config","params":{}})
                .to_string(),
        )
        .await
        .unwrap();
    until(state, "public disable response", || {
        frames
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|v| v["id"] == "disable-media" && v["ok"] == true)
    })
    .await;
    let mut previous = state.polls.load(Ordering::SeqCst);
    let mut quiet = tokio::time::Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while quiet.elapsed() < Duration::from_millis(300) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "disabled poll stops"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        let now = state.polls.load(Ordering::SeqCst);
        if now != previous {
            quiet = tokio::time::Instant::now();
            previous = now;
        }
    }
    state.generation.store(1, Ordering::SeqCst);
    std::fs::write(
        &rig.path,
        format!("{}adapters:\n  weixin:\n    enabled: true\n", rig.base),
    )
    .unwrap();
    connection
        .receive(
            json!({"type":"request","id":"enable-media","method":"reload_config","params":{}})
                .to_string(),
        )
        .await
        .unwrap();
    state.text("reload-media");
    until(state, "reloaded real Weixin A7/outbound", || {
        state.sent("weixin-media-final-1-3")
    })
    .await;
    connection.closed().await.unwrap();
    rig.host
        .shutdown(rig.requests.options.drain_budget)
        .await
        .unwrap();
    let count = state.sent.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(count, state.sent.lock().unwrap().len());
    let mut logs = String::new();
    for p in walk(&rig.requests.options.home.join("projects")) {
        if p.extension().is_some_and(|e| e == "jsonl") {
            logs.push_str(&std::fs::read_to_string(p).unwrap());
        }
    }
    for marker in [
        "weixin-media-final-0-1",
        "weixin-media-final-0-2",
        "weixin-media-final-1-3",
    ] {
        assert!(logs.contains(marker), "durable {marker}");
    }
}
#[tokio::test]
async fn weixin_actual_server_shutdown_cancels_inflight_cdn_upload_without_late_output() {
    let rig = rig().await;
    rig.peer.state.hold_cdn.store(true, Ordering::SeqCst);
    rig.peer.state.text("hold-outgoing-media");
    until(
        &rig.peer.state,
        "real A7 send_attachment reaches held CDN HTTP exchange",
        || rig.peer.state.active_cdn.load(Ordering::SeqCst) == 1,
    )
    .await;
    rig.host
        .shutdown(rig.requests.options.drain_budget)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while rig.peer.state.active_cdn.load(Ordering::SeqCst) != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Gateway shutdown must cancel/drain owned media HTTP, not leave it alive");
    let count = rig.peer.state.sent.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        count,
        rig.peer.state.sent.lock().unwrap().len(),
        "no output after Gateway shutdown"
    );
}
fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    if let Ok(dir) = std::fs::read_dir(root) {
        for row in dir {
            let p = row.unwrap().path();
            if p.is_dir() {
                out.extend(walk(&p))
            } else {
                out.push(p)
            }
        }
    }
    out
}

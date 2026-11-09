//! Real native Server + A7 + real sidecar + loopback model/WeCom peer.
//! Included under native_process only by the test assembly; no mock TurnGateway.
use super::channels::ChannelEndpointOverrides;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

#[derive(Default)]
struct Wire {
    generation: AtomicUsize,
    frames: Mutex<Vec<(usize, Value)>>,
    model: Mutex<Vec<Value>>,
    artifact: Mutex<String>,
}
async fn eventually(mut condition: impl FnMut() -> bool, label: &str) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{label}"));
}
#[tokio::test]
async fn wecom_actual_host_media_and_help() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    struct FailureEvidence(Arc<Wire>);
    impl Drop for FailureEvidence {
        fn drop(&mut self) {
            if std::thread::panicking() {
                eprintln!(
                    "WeCom frames: {:#?}",
                    self.0.frames.lock().unwrap_or_else(|e| e.into_inner())
                );
                eprintln!(
                    "WeCom model: {:#?}",
                    self.0.model.lock().unwrap_or_else(|e| e.into_inner())
                );
            }
        }
    }
    let state = Arc::new(Wire::default());
    let _evidence = FailureEvidence(state.clone());
    let shared = state.clone();
    let server = tokio::spawn(async move {
        let mut clients = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                incoming=listener.accept()=> { let (mut socket,_)=incoming.unwrap(); let state=shared.clone(); clients.spawn(async move {
                    let mut peek=vec![0;8192];
                    loop { let n=match socket.peek(&mut peek).await { Ok(n) if n>0=>n,_=>return }; if peek[..n].windows(4).any(|w|w==b"\r\n\r\n") {break;} tokio::time::sleep(Duration::from_millis(1)).await; }
                    if String::from_utf8_lossy(&peek).to_ascii_lowercase().contains("upgrade: websocket") {
                        let mut peer=tokio_tungstenite::accept_async(socket).await.unwrap();
                        let generation=state.generation.load(Ordering::SeqCst);
                        while let Some(Ok(frame))=peer.next().await {
                            let Message::Text(raw)=frame else { continue };
                            let value:Value=serde_json::from_str(raw.as_str()).unwrap();
                            let cmd=value["cmd"].as_str().unwrap_or(""); let id=value["headers"]["req_id"].as_str().unwrap_or("");
                            state.frames.lock().unwrap().push((generation,value.clone()));
                            let reply = match cmd {
                                "aibot_upload_media_init" => json!({"headers":{"req_id":id},"body":{"errcode":0,"upload_id":"test-upload"}}),
                                "aibot_upload_media_finish" => json!({"headers":{"req_id":id},"body":{"errcode":0,"media_id":"test-media"}}),
                                _ => json!({"headers":{"req_id":id},"body":{"errcode":0}})
                            };
                            peer.send(Message::Text(reply.to_string().into())).await.unwrap();
                            if cmd=="aibot_subscribe" {
                                peer.send(Message::Text(json!({"cmd":"aibot_msg_callback","headers":{"req_id":"in-help"},"body":{"msgid":"m-help","from":{"userid":"alice"},"chatid":"alice","chattype":"single","msgtype":"text","text":{"content":"/help"}}}).to_string().into())).await.unwrap();
                                peer.send(Message::Text(json!({"cmd":"aibot_msg_callback","headers":{"req_id":"in-media"},"body":{"msgid":"m-media","from":{"userid":"alice"},"chatid":"alice","chattype":"single","msgtype":"file","file":{"url":format!("http://{address}/encrypted"),"aeskey":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","filename":"wecom-actual.txt"}}}).to_string().into())).await.unwrap();
                            }
                        }
                        return;
                    }
                    let mut bytes=vec![]; while !bytes.ends_with(b"\r\n\r\n") {let mut byte=[0];if socket.read_exact(&mut byte).await.is_err(){return} bytes.push(byte[0]);assert!(bytes.len()<65536);}
                    let headers=String::from_utf8(bytes).unwrap();
                    if headers.starts_with("GET /encrypted ") {
                        use base64::Engine;
                        let encrypted=base64::engine::general_purpose::STANDARD.decode("6JdUl0nUIkkcDmt4hXQEniF52SgeWTEWDIlNe4n4Av8=").unwrap();
                        let head=format!("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",encrypted.len());
                        let _=socket.write_all(head.as_bytes()).await;
                        let _=socket.write_all(&encrypted).await;
                        return;
                    }
                    let size=headers.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").map(|v|v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                    let mut body=vec![0;size]; if socket.read_exact(&mut body).await.is_err(){return};
                    let body:Value=serde_json::from_slice(&body).unwrap_or(Value::Null);
                    let generation=state.generation.load(Ordering::SeqCst);
                    let (mime,text)=if body.get("stream")==Some(&Value::Bool(true)) {
                        state.model.lock().unwrap().push(body);
                        ("text/event-stream",format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",json!({"id":"fixture","choices":[{"index":0,"delta":{"role":"assistant","content":format!("wecom-actual-output-{generation}\nMEDIA:{}", state.artifact.lock().unwrap())},"finish_reason":null}]}),json!({"id":"fixture","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})))
                    } else { ("application/json",json!({"choices":[{"message":{"role":"assistant","content":"{\"title\":\"WeCom lifecycle\"}"},"finish_reason":"stop"}]}).to_string()) };
                    let response=format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",text.len()); let _=socket.write_all(response.as_bytes()).await;
                }); },
                Some(_)=clients.join_next(),if !clients.is_empty()=>{},
            }
        }
    });
    let (_scratch, requests) = super::management_tests::setup();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    let artifact = requests.options.project.join("wecom-outbound.txt");
    std::fs::write(&artifact, b"wecom-output-fixture").unwrap();
    *state.artifact.lock().unwrap() = artifact.to_string_lossy().into_owned();
    let path = requests.options.home.join("pilotdeck.yaml");
    let base = std::fs::read_to_string(&path)
        .unwrap()
        .replace("http://127.0.0.1:1", &format!("http://{address}/v1"));
    std::fs::write(&path,format!("{base}adapters:\n  wecom:\n    enabled: true\n    token: mock-bot\n    extra:\n      botSecret: mock-secret\n      dmPolicy: open\n      textBatchDelayMs: 0\n      websocketUrl: ws://{address}/wecom\n")).unwrap();
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
        "WeCom actual sidecar={} sha256={:x}",
        options.sidecar.display(),
        sha2::Sha256::digest(&bytes)
    );
    let (host, _channels) = super::assembly::start_with_channel_endpoints(
        options.clone(),
        requests.env.clone(),
        ChannelEndpointOverrides::default(),
    )
    .await
    .unwrap();
    eventually(
        || {
            state.frames.lock().unwrap().iter().any(|(_, v)| {
                v["cmd"] == "aibot_respond_msg"
                    && v["body"]["markdown"]["content"]
                        .as_str()
                        .is_some_and(|s| s.contains("/help") && s.contains("/projects"))
            })
        },
        "real WeCom /help command reply",
    )
    .await;
    eventually(
        || {
            state
                .model
                .lock()
                .unwrap()
                .iter()
                .any(|v| v.to_string().contains("WeCom encrypted attachment"))
        },
        "real WeCom file callback reaches A7/model",
    )
    .await;
    assert!(
        state
            .model
            .lock()
            .unwrap()
            .iter()
            .all(|v| !v.to_string().contains("/help")),
        "help must be consumed by channel command"
    );
    eventually(
        || {
            state.frames.lock().unwrap().iter().any(|(_, v)| {
                v["cmd"] == "aibot_respond_msg"
                    && v["body"]["markdown"]["content"]
                        .as_str()
                        .is_some_and(|s| s.contains("wecom-actual-output-0"))
            })
        },
        "real model reply to media",
    )
    .await;
    eventually(
        || {
            let frames = state.frames.lock().unwrap();
            [
                "aibot_upload_media_init",
                "aibot_upload_media_chunk",
                "aibot_upload_media_finish",
            ]
            .iter()
            .all(|cmd| frames.iter().any(|(_, v)| v["cmd"] == *cmd))
                && frames.iter().any(|(_, v)| {
                    v["cmd"] == "aibot_respond_msg" && v["body"]["file"]["media_id"] == "test-media"
                })
        },
        "real model MEDIA output uploads and sends file via WeCom",
    )
    .await;
    host.shutdown(options.drain_budget).await.unwrap();
    let count = state.frames.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(count, state.frames.lock().unwrap().len());
    server.abort();
    let _ = server.await;
}

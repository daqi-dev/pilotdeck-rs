//! Real Server + Feishu binary WS/resources + A7/model/tools + multipart/output.
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
const FILE_IN: &str = "feishu-inbound-file-marker";
const FILE_OUT: &str = "feishu-outbound-file-marker";
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
struct Wire {
    incoming: tokio::sync::broadcast::Sender<Value>,
    sockets: AtomicUsize,
    transport_active: Mutex<Vec<String>>,
    transport_seen: Mutex<Vec<String>>,
    seq: AtomicUsize,
    acks: AtomicUsize,
    phase: AtomicUsize,
    generation: AtomicUsize,
    delayed_release: tokio::sync::Notify,
    delayed_released: AtomicUsize,
    delayed_write_result: AtomicUsize,
    http: Mutex<Vec<(String, String, Vec<u8>)>>,
    models: Mutex<Vec<Value>>,
    sent: Mutex<Vec<Value>>,
    home: Mutex<Option<std::path::PathBuf>>,
    failure_evidence: Mutex<Option<std::path::PathBuf>>,
}
impl Default for Wire {
    fn default() -> Self {
        Self {
            incoming: tokio::sync::broadcast::channel(32).0,
            sockets: AtomicUsize::new(0),
            transport_active: Mutex::new(vec![]),
            transport_seen: Mutex::new(vec![]),
            seq: AtomicUsize::new(0),
            acks: AtomicUsize::new(0),
            phase: AtomicUsize::new(0),
            generation: AtomicUsize::new(0),
            delayed_release: tokio::sync::Notify::new(),
            delayed_released: AtomicUsize::new(0),
            delayed_write_result: AtomicUsize::new(0),
            http: Mutex::new(vec![]),
            models: Mutex::new(vec![]),
            sent: Mutex::new(vec![]),
            home: Mutex::new(None),
            failure_evidence: Mutex::new(None),
        }
    }
}
impl Wire {
    fn transport_only(&self, id: &str) -> bool {
        let active = self.transport_active.lock().unwrap();
        active.len() == 1 && active[0] == id
    }
    fn transport_counts(&self, id: &str) -> (usize, usize) {
        let seen = self
            .transport_seen
            .lock()
            .unwrap()
            .iter()
            .filter(|item| item.as_str() == id)
            .count();
        let discoveries = self
            .http
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _, body)| {
                path == "/callback/ws/endpoint"
                    && serde_json::from_slice::<Value>(body)
                        .ok()
                        .and_then(|v| v["AppID"].as_str().map(str::to_owned))
                        .as_deref()
                        == Some(id)
            })
            .count();
        (seen, discoveries)
    }
    fn preserve_failure(&self) {
        let mut prior = self
            .failure_evidence
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(path) = prior.as_ref() {
            eprintln!("Feishu full raw evidence={}", path.display());
            return;
        }
        let base = std::env::current_dir()
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("_tmp/native-channel-consumer-failures");
        let path = base.join(format!("feishu-{}.json", uuid::Uuid::new_v4()));
        let http: Vec<Value> = self
            .http
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(p, a, b)| {
                let body = match serde_json::from_slice::<Value>(b) {
                    Ok(body) => body,
                    Err(_) => {
                        use base64::Engine;
                        json!({"base64":base64::engine::general_purpose::STANDARD.encode(b)})
                    }
                };
                json!({"path":p,"authorization":a,"body":body})
            })
            .collect();
        let transcripts:Vec<Value>=self.home.lock().unwrap_or_else(|e|e.into_inner()).as_ref().map(|home|walk(&home.join("projects")).into_iter().filter(|p|p.extension().is_some_and(|e|e=="jsonl")).map(|p|json!({"path":p,"text":std::fs::read_to_string(&p).unwrap_or_default()})).collect()).unwrap_or_default();
        let transport_active = self
            .transport_active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let transport_seen = self
            .transport_seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let value = json!({"http":http,"models":self.models.lock().unwrap_or_else(|e|e.into_inner()).clone(),"sent":self.sent.lock().unwrap_or_else(|e|e.into_inner()).clone(),"sockets":self.sockets.load(Ordering::SeqCst),"transportActive":transport_active,"transportSeen":transport_seen,"acks":self.acks.load(Ordering::SeqCst),"transcripts":transcripts});
        match std::fs::create_dir_all(&base).and_then(|_| {
            std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap_or_default())
        }) {
            Ok(()) => {
                eprintln!("Feishu full raw evidence={}", path.display());
                *prior = Some(path);
            }
            Err(error) => eprintln!("Feishu evidence persistence error: {error}"),
        }
    }
    fn event(&self, kind: &str, content: Value) {
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        self.incoming.send(json!({"schema":"2.0","header":{"event_id":format!("event-{id}"),"event_type":"im.message.receive_v1"},"event":{"sender":{"sender_id":{"open_id":"human"},"sender_type":"user"},"message":{"message_id":format!("message-{id}"),"chat_id":"room","chat_type":"p2p","message_type":kind,"content":content.to_string()}}})).expect("active Feishu Socket Mode");
    }
    fn text(&self, s: &str) {
        self.event("text", json!({"text":s}));
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
            self.state.preserve_failure();
            eprintln!(
                "Feishu REST paths={:#?}; sent={:#?}; models={:#?}",
                self.state
                    .http
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .map(|(p, a, b)| (p, a, b.len()))
                    .collect::<Vec<_>>(),
                self.state.sent.lock().unwrap_or_else(|e| e.into_inner()),
                self.state.models.lock().unwrap_or_else(|e| e.into_inner())
            );
        }
        self.task.abort();
    }
}
async fn command(peer: &Peer, input: &str, expected: &str) -> String {
    super::channel_command_fixture::control(
        |input| peer.state.text(input),
        || {
            peer.state
                .sent
                .lock()
                .unwrap()
                .iter()
                .filter_map(|frame| {
                    frame["content"]
                        .as_str()
                        .and_then(|content| serde_json::from_str::<Value>(content).ok())
                        .and_then(|content| content["text"].as_str().map(str::to_owned))
                })
                .collect()
        },
        || peer.state.models.lock().unwrap().len(),
        input,
        expected,
    )
    .await
}
async fn until(state: &Wire, label: &str, mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !check() {
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
// External peer encoder: pbbp2 fields pinned by the independent SDK golden,
// with a fresh JSON event payload; not an invocation of product Frame::encode.
fn varint(mut n: u64, out: &mut Vec<u8>) {
    while n >= 128 {
        out.push((n as u8) | 128);
        n >>= 7;
    }
    out.push(n as u8);
}
fn bytes(field: u64, value: &[u8], out: &mut Vec<u8>) {
    varint(field * 8 + 2, out);
    varint(value.len() as u64, out);
    out.extend_from_slice(value);
}
fn frame(event: &Value, seq: u64) -> Vec<u8> {
    let mut out = vec![];
    for (field, value) in [(1, seq), (2, seq + 100), (3, 77), (4, 1)] {
        varint(field * 8, &mut out);
        varint(value, &mut out);
    }
    for (k, v) in [
        ("type", "event".to_owned()),
        ("message_id", format!("wire-{seq}")),
        ("sum", "1".into()),
        ("seq", "0".into()),
    ] {
        let mut header = vec![];
        bytes(1, k.as_bytes(), &mut header);
        bytes(2, v.as_bytes(), &mut header);
        bytes(5, &header, &mut out);
    }
    bytes(6, b"utf8", &mut out);
    bytes(7, b"json", &mut out);
    bytes(8, event.to_string().as_bytes(), &mut out);
    out
}
async fn peer(
    output: std::path::PathBuf,
    approved: std::path::PathBuf,
    denied: std::path::PathBuf,
) -> Peer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let origin = format!("http://{address}");
    let state = Arc::new(Wire::default());
    let shared = state.clone();
    let task = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
            incoming=listener.accept()=>{let(mut socket,_)=incoming.unwrap();let state=shared.clone();let(output,approved,denied)=(output.clone(),approved.clone(),denied.clone());tasks.spawn(async move{
            let mut peek=[0;8192];loop{let n=match socket.peek(&mut peek).await{Ok(n)if n>0=>n,_=>return};if peek[..n].windows(4).any(|w|w==b"\r\n\r\n"){break;}tokio::time::sleep(Duration::from_millis(1)).await;}
            if String::from_utf8_lossy(&peek).to_ascii_lowercase().contains("upgrade: websocket"){
            let line=String::from_utf8_lossy(&peek);let target=line.lines().next().unwrap().split_whitespace().nth(1).unwrap();let identity=target.split("device_id=").nth(1).expect("mock WS device identity").split('&').next().unwrap().to_owned();
            let mut ws=tokio_tungstenite::accept_async(socket).await.unwrap();let mut incoming=state.incoming.subscribe();state.sockets.fetch_add(1,Ordering::SeqCst);state.transport_seen.lock().unwrap().push(identity.clone());state.transport_active.lock().unwrap().push(identity.clone());let mut seq=0;
            loop{tokio::select!{event=incoming.recv()=>{let Ok(event)=event else{break};seq+=1;if ws.send(Message::Binary(frame(&event,seq).into())).await.is_err(){break;}},
            message=ws.next()=>match message{Some(Ok(Message::Binary(raw)))=>{if raw.windows(12).any(|w|w==br#"{"code":200}"#){state.acks.fetch_add(1,Ordering::SeqCst);}},Some(Ok(Message::Close(_)))=>{let _=ws.close(None).await;break},Some(Ok(_))=>{},_=>break}
            }}state.sockets.fetch_sub(1,Ordering::SeqCst);let mut active=state.transport_active.lock().unwrap();let pos=active.iter().position(|v|v==&identity).expect("accepted WS identity remains tracked");active.remove(pos);return;}
            let mut raw=vec![];while !raw.ends_with(b"\r\n\r\n"){let mut b=[0];if socket.read_exact(&mut b).await.is_err(){return}raw.push(b[0]);assert!(raw.len()<65536);}
            let header=String::from_utf8(raw).unwrap();let path=header.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned();let authorization=header.lines().find_map(|l|l.split_once(':').filter(|(k,_)|k.eq_ignore_ascii_case("authorization")).map(|(_,v)|v.trim().to_owned())).unwrap_or_default();let size=header.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length:").map(|v|v.trim().parse::<usize>().unwrap())).unwrap_or(0);let mut raw=vec![0;size];if socket.read_exact(&mut raw).await.is_err(){return}state.http.lock().unwrap().push((path.clone(),authorization,raw.clone()));
            let body:Value=serde_json::from_slice(&raw).unwrap_or(Value::Null);
            let mut delayed_write=false;let(status,mime,data)=if path=="/callback/ws/endpoint"{let app_id=body["AppID"].as_str().expect("discovery app id");("200 OK","application/json",json!({"code":0,"data":{"URL":format!("ws://{address}/feishu?service_id=77&device_id={app_id}"),"ClientConfig":{"PingInterval":1,"ReconnectCount":2,"ReconnectInterval":0.01,"ReconnectNonce":0}}}).to_string().into_bytes())
            }else if path.contains("tenant_access_token"){("200 OK","application/json",json!({"code":0,"tenant_access_token":"mock-tenant-token","expire":7200}).to_string().into_bytes())
            }else if path.contains("/resources/img-bad"){("404 Not Found","application/json",b"{}".to_vec())
            }else if path.contains("/resources/img-key"){use base64::Engine;("200 OK","image/png",base64::engine::general_purpose::STANDARD.decode(PNG).unwrap())
            }else if path.contains("/resources/file-key"){("200 OK","text/plain",FILE_IN.as_bytes().to_vec())
            }else if path=="/open-apis/im/v1/files"{("200 OK","application/json",json!({"code":0,"data":{"file_key":"uploaded-file-key"}}).to_string().into_bytes())
            }else if path=="/open-apis/im/v1/images"{("200 OK","application/json",json!({"code":0,"data":{"image_key":"uploaded-image-key"}}).to_string().into_bytes())
            }else if path.starts_with("/open-apis/im/v1/"){state.sent.lock().unwrap().push(body);("200 OK","application/json",json!({"code":0,"data":{"message_id":"reply","reaction_id":"reaction"}}).to_string().into_bytes())
            }else if body["stream"]==true{
            state.models.lock().unwrap().push(body);let phase=state.phase.fetch_add(1,Ordering::SeqCst);let generation=state.generation.load(Ordering::SeqCst);
            let(delta,finish)=match phase{
            0=>(json!({"role":"assistant","tool_calls":[{"index":0,"id":"feishu-send-file","type":"function","function":{"name":"send_attachment","arguments":json!({"file_path":output,"name":"reply.txt","mime_type":"text/plain"}).to_string()}},{"index":1,"id":"feishu-send-image","type":"function","function":{"name":"send_attachment","arguments":json!({"file_path":output.with_extension("png"),"name":"reply.png","mime_type":"image/png"}).to_string()}}]}),"tool_calls"),
            2|5|12|14=>{let path=if phase==2{approved}else{denied};(json!({"role":"assistant","tool_calls":[{"index":0,"id":format!("feishu-permission-{phase}"),"type":"function","function":{"name":"bash","arguments":json!({"command":format!("printf approved > '{}'",path.display())}).to_string()}}]}),"tool_calls")},
            3=>(json!({"role":"assistant","tool_calls":[{"index":0,"id":"feishu-question","type":"function","function":{"name":"ask_user_question","arguments":json!({"questions":[{"header":"Choice","question":"Feishu choose mock option","options":[{"label":"One","description":"Mock one"},{"label":"Two","description":"Mock two"}],"multiSelect":false}]}).to_string()}}]}),"tool_calls"),
            _=>(json!({"role":"assistant","content":format!("feishu-actual-final-{generation}-{phase}")}),"stop")};
            if phase==10 { delayed_write=true; state.delayed_release.notified().await; state.delayed_released.fetch_add(1,Ordering::SeqCst); }
            ("200 OK","text/event-stream",format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",json!({"id":"mock","choices":[{"index":0,"delta":delta,"finish_reason":null}]}),json!({"id":"mock","choices":[{"index":0,"delta":{},"finish_reason":finish}]})).into_bytes())
            }else{("200 OK","application/json",json!({"choices":[{"message":{"role":"assistant","content":"{\"title\":\"Feishu media actual\"}"},"finish_reason":"stop"}]}).to_string().into_bytes())};
            let headers=format!("HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",data.len());let header_write=socket.write_all(headers.as_bytes()).await;let body_write=socket.write_all(&data).await;if delayed_write { let status=if header_write.is_ok()&&body_write.is_ok(){1}else{2};eprintln!("held Feishu provider write outcome={status}; header={header_write:?}; body={body_write:?}");state.delayed_write_result.store(status,Ordering::SeqCst); }
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
async fn feishu_actual_server_media_a7_upload_interactions_new_reload_shutdown() {
    let (_scratch, requests) = super::management_tests::setup();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    let mock_cli = super::channel_command_fixture::prepare(&requests.options);
    let output = requests.options.project.join("feishu-outgoing.txt");
    std::fs::write(&output, FILE_OUT).unwrap();
    {
        use base64::Engine;
        std::fs::write(
            output.with_extension("png"),
            base64::engine::general_purpose::STANDARD
                .decode(PNG)
                .unwrap(),
        )
        .unwrap();
    }
    let approved = requests.options.project.join("feishu-approved.txt");
    let denied = requests.options.project.join("feishu-denied.txt");
    let peer = peer(output, approved.clone(), denied.clone()).await;
    *peer.state.home.lock().unwrap() = Some(requests.options.home.clone());
    let path = requests.options.home.join("pilotdeck.yaml");
    let mut config = json!({"schemaVersion":1,"agent":{"model":"test/test"},"model":{"providers":{"test":{"protocol":"openai","url":format!("{}/v1",peer.origin),"apiKey":"mock-only","models":{"test":{"multimodal":{"input":["text","image"]}}}}}},"adapters":{"feishu":{"enabled":true,"appId":"cli_0123456789abcdef","appSecret":"mock-secret","connectionMode":"stream"}}});
    std::fs::write(&path, config.to_string()).unwrap();
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
        "Feishu media actual sidecar={} sha256={:x}",
        options.sidecar.display(),
        sha2::Sha256::digest(&bytes)
    );
    let (host, _channels) = super::assembly::start_with_channel_endpoints(
        options.clone(),
        requests.env.clone(),
        ChannelEndpointOverrides {
            feishu_api_base: Some(peer.origin.clone()),
            channel_system_cli: Some(mock_cli),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    until(&peer.state, "real Feishu Server stream transport", || {
        peer.state.sockets.load(Ordering::SeqCst) == 1
    })
    .await;
    command(&peer, "/help", "/projects").await;
    let listed = command(&peer, "/projects", "项目列表：").await;
    let name = super::channel_command_fixture::project_name(&listed);
    command(&peer, &format!("/switch-project {name}"), "已切换到项目").await;
    let status = command(&peer, "/status", "PilotDeck Status").await;
    assert!(
        status.contains("fixture") && status.contains("mock channel status"),
        "real git status must reach the external channel"
    );
    command(&peer, "/update check", "已是最新 Release").await;
    command(&peer, "/update", "mock-release-restart").await;
    super::channel_command_fixture::assert_update_calls(&options);
    peer.state.event("image", json!({"image_key":"img-key"}));
    peer.state.event(
        "file",
        json!({"file_key":"file-key","file_name":"inbound.txt","size":FILE_IN.len()}),
    );
    until(
        &peer.state,
        "inbound media reaches actual A7 and final output",
        || peer.state.sent("feishu-actual-final-0-1"),
    )
    .await;
    let models = peer.state.models.lock().unwrap().clone();
    assert!(
        models[0]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |message| message["content"].as_array().is_some_and(|blocks| blocks
                    .iter()
                    .any(|block| block["type"] == "image_url"
                        && block["image_url"]["url"] == format!("data:image/png;base64,{PNG}")))
            ),
        "image bytes must reach actual model as image content"
    );
    assert!(
        models[0].to_string().contains(FILE_IN),
        "text attachment content must reach actual model"
    );
    assert!(
        peer.state
            .http
            .lock()
            .unwrap()
            .iter()
            .any(|(p, a, b)| p == "/open-apis/im/v1/files"
                && a == "Bearer mock-tenant-token"
                && b.windows(FILE_OUT.len()).any(|w| w == FILE_OUT.as_bytes())),
        "actual send_attachment bytes uploaded"
    );
    assert!(
        peer.state
            .sent
            .lock()
            .unwrap()
            .iter()
            .any(|v| v["msg_type"] == "file"
                && v["content"]
                    .as_str()
                    .is_some_and(|c| c.contains("uploaded-file-key"))),
        "actual uploaded file key sent to chat"
    );
    assert!(
        peer.state
            .http
            .lock()
            .unwrap()
            .iter()
            .any(
                |(path, authorization, body)| path == "/open-apis/im/v1/images"
                    && authorization == "Bearer mock-tenant-token"
                    && body.windows(8).any(|bytes| bytes == b"\x89PNG\r\n\x1a\n")
            ),
        "actual image tool uploads the PNG bytes to image API"
    );
    assert!(
        peer.state
            .sent
            .lock()
            .unwrap()
            .iter()
            .any(|frame| frame["msg_type"] == "image"
                && frame["content"]
                    .as_str()
                    .is_some_and(|value| value.contains("uploaded-image-key"))),
        "actual uploaded image key reaches external Feishu chat"
    );
    let files = walk(&options.home.join("im-attachments"));
    assert!(
        files.iter().any(|p| std::fs::read(p)
            .ok()
            .is_some_and(|b| b == FILE_IN.as_bytes())),
        "attachment stored in isolated pilot home"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in &files {
            assert_eq!(
                std::fs::metadata(p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    command(
        &peer,
        "/search feishu-inbound-file-marker --all --limit 3 --role user",
        "🔍 找到",
    )
    .await;
    peer.state.text("interaction-start");
    until(&peer.state, "actual Feishu permission prompt", || {
        peer.state.sent("回复 1 允许一次")
    })
    .await;
    assert!(!approved.exists());
    peer.state.text("1");
    until(&peer.state, "actual Feishu elicitation prompt", || {
        peer.state.sent("Feishu choose mock option")
    })
    .await;
    peer.state.text("1");
    until(
        &peer.state,
        "actual Feishu same A7 interactive continuation",
        || peer.state.sent("feishu-actual-final-0-4"),
    )
    .await;
    assert_eq!(std::fs::read_to_string(&approved).unwrap(), "approved");
    {
        let models = peer.state.models.lock().unwrap();
        let after_permission = models[3]["messages"].as_array().unwrap();
        assert!(
            after_permission
                .iter()
                .any(|message| message["role"] == "tool"
                    && message["tool_call_id"] == "feishu-permission-2"),
            "actual approved bash result must resume the same model history"
        );
        let after_question = models[4]["messages"].as_array().unwrap();
        let answer = after_question
            .iter()
            .rev()
            .find(|message| message["role"] == "tool")
            .expect("actual elicitation must produce a tool result");
        assert_eq!(answer["tool_call_id"], "feishu-question");
        assert!(
            answer["content"].to_string().contains("One"),
            "latest real tool result must contain the chosen answer, not a prior question option"
        );
    }
    let count = peer
        .state
        .sent
        .lock()
        .unwrap()
        .iter()
        .filter(|v| v.to_string().contains("回复 1 允许一次"))
        .count();
    peer.state.text("pending-new");
    until(&peer.state, "new pending Feishu permission", || {
        peer.state
            .sent
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v.to_string().contains("回复 1 允许一次"))
            .count()
            > count
    })
    .await;
    peer.state.text("/new");
    until(&peer.state, "Feishu new resets actual pending turn", || {
        peer.state.sent("已创建新会话")
    })
    .await;
    peer.state.text("post-new");
    until(&peer.state, "new Feishu session enters real A7", || {
        peer.state.sent("feishu-actual-final-0-6")
    })
    .await;
    assert!(!denied.exists());
    peer.state.event("image", json!({"image_key":"img-bad"}));
    until(
        &peer.state,
        "failed media download remains diagnosable actual A7 input",
        || peer.state.sent("feishu-actual-final-0-7"),
    )
    .await;
    assert!(
        peer.state.models.lock().unwrap()[7]
            .to_string()
            .contains("Attachment diagnostics")
    );
    let frames = Arc::new(Frames::default());
    let connection = host.connect(frames.clone(), "token".into(), "fixture".into());
    connection.receive(json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"feishu-media"}).to_string()).await.unwrap();
    config["adapters"]["feishu"]["enabled"] = false.into();
    std::fs::write(&path, config.to_string()).unwrap();
    connection.receive(json!({"type":"request","id":"disable-feishu-media","method":"reload_config","params":{}}).to_string()).await.unwrap();
    until(&peer.state, "public disable closes Feishu stream", || {
        peer.state.sockets.load(Ordering::SeqCst) == 0
    })
    .await;
    let retired_first = peer.state.transport_counts("cli_0123456789abcdef");
    peer.state.generation.store(1, Ordering::SeqCst);
    config["adapters"]["feishu"]["appId"] = "cli_1111111111111111".into();
    config["adapters"]["feishu"]["enabled"] = true.into();
    std::fs::write(&path, config.to_string()).unwrap();
    connection.receive(json!({"type":"request","id":"enable-feishu-media","method":"reload_config","params":{}}).to_string()).await.unwrap();
    until(
        &peer.state,
        "public reload creates fresh Feishu transport",
        || peer.state.transport_only("cli_1111111111111111"),
    )
    .await;
    peer.state.text("reload-start");
    until(&peer.state, "reloaded Feishu still reaches A7", || {
        peer.state.sent("feishu-actual-final-1-8")
    })
    .await;
    let before_plan = peer.state.models.lock().unwrap().len();
    peer.state.text("/plan mock channel planning task");
    until(
        &peer.state,
        "non-system plan command reaches actual A7 final",
        || peer.state.sent("feishu-actual-final-1-9"),
    )
    .await;
    assert_eq!(peer.state.models.lock().unwrap().len(), before_plan + 1);
    // /plan persists read-only mode in this session. Use the real reset command
    // before the independent shutdown scenario that requires a writable tool gate.
    command(&peer, "/new", "已创建新会话").await;
    let prompts = peer
        .state
        .sent
        .lock()
        .unwrap()
        .iter()
        .filter(|frame| frame.to_string().contains("回复 1 允许一次"))
        .count();
    peer.state.text("reload-delayed-final");
    until(&peer.state, "old turn reaches held model response", || {
        peer.state.models.lock().unwrap().len() >= 11
    })
    .await;
    config["adapters"]["feishu"]["enabled"] = false.into();
    std::fs::write(&path, config.to_string()).unwrap();
    connection.receive(json!({"type":"request","id":"disable-feishu-held","method":"reload_config","params":{}}).to_string()).await.unwrap();
    until(
        &peer.state,
        "held turn reload closes prior transport",
        || peer.state.sockets.load(Ordering::SeqCst) == 0,
    )
    .await;
    let retired_second = peer.state.transport_counts("cli_1111111111111111");
    config["adapters"]["feishu"]["appId"] = "cli_2222222222222222".into();
    config["adapters"]["feishu"]["enabled"] = true.into();
    std::fs::write(&path, config.to_string()).unwrap();
    connection.receive(json!({"type":"request","id":"enable-feishu-held","method":"reload_config","params":{}}).to_string()).await.unwrap();
    until(
        &peer.state,
        "held turn reload starts fresh transport",
        || peer.state.transport_only("cli_2222222222222222"),
    )
    .await;
    peer.state.delayed_release.notify_one();
    until(&peer.state, "held provider response released", || {
        peer.state.delayed_released.load(Ordering::SeqCst) == 1
    })
    .await;
    until(
        &peer.state,
        "held provider response write attempted",
        || peer.state.delayed_write_result.load(Ordering::SeqCst) != 0,
    )
    .await;
    until(&peer.state, "held old turn has durable terminal", || {
        let entries: Vec<Value> = walk(&options.home.join("projects"))
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .flat_map(|p| {
                std::fs::read_to_string(p)
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .collect::<Vec<_>>()
            })
            .collect();
        let Some(accepted) = entries.iter().find(|entry| {
            entry["type"] == "accepted_input" && entry.to_string().contains("reload-delayed-final")
        }) else {
            return false;
        };
        entries
            .iter()
            .any(|entry| entry["type"] == "turn_result" && entry["turnId"] == accepted["turnId"])
    })
    .await;
    peer.state.text("after-late-response");
    until(
        &peer.state,
        "fresh turn completes after late response",
        || peer.state.sent("feishu-actual-final-1-11"),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !peer.state.sent("feishu-actual-final-1-10"),
        "prior transport must not emit the delayed final after reload"
    );
    peer.state.text("reload-pending-permission");
    until(
        &peer.state,
        "real owned A7 waits before second reload",
        || {
            peer.state
                .sent
                .lock()
                .unwrap()
                .iter()
                .filter(|frame| frame.to_string().contains("回复 1 允许一次"))
                .count()
                > prompts
        },
    )
    .await;
    config["adapters"]["feishu"]["enabled"] = false.into();
    std::fs::write(&path, config.to_string()).unwrap();
    connection.receive(json!({"type":"request","id":"disable-feishu-pending","method":"reload_config","params":{}}).to_string()).await.unwrap();
    until(&peer.state, "pending reload closes prior transport", || {
        peer.state.sockets.load(Ordering::SeqCst) == 0
    })
    .await;
    let retired_third = peer.state.transport_counts("cli_2222222222222222");
    config["adapters"]["feishu"]["appId"] = "cli_3333333333333333".into();
    config["adapters"]["feishu"]["enabled"] = true.into();
    std::fs::write(&path, config.to_string()).unwrap();
    connection.receive(json!({"type":"request","id":"enable-feishu-pending","method":"reload_config","params":{}}).to_string()).await.unwrap();
    until(&peer.state, "pending reload starts new transport", || {
        peer.state.transport_only("cli_3333333333333333")
    })
    .await;
    peer.state.text("1");
    until(&peer.state, "late approval becomes new input", || {
        peer.state.sent("feishu-actual-final-1-13")
    })
    .await;
    assert!(!denied.exists(), "late approval must not execute old turn");
    let final_active = peer.state.transport_active.lock().unwrap().clone();
    let final_seen = peer.state.transport_seen.lock().unwrap().clone();
    eprintln!(
        "Feishu transport identities active={final_active:?} seen={final_seen:?}; retired={retired_first:?},{retired_second:?},{retired_third:?}"
    );
    assert_eq!(
        peer.state.transport_counts("cli_0123456789abcdef"),
        retired_first,
        "first retired app must not rediscover or reconnect"
    );
    assert_eq!(
        peer.state.transport_counts("cli_1111111111111111"),
        retired_second,
        "second retired app must not rediscover or reconnect"
    );
    assert_eq!(
        peer.state.transport_counts("cli_2222222222222222"),
        retired_third,
        "third retired app must not rediscover or reconnect"
    );
    let prompts = peer
        .state
        .sent
        .lock()
        .unwrap()
        .iter()
        .filter(|frame| frame.to_string().contains("回复 1 允许一次"))
        .count();
    peer.state.text("stop-pending-permission");
    until(
        &peer.state,
        "real owned A7 waits for permission at shutdown",
        || {
            peer.state
                .sent
                .lock()
                .unwrap()
                .iter()
                .filter(|frame| frame.to_string().contains("回复 1 允许一次"))
                .count()
                > prompts
        },
    )
    .await;
    assert!(
        !denied.exists(),
        "shutdown fixture must stop before tool approval"
    );
    connection.closed().await.unwrap();
    host.shutdown(options.drain_budget).await.unwrap();
    until(&peer.state, "shutdown closes Feishu transport", || {
        peer.state.sockets.load(Ordering::SeqCst) == 0
    })
    .await;
    let sent = peer.state.sent.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(sent, peer.state.sent.lock().unwrap().len());
    let mut logs = String::new();
    for p in walk(&options.home.join("projects")) {
        if p.extension().is_some_and(|e| e == "jsonl") {
            logs.push_str(&std::fs::read_to_string(p).unwrap());
        }
    }
    assert!(
        !denied.exists(),
        "shutdown must not execute the pending bash"
    );
    let entries = logs
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect::<Vec<_>>();
    let accepted = entries
        .iter()
        .rev()
        .find(|entry| {
            entry["type"] == "accepted_input"
                && entry["messages"]
                    .to_string()
                    .contains("stop-pending-permission")
        })
        .expect("real pending input persisted");
    assert!(
        entries
            .iter()
            .any(|entry| entry["type"] == "turn_result" && entry["turnId"] == accepted["turnId"]),
        "owned pending A7 must terminate and retain its transcript at shutdown"
    );
    for marker in [
        "feishu-actual-final-0-1",
        "feishu-actual-final-0-4",
        "feishu-actual-final-0-6",
        "feishu-actual-final-0-7",
        "feishu-actual-final-1-8",
    ] {
        assert!(logs.contains(marker), "durable {marker}");
    }
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

//! Actual Server/Socket Mode/A7. External Slack and model are loopback only.
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
struct Wire {
    incoming: tokio::sync::broadcast::Sender<String>,
    sockets: AtomicUsize,
    acks: Mutex<Vec<Value>>,
    http: Mutex<Vec<(String, String, Value)>>,
    models: Mutex<Vec<Value>>,
    phase: AtomicUsize,
    fail_auth: std::sync::atomic::AtomicBool,
    generation: AtomicUsize,
}
impl Default for Wire {
    fn default() -> Self {
        Self {
            incoming: tokio::sync::broadcast::channel(16).0,
            sockets: AtomicUsize::new(0),
            acks: Mutex::new(vec![]),
            http: Mutex::new(vec![]),
            models: Mutex::new(vec![]),
            phase: AtomicUsize::new(0),
            fail_auth: std::sync::atomic::AtomicBool::new(false),
            generation: AtomicUsize::new(0),
        }
    }
}
impl Wire {
    fn sent(&self, s: &str) -> bool {
        self.http.lock().unwrap().iter().any(|(p, _, b)| {
            p == "/api/chat.postMessage" && b["text"].as_str().is_some_and(|v| v.contains(s))
        })
    }
    fn inbound(&self, s: &str) {
        self.incoming
            .send(s.into())
            .expect("active Socket Mode peer");
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
                "Slack HTTP={:#?}; models={:#?}; ACKs={:#?}",
                self.state.http.lock().unwrap_or_else(|e| e.into_inner()),
                self.state.models.lock().unwrap_or_else(|e| e.into_inner()),
                self.state.acks.lock().unwrap_or_else(|e| e.into_inner())
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
            "{label}; HTTP={:#?}; models={:#?}; ACKs={:#?}",
            state.http.lock().unwrap_or_else(|e| e.into_inner()),
            state.models.lock().unwrap_or_else(|e| e.into_inner()),
            state.acks.lock().unwrap_or_else(|e| e.into_inner())
        )
    });
}
async fn peer(approved: std::path::PathBuf) -> Peer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let origin = format!("http://{address}");
    let state = Arc::new(Wire::default());
    let shared = state.clone();
    let task = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
            conn=listener.accept()=>{let(mut socket,_)=conn.unwrap();let state=shared.clone();let approved=approved.clone();tasks.spawn(async move{
            let mut peek=[0;8192];loop{let n=match socket.peek(&mut peek).await{Ok(n)if n>0=>n,_=>return};if peek[..n].windows(4).any(|w|w==b"\r\n\r\n"){break;}tokio::time::sleep(Duration::from_millis(1)).await;}
            if String::from_utf8_lossy(&peek).to_ascii_lowercase().contains("upgrade: websocket"){
            let mut ws=tokio_tungstenite::accept_async(socket).await.unwrap();let mut incoming=state.incoming.subscribe();state.sockets.fetch_add(1,Ordering::SeqCst);let mut sequence=0;
            loop{tokio::select!{text=incoming.recv()=>{let Ok(text)=text else{break};if text=="__REFRESH__"{if ws.send(Message::Text(json!({"type":"disconnect","reason":"refresh_requested"}).to_string().into())).await.is_err(){break;}continue;}sequence+=1;let generation=state.generation.load(Ordering::SeqCst);let envelope=json!({"type":"events_api","envelope_id":format!("g{generation}-e{sequence}"),"accepts_response_payload":false,"payload":{"type":"event_callback","event":{"type":"message","channel":"Droom","channel_type":"im","user":"HUMAN","ts":format!("{generation}.{sequence}"),"text":text}}});if ws.send(Message::Text(envelope.to_string().into())).await.is_err(){break;}},
            frame=ws.next()=>{match frame{Some(Ok(Message::Text(raw)))=>state.acks.lock().unwrap().push(serde_json::from_str(raw.as_str()).unwrap()),Some(Ok(Message::Close(_)))=>{let _=ws.close(None).await;break},Some(Ok(_))=>{},_=>break}}
            }}
            state.sockets.fetch_sub(1,Ordering::SeqCst);return;}
            let mut raw=vec![];while !raw.ends_with(b"\r\n\r\n"){let mut byte=[0];if socket.read_exact(&mut byte).await.is_err(){return}raw.push(byte[0]);assert!(raw.len()<65536);}
            let head=String::from_utf8(raw).unwrap();let path=head.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned();let auth=head.lines().find_map(|l|l.split_once(':').filter(|(k,_)|k.eq_ignore_ascii_case("authorization")).map(|(_,v)|v.trim().to_owned())).unwrap_or_default();let size=head.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length:").map(|v|v.trim().parse::<usize>().unwrap())).unwrap_or(0);let mut raw=vec![0;size];if socket.read_exact(&mut raw).await.is_err(){return}let body:Value=serde_json::from_slice(&raw).unwrap_or(Value::Null);
            let(mime,text)=if path.starts_with("/api/"){state.http.lock().unwrap().push((path.clone(),auth,body));let result=match path.as_str(){"/api/apps.connections.open"=>json!({"ok":true,"url":format!("ws://{address}/socket")}),"/api/auth.test"=>if state.fail_auth.load(Ordering::SeqCst){json!({"ok":false,"error":"mock_auth_failed"})}else{json!({"ok":true,"user_id":"BOT","user":"mockbot"})},"/api/chat.postMessage"=>json!({"ok":true,"ts":"reply"}),_=>json!({"ok":false,"error":"invalid_api_path"})};("application/json",result.to_string())
            }else if body["stream"]==true{
            state.models.lock().unwrap().push(body);let phase=state.phase.fetch_add(1,Ordering::SeqCst);let generation=state.generation.load(Ordering::SeqCst);
            let(delta,finish)=if phase==0{(json!({"role":"assistant","tool_calls":[{"index":0,"id":"slack-permission","type":"function","function":{"name":"bash","arguments":json!({"command":format!("printf approved > '{}'",approved.display())}).to_string()}}]}),"tool_calls")}else if phase==2{(json!({"role":"assistant","tool_calls":[{"index":0,"id":"slack-question","type":"function","function":{"name":"ask_user_question","arguments":json!({"questions":[{"header":"Choice","question":"Slack choose mock option","options":[{"label":"One","description":"Mock one"},{"label":"Two","description":"Mock two"}],"multiSelect":false}]}).to_string()}}]}),"tool_calls")}else{(json!({"role":"assistant","content":format!("slack-actual-final-{generation}-{phase}")}),"stop")};
            ("text/event-stream",format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",json!({"id":"mock","choices":[{"index":0,"delta":delta,"finish_reason":null}]}),json!({"id":"mock","choices":[{"index":0,"delta":{},"finish_reason":finish}]})))
            }else{("application/json",json!({"choices":[{"message":{"role":"assistant","content":"{\"title\":\"Slack actual\"}"},"finish_reason":"stop"}]}).to_string())};
            let reply=format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",text.len());let _=socket.write_all(reply.as_bytes()).await;
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
async fn slack_actual_server_official_envelope_a7_interactions_reconnect_public_reload_shutdown() {
    let (_scratch, requests) = super::management_tests::setup();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    let approved = requests.options.project.join("slack-approved.txt");
    let peer = peer(approved.clone()).await;
    let path = requests.options.home.join("pilotdeck.yaml");
    let base = std::fs::read_to_string(&path)
        .unwrap()
        .replace("http://127.0.0.1:1", &format!("{}/v1", peer.origin));
    let config = |enabled| {
        format!(
            "{base}adapters:\n  slack:\n    enabled: {enabled}\n    token: mock-bot-token\n    extra:\n      appToken: mock-app-token\n"
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
        "Slack actual sidecar={} sha256={:x}",
        options.sidecar.display(),
        sha2::Sha256::digest(&bytes)
    );
    let (host, _channels) = super::assembly::start_with_channel_endpoints(
        options.clone(),
        requests.env.clone(),
        ChannelEndpointOverrides {
            slack_api_base: Some(peer.origin.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    until(
        &peer.state,
        "Socket Mode starts from real configuration",
        || {
            peer.state.sockets.load(Ordering::SeqCst) == 1
                && peer
                    .state
                    .http
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(p, _, _)| p == "/api/auth.test")
        },
    )
    .await;
    peer.state.inbound("permission-start");
    until(
        &peer.state,
        "actual A7 tool requests Slack permission",
        || peer.state.sent("回复 1 允许一次"),
    )
    .await;
    assert!(!approved.exists());
    peer.state.inbound("1");
    until(&peer.state, "same A7 resumes from Slack decision", || {
        peer.state.sent("slack-actual-final-0-1")
    })
    .await;
    assert_eq!(std::fs::read_to_string(&approved).unwrap(), "approved");
    assert!(peer.state.sent("已允许一次"));
    assert!(
        peer.state.models.lock().unwrap()[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["role"] == "tool")
    );
    assert_eq!(
        peer.state.acks.lock().unwrap().len(),
        2,
        "official Socket Mode envelopes acknowledged exactly once"
    );
    peer.state.inbound("elicitation-start");
    until(&peer.state, "actual Slack ask_user_question prompt", || {
        peer.state.sent("Slack choose mock option")
    })
    .await;
    peer.state.inbound("1");
    until(
        &peer.state,
        "actual Slack elicitation answer resumes same A7",
        || peer.state.sent("slack-actual-final-0-3"),
    )
    .await;
    assert!(
        peer.state.models.lock().unwrap()[3]
            .to_string()
            .contains("One")
    );
    peer.state.inbound("__REFRESH__");
    until(
        &peer.state,
        "official refresh_requested reconnects owned Socket Mode",
        || {
            peer.state.sockets.load(Ordering::SeqCst) == 1
                && peer
                    .state
                    .http
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(p, _, _)| p == "/api/auth.test")
                    .count()
                    == 2
        },
    )
    .await;
    peer.state.inbound("reconnected-start");
    until(
        &peer.state,
        "reconnected transport still reaches actual A7",
        || peer.state.sent("slack-actual-final-0-4"),
    )
    .await;
    for (path, auth, _) in peer.state.http.lock().unwrap().iter() {
        assert_eq!(
            auth,
            if path == "/api/apps.connections.open" {
                "Bearer mock-app-token"
            } else {
                "Bearer mock-bot-token"
            }
        );
    }
    let frames = Arc::new(Frames::default());
    let connection = host.connect(frames.clone(), "token".into(), "fixture".into());
    connection.receive(json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"slack-lifecycle"}).to_string()).await.unwrap();
    std::fs::write(&path, config(false)).unwrap();
    connection
        .receive(
            json!({"type":"request","id":"disable-slack","method":"reload_config","params":{}})
                .to_string(),
        )
        .await
        .unwrap();
    until(&peer.state, "public reload closes Slack socket", || {
        peer.state.sockets.load(Ordering::SeqCst) == 0
    })
    .await;
    let count = peer.state.http.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(count, peer.state.http.lock().unwrap().len());
    peer.state.generation.store(1, Ordering::SeqCst);
    std::fs::write(&path, config(true)).unwrap();
    connection
        .receive(
            json!({"type":"request","id":"enable-slack","method":"reload_config","params":{}})
                .to_string(),
        )
        .await
        .unwrap();
    until(
        &peer.state,
        "reload creates second real Slack transport",
        || {
            peer.state.sockets.load(Ordering::SeqCst) == 1
                && peer
                    .state
                    .http
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(p, _, _)| p == "/api/auth.test")
                    .count()
                    == 2
        },
    )
    .await;
    peer.state.inbound("reload-start");
    until(&peer.state, "reloaded Slack A7/outbound", || {
        peer.state.sent("slack-actual-final-1-5")
    })
    .await;
    assert_eq!(peer.state.acks.lock().unwrap().len(), 6);
    connection.closed().await.unwrap();
    host.shutdown(options.drain_budget).await.unwrap();
    until(&peer.state, "shutdown closes Socket Mode", || {
        peer.state.sockets.load(Ordering::SeqCst) == 0
    })
    .await;
    let count = peer.state.http.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(count, peer.state.http.lock().unwrap().len());
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
    assert!(logs.contains("slack-actual-final-0-1") && logs.contains("slack-actual-final-1-5"));
}

struct NeverRespond;
impl crate::channel::bluebubbles::InteractionResponder for NeverRespond {
    fn respond_elicitation(
        &self,
        _: crate::channel::sms::ElicitationResponse,
    ) -> crate::channel::protocol::BoxFuture<Result<(), crate::channel::protocol::ChannelError>>
    {
        panic!("failed startup must not respond")
    }
    fn permission_decide(
        &self,
        _: crate::channel::sms::PermissionDecision,
    ) -> crate::channel::protocol::BoxFuture<
        Result<
            crate::channel::sms::PermissionDecisionOutcome,
            crate::channel::protocol::ChannelError,
        >,
    > {
        panic!("failed startup must not decide")
    }
}
#[tokio::test]
async fn slack_native_partial_auth_failure_closes_owned_socket_without_reconnect() {
    let peer =
        peer(std::env::temp_dir().join(format!("unused-slack-{}", uuid::Uuid::new_v4()))).await;
    peer.state.fail_auth.store(true, Ordering::SeqCst);
    let channel =
        crate::channel::slack::SlackChannel::new(crate::channel::slack::SlackChannelOptions {
            bot_token: Some(pilotdeck_protocol::JsValue::String("mock-bot-token".into())),
            app_token: Some(pilotdeck_protocol::JsValue::String("mock-app-token".into())),
            mapper: None,
            gateway: None,
            env: None,
            base_url: Some(peer.origin.clone().into()),
        })
        .with_responder(Arc::new(NeverRespond));
    let handle = crate::channel::protocol::ChannelAdapter::start(&channel, Default::default())
        .await
        .unwrap();
    until(
        &peer.state,
        "failed native auth closes partially opened Socket Mode",
        || peer.state.sockets.load(Ordering::SeqCst) == 0,
    )
    .await;
    handle.stop("failed-fixture").await.unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        peer.state
            .http
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _, _)| p == "/api/apps.connections.open")
            .count(),
        1,
        "failed initial auth must not schedule reconnect"
    );
    assert!(peer.state.models.lock().unwrap().is_empty());
}

//! Discord/QQ/DingTalk real wire -> Server/A7 permission and elicitation resumes.
use super::channels::ChannelEndpointOverrides;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[derive(Default)]
struct Wire {
    incoming: Mutex<VecDeque<String>>,
    requests: Mutex<Vec<(String, Value)>>,
    models: Mutex<Vec<Value>>,
    phase: AtomicUsize,
    id: AtomicUsize,
    connections: AtomicUsize,
    active: AtomicUsize,
}
impl Wire {
    fn enqueue(&self, s: &str) {
        self.incoming.lock().unwrap().push_back(s.into());
    }
    fn sent(&self, s: &str) -> bool {
        self.requests.lock().unwrap().iter().any(|(p, b)| {
            (p.ends_with("/messages") || p == "/dingtalk/send") && b.to_string().contains(s)
        })
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
                "Platform interaction raw={:#?}; models={:#?}",
                self.state
                    .requests
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()),
                self.state.models.lock().unwrap_or_else(|e| e.into_inner())
            );
        }
        self.task.abort();
    }
}
async fn peer(approved: std::path::PathBuf, platform: &'static str) -> Peer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(Wire::default());
    let shared = state.clone();
    let socket_origin = origin.replace("http://", "ws://");
    let task = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                       accepted=listener.accept()=>{let(mut socket,_)=accepted.unwrap();let state=shared.clone();let approved=approved.clone();let socket_origin=socket_origin.clone();tasks.spawn(async move{
                       let mut peek=[0;8192];loop{let n=match socket.peek(&mut peek).await{Ok(n) if n>0=>n,_=>return};if peek[..n].windows(4).any(|w|w==b"\r\n\r\n"){break;}tokio::time::sleep(Duration::from_millis(1)).await;}
                       if String::from_utf8_lossy(&peek).to_ascii_lowercase().contains("upgrade: websocket"){
                        use futures_util::{SinkExt,StreamExt};
                        let mut ws=tokio_tungstenite::accept_async(socket).await.unwrap();
                        state.connections.fetch_add(1,Ordering::SeqCst);state.active.fetch_add(1,Ordering::SeqCst);
                        let mut ready=platform=="dingtalk";let mut ticks=tokio::time::interval(Duration::from_millis(30));
                        if !ready{let _=ws.send(tokio_tungstenite::tungstenite::Message::Text(json!({"op":10,"d":{"heartbeat_interval":1000}}).to_string().into())).await;}
                        loop{tokio::select!{
                         _=ticks.tick(),if ready=>{
                          let text=state.incoming.lock().unwrap().pop_front();
                          if let Some(text)=text{let id=state.id.fetch_add(1,Ordering::SeqCst)+1;
                           let event=match platform{
                            "discord"=>json!({"op":0,"t":"MESSAGE_CREATE","s":id+1,"d":{"id":format!("event-{id}"),"channel_id":"42","type":0,"content":text,"author":{"id":"human","bot":false}}}),
                            "qq"=>json!({"op":0,"t":"GROUP_AT_MESSAGE_CREATE","s":id+1,"d":{"id":format!("event-{id}"),"group_openid":"group","author":{"member_openid":"human"},"content":text}}),
                            _=>json!({"type":"EVENT","headers":{"topic":"/v1.0/im/bot/messages/get","messageId":format!("event-{id}")},"data":json!({"msgId":format!("event-{id}"),"conversationId":"room","msgtype":"text","text":{"content":text},"sessionWebhook":"https://api.dingtalk.com/lifecycle"}).to_string()})
                           };
                           if ws.send(tokio_tungstenite::tungstenite::Message::Text(event.to_string().into())).await.is_err(){break;}
                          }
                         },
                         frame=ws.next()=>match frame{
                          Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text)))=>{
                           let v:Value=serde_json::from_str(text.as_str()).unwrap();
                           if v["op"]==2{
                            let event=if platform=="discord"{json!({"op":0,"t":"READY","s":1,"d":{"session_id":"fixture","user":{"id":"bot"},"resume_gateway_url":format!("{socket_origin}/discord/stream")}})}else{json!({"op":0,"t":"READY","s":1,"d":{"session_id":"fixture"}})};
                            if ws.send(tokio_tungstenite::tungstenite::Message::Text(event.to_string().into())).await.is_err(){break;}ready=true;
                           }else if v["op"]==1{let _=ws.send(tokio_tungstenite::tungstenite::Message::Text(json!({"op":11}).to_string().into())).await;}
                          },
                          Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_)))=>{let _=ws.close(None).await;break},
                          Some(Ok(_))=>{},_=>break
                         }
                        }}
                        state.active.fetch_sub(1,Ordering::SeqCst);return;
                       }
                       let mut header=vec![];while !header.ends_with(b"\r\n\r\n"){let mut b=[0];if socket.read_exact(&mut b).await.is_err(){return}header.push(b[0]);assert!(header.len()<65536);}
                       let header=String::from_utf8(header).unwrap();let path=header.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned();
                       let size=header.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length:").map(|v|v.trim().parse::<usize>().unwrap())).unwrap_or(0);let mut raw=vec![0;size];if socket.read_exact(&mut raw).await.is_err(){return}let body:Value=serde_json::from_slice(&raw).unwrap_or(Value::Null);
                       state.requests.lock().unwrap().push((path.clone(),body.clone()));
                       let(mime,data)=if path=="/gateway/bot"||path=="/gateway"{("application/json",json!({"url":format!("{socket_origin}/{platform}/stream")}).to_string())}
            else if path=="/qq/token"{("application/json",json!({"access_token":"mock-token","expires_in":"7200"}).to_string())}
            else if path=="/dingtalk/open"{("application/json",json!({"endpoint":format!("{socket_origin}/dingtalk/stream"),"ticket":"mock-ticket"}).to_string())}
            else if path=="/users/@me"{("application/json",json!({"id":"bot"}).to_string())}
            else if path=="/channels/42"{("application/json",json!({"id":"42","type":0}).to_string())}
            else if path.ends_with("/messages")||path.ends_with("/typing")||path=="/dingtalk/send"{("application/json",json!({"id":"outbound"}).to_string())}
            else if path.ends_with("/getMe"){("application/json",json!({"ok":true,"result":{"id":1,"username":"mock"}}).to_string())}
                       else if body["stream"]==true{
                       state.models.lock().unwrap().push(body);let phase=state.phase.fetch_add(1,Ordering::SeqCst);
                       let(delta,finish)=match phase{
                       0=>(json!({"role":"assistant","tool_calls":[{"index":0,"id":"platform-approved-tool","type":"function","function":{"name":"bash","arguments":json!({"command":format!("printf approved > '{}'",approved.display())}).to_string()}}]}),"tool_calls"),
                       1=>(json!({"role":"assistant","tool_calls":[{"index":0,"id":"platform-question","type":"function","function":{"name":"ask_user_question","arguments":json!({"questions":[{"header":"Choice","question":"Platform choose mock option","options":[{"label":"One","description":"Mock one"},{"label":"Two","description":"Mock two"}],"multiSelect":false}]}).to_string()}}]}),"tool_calls"),
                       _=>(json!({"role":"assistant","content":format!("platform-interaction-final-{phase}")}),"stop")};
                       ("text/event-stream",format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",json!({"id":"mock","choices":[{"index":0,"delta":delta,"finish_reason":null}]}),json!({"id":"mock","choices":[{"index":0,"delta":{},"finish_reason":finish}]})))
                       }else{("application/json",json!({"choices":[{"message":{"role":"assistant","content":"{\"title\":\"Platform interaction\"}"},"finish_reason":"stop"}]}).to_string())};
                       let response=format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",data.len());let _=socket.write_all(response.as_bytes()).await;
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
async fn until(state: &Wire, label: &str, mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "{label}; raw={:#?}; model={:#?}",
            state.requests.lock().unwrap_or_else(|e| e.into_inner()),
            state.models.lock().unwrap_or_else(|e| e.into_inner())
        )
    });
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
async fn run(platform: &'static str) {
    let (_scratch, requests) = super::management_tests::setup();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    let approved = requests.options.project.join("platform-approved.txt");
    let peer = peer(approved.clone(), platform).await;
    let path = requests.options.home.join("pilotdeck.yaml");
    let config = |enabled| {
        let settings = match platform {
            "discord" => json!({"enabled":enabled,"token":"mock-token"}),
            "qq" => {
                json!({"enabled":enabled,"appId":"mock-app","clientSecret":"mock-secret","allowGroups":["*"]})
            }
            _ => {
                json!({"enabled":enabled,"extra":{"clientId":"mock-id","clientSecret":"mock-secret"}})
            }
        };
        json!({"schemaVersion":1,"agent":{"model":"test/test"},"model":{"providers":{"test":{"protocol":"openai","url":format!("{}/v1",peer.origin),"apiKey":"mock","models":{"test":{}}}}},"adapters":{(platform):settings}}).to_string()
    };
    std::fs::write(&path, config(true)).unwrap();
    let (host, _channels) = super::assembly::start_with_channel_endpoints(
        super::channel_lifecycle_tests::real_options(&requests.options),
        requests.env.clone(),
        ChannelEndpointOverrides {
            discord_api_root: Some(peer.origin.clone()),
            qq_api_base: Some(peer.origin.clone()),
            qq_token_url: Some(format!("{}/qq/token", peer.origin)),
            dingtalk_stream_endpoint: Some(format!("{}/dingtalk/open", peer.origin)),
            dingtalk_webhook_route: Some({
                let route = format!("{}/dingtalk/send", peer.origin);
                Arc::new(move |_| route.clone())
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    peer.state.enqueue("platform-interaction-start");
    until(&peer.state, "real tool permission appears", || {
        peer.state.sent("回复 1 允许一次")
    })
    .await;
    assert!(!approved.exists(), "permission must gate actual bash");
    peer.state.enqueue("1");
    until(&peer.state, "same run reaches elicitation", || {
        peer.state.sent("Platform choose mock option")
    })
    .await;
    assert_eq!(std::fs::read_to_string(&approved).unwrap(), "approved");
    peer.state.enqueue("1");
    until(&peer.state, "same run resumes final", || {
        peer.state.sent("platform-interaction-final-2")
    })
    .await;
    {
        let models = peer.state.models.lock().unwrap();
        assert_eq!(models.len(), 3);
        let messages = models[2]["messages"].as_array().unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m["role"] == "tool" && m["content"].to_string().contains("One"))
        );
        assert!(
            models[1]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["role"] == "tool")
        );
    }
    let frames = Arc::new(Frames::default());
    let connection = host.connect(frames.clone(), "token".into(), "fixture".into());
    connection.receive(json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"platform-interaction"}).to_string()).await.unwrap();
    std::fs::write(&path, config(false)).unwrap();
    connection
        .receive(
            json!({"type":"request","id":"disable-platform","method":"reload_config","params":{}})
                .to_string(),
        )
        .await
        .unwrap();
    until(&peer.state, "public disable RPC", || {
        frames
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|v| v["id"] == "disable-platform" && v["ok"] == true)
    })
    .await;
    until(&peer.state, "disabled owner closes real WS", || {
        peer.state.active.load(Ordering::SeqCst) == 0
    })
    .await;
    let old = peer.state.connections.load(Ordering::SeqCst);
    std::fs::write(&path, config(true)).unwrap();
    connection
        .receive(
            json!({"type":"request","id":"enable-platform","method":"reload_config","params":{}})
                .to_string(),
        )
        .await
        .unwrap();
    until(
        &peer.state,
        "public reload constructs real second adapter",
        || peer.state.connections.load(Ordering::SeqCst) == old + 1,
    )
    .await;
    peer.state.enqueue("platform-second-generation");
    until(&peer.state, "second adapter reaches A7", || {
        peer.state.sent("platform-interaction-final-3")
    })
    .await;
    host.shutdown(requests.options.drain_budget).await.unwrap();
    until(&peer.state, "shutdown closes owned socket", || {
        peer.state.active.load(Ordering::SeqCst) == 0
    })
    .await;
    let old = peer.state.connections.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        old,
        peer.state.connections.load(Ordering::SeqCst),
        "no reconnect after shutdown"
    );
    let mut pending = vec![requests.options.home.join("projects")];
    let mut logs = String::new();
    while let Some(dir) = pending.pop() {
        for row in std::fs::read_dir(dir).unwrap() {
            let p = row.unwrap().path();
            if p.is_dir() {
                pending.push(p)
            } else if p.extension().is_some_and(|e| e == "jsonl") {
                logs.push_str(&std::fs::read_to_string(p).unwrap());
            }
        }
    }
    assert!(logs.contains("platform-interaction-final-2"));
    assert!(logs.contains("platform-interaction-final-3"));
}

#[tokio::test]
async fn discord_actual_server_a7_permission_elicitation_same_run_public_reload_shutdown() {
    run("discord").await;
}
#[tokio::test]
async fn qq_actual_server_a7_permission_elicitation_same_run_public_reload_shutdown() {
    run("qq").await;
}
#[tokio::test]
async fn dingtalk_actual_server_a7_permission_elicitation_same_run_public_reload_shutdown() {
    run("dingtalk").await;
}

//! Telegram real Server/A7 permission and elicitation resumes; public reload lifecycle.
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
}
impl Wire {
    fn enqueue(&self, s: &str) {
        self.incoming.lock().unwrap().push_back(s.into());
    }
    fn sent(&self, s: &str) -> bool {
        self.requests.lock().unwrap().iter().any(|(p, b)| {
            p.ends_with("/sendMessage") && b["text"].as_str().is_some_and(|v| v.contains(s))
        })
    }
    fn count(&self, s: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.ends_with(s))
            .count()
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
                "Telegram interaction raw={:#?}; models={:#?}",
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
async fn peer(approved: std::path::PathBuf) -> Peer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(Wire::default());
    let shared = state.clone();
    let task = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
            accepted=listener.accept()=>{let(mut socket,_)=accepted.unwrap();let state=shared.clone();let approved=approved.clone();tasks.spawn(async move{
            let mut header=vec![];while !header.ends_with(b"\r\n\r\n"){let mut b=[0];if socket.read_exact(&mut b).await.is_err(){return}header.push(b[0]);assert!(header.len()<65536);}
            let header=String::from_utf8(header).unwrap();let path=header.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned();
            let size=header.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length:").map(|v|v.trim().parse::<usize>().unwrap())).unwrap_or(0);let mut raw=vec![0;size];if socket.read_exact(&mut raw).await.is_err(){return}let body:Value=serde_json::from_slice(&raw).unwrap_or(Value::Null);
            state.requests.lock().unwrap().push((path.clone(),body.clone()));
            let(mime,data)=if path.ends_with("/getMe"){("application/json",json!({"ok":true,"result":{"id":1,"username":"mock"}}).to_string())}
            else if path.ends_with("/getUpdates"){
            let incoming=state.incoming.lock().unwrap().pop_front();tokio::time::sleep(Duration::from_millis(40)).await;
            let rows=incoming.map(|text|{let id=state.id.fetch_add(1,Ordering::SeqCst)+1;json!({"update_id":id,"message":{"message_id":id,"chat":{"id":42,"type":"private"},"from":{"id":7},"text":text}})}).into_iter().collect::<Vec<_>>();
            ("application/json",json!({"ok":true,"result":rows}).to_string())
            }else if path.ends_with("/sendMessage")||path.ends_with("/sendChatAction")||path.ends_with("/deleteWebhook"){("application/json",json!({"ok":true,"result":{}}).to_string())}
            else if body["stream"]==true{
            state.models.lock().unwrap().push(body);let phase=state.phase.fetch_add(1,Ordering::SeqCst);
            let(delta,finish)=match phase{
            0=>(json!({"role":"assistant","tool_calls":[{"index":0,"id":"telegram-approved-tool","type":"function","function":{"name":"bash","arguments":json!({"command":format!("printf approved > '{}'",approved.display())}).to_string()}}]}),"tool_calls"),
            1=>(json!({"role":"assistant","tool_calls":[{"index":0,"id":"telegram-question","type":"function","function":{"name":"ask_user_question","arguments":json!({"questions":[{"header":"Choice","question":"Telegram choose mock option","options":[{"label":"One","description":"Mock one"},{"label":"Two","description":"Mock two"}],"multiSelect":false}]}).to_string()}}]}),"tool_calls"),
            _=>(json!({"role":"assistant","content":format!("telegram-interaction-final-{phase}")}),"stop")};
            ("text/event-stream",format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",json!({"id":"mock","choices":[{"index":0,"delta":delta,"finish_reason":null}]}),json!({"id":"mock","choices":[{"index":0,"delta":{},"finish_reason":finish}]})))
            }else{("application/json",json!({"choices":[{"message":{"role":"assistant","content":"{\"title\":\"Telegram interaction\"}"},"finish_reason":"stop"}]}).to_string())};
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
#[tokio::test]
async fn telegram_actual_server_a7_permission_elicitation_same_run_public_reload_shutdown() {
    let (_scratch, requests) = super::management_tests::setup();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    let approved = requests.options.project.join("telegram-approved.txt");
    let peer = peer(approved.clone()).await;
    let path = requests.options.home.join("pilotdeck.yaml");
    let base = std::fs::read_to_string(&path)
        .unwrap()
        .replace("http://127.0.0.1:1", &format!("{}/v1", peer.origin));
    let config = |enabled| {
        format!("{base}adapters:\n  telegram:\n    enabled: {enabled}\n    token: mock-token\n")
    };
    std::fs::write(&path, config(true)).unwrap();
    let (host, _channels) = super::assembly::start_with_channel_endpoints(
        super::channel_lifecycle_tests::real_options(&requests.options),
        requests.env.clone(),
        ChannelEndpointOverrides {
            telegram_api_root: Some(peer.origin.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    peer.state.enqueue("telegram-interaction-start");
    until(&peer.state, "real tool permission appears", || {
        peer.state.sent("回复 1 允许一次")
    })
    .await;
    assert!(!approved.exists(), "permission must gate actual bash");
    peer.state.enqueue("1");
    until(&peer.state, "same run reaches elicitation", || {
        peer.state.sent("Telegram choose mock option")
    })
    .await;
    assert_eq!(std::fs::read_to_string(&approved).unwrap(), "approved");
    peer.state.enqueue("1");
    until(&peer.state, "same run resumes final", || {
        peer.state.sent("telegram-interaction-final-2")
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
    connection.receive(json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"telegram-interaction"}).to_string()).await.unwrap();
    std::fs::write(&path, config(false)).unwrap();
    connection
        .receive(
            json!({"type":"request","id":"disable-telegram","method":"reload_config","params":{}})
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
            .any(|v| v["id"] == "disable-telegram" && v["ok"] == true)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let count = peer.state.count("/getUpdates");
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(count, peer.state.count("/getUpdates"));
    let old = peer.state.count("/getMe");
    std::fs::write(&path, config(true)).unwrap();
    connection
        .receive(
            json!({"type":"request","id":"enable-telegram","method":"reload_config","params":{}})
                .to_string(),
        )
        .await
        .unwrap();
    until(
        &peer.state,
        "public reload constructs real second adapter",
        || peer.state.count("/getMe") == old + 1,
    )
    .await;
    peer.state.enqueue("telegram-second-generation");
    until(&peer.state, "second adapter reaches A7", || {
        peer.state.sent("telegram-interaction-final-3")
    })
    .await;
    host.shutdown(requests.options.drain_budget).await.unwrap();
    let count = peer.state.count("/getUpdates");
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(count, peer.state.count("/getUpdates"));
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
    assert!(logs.contains("telegram-interaction-final-2"));
    assert!(logs.contains("telegram-interaction-final-3"));
}

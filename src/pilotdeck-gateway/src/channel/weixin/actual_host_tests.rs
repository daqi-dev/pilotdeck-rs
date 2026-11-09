//! Actual Server/A7/sidecar; only external ilink/model endpoints are loopback.
//! Registered under native_process, like wecom::actual_host_tests.
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
    sent: Mutex<Vec<Value>>,
    models: Mutex<Vec<Value>>,
    tools: AtomicUsize,
}
impl Wire {
    fn enqueue(&self, s: &str) {
        self.incoming.lock().unwrap().push_back(s.into());
    }
    fn contains(&self, s: &str) -> bool {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .any(|v| v.to_string().contains(s))
    }
    fn messages(&self, s: &str) -> usize {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v.to_string().contains(s))
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
                "Weixin outbound={:#?}; model={:#?}",
                self.state.sent.lock().unwrap_or_else(|e| e.into_inner()),
                self.state.models.lock().unwrap_or_else(|e| e.into_inner())
            );
        }
        self.task.abort();
    }
}
async fn until(state: &Wire, label: &str, mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(25), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "{label}; outbound={:#?}; models={:#?}",
            state.sent.lock().unwrap_or_else(|e| e.into_inner()),
            state.models.lock().unwrap_or_else(|e| e.into_inner())
        )
    });
}
async fn peer(approved: std::path::PathBuf, denied: std::path::PathBuf) -> Peer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(Wire::default());
    let shared = state.clone();
    let task = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
            conn=listener.accept()=>{let(mut socket,_)=conn.unwrap();let state=shared.clone();let(approved,denied)=(approved.clone(),denied.clone());children.spawn(async move{
            let mut raw=vec![];while !raw.ends_with(b"\r\n\r\n"){let mut byte=[0];if socket.read_exact(&mut byte).await.is_err(){return;}raw.push(byte[0]);assert!(raw.len()<65536);}
            let head=String::from_utf8(raw).unwrap();let path=head.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned();
            let size=head.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length:").map(|v|v.trim().parse::<usize>().unwrap())).unwrap_or(0);
            let mut raw=vec![0;size];if socket.read_exact(&mut raw).await.is_err(){return;}let body:Value=serde_json::from_slice(&raw).unwrap_or(Value::Null);
            let(mime,text)=if path.ends_with("/getupdates"){
            let next=state.incoming.lock().unwrap().pop_front();tokio::time::sleep(Duration::from_millis(50)).await;
            let msgs=next.map(|text|vec![json!({"from_user_id":"chat","context_token":"ctx","message_type":1,"item_list":[{"type":1,"text_item":{"text":text}}]})]).unwrap_or_default();
            ("application/json",json!({"ret":0,"msgs":msgs,"get_updates_buf":"cursor"}).to_string())
            }else if path.ends_with("/sendmessage"){state.sent.lock().unwrap().push(body);("application/json",json!({"ret":0}).to_string())
            }else if path.ends_with("/getconfig")||path.ends_with("/sendtyping"){("application/json",json!({"ret":0}).to_string())
            }else if body["stream"]==true{
            state.models.lock().unwrap().push(body.clone());
            let phase=state.tools.fetch_add(1,Ordering::SeqCst);
            let (delta,finish)=match phase{
            0|4=>{let file=if phase==0{approved}else{denied};let command=format!("printf approved > '{}'",file.display());(json!({"role":"assistant","tool_calls":[{"index":0,"id":format!("tool-{phase}"),"type":"function","function":{"name":"bash","arguments":json!({"command":command}).to_string()}}]}),"tool_calls")},
            2=>(json!({"role":"assistant","tool_calls":[{"index":0,"id":"question-2","type":"function","function":{"name":"ask_user_question","arguments":json!({"questions":[{"header":"Choice","question":"Choose mock option","options":[{"label":"One","description":"First mock"},{"label":"Two","description":"Second mock"}],"multiSelect":false}]}).to_string()}}]}),"tool_calls"),
            _=>(json!({"role":"assistant","content":format!("weixin-actual-final-{phase}")}),"stop")};
            ("text/event-stream",format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",json!({"id":"mock","choices":[{"index":0,"delta":delta,"finish_reason":null}]}),json!({"id":"mock","choices":[{"index":0,"delta":{},"finish_reason":finish}]})))
            }else{("application/json",json!({"choices":[{"message":{"role":"assistant","content":"{\"title\":\"Weixin actual\"}"},"finish_reason":"stop"}]}).to_string())};
            let reply=format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",text.len());let _=socket.write_all(reply.as_bytes()).await;
            });},Some(_)=children.join_next(),if !children.is_empty()=>{}
            }
        }
    });
    Peer {
        origin,
        state,
        task,
    }
}
#[tokio::test]
async fn weixin_actual_server_a7_permissions_elicitation_commands_new_and_shutdown() {
    let (_scratch, requests) = super::management_tests::setup();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    let approved = requests.options.project.join("weixin-approved.txt");
    let denied = requests.options.project.join("weixin-must-not-exist.txt");
    let peer = peer(approved.clone(), denied.clone()).await;
    let config = requests.options.home.join("pilotdeck.yaml");
    let base = std::fs::read_to_string(&config)
        .unwrap()
        .replace("http://127.0.0.1:1", &format!("{}/v1", peer.origin));
    std::fs::write(
        &config,
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
        "Weixin actual sidecar={} sha256={:x}",
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
    peer.state.enqueue("/help");
    until(&peer.state, "central help reply", || {
        peer.state.contains("/projects")
    })
    .await;
    assert_eq!(
        peer.state.tools.load(Ordering::SeqCst),
        0,
        "help bypasses model"
    );
    let before_projects = peer.state.sent.lock().unwrap().len();
    peer.state.enqueue("/projects");
    until(&peer.state, "real host list_projects", || {
        peer.state.sent.lock().unwrap()[before_projects..]
            .iter()
            .any(|v| v.to_string().contains("项目"))
    })
    .await;
    assert_eq!(peer.state.tools.load(Ordering::SeqCst), 0);
    peer.state.enqueue("permission-start");
    until(
        &peer.state,
        "model tool produces IM permission request",
        || peer.state.contains("回复 1 允许一次"),
    )
    .await;
    assert!(!approved.exists(), "tool must await actual user decision");
    peer.state.enqueue("1");
    until(&peer.state, "same A7 resumes after IM decision", || {
        peer.state.contains("weixin-actual-final-1")
    })
    .await;
    assert_eq!(std::fs::read_to_string(&approved).unwrap(), "approved");
    assert!(peer.state.contains("已允许一次"));
    let model = peer.state.models.lock().unwrap().clone();
    assert_eq!(model.len(), 2);
    assert!(
        model[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool"),
        "same model history carries executed tool result"
    );
    peer.state.enqueue("elicitation-start");
    until(&peer.state, "real ask_user_question prompt", || {
        peer.state.contains("Choose mock option")
    })
    .await;
    peer.state.enqueue("1");
    until(
        &peer.state,
        "same A7 resumes after elicitation answer",
        || peer.state.contains("weixin-actual-final-3"),
    )
    .await;
    let model = peer.state.models.lock().unwrap().clone();
    assert!(
        model[3].to_string().contains("One"),
        "actual tool result carries chosen label"
    );
    let prompts = peer.state.messages("回复 1 允许一次");
    peer.state.enqueue("pending-new-start");
    until(&peer.state, "second pending permission", || {
        peer.state.messages("回复 1 允许一次") > prompts
    })
    .await;
    peer.state.enqueue("/new");
    until(&peer.state, "new resets pending permission", || {
        peer.state.contains("已创建新会话")
    })
    .await;
    peer.state.enqueue("post-new");
    until(&peer.state, "new session runs after abort", || {
        peer.state.contains("weixin-actual-final-5")
    })
    .await;
    peer.state.enqueue("1");
    until(
        &peer.state,
        "old pending answer is cleared and now becomes normal A7 input",
        || peer.state.contains("weixin-actual-final-6"),
    )
    .await;
    assert!(!denied.exists(), "new must abort original pending tool");
    host.shutdown(options.drain_budget).await.unwrap();
    let count = peer.state.sent.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        count,
        peer.state.sent.lock().unwrap().len(),
        "no outgoing IM after Gateway shutdown"
    );
    let mut logs = String::new();
    fn collect(path: &std::path::Path, out: &mut String) {
        for entry in std::fs::read_dir(path).unwrap() {
            let p = entry.unwrap().path();
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
    assert!(
        logs.contains("weixin-actual-final-1")
            && logs.contains("weixin-actual-final-3")
            && logs.contains("weixin-actual-final-6"),
        "durable transcripts retain completed interactive runs"
    );
}

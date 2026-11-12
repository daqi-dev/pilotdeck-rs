//! Actual native Server/A7/sidecar with loopback Twilio REST and model.
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
                "SMS sent={:#?}; models={:#?}",
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
fn sms_lifecycle_is(home: &std::path::Path, state: &str, message: &str) -> bool {
    use pilotdeck_protocol::JsValue;
    let snapshot = JsValue::Object(crate::channel::runtime_status::read_snapshot(home));
    let Some(entry) = snapshot.get("channels").and_then(|channels| channels.get("sms")) else {
        return false;
    };
    entry
        .get("state")
        .and_then(JsValue::as_string)
        .and_then(|value| value.to_utf8().ok())
        .as_deref()
        == Some(state)
        && entry
            .get("message")
            .and_then(JsValue::as_string)
            .and_then(|value| value.to_utf8().ok())
            .as_deref()
            == Some(message)
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
            let (status,mime,text)=if path.contains("/2010-04-01/Accounts/"){
            let form:serde_json::Map<String,Value>=url::form_urlencoded::parse(&raw).map(|(k,v)|(k.into_owned(),Value::String(v.into_owned()))).collect();state.sent.lock().unwrap().push(Value::Object(form));("201 Created","application/json",json!({"sid":"SMmock"}).to_string())
            }else{let body:Value=serde_json::from_slice(&raw).unwrap_or(Value::Null);if body["stream"]==true{
            state.models.lock().unwrap().push(body);let phase=state.phase.fetch_add(1,Ordering::SeqCst);let generation=state.generation.load(Ordering::SeqCst);
            let(delta,finish)=match phase{
            0=>(json!({"role":"assistant","tool_calls":[{"index":0,"id":"sms-permission","type":"function","function":{"name":"bash","arguments":json!({"command":format!("printf approved > '{}'",approved.display())}).to_string()}}]}),"tool_calls"),
            2=>(json!({"role":"assistant","tool_calls":[{"index":0,"id":"sms-question","type":"function","function":{"name":"ask_user_question","arguments":json!({"questions":[{"header":"Choice","question":"SMS choose mock option","options":[{"label":"One","description":"Mock one"},{"label":"Two","description":"Mock two"}],"multiSelect":false}]}).to_string()}}]}),"tool_calls"),
            _=>(json!({"role":"assistant","content":format!("sms-actual-final-{generation}-{phase}")}),"stop")};
            ("200 OK","text/event-stream",format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",json!({"id":"mock","choices":[{"index":0,"delta":delta,"finish_reason":null}]}),json!({"id":"mock","choices":[{"index":0,"delta":{},"finish_reason":finish}]})))
            }else{("200 OK","application/json",json!({"choices":[{"message":{"role":"assistant","content":"{\"title\":\"SMS actual\"}"},"finish_reason":"stop"}]}).to_string())}};
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
fn signature(url: &str, text: &str) -> String {
    use base64::Engine;
    use sha1::{Digest, Sha1};
    let mut key = [0u8; 64];
    key[..10].copy_from_slice(b"mock-token");
    let inner: Vec<_> = key.iter().map(|b| b ^ 0x36).collect();
    let outer: Vec<_> = key.iter().map(|b| b ^ 0x5c).collect();
    let mut hash = Sha1::new();
    hash.update(inner);
    hash.update(format!("{url}Body{text}From+15550000002").as_bytes());
    let result = hash.finalize();
    let mut hash = Sha1::new();
    hash.update(outer);
    hash.update(result);
    base64::engine::general_purpose::STANDARD.encode(hash.finalize())
}
async fn incoming(port: u16, text: &str) -> Value {
    let url = format!("http://127.0.0.1:{port}/sms/incoming");
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("From", "+15550000002")
        .append_pair("Body", text)
        .finish();
    let request = format!(
        "POST /sms/incoming HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/x-www-form-urlencoded\r\nX-Twilio-Signature: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        signature(&url, text),
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
    .expect("configured SMS listener readiness")
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
async fn sms_actual_server_a7_permission_elicitation_public_reload_shutdown() {
    let (_scratch, requests) = super::management_tests::setup();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    let approved = requests.options.project.join("sms-approved.txt");
    let peer = peer(approved.clone()).await;
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let path = requests.options.home.join("pilotdeck.yaml");
    let base = std::fs::read_to_string(&path)
        .unwrap()
        .replace("http://127.0.0.1:1", &format!("{}/v1", peer.origin));
    let config = |enabled| {
        format!(
            "{base}adapters:\n  sms:\n    enabled: {enabled}\n    extra:\n      accountSid: AC00000000000000000000000000000000\n      authToken: mock-token\n      phoneNumber: '+15550000001'\n      webhookHost: 127.0.0.1\n      webhookPort: {port}\n      publicUrl: http://127.0.0.1:{port}\n"
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
        "SMS actual sidecar={} sha256={:x}; port reservation is a bounded bind/drop diagnostic seam",
        options.sidecar.display(),
        sha2::Sha256::digest(&bytes)
    );
    let (host, _channels) = super::assembly::start_with_channel_endpoints(
        options.clone(),
        requests.env.clone(),
        ChannelEndpointOverrides {
            sms_twilio_api_base: Some(peer.origin.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // A bound listener can answer before adapter.start's handle is registered.
    // The lifecycle report is published after insertion into the running set
    // that shutdown owns; close deliberately does not wait for pending starts.
    until(&peer.state, "initial SMS handle registered", || {
        sms_lifecycle_is(&options.home, "connected", "sms: started")
    })
    .await;
    assert_eq!(incoming(port, "permission-start").await["status"], 200);
    until(&peer.state, "actual A7 permission request -> SMS", || {
        peer.state.contains("回复 1 允许一次")
    })
    .await;
    assert!(!approved.exists(), "permission gate must hold tool");
    assert_eq!(incoming(port, "1").await["status"], 200);
    until(&peer.state, "same A7 resumes from SMS decision", || {
        peer.state.contains("sms-actual-final-0-1")
    })
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
    until(&peer.state, "actual ask_user_question -> SMS", || {
        peer.state.contains("SMS choose mock option")
    })
    .await;
    assert_eq!(incoming(port, "1").await["status"], 200);
    until(&peer.state, "same A7 resumes from SMS elicitation", || {
        peer.state.contains("sms-actual-final-0-3")
    })
    .await;
    assert!(
        peer.state.models.lock().unwrap()[3]
            .to_string()
            .contains("One")
    );
    let frames = Arc::new(Frames::default());
    let connection = host.connect(frames.clone(), "token".into(), "fixture".into());
    connection.receive(json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"sms-lifecycle"}).to_string()).await.unwrap();
    std::fs::write(&path, config(false)).unwrap();
    connection
        .receive(
            json!({"type":"request","id":"disable-sms","method":"reload_config","params":{}})
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
            .any(|v| v["id"] == "disable-sms" && v["ok"] == true)
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
    .expect("disabled SMS listener closes");
    // Observe the stopped generation before looking for a fresh connected
    // report, so the pre-reload status cannot satisfy the readiness check.
    until(&peer.state, "disabled SMS handle stopped", || {
        sms_lifecycle_is(&options.home, "stopped", "sms: stopped (config-disabled)")
    })
    .await;
    let count = peer.state.sent.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(count, peer.state.sent.lock().unwrap().len());
    peer.state.generation.store(1, Ordering::SeqCst);
    std::fs::write(&path, config(true)).unwrap();
    connection
        .receive(
            json!({"type":"request","id":"enable-sms","method":"reload_config","params":{}})
                .to_string(),
        )
        .await
        .unwrap();
    until(&peer.state, "reloaded SMS handle registered", || {
        sms_lifecycle_is(&options.home, "connected", "sms: started")
    })
    .await;
    assert_eq!(incoming(port, "reload-start").await["status"], 200);
    until(&peer.state, "fresh SMS adapter still uses real A7", || {
        peer.state.contains("sms-actual-final-1-4")
    })
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
        "sms-actual-final-0-1",
        "sms-actual-final-0-3",
        "sms-actual-final-1-4",
    ] {
        assert!(logs.contains(marker), "durable transcript {marker}");
    }
}

async fn cron_public_rpc(
    connection: &crate::runtime::turn::gateway::HostedConnection,
    frames: &Frames,
    state: &Wire,
    id: &str,
    method: &str,
    params: Value,
) -> Value {
    connection
        .receive(json!({"type":"request","id":id,"method":method,"params":params}).to_string())
        .await
        .unwrap();
    until(state, id, || {
        frames.0.lock().unwrap().iter().any(|row| row["id"] == id)
    })
    .await;
    let reply = frames
        .0
        .lock()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap()
        .clone();
    assert_eq!(reply["ok"], true, "public {method}: {reply}");
    reply["result"].clone()
}
async fn cron_completed_run(
    connection: &crate::runtime::turn::gateway::HostedConnection,
    frames: &Frames,
    state: &Wire,
    project: &str,
    task: &str,
    label: &str,
) -> Value {
    let end = tokio::time::Instant::now() + Duration::from_secs(25);
    for index in 0.. {
        let listing = cron_public_rpc(
            connection,
            frames,
            state,
            &format!("{label}-{index}"),
            "cron_list",
            json!({"projectKey":project,"includeHistory":true,"limit":100}),
        )
        .await;
        if let Some(run) = listing["recentRuns"]
            .as_array()
            .and_then(|runs| runs.iter().find(|run| run["taskId"] == task))
        {
            assert_eq!(run["outcome"], "completed", "{listing}");
            return run.clone();
        }
        assert!(
            tokio::time::Instant::now() < end,
            "missing durable Cron terminal: {listing}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    unreachable!()
}

#[tokio::test]
async fn sms_actual_server_cron_a7_twilio_delivery_and_disabled_source_is_quiet() {
    let (_scratch, requests) = super::management_tests::setup();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    let peer = peer(requests.options.project.join("unused-cron-permission")).await;
    // The shared model peer's phase >=4 is an ordinary actual model response;
    // it advertises no fabricated inner gateway or permission completion.
    peer.state.phase.store(4, Ordering::SeqCst);
    let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reserve.local_addr().unwrap().port();
    drop(reserve);
    let path = requests.options.home.join("pilotdeck.yaml");
    let base = std::fs::read_to_string(&path)
        .unwrap()
        .replace("http://127.0.0.1:1", &format!("{}/v1", peer.origin));
    let config = |enabled| {
        format!(
            "{base}cron: {{enabled: true, timezone: UTC}}\nadapters:\n  sms:\n    enabled: {enabled}\n    extra:\n      accountSid: AC00000000000000000000000000000000\n      authToken: mock-token\n      phoneNumber: '+15550000001'\n      webhookHost: 127.0.0.1\n      webhookPort: {port}\n      publicUrl: http://127.0.0.1:{port}\n"
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
    let sidecar = std::fs::read(&options.sidecar).expect("actual sidecar required");
    use sha2::Digest;
    eprintln!(
        "SMS Cron actual sidecar={} sha256={:x}",
        options.sidecar.display(),
        sha2::Sha256::digest(&sidecar)
    );
    let (host, _channels) = super::assembly::start_with_channel_endpoints(
        options.clone(),
        requests.env.clone(),
        ChannelEndpointOverrides {
            sms_twilio_api_base: Some(peer.origin.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let frames = Arc::new(Frames::default());
    let connection = host.connect(frames.clone(), "token".into(), "cron-fixture".into());
    connection.receive(json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"sms-cron-consumer"}).to_string()).await.unwrap();
    let project = options.project.to_string_lossy().into_owned();
    let params = |message: &str| {
        json!({"projectKey":project,"message":message,
        "sessionKey":"sms:chat=+15550000002:general","channelKey":"sms","mode":"default",
        "schedule":{"type":"once","runAt":"2036-01-01T00:00:00.000Z"}})
    };
    let created = cron_public_rpc(
        &connection,
        &frames,
        &peer.state,
        "sms-cron-create",
        "cron_create",
        params("SMS_CRON_ENABLED_ACTUAL"),
    )
    .await;
    let task = created["task"]["taskId"].as_str().unwrap();
    let started = cron_public_rpc(
        &connection,
        &frames,
        &peer.state,
        "sms-cron-run",
        "cron_run_now",
        json!({"projectKey":project,"taskId":task}),
    )
    .await;
    assert_eq!(started["started"], true);
    let first = cron_completed_run(
        &connection,
        &frames,
        &peer.state,
        &project,
        started["taskId"].as_str().unwrap(),
        "sms-cron-persist",
    )
    .await;
    until(
        &peer.state,
        "Cron actual A7 output reaches Twilio REST",
        || peer.state.contains("sms-actual-final-0-4"),
    )
    .await;
    {
        let sent = peer.state.sent.lock().unwrap();
        let output = sent
            .iter()
            .find(|row| {
                row["Body"]
                    .as_str()
                    .is_some_and(|s| s.contains("sms-actual-final-0-4"))
            })
            .unwrap();
        assert_eq!(output["From"], "+15550000001");
        assert_eq!(output["To"], "+15550000002");
    }
    assert!(peer.state.models.lock().unwrap().iter().any(|model| {
        model["messages"]
            .to_string()
            .contains("SMS_CRON_ENABLED_ACTUAL")
    }));
    until(&peer.state, "real Cron terminal notification", || {
        frames.0.lock().unwrap().iter().any(|frame| {
            frame["name"] == "always-on:turn-event"
                && frame["payload"]["event"]["runId"] == first["runId"]
                && frame["payload"]["event"]["type"] == "turn_completed"
        })
    })
    .await;
    std::fs::write(&path, config(false)).unwrap();
    cron_public_rpc(
        &connection,
        &frames,
        &peer.state,
        "sms-cron-disable",
        "reload_config",
        json!({}),
    )
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
    .expect("disabled production SMS listener closes");
    let before = peer.state.sent.lock().unwrap().len();
    let disabled = cron_public_rpc(
        &connection,
        &frames,
        &peer.state,
        "sms-disabled-create",
        "cron_create",
        params("SMS_CRON_DISABLED_ACTUAL"),
    )
    .await;
    let disabled_id = disabled["task"]["taskId"].as_str().unwrap();
    let disabled_started = cron_public_rpc(
        &connection,
        &frames,
        &peer.state,
        "sms-disabled-run",
        "cron_run_now",
        json!({"projectKey":project,"taskId":disabled_id}),
    )
    .await;
    cron_completed_run(
        &connection,
        &frames,
        &peer.state,
        &project,
        disabled_started["taskId"].as_str().unwrap(),
        "sms-disabled-persist",
    )
    .await;
    assert!(peer.state.models.lock().unwrap().iter().any(|model| {
        model["messages"]
            .to_string()
            .contains("SMS_CRON_DISABLED_ACTUAL")
    }));
    connection.closed().await.unwrap();
    // Owned shutdown is the final negative barrier: no pending delivery can
    // appear after this return. Merely sleeping while no send occurred is not enough.
    host.shutdown(options.drain_budget).await.unwrap();
    assert_eq!(
        peer.state.sent.lock().unwrap().len(),
        before,
        "disabled origin cannot send Cron output"
    );
    assert!(!peer.state.contains("sms-actual-final-0-5"));
}

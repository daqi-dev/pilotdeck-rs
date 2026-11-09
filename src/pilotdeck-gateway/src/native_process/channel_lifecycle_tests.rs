//! Real Server configuration + A7 + loopback wire lifecycle (no TurnGateway stub).
use super::channels::ChannelEndpointOverrides;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn real_options(base: &super::Options) -> super::Options {
    let mut options = base.clone();
    options.sidecar = std::env::var_os("PDRS_CHANNEL_SIDECAR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            let exe = std::env::current_exe().expect("test executable");
            exe.parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("pilotdeck-agent-loop-sidecar")
        });
    let bytes = std::fs::read(&options.sidecar)
        .expect("real sidecar binary must be built or PDRS_CHANNEL_SIDECAR supplied");
    assert!(
        bytes.len() > 1024,
        "lifecycle requires real sidecar, never setup fixture text"
    );
    use sha2::Digest;
    eprintln!(
        "channel lifecycle sidecar={} sha256={:x}",
        options.sidecar.display(),
        sha2::Sha256::digest(&bytes)
    );
    options
}

#[derive(Default)]
struct Wire {
    generation: std::sync::atomic::AtomicUsize,
    delivered: Mutex<std::collections::HashSet<usize>>,
    feishu_acks: std::sync::atomic::AtomicUsize,
    feishu_connections: std::sync::atomic::AtomicUsize,
    requests: Mutex<Vec<(String, Value)>>,
}
struct Mock {
    origin: String,
    state: Arc<Wire>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "channel lifecycle raw requests={:#?}",
                self.state
                    .requests
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
            );
        }
        self.task.abort();
    }
}
async fn mock() -> Mock {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(Wire::default());
    let shared = state.clone();
    let socket_origin = origin.replace("http://", "ws://");
    let task = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                             connection=listener.accept()=>{let(mut socket,_)=connection.unwrap();let state=shared.clone();let socket_origin=socket_origin.clone();tasks.spawn(async move{
                              let mut peek=vec![0;8192];
                    loop {let n=match socket.peek(&mut peek).await{Ok(n) if n>0=>n,_=>return};if peek[..n].windows(4).any(|w|w==b"\r\n\r\n"){break;}tokio::time::sleep(Duration::from_millis(1)).await;}
                    if String::from_utf8_lossy(&peek).to_ascii_lowercase().contains("upgrade: websocket"){
                     use futures_util::{SinkExt,StreamExt};
                     if String::from_utf8_lossy(&peek).contains("/feishu/stream"){
                        use base64::Engine;
                        let mut ws=tokio_tungstenite::accept_async(socket).await.unwrap();
                        state.feishu_connections.fetch_add(1,Ordering::SeqCst);
                        let gold:Value=serde_json::from_str(include_str!("../channel/feishu/sdk-oracle.json")).unwrap();
                        let mut sent=false;
                        while let Some(frame)=ws.next().await{match frame{
                            Ok(tokio_tungstenite::tungstenite::Message::Binary(_))=>{
                                if !sent{sent=true;for row in gold["rows"].as_array().unwrap(){if let Some(raw)=row["inboundBinary"].as_str(){let bytes=base64::engine::general_purpose::STANDARD.decode(raw).unwrap();ws.send(tokio_tungstenite::tungstenite::Message::Binary(bytes.into())).await.unwrap();}}}
                                else{state.feishu_acks.fetch_add(1,Ordering::SeqCst);}
                            },
                            Ok(tokio_tungstenite::tungstenite::Message::Close(_))=>{let _=ws.close(None).await;break;},
                            Err(_)=>break,_=>{}
                        }}
                        state.feishu_connections.fetch_sub(1,Ordering::SeqCst);return;
                     }
                     let platform=if String::from_utf8_lossy(&peek).contains("/discord/stream"){"discord"}else if String::from_utf8_lossy(&peek).contains("/qq/stream"){"qq"}else{"dingtalk"};
             let mut ws=tokio_tungstenite::accept_async(socket).await.unwrap();let generation=state.generation.load(Ordering::SeqCst);
             if platform!="dingtalk"{
              let hello=if platform=="discord"{json!({"op":10,"d":{"heartbeat_interval":1000}})}else{json!({"op":10,"d":{"heartbeat_interval":1000}})};
              ws.send(tokio_tungstenite::tungstenite::Message::Text(hello.to_string().into())).await.unwrap();
              while let Some(frame)=ws.next().await{
               match frame{
                Ok(tokio_tungstenite::tungstenite::Message::Text(text))=>{
                 let value:Value=serde_json::from_str(text.as_str()).unwrap();
                 if value["op"]==2{
                  let ready=if platform=="discord"{json!({"op":0,"t":"READY","s":1,"d":{"session_id":"fixture","user":{"id":"bot"},"resume_gateway_url":format!("{socket_origin}/discord/stream")}})}else{json!({"op":0,"t":"READY","s":1,"d":{"session_id":"fixture"}})};
                  ws.send(tokio_tungstenite::tungstenite::Message::Text(ready.to_string().into())).await.unwrap();
                  let message=if platform=="discord"{json!({"op":0,"t":"MESSAGE_CREATE","s":2,"d":{"id":format!("event-{generation}"),"channel_id":"42","type":0,"content":format!("channel-lifecycle-input-{generation}"),"author":{"id":"human","bot":false}}})}else{json!({"op":0,"t":"GROUP_AT_MESSAGE_CREATE","s":2,"d":{"id":format!("event-{generation}"),"group_openid":"group","author":{"member_openid":"human"},"content":format!("channel-lifecycle-input-{generation}")}})};
                  ws.send(tokio_tungstenite::tungstenite::Message::Text(message.to_string().into())).await.unwrap();
                 }else if value["op"]==1{ws.send(tokio_tungstenite::tungstenite::Message::Text(json!({"op":11}).to_string().into())).await.unwrap();}
                },
                Ok(tokio_tungstenite::tungstenite::Message::Close(_))=>{let _=ws.close(None).await;break;},
                Ok(_)=>{},Err(_)=>break,
               }
              }return;
             }
                     let event=json!({"type":"EVENT","headers":{"topic":"/v1.0/im/bot/messages/get","messageId":format!("event-{generation}")},"data":json!({"msgId":format!("event-{generation}"),"conversationId":"room","msgtype":"text","text":{"content":format!("channel-lifecycle-input-{generation}")},"sessionWebhook":"https://api.dingtalk.com/lifecycle"}).to_string()});
                     ws.send(tokio_tungstenite::tungstenite::Message::Text(event.to_string().into())).await.unwrap();
                     while let Some(frame)=ws.next().await {match frame{Ok(tokio_tungstenite::tungstenite::Message::Close(_))=>{let _=ws.close(None).await;break;},Ok(_)=>{},Err(_)=>break,}}
                     return;
                    }
                    let mut bytes=vec![];while !bytes.ends_with(b"\r\n\r\n"){let mut b=[0];if socket.read_exact(&mut b).await.is_err(){return;}bytes.push(b[0]);assert!(bytes.len()<65536);}
                              let headers=String::from_utf8(bytes).unwrap();let path=headers.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned();
                              let len=headers.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length:").map(|v|v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                              let mut body=vec![0;len];if socket.read_exact(&mut body).await.is_err(){return;}
                              let body:Value=serde_json::from_slice(&body).unwrap_or(Value::Null);
                              state.requests.lock().unwrap().push((path.clone(),body.clone()));
                              let generation=state.generation.load(Ordering::SeqCst);
                              let (mime,text)=if path=="/callback/ws/endpoint"{("application/json",json!({"code":0,"data":{"URL":format!("{socket_origin}/feishu/stream?service_id=77&device_id=fixture"),"ClientConfig":{"PingInterval":1,"ReconnectCount":2,"ReconnectInterval":0.01,"ReconnectNonce":0}}}).to_string())}
            else if path.contains("/tenant_access_token/"){("application/json",json!({"code":0,"tenant_access_token":"mock-token","expire":7200}).to_string())}
            else if path.starts_with("/open-apis/im/v1/messages"){("application/json",json!({"code":0,"data":{"message_id":"sent-message","reaction_id":"reaction"}}).to_string())}
            else if path=="/gateway/bot"{("application/json",json!({"url":format!("{socket_origin}/discord/stream")}).to_string())}
            else if path=="/gateway"{("application/json",json!({"url":format!("{socket_origin}/qq/stream")}).to_string())}
            else if path=="/qq/token"{("application/json",json!({"access_token":"mock-token","expires_in":"7200"}).to_string())}
            else if path=="/users/@me"{("application/json",json!({"id":"bot"}).to_string())}
            else if path=="/channels/42"{("application/json",json!({"id":"42","type":0}).to_string())}
            else if path.ends_with("/messages")||path.ends_with("/typing"){("application/json",json!({"id":"outbound"}).to_string())}
            else if path=="/dingtalk/open"{("application/json",json!({"endpoint":format!("{socket_origin}/dingtalk/stream"),"ticket":"mock-ticket"}).to_string())}else if path=="/dingtalk/send"{("application/json","{}".to_owned())}else if path.ends_with("/getMe"){("application/json",json!({"ok":true,"result":{"id":1,"username":"fixture"}}).to_string())}
                              else if path.contains("/getUpdates"){
                               let first=state.delivered.lock().unwrap().insert(generation);
                               if !first{tokio::time::sleep(Duration::from_millis(100)).await;}
                               ("application/json",json!({"ok":true,"result":if first {vec![json!({"update_id":generation+1,"message":{"message_id":generation+1,"chat":{"id":42,"type":"private"},"from":{"id":7},"text":format!("channel-lifecycle-input-{generation}")}})]}else{vec![]}}).to_string())
                              }else if path.contains("/sendMessage")||path.contains("/sendChatAction")||path.contains("/deleteWebhook"){("application/json",json!({"ok":true,"result":{}}).to_string())}
                              else if body.get("stream")==Some(&Value::Bool(true)){
                               ("text/event-stream",format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",json!({"id":"fixture","choices":[{"index":0,"delta":{"role":"assistant","content":format!("channel-lifecycle-output-{generation}")},"finish_reason":null}]}),json!({"id":"fixture","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})))
                              }else{("application/json",json!({"choices":[{"message":{"role":"assistant","content":"{\"title\":\"Channel lifecycle\"}"},"finish_reason":"stop"}]}).to_string())};
                              let reply=format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",text.len());let _=socket.write_all(reply.as_bytes()).await;
                             });},
                             Some(_)=tasks.join_next(),if !tasks.is_empty()=>{}
                            }
        }
    });
    Mock {
        origin,
        state,
        task,
    }
}
async fn until(mut condition: impl FnMut() -> bool, label: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{label}"));
}
fn sent(wire: &Wire, generation: usize) -> bool {
    wire.requests.lock().unwrap().iter().any(|(path, body)| {
        path.ends_with("/sendMessage")
            && body
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|s| s.contains(&format!("channel-lifecycle-output-{generation}")))
    })
}
#[tokio::test]
async fn telegram_real_server_a7_reload_and_shutdown() {
    let mock = mock().await;
    let (_scratch, requests) = super::management_tests::setup();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    let config = requests.options.home.join("pilotdeck.yaml");
    let base = std::fs::read_to_string(&config)
        .unwrap()
        .replace("http://127.0.0.1:1", &format!("{}/v1", mock.origin));
    let enabled =
        format!("{base}adapters:\n  telegram:\n    enabled: true\n    token: mock-token\n");
    std::fs::write(&config, &enabled).unwrap();
    let (host, channels) = super::assembly::start_with_channel_endpoints(
        real_options(&requests.options),
        requests.env.clone(),
        ChannelEndpointOverrides {
            telegram_api_root: Some(mock.origin.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    until(
        || sent(&mock.state, 0),
        "Telegram inbound -> actual A7 -> outbound",
    )
    .await;
    assert!(
        mock.state
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(_, body)| body.get("stream") == Some(&Value::Bool(true))
                && body.to_string().contains("channel-lifecycle-input-0"))
    );
    // Test the same bundle reconciliation used by the production ConfigStore
    // subscription; process.mjs separately pins the public reload_config route.
    channels
        .reload(
            &pilotdeck_protocol::parse_json(r#"{"adapters":{"telegram":{"enabled":false}}}"#)
                .unwrap(),
        )
        .await
        .unwrap();
    let count = mock
        .state
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _)| p.contains("/getUpdates"))
        .count();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        count,
        mock.state
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.contains("/getUpdates"))
            .count(),
        "disabled Telegram must stop polling"
    );
    mock.state.generation.store(1, Ordering::SeqCst);
    channels
        .reload(
            &pilotdeck_protocol::parse_json(
                r#"{"adapters":{"telegram":{"enabled":true,"token":"mock-token"}}}"#,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    until(
        || sent(&mock.state, 1),
        "reloaded Telegram -> actual A7 -> outbound",
    )
    .await;
    assert_eq!(
        mock.state
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.ends_with("/getMe"))
            .count(),
        2,
        "fresh adapter on reload"
    );
    host.shutdown(requests.options.drain_budget).await.unwrap();
    let count = mock.state.requests.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        count,
        mock.state.requests.lock().unwrap().len(),
        "shutdown transport quiescence"
    );
}

fn dingtalk_sent(wire: &Wire, generation: usize) -> bool {
    wire.requests.lock().unwrap().iter().any(|(path, body)| {
        path == "/dingtalk/send"
            && body
                .to_string()
                .contains(&format!("channel-lifecycle-output-{generation}"))
    })
}
#[tokio::test]
async fn dingtalk_real_server_a7_reload_and_shutdown() {
    let mock = mock().await;
    let (_scratch, requests) = super::management_tests::setup();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    let config = requests.options.home.join("pilotdeck.yaml");
    let base = std::fs::read_to_string(&config)
        .unwrap()
        .replace("http://127.0.0.1:1", &format!("{}/v1", mock.origin));
    std::fs::write(&config,format!("{base}adapters:\n  dingtalk:\n    enabled: true\n    extra:\n      clientId: mock-id\n      clientSecret: mock-secret\n")).unwrap();
    let route = format!("{}/dingtalk/send", mock.origin);
    let (host, channels) = super::assembly::start_with_channel_endpoints(
        real_options(&requests.options),
        requests.env.clone(),
        ChannelEndpointOverrides {
            dingtalk_stream_endpoint: Some(format!("{}/dingtalk/open", mock.origin)),
            dingtalk_webhook_route: Some(Arc::new(move |_| route.clone())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    until(
        || dingtalk_sent(&mock.state, 0),
        "DingTalk real A7 outbound",
    )
    .await;
    assert!(
        mock.state
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(_, body)| body.get("stream") == Some(&Value::Bool(true))
                && body.to_string().contains("channel-lifecycle-input-0"))
    );
    channels
        .reload(
            &pilotdeck_protocol::parse_json(r#"{"adapters":{"dingtalk":{"enabled":false}}}"#)
                .unwrap(),
        )
        .await
        .unwrap();
    mock.state.generation.store(1, Ordering::SeqCst);
    channels.reload(&pilotdeck_protocol::parse_json(r#"{"adapters":{"dingtalk":{"enabled":true,"extra":{"clientId":"mock-id","clientSecret":"mock-secret"}}}}"#).unwrap()).await.unwrap();
    until(
        || dingtalk_sent(&mock.state, 1),
        "reloaded DingTalk real A7 outbound",
    )
    .await;
    assert_eq!(
        mock.state
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p == "/dingtalk/open")
            .count(),
        2
    );
    host.shutdown(requests.options.drain_budget).await.unwrap();
    let count = mock.state.requests.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        count,
        mock.state.requests.lock().unwrap().len(),
        "DingTalk shutdown transport quiescence"
    );
}

async fn hardcoded_platform(platform: &str) {
    let mock = mock().await;
    let (_scratch, requests) = super::management_tests::setup();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    let path = requests.options.home.join("pilotdeck.yaml");
    let settings = if platform == "discord" {
        json!({"enabled":true,"token":"mock-token"})
    } else {
        json!({"enabled":true,"appId":"mock-app","clientSecret":"mock-secret","allowGroups":["*"]})
    };
    // JSON is valid YAML and avoids changing the production config parser.
    let mut full = json!({"schemaVersion":1,"agent":{"model":"test/test"},"model":{"providers":{"test":{"protocol":"openai","url":format!("{}/v1",mock.origin),"apiKey":"test-only","models":{"test":{}}}}}});
    full["adapters"] = json!({(platform):settings});
    std::fs::write(&path, full.to_string()).unwrap();
    let endpoints = ChannelEndpointOverrides {
        discord_api_root: Some(mock.origin.clone()),
        qq_api_base: Some(mock.origin.clone()),
        qq_token_url: Some(format!("{}/qq/token", mock.origin)),
        ..Default::default()
    };
    let (host, channels) = super::assembly::start_with_channel_endpoints(
        real_options(&requests.options),
        requests.env.clone(),
        endpoints,
    )
    .await
    .unwrap();
    let delivered = |generation| {
        mock.state.requests.lock().unwrap().iter().any(|(p, b)| {
            p.ends_with("/messages")
                && b.to_string()
                    .contains(&format!("channel-lifecycle-output-{generation}"))
        })
    };
    until(|| delivered(0), "platform -> real A7 -> REST reply").await;
    assert!(
        mock.state
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(_, b)| b.get("stream") == Some(&Value::Bool(true))
                && b.to_string().contains("channel-lifecycle-input-0"))
    );
    channels
        .reload(
            &pilotdeck_protocol::parse_json(
                &json!({"adapters":{(platform):{"enabled":false}}}).to_string(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    mock.state.generation.store(1, Ordering::SeqCst);
    channels
        .reload(&pilotdeck_protocol::parse_json(&full.to_string()).unwrap())
        .await
        .unwrap();
    until(
        || delivered(1),
        "reloaded platform -> real A7 -> REST reply",
    )
    .await;
    host.shutdown(requests.options.drain_budget).await.unwrap();
    let count = mock.state.requests.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        count,
        mock.state.requests.lock().unwrap().len(),
        "shutdown transport quiescence"
    );
}
#[tokio::test]
async fn discord_real_server_a7_reload_and_shutdown() {
    hardcoded_platform("discord").await;
}
#[tokio::test]
async fn qq_real_server_a7_reload_and_shutdown() {
    hardcoded_platform("qq").await;
}

#[derive(Default)]
struct ControlFrames(Mutex<Vec<Value>>);
impl crate::runtime::connection::GatewayTextConnection for ControlFrames {
    fn send_text(&self, text: String) {
        self.0
            .lock()
            .unwrap()
            .push(serde_json::from_str(&text).unwrap());
    }
    fn close(&self, _code: u16, _reason: &str) {}
}
#[tokio::test]
async fn feishu_real_server_protobuf_a7_public_reload_and_shutdown() {
    let mock = mock().await;
    let (_scratch, requests) = super::management_tests::setup();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    let config_path = requests.options.home.join("pilotdeck.yaml");
    let mut full = json!({"schemaVersion":1,"agent":{"model":"test/test"},"model":{"providers":{"test":{"protocol":"openai","url":format!("{}/v1",mock.origin),"apiKey":"test-only","models":{"test":{}}}}},"adapters":{"feishu":{"enabled":true,"appId":"cli_0123456789abcdef","appSecret":"mock-secret","connectionMode":"stream"}}});
    std::fs::write(&config_path, full.to_string()).unwrap();
    let (host, _channels) = super::assembly::start_with_channel_endpoints(
        real_options(&requests.options),
        requests.env.clone(),
        ChannelEndpointOverrides {
            feishu_api_base: Some(mock.origin.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let outbound = |generation| {
        mock.state.requests.lock().unwrap().iter().any(|(p, b)| {
            p.starts_with("/open-apis/im/v1/messages?")
                && b.to_string()
                    .contains(&format!("channel-lifecycle-output-{generation}"))
        })
    };
    until(
        || outbound(0),
        "Feishu SDK protobuf input -> actual Server A7 -> real Lark REST output",
    )
    .await;
    assert!(
        mock.state
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(_, b)| b.get("stream") == Some(&Value::Bool(true))
                && b.to_string().contains("SDK fragmented input"))
    );
    let frames = Arc::new(ControlFrames::default());
    let connection = host.connect(frames.clone(), "token".into(), "fixture".into());
    connection.receive(json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"feishu-lifecycle"}).to_string()).await.unwrap();
    full["adapters"]["feishu"]["enabled"] = false.into();
    std::fs::write(&config_path, full.to_string()).unwrap();
    connection
        .receive(
            json!({"type":"request","id":"disable-feishu","method":"reload_config","params":{}})
                .to_string(),
        )
        .await
        .unwrap();
    until(
        || {
            frames
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|v| v["id"] == "disable-feishu" && v["ok"] == true)
        },
        "public reload response",
    )
    .await;
    until(
        || mock.state.feishu_connections.load(Ordering::SeqCst) == 0,
        "queued disable closes original Feishu socket",
    )
    .await;
    let count = mock.state.requests.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after_disable = mock.state.requests.lock().unwrap().clone();
    eprintln!(
        "Feishu disable boundary count={count}; following requests={:#?}",
        &after_disable[count..]
    );
    assert_eq!(count, after_disable.len(), "disabled adapter is quiet");
    mock.state.generation.store(1, Ordering::SeqCst);
    full["adapters"]["feishu"]["enabled"] = true.into();
    std::fs::write(&config_path, full.to_string()).unwrap();
    connection
        .receive(
            json!({"type":"request","id":"enable-feishu","method":"reload_config","params":{}})
                .to_string(),
        )
        .await
        .unwrap();
    until(|| outbound(1), "reloaded Feishu actual A7 output").await;
    assert_eq!(
        mock.state
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p == "/callback/ws/endpoint")
            .count(),
        2
    );
    assert!(
        mock.state.feishu_acks.load(Ordering::SeqCst) >= 2,
        "both fragmented deliveries acknowledged"
    );
    connection.closed().await.unwrap();
    host.shutdown(requests.options.drain_budget).await.unwrap();
    until(
        || mock.state.feishu_connections.load(Ordering::SeqCst) == 0,
        "Gateway shutdown closes Feishu socket",
    )
    .await;
    let count = mock.state.requests.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        count,
        mock.state.requests.lock().unwrap().len(),
        "no transport work after shutdown"
    );
    let chat = crate::session_chat_dir(
        &requests.options.home,
        &requests.options.project,
        requests.options.project.to_string_lossy().as_ref(),
    );
    let transcripts = std::fs::read_dir(chat)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|p| std::fs::read_to_string(p.path()).ok())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        transcripts.contains("channel-lifecycle-output-0")
            && transcripts.contains("channel-lifecycle-output-1"),
        "both actual A7 turns retained in history"
    );
    let terminal_success = transcripts
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|row| row["type"] == "turn_result" && row["result"]["type"] == "success")
        .count();
    assert!(
        terminal_success >= 2,
        "both actual A7 turns completed durably, got {terminal_success}"
    );
}

use super::*;
#[test]
fn group_mapper_persists_new_sessions_and_qq_split_differs_from_im() {
    let mut mapper =
        QQSessionMapper::new(QQSessionMapperState::default(), Arc::new(|| "fixed".into()));
    let mapped = mapper.resolve(&"g".into(), &"u".into(), &" /new hello ".into());
    assert_eq!(
        mapped.session_key,
        JsString::from("qq:group=g:user=u:s_fixed")
    );
    let state = mapper.snapshot();
    assert_eq!(
        QQSessionMapperState::from_value(&state.to_value())
            .active_by_chat_key
            .len(),
        1
    );
    assert_eq!(
        render::split_text(&"ab\ncd efgh".into(), 8),
        vec![JsString::from("ab\ncd ef"), JsString::from("gh")]
    );
}
#[test]
fn qq_tool_failure_has_chinese_contract() {
    assert_eq!(
        render::render_qq_event(&object([
            ("type", "tool_call_finished".into()),
            ("ok", JsValue::Bool(false)),
            ("toolName", "Read".into())
        ])),
        Some(JsString::from("\n⚠️ Read 执行失败\n"))
    );
}

use futures_util::{SinkExt, StreamExt};
use std::{collections::VecDeque, future::Future, pin::Pin};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;
struct Stream(VecDeque<JsValue>);
impl TurnEventStream for Stream {
    fn next(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>> {
        Box::pin(async move { Ok(self.0.pop_front()) })
    }
}
#[derive(Default)]
struct Gateway(Mutex<Vec<TurnRequest>>);
impl TurnGateway for Gateway {
    fn submit(
        &self,
        input: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        self.0.lock().unwrap().push(input);
        Box::pin(async {
            Ok(Box::new(Stream(VecDeque::from([super::object([
                ("type", "assistant_text_delta".into()),
                ("text", "QQ reply".into()),
            ])]))) as Box<dyn TurnEventStream>)
        })
    }
}
#[tokio::test]
async fn oauth_native_ws_group_and_c2c_use_one_gateway_and_passive_receipts() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let root = format!("http://{addr}");
    let sends = Arc::new(Mutex::new(Vec::new()));
    let record = sends.clone();
    let tokens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let token_count = tokens.clone();
    let closed = Arc::new(tokio::sync::Notify::new());
    let close_signal = closed.clone();
    let server = tokio::spawn(async move {
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
             accepted=listener.accept()=>{let(mut socket,_)=accepted.unwrap();let sends=record.clone();let tokens=token_count.clone();let closed=close_signal.clone();tasks.spawn(async move{
              let mut peek=vec![0;16384];let headers=loop{let n=socket.peek(&mut peek).await.unwrap();if n==0{return;}if let Some(end)=peek[..n].windows(4).position(|v|v==b"\r\n\r\n"){break String::from_utf8(peek[..end+4].to_vec()).unwrap();}tokio::task::yield_now().await;};let path=headers.lines().next().unwrap().split_whitespace().nth(1).unwrap();
              if path=="/socket"{
               let mut ws=tokio_tungstenite::accept_async(socket).await.unwrap();ws.send(Message::Text(r#"{"op":10,"d":{"heartbeat_interval":1000}}"#.into())).await.unwrap();let frame=ws.next().await.unwrap().unwrap();let Message::Text(frame)=frame else{panic!("QQ identify")};let identify=pilotdeck_protocol::parse_json(frame.as_str()).unwrap();assert_eq!(identify.get("op"),Some(&JsValue::Number(2.0)));assert_eq!(identify.get("d").and_then(|d|d.get("token")),Some(&JsValue::from("QQBot access")));assert_eq!(identify.get("d").and_then(|d|d.get("intents")),Some(&JsValue::Number(33_554_432.0)));
               for raw in [r#"{"op":0,"s":1,"t":"READY","d":{"session_id":"session"}}"#,r#"{"op":0,"s":2,"t":"GROUP_AT_MESSAGE_CREATE","d":{"id":"g1","group_openid":"blocked","author":{"member_openid":"u"},"content":"hi"}}"#,r#"{"op":0,"s":3,"t":"GROUP_AT_MESSAGE_CREATE","d":{"id":"g2","group_openid":"g","author":{"member_openid":"u"},"content":"/ask hello"}}"#,r#"{"op":0,"s":4,"t":"C2C_MESSAGE_CREATE","d":{"id":"c1","author":{"user_openid":"u2"},"content":"/new"}}"#]{ws.send(Message::Text(raw.into())).await.unwrap();}
               while let Some(frame)=ws.next().await{match frame{Ok(Message::Close(_))=>{let _=ws.close(None).await;closed.notify_one();return;},Ok(Message::Text(_))=>(),Err(_)=>return,_=>()}}
              }else{
               let mut header=vec![0;headers.len()];socket.read_exact(&mut header).await.unwrap();let n:usize=headers.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length:").map(|s|s.trim().parse().unwrap())).unwrap_or(0);let mut body=vec![0;n];socket.read_exact(&mut body).await.unwrap();
               let response=if path=="/token"{tokens.fetch_add(1,Ordering::SeqCst);let body=pilotdeck_protocol::parse_json(&String::from_utf8(body).unwrap()).unwrap();assert_eq!(body.get("appId"),Some(&JsValue::from("app")));assert_eq!(body.get("clientSecret"),Some(&JsValue::from("fixture")));r#"{"access_token":"access","expires_in":"3600"}"#.to_owned()}else{assert!(headers.to_ascii_lowercase().contains("authorization: qqbot access"));if path=="/gateway"{format!(r#"{{"url":"ws://{addr}/socket"}}"#)}else{sends.lock().unwrap().push((path.to_owned(),pilotdeck_protocol::parse_json(&String::from_utf8(body).unwrap()).unwrap()));"{}".into()}};
               let _=socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).as_bytes()).await;
              }
             });},Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
            }
        }
    });
    let gateway = Arc::new(Gateway::default());
    let channel = QQChannel::new(QQOptions {
        app_id: Some("app".into()),
        client_secret: Some("fixture".into()),
        api_base: Some(root.clone()),
        token_url: Some(format!("{root}/token")),
        allow_groups: Some(vec!["g".into()]),
        ..Default::default()
    });
    let handle = channel
        .start(ChannelStartDeps {
            turn_gateway: Some(gateway.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if sends.lock().unwrap().len() == 2 && channel.inner.active.lock().unwrap().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("QQ group/C2C replies");
    handle.stop("fixture").await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), closed.notified())
        .await
        .expect("QQ actual socket close");
    assert_eq!(tokens.load(Ordering::SeqCst), 1);
    let requests = gateway.0.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().any(
        |r| r.session_key == JsString::from("qq:group=g:user=u:general")
            && r.message == JsString::from("hello")
    ));
    assert!(
        requests
            .iter()
            .any(|r| r.session_key == JsString::from("qq:c2c:u2:general")
                && r.message == JsString::from("/new"))
    );
    assert!(
        requests
            .iter()
            .all(|r| r.channel_key == ChannelKey::new("qq"))
    );
    let golden =
        pilotdeck_protocol::parse_json(include_str!("fixtures/oracle-diagnostic.json")).unwrap();
    assert_eq!(
        golden.get("baseline"),
        Some(&JsValue::from("cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"))
    );
    let Some(JsValue::Array(expected)) = golden.get("requests") else {
        panic!("QQ fixed TS requests")
    };
    assert_eq!(expected.len(), requests.len());
    for request in requests.iter() {
        assert!(expected.iter().any(|row| row.get("sessionKey")
            == Some(&JsValue::String(request.session_key.clone()))
            && row.get("message") == Some(&JsValue::String(request.message.clone()))
            && row.get("channelKey")
                == Some(&JsValue::String(request.channel_key.as_js().clone()))));
    }

    let sends = sends.lock().unwrap();
    for (path, body) in sends.iter() {
        assert!(path == "/v2/groups/g/messages" || path == "/v2/users/u2/messages");
        assert_eq!(body.get("msg_seq"), Some(&JsValue::Number(1.0)));
        assert_eq!(body.get("msg_type"), Some(&JsValue::Number(0.0)));
        assert!(
            matches!(body.get("msg_id"),Some(JsValue::String(id)) if id.equals_str("g2")||id.equals_str("c1"))
        );
    }
    server.abort();
}

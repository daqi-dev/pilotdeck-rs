use super::*;
#[test]
fn endpoint_preserves_fixed_baseline_path_contract() {
    assert_eq!(
        ws_endpoint("https://example.invalid/api/", &"a b".into()).unwrap(),
        (
            "wss://example.invalid/api/api/v4/websocket?token=a%20b".into(),
            "https://example.invalid".into()
        )
    );
}
#[test]
fn thread_session_mapper_and_chunking_are_utf16() {
    let mut mapper = MattermostSessionMapper::default();
    assert_eq!(
        mapper
            .resolve(&"channel:thread".into(), &" hi ".into())
            .session_key,
        JsString::from("mattermost:chat=channel:thread:general")
    );
    assert_eq!(
        render::chunk_text(&"aaaa\nbbbb cccc".into(), 8),
        vec![
            JsString::from("aaaa"),
            JsString::from("bbbb"),
            JsString::from(" cccc")
        ]
    );
}

use std::{collections::VecDeque, future::Future, pin::Pin};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
            Ok(Box::new(Stream(VecDeque::from([parse_json(
                r#"{"type":"assistant_text_delta","text":"Mattermost reply"}"#,
            )
            .unwrap()]))) as Box<dyn TurnEventStream>)
        })
    }
}
fn posted(post: &str) -> JsValue {
    object([
        ("event", "posted".into()),
        ("data", object([("post", JsValue::from(post))])),
    ])
}
#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tungstenite handshake callback requires its concrete ErrorResponse"
)]
async fn real_ws_filter_thread_turn_reply_and_stop_close_handshake() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let root = format!("http://{addr}");
    let sends = Arc::new(Mutex::new(Vec::new()));
    let record = sends.clone();
    let reply = Arc::new(tokio::sync::Notify::new());
    let replied = reply.clone();
    let closed = Arc::new(tokio::sync::Notify::new());
    let close_signal = closed.clone();
    let server = tokio::spawn(async move {
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
            accepted=listener.accept()=>{let(mut socket,_)=accepted.unwrap();let sends=record.clone();let reply=replied.clone();let closed=close_signal.clone();tasks.spawn(async move{
             let mut peek=vec![0;16384];let headers=loop{let count=socket.peek(&mut peek).await.unwrap();if count==0{return;}
             if let Some(end)=peek[..count].windows(4).position(|v|v==b"\r\n\r\n"){break String::from_utf8(peek[..end+4].to_vec()).unwrap();}tokio::task::yield_now().await;};
             let path=headers.lines().next().unwrap().split_whitespace().nth(1).unwrap();
             if path.starts_with("/api/v4/websocket"){
              let mut ws=tokio_tungstenite::accept_hdr_async(socket,move|request:&tokio_tungstenite::tungstenite::handshake::server::Request,response:tokio_tungstenite::tungstenite::handshake::server::Response|{
               assert_eq!(request.uri().to_string(),"/api/v4/websocket?token=fixture");assert_eq!(request.headers().get("Origin").unwrap(),format!("http://{addr}").as_str());Ok(response)
              }).await.unwrap();
              for post in [r#"{"user_id":"bot","channel_id":"channel","message":"self"}"#,r#"{"user_id":"human","channel_id":"channel","message":"hook","props":{"from_webhook":"true"}}"#,r#"{"user_id":"human","channel_id":"channel","root_id":"thread","message":" hello\r\nworld ","props":{"from_bot":true}}"#]{let text=stringify_json(&posted(post)).unwrap().unwrap();ws.send(Message::Text(text.into())).await.unwrap();}
              while let Some(frame)=ws.next().await{match frame{Ok(Message::Close(_))=>{let _=ws.close(None).await;closed.notify_one();return;},Err(_)=>return,_=>()}}
             }else{
              let mut head=vec![0;headers.len()];socket.read_exact(&mut head).await.unwrap();assert!(headers.to_ascii_lowercase().contains("authorization: bearer fixture"));
              let length:usize=headers.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length:").map(|s|s.trim().parse().unwrap())).unwrap_or(0);let mut body=vec![0;length];socket.read_exact(&mut body).await.unwrap();
              let response=if path=="/users/me"{r#"{"id":"bot"}"#}else{assert_eq!(path,"/posts");sends.lock().unwrap().push(parse_json(&String::from_utf8(body).unwrap()).unwrap());reply.notify_one();"{}"};
              socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).as_bytes()).await.unwrap();
             }
            });},Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
            }
        }
    });
    let gateway = Arc::new(Gateway::default());
    let channel = MattermostChannel::new(MattermostOptions {
        token: Some("fixture".into()),
        server_url: Some(root),
        ..Default::default()
    });
    let handle = channel
        .start(ChannelStartDeps {
            turn_gateway: Some(gateway.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), reply.notified())
        .await
        .expect("Mattermost REST reply");
    handle.stop("fixture").await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), closed.notified())
        .await
        .expect("WS actual close");
    assert!(!channel.inner.running.load(Ordering::Acquire));
    assert!(channel.inner.active.lock().unwrap().is_empty());
    let requests = gateway.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].session_key,
        JsString::from("mattermost:chat=channel:thread:general")
    );
    assert_eq!(requests[0].message, JsString::from("hello\nworld"));
    assert_eq!(requests[0].channel_key, ChannelKey::new("mattermost"));
    let golden = parse_json(include_str!("fixtures/oracle-diagnostic.json")).unwrap();
    assert_eq!(
        golden.get("baseline"),
        Some(&JsValue::from("cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"))
    );
    let Some(JsValue::Array(expected)) = golden.get("requests") else {
        panic!("Mattermost fixed TS request")
    };
    assert_eq!(expected.len(), 1);
    assert_eq!(
        expected[0].get("sessionKey"),
        Some(&JsValue::String(requests[0].session_key.clone()))
    );
    assert_eq!(
        expected[0].get("message"),
        Some(&JsValue::String(requests[0].message.clone()))
    );
    assert_eq!(
        expected[0].get("channelKey"),
        Some(&JsValue::String(requests[0].channel_key.as_js().clone()))
    );

    let sends = sends.lock().unwrap();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].get("root_id"), Some(&JsValue::from("thread")));
    assert_eq!(sends[0].get("channel_id"), Some(&JsValue::from("channel")));
    assert_eq!(
        sends[0].get("message"),
        Some(&JsValue::from("Mattermost reply"))
    );
    server.abort();
}

use super::*;
#[test]
fn mapper_and_chunks_preserve_fixed_channel_profile() {
    let mut mapper = DiscordSessionMapper::default();
    assert_eq!(
        mapper.resolve(&"chat".into(), &" hi ".into()).session_key,
        JsString::from("discord:chat=chat:general")
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
                r#"{"type":"assistant_text_delta","text":"Discord reply"}"#,
            )
            .unwrap()]))) as Box<dyn TurnEventStream>)
        })
    }
}
#[tokio::test]
async fn real_gateway_identify_message_filters_rest_reply_and_close() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let root = format!("http://{addr}");
    let posted = Arc::new(Mutex::new(Vec::new()));
    let record = posted.clone();
    let reply = Arc::new(tokio::sync::Notify::new());
    let replied = reply.clone();
    let closed = Arc::new(tokio::sync::Notify::new());
    let close_signal = closed.clone();
    let server = tokio::spawn(async move {
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
            accepted=listener.accept()=>{let(mut socket,_)=accepted.unwrap();let posted=record.clone();let reply=replied.clone();let closed=close_signal.clone();tasks.spawn(async move{
             let mut peek=vec![0;16384];let headers=loop{let n=socket.peek(&mut peek).await.unwrap();if n==0{return;}if let Some(end)=peek[..n].windows(4).position(|v|v==b"\r\n\r\n"){break String::from_utf8(peek[..end+4].to_vec()).unwrap();}tokio::task::yield_now().await;};let path=headers.lines().next().unwrap().split_whitespace().nth(1).unwrap();
             if path.starts_with("/gateway?"){
              let mut ws=tokio_tungstenite::accept_async(socket).await.unwrap();ws.send(Message::Text(r#"{"op":10,"d":{"heartbeat_interval":1000}}"#.into())).await.unwrap();
              loop{let frame=ws.next().await.unwrap().unwrap();if let Message::Text(text)=frame{let value=parse_json(text.as_str()).unwrap();match value.get("op"){Some(JsValue::Number(op)) if *op==2.0=>{assert_eq!(value.get("d").and_then(|d|d.get("token")),Some(&JsValue::from("fixture")));assert_eq!(value.get("d").and_then(|d|d.get("intents")),Some(&JsValue::Number(37377.0)));break;},Some(JsValue::Number(op)) if *op==1.0=>{ws.send(Message::Text(r#"{"op":11,"d":null}"#.into())).await.unwrap();},_=>()}}}
              let ready=packet(0.0,object([("session_id","session".into()),("resume_gateway_url",JsValue::from(format!("ws://{addr}/gateway").as_str())),("user",object([("id","bot".into())]))]));let mut ready=ready.as_object().unwrap().clone();ready.insert("s".into(),JsValue::Number(1.0));ready.insert("t".into(),"READY".into());ws.send(Message::Text(stringify_json(&JsValue::Object(ready)).unwrap().unwrap().into())).await.unwrap();
              for (seq,raw)in [(2.0,r#"{"author":{"id":"bot","bot":false},"channel_id":"c","content":"self"}"#),(3.0,r#"{"author":{"id":"human","bot":true},"channel_id":"c","content":"bot"}"#),(4.0,r#"{"author":{"id":"human","bot":false},"channel_id":"c","content":"system","type":1}"#),(5.0,r#"{"author":{"id":"human","bot":false},"channel_id":"c","content":" hello ","type":0}"#)]{
               let event=object([("op",JsValue::Number(0.0)),("s",JsValue::Number(seq)),("t","MESSAGE_CREATE".into()),("d",parse_json(raw).unwrap())]);ws.send(Message::Text(stringify_json(&event).unwrap().unwrap().into())).await.unwrap();
              }
              while let Some(frame)=ws.next().await{match frame{Ok(Message::Close(_))=>{let _=ws.close(None).await;closed.notify_one();return;},Ok(Message::Text(text))=>{let value=parse_json(text.as_str()).unwrap();if value.get("op")==Some(&JsValue::Number(1.0)){ws.send(Message::Text(r#"{"op":11,"d":null}"#.into())).await.unwrap();}},Err(_)=>return,_=>()}}
             }else{
              let mut header=vec![0;headers.len()];socket.read_exact(&mut header).await.unwrap();assert!(headers.to_ascii_lowercase().contains("authorization: bot fixture"));let n:usize=headers.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length:").map(|s|s.trim().parse().unwrap())).unwrap_or(0);let mut body=vec![0;n];socket.read_exact(&mut body).await.unwrap();
              let response=if path=="/gateway/bot"{format!(r#"{{"url":"ws://{addr}/gateway"}}"#)}else if path=="/channels/c"{r#"{"id":"c","type":0}"#.into()}else if path=="/channels/c/typing"{"{}".into()}else{assert_eq!(path,"/channels/c/messages");posted.lock().unwrap().push(parse_json(&String::from_utf8(body).unwrap()).unwrap());reply.notify_one();"{}".into()};
              let _=socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).as_bytes()).await;
             }
            });},Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
            }
        }
    });
    let gateway = Arc::new(Gateway::default());
    let channel = DiscordChannel::new(DiscordOptions {
        token: Some("fixture".into()),
        api_root: Some(root),
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
        .expect("Discord reply");
    handle.stop("fixture").await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), closed.notified())
        .await
        .expect("actual Discord socket close");
    assert!(channel.inner.active.lock().unwrap().is_empty());
    assert!(!channel.inner.running.load(Ordering::Acquire));
    let requests = gateway.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].channel_key, ChannelKey::new("discord"));
    let golden = parse_json(include_str!("fixtures/oracle-diagnostic.json")).unwrap();
    assert_eq!(
        golden.get("baseline"),
        Some(&JsValue::from("cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"))
    );
    let Some(JsValue::Array(expected)) = golden.get("requests") else {
        panic!("Discord fixed TS request")
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

    assert_eq!(requests[0].message, JsString::from("hello"));
    assert_eq!(
        requests[0].session_key,
        JsString::from("discord:chat=c:general")
    );
    let posts = posted.lock().unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(
        posts[0].get("content"),
        Some(&JsValue::from("Discord reply"))
    );
    server.abort();
}

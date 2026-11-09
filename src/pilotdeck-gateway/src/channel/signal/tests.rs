use super::*;
#[test]
fn sse_line_contract_ignores_control_lines() {
    assert!(parse_line(b":ok\r\n").is_none());
    assert!(parse_line(b"[DONE]\n").is_none());
    assert!(parse_line(b"event: message\n").is_none());
    assert_eq!(
        parse_line(b"data: {\"x\":1}\r\n").unwrap().get("x"),
        Some(&JsValue::Number(1.0))
    );
}
#[test]
fn mapper_and_recipient_fallback_preserve_group_identity() {
    let mut mapper = SignalSessionMapper::default();
    assert_eq!(
        mapper
            .resolve(&"group:abc".into(), &" hello ".into())
            .session_key,
        JsString::from("signal:chat=group:abc:general")
    );
    assert_eq!(
        strip_chat_prefix(&"group:abc".into()),
        JsString::from("abc")
    );
    assert_eq!(encode_path(&"+123".into()), "%2B123");
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
                r#"{"type":"assistant_text_delta","text":"Signal reply"}"#,
            )
            .unwrap()]))) as Box<dyn TurnEventStream>)
        })
    }
}
#[tokio::test]
async fn real_fragmented_sse_enters_shared_turn_and_stop_closes_receive_socket() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    let sent = Arc::new(Mutex::new(Vec::new()));
    let record = sent.clone();
    let replied = Arc::new(tokio::sync::Notify::new());
    let reply_signal = replied.clone();
    let closed = Arc::new(tokio::sync::Notify::new());
    let close_signal = closed.clone();
    let server = tokio::spawn(async move {
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
             accepted=listener.accept()=>{let(mut socket,_)=accepted.unwrap();let sent=record.clone();let replied=reply_signal.clone();let closed=close_signal.clone();tasks.spawn(async move{
              let mut bytes=Vec::new();while !bytes.ends_with(b"\r\n\r\n"){let mut byte=[0];if socket.read_exact(&mut byte).await.is_err(){return;}bytes.push(byte[0]);assert!(bytes.len()<16384);}
              let headers=String::from_utf8(bytes).unwrap();let path=headers.lines().next().unwrap().split_whitespace().nth(1).unwrap();
              if path.starts_with("/v1/receive/"){
               assert_eq!(path,"/v1/receive/%2B123");assert!(headers.contains("text/event-stream"));
               socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
               let event="data: {\"envelope\":{\"source\":\"+human\",\"dataMessage\":{\"message\":\"你好\",\"groupInfo\":{\"groupId\":\"g\"}}}}\r\n\n".as_bytes();let split=event.iter().position(|b|*b==0xe4).unwrap()+1;
               for chunk in [&event[..split],&event[split..]]{socket.write_all(format!("{:x}\r\n",chunk.len()).as_bytes()).await.unwrap();socket.write_all(chunk).await.unwrap();socket.write_all(b"\r\n").await.unwrap();}
               let mut byte=[0];let read=socket.read(&mut byte).await;assert!(matches!(read,Ok(0)|Err(_)));closed.notify_one();
              }else{
               assert_eq!(path,"/v2/send");let len:usize=headers.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length:").map(|s|s.trim().parse().unwrap())).unwrap();let mut body=vec![0;len];socket.read_exact(&mut body).await.unwrap();sent.lock().unwrap().push(parse_json(&String::from_utf8(body).unwrap()).unwrap());socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await.unwrap();replied.notify_one();
              }
             });},Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
            }
        }
    });
    let gateway = Arc::new(Gateway::default());
    let channel = SignalChannel::new(SignalOptions {
        rest_url: Some(root),
        account: Some("+123".into()),
        ..Default::default()
    });
    let handle = channel
        .start(ChannelStartDeps {
            turn_gateway: Some(gateway.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), replied.notified())
        .await
        .expect("Signal reply received");
    handle.stop("fixture").await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), closed.notified())
        .await
        .expect("SSE connection actually closed");
    assert!(!channel.inner.running.load(Ordering::Acquire));
    assert!(channel.inner.active.lock().unwrap().is_empty());
    let requests = gateway.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].session_key,
        JsString::from("signal:chat=group:g:general")
    );
    assert_eq!(requests[0].message, JsString::from("你好"));
    assert_eq!(requests[0].channel_key, ChannelKey::new("signal"));
    let golden = parse_json(include_str!("fixtures/oracle-diagnostic.json")).unwrap();
    assert_eq!(
        golden.get("baseline"),
        Some(&JsValue::from("cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"))
    );
    let Some(JsValue::Array(expected)) = golden.get("requests") else {
        panic!("Signal fixed TS request")
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

    let sends = sent.lock().unwrap();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].get("number"), Some(&JsValue::from("+123")));
    assert_eq!(
        sends[0].get("message"),
        Some(&JsValue::from("Signal reply"))
    );
    assert_eq!(
        sends[0].get("recipients"),
        Some(&JsValue::Array(vec![JsValue::from("+human")]))
    );
    server.abort();
}

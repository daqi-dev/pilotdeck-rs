//! Native comparison against actual Lark SDK pbbp2/WS diagnostic.
use super::{stream::Frame, *};
use crate::channel::protocol::{TurnEventStream, TurnRequest};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn sdk_gold() -> serde_json::Value {
    serde_json::from_str(include_str!("sdk-oracle.json")).unwrap()
}
#[test]
fn protobuf_codec_roundtrips_actual_sdk_out_of_order_frames() {
    let gold = sdk_gold();
    assert_eq!(gold["fixed"], "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85");
    let mut count = 0;
    for row in gold["rows"].as_array().unwrap() {
        if let Some(raw) = row["inboundBinary"].as_str() {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(raw)
                .unwrap();
            let frame = Frame::decode(&bytes).unwrap();
            assert_eq!(
                (frame.seq, frame.log, frame.service, frame.method),
                (5, 9, 77, 1)
            );
            assert_eq!(frame.encode(), bytes);
            count += 1;
        }
    }
    assert_eq!(count, 2);
    assert_eq!(
        gold["streamClosedAfterFixedAdapterStop"], false,
        "intentional owned-close difference must remain disclosed"
    );
}
struct Events(bool);
impl TurnEventStream for Events {
    fn next(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>,
    > {
        Box::pin(async move {
            if self.0 {
                Ok(None)
            } else {
                self.0 = true;
                Ok(Some(
                    pilotdeck_protocol::parse_json(
                        r#"{"type":"assistant_text_delta","text":"Feishu SDK reply"}"#,
                    )
                    .unwrap(),
                ))
            }
        })
    }
}
struct Gateway(tokio::sync::mpsc::UnboundedSender<TurnRequest>);
impl TurnGateway for Gateway {
    fn submit_with_options(
        &self,
        input: TurnRequest,
        options: crate::channel::protocol::ChannelTurnOptions,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        assert_eq!(options.allow_plan_mode_tools, Some(false));
        assert!(options.timeout_ms.is_some());
        assert!(options.attachments.is_empty());
        self.submit(input)
    }
    fn submit(
        &self,
        input: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        self.0.send(input).unwrap();
        Box::pin(async { Ok(Box::new(Events(false)) as Box<dyn TurnEventStream>) })
    }
}
#[tokio::test]
async fn real_protobuf_stream_dispatch_ack_output_and_owned_stop() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let origin = format!("http://{addr}");
    let frames = sdk_gold()["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| {
            r["inboundBinary"]
                .as_str()
                .map(|s| base64::engine::general_purpose::STANDARD.decode(s).unwrap())
        })
        .collect::<Vec<_>>();
    let closed = Arc::new(tokio::sync::Notify::new());
    let acknowledged = Arc::new(tokio::sync::Notify::new());
    let replied = Arc::new(tokio::sync::Notify::new());
    let (c, a, r) = (closed.clone(), acknowledged.clone(), replied.clone());
    let server = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        loop {
            tokio::select! {connection=listener.accept()=>{let(mut socket,_)=connection.unwrap();let frames=frames.clone();let(c,a,r)=(c.clone(),a.clone(),r.clone());children.spawn(async move{
            let mut peek=[0;8192];let n=socket.peek(&mut peek).await.unwrap();
            if String::from_utf8_lossy(&peek[..n]).to_lowercase().contains("upgrade: websocket"){
            let mut ws=tokio_tungstenite::accept_async(socket).await.unwrap();let mut injected=false;
            while let Some(frame)=ws.next().await{match frame{
            Ok(tokio_tungstenite::tungstenite::Message::Binary(raw))=>{let frame=Frame::decode(&raw).unwrap();if frame.method==0&&!injected{injected=true;for raw in &frames{ws.send(tokio_tungstenite::tungstenite::Message::Binary(raw.clone().into())).await.unwrap();}}else if frame.method==1{assert_eq!(String::from_utf8(frame.payload).unwrap(),r#"{"code":200}"#);a.notify_one();}},
            Ok(tokio_tungstenite::tungstenite::Message::Close(_))=>{let _=ws.close(None).await;c.notify_one();break;},Err(_)=>{c.notify_one();break;},_=>{}
            }}return;}
            let mut header=vec![];while !header.ends_with(b"\r\n\r\n"){let mut byte=[0];if socket.read_exact(&mut byte).await.is_err(){return;}header.push(byte[0]);}
            let header=String::from_utf8(header).unwrap();let path=header.lines().next().unwrap().split_whitespace().nth(1).unwrap();let len=header.lines().find_map(|s|s.to_lowercase().strip_prefix("content-length:").map(|s|s.trim().parse::<usize>().unwrap())).unwrap_or(0);let mut body=vec![0;len];socket.read_exact(&mut body).await.unwrap();
            let value=if path=="/callback/ws/endpoint"{serde_json::json!({"code":0,"data":{"URL":format!("ws://{addr}/stream?service_id=77&device_id=fixture"),"ClientConfig":{"PingInterval":1,"ReconnectCount":2,"ReconnectInterval":0.01,"ReconnectNonce":0}}})}else if path.contains("tenant_access_token"){serde_json::json!({"code":0,"tenant_access_token":"token","expire":7200})}else if path.contains("reactions"){serde_json::json!({"code":0,"data":{"reaction_id":"reaction"}})}else{if String::from_utf8_lossy(&body).contains("Feishu SDK reply"){r.notify_one();}serde_json::json!({"code":0,"data":{"message_id":"reply"}})};
            let data=value.to_string();let response=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",data.len());let _=socket.write_all(response.as_bytes()).await;
            });},Some(_)=children.join_next(),if !children.is_empty()=>{}}
        }
    });
    let (tx, mut turns) = tokio::sync::mpsc::unbounded_channel();
    let channel = FeishuChannel::new(
        FeishuOptions {
            app_id: "cli_0123456789abcdef".into(),
            app_secret: "mock-secret".into(),
            connection_mode: "stream".into(),
            base_url: origin,
            ..Default::default()
        },
        Arc::new(super::super::im_interaction::NoInteraction),
    );
    let handle = channel
        .start(ChannelStartDeps {
            turn_gateway: Some(Arc::new(Gateway(tx))),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = tokio::time::timeout(Duration::from_secs(5), turns.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(turn.message, JsString::from("SDK fragmented input"));
    assert_eq!(turn.channel_key, ChannelKey::new("feishu"));
    tokio::time::timeout(Duration::from_secs(5), acknowledged.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), replied.notified())
        .await
        .unwrap();
    handle.stop("fixture").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), closed.notified())
        .await
        .unwrap();
    assert!(turns.try_recv().is_err(), "fragments dispatch exactly once");
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn invalid_app_id_never_attempts_endpoint_request() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let observed = Arc::new(AtomicUsize::new(0));
    let count = observed.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            let mut raw = [0; 8192];
            let _ = socket.read(&mut raw).await;
            let data = r#"{"code":400,"msg":"invalid app"}"#;
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",
                data.len()
            );
            let _ = socket.write_all(reply.as_bytes()).await;
        }
    });
    for id in [
        "app-1",
        "cli_short",
        "CLI_0123456789abcdef",
        "cli_0123456789abcdeg",
        "cli_0123456789abcdef0",
    ] {
        let channel = FeishuChannel::new(
            FeishuOptions {
                app_id: id.into(),
                app_secret: "mock-only".into(),
                connection_mode: "stream".into(),
                base_url: origin.clone(),
                ..Default::default()
            },
            Arc::new(super::super::im_interaction::NoInteraction),
        );
        let handle = channel.start(ChannelStartDeps::default()).await.unwrap();
        handle.stop("fixture").await.unwrap();
    }
    assert_eq!(
        observed.load(Ordering::SeqCst),
        0,
        "actual SDK invalid-appId gate must run before every endpoint request"
    );
    server.abort();
    let _ = server.await;
}

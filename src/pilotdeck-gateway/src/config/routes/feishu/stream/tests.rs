use super::*;
use pilotdeck_protocol::frame::{self, Decoder};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Notify, mpsc},
};

#[test]
fn protobuf_matches_sdk_known_ping_wire_and_rejects_incomplete_frames() {
    let ping = Frame {
        service: 42,
        headers: vec![("type".into(), "ping".into())],
        ..Default::default()
    };
    assert_eq!(
        ping.encode(),
        [
            8, 0, 16, 0, 24, 42, 32, 0, 42, 12, 10, 4, 116, 121, 112, 101, 18, 4, 112, 105, 110,
            103
        ]
    );
    assert_eq!(Frame::decode(&ping.encode()).unwrap(), ping);
    assert!(Frame::decode(&[8, 0, 16, 0, 24, 42]).is_err());
    assert!(Frame::decode(&[8, 0, 16, 0, 24, 42, 32, 0, 66, 255]).is_err());
    let frame = Frame {
        seq: u64::MAX,
        log: 9,
        service: 42,
        method: 1,
        headers: vec![("sum".into(), "1".into())],
        encoding: Some(b"json".to_vec()),
        payload_type: Some(b"application/json".to_vec()),
        payload: Some(b"{}".to_vec()),
        log_new: Some(b"new".to_vec()),
    };
    assert_eq!(Frame::decode(&frame.encode()).unwrap(), frame);
}

#[test]
fn fragments_merge_out_of_order_replace_duplicates_and_expire() {
    let mut cache = Cache::default();
    let mut frame = Frame {
        method: 1,
        headers: vec![
            ("type".into(), "event".into()),
            ("message_id".into(), "id".into()),
            ("sum".into(), "2".into()),
            ("seq".into(), "1".into()),
        ],
        payload: Some(br#""x"}"#.to_vec()),
        ..Default::default()
    };
    assert!(cache.merge(&frame).unwrap().is_none());
    assert!(cache.merge(&frame).unwrap().is_none());
    frame.headers.last_mut().unwrap().1 = "0".into();
    frame.payload = Some(br#"{"value":"#.to_vec());
    assert_eq!(cache.merge(&frame).unwrap(), Some(json!({"value":"x"})));
    assert!(cache.0.is_empty());
    assert!(cache.merge(&frame).unwrap().is_none());
    cache.0.get_mut("id").unwrap().created = Instant::now() - Duration::from_secs(11);
    frame.headers.last_mut().unwrap().1 = "1".into();
    frame.payload = Some(br#""x"}"#.to_vec());
    assert!(cache.merge(&frame).unwrap().is_none());
    frame.headers.last_mut().unwrap().1 = "9".into();
    assert!(cache.merge(&frame).is_err());
}

#[derive(Default)]
pub(crate) struct Gateway {
    pub(crate) turns: StdMutex<Vec<Value>>,
    requests: StdMutex<Vec<(String, Value)>>,
    changed: Notify,
}
impl ChannelGateway for Gateway {
    fn request<'a>(&'a self, method: &'a str, params: Value) -> ChannelCall<'a, Value> {
        Box::pin(async move {
            self.requests.lock().unwrap().push((method.into(), params));
            self.changed.notify_waiters();
            Ok(json!({}))
        })
    }
    fn turn(&self, input: Value) -> ChannelCall<'_, Vec<Value>> {
        Box::pin(async move {
            self.turns.lock().unwrap().push(input);
            self.changed.notify_waiters();
            Ok(vec![])
        })
    }
}

#[tokio::test]
async fn pending_elicitation_reply_bypasses_blocked_media_drainer() {
    let gateway = Arc::new(Gateway::default());
    let owner = FeishuChannel::new(
        FeishuOptions {
            app_id: "app".into(),
            app_secret: "secret".into(),
            connection_mode: "webhook".into(),
            ..Default::default()
        },
        gateway.clone(),
    );
    let handle = owner.start().await.unwrap();
    owner.batches.lock().unwrap().insert(
        "chat".into(),
        Batch {
            messages: VecDeque::new(),
            epoch: 1,
            draining: true,
        },
    );
    owner.chat("chat").lock().await.elicitation = Some((
        "waiting-session".into(),
        json!({"requestId":"Q","questions":[{"question":"Pick","options":[{"label":"A"},{"label":"B"}]}]}),
    ));
    enqueue(
        &owner,
        BatchInput::Text(Inbound {
            chat: "chat".into(),
            text: "2".into(),
            event_id: "answer".into(),
            message_id: None,
        }),
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let changed = gateway.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !gateway.requests.lock().unwrap().is_empty() {
                return;
            }
            changed.await;
        }
    })
    .await
    .unwrap();
    let requests = gateway.requests.lock().unwrap();
    assert_eq!(requests[0].0, "elicitation_respond");
    assert_eq!(requests[0].1["sessionKey"], "waiting-session");
    drop(requests);
    assert!(gateway.turns.lock().unwrap().is_empty());
    assert!(owner.batches.lock().unwrap()["chat"].messages.is_empty());
    handle.close().await;
}
async fn request(stream: &mut TcpStream) -> (String, String, Value) {
    let mut raw = Vec::new();
    while !raw.ends_with(b"\r\n\r\n") {
        raw.push(stream.read_u8().await.unwrap());
    }
    let headers = String::from_utf8(raw).unwrap();
    let path = headers.split_whitespace().nth(1).unwrap().to_owned();
    let len = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|n| n.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    let mut body = vec![0; len];
    stream.read_exact(&mut body).await.unwrap();
    (
        path,
        headers,
        serde_json::from_slice(&body).unwrap_or(Value::Null),
    )
}
async fn reply(stream: &mut TcpStream, body: Value) {
    let body = body.to_string();
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
}
async fn read_frame(stream: &mut TcpStream, decoder: &mut Decoder) -> Option<Frame> {
    loop {
        if let Some(frame) = decoder.next().unwrap() {
            match frame.opcode {
                8 => return None,
                2 => return Some(Frame::decode(&frame.payload).unwrap()),
                _ => continue,
            }
        }
        if stream.read_buf(&mut decoder.buffer).await.unwrap() == 0 {
            return None;
        }
    }
}
async fn send_frame(stream: &mut TcpStream, frame: Frame) {
    stream
        .write_all(&frame::server_frame(2, &frame.encode()))
        .await
        .unwrap();
}
pub(crate) async fn wait_turn(gateway: &Gateway, count: usize) {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let changed = gateway.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if gateway.turns.lock().unwrap().len() >= count {
                return;
            }
            changed.await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn real_stream_endpoint_binary_ping_fragments_ack_reconnect_and_stop() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = endpoint_calls.clone();
    let acks = Arc::new(StdMutex::new(Vec::<Frame>::new()));
    let recorded = acks.clone();
    let (connected, mut connections) = mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        let mut tasks = JoinSet::new();
        loop {
            let (mut io, _) = listener.accept().await.unwrap();
            let calls = calls.clone();
            let recorded = recorded.clone();
            let connected = connected.clone();
            tasks.spawn(async move{
        let (path,headers,body)=request(&mut io).await;
        if path=="/callback/ws/endpoint" {assert_eq!(body,json!({"AppID":"cli_0123456789abcdef","AppSecret":"secret"}));calls.fetch_add(1,Ordering::SeqCst);reply(&mut io,json!({"code":0,"data":{"URL":format!("ws://{address}/socket?device_id=device&service_id=42"),"ClientConfig":{"PingInterval":0.02,"ReconnectCount":3,"ReconnectInterval":0.02,"ReconnectNonce":0}}})).await;return;}
        if path.starts_with("/socket?") {
            let key=headers.lines().find_map(|line|line.split_once(':').filter(|(key,_)|key.eq_ignore_ascii_case("sec-websocket-key")).map(|(_,value)|value.trim())).unwrap();let accept=tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());io.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await.unwrap();
            let mut decoder=Decoder::default();let ping=read_frame(&mut io,&mut decoder).await.unwrap();assert_eq!(ping.service,42);assert_eq!(ping.header("type"),Some("ping"));
            let connection=calls.load(Ordering::SeqCst);connected.send(connection).unwrap();
            send_frame(&mut io,Frame {service:42,headers:vec![("type".into(),"pong".into())],payload:Some(json!({"PingInterval":0.02,"ReconnectCount":3,"ReconnectInterval":0.02,"ReconnectNonce":0}).to_string().into_bytes()),..Default::default()}).await;
            let event=json!({"schema":"2.0","header":{"event_type":"im.message.receive_v1","token":"different-webhook-token"},"event":{"message":{"chat_id":"chat","message_id":"message-once","message_type":"text","content":"{\"text\":\"hello\"}"}}}).to_string().into_bytes();let mid=event.len()/2;
            for (seq,payload) in [(1,event[mid..].to_vec()),(0,event[..mid].to_vec())] {send_frame(&mut io,Frame {seq:7,log:8,service:42,method:1,headers:vec![("type".into(),"event".into()),("message_id".into(),"fragments".into()),("sum".into(),"2".into()),("seq".into(),seq.to_string())],payload:Some(payload),..Default::default()}).await;}
            loop {let Some(frame)=read_frame(&mut io,&mut decoder).await else {return;};if frame.method==1 {assert_eq!(frame.payload,Some(br#"{"code":200}"#.to_vec()));assert!(frame.header("biz_rt").is_some());recorded.lock().unwrap().push(frame);break;}}
            if connection==1 {io.write_all(&frame::server_frame(8,&1000u16.to_be_bytes())).await.unwrap();return;}
            while read_frame(&mut io,&mut decoder).await.is_some() {}return;
        }
        let response=if path.ends_with("/tenant_access_token/internal") {json!({"code":0,"tenant_access_token":"token","expire":7200})}else {json!({"code":0,"data":{"reaction_id":"reaction","message_id":"reply"}})};reply(&mut io,response).await;
    });
            while tasks.try_join_next().is_some() {}
        }
    });
    let gateway = Arc::new(Gateway::default());
    let channel = FeishuChannel::new(
        FeishuOptions {
            app_id: "cli_0123456789abcdef".into(),
            app_secret: "secret".into(),
            api_base: format!("http://{address}/open-apis"),
            verify_token: Some("webhook-only".into()),
            ..Default::default()
        },
        gateway.clone(),
    );
    let handle = channel.start().await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(4), connections.recv())
            .await
            .unwrap(),
        Some(1)
    );
    wait_turn(&gateway, 1).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(4), connections.recv())
            .await
            .unwrap(),
        Some(2)
    );
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if acks.lock().unwrap().len() == 2 {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(gateway.turns.lock().unwrap().len(), 1);
    assert_eq!(gateway.turns.lock().unwrap()[0]["message"], "hello");
    handle.close().await;
    let before = endpoint_calls.load(Ordering::SeqCst);
    sleep(Duration::from_millis(60)).await;
    assert_eq!(endpoint_calls.load(Ordering::SeqCst), before);
    server.abort();
}

#[tokio::test]
async fn endpoint_rejection_keeps_webhook_available_and_stop_cancels_pending_endpoint() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (seen, mut requests) = mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        let (mut io, _) = listener.accept().await.unwrap();
        request(&mut io).await;
        seen.send(()).unwrap();
        reply(&mut io, json!({"code":514,"msg":"denied"})).await;
    });
    let channel = FeishuChannel::new(
        FeishuOptions {
            app_id: "cli_0123456789abcdef".into(),
            app_secret: "secret".into(),
            api_base: format!("http://{address}/open-apis"),
            ..Default::default()
        },
        Arc::new(Gateway::default()),
    );
    let handle = channel.start().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), requests.recv())
        .await
        .unwrap();
    server.await.unwrap();
    assert!(channel.running.load(Ordering::SeqCst));
    assert_eq!(
        channel
            .webhook(r#"{"type":"url_verification","challenge":"ok"}"#)
            .await
            .1,
        json!({"challenge":"ok"})
    );
    handle.close().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (seen, mut requests) = mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        let (mut io, _) = listener.accept().await.unwrap();
        request(&mut io).await;
        seen.send(()).unwrap();
        let mut byte = [0];
        assert_eq!(io.read(&mut byte).await.unwrap(), 0);
    });
    let channel = FeishuChannel::new(
        FeishuOptions {
            app_id: "cli_0123456789abcdef".into(),
            app_secret: "secret".into(),
            api_base: format!("http://{address}/open-apis"),
            ..Default::default()
        },
        Arc::new(Gateway::default()),
    );
    let handle = channel.start().await.unwrap();
    requests.recv().await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), handle.close())
        .await
        .unwrap();
    server.await.unwrap();
}

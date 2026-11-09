use super::*;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

type Peer = BufReader<TcpStream>;
async fn read(peer: &mut Peer) -> Value {
    let mut line = String::new();
    assert_ne!(
        timeout(Duration::from_secs(2), peer.read_line(&mut line))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(line.len() < 1024 * 1024);
    serde_json::from_str(&line).unwrap()
}
async fn send(peer: &mut Peer, value: Value) {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    timeout(Duration::from_secs(2), peer.get_mut().write_all(&bytes))
        .await
        .unwrap()
        .unwrap();
}
fn response(request: &Value, generation: &str) -> Value {
    json!({"kind":"response","messageId":"reply","inReplyTo":request["messageId"],"ok":true,"protocolVersion":"2.0","moduleId":"fixture","moduleInstanceId":"same-external-instance","connectionGeneration":generation,"capabilitiesVersion":"v1"})
}
async fn handshake_peer(peer: &mut Peer, generation: &str) {
    let hello = read(peer).await;
    assert_eq!(hello["method"], "hello");
    send(peer, response(&hello, generation)).await;
    let caps = read(peer).await;
    assert_eq!(caps["method"], "capabilities");
    let mut reply = response(&caps, generation);
    reply["payload"] = json!({"capabilitiesVersion":"v1","methods":[{"name":"execute","profiles":["streaming"],"resumeSupport":"streaming"},{"name":"resume"},{"name":"ack"},{"name":"cancel"}]});
    send(peer, reply).await;
}
fn event(sequence: u64, final_event: bool) -> Value {
    let payload = if final_event {
        json!({"result":{"type":"success","sessionId":"session","turnId":"turn","stopReason":"completed","turns":1},"messages":[]})
    } else {
        json!({"type":"model_request_started","sessionId":"session","turnId":"turn"})
    };
    let mut value = json!({"kind":"event","eventType":"agent_event","streamId":"stream","sequence":sequence,"final":final_event,"runId":"run","operationId":"operation","requestId":"request","payload":payload});
    if final_event {
        value["outcome"] = json!("completed");
    }
    value
}
async fn accepted(peer: &mut Peer, execute: &Value, generation: &str) {
    let mut reply = response(execute, generation);
    reply["requestId"] = json!("request");
    reply["streamId"] = json!("stream");
    send(peer, reply).await;
}
async fn eof(peer: &mut Peer) {
    // ACK/cancel frames are best effort; consume complete protocol lines until
    // this client closes its socket. The listener itself remains external.
    timeout(Duration::from_secs(2), async {
        let mut line = String::new();
        loop {
            line.clear();
            if peer.read_line(&mut line).await.unwrap() == 0 {
                break;
            }
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn tcp_terminal_uses_same_host_and_closes_only_its_socket() {
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
    let address = listener.local_addr().unwrap();
    let retained = listener.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = retained.accept().await.unwrap();
        let mut peer = BufReader::new(stream);
        handshake_peer(&mut peer, "gen-1").await;
        let execute = read(&mut peer).await;
        assert_eq!(execute["method"], "execute");
        accepted(&mut peer, &execute, "gen-1").await;
        send(&mut peer, event(0, true)).await;
        eof(&mut peer).await;
    });
    let client = HostClient::connect_tcp(
        "127.0.0.1",
        address.port(),
        None,
        Some(&"fixture".into()),
        limits(),
    )
    .await
    .unwrap();
    assert_eq!(client.child_id(), None);
    let handler = Handler::new("success");
    assert!(matches!(
        client.execute(request(), handler.clone()).await.unwrap(),
        ExecuteResult::Terminal(_)
    ));
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
    timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
    let _second = TcpStream::connect(address)
        .await
        .expect("external listener survives client close");
}
#[tokio::test]
async fn tcp_disconnect_after_acceptance_resumes_once_without_second_execute() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let executes = Arc::new(AtomicUsize::new(0));
    let observed = executes.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut first = BufReader::new(stream);
        handshake_peer(&mut first, "gen-1").await;
        let execute = read(&mut first).await;
        assert_eq!(execute["method"], "execute");
        observed.fetch_add(1, Ordering::SeqCst);
        accepted(&mut first, &execute, "gen-1").await;
        send(&mut first, event(0, false)).await;
        drop(first);
        let (stream, _) = timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut next = BufReader::new(stream);
        handshake_peer(&mut next, "gen-2").await;
        let resume = read(&mut next).await;
        assert_eq!(resume["method"], "resume");
        assert_eq!(resume["streamId"], "stream");
        assert_eq!(
            resume["previousBinding"],
            json!({"moduleInstanceId":"same-external-instance","connectionGeneration":"gen-1"})
        );
        assert_eq!(resume["lastAppliedSequence"], 0);
        let mut reply = response(&resume, "gen-2");
        reply["streamId"] = json!("stream");
        send(&mut next, reply).await;
        send(&mut next, event(1, true)).await;
        eof(&mut next).await;
    });
    let client = HostClient::connect_tcp(
        "127.0.0.1",
        address.port(),
        None,
        Some(&"fixture".into()),
        limits(),
    )
    .await
    .unwrap();
    let handler = Handler::new("success");
    let result = client.execute(request(), handler.clone()).await.unwrap();
    assert!(matches!(result, ExecuteResult::Terminal(_)));
    assert_eq!(executes.load(Ordering::SeqCst), 1);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
    timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn tcp_disconnect_before_acceptance_does_not_replay_execute() {
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
    let address = listener.local_addr().unwrap();
    let server_listener = listener.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = server_listener.accept().await.unwrap();
        let mut peer = BufReader::new(stream);
        handshake_peer(&mut peer, "gen-1").await;
        assert_eq!(read(&mut peer).await["method"], "execute");
    });
    let client = HostClient::connect_tcp(
        "127.0.0.1",
        address.port(),
        None,
        Some(&"fixture".into()),
        limits(),
    )
    .await
    .unwrap();
    let error = client
        .execute(request(), Handler::new("success"))
        .await
        .unwrap_err();
    assert!(!error.accepted);
    assert!(matches!(
        error.kind,
        HostErrorKind::Io | HostErrorKind::Disconnected
    ));
    server.await.unwrap();
    assert!(
        timeout(Duration::from_millis(80), listener.accept())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn tcp_silent_handshake_is_bounded_and_socket_is_closed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut peer = BufReader::new(stream);
        assert_eq!(read(&mut peer).await["method"], "hello");
        eof(&mut peer).await;
    });
    let mut limits = limits();
    limits.handshake_timeout = Duration::from_millis(80);
    let result = HostClient::connect_tcp(
        "127.0.0.1",
        address.port(),
        None,
        Some(&"fixture".into()),
        limits,
    )
    .await;
    let error = match result {
        Ok(_) => panic!("silent peer accepted"),
        Err(error) => error,
    };
    assert_eq!(error.kind, HostErrorKind::Timeout);
    timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn tcp_input_uses_existing_frame_byte_limit() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut peer = BufReader::new(stream);
        let _ = read(&mut peer).await;
        peer.get_mut().write_all(&vec![b'x'; 513]).await.unwrap();
        eof(&mut peer).await;
    });
    let mut limits = limits();
    limits.max_frame_bytes = 512;
    let result = HostClient::connect_tcp("127.0.0.1", address.port(), None, None, limits).await;
    let error = match result {
        Ok(_) => panic!("oversized frame accepted"),
        Err(error) => error,
    };
    assert_eq!(error.kind, HostErrorKind::Limit);
    timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn tcp_cancellation_reaches_external_peer_and_keeps_authoritative_terminal() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (started, wait) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut peer = BufReader::new(stream);
        handshake_peer(&mut peer, "gen-1").await;
        let execute = read(&mut peer).await;
        accepted(&mut peer, &execute, "gen-1").await;
        send(&mut peer, event(0, false)).await;
        let _ = started.send(());
        loop {
            let frame = read(&mut peer).await;
            if frame["method"] == "cancel" {
                send(&mut peer, response(&frame, "gen-1")).await;
                break;
            }
        }
        let mut terminal = event(1, true);
        terminal["outcome"] = json!("cancelled");
        terminal["payload"]["result"]["type"] = json!("aborted");
        send(&mut peer, terminal).await;
        eof(&mut peer).await;
    });
    let client = HostClient::connect_tcp(
        "127.0.0.1",
        address.port(),
        None,
        Some(&"fixture".into()),
        limits(),
    )
    .await
    .unwrap();
    let cancellation = Cancellation::default();
    let handler = Handler::new("success");
    let running =
        tokio::spawn(client.execute_cancellable(request(), handler.clone(), cancellation.clone()));
    wait.await.unwrap();
    cancellation.cancel(Some("fixture_cancel".into()));
    let result = timeout(Duration::from_secs(3), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match &result {
        ExecuteResult::Terminal(value) => assert!(text_is(value.get("outcome"), "cancelled")),
        _ => panic!("expected authoritative cancelled terminal"),
    };
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
    timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
}

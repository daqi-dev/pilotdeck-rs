use super::*;
use crate::frame::{DecodeError, client_frame, server_frame};
use std::time::Duration;
use tokio::{io::DuplexStream, task::JoinHandle, time::timeout};

const DEADLINE: Duration = Duration::from_secs(3);

struct Pair {
    client: DuplexStream,
    server: DuplexStream,
    task: JoinHandle<std::io::Result<()>>,
}

impl Pair {
    fn new() -> Self {
        let (client, downstream) = tokio::io::duplex(1024 * 1024);
        let (upstream, server) = tokio::io::duplex(1024 * 1024);
        Self {
            client,
            server,
            task: tokio::spawn(run(downstream, upstream)),
        }
    }

    async fn finish(self) {
        assert!(join_pair_task(self.task, DEADLINE).await, "pair must stop");
    }
}

// A timed-out JoinHandle must be aborted and awaited: dropping it detaches.
async fn join_pair_task(mut task: JoinHandle<std::io::Result<()>>, deadline: Duration) -> bool {
    match timeout(deadline, &mut task).await {
        Ok(result) => {
            result.unwrap().unwrap();
            true
        }
        Err(_) => {
            task.abort();
            let _ = task.await; // Also handles completion racing with abort.
            false
        }
    }
}

#[tokio::test]
async fn pair_finish_timeout_aborts_and_awaits_the_task() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let observed = dropped.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _guard = Dropped(observed);
        started.send(()).unwrap();
        std::future::pending::<()>().await;
        Ok(())
    });
    ready.await.unwrap();
    assert!(!join_pair_task(task, Duration::from_millis(1)).await);
    assert!(
        dropped.load(Ordering::SeqCst),
        "task was detached instead of awaited"
    );
}

#[tokio::test]
async fn pair_eof_guard_rejects_trailing_response_bytes() {
    let (mut reader, mut writer) = tokio::io::duplex(1024);
    write(&mut writer, b"responseextra").await;
    read(&mut reader, b"response").await;
    writer.shutdown().await.unwrap();
    let checked = tokio::spawn(async move { eof(&mut reader).await });
    assert!(checked.await.unwrap_err().is_panic());
}

async fn write(io: &mut DuplexStream, bytes: &[u8]) {
    timeout(DEADLINE, io.write_all(bytes))
        .await
        .expect("write timeout")
        .unwrap();
}

async fn read(io: &mut DuplexStream, expected: &[u8]) {
    let mut actual = vec![0; expected.len()];
    timeout(DEADLINE, io.read_exact(&mut actual))
        .await
        .expect("read timeout")
        .unwrap();
    assert_eq!(actual, expected);
}

async fn eof(io: &mut DuplexStream) {
    let mut byte = [0];
    assert_eq!(
        timeout(DEADLINE, io.read(&mut byte))
            .await
            .expect("EOF timeout")
            .unwrap(),
        0
    );
}

async fn silent(io: &mut DuplexStream) {
    let mut byte = [0];
    assert!(
        timeout(Duration::from_millis(25), io.read(&mut byte))
            .await
            .is_err(),
        "unexpected output or EOF"
    );
}

#[tokio::test]
async fn rsv_and_fin_are_not_validated_or_reencoded() {
    let mut pair = Pair::new();
    for first in [0x01, 0x41, 0x81, 0xf1] {
        let wire = client_frame(first, 126, b"{\"type\":\"hello\"}");
        write(&mut pair.client, &wire).await;
        read(&mut pair.server, &wire).await;
    }
    pair.client.shutdown().await.unwrap();
    eof(&mut pair.server).await;
    pair.finish().await;
}

#[tokio::test]
async fn short_126_127_length_forms_and_mask_key_are_preserved() {
    let mut pair = Pair::new();
    for code in [3, 126, 127] {
        let wire = client_frame(0x81, code, b"raw");
        write(&mut pair.client, &wire).await;
        read(&mut pair.server, &wire).await;
    }
    pair.server.shutdown().await.unwrap();
    eof(&mut pair.client).await;
    pair.finish().await;
}

#[tokio::test]
async fn large_text_payload_over_64k_stays_byte_exact() {
    let mut pair = Pair::new();
    let payload: Vec<_> = (0..131072).map(|i| (i % 256) as u8).collect();
    let wire = client_frame(0xb1, 127, &payload);
    write(&mut pair.client, &wire).await;
    read(&mut pair.server, &wire).await;
    let reply = server_frame(1, &payload);
    write(&mut pair.server, &reply).await;
    read(&mut pair.client, &reply).await;
    pair.server.shutdown().await.unwrap();
    pair.finish().await;
}

#[tokio::test]
async fn fragmented_tcp_reads_and_coalesced_frames_decode_independently() {
    let (mut client, downstream) = tokio::io::duplex(7);
    let (upstream, mut server) = tokio::io::duplex(7);
    let relay = tokio::spawn(run(downstream, upstream));
    let mut wire = client_frame(0x01, 127, b"first fragment is itself a message");
    wire.extend(client_frame(0x81, 126, b"second"));
    let expected = wire.clone();
    let sender = tokio::spawn(async move {
        for chunk in wire.chunks(3) {
            write(&mut client, chunk).await;
        }
        client // Keep the endpoint open until all frames reach upstream.
    });
    read(&mut server, &expected).await;
    let mut client = timeout(DEADLINE, sender).await.unwrap().unwrap();
    client.shutdown().await.unwrap();
    eof(&mut server).await;
    timeout(DEADLINE, relay).await.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn continuation_binary_pong_and_reserved_opcodes_are_silently_ignored() {
    let mut pair = Pair::new();
    for opcode in [0, 2, 3, 4, 5, 6, 7, 10, 11, 12, 13, 14, 15] {
        write(
            &mut pair.client,
            &client_frame(0xf0 | opcode, 127, b"ignored"),
        )
        .await;
    }
    // A following text proves the ignored frames were consumed, without any
    // fragment state affecting the next message or output on either endpoint.
    let hello = client_frame(0x81, 5, b"hello");
    write(&mut pair.client, &hello).await;
    read(&mut pair.server, &hello).await;
    silent(&mut pair.client).await;
    silent(&mut pair.server).await;
    pair.client.shutdown().await.unwrap();
    pair.finish().await;
}

#[tokio::test]
async fn ping_returns_payload_in_canonical_unmasked_pong() {
    let mut pair = Pair::new();
    for payload in [vec![], vec![0, 255, 128, 1], vec![42; 126], vec![7; 65536]] {
        // FIN=0, RSV=7, extended encoding and oversized control payloads are
        // accepted by TS; a strict RFC codec would reject this input.
        let ping = client_frame(0x79, 127, &payload);
        write(&mut pair.client, &ping).await;
        read(&mut pair.server, &ping).await;
        silent(&mut pair.client).await;
        write(&mut pair.server, &server_frame(10, &payload)).await;
        read(&mut pair.client, &server_frame(10, &payload)).await;
    }
    silent(&mut pair.server).await;
    pair.client.shutdown().await.unwrap();
    pair.finish().await;
}

async fn check_client_close(payload: &[u8]) {
    let mut pair = Pair::new();
    let wire = client_frame(0x78, 127, payload);
    write(&mut pair.client, &wire).await;
    read(&mut pair.server, &wire).await;
    silent(&mut pair.client).await;
    write(&mut pair.server, &[0x88, 2, 3, 232]).await;
    pair.server.shutdown().await.unwrap();
    read(&mut pair.client, &[0x88, 2, 3, 232]).await;
    eof(&mut pair.client).await;
    eof(&mut pair.server).await;
    pair.finish().await;
}

#[tokio::test]
async fn empty_client_close_replies_1000_not_1005() {
    check_client_close(&[]).await;
}

#[tokio::test]
async fn custom_and_malformed_client_close_replies_1000_without_echo() {
    check_client_close(&[0x11, 0x5c, b'r', 255]).await;
    check_client_close(&[255]).await;
}

#[tokio::test]
async fn close_discards_following_coalesced_text() {
    let mut pair = Pair::new();
    let close = client_frame(0x88, 0, &[]);
    let mut wire = close.clone();
    wire.extend(client_frame(0x81, 5, b"after"));
    write(&mut pair.client, &wire).await;
    read(&mut pair.server, &close).await;
    silent(&mut pair.server).await;
    write(&mut pair.server, &[0x88, 2, 3, 232]).await;
    pair.server.shutdown().await.unwrap();
    read(&mut pair.client, &[0x88, 2, 3, 232]).await;
    eof(&mut pair.server).await;
    pair.finish().await;
}

#[tokio::test]
async fn unmasked_exception_preserves_bytes_without_protocol_close() {
    let mut pair = Pair::new();
    // Exception happens immediately after the length header, even before a
    // declared payload arrives. TS must see these bytes to own CLI shutdown.
    write(&mut pair.client, &[0x81, 126, 0, 20]).await;
    read(&mut pair.server, &[0x81, 126, 0, 20]).await;
    write(&mut pair.client, b"later bytes").await;
    read(&mut pair.server, b"later bytes").await;
    silent(&mut pair.client).await;
    // Simulate the TS CLI's lingering exceptional socket; pair remains alive
    // until the offending client tears down, as in A2's raw probe.
    assert!(!pair.task.is_finished());
    pair.client.shutdown().await.unwrap();
    eof(&mut pair.server).await;
    pair.finish().await;
}

#[tokio::test]
async fn oversized_length_exception_is_forwarded_without_allocation_or_close() {
    let mut pair = Pair::new();
    let mut wire = vec![0x81, 0xff];
    wire.extend_from_slice(&(1u64 << 53).to_be_bytes());
    write(&mut pair.client, &wire).await;
    read(&mut pair.server, &wire).await;
    silent(&mut pair.client).await;
    pair.server.shutdown().await.unwrap();
    eof(&mut pair.client).await;
    pair.finish().await;
}

#[tokio::test]
async fn partial_frame_eof_has_no_protocol_reply_and_closes_upstream() {
    let mut pair = Pair::new();
    write(&mut pair.client, &[0x81, 0xfe, 0]).await;
    pair.client.shutdown().await.unwrap();
    eof(&mut pair.client).await;
    eof(&mut pair.server).await;
    pair.finish().await;
}

#[tokio::test]
async fn downstream_eof_cancels_idle_upstream_without_waiting_for_half_close() {
    let mut pair = Pair::new();
    pair.client.shutdown().await.unwrap();
    eof(&mut pair.server).await;
    eof(&mut pair.client).await;
    pair.finish().await;
}

#[tokio::test]
async fn upstream_eof_cancels_partial_downstream_read() {
    let mut pair = Pair::new();
    write(&mut pair.client, &[0x81]).await;
    pair.server.shutdown().await.unwrap();
    eof(&mut pair.client).await;
    pair.finish().await;
}

#[tokio::test]
async fn distinct_pairs_do_not_share_upstream_or_lifecycle() {
    let mut first = Pair::new();
    let mut second = Pair::new();
    let wire = client_frame(0x81, 3, b"two");
    write(&mut second.client, &wire).await;
    read(&mut second.server, &wire).await;
    silent(&mut first.server).await;
    first.client.shutdown().await.unwrap();
    first.finish().await;
    let reply = server_frame(1, b"still connected");
    write(&mut second.server, &reply).await;
    read(&mut second.client, &reply).await;
    second.server.shutdown().await.unwrap();
    second.finish().await;
}

#[tokio::test]
async fn upstream_close_codes_and_all_ts_reasons_remain_wire_exact() {
    for (code, reason) in [
        (4001u16, "hello_required"),
        (4001, "protocol_mismatch"),
        (4002, "invalid_json"),
        (4002, "invalid_frame"),
        (4003, "auth_failed"),
    ] {
        let mut pair = Pair::new();
        let mut payload = code.to_be_bytes().to_vec();
        payload.extend_from_slice(reason.as_bytes());
        let wire = server_frame(8, &payload);
        write(&mut pair.server, &wire).await;
        pair.server.shutdown().await.unwrap();
        read(&mut pair.client, &wire).await;
        eof(&mut pair.client).await;
        eof(&mut pair.server).await;
        pair.finish().await;
    }
}

#[tokio::test]
async fn hello_binding_notifications_events_and_response_bytes_pass_through() {
    let mut pair = Pair::new();
    let messages: &[&[u8]] = &[
        br#"{ "type": "hello_ok", "interactionBinding": {"connectionId":"original","generation":77} }"#,
        br#"{"type":"event","seq":0,"event":{"type":"text","text":"a"}}"#,
        br#"{"type":"notification","name":"config_changed","payload":null}"#,
        br#"{"type":"response","id":"kept","result":1}"#,
    ];
    for message in messages {
        let wire = server_frame(1, message);
        write(&mut pair.server, &wire).await;
        read(&mut pair.client, &wire).await;
    }
    pair.server.shutdown().await.unwrap();
    pair.finish().await;
}

#[tokio::test]
async fn before_auth_forwards_known_unknown_invalid_json_and_utf8_payloads() {
    let mut pair = Pair::new();
    let payloads: &[&[u8]] = &[
        br#"{ "type":"request", "id":"x", "method":"list_projects", "params":{} }"#,
        br#"{"type":"request","id":"y","method":"unknown_future_method"}"#,
        br#"{"type":"hello","protocolVersion":"1.2","token":"exact"}"#,
        b"invalid json",
        &[0xff, 0x80, 0, 0xef, 0xbf, 0xbd],
        b"null",
        b"",
    ];
    for payload in payloads {
        let wire = client_frame(0x81, 127, payload);
        write(&mut pair.client, &wire).await;
        read(&mut pair.server, &wire).await;
    }
    pair.client.shutdown().await.unwrap();
    pair.finish().await;
}

#[test]
fn decoder_waits_for_complete_extended_header_mask_and_payload() {
    let wire = client_frame(0xf1, 127, b"abc");
    let mut decoder = Decoder::default();
    for (index, byte) in wire.iter().enumerate() {
        decoder.buffer.extend_from_slice(&[*byte]);
        let frame = decoder.next().unwrap();
        if index + 1 < wire.len() {
            assert!(frame.is_none());
        } else {
            let frame = frame.unwrap();
            assert_eq!(frame.opcode, 1);
            assert_eq!(frame.payload, b"abc");
            assert_eq!(frame.wire.as_ref(), wire);
        }
    }
    assert!(decoder.buffer.is_empty());
}

#[test]
fn decoder_length_precedes_mask_validation_and_safe_integer_bound_is_exact() {
    let mut decoder = Decoder::default();
    decoder.buffer.extend_from_slice(&[0x81, 127]);
    assert!(decoder.next().unwrap().is_none());
    decoder
        .buffer
        .extend_from_slice(&((1u64 << 53) - 1).to_be_bytes());
    assert_eq!(decoder.next().unwrap_err(), DecodeError::Unmasked);
    decoder.buffer[1] = 255;
    assert!(decoder.next().unwrap().is_none()); // No huge allocation.
    decoder.buffer[2..10].copy_from_slice(&(1u64 << 53).to_be_bytes());
    assert_eq!(decoder.next().unwrap_err(), DecodeError::TooLarge);
}

#[test]
fn server_reply_headers_match_ts_length_boundaries() {
    for (size, expected) in [
        (0, vec![0x8a, 0]),
        (125, vec![0x8a, 125]),
        (126, vec![0x8a, 126, 0, 126]),
        (65535, vec![0x8a, 126, 255, 255]),
        (65536, vec![0x8a, 127, 0, 0, 0, 0, 0, 1, 0, 0]),
    ] {
        let payload = vec![42; size];
        let wire = server_frame(10, &payload);
        assert_eq!(&wire[..expected.len()], expected);
        assert_eq!(&wire[expected.len()..], payload);
    }
}

#[tokio::test]
async fn upstream_nonminimal_lengths_fin_rsv_and_mask_are_preserved() {
    let mut pair = Pair::new();
    for first in [0x01, 0x41, 0xf1] {
        for length_code in [3, 126, 127] {
            // Even unusual masked upstream output is retained byte for byte.
            let wire = client_frame(first, length_code, b"raw");
            write(&mut pair.server, &wire).await;
            read(&mut pair.client, &wire).await;
        }
    }
    pair.server.shutdown().await.unwrap();
    pair.finish().await;
}

#[tokio::test]
async fn upstream_truncated_tail_remains_exact_on_eof() {
    let mut pair = Pair::new();
    let tail = [0xf1, 127, 0, 0, 0];
    write(&mut pair.server, &tail).await;
    pair.server.shutdown().await.unwrap();
    read(&mut pair.client, &tail).await;
    eof(&mut pair.client).await;
    pair.finish().await;
}

#[tokio::test]
async fn ts_pong_preserves_order_after_fragmented_upstream_text() {
    let mut pair = Pair::new();
    let big = server_frame(1, &vec![b'x'; 65536]);
    // Feed only a prefix, guaranteeing a TCP-chunk boundary in the text frame.
    write(&mut pair.server, &big[..8192]).await;
    let ping = client_frame(0x89, 1, b"p");
    write(&mut pair.client, &ping).await;
    read(&mut pair.server, &ping).await;
    silent(&mut pair.client).await;
    write(&mut pair.server, &big[8192..]).await;
    write(&mut pair.server, &[0x8a, 1, b'p']).await;
    read(&mut pair.client, &big).await;
    read(&mut pair.client, &[0x8a, 1, b'p']).await;
    pair.server.shutdown().await.unwrap();
    pair.finish().await;
}

fn close_wire(code: u16, reason: &str) -> Vec<u8> {
    let mut payload = code.to_be_bytes().to_vec();
    payload.extend_from_slice(reason.as_bytes());
    server_frame(8, &payload)
}

// A bounded in-memory fixture for the TS semantics exercised by these tests:
// synchronous invalid JSON / bad hello closes; close is idempotent; end prevents
// further socket output; queued bytes precede close. It is not a hello/auth oracle.
async fn fake_ts_control_exchange(
    mut server: DuplexStream,
    expected_input: Vec<u8>,
    queued_output: Vec<u8>,
) {
    read(&mut server, &expected_input).await;
    let mut decoder = Decoder::default();
    decoder.buffer.extend_from_slice(&expected_input);
    let mut output = queued_output;
    let mut closed = false;
    while let Some(frame) = decoder.next().unwrap() {
        match frame.opcode {
            1 if !closed => {
                let failure = match serde_json::from_slice::<serde_json::Value>(&frame.payload) {
                    Err(_) => Some((4002, "invalid_json")),
                    Ok(value) if value.get("type").and_then(|v| v.as_str()) != Some("hello") => {
                        Some((4001, "hello_required"))
                    }
                    Ok(_) => None,
                };
                if let Some((code, reason)) = failure {
                    closed = true;
                    output.extend(close_wire(code, reason));
                }
            }
            9 if !closed => output.extend(server_frame(10, &frame.payload)),
            8 => {
                if !closed {
                    output.extend(close_wire(1000, ""));
                }
                break;
            }
            _ => {}
        }
    }
    write(&mut server, &output).await;
    server.shutdown().await.unwrap();
    eof(&mut server).await;
}

async fn assert_ts_control_exchange(client_wire: Vec<u8>, queued: Vec<u8>, expected: &[u8]) {
    let Pair {
        mut client,
        server,
        task,
    } = Pair::new();
    let fake = tokio::spawn(fake_ts_control_exchange(
        server,
        client_wire.clone(),
        queued,
    ));
    // One write holds both frames, reproducing the coalesced TCP input ordering.
    write(&mut client, &client_wire).await;
    let mut output = Vec::new();
    timeout(DEADLINE, client.read_to_end(&mut output))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output, expected, "client byte order must match TS");
    timeout(DEADLINE, fake).await.unwrap().unwrap();
    timeout(DEADLINE, task).await.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn invalid_json_and_client_close_coalesced_reply_only_4002() {
    let mut wire = client_frame(0x81, 127, b"invalid json");
    wire.extend(client_frame(0x88, 126, &[]));
    assert_ts_control_exchange(wire, vec![], &close_wire(4002, "invalid_json")).await;
}

#[tokio::test]
async fn bad_hello_and_ping_coalesced_reply_close_without_pong() {
    let mut wire = client_frame(0x81, 126, br#"{"type":"request","id":"before-hello"}"#);
    wire.extend(client_frame(0x79, 127, b"must not receive a pong"));
    assert_ts_control_exchange(wire, vec![], &close_wire(4001, "hello_required")).await;
}

#[tokio::test]
async fn client_close_drains_inflight_event_response_then_ts_close_to_eof() {
    let Pair {
        mut client,
        mut server,
        task,
    } = Pair::new();
    let event = server_frame(
        1,
        br#"{"type":"event","seq":7,"event":{"type":"text","text":"in flight"}}"#,
    );
    let response = server_frame(1, br#"{"type":"response","id":"pending","result":true}"#);
    // A partial event is already in the receive pump when close arrives.
    write(&mut server, &event[..5]).await;
    silent(&mut client).await;
    let close = client_frame(0x88, 127, &[255]);
    let mut queued = event[5..].to_vec();
    queued.extend_from_slice(&response);
    let fake = tokio::spawn(fake_ts_control_exchange(server, close.clone(), queued));
    write(&mut client, &close).await;
    // Even a subsequent TCP half-close must not cancel the draining receive pump.
    client.shutdown().await.unwrap();
    let mut output = Vec::new();
    timeout(DEADLINE, client.read_to_end(&mut output))
        .await
        .unwrap()
        .unwrap();
    let mut expected = event;
    expected.extend(response);
    expected.extend(close_wire(1000, ""));
    assert_eq!(output, expected);
    timeout(DEADLINE, fake).await.unwrap().unwrap();
    timeout(DEADLINE, task).await.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn upstream_close_followed_by_client_ping_has_no_local_reply() {
    let mut pair = Pair::new();
    let close = close_wire(4002, "invalid_json");
    write(&mut pair.server, &close).await;
    read(&mut pair.client, &close).await;
    let ping = client_frame(0x89, 1, b"p");
    write(&mut pair.client, &ping).await;
    read(&mut pair.server, &ping).await;
    silent(&mut pair.client).await;
    pair.server.shutdown().await.unwrap();
    eof(&mut pair.client).await;
    pair.finish().await;
}

struct BrokenUpstreamWriter(DuplexStream);

impl AsyncRead for BrokenUpstreamWriter {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for BrokenUpstreamWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn upstream_write_failure_on_text_ping_or_close_drains_pending_ts_close() {
    for opcode in [1, 9, 8] {
        let (mut client, downstream) = tokio::io::duplex(4096);
        let (upstream, mut server) = tokio::io::duplex(4096);
        let task = tokio::spawn(run(downstream, BrokenUpstreamWriter(upstream)));
        let close = close_wire(4002, "invalid_json");
        write(&mut server, &close[..3]).await;
        write(&mut client, &client_frame(0x80 | opcode, 0, &[])).await;
        // The partial TS close must survive the injected upstream write error.
        silent(&mut client).await;
        write(&mut server, &close[3..]).await;
        server.shutdown().await.unwrap();
        let mut output = Vec::new();
        timeout(DEADLINE, client.read_to_end(&mut output))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(output, close);
        timeout(DEADLINE, task).await.unwrap().unwrap().unwrap();
    }
}

fn fixture_pair() -> Pair {
    let (client, downstream) = tokio::io::duplex(1024 * 1024);
    let (upstream, server) = tokio::io::duplex(1024 * 1024);
    Pair {
        client,
        server,
        task: tokio::spawn(run_with_metadata(
            downstream,
            upstream,
            Some(crate::dispatch::tests::fixture()),
        )),
    }
}

async fn authenticate(pair: &mut Pair) {
    let hello = client_frame(0x81, 126, br#"{"type":"hello","protocolVersion":"1.2","clientName":"web","clientVersion":"1","token":"fixture"}"#);
    write(&mut pair.client, &hello).await;
    read(&mut pair.server, &hello).await;
    let hello_ok = server_frame(1, br#"{"type":"hello_ok","protocolVersion":"1.2","serverVersion":"fixture","serverInfo":{},"interactionBinding":{"connectionId":"ts-owned","generation":1}}"#);
    write(&mut pair.server, &hello_ok).await;
    read(&mut pair.client, &hello_ok).await;
}

#[tokio::test]
async fn authenticated_native_requests_match_ts_bytes_and_never_reach_upstream() {
    let mut pair = fixture_pair();
    authenticate(&mut pair).await;
    for raw in [
        include_str!("../../tests/fixtures/metadata/supported_agents.json"),
        include_str!("../../tests/fixtures/metadata/list_projects.json"),
        include_str!("../../tests/fixtures/metadata/describe_project.json"),
    ] {
        let golden = crate::dispatch::parse(raw.as_bytes()).unwrap();
        let request = crate::metadata::text_field(&golden, "request").unwrap();
        let response = crate::metadata::text_field(&golden, "response").unwrap();
        write(
            &mut pair.client,
            &client_frame(0x81, 126, request.as_bytes()),
        )
        .await;
        read(&mut pair.client, &server_frame(1, response.as_bytes())).await;
        silent(&mut pair.server).await;
    }
    // A valid unrelated request still traverses exactly, with TS binding intact.
    let request = client_frame(
        0x81,
        127,
        br#"{"type":"request","id":"x","method":"describe_server"}"#,
    );
    write(&mut pair.client, &request).await;
    read(&mut pair.server, &request).await;
    pair.server.shutdown().await.unwrap();
    pair.finish().await;
}

#[test]
fn cjk_local_responses_match_node_without_upstream_requests() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/collation/cjk-node22.json"
    ))
    .unwrap();
    crate::project_files::tests::assert_cjk_fixture_complete(&fixture);
    let rows = fixture["rows"].as_array().unwrap();
    crate::test_process::run_cases(
        "pairing::tests::cjk_pair_worker",
        rows.iter()
            .enumerate()
            .map(|(index, row)| {
                let mut env = crate::test_process::json_env(&row["env"]);
                env.push(("PDRS_FIXTURE_ROW".into(), index.to_string()));
                (format!("cjk-pair-{index}"), env)
            })
            .collect(),
    );
}
#[tokio::test]
#[ignore = "only launched by complete CJK matrix"]
async fn cjk_pair_worker() {
    use base64::Engine;
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/collation/cjk-node22.json"
    ))
    .unwrap();
    crate::project_files::tests::assert_cjk_fixture_complete(&fixture);
    let rows = fixture["rows"].as_array().unwrap();
    let index: usize = std::env::var("PDRS_FIXTURE_ROW").unwrap().parse().unwrap();
    let row = &rows[index];
    let mut count = 0;
    for case in row["cases"].as_array().unwrap() {
        let mut metadata = crate::project_files::tests::snapshot_metadata(row);
        if let Some(group) = case["tag"].as_str().unwrap().strip_prefix("catalog-") {
            metadata
                .files
                .files
                .get_mut(&metadata.home.join("pilotdeck.yaml"))
                .unwrap()["base64"] = base64::engine::general_purpose::STANDARD
                .encode(row["yamls"][group].as_str().unwrap())
                .into();
        }
        let (client, downstream) = tokio::io::duplex(1024 * 1024);
        let (upstream, server) = tokio::io::duplex(1024 * 1024);
        let mut pair = Pair {
            client,
            server,
            task: tokio::spawn(run_with_metadata(downstream, upstream, Some(metadata))),
        };
        authenticate(&mut pair).await;
        // Non-minimal request lengths must still receive the exact local
        // Node response; the independent upstream observer stays silent.
        let request = client_frame(
            0x81,
            if count % 2 == 0 { 126 } else { 127 },
            case["request"].as_str().unwrap().as_bytes(),
        );
        write(&mut pair.client, &request).await;
        let response = server_frame(1, case["response"].as_str().unwrap().as_bytes());
        read(&mut pair.client, &response).await;
        silent(&mut pair.server).await;
        pair.server.shutdown().await.unwrap();
        pair.finish().await;
        count += 1;
    }
    assert_eq!(count, 12);
    crate::test_process::receipt(&format!("cjk-pair-{index}"));
}

#[test]
fn same_catalog_method_replies_locally_for_ascii_and_unicode_on_one_pair() {
    crate::test_process::run_cases(
        "pairing::tests::mixed_catalog_worker",
        vec![("mixed-catalog".into(), vec![("LANG".into(), "C".into())])],
    );
}
#[tokio::test]
#[ignore = "only launched by mixed catalog parent"]
async fn mixed_catalog_worker() {
    use crate::metadata::object;
    use base64::Engine;
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/collation/cjk-node22.json"
    ))
    .unwrap();
    let row = &fixture["rows"][0];
    let case = row["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["tag"] == "catalog-mixed")
        .unwrap();
    let mut metadata = crate::project_files::tests::snapshot_metadata(row);
    metadata
        .files
        .files
        .get_mut(&metadata.home.join("pilotdeck.yaml"))
        .unwrap()["base64"] = base64::engine::general_purpose::STANDARD
        .encode(row["yamls"]["mixed"].as_str().unwrap())
        .into();
    let (client, downstream) = tokio::io::duplex(1024 * 1024);
    let (upstream, server) = tokio::io::duplex(1024 * 1024);
    let mut pair = Pair {
        client,
        server,
        task: tokio::spawn(run_with_metadata(downstream, upstream, Some(metadata))),
    };
    authenticate(&mut pair).await;
    // Filter the recorded Node oracle, retaining its order and all other fields.
    let oracle = crate::dispatch::parse(case["response"].as_str().unwrap().as_bytes()).unwrap();
    let result = oracle.get("result").unwrap();
    let pilotdeck_protocol::JsValue::Array(items) = result.get("items").unwrap() else {
        panic!("items")
    };
    let items = items
        .iter()
        .filter(|item| {
            crate::metadata::text_field(item, "id")
                .is_some_and(|id| matches!(id.as_str(), "parity/z" | "parity/Z"))
        })
        .cloned()
        .collect();
    let expected = object([
        ("type", "response".into()),
        ("id", "ascii".into()),
        ("ok", pilotdeck_protocol::JsValue::Bool(true)),
        (
            "result",
            object([
                ("items", pilotdeck_protocol::JsValue::Array(items)),
                (
                    "defaultSelection",
                    result.get("defaultSelection").unwrap().clone(),
                ),
                ("router", result.get("router").unwrap().clone()),
            ]),
        ),
    ]);
    let expected = pilotdeck_protocol::stringify_json(&expected)
        .unwrap()
        .unwrap();
    let ascii = client_frame(
        0x81,
        127,
        br#"{"type":"request","id":"ascii","method":"model_catalog_list","params":{"query":"z"}}"#,
    );
    write(&mut pair.client, &ascii).await;
    read(&mut pair.client, &server_frame(1, expected.as_bytes())).await;
    silent(&mut pair.server).await;
    let unicode = client_frame(0x81, 126, case["request"].as_str().unwrap().as_bytes());
    write(&mut pair.client, &unicode).await;
    let response = server_frame(1, case["response"].as_str().unwrap().as_bytes());
    read(&mut pair.client, &response).await;
    silent(&mut pair.server).await;
    pair.server.shutdown().await.unwrap();
    pair.finish().await;
    crate::test_process::receipt("mixed-catalog");
}

#[tokio::test]
async fn local_method_before_hello_ok_is_forwarded_and_auth_state_is_per_pair() {
    let mut pair = fixture_pair();
    let request = client_frame(
        0x81,
        126,
        br#"{"type":"request","id":"x","method":"supported_agents"}"#,
    );
    write(&mut pair.client, &request).await;
    read(&mut pair.server, &request).await;
    silent(&mut pair.client).await;
    pair.server.shutdown().await.unwrap();
    pair.finish().await;
}

#[tokio::test]
async fn invalid_authed_frame_prevents_later_coalesced_local_response() {
    for payload in [
        b"invalid json".as_slice(),
        br#"{"type":"hello"}"#,
        br#"{"type":"request","id":7,"method":"supported_agents"}"#,
    ] {
        let mut pair = fixture_pair();
        authenticate(&mut pair).await;
        let mut bytes = client_frame(0x81, 126, payload);
        bytes.extend(client_frame(
            0x81,
            127,
            br#"{"type":"request","id":"later","method":"supported_agents"}"#,
        ));
        write(&mut pair.client, &bytes).await;
        read(&mut pair.server, &bytes).await;
        silent(&mut pair.client).await;
        pair.server.shutdown().await.unwrap();
        pair.finish().await;
    }
}

#[tokio::test]
async fn ts_close_suppresses_local_metadata_even_before_upstream_eof() {
    let mut pair = fixture_pair();
    authenticate(&mut pair).await;
    let close = server_frame(8, &1000u16.to_be_bytes());
    write(&mut pair.server, &close).await;
    read(&mut pair.client, &close).await;
    let request = client_frame(
        0x81,
        126,
        br#"{"type":"request","id":"x","method":"supported_agents"}"#,
    );
    write(&mut pair.client, &request).await;
    read(&mut pair.server, &request).await;
    silent(&mut pair.client).await;
    pair.server.shutdown().await.unwrap();
    pair.finish().await;
}

#[tokio::test]
async fn invalid_text_queued_during_hello_prevents_later_local_reply() {
    let mut pair = fixture_pair();
    let hello=client_frame(0x81,126,br#"{"type":"hello","protocolVersion":"1.2","clientName":"web","clientVersion":"1","token":"fixture"}"#);
    write(&mut pair.client, &hello).await;
    read(&mut pair.server, &hello).await;
    let invalid = client_frame(0x81, 126, b"invalid json");
    write(&mut pair.client, &invalid).await;
    read(&mut pair.server, &invalid).await;
    let hello_ok=server_frame(1,br#"{"type":"hello_ok","protocolVersion":"1.2","interactionBinding":{"connectionId":"ts","generation":1}}"#);
    write(&mut pair.server, &hello_ok).await;
    read(&mut pair.client, &hello_ok).await;
    let later = client_frame(
        0x81,
        126,
        br#"{"type":"request","id":"x","method":"supported_agents"}"#,
    );
    write(&mut pair.client, &later).await;
    read(&mut pair.server, &later).await;
    silent(&mut pair.client).await;
    pair.server.shutdown().await.unwrap();
    pair.finish().await;
}

fn hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| u8::from_str_radix(std::str::from_utf8(bytes).unwrap(), 16).unwrap())
        .collect()
}

#[tokio::test]
async fn mixed_batch_wire_goldens_document_immediate_gap_and_delayed_parity() {
    // TS has concurrent request handlers, not a global FIFO. Keep both cases:
    // a forwarded promise that is already fulfilled and one released later.
    // This is a diagnostic of the known immediate-forward ordering gap, NOT
    // a claim that every mixed batch matches TS (nor an accepted ruler diff).
    let cases =
        crate::dispatch::parse(include_bytes!("../../tests/fixtures/metadata/order.json")).unwrap();
    let pilotdeck_protocol::JsValue::Array(cases) = &cases else {
        panic!("cases")
    };
    assert_eq!(cases.len(), 2);
    for case in cases {
        let name = crate::metadata::text_field(case, "name").unwrap();
        let bytes = |field| hex(&crate::metadata::text_field(case, field).unwrap());
        let input = bytes("inputHex");
        let expected_ts = bytes("outputHex");
        let mut decoder = Decoder::default();
        decoder.buffer.extend_from_slice(&expected_ts);
        let first = decoder.next_server().unwrap().unwrap();
        let second = decoder.next_server().unwrap().unwrap();
        assert!(decoder.next_server().unwrap().is_none());
        let (forwarded, local) = match name.as_str() {
            "immediate-forward" => {
                assert_eq!(bytes("beforeReleaseHex"), expected_ts);
                assert!(bytes("afterReleaseHex").is_empty());
                (first, second)
            }
            "delayed-forward" => {
                assert_eq!(bytes("beforeReleaseHex"), first.wire);
                assert_eq!(bytes("afterReleaseHex"), second.wire);
                (second, first)
            }
            _ => panic!("unknown order case"),
        };
        for (frame, id) in [(&forwarded, "forwarded"), (&local, "local")] {
            assert_eq!(
                crate::metadata::text_field(&dispatch::parse(&frame.payload).unwrap(), "id")
                    .as_deref(),
                Some(id)
            );
        }
        let mut pair = fixture_pair();
        authenticate(&mut pair).await;
        write(&mut pair.client, &input).await;
        let mut requests = Decoder::default();
        requests.buffer.extend_from_slice(&input);
        let request = requests.next().unwrap().unwrap();
        read(&mut pair.server, &request.wire).await;
        if name == "delayed-forward" {
            read(&mut pair.client, &local.wire).await;
            silent(&mut pair.client).await;
        }
        write(&mut pair.server, &forwarded.wire).await;
        let actual = [local.wire, forwarded.wire.clone()].concat();
        if name == "immediate-forward" {
            read(&mut pair.client, &actual).await;
            assert_ne!(actual, expected_ts, "known mixed-batch ordering gap");
        } else {
            read(&mut pair.client, &forwarded.wire).await;
            assert_eq!(actual, expected_ts);
        }
        silent(&mut pair.client).await;
        silent(&mut pair.server).await;
        pair.server.shutdown().await.unwrap();
        pair.finish().await;
    }
}

#[tokio::test]
async fn local_and_coalesced_controls_match_actual_ts_parser_raw_wire_goldens() {
    let cases = crate::dispatch::parse(include_bytes!(
        "../../tests/fixtures/metadata/controls.json"
    ))
    .unwrap();
    let pilotdeck_protocol::JsValue::Array(cases) = &cases else {
        panic!("cases")
    };
    for case in cases {
        let name = crate::metadata::text_field(case, "name").unwrap();
        let mut pair = fixture_pair();
        authenticate(&mut pair).await;
        let input = hex(&crate::metadata::text_field(case, "inputHex").unwrap());
        let expected = hex(&crate::metadata::text_field(case, "outputHex").unwrap());
        write(&mut pair.client, &input).await;
        // The native request never reaches TS; only the following control/bad JSON does.
        let mut decoder = Decoder::default();
        decoder.buffer.extend_from_slice(&input);
        decoder.next().unwrap().unwrap();
        let forwarded = decoder.next().unwrap().unwrap();
        read(&mut pair.server, &forwarded.wire).await;
        match name.as_str() {
            "ping" => write(&mut pair.server, &server_frame(10, b"native-ping")).await,
            "close" => write(&mut pair.server, &server_frame(8, &1000u16.to_be_bytes())).await,
            "invalid" => write(&mut pair.server, &close_wire(4002, "invalid_json")).await,
            _ => panic!("unknown case"),
        }
        read(&mut pair.client, &expected).await;
        silent(&mut pair.client).await;
        silent(&mut pair.server).await;
        pair.server.shutdown().await.unwrap();
        pair.finish().await;
    }
}

struct ReleaseFileRead(std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);
impl ReleaseFileRead {
    fn open(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}
impl Drop for ReleaseFileRead {
    fn drop(&mut self) {
        self.open();
    }
}

struct SlowFiles {
    inner: crate::dispatch::tests::FixtureFiles,
    started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}
impl crate::metadata::Files for SlowFiles {
    fn entries(&self, path: &std::path::Path) -> std::io::Result<Vec<(String, bool)>> {
        if let Some(started) = self.started.lock().unwrap().take() {
            let _ = started.send(());
            let (opened, wait) = &*self.release;
            let result = wait
                .wait_timeout_while(opened.lock().unwrap(), DEADLINE, |opened| !*opened)
                .unwrap();
            assert!(
                !result.1.timed_out(),
                "source-oracle filesystem read release timeout"
            );
        }
        self.inner.entries(path)
    }
    fn read(&self, path: &std::path::Path) -> std::io::Result<Vec<u8>> {
        self.inner.read(path)
    }
    fn stat(&self, path: &std::path::Path) -> std::io::Result<crate::metadata::Stat> {
        self.inner.stat(path)
    }
}

fn slow_pair() -> (Pair, tokio::sync::oneshot::Receiver<()>, ReleaseFileRead) {
    let fixture = crate::dispatch::tests::fixture();
    let (started, observed) = tokio::sync::oneshot::channel();
    let gate = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let metadata = crate::metadata::Metadata {
        home: fixture.home,
        cwd: fixture.cwd,
        files: SlowFiles {
            inner: fixture.files,
            started: std::sync::Mutex::new(Some(started)),
            release: gate.clone(),
        },
    };
    let (client, downstream) = tokio::io::duplex(1024 * 1024);
    let (upstream, server) = tokio::io::duplex(1024 * 1024);
    (
        Pair {
            client,
            server,
            task: tokio::spawn(run_with_metadata(downstream, upstream, Some(metadata))),
        },
        observed,
        ReleaseFileRead(gate),
    )
}

async fn start_slow_list(pair: &mut Pair, observed: tokio::sync::oneshot::Receiver<()>) -> String {
    authenticate(pair).await;
    let golden = crate::dispatch::parse(include_bytes!(
        "../../tests/fixtures/metadata/list_projects.json"
    ))
    .unwrap();
    let request = crate::metadata::text_field(&golden, "request").unwrap();
    write(
        &mut pair.client,
        &client_frame(0x81, 126, request.as_bytes()),
    )
    .await;
    timeout(DEADLINE, observed)
        .await
        .expect("native read must start")
        .unwrap();
    crate::metadata::text_field(&golden, "response").unwrap()
}

#[tokio::test]
async fn pending_local_disk_read_does_not_block_ping_and_pong_precedes_response() {
    let (mut pair, observed, release) = slow_pair();
    let response = start_slow_list(&mut pair, observed).await;
    let ping = client_frame(0x89, 126, b"slow-ping");
    write(&mut pair.client, &ping).await;
    read(&mut pair.server, &ping).await;
    let pong = server_frame(10, b"slow-ping");
    write(&mut pair.server, &pong).await;
    read(&mut pair.client, &pong).await;
    release.open();
    read(&mut pair.client, &server_frame(1, response.as_bytes())).await;
    silent(&mut pair.server).await;
    pair.server.shutdown().await.unwrap();
    pair.finish().await;
}

#[tokio::test]
async fn pending_local_disk_read_does_not_block_client_close_or_reply_after_close() {
    let (mut pair, observed, release) = slow_pair();
    start_slow_list(&mut pair, observed).await;
    let close = client_frame(0x88, 126, b"");
    write(&mut pair.client, &close).await;
    read(&mut pair.server, &close).await;
    let reply = server_frame(8, &1000u16.to_be_bytes());
    write(&mut pair.server, &reply).await;
    read(&mut pair.client, &reply).await;
    release.open();
    silent(&mut pair.client).await;
    silent(&mut pair.server).await;
    pair.server.shutdown().await.unwrap();
    pair.finish().await;
}

#[tokio::test]
async fn session_selection_requests_match_node_frames_with_an_independent_upstream_observer() {
    let fixture = crate::session_model::tests::fixture();
    let mut count = 0;
    for row in fixture["cases"].as_array().unwrap().iter().filter(|row| {
        row["env"].as_object().unwrap().len() == 1
            && row["request"]
                .as_str()
                .unwrap()
                .contains("session_model_get")
    }) {
        let metadata = crate::session_model::tests::snapshot(row);
        let (client, downstream) = tokio::io::duplex(1024 * 1024);
        let (upstream, server) = tokio::io::duplex(1024 * 1024);
        let mut pair = Pair {
            client,
            server,
            task: tokio::spawn(run_with_metadata(downstream, upstream, Some(metadata))),
        };
        authenticate(&mut pair).await;
        let request = client_frame(
            0x81,
            if count % 2 == 0 { 126 } else { 127 },
            row["request"].as_str().unwrap().as_bytes(),
        );
        let expected = server_frame(1, row["response"].as_str().unwrap().as_bytes());
        write(&mut pair.client, &request).await;
        if row["forward"].as_bool() == Some(true) {
            // Host fallback must preserve the complete original request frame.
            read(&mut pair.server, &request).await;
            write(&mut pair.server, &expected).await;
        }
        read(&mut pair.client, &expected).await;
        silent(&mut pair.server).await;
        // A sentinel makes the negative observation causal: all preceding bytes
        // have been consumed when this unrelated forwarded request arrives.
        let sentinel = client_frame(
            0x81,
            127,
            br#"{"type":"request","id":"observer-fence","method":"describe_server"}"#,
        );
        write(&mut pair.client, &sentinel).await;
        read(&mut pair.server, &sentinel).await;
        pair.server.shutdown().await.unwrap();
        pair.finish().await;
        count += 1;
    }
    assert!(count > 50);
}

#[tokio::test]
async fn illformed_collation_preserves_exact_upstream_boundaries_and_node_frames() {
    let fixture = crate::session_model::tests::illformed_fixture();
    let rows = fixture["cases"].as_array().unwrap();
    assert_eq!(rows.len(), 12);
    let mut forwarded = 0;
    for row in rows {
        let metadata = crate::session_model::tests::snapshot(row);
        let (client, downstream) = tokio::io::duplex(1024 * 1024);
        let (upstream, server) = tokio::io::duplex(1024 * 1024);
        let mut pair = Pair {
            client,
            server,
            task: tokio::spawn(run_with_metadata(downstream, upstream, Some(metadata))),
        };
        authenticate(&mut pair).await;
        let request = client_frame(0x81, 127, row["request"].as_str().unwrap().as_bytes());
        let expected = server_frame(1, row["response"].as_str().unwrap().as_bytes());
        write(&mut pair.client, &request).await;
        if row["forward"].as_bool() == Some(true) {
            forwarded += 1;
            read(&mut pair.server, &request).await;
            write(&mut pair.server, &expected).await;
        }
        read(&mut pair.client, &expected).await;
        silent(&mut pair.server).await;
        let sentinel = client_frame(
            0x81,
            127,
            br#"{"type":"request","id":"illformed-fence","method":"describe_server"}"#,
        );
        write(&mut pair.client, &sentinel).await;
        read(&mut pair.server, &sentinel).await;
        pair.server.shutdown().await.unwrap();
        pair.finish().await;
    }
    assert_eq!(forwarded, 4);
}

struct UnknownIo(i32);
impl crate::metadata::Files for UnknownIo {
    fn entries(&self, _: &std::path::Path) -> std::io::Result<Vec<(String, bool)>> {
        Err(std::io::Error::from_raw_os_error(libc::ENOENT))
    }
    fn read(&self, _: &std::path::Path) -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::from_raw_os_error(libc::ENOENT))
    }
    fn stat(&self, _: &std::path::Path) -> std::io::Result<crate::metadata::Stat> {
        Err(std::io::Error::from_raw_os_error(self.0))
    }
    fn canonical(&self, _: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
        Err(std::io::Error::from_raw_os_error(self.0))
    }
}
#[tokio::test]
async fn unknown_errno_forwards_original_complete_frames_and_relays_ts_on_the_same_pair() {
    let metadata = crate::metadata::Metadata {
        home: "/fixture".into(),
        cwd: "/fixture".into(),
        files: UnknownIo(123456),
    };
    let (client, downstream) = tokio::io::duplex(1024 * 1024);
    let (upstream, server) = tokio::io::duplex(1024 * 1024);
    let mut pair = Pair {
        client,
        server,
        task: tokio::spawn(run_with_metadata(downstream, upstream, Some(metadata))),
    };
    authenticate(&mut pair).await;
    for (index, method) in [
        "session_model_get",
        "model_catalog_list",
        "project_file_read",
    ]
    .iter()
    .enumerate()
    {
        let raw = format!(
            "{{ \"type\": \"request\", \"id\": \"unknown-{index}\", \"method\": \"{method}\", \"params\": {{\"projectKey\":\"/fixture\",\"sessionKey\":\"web:unknown\",\"path\":\"file\"}}, \"extra\": [1,2] }}"
        );
        let request = client_frame(0x81, if index % 2 == 0 { 126 } else { 127 }, raw.as_bytes());
        write(&mut pair.client, &request).await;
        read(&mut pair.server, &request).await;
        silent(&mut pair.client).await;
        let response = server_frame(1, format!("{{\"type\":\"response\",\"id\":\"unknown-{index}\",\"ok\":false,\"error\":{{\"code\":\"TS_ORACLE\",\"message\":\"owned by TS\"}}}}").as_bytes());
        write(&mut pair.server, &response).await;
        read(&mut pair.client, &response).await;
        silent(&mut pair.server).await;
    }
    pair.server.shutdown().await.unwrap();
    pair.finish().await;
}

#[tokio::test]
async fn mapped_but_unmeasured_project_errnos_forward_original_frames_and_relay_ts() {
    let fixture = crate::project_files::tests::round6();
    // EIO has a verified UVException format but no project_files source oracle.
    // Exercise all ten fs boundaries in actual pairing, including caught ones.
    for (index, row) in fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .step_by(5)
        .enumerate()
    {
        let metadata = crate::project_files::tests::boundary_metadata(row, Some(libc::EIO));
        let (client, downstream) = tokio::io::duplex(1024 * 1024);
        let (upstream, server) = tokio::io::duplex(1024 * 1024);
        let mut pair = Pair {
            client,
            server,
            task: tokio::spawn(run_with_metadata(downstream, upstream, Some(metadata))),
        };
        authenticate(&mut pair).await;
        let raw = row["request"]
            .as_str()
            .unwrap()
            .replace("{\"type\"", "{ \"extra\": [1,2], \"type\"");
        let request = client_frame(0x81, if index % 2 == 0 { 126 } else { 127 }, raw.as_bytes());
        write(&mut pair.client, &request).await;
        read(&mut pair.server, &request).await;
        silent(&mut pair.client).await;
        let response = server_frame(1, br#"{ "type":"response", "id":"oracle", "ok":false, "error":{"code":"TS_ORACLE","message":"owned by TS"} }"#);
        write(&mut pair.server, &response).await;
        read(&mut pair.client, &response).await;
        pair.server.shutdown().await.unwrap();
        pair.finish().await;
    }
    // Config consumers must not inherit EIO from the shared format map either.
    let metadata = crate::metadata::Metadata {
        home: "/fixture".into(),
        cwd: "/fixture".into(),
        files: UnknownIo(libc::EIO),
    };
    let (client, downstream) = tokio::io::duplex(1024 * 1024);
    let (upstream, server) = tokio::io::duplex(1024 * 1024);
    let mut pair = Pair {
        client,
        server,
        task: tokio::spawn(run_with_metadata(downstream, upstream, Some(metadata))),
    };
    authenticate(&mut pair).await;
    for method in ["session_model_get", "model_catalog_list"] {
        let raw = format!(
            "{{ \"type\":\"request\", \"id\":\"eio\", \"method\":\"{method}\", \"params\":{{\"projectKey\":\"/fixture\",\"sessionKey\":\"web:eio\"}}, \"extra\": [1,2] }}"
        );
        let request = client_frame(0x81, 127, raw.as_bytes());
        write(&mut pair.client, &request).await;
        read(&mut pair.server, &request).await;
        silent(&mut pair.client).await;
        let response = server_frame(
            1,
            br#"{"type":"response","id":"eio","ok":true,"result":"TS passthrough"}"#,
        );
        write(&mut pair.server, &response).await;
        read(&mut pair.client, &response).await;
    }
    pair.server.shutdown().await.unwrap();
    pair.finish().await;
}

#[test]
fn catalog_errno_oracles_reply_locally_with_no_upstream_request() {
    let fixture = crate::session_model::tests::catalog_io_fixture();
    let rows = fixture["cases"].as_array().unwrap();
    assert_eq!(rows.len(), 13);
    crate::test_process::run_cases(
        "pairing::tests::catalog_errno_worker",
        rows.iter()
            .enumerate()
            .map(|(index, row)| {
                let mut env = crate::test_process::json_env(&row["env"]);
                env.push(("PDRS_FIXTURE_ROW".into(), index.to_string()));
                (row["id"].as_str().unwrap().into(), env)
            })
            .collect(),
    );
}
#[tokio::test]
#[ignore = "only launched by complete catalog matrix"]
async fn catalog_errno_worker() {
    let fixture = crate::session_model::tests::catalog_io_fixture();
    let rows = fixture["cases"].as_array().unwrap();
    assert_eq!(rows.len(), 13);
    let index: usize = std::env::var("PDRS_FIXTURE_ROW").unwrap().parse().unwrap();
    let row = &rows[index];
    let metadata = crate::session_model::tests::snapshot(row);
    let (client, downstream) = tokio::io::duplex(1024 * 1024);
    let (upstream, server) = tokio::io::duplex(1024 * 1024);
    let mut pair = Pair {
        client,
        server,
        task: tokio::spawn(run_with_metadata(downstream, upstream, Some(metadata))),
    };
    authenticate(&mut pair).await;
    let request = client_frame(0x81, 127, row["request"].as_str().unwrap().as_bytes());
    write(&mut pair.client, &request).await;
    read(
        &mut pair.client,
        &server_frame(1, row["response"].as_str().unwrap().as_bytes()),
    )
    .await;
    let sentinel = client_frame(
        0x81,
        127,
        br#"{"type":"request","id":"observer-fence","method":"describe_server"}"#,
    );
    write(&mut pair.client, &sentinel).await;
    // The very next upstream bytes must be the fence, not a catalog request.
    read(&mut pair.server, &sentinel).await;
    silent(&mut pair.server).await;
    pair.server.shutdown().await.unwrap();
    pair.finish().await;
    crate::test_process::receipt(row["id"].as_str().unwrap());
}

async fn metadata_io_pairs<F: crate::metadata::Files + Send + Sync + 'static>(
    fixture: serde_json::Value,
    make: impl Fn(&serde_json::Value) -> crate::metadata::Metadata<crate::metadata::tests::Observed<F>>,
) {
    let rows = fixture["cases"].as_array().unwrap();
    assert_eq!(rows.len(), 10);
    for row in rows {
        let metadata = make(row);
        let requests = metadata.files.requests.clone();
        let (client, downstream) = tokio::io::duplex(1024 * 1024);
        let (upstream, server) = tokio::io::duplex(1024 * 1024);
        let mut pair = Pair {
            client,
            server,
            task: tokio::spawn(run_with_metadata(downstream, upstream, Some(metadata))),
        };
        authenticate(&mut pair).await;
        let request = client_frame(0x81, 127, row["request"].as_str().unwrap().as_bytes());
        write(&mut pair.client, &request).await;
        read(
            &mut pair.client,
            &server_frame(1, row["response"].as_str().unwrap().as_bytes()),
        )
        .await;
        let fence = client_frame(
            0x81,
            127,
            br#"{"type":"request","id":"metadata-io-fence","method":"describe_server"}"#,
        );
        write(&mut pair.client, &fence).await;
        read(&mut pair.server, &fence).await;
        silent(&mut pair.server).await;
        pair.server.shutdown().await.unwrap();
        eof(&mut pair.client).await;
        pair.finish().await;
        requests.assert_target(row);
        println!("PDRS_METADATA_PAIR_OK {}", row["id"].as_str().unwrap());
    }
}
#[tokio::test]
async fn metadata_io_node_rpc_pairs_have_no_upstream_request() {
    metadata_io_pairs(
        crate::metadata::tests::fixture(),
        crate::metadata::tests::snapshot,
    )
    .await;
}
#[tokio::test]
#[ignore = "requires retained owned Node fixture before permission restoration"]
async fn metadata_io_native_retained_pairs_have_no_upstream_request() {
    metadata_io_pairs(
        crate::metadata::tests::live_fixture(),
        crate::metadata::tests::native,
    )
    .await;
}

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
        timeout(DEADLINE, self.task)
            .await
            .expect("pair must stop")
            .unwrap()
            .unwrap();
    }
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
        write(&mut pair.client, &client_frame(0x79, 127, &payload)).await;
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
    read(&mut pair.client, &[0x88, 2, 3, 232]).await;
    read(&mut pair.server, &wire).await;
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
    read(&mut pair.client, &[0x88, 2, 3, 232]).await;
    read(&mut pair.server, &close).await;
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
async fn empty_whitelist_forwards_known_unknown_invalid_json_and_utf8_payloads() {
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
async fn local_pong_never_interleaves_inside_upstream_frame_chunks() {
    let mut pair = Pair::new();
    let big = server_frame(1, &vec![b'x'; 65536]);
    // Feed only a prefix, guaranteeing a TCP-chunk boundary in the text frame.
    write(&mut pair.server, &big[..8192]).await;
    let ping = client_frame(0x89, 1, b"p");
    write(&mut pair.client, &ping).await;
    // The whole pong may precede the not-yet-complete upstream frame; it must
    // never appear after that frame's header but before its remaining payload.
    read(&mut pair.client, &[0x8a, 1, b'p']).await;
    write(&mut pair.server, &big[8192..]).await;
    read(&mut pair.client, &big).await;
    pair.server.shutdown().await.unwrap();
    pair.finish().await;
}

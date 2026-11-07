//! #1353 A8.5-pr2 wiring tests: the production faces over the real assembly —
//! the A1 admission seal on a real `ChannelServer`, the A10 flush
//! precondition, the machine's full run over a real phased WS run, and the
//! A2/A4/A5 halt shape (an expired entry drain halts the switch; the death
//! march still sends the survivors their explicit close frames). The
//! machine-order assertions themselves live in `switch_tests.rs` over
//! injected faces; these pin that the production wiring satisfies the same
//! cells over the real components.
use super::channels::{NativeChannels, NativeChannelsOptions};
use super::switch_wiring::{AdmissionGate, ProductionFaces, trigger_stop};
use crate::runtime::turn::gateway::{GatewayRequests, tests::native_ws_fixture};
use crate::switch::{Phase, StepFailure, SwitchError, SwitchFaces, SwitchMachine, SwitchTimeouts};
use crate::{native_ws::NativeWsServer, negative_assert};
use pilotdeck_protocol::{JsValue, parse_json};
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::watch,
    time::timeout,
};

const DEADLINE: Duration = Duration::from_secs(5);

/// A temporary directory this test owns; removed on drop.
struct Root(PathBuf);
impl Root {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pdrs-1353-switch-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&path).expect("#1353 switch test root");
        Self(path)
    }
    fn path(&self) -> PathBuf {
        self.0.clone()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The dispatch fixture's requests: a live `describe_server` is all a
/// connection needs; no method is ever called by these cases.
struct WiringRequests;
impl GatewayRequests for WiringRequests {
    fn describe_server(
        &self,
    ) -> crate::runtime::connection::Call<
        '_,
        Result<JsValue, crate::runtime::connection::RequestError>,
    > {
        Box::pin(async {
            Ok(crate::metadata::object([
                ("name", "switch-wiring-probe".into()),
                ("capabilities", JsValue::Array(vec![])),
            ]))
        })
    }
    fn request(
        &self,
        _: pilotdeck_protocol::JsString,
        _: JsValue,
    ) -> crate::runtime::connection::Call<
        '_,
        Result<JsValue, crate::runtime::connection::RequestError>,
    > {
        Box::pin(async {
            Err(crate::runtime::connection::RequestError::new(
                "#1353 unexpected method",
            ))
        })
    }
}

/// A real production face set over a real phased WS run and a real channel
/// assembly, with the WS budget the caller picks.
async fn wiring_harness(drain_budget: Duration) -> (SocketAddr, ProductionFaces) {
    let home = Root::new("harness");
    let host = native_ws_fixture(Arc::new(WiringRequests));
    let server = NativeWsServer::bind(
        "127.0.0.1:0".parse().expect("#1353 harness listen address"),
        host,
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#1353 harness bind");
    let address = server.local_addr().expect("#1353 harness address");
    let channels = Arc::new(NativeChannels::new(NativeChannelsOptions {
        pilot_home: home.path(),
        config: Arc::new(parse_json("{}").expect("#1353 harness config JSON")),
        weixin: crate::channel::weixin::WeixinChannelOptions::default(),
        weixin_maker: None,
        feishu: crate::channel::feishu::FeishuOptions::default(),
        log: None,
        report_status: None,
        close_host: None,
    }));
    let (trigger, trigger_rx) = watch::channel(false);
    let phased = Arc::new(server.run_phased(trigger_stop(trigger_rx), drain_budget));
    let faces = ProductionFaces::new(None, AdmissionGate::new(channels, trigger), phased);
    (address, faces)
}

fn bound(ms: u64) -> crate::switch::BoundedTimeout {
    crate::switch::BoundedTimeout::from_millis(ms).expect("#1353 harness bound")
}

/// #1353 A1: the seal is one synchronous section. `ChannelServer::close`
/// runs its eager prefix at call time — the `closing` flip and the
/// running-handle claim — and the listener stop trigger fires in the same
/// un-awaited gap, so both effects are observable the moment `seal()`
/// returns, before the close's join has resolved.
#[tokio::test]
async fn the_seal_linearizes_channel_close_and_listener_stop() {
    let home = Root::new("seal");
    let channels = Arc::new(NativeChannels::new(NativeChannelsOptions {
        pilot_home: home.path(),
        config: Arc::new(parse_json("{}").expect("#1353 seal config JSON")),
        weixin: crate::channel::weixin::WeixinChannelOptions::default(),
        weixin_maker: None,
        feishu: crate::channel::feishu::FeishuOptions::default(),
        log: None,
        report_status: None,
        close_host: None,
    }));
    let (trigger, mut trigger_rx) = watch::channel(false);
    let gate = AdmissionGate::new(channels.clone(), trigger);
    let join = gate.seal();
    // No await between the two flips: the trigger has fired …
    assert!(
        *trigger_rx.borrow_and_update(),
        "#1353 A1: the listener stop fired at the seal point"
    );
    // … and the channel server already rejects a start through its closing
    // gate — the seal's synchronous prefix, not the joined close.
    struct Idle {
        key: crate::channel::protocol::ChannelKey,
    }
    impl crate::channel::protocol::ChannelAdapter for Idle {
        fn channel_key(&self) -> &crate::channel::protocol::ChannelKey {
            &self.key
        }
        fn start(
            &self,
            _: crate::channel::protocol::ChannelStartDeps,
        ) -> crate::channel::protocol::BoxFuture<
            Result<
                Arc<dyn crate::channel::protocol::ChannelHandle>,
                crate::channel::protocol::ChannelError,
            >,
        > {
            Box::pin(async {
                Err(crate::channel::protocol::ChannelError::error(
                    "never started",
                ))
            })
        }
    }
    let rejected = channels
        .server()
        .hot_start_channel(Arc::new(Idle {
            key: crate::channel::protocol::ChannelKey::new("idle"),
        }))
        .await;
    match rejected {
        Err(error) => assert_eq!(
            error.message(),
            "channel server is closing",
            "#1353 A1: the closing gate already rejects admission"
        ),
        Ok(()) => panic!("#1353 A1: a start after the seal must reject"),
    }
    join.await.expect("#1353 A1: the close join completes");
}

/// #1353 A10: the flush face rejects until the entry drain completed — a
/// wiring bug that let flush run early cannot write half-drained state.
#[tokio::test]
async fn the_flush_face_rejects_until_the_entries_have_drained() {
    let (_, faces) = wiring_harness(Duration::from_millis(500)).await;
    // The negative-control anchor (`switch-flush-before-drain` deletes the
    // precondition gate): the receipt-emitting assertion leads, so the
    // half-drained flush cannot pass as the drained one.
    let rejected = faces.flush().await;
    negative_assert!(
        matches!(&rejected, Err(StepFailure::Step(message))
            if message == "channel flush before the entry drain completed"),
        "#1353 A10: flush rejects before the entry drain completed"
    );
    faces
        .stop_accepting()
        .await
        .expect("#1353 the empty server drains within the budget");
    faces
        .flush()
        .await
        .expect("#1353 the drained assembly flushes");
}

/// #1353 A8/A9: the machine over the production faces completes every step
/// on an idle server — the gate opens only after stop-owner and recovery,
/// and a later walk over the already-switched assembly observes the recorded
/// results instead of tearing down again.
#[tokio::test]
async fn the_machine_switches_an_idle_production_server() {
    let (_, faces) = wiring_harness(Duration::from_millis(500)).await;
    let machine = SwitchMachine::new(faces.clone(), SwitchTimeouts::uniform(bound(5_000)));
    let gate = machine.gate();
    let phase = machine.phase();
    assert!(!gate.is_open(), "#1353 the gate starts closed");
    machine.run().await.expect("#1353 every face completes");
    assert!(
        gate.is_open(),
        "#1353 A8/A9: the completed switch opens the gate"
    );
    assert_eq!(*phase.borrow(), Phase::Switched);
    faces
        .finish_after_halt()
        .await
        .expect("#1353 the post-switch walk observes the recorded finish");
}

/// A raw WS client: upgrade only, no hello. Its connection stays alive in
/// the transport loop — exactly the survivor shape the drain boundary owes
/// an explicit close frame.
struct RawClient {
    socket: TcpStream,
    decoder: crate::frame::Decoder,
}
impl RawClient {
    async fn connect(address: SocketAddr) -> Self {
        let mut socket = TcpStream::connect(address)
            .await
            .expect("#1353 survivor connect");
        socket
            .write_all(
                format!(
                    "GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\n\
                     Upgrade: websocket\r\nSec-WebSocket-Key: {}\r\n\r\n",
                    crate::test_native_ws_sample::RFC6455_SAMPLE_NONCE
                )
                .as_bytes(),
            )
            .await
            .expect("#1353 survivor upgrade write");
        let mut header = vec![];
        timeout(DEADLINE, async {
            while !header.ends_with(b"\r\n\r\n") {
                let mut byte = [0u8; 1];
                socket
                    .read_exact(&mut byte)
                    .await
                    .expect("#1353 upgrade byte");
                header.push(byte[0]);
            }
        })
        .await
        .expect("#1353 survivor upgrade deadline");
        assert!(
            String::from_utf8_lossy(&header).starts_with("HTTP/1.1 101"),
            "#1353 survivor handshake status"
        );
        Self {
            socket,
            decoder: crate::frame::Decoder::default(),
        }
    }

    /// The next server frame.
    async fn read_frame(&mut self) -> (u8, Vec<u8>) {
        timeout(DEADLINE, async {
            loop {
                if let Some(frame) = self
                    .decoder
                    .next_server()
                    .expect("#1353 survivor frame decode")
                {
                    return (frame.opcode, frame.payload);
                }
                let n = self
                    .socket
                    .read_buf(&mut self.decoder.buffer)
                    .await
                    .expect("#1353 survivor read");
                assert!(n > 0, "#1353 survivor unexpected EOF before the frame");
            }
        })
        .await
        .expect("#1353 survivor frame deadline")
    }

    /// Reads until the peer closes the TCP stream.
    async fn read_eof(&mut self) {
        timeout(DEADLINE, async {
            loop {
                let n = self
                    .socket
                    .read_buf(&mut self.decoder.buffer)
                    .await
                    .expect("#1353 survivor EOF read");
                if n == 0 {
                    return;
                }
            }
        })
        .await
        .expect("#1353 survivor EOF deadline")
    }
}

/// #1353 A2/A4/A5: a survivor past the drain budget halts the switch — the
/// machine reports the drain failure, the new-owner gate stays closed, no
/// flush runs — and the death march still terminates: the S4 half sends the
/// survivor its explicit close(1001, "server switching") before the abort.
#[tokio::test]
async fn an_expired_drain_halts_and_the_death_march_closes_the_survivor() {
    let (address, faces) = wiring_harness(Duration::from_millis(150)).await;
    let mut survivor = RawClient::connect(address).await;
    let machine = SwitchMachine::new(faces.clone(), SwitchTimeouts::uniform(bound(5_000)));
    let gate = machine.gate();
    let phase = machine.phase();
    match machine.run().await {
        Err(SwitchError::Drain(StepFailure::Step(message))) => assert!(
            message.contains("native WS drain deadline exceeded"),
            "#1353 A2/A4: the expired drain surfaces as the face's failure, got {message}"
        ),
        other => panic!("#1353 the expired drain must halt the machine, got {other:?}"),
    }
    assert_eq!(*phase.borrow(), Phase::Halted);
    assert!(
        !gate.is_open(),
        "#1353 A8: the halted switch keeps the gate closed"
    );
    // The death march: SIGTERM must still terminate this process.
    let teardown = faces.finish_after_halt().await;
    let teardown = teardown.expect_err("#1353 the death march reports the expired drain");
    assert!(
        teardown.to_string().contains("drain deadline exceeded"),
        "#1353 the death march keeps the drain failure visible, got {teardown}"
    );
    // A5: the survivor's explicit close frame, then the stream ends.
    let (opcode, payload) = survivor.read_frame().await;
    assert_eq!(opcode, 8, "#1353 A5: the survivor receives a close frame");
    assert!(
        payload.len() >= 2,
        "#1353 A5: the close frame carries a code"
    );
    assert_eq!(
        u16::from_be_bytes([payload[0], payload[1]]),
        1001,
        "#1353 A5: the close code is GOING_AWAY"
    );
    assert_eq!(
        String::from_utf8_lossy(&payload[2..]),
        "server switching",
        "#1353 A5: the close reason states the switch"
    );
    survivor.read_eof().await;
}

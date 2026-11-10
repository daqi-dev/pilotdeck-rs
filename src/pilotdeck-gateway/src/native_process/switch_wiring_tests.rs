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
use crate::runtime::turn::gateway::{
    GatewayRequests,
    tests::{native_ws_fixture, native_ws_fixture_with_cleanup},
};
use crate::switch::{Phase, StepFailure, SwitchError, SwitchFaces, SwitchMachine, SwitchTimeouts};
use crate::test_scratch::Scratch;
use crate::{native_ws::NativeWsServer, negative_assert};
use pilotdeck_protocol::{JsValue, parse_json};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{oneshot, watch},
    time::timeout,
};

const DEADLINE: Duration = Duration::from_secs(5);

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
/// assembly, with the WS budget the caller picks. The scratch home is
/// returned alongside: the channels hold its path, so the caller keeps the
/// tree alive for as long as the faces are used (dropping it early would
/// leave the flush face writing into a deleted directory).
async fn wiring_harness(drain_budget: Duration) -> (SocketAddr, ProductionFaces, Scratch) {
    wiring_harness_over(drain_budget, None).await
}

/// The harness over a channel the caller pre-seeds: `extra` is started
/// through the real admission path before the faces are wired, so the halt
/// walk's close join has a running handle to bound.
async fn wiring_harness_over(
    drain_budget: Duration,
    extra: Option<Arc<dyn crate::channel::protocol::ChannelAdapter>>,
) -> (SocketAddr, ProductionFaces, Scratch) {
    let home = Scratch::new("1353-switch-harness");
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
        pilot_home: home.path().to_owned(),
        config: Arc::new(parse_json("{}").expect("#1353 harness config JSON")),
        weixin: crate::channel::weixin::WeixinChannelOptions::default(),
        weixin_maker: None,
        feishu: crate::channel::feishu::FeishuOptions::default(),
        log: None,
        report_status: None,
        close_host: None,
    }));
    if let Some(adapter) = extra {
        channels
            .server()
            .hot_start_channel(adapter)
            .await
            .expect("#2363 the pre-seeded harness channel starts");
    }
    let (trigger, trigger_rx) = watch::channel(false);
    let phased = Arc::new(server.run_phased(trigger_stop(trigger_rx), drain_budget));
    let faces = ProductionFaces::new(None, AdmissionGate::new(channels, trigger), phased);
    (address, faces, home)
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
    let home = Scratch::new("1353-switch-seal");
    let channels = Arc::new(NativeChannels::new(NativeChannelsOptions {
        pilot_home: home.path().to_owned(),
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
    let (_, faces, _home) = wiring_harness(Duration::from_millis(500)).await;
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
    let (_, faces, _home) = wiring_harness(Duration::from_millis(500)).await;
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
    let (address, faces, _home) = wiring_harness(Duration::from_millis(150)).await;
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

/// A channel that starts fine but never answers its stop — the hung shape
/// the halt walk's close bound exists for.
struct HangingStop;
impl crate::channel::protocol::ChannelHandle for HangingStop {
    fn stop(
        &self,
        _: &str,
    ) -> crate::channel::protocol::BoxFuture<Result<(), crate::channel::protocol::ChannelError>>
    {
        Box::pin(std::future::pending())
    }
}
struct HangingChannel {
    key: crate::channel::protocol::ChannelKey,
}
impl crate::channel::protocol::ChannelAdapter for HangingChannel {
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
            Ok(Arc::new(HangingStop) as Arc<dyn crate::channel::protocol::ChannelHandle>)
        })
    }
}

/// #2363: the death march's close-timeout branch — a channel stop that never
/// returns must surface as an explicit bound failure instead of keeping a
/// halted process alive. The close join is bounded at the entries backstop
/// (the drain budget plus its slack), so the test runs on the paused clock:
/// the only timer on the test runtime is the close bound itself (the host
/// drain underneath runs on its own real-clock thread), making the bound
/// deterministic — the real-time spelling would spend the whole slack.
#[tokio::test(start_paused = true)]
async fn a_hung_channel_stop_exceeds_the_halt_close_bound() {
    let (_, faces, _home) = wiring_harness_over(
        Duration::from_millis(250),
        Some(Arc::new(HangingChannel {
            key: crate::channel::protocol::ChannelKey::new("hanging"),
        })),
    )
    .await;
    let close_bound = Duration::from_millis(250 + crate::switch::BACKSTOP_SLACK_MS);
    let started = tokio::time::Instant::now();
    let teardown = faces.finish_after_halt().await;
    let elapsed = started.elapsed();
    // The negative-control anchor (`switch-halt-close-swallowed` deletes
    // this branch's failure report): the receipt-emitting assertion leads,
    // so a swallowed hung close cannot pass as a completed walk.
    negative_assert!(
        matches!(&teardown, Err(error) if error.to_string()
            .contains("channel close: exceeded the halt close bound")),
        "#2363 the hung channel stop is reported as exceeding the halt close bound"
    );
    // The lower bound carries the same tick-conversion headroom the
    // production filter uses as its epsilon (#2363 fix-r2): the shared
    // deadline is computed from an `Instant::now()` read taken before
    // `timeout_at` registers its timer, so the firing instant can land that
    // read-to-registration skew below the nominal bound measured from this
    // test's own earlier anchor. The paused clock keeps the wait itself
    // deterministic — the deadline timer is the only one on the test runtime.
    assert!(
        elapsed
            >= close_bound.saturating_sub(crate::runtime::turn::gateway::TICK_CONVERSION_HEADROOM),
        "#2363 the walk waits out the full close bound {close_bound:?} (minus the \
         tick-conversion epsilon), got {elapsed:?}"
    );
    assert!(
        elapsed < close_bound + Duration::from_secs(2),
        "#2363 the walk still terminates within the bound's delivery slack, got {elapsed:?}"
    );
}

/// #2363 items 4/5: the wiring harness over a caller-supplied host, with the
/// phased run and the admission gate handed out — the owner-bound and
/// once-only cases drive `finish()` directly (a first call that is dropped
/// mid-run) before revisiting it through the walk. A separate construction
/// rather than a refactor of `wiring_harness_over`: this round's mandate
/// leaves the r1-delivered harness byte-for-byte as it was.
async fn wiring_harness_with_host(
    drain_budget: Duration,
    host: crate::runtime::turn::gateway::TurnGateway,
) -> (
    SocketAddr,
    Arc<crate::native_ws::PhasedRun>,
    AdmissionGate,
    Scratch,
) {
    let home = Scratch::new("1353-switch-harness");
    let server = NativeWsServer::bind(
        "127.0.0.1:0".parse().expect("#2363 harness listen address"),
        host,
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#2363 harness bind");
    let address = server.local_addr().expect("#2363 harness address");
    let channels = Arc::new(NativeChannels::new(NativeChannelsOptions {
        pilot_home: home.path().to_owned(),
        config: Arc::new(parse_json("{}").expect("#2363 harness config JSON")),
        weixin: crate::channel::weixin::WeixinChannelOptions::default(),
        weixin_maker: None,
        feishu: crate::channel::feishu::FeishuOptions::default(),
        log: None,
        report_status: None,
        close_host: None,
    }));
    let (trigger, trigger_rx) = watch::channel(false);
    let phased = Arc::new(server.run_phased(trigger_stop(trigger_rx), drain_budget));
    let gate = AdmissionGate::new(channels, trigger);
    (address, phased, gate, home)
}

/// #2363 item 4: the death march's owner bound — an S4 half that never
/// answers must surface as an explicit bound failure instead of keeping a
/// halted process alive. The hung shape is the host teardown the review
/// named: the host thread is blocked inside its own cleanup poll, so the
/// drain's own deadline cannot fire either (the thread that would fire it
/// is the blocked one) and `stopped()` never resolves. The cleanup spins
/// synchronously rather than returning `pending()` on purpose: an async
/// `pending` leaves the host thread free to expire the drain and answer
/// with an error, which is the error path, not the hang. The test runs on
/// the paused clock like item 1's: the only timer on the test runtime is
/// the owner bound itself, making the bound deterministic.
#[tokio::test(start_paused = true)]
async fn a_hung_host_teardown_exceeds_the_halt_owner_bound() {
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let blocked = release.clone();
    let host = native_ws_fixture_with_cleanup(Arc::new(WiringRequests), async move {
        while !blocked.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    });
    let (_, phased, gate, _home) = wiring_harness_with_host(Duration::from_millis(250), host).await;
    let faces = ProductionFaces::new(None, gate, phased);
    let owner_bound = Duration::from_millis(250 + crate::switch::BACKSTOP_SLACK_MS);
    let started = tokio::time::Instant::now();
    let teardown = faces.finish_after_halt().await;
    let elapsed = started.elapsed();
    // The negative-control anchor (`switch-halt-owner-swallowed` deletes this
    // branch's failure report): the receipt-emitting assertion leads, so a
    // swallowed hung owner finish cannot pass as a completed walk.
    negative_assert!(
        matches!(&teardown, Err(error) if error.to_string()
            .contains("owner finish: exceeded the halt owner bound")),
        "#2363 the hung owner finish is reported as exceeding the halt owner bound"
    );
    // The lower bound carries the same tick-conversion headroom the
    // production filter uses as its epsilon (#2363 fix-r3): the shared
    // deadline is computed from an `Instant::now()` read taken before
    // `timeout_at` registers its timer, so the firing instant can land that
    // read-to-registration skew below the nominal bound measured from this
    // test's own earlier anchor — the same `Option<Instant>`, the same
    // read-to-registration spelling the close-bound and double-hung cases
    // already carry. (Measured 3/3 runs at exactly the nominal bound in this
    // tree; the relaxation is for the three same-source assertions' one
    // convergence, not an observed shortfall.)
    assert!(
        elapsed
            >= owner_bound.saturating_sub(crate::runtime::turn::gateway::TICK_CONVERSION_HEADROOM),
        "#2363 the walk waits out the full owner bound {owner_bound:?} (minus the \
         tick-conversion epsilon), got {elapsed:?}"
    );
    assert!(
        elapsed < owner_bound + Duration::from_secs(2),
        "#2363 the walk still terminates within the bound's delivery slack, got {elapsed:?}"
    );
    // Release the blocked host thread so it can finish its drain and exit.
    release.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// #2363 item 5, error path: an S4 half whose host shutdown fails must
/// record its verdict — the revisit (the death march after the halted
/// machine) observes the recorded failure instead of re-running the whole
/// half. The failing shutdown shape is the existing bounded-cleanup one: a
/// cleanup that never completes expires the host drain's own budget.
#[tokio::test]
async fn an_erroring_owner_finish_is_recorded_for_the_revisit() {
    let host = native_ws_fixture_with_cleanup(Arc::new(WiringRequests), std::future::pending());
    let (_, phased, gate, _home) = wiring_harness_with_host(Duration::from_millis(100), host).await;
    let faces = ProductionFaces::new(None, gate, phased);
    let machine = SwitchMachine::new(faces.clone(), SwitchTimeouts::uniform(bound(5_000)));
    match machine.run().await {
        Err(SwitchError::Drain(StepFailure::Step(message))) => assert!(
            message.contains("gateway host drain deadline exceeded"),
            "#2363 the erroring host shutdown surfaces as the S4 face's failure, got {message}"
        ),
        other => {
            panic!("#2363 the erroring host shutdown must halt the machine at S4, got {other:?}")
        }
    }
    let teardown = faces.finish_after_halt().await;
    // The negative-control anchor (`native-ws-finish-error-escape` restores
    // the pre-fix early escape, so the verdict is never recorded and the
    // revisit sees the interrupted marker instead of this failure).
    negative_assert!(
        matches!(&teardown, Err(error) if error.to_string()
            .contains("gateway host drain deadline exceeded")),
        "#2363 the revisit observes the recorded host shutdown failure"
    );
}

/// #2363 item 5, drop path: a first `finish()` cancelled mid-run (the owner
/// backstop's shape) leaves the in-progress marker; the revisit must observe
/// the interrupted verdict, not re-run the half — no second round of close
/// frames, aborts or host shutdowns. The drop point is deterministic: the
/// gated cleanup signals when the host drain reaches it, which is exactly
/// when the first finish is parked inside `host.shutdown`'s await.
#[tokio::test]
async fn an_interrupted_owner_finish_is_not_rerun_by_the_revisit() {
    let (entered, mut entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release, release_rx) = tokio::sync::oneshot::channel::<()>();
    let host = native_ws_fixture_with_cleanup(Arc::new(WiringRequests), async move {
        let _ = entered.send(());
        let _ = release_rx.await;
        Ok(())
    });
    let (_, phased, gate, _home) = wiring_harness_with_host(Duration::from_secs(5), host).await;
    let faces = ProductionFaces::new(None, gate.clone(), phased.clone());
    // Fire the stop the way the machine's S2 face would and let both entry
    // surfaces settle, so the first finish below starts from the same
    // post-drain state the machine's S4 face starts from.
    let close = gate.seal();
    assert!(
        phased.drained().await,
        "#2363 the idle server drains within the budget"
    );
    close
        .await
        .expect("#2363 the harness channel close completes");
    // Drop the first finish mid-run. Boxed (not `tokio::pin!`-ed): the drop
    // below must drop the future itself — a stack pin would only drop the
    // `Pin` reference and leave the future holding the inner mutex guard,
    // parking the walk's revisit on the lock instead of the interrupted
    // verdict.
    let mut first = Box::pin(phased.finish());
    tokio::select! {
        _ = &mut entered_rx => {},
        verdict = &mut first => panic!(
            "#2363 the gated cleanup must outlive the first finish, got {verdict:?}"
        ),
    }
    drop(first);
    release.send(()).expect("#2363 the gated cleanup release");
    let teardown = faces.finish_after_halt().await;
    // The negative-control anchor (`native-ws-finish-rerun` deletes the
    // in-progress marker, so the revisit re-runs the half and the walk
    // completes with the released host's Ok instead of this verdict).
    negative_assert!(
        matches!(&teardown, Err(error) if error.to_string()
            .contains("native WS owner finish was interrupted")),
        "#2363 the interrupted owner finish is reported, not re-run"
    );
}

/// #2363 fix-r2: the wiring harness over both a caller-supplied host and a
/// caller-pre-seeded channel — the double-hung walk needs the hanging channel
/// stop (the close half's hang) and the blocked host cleanup (the S4 half's)
/// in the one assembly. A separate construction, not a refactor of
/// `wiring_harness_over` / `wiring_harness_with_host`: those two are
/// r1/fix-r1-delivered and stay byte-for-byte as they were.
async fn wiring_harness_with_host_and_channel(
    drain_budget: Duration,
    host: crate::runtime::turn::gateway::TurnGateway,
    extra: Arc<dyn crate::channel::protocol::ChannelAdapter>,
) -> (SocketAddr, ProductionFaces, Scratch) {
    let home = Scratch::new("1353-switch-harness");
    let server = NativeWsServer::bind(
        "127.0.0.1:0".parse().expect("#2363 harness listen address"),
        host,
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#2363 harness bind");
    let address = server.local_addr().expect("#2363 harness address");
    let channels = Arc::new(NativeChannels::new(NativeChannelsOptions {
        pilot_home: home.path().to_owned(),
        config: Arc::new(parse_json("{}").expect("#2363 harness config JSON")),
        weixin: crate::channel::weixin::WeixinChannelOptions::default(),
        weixin_maker: None,
        feishu: crate::channel::feishu::FeishuOptions::default(),
        log: None,
        report_status: None,
        close_host: None,
    }));
    channels
        .server()
        .hot_start_channel(extra)
        .await
        .expect("#2363 the pre-seeded harness channel starts");
    let (trigger, trigger_rx) = watch::channel(false);
    let phased = Arc::new(server.run_phased(trigger_stop(trigger_rx), drain_budget));
    let faces = ProductionFaces::new(None, AdmissionGate::new(channels, trigger), phased);
    (address, faces, home)
}

/// #2363 fix-r2 (review codex#2): the double-hung walk — a hanging channel
/// stop on the close half and a host teardown that never answers on the S4
/// half, in the one walk. Both halves spend the one owner-scale deadline, so
/// the walk still terminates within that single bound and reports both
/// bounds as its aggregated failure. An implementation that re-arms a fresh
/// full owner timeout for the S4 half after the close join's bound expired
/// runs the walk out to twice the scale — the elapsed upper bound below is
/// the deterministic kill (no negative-control row: the table stays at the
/// fix-r1 ten). Runs on the paused clock like item 4's: the only timer on
/// the test runtime is the walk's shared deadline itself (the blocked host
/// thread and its drain run on their own real-clock thread).
#[tokio::test(start_paused = true)]
async fn a_hung_channel_and_a_hung_owner_finish_share_one_owner_bound() {
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let blocked = release.clone();
    let host = native_ws_fixture_with_cleanup(Arc::new(WiringRequests), async move {
        while !blocked.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    });
    let (_, faces, _home) = wiring_harness_with_host_and_channel(
        Duration::from_millis(250),
        host,
        Arc::new(HangingChannel {
            key: crate::channel::protocol::ChannelKey::new("hanging"),
        }),
    )
    .await;
    let owner_bound = Duration::from_millis(250 + crate::switch::BACKSTOP_SLACK_MS);
    let started = tokio::time::Instant::now();
    let teardown = faces.finish_after_halt().await;
    let elapsed = started.elapsed();
    let report = teardown
        .expect_err("#2363 the double-hung walk reports the two bounds")
        .to_string();
    assert!(
        report.contains("channel close: exceeded the halt close bound"),
        "#2363 the double-hung walk reports the close bound, got {report}"
    );
    assert!(
        report.contains("owner finish: exceeded the halt owner bound"),
        "#2363 the double-hung walk reports the owner bound, got {report}"
    );
    assert_eq!(
        report,
        "channel close: exceeded the halt close bound; \
         owner finish: exceeded the halt owner bound",
        "#2363 the two bounds are the walk's whole aggregated failure"
    );
    // The one-owner-scale band, same epsilon spelling as the close-bound
    // test's: the shared deadline is computed before the timer registration,
    // so the firing instant can land the read-to-registration skew below the
    // nominal bound; the upper bound is the re-armed-timeout kill.
    assert!(
        elapsed
            >= owner_bound.saturating_sub(crate::runtime::turn::gateway::TICK_CONVERSION_HEADROOM),
        "#2363 the double-hung walk waits out the one owner bound {owner_bound:?} \
         (minus the tick-conversion epsilon), got {elapsed:?}"
    );
    assert!(
        elapsed < owner_bound + Duration::from_secs(2),
        "#2363 the double-hung walk terminates within the one owner bound's \
         delivery slack, got {elapsed:?}"
    );
    // Release the blocked host thread so it can finish its drain and exit.
    release.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// #2363 fix-r3 (review DS concurrency#1 + boundary#1): the waiver band's
/// regression case — a `Duration::MAX` drain budget must reach the walk's
/// waiver branch and walk to completion instead of panicking at the bound
/// assembly. `close_bound` is built with `saturating_add` for exactly this
/// tier (the saturated sum overflows `Instant::checked_add`, so the deadline
/// is `None` and both halves keep their unbounded await), and this test is
/// the kill for reverting that to the bare `+`: `Duration` addition panics on
/// overflow in release builds too, right at the bound assembly, before the
/// waiver branch is ever reached — and no other case in this module drives
/// `finish_after_halt` on a budget that does not fit the clock. The idle
/// assembly settles at once through both unbounded halves (the empty drain,
/// the instantly-completing host cleanup), so the guard deadline below is
/// only a hang fence, never the bound under test.
#[tokio::test]
async fn a_maximal_drain_budget_walks_to_completion() {
    let (_, faces, _home) = wiring_harness(Duration::MAX).await;
    let teardown = timeout(DEADLINE, faces.finish_after_halt()).await;
    let teardown = teardown.expect("#2363 the waiver-band walk settles within the guard");
    teardown.expect("#2363 a maximal drain budget walks the idle assembly to completion");
}

/// #2351: a real runner panic must fail S2 and leave the post-halt walk able
/// to reach the owner's finish. The stop future injects the panic on the
/// actual runner task, after the production admission gate has sealed.
#[tokio::test]
async fn a_panicking_runner_halts_and_the_death_march_returns() {
    let home = Scratch::new("2351-panicking-runner");
    let host = native_ws_fixture(Arc::new(WiringRequests));
    let server = NativeWsServer::bind(
        "127.0.0.1:0".parse().expect("#2351 walk listen address"),
        host,
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#2351 walk binds a real WS server");
    let channels = Arc::new(NativeChannels::new(NativeChannelsOptions {
        pilot_home: home.path().to_owned(),
        config: Arc::new(parse_json("{}").expect("#2351 walk config JSON")),
        weixin: crate::channel::weixin::WeixinChannelOptions::default(),
        weixin_maker: None,
        feishu: crate::channel::feishu::FeishuOptions::default(),
        log: None,
        report_status: None,
        close_host: None,
    }));
    let (trigger, trigger_rx) = watch::channel(false);
    let phased = Arc::new(server.run_phased(
        async move {
            trigger_stop(trigger_rx).await;
            panic!("#2351 injected production runner panic");
        },
        Duration::from_millis(100),
    ));
    let faces = ProductionFaces::new(None, AdmissionGate::new(channels, trigger), phased);
    let limit = crate::switch::BoundedTimeout::from_millis(100)
        .expect("#2351 walk machine budget is valid");
    let machine = SwitchMachine::new(faces.clone(), SwitchTimeouts::uniform(limit));
    let phase = machine.phase();
    let gate = machine.gate();
    let result = timeout(DEADLINE, machine.run())
        .await
        .expect("#2351 runner panic cannot hang the machine");
    negative_assert!(
        matches!(result, Err(SwitchError::Drain(_))),
        "#2351 runner panic is a drain failure, not a face timeout"
    );
    negative_assert!(
        *phase.borrow() == Phase::Halted && !gate.is_open(),
        "#2351 runner panic halts before opening the new owner gate"
    );
    for _ in 0..2 {
        negative_assert!(
            timeout(DEADLINE, faces.finish_after_halt())
                .await
                .expect("#2351 post-halt walk returns after runner panic")
                .is_err(),
            "#2351 post-halt walk preserves the failed drain verdict"
        );
    }
}

/// #2557: the final-cancellation cell — the walk itself is `finish()`'s
/// first and only caller, the close half spends the whole shared deadline
/// (the hanging channel stop), and the first finish's single poll parks
/// inside the close-notice window (the partial-head survivor holds its
/// join), so the already-elapsed deadline cuts it there, before the request
/// below the window ever fires. The walk's timeout branch must re-issue the
/// host teardown request itself: the process exits when the walk returns
/// and no revisit is coming, so without the re-issue the only request left
/// is the unawaited `StopHost` drop one.
///
/// Real clock: the close half really burns the entries backstop (drain
/// budget + `BACKSTOP_SLACK_MS`) off the shared deadline, and the host
/// thread's cross-thread `entered` wake must race no virtual clock. The
/// construction is inline (like the #2351 walk's) because the test holds
/// its own `Arc<PhasedRun>` for the revisit probe.
#[tokio::test]
async fn the_final_cancellation_cell_still_requests_the_host_teardown() {
    let (entered, entered_rx) = oneshot::channel::<()>();
    let (release, release_rx) = oneshot::channel::<()>();
    let host = native_ws_fixture_with_cleanup(Arc::new(WiringRequests), async move {
        let _ = entered.send(());
        let _ = release_rx.await;
        Ok(())
    });
    let home = Scratch::new("2557-final-cancellation");
    let server = NativeWsServer::bind(
        "127.0.0.1:0".parse().expect("#2557 cell listen address"),
        host,
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#2557 cell binds a real WS server");
    let address = server.local_addr().expect("#2557 cell address");
    let channels = Arc::new(NativeChannels::new(NativeChannelsOptions {
        pilot_home: home.path().to_owned(),
        config: Arc::new(parse_json("{}").expect("#2557 cell config JSON")),
        weixin: crate::channel::weixin::WeixinChannelOptions::default(),
        weixin_maker: None,
        feishu: crate::channel::feishu::FeishuOptions::default(),
        log: None,
        report_status: None,
        close_host: None,
    }));
    channels
        .server()
        .hot_start_channel(Arc::new(HangingChannel {
            key: crate::channel::protocol::ChannelKey::new("hanging"),
        }))
        .await
        .expect("#2557 cell hanging channel starts");
    let (trigger, trigger_rx) = watch::channel(false);
    let phased = Arc::new(server.run_phased(trigger_stop(trigger_rx), Duration::from_millis(150)));
    let faces = ProductionFaces::new(None, AdmissionGate::new(channels, trigger), phased.clone());
    let mut survivor = TcpStream::connect(address)
        .await
        .expect("#2557 survivor connect");
    survivor
        .write_all(b"GET /ws HTTP/1.1\r\n")
        .await
        .expect("#2557 survivor partial head write");
    // The accept-loop barrier (the #749 shape): a completed handshake
    // behind the survivor proves the survivor's task is already in the
    // connections set, so the entry drain below is really held open by it.
    drop(RawClient::connect(address).await);
    // The walk: the entry drain expires on the survivor, the hanging
    // channel stop burns the whole shared deadline, and the first finish
    // is cut inside the close-notice window by the same elapsed deadline.
    let teardown = faces.finish_after_halt().await;
    // The close half also reported, pinning that the deadline really was
    // spent by the close join rather than left for the finish to run in.
    let teardown = teardown.expect_err("#2557 the double-cut walk reports failures");
    assert!(
        teardown
            .to_string()
            .contains("channel close: exceeded the halt close bound"),
        "#2557 the close half burned the shared deadline, got {teardown}"
    );
    // The owner-finish string is the cell itself: the first finish was cut
    // mid-body by the deadline, it did not complete with a verdict of its
    // own (a host-shutdown drain failure would read differently).
    assert!(
        teardown
            .to_string()
            .contains("owner finish: exceeded the halt owner bound"),
        "#2557 the finish half was cut by the shared deadline, got {teardown}"
    );
    // The re-issue: `faces` and the local `phased` clone are both held
    // here, so `StopHost`'s drop request has not fired and cannot be the
    // source — the only request this cell can produce is the walk's own
    // timeout-branch re-issue. The negative-control anchor
    // (`switch-halt-owner-skip-reissue` skips it): with the re-issue gone,
    // no request goes out and the signal never comes.
    negative_assert!(
        matches!(timeout(DEADLINE, entered_rx).await, Ok(Ok(()))),
        "#2557 the final walk re-issues the host teardown request"
    );
    // The revisit (a caller after the walk — the test itself) observes the
    // interrupted verdict, pinning that the cut finish had started: its
    // single poll set the in-progress marker before the window join.
    let verdict = timeout(DEADLINE, phased.finish())
        .await
        .expect("#2557 the revisit is bounded");
    negative_assert!(
        matches!(&verdict, Err(error) if error.to_string()
            .contains("native WS owner finish was interrupted")),
        "#2557 the revisit observes the interrupted verdict"
    );
    release.send(()).expect("#2557 cleanup release");
    drop(survivor);
}

//! #1395: distinguish a synthetic zero-room wake race from the production
//! hyper caller. The ignored characterization deliberately records the old
//! private-stream behavior; it is not a required contract for a future fix.
use super::*;
use std::sync::{Weak, atomic::AtomicUsize, mpsc};

const BOUND: Duration = Duration::from_secs(5);
static PROBES: Mutex<Vec<Weak<Probe>>> = Mutex::new(Vec::new());

struct Probe {
    target: Arc<AtomicBool>,
    capacities: Mutex<Vec<usize>>,
    parks: AtomicUsize,
    pending_bodies: AtomicUsize,
    hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
impl Probe {
    fn new(target: Arc<AtomicBool>) -> Arc<Self> {
        let probe = Arc::new(Self {
            target,
            capacities: Mutex::new(Vec::new()),
            parks: AtomicUsize::new(0),
            pending_bodies: AtomicUsize::new(0),
            hook: Mutex::new(None),
        });
        let mut probes = PROBES.lock().expect("#1395 probe registry lock");
        probes.retain(|probe| probe.strong_count() != 0);
        probes.push(Arc::downgrade(&probe));
        probe
    }
}
fn target_probe(timer: &KeepAliveTimer) -> Option<Arc<Probe>> {
    PROBES
        .lock()
        .expect("#1395 probe lookup lock")
        .iter()
        .filter_map(Weak::upgrade)
        .find(|probe| Arc::ptr_eq(&probe.target, &timer.shutdown_started))
}
pub(super) fn on_read(timer: &KeepAliveTimer, capacity: usize) {
    if let Some(probe) = target_probe(timer) {
        probe
            .capacities
            .lock()
            .expect("#1395 read capacities lock")
            .push(capacity);
    }
}
pub(super) fn before_park(timer: &KeepAliveTimer) {
    if let Some(probe) = target_probe(timer) {
        probe.parks.fetch_add(1, Ordering::SeqCst);
        let hook = probe.hook.lock().expect("#1395 park hook lock").take();
        if let Some(hook) = hook {
            hook();
        }
    }
}

pub(super) fn on_pending(timer: &KeepAliveTimer) {
    if let Some(probe) = target_probe(timer)
        && timer.phase() == TimeoutPhase::Request
        && timer.nearest_deadline().is_none()
    {
        probe.pending_bodies.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct WakeCount(AtomicUsize);
impl std::task::Wake for WakeCount {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Real bytes fill both park states. The one-shot hook fires after the loop
/// has broken but before the waker is stored. A separate blocking task calls
/// begin_shutdown while poll_read is paused. Every handshake has a deadline.
#[tokio::test(start_paused = true)]
#[ignore = "#1395 diagnostic of an unreachable production interleaving"]
async fn issue_1395_synthetic_zero_room_shutdown_window() {
    for eof in [false, true] {
        for deadline in [false, true] {
            let signal = Arc::new(AtomicBool::new(false));
            let probe = Probe::new(signal.clone());
            let timer = Arc::new(KeepAliveTimer::new(
                Duration::from_secs(60),
                Duration::from_secs(60),
                signal,
            ));
            // A foreign connection must neither record nor consume our hook.
            let foreign = KeepAliveTimer::new(
                Duration::from_secs(60),
                Duration::from_secs(60),
                Arc::new(AtomicBool::new(false)),
            );
            before_park(&foreign);
            on_read(&foreign, 0);
            assert_eq!(
                probe.parks.load(Ordering::SeqCst),
                0,
                "#1395 foreign park must not hit target"
            );
            assert!(
                probe
                    .capacities
                    .lock()
                    .expect("#1395 foreign capacities lock")
                    .is_empty(),
                "#1395 foreign read must not hit target"
            );
            let (mut peer, io) = tokio::io::duplex(READ_AHEAD_BYTES * 2);
            let mut stream = TimeoutStream::new(io, timer.clone(), DEFAULT_MAX_HEADER_BYTES);
            peer.write_all(b"POST /x HTTP/1.1\r\nHost: h\r\nContent-Length: 9999999\r\n\r\n")
                .await
                .expect("#1395 framed head write");
            let size = if eof { 64 } else { READ_AHEAD_BYTES + 512 };
            peer.write_all(&vec![b'x'; size])
                .await
                .expect("#1395 body write");
            if eof {
                peer.shutdown().await.expect("#1395 peer half-close");
            }
            let mut first = [0];
            stream
                .read_exact(&mut first)
                .await
                .expect("#1395 initial byte read");
            assert_eq!(
                first,
                [b'P'],
                "#1395 initial read reached the target stream"
            );
            assert_eq!(
                stream.ahead_eof, eof,
                "#1395 real socket reaches requested EOF state"
            );
            assert!(
                if eof {
                    !stream.ahead.is_empty()
                } else {
                    stream.ahead.len() == READ_AHEAD_BYTES
                },
                "#1395 real socket reaches requested park state"
            );
            if !deadline {
                timer.request_started();
            }
            assert_eq!(
                timer.nearest_deadline().is_some(),
                deadline,
                "#1395 deadline precondition"
            );
            assert!(
                timer
                    .waker
                    .lock()
                    .expect("#1395 pre-park waker lock")
                    .is_none(),
                "#1395 no old waker may rescue the race"
            );
            let (entered_tx, entered_rx) = mpsc::channel();
            let (finished_tx, finished_rx) = mpsc::channel();
            let shutdown_timer = timer.clone();
            let worker = tokio::task::spawn_blocking(move || {
                if entered_rx.recv_timeout(BOUND).is_err() {
                    return false;
                }
                shutdown_timer.begin_shutdown();
                finished_tx.send(()).is_ok()
            });
            *probe.hook.lock().expect("#1395 install park hook") = Some(Box::new(move || {
                entered_tx
                    .send(())
                    .expect("#1395 target hook releases shutdown task");
                finished_rx
                    .recv_timeout(BOUND)
                    .expect("#1395 shutdown completes before waker registration");
            }));
            let wakes = Arc::new(WakeCount::default());
            let waker = Waker::from(wakes.clone());
            let mut cx = Context::from_waker(&waker);
            let mut empty = [];
            let result = Pin::new(&mut stream).poll_read(&mut cx, &mut ReadBuf::new(&mut empty));
            let completed = worker.await.expect("#1395 shutdown worker joins");
            assert_eq!(
                probe.parks.load(Ordering::SeqCst),
                1,
                "#1395 A target park hook must be reached exactly once"
            );
            assert!(
                completed,
                "#1395 B shutdown task must complete inside target hook"
            );
            assert!(
                timer
                    .state
                    .lock()
                    .expect("#1395 shutdown state lock")
                    .shutting_down,
                "#1395 C begin_shutdown must change target state"
            );
            assert!(result.is_pending(), "#1395 synthetic zero-room poll parks");
            assert_eq!(
                wakes.0.load(Ordering::SeqCst),
                0,
                "#1395 diagnostic: shutdown wake before registration is lost"
            );
            assert!(
                timer
                    .waker
                    .lock()
                    .expect("#1395 registered waker lock")
                    .is_some(),
                "#1395 parked caller eventually registers its waker"
            );
            tokio::time::advance(Duration::from_secs(61)).await;
            tokio::task::yield_now().await;
            let timeout_wakes = wakes.0.load(Ordering::SeqCst);
            assert_eq!(
                timeout_wakes > 0,
                deadline,
                "#1395 only an armed deadline rescues the synthetic park"
            );
            println!(
                "#1395 eof={eof} deadline={deadline} hook=1 shutdown=true poll=Pending immediate_wakes=0 deadline_wakes={timeout_wakes}"
            );
            let before = wakes.0.load(Ordering::SeqCst);
            timer.begin_shutdown();
            assert_eq!(
                wakes.0.load(Ordering::SeqCst),
                before + 1,
                "#1395 shutdown after registration wakes the caller"
            );
        }
    }
}

async fn server(
    limits: ServeLimits,
) -> (
    GatewayUserDialogStoreHttpServer,
    Arc<Probe>,
    std::path::PathBuf,
) {
    let directory = std::env::temp_dir().join(format!("pdrs-1395-{}", uuid::Uuid::new_v4()));
    let store =
        FileGatewayUserDialogStore::new(crate::user_dialog::StoreOptions::new(directory.clone()))
            .expect("#1395 create isolated store");
    let server = start_gateway_user_dialog_store_http_server_with_limits(
        Arc::new(store),
        "issue-1395-token",
        Some("127.0.0.1"),
        Some(0),
        limits,
    )
    .await
    .expect("#1395 start real HTTP store server");
    let probe = Probe::new(server.shutdown_started.clone());
    (server, probe, directory)
}
async fn connect(server: &GatewayUserDialogStoreHttpServer) -> TcpStream {
    let port = Url::parse(server.url())
        .expect("#1395 parse server URL")
        .port()
        .expect("#1395 server URL has port");
    tokio::time::timeout(BOUND, TcpStream::connect(("127.0.0.1", port)))
        .await
        .expect("#1395 connection deadline")
        .expect("#1395 connect real socket")
}
async fn observed_read(probe: &Probe) {
    tokio::time::timeout(BOUND, async {
        loop {
            if !probe
                .capacities
                .lock()
                .expect("#1395 wait capacities lock")
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#1395 real server must reach target read observer");
}
fn assert_production_reads(probe: &Probe) {
    let capacities = probe
        .capacities
        .lock()
        .expect("#1395 production capacities lock");
    assert!(
        !capacities.is_empty(),
        "#1395 production read observer must hit target"
    );
    assert!(
        capacities.iter().all(|capacity| *capacity >= 8192),
        "#1395 hyper must always provide at least 8192 bytes of read capacity: {capacities:?}"
    );
    assert_eq!(
        probe.parks.load(Ordering::SeqCst),
        0,
        "#1395 production must never enter the zero-room park"
    );
    println!("#1395 production capacities={capacities:?}; zero_room_parks=0");
}

/// Exercises the actual server's builder and adapter, including its normal
/// buffer limit: request head, body beyond the read-ahead cap, pipelined
/// next request and EOF. A dependency/configuration change permitting empty
/// reads must fail here before #1353 switches the default route.
#[tokio::test]
async fn issue_1395_production_hyper_reads_have_capacity() {
    let (server, probe, directory) = server(ServeLimits::default()).await;
    let mut peer = connect(&server).await;
    observed_read(&probe).await;
    assert_production_reads(&probe);
    let mut body = br#"{"version":1,"key":{"projectRoot":"/workspace/project","pilotHome":"/workspace/home","sessionId":"sdk:1395"}}"#.to_vec();
    body.resize(READ_AHEAD_BYTES * 2, b' ');
    let head = format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: h\r\nAuthorization: Bearer issue-1395-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    tokio::time::timeout(BOUND, async {
        peer.write_all(head.as_bytes())
            .await
            .expect("#1395 fragmented head write");
        tokio::task::yield_now().await;
        peer.write_all(b"\r\n")
            .await
            .expect("#1395 complete head write");
        peer.write_all(&body).await.expect("#1395 large body write");
        peer.write_all(
            b"POST /missing HTTP/1.1\r\nHost: h\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await
        .expect("#1395 pipelined request write");
        let mut response = Vec::new();
        peer.read_to_end(&mut response)
            .await
            .expect("#1395 read pipelined responses through EOF");
        let response = String::from_utf8(response).expect("#1395 response is UTF-8");
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "#1395 real store must consume the large body: {response}"
        );
        assert!(
            response.contains("HTTP/1.1 404"),
            "#1395 pipelined request must reach the handler: {response}"
        );
    })
    .await
    .expect("#1395 production pipeline terminates within bound");
    tokio::time::timeout(BOUND, server.close())
        .await
        .expect("#1395 production server close deadline")
        .expect("#1395 production server closes successfully");
    assert_production_reads(&probe);
    let _ = std::fs::remove_dir_all(directory);
}

/// A predecessor which never supplies the rest of its body, and a predecessor
/// cancelled by its peer, both leave server.close bounded. No CPU load or
/// scheduler luck is used to create either pending state.
#[tokio::test]
async fn issue_1395_shutdown_with_pending_or_cancelled_body_is_bounded() {
    for cancel_peer in [false, true] {
        let limits = ServeLimits {
            request_timeout: Duration::from_millis(100),
            ..ServeLimits::default()
        };
        let (server, probe, directory) = server(limits).await;
        let mut peer = connect(&server).await;
        let request = format!(
            "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: h\r\nAuthorization: Bearer issue-1395-token\r\nContent-Length: 999\r\n\r\n{{"
        );
        peer.write_all(request.as_bytes())
            .await
            .expect("#1395 unfinished body write");
        tokio::time::timeout(BOUND, async {
            while probe.pending_bodies.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("#1395 target body must be Pending with no connection deadline before shutdown");
        if cancel_peer {
            peer.shutdown()
                .await
                .expect("#1395 cancel body with peer FIN");
        }
        tokio::time::timeout(BOUND, server.close())
            .await
            .expect("#1395 unfinished or cancelled body cannot hang shutdown")
            .expect("#1395 unfinished or cancelled body closes successfully");
        assert_production_reads(&probe);
        println!("#1395 pending_body cancel_peer={cancel_peer} close=completed");
        let _ = std::fs::remove_dir_all(directory);
    }
}

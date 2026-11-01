//! #983: replay coordinator-owned TS captures through a real local WebSocket.
//! The large-frame fixture is a live TS transport capture; other supplemental
//! transport cases below remain source-inferred expectations.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Write},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::TcpListener,
    time::{Instant, timeout},
};
use tokio_tungstenite::{
    accept_async,
    tungstenite::{
        Message,
        protocol::{CloseFrame, frame::coding::CloseCode},
    },
};

const ORACLE: &str = include_str!("fixtures/sdk-sessions-list/oracle.json");
const LIMITS_ORACLE: &str = include_str!("fixtures/sdk-transport-limits/oracle.json");
const ADDRESS: &str = "127.0.0.1:28999";
const TOKEN: &str = "d1-fixture-not-a-secret";
static PORT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[path = "common/sdk_exit.rs"]
mod sdk_exit;
use sdk_exit::{
    BOUND, Runtime, collect, collect_bounded, snapshot, spawn, verify,
    verify_no_pending_connections,
};

#[test]
fn fixture_stdin_writes_all_bytes() {
    sdk_exit::fixture_stdin_writes_all_bytes();
}

#[test]
fn fixture_stdin_other_errors_still_fail() {
    sdk_exit::fixture_stdin_other_errors_still_fail();
}

#[tokio::test]
async fn fixture_stdin_broken_pipe_preserves_output() {
    sdk_exit::fixture_stdin_broken_pipe_preserves_output().await;
}

#[tokio::test]
async fn fixture_stdin_writer_yields_before_child_reads() {
    sdk_exit::fixture_stdin_writer_yields_before_child_reads().await;
}

#[tokio::test]
async fn fixture_stdin_deferred_until_requested() {
    sdk_exit::fixture_stdin_deferred_until_requested().await;
}

#[test]
fn fixture_stdin_cancel_reaps_writer() {
    sdk_exit::fixture_stdin_cancel_reaps_writer();
}

#[tokio::test]
async fn fixture_stdin_registered_after_exit_is_joined() {
    sdk_exit::fixture_stdin_registered_after_exit_is_joined().await;
}

async fn replay(listener: &TcpListener, case: &Value, pid: u32) {
    let (stream, _) = listener.accept().await.expect("#983 owned mock accept");
    let mut socket = accept_async(stream).await.expect("#983 WS upgrade");
    let mut id = String::new();
    let mut wire_checks = Vec::new();
    for row in case["wire"].as_array().expect("#983 wire rows") {
        if row["kind"] == "receive" {
            let message = socket
                .next()
                .await
                .expect("#983 wire frame exists")
                .expect("#983 readable wire frame");
            let raw = message.to_text().expect("#983 text wire frame");
            let mut frame: Value = serde_json::from_str(raw).expect("#983 JSON wire frame");
            if frame["type"] == "request" {
                id = frame["id"]
                    .as_str()
                    .expect("#983 request id string")
                    .to_owned();
                verify(
                    uuid::Uuid::parse_str(&id).is_ok(),
                    &format!(
                        "#983 request UUID {}",
                        case["input"]["id"].as_str().expect("#983 cell id")
                    ),
                );
                frame["id"] = "<request>".into();
                // P04's RawValue entry must retain P02's Value-encoded bytes,
                // including the legacy sorted envelope keys (not a TS oracle).
                let legacy =
                    serde_json::to_string(&frame).expect("#983 legacy request serialization");
                verify(
                    raw.replace(&id, "<request>") == legacy,
                    &format!(
                        "#983 legacy request encoding {}",
                        case["input"]["id"].as_str().expect("#983 cell id")
                    ),
                );
            }
            wire_checks.push((
                frame == row["frame"],
                format!(
                    "#983 hello and request wire {}",
                    case["input"]["id"].as_str().expect("#983 cell id")
                ),
            ));
        } else {
            let raw = row["raw"]
                .as_str()
                .expect("#983 original wire JSON")
                .replacen("\"id\":\"<request>\"", &format!("\"id\":\"{id}\""), 1);
            socket
                .send(Message::Text(raw.into()))
                .await
                .expect("#983 mock response");
        }
    }
    // Finish the deterministic reply barrier before reporting a wire mismatch.
    // A bad hello must not strand the child waiting for hello_ok (or its RPC).
    for (matches, assertion) in wire_checks {
        verify(matches, &assertion);
    }
    let signal = case["input"]["signal"].as_str();
    let rejected = case["input"]["acceptToken"] != case["input"]["env"]["PILOTDECK_SDK_AUTH_TOKEN"]
        && case["input"]["axis"] == "gateway-auth-rejected";
    if rejected {
        socket
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::Library(4003),
                reason: "auth_failed".into(),
            })))
            .await
            .expect("#983 auth close");
    }
    if let Some(signal) = signal {
        let status = Command::new("/bin/kill")
            .args(["-s", signal, &pid.to_string()])
            .status()
            .expect("#983 send signal after request");
        assert!(status.success(), "#983 signal delivered");
    }
    // Success/error gets a close frame; signal termination releases TCP without
    // a handshake. In both cases consume to EOF to prove no retained connection.
    let mut closed = false;
    loop {
        match socket.next().await {
            Some(Ok(Message::Close(frame))) => {
                closed = true;
                if rejected {
                    println!("#983 auth close response: {frame:?}");
                }
                if !rejected {
                    verify(
                        frame.is_none(),
                        &format!(
                            "#983 empty normal close frame {}",
                            case["input"]["id"].as_str().expect("#983 cell id")
                        ),
                    );
                }
                let _ = socket.flush().await;
            }
            None | Some(Err(_)) => break,
            Some(Ok(_)) => panic!("#983 unexpected frame after terminal RPC"),
        }
    }
    verify(
        signal.is_some() || closed,
        &format!(
            "#983 terminal connection closed {}",
            case["input"]["id"].as_str().expect("#983 cell id")
        ),
    );
}

// Same directory lease as scripts/cli-parity/common.py: a stable flock guard,
// mkdir and pid/timestamp/token ownership. A Tokio mutex alone cannot serialize
// nextest's separate processes. Never reclaim a predecessor's directory.
struct ParityLock {
    path: PathBuf,
    identity: (u64, u64),
    owner: Vec<u8>,
}
impl ParityLock {
    async fn acquire() -> Self {
        Self::acquire_at(Path::new("/tmp/pdrs-parity.lock"), Duration::from_secs(180))
            .await
            .expect("#983 D0 parity lease acquisition failed (bounded wait; no reclaim)")
    }

    async fn acquire_at(path: &Path, bound: Duration) -> io::Result<Self> {
        let deadline = Instant::now() + bound;
        let mut waiting = false;
        loop {
            // A delayed scheduler wake must not acquire a newly freed lease
            // after the caller's deadline has already expired.
            if waiting && Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("#983 parity lease deadline: {}", path.display()),
                ));
            }
            match Self::try_acquire(path) {
                Ok(lease) => return Ok(lease),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::AlreadyExists | io::ErrorKind::WouldBlock
                    ) =>
                {
                    if !waiting {
                        eprintln!("#983 waiting for parity lease: {}", path.display());
                        waiting = true;
                    }
                    tokio::time::sleep_until(
                        deadline.min(Instant::now() + Duration::from_millis(100)),
                    )
                    .await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn guard(path: &Path) -> io::Result<fs::File> {
        let mut name = path.as_os_str().to_owned();
        name.push(".guard");
        let guard = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(Path::new(&name))?;
        let metadata = guard.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(io::Error::other("#983 unsafe parity guard"));
        }
        // std uses flock on Unix; the interoperability probe checks this
        // against the D0 Python owner's fcntl.flock, not just another Rust call.
        guard.try_lock().map_err(io::Error::from)?;
        Ok(guard)
    }

    fn try_acquire(path: &Path) -> io::Result<Self> {
        let _guard = Self::guard(path)?;
        fs::DirBuilder::new().mode(0o700).create(path)?;
        let metadata = fs::symlink_metadata(path)?;
        let owner = serde_json::to_vec(&json!({
            "pid": std::process::id(),
            "timestamp": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(io::Error::other)?.as_secs_f64(),
            "token": uuid::Uuid::new_v4().simple().to_string(),
        }))?;
        // Publish complete bytes atomically: readers may inspect owner.json as
        // soon as it exists. On an IO error retain the directory, fail closed.
        let temporary = path.join("owner.tmp");
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?
            .write_all(&owner)?;
        fs::rename(temporary, path.join("owner.json"))?;
        Ok(Self {
            path: path.to_owned(),
            identity: (metadata.dev(), metadata.ino()),
            owner,
        })
    }

    fn release_guard(path: &Path, mut on_wait: impl FnMut()) -> io::Result<fs::File> {
        // A waiter may briefly hold the guard when the owner drops. Retry that
        // race, but bound cleanup even during unwinding; never steal the guard.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match Self::guard(path) {
                Ok(guard) => return Ok(guard),
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    on_wait();
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn release(&self) -> io::Result<()> {
        let _guard = Self::release_guard(&self.path, || {})?;
        let metadata = fs::symlink_metadata(&self.path)?;
        if metadata.is_dir()
            && (metadata.dev(), metadata.ino()) == self.identity
            && fs::read(self.path.join("owner.json"))? == self.owner
        {
            fs::remove_file(self.path.join("owner.json"))?;
            fs::remove_dir(&self.path)?;
        }
        Ok(())
    }
}
impl Drop for ParityLock {
    fn drop(&mut self) {
        if let Err(error) = self.release() {
            eprintln!("#983 parity lease release failed; retained lease: {error}");
            assert!(std::thread::panicking(), "#983 parity lease release failed");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn parity_lock_publishes_and_releases_owned_lease() {
    let runtime = Runtime::new();
    let path = runtime.0.join("lease");
    let lease = ParityLock::acquire_at(&path, Duration::from_secs(1))
        .await
        .expect("#983 acquire uncontended test lease");
    let owner = fs::read(path.join("owner.json"));
    let owner: Option<Value> = owner
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    verify(
        owner.as_ref().is_some_and(|owner| {
            owner["pid"] == std::process::id()
                && owner["timestamp"].as_f64().is_some_and(|time| time > 0.0)
                && owner["token"]
                    .as_str()
                    .is_some_and(|token| token.len() == 32 && uuid::Uuid::parse_str(token).is_ok())
        }),
        "#983 parity lease publishes complete ownership",
    );
    drop(lease);
    verify(!path.exists(), "#983 parity lease releases owned directory");
    verify(
        runtime.0.join("lease.guard").is_file(),
        "#983 parity guard inode remains stable",
    );
}

#[tokio::test(flavor = "current_thread")]
async fn parity_lock_waits_for_predecessor() {
    let runtime = Runtime::new();
    let path = runtime.0.join("lease");
    let predecessor = ParityLock::acquire_at(&path, Duration::from_secs(1))
        .await
        .expect("#983 acquire predecessor lease");
    let mut waiter = Box::pin(ParityLock::acquire_at(&path, Duration::from_secs(1)));
    let pending =
        std::future::poll_fn(|cx| std::task::Poll::Ready(waiter.as_mut().poll(cx).is_pending()))
            .await;
    verify(pending, "#983 occupied lease waits before acquisition");
    drop(predecessor);
    let acquired = waiter.await;
    verify(
        acquired.is_ok(),
        "#983 released predecessor unblocks waiter",
    );
}

#[tokio::test(flavor = "current_thread")]
async fn parity_lock_times_out_without_reclaiming() {
    let runtime = Runtime::new();
    let path = runtime.0.join("lease");
    fs::create_dir(&path).expect("#983 predecessor directory fixture");
    fs::write(path.join("owner.json"), b"predecessor").expect("#983 predecessor owner fixture");
    let result = ParityLock::acquire_at(&path, Duration::from_millis(20)).await;
    verify(
        matches!(result, Err(ref error) if error.kind() == io::ErrorKind::TimedOut),
        "#983 occupied lease fails explicitly at deadline",
    );
    verify(
        fs::read(path.join("owner.json")).is_ok_and(|owner| owner == b"predecessor"),
        "#983 timed out waiter preserves predecessor ownership",
    );
}

#[tokio::test(flavor = "current_thread")]
async fn parity_lock_does_not_acquire_after_deadline() {
    let runtime = Runtime::new();
    let path = runtime.0.join("lease");
    let predecessor = ParityLock::acquire_at(&path, Duration::from_secs(1))
        .await
        .expect("#983 late wake predecessor");
    let mut waiter = Box::pin(ParityLock::acquire_at(&path, Duration::from_millis(20)));
    let pending =
        std::future::poll_fn(|cx| std::task::Poll::Ready(waiter.as_mut().poll(cx).is_pending()))
            .await;
    verify(pending, "#983 late wake waiter reached occupied lease");
    // Deliberately stall this current-thread executor beyond the deadline,
    // then release before polling the waiter: no scheduler ordering assumption.
    std::thread::sleep(Duration::from_millis(30));
    drop(predecessor);
    let result = waiter.await;
    verify(
        matches!(result, Err(ref error) if error.kind() == io::ErrorKind::TimedOut),
        "#983 expired waiter cannot acquire released lease",
    );
}

#[tokio::test(flavor = "current_thread")]
async fn parity_lock_cancelled_wait_preserves_owner() {
    let runtime = Runtime::new();
    let path = runtime.0.join("lease");
    let predecessor = ParityLock::acquire_at(&path, Duration::from_secs(1))
        .await
        .expect("#983 cancelled wait predecessor");
    let mut waiter = Box::pin(ParityLock::acquire_at(&path, Duration::from_secs(1)));
    let pending =
        std::future::poll_fn(|cx| std::task::Poll::Ready(waiter.as_mut().poll(cx).is_pending()))
            .await;
    verify(pending, "#983 cancelled waiter reached occupied lease");
    drop(waiter);
    verify(
        fs::read(path.join("owner.json")).is_ok_and(|owner| owner == predecessor.owner),
        "#983 cancelled waiter preserves predecessor ownership",
    );
    drop(predecessor);
    verify(
        ParityLock::acquire_at(&path, Duration::from_secs(1))
            .await
            .is_ok(),
        "#983 cancelled waiter leaves no reservation",
    );
}

#[tokio::test(flavor = "current_thread")]
async fn parity_lock_guard_wait_is_bounded() {
    let runtime = Runtime::new();
    let path = runtime.0.join("lease");
    let guard = ParityLock::guard(&path).expect("#983 own guard fixture");
    let result = ParityLock::acquire_at(&path, Duration::from_millis(20)).await;
    verify(
        matches!(result, Err(ref error) if error.kind() == io::ErrorKind::TimedOut),
        "#983 occupied guard shares acquisition deadline",
    );
    verify(
        !path.exists(),
        "#983 occupied guard forbids lease publication",
    );
    drop(guard);
}

#[test]
fn parity_lock_release_waits_for_guard() {
    let runtime = Runtime::new();
    let path = runtime.0.join("lease");
    let mut guard = Some(ParityLock::guard(&path).expect("#983 own release guard fixture"));
    let mut waited = false;
    let result = ParityLock::release_guard(&path, || {
        waited = true;
        // Deterministic handoff only after the release path observes WouldBlock.
        drop(guard.take());
    });
    verify(
        waited && result.is_ok(),
        "#983 release retries a briefly occupied guard",
    );
}

#[test]
fn parity_lock_release_guard_deadline_is_bounded() {
    let runtime = Runtime::new();
    let path = runtime.0.join("lease");
    let _guard = ParityLock::guard(&path).expect("#983 own never released guard fixture");
    let started = std::time::Instant::now();
    let result = ParityLock::release_guard(&path, || {});
    verify(
        result.is_err() && started.elapsed() >= Duration::from_millis(1900),
        "#983 release guard exhausts bounded retry before failure",
    );
}

#[test]
fn parity_lock_rejects_guard_hardlinks() {
    let runtime = Runtime::new();
    let path = runtime.0.join("lease");
    let original = runtime.0.join("original");
    fs::write(&original, b"").expect("#983 hardlink source fixture");
    fs::hard_link(original, runtime.0.join("lease.guard")).expect("#983 guard hardlink fixture");
    verify(
        matches!(ParityLock::guard(&path), Err(error) if error.to_string() == "#983 unsafe parity guard"),
        "#983 parity guard rejects multiple hardlinks",
    );
}

#[test]
fn parity_lock_rejects_guard_symlinks() {
    let runtime = Runtime::new();
    let path = runtime.0.join("lease");
    let original = runtime.0.join("original");
    fs::write(&original, b"").expect("#983 symlink source fixture");
    std::os::unix::fs::symlink(original, runtime.0.join("lease.guard"))
        .expect("#983 guard symlink fixture");
    verify(
        ParityLock::guard(&path).is_err(),
        "#983 parity guard rejects symlinks",
    );
}

#[test]
fn parity_lock_rejects_guard_fifo() {
    let runtime = Runtime::new();
    let path = runtime.0.join("lease");
    let status = Command::new("mkfifo")
        .arg(runtime.0.join("lease.guard"))
        .status()
        .expect("#983 create FIFO guard fixture");
    assert!(status.success(), "#983 FIFO guard fixture created");
    verify(
        matches!(ParityLock::guard(&path), Err(error) if error.to_string() == "#983 unsafe parity guard"),
        "#983 parity guard rejects nonregular files",
    );
}

#[tokio::test(flavor = "current_thread")]
async fn parity_lock_preserves_replaced_owner() {
    let runtime = Runtime::new();
    let path = runtime.0.join("lease");
    let lease = ParityLock::acquire_at(&path, Duration::from_secs(1))
        .await
        .expect("#983 owner replacement lease");
    let temporary = path.join("replacement.tmp");
    fs::write(&temporary, b"successor").expect("#983 replacement owner fixture");
    fs::rename(temporary, path.join("owner.json")).expect("#983 publish replacement owner");
    drop(lease);
    verify(
        fs::read(path.join("owner.json")).is_ok_and(|owner| owner == b"successor"),
        "#983 changed ownership prevents lease release",
    );
}

#[tokio::test(flavor = "current_thread")]
async fn parity_lock_preserves_replaced_directory() {
    let runtime = Runtime::new();
    let path = runtime.0.join("lease");
    let lease = ParityLock::acquire_at(&path, Duration::from_secs(1))
        .await
        .expect("#983 directory replacement lease");
    fs::rename(&path, runtime.0.join("old-lease")).expect("#983 retain old directory inode");
    fs::create_dir(&path).expect("#983 successor directory fixture");
    fs::write(path.join("owner.json"), &lease.owner).expect("#983 successor same token fixture");
    drop(lease);
    verify(
        path.is_dir(),
        "#983 changed directory identity prevents lease release",
    );
}

#[tokio::test(flavor = "current_thread")]
async fn sessions_list_matches_ts_matrix() {
    let _port = PORT.lock().await;
    let _lock = ParityLock::acquire().await;
    let oracle: Value = serde_json::from_str(ORACLE).expect("#983 TS oracle JSON");
    let cases = oracle["cases"].as_array().expect("#983 oracle cells");
    assert_eq!(
        cases.len(),
        77,
        "#983 76 registered cells plus unknown leaf"
    );
    for case in cases {
        let runtime = Runtime::new();
        let mut command = runtime.command(&case["input"]);
        let before = snapshot(&runtime.0);
        assert_eq!(
            before, case["disk"],
            "#983 same initial fixture: {}",
            case["input"]["id"]
        );
        let listener = if case["input"]["mockGateway"] == true {
            Some(
                TcpListener::bind(ADDRESS)
                    .await
                    .expect("#983 exclusively bind D0 mock 28999"),
            )
        } else {
            None
        };
        let child = spawn(&mut command, &case["input"]);
        let pid = child.0.as_ref().expect("#983 child PID").id();
        let output = if case["connections"] == 1 {
            let mock = replay(listener.as_ref().expect("#983 observed mock"), case, pid);
            let ((), output) = tokio::join!(
                async {
                    verify(
                        timeout(BOUND, mock).await.is_ok(),
                        &format!(
                            "#983 expected gateway lifecycle {}",
                            case["input"]["id"].as_str().expect("#983 cell id")
                        ),
                    );
                },
                collect(child)
            );
            output
        } else {
            collect(child).await
        };
        if let Some(listener) = listener.as_ref() {
            let reason = if case["connections"] == 1 {
                format!(
                    "#983 no reconnect {}",
                    case["input"]["id"].as_str().expect("#1136 cell id")
                )
            } else {
                format!(
                    "#983 preflight never connects {}",
                    case["input"]["id"].as_str().expect("#1136 cell id")
                )
            };
            verify_no_pending_connections(listener, &reason).await;
        }
        let matches = output.stdout == case["stdout"].as_str().expect("#983 TS stdout").as_bytes()
            && output.stderr == case["stderr"].as_str().expect("#983 TS stderr").as_bytes()
            && output.status.code().map(i64::from) == case["exit"].as_i64()
            && output.status.signal().map(i64::from) == case["signal"].as_i64();
        if !matches {
            eprintln!(
                "#983 {}: {:?}, stdout={:?}, stderr={:?}",
                case["input"]["id"],
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        verify(
            matches,
            &format!(
                "#983 TS output and exit {}",
                case["input"]["id"].as_str().expect("#983 cell id")
            ),
        );
        verify(
            snapshot(&runtime.0) == before,
            &format!(
                "#983 complete disk unchanged {}",
                case["input"]["id"].as_str().expect("#983 cell id")
            ),
        );
        println!("#983 observed {}", case["input"]["id"]);
    }
}

fn regenerates_identically(fixture: &str) {
    let runtime = Runtime::new();
    let output = runtime.0.join("extracted");
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    let node = std::env::var_os("NODE22").unwrap_or_else(|| "node".into());
    let version = Command::new(&node)
        .arg("--version")
        .output()
        .expect("#983 run selected Node --version (NODE22 or PATH node)");
    verify(
        version.status.success(),
        "#983 selected Node --version must exit successfully",
    );
    verify(
        version.stdout == b"v22.22.0\n",
        "#983 selected Node must report v22.22.0 before extraction",
    );
    let mut command = Command::new(&node);
    if fixture == "sdk-sessions-list" {
        command.env_remove("PDRS_D1_PLAN");
    }
    let result = command
        .arg(directory.join("generate.mjs"))
        .arg(&output)
        .output()
        .expect("#983 run extraction guard");
    if !result.status.success() {
        eprintln!(
            "#983 extraction failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    verify(
        result.status.success()
            && fs::read(output.join("oracle.json")).expect("#983 regenerated oracle")
                == fs::read(directory.join("oracle.json")).expect("#983 checked-in oracle"),
        &format!("#983 identical TS fixture {fixture}"),
    );
}
#[test]
#[ignore = "requires Node v22.22.0; extracts checked-in captures, never reruns TS"]
fn sessions_list_oracle_regenerates_identically() {
    regenerates_identically("sdk-sessions-list");
}

#[test]
#[ignore = "requires Node v22.22.0 and the original TS SDK with installed dependencies"]
fn sessions_list_transport_limits_regenerates_identically() {
    regenerates_identically("sdk-transport-limits");
}

#[tokio::test(flavor = "current_thread")]
async fn sessions_list_accepts_ts_large_frames() {
    let _port = PORT.lock().await;
    let _lock = ParityLock::acquire().await;
    let oracle: Value = serde_json::from_str(LIMITS_ORACLE).expect("#983 limits oracle JSON");
    let cases: Vec<_> = oracle["cases"]
        .as_array()
        .expect("#983 limits oracle cells")
        .iter()
        .filter(|case| case["implementation"] == "node-native")
        .collect();
    verify(
        cases
            .iter()
            .map(|case| case["frameBytes"].as_u64())
            .eq([Some(16_777_217), Some(104_857_601)]),
        "#983 native large-frame fixture coverage",
    );
    for case in cases {
        let frame_bytes =
            usize::try_from(case["frameBytes"].as_u64().expect("#983 TS frame bytes"))
                .expect("#983 TS frame bytes fit usize");
        assert!(
            case["accepted"] == true,
            "#983 native TS accepts recorded frame"
        );
        let runtime = Runtime::new();
        let input = json!({"argv":["sessions","list"],"env":{"PILOTDECK_SDK_AUTH_TOKEN":TOKEN}});
        let mut command = runtime.command(&input);
        let listener = TcpListener::bind(ADDRESS)
            .await
            .expect("#983 large-frame mock bind");
        let child = spawn(&mut command, &input);
        let mock = async {
            let (stream, _) = listener.accept().await.expect("#983 large-frame accept");
            let mut socket = accept_async(stream)
                .await
                .expect("#983 large-frame upgrade");
            let hello = socket
                .next()
                .await
                .expect("#983 large-frame hello exists")
                .expect("#983 large-frame hello read");
            let hello: Value =
                serde_json::from_str(hello.to_text().expect("#983 large-frame hello text"))
                    .expect("#983 large-frame hello JSON");
            assert_eq!(
                hello["protocolVersion"],
                pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,
                "#983 large-frame hello uses protocol constant"
            );
            socket
                .send(Message::Text(
                    json!({"type":"hello_ok","protocolVersion":oracle["protocolVersion"],
                "serverVersion":"test","serverInfo":{}})
                    .to_string()
                    .into(),
                ))
                .await
                .expect("#983 large-frame hello response");
            let request = socket
                .next()
                .await
                .expect("#983 large-frame request exists")
                .expect("#983 large-frame request read");
            let request: Value =
                serde_json::from_str(request.to_text().expect("#983 large-frame request text"))
                    .expect("#983 large-frame request JSON");
            assert_eq!(
                request["method"], "list_sessions",
                "#983 large-frame ordinary RPC"
            );
            let mut raw =
                json!({"type":"response","id":request["id"],"ok":true,"result":case["result"]})
                    .to_string();
            assert!(raw.len() < frame_bytes, "#983 large-frame padding length");
            raw.push_str(&" ".repeat(frame_bytes - raw.len()));
            // One Text message is one unfragmented frame. A size-limit mutant
            // may reset during send; let the CLI's exact output decide the kill.
            let sent = socket.send(Message::Text(raw.into())).await.is_ok();
            loop {
                match socket.next().await {
                    Some(Ok(Message::Close(_))) => {
                        let _ = socket.flush().await;
                    }
                    None | Some(Err(_)) => break,
                    _ => panic!("#983 large-frame unexpected client frame"),
                }
            }
            sent
        };
        let (sent, output) = tokio::join!(
            async {
                timeout(BOUND, mock)
                    .await
                    .expect("#983 bounded large-frame mock")
            },
            collect(child)
        );
        let expected = format!(
            "{}\n",
            serde_json::to_string_pretty(&case["result"]["sessions"])
                .expect("#983 large-frame observed sessions JSON")
        );
        verify(
            output.status.success()
                && output.stdout == expected.as_bytes()
                && output.stderr.is_empty(),
            &format!("#983 TS native accepts {frame_bytes} byte frame"),
        );
        assert!(sent, "#983 large-frame response sent completely");
        println!("#983 TS native accepted {frame_bytes} byte single frame");
    }
}

// Security exception D1-SDK-WS-RESOURCE-LIMIT-001: these values are
// independent of the product constant so changing the cap cannot move the test.
async fn resource_limit_connection() -> (
    Runtime,
    sdk_exit::OwnedChild,
    tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    Value,
) {
    let runtime = Runtime::new();
    let input = json!({"argv":["sessions","list"],"env":{"PILOTDECK_SDK_AUTH_TOKEN":TOKEN}});
    let listener = TcpListener::bind(ADDRESS)
        .await
        .expect("#983 resource-limit mock bind");
    let child = spawn(&mut runtime.command(&input), &input);
    let (socket, request) = timeout(BOUND, async {
        let (stream, _) = listener.accept().await.expect("#983 resource-limit accept");
        let mut socket = accept_async(stream)
            .await
            .expect("#983 resource-limit upgrade");
        let hello = socket
            .next()
            .await
            .expect("#983 resource-limit hello exists")
            .expect("#983 resource-limit hello read");
        let hello: Value =
            serde_json::from_str(hello.to_text().expect("#983 resource-limit hello text"))
                .expect("#983 resource-limit hello JSON");
        assert_eq!(hello["type"], "hello", "#983 resource-limit hello type");
        socket
            .send(Message::Text(
                json!({"type":"hello_ok","protocolVersion":"1.2",
                    "serverVersion":"test","serverInfo":{}})
                .to_string()
                .into(),
            ))
            .await
            .expect("#983 resource-limit hello response");
        let request = socket
            .next()
            .await
            .expect("#983 resource-limit request exists")
            .expect("#983 resource-limit request read");
        let request: Value =
            serde_json::from_str(request.to_text().expect("#983 resource-limit request text"))
                .expect("#983 resource-limit request JSON");
        assert_eq!(
            request["method"], "list_sessions",
            "#983 resource-limit ordinary RPC reached"
        );
        (socket, request)
    })
    .await
    .expect("#983 bounded resource-limit handshake");
    (runtime, child, socket, request)
}

fn resource_limit_header(opcode: u8, bytes: u64) -> [u8; 10] {
    // Server frames are unmasked; the full 64-bit declaration is present,
    // so rejection cannot be attributed to an incomplete/invalid header.
    let mut header = [0_u8; 10];
    header[0] = opcode;
    header[1] = 127;
    header[2..].copy_from_slice(&bytes.to_be_bytes());
    header
}

fn verify_resource_rejection(output: &std::process::Output, assertion: &str) {
    println!(
        "#983 resource limit: code={:?}, signal={:?}, stdout={:?}, stderr={:?}",
        output.status.code(),
        output.status.signal(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    verify(
        output.status.signal().is_none()
            && output.status.code() == Some(1)
            && output.stdout.is_empty()
            && output.stderr == b"Gateway WebSocket closed.\n",
        assertion,
    );
}

async fn reject_declared_frame(bytes: u64) {
    let _port = PORT.lock().await;
    let _lock = ParityLock::acquire().await;
    let (_runtime, child, mut socket, _) = resource_limit_connection().await;
    timeout(
        BOUND,
        socket
            .get_mut()
            .write_all(&resource_limit_header(0x81, bytes)),
    )
    .await
    .expect("#983 bounded hostile frame header write")
    .expect("#983 hostile frame header sent");
    // Keep TCP open and send no payload. EOF must not stand in for size rejection.
    let output = collect(child).await;
    verify_resource_rejection(
        &output,
        &format!("#983 hostile frame {bytes} rejected cleanly before payload"),
    );
    drop(socket);
}

#[tokio::test(flavor = "current_thread")]
async fn sessions_list_rejects_frame_cap_plus_one_header() {
    reject_declared_frame(128 * 1024 * 1024 + 1).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sessions_list_rejects_extreme_frame_header() {
    reject_declared_frame(1_u64 << 62).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sessions_list_rejects_fragmented_message_over_cap() {
    let _port = PORT.lock().await;
    let _lock = ParityLock::acquire().await;
    let (_runtime, child, mut socket, request) = resource_limit_connection().await;
    let raw = json!({"type":"response","id":request["id"],"ok":true,
        "result":{"sessions":[]}})
    .to_string();
    let mut chunk = vec![b' '; 1024 * 1024];
    assert!(
        raw.len() < chunk.len(),
        "#983 fragmented response fits first chunk"
    );
    chunk[..raw.len()].copy_from_slice(raw.as_bytes());
    // 128 non-final 1 MiB frames, then a final one-byte continuation. Every
    // frame is below the cap; removing only max_message_size returns valid [].
    let sent = timeout(BOUND, async {
        for index in 0..128 {
            let opcode = if index == 0 { 0x01 } else { 0x00 };
            socket
                .get_mut()
                .write_all(&resource_limit_header(opcode, chunk.len() as u64))
                .await?;
            socket.get_mut().write_all(&chunk).await?;
            if index == 0 {
                chunk.fill(b' ');
            }
        }
        socket.get_mut().write_all(&[0x80, 1, b' ']).await
    })
    .await
    .expect("#983 bounded fragmented response send");
    let output = collect(child).await;
    verify_resource_rejection(
        &output,
        "#983 fragmented message 134217729 bytes rejected cleanly",
    );
    println!("#983 fragmented response send completed={}", sent.is_ok());
    drop(socket);
}

// These are protocol regression expectations inferred from TS source; the
// coordinator's 76-cell capture did not observe these additional boundaries.
#[tokio::test(flavor = "current_thread")]
async fn sessions_list_transport_boundaries() {
    let _port = PORT.lock().await;
    let _lock = ParityLock::acquire().await;
    let cells = [
        ("upgrade-never", "", "Gateway connection timed out.\n", 1),
        ("hello-close", "", "protocol_mismatch\n", 1),
        ("rpc-close", "", "Gateway WebSocket closed.\n", 1),
        ("rpc-invalid-json", "", "Invalid Gateway JSON frame.\n", 1),
        ("rpc-binary", "", "Invalid Gateway JSON frame.\n", 1),
        ("error-default", "", "Gateway request failed.\n", 1),
        ("error-empty", "", "\n", 1),
        ("missing-sessions", "[]\n", "", 0),
        ("null-sessions", "[]\n", "", 0),
        ("false-sessions", "false\n", "", 0),
        (
            "null-result",
            "",
            "Cannot read properties of null (reading 'sessions')\n",
            1,
        ),
        (
            // A response without `result`: the missing-key fallback must stay
            // Undefined, not Null (#983 take_field).
            "undefined-result",
            "",
            "Cannot read properties of undefined (reading 'sessions')\n",
            1,
        ),
        ("wrong-id-and-ping", "[]\n", "", 0),
        ("hello-never", "", "Gateway hello timed out.\n", 1),
        (
            "rpc-never",
            "",
            "Gateway request timed out: list_sessions\n",
            1,
        ),
        (
            "nested-projection",
            "[\n  {\n    \"z\": [\n      {},\n      [],\n      \"中\\\"[,]\"\n    ],\n    \"a\": 0,\n    \"b\": 1e+21,\n    \"s\": \"\\ud800\"\n  }\n]\n",
            "",
            0,
        ),
    ];
    for (mode, stdout, stderr, code) in cells {
        let runtime = Runtime::new();
        let input = json!({"argv":["sessions","list"],"env":{"PILOTDECK_SDK_AUTH_TOKEN":TOKEN}});
        let mut command = runtime.command(&input);
        let before = snapshot(&runtime.0);
        let listener = TcpListener::bind(ADDRESS)
            .await
            .expect("#983 boundary mock bind");
        let started = std::time::Instant::now();
        let child = spawn(&mut command, &input);
        let mock = async {
            let (mut stream, _) = listener.accept().await.expect("#983 boundary accept");
            if mode == "upgrade-never" {
                use tokio::io::AsyncReadExt;
                let mut request = Vec::new();
                stream
                    .read_to_end(&mut request)
                    .await
                    .expect("#983 bounded unanswered HTTP upgrade");
                verify(
                    request.starts_with(b"GET /ws HTTP/1.1"),
                    &format!("#983 real HTTP upgrade {mode}"),
                );
                return;
            }
            let mut socket = accept_async(stream)
                .await
                .expect("#983 boundary WS upgrade");
            let hello = socket
                .next()
                .await
                .expect("#983 boundary hello")
                .expect("#983 boundary hello read");
            let hello: Value =
                serde_json::from_str(hello.to_text().expect("#983 boundary hello text"))
                    .expect("#983 boundary hello JSON");
            assert_eq!(hello["type"], "hello", "#983 boundary hello first");
            if mode == "hello-close" {
                socket
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Library(4001),
                        reason: "protocol_mismatch".into(),
                    })))
                    .await
                    .expect("#983 boundary hello close");
            } else if mode != "hello-never" {
                socket.send(Message::Text(json!({"type":"hello_ok","protocolVersion":"1.2","serverVersion":"test","serverInfo":{}}).to_string().into())).await.expect("#983 boundary hello ok");
                let request = socket
                    .next()
                    .await
                    .expect("#983 boundary request")
                    .expect("#983 boundary request read");
                let request: Value =
                    serde_json::from_str(request.to_text().expect("#983 boundary request text"))
                        .expect("#983 boundary request JSON");
                assert_eq!(
                    request["method"], "list_sessions",
                    "#983 boundary ordinary request"
                );
                let id = request["id"].clone();
                match mode {
                    "rpc-never" => {}
                    "rpc-close" => socket
                        .send(Message::Close(None))
                        .await
                        .expect("#983 boundary rpc close"),
                    "rpc-invalid-json" => socket
                        .send(Message::Text("{".into()))
                        .await
                        .expect("#983 boundary invalid JSON"),
                    "rpc-binary" => socket
                        .send(Message::Binary(vec![1, 2].into()))
                        .await
                        .expect("#983 boundary binary"),
                    "error-default" | "error-empty" => {
                        let error = if mode == "error-empty" {
                            json!({"message":""})
                        } else {
                            json!({"code":"x"})
                        };
                        socket
                            .send(Message::Text(
                                json!({"type":"response","id":id,"ok":false,"error":error})
                                    .to_string()
                                    .into(),
                            ))
                            .await
                            .expect("#983 boundary RPC error");
                    }
                    _ => {
                        // Queue all frames before flushing the valid response, so a bad
                        // ID matcher cannot race the mock's next write.
                        if mode == "wrong-id-and-ping" {
                            socket
                                .feed(Message::Ping(vec![9].into()))
                                .await
                                .expect("#983 boundary ping");
                            socket
                                .feed(Message::Text(
                                    json!({"type":"notification","name":"changed"})
                                        .to_string()
                                        .into(),
                                ))
                                .await
                                .expect("#983 boundary notification");
                            socket.feed(Message::Text(json!({"type":"response","id":"unrelated","ok":true,"result":{"sessions":["wrong"]}}).to_string().into())).await.expect("#983 boundary unrelated response");
                        }
                        let result = match mode {
                            "null-sessions" => Some("{\"sessions\":null}"),
                            "false-sessions" => Some("{\"sessions\":false}"),
                            "null-result" => Some("null"),
                            "undefined-result" => None,
                            "nested-projection" => Some(
                                r#"{"sessions":[{"z":[{},[],"中\"[,]"],"a":-0,"b":1e21,"s":"\ud800"}]}"#,
                            ),
                            _ => Some("{}"),
                        };
                        let frame = match result {
                            Some(result) => format!(
                                "{{\"type\":\"response\",\"id\":{id},\"ok\":true,\"result\":{result}}}"
                            ),
                            None => format!("{{\"type\":\"response\",\"id\":{id},\"ok\":true}}"),
                        };
                        socket
                            .send(Message::Text(frame.into()))
                            .await
                            .expect("#983 boundary result");
                    }
                }
            }
            let mut close = false;
            let mut pong = false;
            loop {
                match socket.next().await {
                    Some(Ok(Message::Close(_))) => {
                        close = true;
                        let _ = socket.flush().await;
                    }
                    Some(Ok(Message::Pong(bytes))) => {
                        verify(bytes.as_ref() == [9], &format!("#983 exact pong {mode}"));
                        pong = true;
                    }
                    None | Some(Err(_)) => break,
                    _ => panic!("#983 boundary unexpected extra frame"),
                }
            }
            verify(close, &format!("#983 boundary socket closed {mode}"));
            if mode == "wrong-id-and-ping" {
                verify(pong, &format!("#983 pong response {mode}"));
            }
        };
        let bound = Duration::from_secs(45);
        let ((), output) = tokio::join!(
            async {
                timeout(bound, mock)
                    .await
                    .expect("#983 bounded boundary mock");
            },
            collect_bounded(child, bound)
        );
        if mode.ends_with("-never") {
            let minimum = if mode == "rpc-never" { 29 } else { 9 };
            verify(
                started.elapsed() >= Duration::from_secs(minimum),
                &format!("#983 transport deadline duration {mode}"),
            );
        }
        let matches = output.stdout == stdout.as_bytes()
            && output.stderr == stderr.as_bytes()
            && output.status.code() == Some(code);
        if !matches {
            eprintln!(
                "#983 {mode}: {:?} {:?} {:?}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        verify(matches, &format!("#983 transport boundary output {mode}"));
        verify(
            snapshot(&runtime.0) == before,
            &format!("#983 complete disk unchanged {mode}"),
        );
        println!("#983 inferred boundary {mode}");
    }
}

//! #983: replay coordinator-owned TS captures through a real local WebSocket.
//! Supplemental transport cases below are explicitly not TS goldens.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    time::{Instant, sleep, timeout},
};
use tokio_tungstenite::{
    accept_async,
    tungstenite::{
        Message,
        protocol::{CloseFrame, frame::coding::CloseCode},
    },
};

const ORACLE: &str = include_str!("fixtures/sdk-sessions-list/oracle.json");
const ADDRESS: &str = "127.0.0.1:28999";
const URL: &str = "ws://127.0.0.1:28999/ws";
const TOKEN: &str = "d1-fixture-not-a-secret";
const BOUND: Duration = Duration::from_secs(15);
static PORT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// Bind the negative verdict to the current libtest and exact assertion; arbitrary
// server/CLI text cannot forge a kill. Uses negative_suite.ReceiptRun's protocol.
fn verify(condition: bool, assertion: &str) {
    if condition {
        return;
    }
    if let Ok(config) = std::env::var("PDRS_NEGATIVE_RECEIPTS") {
        let config: Value = serde_json::from_str(&config).expect("#983 receipt configuration");
        let thread = std::thread::current();
        let name = thread.name().expect("#983 named test");
        for target in config["targets"].as_array().expect("#983 receipt targets") {
            if target["test"] == name && target["assertion"] == assertion {
                let mut receipt = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(target["path"].as_str().expect("#983 receipt path"))
                    .expect("#983 fresh receipt");
                receipt
                    .write_all(
                        config["nonce"]
                            .as_str()
                            .expect("#983 receipt nonce")
                            .as_bytes(),
                    )
                    .expect("#983 receipt write");
            }
        }
    }
    panic!("{assertion}");
}

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-983-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).expect("#983 exclusive runtime");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("#983 runtime mode");
        for dir in [
            "home",
            "pilot",
            "project",
            "tmp",
            "bin",
            "xdg-config",
            "xdg-cache",
            "xdg-data",
            "xdg-state",
        ] {
            fs::create_dir(root.join(dir)).expect("#983 fixture directory");
            fs::set_permissions(root.join(dir), fs::Permissions::from_mode(0o700))
                .expect("#983 directory mode");
        }
        for (file, bytes) in [
            ("pilot/pilotdeck.yaml", &b"schemaVersion: 1\n"[..]),
            ("project/keep.txt", &b"D0 fixture\0\xff\n"[..]),
            ("openssl.cnf", &b""[..]),
        ] {
            fs::write(root.join(file), bytes).expect("#983 fixture file");
            fs::set_permissions(root.join(file), fs::Permissions::from_mode(0o600))
                .expect("#983 file mode");
        }
        Self(root)
    }
    fn command(&self, input: &Value) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pilotdeck"));
        command.env_clear().current_dir(self.0.join("project"));
        for (key, relative) in [
            ("HOME", "home"),
            ("USERPROFILE", "home"),
            ("CFFIXED_USER_HOME", "home"),
            ("PILOT_HOME", "pilot"),
            ("PILOTDECK_CONFIG_PATH", "pilot/pilotdeck.yaml"),
            ("XDG_CONFIG_HOME", "xdg-config"),
            ("XDG_CACHE_HOME", "xdg-cache"),
            ("XDG_DATA_HOME", "xdg-data"),
            ("XDG_STATE_HOME", "xdg-state"),
            ("TMPDIR", "tmp"),
            ("TMP", "tmp"),
            ("TEMP", "tmp"),
            ("PWD", "project"),
            ("PATH", "bin"),
            ("OPENSSL_CONF", "openssl.cnf"),
        ] {
            command.env(key, self.0.join(relative));
        }
        command.envs([
            ("LANG", "C"),
            ("LC_ALL", "C"),
            ("TZ", "UTC"),
            ("TERM", "dumb"),
            ("NO_COLOR", "1"),
            ("ANALYTICS_ENABLED", "0"),
            ("DOCKER_CONTAINER", "1"),
            ("COMMIT_HASH", "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"),
            ("PILOTDECK_GATEWAY_PORT", "28999"),
            ("PILOTDECK_GATEWAY_HOST", "127.0.0.1"),
            ("PILOTDECK_SDK_GATEWAY_URL", URL),
            ("PILOTDECK_GATEWAY_URL", URL),
        ]);
        for (key, value) in input["env"].as_object().expect("#983 input env") {
            command.env(key, value.as_str().expect("#983 env string"));
        }
        if let Some(unset) = input["unsetEnv"].as_array() {
            for key in unset {
                command.env_remove(key.as_str().expect("#983 unset key"));
            }
        }
        if let Some(token) = input["tokenFile"].as_str() {
            fs::write(self.0.join("pilot/server-token"), token).expect("#983 token fixture");
            fs::set_permissions(
                self.0.join("pilot/server-token"),
                fs::Permissions::from_mode(0o644),
            )
            .expect("#983 token mode");
        }
        for arg in input["argv"].as_array().expect("#983 input argv") {
            command.arg(arg.as_str().expect("#983 argv string"));
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct OwnedChild(Option<Child>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn spawn(command: &mut Command, input: &Value) -> OwnedChild {
    let mut owned = OwnedChild(Some(command.spawn().expect("#983 spawn CLI")));
    let child = owned.0.as_mut().expect("#983 immediately owned child");
    let hex = input["stdinHex"].as_str().unwrap_or("");
    let data: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("#983 stdin hex"))
        .collect();
    let mut stdin = child.stdin.take().expect("#983 stdin pipe");
    stdin.write_all(&data).expect("#983 fixture stdin");
    drop(stdin);
    owned
}
async fn collect(child: OwnedChild) -> std::process::Output {
    collect_bounded(child, BOUND).await
}
async fn collect_bounded(mut child: OwnedChild, bound: Duration) -> std::process::Output {
    timeout(bound, async {
        loop {
            if child
                .0
                .as_mut()
                .expect("#983 owned child")
                .try_wait()
                .expect("#983 poll CLI")
                .is_some()
            {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("#983 bounded CLI exit");
    child
        .0
        .take()
        .expect("#983 reap owned CLI")
        .wait_with_output()
        .expect("#983 collect output")
}
fn snapshot(root: &Path) -> Value {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<String, Value>) {
        let metadata = fs::symlink_metadata(path).expect("#983 snapshot metadata");
        let relative = path
            .strip_prefix(root)
            .expect("#983 snapshot contained path");
        let name = if relative.as_os_str().is_empty() {
            ".".into()
        } else {
            relative.to_string_lossy().into_owned()
        };
        let mode = metadata.permissions().mode() & 0o777;
        let record = if metadata.is_dir() {
            json!({"type":"directory","mode":mode})
        } else if metadata.is_file() {
            let bytes = fs::read(path).expect("#983 snapshot bytes");
            json!({"type":"file","mode":mode,"size":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))})
        } else {
            panic!("#983 unexpected fixture node: {name}");
        };
        entries.insert(name, record);
        if metadata.is_dir() {
            for entry in fs::read_dir(path).expect("#983 snapshot directory") {
                visit(root, &entry.expect("#983 snapshot entry").path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    serde_json::to_value(entries).expect("#983 snapshot JSON")
}

async fn replay(listener: TcpListener, case: &Value, pid: u32) {
    let (stream, _) = listener.accept().await.expect("#983 owned mock accept");
    let mut socket = accept_async(stream).await.expect("#983 WS upgrade");
    let mut id = String::new();
    for row in case["wire"].as_array().expect("#983 wire rows") {
        if row["kind"] == "receive" {
            let message = socket
                .next()
                .await
                .expect("#983 wire frame exists")
                .expect("#983 readable wire frame");
            let mut frame: Value =
                serde_json::from_str(message.to_text().expect("#983 text wire frame"))
                    .expect("#983 JSON wire frame");
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
            }
            verify(
                frame == row["frame"],
                &format!(
                    "#983 hello and request wire {}",
                    case["input"]["id"].as_str().expect("#983 cell id")
                ),
            );
        } else {
            let raw = row["raw"]
                .as_str()
                .expect("#983 original wire JSON")
                .replace("<request>", &id);
            socket
                .send(Message::Text(raw.into()))
                .await
                .expect("#983 mock response");
        }
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
    // The listener remains owned throughout: any extra connection is a failure.
    verify(
        timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err(),
        &format!(
            "#983 no reconnect {}",
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
            let mock = replay(listener.expect("#983 observed mock"), case, pid);
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
            let output = collect(child).await;
            if let Some(listener) = listener {
                verify(
                    timeout(Duration::from_millis(20), listener.accept())
                        .await
                        .is_err(),
                    &format!(
                        "#983 preflight never connects {}",
                        case["input"]["id"].as_str().expect("#983 cell id")
                    ),
                );
            }
            output
        };
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
    let result = Command::new(&node)
        .arg(directory.join("generate.mjs"))
        .arg(&output)
        .output()
        .expect("#983 run extraction guard");
    assert!(
        result.status.success(),
        "#983 extraction failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    verify(
        fs::read(output.join("oracle.json")).expect("#983 regenerated oracle") == ORACLE.as_bytes(),
        "#983 identical extraction from original TS records",
    );
}
#[test]
#[ignore = "requires coordinator-owned Node v22.22.0 captures; never reruns TS"]
fn sessions_list_oracle_regenerates_identically() {
    regenerates_identically("sdk-sessions-list");
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
                            "null-sessions" => "{\"sessions\":null}",
                            "false-sessions" => "{\"sessions\":false}",
                            "null-result" => "null",
                            "nested-projection" => {
                                r#"{"sessions":[{"z":[{},[],"中\"[,]"],"a":-0,"b":1e21,"s":"\ud800"}]}"#
                            }
                            _ => "{}",
                        };
                        let frame = format!(
                            "{{\"type\":\"response\",\"id\":{id},\"ok\":true,\"result\":{result}}}"
                        );
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

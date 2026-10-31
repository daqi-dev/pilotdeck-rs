//! #1029: replay 157 D1 and 36 supplemental real Node v22.22.0 captures.
//! Runtime and bounded parity lease follow the P02 test helper contract.
use futures_util::{FutureExt, SinkExt, StreamExt};
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

const ORACLE: &str = include_str!("fixtures/sdk-settings/oracle.json");
const ADDRESS: &str = "127.0.0.1:28999";
const URL: &str = "ws://127.0.0.1:28999/ws";
const BOUND: Duration = Duration::from_secs(15);
static PORT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[path = "common/negative_receipt.rs"]
mod negative_receipt;
use negative_receipt::verify;

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-1029-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).expect("#1029 exclusive runtime");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("#1029 runtime mode");
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
            fs::create_dir(root.join(dir)).expect("#1029 fixture directory");
            fs::set_permissions(root.join(dir), fs::Permissions::from_mode(0o700))
                .expect("#1029 directory mode");
        }
        for (file, bytes) in [
            ("pilot/pilotdeck.yaml", &b"schemaVersion: 1\n"[..]),
            ("project/keep.txt", &b"D0 fixture\0\xff\n"[..]),
            ("openssl.cnf", &b""[..]),
        ] {
            fs::write(root.join(file), bytes).expect("#1029 fixture file");
            fs::set_permissions(root.join(file), fs::Permissions::from_mode(0o600))
                .expect("#1029 file mode");
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
        for (key, value) in input["env"].as_object().expect("#1029 input env") {
            command.env(key, value.as_str().expect("#1029 env string"));
        }
        if let Some(unset) = input["unsetEnv"].as_array() {
            for key in unset {
                command.env_remove(key.as_str().expect("#1029 unset key"));
            }
        }
        if let Some(token) = input["tokenFile"].as_str() {
            fs::write(self.0.join("pilot/server-token"), token).expect("#1029 token fixture");
            fs::set_permissions(
                self.0.join("pilot/server-token"),
                fs::Permissions::from_mode(0o644),
            )
            .expect("#1029 token mode");
        }
        for arg in input["argv"].as_array().expect("#1029 input argv") {
            command.arg(arg.as_str().expect("#1029 argv string"));
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
type Writers = std::sync::Arc<std::sync::Mutex<Vec<StdinWriter>>>;
struct OwnedChild(Option<Child>, Writers);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
        // The child owns every early/delayed writer. Kill/reap happens first,
        // including when collect or the mock future is cancelled or panics.
        for writer in self.1.lock().expect("#1136 owned stdin writers").drain(..) {
            let finished =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer.finish()));
            if let Err(payload) = finished {
                if std::thread::panicking() {
                    eprintln!("#1136 stdin cleanup failed during unwinding");
                } else {
                    std::panic::resume_unwind(payload);
                }
            }
        }
    }
}
fn write_fixture_stdin(stdin: &mut impl Write, data: &[u8]) {
    match stdin.write_all(data) {
        Ok(()) => {}
        // Local preflight failures may exit without consuming any input.
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
        Err(error) => panic!("#1075 fixture stdin: {error:?}"),
    }
}

// Writers run independently of the current-thread Tokio mock. Completion is
// bounded and owned by OwnedChild, which closes the reader before joining.
struct StdinWriter {
    handle: std::thread::JoinHandle<()>,
    done: std::sync::mpsc::Receiver<std::thread::Result<()>>,
    joined: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl StdinWriter {
    fn start(mut stdin: impl Write + Send + 'static, data: Vec<u8>) -> Self {
        let (finished, done) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                write_fixture_stdin(&mut stdin, &data)
            }));
            drop(stdin);
            let _ = finished.send(result);
        });
        Self {
            handle,
            done,
            joined: Default::default(),
        }
    }
    fn finish(self) {
        let result = self
            .done
            .recv_timeout(BOUND)
            .expect("#1136 bounded stdin cleanup");
        self.handle.join().expect("#1136 owned stdin thread");
        self.joined
            .store(true, std::sync::atomic::Ordering::Release);
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }
}

fn spawn(command: &mut Command, input: &Value) -> OwnedChild {
    let mut owned = OwnedChild(
        Some(command.spawn().expect("#1029 spawn CLI")),
        Writers::default(),
    );
    let child = owned.0.as_mut().expect("#1029 immediately owned child");
    let hex = input["stdinHex"].as_str().unwrap_or("");
    let data: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("#1029 stdin hex"))
        .collect();
    owned
        .1
        .lock()
        .expect("#1136 register early stdin")
        .push(StdinWriter::start(
            child.stdin.take().expect("#1029 stdin pipe"),
            data,
        ));
    owned
}
#[test]
fn fixture_stdin_writes_all_bytes() {
    let data = b"fixture input\0\xff\n";
    let mut written = Vec::new();
    write_fixture_stdin(&mut written, data);
    verify(written == data, "#1075 complete stdin write");
}

#[test]
fn fixture_stdin_other_errors_still_fail() {
    // An exhausted slice returns WriteZero, not BrokenPipe.
    let result = std::panic::catch_unwind(|| {
        let mut writer = &mut [0_u8; 0][..];
        write_fixture_stdin(&mut writer, b"fixture input");
    });
    verify(result.is_err(), "#1075 other stdin errors must fail");
}

#[tokio::test]
async fn fixture_stdin_broken_pipe_preserves_output() {
    let mut command = Command::new("/bin/sh");
    command
        .env_clear()
        .args(["-c", "printf fixture-out; printf fixture-err >&2; exit 23"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Larger than the pipe capacity: a child that never reads must close the
    // reader before write_all can finish, independently of scheduler order.
    let input = json!({"stdinHex": "61".repeat(2 * 1024 * 1024)});
    let child = spawn(&mut command, &input);
    let output = std::panic::AssertUnwindSafe(collect(child))
        .catch_unwind()
        .await
        .unwrap_or_else(|error| {
            // Only a write-side EPIPE is this regression. Setup/collection failures
            // must not publish the negative control's assertion receipt.
            let broken_pipe = error.downcast_ref::<String>().is_some_and(|message| {
                message.starts_with("#1075 fixture stdin:") && message.contains("kind: BrokenPipe")
            });
            verify(!broken_pipe, "#1075 broken pipe must not panic");
            std::panic::resume_unwind(error)
        });
    verify(output.status.code() == Some(23), "#1075 preserve exit code");
    verify(output.stdout == b"fixture-out", "#1075 preserve stdout");
    verify(output.stderr == b"fixture-err", "#1075 preserve stderr");
}

#[test]
fn fixture_stdin_writer_yields_before_child_reads() {
    // Observe the thread entering the real pipe write. Both asynchronous and
    // synchronous writers publish this event; elapsed time is never the oracle.
    struct ObservedStdin {
        stdin: std::process::ChildStdin,
        entered: Option<std::sync::mpsc::Sender<std::thread::ThreadId>>,
    }
    impl Write for ObservedStdin {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(entered) = self.entered.take() {
                entered
                    .send(std::thread::current().id())
                    .expect("#1136 publish first pipe write");
            }
            self.stdin.write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.stdin.flush()
        }
    }
    let mut command = Command::new("/bin/sleep");
    command
        .env_clear()
        .arg("30")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = OwnedChild(
        Some(command.spawn().expect("#1136 spawn blocked reader")),
        Writers::default(),
    );
    let stdin = child
        .0
        .as_mut()
        .expect("#1136 own blocked reader")
        .stdin
        .take()
        .expect("#1136 observe blocked stdin");
    let (entered, observed) = std::sync::mpsc::channel();
    let starter = std::thread::spawn(move || {
        StdinWriter::start(
            ObservedStdin {
                stdin,
                entered: Some(entered),
            },
            vec![b'x'; 2 * 1024 * 1024],
        )
    });
    let caller = starter.thread().id();
    let first_write = observed.recv_timeout(BOUND);
    // The child never reads. Close its pipe before joining even if the progress
    // wait failed, so neither a synchronous mutant nor a failed setup leaks it.
    drop(child);
    starter.join().expect("#1136 starter joined").finish();
    let writer = first_write.expect("#1136 first pipe write progress");
    verify(
        writer != caller,
        "#1136 stdin writer yields before child reads",
    );
}

#[test]
fn fixture_stdin_cancel_reaps_writer() {
    let mut command = Command::new("/bin/sleep");
    command
        .env_clear()
        .arg("30")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = spawn(
        &mut command,
        &json!({"stdinHex": "61".repeat(2 * 1024 * 1024)}),
    );
    let writers = child.1.clone();
    verify(
        writers.lock().expect("#1136 writer ownership").len() == 1,
        "#1136 writer registered",
    );
    let joined = writers
        .lock()
        .expect("#1136 observe writer join")
        .first()
        .expect("#1136 registered writer exists")
        .joined
        .clone();
    // This child never consumes stdin. Cancelling its owner must close the
    // read end, finish the blocked write, and join the registered thread.
    drop(child);
    verify(
        writers.lock().expect("#1136 joined writers").is_empty()
            && joined.load(std::sync::atomic::Ordering::Acquire),
        "#1136 cancellation joins stdin writer",
    );
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
                .expect("#1029 owned child")
                .try_wait()
                .expect("#1029 poll CLI")
                .is_some()
            {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("#1029 bounded CLI exit");
    child
        .0
        .take()
        .expect("#1029 reap owned CLI")
        .wait_with_output()
        .expect("#1029 collect output")
}
// After child exit every unobserved connection is in the retained backlog.
async fn verify_no_pending_connections(listener: &TcpListener, reason: &str) {
    if let Ok(accepted) = timeout(Duration::from_millis(20), listener.accept()).await {
        let (_, peer) = accepted.expect("#1136 owned extra accept");
        eprintln!("#1136 extra connection after captured wire: {peer}");
        verify(false, reason);
    }
}

fn snapshot(root: &Path) -> Value {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<String, Value>) {
        let metadata = fs::symlink_metadata(path).expect("#1029 snapshot metadata");
        let relative = path
            .strip_prefix(root)
            .expect("#1029 snapshot contained path");
        let name = if relative.as_os_str().is_empty() {
            ".".into()
        } else {
            relative.to_string_lossy().into_owned()
        };
        let mode = metadata.permissions().mode() & 0o777;
        let record = if metadata.is_dir() {
            json!({"type":"directory","mode":mode})
        } else if metadata.is_file() {
            let bytes = fs::read(path).expect("#1029 snapshot bytes");
            json!({"type":"file","mode":mode,"size":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))})
        } else {
            panic!("#1029 unexpected fixture node: {name}");
        };
        entries.insert(name, record);
        if metadata.is_dir() {
            for entry in fs::read_dir(path).expect("#1029 snapshot directory") {
                visit(root, &entry.expect("#1029 snapshot entry").path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    serde_json::to_value(entries).expect("#1029 snapshot JSON")
}

async fn replay(listener: &TcpListener, case: &Value, pid: u32) {
    let (stream, _) = listener.accept().await.expect("#1029 owned mock accept");
    let mut socket = accept_async(stream).await.expect("#1029 WS upgrade");
    let mut id = String::new();
    let mut wire_checks = Vec::new();
    for row in case["wire"].as_array().expect("#1029 wire rows") {
        if row["kind"] == "receive" {
            let message = socket
                .next()
                .await
                .expect("#1029 wire frame exists")
                .expect("#1029 readable wire frame");
            verify(
                message.is_text(),
                &format!(
                    "#1029 expected text request frame {}",
                    case["input"]["id"].as_str().expect("#1029 cell id")
                ),
            );
            let raw = message.to_text().expect("#1029 verified text wire frame");
            let frame = pilotdeck_protocol::parse_json(raw).expect("#1029 JSON wire frame");
            if frame
                .get("type")
                .and_then(pilotdeck_protocol::JsValue::as_string)
                .is_some_and(|kind| kind.equals_str("request"))
            {
                id = frame
                    .get("id")
                    .and_then(pilotdeck_protocol::JsValue::as_string)
                    .expect("#1029 request id string")
                    .to_utf8()
                    .expect("#1029 UUID UTF-8");
                verify(
                    uuid::Uuid::parse_str(&id).is_ok(),
                    &format!(
                        "#1029 request UUID {}",
                        case["input"]["id"].as_str().expect("#1029 cell id")
                    ),
                );
            }
            let expected = row["raw"]
                .as_str()
                .expect("#1029 expected wire JSON")
                .replacen("\"id\":\"<request>\"", &format!("\"id\":\"{id}\""), 1);
            wire_checks.push((
                raw == expected,
                format!(
                    "#1029 hello and request wire {}",
                    case["input"]["id"].as_str().expect("#1029 cell id")
                ),
            ));
        } else {
            let raw = row["raw"]
                .as_str()
                .expect("#1029 original wire JSON")
                .replacen("\"id\":\"<request>\"", &format!("\"id\":\"{id}\""), 1);
            socket
                .send(Message::Text(raw.into()))
                .await
                .expect("#1029 mock response");
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
            .expect("#1029 auth close");
    }
    if let Some(signal) = signal {
        let status = Command::new("/bin/kill")
            .args(["-s", signal, &pid.to_string()])
            .status()
            .expect("#1029 send signal after request");
        assert!(status.success(), "#1029 signal delivered");
    }
    // Success/error gets a close frame; signal termination releases TCP without
    // a handshake. In both cases consume to EOF to prove no retained connection.
    let mut closed = false;
    loop {
        match socket.next().await {
            Some(Ok(Message::Close(frame))) => {
                closed = true;
                if rejected {
                    println!("#1029 auth close response: {frame:?}");
                }
                if !rejected {
                    verify(
                        frame.is_none(),
                        &format!(
                            "#1029 empty normal close frame {}",
                            case["input"]["id"].as_str().expect("#1029 cell id")
                        ),
                    );
                }
                let _ = socket.flush().await;
            }
            None | Some(Err(_)) => break,
            Some(Ok(_)) => panic!("#1029 unexpected frame after terminal RPC"),
        }
    }
    verify(
        signal.is_some() || closed,
        &format!(
            "#1029 terminal connection closed {}",
            case["input"]["id"].as_str().expect("#1029 cell id")
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
            .expect("#1029 D0 parity lease acquisition failed (bounded wait; no reclaim)")
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
                    format!("#1029 parity lease deadline: {}", path.display()),
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
                        eprintln!("#1029 waiting for parity lease: {}", path.display());
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
            return Err(io::Error::other("#1029 unsafe parity guard"));
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
            eprintln!("#1029 parity lease release failed; retained lease: {error}");
            assert!(
                std::thread::panicking(),
                "#1029 parity lease release failed"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn settings_matches_ts_matrix() {
    replay_matrix(false, None).await;
}

#[tokio::test(flavor = "current_thread")]
async fn settings_local_preflight_matches_ts_matrix() {
    replay_matrix(true, None).await;
}

#[tokio::test(flavor = "current_thread")]
async fn settings_hello_matches_ts_bytes() {
    replay_matrix(false, Some("settings-default--argv")).await;
}

#[tokio::test(flavor = "current_thread")]
async fn settings_request_matches_ts_bytes() {
    replay_matrix(false, Some("settings-set--argv")).await;
}

#[tokio::test(flavor = "current_thread")]
async fn settings_raw_params_preserve_lone_surrogates() {
    replay_matrix(false, Some("settings-extra--set-surrogate")).await;
}

#[tokio::test(flavor = "current_thread")]
async fn settings_raw_params_preserve_deep_object() {
    replay_matrix(false, Some("settings-extra--set-deep-object")).await;
}

#[tokio::test(flavor = "current_thread")]
async fn settings_request_preserves_placeholder_literals() {
    replay_matrix(false, Some("settings-extra--set-request-literal")).await;
}

#[tokio::test(flavor = "current_thread")]
async fn settings_response_preserves_placeholder_literals() {
    replay_matrix(false, Some("settings-extra--get-request-literal")).await;
}

async fn replay_matrix(local_only: bool, selected: Option<&str>) {
    let _port = PORT.lock().await;
    let _lock = ParityLock::acquire().await;
    let oracle: Value = serde_json::from_str(ORACLE).expect("#1029 TS oracle JSON");
    let cases = oracle["cases"].as_array().expect("#1029 oracle cells");
    verify(
        cases.len() == 193,
        "#1029 157 registered cells plus 36 live settings boundary cells",
    );
    if let Some(id) = selected {
        verify(
            cases
                .iter()
                .filter(|case| case["input"]["id"] == id)
                .count()
                == 1,
            &format!("#1029 exactly one selected TS cell {id}"),
        );
    }
    for case in cases
        .iter()
        .filter(|case| !local_only || case["connections"] == 0)
        .filter(|case| selected.is_none_or(|id| case["input"]["id"] == id))
    {
        let runtime = Runtime::new();
        let mut command = runtime.command(&case["input"]);
        let before = snapshot(&runtime.0);
        assert_eq!(
            before, case["disk"],
            "#1029 same initial fixture: {}",
            case["input"]["id"]
        );
        let listener = if case["input"]["mockGateway"] == true {
            Some(
                TcpListener::bind(ADDRESS)
                    .await
                    .expect("#1029 exclusively bind D0 mock 28999"),
            )
        } else {
            None
        };
        let child = spawn(&mut command, &case["input"]);
        let pid = child.0.as_ref().expect("#1029 child PID").id();
        let output = if case["connections"] == 1 {
            let mock = replay(listener.as_ref().expect("#1029 observed mock"), case, pid);
            let ((), output) = tokio::join!(
                async {
                    verify(
                        timeout(BOUND, mock).await.is_ok(),
                        &format!(
                            "#1029 expected gateway lifecycle {}",
                            case["input"]["id"].as_str().expect("#1029 cell id")
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
                    "#1029 no reconnect {}",
                    case["input"]["id"].as_str().expect("#1136 cell id")
                )
            } else {
                format!(
                    "#1029 preflight never connects {}",
                    case["input"]["id"].as_str().expect("#1136 cell id")
                )
            };
            verify_no_pending_connections(listener, &reason).await;
        }
        let matches = output.stdout == case["stdout"].as_str().expect("#1029 TS stdout").as_bytes()
            && output.stderr == case["stderr"].as_str().expect("#1029 TS stderr").as_bytes()
            && output.status.code().map(i64::from) == case["exit"].as_i64()
            && output.status.signal().map(i64::from) == case["signal"].as_i64();
        if !matches {
            eprintln!(
                "#1029 {}: {:?}, stdout={:?}, stderr={:?}",
                case["input"]["id"],
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        verify(
            matches,
            &format!(
                "#1029 TS output and exit {}",
                case["input"]["id"].as_str().expect("#1029 cell id")
            ),
        );
        verify(
            snapshot(&runtime.0) == before,
            &format!(
                "#1029 complete disk unchanged {}",
                case["input"]["id"].as_str().expect("#1029 cell id")
            ),
        );
        println!("#1029 observed {}", case["input"]["id"]);
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
        .expect("#1029 run selected Node --version (NODE22 or PATH node)");
    verify(
        version.status.success(),
        "#1029 selected Node --version must exit successfully",
    );
    verify(
        version.stdout == b"v22.22.0\n",
        "#1029 selected Node must report v22.22.0 before extraction",
    );
    let mut command = Command::new(&node);
    command.env_remove("PDRS_D1_PLAN");
    let result = command
        .arg(directory.join("generate.mjs"))
        .arg(&output)
        .output()
        .expect("#1029 run extraction guard");
    if !result.status.success() {
        eprintln!(
            "#1029 extraction failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    verify(
        result.status.success()
            && fs::read(output.join("oracle.json")).expect("#1029 regenerated oracle")
                == fs::read(directory.join("oracle.json")).expect("#1029 checked-in oracle"),
        &format!("#1029 identical TS fixture {fixture}"),
    );
}
#[test]
#[ignore = "requires Node v22.22.0; extracts checked-in captures, never reruns TS"]
fn settings_oracle_regenerates_identically() {
    regenerates_identically("sdk-settings");
}

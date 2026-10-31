//! #1097: real CLI resume and terminal replay of Node v22.22.0 captures.
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

const ORACLE: &str = include_str!("fixtures/sdk-format/oracle.json");
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
        let root = std::env::temp_dir().join(format!("pdrs-1097-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).expect("#1097 exclusive runtime");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("#1097 runtime mode");
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
            fs::create_dir(root.join(dir)).expect("#1097 fixture directory");
            fs::set_permissions(root.join(dir), fs::Permissions::from_mode(0o700))
                .expect("#1097 directory mode");
        }
        for (file, bytes) in [
            ("pilot/pilotdeck.yaml", &b"schemaVersion: 1\n"[..]),
            ("project/keep.txt", &b"D0 fixture\0\xff\n"[..]),
            ("openssl.cnf", &b""[..]),
        ] {
            fs::write(root.join(file), bytes).expect("#1097 fixture file");
            fs::set_permissions(root.join(file), fs::Permissions::from_mode(0o600))
                .expect("#1097 file mode");
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
        for (key, value) in input["env"].as_object().expect("#1097 input env") {
            command.env(key, value.as_str().expect("#1097 env string"));
        }
        if let Some(unset) = input["unsetEnv"].as_array() {
            for key in unset {
                command.env_remove(key.as_str().expect("#1097 unset key"));
            }
        }
        if let Some(token) = input["tokenFile"].as_str() {
            fs::write(self.0.join("pilot/server-token"), token).expect("#1097 token fixture");
            fs::set_permissions(
                self.0.join("pilot/server-token"),
                fs::Permissions::from_mode(0o644),
            )
            .expect("#1097 token mode");
        }
        for arg in input["argv"].as_array().expect("#1097 input argv") {
            command.arg(arg.as_str().expect("#1097 argv string"));
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
        for writer in self.1.lock().expect("#1097 owned stdin writers").drain(..) {
            let finished =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer.finish()));
            if let Err(payload) = finished {
                if std::thread::panicking() {
                    eprintln!("#1097 stdin cleanup failed during unwinding");
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
        Err(error) => panic!("#1097 fixture stdin: {error:?}"),
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
            .expect("#1097 bounded stdin cleanup");
        self.handle.join().expect("#1097 owned stdin thread");
        self.joined
            .store(true, std::sync::atomic::Ordering::Release);
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }
}

fn spawn(command: &mut Command, input: &Value) -> OwnedChild {
    let mut owned = OwnedChild(
        Some(command.spawn().expect("#1097 spawn CLI")),
        Writers::default(),
    );
    let child = owned.0.as_mut().expect("#1097 immediately owned child");
    let hex = input["stdinHex"].as_str().unwrap_or("");
    let data: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("#1097 stdin hex"))
        .collect();
    if input["holdStdin"] != true && input["stdinAfterCreate"] != true {
        owned
            .1
            .lock()
            .expect("#1097 register early stdin")
            .push(StdinWriter::start(
                child.stdin.take().expect("#1097 stdin pipe"),
                data,
            ));
    }
    owned
}
#[test]
fn fixture_stdin_writes_all_bytes() {
    let data = b"fixture input\0\xff\n";
    let mut written = Vec::new();
    write_fixture_stdin(&mut written, data);
    verify(written == data, "#1097 complete stdin write");
}

#[test]
fn fixture_stdin_other_errors_still_fail() {
    // An exhausted slice returns WriteZero, not BrokenPipe.
    let result = std::panic::catch_unwind(|| {
        let mut writer = &mut [0_u8; 0][..];
        write_fixture_stdin(&mut writer, b"fixture input");
    });
    verify(result.is_err(), "#1097 other stdin errors must fail");
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
                message.starts_with("#1097 fixture stdin:") && message.contains("kind: BrokenPipe")
            });
            verify(!broken_pipe, "#1097 broken pipe must not panic");
            std::panic::resume_unwind(error)
        });
    verify(output.status.code() == Some(23), "#1097 preserve exit code");
    verify(output.stdout == b"fixture-out", "#1097 preserve stdout");
    verify(output.stderr == b"fixture-err", "#1097 preserve stderr");
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
        "#1097 stdin writer yields before child reads",
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
        writers.lock().expect("#1097 writer ownership").len() == 1,
        "#1097 writer registered",
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
        writers.lock().expect("#1097 joined writers").is_empty()
            && joined.load(std::sync::atomic::Ordering::Acquire),
        "#1097 cancellation joins stdin writer",
    );
}

async fn collect(child: OwnedChild) -> std::process::Output {
    collect_bounded(child, BOUND).await
}
async fn collect_bounded(mut child: OwnedChild, bound: Duration) -> std::process::Output {
    collect_owned(&mut child, bound).await
}
// The replay may register delayed stdin after process exit; retain its owner.
async fn collect_owned(child: &mut OwnedChild, bound: Duration) -> std::process::Output {
    timeout(bound, async {
        loop {
            if child
                .0
                .as_mut()
                .expect("#1097 owned child")
                .try_wait()
                .expect("#1097 poll CLI")
                .is_some()
            {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("#1097 bounded CLI exit");
    child
        .0
        .take()
        .expect("#1097 reap owned CLI")
        .wait_with_output()
        .expect("#1097 collect output")
}
fn snapshot(root: &Path) -> Value {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<String, Value>) {
        let metadata = fs::symlink_metadata(path).expect("#1097 snapshot metadata");
        let relative = path
            .strip_prefix(root)
            .expect("#1097 snapshot contained path");
        let name = if relative.as_os_str().is_empty() {
            ".".into()
        } else {
            relative.to_string_lossy().into_owned()
        };
        let mode = metadata.permissions().mode() & 0o777;
        let record = if metadata.is_dir() {
            json!({"type":"directory","mode":mode})
        } else if metadata.is_file() {
            let bytes = fs::read(path).expect("#1097 snapshot bytes");
            json!({"type":"file","mode":mode,"size":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))})
        } else {
            panic!("#1097 unexpected fixture node: {name}");
        };
        entries.insert(name, record);
        if metadata.is_dir() {
            for entry in fs::read_dir(path).expect("#1097 snapshot directory") {
                visit(root, &entry.expect("#1097 snapshot entry").path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    serde_json::to_value(entries).expect("#1097 snapshot JSON")
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
            .expect("#1097 D0 parity lease acquisition failed (bounded wait; no reclaim)")
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
                    format!("#1097 parity lease deadline: {}", path.display()),
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
                        eprintln!("#1097 waiting for parity lease: {}", path.display());
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
            return Err(io::Error::other("#1097 unsafe parity guard"));
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
            eprintln!("#1097 parity lease release failed; retained lease: {error}");
            assert!(
                std::thread::panicking(),
                "#1097 parity lease release failed"
            );
        }
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
        .expect("#1097 run selected Node --version (NODE22 or PATH node)");
    verify(
        version.status.success(),
        "#1097 selected Node --version must exit successfully",
    );
    verify(
        version.stdout == b"v22.22.0\n",
        "#1097 selected Node must report v22.22.0 before extraction",
    );
    let mut command = Command::new(&node);
    command.env_remove("PDRS_D1_PLAN");
    let result = command
        .arg(directory.join("generate.mjs"))
        .arg(&output)
        .output()
        .expect("#1097 run extraction guard");
    if !result.status.success() {
        eprintln!(
            "#1097 extraction failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    verify(
        result.status.success()
            && fs::read(output.join("oracle.json")).expect("#1097 regenerated oracle")
                == fs::read(directory.join("oracle.json")).expect("#1097 checked-in oracle"),
        &format!("#1097 identical TS fixture {fixture}"),
    );
}

async fn replay(
    listener: &TcpListener,
    case: &Value,
    mut stdin: Option<std::process::ChildStdin>,
    pid: u32,
    writers: Writers,
    pending: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
) -> BTreeMap<String, String> {
    let mut sockets =
        BTreeMap::<u64, tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>>::new();
    let mut ids = BTreeMap::<String, String>::new();
    let mut run_ids = BTreeMap::<String, String>::new();
    let mut checks = Vec::new();
    for row in case["wire"].as_array().expect("#1097 wire rows") {
        let connection = row["connection"].as_u64().expect("#1097 connection number");
        if row["kind"] == "connection" {
            let accepted = if connection == 2 {
                let control = sockets.get_mut(&1).expect("#1097 live control socket");
                // The TS wire opens a query socket before any further control
                // traffic. Observe premature close/reuse immediately, so these
                // mutations cannot earn a kill receipt from a timeout.
                tokio::select! {
                    accepted = listener.accept() => accepted,
                    unexpected = control.next() => {
                        eprintln!("#1097 control traffic before query: {unexpected:?}");
                        verify(false, "#1097 expected second connection");
                        Err(io::Error::other("#1097 missing query connection"))
                    }
                }
            } else {
                listener.accept().await
            };
            let (stream, _) = accepted.expect("#1097 owned mock accept");
            let socket = accept_async(stream).await.expect("#1097 WS upgrade");
            sockets.insert(connection, socket);
            continue;
        }
        let socket = sockets
            .get_mut(&connection)
            .expect("#1097 established socket");
        if row["kind"] == "close" {
            let rejected = case["input"]["axis"] == "gateway-auth-rejected";
            if rejected {
                socket
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Library(4003),
                        reason: "auth_failed".into(),
                    })))
                    .await
                    .expect("#1097 reject auth");
            }
            let mut closed = false;
            while let Some(message) = socket.next().await {
                match message {
                    Ok(Message::Close(frame)) => {
                        closed = true;
                        checks.push((rejected || frame.is_none(), "empty close"));
                        let _ = socket.flush().await;
                    }
                    Err(_) => break,
                    _ => checks.push((false, "unexpected frame after stream")),
                }
            }
            checks.push((closed, "close handshake"));
            continue;
        }
        if row["kind"] == "server-close" {
            socket
                .send(Message::Close(Some(CloseFrame {
                    code: CloseCode::Normal,
                    reason: row["reason"]
                        .as_str()
                        .expect("#1097 close reason")
                        .to_owned()
                        .into(),
                })))
                .await
                .expect("#1097 server close sent");
            continue;
        }
        let mut expected = row["raw"]
            .as_str()
            .expect("#1097 original wire JSON")
            .to_owned();
        if row["kind"] == "receive" {
            let message = socket
                .next()
                .await
                .expect("#1097 wire frame exists")
                .expect("#1097 readable wire frame");
            verify(
                matches!(message, Message::Text(_)),
                "#1097 expected gateway lifecycle",
            );
            let raw = message.to_text().expect("#1097 text wire frame");
            let actual: Value = serde_json::from_str(raw).expect("#1097 JSON wire frame");
            if row["frame"]["type"] == "request" {
                let old_id = row["frame"]["id"]
                    .as_str()
                    .expect("#1097 original request id");
                let id = actual["id"].as_str().expect("#1097 actual request id");
                checks.push((
                    uuid::Uuid::parse_str(id).is_ok() && !ids.values().any(|v| v == id),
                    "fresh request UUID",
                ));
                ids.insert(old_id.into(), id.into());
                if row["frame"]["method"] == "submit_turn" {
                    let original = row["frame"]["params"]["runId"]
                        .as_str()
                        .expect("#1097 original runId");
                    let actual = actual["params"]["runId"]
                        .as_str()
                        .expect("#1097 actual runId");
                    checks.push((uuid::Uuid::parse_str(actual).is_ok(), "run UUID"));
                    run_ids.insert(original.into(), actual.into());
                }
            }
            bind_ids(&mut expected, row, &ids, &run_ids);
            checks.push((raw == expected, "wire bytes"));
            if let Some(signal) = case["input"]["signal"].as_str()
                && row["connection"] == 1
                && matches!(
                    row["frame"]["method"].as_str(),
                    Some("new_session" | "resume_session")
                )
            {
                let status = Command::new("/bin/kill")
                    .args(["-s", signal, &pid.to_string()])
                    .status()
                    .expect("#1097 signal request barrier");
                assert!(status.success(), "#1097 predecessor signal delivered");
            }
            if row["connection"] == 1
                && matches!(
                    row["frame"]["method"].as_str(),
                    Some("new_session" | "resume_session")
                )
                && let Some(stdin) = stdin.take()
            {
                let hex = case["input"]["stdinHex"]
                    .as_str()
                    .expect("#1097 delayed stdin hex");
                let bytes: Vec<_> = (0..hex.len())
                    .step_by(2)
                    .map(|i| {
                        u8::from_str_radix(&hex[i..i + 2], 16).expect("#1097 delayed hex byte")
                    })
                    .collect();
                writers
                    .lock()
                    .expect("#1097 register delayed stdin")
                    .push(StdinWriter::start(stdin, bytes));
            }
        } else {
            bind_ids(&mut expected, row, &ids, &run_ids);
            socket
                .send(Message::Text(expected.into()))
                .await
                .expect("#1097 mock response");
        }
    }
    if case["input"]["signal"].is_string() {
        for socket in sockets.values_mut() {
            match socket.next().await {
                None | Some(Err(_) | Ok(Message::Close(_))) => {}
                _ => panic!("#1097 unexpected frame after predecessor signal"),
            }
        }
    }
    for (matched, reason) in checks {
        if !matched {
            eprintln!("#1097 mismatch: {reason}");
        }
        verify(matched, "#1097 complete two-connection wire and cleanup");
    }
    if let Some((ready, release)) = pending {
        ready.send(()).expect("#1097 publish predecessor barrier");
        release
            .await
            .expect("#1097 observation cleanup acknowledgment");
    }
    run_ids
}

// The 20 ms accept sampling inside replay could miss a socket opened between
// wire-replay end and child exit. After the child has exited, every connection
// the mock never accepted is still parked in the listener backlog, so drain
// and fail on the first one instead of sampling a single window.
async fn verify_no_pending_connections(listener: &TcpListener, reason: &'static str) {
    loop {
        match timeout(Duration::from_millis(20), listener.accept()).await {
            Err(_) => break,
            Ok(accepted) => {
                let (_, peer) = accepted.expect("#1097 owned extra accept");
                eprintln!("#1097 extra connection from {peer} after the captured wire");
                verify(false, reason);
            }
        }
    }
}

fn bind_ids(
    raw: &mut String,
    row: &Value,
    ids: &BTreeMap<String, String>,
    runs: &BTreeMap<String, String>,
) {
    // Only replace structural UUID fields whose originals were captured. Text
    // and nested payloads containing placeholder-like strings remain untouched.
    if let Some(id) = row["frame"]["id"].as_str()
        && let Some(actual) = ids.get(id)
    {
        *raw = raw.replacen(
            &format!("\"id\":\"{id}\""),
            &format!("\"id\":\"{actual}\""),
            1,
        );
    }
    let run = row["frame"]["params"]["runId"]
        .as_str()
        .or_else(|| row["frame"]["event"]["runId"].as_str());
    if let Some(run) = run {
        let Some(actual) = runs.get(run) else { return };
        *raw = raw.replacen(
            &format!("\"runId\":\"{run}\""),
            &format!("\"runId\":\"{actual}\""),
            1,
        );
    }
}

fn bind_stdout(case: &Value, runs: &BTreeMap<String, String>) -> String {
    let stdout = case["stdout"].as_str().expect("#1097 TS stdout");
    let args = case["input"]["argv"].as_array().expect("#1097 format argv");
    let json = args.iter().any(|arg| arg == "--json");
    let jsonl = args.iter().any(|arg| arg == "--stream-json");
    let mut expected = String::new();
    for line in stdout.split_inclusive('\n') {
        let mut bound = line.to_owned();
        if jsonl
            && let Ok(value) = pilotdeck_protocol::parse_json(line)
            && value.get("type").is_some()
            && let Some(id) = value
                .get("runId")
                .and_then(pilotdeck_protocol::JsValue::as_string)
                .and_then(|id| id.to_utf8().ok())
            && let Some(actual) = runs.get(&id)
        {
            let old = format!("\"runId\":\"{id}\"");
            let new = format!("\"runId\":\"{actual}\"");
            assert_eq!(
                bound.matches(&old).count(),
                1,
                "#1097 unique bound public runId field"
            );
            bound = bound.replacen(&old, &new, 1);
        }
        expected.push_str(&bound);
    }
    // --json can follow a non-newline-terminated text prefix. Locate the
    // complete terminal envelope, not its inner result with an extra `}`.
    let marker = if json {
        "{\"sessionId\":\"d1-session\",\"result\":"
    } else {
        "{\"status\":\"result_unknown\",\"recovery\":"
    };
    if let Some(index) = expected.rfind(marker)
        && let Ok(value) = pilotdeck_protocol::parse_json(&expected[index..])
    {
        let result = if json {
            value.get("result")
        } else {
            Some(&value)
        };
        let recovery = result
            .filter(|value| {
                value
                    .get("status")
                    .and_then(pilotdeck_protocol::JsValue::as_string)
                    .is_some_and(|status| status.equals_str("result_unknown"))
            })
            .and_then(|value| value.get("recovery"));
        if let Some(id) = recovery
            .and_then(|value| value.get("runId"))
            .and_then(pilotdeck_protocol::JsValue::as_string)
            .and_then(|id| id.to_utf8().ok())
            && let Some(actual) = runs.get(&id)
        {
            let old = format!("\"runId\":\"{id}\"");
            let new = format!("\"runId\":\"{actual}\"");
            let suffix = &expected[index..];
            assert_eq!(
                suffix.matches(&old).count(),
                1,
                "#1097 unique bound recovery runId field"
            );
            let suffix = suffix.replacen(&old, &new, 1);
            expected.replace_range(index.., &suffix);
        }
    }
    expected
}

async fn run_case(case: &Value) {
    let runtime = Runtime::new();
    let mut command = runtime.command(&case["input"]);
    let before = snapshot(&runtime.0);
    assert_eq!(before, case["disk"], "#1097 same initial disk fixture");
    let listener = if case["input"]["mockGateway"] == true {
        Some(
            TcpListener::bind(ADDRESS)
                .await
                .expect("#1097 exclusively bind D0 mock 28999"),
        )
    } else {
        None
    };
    let mut child = spawn(&mut command, &case["input"]);
    let writers = child.1.clone();
    let stdin = if case["input"]["stdinAfterCreate"] == true {
        child
            .0
            .as_mut()
            .expect("#1097 owned stdin child")
            .stdin
            .take()
    } else {
        None
    };
    let mut run_ids = BTreeMap::new();
    let output = if case["connections"]
        .as_u64()
        .expect("#1097 connection count")
        > 0
    {
        let pid = child.0.as_ref().expect("#1097 owned child pid").id();
        let listener = listener.as_ref().expect("#1097 observed mock");
        if case["observation"] == "OBSERVED_TIMEOUT" {
            let (ready, observed) = tokio::sync::oneshot::channel();
            let (release, released) = tokio::sync::oneshot::channel();
            let mock = replay(listener, case, stdin, pid, writers, Some((ready, released)));
            let observe = async {
                timeout(BOUND, observed)
                    .await
                    .expect("#1097 predecessor ready deadline")
                    .expect("#1097 predecessor ready");
                // The TS host observed 12 seconds without an exit. Keep that
                // observation separate from our explicit cleanup signal.
                let deadline = Instant::now() + Duration::from_secs(12);
                while Instant::now() < deadline {
                    verify(
                        child
                            .0
                            .as_mut()
                            .expect("#1097 held child")
                            .try_wait()
                            .expect("#1097 held child poll")
                            .is_none(),
                        "#1097 predecessor stays pending until observation ends",
                    );
                    sleep(Duration::from_millis(10)).await;
                }
                child
                    .0
                    .as_mut()
                    .expect("#1097 cleanup owner")
                    .kill()
                    .expect("#1097 explicit cleanup kill");
                let output = collect_owned(&mut child, BOUND).await;
                release
                    .send(())
                    .expect("#1097 release held mock after cleanup");
                output
            };
            let (ids, output) = tokio::join!(mock, observe);
            run_ids = ids;
            output
        } else {
            let mock = replay(listener, case, stdin, pid, writers, None);
            let (ids, output) = tokio::join!(
                async {
                    timeout(BOUND, mock)
                        .await
                        .expect("#1097 mock bounded; not a negative kill")
                },
                collect_owned(&mut child, BOUND)
            );
            run_ids = ids;
            output
        }
    } else {
        collect_owned(&mut child, BOUND).await
    };
    drop(child); // replay can no longer register a writer
    if let Some(listener) = listener.as_ref() {
        if case["connections"]
            .as_u64()
            .expect("#1097 connection count")
            > 0
        {
            verify_no_pending_connections(listener, "#1097 no third connection").await;
        } else {
            verify_no_pending_connections(listener, "#1097 preflight never connects").await;
        }
    }
    // Bind only registered JSON fields, keeping every other stdout byte intact.
    // JSONL public runId is top-level; terminal recovery sits inside result
    // when --json is present. Text prefixes and nested payloads are untouched.
    let expected = bind_stdout(case, &run_ids);
    let matches = output.stdout == expected.as_bytes()
        && output.stderr == case["stderr"].as_str().expect("#1097 TS stderr").as_bytes()
        && (case["observation"] == "OBSERVED_TIMEOUT"
            || (output.status.code().map(i64::from) == case["exit"].as_i64()
                && output.status.signal().map(i64::from) == case["signal"].as_i64()));
    if !matches {
        eprintln!(
            "#1097 {}: {:?}, stdout={:?}, stderr={:?}",
            case["input"]["id"],
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    verify(
        matches,
        &format!(
            "#1097 output bytes and exit: {}",
            case["input"]["id"].as_str().expect("#1097 case id")
        ),
    );
    verify(
        snapshot(&runtime.0) == before,
        "#1097 complete disk unchanged",
    );
    println!("#1097 observed {}", case["input"]["id"]);
}
fn cases() -> Vec<Value> {
    let oracle: Value = serde_json::from_str(ORACLE).expect("#1097 TS oracle JSON");
    let cases = oracle["cases"].as_array().expect("#1097 oracle cells");
    verify(cases.len() == 90, "#1097 registered 90 cells");
    cases.clone()
}

#[tokio::test(flavor = "current_thread")]
async fn format_matches_ts_matrix() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    for case in cases() {
        run_case(&case).await;
    }
}

#[test]
#[ignore = "requires Node v22.22.0; re-extracts checked-in real TS originals"]
fn format_oracle_regenerates_identically() {
    regenerates_identically("sdk-format");
}

#[tokio::test(flavor = "current_thread")]
async fn format_no_late_connection() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    let case = cases()
        .into_iter()
        .find(|case| case["input"]["id"] == "run--format-json")
        .expect("#1097 format late-connection fixture");
    run_case(&case).await;
}

#[tokio::test(flavor = "current_thread")]
async fn format_preflight_no_late_connection() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    let case = cases()
        .into_iter()
        .find(|case| case["input"]["id"] == "help--format-json")
        .expect("#1097 format preflight fixture");
    run_case(&case).await;
}

// Each negative control invokes one exact libtest identity. Keep the full
// matrix above as the unfiltered positive control, independent of this list.
mod format_cases {
    use super::*;

    macro_rules! cell {
        ($test:ident, $id:literal) => {
            #[tokio::test(flavor = "current_thread")]
            async fn $test() {
                let _port = PORT.lock().await;
                let _lease = ParityLock::acquire().await;
                let mut matched = cases()
                    .into_iter()
                    .filter(|case| case["input"]["id"] == $id);
                let case = matched
                    .next()
                    .expect(concat!("#1097 registered format cell: ", $id));
                assert!(
                    matched.next().is_none(),
                    concat!("#1097 unique format cell: ", $id)
                );
                run_case(&case).await;
            }
        };
    }
    cell!(
        resume_unknown_snapshot_both,
        "format-probe--resume-unknown-snapshot-both"
    );
    cell!(
        resume_unknown_snapshot_json,
        "format-probe--resume-unknown-snapshot-json"
    );
    cell!(
        resume_unknown_snapshot_jsonl,
        "format-probe--resume-unknown-snapshot-jsonl"
    );
    cell!(
        run_completed_structured_json,
        "format-probe--run-completed-structured-json"
    );
    cell!(
        run_completed_structured_null_json,
        "format-probe--run-completed-structured-null-json"
    );
    cell!(run_failed_json, "format-probe--run-failed-json");
    cell!(
        run_json_finish_reason,
        "format-probe--run-json-finish-reason"
    );
    cell!(run_json_usage_array, "format-probe--run-json-usage-array");
    cell!(run_json_usage_scalar, "format-probe--run-json-usage-scalar");
    cell!(
        run_jsonl_all_context_keys,
        "format-probe--run-jsonl-all-context-keys"
    );
    cell!(run_jsonl_all_types, "format-probe--run-jsonl-all-types");
    cell!(
        run_jsonl_duplicate_frame,
        "format-probe--run-jsonl-duplicate-frame"
    );
    cell!(
        run_jsonl_error_fields,
        "format-probe--run-jsonl-error-fields"
    );
    cell!(
        run_jsonl_sequence_only,
        "format-probe--run-jsonl-sequence-only"
    );
    cell!(
        run_jsonl_spread_array,
        "format-probe--run-jsonl-spread-array"
    );
    cell!(
        run_jsonl_spread_string,
        "format-probe--run-jsonl-spread-string"
    );
    cell!(
        run_jsonl_type_null_missing,
        "format-probe--run-jsonl-type-null-missing"
    );
    cell!(run_jsonl_type_number, "format-probe--run-jsonl-type-number");
    cell!(
        run_prototype_proto_jsonl,
        "format-probe--run-prototype-__proto__-jsonl"
    );
    cell!(
        run_prototype_to_string_json,
        "format-probe--run-prototype-toString-json"
    );
    cell!(
        run_prototype_to_string_jsonl,
        "format-probe--run-prototype-toString-jsonl"
    );
    cell!(
        run_unknown_empty_json,
        "format-probe--run-unknown-empty-json"
    );
    cell!(
        run_unknown_snapshot_both,
        "format-probe--run-unknown-snapshot-both"
    );
    cell!(
        run_unknown_snapshot_json,
        "format-probe--run-unknown-snapshot-json"
    );
    cell!(
        run_unknown_snapshot_jsonl,
        "format-probe--run-unknown-snapshot-jsonl"
    );
    cell!(run_format_json, "run--format-json");
    cell!(run_format_jsonl, "run--format-jsonl");
}

#[tokio::test]
async fn fixture_stdin_registered_after_exit_is_joined() {
    let mut command = Command::new("/bin/sh");
    command
        .env_clear()
        .args(["-c", "exit 23"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn(&mut command, &json!({"holdStdin": true}));
    let stdin = child
        .0
        .as_mut()
        .expect("#1136 late writer child")
        .stdin
        .take()
        .expect("#1136 late writer pipe");
    let output = collect_owned(&mut child, BOUND).await;
    assert_eq!(
        output.status.code(),
        Some(23),
        "#1136 predecessor exited before registration"
    );
    let writer = StdinWriter::start(stdin, vec![b'x'; 128 * 1024]);
    let joined = writer.joined.clone();
    child
        .1
        .lock()
        .expect("#1136 late writer registry")
        .push(writer);
    drop(child); // final replay owner
    verify(
        joined.load(std::sync::atomic::Ordering::Acquire),
        "#1136 post-exit writer joined",
    );
}

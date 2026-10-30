//! #1086: real CLI resume and terminal replay of Node v22.22.0 captures.
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

const ORACLE: &str = include_str!("fixtures/sdk-terminal/oracle.json");
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
        let root = std::env::temp_dir().join(format!("pdrs-1086-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).expect("#1086 exclusive runtime");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("#1086 runtime mode");
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
            fs::create_dir(root.join(dir)).expect("#1086 fixture directory");
            fs::set_permissions(root.join(dir), fs::Permissions::from_mode(0o700))
                .expect("#1086 directory mode");
        }
        for (file, bytes) in [
            ("pilot/pilotdeck.yaml", &b"schemaVersion: 1\n"[..]),
            ("project/keep.txt", &b"D0 fixture\0\xff\n"[..]),
            ("openssl.cnf", &b""[..]),
        ] {
            fs::write(root.join(file), bytes).expect("#1086 fixture file");
            fs::set_permissions(root.join(file), fs::Permissions::from_mode(0o600))
                .expect("#1086 file mode");
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
        for (key, value) in input["env"].as_object().expect("#1086 input env") {
            command.env(key, value.as_str().expect("#1086 env string"));
        }
        if let Some(unset) = input["unsetEnv"].as_array() {
            for key in unset {
                command.env_remove(key.as_str().expect("#1086 unset key"));
            }
        }
        if let Some(token) = input["tokenFile"].as_str() {
            fs::write(self.0.join("pilot/server-token"), token).expect("#1086 token fixture");
            fs::set_permissions(
                self.0.join("pilot/server-token"),
                fs::Permissions::from_mode(0o644),
            )
            .expect("#1086 token mode");
        }
        for arg in input["argv"].as_array().expect("#1086 input argv") {
            command.arg(arg.as_str().expect("#1086 argv string"));
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
        Err(error) => panic!("#1086 fixture stdin: {error:?}"),
    }
}

// Writers run independently of the current-thread Tokio mock. Completion is
// bounded and owned by OwnedChild, which closes the reader before joining.
struct StdinWriter {
    handle: std::thread::JoinHandle<()>,
    done: std::sync::mpsc::Receiver<std::thread::Result<()>>,
}
impl StdinWriter {
    fn start(mut stdin: std::process::ChildStdin, data: Vec<u8>) -> Self {
        let (finished, done) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                write_fixture_stdin(&mut stdin, &data)
            }));
            drop(stdin);
            let _ = finished.send(result);
        });
        Self { handle, done }
    }
    fn finish(self) {
        let result = self
            .done
            .recv_timeout(BOUND)
            .expect("#1097 bounded stdin cleanup");
        self.handle.join().expect("#1097 owned stdin thread");
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }
}

fn spawn(command: &mut Command, input: &Value) -> OwnedChild {
    let mut owned = OwnedChild(
        Some(command.spawn().expect("#1086 spawn CLI")),
        Writers::default(),
    );
    let child = owned.0.as_mut().expect("#1086 immediately owned child");
    let hex = input["stdinHex"].as_str().unwrap_or("");
    let data: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("#1086 stdin hex"))
        .collect();
    if input["holdStdin"] != true && input["stdinAfterCreate"] != true {
        owned
            .1
            .lock()
            .expect("#1097 register early stdin")
            .push(StdinWriter::start(
                child.stdin.take().expect("#1086 stdin pipe"),
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
    verify(written == data, "#1086 complete stdin write");
}

#[test]
fn fixture_stdin_other_errors_still_fail() {
    // An exhausted slice returns WriteZero, not BrokenPipe.
    let result = std::panic::catch_unwind(|| {
        let mut writer = &mut [0_u8; 0][..];
        write_fixture_stdin(&mut writer, b"fixture input");
    });
    verify(result.is_err(), "#1086 other stdin errors must fail");
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
                message.starts_with("#1086 fixture stdin:") && message.contains("kind: BrokenPipe")
            });
            verify(!broken_pipe, "#1086 broken pipe must not panic");
            std::panic::resume_unwind(error)
        });
    verify(output.status.code() == Some(23), "#1086 preserve exit code");
    verify(output.stdout == b"fixture-out", "#1086 preserve stdout");
    verify(output.stderr == b"fixture-err", "#1086 preserve stderr");
}

#[tokio::test]
async fn fixture_stdin_writer_yields_before_child_reads() {
    let runtime = Runtime::new();
    let release = runtime.0.join("release");
    let mut command = Command::new("/bin/sh");
    command
        .env_clear()
        .args([
            "-c",
            "while [ ! -f \"$1\" ]; do /bin/sleep 0.01; done; exit 23",
            "fixture",
        ])
        .arg(&release)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let input = json!({"stdinHex": "61".repeat(2 * 1024 * 1024)});
    let (ready, observed) = std::sync::mpsc::channel();
    let starter = std::thread::spawn(move || {
        ready
            .send(spawn(&mut command, &input))
            .unwrap_or_else(|_| panic!("#1097 publish owned child"));
    });
    // Always release the child before checking: a synchronous-writer mutant
    // fails this assertion without leaking a blocked writer or timing out.
    let early = observed.recv_timeout(Duration::from_secs(3));
    let yielded = early.is_ok();
    let temporary = release.with_extension("tmp");
    fs::write(&temporary, b"release").expect("#1097 release bytes");
    fs::rename(temporary, &release).expect("#1097 atomic child release");
    let child =
        early.unwrap_or_else(|_| observed.recv_timeout(BOUND).expect("#1097 released child"));
    starter.join().expect("#1097 starter joined");
    let output = collect(child).await;
    verify(yielded, "#1097 stdin writer yields before child reads");
    verify(
        output.status.code() == Some(23),
        "#1097 blocked reader exit preserved",
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
    // This child never consumes stdin. Cancelling its owner must close the
    // read end, finish the blocked write, and join the registered thread.
    drop(child);
    verify(
        writers.lock().expect("#1097 joined writers").is_empty(),
        "#1097 cancellation joins stdin writer",
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
                .expect("#1086 owned child")
                .try_wait()
                .expect("#1086 poll CLI")
                .is_some()
            {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("#1086 bounded CLI exit");
    child
        .0
        .take()
        .expect("#1086 reap owned CLI")
        .wait_with_output()
        .expect("#1086 collect output")
}
fn snapshot(root: &Path) -> Value {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<String, Value>) {
        let metadata = fs::symlink_metadata(path).expect("#1086 snapshot metadata");
        let relative = path
            .strip_prefix(root)
            .expect("#1086 snapshot contained path");
        let name = if relative.as_os_str().is_empty() {
            ".".into()
        } else {
            relative.to_string_lossy().into_owned()
        };
        let mode = metadata.permissions().mode() & 0o777;
        let record = if metadata.is_dir() {
            json!({"type":"directory","mode":mode})
        } else if metadata.is_file() {
            let bytes = fs::read(path).expect("#1086 snapshot bytes");
            json!({"type":"file","mode":mode,"size":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))})
        } else {
            panic!("#1086 unexpected fixture node: {name}");
        };
        entries.insert(name, record);
        if metadata.is_dir() {
            for entry in fs::read_dir(path).expect("#1086 snapshot directory") {
                visit(root, &entry.expect("#1086 snapshot entry").path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    serde_json::to_value(entries).expect("#1086 snapshot JSON")
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
            .expect("#1086 D0 parity lease acquisition failed (bounded wait; no reclaim)")
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
                    format!("#1086 parity lease deadline: {}", path.display()),
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
                        eprintln!("#1086 waiting for parity lease: {}", path.display());
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
            return Err(io::Error::other("#1086 unsafe parity guard"));
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
            eprintln!("#1086 parity lease release failed; retained lease: {error}");
            assert!(
                std::thread::panicking(),
                "#1086 parity lease release failed"
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
        .expect("#1086 run selected Node --version (NODE22 or PATH node)");
    verify(
        version.status.success(),
        "#1086 selected Node --version must exit successfully",
    );
    verify(
        version.stdout == b"v22.22.0\n",
        "#1086 selected Node must report v22.22.0 before extraction",
    );
    let mut command = Command::new(&node);
    command.env_remove("PDRS_D1_PLAN");
    let result = command
        .arg(directory.join("generate.mjs"))
        .arg(&output)
        .output()
        .expect("#1086 run extraction guard");
    if !result.status.success() {
        eprintln!(
            "#1086 extraction failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    verify(
        result.status.success()
            && fs::read(output.join("oracle.json")).expect("#1086 regenerated oracle")
                == fs::read(directory.join("oracle.json")).expect("#1086 checked-in oracle"),
        &format!("#1086 identical TS fixture {fixture}"),
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
    for row in case["wire"].as_array().expect("#1086 wire rows") {
        let connection = row["connection"].as_u64().expect("#1086 connection number");
        if row["kind"] == "connection" {
            let accepted = if connection == 2 {
                let control = sockets.get_mut(&1).expect("#1086 live control socket");
                // The TS wire opens a query socket before any further control
                // traffic. Observe premature close/reuse immediately, so these
                // mutations cannot earn a kill receipt from a timeout.
                tokio::select! {
                    accepted = listener.accept() => accepted,
                    unexpected = control.next() => {
                        eprintln!("#1086 control traffic before query: {unexpected:?}");
                        verify(false, "#1086 expected second connection");
                        Err(io::Error::other("#1086 missing query connection"))
                    }
                }
            } else {
                listener.accept().await
            };
            let (stream, _) = accepted.expect("#1086 owned mock accept");
            let socket = accept_async(stream).await.expect("#1086 WS upgrade");
            sockets.insert(connection, socket);
            continue;
        }
        let socket = sockets
            .get_mut(&connection)
            .expect("#1086 established socket");
        if row["kind"] == "close" {
            let rejected = case["input"]["axis"] == "gateway-auth-rejected";
            if rejected {
                socket
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Library(4003),
                        reason: "auth_failed".into(),
                    })))
                    .await
                    .expect("#1086 reject auth");
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
                        .expect("#1086 close reason")
                        .to_owned()
                        .into(),
                })))
                .await
                .expect("#1086 server close sent");
            continue;
        }
        let mut expected = row["raw"]
            .as_str()
            .expect("#1086 original wire JSON")
            .to_owned();
        if row["kind"] == "receive" {
            let message = socket
                .next()
                .await
                .expect("#1086 wire frame exists")
                .expect("#1086 readable wire frame");
            verify(
                matches!(message, Message::Text(_)),
                "#1086 expected gateway lifecycle",
            );
            let raw = message.to_text().expect("#1086 text wire frame");
            let actual: Value = serde_json::from_str(raw).expect("#1086 JSON wire frame");
            if row["frame"]["type"] == "request" {
                let old_id = row["frame"]["id"]
                    .as_str()
                    .expect("#1086 original request id");
                let id = actual["id"].as_str().expect("#1086 actual request id");
                checks.push((
                    uuid::Uuid::parse_str(id).is_ok() && !ids.values().any(|v| v == id),
                    "fresh request UUID",
                ));
                ids.insert(old_id.into(), id.into());
                if row["frame"]["method"] == "submit_turn" {
                    let original = row["frame"]["params"]["runId"]
                        .as_str()
                        .expect("#1086 original runId");
                    let actual = actual["params"]["runId"]
                        .as_str()
                        .expect("#1086 actual runId");
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
                    .expect("#1086 signal request barrier");
                assert!(status.success(), "#1086 predecessor signal delivered");
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
                    .expect("#1086 delayed stdin hex");
                let bytes: Vec<_> = (0..hex.len())
                    .step_by(2)
                    .map(|i| {
                        u8::from_str_radix(&hex[i..i + 2], 16).expect("#1086 delayed hex byte")
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
                .expect("#1086 mock response");
        }
    }
    if case["input"]["signal"].is_string() {
        for socket in sockets.values_mut() {
            match socket.next().await {
                None | Some(Err(_) | Ok(Message::Close(_))) => {}
                _ => panic!("#1086 unexpected frame after predecessor signal"),
            }
        }
    }
    for (matched, reason) in checks {
        if !matched {
            eprintln!("#1086 mismatch: {reason}");
        }
        verify(matched, "#1086 complete two-connection wire and cleanup");
    }
    if let Some((ready, release)) = pending {
        ready.send(()).expect("#1086 publish predecessor barrier");
        release
            .await
            .expect("#1086 observation cleanup acknowledgment");
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
                let (_, peer) = accepted.expect("#1086 owned extra accept");
                eprintln!("#1086 extra connection from {peer} after the captured wire");
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

async fn run_case(case: &Value) {
    let runtime = Runtime::new();
    let mut command = runtime.command(&case["input"]);
    let before = snapshot(&runtime.0);
    assert_eq!(before, case["disk"], "#1086 same initial disk fixture");
    let listener = if case["input"]["mockGateway"] == true {
        Some(
            TcpListener::bind(ADDRESS)
                .await
                .expect("#1086 exclusively bind D0 mock 28999"),
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
            .expect("#1086 owned stdin child")
            .stdin
            .take()
    } else {
        None
    };
    let mut run_ids = BTreeMap::new();
    let output = if case["connections"]
        .as_u64()
        .expect("#1086 connection count")
        > 0
    {
        let pid = child.0.as_ref().expect("#1086 owned child pid").id();
        let listener = listener.as_ref().expect("#1086 observed mock");
        if case["observation"] == "OBSERVED_TIMEOUT" {
            let (ready, observed) = tokio::sync::oneshot::channel();
            let (release, released) = tokio::sync::oneshot::channel();
            let mock = replay(listener, case, stdin, pid, writers, Some((ready, released)));
            let observe = async {
                timeout(BOUND, observed)
                    .await
                    .expect("#1086 predecessor ready deadline")
                    .expect("#1086 predecessor ready");
                // The TS host observed 12 seconds without an exit. Keep that
                // observation separate from our explicit cleanup signal.
                let deadline = Instant::now() + Duration::from_secs(12);
                while Instant::now() < deadline {
                    verify(
                        child
                            .0
                            .as_mut()
                            .expect("#1086 held child")
                            .try_wait()
                            .expect("#1086 held child poll")
                            .is_none(),
                        "#1086 predecessor stays pending until observation ends",
                    );
                    sleep(Duration::from_millis(10)).await;
                }
                child
                    .0
                    .as_mut()
                    .expect("#1086 cleanup owner")
                    .kill()
                    .expect("#1086 explicit cleanup kill");
                let output = collect(child).await;
                release
                    .send(())
                    .expect("#1086 release held mock after cleanup");
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
                        .expect("#1086 mock bounded; not a negative kill")
                },
                collect(child)
            );
            run_ids = ids;
            output
        }
    } else {
        collect(child).await
    };
    if let Some(listener) = listener.as_ref() {
        if case["connections"]
            .as_u64()
            .expect("#1086 connection count")
            > 0
        {
            verify_no_pending_connections(listener, "#1086 no third connection").await;
        } else {
            verify_no_pending_connections(listener, "#1086 preflight never connects").await;
        }
    }
    let mut expected = case["stdout"].as_str().expect("#1086 TS stdout").to_owned();
    // Instantiate only the recovery.runId in the expected final JSON suffix.
    // Actual stdout and all text prefixes stay byte-for-byte unchanged.
    if let Some(index) = expected.rfind("{\"status\":\"result_unknown\",\"recovery\":") {
        let suffix = &expected[index..];
        let result = pilotdeck_protocol::parse_json(suffix).expect("#1086 recovery JSON suffix");
        let id = result
            .get("recovery")
            .and_then(|v| v.get("runId"))
            .and_then(pilotdeck_protocol::JsValue::as_string)
            .expect("#1086 recovery runId")
            .to_utf8()
            .ok();
        if let Some(id) = id
            && let Some(actual) = run_ids.get(&id)
        {
            let replacement = suffix.replacen(
                &format!("\"runId\":\"{id}\""),
                &format!("\"runId\":\"{actual}\""),
                1,
            );
            expected.replace_range(index.., &replacement);
        }
    }
    let matches = output.stdout == expected.as_bytes()
        && output.stderr == case["stderr"].as_str().expect("#1086 TS stderr").as_bytes()
        && (case["observation"] == "OBSERVED_TIMEOUT"
            || (output.status.code().map(i64::from) == case["exit"].as_i64()
                && output.status.signal().map(i64::from) == case["signal"].as_i64()));
    if !matches {
        eprintln!(
            "#1086 {}: {:?}, stdout={:?}, stderr={:?}",
            case["input"]["id"],
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    verify(matches, "#1086 output bytes and exit");
    verify(
        snapshot(&runtime.0) == before,
        "#1086 complete disk unchanged",
    );
    println!("#1086 observed {}", case["input"]["id"]);
}
fn cases() -> Vec<Value> {
    let oracle: Value = serde_json::from_str(ORACLE).expect("#1086 TS oracle JSON");
    let cases = oracle["cases"].as_array().expect("#1086 oracle cells");
    verify(cases.len() == 143, "#1086 registered 143 cells");
    cases.clone()
}

#[tokio::test(flavor = "current_thread")]
async fn resume_matches_ts_matrix() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    let selected: Vec<_> = cases()
        .into_iter()
        .filter(|case| case["observation"] != "OBSERVED_TIMEOUT")
        .collect();
    verify(selected.len() == 141, "#1086 registered 141 exit cells");
    for case in selected {
        run_case(&case).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn predecessor_never_is_an_observation_timeout() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    let selected: Vec<_> = cases()
        .into_iter()
        .filter(|case| case["observation"] == "OBSERVED_TIMEOUT")
        .collect();
    verify(
        selected.len() == 2,
        "#1086 registered two never predecessors",
    );
    for case in selected {
        run_case(&case).await;
    }
}

#[test]
#[ignore = "requires Node v22.22.0; re-extracts checked-in real TS originals"]
fn terminal_oracle_regenerates_identically() {
    regenerates_identically("sdk-terminal");
}

#[tokio::test(flavor = "current_thread")]
async fn resume_token_reaches_gateway() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    let case = cases()
        .into_iter()
        .find(|case| case["input"]["id"] == "resume--gateway-unreachable")
        .expect("#1086 registered resume refusal cell");
    let listener = TcpListener::bind(ADDRESS)
        .await
        .expect("#1086 refusal port unoccupied");
    drop(listener);
    run_case(&case).await;
}

#[tokio::test(flavor = "current_thread")]
async fn resume_no_late_connection() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    let case = cases()
        .into_iter()
        .find(|case| case["input"]["id"] == "resume--argv")
        .expect("#1097 resume late-connection fixture");
    run_case(&case).await;
}

#[tokio::test(flavor = "current_thread")]
async fn resume_preflight_no_late_connection() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    let case = cases()
        .into_iter()
        .find(|case| case["input"]["id"] == "resume--missing-session")
        .expect("#1097 resume preflight fixture");
    run_case(&case).await;
}

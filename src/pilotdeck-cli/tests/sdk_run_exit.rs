//! #1058: real CLI process and two-socket run replay of Node v22.22.0 captures.
//! Runtime and bounded parity lease follow the P02 test helper contract.
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

const ORACLE: &str = include_str!("fixtures/sdk-run/oracle.json");
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
        let root = std::env::temp_dir().join(format!("pdrs-1058-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).expect("#1058 exclusive runtime");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("#1058 runtime mode");
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
            fs::create_dir(root.join(dir)).expect("#1058 fixture directory");
            fs::set_permissions(root.join(dir), fs::Permissions::from_mode(0o700))
                .expect("#1058 directory mode");
        }
        for (file, bytes) in [
            ("pilot/pilotdeck.yaml", &b"schemaVersion: 1\n"[..]),
            ("project/keep.txt", &b"D0 fixture\0\xff\n"[..]),
            ("openssl.cnf", &b""[..]),
        ] {
            fs::write(root.join(file), bytes).expect("#1058 fixture file");
            fs::set_permissions(root.join(file), fs::Permissions::from_mode(0o600))
                .expect("#1058 file mode");
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
        for (key, value) in input["env"].as_object().expect("#1058 input env") {
            command.env(key, value.as_str().expect("#1058 env string"));
        }
        if let Some(unset) = input["unsetEnv"].as_array() {
            for key in unset {
                command.env_remove(key.as_str().expect("#1058 unset key"));
            }
        }
        if let Some(token) = input["tokenFile"].as_str() {
            fs::write(self.0.join("pilot/server-token"), token).expect("#1058 token fixture");
            fs::set_permissions(
                self.0.join("pilot/server-token"),
                fs::Permissions::from_mode(0o644),
            )
            .expect("#1058 token mode");
        }
        for arg in input["argv"].as_array().expect("#1058 input argv") {
            command.arg(arg.as_str().expect("#1058 argv string"));
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
fn write_fixture_stdin(stdin: &mut impl Write, data: &[u8]) {
    match stdin.write_all(data) {
        Ok(()) => {}
        // Local preflight failures may exit without consuming any input.
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
        Err(error) => panic!("#1058 fixture stdin: {error:?}"),
    }
}

fn spawn(command: &mut Command, input: &Value) -> OwnedChild {
    let mut owned = OwnedChild(Some(command.spawn().expect("#1058 spawn CLI")));
    let child = owned.0.as_mut().expect("#1058 immediately owned child");
    let hex = input["stdinHex"].as_str().unwrap_or("");
    let data: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("#1058 stdin hex"))
        .collect();
    if input["holdStdin"] != true && input["stdinAfterCreate"] != true {
        let mut stdin = child.stdin.take().expect("#1058 stdin pipe");
        write_fixture_stdin(&mut stdin, &data);
        drop(stdin);
    }
    owned
}
#[test]
fn fixture_stdin_writes_all_bytes() {
    let data = b"fixture input\0\xff\n";
    let mut written = Vec::new();
    write_fixture_stdin(&mut written, data);
    verify(written == data, "#1058 complete stdin write");
}

#[test]
fn fixture_stdin_other_errors_still_fail() {
    // An exhausted slice returns WriteZero, not BrokenPipe.
    let result = std::panic::catch_unwind(|| {
        let mut writer = &mut [0_u8; 0][..];
        write_fixture_stdin(&mut writer, b"fixture input");
    });
    verify(result.is_err(), "#1058 other stdin errors must fail");
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
    let child =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| spawn(&mut command, &input)));
    let child = child.unwrap_or_else(|error| {
        // Only a write-side EPIPE is this regression. Setup/collection failures
        // must not publish the negative control's assertion receipt.
        let broken_pipe = error.downcast_ref::<String>().is_some_and(|message| {
            message.starts_with("#1058 fixture stdin:") && message.contains("kind: BrokenPipe")
        });
        verify(!broken_pipe, "#1058 broken pipe must not panic");
        std::panic::resume_unwind(error)
    });
    let output = collect(child).await;
    verify(output.status.code() == Some(23), "#1058 preserve exit code");
    verify(output.stdout == b"fixture-out", "#1058 preserve stdout");
    verify(output.stderr == b"fixture-err", "#1058 preserve stderr");
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
                .expect("#1058 owned child")
                .try_wait()
                .expect("#1058 poll CLI")
                .is_some()
            {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("#1058 bounded CLI exit");
    child
        .0
        .take()
        .expect("#1058 reap owned CLI")
        .wait_with_output()
        .expect("#1058 collect output")
}
fn snapshot(root: &Path) -> Value {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<String, Value>) {
        let metadata = fs::symlink_metadata(path).expect("#1058 snapshot metadata");
        let relative = path
            .strip_prefix(root)
            .expect("#1058 snapshot contained path");
        let name = if relative.as_os_str().is_empty() {
            ".".into()
        } else {
            relative.to_string_lossy().into_owned()
        };
        let mode = metadata.permissions().mode() & 0o777;
        let record = if metadata.is_dir() {
            json!({"type":"directory","mode":mode})
        } else if metadata.is_file() {
            let bytes = fs::read(path).expect("#1058 snapshot bytes");
            json!({"type":"file","mode":mode,"size":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))})
        } else {
            panic!("#1058 unexpected fixture node: {name}");
        };
        entries.insert(name, record);
        if metadata.is_dir() {
            for entry in fs::read_dir(path).expect("#1058 snapshot directory") {
                visit(root, &entry.expect("#1058 snapshot entry").path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    serde_json::to_value(entries).expect("#1058 snapshot JSON")
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
            .expect("#1058 D0 parity lease acquisition failed (bounded wait; no reclaim)")
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
                    format!("#1058 parity lease deadline: {}", path.display()),
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
                        eprintln!("#1058 waiting for parity lease: {}", path.display());
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
            return Err(io::Error::other("#1058 unsafe parity guard"));
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
            eprintln!("#1058 parity lease release failed; retained lease: {error}");
            assert!(
                std::thread::panicking(),
                "#1058 parity lease release failed"
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
        .expect("#1058 run selected Node --version (NODE22 or PATH node)");
    verify(
        version.status.success(),
        "#1058 selected Node --version must exit successfully",
    );
    verify(
        version.stdout == b"v22.22.0\n",
        "#1058 selected Node must report v22.22.0 before extraction",
    );
    let mut command = Command::new(&node);
    command.env_remove("PDRS_D1_PLAN");
    let result = command
        .arg(directory.join("generate.mjs"))
        .arg(&output)
        .output()
        .expect("#1058 run extraction guard");
    if !result.status.success() {
        eprintln!(
            "#1058 extraction failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    verify(
        result.status.success()
            && fs::read(output.join("oracle.json")).expect("#1058 regenerated oracle")
                == fs::read(directory.join("oracle.json")).expect("#1058 checked-in oracle"),
        &format!("#1058 identical TS fixture {fixture}"),
    );
}

async fn replay(
    listener: TcpListener,
    case: &Value,
    mut stdin: Option<std::process::ChildStdin>,
    pid: u32,
) {
    let mut sockets =
        BTreeMap::<u64, tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>>::new();
    let mut ids = BTreeMap::<String, String>::new();
    let mut run_ids = BTreeMap::<String, String>::new();
    let mut checks = Vec::new();
    for row in case["wire"].as_array().expect("#1058 wire rows") {
        let connection = row["connection"].as_u64().expect("#1058 connection number");
        if row["kind"] == "connection" {
            let accepted = if connection == 2 {
                let control = sockets.get_mut(&1).expect("#1058 live control socket");
                // The TS wire opens a query socket before any further control
                // traffic. Observe premature close/reuse immediately, so these
                // mutations cannot earn a kill receipt from a timeout.
                tokio::select! {
                    accepted = listener.accept() => accepted,
                    unexpected = control.next() => {
                        eprintln!("#1058 control traffic before query: {unexpected:?}");
                        verify(false, "#1058 expected second connection");
                        Err(io::Error::other("#1058 missing query connection"))
                    }
                }
            } else {
                listener.accept().await
            };
            let (stream, _) = accepted.expect("#1058 owned mock accept");
            let socket = accept_async(stream).await.expect("#1058 WS upgrade");
            sockets.insert(connection, socket);
            continue;
        }
        let socket = sockets
            .get_mut(&connection)
            .expect("#1058 established socket");
        if row["kind"] == "close" {
            let rejected = case["input"]["axis"] == "gateway-auth-rejected";
            if rejected {
                socket
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Library(4003),
                        reason: "auth_failed".into(),
                    })))
                    .await
                    .expect("#1058 reject auth");
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
        let mut expected = row["raw"]
            .as_str()
            .expect("#1058 original wire JSON")
            .to_owned();
        if row["kind"] == "receive" {
            let message = socket
                .next()
                .await
                .expect("#1058 wire frame exists")
                .expect("#1058 readable wire frame");
            verify(
                matches!(message, Message::Text(_)),
                "#1058 expected gateway lifecycle",
            );
            let raw = message.to_text().expect("#1058 text wire frame");
            let actual: Value = serde_json::from_str(raw).expect("#1058 JSON wire frame");
            if row["frame"]["type"] == "request" {
                let old_id = row["frame"]["id"]
                    .as_str()
                    .expect("#1058 original request id");
                let id = actual["id"].as_str().expect("#1058 actual request id");
                checks.push((
                    uuid::Uuid::parse_str(id).is_ok() && !ids.values().any(|v| v == id),
                    "fresh request UUID",
                ));
                ids.insert(old_id.into(), id.into());
                if row["frame"]["method"] == "submit_turn" {
                    let original = row["frame"]["params"]["runId"]
                        .as_str()
                        .expect("#1058 original runId");
                    let actual = actual["params"]["runId"]
                        .as_str()
                        .expect("#1058 actual runId");
                    checks.push((uuid::Uuid::parse_str(actual).is_ok(), "run UUID"));
                    run_ids.insert(original.into(), actual.into());
                }
            }
            bind_ids(&mut expected, row, &ids, &run_ids);
            checks.push((raw == expected, "wire bytes"));
            if let Some(signal) = case["input"]["signal"].as_str()
                && row["frame"]["method"] == "new_session"
            {
                let status = Command::new("/bin/kill")
                    .args(["-s", signal, &pid.to_string()])
                    .status()
                    .expect("#1058 signal request barrier");
                assert!(status.success(), "#1058 predecessor signal delivered");
            }
            if row["frame"]["method"] == "new_session"
                && let Some(mut stdin) = stdin.take()
            {
                let hex = case["input"]["stdinHex"]
                    .as_str()
                    .expect("#1058 delayed stdin hex");
                let bytes: Vec<_> = (0..hex.len())
                    .step_by(2)
                    .map(|i| {
                        u8::from_str_radix(&hex[i..i + 2], 16).expect("#1058 delayed hex byte")
                    })
                    .collect();
                stdin
                    .write_all(&bytes)
                    .expect("#1058 stdin released after new_session request");
            }
        } else {
            bind_ids(&mut expected, row, &ids, &run_ids);
            socket
                .send(Message::Text(expected.into()))
                .await
                .expect("#1058 mock response");
        }
    }
    if case["input"]["signal"].is_string() {
        for socket in sockets.values_mut() {
            match socket.next().await {
                None | Some(Err(_) | Ok(Message::Close(_))) => {}
                _ => panic!("#1058 unexpected frame after predecessor signal"),
            }
        }
    }
    for (matched, reason) in checks {
        if !matched {
            eprintln!("#1058 mismatch: {reason}");
        }
        verify(matched, "#1058 complete two-connection wire and cleanup");
    }
    verify(
        timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err(),
        "#1058 no third connection",
    );
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
        let actual = runs.get(run).expect("#1058 previously observed run UUID");
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
    assert_eq!(before, case["disk"], "#1058 same initial disk fixture");
    let listener = if case["input"]["mockGateway"] == true {
        Some(
            TcpListener::bind(ADDRESS)
                .await
                .expect("#1058 exclusively bind D0 mock 28999"),
        )
    } else {
        None
    };
    let mut child = spawn(&mut command, &case["input"]);
    let stdin = if case["input"]["stdinAfterCreate"] == true {
        child
            .0
            .as_mut()
            .expect("#1058 owned stdin child")
            .stdin
            .take()
    } else {
        None
    };
    let output = if case["connections"]
        .as_u64()
        .expect("#1058 connection count")
        > 0
    {
        let pid = child.0.as_ref().expect("#1058 owned child pid").id();
        let mock = replay(listener.expect("#1058 observed mock"), case, stdin, pid);
        let ((), output) = tokio::join!(
            async {
                assert!(
                    timeout(BOUND, mock).await.is_ok(),
                    "#1058 mock timed out; not a negative-control kill",
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
                "#1058 preflight never connects",
            );
        }
        output
    };
    let matches = output.stdout == case["stdout"].as_str().expect("#1058 TS stdout").as_bytes()
        && output.stderr == case["stderr"].as_str().expect("#1058 TS stderr").as_bytes()
        && output.status.code().map(i64::from) == case["exit"].as_i64()
        && output.status.signal().map(i64::from) == case["signal"].as_i64();
    if !matches {
        eprintln!(
            "#1058 {}: {:?}, stdout={:?}, stderr={:?}",
            case["input"]["id"],
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    verify(matches, "#1058 output bytes and exit");
    verify(
        snapshot(&runtime.0) == before,
        "#1058 complete disk unchanged",
    );
    println!("#1058 observed {}", case["input"]["id"]);
}
fn cases() -> Vec<Value> {
    let oracle: Value = serde_json::from_str(ORACLE).expect("#1058 TS oracle JSON");
    let cases = oracle["cases"].as_array().expect("#1058 oracle cells");
    assert_eq!(cases.len(), 66, "#1058 registered observed cases");
    cases.clone()
}
fn case(id: &str) -> Value {
    cases()
        .into_iter()
        .find(|c| c["input"]["id"] == id)
        .expect("#1058 registered cell exists")
}
#[tokio::test(flavor = "current_thread")]
async fn run_matches_ts_matrix() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    for case in cases() {
        run_case(&case).await;
    }
}
#[tokio::test(flavor = "current_thread")]
async fn run_channel_and_two_connections() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    run_case(&case("run--flag-combined")).await;
}
#[tokio::test(flavor = "current_thread")]
async fn run_token_file_reaches_gateway() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    // Keep the token-file input/disk and reuse the registered refusal oracle's
    // output. This derived case is not a separate full TS host capture.
    let mut input = case("run--token-file");
    let refusal = case("run--gateway-unreachable");
    input["input"]["mockGateway"] = json!(false);
    input["connections"] = json!(0);
    input["wire"] = json!([]);
    for field in ["stdout", "stderr", "exit"] {
        input[field] = refusal[field].clone();
    }
    // Best-effort conflict check; an external listener may still race after drop.
    let listener = TcpListener::bind(ADDRESS)
        .await
        .expect("#1058 refusal port must be unoccupied");
    drop(listener);
    run_case(&input).await;
}
#[tokio::test(flavor = "current_thread")]
async fn run_argv_does_not_wait_for_stdin() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    let mut input = case("run--stdin-argv-priority");
    input["input"]["holdStdin"] = json!(true);
    run_case(&input).await;
}
#[test]
#[ignore = "requires Node v22.22.0; extracts real checked-in host captures"]
fn run_oracle_regenerates_identically() {
    regenerates_identically("sdk-run");
}

#[test]
#[ignore = "requires Node v22.22.0 and the pinned TS SDK; direct validation probes"]
fn run_validation_oracle_regenerates_identically() {
    regenerates_identically("sdk-run-validation");
}

// Targeted regressions for the supplemental Node function-probe cells. The
// full matrix also replays the captured bytes; these variants add stdin barriers.
fn set_prompt(input: &mut Value, prompt: &str) {
    for row in input["wire"].as_array_mut().expect("#1058 mutable wire") {
        if row["kind"] == "receive" && row["frame"]["method"] == "submit_turn" {
            let old = serde_json::to_string(&row["frame"]["params"]["message"])
                .expect("#1058 old prompt JSON");
            let new = serde_json::to_string(prompt).expect("#1058 prompt JSON");
            row["raw"] = json!(row["raw"].as_str().expect("#1058 request raw").replacen(
                &format!("\"message\":{old}"),
                &format!("\"message\":{new}"),
                1
            ));
            row["frame"]["params"]["message"] = json!(prompt);
        }
    }
}
fn set_events(input: &mut Value, events: Vec<Value>, response_end: bool) {
    let rows = input["wire"]
        .as_array_mut()
        .expect("#1058 mutable event wire");
    let id = rows
        .iter()
        .find(|row| row["frame"]["method"] == "submit_turn")
        .expect("#1058 submit fixture")["frame"]["id"]
        .clone();
    rows.retain(|row| row["frame"]["type"] != "event");
    let index = rows
        .iter()
        .position(|row| row["kind"] == "close")
        .expect("#1058 close barrier");
    let count = events.len();
    let mut frames: Vec<_> = events.into_iter().enumerate().map(|(seq,event)| {
        let frame = json!({"type":"event","id":id,"seq":seq,"final":!response_end && seq+1==count,"event":event});
        json!({"kind":"send","connection":2,"raw":serde_json::to_string(&frame).expect("#1058 event JSON"),"frame":frame})
    }).collect();
    if response_end {
        let frame = json!({"type":"response","id":id,"ok":true,"result":{}});
        frames.push(json!({"kind":"send","connection":2,"raw":serde_json::to_string(&frame).expect("#1058 final response JSON"),"frame":frame}));
    }
    rows.splice(index..index, frames);
}

#[tokio::test(flavor = "current_thread")]
async fn run_stdin_contract() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    for (argv, stdin, prompt) in [
        (vec!["run"], "  fixture stdin 中文\n", "fixture stdin 中文"),
        (
            vec!["run"],
            "{\"prompt\":\"json input\"}\n",
            "{\"prompt\":\"json input\"}",
        ),
        (
            vec!["run"],
            "{\"prompt\":\"first\"}\n{\"prompt\":\"second\"}\n",
            "{\"prompt\":\"first\"}\n{\"prompt\":\"second\"}",
        ),
        (
            vec!["run", "\u{feff} \t"],
            "\u{feff} fallback \u{a0}",
            "fallback",
        ),
        (vec!["run", ""], "fallback", "fallback"),
    ] {
        let mut input = case("run--argv");
        input["input"]["argv"] = json!(argv);
        input["input"]["stdinHex"] = json!(
            stdin
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        input["input"]["stdinAfterCreate"] = json!(true);
        set_prompt(&mut input, prompt);
        run_case(&input).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn run_empty_prompt_contract() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    for stdin in ["", " \n\t\u{feff}"] {
        let mut input = case("run--argv");
        input["input"]["argv"] = json!(["run", " "]);
        input["input"]["stdinHex"] = json!(
            stdin
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        input["connections"] = json!(1);
        input["wire"]
            .as_array_mut()
            .expect("#1058 empty wire")
            .retain(|row| row["connection"] == 1);
        input["stdout"] = json!("");
        input["stderr"] = json!("A prompt is required, or provide it through stdin.\n");
        input["exit"] = json!(1);
        run_case(&input).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn run_text_contract() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    for (events, expected) in [
        (
            vec![
                json!({"type":"assistant_text_delta","text":"甲"}),
                json!({"type":"assistant.message","text":"乙"}),
            ],
            "甲乙\n",
        ),
        (vec![json!({"type":"assistant_text_delta","text":""})], "\n"),
        (vec![], ""),
        (
            vec![
                json!({"type":"assistant_thinking_delta","text":"hidden"}),
                json!({"type":"subagent_text_delta","text":"visible","delta":"ignored"}),
            ],
            "visible\n",
        ),
        (
            vec![
                json!({"type":"subagent_text_delta","text":"","delta":"ignored"}),
                json!({"type":"assistant_block","kind":"text","text":"block"}),
            ],
            "block\n",
        ),
    ] {
        let mut events = events;
        events.push(json!({"type":"turn_completed","result":"must not print","usage":{}}));
        let mut input = case("run--argv");
        input["stdout"] = json!(expected);
        set_events(&mut input, events, false);
        run_case(&input).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn run_waits_for_stream_final() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    let mut input = case("run--argv");
    // Tail text after terminal is a deterministic consumer acknowledgement:
    // an early terminal return cannot produce this byte sequence.
    set_events(
        &mut input,
        vec![
            json!({"type":"turn_completed"}),
            json!({"type":"subagent_text_delta","text":"tail"}),
        ],
        false,
    );
    input["stdout"] = json!("tail\n");
    run_case(&input).await;
}

#[tokio::test(flavor = "current_thread")]
async fn run_success_response_ends_stream() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    let mut input = case("run--argv");
    set_events(
        &mut input,
        vec![
            json!({"type":"assistant_text_delta","text":"done"}),
            json!({"type":"turn_completed"}),
        ],
        true,
    );
    input["stdout"] = json!("done\n");
    run_case(&input).await;
}

#[tokio::test(flavor = "current_thread")]
async fn run_duplicate_event_is_not_printed() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    let mut input = case("run--argv");
    let rows = input["wire"].as_array_mut().expect("#1058 duplicate wire");
    let index = rows
        .iter()
        .position(|row| row["frame"]["seq"] == 2)
        .expect("#1058 subagent row");
    rows.insert(index + 1, rows[index].clone());
    run_case(&input).await;
}

#[tokio::test(flavor = "current_thread")]
async fn run_unimplemented_options_are_explicit() {
    let _port = PORT.lock().await;
    let _lease = ParityLock::acquire().await;
    for (flags, gap) in [
        (vec!["--json"], "pilotdeck run JSON output (#123 P07)"),
        (
            vec!["--stream-json"],
            "pilotdeck run JSON output (#123 P07)",
        ),
        (
            vec!["--json", "--stream-json"],
            "pilotdeck run JSON output (#123 P07)",
        ),
        (
            vec!["--model", "fixture/model"],
            "pilotdeck run model without project (#123 P06/P08)",
        ),
    ] {
        let mut input = case("run--argv");
        input["input"]["argv"]
            .as_array_mut()
            .expect("#1058 option argv")
            .extend(flags.into_iter().map(|v| json!(v)));
        input["stdout"] = json!("");
        input["stderr"] = json!(format!(
            "pilotdeck-rs TODO: {gap} is not implemented yet.\n"
        ));
        input["exit"] = json!(2);
        input["connections"] = json!(0);
        run_case(&input).await;
    }
}

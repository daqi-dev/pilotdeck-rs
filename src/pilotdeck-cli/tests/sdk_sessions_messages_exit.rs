//! #1028: replay 39 D1 and 13 supplemental real Node v22.22.0 captures.
//! Runtime, wire replay, and bounded parity lease reuse the P02 test helpers.
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

const ORACLE: &str = include_str!("fixtures/sdk-sessions-messages/oracle.json");
const ADDRESS: &str = "127.0.0.1:28999";
const URL: &str = "ws://127.0.0.1:28999/ws";
const BOUND: Duration = Duration::from_secs(15);
static PORT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// Bind the negative verdict to the current libtest and exact assertion; arbitrary
// server/CLI text cannot forge a kill. Uses negative_suite.ReceiptRun's protocol.
fn verify(condition: bool, assertion: &str) {
    if condition {
        return;
    }
    if let Ok(config) = std::env::var("PDRS_NEGATIVE_RECEIPTS") {
        let config: Value = serde_json::from_str(&config).expect("#1028 receipt configuration");
        let thread = std::thread::current();
        let name = thread.name().expect("#1028 named test");
        for target in config["targets"].as_array().expect("#1028 receipt targets") {
            if target["test"] == name && target["assertion"] == assertion {
                let path = Path::new(target["path"].as_str().expect("#1028 receipt path"));
                let temporary = path.with_extension("tmp");
                let mut receipt = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)
                    .expect("#1028 fresh receipt");
                receipt
                    .write_all(
                        config["nonce"]
                            .as_str()
                            .expect("#1028 receipt nonce")
                            .as_bytes(),
                    )
                    .expect("#1028 receipt write");
                fs::rename(temporary, path).expect("#1028 atomic receipt publication");
            }
        }
    }
    panic!("{assertion}");
}

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-1028-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).expect("#1028 exclusive runtime");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("#1028 runtime mode");
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
            fs::create_dir(root.join(dir)).expect("#1028 fixture directory");
            fs::set_permissions(root.join(dir), fs::Permissions::from_mode(0o700))
                .expect("#1028 directory mode");
        }
        for (file, bytes) in [
            ("pilot/pilotdeck.yaml", &b"schemaVersion: 1\n"[..]),
            ("project/keep.txt", &b"D0 fixture\0\xff\n"[..]),
            ("openssl.cnf", &b""[..]),
        ] {
            fs::write(root.join(file), bytes).expect("#1028 fixture file");
            fs::set_permissions(root.join(file), fs::Permissions::from_mode(0o600))
                .expect("#1028 file mode");
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
        for (key, value) in input["env"].as_object().expect("#1028 input env") {
            command.env(key, value.as_str().expect("#1028 env string"));
        }
        if let Some(unset) = input["unsetEnv"].as_array() {
            for key in unset {
                command.env_remove(key.as_str().expect("#1028 unset key"));
            }
        }
        if let Some(token) = input["tokenFile"].as_str() {
            fs::write(self.0.join("pilot/server-token"), token).expect("#1028 token fixture");
            fs::set_permissions(
                self.0.join("pilot/server-token"),
                fs::Permissions::from_mode(0o644),
            )
            .expect("#1028 token mode");
        }
        for arg in input["argv"].as_array().expect("#1028 input argv") {
            command.arg(arg.as_str().expect("#1028 argv string"));
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
        Err(error) => panic!("#1075 fixture stdin: {error:?}"),
    }
}

fn spawn(command: &mut Command, input: &Value) -> OwnedChild {
    let mut owned = OwnedChild(Some(command.spawn().expect("#1028 spawn CLI")));
    let child = owned.0.as_mut().expect("#1028 immediately owned child");
    let hex = input["stdinHex"].as_str().unwrap_or("");
    let data: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("#1028 stdin hex"))
        .collect();
    let mut stdin = child.stdin.take().expect("#1028 stdin pipe");
    write_fixture_stdin(&mut stdin, &data);
    drop(stdin);
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
    let child =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| spawn(&mut command, &input)));
    let child = child.unwrap_or_else(|error| {
        // Only a write-side EPIPE is this regression. Setup/collection failures
        // must not publish the negative control's assertion receipt.
        let broken_pipe = error.downcast_ref::<String>().is_some_and(|message| {
            message.starts_with("#1075 fixture stdin:") && message.contains("kind: BrokenPipe")
        });
        verify(!broken_pipe, "#1075 broken pipe must not panic");
        std::panic::resume_unwind(error)
    });
    let output = collect(child).await;
    verify(output.status.code() == Some(23), "#1075 preserve exit code");
    verify(output.stdout == b"fixture-out", "#1075 preserve stdout");
    verify(output.stderr == b"fixture-err", "#1075 preserve stderr");
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
                .expect("#1028 owned child")
                .try_wait()
                .expect("#1028 poll CLI")
                .is_some()
            {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("#1028 bounded CLI exit");
    child
        .0
        .take()
        .expect("#1028 reap owned CLI")
        .wait_with_output()
        .expect("#1028 collect output")
}
fn snapshot(root: &Path) -> Value {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<String, Value>) {
        let metadata = fs::symlink_metadata(path).expect("#1028 snapshot metadata");
        let relative = path
            .strip_prefix(root)
            .expect("#1028 snapshot contained path");
        let name = if relative.as_os_str().is_empty() {
            ".".into()
        } else {
            relative.to_string_lossy().into_owned()
        };
        let mode = metadata.permissions().mode() & 0o777;
        let record = if metadata.is_dir() {
            json!({"type":"directory","mode":mode})
        } else if metadata.is_file() {
            let bytes = fs::read(path).expect("#1028 snapshot bytes");
            json!({"type":"file","mode":mode,"size":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))})
        } else {
            panic!("#1028 unexpected fixture node: {name}");
        };
        entries.insert(name, record);
        if metadata.is_dir() {
            for entry in fs::read_dir(path).expect("#1028 snapshot directory") {
                visit(root, &entry.expect("#1028 snapshot entry").path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    serde_json::to_value(entries).expect("#1028 snapshot JSON")
}

async fn replay(listener: TcpListener, case: &Value, pid: u32) {
    let (stream, _) = listener.accept().await.expect("#1028 owned mock accept");
    let mut socket = accept_async(stream).await.expect("#1028 WS upgrade");
    let mut id = String::new();
    for row in case["wire"].as_array().expect("#1028 wire rows") {
        if row["kind"] == "receive" {
            let message = socket
                .next()
                .await
                .expect("#1028 wire frame exists")
                .expect("#1028 readable wire frame");
            let mut frame: Value =
                serde_json::from_str(message.to_text().expect("#1028 text wire frame"))
                    .expect("#1028 JSON wire frame");
            if frame["type"] == "request" {
                id = frame["id"]
                    .as_str()
                    .expect("#1028 request id string")
                    .to_owned();
                verify(
                    uuid::Uuid::parse_str(&id).is_ok(),
                    &format!(
                        "#1028 request UUID {}",
                        case["input"]["id"].as_str().expect("#1028 cell id")
                    ),
                );
                frame["id"] = "<request>".into();
            }
            verify(
                frame == row["frame"],
                &format!(
                    "#1028 hello and request wire {}",
                    case["input"]["id"].as_str().expect("#1028 cell id")
                ),
            );
        } else {
            let raw = row["raw"]
                .as_str()
                .expect("#1028 original wire JSON")
                .replace("<request>", &id);
            socket
                .send(Message::Text(raw.into()))
                .await
                .expect("#1028 mock response");
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
            .expect("#1028 auth close");
    }
    if let Some(signal) = signal {
        let status = Command::new("/bin/kill")
            .args(["-s", signal, &pid.to_string()])
            .status()
            .expect("#1028 send signal after request");
        assert!(status.success(), "#1028 signal delivered");
    }
    // Success/error gets a close frame; signal termination releases TCP without
    // a handshake. In both cases consume to EOF to prove no retained connection.
    let mut closed = false;
    loop {
        match socket.next().await {
            Some(Ok(Message::Close(frame))) => {
                closed = true;
                if rejected {
                    println!("#1028 auth close response: {frame:?}");
                }
                if !rejected {
                    verify(
                        frame.is_none(),
                        &format!(
                            "#1028 empty normal close frame {}",
                            case["input"]["id"].as_str().expect("#1028 cell id")
                        ),
                    );
                }
                let _ = socket.flush().await;
            }
            None | Some(Err(_)) => break,
            Some(Ok(_)) => panic!("#1028 unexpected frame after terminal RPC"),
        }
    }
    verify(
        signal.is_some() || closed,
        &format!(
            "#1028 terminal connection closed {}",
            case["input"]["id"].as_str().expect("#1028 cell id")
        ),
    );
    // The listener remains owned throughout: any extra connection is a failure.
    verify(
        timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err(),
        &format!(
            "#1028 no reconnect {}",
            case["input"]["id"].as_str().expect("#1028 cell id")
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
            .expect("#1028 D0 parity lease acquisition failed (bounded wait; no reclaim)")
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
                    format!("#1028 parity lease deadline: {}", path.display()),
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
                        eprintln!("#1028 waiting for parity lease: {}", path.display());
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
            return Err(io::Error::other("#1028 unsafe parity guard"));
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
            eprintln!("#1028 parity lease release failed; retained lease: {error}");
            assert!(
                std::thread::panicking(),
                "#1028 parity lease release failed"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn sessions_messages_matches_ts_matrix() {
    let _port = PORT.lock().await;
    let _lock = ParityLock::acquire().await;
    let oracle: Value = serde_json::from_str(ORACLE).expect("#1028 TS oracle JSON");
    let cases = oracle["cases"].as_array().expect("#1028 oracle cells");
    verify(
        cases.len() == 52,
        "#1028 39 registered messages cells plus 13 live shape and ID cells",
    );
    for case in cases {
        let runtime = Runtime::new();
        let mut command = runtime.command(&case["input"]);
        let before = snapshot(&runtime.0);
        assert_eq!(
            before, case["disk"],
            "#1028 same initial fixture: {}",
            case["input"]["id"]
        );
        let listener = if case["input"]["mockGateway"] == true {
            Some(
                TcpListener::bind(ADDRESS)
                    .await
                    .expect("#1028 exclusively bind D0 mock 28999"),
            )
        } else {
            None
        };
        let child = spawn(&mut command, &case["input"]);
        let pid = child.0.as_ref().expect("#1028 child PID").id();
        let output = if case["connections"] == 1 {
            let mock = replay(listener.expect("#1028 observed mock"), case, pid);
            let ((), output) = tokio::join!(
                async {
                    verify(
                        timeout(BOUND, mock).await.is_ok(),
                        &format!(
                            "#1028 expected gateway lifecycle {}",
                            case["input"]["id"].as_str().expect("#1028 cell id")
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
                        "#1028 preflight never connects {}",
                        case["input"]["id"].as_str().expect("#1028 cell id")
                    ),
                );
            }
            output
        };
        let matches = output.stdout == case["stdout"].as_str().expect("#1028 TS stdout").as_bytes()
            && output.stderr == case["stderr"].as_str().expect("#1028 TS stderr").as_bytes()
            && output.status.code().map(i64::from) == case["exit"].as_i64()
            && output.status.signal().map(i64::from) == case["signal"].as_i64();
        if !matches {
            eprintln!(
                "#1028 {}: {:?}, stdout={:?}, stderr={:?}",
                case["input"]["id"],
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        verify(
            matches,
            &format!(
                "#1028 TS output and exit {}",
                case["input"]["id"].as_str().expect("#1028 cell id")
            ),
        );
        verify(
            snapshot(&runtime.0) == before,
            &format!(
                "#1028 complete disk unchanged {}",
                case["input"]["id"].as_str().expect("#1028 cell id")
            ),
        );
        println!("#1028 observed {}", case["input"]["id"]);
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
        .expect("#1028 run selected Node --version (NODE22 or PATH node)");
    verify(
        version.status.success(),
        "#1028 selected Node --version must exit successfully",
    );
    verify(
        version.stdout == b"v22.22.0\n",
        "#1028 selected Node must report v22.22.0 before extraction",
    );
    let mut command = Command::new(&node);
    command.env_remove("PDRS_D1_PLAN");
    let result = command
        .arg(directory.join("generate.mjs"))
        .arg(&output)
        .output()
        .expect("#1028 run extraction guard");
    if !result.status.success() {
        eprintln!(
            "#1028 extraction failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    verify(
        result.status.success()
            && fs::read(output.join("oracle.json")).expect("#1028 regenerated oracle")
                == fs::read(directory.join("oracle.json")).expect("#1028 checked-in oracle"),
        &format!("#1028 identical TS fixture {fixture}"),
    );
}
#[test]
#[ignore = "requires Node v22.22.0; extracts checked-in captures, never reruns TS"]
fn sessions_messages_oracle_regenerates_identically() {
    regenerates_identically("sdk-sessions-messages");
}

#[test]
#[ignore = "requires Node v22.22.0; corrupts private capture inputs, never reruns TS"]
fn sessions_messages_extraction_rejects_unbound_evidence() {
    let node = std::env::var_os("NODE22").unwrap_or_else(|| "node".into());
    let version = Command::new(&node)
        .arg("--version")
        .output()
        .expect("#1028 integrity guard Node version");
    verify(
        version.status.success() && version.stdout == b"v22.22.0\n",
        "#1028 integrity guard requires Node v22.22.0",
    );
    let original: Value =
        serde_json::from_str(include_str!("fixtures/sdk-sessions-messages/source.json"))
            .expect("#1028 integrity original source");
    // Preserve raw values: the observed wire includes a lone UTF-16 surrogate,
    // which serde_json::Value deliberately cannot materialize as a Rust String.
    type RawObject = BTreeMap<String, Box<serde_json::value::RawValue>>;
    let supplemental: RawObject = serde_json::from_str(
        original["supplemental.json"]
            .as_str()
            .expect("#1028 integrity supplemental bytes"),
    )
    .expect("#1028 integrity supplemental JSON");
    let extract = |source: &Value| {
        let runtime = Runtime::new();
        fs::write(
            runtime.0.join("generate.mjs"),
            include_str!("fixtures/sdk-sessions-messages/generate.mjs"),
        )
        .expect("#1028 private generator");
        fs::write(
            runtime.0.join("source.json"),
            serde_json::to_vec(source).expect("#1028 private source serialization"),
        )
        .expect("#1028 private source");
        let output = runtime.0.join("extracted");
        let result = Command::new(&node)
            .arg(runtime.0.join("generate.mjs"))
            .arg(&output)
            .output()
            .expect("#1028 private extraction");
        (result, fs::read(output.join("oracle.json")).ok())
    };
    let positive = || {
        let (result, oracle) = extract(&original);
        verify(
            result.status.success() && oracle.as_deref() == Some(ORACLE.as_bytes()),
            "#1028 integrity positive extracts identical fixture",
        );
    };
    positive();
    let mut cases = Vec::new();
    for file in [
        "dist/src/cli/pilotdeck.js",
        "dist/src/cli/sdkCli.js",
        "packages/sdk/dist/index.js",
        "packages/sdk/dist/client.js",
    ] {
        for invalid in [false, true] {
            let mut changed = supplemental.clone();
            let mut hashes: Value = serde_json::from_str(
                changed
                    .get("sourceSha256")
                    .expect("#1028 source hashes exist")
                    .get(),
            )
            .expect("#1028 recorded source hashes");
            if invalid {
                hashes[file] = "not-a-sha256".into();
            } else {
                hashes
                    .as_object_mut()
                    .expect("#1028 provenance object")
                    .remove(file);
            }
            changed.insert(
                "sourceSha256".into(),
                serde_json::value::to_raw_value(&hashes).expect("#1028 changed source hashes"),
            );
            let mut source = original.clone();
            source["supplemental.json"] = serde_json::to_string(&changed)
                .expect("#1028 changed supplemental provenance")
                .into();
            let kind = if invalid { "invalid" } else { "missing" };
            cases.push((
                format!("{kind} provenance {file}"),
                source,
                format!("#1028 compiled provenance {file}"),
            ));
        }
    }
    let host = "raw/host-b10/cases/sessions-messages--argv/events.json";
    for supplemental_case in [false, true] {
        for stream in ["stdout", "stderr"] {
            let mut source = original.clone();
            let mut changed = supplemental.clone();
            let mut captures: Vec<RawObject> = serde_json::from_str(
                changed
                    .get("cases")
                    .expect("#1028 supplemental captures exist")
                    .get(),
            )
            .expect("#1028 raw supplemental captures");
            let mut events: Value = if supplemental_case {
                serde_json::from_str(
                    captures
                        .first()
                        .expect("#1028 first supplemental capture")
                        .get("events")
                        .expect("#1028 supplemental events exist")
                        .get(),
                )
                .expect("#1028 supplemental events JSON")
            } else {
                serde_json::from_str(source[host].as_str().expect("#1028 host events bytes"))
                    .expect("#1028 host events JSON")
            };
            let event = events
                .as_array_mut()
                .expect("#1028 recorded events array")
                .iter_mut()
                .find(|event| event["stream"] == stream)
                .expect("#1028 recorded stream event");
            event["bytesBase64"] = "".into();
            let (kind, id) = if supplemental_case {
                captures
                    .first_mut()
                    .expect("#1028 first supplemental capture to change")
                    .insert(
                        "events".into(),
                        serde_json::value::to_raw_value(&events)
                            .expect("#1028 changed stream events"),
                    );
                changed.insert(
                    "cases".into(),
                    serde_json::value::to_raw_value(&captures).expect("#1028 changed captures"),
                );
                source["supplemental.json"] = serde_json::to_string(&changed)
                    .expect("#1028 changed supplemental events")
                    .into();
                ("supplemental", "sessions-messages--shape-complete")
            } else {
                source[host] = events.to_string().into();
                ("host", "sessions-messages--argv")
            };
            cases.push((
                format!("{kind} {stream} events"),
                source,
                format!("#1028 raw {stream}/events bytes {id}"),
            ));
        }
    }
    for (label, source, expected) in cases {
        let (result, oracle) = extract(&source);
        verify(
            !result.status.success()
                && String::from_utf8_lossy(&result.stderr).contains(&expected)
                && oracle.is_none(),
            &format!("#1028 extraction rejects {label}"),
        );
        println!("#1028 extraction rejects {label}: {expected}");
    }
    positive();
}

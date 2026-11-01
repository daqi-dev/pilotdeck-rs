//! Shared sdk exit-harness pieces (#1044): exclusive runtime fixture, owned
//! child with bounded stdin writers, output collection and disk snapshots.
//! Test entry points stay in each `sdk_*_exit.rs` so libtest keeps bare names
//! for the negative-control receipt protocol; only the bodies live here.
#[path = "negative_receipt.rs"]
mod negative_receipt;
pub(crate) use negative_receipt::verify;

use futures_util::FutureExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    time::{sleep, timeout},
};

pub(crate) const URL: &str = "ws://127.0.0.1:28999/ws";
pub(crate) const BOUND: Duration = Duration::from_secs(15);

pub(crate) struct Runtime(pub(crate) PathBuf);
impl Runtime {
    pub(crate) fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-sdk-exit-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).expect("#1044 exclusive runtime");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("#1044 runtime mode");
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
            fs::create_dir(root.join(dir)).expect("#1044 fixture directory");
            fs::set_permissions(root.join(dir), fs::Permissions::from_mode(0o700))
                .expect("#1044 directory mode");
        }
        for (file, bytes) in [
            ("pilot/pilotdeck.yaml", &b"schemaVersion: 1\n"[..]),
            ("project/keep.txt", &b"D0 fixture\0\xff\n"[..]),
            ("openssl.cnf", &b""[..]),
        ] {
            fs::write(root.join(file), bytes).expect("#1044 fixture file");
            fs::set_permissions(root.join(file), fs::Permissions::from_mode(0o600))
                .expect("#1044 file mode");
        }
        Self(root)
    }
    pub(crate) fn command(&self, input: &Value) -> Command {
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
        for (key, value) in input["env"].as_object().expect("#1044 input env") {
            command.env(key, value.as_str().expect("#1044 env string"));
        }
        if let Some(unset) = input["unsetEnv"].as_array() {
            for key in unset {
                command.env_remove(key.as_str().expect("#1044 unset key"));
            }
        }
        if let Some(token) = input["tokenFile"].as_str() {
            fs::write(self.0.join("pilot/server-token"), token).expect("#1044 token fixture");
            fs::set_permissions(
                self.0.join("pilot/server-token"),
                fs::Permissions::from_mode(0o644),
            )
            .expect("#1044 token mode");
        }
        for arg in input["argv"].as_array().expect("#1044 input argv") {
            command.arg(arg.as_str().expect("#1044 argv string"));
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
pub(crate) type Writers = std::sync::Arc<std::sync::Mutex<Vec<StdinWriter>>>;
pub(crate) struct OwnedChild(pub(crate) Option<Child>, pub(crate) Writers);
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
pub(crate) fn write_fixture_stdin(stdin: &mut impl Write, data: &[u8]) {
    match stdin.write_all(data) {
        Ok(()) => {}
        // Local preflight failures may exit without consuming any input.
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
        Err(error) => panic!("#1075 fixture stdin: {error:?}"),
    }
}

// Writers run independently of the current-thread Tokio mock. Completion is
// bounded and owned by OwnedChild, which closes the reader before joining.
pub(crate) struct StdinWriter {
    handle: std::thread::JoinHandle<()>,
    done: std::sync::mpsc::Receiver<std::thread::Result<()>>,
    joined: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl StdinWriter {
    pub(crate) fn start(mut stdin: impl Write + Send + 'static, data: Vec<u8>) -> Self {
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

// The hook belongs only to the thread invoking spawn. Ordinary replays leave
// it empty; the wrapper reports the first actual write, including inline writes.
type StdinObservation = (std::thread::ThreadId, Vec<u8>);
thread_local! {
    static STDIN_OBSERVER: std::cell::RefCell<Option<std::sync::mpsc::Sender<StdinObservation>>> = const { std::cell::RefCell::new(None) };
}
struct ObservedStdin {
    stdin: std::process::ChildStdin,
    entered: Option<std::sync::mpsc::Sender<StdinObservation>>,
}
impl Write for ObservedStdin {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(entered) = self.entered.take() {
            entered
                .send((std::thread::current().id(), bytes.to_vec()))
                .expect("#1136 publish spawn first pipe write");
        }
        self.stdin.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stdin.flush()
    }
}

pub(crate) fn spawn(command: &mut Command, input: &Value) -> OwnedChild {
    let mut owned = OwnedChild(
        Some(command.spawn().expect("#1136 spawn CLI")),
        Writers::default(),
    );
    let child = owned.0.as_mut().expect("#1136 immediately owned child");
    let hex = input["stdinHex"].as_str().unwrap_or("");
    let data: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("#1136 stdin hex"))
        .collect();
    if input["holdStdin"] != true && input["stdinAfterCreate"] != true {
        let stdin = ObservedStdin {
            stdin: child.stdin.take().expect("#1136 stdin pipe"),
            entered: STDIN_OBSERVER.with(|observer| observer.borrow_mut().take()),
        };
        owned
            .1
            .lock()
            .expect("#1136 register early stdin")
            .push(StdinWriter::start(stdin, data));
    }
    owned
}

pub(crate) async fn collect(child: OwnedChild) -> std::process::Output {
    collect_bounded(child, BOUND).await
}
pub(crate) async fn collect_bounded(
    mut child: OwnedChild,
    bound: Duration,
) -> std::process::Output {
    collect_owned(&mut child, bound).await
}
// Retain the registry for writers registered by replay after process exit.
pub(crate) async fn collect_owned(child: &mut OwnedChild, bound: Duration) -> std::process::Output {
    timeout(bound, async {
        loop {
            if child
                .0
                .as_mut()
                .expect("#1136 owned child")
                .try_wait()
                .expect("#1136 poll CLI")
                .is_some()
            {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("#1136 bounded CLI exit");
    child
        .0
        .take()
        .expect("#1136 reap owned CLI")
        .wait_with_output()
        .expect("#1136 collect output")
}
// After child exit every unobserved connection is in the retained backlog.
pub(crate) async fn verify_no_pending_connections(listener: &TcpListener, reason: &str) {
    if let Ok(accepted) = timeout(Duration::from_millis(20), listener.accept()).await {
        let (_, peer) = accepted.expect("#1136 owned extra accept");
        eprintln!("#1136 extra connection after captured wire: {peer}");
        verify(false, reason);
    }
}

pub(crate) fn snapshot(root: &Path) -> Value {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<String, Value>) {
        let metadata = fs::symlink_metadata(path).expect("#1044 snapshot metadata");
        let relative = path
            .strip_prefix(root)
            .expect("#1044 snapshot contained path");
        let name = if relative.as_os_str().is_empty() {
            ".".into()
        } else {
            relative.to_string_lossy().into_owned()
        };
        let mode = metadata.permissions().mode() & 0o777;
        let record = if metadata.is_dir() {
            json!({"type":"directory","mode":mode})
        } else if metadata.is_file() {
            let bytes = fs::read(path).expect("#1044 snapshot bytes");
            json!({"type":"file","mode":mode,"size":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))})
        } else {
            panic!("#1044 unexpected fixture node: {name}");
        };
        entries.insert(name, record);
        if metadata.is_dir() {
            for entry in fs::read_dir(path).expect("#1044 snapshot directory") {
                visit(root, &entry.expect("#1044 snapshot entry").path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    serde_json::to_value(entries).expect("#1044 snapshot JSON")
}

pub(crate) fn fixture_stdin_writes_all_bytes() {
    let data = b"fixture input\0\xff\n";
    let mut written = Vec::new();
    write_fixture_stdin(&mut written, data);
    verify(written == data, "#1075 complete stdin write");
}

pub(crate) fn fixture_stdin_other_errors_still_fail() {
    // An exhausted slice returns WriteZero, not BrokenPipe.
    let result = std::panic::catch_unwind(|| {
        let mut writer = &mut [0_u8; 0][..];
        write_fixture_stdin(&mut writer, b"fixture input");
    });
    verify(result.is_err(), "#1075 other stdin errors must fail");
}

pub(crate) async fn fixture_stdin_broken_pipe_preserves_output() {
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

pub(crate) async fn fixture_stdin_writer_yields_before_child_reads() {
    // This is the real spawn entry, including hex decoding and registration.
    // The child never reads stdin. A separate socket gates its exit, so even
    // an inline writer is released after its first-write event, before verdict.
    let (mut release, reader_release) =
        std::os::unix::net::UnixStream::pair().expect("#1136 blocked reader release socket");
    release
        .set_write_timeout(Some(BOUND))
        .expect("#1136 bounded reader release");
    let mut command = Command::new("/bin/sh");
    command
        .env_clear()
        .args(["-c", "read release <&1; exit 23"])
        .stdin(Stdio::piped())
        .stdout(Stdio::from(std::os::fd::OwnedFd::from(reader_release)))
        .stderr(Stdio::piped());
    let input = json!({"stdinHex": "00ff78c3".repeat(512 * 1024)});
    let (entered, observed) = std::sync::mpsc::channel();
    let (returned, spawned) = std::sync::mpsc::channel();
    let starter = std::thread::spawn(move || {
        STDIN_OBSERVER.with(|observer| *observer.borrow_mut() = Some(entered));
        let child = spawn(&mut command, &input);
        returned.send(child).expect("#1136 publish spawn return");
    });
    let caller = starter.thread().id();
    let first_write = observed.recv_timeout(BOUND);
    // Release even on a missing progress event. Never turn a timeout/setup
    // panic into the receipt for a synchronous writer regression.
    let released = release.write_all(b"exit\n");
    drop(release);
    let mut child = spawned.recv_timeout(BOUND).expect("#1136 spawn returned");
    starter.join().expect("#1136 spawn caller joined");
    let output = collect_owned(&mut child, BOUND).await;
    let writers = child.1.clone();
    let registered = writers.lock().expect("#1136 spawn registry").len();
    drop(child);
    released.expect("#1136 release blocked reader");
    let (writer, bytes) = first_write.expect("#1136 spawn first pipe write progress");
    verify(
        writer != caller,
        "#1136 stdin writer yields before child reads",
    );
    verify(
        bytes == [0x00, 0xff, 0x78, 0xc3].repeat(512 * 1024),
        "#1136 spawn decodes complete stdinHex",
    );
    verify(registered == 1, "#1136 spawn registers stdin writer");
    verify(
        output.status.code() == Some(23),
        "#1136 blocked reader exit 23 preserved",
    );
}

pub(crate) async fn fixture_stdin_deferred_until_requested() {
    for (hold, delayed) in [(true, false), (false, true), (true, true)] {
        let mut command = Command::new("/bin/sh");
        command
            .env_clear()
            .args(["-c", "exit 23"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = spawn(
            &mut command,
            &json!({"stdinHex": "00ff78c3", "holdStdin": hold, "stdinAfterCreate": delayed}),
        );
        let retained = child
            .0
            .as_ref()
            .expect("#1136 deferred child")
            .stdin
            .is_some();
        let registered = child.1.lock().expect("#1136 deferred registry").len();
        let output = collect(child).await;
        verify(
            retained && registered == 0,
            "#1136 hold or delayed stdin stays with child",
        );
        assert_eq!(output.status.code(), Some(23), "#1136 deferred child exit");
    }
}

pub(crate) fn fixture_stdin_cancel_reaps_writer() {
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

pub(crate) async fn fixture_stdin_registered_after_exit_is_joined() {
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

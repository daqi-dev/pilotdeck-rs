//! Diagnostic-only observer and opt-in real-native/default-TS recording.
//! COMPLETE is distinct from JoinSet notification and from join consumption.
use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    pin::Pin,
    process::{Child, Command, Stdio},
    sync::Mutex as StdMutex,
    task::{Context, Poll, ready},
    time::Duration,
};
use tokio::{io::ReadBuf, task::AbortHandle, time::timeout};

const LIMIT: Duration = Duration::from_secs(8);
#[path = "ready_admission.rs"]
mod ready_admission;
const SCRIPT_NAMES: &[&str] = &[
    "run.mjs",
    "guard.mjs",
    "probe.mjs",
    "protocol.mjs",
    "offline.mjs",
    "fixture.mjs",
    "stdio-host.mjs",
];

pub(super) fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Default)]
struct State {
    events: Vec<Value>,
    handles: Vec<(String, AbortHandle)>,
}

#[derive(Clone, Default)]
pub(super) struct Observer(Option<Arc<StdMutex<State>>>);

tokio::task_local! { static ACTIVE: Observer; }

pub(super) fn current() -> Observer {
    ACTIVE.try_with(Clone::clone).unwrap_or_default()
}

fn push(state: &mut State, event: &str, fields: Value) {
    let mut fields = fields.as_object().unwrap().clone();
    fields.insert("event".into(), event.into());
    fields.insert("eventIndex".into(), (state.events.len() + 1).into());
    state.events.push(Value::Object(fields));
}

impl Observer {
    pub(super) fn new() -> Self {
        Self(Some(Arc::new(StdMutex::new(State::default()))))
    }

    pub(super) fn event(&self, event: &str, fields: Value) {
        if let Some(state) = &self.0 {
            push(&mut state.lock().unwrap(), event, fields);
        }
    }

    pub(super) fn parsed(&self, frame: &frame::Frame) {
        if self.0.is_none() {
            return;
        }
        self.sample("before_next_frame");
        self.event(
            "input_frame_parsed",
            json!({"opcode":frame.opcode,"wireHex":hex(&frame.wire)}),
        );
    }

    pub(super) fn job(&self, frame: &frame::Frame) -> Job {
        if self.0.is_none() {
            return Job {
                observer: self.clone(),
                id: String::new(),
                method: String::new(),
            };
        }
        let value = dispatch::parse(&frame.payload);
        Job {
            observer: self.clone(),
            id: value
                .as_ref()
                .and_then(|v| crate::metadata::text_field(v, "id"))
                .unwrap_or_default(),
            method: value
                .as_ref()
                .and_then(|v| crate::metadata::text_field(v, "method"))
                .unwrap_or_default(),
        }
    }

    pub(super) fn sample(&self, at: &str) {
        if let Some(state) = &self.0 {
            let mut state = state.lock().unwrap();
            let observations: Vec<_> = state
                .handles
                .iter()
                .map(|(id, handle)| (id.clone(), handle.id().to_string(), handle.is_finished()))
                .collect();
            for (id, task, complete) in observations {
                push(
                    &mut state,
                    "task_complete_sample",
                    json!({"requestId":id,"taskId":task,"at":at,"complete":complete,"joinsetConsumable":"NOT_OBSERVED"}),
                );
            }
        }
    }

    pub(super) fn joined(&self, frame: &frame::Frame) {
        let job = self.job(frame);
        if let Some(state) = &self.0 {
            let mut state = state.lock().unwrap();
            state.handles.retain(|(id, _)| id != &job.id);
            push(
                &mut state,
                "join_consumed",
                json!({"requestId":job.id,"method":job.method}),
            );
        }
    }

    pub(super) fn snapshot(&self) -> Vec<Value> {
        self.0.as_ref().unwrap().lock().unwrap().events.clone()
    }
}

#[derive(Clone)]
pub(super) struct Job {
    observer: Observer,
    id: String,
    method: String,
}

impl Job {
    pub(super) fn result_ready(&self, route: &Route) {
        let result = match route {
            Route::Forward => json!({"route":"forward"}),
            Route::LocalResponse(bytes) => json!({"route":"local","actualResultHash":hash(bytes)}),
        };
        self.observer.event("route_result_ready",json!({"requestId":self.id,"method":self.method,"result":result,"taskComplete":"NOT_OBSERVED"}));
    }

    pub(super) fn spawned(&self, handle: AbortHandle) {
        if let Some(state) = &self.observer.0 {
            let mut state = state.lock().unwrap();
            push(
                &mut state,
                "blocking_job_registered",
                json!({"requestId":self.id,"method":self.method,"taskId":handle.id().to_string()}),
            );
            state.handles.push((self.id.clone(), handle));
        }
    }
}

struct TracedFiles(Observer);
impl crate::metadata::Files for TracedFiles {
    fn entries(&self, path: &Path) -> io::Result<Vec<(String, bool)>> {
        let result = crate::metadata::NativeFiles.entries(path);
        self.0.event("native_files_api_return",json!({"operation":"entries","path":path,"actualResultHash":hash(format!("{result:?}")),"osCompletion":"NOT_OBSERVED"}));
        result
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let result = crate::metadata::NativeFiles.read(path);
        self.0.event("native_files_api_return",json!({"operation":"read","path":path,"actualResultHash":hash(format!("{result:?}")),"osCompletion":"NOT_OBSERVED"}));
        result
    }
    fn stat(&self, path: &Path) -> io::Result<crate::metadata::Stat> {
        let result = crate::metadata::NativeFiles.stat(path);
        self.0.event("native_files_api_return",json!({"operation":"stat","path":path,"actualResultHash":hash(format!("{result:?}")),"osCompletion":"NOT_OBSERVED"}));
        result
    }
    fn session_summary(&self, path: &Path, size: u64) -> Result<bool, ()> {
        let result = crate::metadata::NativeFiles.session_summary(path, size);
        self.0.event("native_files_api_return",json!({"operation":"session_summary","path":path,"actualResultHash":hash(format!("{result:?}")),"osCompletion":"NOT_OBSERVED"}));
        result
    }
}

struct Traced<T> {
    io: T,
    observer: Observer,
    name: &'static str,
}
impl<T: AsyncRead + Unpin> AsyncRead for Traced<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let start = buf.filled().len();
        let result = ready!(Pin::new(&mut self.io).poll_read(cx, buf));
        if result.is_ok() {
            self.observer.event(
                "transport_read",
                json!({"stream":self.name,"wireHex":hex(&buf.filled()[start..])}),
            );
        }
        Poll::Ready(result)
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for Traced<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let count = ready!(Pin::new(&mut self.io).poll_write(cx, buf))?;
        self.observer.event(
            "transport_write",
            json!({"stream":self.name,"wireHex":hex(&buf[..count]),"bytesAccepted":count,"observation":"ASYNC_WRITE_ACCEPTED_BYTES"}),
        );
        Poll::Ready(Ok(count))
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

// AsyncFd wraps existing child pipes only; no socket/listener or unsafe IO.
struct Pipe {
    reader: tokio::io::unix::AsyncFd<fs::File>,
    writer: tokio::io::unix::AsyncFd<fs::File>,
}
fn nonblocking(fd: std::os::fd::OwnedFd) -> io::Result<tokio::io::unix::AsyncFd<fs::File>> {
    let file = fs::File::from(fd);
    let flags = rustix::fs::fcntl_getfl(&file)?;
    rustix::fs::fcntl_setfl(&file, flags | rustix::fs::OFlags::NONBLOCK)?;
    tokio::io::unix::AsyncFd::new(file)
}
impl AsyncRead for Pipe {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            let mut guard = ready!(self.reader.poll_read_ready(cx))?;
            match guard.try_io(|fd| {
                rustix::io::read(fd.get_ref(), buf.initialize_unfilled()).map_err(io::Error::from)
            }) {
                Ok(Ok(n)) => {
                    buf.advance(n);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                Err(_) => continue,
            }
        }
    }
}
impl AsyncWrite for Pipe {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut guard = ready!(self.writer.poll_write_ready(cx))?;
            match guard.try_io(|fd| rustix::io::write(fd.get_ref(), buf).map_err(io::Error::from)) {
                Ok(result) => return Poll::Ready(result),
                Err(_) => continue,
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

const HOST_LOG_LIMIT: usize = 1024 * 1024;
fn append_host_log(
    output: &mut impl std::io::Write,
    bytes: &[u8],
    total: &mut usize,
) -> io::Result<()> {
    let accepted = bytes.len().min(HOST_LOG_LIMIT - *total);
    output.write_all(&bytes[..accepted])?;
    *total += accepted;
    if accepted != bytes.len() {
        return Err(io::Error::other("owned host stderr exceeded 1MiB"));
    }
    Ok(())
}

#[test]
fn bounded_host_log_rejects_first_excess_byte() {
    let mut output = Vec::new();
    let mut total = 0;
    append_host_log(&mut output, &[], &mut total).unwrap();
    append_host_log(&mut output, &vec![b'x'; HOST_LOG_LIMIT], &mut total).unwrap();
    assert_eq!(total, HOST_LOG_LIMIT);
    assert!(append_host_log(&mut output, b"y", &mut total).is_err());
    assert_eq!(output.len(), HOST_LOG_LIMIT);
    assert_eq!(total, HOST_LOG_LIMIT);
}

struct Process {
    child: Child,
    stderr: tokio::task::JoinHandle<io::Result<usize>>,
    reap_path: PathBuf,
    stderr_path: PathBuf,
    stderr_joined: bool,
    stderr_bytes: Option<usize>,
    stderr_failure: Option<String>,
}
impl Process {
    fn new(mut child: Child, path: PathBuf) -> Self {
        let reap_path = path.with_file_name("host-reap.json");
        let stderr_path = path.clone();
        let stderr = child.stderr.take().expect("owned stderr pipe");
        let capture = tokio::spawn(async move {
            let input = nonblocking(stderr.into())?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?;
            let mut total = 0;
            let mut bytes = [0_u8; 8192];
            loop {
                let mut guard = input.readable().await?;
                let count = match guard.try_io(|fd| {
                    rustix::io::read(fd.get_ref(), &mut bytes).map_err(io::Error::from)
                }) {
                    Ok(result) => result?,
                    Err(_) => continue,
                };
                if count == 0 {
                    return Ok(total);
                }
                append_host_log(&mut output, &bytes[..count], &mut total)?;
            }
        });
        Self {
            child,
            stderr: capture,
            reap_path,
            stderr_path,
            stderr_joined: false,
            stderr_bytes: None,
            stderr_failure: None,
        }
    }

    // ReadyAdmission may accept host health only after the actual capture task
    // has completed successfully. Drop cannot provide this async evidence.
    async fn finish(&mut self) -> io::Result<(std::process::ExitStatus, usize)> {
        let status = self.child.wait()?;
        let result = match timeout(LIMIT, &mut self.stderr).await {
            Ok(captured) => {
                self.stderr_joined = true;
                match captured {
                    Ok(result) => result,
                    Err(error) => Err(io::Error::other(format!("host stderr task: {error}"))),
                }
            }
            Err(error) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("host stderr teardown watchdog: {error}"),
            )),
        }
        .and_then(|bytes| {
            if bytes > HOST_LOG_LIMIT || fs::metadata(&self.stderr_path)?.len() != bytes as u64 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "host stderr captured byte count disagrees with bounded file",
                ));
            }
            Ok(bytes)
        });
        match result {
            Ok(bytes) => {
                self.stderr_bytes = Some(bytes);
                Ok((status, bytes))
            }
            Err(error) => {
                self.stderr_failure = Some(error.to_string());
                Err(error)
            }
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let kill = self.child.kill();
        let waited = self.child.wait();
        let abort_requested = !self.stderr_joined;
        if abort_requested {
            self.stderr.abort();
        }
        if let Ok(file) = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.reap_path)
        {
            let _ = serde_json::to_writer_pretty(
                file,
                &json!({"pid":self.child.id(),"killResult":format!("{kill:?}"),"waitResult":format!("{waited:?}"),"childReaped":waited.is_ok(),"stderrCaptureAbortRequested":abort_requested,"stderrCaptureTaskJoined":self.stderr_joined,"stderrCaptureComplete":self.stderr_bytes.is_some(),"stderrCaptureBytes":self.stderr_bytes,"stderrCaptureFailure":self.stderr_failure,"stderrCaptureStatus":if self.stderr_bytes.is_some() {"COMPLETE"} else if self.stderr_joined {"FAILED"} else {"INCOMPLETE_ABORT_REQUESTED"}}),
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ready_finish_rejects_existing_stderr_file_after_successful_child() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let directory = std::env::temp_dir().join(format!(
        "pdrs-ready-capture-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("stderr.log");
    let original = b"owned existing evidence\n";
    fs::write(&path, original).unwrap();
    // Real child and real create_new collision: no fabricated capture result.
    let child = Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .env_clear()
        .current_dir(&directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut process = Process::new(child, path.clone());
    let error = process.finish().await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert!(process.child.wait().unwrap().success(), "actual child exit");
    assert!(process.stderr_joined, "actual capture task joined");
    assert_eq!(process.stderr_bytes, None, "capture did not complete");
    assert_eq!(fs::read(&path).unwrap(), original);
    drop(process);
    let reap: Value =
        serde_json::from_slice(&fs::read(directory.join("host-reap.json")).unwrap()).unwrap();
    assert_eq!(reap["childReaped"], true);
    assert_eq!(reap["stderrCaptureTaskJoined"], true);
    assert_eq!(reap["stderrCaptureComplete"], false);
    assert_eq!(reap["stderrCaptureAbortRequested"], false);
    assert_eq!(reap["stderrCaptureStatus"], "FAILED");
    assert_eq!(fs::read(&path).unwrap(), original);
    // Files are tiny owned test evidence; the root executor controls TMPDIR.
}

fn pinned(name: &str) -> (PathBuf, String) {
    let path = fs::canonicalize(
        std::env::var(format!("PDRS_ORDER_{name}")).expect("explicit pinned path required"),
    )
    .unwrap();
    assert!(path.is_absolute());
    let digest =
        std::env::var(format!("PDRS_ORDER_{name}_SHA256")).expect("explicit pinned hash required");
    assert_eq!(
        hash(fs::read(&path).unwrap()),
        digest,
        "tool identity mismatch"
    );
    (path, digest)
}

// Match the actual default TS six-cell client bytes, including zero mask and
// nonminimal 126 length form. No parser/dispatcher is replaced by this fixture.
fn owned_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut wire = vec![0x80 | opcode, 0xfe];
    wire.extend_from_slice(&u16::try_from(payload.len()).unwrap().to_be_bytes());
    wire.extend_from_slice(&[0; 4]);
    wire.extend_from_slice(payload);
    wire
}

async fn next_frame(io: &mut tokio::io::DuplexStream, decoder: &mut Decoder) -> frame::Frame {
    timeout(LIMIT, async {
        loop {
            if let Some(frame) = decoder.next_server().unwrap() {
                return frame;
            }
            assert_ne!(
                io.read_buf(&mut decoder.buffer).await.unwrap(),
                0,
                "unexpected EOF"
            );
        }
    })
    .await
    .expect("wire frame watchdog")
}

async fn client_frame_observed(
    io: &mut tokio::io::DuplexStream,
    decoder: &mut Decoder,
    observer: &Observer,
    mode: super::result_acquisition::Mode,
) -> frame::Frame {
    let frame = next_frame(io, decoder).await;
    if mode != super::result_acquisition::Mode::Off {
        let value = dispatch::parse(&frame.payload);
        observer.event("client_frame_read",json!({"wireHex":hex(&frame.wire),"requestId":value.as_ref().and_then(|v|crate::metadata::text_field(v,"id")),"observation":"CLIENT_DECODED_COMPLETE"}));
    }
    frame
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit root-assigned execution window, pinned tools and fresh owned output required"]
async fn native_default_six_probe() {
    record_native_cells(super::result_acquisition::Mode::Off).await;
}

pub(super) async fn record_native_cells(mode: super::result_acquisition::Mode) {
    record_native_cells_with_mutation(mode, super::result_acquisition::Mutation::None).await;
}

pub(super) async fn record_native_cells_with_mutation(
    mode: super::result_acquisition::Mode,
    mutation: super::result_acquisition::Mutation,
) {
    let (node, node_sha) = pinned("NODE");
    let (git, git_sha) = pinned("GIT");
    let (loader, loader_sha) = pinned("LOADER");
    let baseline = fs::canonicalize(std::env::var("PDRS_ORDER_BASELINE").unwrap()).unwrap();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest
        .ancestors()
        .find(|p| p.join(".git").exists())
        .unwrap();
    let git_read = |args: &[&str]| {
        let result = Command::new(&git)
            .args(["--no-replace-objects", "-C", workspace.to_str().unwrap()])
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(result.status.success());
        String::from_utf8(result.stdout).unwrap().trim().to_string()
    };
    let source_head = git_read(&["rev-parse", "HEAD"]);
    assert_eq!(
        option_env!("PDRS_ORDER_BUILD_SOURCE_HEAD"),
        Some(source_head.as_str()),
        "compile-time source HEAD binding required for this opt-in probe"
    );
    assert!(
        git_read(&["status", "--porcelain", "--", "src/pilotdeck-gateway"]).is_empty(),
        "probe source must be committed and clean"
    );
    let executable = std::env::current_exe().unwrap();
    let binary_sha = hash(fs::read(&executable).unwrap());
    assert_eq!(
        binary_sha,
        std::env::var("PDRS_ORDER_BINARY_SHA256")
            .expect("exact Cargo test compiler-artifact SHA required")
    );
    let common = PathBuf::from(git_read(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ]));
    let main_workspace = common.parent().unwrap();
    for tool in [&node, &git] {
        for root in [workspace, main_workspace, baseline.as_path()] {
            assert!(
                !tool.starts_with(root),
                "interpreter must be external to source trees"
            );
        }
    }
    let output = PathBuf::from(std::env::var("PDRS_ORDER_OUTPUT").unwrap());
    assert!(output.is_absolute());
    assert!(
        fs::canonicalize(output.parent().unwrap())
            .unwrap()
            .starts_with(main_workspace.join("_tmp")),
        "owned main-workspace _tmp output required"
    );
    assert!(!output.exists(), "fresh evidence directory required");
    let script_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/order");
    let scripts: BTreeMap<_, _> = SCRIPT_NAMES
        .iter()
        .map(|name| {
            (
                name.to_string(),
                hash(fs::read(script_root.join(name)).unwrap()),
            )
        })
        .collect();
    let preregistration = if mode != super::result_acquisition::Mode::Off {
        let directory = fs::canonicalize(
            std::env::var("PDRS_ACQUISITION_PREREG").expect("explicit preregistration directory"),
        )
        .unwrap();
        assert!(
            directory.starts_with(main_workspace.join("_tmp")),
            "owned preregistration path"
        );
        let mut entries = Vec::new();
        for (name, key) in [
            ("task.md", "PDRS_ACQUISITION_TASK_SHA256"),
            ("matrix.json", "PDRS_ACQUISITION_MATRIX_SHA256"),
        ] {
            let path = directory.join(name);
            if name == "matrix.json" {
                let matrix: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                let ids: Vec<_> = matrix["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["id"].as_str().unwrap())
                    .collect();
                assert_eq!(
                    ids,
                    [
                        "A0-list-supported",
                        "A0-supported-list",
                        "A0-list-ping",
                        "A0-list-close",
                        "A0-list-response-ping",
                        "A1",
                        "A2"
                    ],
                    "complete preregistered matrix"
                );
                assert_eq!(
                    matrix["mutants"].as_array().unwrap().len(),
                    4,
                    "implementation mutant plans required"
                );
            }
            let digest = hash(fs::read(&path).unwrap());
            assert_eq!(
                digest,
                std::env::var(key).expect("explicit preregistration SHA")
            );
            entries.push(json!({"path":path,"sha256":digest}));
        }
        Value::Array(entries)
    } else {
        Value::Null
    };
    // A0 reuses the exact physical fixture of a previously fixed independent TS run.
    // The TS host attached to a native pair cannot serve as its local-method oracle.
    let oracle = if mode != super::result_acquisition::Mode::Off {
        let path = fs::canonicalize(
            std::env::var("PDRS_ACQUISITION_TS_ORACLE").expect("independent TS result path"),
        )
        .unwrap();
        assert!(path.starts_with(main_workspace.join("_tmp")));
        let bytes = fs::read(&path).unwrap();
        let digest = hash(&bytes);
        assert_eq!(
            digest,
            std::env::var("PDRS_ACQUISITION_TS_ORACLE_SHA256").expect("fixed oracle SHA")
        );
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["status"], "RECORDED_NOT_ACCEPTED");
        assert_eq!(value["cleanup"]["disposed"], true);
        assert_eq!(
            value["results"].as_array().unwrap().len(),
            6,
            "independent default six-cell oracle"
        );
        let runner_path = path.parent().unwrap().join("runner.json");
        let runner_bytes = fs::read(&runner_path).unwrap();
        let runner_sha = hash(&runner_bytes);
        let runner: Value = serde_json::from_slice(&runner_bytes).unwrap();
        assert_eq!(runner["status"], "RECORDED_NOT_ACCEPTED");
        assert_eq!(runner["childReaped"], true);
        assert_eq!(runner["sourceBefore"], runner["sourceAfter"]);
        assert_eq!(runner["sourceBefore"]["count"], 1000);
        assert_eq!(
            runner["sourceBefore"]["ref"],
            "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"
        );
        assert_eq!(runner["scriptsBefore"], runner["scriptsAfter"]);
        for (name, digest) in &scripts {
            assert_eq!(runner["scriptsBefore"][name].as_str().unwrap(), digest);
        }

        for (name, path, digest) in [
            ("node", &node, &node_sha),
            ("git", &git, &git_sha),
            ("loader", &loader, &loader_sha),
        ] {
            assert_eq!(
                runner["identity"][name]["path"].as_str().unwrap(),
                path.to_str().unwrap()
            );
            assert_eq!(runner["identity"][name]["sha256"].as_str().unwrap(), digest);
        }
        Some((path, digest, value, runner_path, runner_sha))
    } else {
        None
    };
    fs::create_dir(&output).unwrap();
    let run_identity = json!({"status":"STARTED_NOT_ACCEPTED","sourceHead":source_head,"executable":{"path":executable,"sha256":binary_sha},"mode":format!("{mode:?}"),"mutation":format!("{mutation:?}"),"tools":{"node":node,"nodeSha256":node_sha,"git":git,"gitSha256":git_sha,"loader":loader,"loaderSha256":loader_sha},"scripts":scripts,"preregistration":preregistration,"argv":std::env::args().collect::<Vec<_>>(),"controlledEnv":std::env::vars().filter(|(key,_)|key.starts_with("PDRS_ORDER_")||key.starts_with("PDRS_ACQUISITION_")).collect::<BTreeMap<_,_>>()});
    fs::write(
        output.join("run-identity.json"),
        serde_json::to_vec_pretty(&run_identity).unwrap(),
    )
    .unwrap();
    let cases = [
        (
            "describe-supported",
            vec!["describe_server", "supported_agents"],
        ),
        ("list-supported", vec!["list_projects", "supported_agents"]),
        ("supported-list", vec!["supported_agents", "list_projects"]),
        ("list-ping", vec!["list_projects", "ping"]),
        ("list-close", vec!["list_projects", "close"]),
        ("list-response-then-ping", vec!["list_projects", "ping"]),
    ];
    let mut rows = Vec::new();
    let cases: Vec<_> = cases
        .into_iter()
        .filter(|(name, _)| match mode {
            super::result_acquisition::Mode::Off => true,
            super::result_acquisition::Mode::Natural => *name != "describe-supported",
            _ => *name == "list-supported",
        })
        .collect();
    let expected_count = match mode {
        super::result_acquisition::Mode::Off => 6,
        super::result_acquisition::Mode::Natural => 5,
        _ => 1,
    };
    assert_eq!(
        cases.len(),
        expected_count,
        "complete source matrix before recording"
    );
    for (name, methods) in cases {
        let case = output.join(name);
        fs::create_dir(&case).unwrap();
        let scratch = if let Some((path, _, _, _, _)) = &oracle {
            let scratch = fs::canonicalize(path.parent().unwrap().join("isolated")).unwrap();
            assert!(scratch.starts_with(main_workspace.join("_tmp")));
            scratch
        } else {
            let scratch = case.join("isolated");
            fs::create_dir(&scratch).unwrap();
            for dir in ["home", "config", "cache", "data", "tmp", "pilot", "project"] {
                fs::create_dir(scratch.join(dir)).unwrap();
            }
            scratch
        };
        let spec = json!({"baseline":baseline,"git":{"path":git,"sha256":git_sha},"node":{"path":node,"sha256":node_sha},"loader":{"path":loader,"sha256":loader_sha},"scriptRoot":script_root,"scripts":scripts,"scratch":scratch,"report":case.join("host.json")});
        let spec_path = case.join("identity.json");
        fs::write(&spec_path, serde_json::to_vec_pretty(&spec).unwrap()).unwrap();
        let mut command = Command::new(&node);
        command
            .args([
                "--import",
                script_root.join("offline.mjs").to_str().unwrap(),
                "--import",
                loader.to_str().unwrap(),
                script_root.join("stdio-host.mjs").to_str().unwrap(),
                spec_path.to_str().unwrap(),
            ])
            .env_clear()
            .current_dir(&scratch);
        for (key, dir) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_DATA_HOME", "data"),
            ("TMPDIR", "tmp"),
            ("PILOT_HOME", "pilot"),
        ] {
            command.env(key, scratch.join(dir));
        }
        command
            .env("PATH", "/usr/bin:/bin")
            .env("PDRS_ORDER_PROBE", "owned-default-six");
        for key in [
            "PILOTDECK_SKIP_BROWSER_OPEN",
            "PILOT_SKIP_BROWSER_OPEN",
            "SKIP_BROWSER_OPEN",
            "PILOTDECK_SKIP_BROWSER",
        ] {
            command.env(key, "1");
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let invocation = json!({"program":command.get_program().to_string_lossy(),"args":command.get_args().map(|x|x.to_string_lossy().into_owned()).collect::<Vec<_>>(),"cwd":command.get_current_dir(),"envClear":true,"env":command.get_envs().map(|(k,v)|(k.to_string_lossy().into_owned(),v.map(|x|x.to_string_lossy().into_owned()))).collect::<BTreeMap<_,_>>()});
        fs::write(
            case.join("host-invocation.json"),
            serde_json::to_vec_pretty(&invocation).unwrap(),
        )
        .unwrap();
        let mut child = Process::new(command.spawn().unwrap(), case.join("host.log"));
        let pipes = Pipe {
            reader: nonblocking(child.child.stdout.take().unwrap().into()).unwrap(),
            writer: nonblocking(child.child.stdin.take().unwrap().into()).unwrap(),
        };
        let observer = Observer::new();
        let _failure_trace = super::result_acquisition::FailureTrace::new(
            observer.clone(),
            case.join("failure-trace.json"),
        );
        let acquisition = super::result_acquisition::Tap::with_mutation(
            mode,
            mutation,
            observer.clone(),
            output.to_string_lossy().into_owned(),
            name,
        );
        let mut acquisition_cleanup =
            super::result_acquisition::OwnedRunCleanup::new(acquisition.clone());
        let metadata = crate::metadata::Metadata {
            home: scratch.join("pilot"),
            cwd: scratch.join("project"),
            files: TracedFiles(observer.clone()),
        };
        let (mut client, downstream) = tokio::io::duplex(1024 * 1024);
        let down = Traced {
            io: downstream,
            observer: observer.clone(),
            name: "downstream",
        };
        let up = Traced {
            io: pipes,
            observer: observer.clone(),
            name: "upstream",
        };
        let scope = observer.clone();
        let acquisition_scope = acquisition.clone();
        let task = tokio::spawn(async move {
            super::result_acquisition::ACTIVE
                .scope(
                    acquisition_scope,
                    ACTIVE.scope(scope, run_with_metadata(down, up, Some(metadata))),
                )
                .await
        });
        acquisition_cleanup.track(task.abort_handle());
        let mut decoder = Decoder::default();
        let hello=owned_frame(1,br#"{"type":"hello","protocolVersion":"1.2","clientName":"web","clientVersion":"owned-probe","token":"fixture"}"#);
        client.write_all(&hello).await.unwrap();
        let hello = client_frame_observed(&mut client, &mut decoder, &observer, mode).await;
        assert_eq!(
            crate::metadata::text_field(&dispatch::parse(&hello.payload).unwrap(), "type")
                .as_deref(),
            Some("hello_ok")
        );
        let inputs: Vec<_> = methods
            .iter()
            .enumerate()
            .map(|(i, method)| match *method {
                "ping" => owned_frame(9, b""),
                "close" => owned_frame(8, b""),
                _ => owned_frame(
                    1,
                    format!(r#"{{"type":"request","id":"{name}-{i}","method":"{method}"}}"#)
                        .as_bytes(),
                ),
            })
            .collect();
        let mut wire = Vec::new();
        if matches!(
            mode,
            super::result_acquisition::Mode::HoldRouteReturn
                | super::result_acquisition::Mode::HoldAcquired
        ) {
            client.write_all(&inputs[0]).await.unwrap();
            acquisition.wait_admission_barrier().await;
            client.write_all(&inputs[1]).await.unwrap();
            let first = client_frame_observed(&mut client, &mut decoder, &observer, mode).await;
            wire.push(hex(&first.wire));
            acquisition.release_return();
            let second = client_frame_observed(&mut client, &mut decoder, &observer, mode).await;
            wire.push(hex(&second.wire));
        } else if name == "list-response-then-ping" {
            client.write_all(&inputs[0]).await.unwrap();
            let response = client_frame_observed(&mut client, &mut decoder, &observer, mode).await;
            wire.push(hex(&response.wire));
            observer.event(
                "causal_first_response_observed",
                json!({"wireHex":hex(&response.wire)}),
            );
            client.write_all(&inputs[1]).await.unwrap();
            wire.push(hex(&client_frame_observed(
                &mut client,
                &mut decoder,
                &observer,
                mode,
            )
            .await
            .wire));
        } else {
            client.write_all(&inputs.concat()).await.unwrap();
            if name == "list-close" {
                loop {
                    let frame =
                        client_frame_observed(&mut client, &mut decoder, &observer, mode).await;
                    let closed = frame.opcode == 8;
                    wire.push(hex(&frame.wire));
                    assert!(wire.len() <= 2, "extra close-case output");
                    if closed {
                        break;
                    }
                }
            } else {
                for _ in 0..2 {
                    wire.push(hex(&client_frame_observed(
                        &mut client,
                        &mut decoder,
                        &observer,
                        mode,
                    )
                    .await
                    .wire));
                }
            }
        }
        observer.event("driver_response_pair_complete", json!({"wireHex":wire}));
        // A real host close is the causal end barrier, not a silence interval.
        if name != "list-close" {
            client.write_all(&owned_frame(8, b"")).await.unwrap();
            let close = client_frame_observed(&mut client, &mut decoder, &observer, mode).await;
            observer.event(
                "driver_close_frame",
                json!({"opcode":close.opcode,"wireHex":hex(&close.wire)}),
            );
            assert_eq!(close.opcode, 8, "extra response before causal close");
        }
        timeout(LIMIT, task)
            .await
            .expect("pair teardown watchdog")
            .unwrap()
            .unwrap();
        observer.sample("pair_returned");
        assert!(
            decoder.buffer.is_empty(),
            "unrecorded downstream buffered tail"
        );
        let (child_status, _stderr_bytes) = child
            .finish()
            .await
            .expect("default host exit and complete bounded stderr capture");
        assert!(child_status.success(), "default host failed");
        let host: Value =
            serde_json::from_slice(&fs::read(case.join("host.json")).unwrap()).unwrap();
        assert_eq!(host["status"], "RECORDED_NOT_ACCEPTED");
        let host_bytes = fs::read(case.join("host.json")).unwrap();
        let oracle_comparison = if mode == super::result_acquisition::Mode::Natural {
            let (_, _, oracle, _, _) = oracle.as_ref().unwrap();
            let row = oracle["results"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["name"] == name)
                .expect("exact independent oracle row");
            let expected: Vec<_> = row["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|event| {
                    event["event"] == "wire_write" && event["json"]["type"] != "hello_ok"
                })
                .map(|event| event["wireHex"].as_str().unwrap().to_string())
                .collect();
            json!({"status":if wire==expected {"EXACT_WIRE_MATCH_FOR_THIS_ROW"}else{"EXACT_WIRE_DIFF"},"fullWireCompared":true,"samePhysicalFixture":true,"callbackDomainEquivalence":"NOT_INFERRED"})
        } else {
            json!({"status":"NOT_COMPARED_INTERVENTION_INPUT_SCRIPT_DIFFERS"})
        };
        let row = json!({"name":name,"wireHex":wire,"events":observer.snapshot(),"host":{"path":"host.json","sha256":hash(host_bytes),"status":host["status"]},"scope":{"osCompletion":"NOT_OBSERVED","joinsetConsumable":"NOT_INFERRED_FROM_COMPLETE","taskComplete":"SAMPLED_ONLY","productionBehaviorChanged":false,"testMutation":format!("{mutation:?}")},"acquisition":{"status":"VALIDATION_RECORDED_SEPARATELY","mode":format!("{mode:?}")},"oracleComparison":oracle_comparison});
        let row_bytes = serde_json::to_vec_pretty(&row).unwrap();
        fs::write(case.join("rust.json"), &row_bytes).unwrap();
        let validation = std::panic::catch_unwind(|| {
            super::result_acquisition::validate(&observer.snapshot(), mode, name)
        });
        match validation {
            Ok(value) => fs::write(
                case.join("acquisition-check.json"),
                serde_json::to_vec_pretty(&value).unwrap(),
            )
            .unwrap(),
            Err(error) => {
                fs::write(case.join("acquisition-check.json"),serde_json::to_vec_pretty(&json!({"status":"FAIL","reason":panic_reason(error.as_ref()),"raw":"rust.json"})).unwrap()).unwrap();
                std::panic::resume_unwind(error);
            }
        }

        rows.push(json!({"name":name,"path":format!("{name}/rust.json"),"sha256":hash(row_bytes),"validation":{"path":format!("{name}/acquisition-check.json"),"sha256":hash(fs::read(case.join("acquisition-check.json")).unwrap())}}));
    }
    for (name, digest) in &scripts {
        assert_eq!(
            &hash(fs::read(script_root.join(name)).unwrap()),
            digest,
            "script postflight drift"
        );
    }
    for (path, digest) in [(&node, &node_sha), (&git, &git_sha), (&loader, &loader_sha)] {
        assert_eq!(
            &hash(fs::read(path).unwrap()),
            digest,
            "tool postflight drift"
        );
    }
    let oracle_identity = if let Some((path, digest, _, runner, runner_sha)) = &oracle {
        assert_eq!(
            hash(fs::read(path).unwrap()),
            *digest,
            "fixed oracle postflight drift"
        );
        assert_eq!(
            hash(fs::read(runner).unwrap()),
            *runner_sha,
            "oracle runner postflight drift"
        );
        json!({"path":path,"sha256":digest,"runner":runner,"runnerSha256":runner_sha})
    } else {
        Value::Null
    };
    if let Some(entries) = preregistration.as_array() {
        for entry in entries {
            assert_eq!(
                hash(fs::read(entry["path"].as_str().unwrap()).unwrap()),
                entry["sha256"].as_str().unwrap(),
                "preregistration postflight drift"
            );
        }
    }
    assert_eq!(rows.len(), expected_count);
    assert_eq!(
        git_read(&["rev-parse", "HEAD"]),
        source_head,
        "Rust source HEAD drift"
    );
    assert!(
        git_read(&["status", "--porcelain", "--", "src/pilotdeck-gateway"]).is_empty(),
        "Rust source postflight drift"
    );
    assert_eq!(
        hash(fs::read(&executable).unwrap()),
        binary_sha,
        "Rust binary postflight drift"
    );
    fs::write(output.join(if mode == super::result_acquisition::Mode::Off {"rust-six.json"} else {"acquisition.json"}),serde_json::to_vec_pretty(&json!({"status":"RECORDED_NOT_ACCEPTED","cases":rows,"acceptance":"NOT_RUN","sourceHead":source_head,"executable":{"path":executable,"sha256":binary_sha},"tools":{"node":node,"nodeSha256":node_sha,"git":git,"gitSha256":git_sha,"loader":loader,"loaderSha256":loader_sha},"scripts":scripts,"preregistration":preregistration,"acquisitionMode":format!("{mode:?}"),"mutation":format!("{mutation:?}"),"independentTsOracle":oracle_identity})).unwrap()).unwrap();
}

fn panic_reason(error: &(dyn std::any::Any + Send)) -> String {
    error
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "non-string panic payload".into())
}

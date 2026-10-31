//! #1145 real CLI process replay against the registered TS signal cells.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_tungstenite::{
    accept_async,
    tungstenite::{
        Message,
        protocol::{CloseFrame, frame::coding::CloseCode},
    },
};

const OBSERVATION: Duration = Duration::from_secs(12);
const CLEANUP: Duration = Duration::from_secs(3);
const DRIVER_MARKER: &str = "#1145 SIGNAL_DRIVER_READY\n";
const DRIVER_PREFIX: &str = "\nrunning 1 test\ntest signal_driver ... #1145 SIGNAL_DRIVER_READY\n";
static SERIAL: Mutex<()> = Mutex::new(());

fn require<T, E: std::fmt::Display>(result: Result<T, E>, id: &str, detail: &str) -> T {
    result.unwrap_or_else(|error| panic!("#1145 {id} {detail}: {error}"))
}

fn ensure(condition: bool, id: &str, detail: &str) {
    crate::test_negative_receipt::verify(condition, &format!("#1145 {id} {detail}"));
}

/// Called only by the separately selected, same-source product child test.
pub(super) fn driver() {
    let Ok(id) = std::env::var("PDRS_SIGNAL_CHILD") else {
        return;
    };
    let argv: Vec<String> = require(
        serde_json::from_str(&require(
            std::env::var("PDRS_SIGNAL_ARGV"),
            &id,
            "child argv environment",
        )),
        &id,
        "child argv JSON",
    );
    let mut stdout = std::io::stdout().lock();
    require(
        stdout.write_all(DRIVER_MARKER.as_bytes()),
        &id,
        "child marker write",
    );
    require(stdout.flush(), &id, "child marker flush");
    drop(stdout);
    let environment = crate::paths::Env::from_process();
    let cwd = require(std::env::current_dir(), &id, "child current directory");
    let code = match crate::product::run(&argv, &environment, &cwd) {
        Ok(code) => code,
        Err(crate::Failure::Error(message)) => {
            crate::stderr_line(&message);
            1
        }
        Err(crate::Failure::Todo(message)) => {
            crate::stderr_line(&format!(
                "pilotdeck-rs TODO: {message} is not implemented yet."
            ));
            2
        }
    };
    std::process::exit(i32::from(code));
}

struct OwnedChild {
    child: Child,
    id: String,
}
impl OwnedChild {
    fn poll(&mut self) -> Option<ExitStatus> {
        require(self.child.try_wait(), &self.id, "observe owned child exit")
    }
    fn signal(&mut self, signal: &str) {
        ensure(
            matches!(signal, "SIGINT" | "SIGTERM"),
            &self.id,
            "registered signal name",
        );
        let mut helper = require(
            Command::new("/bin/kill")
                .args([format!("-{}", &signal[3..]), self.child.id().to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn(),
            &self.id,
            "spawn owned-pid signal helper",
        );
        let deadline = Instant::now() + CLEANUP;
        loop {
            if let Some(status) = require(helper.try_wait(), &self.id, "signal helper status") {
                ensure(
                    status.success(),
                    &self.id,
                    "signal delivered to owned child",
                );
                return;
            }
            if Instant::now() >= deadline {
                let _ = helper.kill();
                panic!("#1145 {} signal helper exceeded cleanup bound", self.id);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_some() {
            return;
        }
        let _ = self.child.kill();
        let deadline = Instant::now() + CLEANUP;
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        eprintln!("#1145 {} owned child cleanup unverified", self.id);
    }
}

fn snapshot(root: &Path, id: &str) -> Value {
    fn visit(root: &Path, name: &str, id: &str, entries: &mut Map<String, Value>) {
        let path = if name == "." {
            root.to_owned()
        } else {
            root.join(name)
        };
        let metadata = require(fs::symlink_metadata(&path), id, "snapshot metadata");
        let mode = metadata.permissions().mode() & 0o777;
        if metadata.is_dir() {
            entries.insert(name.into(), json!({"type":"directory", "mode":mode}));
            let children = require(fs::read_dir(&path), id, "snapshot directory");
            for entry in children {
                let entry = require(entry, id, "snapshot directory entry");
                let child = require(
                    entry.file_name().into_string().map_err(|_| "non-UTF8 name"),
                    id,
                    "snapshot filename",
                );
                let child = if name == "." {
                    child
                } else {
                    format!("{name}/{child}")
                };
                visit(root, &child, id, entries);
            }
        } else {
            ensure(
                metadata.is_file(),
                id,
                "snapshot rejects unexpected file type",
            );
            let bytes = require(fs::read(path), id, "snapshot file bytes");
            entries.insert(
                name.into(),
                json!({"type":"file", "mode":mode,
                "size":bytes.len(), "sha256":format!("{:x}", Sha256::digest(&bytes))}),
            );
        }
    }
    let mut entries = Map::new();
    visit(root, ".", id, &mut entries);
    Value::Object(entries)
}

fn private_directory(path: &Path, id: &str) {
    require(fs::create_dir(path), id, "exclusive runtime directory");
    require(
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)),
        id,
        "runtime directory permissions",
    );
}

fn private_file(path: &Path, bytes: &[u8], id: &str) {
    let mut file = require(
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path),
        id,
        "exclusive runtime file",
    );
    require(
        file.set_permissions(fs::Permissions::from_mode(0o600)),
        id,
        "runtime file permissions",
    );
    require(file.write_all(bytes), id, "runtime file content");
}

fn publish(directory: &Path, name: &str, value: &Value, id: &str) {
    let temporary = directory.join(format!(".{name}.tmp"));
    private_file(
        &temporary,
        &require(serde_json::to_vec_pretty(value), id, "evidence JSON"),
        id,
    );
    require(
        fs::rename(temporary, directory.join(name)),
        id,
        "atomic evidence publication",
    );
}

#[derive(Clone)]
struct Request {
    connection: usize,
    frame: Value,
}
enum Action {
    Send(Value),
    Close,
}
enum Event {
    Frame(usize, Value),
    Closed(usize, u16, String, bool),
    Error(String),
}

async fn socket_actor(
    socket: TcpStream,
    connection: usize,
    mut actions: mpsc::UnboundedReceiver<Action>,
    events: mpsc::UnboundedSender<Event>,
    cleanup: Arc<AtomicBool>,
) {
    let mut socket = match tokio::time::timeout(CLEANUP, accept_async(socket)).await {
        Ok(Ok(socket)) => socket,
        other => {
            let _ = events.send(Event::Error(format!("WebSocket handshake: {other:?}")));
            return;
        }
    };
    let (mut code, mut reason) = (1006, String::new());
    loop {
        tokio::select! {
            action = actions.recv() => {
                let message = match action {
                    Some(Action::Send(frame)) => Message::Text(frame.to_string().into()),
                    Some(Action::Close) => Message::Close(Some(CloseFrame {
                        code: CloseCode::Library(4003), reason: "auth_failed".into(),
                    })),
                    None => break,
                };
                match tokio::time::timeout(CLEANUP, socket.send(message)).await {
                    Ok(Ok(())) => {},
                    Ok(Err(_)) => break,
                    Err(_) => { let _ = events.send(Event::Error("WebSocket write exceeded cleanup bound".into())); break; }
                }
            }
            frame = socket.next() => {
                match frame {
                    Some(Ok(Message::Text(bytes))) => match serde_json::from_str(&bytes) {
                        Ok(frame) => { if events.send(Event::Frame(connection, frame)).is_err() { break; } }
                        Err(error) => { let _ = events.send(Event::Error(format!("request JSON: {error}"))); break; }
                    },
                    Some(Ok(Message::Close(frame))) => {
                        if let Some(frame) = frame { code = u16::from(frame.code); reason = frame.reason.to_string(); }
                        else { code = 1005; }
                        let _ = tokio::time::timeout(CLEANUP, socket.flush()).await;
                        break;
                    }
                    Some(Ok(Message::Ping(_))) => { let _ = tokio::time::timeout(CLEANUP, socket.flush()).await; }
                    Some(Ok(Message::Pong(_))) => {},
                    Some(Ok(_)) => { let _ = events.send(Event::Error("unexpected non-text request".into())); break; }
                    Some(Err(_)) | None => break,
                }
            }
        }
    }
    let _ = events.send(Event::Closed(
        connection,
        code,
        reason,
        cleanup.load(Ordering::SeqCst),
    ));
}

struct Replay {
    id: String,
    input: Value,
    child: OwnedChild,
    directory: PathBuf,
    observer: PathBuf,
    sockets: BTreeMap<usize, mpsc::UnboundedSender<Action>>,
    active: BTreeSet<usize>,
    closes: BTreeMap<usize, Value>,
    hooks: Vec<Value>,
    wire: Vec<Value>,
    connections: usize,
    aborts: usize,
    registered: bool,
    removed: bool,
    abort_entered: bool,
    terminal_consumed: bool,
    terminal_after_abort: bool,
    late_abort_handled: bool,
    resolved_held: bool,
    trigger: bool,
    first_signal: bool,
    second_signal: bool,
    held: Option<Request>,
    stream: Option<Request>,
    pending_abort: Option<Request>,
    deadline: Instant,
    cleanup: Arc<AtomicBool>,
}

impl Replay {
    fn stage(&self) -> &str {
        self.input["stage"].as_str().unwrap_or("")
    }
    fn action(&self) -> &str {
        self.input["action"].as_str().unwrap_or("")
    }
    fn send(&mut self, request: &Request, frame: Value) {
        self.wire
            .push(json!({"direction":"send", "connection":request.connection, "frame":frame}));
        let socket = self
            .sockets
            .get(&request.connection)
            .unwrap_or_else(|| panic!("#1145 {} registered socket sender", self.id));
        require(
            socket.send(Action::Send(frame)),
            &self.id,
            "send to owned socket actor",
        );
    }
    fn reply(&mut self, request: &Request, ok: bool) {
        let frame = if request.frame["type"] == "hello" {
            json!({"type":"hello_ok","protocolVersion":"1.2","serverVersion":"0.1.0-mock",
                "serverInfo":{"mode":"remote","projectKey":"d1-project","sessionCount":1,"capabilities":[]}})
        } else if ok {
            let result = match request.frame["method"].as_str() {
                Some("model_catalog_list") => {
                    json!({"items":[{"id":"fixture/model","provider":"fixture","model":"model"}]})
                }
                Some("abort_turn") => json!({"aborted":true}),
                _ => json!({"sessionKey":"d1-session"}),
            };
            json!({"type":"response","id":request.frame["id"],"ok":true,"result":result})
        } else {
            json!({"type":"response","id":request.frame["id"],"ok":false,
            "error":{"code":"server_error","message":"d1 fixture rejected"}})
        };
        self.send(request, frame);
    }
    fn terminal(&mut self, kind: &str, final_frame: bool) {
        let stream = self
            .stream
            .clone()
            .unwrap_or_else(|| panic!("#1145 {} terminal has submit stream", self.id));
        let event = match kind {
            "failed" => {
                json!({"type":"error","code":"server_error","message":"d1 terminal failed"})
            }
            "unknown" => json!({"type":"input_accepted"}),
            _ => json!({"type":"turn_completed","result":"wire terminal","usage":{}}),
        };
        self.send(&stream, json!({"type":"event","id":stream.frame["id"],"seq":0,"final":final_frame,"event":event}));
    }
    fn reach(&mut self) {
        if self.trigger {
            return;
        }
        self.trigger = true;
        self.deadline = Instant::now() + OBSERVATION;
        if let Some(signal) = self.input["signal"].as_str().map(str::to_owned) {
            self.child.signal(&signal);
            self.first_signal = true;
        }
    }
    fn maybe_trigger(&mut self) {
        if (self.stage().starts_with("query-") && self.held.is_some() && self.registered)
            || (matches!(self.stage(), "stream" | "abort-terminal")
                && self.stream.is_some()
                && self.registered)
            || (self.stage() == "terminal" && self.terminal_consumed && self.registered)
        {
            self.reach();
        }
    }
    fn on_hook(&mut self, hook: Value) {
        let sequence = hook["seq"].as_u64().unwrap_or(u64::MAX);
        ensure(
            sequence == self.hooks.len() as u64,
            &self.id,
            "observer sequence is complete",
        );
        let kind = hook["kind"].as_str().unwrap_or("").to_owned();
        self.hooks.push(hook);
        match kind.as_str() {
            "listeners-ready" => self.registered = true,
            "listeners-removed" => self.removed = true,
            "abort-enter-1" | "abort-enter-2" => {
                self.abort_entered = true;
                if self.held.is_some() && !self.resolved_held && self.action() != "hold" {
                    self.resolved_held = true;
                    let held = self
                        .held
                        .clone()
                        .unwrap_or_else(|| panic!("#1145 {} held predecessor", self.id));
                    if self.action() == "reject" && self.stage() == "query-hello" {
                        let socket = self
                            .sockets
                            .get(&held.connection)
                            .unwrap_or_else(|| panic!("#1145 {} hello socket", self.id));
                        require(socket.send(Action::Close), &self.id, "reject held hello");
                    } else {
                        self.reply(&held, self.action() != "reject");
                    }
                }
            }
            "terminal-consumed" => {
                self.terminal_consumed = true;
                if self.stage() == "abort-terminal" {
                    self.terminal_after_abort = self.aborts > 0;
                    if self.input["final"] == false {
                        let request = self.pending_abort.take().unwrap_or_else(|| {
                            panic!("#1145 {} terminal follows pending abort", self.id)
                        });
                        self.reply(&request, true);
                        self.late_abort_handled = true;
                    }
                }
            }
            _ => panic!("#1145 {} unknown observation kind {kind}", self.id),
        }
        if self.stage() == "abort-terminal"
            && self.input["final"] == true
            && self.removed
            && let Some(request) = self.pending_abort.take()
        {
            // A final stream may close the peer before this observation is read.
            // Preserve that as unreachable; never label it a consumed reply.
            if self.active.contains(&request.connection) {
                let frame = json!({"type":"response","id":request.frame["id"],"ok":true,"result":{"aborted":true}});
                if let Some(sender) = self.sockets.get(&request.connection) {
                    let _ = sender.send(Action::Send(frame));
                }
            }
            self.late_abort_handled = true;
        }
        self.maybe_trigger();
    }
    fn read_hooks(&mut self) {
        let mut paths = require(
            fs::read_dir(&self.observer),
            &self.id,
            "read observation directory",
        )
        .map(|entry| require(entry, &self.id, "observation entry").path())
        .filter(|path| {
            path.extension().is_some_and(|value| value == "json")
                && !path
                    .file_name()
                    .is_some_and(|value| value.to_string_lossy().starts_with('.'))
        })
        .collect::<Vec<_>>();
        paths.sort();
        for path in paths.into_iter().skip(self.hooks.len()) {
            let bytes = require(fs::read(path), &self.id, "atomic observation bytes");
            self.on_hook(require(
                serde_json::from_slice(&bytes),
                &self.id,
                "observation JSON",
            ));
        }
    }
    fn on_frame(&mut self, connection: usize, frame: Value) {
        self.wire
            .push(json!({"direction":"receive","connection":connection,"frame":frame}));
        let request = Request { connection, frame };
        if request.frame["type"] == "hello" {
            ensure(
                request.frame["token"] == "d1-fixture-not-a-secret",
                &self.id,
                "synthetic authentication",
            );
            if self.stage() == "query-hello" && connection == 2 {
                self.held = Some(request);
                self.maybe_trigger();
            } else {
                self.reply(&request, true);
            }
            return;
        }
        ensure(
            request.frame["type"] == "request",
            &self.id,
            "request frame type",
        );
        let method = request.frame["method"].as_str().unwrap_or("").to_owned();
        if connection == 1 {
            let expected = if self.input["leaf"] == "run" {
                "new_session"
            } else {
                "resume_session"
            };
            ensure(method == expected, &self.id, "expected control predecessor");
            if self.stage() == "control" {
                self.held = Some(request);
                self.reach();
                return;
            }
            if self.stage() == "control-reject" {
                self.reach();
            }
            self.reply(&request, self.stage() != "control-reject");
            return;
        }
        if (self.stage() == "query-resume" && method == "resume_session")
            || (self.stage() == "query-model" && method == "model_catalog_list")
        {
            self.held = Some(request);
            self.maybe_trigger();
            return;
        }
        if method == "resume_session" || method == "model_catalog_list" {
            if self.stage() == "query-reject" {
                self.reach();
            }
            self.reply(&request, self.stage() != "query-reject");
            return;
        }
        if method == "submit_turn" {
            self.stream = Some(request);
            if self.stage() == "terminal" {
                let terminal = self.input["terminal"].as_str().unwrap_or("").to_owned();
                self.terminal(&terminal, false);
            } else if matches!(self.stage(), "complete" | "failed" | "unknown") {
                self.reach();
                let stage = self.stage().to_owned();
                self.terminal(&stage, true);
            } else if self.stage() == "response-error" {
                self.reach();
                let stream = self
                    .stream
                    .clone()
                    .unwrap_or_else(|| panic!("#1145 {} response-error stream", self.id));
                self.reply(&stream, false);
            }
            self.maybe_trigger();
            return;
        }
        ensure(method == "abort_turn", &self.id, "expected abort RPC");
        self.aborts += 1;
        if self.aborts == 1
            && let Some(signal) = self.input["second"].as_str().map(str::to_owned)
        {
            self.child.signal(&signal);
            self.second_signal = true;
        }
        if self.stage() == "abort-terminal" {
            self.pending_abort = Some(request);
            self.terminal("completed", self.input["final"] == true);
        } else if self.action() != "hold" {
            self.reply(&request, self.action() != "error");
        }
    }
    fn on_event(&mut self, event: Event) {
        match event {
            Event::Frame(connection, frame) => self.on_frame(connection, frame),
            Event::Closed(connection, code, reason, cleanup) => {
                ensure(
                    self.active.remove(&connection),
                    &self.id,
                    "one close per connection",
                );
                self.closes.insert(
                    connection,
                    json!({"connection":connection,"code":code,"reason":reason,"cleanup":cleanup}),
                );
            }
            Event::Error(error) => panic!("#1145 {} mock transport error: {error}", self.id),
        }
    }
}

fn signal_name(signal: i32) -> String {
    match signal {
        2 => "SIGINT".into(),
        9 => "SIGKILL".into(),
        15 => "SIGTERM".into(),
        other => format!("SIGNAL-{other}"),
    }
}

fn strip_sqlite_warning(raw: &str) -> &str {
    let Some(rest) = raw.strip_prefix("(node:") else {
        return raw;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return raw;
    }
    let prefix = ") ExperimentalWarning: SQLite is an experimental feature and might change at any time\n(Use `node --trace-warnings ...` to show where the warning was created)\n";
    rest[digits..].strip_prefix(prefix).unwrap_or(raw)
}

// #1151 一 A: only a complete, observed Node trailer after its business line.
// Keeping the raw records intact also makes every rejected shape comparable.
fn ts_stderr(raw: &str) -> &str {
    static TRAILER: std::sync::LazyLock<fancy_regex::Regex> = std::sync::LazyLock::new(|| {
        fancy_regex::Regex::new(concat!(
            r"\A(?P<business>[^\r\n]+\n)",
            r"file:///[^\r\n]+/packages/sdk/dist/transport\.js:[0-9]+\n",
            r" +[^ \t\r\n][^\r\n]*\n +\^\n\n",
            r"PilotDeckError: (?P<message>[^\r\n]+)\n",
            r"(?:    at [^ \t\r\n{}][^\r\n{}]*\n)*    at [^ \t\r\n{}][^\r\n{}]* \{\n",
            r"  code: '(?:timeout|authentication_error|server_error|transport_error)',\n",
            r"  requestId: undefined,\n  details: undefined,\n",
            r"  retryable: (?:true|false),\n  \[cause\]: undefined\n",
            r"\}\n\nNode\.js v22\.22\.0\n\z",
        ))
        .expect("#1145 exact Node trailer regex")
    });
    let raw = strip_sqlite_warning(raw);
    let Some(captures) = TRAILER
        .captures(raw)
        .expect("#1145 bounded Node trailer match")
    else {
        return raw;
    };
    let business = captures
        .name("business")
        .expect("#1145 business capture")
        .as_str();
    let message = captures
        .name("message")
        .expect("#1145 error message capture")
        .as_str();
    if business.strip_suffix('\n') == Some(message) {
        business
    } else {
        raw
    }
}

fn stderr_matches(ts: &str, rust: &[u8]) -> bool {
    // Never normalize Rust, including a byte-identical Node stack on that side.
    ts_stderr(ts).as_bytes() == rust
}

// #1164 B / D1-SDK-SIGNAL-UNHANDLED-REJECTION-001. The accepted difference
// names eight cells, not an exception shape; all other cells retain their rules.
fn approved_signal_stderr(id: &str) -> Option<&'static [u8]> {
    match id {
        "run--abort-error-sigint"
        | "run--abort-error-sigterm"
        | "resume--abort-error-sigint"
        | "resume--abort-error-sigterm" => Some(b"d1 fixture rejected\n"),
        "run--terminal-during-abort-final-sigint"
        | "run--terminal-during-abort-final-sigterm"
        | "resume--terminal-during-abort-final-sigint"
        | "resume--terminal-during-abort-final-sigterm" => Some(b"Gateway client closed.\n"),
        _ => None,
    }
}

fn stderr_matches_for_case(id: &str, ts: &str, rust: &[u8]) -> bool {
    match approved_signal_stderr(id) {
        Some(expected) => expected == rust,
        None => stderr_matches(ts, rust),
    }
}

fn closes_match(id: &str, expected: &Value, actual: &Value) -> bool {
    let mut closes = expected["closes"]
        .as_array()
        .unwrap_or_else(|| panic!("#1145 {id} TS close observations"))
        .iter()
        .map(|row| {
            json!({"connection":row["connection"],"code":row["code"],
            "reason":row["reason"],"cleanup":row["cleanup"]})
        })
        .collect::<Vec<_>>();
    closes.sort_by_key(|row| row["connection"].as_u64());
    if *actual == json!(closes) {
        return true;
    }
    let approved = matches!(
        id,
        "run--query-hello-reject-sigint"
            | "run--query-hello-reject-sigterm"
            | "resume--query-hello-reject-sigint"
            | "resume--query-hello-reject-sigterm"
    );
    if approved {
        for row in &mut closes {
            if row["connection"] == 2
                && row["code"] == 4003
                && row["reason"] == ""
                && row["cleanup"] == false
            {
                // #1151 二 A / D1-SDK-WS-CLOSE-REASON-001. This is an exact
                // expected value, not a wildcard over the actual reason.
                row["reason"] = json!("auth_failed");
            }
        }
    }
    closes.sort_by_key(|row| row["connection"].as_u64());
    *actual == json!(closes)
}

async fn capture(expected: &Value, directory: PathBuf, id: &str) -> Value {
    let runtime = directory.join("runtime");
    private_directory(&runtime, id);
    for name in [
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
        private_directory(&runtime.join(name), id);
    }
    private_file(
        &runtime.join("pilot/pilotdeck.yaml"),
        b"schemaVersion: 1\n",
        id,
    );
    private_file(
        &runtime.join("project/keep.txt"),
        b"P08 fixture\0\xff\n",
        id,
    );
    private_file(&runtime.join("openssl.cnf"), b"", id);
    let initial = snapshot(&runtime, id);
    let observer = directory.join("observations");
    private_directory(&observer, id);
    let stdout_path = directory.join("stdout.raw");
    let stderr_path = directory.join("stderr.raw");
    let stdout = require(
        fs::File::create_new(&stdout_path),
        id,
        "exclusive child stdout",
    );
    let stderr = require(
        fs::File::create_new(&stderr_path),
        id,
        "exclusive child stderr",
    );
    let listener = require(
        tokio::time::timeout(CLEANUP, TcpListener::bind("127.0.0.1:28999")).await,
        id,
        "bounded mock bind",
    );
    let listener = require(listener, id, "exclusive owned mock port 28999");
    let executable = require(std::env::current_exe(), id, "same-source test executable");
    let wrapper = std::env::var_os("PDRS_SIGINT_WRAPPER").unwrap_or_else(|| {
        super::fixture_directory()
            .join("sigint_default.py")
            .into_os_string()
    });
    ensure(
        Path::new(&wrapper).is_file(),
        id,
        "SIGINT default wrapper exists",
    );
    let mut command = Command::new("/usr/bin/python3");
    command.args(["-I", "-B"]).arg(wrapper).arg(executable);
    command
        .args([
            "signal_driver",
            "--exact",
            "--nocapture",
            "--test-threads=1",
            "--format=pretty",
        ])
        .env_clear()
        .current_dir(runtime.join("project"))
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .envs([
            ("LANG", "C"),
            ("LC_ALL", "C"),
            ("TZ", "UTC"),
            ("TERM", "dumb"),
            ("NO_COLOR", "1"),
            ("ANALYTICS_ENABLED", "0"),
            ("DOCKER_CONTAINER", "1"),
            ("PILOTDECK_GATEWAY_HOST", "127.0.0.1"),
            ("PILOTDECK_GATEWAY_PORT", "28999"),
            ("PILOTDECK_SDK_GATEWAY_URL", "ws://127.0.0.1:28999/ws"),
            ("PILOTDECK_GATEWAY_URL", "ws://127.0.0.1:28999/ws"),
            ("PILOTDECK_SDK_AUTH_TOKEN", "d1-fixture-not-a-secret"),
            ("COMMIT_HASH", "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"),
        ])
        .env("PDRS_SIGNAL_CHILD", id)
        .env("PDRS_SIGNAL_OBSERVER", &observer)
        .env("PDRS_SIGNAL_ARGV", expected["input"]["argv"].to_string());
    for (key, path) in [
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
        command.env(key, runtime.join(path));
    }
    let child = OwnedChild {
        child: require(command.spawn(), id, "spawn isolated same-source product"),
        id: id.into(),
    };
    let cleanup = Arc::new(AtomicBool::new(false));
    let mut replay = Replay {
        id: id.into(),
        input: expected["input"].clone(),
        child,
        directory,
        observer,
        sockets: BTreeMap::new(),
        active: BTreeSet::new(),
        closes: BTreeMap::new(),
        hooks: Vec::new(),
        wire: Vec::new(),
        connections: 0,
        aborts: 0,
        registered: false,
        removed: false,
        abort_entered: false,
        terminal_consumed: false,
        terminal_after_abort: false,
        late_abort_handled: false,
        resolved_held: false,
        trigger: false,
        first_signal: false,
        second_signal: false,
        held: None,
        stream: None,
        pending_abort: None,
        deadline: Instant::now() + OBSERVATION,
        cleanup,
    };
    let (events, mut receiver) = mpsc::unbounded_channel();
    let mut actors = Vec::new();
    let mut expired = false;
    let status = loop {
        replay.read_hooks();
        while let Ok(event) = receiver.try_recv() {
            replay.on_event(event);
        }
        if let Some(status) = replay.child.poll() {
            break status;
        }
        if Instant::now() >= replay.deadline {
            ensure(
                !expired,
                id,
                "owned SIGKILL child reaped within cleanup bound",
            );
            expired = true;
            replay.cleanup.store(true, Ordering::SeqCst);
            require(
                replay.child.child.kill(),
                id,
                "bounded observation SIGKILL cleanup",
            );
            replay.deadline = Instant::now() + CLEANUP;
        }
        tokio::select! {
            accepted = listener.accept() => {
                let (socket, address) = require(accepted, id, "accept owned mock connection");
                ensure(address.ip().is_loopback(), id, "only loopback mock connections");
                replay.connections += 1;
                let connection = replay.connections;
                replay.active.insert(connection);
                let (sender, actions) = mpsc::unbounded_channel();
                replay.sockets.insert(connection, sender);
                actors.push(tokio::spawn(socket_actor(socket, connection, actions, events.clone(), Arc::clone(&replay.cleanup))));
            }
            event = receiver.recv() => if let Some(event) = event { replay.on_event(event); },
            _ = tokio::time::sleep(Duration::from_millis(2)) => {},
        }
    };
    drop(listener);
    let deadline = Instant::now() + CLEANUP;
    while !replay.active.is_empty() {
        replay.read_hooks();
        ensure(
            Instant::now() < deadline,
            id,
            "all connections released after child exit",
        );
        match tokio::time::timeout(Duration::from_millis(10), receiver.recv()).await {
            Ok(Some(event)) => replay.on_event(event),
            Ok(None) => panic!("#1145 {id} mock event channel closed before socket release"),
            Err(_) => {}
        }
    }
    replay.read_hooks();
    replay.sockets.clear();
    for actor in actors {
        require(
            require(
                tokio::time::timeout(CLEANUP, actor).await,
                id,
                "bounded socket actor completion",
            ),
            id,
            "socket actor completion",
        );
    }
    let stdout = require(fs::read(&stdout_path), id, "raw product stdout");
    let stdout = stdout
        .strip_prefix(DRIVER_PREFIX.as_bytes())
        .unwrap_or_else(|| {
            panic!(
                "#1145 {id} exact libtest driver prefix: {:?}",
                String::from_utf8_lossy(&stdout)
            )
        });
    private_file(&replay.directory.join("stdout.bin"), stdout, id);
    let stderr = require(fs::read(&stderr_path), id, "raw product stderr");
    private_file(&replay.directory.join("stderr.bin"), &stderr, id);
    let exit =
        json!({"code":status.code(),"signal":status.signal().map(signal_name),"cleanup":expired});
    let milestones = json!({"observer":true,"trigger":replay.trigger,
        "firstSignal":replay.input["signal"].is_null() || replay.first_signal,
        "abortEntered":replay.input["signal"].is_null() || replay.stage()=="control" || replay.abort_entered,
        "secondSignal":replay.input["second"].is_null() || replay.second_signal,
        "predecessorAction":!matches!(replay.action(),"release"|"reject") || replay.resolved_held,
        "terminalConsumed":replay.stage()!="terminal" || replay.terminal_consumed,
        "terminalAfterAbort":replay.stage()!="abort-terminal" || replay.terminal_after_abort,
        "lateAbortHandled":replay.stage()!="abort-terminal" || replay.late_abort_handled,
        "blockingStdin":true});
    let actual = json!({"input":replay.input,"status":if expired {"OBSERVED_TIMEOUT"} else {"OBSERVED_EXIT"},
        "milestones":milestones,"aborts":replay.aborts,"connections":replay.connections,
        "activeAfterExit":replay.active.len(),"naturalExit":if expired {Value::Null} else {exit.clone()},
        "cleanupExit":if expired {exit} else {Value::Null},
        "stdout":String::from_utf8_lossy(stdout),"stderrRaw":String::from_utf8_lossy(&stderr),
        "stdoutHex":stdout.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
        "stderrHex":stderr.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
        "initial":initial,"final":snapshot(&runtime,id),"hooks":replay.hooks,"wire":replay.wire,
        "closes":replay.closes.into_values().collect::<Vec<_>>()});
    publish(&replay.directory, "actual.json", &actual, id);
    actual
}

fn compare_surfaces(
    id: &str,
    expected: &Value,
    actual: &Value,
    stdout: &[u8],
    stderr: &[u8],
) -> Vec<String> {
    let mut differences = Vec::new();
    for field in [
        "status",
        "naturalExit",
        "cleanupExit",
        "aborts",
        "connections",
        "activeAfterExit",
        "initial",
        "final",
        "milestones",
    ] {
        if actual[field] != expected[field] {
            differences.push(field.to_owned());
        }
    }
    let expected_stdout = expected["stdout"]
        .as_str()
        .unwrap_or_else(|| panic!("#1145 {id} TS stdout bytes"));
    if stdout != expected_stdout.as_bytes() {
        differences.push("stdout".into());
    }
    let expected_stderr = expected["stderrRaw"]
        .as_str()
        .unwrap_or_else(|| panic!("#1145 {id} TS stderr bytes"));
    if !stderr_matches_for_case(id, expected_stderr, stderr) {
        differences.push("stderr".into());
    }
    if !closes_match(id, expected, &actual["closes"]) {
        differences.push("connection closures".into());
    }
    let ts_removed = expected["hooks"]
        .as_array()
        .unwrap_or_else(|| panic!("#1145 {id} TS listener observations"))
        .iter()
        .any(|row| {
            row["hook"]["kind"] == "listener-removed"
                && row["hook"]["listeners"]["SIGINT"] == 0
                && row["hook"]["listeners"]["SIGTERM"] == 0
        });
    let rust_removed = actual["hooks"]
        .as_array()
        .unwrap_or_else(|| panic!("#1145 {id} Rust listener observations"))
        .iter()
        // Each independently consumed once-listener is also removed. TS's
        // removeListener hook reports both consumption and finally cleanup.
        .any(|row| row["kind"] == "listeners-removed" || row["kind"] == "abort-enter-2");
    if ts_removed != rust_removed {
        differences.push("listener removal".into());
    }
    differences
}

pub(super) fn run(id: &str) {
    // Bounded local serial ownership: all cells share only the explicitly owned
    // mock port. Poison from a preceding assertion must not suppress later cells.
    let deadline = Instant::now() + Duration::from_secs(1800);
    let _guard = loop {
        match SERIAL.try_lock() {
            Ok(guard) => break guard,
            Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                ensure(
                    Instant::now() < deadline,
                    id,
                    "bounded serial mock ownership",
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };
    let fixture = super::fixture_directory();
    let source: Value = require(
        serde_json::from_slice(&require(
            fs::read(fixture.join("source.json")),
            id,
            "TS original bytes",
        )),
        id,
        "TS original JSON",
    );
    ensure(
        std::panic::catch_unwind(|| {
            super::verify_local_oracle(&fixture);
            super::verify_all_files(&source, &fixture);
        })
        .is_ok(),
        id,
        "verified local golden and real TS producer bytes",
    );
    let captures = source["captures"]
        .as_array()
        .unwrap_or_else(|| panic!("#1145 {id} TS captured cells"));
    let expected = captures
        .iter()
        .find(|capture| capture["input"]["id"] == id)
        .unwrap_or_else(|| panic!("#1145 {id} unique TS captured cell"));
    ensure(
        captures
            .iter()
            .filter(|capture| capture["input"]["id"] == id)
            .count()
            == 1,
        id,
        "unique captured identity",
    );
    let evidence = std::env::var_os("PDRS_SIGNAL_RESULTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("pdrs-1145-replay-{}", uuid::Uuid::new_v4()))
        });
    require(fs::create_dir_all(&evidence), id, "replay evidence root");
    let directory = evidence.join(id);
    private_directory(&directory, id);
    let runtime = require(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build(),
        id,
        "mock runtime",
    );
    let actual = runtime.block_on(capture(expected, directory.clone(), id));
    let stdout = require(
        fs::read(directory.join("stdout.bin")),
        id,
        "product stdout bytes",
    );
    let stderr = require(
        fs::read(directory.join("stderr.bin")),
        id,
        "product stderr bytes",
    );
    let differences = compare_surfaces(id, expected, &actual, &stdout, &stderr);
    publish(
        &directory,
        "comparison.json",
        &json!({"id":id,"differences":differences,
        "normalization":"TS SQLite prefix and complete business-prefixed Node v22.22.0 tail only; UUID bytes stay literal",
        "acceptedDifferences":["D1-SDK-WS-CLOSE-REASON-001 (four query-hello-reject cells only)",
            "D1-SDK-SIGNAL-UNHANDLED-REJECTION-001 (eight exact signal cells; stderr only)"]}),
        id,
    );
    if !differences.is_empty() {
        eprintln!(
            "#1145 {id} differing surfaces: {differences:?}; evidence {}",
            directory.display()
        );
    }
    ensure(
        differences.is_empty(),
        id,
        "four surfaces, abort count, listeners and connections match TS",
    );
}

#[cfg(test)]
mod comparison_tests {
    use super::*;

    fn captured() -> &'static Value {
        static SOURCE: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
            serde_json::from_slice(include_bytes!("../fixtures/sdk-signals/source.json"))
                .expect("#1145 comparison original JSON")
        });
        &SOURCE
    }

    fn original(id: &str) -> &'static str {
        captured()["captures"]
            .as_array()
            .expect("#1145 comparison captures")
            .iter()
            .find(|row| row["input"]["id"] == id)
            .expect("#1145 comparison original case")["stderrRaw"]
            .as_str()
            .expect("#1145 comparison stderr bytes")
    }

    #[test]
    fn stderr_complete_node_tail_preserves_business_line() {
        let mut normalized = 0;
        let mut retained = 0;
        for row in captured()["captures"]
            .as_array()
            .expect("#1145 captured matrix")
        {
            let raw = row["stderrRaw"].as_str().expect("#1145 captured stderr");
            if raw.ends_with("Node.js v22.22.0\n") {
                let stripped = strip_sqlite_warning(raw);
                if stripped.starts_with("file:///") {
                    assert_eq!(
                        ts_stderr(raw),
                        stripped,
                        "#1145 unprefixed stack remains literal"
                    );
                    retained += 1;
                } else {
                    let business = stripped
                        .split_inclusive('\n')
                        .next()
                        .expect("#1145 business line");
                    assert!(
                        stderr_matches(raw, business.as_bytes()),
                        "#1145 complete TS tail preserves business bytes"
                    );
                    normalized += 1;
                }
            }
        }
        assert_eq!(
            (normalized, retained),
            (16, 8),
            "#1145 observed business-prefixed and unprefixed denominator"
        );
    }

    #[test]
    fn stderr_missing_node_version_is_not_normalized() {
        let raw = original("run--query-hello-reject-sigint");
        assert!(
            stderr_matches(raw, b"auth_failed\n"),
            "#1145 version positive control"
        );
        let bad = raw
            .strip_suffix("Node.js v22.22.0\n")
            .expect("#1145 original Node version");
        assert!(
            !stderr_matches(bad, b"auth_failed\n"),
            "#1145 incomplete Node version cannot pass"
        );
        assert!(
            stderr_matches(raw, b"auth_failed\n"),
            "#1145 version restored control"
        );
    }

    #[test]
    fn stderr_missing_stack_frames_is_not_normalized() {
        let raw = original("run--query-hello-reject-sigint");
        assert!(
            stderr_matches(raw, b"auth_failed\n"),
            "#1145 frame positive control"
        );
        let bad: String = raw
            .split_inclusive('\n')
            .filter(|line| !line.starts_with("    at "))
            .collect();
        assert!(
            !stderr_matches(&bad, b"auth_failed\n"),
            "#1145 missing stack frames cannot pass"
        );
        assert!(
            stderr_matches(raw, b"auth_failed\n"),
            "#1145 frame restored control"
        );
    }

    #[test]
    fn stderr_changed_business_line_is_not_hidden() {
        let raw = original("run--query-hello-reject-sigint");
        assert!(
            stderr_matches(raw, b"auth_failed\n"),
            "#1145 business positive control"
        );
        let bad = raw.replacen("\nauth_failed\n", "\nchanged business\n", 1);
        assert_eq!(
            ts_stderr(&bad),
            strip_sqlite_warning(&bad),
            "#1145 mismatched business and exception remain literal"
        );
        assert!(
            !stderr_matches(&bad, b"auth_failed\n"),
            "#1145 changed TS business line cannot pass"
        );
        assert!(
            !stderr_matches(raw, b"changed business\n"),
            "#1145 changed Rust business line cannot pass"
        );
        assert!(
            stderr_matches(raw, b"auth_failed\n"),
            "#1145 business restored control"
        );
    }

    #[test]
    fn stderr_rust_node_stack_is_not_normalized() {
        let raw = original("run--query-hello-reject-sigint");
        assert!(
            stderr_matches(raw, b"auth_failed\n"),
            "#1145 Rust stack positive control"
        );
        assert!(
            !stderr_matches(raw, strip_sqlite_warning(raw).as_bytes()),
            "#1145 Rust Node stack cannot pass"
        );
        assert!(
            !stderr_matches(raw, raw.as_bytes()),
            "#1145 Rust SQLite prefix cannot pass"
        );
        assert!(
            stderr_matches(raw, b"auth_failed\n"),
            "#1145 Rust stack restored control"
        );
    }

    #[test]
    fn stderr_partial_or_extended_shapes_remain_literal() {
        let raw = strip_sqlite_warning(original("run--query-hello-reject-sigint"));
        let bad = [
            raw.replace("Node.js v22.22.0", "Node.js v22.22.1"),
            raw.replace("\n                          ^\n", "\n"),
            raw.replace("  requestId: undefined,\n", ""),
            raw.replace(
                "  [cause]: undefined\n",
                "  [cause]: undefined,\n  extra: true\n",
            ),
            raw.replace('\n', "\r\n"),
            format!("{raw}extra\n"),
            format!("{raw}{raw}"),
        ];
        for value in bad {
            assert_ne!(value, raw, "#1145 shape mutation must change input");
            assert_eq!(
                ts_stderr(&value),
                value,
                "#1145 partial or extended shape remains literal"
            );
        }
        assert!(
            stderr_matches(raw, b"auth_failed\n"),
            "#1145 shape restored control"
        );
    }

    #[test]
    fn stderr_blank_source_or_stack_body_is_not_normalized() {
        let raw = strip_sqlite_warning(original("run--query-hello-reject-sigint"));
        let source = raw.lines().nth(2).expect("#1145 original source excerpt");
        let blank_source = raw.replacen(source, "  ", 1);
        assert!(
            stderr_matches(raw, b"auth_failed\n"),
            "#1145 nonblank positive control"
        );
        assert!(
            !stderr_matches(&blank_source, b"auth_failed\n"),
            "#1145 blank source excerpt cannot pass"
        );
        let no_frames: String = raw
            .split_inclusive('\n')
            .filter(|line| !line.starts_with("    at "))
            .collect();
        let blank_frame = no_frames.replacen(
            "PilotDeckError: auth_failed\n",
            "PilotDeckError: auth_failed\n    at   {\n",
            1,
        );
        assert!(
            !stderr_matches(&blank_frame, b"auth_failed\n"),
            "#1145 blank stack body cannot pass"
        );
        assert!(
            stderr_matches(raw, b"auth_failed\n"),
            "#1145 nonblank restored control"
        );
    }

    // Independent expected identities: adding a ninth production entry must fail.
    const UNHANDLED_CASES: [(&str, &[u8]); 8] = [
        ("run--abort-error-sigint", b"d1 fixture rejected\n"),
        ("run--abort-error-sigterm", b"d1 fixture rejected\n"),
        ("resume--abort-error-sigint", b"d1 fixture rejected\n"),
        ("resume--abort-error-sigterm", b"d1 fixture rejected\n"),
        (
            "run--terminal-during-abort-final-sigint",
            b"Gateway client closed.\n",
        ),
        (
            "run--terminal-during-abort-final-sigterm",
            b"Gateway client closed.\n",
        ),
        (
            "resume--terminal-during-abort-final-sigint",
            b"Gateway client closed.\n",
        ),
        (
            "resume--terminal-during-abort-final-sigterm",
            b"Gateway client closed.\n",
        ),
    ];

    #[test]
    fn unhandled_rejection_stderr_is_exact_and_scoped() {
        for (id, message) in UNHANDLED_CASES {
            let raw = original(id);
            assert!(
                stderr_matches_for_case(id, raw, message),
                "#1145 approved unhandled rejection line must pass: {id}"
            );
            let mut extra_line = message.to_vec();
            extra_line.extend_from_slice(b"extra output\n");
            for bad in [
                b"changed error\n".as_slice(),
                b"",
                raw.as_bytes(),
                &message[..message.len() - 1],
                extra_line.as_slice(),
            ] {
                assert!(
                    !stderr_matches_for_case(id, raw, bad),
                    "#1145 unhandled rejection requires exact Rust line: {id}"
                );
            }
        }
        // Supply the approved error bytes even for other cells, so expanding the
        // match list cannot hide behind an unrelated original stderr mismatch.
        for row in captured()["captures"]
            .as_array()
            .expect("#1145 rejection scope captures")
        {
            let id = row["input"]["id"]
                .as_str()
                .expect("#1145 rejection scope id");
            if UNHANDLED_CASES.iter().any(|(known, _)| *known == id) {
                continue;
            }
            for (source, message) in UNHANDLED_CASES {
                assert!(
                    !stderr_matches_for_case(id, original(source), message),
                    "#1145 unregistered unhandled rejection cannot pass: {id}"
                );
            }
        }
        for id in [
            "run--abort-error-sigint-extra",
            "other--abort-error-sigint",
            "run--terminal-during-abort-final-sighup",
        ] {
            assert!(
                !stderr_matches_for_case(id, original(UNHANDLED_CASES[0].0), UNHANDLED_CASES[0].1),
                "#1145 unregistered unhandled rejection cannot pass: {id}"
            );
        }
        for (id, message) in UNHANDLED_CASES {
            assert!(
                stderr_matches_for_case(id, original(id), message),
                "#1145 rejection restored positive"
            );
        }
    }

    #[test]
    fn unhandled_rejection_preserves_other_surfaces() {
        for (id, message) in UNHANDLED_CASES {
            let expected = captured()["captures"]
                .as_array()
                .expect("#1145 rejection surface captures")
                .iter()
                .find(|row| row["input"]["id"] == id)
                .expect("#1145 rejection surface original");
            let stdout = expected["stdout"]
                .as_str()
                .expect("#1145 rejection surface stdout")
                .as_bytes();
            let mut actual = expected.clone();
            actual["closes"] = json!(
                expected["closes"]
                    .as_array()
                    .expect("#1145 rejection surface closes")
                    .iter()
                    .map(
                        |row| json!({"connection":row["connection"],"code":row["code"],
                    "reason":row["reason"],"cleanup":row["cleanup"]})
                    )
                    .collect::<Vec<_>>()
            );
            actual["closes"]
                .as_array_mut()
                .expect("#1145 rejection surface close array")
                .sort_by_key(|row| row["connection"].as_u64());
            // Abort rejection crashes before finally; final-terminal consumption
            // reaches finally and removes both listeners in the real TS capture.
            let removed = message == b"Gateway client closed.\n";
            actual["hooks"] = if removed {
                json!([{"kind":"listeners-removed"}])
            } else {
                json!([])
            };
            assert!(
                compare_surfaces(id, expected, &actual, stdout, message).is_empty(),
                "#1145 rejection all surfaces positive: {id}"
            );
            for (field, value) in [
                ("status", json!("wrong status")),
                (
                    "naturalExit",
                    json!({"code":0,"signal":null,"cleanup":false}),
                ),
                (
                    "cleanupExit",
                    json!({"code":1,"signal":null,"cleanup":true}),
                ),
                ("aborts", json!(99)),
                ("connections", json!(99)),
                ("activeAfterExit", json!(99)),
                ("initial", json!({})),
                ("final", json!({})),
                ("milestones", json!({})),
                ("closes", json!([])),
                (
                    "hooks",
                    if removed {
                        json!([])
                    } else {
                        json!([{"kind":"listeners-removed"}])
                    },
                ),
            ] {
                let mut bad = actual.clone();
                assert!(
                    bad[field] != value,
                    "#1145 rejection mutation changes {field}: {id}"
                );
                bad[field] = value;
                let surface = match field {
                    "closes" => "connection closures",
                    "hooks" => "listener removal",
                    other => other,
                };
                assert!(
                    compare_surfaces(id, expected, &bad, stdout, message) == [surface],
                    "#1145 rejection exception cannot hide {field}: {id}"
                );
            }
            let mut bad_stdout = stdout.to_vec();
            bad_stdout.extend_from_slice(b"extra output\n");
            assert!(
                compare_surfaces(id, expected, &actual, &bad_stdout, message) == ["stdout"],
                "#1145 rejection exception cannot hide stdout: {id}"
            );
            assert!(
                compare_surfaces(id, expected, &actual, stdout, b"wrong error\n") == ["stderr"],
                "#1145 rejection exception cannot hide stderr: {id}"
            );
            assert!(
                compare_surfaces(id, expected, &actual, stdout, message).is_empty(),
                "#1145 rejection all surfaces restored: {id}"
            );
        }
    }

    #[test]
    fn close_reason_exception_is_exact_and_scoped() {
        let id = "run--query-hello-reject-sigint";
        let expected = json!({"closes":[{"connection":2,"code":4003,"reason":"","cleanup":false}]});
        let actual = json!([{"connection":2,"code":4003,"reason":"auth_failed","cleanup":false}]);
        for id in [
            id,
            "run--query-hello-reject-sigterm",
            "resume--query-hello-reject-sigint",
            "resume--query-hello-reject-sigterm",
        ] {
            assert!(
                closes_match(id, &expected, &actual),
                "#1145 registered close reason positive control"
            );
            assert!(
                closes_match(id, &expected, &expected["closes"]),
                "#1145 identical close remains valid"
            );
        }
        for (field, value) in [
            ("connection", json!(1)),
            ("code", json!(4002)),
            ("reason", json!("other")),
            ("cleanup", json!(true)),
            ("extra", json!(true)),
        ] {
            let mut bad = actual.clone();
            bad[0][field] = value;
            assert!(
                !closes_match(id, &expected, &bad),
                "#1145 close exception cannot hide {field}"
            );
        }
        for (field, value) in [
            ("connection", json!(1)),
            ("code", json!(4002)),
            ("reason", json!("other")),
            ("cleanup", json!(true)),
        ] {
            let mut bad = expected.clone();
            bad["closes"][0][field] = value.clone();
            let mut observed = actual.clone();
            if field != "reason" {
                observed[0][field] = value;
            }
            assert!(
                !closes_match(id, &bad, &observed),
                "#1145 close exception requires TS {field}"
            );
        }
        let mut duplicate = actual.clone();
        duplicate
            .as_array_mut()
            .expect("#1145 close array")
            .push(actual[0].clone());
        assert!(
            !closes_match(id, &expected, &duplicate),
            "#1145 extra close cannot pass"
        );
        assert!(
            !closes_match("run--complete-cleanup", &expected, &actual),
            "#1145 unregistered close case cannot pass"
        );
        assert!(
            closes_match(id, &expected, &actual),
            "#1145 close reason restored control"
        );
    }

    #[test]
    fn signal_wrapper_restores_inherited_sigint_ignore() {
        let wrapper = super::super::fixture_directory().join("sigint_default.py");
        let launch = |wrapped: bool| {
            let mut command = Command::new("/usr/bin/python3");
            command.args(["-I", "-B", "-c", "import os,signal,sys; signal.signal(signal.SIGINT,signal.SIG_IGN); os.execv(sys.argv[1],sys.argv[1:])"]);
            if wrapped {
                command.args(["/usr/bin/python3", "-I", "-B"]).arg(&wrapper);
            }
            let child = command
                .args(["/bin/sh", "-c", "kill -INT $$; exit 99"])
                .spawn()
                .expect("#1145 spawn wrapper disposition probe");
            let mut child = OwnedChild {
                child,
                id: "wrapper-probe".into(),
            };
            let deadline = Instant::now() + CLEANUP;
            loop {
                if let Some(status) = child.poll() {
                    break status;
                }
                assert!(
                    Instant::now() < deadline,
                    "#1145 wrapper probe bounded; not a negative kill"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        assert_eq!(
            launch(true).signal(),
            Some(2),
            "#1145 wrapper restores SIGINT default before exec"
        );
        assert_eq!(
            launch(false).code(),
            Some(99),
            "#1145 missing wrapper retains inherited SIGINT ignore"
        );
        assert_eq!(
            launch(true).signal(),
            Some(2),
            "#1145 wrapper restored positive control"
        );
    }
}

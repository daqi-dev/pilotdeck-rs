//! HookRuntime/executor/lifecycle replay against the real TS oracle.
//!
//! `tests/fixtures/hooks-runtime/generate.mts` ran the fixed baseline: command
//! hooks spawned real scripts in its own mkdtemp tree, HTTP hooks used a
//! loopback server, callbacks were injected, and LifecycleRuntime dispatched
//! in-process. This test recreates the same scripts in its own scratch tree,
//! starts an equivalent loopback server, and compares every result byte for
//! byte (paths are masked as `<dir>`/`<url>`/`<refused>`/`<cwd>`).
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::{
    hooks::{config::parse_hooks_config, protocol::*},
    host::{HostError, modules::lifecycle as host_lifecycle},
    runtime::hooks::{
        exec::{
            CallbackFuture, CallbackHookHandler, CallbackInput, CallbackResult, Cancellation,
            CommandHookExecutor, CommandHookOptions, EvaluatorFuture, EvaluatorInput,
            ExecutionResult, HookOutcome, HttpHookExecutor, HttpHookOptions, LocalShellPort,
            PromptEvaluator, PromptHookExecutor, hook_output_to_value, parse_hook_output,
        },
        pending::AsyncHookCompletion,
        runtime::{
            HookExecutionEventHandler, HookRuntime, HookRuntimeOptions, HookRuntimeRunInput,
            LifecycleDispatchInput, LifecycleRuntime, lifecycle_module_failure,
            validate_lifecycle_dispatch_result,
        },
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use scratch::Scratch;
use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    time::{sleep, timeout},
};

static UNDEFINED: JsValue = JsValue::Undefined;
const FIXED_START: &str = "2026-01-01T00:00:00.000Z";

fn oracle() -> JsValue {
    baseline::oracle(include_str!("fixtures/hooks-runtime/oracle.json"))
}
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn string(value: &JsValue) -> JsString {
    value.as_string().expect("string").clone()
}
fn text(value: &JsValue) -> String {
    string(value).to_utf8().expect("utf8")
}
fn number(value: &JsValue) -> f64 {
    value.as_number().expect("number")
}
fn bool_value(value: &JsValue) -> bool {
    value.as_bool().expect("bool")
}
fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => panic!("expected array: {value:?}"),
    }
}
fn element(value: &JsValue, index: usize) -> &JsValue {
    &array(value)[index]
}
fn object(value: &JsValue) -> JsObject {
    value.as_object().expect("object").clone()
}
fn json(value: &JsValue) -> String {
    stringify_json(value).unwrap().expect("json")
}
fn scratch_root(scratch: &Scratch) -> String {
    scratch.path().to_string_lossy().into_owned()
}
/// Mask this run's scratch tree (and its canonical `/private/...` spelling) the
/// same way the generator masked its own tree.
fn mask_paths(text: &str, root: &str) -> String {
    let real = std::fs::canonicalize(root)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| root.to_owned());
    text.replace(&real, "<dir>").replace(root, "<dir>")
}
/// Fixture JSON values are small and bounded; serialize/replace/reparse keeps
/// placeholder substitution in one place without recursive product code.
fn substitute(value: &JsValue, pairs: &[(&str, &str)]) -> JsValue {
    let mut text = json(value);
    for (from, to) in pairs {
        text = text.replace(from, to);
    }
    parse_json(&text).unwrap()
}
fn substitute_text(value: &str, pairs: &[(&str, &str)]) -> String {
    let mut text = value.to_owned();
    for (from, to) in pairs {
        text = text.replace(from, to);
    }
    text
}
fn module_message(error: HostError) -> String {
    let module = error.module_error.expect("module error");
    String::from_utf16_lossy(module.message.units())
}

// ---------------------------------------------------------------- parseHookOutput

#[tokio::test]
async fn parse_hook_output_matches_real_ts() {
    let oracle = oracle();
    assert_eq!(text(field(&oracle, "nodeVersion")), "v22.22.0");
    assert_eq!(
        field(&oracle, "sourceSha256")
            .as_object()
            .expect("source hashes")
            .entries()
            .len(),
        25
    );
    let cases = array(field(&oracle, "parse"));
    assert_eq!(cases.len(), 52);
    for case in cases {
        let stdout = text(field(case, "stdout"));
        let actual = hook_output_to_value(&parse_hook_output(&stdout));
        assert_eq!(
            json(&actual),
            text(field(case, "expected")),
            "{}",
            text(field(case, "name"))
        );
    }
}

// ---------------------------------------------------------------- command

fn write_scripts(oracle: &JsValue, root: &str) {
    let scripts = oracle
        .get("scripts")
        .and_then(JsValue::as_object)
        .expect("scripts");
    assert_eq!(scripts.entries().len(), 16);
    for (path, content) in scripts.entries() {
        let path = path.to_utf8().unwrap().replace("<dir>", root);
        let path = std::path::Path::new(&path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text(content)).unwrap();
    }
}

/// Kills a background process a fixture started, including when an assertion
/// panics, so a failed replay cannot leave an orphan behind.
struct BackgroundProcess {
    pid: i32,
    armed: bool,
}
impl BackgroundProcess {
    fn new(pid: i32) -> Self {
        Self { pid, armed: true }
    }
    fn alive(&self) -> bool {
        rustix::process::Pid::from_raw(self.pid)
            .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
    }
    fn kill(&mut self) {
        if self.armed
            && let Some(pid) = rustix::process::Pid::from_raw(self.pid)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
    /// The process is confirmed gone; never signal this PID again in case it
    /// was reused before the guard drops.
    fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for BackgroundProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

#[tokio::test]
async fn command_executor_matches_real_ts_subprocesses() {
    let oracle = oracle();
    let scratch = Scratch::new("hooks-runtime-command");
    let root = scratch_root(&scratch);
    write_scripts(&oracle, &root);
    let cases = array(field(&oracle, "command"));
    assert_eq!(cases.len(), 27);
    for case in cases {
        let name = text(field(case, "name"));
        let command = substitute_text(&text(field(case, "command")), &[("<dir>", &root)]);
        let cwd = substitute_text(&text(field(case, "cwd")), &[("<dir>", &root)]);
        let signal = match case.get("abortAtMs") {
            None => None,
            Some(ms) => {
                let signal = Cancellation::new();
                let ms = number(ms);
                if ms == 0.0 {
                    signal.abort();
                } else {
                    let signal = signal.clone();
                    tokio::spawn(async move {
                        sleep(Duration::from_millis(ms as u64)).await;
                        signal.abort();
                    });
                }
                Some(signal)
            }
        };
        let result = timeout(
            Duration::from_secs(15),
            CommandHookExecutor::default().execute(CommandHookOptions {
                command: command.into(),
                hook_input: object(field(case, "hookInput")),
                cwd: cwd.into(),
                env: case.get("env").map(object),
                signal,
                timeout_ms: case.get("timeoutMs").map(number),
            }),
        )
        .await
        .unwrap_or_else(|_| panic!("command case {name} did not resolve within 15s"));
        // The guard is created before the comparison so a mismatch still kills
        // the background process this case started.
        let background = case.get("detachedPidFile").map(|value| {
            let path = substitute_text(&text(value), &[("<dir>", &root)]);
            let pid = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("background pid file {path}: {error}"))
                .trim()
                .parse::<i32>()
                .expect("background pid");
            BackgroundProcess::new(pid)
        });
        assert_eq!(
            mask_paths(&json(&result.to_value()), &root),
            text(field(case, "expected")),
            "command case {name}"
        );
        if let Some(marker) = case.get("markerFile") {
            // The result resolves at close; TS then lets the TERM handler finish
            // inside its 3 s SIGKILL grace. A descendant that cleans up in the
            // handler must have written the marker by the recorded deadline.
            let marker = substitute_text(&text(marker), &[("<dir>", &root)]);
            let wait_ms = number(field(case, "markerWaitMs")) as u64;
            sleep(Duration::from_millis(wait_ms)).await;
            assert!(
                std::path::Path::new(&marker).exists(),
                "command case {name}: TERM-handler cleanup marker {marker} is missing (SIGKILL arrived before the grace expired)"
            );
            let _ = std::fs::remove_file(&marker);
        }
        if let Some(mut background) = background {
            let delay = number(field(case, "detachedAliveAfterMs")) as u64;
            sleep(Duration::from_millis(delay)).await;
            assert!(
                background.alive(),
                "command case {name}: background process {} must survive a successful hook",
                background.pid
            );
            background.kill();
            for _ in 0..200 {
                if !background.alive() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
            assert!(
                !background.alive(),
                "command case {name}: background process {} was not cleaned up",
                background.pid
            );
            background.disarm();
        }
    }
}

/// #363-1: a command hook's stdout/stderr are capped like the bash tool's
/// `StreamText` at V8 `MAX_STRING_LENGTH` UTF-16 units. TS's shell runner
/// appends with `stdout += text` and throws `RangeError: Invalid string
/// length` for a chunk that does not fit (`tool/builtin/bash/commandRunner.ts:132`,
/// probe `fixtures/hooks-output/generate.mts`). TS's hook catch does not
/// receive this asynchronous exception; Rust's `non_blocking_error` is the
/// approved A6.7-HOOK-OUTPUT-001 / -R1 containment. The limit is injected
/// small here; the product default is 0x1fffffe8 units.
#[tokio::test]
async fn command_output_over_string_limit_is_a_range_error() {
    fn options(command: &str) -> CommandHookOptions {
        CommandHookOptions {
            command: command.into(),
            hook_input: JsObject::new(),
            cwd: "/tmp".into(),
            env: None,
            signal: None,
            timeout_ms: Some(5_000.0),
        }
    }
    fn project(result: &ExecutionResult) -> (HookOutcome, String, String, Option<i64>) {
        (
            result.outcome,
            String::from_utf16_lossy(result.stdout.units()),
            String::from_utf16_lossy(result.stderr.units()),
            result.exit_code,
        )
    }
    let executor = CommandHookExecutor::new(Arc::new(LocalShellPort::with_output_limit(10)));
    // Exactly at the cap succeeds; one unit over fails with V8's message.
    assert_eq!(
        project(&executor.execute(options("printf 0123456789")).await),
        (
            HookOutcome::Success,
            "0123456789".to_owned(),
            String::new(),
            Some(0)
        )
    );
    for (stream, command) in [
        ("stdout", "printf 0123456789a"),
        ("stderr", "printf 0123456789a >&2"),
    ] {
        assert_eq!(
            project(&executor.execute(options(command)).await),
            (
                HookOutcome::NonBlockingError,
                String::new(),
                "Invalid string length".to_owned(),
                None
            ),
            "{stream} over the cap"
        );
    }
    // The cap counts UTF-16 units, not bytes: U+1F600 is two units and its
    // UTF-8 form is four bytes.
    let emoji = CommandHookExecutor::new(Arc::new(LocalShellPort::with_output_limit(3)));
    assert_eq!(
        project(&emoji.execute(options("printf '😀a'")).await),
        (
            HookOutcome::Success,
            "😀a".to_owned(),
            String::new(),
            Some(0)
        )
    );
    assert_eq!(
        project(&emoji.execute(options("printf '😀ab'")).await),
        (
            HookOutcome::NonBlockingError,
            String::new(),
            "Invalid string length".to_owned(),
            None
        )
    );
    // Exit 2 stays blocking: overflow must not turn a refusing policy hook
    // into `non_blocking_error` (fail-open).
    assert_eq!(
        project(
            &executor
                .execute(options("printf 0123456789a; exit 2"))
                .await
        ),
        (HookOutcome::Blocking, String::new(), String::new(), Some(2))
    );
}

/// Negative control: a timeout must actually kill the owned process group and
/// reap the leader, not just resolve a future and leave an orphan.
#[tokio::test]
async fn timed_out_command_leaves_no_process() {
    let scratch = Scratch::new("hooks-runtime-reap");
    let root = scratch_root(&scratch);
    let pid_file = std::path::Path::new(&root).join("pid");
    let script = std::path::Path::new(&root).join("sleep.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/bash\nprintf '%s' \"$$\" > '{}'\nprintf 'start\\n'\nsleep 30\n",
            pid_file.display()
        ),
    )
    .unwrap();
    let result = timeout(
        Duration::from_secs(10),
        CommandHookExecutor::default().execute(CommandHookOptions {
            command: format!("/bin/bash '{}'", script.display()).into(),
            hook_input: JsObject::new(),
            cwd: root.clone().into(),
            env: None,
            signal: None,
            timeout_ms: Some(300.0),
        }),
    )
    .await
    .expect("timeout case must resolve");
    assert_eq!(result.outcome, HookOutcome::Timeout);
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("script wrote its pid")
        .parse()
        .unwrap();
    let pid = rustix::process::Pid::from_raw(pid).expect("pid");
    // TS resolves the result at close but keeps its 3 s KILL timer; the leader
    // intentionally stays an unreaped zombie (reserving the PGID) until the
    // deferred group KILL, so wait past the grace before requiring ESRCH.
    let mut dead = false;
    for _ in 0..200 {
        if rustix::process::test_kill_process(pid).is_err() {
            dead = true;
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert!(dead, "timed out shell leader {pid:?} is still alive");
}

/// gate3 P1: a timeout whose result resolves at pipe EOF must still honour
/// TS's TERM -> 3 s -> KILL timeline. The leader's stdio closes early (its
/// background descendant is redirected), so the result is immediate; the
/// descendant ignores TERM and must survive the grace, then die from the
/// deferred group SIGKILL that the background reaper sends.
#[tokio::test]
async fn timeout_result_returns_before_the_group_kill_grace() {
    let scratch = Scratch::new("hooks-runtime-grace-reap");
    let root = scratch_root(&scratch);
    let pid_file = std::path::Path::new(&root).join("bg.pid");
    // The leader only sleeps, so its pipes close as soon as TERM kills it; the
    // redirected descendant ignores TERM and keeps the group alive.
    let command = format!(
        "bash -c 'trap \"\" TERM; while :; do sleep 0.2; done' >/dev/null 2>&1 & echo $! > '{}'; sleep 30",
        pid_file.display()
    );
    let started = Instant::now();
    let result = timeout(
        Duration::from_secs(10),
        CommandHookExecutor::default().execute(CommandHookOptions {
            command: command.into(),
            hook_input: JsObject::new(),
            cwd: root.clone().into(),
            env: None,
            signal: None,
            timeout_ms: Some(500.0),
        }),
    )
    .await
    .expect("timeout case must resolve");
    let elapsed = started.elapsed();
    assert_eq!(result.outcome, HookOutcome::Timeout);
    assert!(
        elapsed < Duration::from_millis(2000),
        "result must resolve at close (about the timeout), not after the grace: {elapsed:?}"
    );
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("leader wrote the background pid")
        .trim()
        .parse()
        .unwrap();
    let mut survivor = BackgroundProcess::new(pid);
    // Inside the grace the descendant is still alive: the old code KILLed the
    // group at pipe EOF, which the next assertion alone would not catch.
    sleep(Duration::from_millis(1000)).await;
    assert!(
        survivor.alive(),
        "descendant {pid} was killed before the 3 s grace expired"
    );
    let mut dead = false;
    for _ in 0..100 {
        if !survivor.alive() {
            dead = true;
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert!(
        dead,
        "deferred group SIGKILL never reached descendant {pid}"
    );
    survivor.disarm();
}

/// C6: a same-group grandchild can keep stdin open while the command never
/// reads the >64 KiB payload, leaving the writer blocked. Finishing the hook
/// must abort that writer task, not leak it past the execution.
#[tokio::test]
async fn command_cleanup_aborts_stdin_writer() {
    let before = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    let mut input = JsObject::new();
    input.insert("big".into(), JsValue::String("x".repeat(200_000).into()));
    let result = timeout(
        Duration::from_secs(10),
        CommandHookExecutor::default().execute(CommandHookOptions {
            // The shell exits immediately; `sleep` inherits all three pipes and
            // neither reads stdin nor closes stdout.
            command: "sleep 30 &".into(),
            hook_input: input,
            cwd: std::env::temp_dir().to_string_lossy().into_owned().into(),
            env: None,
            signal: None,
            timeout_ms: Some(500.0),
        }),
    )
    .await
    .expect("cleanup must not hang while a grandchild holds the pipes");
    assert_eq!(result.outcome, HookOutcome::Timeout);
    // The timeout path intentionally leaves one deferred-reaper task until the
    // 3 s grace expires; wait for it to finish, then require that no stdin
    // writer (or anything else) leaked. A leaked writer keeps the count above
    // the baseline for the whole poll window.
    let mut settled = false;
    for _ in 0..200 {
        if tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks()
            <= before
        {
            settled = true;
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert!(settled, "stdin writer task leaked past command cleanup");
}

/// #552 修复轮 1: TS runs command hooks through `NodeShellCommandRunner`
/// (`commandRunner.ts` `spawn(..., { detached: !isWindows })` via the hook
/// executor's `ShellPort`), so the hook shell must lead a new session of its
/// own: `getsid(shell) == shell pid`, different from the test process's
/// session, asserted while the hook is still running. Dropping the pending
/// run must still reclaim the process group.
#[tokio::test]
async fn command_hook_shell_leads_its_own_session() {
    let scratch = Scratch::new("hooks-session");
    let root = scratch_root(&scratch);
    let pid_file = format!("{root}/leader.pid");
    // The loop stops on its own once the spawning test process is gone, so a
    // killed test cannot leave this shell spinning (see grep's WATCH_PARENT).
    let command = format!(
        "echo $$ > '{pid_file}'; while kill -0 $PPID 2>/dev/null; do /bin/sleep 0.02; done"
    );
    let executor = CommandHookExecutor::default();
    let mut run = Box::pin(executor.execute(CommandHookOptions {
        command: command.into(),
        hook_input: object(&parse_json("{}").unwrap()),
        cwd: root.clone().into(),
        env: None,
        signal: None,
        timeout_ms: Some(600_000.0),
    }));
    let deadline = Instant::now() + Duration::from_secs(30);
    let leader = loop {
        assert!(Instant::now() < deadline, "no leader pid file appeared");
        tokio::select! {
            result = &mut run => panic!("the hook must still be running: {result:?}"),
            _ = sleep(Duration::from_millis(10)) => {}
        }
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .as_deref()
            .map(str::trim)
            .and_then(|text| text.parse::<i32>().ok())
        {
            break rustix::process::Pid::from_raw(pid).expect("valid pid");
        }
    };
    assert_eq!(
        rustix::process::getsid(Some(leader)).expect("getsid"),
        leader,
        "setsid: the hook shell leads its own session"
    );
    assert_ne!(
        rustix::process::getsid(Some(leader)).expect("getsid"),
        rustix::process::getsid(None).expect("getsid"),
        "the hook shell's session must differ from ours"
    );
    // Dropping the pending run kills the group; the session change must not
    // disturb that reclamation.
    drop(run);
    let deadline = Instant::now() + Duration::from_secs(10);
    while rustix::process::test_kill_process_group(leader).is_ok() {
        assert!(
            Instant::now() < deadline,
            "process group {} was not reclaimed",
            leader.as_raw_pid()
        );
        sleep(Duration::from_millis(10)).await;
    }
}

// ---------------------------------------------------------------- HTTP mock

fn response(status: u16, body: Vec<u8>) -> hyper::Response<http_body_util::Full<bytes::Bytes>> {
    hyper::Response::builder()
        .status(status)
        .body(http_body_util::Full::new(bytes::Bytes::from(body)))
        .unwrap()
}

const SELECTED_HEADERS: [&str; 7] = [
    "content-type",
    "x-a",
    "x-b",
    "x-c",
    "x-d",
    "authorization",
    "cookie",
];
fn selected_headers(headers: &hyper::HeaderMap) -> Vec<(String, String)> {
    SELECTED_HEADERS
        .into_iter()
        .filter_map(|name| {
            headers.get(name).map(|value| {
                // Node's http server exposes header bytes as latin1 strings
                // (`José` is E9 on the wire), so decode the same way here.
                let value = value
                    .as_bytes()
                    .iter()
                    .map(|byte| char::from(*byte))
                    .collect();
                (name.to_owned(), value)
            })
        })
        .collect()
}

/// Every wire line of each selected name (mirrors the generator's
/// `rawHeaders` scan), so a header split into two lines cannot pass.
fn selected_header_lines(headers: &hyper::HeaderMap) -> JsObject {
    let mut lines = JsObject::new();
    for name in SELECTED_HEADERS {
        let values: Vec<JsValue> = headers
            .get_all(name)
            .iter()
            .map(|value| {
                let text: String = value.as_bytes().iter().map(|b| char::from(*b)).collect();
                JsValue::String(text.into())
            })
            .collect();
        if !values.is_empty() {
            lines.insert(name.into(), JsValue::Array(values));
        }
    }
    lines
}

fn headers_json(headers: &[(String, String)]) -> String {
    let entries: Vec<String> = headers
        .iter()
        .map(|(name, value)| {
            format!(
                "{}:{}",
                serde_json::to_string(name).unwrap(),
                serde_json::to_string(value).unwrap()
            )
        })
        .collect();
    format!("{{{}}}", entries.join(","))
}

/// Mirrors the generator's redirect table: (status, Location) by path.
fn redirect_for(path: &str, base: &str, refused: &str) -> Option<(u16, Option<String>)> {
    if let Some(n) = path
        .strip_prefix("/chain/")
        .and_then(|n| n.parse::<u32>().ok())
        .filter(|n| *n > 0)
    {
        return Some((302, Some(format!("/chain/{}", n - 1))));
    }
    let (status, location) = match path {
        "/redirect/301" => (301, Some("/echo".to_owned())),
        "/redirect/302" => (302, Some("/echo".to_owned())),
        "/redirect/303" => (303, Some("/echo".to_owned())),
        "/redirect/307" => (307, Some("/echo".to_owned())),
        "/redirect/308" => (308, Some(format!("{base}/echo"))),
        "/redirect/300" => (300, Some("/echo".to_owned())),
        "/redirect/no-location" => (302, None),
        "/redirect/307-to-302" => (307, Some("/redirect/302".to_owned())),
        "/redirect/slow" => (307, Some("/slow".to_owned())),
        "/redirect/refused" => (302, Some(format!("{refused}/x"))),
        "/redirect/blocked-port" => (302, Some("http://127.0.0.1:6000/x".to_owned())),
        "/nested/deep/relative" => (302, Some("../../echo".to_owned())),
        "/redirect/cross-origin" => (
            307,
            Some(format!(
                "{}/echo",
                base.replace("http://127.0.0.1:", "http://localhost:")
            )),
        ),
        _ => return None,
    };
    Some((status, location))
}

async fn serve_connection(
    stream: tokio::net::TcpStream,
    recorded: Arc<Mutex<Vec<JsValue>>>,
    base: Arc<str>,
    refused: Arc<str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use http_body_util::BodyExt;
    let service =
        hyper::service::service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
            let recorded = recorded.clone();
            let (base, refused) = (base.clone(), refused.clone());
            async move {
                let path = request.uri().path().to_owned();
                let method = request.method().as_str().to_owned();
                let headers = selected_headers(request.headers());
                let header_lines = selected_header_lines(request.headers());
                let body = match request.into_body().collect().await {
                    Ok(body) => body.to_bytes().to_vec(),
                    Err(_) => Vec::new(),
                };
                let body_text = String::from_utf8_lossy(&body).into_owned();
                let mut recorded_request = JsObject::new();
                recorded_request.insert("method".into(), JsValue::String(method.into()));
                recorded_request.insert("path".into(), JsValue::String(path.clone().into()));
                let mut header_object = JsObject::new();
                for (name, value) in &headers {
                    header_object
                        .insert(name.clone().into(), JsValue::String(value.clone().into()));
                }
                recorded_request.insert("headers".into(), JsValue::Object(header_object));
                recorded_request.insert("headerLines".into(), JsValue::Object(header_lines));
                recorded_request.insert("body".into(), JsValue::String(body_text.clone().into()));
                recorded
                    .lock()
                    .unwrap()
                    .push(JsValue::Object(recorded_request));

                if let Some((status, location)) = redirect_for(&path, &base, &refused) {
                    let mut redirect =
                        response(status, br#"{"systemMessage":"redirect body"}"#.to_vec());
                    if let Some(location) = location {
                        redirect
                            .headers_mut()
                            .insert(hyper::header::LOCATION, location.parse().unwrap());
                    }
                    return Ok::<_, Infallible>(redirect);
                }
                let response = match path.as_str() {
                    "/bad" => response(400, br#"{"continue":false,"reason":"bad"}"#.to_vec()),
                    "/empty" => response(204, Vec::new()),
                    "/invalid-utf8" => response(200, vec![0xff, 0xfe]),
                    "/slow" => {
                        sleep(Duration::from_secs(5)).await;
                        response(200, b"slow".to_vec())
                    }
                    "/echo" => {
                        let echo = format!(
                            "{{\"body\":{},\"headers\":{}}}",
                            serde_json::to_string(&body_text).unwrap(),
                            headers_json(&headers)
                        );
                        response(200, echo.into_bytes())
                    }
                    "/server-error" => response(500, Vec::new()),
                    _ => response(200, br#"{"systemMessage":"hello"}"#.to_vec()),
                };
                Ok::<_, Infallible>(response)
            }
        });
    hyper::server::conn::http1::Builder::new()
        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
        .await?;
    Ok(())
}

async fn mock_server(
    recorded: Arc<Mutex<Vec<JsValue>>>,
    refused: &str,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (shared_base, refused): (Arc<str>, Arc<str>) = (base.as_str().into(), refused.into());
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let recorded = recorded.clone();
            let (base, refused) = (shared_base.clone(), refused.clone());
            tokio::spawn(async move {
                let _ = serve_connection(stream, recorded, base, refused).await;
            });
        }
    });
    (base, task)
}

/// Echoes the prompt it receives, like the generator's evaluator.
struct EchoEvaluator;
impl PromptEvaluator for EchoEvaluator {
    fn evaluate(&self, input: EvaluatorInput) -> EvaluatorFuture {
        Box::pin(async move { Ok(input.prompt) })
    }
}

#[tokio::test]
async fn prompt_arguments_replace_matches_real_ts() {
    let oracle = oracle();
    let cases = array(field(field(&oracle, "prompt"), "cases"));
    assert_eq!(cases.len(), 3);
    let executor = PromptHookExecutor::new(Arc::new(EchoEvaluator));
    for case in cases {
        let name = text(field(case, "name"));
        let result = executor
            .execute(
                &string(field(case, "prompt")),
                None,
                &object(field(case, "hookInput")),
                None,
            )
            .await;
        assert_eq!(
            json(&result.to_value()),
            text(field(case, "expected")),
            "prompt case {name}"
        );
    }
}

#[tokio::test]
async fn http_executor_matches_real_ts_and_records_requests() {
    let oracle = oracle();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let closed = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let refused = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let (base, server) = mock_server(recorded.clone(), &refused).await;
    let cases = array(field(field(&oracle, "http"), "cases"));
    assert_eq!(cases.len(), 48);
    let executor = HttpHookExecutor::new();
    for case in cases {
        let name = text(field(case, "name"));
        let target = substitute_text(
            &text(field(case, "target")),
            &[("<url>", &base), ("<refused>", &refused)],
        );
        let signal = match case.get("abortAtMs") {
            None => None,
            Some(ms) => {
                let signal = Cancellation::new();
                let ms = number(ms);
                if ms == 0.0 {
                    signal.abort();
                } else {
                    let signal = signal.clone();
                    tokio::spawn(async move {
                        sleep(Duration::from_millis(ms as u64)).await;
                        signal.abort();
                    });
                }
                Some(signal)
            }
        };
        let before = recorded.lock().unwrap().len();
        let mut http_env = JsObject::new();
        http_env.insert("TOKEN".into(), "t".into());
        http_env.insert("SECRET".into(), "s".into());
        let result = executor
            .execute(HttpHookOptions {
                url: target.into(),
                headers: case.get("headers").map(object),
                allowed_env_vars: case
                    .get("allowedEnvVars")
                    .map(|values| array(values).iter().map(string).collect()),
                hook_input: object(field(case, "hookInput")),
                env: Some(http_env),
                signal,
            })
            .await;
        assert_eq!(
            json(&result.to_value()),
            text(field(case, "expected")),
            "http case {name}"
        );
        let expected_request = field(case, "request");
        if !expected_request.is_undefined() && !matches!(expected_request, JsValue::Null) {
            let requests = recorded.lock().unwrap();
            // Redirect hops: every request after the first, in order, with
            // the method/body the follow policy chose (POST→GET drops body).
            let followed = case.get("followed").map(array).unwrap_or_default();
            assert_eq!(
                requests.len(),
                before + 1 + followed.len(),
                "http request count {name}"
            );
            assert_eq!(
                json(&requests[before]),
                json(expected_request),
                "http request {name}"
            );
            for (hop, expected) in followed.iter().enumerate() {
                assert_eq!(
                    json(&requests[before + 1 + hop]),
                    json(expected),
                    "http redirect hop {hop} {name}"
                );
            }
            if name == "headers-env" {
                // Negative control: only allow-listed variables interpolate.
                let headers = field(&requests[before], "headers").clone();
                assert_eq!(text(field(&headers, "x-a")), "t");
                assert_eq!(text(field(&headers, "x-b")), "t");
                assert_eq!(text(field(&headers, "x-c")), "");
                assert_eq!(text(field(&headers, "x-d")), "pret-post");
            }
        } else {
            assert_eq!(
                recorded.lock().unwrap().len(),
                before,
                "http case {name} must not reach the server"
            );
        }
    }
    server.abort();
    let _ = server.await;
}

/// TS `fetch` gives up after the dispatcher's headers timeout and reports
/// `fetch failed`. The TS CLI installs `undici.Agent({ headersTimeout:
/// 600_000, bodyTimeout: 600_000 })` (too slow for a test), so inject a short
/// one against a server that accepts but never responds.
#[tokio::test]
async fn http_headers_timeout_projects_fetch_failed() {
    assert_eq!(
        HttpHookExecutor::new().timeout(),
        Duration::from_secs(600),
        "default must match the TS CLI dispatcher (cli/proxy.ts createLongTimeoutOptions)"
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    let started = Instant::now();
    let result = timeout(
        Duration::from_secs(5),
        HttpHookExecutor::with_timeout(Duration::from_millis(100)).execute(HttpHookOptions {
            url: format!("http://{address}/").into(),
            headers: None,
            allowed_env_vars: None,
            hook_input: JsObject::new(),
            env: None,
            signal: None,
        }),
    )
    .await
    .expect("a headers timeout must resolve the request");
    assert_eq!(result.outcome, HookOutcome::NonBlockingError);
    assert_eq!(
        String::from_utf16_lossy(result.stderr.units()),
        "fetch failed"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    server.abort();
    let _ = server.await;
}

/// Raw loopback server for timing/size behavior the TS oracle cannot pin
/// (semantics measured on the baseline's undici with small timeouts:
/// `_tmp/20261019-a67-pr4/probe-r3/timeouts-3s.mjs`, `body-cap.mjs`).
/// Paths: `/hop/<delay ms>/<hops left>` answers after `delay` with a 302 to
/// the next hop (200 `{}` at 0); `/drip/<gap ms>/<chunks>` sends chunked
/// `x` frames `gap` apart; `/stall` sends headers and one frame, then idles;
/// `/size/<n>` sends `n` bytes.
async fn timing_server() -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let Ok(n) = stream.read(&mut buffer).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..n]);
                    let text = String::from_utf8_lossy(&request);
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text[..end]
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())?
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&request).into_owned();
                let path = text.split(' ').nth(1).unwrap_or("/").to_owned();
                let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
                let number = |i: usize| {
                    parts
                        .get(i)
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(0)
                };
                match parts[0] {
                    "hop" => {
                        sleep(Duration::from_millis(number(1))).await;
                        let response = if number(2) == 0 {
                            "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}"
                                .to_owned()
                        } else {
                            format!(
                                "HTTP/1.1 302 Found\r\nlocation: /hop/{}/{}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                                number(1),
                                number(2) - 1
                            )
                        };
                        let _ = stream.write_all(response.as_bytes()).await;
                    }
                    "drip" | "stall" => {
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n1\r\nx\r\n")
                            .await;
                        if parts[0] == "stall" {
                            sleep(Duration::from_secs(30)).await;
                            return;
                        }
                        for _ in 1..number(2) {
                            sleep(Duration::from_millis(number(1))).await;
                            let _ = stream.write_all(b"1\r\nx\r\n").await;
                        }
                        let _ = stream.write_all(b"0\r\n\r\n").await;
                    }
                    "size" => {
                        let n = number(1) as usize;
                        let head = format!(
                            "HTTP/1.1 200 OK\r\ncontent-length: {n}\r\nconnection: close\r\n\r\n"
                        );
                        let _ = stream.write_all(head.as_bytes()).await;
                        let _ = stream.write_all(&vec![b'a'; n]).await;
                    }
                    _ => {}
                }
                let _ = stream.shutdown().await;
            });
        }
    });
    (base, task)
}
async fn run_http(executor: &HttpHookExecutor, url: String) -> (HookOutcome, String, String) {
    let result = executor
        .execute(HttpHookOptions {
            url: url.into(),
            headers: None,
            allowed_env_vars: None,
            hook_input: JsObject::new(),
            env: None,
            signal: None,
        })
        .await;
    (
        result.outcome,
        String::from_utf16_lossy(result.stdout.units()),
        String::from_utf16_lossy(result.stderr.units()),
    )
}

/// undici `headersTimeout` is armed per dispatch (each redirect hop), and
/// `bodyTimeout` is the idle gap between body frames, not a deadline for the
/// whole fetch. Baseline probe with 3 s timeouts: 3 hops x 2 s succeed
/// (6 s total), one 5 s hop fails `fetch failed`, 4 frames 2 s apart succeed
/// (8 s total), a 5 s gap / a stalled body fail `terminated`.
#[tokio::test]
async fn http_timeouts_are_per_hop_headers_and_idle_body() {
    let (base, server) = timing_server().await;
    let executor = HttpHookExecutor::with_timeout(Duration::from_millis(400));
    let hops = run_http(&executor, format!("{base}/hop/250/3")).await;
    assert_eq!(
        hops,
        (HookOutcome::Success, "{}".into(), String::new()),
        "4 dispatches x 250 ms (1 s total) each fit the 400 ms headers timer"
    );
    let slow_hop = run_http(&executor, format!("{base}/hop/700/0")).await;
    assert_eq!(slow_hop.2, "fetch failed");
    let drip = run_http(&executor, format!("{base}/drip/250/4")).await;
    assert_eq!(
        drip,
        (HookOutcome::Success, "xxxx".into(), String::new()),
        "frames 250 ms apart (750 ms total) never idle past 400 ms"
    );
    let gap = run_http(&executor, format!("{base}/drip/700/2")).await;
    assert_eq!(
        (gap.0, gap.2.as_str()),
        (HookOutcome::NonBlockingError, "terminated")
    );
    let stall = run_http(&executor, format!("{base}/stall")).await;
    assert_eq!(
        (stall.0, stall.2.as_str()),
        (HookOutcome::NonBlockingError, "terminated")
    );
    server.abort();
    let _ = server.await;
}

/// `response.text()` cannot build a string past `MAX_STRING_LENGTH` bytes
/// (Node: `Cannot create a string longer than 0x1fffffe8 characters` at
/// 0x1fffffe8 + 1 bytes, success at exactly 0x1fffffe8; probe-r3/body-cap.mjs).
/// The executor counts bytes per frame and stops at the limit instead of
/// buffering an unbounded body; a small injected limit exercises that path.
#[tokio::test]
async fn http_body_over_string_limit_is_a_range_error() {
    let (base, server) = timing_server().await;
    let executor = HttpHookExecutor::new().with_body_limit(16);
    let fits = run_http(&executor, format!("{base}/size/16")).await;
    assert_eq!(fits, (HookOutcome::Success, "a".repeat(16), String::new()));
    let over = run_http(&executor, format!("{base}/size/17")).await;
    assert_eq!(
        (over.0, over.1.as_str(), over.2.as_str()),
        (
            HookOutcome::NonBlockingError,
            "",
            "Cannot create a string longer than 0x1fffffe8 characters"
        )
    );
    server.abort();
    let _ = server.await;
}

// ---------------------------------------------------------------- callback

struct FnHandler<F>(F);
impl<F> CallbackHookHandler for FnHandler<F>
where
    F: Fn(CallbackInput) -> CallbackFuture + Send + Sync,
{
    fn call(&self, input: CallbackInput) -> CallbackFuture {
        (self.0)(input)
    }
}
fn handler<F>(f: F) -> Arc<dyn CallbackHookHandler>
where
    F: Fn(CallbackInput) -> CallbackFuture + Send + Sync + 'static,
{
    Arc::new(FnHandler(f))
}
fn text_handler(value: &str) -> Arc<dyn CallbackHookHandler> {
    let value = value.to_owned();
    handler(move |_| {
        let value = value.clone();
        Box::pin(async move { Ok(CallbackResult::Text(value.into())) })
    })
}
fn throw_handler(message: &str) -> Arc<dyn CallbackHookHandler> {
    let message = message.to_owned();
    handler(move |_| {
        let message = message.clone();
        Box::pin(async move { Err(message.into()) })
    })
}

struct Cases<'a> {
    cases: &'a [JsValue],
    index: usize,
}
impl<'a> Cases<'a> {
    fn new(cases: &'a [JsValue]) -> Self {
        Self { cases, index: 0 }
    }
    fn next(&mut self, name: &str) -> &'a JsValue {
        let case = &self.cases[self.index];
        self.index += 1;
        assert_eq!(text(field(case, "name")), name, "callback case order");
        case
    }
    fn finish(&self) {
        assert_eq!(self.index, self.cases.len(), "unconsumed callback cases");
    }
}
fn assert_case(case: &JsValue, actual: JsValue) {
    if let Some(error) = case.get("error") {
        assert_eq!(text(error), text(&actual), "callback error");
        return;
    }
    let expected = field(case, "value");
    if expected.get("undefined").is_some_and(bool_value) {
        assert!(actual.is_undefined());
        return;
    }
    assert_eq!(json(&actual), json(expected));
}

#[tokio::test]
async fn callback_executor_matches_real_ts() {
    let oracle = oracle();
    let cases = array(field(field(&oracle, "callback"), "cases"));
    assert_eq!(cases.len(), 21);
    let mut cursor = Cases::new(cases);
    let ex = pilotdeck_gateway::runtime::hooks::exec::CallbackHookExecutor::new();
    let input = || CallbackInput {
        hook_input: JsObject::new(),
        signal: None,
    };
    let name = JsString::from("cb");

    let r1 = ex
        .register(
            &"  cb  ".into(),
            text_handler(r#"{"continue":false,"reason":"cb"}"#),
        )
        .unwrap();
    let mut registered = JsObject::new();
    registered.insert("name".into(), JsValue::String(r1.name().clone()));
    registered.insert("generation".into(), JsValue::Number(r1.generation() as f64));
    registered.insert("active".into(), JsValue::Bool(r1.active()));
    assert_case(cursor.next("register-trimmed"), JsValue::Object(registered));

    let result = ex.execute(&name, input()).await;
    assert_case(cursor.next("execute-text"), result.to_value());

    let result = ex.execute(&"nope".into(), input()).await;
    assert_case(cursor.next("execute-missing"), result.to_value());

    let r2 = ex
        .register(
            &name,
            handler(move |_| {
                Box::pin(async {
                    Ok(CallbackResult::Output(HookOutput::Sync(Box::new(
                        HookSyncOutput {
                            decision: Some(HookDecision::Block),
                            reason: Some("r".into()),
                            ..Default::default()
                        },
                    ))))
                })
            }),
        )
        .unwrap();
    let mut replaced = JsObject::new();
    replaced.insert("oldActive".into(), JsValue::Bool(r1.active()));
    replaced.insert("generation".into(), JsValue::Number(r2.generation() as f64));
    replaced.insert("active".into(), JsValue::Bool(r2.active()));
    assert_case(cursor.next("replace"), JsValue::Object(replaced));

    let result = ex.execute(&name, input()).await;
    assert_case(cursor.next("execute-output"), result.to_value());

    let rv = ex
        .register(
            &"void".into(),
            handler(|_| Box::pin(async { Ok(CallbackResult::Void) })),
        )
        .unwrap();
    let result = ex.execute(&"void".into(), input()).await;
    assert_case(cursor.next("execute-void"), result.to_value());

    ex.register(&"thrower".into(), throw_handler("boom"))
        .unwrap();
    let result = ex.execute(&"thrower".into(), input()).await;
    assert_case(cursor.next("execute-throw"), result.to_value());

    ex.register(&"throwstr".into(), throw_handler("plain"))
        .unwrap();
    let result = ex.execute(&"throwstr".into(), input()).await;
    assert_case(cursor.next("execute-throw-string"), result.to_value());

    ex.register(&"promise".into(), text_handler(r#"{"decision":"approve"}"#))
        .unwrap();
    let result = ex.execute(&"promise".into(), input()).await;
    assert_case(cursor.next("execute-promise"), result.to_value());

    assert_case(
        cursor.next("unregister"),
        JsValue::Bool(ex.unregister(&name).unwrap()),
    );
    assert_case(
        cursor.next("unregister-again"),
        JsValue::Bool(ex.unregister(&name).unwrap()),
    );
    let error = ex
        .register(&"".into(), text_handler(""))
        .err()
        .expect("empty name must fail");
    assert_case(cursor.next("empty-name-error"), JsValue::String(error));

    assert_case(
        cursor.next("state-active"),
        JsValue::String(ex.state().as_str().into()),
    );
    ex.dispose().await;
    assert_case(
        cursor.next("state-disposed"),
        JsValue::String(ex.state().as_str().into()),
    );
    let result = ex.execute(&"thrower".into(), input()).await;
    assert_case(cursor.next("execute-after-dispose"), result.to_value());
    let error = ex
        .register(&"x".into(), text_handler(""))
        .err()
        .expect("register after dispose must fail");
    assert_case(
        cursor.next("register-after-dispose-error"),
        JsValue::String(error),
    );
    let mut old = JsObject::new();
    old.insert("r2".into(), JsValue::Bool(r2.active()));
    assert_case(
        cursor.next("old-registration-after-dispose"),
        JsValue::Object(old),
    );
    let mut void = JsObject::new();
    void.insert("rv".into(), JsValue::Bool(rv.active()));
    assert_case(
        cursor.next("void-registration-after-dispose"),
        JsValue::Object(void),
    );

    // Dispose waits for one in-flight callback.
    let ex = pilotdeck_gateway::runtime::hooks::exec::CallbackHookExecutor::new();
    ex.register(
        &"slow".into(),
        handler(|_| {
            Box::pin(async {
                sleep(Duration::from_millis(150)).await;
                Ok(CallbackResult::Text(r#"{"decision":"block"}"#.into()))
            })
        }),
    )
    .unwrap();
    let running = {
        let ex = ex.clone();
        tokio::spawn(async move { ex.execute(&"slow".into(), input()).await })
    };
    tokio::task::yield_now().await;
    let disposing = {
        let ex = ex.clone();
        tokio::spawn(async move { ex.dispose().await })
    };
    tokio::task::yield_now().await;
    assert_case(
        cursor.next("dispose-drain-state"),
        JsValue::String(ex.state().as_str().into()),
    );
    disposing.await.unwrap();
    assert_case(
        cursor.next("dispose-drain-state-after"),
        JsValue::String(ex.state().as_str().into()),
    );
    let result = running.await.unwrap();
    assert_case(cursor.next("dispose-drain-result"), result.to_value());
    cursor.finish();
}

// ---------------------------------------------------------------- HookRuntime

struct RecordingHandler(Arc<Mutex<Vec<JsObject>>>);
impl HookExecutionEventHandler for RecordingHandler {
    fn handle(&self, event: &JsObject) {
        self.0.lock().unwrap().push(event.clone());
    }
}
struct PanicHandler;
impl HookExecutionEventHandler for PanicHandler {
    fn handle(&self, _: &JsObject) {
        panic!("observer");
    }
}

fn fixed_options() -> HookRuntimeOptions {
    HookRuntimeOptions {
        clock: Arc::new(|| JsString::from(FIXED_START)),
        shell: None,
        on_async_registered: None,
    }
}

fn parsed_settings(value: &JsValue) -> HooksSettings {
    let parsed = parse_hooks_config(value);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    parsed.settings
}

fn run_input(case: &JsValue, root: &str) -> HookRuntimeRunInput {
    let pairs = [("<dir>", root)];
    HookRuntimeRunInput {
        event: string(field(case, "event")),
        hook_input: object(field(case, "hookInput")),
        match_query: case.get("matchQuery").map(string),
        cwd: substitute_text(&text(field(case, "cwd")), &pairs).into(),
        env: Some(object(field(case, "env"))),
        signal: None,
    }
}

async fn replay_run(
    runtime: &HookRuntime,
    case: &JsValue,
    root: &str,
    throwing: bool,
) -> (String, String) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorder = runtime
        .subscribe_execution_events(Arc::new(RecordingHandler(events.clone())))
        .unwrap();
    let thrower = throwing.then(|| {
        runtime
            .subscribe_execution_events(Arc::new(PanicHandler))
            .unwrap()
    });
    let result = runtime.run(run_input(case, root)).await.unwrap();
    recorder.dispose();
    if let Some(thrower) = thrower {
        thrower.dispose();
    }
    let recorded = JsValue::Array(
        events
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .map(JsValue::Object)
            .collect(),
    );
    (json(&result_value(&result)), json(&recorded))
}
fn result_value(result: &pilotdeck_gateway::runtime::hooks::HookRuntimeRunResult) -> JsValue {
    let mut object = JsObject::new();
    object.insert(
        "effects".into(),
        JsValue::Array(
            result
                .effects
                .iter()
                .map(|effect| effect.to_value())
                .collect(),
        ),
    );
    object.insert(
        "events".into(),
        JsValue::Array(result.events.iter().cloned().map(JsValue::Object).collect()),
    );
    object.insert(
        "blockingErrors".into(),
        JsValue::Array(
            result
                .blocking_errors
                .iter()
                .map(|error| error.to_value())
                .collect(),
        ),
    );
    object.insert(
        "nonBlockingErrors".into(),
        JsValue::Array(
            result
                .non_blocking_errors
                .iter()
                .map(|error| error.to_value())
                .collect(),
        ),
    );
    object.insert(
        "pendingAsyncHooks".into(),
        JsValue::Array(
            result
                .pending_async_hooks
                .iter()
                .map(|descriptor| descriptor.to_value())
                .collect(),
        ),
    );
    JsValue::Object(object)
}

#[tokio::test]
async fn hook_runtime_matches_real_ts() {
    let oracle = oracle();
    let runtime_section = field(&oracle, "runtime");
    let scratch = Scratch::new("hooks-runtime-runtime");
    let root = scratch_root(&scratch);
    write_scripts(&oracle, &root);
    let settings = parsed_settings(&substitute(
        field(runtime_section, "settings"),
        &[("<dir>", &root)],
    ));
    let runtime = HookRuntime::with_options(settings, fixed_options());
    let runs = array(field(runtime_section, "runs"));
    assert_eq!(runs.len(), 5);
    for case in runs {
        let name = text(field(case, "name"));
        let (result, events) = replay_run(&runtime, case, &root, name == "observer-error").await;
        assert_eq!(
            mask_paths(&result, &root),
            text(field(case, "expected")),
            "runtime run {name}"
        );
        assert_eq!(
            mask_paths(&events, &root),
            text(field(case, "expectedEvents")),
            "runtime events {name}"
        );
    }
    runtime.dispose().await;
    let dispose = field(runtime_section, "dispose");
    assert_eq!(runtime.lifecycle_state(), text(field(dispose, "state")));
    let error = runtime
        .subscribe_execution_events(Arc::new(RecordingHandler(Arc::new(Mutex::new(Vec::new())))))
        .err()
        .expect("subscribe after dispose must fail");
    assert_eq!(
        module_message(error),
        text(field(dispose, "subscribeError"))
    );
    let error = runtime
        .run(run_input(&oracle_runs_first(&oracle), &root))
        .await
        .unwrap_err();
    assert_eq!(module_message(error), text(field(dispose, "runError")));

    // pluginRoot replaces the command cwd.
    let scratch = Scratch::new("hooks-runtime-plugin-root");
    let root = scratch_root(&scratch);
    write_scripts(&oracle, &root);
    let extra = field(runtime_section, "pluginRoot");
    let runtime = HookRuntime::with_options(
        parsed_settings(&substitute(field(extra, "settings"), &[("<dir>", &root)])),
        fixed_options(),
    );
    let (result, events) = replay_run(&runtime, field(extra, "run"), &root, false).await;
    assert_eq!(
        mask_paths(&result, &root),
        text(field(field(extra, "run"), "expected"))
    );
    assert_eq!(
        mask_paths(&events, &root),
        text(field(field(extra, "run"), "expectedEvents"))
    );
    runtime.dispose().await;

    // SessionEnd uses the fixed 1500 ms budget.
    let scratch = Scratch::new("hooks-runtime-session-end");
    let root = scratch_root(&scratch);
    write_scripts(&oracle, &root);
    let extra = field(runtime_section, "sessionEnd");
    let runtime = HookRuntime::with_options(
        parsed_settings(&substitute(field(extra, "settings"), &[("<dir>", &root)])),
        fixed_options(),
    );
    let started = Instant::now();
    let (result, events) = replay_run(&runtime, field(extra, "run"), &root, false).await;
    assert_eq!(
        mask_paths(&result, &root),
        text(field(field(extra, "run"), "expected"))
    );
    assert_eq!(
        mask_paths(&events, &root),
        text(field(field(extra, "run"), "expectedEvents"))
    );
    assert!(bool_value(field(extra, "elapsedAtLeast1500")));
    assert!(started.elapsed() >= Duration::from_millis(1500));
    runtime.dispose().await;

    // Pending async hooks: describe/complete/collect/remove/cancel.
    let scratch = Scratch::new("hooks-runtime-async");
    let root = scratch_root(&scratch);
    write_scripts(&oracle, &root);
    let extra = field(runtime_section, "async");
    let runtime = HookRuntime::with_options(
        parsed_settings(&substitute(field(extra, "settings"), &[("<dir>", &root)])),
        fixed_options(),
    );
    let input = run_input(&oracle_runs_first(&oracle), &root);
    let result = runtime.run(input).await.unwrap();
    assert_eq!(json(&result_value(&result)), text(field(extra, "run")));
    assert_eq!(
        json(&JsValue::Array(
            result.events.iter().cloned().map(JsValue::Object).collect()
        )),
        text(field(extra, "events"))
    );
    assert_eq!(
        json(&JsValue::Array(
            result
                .pending_async_hooks
                .iter()
                .map(|descriptor| descriptor.to_value())
                .collect()
        )),
        text(field(extra, "pending"))
    );
    let pending = &result.pending_async_hooks[0];
    assert_eq!(
        json(&JsValue::Array(
            runtime
                .collect_async_responses()
                .iter()
                .map(|response| response.to_value())
                .collect()
        )),
        text(field(extra, "collectBefore"))
    );
    assert_eq!(
        runtime.complete_async_hook(
            &pending.id,
            AsyncHookCompletion {
                output: Some(HookOutput::Async(
                    pilotdeck_gateway::hooks::protocol::HookAsyncOutput::default()
                )),
                ..Default::default()
            }
        ),
        bool_value(field(extra, "completeInvalid"))
    );
    assert_eq!(
        runtime.complete_async_hook(
            &pending.id,
            AsyncHookCompletion {
                stdout: Some(r#"{"continue":false,"reason":"r"}"#.into()),
                ..Default::default()
            }
        ),
        bool_value(field(extra, "completeValid"))
    );
    assert_eq!(
        json(&JsValue::Array(
            runtime
                .collect_async_responses()
                .iter()
                .map(|response| response.to_value())
                .collect()
        )),
        text(field(extra, "collect"))
    );
    assert_eq!(
        json(&JsValue::Array(
            runtime
                .collect_async_responses()
                .iter()
                .map(|response| response.to_value())
                .collect()
        )),
        text(field(extra, "collectAgain"))
    );
    runtime.remove_delivered_async_responses();
    assert_eq!(
        json(&JsValue::Array(
            runtime
                .collect_async_responses()
                .iter()
                .map(|response| response.to_value())
                .collect()
        )),
        text(field(extra, "afterRemove"))
    );
    let second = runtime
        .run(run_input(&oracle_runs_first(&oracle), &root))
        .await
        .unwrap();
    assert_eq!(
        json(&JsValue::Array(
            second
                .pending_async_hooks
                .iter()
                .map(|descriptor| descriptor.to_value())
                .collect()
        )),
        text(field(extra, "secondPending"))
    );
    let id = &second.pending_async_hooks[0].id;
    assert_eq!(
        runtime.cancel_async_hook(id),
        bool_value(field(extra, "cancel"))
    );
    assert_eq!(
        runtime.cancel_async_hook(id),
        bool_value(field(extra, "cancelAgain"))
    );
    assert_eq!(
        json(&JsValue::Array(
            runtime
                .collect_async_responses()
                .iter()
                .map(|response| response.to_value())
                .collect()
        )),
        text(field(extra, "afterCancel"))
    );
    runtime.dispose().await;
    assert_eq!(
        runtime.complete_async_hook(
            &pending.id,
            AsyncHookCompletion {
                stdout: Some("{}".into()),
                ..Default::default()
            }
        ),
        bool_value(field(extra, "completeAfterDispose"))
    );
    assert_eq!(
        json(&JsValue::Array(
            runtime
                .collect_async_responses()
                .iter()
                .map(|response| response.to_value())
                .collect()
        )),
        text(field(extra, "collectAfterDispose"))
    );

    // HookRuntime dispatches callback hooks through the injected executor.
    let scratch = Scratch::new("hooks-runtime-callback-hook");
    let root = scratch_root(&scratch);
    write_scripts(&oracle, &root);
    let extra = field(runtime_section, "callbackHook");
    // The persistent parser intentionally drops callback hooks; this scenario
    // exists to exercise HookRuntime's runtime-only callback route.
    let mut settings =
        parse_hooks_config(&substitute(field(extra, "settings"), &[("<dir>", &root)])).settings;
    settings[0].1[0].hooks.insert(
        0,
        HookCommand::Callback {
            name: "cb".into(),
            condition: None,
            status_message: None,
        },
    );
    let runtime = HookRuntime::with_options(settings, fixed_options());
    runtime
        .callback_executor()
        .register(
            &"cb".into(),
            handler(|_| {
                Box::pin(async {
                    // The TS handler returned a `PilotDeckHookOutput`-shaped
                    // object: `systemMessage` is read, while the unrelated
                    // `hookSpecificOutput` key is not `specific` and has no
                    // effect.
                    Ok(CallbackResult::Output(HookOutput::Sync(Box::new(
                        HookSyncOutput {
                            system_message: Some("sys".into()),
                            ..Default::default()
                        },
                    ))))
                })
            }),
        )
        .unwrap();
    let case = field(extra, "run");
    let (result, events) = replay_run(&runtime, case, &root, false).await;
    assert_eq!(mask_paths(&result, &root), text(field(case, "expected")));
    assert_eq!(
        mask_paths(&events, &root),
        text(field(case, "expectedEvents"))
    );
    runtime.dispose().await;

    // dispose drains an in-flight run before reporting disposed.
    let scratch = Scratch::new("hooks-runtime-drain");
    let root = scratch_root(&scratch);
    write_scripts(&oracle, &root);
    let extra = field(runtime_section, "disposeDrain");
    let runtime = HookRuntime::with_options(
        parsed_settings(&substitute(field(extra, "settings"), &[("<dir>", &root)])),
        fixed_options(),
    );
    let input = run_input(&oracle_runs_first(&oracle), &root);
    let running = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.run(input).await })
    };
    tokio::task::yield_now().await;
    let disposing = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.dispose().await })
    };
    tokio::task::yield_now().await;
    assert_eq!(
        runtime.lifecycle_state(),
        text(field(extra, "immediateState"))
    );
    disposing.await.unwrap();
    assert_eq!(runtime.lifecycle_state(), text(field(extra, "afterState")));
    let result = running.await.unwrap().unwrap();
    assert_eq!(json(&result_value(&result)), text(field(extra, "result")));
}

fn oracle_runs_first(oracle: &JsValue) -> JsValue {
    element(field(field(oracle, "runtime"), "runs"), 0).clone()
}

// ---------------------------------------------------------------- lifecycle

#[tokio::test]
async fn lifecycle_dispatch_matches_real_ts() {
    let oracle = oracle();
    let section = field(&oracle, "lifecycle");
    let scratch = Scratch::new("hooks-runtime-lifecycle");
    let root = scratch_root(&scratch);
    write_scripts(&oracle, &root);
    let base_input = object(&substitute(
        field(section, "baseInput"),
        &[("<dir>", &root)],
    ));
    let settings = parsed_settings(&substitute(field(section, "settings"), &[("<dir>", &root)]));
    let runtime = LifecycleRuntime::new(HookRuntime::with_options(settings, fixed_options()));
    let result = runtime
        .dispatch(LifecycleDispatchInput {
            event: "Stop".into(),
            base_input: base_input.clone(),
            payload: Some(object(&parse_json(r#"{"extra":"v"}"#).unwrap())),
            match_query: Some("Stop".into()),
            cwd: root.clone().into(),
            env: Some(object(
                &parse_json(r#"{"PATH":"/usr/bin:/bin","HOME":"/tmp"}"#).unwrap(),
            )),
            signal: None,
        })
        .await
        .unwrap();
    let value = result.to_value();
    assert_eq!(
        mask_paths(&json(&value), &root),
        text(field(section, "dispatch"))
    );
    assert_eq!(
        mask_paths(&json(field(&value, "messages")), &root),
        text(field(section, "dispatchMessages"))
    );
    // The session runtime is reused across turns: a second dispatch still runs
    // hooks and keeps the first turn's pending async registration.
    let second = runtime
        .dispatch(LifecycleDispatchInput {
            event: "Stop".into(),
            base_input: base_input.clone(),
            payload: Some(object(&parse_json(r#"{"extra":"v"}"#).unwrap())),
            match_query: Some("Stop".into()),
            cwd: root.clone().into(),
            env: Some(object(
                &parse_json(r#"{"PATH":"/usr/bin:/bin","HOME":"/tmp"}"#).unwrap(),
            )),
            signal: None,
        })
        .await
        .unwrap();
    let across = field(section, "acrossTurns");
    assert_eq!(
        mask_paths(&json(&value), &root),
        text(field(across, "first"))
    );
    assert_eq!(
        mask_paths(
            &json(&JsValue::Array(
                result
                    .pending_async_hooks
                    .iter()
                    .map(|descriptor| descriptor.to_value())
                    .collect()
            )),
            &root
        ),
        text(field(across, "firstPending"))
    );
    assert_eq!(
        mask_paths(&json(&second.to_value()), &root),
        text(field(across, "second"))
    );
    assert_eq!(
        mask_paths(
            &json(&JsValue::Array(
                second
                    .pending_async_hooks
                    .iter()
                    .map(|descriptor| descriptor.to_value())
                    .collect()
            )),
            &root
        ),
        text(field(across, "secondPending"))
    );
    runtime.dispose().await;
    let error = runtime
        .dispatch(LifecycleDispatchInput {
            event: "Stop".into(),
            base_input,
            payload: Some(JsObject::new()),
            match_query: Some("Stop".into()),
            cwd: root.clone().into(),
            env: None,
            signal: None,
        })
        .await
        .unwrap_err();
    assert_eq!(module_message(error), text(field(section, "disposedError")));

    let null = LifecycleRuntime::null();
    let empty = null
        .dispatch(LifecycleDispatchInput {
            event: "Stop".into(),
            base_input: JsObject::new(),
            payload: None,
            match_query: Some("Stop".into()),
            cwd: root.clone().into(),
            env: None,
            signal: None,
        })
        .await
        .unwrap();
    assert_eq!(
        json(&empty.to_value()),
        text(field(section, "nullDispatch"))
    );
    null.dispose().await;

    // buildTurnEnvironment, including path resolution and segment sanitizing.
    // The oracle masks the generator's process cwd as `<cwd>`; a relative
    // `cwd` argument resolves against the process cwd in both implementations.
    let cwd = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let env_cases = array(field(section, "envCases"));
    assert_eq!(env_cases.len(), 12);
    for case in env_cases {
        let name = text(field(case, "name"));
        let base_env = case.get("base").map(object);
        let cwd_value = substitute_text(&text(field(case, "cwd")), &[("<dir>", &root)]);
        let actual = host_lifecycle::build_turn_environment(
            base_env.as_ref(),
            &cwd_value.clone().into(),
            &string(field(case, "sessionId")),
            &string(field(case, "turnId")),
        );
        let expected = substitute_text(
            &text(field(case, "expected")),
            &[("<dir>", &root), ("<cwd>", &cwd)],
        );
        assert_eq!(json(&JsValue::Object(actual)), expected, "env case {name}");
    }

    // HostLifecycleRuntime error projection and result validation.
    let host_cases = array(field(section, "hostClientCases"));
    assert_eq!(host_cases.len(), 11);
    for case in host_cases {
        let name = text(field(case, "name"));
        if name == "request" || name == "request-without-idempotency-key" {
            continue;
        }
        let response = field(case, "response");
        if bool_value(field(response, "ok")) {
            let value = response
                .get("payload")
                .and_then(JsValue::as_object)
                .and_then(|payload| payload.get("result"))
                .unwrap_or(&UNDEFINED);
            match validate_lifecycle_dispatch_result(value) {
                Ok(()) => {
                    assert_eq!(json(value), text(field(case, "resolved")), "{name}");
                }
                Err(message) => {
                    assert_eq!(
                        String::from_utf16_lossy(message.units()),
                        text(field(case, "error")),
                        "{name}"
                    );
                }
            }
        } else {
            let (message, code) = lifecycle_module_failure(response);
            assert_eq!(
                String::from_utf16_lossy(message.units()),
                text(field(case, "error")),
                "{name}"
            );
            match (code, case.get("code")) {
                (None, Some(JsValue::Null)) | (None, None) => {}
                (Some(code), Some(expected)) => assert_eq!(code, string(expected), "{name}"),
                (code, expected) => panic!("{name}: code {code:?} vs {expected:?}"),
            }
        }
    }

    // The module adapter produces exactly the dispatch result the sidecar
    // client validates.
    let handler_settings =
        parsed_settings(&substitute(field(section, "settings"), &[("<dir>", &root)]));
    let handler_runtime =
        LifecycleRuntime::new(HookRuntime::with_options(handler_settings, fixed_options()));
    let handler = host_lifecycle::Handler::new(
        handler_runtime.clone(),
        host_lifecycle::Binding {
            session_id: "session-1".into(),
            turn_id: "turn-1".into(),
            cwd: root.clone().into(),
            permission_mode: Some("default".into()),
            base_env: Some(object(
                &parse_json(r#"{"PATH":"/usr/bin:/bin","HOME":"/tmp"}"#).unwrap(),
            )),
            signal: None,
        },
    );
    let mut payload = JsObject::new();
    payload.insert("extra".into(), "v".into());
    let response = handler
        .call(module_call(
            "dispatch",
            "Stop",
            Some(JsValue::Object(payload)),
        ))
        .await
        .unwrap();
    assert_eq!(
        mask_paths(&json(field(&JsValue::Object(response), "result")), &root),
        text(field(section, "dispatch"))
    );
    // TS dispatcher dispose never disposes the session lifecycle runtime, so a
    // second turn still executes hooks.
    handler.dispose().await.unwrap();
    let mut payload = JsObject::new();
    payload.insert("extra".into(), "v".into());
    let response = handler
        .call(module_call(
            "dispatch",
            "Stop",
            Some(JsValue::Object(payload)),
        ))
        .await
        .expect("handler dispose must not dispose the session runtime");
    assert_eq!(
        mask_paths(&json(field(&JsValue::Object(response), "result")), &root),
        text(field(field(section, "acrossTurns"), "second"))
    );
    handler_runtime.dispose().await;
    let mut payload = JsObject::new();
    payload.insert("extra".into(), "v".into());
    let error = handler
        .call(module_call(
            "dispatch",
            "Stop",
            Some(JsValue::Object(payload)),
        ))
        .await
        .unwrap_err();
    assert_eq!(
        module_message(error),
        "Cannot run hooks; hook runtime is disposed."
    );

    // Dispatcher-level input validation and the missing-capability default.
    let empty_handler = host_lifecycle::Handler::default();
    let error = empty_handler
        .call(module_call("dispatch", "Stop", None))
        .await
        .unwrap_err();
    assert_eq!(
        module_message(error),
        "Unsupported sidecar host module: lifecycle"
    );
    let handler = host_lifecycle::Handler::null(host_lifecycle::Binding {
        session_id: "s".into(),
        turn_id: "t".into(),
        cwd: root.clone().into(),
        permission_mode: None,
        base_env: None,
        signal: None,
    });
    let error = handler
        .call(module_call("other", "Stop", None))
        .await
        .unwrap_err();
    assert_eq!(
        module_message(error),
        "Unsupported sidecar lifecycle operation."
    );
    let error = handler
        .call(module_call("dispatch", "Nope", None))
        .await
        .unwrap_err();
    assert_eq!(
        module_message(error),
        "Unsupported sidecar lifecycle event: Nope"
    );
    let mut call = JsObject::new();
    call.insert("module".into(), "lifecycle".into());
    let mut payload = JsObject::new();
    payload.insert("operation".into(), "dispatch".into());
    payload.insert("event".into(), "Stop".into());
    payload.insert("payload".into(), JsValue::Number(5.0));
    call.insert("payload".into(), JsValue::Object(payload));
    let error = handler.call(JsValue::Object(call)).await.unwrap_err();
    assert_eq!(
        module_message(error),
        "Lifecycle payload must be an object."
    );
    handler.dispose().await.unwrap();
}

/// P1: `Handler::dispose` is the per-turn dispatcher teardown and must not
/// dispose the session-level runtime. A second turn still runs command and
/// callback hooks, and a pending async registration from turn 1 survives both
/// the dispose and the next turn.
#[tokio::test]
async fn lifecycle_handler_dispose_keeps_session_state_across_turns() {
    let calls = Arc::new(Mutex::new(0usize));
    let settings: HooksSettings = vec![(
        "Stop".into(),
        vec![HookMatcher {
            hooks: vec![
                HookCommand::Callback {
                    name: "cb".into(),
                    condition: None,
                    status_message: None,
                },
                HookCommand::Command {
                    command: r#"printf '{"systemMessage":"round"}'"#.into(),
                    shell: None,
                    asynchronous: None,
                    async_rewake: None,
                    common: HookCommon::default(),
                },
                HookCommand::Command {
                    command: r#"printf '{"async":true}'"#.into(),
                    shell: None,
                    asynchronous: None,
                    async_rewake: None,
                    common: HookCommon::default(),
                },
            ],
            ..HookMatcher::default()
        }],
    )];
    let hooks = HookRuntime::with_options(settings, fixed_options());
    hooks
        .callback_executor()
        .register(
            &"cb".into(),
            handler({
                let calls = calls.clone();
                move |_| {
                    let calls = calls.clone();
                    Box::pin(async move {
                        *calls.lock().unwrap() += 1;
                        Ok(CallbackResult::Text("{}".into()))
                    })
                }
            }),
        )
        .unwrap();
    let runtime = LifecycleRuntime::new(hooks);
    let cwd = std::env::temp_dir().to_string_lossy().into_owned();
    let handler = host_lifecycle::Handler::new(
        runtime.clone(),
        host_lifecycle::Binding {
            session_id: "session-1".into(),
            turn_id: "turn-1".into(),
            cwd: cwd.clone().into(),
            permission_mode: None,
            base_env: None,
            signal: None,
        },
    );

    let response = handler
        .call(module_call("dispatch", "Stop", None))
        .await
        .unwrap();
    let first = field(&JsValue::Object(response), "result").clone();
    assert!(json(field(&first, "effects")).contains("round"));
    let first_pending = array(field(&first, "pendingAsyncHooks"));
    assert_eq!(first_pending.len(), 1);
    let first_id = string(field(&first_pending[0], "id"));
    assert_eq!(*calls.lock().unwrap(), 1);

    handler.dispose().await.unwrap();

    // Pending async and callback registrations survive the turn teardown.
    assert!(runtime.complete_async_hook(
        &first_id,
        AsyncHookCompletion {
            stdout: Some(r#"{"reason":"late"}"#.into()),
            ..Default::default()
        }
    ));
    assert_eq!(runtime.collect_async_hook_responses().len(), 1);

    let response = handler
        .call(module_call("dispatch", "Stop", None))
        .await
        .expect("second turn must still run hooks");
    let second = field(&JsValue::Object(response), "result").clone();
    assert!(json(field(&second, "effects")).contains("round"));
    let second_pending = array(field(&second, "pendingAsyncHooks"));
    assert_eq!(second_pending.len(), 1);
    assert_ne!(
        string(field(&second_pending[0], "id")),
        first_id,
        "the second turn registers a new pending async hook"
    );
    assert_eq!(
        *calls.lock().unwrap(),
        2,
        "callback survives and runs again"
    );

    runtime.dispose().await;
}

/// C5: dropping the first `dispose()` future (as `TurnScope`'s bounded wait
/// does) must not leave the runtime stuck in `draining`; a later dispose must
/// still finish.
#[tokio::test]
async fn runtime_dispose_survives_dropped_first_caller() {
    let settings: HooksSettings = vec![(
        "Stop".into(),
        vec![HookMatcher {
            hooks: vec![HookCommand::Command {
                command: "sleep 0.4".into(),
                shell: None,
                asynchronous: None,
                async_rewake: None,
                common: HookCommon::default(),
            }],
            ..HookMatcher::default()
        }],
    )];
    let runtime = HookRuntime::with_options(settings, fixed_options());
    let input = HookRuntimeRunInput {
        event: "Stop".into(),
        hook_input: JsObject::new(),
        match_query: Some("Stop".into()),
        cwd: std::env::temp_dir().to_string_lossy().into_owned().into(),
        env: None,
        signal: None,
    };
    let running = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.run(input).await })
    };
    tokio::task::yield_now().await;
    let disposing = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.dispose().await })
    };
    tokio::task::yield_now().await;
    assert_eq!(runtime.lifecycle_state(), "draining");
    disposing.abort();
    let _ = disposing.await;
    timeout(Duration::from_secs(3), runtime.dispose())
        .await
        .expect("a later dispose must not hang after the first caller is dropped");
    assert_eq!(runtime.lifecycle_state(), "disposed");
    let _ = running.await;
}

fn module_call(operation: &str, event: &str, payload: Option<JsValue>) -> JsValue {
    let mut call = JsObject::new();
    call.insert("module".into(), "lifecycle".into());
    let mut body = JsObject::new();
    body.insert("operation".into(), operation.into());
    body.insert("event".into(), event.into());
    if let Some(value) = payload {
        body.insert("payload".into(), value);
    }
    call.insert("payload".into(), JsValue::Object(body));
    JsValue::Object(call)
}

#[derive(Debug, PartialEq)]
struct SeenRegistration {
    hook_name: String,
    invocation_id: String,
    timeout_ms: Option<f64>,
}
struct RegistrationSink(Arc<Mutex<Vec<SeenRegistration>>>);
impl pilotdeck_gateway::runtime::hooks::runtime::AsyncHookRegistrationHandler for RegistrationSink {
    fn handle(
        &self,
        registration: pilotdeck_gateway::runtime::hooks::runtime::AsyncHookRegistration,
    ) {
        self.0.lock().unwrap().push(SeenRegistration {
            hook_name: registration.hook_name.to_utf8().unwrap(),
            invocation_id: registration.invocation_id.to_utf8().unwrap(),
            timeout_ms: registration.timeout_ms,
        });
    }
}

#[tokio::test]
async fn async_invocation_id_prefers_the_registration_bridge() {
    let sink = Arc::new(Mutex::new(Vec::new()));
    let runtime = HookRuntime::with_options(
        vec![(
            "Stop".into(),
            vec![HookMatcher {
                hooks: vec![HookCommand::Command {
                    command:
                        r#"/bin/echo '{"async":true,"asyncHookId":"aid","asyncTimeout":1.25}'"#
                            .into(),
                    shell: None,
                    asynchronous: None,
                    async_rewake: Some(true),
                    common: HookCommon::default(),
                }],
                ..HookMatcher::default()
            }],
        )],
        HookRuntimeOptions {
            clock: Arc::new(|| JsString::from(FIXED_START)),
            shell: None,
            on_async_registered: Some(Arc::new(RegistrationSink(sink.clone()))),
        },
    );
    let result = runtime
        .run(HookRuntimeRunInput {
            event: "Stop".into(),
            hook_input: JsObject::new(),
            match_query: Some("Stop".into()),
            cwd: std::env::temp_dir().to_string_lossy().into_owned().into(),
            env: None,
            signal: None,
        })
        .await
        .unwrap();
    assert!(result.pending_async_hooks.is_empty());
    assert_eq!(
        *sink.lock().unwrap(),
        vec![SeenRegistration {
            hook_name: "command".into(),
            invocation_id: "aid".into(),
            timeout_ms: Some(1250.0),
        }]
    );
    runtime.dispose().await;
}

/// Regenerates the oracle from the real TS baseline and compares bytes.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn hooks_runtime_fixtures_regenerate_identically_from_ts() {
    baseline::regenerates_identically("hooks-runtime");
}

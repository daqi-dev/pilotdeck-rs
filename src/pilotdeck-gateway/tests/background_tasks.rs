//! Replay the real-TS background task oracle (A6.5-pr12): task_* tool
//! definitions, the no-runtime errors, task_wait validation, and step
//! sequences over real detached shells, background subagents and the SDK
//! stop/list projections. Every recorded value is compared as serialized
//! JSON bytes after the same placeholder pass the generator applies (see
//! fixtures/background-tasks/README.md).
#![cfg(unix)]
#![allow(clippy::result_large_err)]
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/child_signals.rs"]
mod child_signals;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::{
    host::modules::{
        capability::RuntimeContext,
        tool_context::{HostToolCheckpoint, ToolRuntimeContext},
    },
    runtime::{
        background::{
            BackgroundResources, BackgroundSubagentRuntime, BackgroundTaskRuntime,
            BackgroundTaskRuntimeOptions, StartBackgroundSubagentInput, StartTaskSpec,
            SubagentKind, SubagentListFilter, background_tasks_for_sdk,
            background_tasks_in_registry, stop_background_task_for_sdk,
            stop_background_task_in_registry,
        },
        project::{ProjectRuntimeRegistry, ResourceBundle, RuntimeResult},
        tools::{
            ToolCancellation, ToolError, ToolOutput, ToolRunContext, builtin::task::task_tools,
        },
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{
    collections::BTreeSet,
    fs,
    future::Future,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

const ORACLE: &str = include_str!("fixtures/background-tasks/oracle.json");
const FIXED_MS: f64 = 1_767_225_600_000.0;
const TOOL_NAMES: [&str; 5] = [
    "task_create",
    "task_list",
    "task_output",
    "task_wait",
    "task_stop",
];

fn oracle() -> &'static JsValue {
    static PARSED: OnceLock<JsValue> = OnceLock::new();
    // #1109: the optional-baseline git verification runs once per process
    // on a bounded thread (see baseline::oracle_bounded); the signal worker
    // spawned below replays without PDRS_BASELINE, so it cannot re-run the
    // 728-source probe table under its parent's bounds.
    PARSED.get_or_init(|| baseline::oracle_bounded(ORACLE))
}

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn text(value: &JsValue) -> String {
    String::from_utf16_lossy(value.as_string().expect("#956 #957 fixture string").units())
}
fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(values) => values,
        other => panic!("#956 #957 fixture array, got {other:?}"),
    }
}
fn number(value: &JsValue) -> f64 {
    value.as_number().expect("#956 #957 fixture number")
}
fn object(entries: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    object
}
fn wire(value: &JsValue) -> String {
    stringify_json(value)
        .expect("#956 #957 replay serialization/runtime")
        .unwrap_or_default()
}
fn section<'a>(name: &str, count: usize) -> &'a [JsValue] {
    let items = array(field(oracle(), name));
    assert_eq!(items.len(), count, "#956 #957 oracle section {name}");
    assert_eq!(
        field(field(oracle(), "counts"), name).as_number(),
        Some(count as f64),
        "#956 #957 oracle counts.{name}"
    );
    items
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("#956 #957 replay serialization/runtime")
}

fn context(fields: JsObject, signal: Option<ToolCancellation>) -> ToolRunContext {
    let checkpoint = HostToolCheckpoint::default();
    ToolRunContext::new(Arc::new(ToolRuntimeContext {
        fields,
        files: checkpoint.tool_context_state(),
        abort_signal: signal.map(|signal| Arc::new(signal) as RuntimeContext),
        model: None,
        elicitation: None,
        user_dialog: None,
        audit_recorder: None,
        file_history: None,
        file_update_notifier: None,
        now: Some(Arc::new(|| JsValue::Number(FIXED_MS))),
        progress: None,
        subagent: None,
        plan_todo: None,
        goal: None,
        plan_directory: None,
    }))
}

fn tool_outcome(result: Result<ToolOutput, ToolError>) -> JsValue {
    match result {
        Ok(output) => {
            let mut value = object([("content", JsValue::Array(output.content))]);
            if let Some(data) = output.data {
                value.insert("data".into(), data);
            }
            JsValue::Object(value)
        }
        Err(error) => JsValue::Object(object([(
            "error",
            JsValue::Object(object([
                ("code", JsValue::from(error.code.as_str())),
                ("message", JsValue::String(error.message)),
            ])),
        )])),
    }
}

// ---- definitions, unsupported, validation -------------------------------------

#[test]
fn definitions_replay() {
    let tools = task_tools(Some(BackgroundTaskRuntime::default()));
    let expected = section("definitions", 5);
    assert_eq!(tools.len(), expected.len());
    for (tool, expected) in tools.iter().zip(expected) {
        let mut actual = tool.fields().clone();
        actual.insert(
            "isReadOnly".into(),
            tool.is_read_only(&JsValue::Undefined).unwrap(),
        );
        actual.insert(
            "isConcurrencySafe".into(),
            tool.is_concurrency_safe(&JsValue::Undefined).unwrap(),
        );
        assert_eq!(
            wire(&JsValue::Object(actual)),
            wire(expected),
            "definition {}",
            text(field(expected, "name"))
        );
    }
}

#[test]
fn tools_without_runtime_are_unsupported() {
    let expected = section("unsupported", 5);
    runtime().block_on(async {
        for (tool, expected) in task_tools(None).iter().zip(expected) {
            let input = JsValue::Object(object([
                ("command", JsValue::from("x")),
                ("taskId", JsValue::from("x")),
            ]));
            let fields = object([
                ("cwd", JsValue::from("/")),
                ("sessionId", JsValue::from("s1")),
            ]);
            let error = tool
                .execute(input, context(fields, None))
                .await
                .expect_err("no runtime");
            let actual = object([
                ("tool", tool.fields().get("name").unwrap().clone()),
                ("code", JsValue::from(error.code.as_str())),
                ("message", JsValue::String(error.message)),
            ]);
            assert_eq!(wire(&JsValue::Object(actual)), wire(expected));
        }
    });
}

#[test]
fn task_wait_validation_replays() {
    let wait = Arc::clone(&task_tools(Some(BackgroundTaskRuntime::default()))[3]);
    runtime().block_on(async {
        for case in section("validations", 12) {
            let input = field(case, "input").clone();
            let actual = match wait
                .validate_input(input.clone(), context(JsObject::new(), None))
                .await
                .expect("validation")
            {
                None => object([("ok", JsValue::Bool(true)), ("input", input.clone())]),
                Some(issues) => object([
                    ("ok", JsValue::Bool(false)),
                    (
                        "issues",
                        JsValue::Array(issues.into_iter().map(JsValue::Object).collect()),
                    ),
                ]),
            };
            assert_eq!(
                wire(&JsValue::Object(actual)),
                wire(field(case, "expected")),
                "validate {}",
                wire(&input)
            );
        }
    });
}

// ---- sequences --------------------------------------------------------------------

/// The generator's placeholder pass, on UTF-16 units: per-run root, task and
/// subagent ids, wall-clock `waitedMs` and real pids.
struct Placeholders {
    root: String,
    tasks: Vec<String>,
    subs: Vec<String>,
}

fn replace_digits_after(text: &str, prefix: &str, replacement: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(index) = rest.find(prefix) {
        out.push_str(&rest[..index + prefix.len()]);
        let after = &rest[index + prefix.len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            out.push_str(replacement);
        }
        rest = &after[digits..];
    }
    out.push_str(rest);
    out
}

impl Placeholders {
    fn text(&self, value: &str) -> String {
        let mut out = value.replace(&self.root, "<root>");
        for (index, id) in self.tasks.iter().enumerate() {
            out = out.replace(id, &format!("<task:{index}>"));
        }
        for (index, id) in self.subs.iter().enumerate() {
            out = out.replace(id, &format!("<sub:{index}>"));
        }
        let out = replace_digits_after(&out, "waitedMs=", "<ms>");
        replace_digits_after(&out, "pid=", "<pid>")
    }
    fn apply(&self, value: &JsValue) -> JsValue {
        match value {
            JsValue::String(value) => {
                JsValue::from(self.text(&String::from_utf16_lossy(value.units())).as_str())
            }
            JsValue::Array(items) => {
                JsValue::Array(items.iter().map(|item| self.apply(item)).collect())
            }
            JsValue::Object(entries) => {
                let mut out = JsObject::new();
                for (key, item) in entries.entries() {
                    let item = if key.equals_str("waitedMs") {
                        JsValue::from("<ms>")
                    } else if key.equals_str("pid") && matches!(item, JsValue::Number(_)) {
                        JsValue::from("<pid>")
                    } else {
                        self.apply(item)
                    };
                    out.insert(key.clone(), item);
                }
                JsValue::Object(out)
            }
            other => other.clone(),
        }
    }
    fn substitute(&self, value: &JsValue) -> JsValue {
        match value {
            JsValue::String(value) => {
                let text = String::from_utf16_lossy(value.units());
                if let Some(index) = text
                    .strip_prefix("<task:")
                    .and_then(|rest| rest.strip_suffix('>'))
                {
                    return JsValue::from(
                        self.tasks[index
                            .parse::<usize>()
                            .expect("#956 #957 sequence prerequisite")]
                        .as_str(),
                    );
                }
                if let Some(index) = text
                    .strip_prefix("<sub:")
                    .and_then(|rest| rest.strip_suffix('>'))
                {
                    return JsValue::from(
                        self.subs[index
                            .parse::<usize>()
                            .expect("#956 #957 sequence prerequisite")]
                        .as_str(),
                    );
                }
                if text.contains("<root>") {
                    // Root substitution never touches lone surrogates in the
                    // fixture (they only appear in absolute non-root paths).
                    return JsValue::from(text.replace("<root>", &self.root).as_str());
                }
                JsValue::String(value.clone())
            }
            JsValue::Array(items) => {
                JsValue::Array(items.iter().map(|item| self.substitute(item)).collect())
            }
            JsValue::Object(entries) => {
                let mut out = JsObject::new();
                for (key, item) in entries.entries() {
                    out.insert(key.clone(), self.substitute(item));
                }
                JsValue::Object(out)
            }
            other => other.clone(),
        }
    }
}

fn group_alive(pid: u32) -> bool {
    let pid = rustix::process::Pid::from_raw(pid as i32).expect("#956 #957 task pid");
    rustix::process::test_kill_process_group(pid).is_ok()
}

/// A replay that panics must not leave the detached shells it started: the TS
/// baseline unrefs them, so dropping `BackgroundTaskRuntime` does not stop
/// them, and the `touch go` that ends a gated `until [ -f go ]` loop only
/// happens for a healthy sequence. Every recorded group is SIGKILLed on
/// unwind; the success path waits the groups out and disarms first, so a PID
/// reused after that point is never signalled.
struct ProcessGroups {
    pids: Mutex<BTreeSet<u32>>,
}
impl ProcessGroups {
    fn new() -> Self {
        Self {
            pids: Mutex::new(BTreeSet::new()),
        }
    }
    fn track(&self, pid: u32) {
        self.pids
            .lock()
            .expect("#956 #957 sequence prerequisite")
            .insert(pid);
    }
    fn snapshot(&self) -> Vec<u32> {
        self.pids
            .lock()
            .expect("#956 #957 sequence prerequisite")
            .iter()
            .copied()
            .collect()
    }
    fn disarm(&self) {
        self.pids
            .lock()
            .expect("#956 #957 sequence prerequisite")
            .clear();
    }
}
impl Drop for ProcessGroups {
    fn drop(&mut self) {
        for pid in self
            .pids
            .lock()
            .expect("#956 #957 sequence prerequisite")
            .iter()
        {
            if group_alive(*pid)
                && let Some(pid) = rustix::process::Pid::from_raw(*pid as i32)
            {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
        }
    }
}

async fn until(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !check() {
        assert!(
            Instant::now() < deadline,
            "#956 #957 timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn task_id(placeholders: &Placeholders, index: &JsValue) -> JsString {
    JsString::from(placeholders.tasks[number(index) as usize].as_str())
}

async fn subagent_bounded<T>(work: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(8), work)
        .await
        .expect("#957 subagent event deadline")
}

// Like CreateGate, elapsed time never releases a body. Entry and completion
// are separate events: cancellation can commit while the body is still held.
struct SubagentGate {
    behavior: String,
    entered: Option<oneshot::Receiver<ToolCancellation>>,
    release: Option<oneshot::Sender<()>>,
    signal: Option<ToolCancellation>,
}

impl SubagentGate {
    fn new(behavior: &str) -> (Self, pilotdeck_gateway::runtime::background::SubagentRun) {
        let (enter, entered) = oneshot::channel();
        let (release, receiver) = oneshot::channel();
        let gate = Self {
            behavior: behavior.into(),
            entered: Some(entered),
            release: Some(release),
            signal: None,
        };
        let behavior = behavior.to_string();
        let run: pilotdeck_gateway::runtime::background::SubagentRun = Box::new(move |signal| {
            Box::pin(async move {
                let _ = enter.send(signal.clone());
                match behavior.as_str() {
                    "fail" => Err(JsString::from("boom")),
                    "release" | "ignore-abort" => {
                        receiver.await.expect("#957 explicit body release");
                        Ok(())
                    }
                    "block" => {
                        signal.cancelled().await;
                        Err(JsString::from("stopped"))
                    }
                    other => panic!("#957 unknown subagent behavior {other}"),
                }
            })
        });
        (gate, run)
    }

    fn release(&mut self) {
        self.release
            .take()
            .expect("#957 release exactly once")
            .send(())
            .expect("#957 held body receives release");
    }

    async fn settle(&mut self, runtime: &BackgroundSubagentRuntime, id: &JsString) {
        if let Some(entered) = self.entered.take() {
            self.signal = Some(subagent_bounded(entered).await.expect("#957 body entered"));
        }
        if self.behavior == "fail" || self.release.is_none() {
            // Wait for registration, not a particular status: the unchanged
            // oracle below must still catch incorrect status/error projections.
            until("#957 subagent terminal registration", || {
                runtime.get(id).is_some_and(|task| task.ended_at.is_some())
            })
            .await;
        }
    }
}

// A real shell rendezvous reproduces Node's creation-before-exit observation
// without changing the command, task projection, or production scheduler.
struct CreateGate {
    fifo: PathBuf,
    shell: PathBuf,
    writer: Option<fs::File>,
}

impl CreateGate {
    fn new(root: &Path, index: usize) -> Self {
        let fifo = root.join(format!("create-{index}.fifo"));
        let shell = root.join(format!("create-{index}.sh"));
        assert!(
            std::process::Command::new("/usr/bin/mkfifo")
                .arg(&fifo)
                .status()
                .expect("#956 #957 create FIFO process")
                .success(),
            "#956 #957 create FIFO"
        );
        fs::write(
            &shell,
            include_str!("fixtures/background-create-gate/shell.sh"),
        )
        .expect("#956 #957 write gated shell");
        fs::set_permissions(&shell, fs::Permissions::from_mode(0o700))
            .expect("#956 #957 executable gated shell");
        Self {
            fifo,
            shell,
            writer: None,
        }
    }

    fn configure(&self, env: &mut JsObject) {
        env.insert(
            "PILOTDECK_SHELL_PATH".into(),
            JsValue::from(self.shell.to_string_lossy().as_ref()),
        );
        env.insert(
            "PDRS_CREATE_FIFO".into(),
            JsValue::from(self.fifo.to_string_lossy().as_ref()),
        );
    }

    async fn connect(&mut self, tasks: &BackgroundTaskRuntime, id: &JsString) {
        let start = Instant::now();
        self.connect_with_elapsed(tasks, id, || start.elapsed())
            .await
            .unwrap_or_else(|error| panic!("{error}"));
    }

    async fn connect_with_elapsed(
        &mut self,
        tasks: &BackgroundTaskRuntime,
        id: &JsString,
        elapsed: impl Fn() -> Duration,
    ) -> Result<(), String> {
        loop {
            // Success is the reader rendezvous, never an elapsed-time release.
            match fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&self.fifo)
            {
                Ok(writer) => {
                    self.writer = Some(writer);
                    return Ok(());
                }
                Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {}
                Err(error) => return Err(format!("#960 connect FIFO: {error}")),
            }
            let task = tasks.get(id, None).expect("#960 FIFO task registered");
            let pid = task.pid.expect("#960 FIFO child pid");
            let child = rustix::process::Pid::from_raw(pid as i32).expect("#960 valid child pid");
            let alive = rustix::process::test_kill_process(child).is_ok();
            let waited = elapsed();
            let detail = format!(
                "pid={pid} status={} alive={alive} waited={}ms",
                task.status.as_str(),
                waited.as_millis()
            );
            if !alive || task.ended_at.is_some() || task.status.as_str() != "running" {
                return Err(format!(
                    "#960 FIFO child exited before reader rendezvous: {detail}"
                ));
            }
            if waited >= Duration::from_secs(60) {
                return Err(format!("#960 FIFO reader rendezvous total limit: {detail}"));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn release(mut self) {
        self.writer
            .as_mut()
            .expect("#956 #957 connected FIFO writer")
            .write_all(b"release\n")
            .expect("#956 #957 release FIFO");
    }
}

async fn run_sequence(sequence: &JsValue, scratch_root: &Path) {
    let name = text(field(sequence, "name"));
    let root = scratch_root.join(&name);
    fs::create_dir(&root).expect("#956 #957 sequence prerequisite");
    fs::write(root.join("file"), "not a directory\n").expect("#956 #957 sequence prerequisite");
    fs::set_permissions(root.join("file"), fs::Permissions::from_mode(0o644))
        .expect("#956 #957 sequence prerequisite");

    let completions = Arc::new(Mutex::new(Vec::new()));
    let options = field(sequence, "options");
    let tasks = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
        disk_spill_dir: None,
        snapshot_store: None,
        now: Some(Arc::new(|| FIXED_MS)),
        max_tasks: field(options, "maxTasks").as_number().map(|n| n as usize),
        on_completion: Some({
            let completions = completions.clone();
            Arc::new(move |event: JsObject| {
                completions
                    .lock()
                    .expect("#956 #957 sequence prerequisite")
                    .push(JsValue::Object(event))
            })
        }),
        completion_preview_bytes: field(options, "completionPreviewBytes").as_number(),
    });
    let subagents = BackgroundSubagentRuntime::new(Some(Arc::new(|| FIXED_MS)));
    let resources = BackgroundResources {
        tasks: tasks.clone(),
        subagents: subagents.clone(),
    };
    let tools = task_tools(Some(tasks.clone()));
    let tool = |name: &str| {
        let index = TOOL_NAMES
            .iter()
            .position(|tool| *tool == name)
            .expect("#956 #957 sequence prerequisite");
        Arc::clone(&tools[index])
    };
    let mut placeholders = Placeholders {
        root: root.to_string_lossy().into_owned(),
        tasks: Vec::new(),
        subs: Vec::new(),
    };
    let groups = ProcessGroups::new();
    let mut sub_gates: Vec<SubagentGate> = Vec::new();

    for (index, step) in array(field(sequence, "steps")).iter().enumerate() {
        // #1109: progress receipt. Each step is real replay work; printing
        // it lets the signal-injected worker's parent distinguish a slow
        // but progressing run from a hung one instead of bounding the real
        // work by wall clock alone.
        println!("BACKGROUND_SEQUENCE_STEP {name} {index}");
        let op = text(field(step, "op"));
        let label = format!("{name} step {index} {op}");
        let expected = field(step, "result");
        let mut create_gate = None;
        let actual: Option<JsValue> = match op.as_str() {
            "tool" => {
                let signal = (!field(step, "abort").is_undefined()).then(ToolCancellation::default);
                if field(step, "abort")
                    .as_string()
                    .is_some_and(|s| s.equals_str("pre"))
                {
                    signal
                        .as_ref()
                        .expect("#956 #957 sequence prerequisite")
                        .cancel();
                }
                let mut env = object([
                    ("PATH", JsValue::from("/usr/bin:/bin")),
                    ("LANG", JsValue::from("en_US.UTF-8")),
                    ("HOME", JsValue::from("<root>")),
                ]);
                if let JsValue::Object(extra) = field(step, "env") {
                    for (key, value) in extra.entries() {
                        env.insert(key.clone(), value.clone());
                    }
                }
                let cwd = match field(step, "cwd") {
                    JsValue::Undefined => JsValue::from("<root>"),
                    value => value.clone(),
                };
                let session = match field(step, "session") {
                    JsValue::Undefined => JsValue::from("s1"),
                    value => value.clone(),
                };
                // Only successful native starts have an oracle PID. Preserve
                // synchronous spawn errors and the no-child async error case.
                if text(field(step, "tool")) == "task_create"
                    && field(field(expected, "data"), "pid").as_string().is_some()
                {
                    let gate = CreateGate::new(&root, index);
                    gate.configure(&mut env);
                    create_gate = Some(gate);
                }
                let fields = object([
                    ("cwd", placeholders.substitute(&cwd)),
                    ("env", placeholders.substitute(&JsValue::Object(env))),
                    ("sessionId", session),
                ]);
                let tool_name = text(field(step, "tool"));
                let input = placeholders.substitute(field(step, "input"));
                let call = tool(&tool_name).execute(input, context(fields, signal.clone()));
                let canceller = async {
                    if let (Some(ms), Some(signal)) = (field(step, "abort").as_number(), &signal) {
                        tokio::time::sleep(Duration::from_millis(ms as u64)).await;
                        signal.cancel();
                    }
                };
                let (result, ()) = tokio::join!(call, canceller);
                if tool_name == "task_create"
                    && let Ok(output) = &result
                {
                    let data = output
                        .data
                        .as_ref()
                        .expect("#956 #957 sequence prerequisite");
                    placeholders.tasks.push(text(field(data, "taskId")));
                    if let Some(pid) = field(data, "pid").as_number() {
                        groups.track(pid as u32);
                    }
                }
                if let Some(gate) = &mut create_gate {
                    let id = placeholders.tasks.last().expect("#960 created task ID");
                    gate.connect(&tasks, &JsString::from(id.as_str())).await;
                }
                Some(placeholders.apply(&tool_outcome(result)))
            }
            "awaitBytes" => {
                let id = task_id(&placeholders, field(step, "task"));
                let bytes = number(field(step, "bytes"));
                let total = || {
                    tasks
                        .get_output(&id, &JsValue::Number(0.0), &JsValue::Undefined, None)
                        .expect("#956 #957 sequence prerequisite")
                        .total_bytes
                };
                until(&label, || total() >= bytes).await;
                assert_eq!(total(), bytes, "#956 #957 {label}: exact byte count");
                None
            }
            "awaitStatus" => {
                let id = task_id(&placeholders, field(step, "task"));
                let status = text(field(step, "status"));
                until(&label, || {
                    tasks
                        .get(&id, None)
                        .is_some_and(|task| task.status.as_str() == status)
                })
                .await;
                None
            }
            "touch" => {
                fs::write(root.join(text(field(step, "path"))), "")
                    .expect("#956 #957 sequence prerequisite");
                None
            }
            "groupGone" => {
                let id = task_id(&placeholders, field(step, "task"));
                let pid = tasks
                    .get(&id, None)
                    .and_then(|task| task.pid)
                    .expect("#956 #957 task pid");
                until(&label, || !group_alive(pid)).await;
                Some(JsValue::Bool(true))
            }
            "dispose" => {
                tasks.dispose().await;
                subagents.shutdown();
                Some(JsValue::Bool(true))
            }
            "sdkStop" | "sdkList" => {
                let session = field(step, "sessionKey")
                    .as_string()
                    .expect("#956 #957 sequence prerequisite")
                    .clone();
                let task = match step.get("task") {
                    Some(value) => placeholders.substitute(value),
                    None => JsValue::Undefined,
                };
                let result = if op == "sdkStop" {
                    let id = task.as_string().expect("#957 SDK task id");
                    let stop = stop_background_task_for_sdk(&resources, &session, id);
                    tokio::pin!(stop);
                    let gated = placeholders
                        .subs
                        .iter()
                        .position(|sub| id.equals_str(sub))
                        .and_then(|index| sub_gates.get_mut(index))
                        .filter(|gate| gate.behavior == "ignore-abort" && gate.release.is_some());
                    subagent_bounded(async {
                        if let Some(gate) = gated {
                            let signal = gate.signal.clone().expect("#957 stop follows body entry");
                            tokio::select! {
                                result = &mut stop => result,
                                () = signal.cancelled() => {
                                    gate.release();
                                    stop.await
                                }
                            }
                        } else {
                            stop.await
                        }
                    })
                    .await
                } else {
                    Ok(background_tasks_for_sdk(&resources, &session, &task))
                };
                let value = match result {
                    Ok(result) => JsValue::Object(result),
                    Err(error) => {
                        let mut inner = object([("name", JsValue::from(error.name.as_str()))]);
                        if let Some(code) = error.code {
                            inner.insert("code".into(), JsValue::from(code.as_str()));
                        }
                        inner.insert("message".into(), JsValue::String(error.message));
                        JsValue::Object(object([("error", JsValue::Object(inner))]))
                    }
                };
                Some(placeholders.apply(&value))
            }
            "subStart" => {
                let index = placeholders.subs.len();
                let behavior = text(field(step, "behavior"));
                let (gate, run) = SubagentGate::new(&behavior);
                sub_gates.push(gate);
                let task = subagents.start(StartBackgroundSubagentInput {
                    kind: field(step, "kind").as_string().map(|kind| {
                        if kind.equals_str("observer") {
                            SubagentKind::Observer
                        } else {
                            SubagentKind::Background
                        }
                    }),
                    session_id: field(step, "sessionId")
                        .as_string()
                        .expect("#956 #957 sequence prerequisite")
                        .clone(),
                    parent_turn_id: JsString::from(format!("turn-{index}").as_str()),
                    subagent_id: JsString::from(format!("sa-{index}").as_str()),
                    subagent_type: JsString::from("general"),
                    observed_subagent_id: field(step, "observedSubagentId").as_string().cloned(),
                    run,
                });
                placeholders
                    .subs
                    .push(String::from_utf16_lossy(task.task_id.units()));
                let mut value = object([
                    ("taskId", JsValue::String(task.task_id)),
                    ("kind", JsValue::from(task.kind.as_str())),
                    ("sessionId", JsValue::String(task.session_id)),
                    ("parentTurnId", JsValue::String(task.parent_turn_id)),
                    ("subagentId", JsValue::String(task.subagent_id)),
                    ("subagentType", JsValue::String(task.subagent_type)),
                ]);
                if let Some(observed) = task.observed_subagent_id {
                    value.insert("observedSubagentId".into(), JsValue::String(observed));
                }
                value.insert("status".into(), JsValue::from(task.status.as_str()));
                Some(placeholders.apply(&JsValue::Object(value)))
            }
            "subRelease" => {
                let index = number(field(step, "sub")) as usize;
                sub_gates[index].release();
                None
            }
            "subTick" => {
                for (gate, id) in sub_gates.iter_mut().zip(&placeholders.subs) {
                    gate.settle(&subagents, &JsString::from(id.as_str())).await;
                }
                let listed = subagents
                    .list(&SubagentListFilter::default())
                    .into_iter()
                    .map(|task| {
                        let mut value = object([
                            ("taskId", JsValue::String(task.task_id)),
                            ("kind", JsValue::from(task.kind.as_str())),
                            ("status", JsValue::from(task.status.as_str())),
                        ]);
                        if let Some(error) = task.error {
                            value.insert("error".into(), JsValue::String(error));
                        }
                        value.insert("ended".into(), JsValue::Bool(task.ended_at.is_some()));
                        JsValue::Object(value)
                    })
                    .collect();
                Some(placeholders.apply(&JsValue::Array(listed)))
            }
            other => panic!("#956 #957 {label}: unknown op {other}"),
        };
        match actual {
            Some(actual) => assert_eq!(wire(&actual), wire(expected), "#956 #957 {label}"),
            None => assert!(
                expected.is_undefined(),
                "#956 #957 {label}: unexpected result"
            ),
        }
        // The shell cannot finish before the strict creation snapshot above.
        if let Some(gate) = create_gate {
            gate.release();
        }
    }

    assert_eq!(
        placeholders.tasks.len() as f64,
        number(field(sequence, "tasks")),
        "#956 #957 {name}: task count"
    );
    if field(sequence, "noDispose").as_bool() != Some(true) {
        for pid in groups.snapshot() {
            until(&format!("#956 #957 {name}: leftover group {pid}"), || {
                !group_alive(pid)
            })
            .await;
        }
        // Confirmed gone; never signal these PIDs again in case one is reused
        // before the guard drops.
        groups.disarm();
    }
    let task_index = |event: &JsValue| {
        let id = text(field(event, "taskId"));
        placeholders
            .tasks
            .iter()
            .position(|task| *task == id)
            .expect("#956 #957 sequence prerequisite")
    };
    let mut completions = completions
        .lock()
        .expect("#956 #957 sequence prerequisite")
        .clone();
    completions.sort_by_key(task_index);
    assert_eq!(
        wire(&placeholders.apply(&JsValue::Array(completions))),
        wire(field(sequence, "completions")),
        "#956 #957 {name}: completion events"
    );
}

fn subagent_state(runtime: &BackgroundSubagentRuntime, id: &JsString) -> JsValue {
    let task = runtime.get(id).expect("#957 registered subagent");
    JsValue::Object(object([
        ("status", JsValue::from(task.status.as_str())),
        ("ended", JsValue::Bool(task.ended_at.is_some())),
        ("error", task.error.map_or(JsValue::Null, JsValue::String)),
    ]))
}

#[test]
fn subagent_gate_holds_until_release() {
    runtime().block_on(async {
        for behavior in ["release", "ignore-abort", "block"] {
            let (mut gate, run) = SubagentGate::new(behavior);
            let signal = ToolCancellation::default();
            let mut body = run(signal.clone());
            let pending = std::future::poll_fn(|cx| {
                std::task::Poll::Ready(body.as_mut().poll(cx).is_pending())
            })
            .await;
            assert!(pending, "#957 {behavior} body held before release");
            if behavior == "ignore-abort" || behavior == "block" {
                signal.cancel();
                subagent_bounded(signal.cancelled()).await;
            }
            if behavior == "ignore-abort" {
                let pending = std::future::poll_fn(|cx| {
                    std::task::Poll::Ready(body.as_mut().poll(cx).is_pending())
                })
                .await;
                assert!(pending, "#957 cancelled body held before release");
            }
            if behavior != "block" {
                gate.release();
            }
            assert_eq!(
                subagent_bounded(body).await,
                if behavior == "block" {
                    Err(JsString::from("stopped"))
                } else {
                    Ok(())
                },
                "#957 {behavior} body outcome"
            );
        }
    });
}

#[test]
fn subagent_gate_lifecycle() {
    let expected = baseline::oracle(include_str!("fixtures/background-subtick-gate/oracle.json"));
    let cases = array(field(&expected, "cases"));
    assert_eq!(
        cases
            .iter()
            .map(|case| text(field(case, "behavior")))
            .collect::<Vec<_>>(),
        ["release", "fail", "block", "ignore-abort"],
        "#957 lifecycle matrix coverage"
    );
    runtime().block_on(async {
        for case in cases {
            let behavior = text(field(case, "behavior"));
            let resources = BackgroundResources::default();
            let subagents = &resources.subagents;
            let (mut gate, run) = SubagentGate::new(&behavior);
            let session = JsString::from("s1");
            let task = subagents.start(StartBackgroundSubagentInput {
                kind: None,
                session_id: session.clone(),
                parent_turn_id: JsString::from("turn"),
                subagent_id: JsString::from("sa"),
                subagent_type: JsString::from("general"),
                observed_subagent_id: None,
                run,
            });
            let id = &task.task_id;
            gate.settle(subagents, id).await;
            assert!(gate.signal.is_some(), "#957 entry signal captured");
            let held = subagent_state(subagents, id);
            if behavior == "release" {
                gate.release();
                gate.settle(subagents, id).await;
            }
            let stop = stop_background_task_for_sdk(&resources, &session, id);
            tokio::pin!(stop);
            let mut cancelled = JsValue::Null;
            let mut stop_pending = JsValue::Null;
            if behavior == "ignore-abort" {
                let pending = std::future::poll_fn(|cx| {
                    std::task::Poll::Ready(stop.as_mut().poll(cx).is_pending())
                })
                .await;
                assert!(pending, "#957 stop pending while body held");
                cancelled = subagent_state(subagents, id);
                stop_pending = JsValue::Bool(pending);
                gate.release();
            }
            let stopped = subagent_bounded(stop).await.expect("#957 SDK stop result");
            let actual = JsValue::Object(object([
                ("behavior", JsValue::from(behavior.as_str())),
                ("start", JsValue::from(task.status.as_str())),
                ("held", held),
                ("cancelled", cancelled),
                ("stopPending", stop_pending),
                ("stopped", JsValue::Object(stopped)),
                ("terminal", subagent_state(subagents, id)),
            ]));
            assert_eq!(wire(&actual), wire(case), "#957 lifecycle {behavior}");
        }
    });
}

#[test]
#[ignore = "requires the fixed TS baseline and Node v22.22.0"]
fn subagent_gate_regenerates_identically() {
    baseline::regenerates_identically("background-subtick-gate");
}

#[test]
fn sdk_projections_replay() {
    let sequence = section("sequences", 11)
        .iter()
        .find(|sequence| text(field(sequence, "name")) == "sdk-projections")
        .expect("#957 sdk-projections sequence exists");
    let scratch = scratch::Scratch::new("background-sdk-projections");
    let root = fs::canonicalize(scratch.path()).expect("#957 SDK scratch root");
    runtime().block_on(run_sequence(sequence, &root));
}

#[test]
fn dispose_drains_replay() {
    let sequence = section("sequences", 11)
        .iter()
        .find(|sequence| text(field(sequence, "name")) == "dispose-drains")
        .expect("#956 #957 dispose-drains sequence exists");
    let scratch = scratch::Scratch::new("background-dispose-drains");
    let root = fs::canonicalize(scratch.path()).expect("#956 #957 dispose-drains scratch root");
    runtime().block_on(run_sequence(sequence, &root));
}

#[test]
fn creation_gate_lifecycle() {
    let expected = baseline::oracle(include_str!("fixtures/background-create-gate/oracle.json"));
    let scratch = scratch::Scratch::new("background-create-gate");
    let root = fs::canonicalize(scratch.path()).expect("#956 #957 gate scratch root");
    let cases = array(field(&expected, "cases"));
    assert_eq!(
        cases
            .iter()
            .map(|case| text(field(case, "mode")))
            .collect::<Vec<_>>(),
        ["release", "stop", "dispose"],
        "#956 #957 lifecycle matrix coverage"
    );
    runtime().block_on(async {
        for (index, case) in cases.iter().enumerate() {
            let mode = text(field(case, "mode"));
            let tasks = BackgroundTaskRuntime::default();
            let groups = ProcessGroups::new();
            let mut gate = CreateGate::new(&root, index);
            let mut env = object([("PATH", JsValue::from("/usr/bin:/bin"))]);
            gate.configure(&mut env);
            let fields = object([
                ("cwd", JsValue::from(root.to_string_lossy().as_ref())),
                ("sessionId", JsValue::from("s1")),
                ("env", JsValue::Object(env)),
            ]);
            let output = task_tools(Some(tasks.clone()))[0]
                .execute(
                    JsValue::Object(object([("command", field(case, "command").clone())])),
                    context(fields, None),
                )
                .await
                .expect("#956 #957 gated create must succeed");
            let data = output.data.expect("#956 #957 gated create data");
            let id = field(&data, "taskId")
                .as_string()
                .expect("#956 #957 gated task ID")
                .clone();
            groups.track(number(field(&data, "pid")) as u32);
            gate.connect(&tasks, &id).await;
            assert_eq!(
                field(&data, "status"),
                field(case, "create"),
                "#956 #957 gated create snapshot"
            );
            // No token is ever sent on stop/dispose: cancellation must drain a
            // predecessor that will never complete by itself.
            let held = tasks.get(&id, None).expect("#956 #957 held task exists");
            assert_eq!(
                held.status.as_str(),
                text(field(case, "held")),
                "#956 #957 held status"
            );
            assert_eq!(
                &held.command,
                field(case, "command"),
                "#956 #957 original command retained"
            );
            assert_eq!(held.output_bytes, 0, "#956 #957 no output before release");
            if mode == "release" {
                gate.release();
                until("#956 #957 released command completes", || {
                    tasks
                        .get(&id, None)
                        .is_some_and(|task| task.ended_at.is_some())
                })
                .await;
            } else if mode == "stop" {
                tokio::time::timeout(
                    Duration::from_secs(8),
                    tasks.stop(&id, &JsValue::Undefined, None),
                )
                .await
                .expect("#956 #957 stop deadline")
                .expect("#956 #957 stop blocked command");
            } else {
                assert_eq!(mode, "dispose", "#956 #957 lifecycle mode");
                tokio::time::timeout(Duration::from_secs(8), tasks.dispose())
                    .await
                    .expect("#956 #957 dispose deadline");
            }
            let task = tasks
                .get(&id, None)
                .expect("#956 #957 terminal task exists");
            assert_eq!(
                task.status.as_str(),
                text(field(case, "terminal")),
                "#956 #957 terminal status"
            );
            assert_eq!(
                task.output_bytes as f64,
                number(field(case, "outputBytes")),
                "#956 #957 terminal output bytes"
            );
            tokio::time::timeout(Duration::from_secs(8), tasks.dispose())
                .await
                .expect("#956 #957 cleanup deadline");
            for pid in groups.snapshot() {
                until("#956 #957 gate group exits", || !group_alive(pid)).await;
            }
            groups.disarm();
        }
    });
}

// The second FIFO holds a real child before it can reach the creation FIFO.
// Only the failure clock is injected: these tests still open the real FIFO,
// inspect the actual runtime entry/PID, and cancel/drain the real process.
fn creation_watchdog_case(mode: &str) {
    let expected = baseline::oracle(include_str!("fixtures/background-create-gate/oracle.json"));
    let cases = array(field(&expected, "watchdog"));
    assert_eq!(
        cases
            .iter()
            .map(|case| text(field(case, "mode")))
            .collect::<Vec<_>>(),
        [
            "delayed-reader",
            "exited-reader",
            "cancelled-reader",
            "never-reader"
        ],
        "#960 watchdog matrix coverage"
    );
    let case = cases
        .iter()
        .find(|case| text(field(case, "mode")) == mode)
        .expect("#960 watchdog case exists");
    let scratch = scratch::Scratch::new("background-create-watchdog");
    let root = fs::canonicalize(scratch.path()).expect("#960 watchdog scratch root");
    runtime().block_on(async {
        let tasks = BackgroundTaskRuntime::default();
        let groups = ProcessGroups::new();
        let mut gate = CreateGate::new(&root, 0);
        let mut admission = CreateGate::new(&root, 1);
        let body = if mode == "exited-reader" {
            "exit 0\n".to_owned()
        } else {
            fs::read_to_string(&gate.shell).expect("#960 read original shell")
        };
        fs::write(
            &gate.shell,
            format!(
                "#!/bin/bash\nIFS= read -r token < '{}' || exit 96\n{body}",
                admission.fifo.display()
            ),
        )
        .expect("#960 write admission shell");
        let mut env = object([("PATH", JsValue::from("/usr/bin:/bin"))]);
        gate.configure(&mut env);
        let fields = object([
            ("cwd", JsValue::from(root.to_string_lossy().as_ref())),
            ("sessionId", JsValue::from("s1")),
            ("env", JsValue::Object(env)),
        ]);
        let output = task_tools(Some(tasks.clone()))[0]
            .execute(
                JsValue::Object(object([(
                    "command",
                    JsValue::from("printf 'done-output\\n'"),
                )])),
                context(fields, None),
            )
            .await
            .expect("#960 watchdog create succeeds");
        let data = output.data.expect("#960 watchdog create data");
        let id = field(&data, "taskId")
            .as_string()
            .expect("#960 watchdog task ID")
            .clone();
        let pid = number(field(&data, "pid")) as u32;
        groups.track(pid);
        admission.connect(&tasks, &id).await;
        let mut admission = Some(admission);
        if mode == "exited-reader" {
            admission.take().expect("#960 admission exists").release();
            until("#960 pre-reader natural exit", || {
                tasks
                    .get(&id, None)
                    .is_some_and(|task| task.ended_at.is_some())
            })
            .await;
        } else if mode == "cancelled-reader" {
            tokio::time::timeout(
                Duration::from_secs(8),
                tasks.stop(&id, &JsValue::Undefined, None),
            )
            .await
            .expect("#960 pre-reader stop deadline")
            .expect("#960 pre-reader stop succeeds");
        }
        // Terminal cases use the cap too, so a removed exit check yields a
        // wrong *returned error*, not a harness timeout accepted as a kill.
        let waited = if mode == "delayed-reader" { 9 } else { 60 };
        let mut connecting =
            std::pin::pin!(gate.connect_with_elapsed(&tasks, &id, || Duration::from_secs(waited)));
        let first = connecting
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));
        let outcome = if mode == "delayed-reader" {
            assert!(
                first.is_pending(),
                "#960 live delayed reader stays pending past 8s: {first:?}"
            );
            assert_eq!(
                tasks
                    .get(&id, None)
                    .expect("#960 held task exists")
                    .output_bytes,
                0,
                "#960 admission holds command"
            );
            admission
                .take()
                .expect("#960 admission release once")
                .release();
            tokio::time::timeout(Duration::from_secs(8), connecting.as_mut())
                .await
                .expect("#960 delayed reader connection deadline")
                .expect("#960 delayed reader connects after admission release");
            "connected"
        } else {
            let std::task::Poll::Ready(Err(error)) = first else {
                panic!("#960 watchdog must reject without polling delay: {mode} {first:?}");
            };
            assert!(
                error.contains(&format!("pid={pid} "))
                    && error.contains("status=")
                    && error.contains("alive=")
                    && error.contains("waited=60000ms"),
                "#960 watchdog diagnostic fields: {error}"
            );
            if mode == "never-reader" {
                assert!(
                    error.starts_with("#960 FIFO reader rendezvous total limit:"),
                    "#960 live missing reader reaches total limit: {error}"
                );
                "total-limit"
            } else {
                assert!(
                    error.starts_with("#960 FIFO child exited before reader rendezvous:"),
                    "#960 terminal reader rejected before total limit: {error}"
                );
                "child-exited"
            }
        };
        let task = tasks.get(&id, None).expect("#960 watchdog task retained");
        let actual = JsValue::Object(object([
            ("mode", JsValue::from(mode)),
            ("outcome", JsValue::from(outcome)),
            ("status", JsValue::from(task.status.as_str())),
            ("outputBytes", JsValue::Number(task.output_bytes as f64)),
        ]));
        assert_eq!(
            wire(&actual),
            wire(case),
            "#960 watchdog TS projection {mode}"
        );
        tokio::time::timeout(Duration::from_secs(8), tasks.dispose())
            .await
            .expect("#960 watchdog dispose deadline");
        until("#960 watchdog child drained", || !group_alive(pid)).await;
        groups.disarm();
    });
}

#[test]
fn creation_gate_waits_for_live_delayed_reader() {
    creation_watchdog_case("delayed-reader");
}

#[test]
fn creation_gate_rejects_exited_reader() {
    creation_watchdog_case("exited-reader");
}

#[test]
fn creation_gate_rejects_cancelled_reader() {
    creation_watchdog_case("cancelled-reader");
}

#[test]
fn creation_gate_bounds_missing_reader() {
    creation_watchdog_case("never-reader");
}

#[test]
#[ignore = "requires the fixed TS baseline and Node v22.22.0"]
fn creation_gate_regenerates_identically() {
    baseline::regenerates_identically("background-create-gate");
}

/// Sequences that depend on the macOS `/bin/bash` 3.2 diagnostic text; every
/// other sequence replays on every Unix.
const DARWIN_ONLY: [&str; 1] = ["non-string-command"];

#[test]
fn exit_statuses_with_inherited_ignored_and_blocked_signals() {
    // #1109: verify the committed oracle here, once for this process,
    // before launching the worker (which runs without PDRS_BASELINE).
    let _ = oracle();
    let stdout =
        child_signals::ChildSignalProbe::new().launch("inherited_exit_statuses_worker", false);
    // #1109: the progress receipts the worker's bounds rely on must actually
    // flow; a severed receipt channel would quietly degrade the bounds back
    // to wall clock alone.
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("BACKGROUND_SEQUENCE_STEP ")),
        "#1109 worker emitted no sequence progress receipts: {stdout}"
    );
}

#[test]
#[ignore = "isolated worker launched with inherited SIGINT ignored and SIGTERM blocked"]
fn inherited_exit_statuses_worker() {
    assert!(
        std::process::Command::new(child_signals::probe_path())
            .arg("inherited")
            .status()
            .expect("#1109 run the inherited-disposition probe")
            .success(),
        "#1109 inherited dispositions must hold inside the signal-injected worker"
    );
    let sequence = section("sequences", 11)
        .iter()
        .find(|sequence| text(field(sequence, "name")) == "exit-statuses")
        .expect("the complete TS exit-statuses sequence");
    let scratch = scratch::Scratch::new("background-inherited-signals");
    let root = fs::canonicalize(scratch.path()).unwrap();
    runtime().block_on(run_sequence(sequence, &root));
}

#[test]
fn sequences_replay() {
    let sequences = section("sequences", 11);
    let steps: usize = sequences
        .iter()
        .map(|sequence| array(field(sequence, "steps")).len())
        .sum();
    assert_eq!(
        (
            steps as f64,
            number(field(field(oracle(), "counts"), "steps"))
        ),
        (167.0, 167.0),
        "#956 #957 sequence step count"
    );
    let scratch = scratch::Scratch::new("background-tasks");
    let root = fs::canonicalize(scratch.path()).expect("#956 #957 canonical scratch root");
    let mut ran = Vec::new();
    runtime().block_on(async {
        for sequence in sequences {
            let name = text(field(sequence, "name"));
            let darwin_only = !field(sequence, "darwinOnly").is_undefined();
            assert_eq!(
                darwin_only,
                DARWIN_ONLY.contains(&name.as_str()),
                "#956 #957 {name}: darwinOnly"
            );
            if darwin_only && !cfg!(target_os = "macos") {
                continue;
            }
            run_sequence(sequence, &root).await;
            ran.push(name);
        }
    });
    assert_eq!(
        ran.len(),
        if cfg!(target_os = "macos") { 11 } else { 10 },
        "#956 #957 sequence coverage"
    );
}

// ---- Rust-only: project lifecycle --------------------------------------------------

/// The pair is a project resource group: disposing the registry drains every
/// running task (group gone, task cancelled), and the SDK projections resolve
/// through the registry.
#[test]
fn registry_dispose_drains_background_tasks() {
    let scratch = scratch::Scratch::new("background-registry");
    let root: PathBuf = fs::canonicalize(scratch.path()).unwrap();
    runtime().block_on(async {
        let resources = BackgroundResources::default();
        let staged = resources.clone();
        let registry = ProjectRuntimeRegistry::new(
            root.clone(),
            Arc::new(
                move |_: &Path, bundle: &mut ResourceBundle| -> RuntimeResult {
                    bundle.add(staged.clone());
                    Ok(())
                },
            ),
        );
        registry.resolve(None).unwrap();
        let tool = Arc::clone(&task_tools(Some(resources.tasks.clone()))[0]);
        let fields = object([
            ("cwd", JsValue::from(root.to_string_lossy().as_ref())),
            (
                "env",
                JsValue::Object(object([("PATH", JsValue::from("/usr/bin:/bin"))])),
            ),
            ("sessionId", JsValue::from("s1")),
        ]);
        let created = tool
            .execute(
                JsValue::Object(object([("command", JsValue::from("exec sleep 30"))])),
                context(fields, None),
            )
            .await
            .unwrap();
        let data = created.data.unwrap();
        let id = field(&data, "taskId").as_string().unwrap().clone();
        let pid = number(field(&data, "pid")) as u32;

        let listed = background_tasks_in_registry(
            &registry,
            None,
            &JsString::from("s1"),
            &JsValue::Undefined,
        )
        .unwrap();
        assert_eq!(
            wire(&JsValue::Object(listed)),
            r#"{"backgrounded":false,"reason":"no_foreground_tasks"}"#
        );
        let denied = stop_background_task_in_registry(&registry, None, &JsString::from("s2"), &id)
            .await
            .expect_err("other session");
        assert_eq!(denied.code.as_deref(), Some("BACKGROUND_TASK_NOT_FOUND"));

        registry.dispose().await.unwrap();
        until("registry dispose", || !group_alive(pid)).await;
        let task = resources.tasks.get(&id, None).unwrap();
        assert_eq!(task.status.as_str(), "cancelled");
        assert!(task.interrupted);
        assert!(
            resources
                .tasks
                .start(StartTaskSpec {
                    command: JsValue::from("true"),
                    cwd: JsString::from("/"),
                    env: None,
                    session_id: JsValue::Undefined,
                    agent_id: JsValue::Undefined,
                    kind: JsValue::Undefined,
                })
                .await
                .is_err_and(|error| String::from_utf16_lossy(error.message.units())
                    == "Background task runtime is disposed; refusing a new task.")
        );
    });
}

/// A shell whose whole group ignores SIGTERM (the disposition is inherited
/// across exec), so only the grace-period SIGKILL can end it.
fn term_ignoring(
    runtime: &BackgroundTaskRuntime,
    root: &Path,
) -> impl Future<Output = (JsString, u32)> {
    let runtime = runtime.clone();
    let cwd = JsString::from(root.to_string_lossy().as_ref());
    async move {
        let task = runtime
            .start(StartTaskSpec {
                command: JsValue::from("trap '' TERM; sleep 30"),
                cwd,
                env: None,
                session_id: JsValue::Undefined,
                agent_id: JsValue::Undefined,
                kind: JsValue::Undefined,
            })
            .await
            .unwrap();
        (task.task_id, task.pid.expect("pid"))
    }
}

/// Repair round 1 P1-1: dropping `stop` after SIGTERM must not cancel the
/// grace-period SIGKILL (the baseline's `rt.stop()` keeps running).
#[test]
fn dropped_stop_still_kills_a_term_ignoring_group() {
    let scratch = scratch::Scratch::new("background-dropped-stop");
    let root: PathBuf = fs::canonicalize(scratch.path()).unwrap();
    runtime().block_on(async {
        let tasks = BackgroundTaskRuntime::default();
        let (id, pid) = term_ignoring(&tasks, &root).await;
        // Let the shell install its trap before the TERM is sent.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let grace = JsValue::Number(300.0);
        let stop = tasks.stop(&id, &grace, None);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), stop)
                .await
                .is_err(),
            "the group ignores TERM, so stop is still in its grace period"
        );
        let during_grace = tasks.get(&id, None).unwrap();
        assert_eq!(during_grace.status.as_str(), "running");
        assert!(during_grace.interrupted);
        assert!(group_alive(pid), "TERM alone must not end the group");
        until("SIGKILL after the dropped stop", || !group_alive(pid)).await;
        until("dropped stop settles", || {
            tasks.get(&id, None).unwrap().status.as_str() == "cancelled"
        })
        .await;
        assert!(tasks.get(&id, None).unwrap().interrupted);
        tasks.dispose().await;
    });
}

/// Repair round 1 P1-1: a dropped first `dispose` must still finish, and a
/// later `dispose` must return instead of waiting forever.
#[test]
fn dropped_dispose_still_completes() {
    let scratch = scratch::Scratch::new("background-dropped-dispose");
    let root: PathBuf = fs::canonicalize(scratch.path()).unwrap();
    runtime().block_on(async {
        let tasks = BackgroundTaskRuntime::default();
        let (id, pid) = term_ignoring(&tasks, &root).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), tasks.dispose())
                .await
                .is_err(),
            "dispose waits out the 5 s grace of a TERM-ignoring group"
        );
        tokio::time::timeout(Duration::from_secs(8), tasks.dispose())
            .await
            .expect("a second dispose returns once the first one finishes");
        assert!(!group_alive(pid));
        let task = tasks.get(&id, None).unwrap();
        assert_eq!(task.status.as_str(), "cancelled");
        assert!(task.interrupted);
    });
}

/// Human-approved: specs/parity/accepted-diffs.md #5 (2026-10-21).
/// Repair round 1 P1-2 (deliberate deviation, see the module docs): a spawn
/// Node reports asynchronously never exits in the baseline, so stop/dispose
/// would hang there. Here stop settles it as cancelled at once, with the
/// completion event, and dispose does not wait.
#[test]
fn async_spawn_failure_is_settled_by_stop_and_dispose() {
    let scratch = scratch::Scratch::new("background-async-spawn");
    let root: PathBuf = fs::canonicalize(scratch.path()).unwrap();
    runtime().block_on(async {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let tasks = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
            now: Some(Arc::new(|| FIXED_MS)),
            on_completion: Some(Arc::new(move |event| sink.lock().unwrap().push(event))),
            ..Default::default()
        });
        let spec = |cwd: &str| StartTaskSpec {
            command: JsValue::from("echo x"),
            cwd: JsString::from(cwd),
            env: None,
            session_id: JsValue::from("s1"),
            agent_id: JsValue::Undefined,
            kind: JsValue::Undefined,
        };
        let missing = root.join("missing").to_string_lossy().into_owned();
        let stopped = tasks.start(spec(&missing)).await.unwrap();
        let disposed = tasks.start(spec(&missing)).await.unwrap();
        for task in [&stopped, &disposed] {
            assert_eq!(task.status.as_str(), "running");
            assert_eq!(task.pid, None);
        }
        let started = Instant::now();
        tokio::time::timeout(
            Duration::from_secs(2),
            tasks.stop(&stopped.task_id, &JsValue::Undefined, None),
        )
        .await
        .expect("stop does not wait out a grace period with no process")
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        let task = tasks.get(&stopped.task_id, None).unwrap();
        assert_eq!(
            (
                task.status.as_str(),
                task.interrupted,
                task.ended_at,
                task.exit_code
            ),
            ("cancelled", true, Some(FIXED_MS), None)
        );
        let waited = tasks
            .wait(&stopped.task_id, &JsValue::Number(50.0), None, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(waited.outcome, "completed");
        // A second stop is a no-op on the settled task.
        tasks
            .stop(&stopped.task_id, &JsValue::Undefined, None)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), tasks.dispose())
            .await
            .expect("dispose settles the other pid-less task without waiting");
        let task = tasks.get(&disposed.task_id, None).unwrap();
        assert_eq!(
            (task.status.as_str(), task.interrupted),
            ("cancelled", true)
        );
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2, "one completion event per settled task");
        for (event, task) in events.iter().zip([&stopped, &disposed]) {
            let event = JsValue::Object(event.clone());
            assert_eq!(field(&event, "taskId").as_string(), Some(&task.task_id));
            assert_eq!(text(field(&event, "status")), "cancelled");
            assert!(matches!(field(&event, "exitCode"), JsValue::Undefined));
            assert!(text(field(&event, "outputPreview")).starts_with("error: spawn "));
        }
    });
}

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
    os::unix::fs::PermissionsExt,
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
    PARSED.get_or_init(|| baseline::oracle(ORACLE))
}

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn text(value: &JsValue) -> String {
    String::from_utf16_lossy(value.as_string().expect("fixture string").units())
}
fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(values) => values,
        other => panic!("fixture array, got {other:?}"),
    }
}
fn number(value: &JsValue) -> f64 {
    value.as_number().expect("fixture number")
}
fn object(entries: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    object
}
fn wire(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap_or_default()
}
fn section<'a>(name: &str, count: usize) -> &'a [JsValue] {
    let items = array(field(oracle(), name));
    assert_eq!(items.len(), count, "oracle section {name}");
    assert_eq!(
        field(field(oracle(), "counts"), name).as_number(),
        Some(count as f64),
        "oracle counts.{name}"
    );
    items
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
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
                    return JsValue::from(self.tasks[index.parse::<usize>().unwrap()].as_str());
                }
                if let Some(index) = text
                    .strip_prefix("<sub:")
                    .and_then(|rest| rest.strip_suffix('>'))
                {
                    return JsValue::from(self.subs[index.parse::<usize>().unwrap()].as_str());
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
    let pid = rustix::process::Pid::from_raw(pid as i32).expect("pid");
    rustix::process::test_kill_process_group(pid).is_ok()
}

async fn until(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn task_id(placeholders: &Placeholders, index: &JsValue) -> JsString {
    JsString::from(placeholders.tasks[number(index) as usize].as_str())
}

/// The subagent body for one fixture behavior.
fn subagent_run(
    behavior: &str,
    release: Option<oneshot::Receiver<()>>,
) -> pilotdeck_gateway::runtime::background::SubagentRun {
    let behavior = behavior.to_string();
    Box::new(move |signal: ToolCancellation| {
        Box::pin(async move {
            match behavior.as_str() {
                "fail" => Err(JsString::from("boom")),
                "release" => {
                    let _ = release.expect("release channel").await;
                    Ok(())
                }
                "ignore-abort" => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok(())
                }
                "block" => {
                    signal.cancelled().await;
                    Err(JsString::from("stopped"))
                }
                other => panic!("unknown subagent behavior {other}"),
            }
        })
    })
}

async fn run_sequence(sequence: &JsValue, scratch_root: &Path) {
    let name = text(field(sequence, "name"));
    let root = scratch_root.join(&name);
    fs::create_dir(&root).unwrap();
    fs::write(root.join("file"), "not a directory\n").unwrap();
    fs::set_permissions(root.join("file"), fs::Permissions::from_mode(0o644)).unwrap();

    let completions = Arc::new(Mutex::new(Vec::new()));
    let options = field(sequence, "options");
    let tasks = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
        now: Some(Arc::new(|| FIXED_MS)),
        max_tasks: field(options, "maxTasks").as_number().map(|n| n as usize),
        on_completion: Some({
            let completions = completions.clone();
            Arc::new(move |event: JsObject| {
                completions.lock().unwrap().push(JsValue::Object(event))
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
        let index = TOOL_NAMES.iter().position(|tool| *tool == name).unwrap();
        Arc::clone(&tools[index])
    };
    let mut placeholders = Placeholders {
        root: root.to_string_lossy().into_owned(),
        tasks: Vec::new(),
        subs: Vec::new(),
    };
    let mut pids = BTreeSet::new();
    let mut releases: Vec<Option<oneshot::Sender<()>>> = Vec::new();

    for (index, step) in array(field(sequence, "steps")).iter().enumerate() {
        let op = text(field(step, "op"));
        let label = format!("{name} step {index} {op}");
        let expected = field(step, "result");
        let actual: Option<JsValue> = match op.as_str() {
            "tool" => {
                let signal = (!field(step, "abort").is_undefined()).then(ToolCancellation::default);
                if field(step, "abort")
                    .as_string()
                    .is_some_and(|s| s.equals_str("pre"))
                {
                    signal.as_ref().unwrap().cancel();
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
                    let data = output.data.as_ref().unwrap();
                    placeholders.tasks.push(text(field(data, "taskId")));
                    if let Some(pid) = field(data, "pid").as_number() {
                        pids.insert(pid as u32);
                    }
                }
                Some(placeholders.apply(&tool_outcome(result)))
            }
            "awaitBytes" => {
                let id = task_id(&placeholders, field(step, "task"));
                let bytes = number(field(step, "bytes"));
                let total = || {
                    tasks
                        .get_output(&id, &JsValue::Number(0.0), &JsValue::Undefined, None)
                        .unwrap()
                        .total_bytes
                };
                until(&label, || total() >= bytes).await;
                assert_eq!(total(), bytes, "{label}: exact byte count");
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
                fs::write(root.join(text(field(step, "path"))), "").unwrap();
                None
            }
            "groupGone" => {
                let id = task_id(&placeholders, field(step, "task"));
                let pid = tasks.get(&id, None).and_then(|task| task.pid).expect("pid");
                until(&label, || !group_alive(pid)).await;
                Some(JsValue::Bool(true))
            }
            "dispose" => {
                tasks.dispose().await;
                subagents.shutdown();
                Some(JsValue::Bool(true))
            }
            "sdkStop" | "sdkList" => {
                let session = field(step, "sessionKey").as_string().unwrap().clone();
                let task = match step.get("task") {
                    Some(value) => placeholders.substitute(value),
                    None => JsValue::Undefined,
                };
                let result = if op == "sdkStop" {
                    stop_background_task_for_sdk(&resources, &session, task.as_string().unwrap())
                        .await
                } else {
                    Ok(background_tasks_for_sdk(&resources, &session, &task))
                };
                let value = match result {
                    Ok(result) => JsValue::Object(result),
                    Err(error) => {
                        let mut inner = object([("name", JsValue::from(error.name))]);
                        if let Some(code) = error.code {
                            inner.insert("code".into(), JsValue::from(code));
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
                let (sender, receiver) = oneshot::channel();
                releases.push(Some(sender));
                let task = subagents.start(StartBackgroundSubagentInput {
                    kind: field(step, "kind").as_string().map(|kind| {
                        if kind.equals_str("observer") {
                            SubagentKind::Observer
                        } else {
                            SubagentKind::Background
                        }
                    }),
                    session_id: field(step, "sessionId").as_string().unwrap().clone(),
                    parent_turn_id: JsString::from(format!("turn-{index}").as_str()),
                    subagent_id: JsString::from(format!("sa-{index}").as_str()),
                    subagent_type: JsString::from("general"),
                    observed_subagent_id: field(step, "observedSubagentId").as_string().cloned(),
                    run: subagent_run(&behavior, Some(receiver)),
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
                let _ = releases[index].take().expect("release once").send(());
                None
            }
            "subTick" => {
                tokio::time::sleep(Duration::from_millis(20)).await;
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
            other => panic!("{label}: unknown op {other}"),
        };
        match actual {
            Some(actual) => assert_eq!(wire(&actual), wire(expected), "{label}"),
            None => assert!(expected.is_undefined(), "{label}: unexpected result"),
        }
    }

    assert_eq!(
        placeholders.tasks.len() as f64,
        number(field(sequence, "tasks")),
        "{name}: task count"
    );
    if field(sequence, "noDispose").as_bool() != Some(true) {
        for pid in &pids {
            until(&format!("{name}: leftover group {pid}"), || {
                !group_alive(*pid)
            })
            .await;
        }
    }
    let task_index = |event: &JsValue| {
        let id = text(field(event, "taskId"));
        placeholders
            .tasks
            .iter()
            .position(|task| *task == id)
            .unwrap()
    };
    let mut completions = completions.lock().unwrap().clone();
    completions.sort_by_key(task_index);
    assert_eq!(
        wire(&placeholders.apply(&JsValue::Array(completions))),
        wire(field(sequence, "completions")),
        "{name}: completion events"
    );
}

/// Sequences that depend on the macOS `/bin/bash` 3.2 diagnostic text; every
/// other sequence replays on every Unix.
const DARWIN_ONLY: [&str; 1] = ["non-string-command"];

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
        (167.0, 167.0)
    );
    let scratch = scratch::Scratch::new("background-tasks");
    let root = fs::canonicalize(scratch.path()).unwrap();
    let mut ran = Vec::new();
    runtime().block_on(async {
        for sequence in sequences {
            let name = text(field(sequence, "name"));
            let darwin_only = !field(sequence, "darwinOnly").is_undefined();
            assert_eq!(
                darwin_only,
                DARWIN_ONLY.contains(&name.as_str()),
                "{name}: darwinOnly"
            );
            if darwin_only && !cfg!(target_os = "macos") {
                continue;
            }
            run_sequence(sequence, &root).await;
            ran.push(name);
        }
    });
    assert_eq!(ran.len(), if cfg!(target_os = "macos") { 11 } else { 10 });
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
        assert_eq!(denied.code, Some("BACKGROUND_TASK_NOT_FOUND"));

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

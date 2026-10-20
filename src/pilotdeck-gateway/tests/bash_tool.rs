//! Replay the real-TS bash tool oracle (A6.5-pr7): definition, permission
//! classification, PermissionRuntime decisions with the real tool, scripted
//! runner formatting/diagnostics, and real `/bin/bash` processes. Every
//! comparison is the serialized JSON bytes; only `durationMs` and the per-run
//! temporary paths are normalized (see fixtures/bash-tool/README.md).
#![cfg(unix)]
#![allow(clippy::result_large_err)]
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::{
    config::permission::{PermissionMode, PermissionRule, PermissionRuleSet},
    host::{
        HostFuture,
        modules::{
            capability::{
                RuntimeContext, ToolCall, ToolDefinition, ToolExecution, ToolInvocationContext,
            },
            permission::PermissionDecision,
            tool_context::{ExecutionContext, HostToolCheckpoint, ToolRuntimeContext},
        },
    },
    runtime::{
        permission::{PermissionContext, decide},
        tools::{
            ExecutableTool, SchedulerMode, ToolCancellation, ToolError, ToolFuture, ToolOutput,
            ToolRunContext, ToolRuntime,
            builtin::bash::{
                BashTool, BashToolOptions, CommandOptions, CommandResult, CommandRunner,
                NodeShellCommandRunner, check_bash_permissions, classify_bash_permission,
                is_read_only_shell_command,
            },
            registry::ToolRegistry,
        },
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

const ORACLE: &str = include_str!("fixtures/bash-tool/oracle.json");
const FIXED_MS: f64 = 1_767_225_600_000.0;

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

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn outcome(result: Result<ToolOutput, ToolError>) -> JsValue {
    match result {
        Ok(output) => {
            let mut ok = object([("content", JsValue::Array(output.content))]);
            if let Some(data) = output.data {
                ok.insert("data".into(), data);
            }
            if let Some(metadata) = output.metadata {
                ok.insert("metadata".into(), JsValue::Object(metadata));
            }
            JsValue::Object(object([("ok", JsValue::Object(ok))]))
        }
        Err(error) => {
            let mut value = object([
                ("code", JsValue::from(error.code.as_str())),
                ("message", JsValue::String(error.message.clone())),
            ]);
            if let Some(details) = error.details {
                value.insert("details".into(), JsValue::Object(details));
            }
            JsValue::Object(object([("error", JsValue::Object(value))]))
        }
    }
}

fn host_context(
    fields: JsObject,
    signal: Option<ToolCancellation>,
    progress: Option<Arc<dyn Fn(JsObject) + Send + Sync>>,
) -> ToolRunContext {
    let checkpoint = HostToolCheckpoint::default();
    let host = ToolRuntimeContext {
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
        progress,
        subagent: None,
        plan_todo: None,
        goal: None,
        plan_directory: None,
    };
    ToolRunContext::new(Arc::new(host))
}

#[test]
fn definition_matches_baseline() {
    let tool = BashTool::new();
    assert_eq!(
        wire(&JsValue::Object(tool.fields().clone())),
        wire(field(oracle(), "definition"))
    );
}

#[test]
fn command_classification_replays() {
    for case in section("commands", 199) {
        let command = field(case, "command").as_string().unwrap();
        let lossy = String::from_utf16_lossy(command.units());
        assert_eq!(
            JsValue::Bool(is_read_only_shell_command(&lossy)),
            *field(case, "readOnly"),
            "readOnly {lossy:?}"
        );
        assert_eq!(
            wire(&classify_bash_permission(command)),
            wire(field(case, "permission")),
            "permission {lossy:?}"
        );
    }
}

#[test]
fn tool_predicates_replay() {
    let tool = BashTool::new();
    for case in section("predicates", 10) {
        let input = field(case, "input");
        assert_eq!(
            ToolDefinition::is_read_only(&tool, input).unwrap(),
            *field(case, "isReadOnly"),
            "isReadOnly {}",
            wire(input)
        );
        assert_eq!(
            tool.is_concurrency_safe(input).unwrap(),
            *field(case, "isConcurrencySafe"),
            "isConcurrencySafe {}",
            wire(input)
        );
        assert_eq!(
            wire(&check_bash_permissions(input)),
            wire(field(case, "checkPermissions")),
            "checkPermissions {}",
            wire(input)
        );
    }
}

fn parse_rules(value: &JsValue) -> Vec<PermissionRule> {
    array(value)
        .iter()
        .map(|rule| PermissionRule::from_value(rule).expect("rule object"))
        .collect()
}

fn parse_context(value: &JsValue) -> PermissionContext {
    let rules = field(value, "rules");
    PermissionContext {
        mode: PermissionMode::new(field(value, "mode").as_string().unwrap().clone()),
        rules: PermissionRuleSet {
            allow: parse_rules(field(rules, "allow")),
            deny: parse_rules(field(rules, "deny")),
            ask: parse_rules(field(rules, "ask")),
        },
        cwd: field(value, "cwd").as_string().unwrap().clone(),
        additional_working_directories: array(field(value, "additionalWorkingDirectories"))
            .iter()
            .map(|dir| dir.as_string().unwrap().clone())
            .collect(),
        can_prompt: field(value, "canPrompt").as_bool().unwrap(),
        policy_can_prompt: field(value, "policyCanPrompt").as_bool(),
        bypass_available: field(value, "bypassAvailable").as_bool().unwrap(),
        accept_edits: field(value, "acceptEdits").as_bool(),
        plan_directory_path: field(value, "planDirectoryPath").as_string().cloned(),
    }
}

#[test]
fn permission_runtime_decisions_replay() {
    let tool = BashTool::new();
    block_on(async {
        for case in section("decisions", 24) {
            let context = parse_context(field(case, "context"));
            let decision = decide(
                &context,
                &tool,
                field(case, "input"),
                &JsString::from("call-1"),
            )
            .await
            .expect("decision");
            assert_eq!(
                wire(&decision),
                wire(field(case, "expected")),
                "decision {}",
                text(field(case, "name"))
            );
        }
    });
}

// ---- scripted runner -------------------------------------------------------

struct ScriptedRunner {
    scenario: JsValue,
    calls: Mutex<Vec<JsValue>>,
}

impl CommandRunner for ScriptedRunner {
    fn run(&self, command: JsString, options: CommandOptions) -> ToolFuture<CommandResult> {
        self.calls.lock().unwrap().push(JsValue::Object(object([
            // What the OS receives: the UTF-8 form, lone surrogates as U+FFFD.
            (
                "command",
                JsValue::from(String::from_utf16_lossy(command.units()).as_str()),
            ),
            ("cwd", JsValue::String(options.cwd.clone())),
            (
                "envKeys",
                options.env.as_ref().map_or(JsValue::Null, |env| {
                    JsValue::Array(
                        env.iter()
                            .map(|(key, _)| JsValue::String(key.clone()))
                            .collect(),
                    )
                }),
            ),
            ("timeoutMs", JsValue::Number(options.timeout_ms)),
            ("signal", JsValue::Bool(options.signal.is_some())),
            ("onStdout", JsValue::Bool(options.on_stdout.is_some())),
            ("onStderr", JsValue::Bool(options.on_stderr.is_some())),
        ])));
        if let JsValue::Array(chunks) = field(&self.scenario, "chunks") {
            for chunk in chunks {
                let items = array(chunk);
                let callback = if text(&items[0]) == "stdout" {
                    &options.on_stdout
                } else {
                    &options.on_stderr
                };
                if let Some(callback) = callback {
                    callback(&text(&items[1]));
                }
            }
        }
        let scenario = self.scenario.clone();
        Box::pin(async move {
            if let Some(message) = scenario.get("throws") {
                return Err(ToolError::failed(text(message)));
            }
            let result = field(&scenario, "result");
            Ok(CommandResult {
                exit_code: field(result, "exitCode")
                    .as_number()
                    .map(|code| code as i32),
                stdout: text(field(result, "stdout")),
                stderr: text(field(result, "stderr")),
                timed_out: field(result, "timedOut").as_bool().unwrap(),
                duration_ms: 5.0,
                pid: None,
            })
        })
    }
}

#[test]
fn scripted_runner_scenarios_replay() {
    block_on(async {
        for scenario in section("scripted", 118) {
            let name = text(field(scenario, "name"));
            let runner = Arc::new(ScriptedRunner {
                scenario: scenario.clone(),
                calls: Mutex::new(Vec::new()),
            });
            let options = field(scenario, "options");
            let tool = BashTool::with_options(BashToolOptions {
                runner: Some(runner.clone()),
                default_timeout_ms: field(options, "defaultTimeoutMs").as_number(),
                max_timeout_ms: field(options, "maxTimeoutMs").as_number(),
            });
            let mut fields = object([
                ("cwd", JsValue::from("/ws/project")),
                (
                    "env",
                    JsValue::Object(object([
                        ("PATH", JsValue::from("/usr/bin:/bin")),
                        ("FOO", JsValue::from("1")),
                    ])),
                ),
                ("sessionId", JsValue::from("s1")),
                ("turnId", JsValue::from("t1")),
            ]);
            if field(scenario, "noEnv").as_bool() == Some(true) {
                fields.insert("env".into(), JsValue::Undefined);
            }
            let events = Arc::new(Mutex::new(Vec::new()));
            let progress = (field(scenario, "progress").as_bool() == Some(true)).then(|| {
                let events = events.clone();
                Arc::new(move |event: JsObject| {
                    events.lock().unwrap().push(JsValue::Object(event));
                }) as Arc<dyn Fn(JsObject) + Send + Sync>
            });
            let signal =
                (field(scenario, "signal").as_bool() == Some(true)).then(ToolCancellation::default);
            let context = host_context(fields, signal, progress);
            let result = outcome(
                tool.execute(field(scenario, "input").clone(), context)
                    .await,
            );
            assert_eq!(
                wire(&result),
                wire(field(scenario, "expected")),
                "scripted {name}"
            );
            assert_eq!(
                wire(&JsValue::Array(runner.calls.lock().unwrap().clone())),
                wire(field(scenario, "calls")),
                "scripted {name} runner calls"
            );
            assert_eq!(
                wire(&JsValue::Array(events.lock().unwrap().clone())),
                wire(field(scenario, "events")),
                "scripted {name} progress events"
            );
        }
    });
}

// ---- real processes --------------------------------------------------------

/// Delegates to the real runner and keeps each shell PID so the test can prove
/// its process group is gone.
#[derive(Clone, Default)]
struct RecordingRunner {
    pids: Arc<Mutex<Vec<u32>>>,
}

impl CommandRunner for RecordingRunner {
    fn run(&self, command: JsString, options: CommandOptions) -> ToolFuture<CommandResult> {
        let pids = self.pids.clone();
        Box::pin(async move {
            let result = NodeShellCommandRunner.run(command, options).await?;
            if let Some(pid) = result.pid {
                pids.lock().unwrap().push(pid);
            }
            Ok(result)
        })
    }
}

fn group_alive(pid: u32) -> bool {
    let pid = rustix::process::Pid::from_raw(pid as i32).expect("pid");
    rustix::process::test_kill_process_group(pid).is_ok()
}

async fn assert_group_gone(pid: u32, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while group_alive(pid) {
        assert!(
            Instant::now() < deadline,
            "{label}: process group {pid} still exists"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// `value.split(from).join(to)` on UTF-16 units, so lone surrogates in the
/// oracle survive the placeholder substitution.
fn replace_units(value: &JsString, from: &str, to: &str) -> JsString {
    let from: Vec<u16> = from.encode_utf16().collect();
    let to: Vec<u16> = to.encode_utf16().collect();
    let units = value.units();
    let mut out = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        if !from.is_empty() && units[index..].starts_with(&from) {
            out.extend_from_slice(&to);
            index += from.len();
        } else {
            out.push(units[index]);
            index += 1;
        }
    }
    JsString::from_utf16(out)
}

fn replace_paths(value: &JsValue, root: &str, scratch: &str) -> JsValue {
    match value {
        JsValue::String(value) => JsValue::String(replace_units(
            &replace_units(value, root, "<root>"),
            scratch,
            "<scratch>",
        )),
        JsValue::Array(items) => JsValue::Array(
            items
                .iter()
                .map(|item| replace_paths(item, root, scratch))
                .collect(),
        ),
        JsValue::Object(entries) => {
            let mut out = JsObject::new();
            for (key, item) in entries.entries() {
                let item = if key.equals_str("durationMs") {
                    JsValue::from("<duration>")
                } else {
                    replace_paths(item, root, scratch)
                };
                out.insert(key.clone(), item);
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

fn substitute(value: &JsValue, root: &str) -> JsValue {
    match value {
        JsValue::String(value) => JsValue::String(replace_units(value, "<root>", root)),
        JsValue::Array(items) => {
            JsValue::Array(items.iter().map(|item| substitute(item, root)).collect())
        }
        JsValue::Object(entries) => {
            let mut out = JsObject::new();
            for (key, item) in entries.entries() {
                out.insert(key.clone(), substitute(item, root));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

/// The corpus is recorded on macOS. Scenarios marked `darwinOnly` depend on
/// the macOS `/bin/bash` 3.2 (diagnostic wording, job control without a tty)
/// and are skipped elsewhere; every other scenario replays on every Unix.
const DARWIN_ONLY: [&str; 2] = ["not-found", "cancel-escaped"];

fn replays_here(scenario: &JsValue) -> bool {
    let darwin_only = !field(scenario, "darwinOnly").is_undefined();
    assert_eq!(
        darwin_only,
        DARWIN_ONLY.contains(&text(field(scenario, "name")).as_str()),
        "darwinOnly set must be exactly {DARWIN_ONLY:?}"
    );
    cfg!(target_os = "macos") || !darwin_only
}

struct AllowAll;
impl PermissionDecision for AllowAll {
    fn decide(
        &self,
        _: Arc<dyn ToolDefinition>,
        _: JsValue,
        _: RuntimeContext,
        _: JsString,
    ) -> HostFuture<JsValue> {
        Box::pin(async { Ok(JsValue::Object(object([("type", JsValue::from("allow"))]))) })
    }
}

/// `escaped.pid` is written by the `cancel-escaped` command: a job-control
/// child in its own process group that outlives the shell. Kill that group.
async fn kill_escaped(root: &Path) {
    let Ok(text) = fs::read_to_string(root.join("escaped.pid")) else {
        return;
    };
    let pid: u32 = text.trim().parse().expect("escaped pid");
    assert!(pid > 1);
    let group = rustix::process::Pid::from_raw(pid as i32).unwrap();
    let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    assert_group_gone(pid, "escaped job").await;
}

async fn run_real(scenario: &JsValue, scratch: &Path) {
    let name = text(field(scenario, "name"));
    let scratch_text = scratch.to_string_lossy().into_owned();
    let root = scratch.join(&name);
    fs::create_dir(&root).unwrap();
    fs::write(root.join("file"), "not a directory\n").unwrap();
    fs::set_permissions(root.join("file"), fs::Permissions::from_mode(0o644)).unwrap();
    let root_text = root.to_string_lossy().into_owned();

    let runner = RecordingRunner::default();
    let tool = Arc::new(BashTool::with_options(BashToolOptions {
        runner: Some(Arc::new(runner.clone())),
        ..BashToolOptions::default()
    }));
    let cwd = match field(scenario, "cwd") {
        JsValue::Undefined => JsValue::from(root_text.as_str()),
        value => substitute(value, &root_text),
    };
    let fields = object([
        ("cwd", cwd),
        ("env", substitute(field(scenario, "env"), &root_text)),
        ("sessionId", JsValue::from("s1")),
        ("turnId", JsValue::from("t1")),
    ]);
    let abort_after = field(scenario, "abortAfterMs").as_number();
    let pre_aborted = field(scenario, "preAborted").as_bool() == Some(true);
    let signal = (abort_after.is_some() || pre_aborted).then(ToolCancellation::default);
    if pre_aborted {
        signal.as_ref().unwrap().cancel();
    }
    let streams = Arc::new(Mutex::new((String::new(), String::new(), 0usize)));
    let progress = (field(scenario, "progress").as_bool() == Some(true)).then(|| {
        let streams = streams.clone();
        Arc::new(move |event: JsObject| {
            let event = JsValue::Object(event);
            let metadata = field(&event, "metadata");
            let stream = text(field(metadata, "stream"));
            let chunk = text(field(metadata, "chunk"));
            assert_eq!(
                text(field(&event, "message")),
                format!("{stream}: {} bytes", chunk.encode_utf16().count())
            );
            assert_eq!(
                field(metadata, "byteCount").as_number(),
                Some(chunk.len() as f64)
            );
            let mut streams = streams.lock().unwrap();
            streams.2 += 1;
            if stream == "stdout" {
                streams.0.push_str(&chunk);
            } else {
                streams.1.push_str(&chunk);
            }
        }) as Arc<dyn Fn(JsObject) + Send + Sync>
    });
    let context = host_context(fields, signal.clone(), progress);
    let input = substitute(field(scenario, "input"), &root_text);
    let hash_only = field(scenario, "hashOnly").as_bool() == Some(true);

    let started = Instant::now();
    let canceller = async {
        if let (Some(ms), Some(signal)) = (abort_after, &signal) {
            tokio::time::sleep(Duration::from_millis(ms as u64)).await;
            signal.cancel();
        }
    };
    let (result, preview) = if hash_only {
        // Through the scheduler, so `previewLimit` comes from the production
        // `maxResultBytes` path rather than a re-implementation here.
        let registry = ToolRegistry::default();
        registry.register(tool.clone()).unwrap();
        let runtime = ToolRuntime::new(registry, Arc::new(AllowAll), SchedulerMode::Sequential);
        let (results, ()) = tokio::join!(
            runtime.execute_all(
                vec![ToolCall {
                    id: JsString::from("call-1"),
                    name: JsString::from("bash"),
                    input,
                }],
                ToolInvocationContext {
                    runtime: context.host.clone(),
                    execution: Arc::new(ExecutionContext {
                        fields: JsObject::new(),
                        abort_signal: None,
                    }),
                },
            ),
            canceller
        );
        let result = JsValue::Object(results.unwrap().remove(0));
        assert_eq!(text(field(&result, "type")), "success", "{name}");
        let output = ToolOutput {
            content: array(field(&result, "content")).to_vec(),
            data: Some(field(&result, "data").clone()),
            ..ToolOutput::default()
        };
        (
            Ok(output),
            Some(field(field(&result, "metadata"), "previewLimit").clone()),
        )
    } else {
        let (result, ()) = tokio::join!(tool.execute(input, context), canceller);
        (result, None)
    };
    let wall_ms = started.elapsed().as_millis() as f64;
    kill_escaped(&root).await;
    let pids = runner.pids.lock().unwrap().clone();
    for pid in pids {
        assert_group_gone(pid, &name).await;
    }
    if let Some(min) = field(scenario, "minWallMs").as_number() {
        assert!(wall_ms >= min, "{name}: {wall_ms}ms < {min}ms");
    }
    if let Some(max) = field(scenario, "maxWallMs").as_number() {
        assert!(wall_ms <= max, "{name}: {wall_ms}ms > {max}ms");
    }

    let stdout_length = result
        .as_ref()
        .ok()
        .and_then(|output| output.data.as_ref())
        .map(|data| field(data, "stdout").as_string().unwrap().units().len());
    let actual = replace_paths(&outcome(result), &root_text, &scratch_text);
    let expected = field(scenario, "expected");
    if hash_only {
        let digest = format!("{:x}", Sha256::digest(wire(&actual).as_bytes()));
        assert_eq!(digest, text(field(expected, "sha256")), "{name} sha256");
        assert_eq!(
            stdout_length.map(|length| length as f64),
            field(expected, "stdoutLength").as_number(),
            "{name} stdout length"
        );
        assert_eq!(
            wire(preview.as_ref().unwrap()),
            wire(field(expected, "previewLimit")),
            "{name} previewLimit"
        );
    } else {
        assert_eq!(wire(&actual), wire(expected), "real {name}");
    }
    if let Some(at) = field(scenario, "postCheckAtMs").as_number() {
        let wait = Duration::from_millis(at as u64).saturating_sub(started.elapsed());
        assert!(!wait.is_zero(), "{name}: post check already late");
        tokio::time::sleep(wait).await;
        let exist: Vec<JsValue> = array(field(scenario, "postFiles"))
            .iter()
            .map(|file| JsValue::Bool(root.join(text(file)).exists()))
            .collect();
        assert_eq!(
            wire(&JsValue::Array(exist)),
            wire(field(scenario, "postFilesExist")),
            "{name} files after the deferred SIGKILL"
        );
    }
    if let JsValue::Object(_) = field(scenario, "progressStreams") {
        let streams = streams.lock().unwrap();
        assert!(streams.2 > 0, "{name}: no progress events");
        let expected = field(scenario, "progressStreams");
        assert_eq!(streams.0, text(field(expected, "stdout")), "{name} stdout");
        assert_eq!(streams.1, text(field(expected, "stderr")), "{name} stderr");
    }
}

fn canonical_scratch(scratch: &scratch::Scratch) -> PathBuf {
    fs::canonicalize(scratch.path()).expect("canonical scratch")
}

#[test]
fn real_process_scenarios_replay() {
    let scenarios = section("real", 49);
    let scratch = scratch::Scratch::new("bash-tool");
    let root = canonical_scratch(&scratch);
    let mut ran = 0;
    let mut skipped = Vec::new();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for scenario in scenarios {
                let name = text(field(scenario, "name"));
                if name == "cancel-escaped" {
                    continue;
                }
                if !replays_here(scenario) {
                    skipped.push(name);
                    continue;
                }
                run_real(scenario, &root).await;
                ran += 1;
            }
        });
    if cfg!(target_os = "macos") {
        assert_eq!((ran, skipped.len()), (48, 0));
    } else {
        assert_eq!((ran, skipped), (47, vec!["not-found".to_string()]));
    }
}

/// The 15 s force-resolve path, split out so it runs beside the others.
#[test]
fn real_force_resolve_after_escaped_job_replays() {
    let scenario = section("real", 49)
        .iter()
        .find(|scenario| text(field(scenario, "name")) == "cancel-escaped")
        .expect("cancel-escaped scenario");
    if !replays_here(scenario) {
        return;
    }
    let scratch = scratch::Scratch::new("bash-tool");
    let root = canonical_scratch(&scratch);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(run_real(scenario, &root));
}

/// Rust-only: dropping the run future mid-flight kills the whole group (the
/// port contract forbids detached work), and the leader is reaped.
#[test]
fn dropped_run_kills_process_group() {
    let scratch = scratch::Scratch::new("bash-tool-drop");
    let root = canonical_scratch(&scratch);
    let pid_file = root.join("leader.pid");
    block_on(async {
        let options = CommandOptions {
            cwd: JsString::from(root.to_string_lossy().as_ref()),
            env: Some(vec![(
                JsString::from("PATH"),
                JsString::from("/usr/bin:/bin"),
            )]),
            timeout_ms: 60_000.0,
            signal: None,
            on_stdout: None,
            on_stderr: None,
            max_output_units: None,
        };
        let run = NodeShellCommandRunner.run(
            JsString::from("echo $$ > leader.pid; sleep 30 | sleep 30"),
            options,
        );
        let wait_for_pid = async {
            loop {
                if let Ok(text) = fs::read_to_string(&pid_file)
                    && text.ends_with('\n')
                {
                    return text.trim().parse::<u32>().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let pid = tokio::select! {
            _ = run => panic!("the command must still be running"),
            pid = wait_for_pid => pid,
        };
        // `run` was dropped when select! returned.
        assert_group_gone(pid, "dropped run").await;
    });
}

/// Rust-only: `bash.ts` wraps every progress emission in `try {} catch {}`,
/// so a failing sink never fails the command.
#[test]
fn panicking_progress_sink_does_not_fail_the_run() {
    let scratch = scratch::Scratch::new("bash-tool-sink");
    let root = canonical_scratch(&scratch);
    let calls = Arc::new(Mutex::new(0usize));
    let sink = {
        let calls = calls.clone();
        Arc::new(move |_: JsObject| {
            let count = {
                let mut calls = calls.lock().unwrap();
                *calls += 1;
                *calls
            };
            if count > 0 {
                panic!("sink failure");
            }
        }) as Arc<dyn Fn(JsObject) + Send + Sync>
    };
    let fields = object([
        ("cwd", JsValue::from(root.to_string_lossy().as_ref())),
        (
            "env",
            JsValue::Object(object([("PATH", JsValue::from("/usr/bin:/bin"))])),
        ),
    ]);
    let context = host_context(fields, None, Some(sink));
    let input = JsValue::Object(object([(
        "command",
        JsValue::from("echo out; echo err 1>&2"),
    )]));
    let result = block_on(BashTool::new().execute(input, context)).expect("bash run");
    let data = result.data.expect("data");
    assert_eq!(text(field(&data, "stdout")), "out\n");
    assert_eq!(text(field(&data, "stderr")), "err\n");
    assert!(*calls.lock().unwrap() >= 2);
}

#[test]
fn shell_tools_register_in_catalog() {
    let registry = ToolRegistry::default();
    let registrations = pilotdeck_gateway::runtime::tools::builtin::register_shell_tools(&registry)
        .expect("register shell tools");
    assert_eq!(registrations.len(), 1);
    let names: Vec<String> = registry
        .list()
        .unwrap()
        .iter()
        .map(|tool| text(field(&JsValue::Object(tool.fields().clone()), "name")))
        .collect();
    assert_eq!(names, vec!["bash"]);
}

/// Runs `command` with a per-stream cap and returns the result and the
/// chunks each progress callback saw.
fn run_capped(
    root: &Path,
    command: &str,
    limit: Option<usize>,
) -> (CommandResult, Vec<String>, Vec<String>) {
    let seen = Arc::new(Mutex::new((Vec::new(), Vec::new())));
    let callback = |stdout: bool| {
        let seen = seen.clone();
        Some(
            Arc::new(move |chunk: &str| {
                let mut seen = seen.lock().unwrap();
                let list = if stdout { &mut seen.0 } else { &mut seen.1 };
                list.push(chunk.to_string());
            }) as pilotdeck_gateway::runtime::tools::builtin::bash::OutputCallback,
        )
    };
    let options = CommandOptions {
        cwd: JsString::from(root.to_string_lossy().as_ref()),
        env: Some(vec![(
            JsString::from("PATH"),
            JsString::from("/usr/bin:/bin"),
        )]),
        timeout_ms: 120_000.0,
        signal: None,
        on_stdout: callback(true),
        on_stderr: callback(false),
        max_output_units: limit,
    };
    let result = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let result = NodeShellCommandRunner
                .run(JsString::from(command), options)
                .await
                .expect("capped run");
            assert_group_gone(result.pid.expect("pid"), "capped run").await;
            result
        });
    let seen = seen.lock().unwrap();
    (result, seen.0.clone(), seen.1.clone())
}

/// The baseline's append past V8's string limit throws for that chunk only:
/// the chunk and its progress callback are lost, later chunks that still fit
/// are kept, the child runs to completion (probes/oom-rule.out:
/// `tail: "xyzQ"`, `progressChunksAfterFill: ["xyz","Q"]`). Same shape here
/// with the cap lowered to 10 units: 8 fit, 4 more do not, 1 does.
#[test]
fn stream_cap_drops_only_chunks_that_do_not_fit() {
    let scratch = scratch::Scratch::new("bash-tool-cap");
    let root = canonical_scratch(&scratch);
    let (result, stdout_chunks, stderr_chunks) = run_capped(
        &root,
        "printf 'aaaaaaaa'; sleep 0.3; printf 'bbbb'; sleep 0.3; printf 'c'; \
         printf 'eeeeeeeeeee' 1>&2; sleep 0.3; printf 'E' 1>&2; printf 'é' 1>&2",
        Some(10),
    );
    assert_eq!(result.stdout, "aaaaaaaac");
    assert_eq!(result.stderr, "Eé");
    assert_eq!(result.exit_code, Some(0));
    assert!(!result.timed_out);
    assert_eq!(stdout_chunks, vec!["aaaaaaaa", "c"]);
    assert_eq!(stderr_chunks.concat(), "Eé");
}

/// The real cap (V8 `MAX_STRING_LENGTH`), at the probe's exact sizes: the
/// default runner keeps `fill` zeros plus "xyzQ" and drops the 11-byte chunk.
#[test]
fn stream_cap_defaults_to_v8_max_string_length() {
    let scratch = scratch::Scratch::new("bash-tool-cap-real");
    let root = canonical_scratch(&scratch);
    let fill = 536_870_888 - 8;
    let (result, stdout_chunks, _) = run_capped(
        &root,
        &format!(
            "head -c {fill} /dev/zero; sleep 0.3; printf 'abcdefghijk'; sleep 0.3; \
             printf 'xyz'; sleep 0.3; printf 'Q'"
        ),
        None,
    );
    assert_eq!(result.stdout.len(), 536_870_884);
    assert_eq!(&result.stdout[fill..], "xyzQ");
    assert_eq!(result.exit_code, Some(0));
    let tail: Vec<&String> = stdout_chunks
        .iter()
        .filter(|chunk| !chunk.bytes().all(|byte| byte == 0))
        .collect();
    assert_eq!(tail, vec!["xyz", "Q"]);
}

//! Replay of the real-TS `grep` fixture corpus (A6.5-pr4b).
//!
//! `oracle.json` is recorded by `tests/fixtures/grep-tool/generate.mts` from the
//! fixed baseline's `createGrepTool`, which spawns the ripgrep binary bundled by
//! `@vscode/ripgrep`. The replay therefore needs a ripgrep binary too:
//! `PILOTDECK_RIPGREP_PATH` (what the tool itself honors) or `rg` on `PATH`.
//! Without one the test prints a loud skip; `PDRS_REQUIRE_RG=1` turns that into
//! a failure for parity runs. See `tests/fixtures/grep-tool/README.md`.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/child_signals.rs"]
mod child_signals;
#[path = "common/scratch.rs"]
mod scratch;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use pilotdeck_gateway::{
    host::modules::{
        capability::ToolDefinition,
        tool_context::{HostToolCheckpoint, ToolRuntimeContext},
    },
    runtime::tools::{ExecutableTool, ToolOutput, ToolRunContext, builtin::grep::GrepTool},
};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, UNIX_EPOCH},
};

const ORACLE: &str = include_str!("fixtures/grep-tool/oracle.json");

static SCRATCH_COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Scratch {
    root: PathBuf,
    outside: PathBuf,
}
impl Scratch {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir()).expect("canonical temp");
        let unique = format!(
            "pdrs-grep-tool-{}-{}",
            std::process::id(),
            SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst)
        );
        let root = base.join(&unique);
        let outside = base.join(format!("{unique}-outside"));
        for path in [&root, &outside] {
            match fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    panic!("scratch path already exists: {}", path.display())
                }
                Err(error) => panic!("create {}: {error}", path.display()),
            }
        }
        Self { root, outside }
    }
    fn text(&self, path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let base = fs::canonicalize(std::env::temp_dir()).expect("canonical temp");
        for path in [&self.outside, &self.root] {
            let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            assert!(
                canonical.starts_with(&base),
                "refusing to remove {} outside {}",
                canonical.display(),
                base.display()
            );
            let _ = fs::remove_dir_all(canonical);
        }
    }
}

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("fixture string")
        .to_utf8()
        .expect("fixture UTF-8 text")
}
fn number(value: &JsValue) -> Option<f64> {
    value.as_number()
}
fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(values) => values,
        _ => panic!("fixture array"),
    }
}
fn optional_array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(values) => values,
        _ => &[],
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

fn substitute(value: &JsValue, scratch: &Scratch) -> JsValue {
    match value {
        JsValue::String(value) => {
            let value = value
                .to_utf8()
                .unwrap()
                .replace("<outside>", &scratch.text(&scratch.outside))
                .replace("<root>", &scratch.text(&scratch.root));
            JsValue::from(value.as_str())
        }
        JsValue::Array(values) => JsValue::Array(
            values
                .iter()
                .map(|value| substitute(value, scratch))
                .collect(),
        ),
        JsValue::Object(object) => {
            let mut result = JsObject::new();
            for (key, value) in object.entries() {
                result.insert(key.clone(), substitute(value, scratch));
            }
            JsValue::Object(result)
        }
        other => other.clone(),
    }
}

fn apply_step(step: &JsValue, scratch: &Scratch) -> Result<(), String> {
    let op = text(field(step, "op"));
    let path = field(step, "path")
        .as_string()
        .map(|value| value.to_utf8().unwrap())
        .unwrap_or_default();
    let absolute = scratch.root.join(&path);
    match op.as_str() {
        "file" => {
            if let Some(parent) = absolute.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let content = if let Some(encoded) = field(step, "base64").as_string() {
                BASE64
                    .decode(encoded.to_utf8().unwrap())
                    .map_err(|e| e.to_string())?
            } else if let Some(repeat) = field(step, "repeat").as_object() {
                let repeat = JsValue::Object(repeat.clone());
                let unit = field(&repeat, "unit")
                    .as_string()
                    .ok_or("repeat.unit")?
                    .to_utf8()
                    .unwrap();
                let count = field(&repeat, "count").as_number().ok_or("repeat.count")? as usize;
                unit.repeat(count).into_bytes()
            } else {
                text(field(step, "content")).into_bytes()
            };
            fs::write(&absolute, content).map_err(|e| e.to_string())?;
            if let Some(mtime) = number(field(step, "mtime")) {
                let taken = UNIX_EPOCH + Duration::from_secs_f64(mtime / 1_000.0);
                fs::OpenOptions::new()
                    .write(true)
                    .open(&absolute)
                    .and_then(|file| file.set_modified(taken))
                    .map_err(|e| e.to_string())?;
            }
        }
        "dir" => fs::create_dir_all(&absolute).map_err(|e| e.to_string())?,
        "chmod" => {
            let mode = number(field(step, "mode")).ok_or("chmod mode")? as u32;
            fs::set_permissions(&absolute, fs::Permissions::from_mode(mode))
                .map_err(|e| e.to_string())?;
        }
        other => return Err(format!("unknown setup step {other}")),
    }
    Ok(())
}

fn replay_context(scratch: &Scratch, cwd: &Path) -> ToolRunContext {
    let checkpoint = HostToolCheckpoint::default();
    let permission = object([
        ("mode", JsValue::from("default")),
        ("canPrompt", JsValue::Bool(true)),
        ("additionalWorkingDirectories", JsValue::Array(Vec::new())),
    ]);
    let fields = object([
        ("cwd", JsValue::from(cwd.to_string_lossy().as_ref())),
        ("permissionMode", JsValue::from("default")),
        ("permissionContext", JsValue::Object(permission)),
        (
            "env",
            JsValue::Object(object([
                (
                    "PILOT_HOME",
                    JsValue::from(scratch.root.join("pilot-home").to_string_lossy().as_ref()),
                ),
                ("HOME", JsValue::from(scratch.text(&scratch.root).as_str())),
            ])),
        ),
    ]);
    let host = ToolRuntimeContext {
        fields,
        files: checkpoint.tool_context_state(),
        abort_signal: None,
        model: None,
        elicitation: None,
        user_dialog: None,
        audit_recorder: None,
        file_history: None,
        file_update_notifier: None,
        now: None,
        progress: None,
        subagent: None,
        plan_todo: None,
        goal: None,
        plan_directory: None,
    };
    ToolRunContext::new(Arc::new(host))
}

fn env_context(env: &[(&str, &str)], cwd: &Path) -> ToolRunContext {
    let mut environment = JsObject::new();
    for (key, value) in env {
        environment.insert((*key).into(), JsValue::from(*value));
    }
    let permission = object([
        ("mode", JsValue::from("default")),
        ("canPrompt", JsValue::Bool(true)),
        ("additionalWorkingDirectories", JsValue::Array(Vec::new())),
    ]);
    let fields = object([
        ("cwd", JsValue::from(cwd.to_string_lossy().as_ref())),
        ("permissionMode", JsValue::from("default")),
        ("permissionContext", JsValue::Object(permission)),
        ("env", JsValue::Object(environment)),
    ]);
    let host = ToolRuntimeContext {
        fields,
        files: HostToolCheckpoint::default().tool_context_state(),
        abort_signal: None,
        model: None,
        elicitation: None,
        user_dialog: None,
        audit_recorder: None,
        file_history: None,
        file_update_notifier: None,
        now: None,
        progress: None,
        subagent: None,
        plan_todo: None,
        goal: None,
        plan_directory: None,
    };
    ToolRunContext::new(Arc::new(host))
}

fn futures_block<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn error_value(error: &pilotdeck_gateway::runtime::tools::ToolError) -> JsValue {
    JsValue::Object(object([(
        "error",
        JsValue::Object(object([
            ("code", JsValue::from(error.code.as_str())),
            ("message", JsValue::String(error.message.clone())),
        ])),
    )]))
}

fn output_value(output: ToolOutput) -> JsValue {
    let mut value = object([("content", JsValue::Array(output.content))]);
    if let Some(data) = output.data {
        value.insert("data".into(), data);
    }
    if let Some(metadata) = output.metadata {
        value.insert("metadata".into(), JsValue::Object(metadata));
    }
    JsValue::Object(value)
}

fn normalize(value: &JsValue, scratch: &Scratch) -> String {
    wire(value)
        .replace(&scratch.text(&scratch.outside), "<outside>")
        .replace(&scratch.text(&scratch.root), "<root>")
}

/// The replay guard must use the same resolver as the tool, so a machine with
/// only `resources/PilotDeck/node_modules/.../rg` is not skipped.
fn ripgrep_available() -> bool {
    pilotdeck_gateway::runtime::tools::builtin::grep::ripgrep_available()
}

#[test]
fn grep_outputs_match_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let scenarios = array(field(&oracle, "scenarios"));
    assert_eq!(scenarios.len(), 61, "scenario count");
    if !ripgrep_available() {
        let message = "grep fixture replay skipped: no ripgrep binary \
            (set PILOTDECK_RIPGREP_PATH or install rg)";
        if std::env::var_os("PDRS_REQUIRE_RG").is_some() {
            panic!("{message}");
        }
        eprintln!("{message}");
        return;
    }
    let mut calls = 0;
    for scenario in scenarios {
        let name = text(field(scenario, "name"));
        let scratch = Scratch::new();
        for outside in optional_array(field(scenario, "outside")) {
            let path = text(field(outside, "path"));
            let absolute = scratch.outside.join(path);
            if let Some(parent) = absolute.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&absolute, text(field(outside, "content"))).unwrap();
        }
        let cwd = match field(scenario, "cwd").as_string() {
            Some(cwd) => scratch.root.join(cwd.to_utf8().unwrap()),
            None => scratch.root.clone(),
        };
        let context = replay_context(&scratch, &cwd);
        let calls_value = array(field(scenario, "calls"));
        for step in array(field(scenario, "steps")) {
            if text(field(step, "op")) != "call" {
                apply_step(step, &scratch).unwrap_or_else(|e| panic!("{name}: setup: {e}"));
                continue;
            }
            let index = number(field(step, "index")).expect("call index") as usize;
            let call = &calls_value[index];
            calls += 1;
            let input = substitute(field(call, "input"), &scratch);
            let expected = field(call, "execute");
            if matches!(expected, JsValue::Null) {
                continue;
            }
            let actual = match futures_block(GrepTool::new().execute(input, context.clone())) {
                Ok(output) => output_value(output),
                Err(error) => error_value(&error),
            };
            assert_eq!(
                normalize(&actual, &scratch),
                wire(expected),
                "{name}: execute"
            );
        }
    }
    assert_eq!(calls, 61, "call count");
}

#[test]
fn grep_spawn_resets_inherited_signal_dispositions() {
    child_signals::ChildSignalProbe::new().launch(
        "grep_spawn_resets_inherited_signal_dispositions_worker",
        false,
    );
}

/// Runs under the probe launcher: the parent has SIGINT ignored and SIGTERM
/// blocked. A wrapper "rg" execs the C reporter, so the report reflects the
/// dispositions the tool's spawn actually delivered.
#[test]
#[ignore = "runs under the signal-injecting probe"]
fn grep_spawn_resets_inherited_signal_dispositions_worker() {
    let probe = child_signals::probe_path();
    let scratch = scratch::Scratch::new("grep-signal-reset");
    let report = scratch.path().join("report.txt");
    let wrapper = scratch.path().join("rg-report");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexec '{}' report-file '{}'\n",
            probe.display(),
            report.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();

    let context = env_context(
        &[
            (
                "PILOTDECK_RIPGREP_PATH",
                wrapper.to_str().expect("utf8 wrapper"),
            ),
            ("PATH", "/usr/bin:/bin"),
        ],
        scratch.path(),
    );
    let input = JsValue::Object(object([
        ("pattern", JsValue::from("x")),
        (
            "path",
            JsValue::from(scratch.path().to_string_lossy().as_ref()),
        ),
    ]));
    // Whether the wrapper exits 0 or 14, the report is what this test checks.
    let _ = futures_block(GrepTool::new().execute(input, context));

    let report = fs::read_to_string(&report).expect("signal report");
    assert!(
        report.contains("checked=29 mask_empty=1"),
        "spawn did not clear the inherited mask: {report}"
    );
    for line in report.lines().filter(|line| line.starts_with("signal=")) {
        assert!(
            line.contains("default=1") && line.contains("blocked=0"),
            "spawn did not reset an inherited disposition: {line}"
        );
    }
}

#[test]
fn grep_tool_is_registered_as_read_only_and_concurrency_safe() {
    let empty = JsValue::Object(JsObject::new());
    let tool = GrepTool::new();
    assert_eq!(
        ToolDefinition::is_read_only(&tool, &empty).unwrap(),
        JsValue::Bool(true)
    );
    assert_eq!(
        ToolDefinition::is_concurrency_safe(&tool, &empty).unwrap(),
        JsValue::Bool(true)
    );
    let fields = JsValue::Object(tool.fields().clone());
    assert_eq!(text(field(&fields, "name")), "grep");
    assert_eq!(text(field(&fields, "kind")), "filesystem");
    assert_eq!(
        field(&fields, "maxResultBytes"),
        &JsValue::Number(200_000.0)
    );
    let aliases = array(field(&fields, "aliases"));
    assert_eq!(text(&aliases[0]), "Grep");
    // The schema is part of the model contract; parse it to show it stays JSON.
    let schema = field(&fields, "inputSchema");
    assert!(parse_json(&stringify_json(schema).unwrap().unwrap()).is_ok());
}

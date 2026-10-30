//! Replay of the real-TS `grep` fixture corpus (A6.5-pr4b).
//!
//! `oracle.json` is recorded by `tests/fixtures/grep-tool/generate.mts` from the
//! fixed baseline's `createGrepTool`, which spawns the ripgrep binary bundled by
//! `@vscode/ripgrep`. The product resolver is bundled-only, exactly like TS
//! (`filesystem/ripgrep.ts:89-103`): `PILOTDECK_RIPGREP_PATH` is the only
//! override. The replay therefore resolves a bundled engine directly, or — on a
//! machine without the baseline module tree — finds a test-only system `rg` and
//! feeds it through that override. Without either the replay fails; only an
//! explicit `PDRS_ALLOW_NO_RG=1` turns it into a loud skip (CI never sets it).
//! See `tests/fixtures/grep-tool/README.md`.
#[path = "common/oracle_replay.rs"]
mod oracle_replay;
use oracle_replay::*;

#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/child_signals.rs"]
mod child_signals;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::{
    host::modules::{
        capability::ToolDefinition,
        tool_context::{HostToolCheckpoint, ToolRuntimeContext},
    },
    runtime::tools::{ExecutableTool, ToolRunContext, builtin::grep::GrepTool},
};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

const ORACLE: &str = include_str!("fixtures/grep-tool/oracle.json");

fn replay_context(scratch: &Scratch, cwd: &Path, ripgrep: Option<&Path>) -> ToolRunContext {
    let checkpoint = HostToolCheckpoint::default();
    let permission = object([
        ("mode", JsValue::from("default")),
        ("canPrompt", JsValue::Bool(true)),
        ("additionalWorkingDirectories", JsValue::Array(Vec::new())),
    ]);
    let mut environment = object([
        (
            "PILOT_HOME",
            JsValue::from(scratch.root.join("pilot-home").to_string_lossy().as_ref()),
        ),
        ("HOME", JsValue::from(scratch.text(&scratch.root).as_str())),
    ]);
    if let Some(ripgrep) = ripgrep {
        environment.insert(
            "PILOTDECK_RIPGREP_PATH".into(),
            JsValue::from(ripgrep.to_string_lossy().as_ref()),
        );
    }
    let fields = object([
        ("cwd", JsValue::from(cwd.to_string_lossy().as_ref())),
        ("permissionMode", JsValue::from("default")),
        ("permissionContext", JsValue::Object(permission)),
        ("env", JsValue::Object(environment)),
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

/// A missing engine fails the parity replay by default, so a machine without
/// `rg` can never report the 63 comparisons as passed. Skipping needs an
/// explicit opt-out (`PDRS_ALLOW_NO_RG=1`), which CI never sets.
fn ripgrep_skip_allowed() -> bool {
    std::env::var_os("PDRS_ALLOW_NO_RG").is_some_and(|value| {
        let value = value.to_string_lossy();
        !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
    })
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

/// Test-only engine lookup. The product resolver is bundled-only (the TS
/// contract), so a parity run on a machine without the baseline module tree
/// finds `rg` here and passes it through `PILOTDECK_RIPGREP_PATH`.
fn parity_ripgrep_override() -> Option<PathBuf> {
    if pilotdeck_gateway::runtime::tools::builtin::grep::ripgrep_available() {
        // The override from the process env or the bundled package already
        // resolves; leave the product path untouched.
        return None;
    }
    let paths = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&paths) {
        for name in ["rg", "rg.exe"] {
            let candidate = directory.join(name);
            if executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[test]
fn grep_outputs_match_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let scenarios = array(field(&oracle, "scenarios"));
    assert_eq!(scenarios.len(), 63, "scenario count");
    let ripgrep = parity_ripgrep_override();
    if ripgrep.is_none() && !pilotdeck_gateway::runtime::tools::builtin::grep::ripgrep_available() {
        let message = "grep fixture replay cannot run: no ripgrep binary \
            (set PILOTDECK_RIPGREP_PATH or install rg; PDRS_ALLOW_NO_RG=1 skips explicitly)";
        if !ripgrep_skip_allowed() {
            panic!("{message}");
        }
        eprintln!("SKIPPED (PDRS_ALLOW_NO_RG): {message}");
        return;
    }
    let mut calls = 0;
    let mut compared = 0;
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
        let context = replay_context(&scratch, &cwd, ripgrep.as_deref());
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
            let Some(expected) = expected_execute(call) else {
                continue;
            };
            compared += 1;
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
    assert_eq!(calls, 63, "call count");
    assert_eq!(compared, 63, "compared call count");
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
    scratch::write_executable(
        &wrapper,
        &format!(
            "#!/bin/sh\nexec '{}' report-file '{}'\n",
            probe.display(),
            report.display()
        ),
        0o755,
    )
    .expect("#1125 publish wrapper by helper");

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

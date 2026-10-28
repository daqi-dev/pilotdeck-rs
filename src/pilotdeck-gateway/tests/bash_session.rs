//! Replay the real-TS bash-session oracle (#552, the A6.5-BASH-OS-001 setsid
//! half): what the shell spawned by `NodeShellCommandRunner` reports about
//! its own session, process group, controlling terminal and `/dev/tty`,
//! recorded with the runner's parent demonstrably holding a controlling
//! terminal (`script -q /dev/null`; see fixtures/bash-session/README.md).
//! Plus a direct OS assertion that the shell leads a new session.
#![cfg(unix)]
// The real-TS replay below is macOS-only (the oracle is recorded on darwin); on other
// Unix hosts only `detached_shell_leads_its_own_session` runs, so its replay helpers
// are unused there (Linux CI lint, PR #573 codex r3).
#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_imports))]
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/node_provenance.rs"]
mod node_provenance;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::runtime::tools::builtin::bash::{
    CommandOptions, CommandRunner, NodeShellCommandRunner,
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

const ORACLE: &str = include_str!("fixtures/bash-session/oracle.json");

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

fn options(cwd: &Path) -> CommandOptions {
    CommandOptions {
        cwd: JsString::from(cwd.to_string_lossy().as_ref()),
        env: Some(vec![(
            JsString::from("PATH"),
            JsString::from("/usr/bin:/bin"),
        )]),
        timeout_ms: 60_000.0,
        signal: None,
        on_stdout: None,
        on_stderr: None,
        max_output_units: None,
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn group_alive(pid: u32) -> bool {
    let pid = rustix::process::Pid::from_raw(pid as i32).expect("pid");
    rustix::process::test_kill_process_group(pid).is_ok()
}

/// Every member of process group `pid` that can be enumerated, with its state:
/// the failure diagnostic for a group that outlived its deadline, so a
/// zombie-only group (an unreaped leader — Linux keeps zombies in their
/// process group; XNU removes them at exit) is distinguishable from a live
/// member the reclamation signal never reached. Linux reads
/// `/proc/<pid>/stat` (state and pgrp fields); other Unixes enumerate with
/// `ps -o pid,pgid,stat`.
fn group_members(pid: u32) -> String {
    let pid_text = pid.to_string();
    let mut members = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(member) = name
                .to_str()
                .filter(|name| !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()))
            else {
                continue;
            };
            let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            // `<pid> (<comm>) <state> <ppid> <pgrp> ...`: comm may contain
            // spaces and parens, so parse after the last ')'.
            let Some(rest) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
                continue;
            };
            let fields: Vec<&str> = rest.split_whitespace().collect();
            if fields.len() >= 3 && fields[2] == pid_text {
                members.push(format!(
                    "pid {member} state {} pgrp {}",
                    fields[0], fields[2]
                ));
            }
        }
    } else if let Ok(output) = std::process::Command::new("ps")
        .args(["-eo", "pid=,pgid=,stat=,comm="])
        .output()
        && output.status.success()
    {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() >= 3 && fields[1] == pid_text {
                members.push(fields.join(" "));
            }
        }
    }
    if members.is_empty() {
        "no members enumerated (the group id may already be stale)".to_owned()
    } else {
        members.join("\n")
    }
}

/// The recording ran with a parent holding a controlling terminal, and the
/// TS `detached` child still reported no session terminal of its own.
#[test]
#[cfg(target_os = "macos")]
fn detached_shell_session_matches_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    assert_eq!(text(field(&oracle, "platform")), "darwin");
    assert_eq!(text(field(&oracle, "parentDevTty")), "open");
    let cases = array(field(&oracle, "cases"));
    assert_eq!(cases.len(), 4, "session, pgroup, ctty and devtty probes");
    for case in cases {
        let name = text(field(case, "name"));
        let scratch = scratch::Scratch::new(&format!("bash-session-{name}"));
        let result = block_on(async {
            NodeShellCommandRunner
                .run(
                    JsString::from(text(field(case, "command"))),
                    options(scratch.path()),
                )
                .await
                .expect("session probe run")
        });
        // Only reproducible fields are in the oracle (no durationMs).
        assert_eq!(result.exit_code, number(field(case, "exitCode")), "{name}");
        assert_eq!(result.stdout, text(field(case, "stdout")), "{name}");
        assert_eq!(result.stderr, text(field(case, "stderr")), "{name}");
        assert_eq!(
            result.timed_out,
            field(case, "timedOut").as_bool().expect("timedOut boolean"),
            "{name}"
        );
    }
}

fn number(value: &JsValue) -> Option<i32> {
    value.as_number().map(|code| code as i32)
}

/// `getsid(shell) == shell pid`: `setsid()` between fork and exec, so the
/// shell leads a session of its own — independent of whether this test
/// process itself has a controlling terminal.
#[test]
fn detached_shell_leads_its_own_session() {
    let scratch = scratch::Scratch::new("bash-session-direct");
    let root = scratch.path().canonicalize().unwrap();
    let pid_file = root.join("leader.pid");
    block_on(async {
        let mut run = NodeShellCommandRunner.run(
            JsString::from("echo $$ > leader.pid; sleep 30 | sleep 30"),
            options(&root),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        let pid = loop {
            assert!(Instant::now() < deadline, "no leader pid file appeared");
            tokio::select! {
                status = &mut run => panic!("the command must still be running: {status:?}"),
                _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
            if let Ok(text) = fs::read_to_string(&pid_file)
                && text.ends_with('\n')
            {
                break text.trim().parse::<u32>().unwrap();
            }
        };
        // The run is still alive here (not dropped), so the leader is not
        // racing its own termination.
        let leader = rustix::process::Pid::from_raw(pid as i32).expect("pid");
        assert_eq!(
            rustix::process::getsid(Some(leader)).expect("getsid"),
            leader,
            "setsid: the shell leads its own session"
        );
        assert_ne!(
            rustix::process::getsid(Some(leader)).expect("getsid"),
            rustix::process::getsid(None).expect("getsid"),
            "the shell's session must differ from ours"
        );
        // Dropping the run kills the group; the session change must not
        // disturb that reclamation.
        drop(run);
    });
    let pid = fs::read_to_string(&pid_file)
        .expect("leader pid")
        .trim()
        .parse::<u32>()
        .expect("pid number");
    // The dropped run has SIGKILLed the group, but the leader itself stays a
    // zombie until its parent reaps it — and Linux keeps an unreaped zombie in
    // its process group, so `kill(-pgid, 0)` would keep succeeding there
    // (XNU refuses a zombie-only group, which is why this test passed on
    // macOS unchanged). Nothing else reaps it here: the runtime that parks
    // and drains tokio's orphan queue died with `block_on`, so this process
    // — the leader's parent — observes the kill itself by reaping, with the
    // same deadline the group wait below uses.
    let leader = rustix::process::Pid::from_raw(pid as i32).expect("pid");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match rustix::process::waitid(
            rustix::process::WaitId::Pid(leader),
            rustix::process::WaitIdOptions::EXITED | rustix::process::WaitIdOptions::NOHANG,
        ) {
            // Reaped here — or ECHILD: someone (the orphan queue) won the race.
            Ok(Some(_)) | Err(rustix::io::Errno::CHILD) => break,
            Ok(None) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => panic!("waitid({pid}): {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "the dropped run must have killed the shell\n{}",
            group_members(pid)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while group_alive(pid) {
        assert!(
            Instant::now() < deadline,
            "process group {pid} still exists\n{}",
            group_members(pid)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The committed oracle is exactly what the real TS baseline produces.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules), Node v22.22.0, darwin and script(1)"]
#[cfg(target_os = "macos")]
fn fixture_regenerates_identically_from_the_baseline() {
    let _ = node_provenance::pin(baseline::node_command());
    baseline::regenerates_identically("bash-session");
}

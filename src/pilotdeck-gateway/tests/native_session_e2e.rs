//! Offline #1140 fix-r1 end-to-end controls: the collector's own output, scored.
//!
//! #1408: the fixture is a Python child, so `Command::output()` parked the test
//! binary for as long as the interpreter made no progress, with no deadline and
//! no diagnostics. This is the fixture that reaches the collector's *longest*
//! honest path — `_collect` runs `_native_run(..., session_domain=True)` under
//! `SESSION_LIMIT_SECONDS = 900` (`scripts/native-witness/collect.py`) and then
//! spends up to 30 s in `gateway.close(30)` (`test_e2e.py`) — so it is exactly
//! the one that needs the shared backstop. Measured worst case with a peer that
//! never replies: 930.1 s, reported as `TimeoutError: timed out`
//! (`_tmp/20261107-issue-1408/03-wedged-peer-shipped900.log`).
use std::{path::PathBuf, process::Command};

#[allow(dead_code)]
#[path = "common/baseline.rs"]
mod baseline;

#[test]
fn native_session_e2e_offline_controls() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/native-session-e2e/test_e2e.py");
    let mut command = Command::new("python3");
    command.arg("-I").arg(script);
    let (status, stdout, stderr) = baseline::run_generator_with_deadline(
        &mut command,
        "native-session-e2e",
        baseline::native_collector_backstop(),
    )
    .unwrap_or_else(|error| panic!("#1408 launch isolated Python e2e session controls: {error}"));
    assert!(
        status.success(),
        "#1140 fix-r1 offline e2e session controls failed\n{}\n{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
}

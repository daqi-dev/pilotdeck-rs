//! Offline #1140 trace/audit controls; live Linux collection is a separate command.
//!
//! #1408: the fixtures are Python children, so `Command::output()` parked the
//! test binary for as long as an interpreter made no progress, with no deadline
//! and no diagnostics. Two of the four scripts drive the native collector on its
//! longest honest path — `test_r11.py` calls `_collect(real_settle=True)` and
//! `_native_run(..., session_domain=True)`, which is bounded at
//! `SESSION_LIMIT_SECONDS = 900` plus 30 s of `gateway.close` — so they get the
//! same shared backstop as `native_stimulus_plan.rs` / `native_session_e2e.rs`.
use std::{path::PathBuf, process::Command};

#[allow(dead_code)]
#[path = "common/baseline.rs"]
mod baseline;

#[test]
fn native_witness_offline_controls() {
    for filename in [
        "test_witness.py",
        "test_authentication.py",
        "test_chain.py",
        "test_r11.py",
    ] {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/native-witness")
            .join(filename);
        let mut command = Command::new("python3");
        command.arg("-I").arg(script);
        let (status, stdout, stderr) = baseline::run_generator_with_deadline(
            &mut command,
            "native-witness",
            baseline::native_collector_backstop(),
        )
        .unwrap_or_else(|error| {
            panic!("#1408 launch isolated Python witness controls ({filename}): {error}")
        });
        assert!(
            status.success(),
            "#1140 offline witness controls failed ({filename})\n{}\n{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
    }
}

//! Offline #1140 fix-r1 end-to-end controls: the collector's own output, scored.
use std::{path::PathBuf, process::Command};

#[test]
fn native_session_e2e_offline_controls() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/native-session-e2e/test_e2e.py");
    let output = Command::new("python3")
        .arg("-I")
        .arg(script)
        .output()
        .expect("#1140 fix-r1 launch isolated Python e2e session controls");
    assert!(
        output.status.success(),
        "#1140 fix-r1 offline e2e session controls failed\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

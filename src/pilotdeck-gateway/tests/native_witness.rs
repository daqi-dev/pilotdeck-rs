//! Offline #1140 trace/audit controls; live Linux collection is a separate command.
use std::{path::PathBuf, process::Command};

#[test]
fn native_witness_offline_controls() {
    for filename in ["test_witness.py", "test_authentication.py", "test_chain.py"] {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/native-witness")
            .join(filename);
        let output = Command::new("python3")
            .arg("-I")
            .arg(script)
            .output()
            .expect("#1140 launch isolated Python witness controls");
        assert!(
            output.status.success(),
            "#1140 offline witness controls failed\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

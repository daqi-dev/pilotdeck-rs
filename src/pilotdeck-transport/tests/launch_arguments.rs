use std::process::Command;

#[test]
fn empty_positional_argument_exits_with_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_pilotdeck-agent-loop-sidecar"))
        .arg("")
        .output()
        .expect("sidecar should launch");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("Usage:"));
}

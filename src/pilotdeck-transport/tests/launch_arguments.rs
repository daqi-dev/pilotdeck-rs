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

#[cfg(unix)]
#[test]
fn non_utf8_stimulus_is_rejected_before_serving() {
    use std::os::unix::ffi::OsStringExt;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_pilotdeck-agent-loop-sidecar"))
        .env_clear()
        .env("PILOTDECK_NATIVE_LOOP_STIMULUS", std::ffi::OsString::from_vec(vec![0xff]))
        .stdin(std::process::Stdio::null())
        .output()
        .expect("spawn isolated sidecar");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("native loop stimulus must be valid UTF-8"));
    assert!(output.stdout.is_empty());
}

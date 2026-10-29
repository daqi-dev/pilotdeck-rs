//! CI preflight for fixture mutation anchors; never launches mutation runners.
use std::path::Path;
use std::process::Command;

fn python_output(mut command: Command, script: &Path, args: &[&str]) -> Result<String, String> {
    let output = command
        .arg("-I")
        .arg("-B")
        .arg(script)
        .args(args)
        .output()
        .map_err(|error| {
            format!("negative anchor selfcheck requires python3; could not start it: {error}")
        })?;
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err(format!("{}: {}\n{report}", script.display(), output.status));
    }
    Ok(report)
}

#[test]
fn negative_fixture_anchors_and_checker_controls() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (script, args) in [
        ("test_anchor_selfcheck.py", &[][..]),
        ("negative-restore/test_restore.py", &[][..]),
        (
            "negative-restore/test_restore.py",
            &["--negative-controls"][..],
        ),
        // CI prepares the TS baseline after cargo test. This explicit mode still
        // lists missing launcher prerequisites; local invocation is strict.
        ("anchor_selfcheck.py", &["--anchors-only"][..]),
    ] {
        let report = python_output(Command::new("python3"), &fixtures.join(script), args)
            .unwrap_or_else(|error| panic!("{error}"));
        print!("{report}");
    }
}

#[test]
fn missing_python_is_an_error() {
    // A nonexistent child of a real source file cannot resolve to an executable.
    let missing =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/negative_anchor_selfcheck.rs/python3");
    let error = python_output(Command::new(missing), Path::new("unused.py"), &[]).unwrap_err();
    assert!(error.contains("requires python3"), "{error}");
}

#[test]
fn checker_failure_is_an_error() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/anchor_selfcheck.py");
    let error = python_output(
        Command::new("python3"),
        &script,
        &["--unknown-selfcheck-option"],
    )
    .unwrap_err();
    assert!(error.contains("unrecognized arguments"), "{error}");
}

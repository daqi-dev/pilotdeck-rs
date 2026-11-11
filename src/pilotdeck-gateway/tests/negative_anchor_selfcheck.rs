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
        (
            "test_negative_process.py",
            &[
                // ProcessControls 持 wait_for_slot / run_owned 的契约测试（含
                // cargo-slots.sh 退出码语义）；不列进来等于这些测试在 CI 里从没跑过。
                "ProcessControls",
                "OutputDirectoryGuard",
                "OutputDirectoryClaim",
                "OutputDirectoryRunnerTests",
                "TreeLockBudget",
            ][..],
        ),
        ("_negkit/test_compare_verdicts.py", &[][..]),
        ("_negkit/test_contract.py", &[][..]),
        ("_negkit/negative_contract.py", &[][..]),
        ("_negkit/test_submit_migration.py", &[][..]),
        ("_negkit/negative_submit_migration.py", &[][..]),
        // #1540：non_head 契约单测与配套变异负控；不列进来等于这两个测试在
        // CI 里从没跑过（评审 r1 sibling#2 / concurrency#5）。
        ("_negkit/test_non_head.py", &[][..]),
        ("_negkit/negative_non_head.py", &[][..]),
        // #1540 r3：共享件 claim_dir/select/registered 的自测此前不在任何
        // CI 入口（评审 r3 sibling#4）；-I 下自举导入路径后可直接跑。
        ("test_negative_selection.py", &[][..]),
        ("negative-restore/test_restore.py", &[][..]),
        (
            "negative-restore/test_restore.py",
            &["--negative-controls"][..],
        ),
        ("pending-reanchor/test_controls.py", &[][..]),
        // #1695 fix-r1（评审 sibling#2 / simplify#9）：#1713 判决链自测 +
        // #1695 handoff / #1705 kill-tree 行为自测此前不在任何闸门里
        // （评审原话：不列进来等于这些测试在 CI 里从没跑过）；正跑走 12 个
        // 用例，--negative 走 21 个内存变异体（negative-restore 的两档先例）。
        ("user-dialog-http-store-1555/test_controls.py", &[][..]),
        (
            "user-dialog-http-store-1555/test_controls.py",
            &["--negative"][..],
        ),
        // CI prepares the TS baseline after cargo test. This explicit mode still
        // lists missing launcher prerequisites; local invocation is strict.
        ("anchor_selfcheck.py", &["--anchors-only"][..]),
    ] {
        let report = python_output(Command::new("python3"), &fixtures.join(script), args)
            .unwrap_or_else(|error| panic!("#1110 fixture preflight: {error}"));
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

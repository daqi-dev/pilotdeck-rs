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
                // #2513 fix-r10（DS boundary#1）：三个机检持有人清单类不在任何 CI
                // 入口里，等于「第七个族 fail-closed」这句自述在 CI 从没跑过。
                // 不列进来等于这些测试在 CI 里从没跑过（本文件自己的注释点名
                // 的失败形态）。
                "ParityGuardedPredicate",
                "BaselineTracksRuler",
                "ParityLockHolderReconciliation",
                // #2513 fix-r11（DS r10 boundary#1 / concurrency#1，a 档当轮修）：
                // DeadStreamDetach / DeadStreamPrint / RunTokenAndRefuseSummary
                // 19 条测试此前不在任何 CI 入口（全仓 grep 只命中定义处）。
                // 本机纯 python 冒烟 19 tests OK 0.5 s，无宿主依赖，无超时顾虑。
                "DeadStreamDetach",
                "DeadStreamPrint",
                "RunTokenAndRefuseSummary",
                // #2513 fix-r12 (DS r11 sibling#5)：另 5 个类此前不在任何 CI 入口。
                // 本机纯 python 实测各类耗时（总 ~4.4 s，最大单类 1.85 s，均无
                // 宿主依赖）：NodeVersionProbe 0.09 s / MutatingSources 1.85 s /
                // ParityMutationLock 1.67 s / RestoreWindowSignals 0.66 s /
                // TerminateHandlerRegistration 0.11 s，远低于 60 s 线。
                // 另 fix-r12 (DS r11 boundary#2)：RunTokenAndRefuseSummary
                // 里 3 条 chmod(0o555) 孪生改为调用点注入（write_text/open/
                // unlink 打桩），消除 root / overlay TMPDIR 下的宿主依赖。
                "NodeVersionProbe",
                "MutatingSources",
                "ParityMutationLock",
                "RestoreWindowSignals",
                "TerminateHandlerRegistration",
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

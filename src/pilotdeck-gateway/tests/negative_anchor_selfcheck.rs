//! CI preflight for fixture mutation anchors; never launches mutation runners.
use std::path::Path;
use std::process::Command;

/// `test_negative_process.py` class selectors exercised by this preflight.
///
/// Contract tests of the process/parity guard surface. A selector that names a
/// class with zero `test_*` methods is silently green on every Python version
/// (measured 3.9.6 and 3.14.3: `Ran N tests ... OK`, rc 0, the class absent),
/// so the report must be asserted, not the exit code.
const NEGATIVE_PROCESS_CLASSES: &[&str] = &[
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
];

/// Every selector must have executed at least one test of its own class.
///
/// With `verbosity=2` unittest prints one `test_x (__main__.Class.test_x)`
/// line per executed method, so a class's absence from the report means its
/// selector matched zero tests — the version-independent hole this closes.
/// A `... skipped` or `... expected failure` line is not an execution
/// (#2513 fix-r14, DS r13 review): unittest counts those as ran and the
/// report still ends `OK`, so only a line in an executed status counts.
fn selector_coverage(report: &str, classes: &[&str]) -> Result<(), String> {
    for class in classes {
        let prefix = format!("__main__.{class}.");
        let executed = report
            .lines()
            .filter(|line| line.contains(&prefix))
            .any(|line| !line.contains("... skipped") && !line.contains("... expected failure"));
        if !executed {
            return Err(format!(
                "selector {class} ran zero tests of its own (the class is absent, \
                 or every line was skipped or expected failure)"
            ));
        }
    }
    if report.contains("Ran 0 tests") || report.contains("NO TESTS RAN") {
        return Err("the run executed zero tests".to_string());
    }
    Ok(())
}

fn assert_selector_coverage(report: &str, classes: &[&str]) {
    if let Err(error) = selector_coverage(report, classes) {
        panic!("#2513 fix-r13 anchor preflight: {error}");
    }
}

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
        ("test_negative_process.py", NEGATIVE_PROCESS_CLASSES),
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
        if script == "test_negative_process.py" {
            // #2513 fix-r13: the class-selector run must actually execute every
            // selector's tests; a zero-method class is silently green (rc 0)
            // on every measured Python version, so assert the report text.
            assert_selector_coverage(&report, NEGATIVE_PROCESS_CLASSES);
        }
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

#[test]
fn zero_method_selector_is_rejected() {
    // Captured shape of `python3 -B probe.py HasTests ZeroCase` (3.14.3; 3.9.6
    // measured the same silent shape): rc 0, `Ran 1 test ... OK`, the zero-method
    // class absent from the report. Exit codes alone cannot prove selector
    // coverage, so this must be red.
    let mixed = "test_it (__main__.HasTests.test_it) ... ok\n\
                 \n\
                 ----------------------------------------------------------------------\n\
                 Ran 1 test in 0.000s\n\
                 \n\
                 OK\n";
    let error = selector_coverage(mixed, &["HasTests", "ZeroCase"]).unwrap_err();
    assert!(error.contains("ZeroCase"), "{error}");
    assert!(selector_coverage(mixed, &["HasTests"]).is_ok());
}

#[test]
fn zero_test_run_is_rejected() {
    // Captured shape of an all-zero-method selection (3.14.3): rc 5 with
    // `Ran 0 tests` / `NO TESTS RAN`; on <=3.11 the same run is rc 0 with
    // `Ran 0 tests ... OK`. Both textual shapes must be rejected regardless
    // of the exit code.
    let empty = "----------------------------------------------------------------------\n\
                 Ran 0 tests in 0.000s\n\
                 \n\
                 NO TESTS RAN\n";
    let error = selector_coverage(empty, &["HasTests"]).unwrap_err();
    assert!(error.contains("zero tests"), "{error}");
    // The Ran-0 branch below the loop is only reachable with an empty
    // selector list — every non-empty list fails earlier on its first absent
    // class — so pin it directly (#2513 fix-r14 simplify#8, choice recorded
    // in the round RESULT: keep the branch, add the direct call).
    let error = selector_coverage(empty, &[]).unwrap_err();
    assert!(error.contains("zero tests"), "{error}");
}

#[test]
fn skipped_only_selector_is_rejected() {
    // #2513 fix-r14 (DS r13 review): a selector whose every line is skipped
    // or expected-failure has executed nothing of its own — unittest still
    // reports `Ran N tests ... OK`, so those lines must not count as
    // coverage.
    let skipped = "test_one (__main__.Alpha.test_one) ... skipped 'reason'\n\
                   test_two (__main__.Alpha.test_two) ... expected failure\n\
                   ----------------------------------------------------------------------\n\
                   Ran 2 tests in 0.001s (1 skipped, 1 expected failure)\n\
                   \n\
                   OK\n";
    let error = selector_coverage(skipped, &["Alpha"]).unwrap_err();
    assert!(error.contains("Alpha"), "{error}");
    assert!(error.contains("skipped"), "{error}");
    // A mixed report still covers the class that really executed.
    let mixed = "test_one (__main__.Alpha.test_one) ... ok\n\
                 test_two (__main__.Alpha.test_two) ... skipped 'reason'\n\
                 ----------------------------------------------------------------------\n\
                 Ran 2 tests in 0.001s (1 skipped)\n\
                 \n\
                 OK\n";
    assert!(selector_coverage(mixed, &["Alpha"]).is_ok());
    assert!(selector_coverage(mixed, &["Beta"]).is_err());
}

#[test]
fn covered_selectors_pass() {
    let covered = "test_one (__main__.Alpha.test_one) ... ok\n\
                   test_two (__main__.Beta.test_two) ... ok\n\
                   ----------------------------------------------------------------------\n\
                   Ran 2 tests in 0.001s\n\
                   \n\
                   OK\n";
    assert!(selector_coverage(covered, &["Alpha", "Beta"]).is_ok());
    assert!(selector_coverage(covered, NEGATIVE_PROCESS_CLASSES).is_err());
}

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

/// The status one report line carries, if any. Unittest writes the id, then
/// `...`, then the status only when the method finishes, so a child process
/// printing in between splits them; a docstring inserts its own line between
/// them (#2513 fix-r15, DS r14 boundary#5). The status is the text after the
/// last `...` of a carrier line, or the whole line when it has none. Measured
/// `python3 -I -B probe.py -v` shapes (CPython 3.12, this round's
/// task9/probe-verbose-docstring.log plus the real ClaimDir entry below):
///
/// ```text
/// test_plain_skipped (__main__.Alpha.test_plain_skipped) ... skipped 'other'
/// test_ok_doc (__main__.Alpha.test_ok_doc)
/// Executed docstring. ... ok
/// test_existing_directory_is_a_refusal (__main__.ClaimDir....) ... INVALID: output directory already exists: /tmp/.../out
/// ok
/// ```
///
/// Anything that is not a known status is `None` — fail closed. `ok`, `FAIL`,
/// `ERROR` and `unexpected success` mean the method ran; `skipped` and
/// `expected failure` do not (#2513 fix-r14, DS r13 review: unittest counts
/// those as ran and the report still ends `OK`).
fn line_status(line: &str) -> Option<&'static str> {
    let text = match line.rfind("...") {
        Some(dot) => line[dot + 3..].trim(),
        None => line.trim(),
    };
    if text.is_empty() {
        return None;
    }
    for executed in ["ok", "FAIL", "ERROR", "unexpected success"] {
        if text == executed {
            return Some("executed");
        }
    }
    for not_executed in ["skipped", "expected failure"] {
        // skipped carries a reason: `skipped 'other'`.
        if text == not_executed || text.starts_with(not_executed) {
            return Some(not_executed);
        }
    }
    None
}

/// Whether the report entry starting at `lines[start]` executed: the first
/// status from the id line forward wins (the status is written before the
/// next entry starts), scanning across the docstring line and any child
/// output that interleaved, and failing closed at the summary separator.
fn entry_executed(lines: &[&str], start: usize) -> bool {
    for line in &lines[start..] {
        if line.starts_with("----") || line.trim_start().starts_with("Ran ") {
            return false;
        }
        match line_status(line) {
            Some("skipped") | Some("expected failure") => return false,
            Some(_) => return true,
            None => continue,
        }
    }
    false
}

/// Whether one class name has an executed entry in the report.
fn class_executed(lines: &[&str], class: &str) -> bool {
    let prefix = format!("__main__.{class}.");
    lines
        .iter()
        .enumerate()
        .any(|(index, line)| line.contains(&prefix) && entry_executed(lines, index))
}

/// Every selector must have executed at least one test of its own class.
///
/// With `verbosity=2` unittest prints one `test_x (__main__.Class.test_x)`
/// entry per method, so a class's absence from the report means its
/// selector matched zero tests — the version-independent hole this closes.
fn selector_coverage(report: &str, classes: &[&str]) -> Result<(), String> {
    let lines: Vec<&str> = report.lines().collect();
    for class in classes {
        if !class_executed(&lines, class) {
            return Err(format!(
                "selector {class} ran zero tests of its own (the class is absent, \
                 or every entry was skipped or expected failure)"
            ));
        }
    }
    if report.contains("Ran 0 tests") || report.contains("NO TESTS RAN") {
        return Err("the run executed zero tests".to_string());
    }
    Ok(())
}

/// Whether a declared class is covered: any of its coverage group's names has
/// an executed entry. This is a disjunction, unlike a selector list — a
/// base whose inherited methods run under a subclass's name is covered by
/// that subclass, while `selector_coverage` demands an entry per name.
fn group_executed(report: &str, names: &[&str]) -> bool {
    let lines: Vec<&str> = report.lines().collect();
    names.iter().any(|class| class_executed(&lines, class))
}

/// The `class Name(Base, ...)` declarations at the top of a script.
fn class_declarations(source: &str) -> Vec<(String, Vec<String>)> {
    let mut declarations: Vec<(String, Vec<String>)> = Vec::new();
    for line in source.lines() {
        let Some(rest) = line.strip_prefix("class ") else {
            continue;
        };
        let Some(open) = rest.find('(') else { continue };
        let close = rest[open..].find(')').map(|index| index + open);
        let bases = rest[open + 1..close.unwrap_or(rest.len())]
            .split(',')
            .map(|base| {
                base.trim()
                    .rsplit('.')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string()
            })
            .filter(|base| !base.is_empty())
            .collect();
        declarations.push((rest[..open].to_string(), bases));
    }
    declarations
}

/// The unittest classes a script declares, derived from its own source so
/// the expected coverage cannot drift from a second hand-written roster
/// (#2513 fix-r15, MM r14 sib#3). A class counts when it inherits —
/// directly or through the script's own bases — from a TestCase base, which
/// keeps mutation drivers' helpers (`class BoundResult(unittest.TextTestResult)`)
/// out of the roster.
fn declared_test_classes(declarations: &[(String, Vec<String>)]) -> Vec<String> {
    let mut test_classes: Vec<String> = Vec::new();
    loop {
        let before = test_classes.len();
        for (name, bases) in declarations {
            if test_classes.contains(name) {
                continue;
            }
            if bases.iter().any(|base| {
                base.contains("TestCase") || test_classes.iter().any(|known| known == base)
            }) {
                test_classes.push(name.clone());
            }
        }
        if test_classes.len() == before {
            break;
        }
    }
    test_classes.sort();
    test_classes
}

/// The report names that satisfy one declared class: itself plus, to a fixed
/// point, every declared subclass — inherited methods run under the
/// subclass's name (negative-restore runs `RestoreTests` only through its
/// three subclasses), so a base whose every subclass was hollowed out or
/// skipped must stay red while a live subclass still covers it.
fn coverage_group(declarations: &[(String, Vec<String>)], class: &str) -> Vec<String> {
    let mut group = vec![class.to_string()];
    loop {
        let before = group.len();
        for (name, bases) in declarations {
            if group.contains(name) {
                continue;
            }
            if bases.iter().any(|base| group.contains(base)) {
                group.push(name.clone());
            }
        }
        if group.len() == before {
            break;
        }
    }
    group
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
        // #2513 fix-r15 (MM r14 sib#3): the selector-coverage judgment is no
        // longer only test_negative_process.py's. A whole-file unittest run
        // must execute every class the script itself declares — the roster is
        // derived from the script's source, so a hollowed-out or all-skip
        // class is red here too, not only for the hand-listed selectors.
        let source = std::fs::read_to_string(fixtures.join(script))
            .unwrap_or_else(|error| panic!("#2513 fix-r15 could not read {script}: {error}"));
        let declarations = class_declarations(&source);
        let declared = declared_test_classes(&declarations);
        if args.is_empty() {
            if report.contains("Ran 0 tests") || report.contains("NO TESTS RAN") {
                panic!("#2513 fix-r15 anchor preflight: {script} executed zero tests");
            }
            for class in &declared {
                let group = coverage_group(&declarations, class);
                let names: Vec<&str> = group.iter().map(String::as_str).collect();
                assert!(
                    group_executed(&report, &names),
                    "#2513 fix-r15 anchor preflight: class {class} of {script} ran zero \
                     tests of its own (the class is absent, or every entry was skipped \
                     or expected failure; group {group:?})"
                );
            }
        } else if script == "test_negative_process.py" {
            // #2513 fix-r13: the class-selector run must actually execute every
            // selector's tests; a zero-method class is silently green (rc 0)
            // on every measured Python version, so assert the report text.
            // #2513 fix-r15: every hand-listed selector must also name a class
            // the script declares, so a renamed or typo'd selector cannot hide
            // behind the derived-roster judgment.
            assert!(
                !declared.is_empty(),
                "#2513 fix-r15 derived an empty roster for {script}"
            );
            for class in NEGATIVE_PROCESS_CLASSES {
                assert!(
                    declared.iter().any(|name| name == class),
                    "#2513 fix-r15 selector {class} names no class {script} declares"
                );
            }
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
fn docstring_split_skipped_selector_is_rejected() {
    // #2513 fix-r15 (DS r14 boundary#5): with verbosity=2 a method with a
    // docstring prints its id alone and the docstring's first line carries
    // the status; the pre-fix per-line judgment counted the bare id line as
    // an execution, so an all-skip docstring class stayed green. Every shape
    // below is measured CPython 3.12 output (this round's
    // task9/probe-verbose-docstring.log).
    let skipped = "test_skipped_doc (__main__.Alpha.test_skipped_doc)\n\
                   Keep the lease while the round is owned. ... skipped 'reason'\n\
                   test_expected_doc (__main__.Alpha.test_expected_doc)\n\
                   Second docstring. ... expected failure\n\
                   ----------------------------------------------------------------------\n\
                   Ran 2 tests in 0.000s (1 skipped, 1 expected failure)\n\
                   \n\
                   OK\n";
    let error = selector_coverage(skipped, &["Alpha"]).unwrap_err();
    assert!(error.contains("Alpha"), "{error}");
    assert!(error.contains("skipped"), "{error}");
    // The same split with executed statuses must count: ok on the docstring
    // line, unexpected success is an execution (the method ran).
    let executed = "test_ok_doc (__main__.Alpha.test_ok_doc)\n\
                    Executed docstring. ... ok\n\
                    test_unexpected (__main__.Alpha.test_unexpected)\n\
                    Second docstring. ... unexpected success\n\
                    ----------------------------------------------------------------------\n\
                    Ran 2 tests in 0.000s\n\
                    \n\
                    OK\n";
    assert!(selector_coverage(executed, &["Alpha"]).is_ok());
    // A mixed report: the same-line execution covers the class while the
    // split skip contributes nothing.
    let mixed = "test_plain_ok (__main__.Alpha.test_plain_ok) ... ok\n\
                 test_skipped_doc (__main__.Alpha.test_skipped_doc)\n\
                 Keep the lease while the round is owned. ... skipped 'reason'\n\
                 ----------------------------------------------------------------------\n\
                 Ran 2 tests in 0.000s (1 skipped)\n\
                 \n\
                 OK\n";
    assert!(selector_coverage(mixed, &["Alpha"]).is_ok());
    // The interleaved shape this round caught live (test_negative_selection
    // ClaimDir): the test's own print lands between the id write and the
    // status write, so the status is alone on the next line — executed, and
    // the same interleave with a skip must stay red.
    let interleaved_ok = "test_existing (__main__.ClaimDir.test_existing) ... INVALID: output directory already exists: /tmp/tmpoz_11kxz/out\n\
                          ok\n\
                          ----------------------------------------------------------------------\n\
                          Ran 1 test in 0.000s\n\
                          \n\
                          OK\n";
    assert!(selector_coverage(interleaved_ok, &["ClaimDir"]).is_ok());
    let interleaved_skipped = "test_existing (__main__.ClaimDir.test_existing) ... INVALID: output directory already exists: /tmp/tmpoz_11kxz/out\n\
                               skipped 'reason'\n\
                               ----------------------------------------------------------------------\n\
                               Ran 1 test in 0.000s (1 skipped)\n\
                               \n\
                               OK\n";
    assert!(selector_coverage(interleaved_skipped, &["ClaimDir"]).is_err());
}

#[test]
fn declared_rosters_derive_from_the_script_source() {
    // #2513 fix-r15 (MM r14 sib#3): the rosters the preflight asserts come
    // from the scripts' own class declarations — TestCase bases in, mutation
    // drivers' helpers (TextTestResult) out, and a base covered through its
    // subclasses rather than run directly.
    let source = "class Stop(BaseException):\n\
                  class BoundResult(unittest.TextTestResult):\n\
                  class RestoreTests(unittest.TestCase):\n\
                  class McpRestoreTests(RestoreTests):\n\
                  class NodeRestoreTests(RestoreTests):\n";
    let declarations = class_declarations(source);
    let declared = declared_test_classes(&declarations);
    assert_eq!(
        declared,
        vec![
            "McpRestoreTests".to_string(),
            "NodeRestoreTests".to_string(),
            "RestoreTests".to_string()
        ]
    );
    let group = coverage_group(&declarations, "RestoreTests");
    assert_eq!(
        group,
        vec![
            "RestoreTests".to_string(),
            "McpRestoreTests".to_string(),
            "NodeRestoreTests".to_string()
        ]
    );
    // A subclass's inherited methods run under the subclass's name, so the
    // group is exactly the alias set that satisfies the base: a report with
    // only a subclass's entry covers it, a report without any of them stays
    // red, and a selector list over the same names stays a conjunction.
    let covered = "test_healthy (__main__.McpRestoreTests.test_healthy) ... ok\n\
                   ----------------------------------------------------------------------\n\
                   Ran 1 test in 0.000s\n\
                   \n\
                   OK\n";
    let names: Vec<&str> = group.iter().map(String::as_str).collect();
    assert!(group_executed(covered, &names));
    let empty_report = "----------------------------------------------------------------------\n\
                        Ran 0 tests in 0.000s\n\
                        \n\
                        NO TESTS RAN\n";
    assert!(!group_executed(empty_report, &names));
    assert!(selector_coverage(covered, &names).is_err());
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

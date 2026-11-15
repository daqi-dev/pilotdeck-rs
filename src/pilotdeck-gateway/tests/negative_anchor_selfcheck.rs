//! CI preflight for fixture mutation anchors; never launches mutation runners.
//!
//! #2513 fix-r16 (DS conc#1 + DS conc#2 + MM simp#1 + DS bnd#3): the roster
//! and the execution statuses are produced by the one python3 subprocess
//! this preflight spawns — `tests/fixtures/negative_anchor_report.py`.  The
//! helper derives the class roster with `ast` (a multi-line
//! `class Name(Base):` header is one declaration, where the pre-fix line
//! scanner silently dropped the class) and records every test outcome
//! through unittest's own `TestResult` callbacks, so the judgments below
//! read structured entries only: a printed line containing
//! `__main__.Class.` is no longer entry evidence, and the statuses no longer
//! depend on CPython's human verbose formatting.  The helper writes the
//! JSON report to a file; stdout/stderr pass through for the CI log.
use serde_json::Value;
use std::path::{Path, PathBuf};
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

/// The recorded statuses that mean a method executed.  `skipped` and
/// `expected-failure` do not; a status outside both sets fails closed
/// (counted as not executed), exactly like the pre-fix text parser's
/// unknown-status `None`.
const EXECUTED_STATUSES: &[&str] = &["ok", "fail", "error", "unexpected-success"];

/// One recorded outcome: the class it ran under and the status
/// unittest's TestResult callback reported.  The test id is validated
/// during parse (the helper's report must carry it as a string) though
/// only the class and status feed the judgments.
#[derive(Debug)]
struct Entry {
    class: String,
    status: String,
}

/// The helper's structured product for one run.
#[derive(Debug)]
struct AnchorReport {
    roster: Vec<(String, Vec<String>)>,
    entries: Vec<Entry>,
    ran: usize,
}

impl AnchorReport {
    fn parse(text: &str) -> Result<Self, String> {
        let value: Value =
            serde_json::from_str(text).map_err(|error| format!("report JSON: {error}"))?;
        let mut roster = Vec::new();
        for declaration in value["roster"]
            .as_array()
            .ok_or("report JSON: roster missing")?
        {
            let name = declaration["name"]
                .as_str()
                .ok_or("report JSON: roster name missing")?
                .to_string();
            let mut bases = Vec::new();
            for base in declaration["bases"]
                .as_array()
                .ok_or("report JSON: roster bases missing")?
            {
                bases.push(
                    base.as_str()
                        .ok_or("report JSON: base is not a string")?
                        .to_string(),
                );
            }
            roster.push((name, bases));
        }
        let mut entries = Vec::new();
        for entry in value["entries"]
            .as_array()
            .ok_or("report JSON: entries missing")?
        {
            // The report must carry the id as a string too; the
            // judgments read only the class and status, but an entry
            // without an id is a malformed report (fail closed).
            entry["id"]
                .as_str()
                .ok_or("report JSON: entry id missing")?;
            entries.push(Entry {
                class: entry["class"]
                    .as_str()
                    .ok_or("report JSON: entry class missing")?
                    .to_string(),
                status: entry["status"]
                    .as_str()
                    .ok_or("report JSON: entry status missing")?
                    .to_string(),
            });
        }
        let ran = value["ran"].as_u64().ok_or("report JSON: ran missing")? as usize;
        Ok(Self {
            roster,
            entries,
            ran,
        })
    }

    fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("report {}: {error}", path.display()))?;
        Self::parse(&text)
    }
}

/// The unittest classes a script declares, derived from the ast roster the
/// helper reports (#2513 fix-r15, MM r14 sib#3; fix-r16: the declarations
/// arrive parsed, multi-line headers included). A class counts when it
/// inherits — directly or through the script's own bases — from a TestCase
/// base, which keeps mutation drivers' helpers (`class
/// BoundResult(unittest.TextTestResult)`) out of the roster.
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

/// Whether one class has an executed entry: a recorded outcome of the
/// runner's own, in a status that means the method ran.  The pre-fix text
/// scanner counted any report line containing `__main__.Class.` here.
fn class_executed(report: &AnchorReport, class: &str) -> bool {
    report
        .entries
        .iter()
        .any(|entry| entry.class == class && EXECUTED_STATUSES.contains(&entry.status.as_str()))
}

/// Whether a declared class is covered: any of its coverage group's names has
/// an executed entry. This is a disjunction, unlike a selector list — a
/// base whose inherited methods run under a subclass's name is covered by
/// that subclass, while `selector_coverage` demands an entry per name.
fn group_executed(report: &AnchorReport, names: &[&str]) -> bool {
    names.iter().any(|class| class_executed(report, class))
}

/// Every selector must have executed at least one test of its own class.
fn selector_coverage(report: &AnchorReport, classes: &[&str]) -> Result<(), String> {
    for class in classes {
        if !class_executed(report, class) {
            return Err(format!(
                "selector {class} ran zero tests of its own (the class is absent, \
                 or every entry was skipped or expected failure)"
            ));
        }
    }
    if report.ran == 0 {
        return Err("the run executed zero tests".to_string());
    }
    Ok(())
}

fn assert_selector_coverage(report: &AnchorReport, classes: &[&str]) {
    if let Err(error) = selector_coverage(report, classes) {
        panic!("#2513 fix-r13 anchor preflight: {error}");
    }
}

fn temporary_report_path(ordinal: usize) -> PathBuf {
    std::env::temp_dir().join(format!(
        "pdrs-negative-anchor-report-{}-{ordinal}.json",
        std::process::id()
    ))
}

fn python_output(
    mut command: Command,
    helper: &Path,
    script: &Path,
    args: &[&str],
    report: &Path,
) -> Result<String, String> {
    let output = command
        .arg("-I")
        .arg("-B")
        .arg(helper)
        .arg("--report")
        .arg(report)
        .arg(script)
        .args(args)
        .output()
        .map_err(|error| {
            format!("negative anchor selfcheck requires python3; could not start it: {error}")
        })?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err(format!("{}: {}\n{text}", script.display(), output.status));
    }
    Ok(text)
}

/// Parse a sample report the same way the real one is parsed, so the unit
/// judgments below exercise the JSON product end to end.
fn sample_report(json: &str) -> AnchorReport {
    AnchorReport::parse(json).unwrap_or_else(|error| panic!("sample report JSON: {error}"))
}

// #2513 fix-r16 (DS conc#1): the two pre-fix false-pass shapes, as one
// probe script the real helper runs.  The hollow Beta prints a line
// containing `__main__.Beta.test_gone ... ok`, which the pre-fix text
// scanner counted as Beta's entry; the hollow Gamma declares itself with a
// multi-line class header, which the pre-fix line scanner silently dropped
// from the roster.
const HOLLOW_PROBE_SOURCE: &str = "import unittest\n\n\n\
     class Alpha(unittest.TestCase):\n\
     \x20   def test_ok(self):\n\
     \x20       print('__main__.Beta.test_gone ... ok')\n\n\n\
     class Beta(unittest.TestCase):\n\
     \x20   def test_hollowed(self):\n\
     \x20       raise unittest.SkipTest('hollowed out')\n\n\n\
     class Gamma(\n\
     \x20       unittest.TestCase):\n\
     \x20   pass\n\n\n\
     if __name__ == '__main__':\n\
     \x20   unittest.main(verbosity=2)\n";

const HEALTHY_PROBE_SOURCE: &str = "import unittest\n\n\n\
     class Alpha(unittest.TestCase):\n\
     \x20   def test_ok(self):\n\
     \x20       print('__main__.Beta.test_gone ... ok')\n\n\n\
     class Beta(unittest.TestCase):\n\
     \x20   def test_ok(self):\n\
     \x20       pass\n\n\n\
     class Gamma(\n\
     \x20       unittest.TestCase):\n\
     \x20   def test_ok(self):\n\
     \x20       pass\n\n\n\
     if __name__ == '__main__':\n\
     \x20   unittest.main(verbosity=2)\n";

#[test]
fn negative_fixture_anchors_and_checker_controls() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let helper = fixtures.join("negative_anchor_report.py");
    let mut ordinal = 0usize;
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
        ordinal += 1;
        let report_path = temporary_report_path(ordinal);
        let text = python_output(
            Command::new("python3"),
            &helper,
            &fixtures.join(script),
            args,
            &report_path,
        )
        .unwrap_or_else(|error| panic!("#1110 fixture preflight: {error}"));
        let report = AnchorReport::read(&report_path)
            .unwrap_or_else(|error| panic!("#2513 fix-r16 fixture preflight: {script}: {error}"));
        let _ = std::fs::remove_file(&report_path);
        // #2513 fix-r15 (MM r14 sib#3): the selector-coverage judgment is no
        // longer only test_negative_process.py's. A whole-file unittest run
        // must execute every class the script itself declares — the roster is
        // derived from the script's source, so a hollowed-out or all-skip
        // class is red here too, not only for the hand-listed selectors.
        let declared = declared_test_classes(&report.roster);
        if args.is_empty() {
            if report.ran == 0 {
                panic!("#2513 fix-r15 anchor preflight: {script} executed zero tests");
            }
            for class in &declared {
                let group = coverage_group(&report.roster, class);
                let names: Vec<&str> = group.iter().map(String::as_str).collect();
                assert!(
                    group_executed(&report, &names),
                    "#2513 fix-r15 anchor preflight: class {class} of {script} ran zero \
                     tests of its own (the class is absent, or every entry was skipped \
                     or expected failure; group {group:?}, entries {:?})",
                    report.entries
                );
            }
        } else if script == "test_negative_process.py" {
            // #2513 fix-r13: the class-selector run must actually execute every
            // selector's tests; a zero-method class is silently green (rc 0)
            // on every measured Python version, so assert the report.
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
        print!("{text}");
    }
}

#[test]
fn missing_python_is_an_error() {
    // A nonexistent child of a real source file cannot resolve to an executable.
    let missing =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/negative_anchor_selfcheck.rs/python3");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let report = temporary_report_path(90);
    let error = python_output(
        Command::new(missing),
        &fixtures.join("negative_anchor_report.py"),
        Path::new("unused.py"),
        &[],
        &report,
    )
    .unwrap_err();
    assert!(error.contains("requires python3"), "{error}");
}

#[test]
fn checker_failure_is_an_error() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let report = temporary_report_path(91);
    let error = python_output(
        Command::new("python3"),
        &fixtures.join("negative_anchor_report.py"),
        &fixtures.join("anchor_selfcheck.py"),
        &["--unknown-selfcheck-option"],
        &report,
    )
    .unwrap_err();
    assert!(error.contains("unrecognized arguments"), "{error}");
    let _ = std::fs::remove_file(&report);
}

#[test]
fn zero_method_selector_is_rejected() {
    // The recorded entries are the runner's own outcomes, so a class with
    // zero executed entries has run nothing of its own — the pre-fix text
    // scanner accepted a report whose only mention of the class was a
    // printed line (#2513 fix-r16, DS conc#1).
    let report = sample_report(
        r#"{"roster": [{"name": "HasTests", "bases": ["unittest.TestCase"]},
                      {"name": "ZeroCase", "bases": ["unittest.TestCase"]}],
            "entries": [{"id": "__main__.HasTests.test_it", "class": "HasTests",
                         "status": "ok"}],
            "ran": 1}"#,
    );
    let error = selector_coverage(&report, &["HasTests", "ZeroCase"]).unwrap_err();
    assert!(error.contains("ZeroCase"), "{error}");
    assert!(selector_coverage(&report, &["HasTests"]).is_ok());
}

#[test]
fn zero_test_run_is_rejected() {
    // A run with zero recorded outcomes must be rejected regardless of the
    // exit code; the pre-fix text shapes (`Ran 0 tests ... OK` on <=3.11)
    // are the ran count now.
    let empty = sample_report(r#"{"roster": [], "entries": [], "ran": 0}"#);
    let error = selector_coverage(&empty, &["HasTests"]).unwrap_err();
    assert!(error.contains("zero tests"), "{error}");
    // The Ran-0 branch below the loop is only reachable with an empty
    // selector list — every non-empty list fails earlier on its first absent
    // class — so pin it directly (#2513 fix-r14 simplify#8, choice recorded
    // in the round RESULT: keep the branch, add the direct call).
    let error = selector_coverage(&empty, &[]).unwrap_err();
    assert!(error.contains("zero tests"), "{error}");
}

#[test]
fn skipped_only_selector_is_rejected() {
    // #2513 fix-r14 (DS r13 review): a selector whose every entry is skipped
    // or expected-failure has executed nothing of its own — unittest still
    // reports those runs as collected, so those statuses must not count as
    // coverage. fix-r16: the statuses arrive from unittest's TestResult
    // callbacks, not from the verbose text.
    let skipped = sample_report(
        r#"{"roster": [{"name": "Alpha", "bases": ["unittest.TestCase"]}],
            "entries": [{"id": "__main__.Alpha.test_one", "class": "Alpha",
                         "status": "skipped"},
                        {"id": "__main__.Alpha.test_two", "class": "Alpha",
                         "status": "expected-failure"}],
            "ran": 2}"#,
    );
    let error = selector_coverage(&skipped, &["Alpha"]).unwrap_err();
    assert!(error.contains("Alpha"), "{error}");
    assert!(error.contains("skipped"), "{error}");
    // A mixed report still covers the class that really executed.
    let mixed = sample_report(
        r#"{"roster": [{"name": "Alpha", "bases": ["unittest.TestCase"]}],
            "entries": [{"id": "__main__.Alpha.test_one", "class": "Alpha",
                         "status": "ok"},
                        {"id": "__main__.Alpha.test_two", "class": "Alpha",
                         "status": "skipped"}],
            "ran": 2}"#,
    );
    assert!(selector_coverage(&mixed, &["Alpha"]).is_ok());
    assert!(selector_coverage(&mixed, &["Beta"]).is_err());
    // fail / error / unexpected-success executed the method (the pre-fix
    // text statuses FAIL / ERROR / unexpected success meant the same), and
    // an unknown status fails closed.
    for status in ["fail", "error", "unexpected-success", "weird"] {
        let report = sample_report(&format!(
            r#"{{"roster": [{{"name": "Alpha", "bases": ["unittest.TestCase"]}}],
                "entries": [{{"id": "__main__.Alpha.test_one", "class": "Alpha",
                             "status": "{status}"}}], "ran": 1}}"#
        ));
        let covered = selector_coverage(&report, &["Alpha"]).is_ok();
        assert_eq!(
            covered,
            status != "weird",
            "status {status} must {}count as executed",
            if status == "weird" { "not " } else { "" }
        );
    }
}

#[test]
fn printed_text_and_multiline_headers_cannot_fake_coverage() {
    // #2513 fix-r16 (DS conc#1 + DS conc#2): the two pre-fix false-pass
    // shapes, pinned end to end through the real helper. The hollow Beta
    // prints a line containing `__main__.Beta.test_gone ... ok` mid-report
    // (unittest does not buffer per-test output by default); the pre-fix
    // class_executed counted that line as Beta's entry. The hollow Gamma
    // declares itself with a multi-line class header, which the pre-fix
    // line scanner silently dropped from the roster. Both hollow shapes
    // stay red; the healthy control with a real Beta test and a populated
    // Gamma stays green.
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let helper = fixtures.join("negative_anchor_report.py");
    let scratch = std::env::temp_dir().join(format!(
        "pdrs-negative-anchor-selfcheck-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&scratch).expect("scratch directory");
    let hollow = scratch.join("hollow_probe.py");
    std::fs::write(&hollow, HOLLOW_PROBE_SOURCE).expect("write hollow probe");
    let healthy = scratch.join("healthy_probe.py");
    std::fs::write(&healthy, HEALTHY_PROBE_SOURCE).expect("write healthy probe");
    let report_path = scratch.join("probe-report.json");
    python_output(Command::new("python3"), &helper, &hollow, &[], &report_path)
        .expect("hollow probe run");
    let report = AnchorReport::read(&report_path).expect("hollow probe report");
    let _ = std::fs::remove_file(&report_path);
    // The multi-line header survived into the roster (the pre-fix scanner
    // dropped Gamma here, and the hollow class escaped judgment).
    assert!(
        report.roster.iter().any(|(name, _)| name == "Gamma"),
        "multi-line class header dropped from roster: {:?}",
        report.roster
    );
    // The printed text is not entry evidence: both hollow classes stay red.
    assert!(selector_coverage(&report, &["Alpha"]).is_ok());
    let beta = selector_coverage(&report, &["Beta"]).unwrap_err();
    assert!(beta.contains("Beta"), "{beta}");
    let gamma = selector_coverage(&report, &["Gamma"]).unwrap_err();
    assert!(gamma.contains("Gamma"), "{gamma}");
    python_output(
        Command::new("python3"),
        &helper,
        &healthy,
        &[],
        &report_path,
    )
    .expect("healthy probe run");
    let report = AnchorReport::read(&report_path).expect("healthy probe report");
    let _ = std::fs::remove_file(&report_path);
    let _ = std::fs::remove_file(&hollow);
    let _ = std::fs::remove_file(&healthy);
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(selector_coverage(&report, &["Alpha", "Beta", "Gamma"]).is_ok());
}

#[test]
fn declared_rosters_derive_from_the_script_source() {
    // #2513 fix-r15 (MM r14 sib#3): the rosters the preflight asserts come
    // from the scripts' own class declarations — TestCase bases in, mutation
    // drivers' helpers (TextTestResult) out, and a base covered through its
    // subclasses rather than run directly. fix-r16: the declarations arrive
    // parsed from the helper's ast roster.
    let report = sample_report(
        r#"{"roster": [{"name": "Stop", "bases": ["BaseException"]},
                      {"name": "BoundResult", "bases": ["unittest.TextTestResult"]},
                      {"name": "RestoreTests", "bases": ["unittest.TestCase"]},
                      {"name": "McpRestoreTests", "bases": ["RestoreTests"]},
                      {"name": "NodeRestoreTests", "bases": ["RestoreTests"]}],
            "entries": [], "ran": 0}"#,
    );
    let declared = declared_test_classes(&report.roster);
    assert_eq!(
        declared,
        vec![
            "McpRestoreTests".to_string(),
            "NodeRestoreTests".to_string(),
            "RestoreTests".to_string()
        ]
    );
    let group = coverage_group(&report.roster, "RestoreTests");
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
    let covered = sample_report(
        r#"{"roster": [{"name": "RestoreTests", "bases": ["unittest.TestCase"]},
                      {"name": "McpRestoreTests", "bases": ["RestoreTests"]},
                      {"name": "NodeRestoreTests", "bases": ["RestoreTests"]}],
            "entries": [{"id": "__main__.McpRestoreTests.test_healthy",
                         "class": "McpRestoreTests", "status": "ok"}],
            "ran": 1}"#,
    );
    let names: Vec<&str> = group.iter().map(String::as_str).collect();
    assert!(group_executed(&covered, &names));
    assert!(!group_executed(&report, &names));
    assert!(selector_coverage(&covered, &names).is_err());
}

#[test]
fn covered_selectors_pass() {
    let covered = sample_report(
        r#"{"roster": [{"name": "Alpha", "bases": ["unittest.TestCase"]},
                      {"name": "Beta", "bases": ["unittest.TestCase"]}],
            "entries": [{"id": "__main__.Alpha.test_one", "class": "Alpha",
                         "status": "ok"},
                        {"id": "__main__.Beta.test_two", "class": "Beta",
                         "status": "ok"}],
            "ran": 2}"#,
    );
    assert!(selector_coverage(&covered, &["Alpha", "Beta"]).is_ok());
    assert!(selector_coverage(&covered, NEGATIVE_PROCESS_CLASSES).is_err());
}

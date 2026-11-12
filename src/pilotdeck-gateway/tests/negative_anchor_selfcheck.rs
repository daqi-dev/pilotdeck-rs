//! CI preflight for fixture mutation anchors; never launches mutation runners.
use std::path::Path;
use std::process::Command;

// #2692 fix-r11 (review boundary#1 / concurrency#1, P1): the run list is a
// file-level constant so the consumer test and the discovery test below read
// the same array. The fix-r10 shape scanned this file's own source text with
// `contains`, so a twin commented out of the list kept the substring (and the
// green) while the twin silently stopped running -- the exact failure mode
// this preflight exists to catch.
const PREFLIGHT: &[(&str, &[&str])] = &[
    ("test_anchor_selfcheck.py", &[]),
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
        ],
    ),
    ("_negkit/test_compare_verdicts.py", &[]),
    ("_negkit/test_contract.py", &[]),
    ("_negkit/negative_contract.py", &[]),
    ("_negkit/test_submit_migration.py", &[]),
    ("_negkit/negative_submit_migration.py", &[]),
    // #1540：non_head 契约单测与配套变异负控；不列进来等于这两个测试在
    // CI 里从没跑过（评审 r1 sibling#2 / concurrency#5）。
    ("_negkit/test_non_head.py", &[]),
    ("_negkit/negative_non_head.py", &[]),
    // #1540 r3：共享件 claim_dir/select/registered 的自测此前不在任何
    // CI 入口（评审 r3 sibling#4）；-I 下自举导入路径后可直接跑。
    ("test_negative_selection.py", &[]),
    ("negative-restore/test_restore.py", &[]),
    ("negative-restore/test_restore.py", &["--negative-controls"]),
    ("pending-reanchor/test_controls.py", &[]),
    // #2692 fix-r9（评审 sibling#1）：native-config 的红侧测试（改名腿 /
    // stale-anchor 腿 / 三串计数正控）此前不进任何自动入口——CI 只跑
    // negative_controls.py --check 的正向，绑定门禁删坏时唯一会红的文件
    // 没人跑。纯 Python 零 cargo（一次只读 --check 子进程，秒级）。
    ("native-config/test_literal_anchors.py", &[]),
    // #2692 fix-r9（评审 sibling#3）：turn-read / turn-publish / blankkey
    // 三家 declares_test 门禁载体的同款红侧（fix-r8 只给 native-config /
    // failure-guard 补过进树红侧），一并接进本入口，不再只靠宿主链的
    // 出树负控轮；同样纯 Python 零 cargo。
    ("turn-read/test_bound_test_gate.py", &[]),
    ("turn-publish/test_bound_test_gate.py", &[]),
    ("native-blankkey-submit/test_bound_test_gate.py", &[]),
    // #2692 fix-r10（评审 sibling#1）：blankkey 自家 check_literal_anchors
    // 字面锚守卫的红侧（pin 不在源 / 出扫描面 / 无行绑定 / 无 pin 断言 /
    // 行出扫描面 / 子集绑定全表），同款纯 Python 零 cargo。
    ("native-blankkey-submit/test_literal_anchors.py", &[]),
    // CI prepares the TS baseline after cargo test. This explicit mode still
    // lists missing launcher prerequisites; local invocation is strict.
    ("anchor_selfcheck.py", &["--anchors-only"]),
];

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
    for (script, args) in PREFLIGHT {
        let report = python_output(Command::new("python3"), &fixtures.join(script), args)
            .unwrap_or_else(|error| panic!("#1110 fixture preflight: {error}"));
        print!("{report}");
    }
}

#[test]
fn fixture_red_side_twins_are_registered_in_the_preflight_list() {
    // #2692 fix-r10 (review simplify#5): the run list above is hand-maintained,
    // so a red-side twin that is not in it silently never runs -- the exact
    // failure mode this preflight exists to catch. Discovery: every
    // `fixtures/<dir>/test_bound_test_gate.py` and
    // `fixtures/<dir>/test_literal_anchors.py` twin (the two shapes this
    // family uses for in-tree red sides) must appear in PREFLIGHT above.
    // #2692 fix-r11 (review boundary#1 / concurrency#1, P1): the check
    // compares against the constant itself. The fix-r10 shape scanned this
    // file's own source text with `contains`, so "the spelling appears
    // anywhere in this file" passed for "the twin is in the run list": a
    // twin commented out of the array stayed green here, and a cross-line
    // entry could false-red it.
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut twins = Vec::new();
    for entry in std::fs::read_dir(&fixtures).unwrap_or_else(|error| {
        panic!("#2692 fix-r10: cannot read {}: {error}", fixtures.display())
    }) {
        let entry = entry.unwrap_or_else(|error| {
            panic!("#2692 fix-r10: cannot read {}: {error}", fixtures.display())
        });
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        for name in ["test_bound_test_gate.py", "test_literal_anchors.py"] {
            if entry.path().join(name).is_file() {
                twins.push(format!("{}/{}", entry.file_name().to_string_lossy(), name));
            }
        }
    }
    twins.sort();
    assert!(
        !twins.is_empty(),
        "#2692 fix-r10: discovery found no red-side twins under {}",
        fixtures.display()
    );
    for script in &twins {
        assert!(
            PREFLIGHT
                .iter()
                .any(|(registered, _)| *registered == script.as_str()),
            "#2692 fix-r11 (review boundary#1): the red-side twin {script} exists in \
             tests/fixtures but is not registered in the PREFLIGHT list above"
        );
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

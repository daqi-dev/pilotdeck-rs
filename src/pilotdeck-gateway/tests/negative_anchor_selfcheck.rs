//! CI preflight for fixture mutation anchors; never launches mutation runners.
//!
//! #2692 fix-r12 (review DS boundary P2 / MM boundary P2 / MM sibling P2#4)
//! tightened the fix-r10/r11 shape in three ways:
//! 1. Judgment (DS boundary P2): an entry that reports unittest's summary
//!    must show `Ran N tests` with N >= 1 and `OK` -- exit 0 alone stayed
//!    green for an emptied or renamed test module (`Ran 0 tests` still
//!    exits 0).
//! 2. Discovery (MM boundary P2): the twin scan walks `tests/fixtures` to
//!    arbitrary depth; the depth-1 `read_dir` silently skipped nested
//!    twins (`fixtures/x/y/test_bound_test_gate.py`).
//! 3. Census (MM sibling P2#4): every directory holding a mutation runner
//!    (`negative.py` / `negative_controls.py` / `negative-controls.py`)
//!    must carry an in-tree red-side twin (`test_bound_test_gate.py` /
//!    `test_literal_anchors.py`) or be listed in TWIN_EXEMPTIONS with a
//!    one-line reason; third-named red sides such as
//!    `failure-guard/test_verdict.py` enter the surface through the same
//!    census and run through their PREFLIGHT entry.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The mutation-runner file names the census counts, at any depth.
const RUNNER_NAMES: [&str; 3] = [
    "negative.py",
    "negative_controls.py",
    "negative-controls.py",
];

/// The in-tree red-side file names the census accepts. The first two are the
/// shapes the #2692 family uses; the third names failure-guard's red side
/// (its tests import the sibling `verdict.py`, so `-I` cannot run the file
/// directly and discovery carries it in PREFLIGHT instead).
const RED_SIDE_NAMES: [&str; 3] = [
    "test_bound_test_gate.py",
    "test_literal_anchors.py",
    "test_verdict.py",
];

/// Reason shared by the legacy runners below: the in-tree gate is the
/// fixture's own `--check` plus the host's out-of-tree mutation round; no
/// red-side twin has been written yet. fix-r12 registers the gap instead of
/// leaving it silent -- new fixtures must add a twin, not extend this list.
const LEGACY_RUNNER: &str =
    "legacy runner: --check plus the host out-of-tree round; no in-tree twin yet";

/// One preflight entry: how to invoke it and how to judge the output.
struct Entry {
    /// Path under tests/fixtures (plain mode), or the discovery source
    /// directory (discover mode).
    script: &'static str,
    /// Script arguments; in discover mode slot 0 is the `-p` pattern.
    args: &'static [&'static str],
    /// Discover mode: `python3 -I -B -m unittest discover -s <script>
    /// -p <args[0]>`. The third-named red sides import sibling modules
    /// (`test_verdict.py` imports `verdict.py`); `-I` drops the script
    /// directory from sys.path, so only discovery puts the top-level
    /// directory back (#2692 fix-r12, MM sibling P2#4).
    discover: bool,
    /// Require unittest's `Ran N tests` (N >= 1) and `OK` in the output
    /// (#2692 fix-r12, DS boundary P2): exit 0 alone stayed green for an
    /// emptied or renamed test module. The mutation runners that print
    /// their own summaries keep the exit-0 contract.
    ran: bool,
}

impl Entry {
    /// The fixtures-relative path this entry vouches for.
    fn path(&self) -> String {
        if self.discover {
            format!("{}/{}", self.script, self.args[0])
        } else {
            self.script.to_string()
        }
    }
}

// #2692 fix-r11 (review boundary#1 / concurrency#1, P1): the run list is a
// constant so the consumer test and the discovery test below read the same
// array. The fix-r10 shape scanned this file's own source text with
// `contains`, so a twin commented out of the list kept the substring (and the
// green) while the twin silently stopped running -- the exact failure mode
// this preflight exists to catch.
const PREFLIGHT: &[Entry] = &[
    Entry {
        script: "test_anchor_selfcheck.py",
        args: &[],
        discover: false,
        ran: true,
    },
    Entry {
        script: "test_negative_process.py",
        args: &[
            // ProcessControls 持 wait_for_slot / run_owned 的契约测试（含
            // cargo-slots.sh 退出码语义）；不列进来等于这些测试在 CI 里从没跑过。
            "ProcessControls",
            "OutputDirectoryGuard",
            "OutputDirectoryClaim",
            "OutputDirectoryRunnerTests",
            "TreeLockBudget",
        ],
        discover: false,
        ran: true,
    },
    Entry {
        script: "_negkit/test_compare_verdicts.py",
        args: &[],
        discover: false,
        ran: true,
    },
    Entry {
        script: "_negkit/test_contract.py",
        args: &[],
        discover: false,
        ran: true,
    },
    Entry {
        script: "_negkit/negative_contract.py",
        args: &[],
        discover: false,
        ran: false,
    },
    Entry {
        script: "_negkit/test_submit_migration.py",
        args: &[],
        discover: false,
        ran: true,
    },
    Entry {
        script: "_negkit/negative_submit_migration.py",
        args: &[],
        discover: false,
        ran: false,
    },
    // #1540：non_head 契约单测与配套变异负控；不列进来等于这两个测试在
    // CI 里从没跑过（评审 r1 sibling#2 / concurrency#5）。
    Entry {
        script: "_negkit/test_non_head.py",
        args: &[],
        discover: false,
        ran: true,
    },
    Entry {
        script: "_negkit/negative_non_head.py",
        args: &[],
        discover: false,
        ran: false,
    },
    // #1540 r3：共享件 claim_dir/select/registered 的自测此前不在任何
    // CI 入口（评审 r3 sibling#4）；-I 下自举导入路径后可直接跑。
    Entry {
        script: "test_negative_selection.py",
        args: &[],
        discover: false,
        ran: true,
    },
    Entry {
        script: "negative-restore/test_restore.py",
        args: &[],
        discover: false,
        ran: true,
    },
    // Prints its own KILLED/SURVIVED summary (54 child `Ran` lines), so the
    // fix-r12 `Ran N tests` judgment does not apply -- exit 0 is the verdict.
    Entry {
        script: "negative-restore/test_restore.py",
        args: &["--negative-controls"],
        discover: false,
        ran: false,
    },
    Entry {
        script: "pending-reanchor/test_controls.py",
        args: &[],
        discover: false,
        ran: true,
    },
    // #2692 fix-r9（评审 sibling#1）：native-config 的红侧测试（改名腿 /
    // stale-anchor 腿 / 三串计数正控）此前不进任何自动入口——CI 只跑
    // negative_controls.py --check 的正向，绑定门禁删坏时唯一会红的文件
    // 没人跑。纯 Python 零 cargo（一次只读 --check 子进程，秒级）。
    Entry {
        script: "native-config/test_literal_anchors.py",
        args: &[],
        discover: false,
        ran: true,
    },
    // #2692 fix-r9（评审 sibling#3）：turn-read / turn-publish / blankkey
    // 三家 declares_test 门禁载体的同款红侧（fix-r8 只给 native-config /
    // failure-guard 补过进树红侧），一并接进本入口，不再只靠宿主链的
    // 出树负控轮；同样纯 Python 零 cargo。
    Entry {
        script: "turn-read/test_bound_test_gate.py",
        args: &[],
        discover: false,
        ran: true,
    },
    Entry {
        script: "turn-publish/test_bound_test_gate.py",
        args: &[],
        discover: false,
        ran: true,
    },
    Entry {
        script: "native-blankkey-submit/test_bound_test_gate.py",
        args: &[],
        discover: false,
        ran: true,
    },
    // #2692 fix-r10（评审 sibling#1）：blankkey 自家 check_literal_anchors
    // 字面锚守卫的红侧（pin 不在源 / 出扫描面 / 无行绑定 / 无 pin 断言 /
    // 行出扫描面 / 子集绑定全表）。
    Entry {
        script: "native-blankkey-submit/test_literal_anchors.py",
        args: &[],
        discover: false,
        ran: true,
    },
    // #2692 fix-r12 (MM sibling P1): turn-submit 的静态名册门禁红侧（删行 /
    // 改名 / 重名腿），与四家 EXPECTED_NAMES 先例同款。
    Entry {
        script: "turn-submit/test_bound_test_gate.py",
        args: &[],
        discover: false,
        ran: true,
    },
    // #2692 fix-r12 (DS + MM 同条）：session 两家 check_literal_anchors
    // 完整性守卫拷贝的树内红侧，与 blankkey / native-config
    // 同款形态。
    Entry {
        script: "session-reservation-2391/test_literal_anchors.py",
        args: &[],
        discover: false,
        ran: true,
    },
    Entry {
        script: "session-delete-2392/test_literal_anchors.py",
        args: &[],
        discover: false,
        ran: true,
    },
    // #2692 fix-r12 (MM sibling P2#4): failure-guard 的第三命名红侧
    // test_verdict.py——它 import 同目录的 verdict.py，-I 下直接跑会
    // ModuleNotFoundError，只能 unittest discover 把顶层目录放回 sys.path；
    // 此前不在任何 CI 入口。
    Entry {
        script: "failure-guard",
        args: &["test_verdict.py"],
        discover: true,
        ran: true,
    },
    // CI prepares the TS baseline after cargo test. This explicit mode still
    // lists missing launcher prerequisites; local invocation is strict. Prints
    // its own SUMMARY line, so the fix-r12 `Ran N tests` judgment does not
    // apply.
    Entry {
        script: "anchor_selfcheck.py",
        args: &["--anchors-only"],
        discover: false,
        ran: false,
    },
];

/// Dirs whose red side lives in a file this preflight does not run; the
/// census above accepts these instead of the canonical twin names.
const TWIN_EXEMPTIONS: &[(&str, &str)] = &[
    (
        "background-output",
        "red side is the third-named test_negative.py, not wired into this preflight",
    ),
    (
        "background-panic",
        "red side is the third-named test_negative_controls.py, not wired into this preflight",
    ),
    (
        "cli-parity-isolation",
        "red side is the third-named test_negative_controls.py, not wired into this preflight",
    ),
    (
        "cli-parity-lock-bounded",
        "red side is the third-named test_negative_controls.py, not wired into this preflight",
    ),
    (
        "cli-parity-mock-gateway",
        "red side is the third-named test_negative_controls.py, not wired into this preflight",
    ),
    (
        "edgeclaw-capture-turn",
        "red side is the third-named test_negative.py, not wired into this preflight",
    ),
    (
        "edgeclaw-entries",
        "red side is the third-named test_negative_runners.py, not wired into this preflight",
    ),
    (
        "edgeclaw-llm",
        "red side is the third-named test_controls.py, not wired into this preflight",
    ),
    (
        "edgeclaw-llm-telemetry",
        "red side is the third-named test_controls.py, not wired into this preflight",
    ),
    (
        "edgeclaw-real-provider",
        "red side is the third-named test_smoke.py, not wired into this preflight",
    ),
    (
        "edgeclaw-service-facade",
        "red side is the third-named test_schemata.py + test_switch_negative.py, not wired into this preflight",
    ),
    (
        "edgeclaw-service-orchestration",
        "red side is the third-named test_switch_negative.py, not wired into this preflight",
    ),
    (
        "frozen-manifest",
        "red side is the third-named test_cli.py + test_manifest.py, not wired into this preflight",
    ),
    (
        "frozen-reinsert-guard",
        "red side is the third-named test_entry_guard.py + test_negative_controls.py, not wired into this preflight",
    ),
    (
        "issue-2495",
        "red side is the third-named test_negative_controls.py, not wired into this preflight",
    ),
    (
        "ledger-diffs",
        "red side is the third-named test_ledger.py, not wired into this preflight",
    ),
    (
        "live-e2e",
        "red side is the third-named test_acceptance.py, not wired into this preflight",
    ),
    (
        "native-dispatch",
        "red side is the third-named test_protocol.py, not wired into this preflight",
    ),
    (
        "native-entry-authority",
        "red side is the third-named test_native_entry_binding.py, not wired into this preflight",
    ),
    (
        "native-management",
        "red side is the third-named test_protocol.py, not wired into this preflight",
    ),
    (
        "native-session-e2e",
        "red side is the third-named test_e2e.py, not wired into this preflight",
    ),
    (
        "native-stimulus-plan",
        "red side is the third-named test_plan.py, not wired into this preflight",
    ),
    (
        "native-witness",
        "red side is the third-named test_authentication.py + test_boundary.py + test_chain.py + test_r11.py + test_witness.py, not wired into this preflight",
    ),
    (
        "network-timing",
        "red side is the third-named test_negative_controls.py + test_network_timing_judge.py, not wired into this preflight",
    ),
    (
        "parity-lock-release",
        "red side is the third-named test_lock_release.py, not wired into this preflight",
    ),
    (
        "pending-reanchor",
        "red side is the third-named test_controls.py, registered in PREFLIGHT",
    ),
    (
        "replacement",
        "red side is the third-named test_negative_518.py, not wired into this preflight",
    ),
    (
        "session-admission",
        "red side is the third-named test_negative.py, not wired into this preflight",
    ),
    (
        "session-domain-expectations",
        "red side is the third-named test_meta_controls.py, not wired into this preflight",
    ),
    (
        "session-domain-lock-release",
        "red side is the third-named test_lock_release.py, not wired into this preflight",
    ),
    (
        "session-lease",
        "red side is the third-named test_lease.py, not wired into this preflight",
    ),
    (
        "settings-gateway",
        "red side is the third-named test_negative_controls.py, not wired into this preflight",
    ),
    (
        "workspace-pristine",
        "red side is the third-named test_controls.py, not wired into this preflight",
    ),
    // The legacy runners: the in-tree gate is the fixture's own `--check`
    // plus the host's out-of-tree mutation round; no twin has been written
    // yet. fix-r12 registers the gap instead of leaving it silent -- new
    // fixtures must add a twin, not extend this list.
    ("agent-event-projector", LEGACY_RUNNER),
    ("always-on-2294", LEGACY_RUNNER),
    ("anchor-marker", LEGACY_RUNNER),
    ("api-server-channel", LEGACY_RUNNER),
    ("api-web", LEGACY_RUNNER),
    ("attach-followups-a", LEGACY_RUNNER),
    ("attach-twopass-1460", LEGACY_RUNNER),
    ("background-create-gate", LEGACY_RUNNER),
    ("background-persist-progress", LEGACY_RUNNER),
    ("background-persist-restart", LEGACY_RUNNER),
    ("background-subtick-gate", LEGACY_RUNNER),
    ("baseline-provenance", LEGACY_RUNNER),
    ("channel-bluebubbles", LEGACY_RUNNER),
    ("channel-core", LEGACY_RUNNER),
    ("channel-feishu", LEGACY_RUNNER),
    ("channel-loader-startup", LEGACY_RUNNER),
    ("channel-slack", LEGACY_RUNNER),
    ("channel-sms", LEGACY_RUNNER),
    ("channel-sms-head-timeout", LEGACY_RUNNER),
    ("channel-unmigrated", LEGACY_RUNNER),
    ("channel-unmigrated-startup", LEGACY_RUNNER),
    ("commands-catalog", LEGACY_RUNNER),
    ("config-store", LEGACY_RUNNER),
    ("connection-context", LEGACY_RUNNER),
    ("cron-schedule", LEGACY_RUNNER),
    ("cron-scheduler", LEGACY_RUNNER),
    ("cron-store", LEGACY_RUNNER),
    ("diff-backfill", LEGACY_RUNNER),
    ("edgeclaw-accessors", LEGACY_RUNNER),
    ("edgeclaw-dangling-links", LEGACY_RUNNER),
    ("edgeclaw-dream", LEGACY_RUNNER),
    ("edgeclaw-dream-run", LEGACY_RUNNER),
    ("edgeclaw-dream-runner", LEGACY_RUNNER),
    ("edgeclaw-file-memory", LEGACY_RUNNER),
    ("edgeclaw-heartbeat", LEGACY_RUNNER),
    ("edgeclaw-heartbeat-indexer", LEGACY_RUNNER),
    ("edgeclaw-heartbeat-isolation", LEGACY_RUNNER),
    ("edgeclaw-llm-config", LEGACY_RUNNER),
    ("edgeclaw-llm-dream", LEGACY_RUNNER),
    ("edgeclaw-llm-envelope", LEGACY_RUNNER),
    ("edgeclaw-llm-index", LEGACY_RUNNER),
    ("edgeclaw-llm-recall", LEGACY_RUNNER),
    ("edgeclaw-messages", LEGACY_RUNNER),
    ("edgeclaw-native-default", LEGACY_RUNNER),
    ("edgeclaw-provider", LEGACY_RUNNER),
    ("edgeclaw-retrieval-helpers", LEGACY_RUNNER),
    ("edgeclaw-retriever", LEGACY_RUNNER),
    ("edgeclaw-same-root-generation", LEGACY_RUNNER),
    ("edgeclaw-text", LEGACY_RUNNER),
    ("exact-receipt-yaml", LEGACY_RUNNER),
    ("exact-rerun-receipt", LEGACY_RUNNER),
    ("fe-baseline-report", LEGACY_RUNNER),
    ("fe-coexist", LEGACY_RUNNER),
    ("file-history-cwd", LEGACY_RUNNER),
    ("flake-progress", LEGACY_RUNNER),
    ("gateway-batch", LEGACY_RUNNER),
    ("gateway-entry-guard", LEGACY_RUNNER),
    ("grep-etxtbsy", LEGACY_RUNNER),
    ("handle-path-macos", LEGACY_RUNNER),
    ("hooks-output", LEGACY_RUNNER),
    ("host-event-pump", LEGACY_RUNNER),
    ("html-timing", LEGACY_RUNNER),
    ("http-store-park", LEGACY_RUNNER),
    ("interaction-bus", LEGACY_RUNNER),
    ("interaction-values", LEGACY_RUNNER),
    ("issue-1246", LEGACY_RUNNER),
    ("issue-1437-methods", LEGACY_RUNNER),
    ("issue-1604-require-text", LEGACY_RUNNER),
    ("issue-297", LEGACY_RUNNER),
    ("issue-297-domain-fold", LEGACY_RUNNER),
    ("js-math-zero", LEGACY_RUNNER),
    ("live-e2e-tmpdir", LEGACY_RUNNER),
    ("live-history", LEGACY_RUNNER),
    ("locale-adaptive", LEGACY_RUNNER),
    ("locale-contract", LEGACY_RUNNER),
    ("locked-storage", LEGACY_RUNNER),
    ("manager-gateway", LEGACY_RUNNER),
    ("mcp-detached-dispatch", LEGACY_RUNNER),
    ("mcp-gateway", LEGACY_RUNNER),
    ("mcp-hooks-spawn", LEGACY_RUNNER),
    ("mcp-http-corrupt-body", LEGACY_RUNNER),
    ("mcp-http-encoding", LEGACY_RUNNER),
    ("memory-gateway", LEGACY_RUNNER),
    ("memory-maintenance", LEGACY_RUNNER),
    ("metadata-from-env", LEGACY_RUNNER),
    ("metadata-process-cwd", LEGACY_RUNNER),
    ("mode-routing", LEGACY_RUNNER),
    ("model-catalog-cwd", LEGACY_RUNNER),
    ("model-complete", LEGACY_RUNNER),
    ("model-complete-timeout", LEGACY_RUNNER),
    ("model-factory", LEGACY_RUNNER),
    ("model-http-encoding", LEGACY_RUNNER),
    ("module-host", LEGACY_RUNNER),
    ("module-host-approvals", LEGACY_RUNNER),
    ("native-archive-cwd", LEGACY_RUNNER),
    ("native-collation", LEGACY_RUNNER),
    ("native-cron", LEGACY_RUNNER),
    ("native-http-basic", LEGACY_RUNNER),
    ("native-loop-stimulus", LEGACY_RUNNER),
    ("native-metadata-cwd", LEGACY_RUNNER),
    ("native-process", LEGACY_RUNNER),
    ("native-registry-switch", LEGACY_RUNNER),
    ("native-settings/negative", LEGACY_RUNNER),
    ("native-storage-stimulus", LEGACY_RUNNER),
    ("native-ws-caps", LEGACY_RUNNER),
    ("native-ws-entry", LEGACY_RUNNER),
    ("native-ws-notify", LEGACY_RUNNER),
    ("negative-adaptive", LEGACY_RUNNER),
    ("negative-output-directory", LEGACY_RUNNER),
    ("node-io", LEGACY_RUNNER),
    ("node-provenance", LEGACY_RUNNER),
    ("operation-ledger", LEGACY_RUNNER),
    ("output-schema", LEGACY_RUNNER),
    ("p01-preflight", LEGACY_RUNNER),
    ("parity-subprocess-hardening", LEGACY_RUNNER),
    ("permission-mode-surrogate", LEGACY_RUNNER),
    ("phased-run-drained", LEGACY_RUNNER),
    ("plugin-loader", LEGACY_RUNNER),
    ("plugin-runtime", LEGACY_RUNNER),
    ("private-ca", LEGACY_RUNNER),
    ("project-files-linux-dirent", LEGACY_RUNNER),
    ("projector-cwd", LEGACY_RUNNER),
    ("provider-http-boundaries", LEGACY_RUNNER),
    ("proxy-listen-address", LEGACY_RUNNER),
    ("pump-order-cases", LEGACY_RUNNER),
    ("regeneration-masks", LEGACY_RUNNER),
    ("regex-proto-budget", LEGACY_RUNNER),
    ("retirement-negctl-roster", LEGACY_RUNNER),
    ("retry-after-dialects", LEGACY_RUNNER),
    ("runtime-spawn-panic", LEGACY_RUNNER),
    ("same-json-surfaces", LEGACY_RUNNER),
    ("sdk-cli-lifecycle", LEGACY_RUNNER),
    ("sdk-format", LEGACY_RUNNER),
    ("sdk-mcp-p2", LEGACY_RUNNER),
    ("sdk-mcp-session-tools", LEGACY_RUNNER),
    ("sdk-run", LEGACY_RUNNER),
    ("sdk-sessions-list", LEGACY_RUNNER),
    ("sdk-sessions-messages", LEGACY_RUNNER),
    ("sdk-settings", LEGACY_RUNNER),
    ("sdk-signal-read-lease", LEGACY_RUNNER),
    ("sdk-signals", LEGACY_RUNNER),
    ("sdk-signals-close-order-pin", LEGACY_RUNNER),
    ("sdk-terminal", LEGACY_RUNNER),
    ("session-assembly-1316", LEGACY_RUNNER),
    ("session-cwd-timing", LEGACY_RUNNER),
    ("session-domain-cleanup", LEGACY_RUNNER),
    ("session-domain-native", LEGACY_RUNNER),
    ("session-drop-release", LEGACY_RUNNER),
    ("session-factory", LEGACY_RUNNER),
    ("session-factory/append-delta", LEGACY_RUNNER),
    ("session-factory/projection-cache", LEGACY_RUNNER),
    ("settings", LEGACY_RUNNER),
    ("shared-runner-failclosed", LEGACY_RUNNER),
    ("sidecar-liveness-1891", LEGACY_RUNNER),
    ("sidecar-payload", LEGACY_RUNNER),
    ("skills", LEGACY_RUNNER),
    ("skills-external", LEGACY_RUNNER),
    ("skills-tree-files", LEGACY_RUNNER),
    ("slot-probe-residual", LEGACY_RUNNER),
    ("sms-stringify-depth", LEGACY_RUNNER),
    ("snapshot-projection", LEGACY_RUNNER),
    ("spill-isolation", LEGACY_RUNNER),
    ("stats-cwd", LEGACY_RUNNER),
    ("stress-followups", LEGACY_RUNNER),
    ("surrogate-collation-matrix", LEGACY_RUNNER),
    ("switch-state-machine", LEGACY_RUNNER),
    ("task-budget-zero", LEGACY_RUNNER),
    ("transcript-resolve", LEGACY_RUNNER),
    ("transcript-trim", LEGACY_RUNNER),
    ("treelock-budget", LEGACY_RUNNER),
    ("turn-gateway", LEGACY_RUNNER),
    ("turn-gateway-clock-edge", LEGACY_RUNNER),
    ("turn-replay", LEGACY_RUNNER),
    ("turn-runner", LEGACY_RUNNER),
    ("turn-submit-admission", LEGACY_RUNNER),
    ("turn-submit-panic-close", LEGACY_RUNNER),
    ("ui-server-parity-isolation", LEGACY_RUNNER),
    ("unsafe-config-boundary", LEGACY_RUNNER),
    ("upload-gateway", LEGACY_RUNNER),
    ("user-dialog-channel", LEGACY_RUNNER),
    ("user-dialog-guard-order", LEGACY_RUNNER),
    ("user-dialog-lock-scope", LEGACY_RUNNER),
    ("utf16-residual-sort", LEGACY_RUNNER),
    ("web-conversion-lifecycle", LEGACY_RUNNER),
    ("webhook-channel", LEGACY_RUNNER),
    ("wecom-callback", LEGACY_RUNNER),
    ("weixin-channel", LEGACY_RUNNER),
    ("workspace-snapshot-gate", LEGACY_RUNNER),
    ("workspace-snapshot-nonutf8", LEGACY_RUNNER),
    ("zone-read-bound", LEGACY_RUNNER),
];

/// Walk every file under `root`, at any depth (fix-r12 MM boundary P2: the
/// depth-1 `read_dir` silently skipped nested twins).
fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).unwrap_or_else(|error| {
            panic!("#2692 fix-r12: cannot read {}: {error}", dir.display())
        });
        for entry in entries {
            let entry = entry.unwrap_or_else(|error| {
                panic!("#2692 fix-r12: cannot read {}: {error}", dir.display())
            });
            let path = entry.path();
            if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn capture(mut command: Command, label: &str) -> Result<String, String> {
    let output = command.output().map_err(|error| {
        format!("negative anchor selfcheck requires python3; could not start it: {error}")
    })?;
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err(format!("{label}: {}\n{report}", output.status));
    }
    Ok(report)
}

fn python_output(entry: &Entry, fixtures: &Path) -> Result<String, String> {
    let mut command = Command::new("python3");
    command.arg("-I").arg("-B");
    if entry.discover {
        command
            .arg("-m")
            .arg("unittest")
            .arg("discover")
            .arg("-s")
            .arg(fixtures.join(entry.script))
            .arg("-p")
            .arg(entry.args[0]);
    } else {
        command.arg(fixtures.join(entry.script)).args(entry.args);
    }
    let label = entry.path();
    let report = capture(command, &label)?;
    if entry.ran {
        require_unittest_summary(&label, &report)?;
    }
    Ok(report)
}

/// #2692 fix-r12 (review DS boundary P2): exit 0 alone did not prove a
/// red-side module ran anything -- `Ran 0 tests` still exits 0, so an emptied
/// or renamed test module kept the preflight green. The last `Ran N` line is
/// the authoritative summary (a parent prints its own after any children).
fn require_unittest_summary(label: &str, report: &str) -> Result<(), String> {
    let mut ran: Option<usize> = None;
    let mut ok = false;
    for line in report.lines() {
        if let Some(rest) = line.strip_prefix("Ran ")
            && let Some(count) = rest
                .split_whitespace()
                .next()
                .and_then(|token| token.parse::<usize>().ok())
        {
            ran = Some(count);
        }
        if line == "OK" || line.starts_with("OK (") {
            ok = true;
        }
    }
    match ran {
        None => Err(format!(
            "{label}: the unittest summary `Ran N tests` is missing:\n{report}"
        )),
        Some(0) => Err(format!(
            "{label}: ran 0 tests -- an emptied or renamed red-side module still exits 0:\n{report}"
        )),
        Some(_) if ok => Ok(()),
        Some(_) => Err(format!("{label}: the unittest run is not OK:\n{report}")),
    }
}

#[test]
fn negative_fixture_anchors_and_checker_controls() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for entry in PREFLIGHT {
        let report = python_output(entry, &fixtures)
            .unwrap_or_else(|error| panic!("#1110 fixture preflight: {error}"));
        print!("{report}");
    }
}

#[test]
fn a_missing_run_tests_summary_is_an_error() {
    // #2692 fix-r12 (DS boundary P2): the judgment's own red side.
    assert!(require_unittest_summary("label", "Ran 3 tests in 0.1s\n\nOK\n").is_ok());
    let empty = require_unittest_summary("label", "Ran 0 tests in 0.0s\n\nOK\n").unwrap_err();
    assert!(empty.contains("ran 0 tests"), "{empty}");
    let missing = require_unittest_summary("label", "all quiet\n").unwrap_err();
    assert!(missing.contains("`Ran N tests` is missing"), "{missing}");
    let failed = require_unittest_summary("label", "Ran 2 tests in 0.1s\n\nFAILED (failures=1)\n")
        .unwrap_err();
    assert!(failed.contains("not OK"), "{failed}");
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
    // #2692 fix-r12 (review MM boundary P2): the scan walks to arbitrary
    // depth -- a nested twin (`fixtures/x/y/test_bound_test_gate.py`) was
    // silently skipped by the depth-1 read_dir. failure-guard's third-named
    // test_verdict.py joins the two canonical shapes through the same list.
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut twins = Vec::new();
    for path in walk_files(&fixtures) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if RED_SIDE_NAMES.contains(&name) {
            twins.push(
                path.strip_prefix(&fixtures)
                    .expect("the walk starts at fixtures")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    twins.sort();
    twins.dedup();
    assert!(
        !twins.is_empty(),
        "#2692 fix-r10: discovery found no red-side twins under {}",
        fixtures.display()
    );
    for script in &twins {
        assert!(
            PREFLIGHT.iter().any(|entry| entry.path() == *script),
            "#2692 fix-r11 (review boundary#1): the red-side twin {script} exists in \
             tests/fixtures but is not registered in the PREFLIGHT list above"
        );
    }
}

#[test]
fn mutation_runner_dirs_carry_a_red_side_twin_or_a_registered_exemption() {
    // #2692 fix-r12 (review MM sibling P2#4): the twin names above only
    // cover what someone already named. The census works from the other
    // side: every directory that holds a mutation runner (at any depth)
    // must carry an in-tree red-side twin, or carry a TWIN_EXEMPTIONS
    // entry with a one-line reason. A runner whose red side was deleted --
    // or never named -- therefore fails here instead of silently never being
    // red-checked. Exemptions must stay live and unique: a stale or
    // duplicated entry hides the very drift the census exists to catch.
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut runner_dirs = Vec::new();
    for path in walk_files(&fixtures) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if RUNNER_NAMES.contains(&name) {
            runner_dirs.push(
                path.parent()
                    .expect("a walked file always has a parent")
                    .strip_prefix(&fixtures)
                    .expect("the walk starts at fixtures")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    runner_dirs.sort();
    runner_dirs.dedup();
    assert!(
        !runner_dirs.is_empty(),
        "#2692 fix-r12: the census found no mutation runners under {}",
        fixtures.display()
    );
    for dir in &runner_dirs {
        let has_red_side = RED_SIDE_NAMES
            .iter()
            .any(|name| fixtures.join(dir).join(name).is_file());
        if has_red_side {
            continue;
        }
        assert!(
            TWIN_EXEMPTIONS.iter().any(|(exempt, _)| exempt == dir),
            "#2692 fix-r12 (MM sibling P2#4): {dir} holds a mutation runner but no \
             red-side twin (test_bound_test_gate.py / test_literal_anchors.py) and no \
             TWIN_EXEMPTIONS entry with a one-line reason"
        );
    }
    let live: HashSet<&str> = runner_dirs.iter().map(String::as_str).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    for (dir, _reason) in TWIN_EXEMPTIONS {
        assert!(
            live.contains(dir),
            "#2692 fix-r12: stale TWIN_EXEMPTIONS entry: {dir} no longer holds a \
             mutation runner"
        );
        assert!(
            seen.insert(dir),
            "#2692 fix-r12: duplicate TWIN_EXEMPTIONS entry: {dir}"
        );
    }
}

#[test]
fn missing_python_is_an_error() {
    // A nonexistent child of a real source file cannot resolve to an executable.
    let missing =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/negative_anchor_selfcheck.rs/python3");
    let error = capture(Command::new(missing), "unused.py").unwrap_err();
    assert!(error.contains("requires python3"), "{error}");
}

#[test]
fn checker_failure_is_an_error() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut command = Command::new("python3");
    command
        .arg("-I")
        .arg("-B")
        .arg(fixtures.join("anchor_selfcheck.py"))
        .arg("--unknown-selfcheck-option");
    let error = capture(command, "anchor_selfcheck.py").unwrap_err();
    assert!(error.contains("unrecognized arguments"), "{error}");
}

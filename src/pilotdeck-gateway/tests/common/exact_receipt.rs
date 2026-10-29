//! An `--exact` child exit is a receipt only when libtest itself reports the
//! one expected test: a renamed filter matches nothing and still exits 0, and
//! a quiet/terse format drops the per-test line (the #273 / fix-r1 lesson).
/// Criterion: exit 0, exactly one `running 1 test` banner, exactly one
/// `test result:` summary reporting one passing test (same literal prefix as
/// `src/test_process.rs` `run_case`), and the per-test `... ok` line the
/// `tests/fixtures/exact-rerun-receipt/` executor keys on. Keep the copy in
/// `src/pilotdeck-cli/src/skills_migrate.rs` in sync (no shared crate there),
/// and the Python twin `libtest_receipt` in
/// `src/pilotdeck-yaml/tests/run_resource_scaling.py` (no shared language):
/// the three drift independently, and drift only ever loosens a check.
///
/// #942 fix-r1/fix-r2: the exact `test <name> ... ok` line assumes the
/// multithreaded libtest layout, where the name+result line is emitted
/// atomically after the test body. Single-threaded libtest prints
/// `test <name> ... ` *before* the body runs, so a child that prints to
/// stdout splices its output into that line under an ambient
/// `RUST_TEST_THREADS=1`. The pin rule is therefore: **a spawn feeding this
/// criterion must pin `--test-threads=2` whenever its child may print to
/// stdout — directly or through a shared helper** (the explicit flag
/// overrides the env default). "This file has no println" is *not* an
/// exemption: `common/baseline.rs` `regenerates_identically` prints for its
/// callers (r3 review Codex P1). A child that provably never writes stdout
/// keeps the whole-line receipt intact in either layout and needs no pin.
/// Current exemptions, each verified against its child's full call path:
/// the entries/snapshots *worker* spawns (bodies silent; worker stdout goes
/// to log files, and the locale rerun's output is captured, never echoed),
/// `plugin_traversal.rs` children (loader path prints nothing),
/// `skills_migrate.rs`'s umask child (no baseline helper in pilotdeck-cli),
/// and the deliberate `--test-threads=1` probes (`overview_lock_tests.rs`,
/// `file_memory.rs`, `test_process.rs`, `projector_cwd_tests.rs`,
/// `timing.rs`; eprintln/marker criteria of their own). Sites whose
/// criterion is not the whole line (contains/sentinel/handshake:
/// `child_signals.rs`, `oracle_locale.rs`, `workspace_snapshot.rs`,
/// `grep.rs`) are layout-insensitive by construction. The whole-line
/// criterion also assumes newline-terminated child output: child markers
/// must use `println!`, never a bare `print!`.
/// Regression guards: the
/// `run_events_pathological_depth_last_seq_is_bounded_under_single_thread_ambient`
/// test (`src/runtime/turn/read_tests.rs`, direct marker) and the
/// `real_ts_entries_locale_receipt_survives_single_thread_ambient` /
/// `real_ts_snapshots_locale_receipt_survives_single_thread_ambient` tests
/// (`tests/edgeclaw_{entries,snapshots}.rs`, indirect print through
/// `common/baseline.rs`).
pub(crate) fn single_pass(status: &std::process::ExitStatus, test: &str, output: &str) -> bool {
    let summaries: Vec<&str> = output
        .lines()
        .filter(|line| line.starts_with("test result:"))
        .collect();
    let receipt = format!("test {test} ... ok");
    status.success()
        && output
            .lines()
            .filter(|line| *line == "running 1 test")
            .count()
            == 1
        && summaries.len() == 1
        && summaries[0].starts_with("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;")
        && output.lines().any(|line| line == receipt)
}

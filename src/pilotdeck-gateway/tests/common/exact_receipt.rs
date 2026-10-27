//! An `--exact` child exit is a receipt only when libtest itself reports the
//! one expected test: a renamed filter matches nothing and still exits 0, and
//! a quiet/terse format drops the per-test line (the #273 / fix-r1 lesson).
/// Criterion: exit 0, exactly one `running 1 test` banner, exactly one
/// `test result:` summary reporting one passing test (same literal prefix as
/// `src/test_process.rs` `run_case`), and the per-test `... ok` line the
/// `tests/fixtures/exact-rerun-receipt/` executor keys on. Keep the copy in
/// `src/pilotdeck-cli/src/skills_migrate.rs` in sync (no shared crate there).
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

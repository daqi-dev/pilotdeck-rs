//! Shared constants of the native entry's executable-level tests: the ones
//! `tests/mode_routing.rs` (the entry's routing switch, #1354) and
//! `tests/switch_e2e.rs` (the switch machine behind the same entry, #1353)
//! both observe on the real `pilotdeck-gateway` child. One spelling per
//! product fact; role-specific budgets stay in their own files.
//!
//! `tests/native_process.rs` keeps its own spellings: its advertisement
//! literals are needle-pinned by the frozen `native-entry-authority` binding
//! (`tests/fixtures/native-entry-authority/native_entry_binding.py`), so
//! migrating them here would break a frozen device. Its `VALID_HOME_CONFIG`
//! copy is *not* needle-pinned — consolidating it here is possible follow-up
//! work, deliberately not a frozen-adjacent edit in this round.
use std::time::Duration;

/// The child's own advertisement line once it is serving, exactly as the
/// product writes it (`eprintln!("native listening {address}")` in
/// `src/native_process.rs`).
pub const NATIVE_LISTENING: &str = "native listening";
/// A slow host still reaches the advertisement eventually; silence past this
/// bound is a launch failure, never a pass.
pub const ADVERTISEMENT_BUDGET: Duration = Duration::from_secs(120);
/// The whole idle switch: every face is empty or immediate, but the S4
/// teardown still flushes durable channel state and disposes the project, so
/// a slow host needs room. The faces' own bounds (production timeouts over
/// the `--drain-ms` budget) bound a hung face first.
pub const SWITCH_DEADLINE: Duration = Duration::from_secs(60);
/// The child prints a handful of lines; evidence retained past this bound is
/// dropped (the read itself keeps draining so the child can still exit —
/// a stopped read would block it on a full pipe).
pub const STDERR_LIMIT: u64 = 64 * 1024;
/// The machine's S2 opening line (`eprintln!("native stopping")` in
/// `src/native_process.rs`): the positive evidence that the SIGTERM really
/// entered the switch machine — exit 0 alone would also pass for a child
/// that self-exited after its advertisement.
pub const NATIVE_STOPPING: &str = "native stopping";
/// One complete valid profile: nothing but the routing/signal itself may
/// decide the child's fate (no `cron:` section — the switch's S1 has nothing
/// to stop in this embedding).
pub const VALID_HOME_CONFIG: &str = "schemaVersion: 1\nagent:\n  model: test/test\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:1\n      apiKey: test-only\n      models:\n        test: {}\n";

/// Whether one *finished* line is the child's listening advertisement: the
/// phrase, a space, then a non-empty address with no whitespace of its own.
/// A diagnostic that merely mentions the phrase is not proof that something
/// is serving (the #1401 rule both sibling tests anchor on). The byte-level
/// spelling in `tests/native_process.rs` is frozen-bound there (see the
/// module header); this is the `&str` spelling for the two siblings.
pub fn advertises(line: &str) -> bool {
    match line
        .strip_prefix(NATIVE_LISTENING)
        .map(|rest| rest.strip_prefix(' '))
    {
        Some(Some(address)) => !address.is_empty() && !address.contains(char::is_whitespace),
        _ => false,
    }
}

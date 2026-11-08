//! Shared constants of the native entry's executable-level tests: the ones
//! `tests/mode_routing.rs` (the entry's routing switch, #1354) and
//! `tests/switch_e2e.rs` (the switch machine behind the same entry, #1353)
//! both observe on the real `pilotdeck-gateway` child. One spelling per
//! product fact; role-specific budgets stay in their own files.
//!
//! `tests/native_process.rs` keeps its own spellings: its advertisement
//! literals are needle-pinned by the frozen `native-entry-authority` binding
//! (`tests/fixtures/native-entry-authority/native_entry_binding.py`), so
//! migrating them here would break a frozen device. Everything else about
//! the same child it shares with the two siblings through this file
//! (#2510 item 2): the `VALID_HOME_CONFIG` profile its own children load has
//! its one encoding here.
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

/// The address of one *finished* advertisement line, or `None` for any line
/// `advertises` rejects. The judgment stays in the predicate; this only
/// extracts what it accepted, so the address a sibling connects to is the
/// address the shared rule judged — never a second hand-written prefix walk
/// that could drift on its own (#2352: `tests/switch_e2e.rs` and the
/// `native_child_serves` loop each carried one).
pub fn advertised_address(line: &str) -> Option<&str> {
    let address = line.strip_prefix(NATIVE_LISTENING)?.strip_prefix(' ')?;
    advertises(line).then_some(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The shape lock for the shared `&str` spellings (#2352: the original
    // complaint was a copy without the shape-lock test silently drifting).
    // The byte-level twin in `tests/native_process.rs` additionally requires
    // the finished newline; `lines()` here already delivers finished lines.
    #[test]
    fn advertisement_shape_is_locked() {
        for (line, address) in [
            ("native listening 127.0.0.1:4321", "127.0.0.1:4321"),
            ("native listening [::1]:4321", "[::1]:4321"),
        ] {
            assert!(advertises(line), "must count as an advertisement: {line:?}");
            assert_eq!(
                advertised_address(line),
                Some(address),
                "the address the predicate judged: {line:?}"
            );
        }
        for line in [
            "native listening",
            "native listening ",
            "native listening 127.0.0.1:4321 extra",
            "native listening: 127.0.0.1:4321",
            "warn: native listening 127.0.0.1:4321",
            "",
        ] {
            assert!(
                !advertises(line),
                "must not count as an advertisement: {line:?}"
            );
            assert_eq!(advertised_address(line), None, "no address: {line:?}");
        }
    }
}

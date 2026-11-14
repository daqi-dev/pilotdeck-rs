//! Shared constants of the native entry's executable-level tests: the ones
//! `tests/mode_routing.rs` (the entry's routing switch, #1354) and
//! `tests/switch_e2e.rs` (the switch machine behind the same entry, #1353)
//! both observe on the real `pilotdeck-gateway` child. One spelling per
//! product fact; role-specific budgets stay in their own files.
//!
//! `tests/native_process.rs` keeps its own advertisement spellings: its
//! advertisement literals are needle-pinned by the frozen
//! `native-entry-authority` binding
//! (`tests/fixtures/native-entry-authority/native_entry_binding.py`), so
//! migrating them here would break a frozen device. Everything else about
//! the same child it shares with the two siblings through this file
//! (#2510 item 2): the `VALID_HOME_CONFIG` profile its own children load has
//! its one encoding here, and the stop-side timing segments both siblings
//! duplicate live here too (#2511 item 1 — the steady-state WS upgrade, the
//! SIGTERM handoff, and the completed-switch verdicts).
use std::{process::ExitStatus, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

// The RFC 6455 public handshake sample key the steady-state upgrade below
// sends: internal to this module since the upgrade moved here, so the two
// consumers stopped spelling the nonce themselves (#2511 item 1).
#[path = "native_ws_sample.rs"]
mod native_ws_sample;
use native_ws_sample::RFC6455_SAMPLE_NONCE;

// The machine's halt-line prefix has exactly one home (#2511 item 1): the
// non-panicking console line in `src/native_process.rs` prints it, and the
// completed-switch verdicts below read it back from the child's tail.
use pilotdeck_gateway::native_process::NATIVE_SWITCH_HALTED;

/// The child's own advertisement line once it is serving, exactly as the
/// product writes it (`console_log_line` on stderr in `src/native_process.rs`;
/// the #2558 fix changed only the panic behavior of the write, not the bytes).
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
/// dropped. The two consumers spell the bound differently (#2511 item 2): the
/// switch e2e read `take`s the stream itself, so its read stops at the bound
/// (its child is bounded by `kill_on_drop` and the switch deadline anyway),
/// while mode_routing keeps draining past the bound and bounds only
/// retention — a stopped read there would block a chatty child on a full
/// pipe.
pub const STDERR_LIMIT: u64 = 64 * 1024;
/// The machine's S2 opening line (`console_log_line` on stderr in
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

/// One steady-state probe between the advertisement and the signal: complete
/// a real WS upgrade against the advertised address (the RFC 6455 public
/// sample key), proving the accept loop is live from launch — the phased run
/// the switch later drives is constructed before the signal wait, so serving
/// must not wait for SIGTERM (a regression here hangs the upgrade instead of
/// the clean exit). Shared by both sibling tests (#2511 item 1: the segment
/// was duplicated whole).
pub async fn steady_state_ws_upgrade(address: &str) {
    let mut client = timeout(ADVERTISEMENT_BUDGET, TcpStream::connect(address))
        .await
        .expect("#2511 native steady-state connect deadline")
        .expect("#2511 native steady-state connect");
    client
        .write_all(
            format!(
                "GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\n\
                 Upgrade: websocket\r\nSec-WebSocket-Key: {RFC6455_SAMPLE_NONCE}\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("#2511 native steady-state upgrade write");
    let mut header = Vec::new();
    timeout(ADVERTISEMENT_BUDGET, async {
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0u8; 1];
            client
                .read_exact(&mut byte)
                .await
                .expect("#2511 native steady-state upgrade byte");
            header.push(byte[0]);
        }
    })
    .await
    .expect("#2511 native steady-state upgrade deadline");
    assert!(
        header.starts_with(b"HTTP/1.1 101"),
        "#2511 native steady-state upgrade status, got {}",
        String::from_utf8_lossy(&header)
    );
}

/// The signal the production entry is built around: SIGTERM the
/// still-serving child. `kill_on_drop` only SIGKILLs, so the wait afterwards
/// observes the machine's own exit, not this. A child that already exited
/// cannot take the signal: report its real status instead of surfacing a
/// bare signal-send failure later; a delivery failure is reported with the
/// child's own outcome (bounded by the switch deadline) so the panic names
/// both. rustix's safe `kill` wrapper: the workspace forbids `unsafe`, so the
/// raw `libc::kill` call is made through the same syscall's wrapper (the
/// `handle_path.rs` precedent).
pub async fn sigterm_child(child: &mut tokio::process::Child) {
    if let Ok(Some(status)) = child.try_wait() {
        panic!("#2511 native child exited before the signal: {status}");
    }
    let pid = rustix::process::Pid::from_raw(child.id().expect("#2511 native child pid") as i32)
        .expect("#2511 native child pid in range");
    if let Err(error) = rustix::process::kill_process(pid, rustix::process::Signal::TERM) {
        match timeout(SWITCH_DEADLINE, child.wait()).await {
            Ok(Ok(status)) => {
                panic!("#2511 SIGTERM delivery failed: {error}; child status: {status}")
            }
            Ok(Err(wait_error)) => {
                panic!("#2511 SIGTERM delivery failed: {error}; child wait: {wait_error}")
            }
            Err(_) => panic!(
                "#2511 SIGTERM delivery failed: {error}; child did not exit within the switch bound"
            ),
        }
    }
}

/// The completed-switch verdicts over the collected tail and exit status: the
/// machine's S2 opening line must be there (the signal really entered the
/// machine — exit 0 alone would also pass for a child that self-exited after
/// its advertisement), an idle server must not halt, and the completed
/// machine is the clean exit. The tail is asserted non-empty first: a
/// "contains no halt line" verdict over zero output would be vacuously true
/// (#2511 item 1).
pub fn assert_completed_switch(tail: &[String], status: &ExitStatus) {
    assert!(
        !tail.is_empty(),
        "#2511 the child's switch tail is empty; the outcome is unjudgeable"
    );
    assert!(
        tail.iter().any(|line| line == NATIVE_STOPPING),
        "#2511 the child never announced the switch; tail: {tail:?}"
    );
    assert!(
        !tail
            .iter()
            .any(|line| line.starts_with(NATIVE_SWITCH_HALTED)),
        "#2511 the switch over an idle server must complete, not halt; tail: {tail:?}"
    );
    assert!(
        status.success(),
        "#2511 the completed switch exits 0, got {status}; tail: {tail:?}"
    );
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

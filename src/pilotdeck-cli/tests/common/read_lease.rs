//! #1885 shared read lease over committed fixture bytes.
//!
//! The gateway negative controls mutate guarded sources in place while an
//! exclusive `flock` on `<root>/.negative-source.lock` is held (the fixtures'
//! `negative_process.py` `TreeLock`). A resident test reading those bytes
//! without that lease races the mutation window and can false-fail a golden
//! sha256; a SIGKILLed runner's leftover mutant keeps it red until the next
//! run restores. A shared lock suffices: readers never exclude each other, and
//! every writer restores its guarded sources before releasing.
//!
//! Lock taxonomy, for anyone pointing at this header as the canonical
//! wording: one lock file, two acquisition modes. Writers take it
//! exclusively (`tree_lock`, LOCK_EX); the resident readers in this
//! module take it shared (`fixture_read_lease`, `try_lock_shared`) and
//! are thereby excluded from writer windows but never from each other.
//! Whether any *other* command takes the lock at all is a per-runner
//! fact, not a property of reading or writing: `utf16-residual-sort/
//! negative_controls.py --check-anchors` takes it; `issue-1246/
//! negative_controls.py --check` takes it (its check runs inside the
//! tree lock), and so does `issue-2495/negative_controls.py --check`
//! (its stale scan runs inside the tree lock); `always-on-2294/
//! negative_controls.py --check` does not; a plain `cargo test` takes
//! none itself, but the resident tests that call `fixture_read_lease`
//! hold it shared for their reads (`sdk_signal_exit.rs` and `common/signal_replay.rs` here). Nothing that
//! never takes the lock is excluded from anything — any lockless command
//! can read a mid-window mutant and false-fail. A writer SIGKILLed while
//! inside the mutation window leaves the mutant on disk (its `finally`
//! restore never ran); the lock dies with the process, so an absent lock
//! proves nothing about tree cleanliness.
//!
//! #1662 started this in `sdk_signal_exit.rs`; #1885 lifts it into this shared
//! module so `signal_replay.rs` (96-cell replay, the SIGINT wrapper probe and
//! both D1 ledger readers) uses the exact same lease instead of a second
//! protocol.
#![allow(dead_code)]

use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

/// Upper bound for waiting out one *exclusive* holder of the tree lock.
///
/// This is **not** the writer's acquisition budget: `negative_process.mutating_sources`
/// bounds its own acquisition at `lock_timeout` (900 s by default), while the
/// holder's window has no fixed length. This constant bounds how long a reader
/// tolerates a busy workspace before failing with its own diagnostic. The
/// paired GREEN control's harness timeout (`CARGO_TIMEOUT = 1800` in
/// `fixtures/sdk-signal-read-lease/negative_controls.py`) is deliberately
/// larger, so a lease timeout is reported by *this* reader's marker instead of
/// surfacing as a generic killed-test timeout (#1885).
pub(crate) const READ_LEASE_WAIT: Duration = Duration::from_secs(1200);

/// One stderr line per lease acquisition: the measured wait (ms), the instant of
/// the first attempt (epoch ms), the instant of the first *refused* attempt
/// (epoch ms, 0 when the first attempt succeeded) and the acquisition instant
/// (epoch ms). The paired GREEN control in `sdk-signal-read-lease/` uses the
/// refused attempt as its witness: `WouldBlock` observed inside the writer's
/// published hold -- less the upper-end margin `WITNESS_MARGIN_MS` in that
/// fixture's `negative_controls.py`, which covers the truncated millisecond
/// bucket -- proves the reader met that lock, and no scheduling delay can
/// manufacture it. Both sides must agree on this exact spelling.
pub(crate) const READ_LEASE_MARKER: &str = "#1662 read lease wait ms=";

/// Set by a runner that already holds the exclusive tree lock for its whole
/// run, to that run's repository root.
///
/// Its child test processes must **not** take the shared lease: `flock` is per
/// open file description, so a child asking for `LOCK_SH` while its own parent
/// holds `LOCK_EX` blocks until the parent releases — a self-deadlock that no
/// amount of waiting can resolve, not contention. A runner that injects this
/// value is the writer for the bytes its children read, so the short-circuit is
/// sound; an unrelated tree's value never matches and never bypasses.
const LEASE_HELD_ROOT_ENV: &str = "PDRS_NEGATIVE_LEASE_HELD_ROOT";

/// Wall-clock milliseconds since the Unix epoch.
fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0)
}

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("#1662 repository root above the pilotdeck-cli manifest")
}

fn lease_bypassed() -> bool {
    let Some(declared) = std::env::var_os(LEASE_HELD_ROOT_ENV) else {
        return false;
    };
    let declared = PathBuf::from(declared);
    match (declared.canonicalize(), repo_root().canonicalize()) {
        (Ok(declared), Ok(actual)) => declared == actual,
        _ => declared == repo_root(),
    }
}

/// Acquire the shared fixture read lease, or `None` when a parent run already
/// holds the exclusive tree lock (`LEASE_HELD_ROOT_ENV`).
///
/// The returned file is the guard: keep it alive for as long as the fixture
/// bytes are being read. Exceeding [`READ_LEASE_WAIT`] fails here with this
/// module's marker instead of hanging.
pub(crate) fn fixture_read_lease() -> Option<fs::File> {
    if lease_bypassed() {
        return None;
    }
    let path = repo_root().join(".negative-source.lock");
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .expect("#1662 negative source lock file must open");
    let deadline = Instant::now() + READ_LEASE_WAIT;
    let started = Instant::now();
    let first = epoch_ms();
    // The instant of the first *refused* attempt. `try_lock_shared` returning
    // `WouldBlock` is a direct observation that an exclusive holder was there at
    // that instant; nothing else in the process can produce it. The paired
    // control requires this instant to fall inside the writer's published hold,
    // which no scheduling delay can fake: a pause before the call, or a stall
    // after an uncontended acquisition, cannot move it.
    let mut blocked: Option<u128> = None;
    loop {
        match file.try_lock_shared() {
            Ok(()) => {
                let at = epoch_ms();
                eprintln!(
                    "{READ_LEASE_MARKER}{} first={first} blocked={} at={at}",
                    started.elapsed().as_millis(),
                    blocked.unwrap_or(0)
                );
                return Some(file);
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                if blocked.is_none() {
                    blocked = Some(epoch_ms());
                }
                assert!(
                    Instant::now() < deadline,
                    "#1662 fixture read lease wait exceeded {} s on {}: workspace busy -- an \
                     exclusive negative-control run holds the tree lock and may be mutating \
                     fixture bytes; refusing to read a possibly-mutated golden",
                    READ_LEASE_WAIT.as_secs(),
                    path.display()
                );
                thread::sleep(Duration::from_millis(50));
            }
            Err(std::fs::TryLockError::Error(error)) => {
                panic!("#1662 fixture read lease acquisition failed: {error}")
            }
        }
    }
}

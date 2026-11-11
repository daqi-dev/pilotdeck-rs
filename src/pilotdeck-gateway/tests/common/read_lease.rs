//! #1885 shared read lease over committed fixture bytes.
//!
//! The gateway negative controls mutate guarded sources in place while an
//! exclusive `flock` on `<root>/.negative-source.lock` is held (this crate's
//! `fixtures/negative_process.py` `TreeLock`). A resident test reading those
//! bytes without that lease races the mutation window and can false-fail a
//! golden sha256; a SIGKILLed runner's leftover mutant keeps it red until the
//! next run restores. A shared lock suffices: readers never exclude each other,
//! and every writer restores its guarded sources before releasing.
//!
//! Lock taxonomy, for anyone pointing at this header as the canonical
//! wording: the lock has one mechanism — `tree_lock`, LOCK_EX on that file —
//! and whether a command holds it is a per-runner fact, not a property of
//! reading or writing: `utf16-residual-sort/negative_controls.py
//! --check-anchors` takes it; a fixture's `--check` branch does not;
//! neither does a plain `cargo test`. Nothing that never takes the lock
//! is excluded from anything — any of them can read a mid-window mutant
//! and false-fail. A writer SIGKILLed while inside the mutation window
//! leaves the mutant on disk (its `finally` restore never ran); the lock
//! dies with the process, so an absent lock proves nothing about tree
//! cleanliness.
//!
//! This mirrors `src/pilotdeck-cli/tests/common/read_lease.rs`. Rust integration
//! tests are separate crates, so a helper cannot be shared without a dedicated
//! dev-dependency crate; the two copies must stay in sync (same marker spelling,
//! same bypass variable). `live_e2e.rs` is the only gateway consumer today.
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

/// One stderr line per lease acquisition. Spelled exactly like the CLI-side
/// constant: `fixtures/sdk-signal-read-lease/` parses this marker, and both
/// readers must remain indistinguishable to it.
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
        .expect("#1885 repository root above the pilotdeck-gateway manifest")
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
        .expect("#1885 negative source lock file must open");
    let deadline = Instant::now() + READ_LEASE_WAIT;
    let started = Instant::now();
    let first = epoch_ms();
    // The instant of the first *refused* attempt: `try_lock_shared` returning
    // `WouldBlock` is a direct observation that an exclusive holder was there.
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
                    "#1885 fixture read lease wait exceeded {} s on {}: workspace busy -- an \
                     exclusive negative-control run holds the tree lock and may be mutating \
                     fixture bytes; refusing to read a possibly-mutated golden",
                    READ_LEASE_WAIT.as_secs(),
                    path.display()
                );
                thread::sleep(Duration::from_millis(50));
            }
            Err(std::fs::TryLockError::Error(error)) => {
                panic!("#1885 fixture read lease acquisition failed: {error}")
            }
        }
    }
}

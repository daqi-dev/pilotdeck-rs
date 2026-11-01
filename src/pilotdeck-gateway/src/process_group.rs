//! Owned process-group helpers shared by the sidecar connection and the hook
//! command executor. Both spawn a leader in its own group — the sidecar via
//! `process_group(0)`, the hook executor via the detached `setsid` (#552),
//! whose session leader is also the group leader — and must not lose the
//! group while a descendant still holds a pipe.
//!
//! Cleanup policy belongs to the caller, not to this module: the sidecar
//! connection owns every descendant and clears the group after a natural exit,
//! while the hook executor mirrors TS `commandRunner` and only signals the
//! group on timeout/abort (a successful hook may leave its own background
//! process running). On timeout/abort the hook executor sends TERM first, then
//! keeps the leader unreaped for TS's 3 s grace before the final KILL; a
//! result that resolves at pipe EOF hands that deferred KILL to a background
//! task instead of cutting the grace short.
//!
//! The leader is observed with `WNOWAIT` so it stays a zombie (and therefore
//! its PGID cannot be reused) until the caller has sent the final group
//! signal; only then may `Child::wait` reap it.
//!
//! The background task runtime and the foreground bash runner additionally
//! await the leader's exit status through [`wait_exit_unreaped`].
use rustix::process::{Pid, Signal};
use std::time::Duration;

/// Send `signal` to the whole owned group. `None` means the child was not
/// spawned on an owned group (non-unix or a missing pid).
pub(crate) fn kill(group: Option<Pid>, signal: Signal) {
    #[cfg(unix)]
    if let Some(group) = group {
        let _ = rustix::process::kill_process_group(group, signal);
    }
    #[cfg(not(unix))]
    let _ = (group, signal);
}

/// Kills the process group on Drop unless disarmed after the leader is reaped.
pub(crate) struct GroupGuard(pub(crate) Option<Pid>);

impl GroupGuard {
    pub(crate) fn signal(&self, signal: Signal) {
        if let Some(group) = self.0 {
            kill(Some(group), signal);
        }
    }
    pub(crate) fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        self.signal(Signal::KILL);
    }
}

/// Whether the group still has at least one member (including zombies).
/// #1155: mirrored by `group_exists` in `tests/scratch_publication.rs` —
/// the integration crate cannot see this private module — so change the
/// two copies together.
pub(crate) fn group_exists(group: Pid) -> bool {
    rustix::process::test_kill_process_group(group).is_ok()
}

/// `WNOWAIT` observation of the leader's exit without reaping it.
///
/// Returns `Ok(false)` when the leader is still running (and also on platforms
/// without `WNOWAIT`, where callers must fall back to their bounded wait).
pub(crate) fn leader_exited_unreaped(group: Pid) -> Result<bool, rustix::io::Errno> {
    #[cfg(not(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    )))]
    {
        use rustix::process::{WaitId, WaitIdOptions, waitid};
        match waitid(
            WaitId::Pid(group),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(rustix::io::Errno::INTR) => Ok(false),
            Err(error) => Err(error),
        }
    }
    #[cfg(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    ))]
    {
        let _ = group;
        Ok(false)
    }
}

/// Exit code of an already-exited leader observed without reaping it.
///
/// `None` when the leader was killed by a signal, has not been observed as
/// exited, or the platform cannot wait with `WNOWAIT`. The zombie (and therefore
/// its PGID) stays reserved for the caller's later group signal + `Child::wait`.
pub(crate) fn leader_exit_code_unreaped(group: Option<Pid>) -> Option<i32> {
    let group = group?;
    // `WaitIdStatus::exit_status` is unavailable on these targets even when
    // `waitid` itself exists; fall back to the next `Child::wait` instead.
    #[cfg(not(any(
        target_os = "cygwin",
        target_os = "emscripten",
        target_os = "fuchsia",
        target_os = "horizon",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    )))]
    {
        use rustix::process::{WaitId, WaitIdOptions, waitid};
        match waitid(
            WaitId::Pid(group),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(status)) => status.exit_status(),
            _ => None,
        }
    }
    #[cfg(any(
        target_os = "cygwin",
        target_os = "emscripten",
        target_os = "fuchsia",
        target_os = "horizon",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    ))]
    {
        let _ = group;
        None
    }
}

/// Wait until the leader has exited without reaping it, or `limit` elapses.
///
/// `Ok(true)` means the leader exited; `Ok(false)` means the bounded wait
/// expired (or the platform cannot observe exit without reaping).
pub(crate) async fn wait_for_exit_unreaped(
    group: Option<Pid>,
    limit: Duration,
) -> Result<bool, rustix::io::Errno> {
    let Some(group) = group else {
        return Ok(true);
    };
    let observe = async {
        loop {
            match leader_exited_unreaped(group) {
                Ok(true) => return Ok(true),
                Ok(false) => tokio::time::sleep(Duration::from_millis(5)).await,
                Err(error) => return Err(error),
            }
        }
    };
    match tokio::time::timeout(limit, observe).await {
        Ok(result) => result,
        Err(_) => Ok(false),
    }
}

/// Wait until every member of the group is gone, bounded by `limit`.
pub(crate) async fn wait_for_group_empty(group: Option<Pid>, limit: Duration) {
    let Some(group) = group else {
        return;
    };
    let observe = async {
        while group_exists(group) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    let _ = tokio::time::timeout(limit, observe).await;
}

/// How the group leader ended, as `child.on("exit")` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LeaderExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

/// Resolves once `leader` has exited, without reaping it. `Ok(None)` when the
/// platform has no WNOWAIT; `Err` for a non-EINTR `waitid` failure (for
/// example ECHILD once someone else reaped it).
pub(crate) async fn wait_exit_unreaped(
    leader: Pid,
) -> Result<Option<LeaderExit>, rustix::io::Errno> {
    #[cfg(not(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    )))]
    loop {
        use rustix::process::{WaitId, WaitIdOptions, waitid};
        match waitid(
            WaitId::Pid(leader),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(status)) => {
                return Ok(Some(LeaderExit {
                    code: status.exit_status(),
                    signal: status.terminating_signal(),
                }));
            }
            Ok(None) => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error),
        }
    }
    #[cfg(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    ))]
    {
        let _ = leader;
        Ok(None)
    }
}

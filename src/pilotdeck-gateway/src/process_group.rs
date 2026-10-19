//! Owned process-group helpers shared by the sidecar connection and the hook
//! command executor. Both spawn a leader in its own group (`process_group(0)`)
//! and must not lose the group while a descendant still holds a pipe.
//!
//! Cleanup policy belongs to the caller, not to this module: the sidecar
//! connection owns every descendant and clears the group after a natural exit,
//! while the hook executor mirrors TS `commandRunner` and only signals the
//! group on timeout/abort (a successful hook may leave its own background
//! process running).
//!
//! The leader is observed with `WNOWAIT` so it stays a zombie (and therefore
//! its PGID cannot be reused) until the caller has sent the final group
//! signal; only then may `Child::wait` reap it.
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

/// Whether the group still has at least one member (including zombies).
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

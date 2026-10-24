//! The #386 human-approved exception: reset inherited child signals before exec.
//! The #552 human-approved addition: libuv's `detached` `setsid()` in the same
//! fork-to-exec window, for every spawn whose TS counterpart is detached (the
//! bash runner, background tasks, the ripgrep engine spawn and command hooks;
//! A6.5-BASH-OS-001 setsid half).
#![cfg(unix)]

use std::{io, os::unix::process::CommandExt, process::Command};

// libuv's Unix child-init range is 1..32, excluding unchangeable signals.
// This is 29 signals on macOS and Linux; Linux 32/33 (glibc's internal
// realtime signals), and all other realtime dispositions, are left alone.
// Build the list at compile time, never by querying libc after fork.
const CHILD_SIGNALS: [libc::c_int; 29] = {
    let mut signals = [0; 29];
    let mut signal = 1;
    let mut index = 0;
    while signal < 32 {
        if signal != libc::SIGKILL && signal != libc::SIGSTOP {
            signals[index] = signal;
            index += 1;
        }
        signal += 1;
    }
    signals
};

/// Install before spawn, after any caller-owned pre-exec hooks. Tokio callers
/// pass `as_std_mut()`. Only the forked child executes this hook; std retains
/// ownership of cwd, stdio, exec-error reporting, and the child handle.
#[allow(unsafe_code)]
pub fn install_child_signal_reset(command: &mut Command) {
    // SAFETY: pre_exec runs only in the single-threaded child after fork. The
    // closure captures nothing and uses only stack/POD data, a constant list,
    // and async-signal-safe sigemptyset/sigaction/sigprocmask. A zeroed sigaction
    // is valid on macOS/Linux (integer fields and nullable sa_restorer); its
    // mask is explicitly initialized before use and its handler is SIG_DFL.
    // All pointers refer to live stack storage; null old-action/mask pointers
    // request no output. Each error immediately reads thread-local errno via
    // last_os_error, which constructs an inline OS error without allocation.
    // No locks, environment access, formatting, destructors, or panics occur.
    unsafe {
        command.pre_exec(|| {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = libc::SIG_DFL;
            if libc::sigemptyset(&mut action.sa_mask) != 0 {
                return Err(io::Error::last_os_error());
            }
            for signal in CHILD_SIGNALS {
                if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            // Unblock only after every disposition has been reset.
            if libc::sigprocmask(libc::SIG_SETMASK, &action.sa_mask, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// `detached: true` (#552): libuv's child init calls `setsid()`, so the
/// child leads a fresh session — its own process group with no controlling
/// terminal, so commands cannot read the gateway's terminal (`/dev/tty`
/// opens fail ENXIO). Install before `install_child_signal_reset`, which
/// stays the last hook per its own contract; the run order relative to the
/// resets is immaterial (the steps are independent, unlike in libuv where
/// the resets come first). Installers are the spawns whose TS counterparts
/// are detached: the bash runner (with its shared background-task spawn),
/// the ripgrep engine spawn and command hooks. The sidecar and MCP stdio
/// spawns mirror TS spawns that are not detached. Callers that use this
/// must not also request `process_group(0)`: the forked child would already
/// be a group leader and `setsid` would fail with EPERM.
#[allow(unsafe_code)]
pub fn install_child_detach_session(command: &mut Command) {
    // SAFETY: pre_exec runs only in the single-threaded child after fork. The
    // closure captures nothing and calls only setsid, an async-signal-safe
    // syscall with no locks, environment access, allocation, formatting, or
    // destructors. Its error path reports the child's errno through
    // last_os_error, which constructs an inline OS error without allocating.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

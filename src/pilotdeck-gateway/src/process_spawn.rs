//! The #386 human-approved exception: reset inherited child signals before exec.
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
pub(crate) fn install_child_signal_reset(command: &mut Command) {
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

#[cfg(test)]
use crate::test_scratch as scratch;

#[cfg(test)]
#[path = "../tests/common/child_signals.rs"]
pub(crate) mod child_signals;

#[cfg(test)]
pub(crate) mod test_support {
    use super::child_signals::probe_path;
    use std::{path::Path, process::Command, time::Duration};

    pub(crate) fn assert_inherited() {
        let output = Command::new(probe_path())
            .arg("inherited")
            .output()
            .unwrap();
        assert!(output.status.success(), "parent injection was changed");
        assert_eq!(output.stdout, b"INT_IGN=1 TERM_BLOCKED=1\n");
    }

    pub(crate) fn assert_reset_report(report: &str) {
        print!("{report}");
        assert!(report.contains("checked=29 mask_empty=1"), "{report}");
        assert_eq!(
            report.matches("default=1 blocked=0").count(),
            29,
            "{report}"
        );
        #[cfg(target_os = "linux")]
        for key in ["SigIgn:", "SigBlk:"] {
            let line = report.lines().find(|line| line.starts_with(key)).unwrap();
            assert_eq!(u64::from_str_radix(line[7..].trim(), 16).unwrap(), 0);
        }
    }

    pub(crate) async fn assert_reset_report_file(path: &Path) {
        let report = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(report) = std::fs::read_to_string(path)
                    && report.contains("checked=29")
                {
                    return report;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("native reporter must finish");
        assert_reset_report(&report);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resets_native_dispositions_and_entire_mask() {
        child_signals::ChildSignalProbe::new()
            .launch("process_spawn::tests::native_signal_worker", true);
    }

    #[test]
    #[ignore = "isolated worker launched with inherited ignored/blocked signals"]
    fn native_signal_worker() {
        let probe = child_signals::probe_path();
        test_support::assert_inherited();
        let mut command = Command::new(&probe);
        command.arg("report");
        install_child_signal_reset(&mut command);
        let output = command.output().unwrap();
        let report = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{report}");
        test_support::assert_reset_report(&report);
        test_support::assert_inherited(); // The hook must never change the parent process.
    }
}

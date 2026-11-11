//! Test support for inherited child signal regressions.

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
        {
            fn status_bits(status: &str, key: &str) -> u64 {
                let value = status.lines().find_map(|line| line.strip_prefix(key));
                u64::from_str_radix(value.unwrap().trim(), 16).unwrap()
            }

            // The launcher's glibc posix_spawn can ignore its internal signals
            // before the injected worker starts. libuv resets 1..31 and leaves
            // every realtime disposition alone, even when inherited ignored.
            let inherited = std::fs::read_to_string("/proc/self/status").unwrap();
            const LOW_SIGNALS: u64 = (1 << 31) - 1;
            let expected_ignored = status_bits(&inherited, "SigIgn:") & !LOW_SIGNALS;
            assert_eq!(status_bits(report, "SigIgn:"), expected_ignored, "{report}");
            assert_eq!(status_bits(report, "SigBlk:"), 0, "{report}");
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
    use pilotdeck_child_signals::install_child_signal_reset;
    use std::process::Command;

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

//! Isolate inherited signal injection in a disposable worker, never libtest's
//! shared parent. C is necessary to inspect dispositions before Rust/sh startup.
use super::scratch;

use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub(crate) struct ChildSignalProbe(scratch::Scratch);

impl ChildSignalProbe {
    pub(crate) fn new() -> Self {
        let scratch = scratch::Scratch::new("child-signals");
        let source = scratch.path().join("probe.c");
        std::fs::write(&source, include_str!("child_signals.c")).unwrap();
        let output = Command::new("cc")
            .args(["-std=c99", "-D_GNU_SOURCE", "-Wall", "-Wextra", "-Werror"])
            .arg(&source)
            .arg("-o")
            .arg(scratch.path().join("probe"))
            .output()
            .expect("a C compiler is required for the native signal reporter");
        assert!(
            output.status.success(),
            "native signal reporter compilation: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Self(scratch)
    }

    pub(crate) fn launch(&self, test: &str, all: bool) {
        let probe = self.0.path().join("probe");
        let mut child = Command::new(&probe)
            .arg(if all { "launch-all" } else { "launch" })
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", test, "--ignored", "--nocapture"])
            .env("PDRS_CHILD_SIGNAL_PROBE", &probe)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // These fixed workers emit at most a few KiB. Bound a broken pre-exec
        // hook or wait path without ever signalling libtest's shared parent.
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut timed_out = false;
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                timed_out = true;
                let _ = child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("INT_IGN=1 TERM_BLOCKED=1"), "{stdout}");
        assert!(
            !timed_out && output.status.success(),
            "signal-injected worker {} (timed_out={timed_out}): {stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            stdout
                .lines()
                .filter(|line| *line == "running 1 test")
                .count(),
            1
        );
        let summaries: Vec<_> = stdout
            .lines()
            .filter(|line| line.starts_with("test result:"))
            .collect();
        assert_eq!(summaries.len(), 1, "worker result count: {stdout}");
        assert!(
            summaries[0].starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;"),
            "worker did not run: {stdout}"
        );
        print!("{stdout}");
    }
}

pub(crate) fn probe_path() -> std::path::PathBuf {
    let path = std::env::var_os("PDRS_CHILD_SIGNAL_PROBE")
        .expect("run through the signal-injecting parent test");
    let path = Path::new(&path).to_path_buf();
    assert!(path.is_absolute());
    path
}

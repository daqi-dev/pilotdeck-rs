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

    pub(crate) fn launch(&self, test: &str, all: bool) -> String {
        let probe = self.0.path().join("probe");
        // Regular files avoid waiting for EOF from inherited pipe writers in
        // grandchildren, even when the direct worker has already exited. They
        // are also the progress channel: file growth is real worker output.
        let stdout_path = self.0.path().join("worker.stdout");
        let stderr_path = self.0.path().join("worker.stderr");
        let mut child = Command::new(&probe)
            .arg(if all { "launch-all" } else { "launch" })
            .arg(
                std::env::current_exe()
                    .expect("#1109 signal-injected worker needs the test binary path"),
            )
            .args(["--exact", test, "--ignored", "--nocapture"])
            .env("PDRS_CHILD_SIGNAL_PROBE", &probe)
            // #1109: the calling parent verified the committed oracle once;
            // the worker must not re-run the git probe table under the
            // bounds below (the #978 scan killed it mid-verification at
            // load 90-106). Contract: a worker launched here never needs
            // PDRS_BASELINE, so a parent whose worker replays a committed
            // oracle must verify that oracle itself before launching (as
            // background_tasks and transcript_resolve do).
            .env_remove("PDRS_BASELINE")
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(&stdout_path).expect("#1109 worker stdout file"))
            .stderr(std::fs::File::create(&stderr_path).expect("#1109 worker stderr file"))
            .spawn()
            .expect("#1109 spawn the signal-injected worker");
        // These fixed workers emit at most a few KiB. Bound a broken pre-exec
        // hook or wait path without ever signalling libtest's shared parent.
        // #1109: the bound is now progress-aware. A silent worker gets the
        // historical 30 s; a worker that keeps producing real output (probe
        // dispositions, replay step receipts, libtest summary) may run up
        // to the total bound, because at load 90-106 the replay's real work
        // alone outgrew a flat 30 s. Neither wait is unbounded.
        let silent_bound = Duration::from_secs(30);
        let total_bound = Duration::from_secs(300);
        let start = Instant::now();
        let mut last_output = Instant::now();
        let mut seen = 0_u64;
        let status = loop {
            if let Some(status) = child
                .try_wait()
                .expect("#1109 poll the signal-injected worker")
            {
                break status;
            }
            let grown = std::fs::metadata(&stdout_path)
                .map(|meta| meta.len())
                .unwrap_or(seen);
            if grown > seen {
                seen = grown;
                last_output = Instant::now();
            }
            if last_output.elapsed() >= silent_bound || start.elapsed() >= total_bound {
                let kill_result = child.kill();
                // Diagnostic only, on the captured test stdout: the panic
                // payload itself stays one stable line, which is the shape
                // the strict per-test negative-control contract binds to
                // (#1109 fix-r2).
                println!("#1109 worker bound exceeded; kill result {kill_result:?}");
                let reap_deadline = Instant::now() + Duration::from_secs(2);
                while child
                    .try_wait()
                    .expect("#1109 reap the signal-injected worker")
                    .is_none()
                {
                    assert!(
                        Instant::now() < reap_deadline,
                        "#1109 signal-injected worker {test} still not reaped 2 s after the bound kill (kill result {kill_result:?})"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                // The total bound is checked first: retracting it to 0/30 s
                // must bind to the total-bound panic even when the silent
                // bound would also fire on the same poll.
                if start.elapsed() >= total_bound {
                    panic!(
                        "#1109 signal-injected worker {test} exceeded the {total_bound:?} total bound without finishing"
                    );
                }
                panic!(
                    "#1109 signal-injected worker {test} silent for {silent_bound:?} within the {total_bound:?} total bound"
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let stdout =
            std::fs::read_to_string(stdout_path).expect("#1109 read worker stdout receipt");
        let stderr =
            std::fs::read_to_string(stderr_path).expect("#1109 read worker stderr receipt");
        assert!(stdout.contains("INT_IGN=1 TERM_BLOCKED=1"), "{stdout}");
        assert!(
            status.success(),
            "signal-injected worker {status}: {stdout}\n{stderr}"
        );
        assert_eq!(
            stdout
                .lines()
                .filter(|line| *line == "running 1 test")
                .count(),
            1,
            "#1109 worker printed its libtest header exactly once"
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
        stdout
    }
}

pub(crate) fn probe_path() -> std::path::PathBuf {
    let path = std::env::var_os("PDRS_CHILD_SIGNAL_PROBE")
        .expect("run through the signal-injecting parent test");
    let path = Path::new(&path).to_path_buf();
    assert!(path.is_absolute());
    path
}

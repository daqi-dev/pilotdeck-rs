//! #1145 P08 phase one: preserve the TS observations before changing Rust.
//! This extraction guard does not claim Rust signal/lifecycle parity.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pdrs-1145-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).expect("#1145 exclusive extraction directory");
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run_node(directory: &Path, args: &[&std::ffi::OsStr], label: &str, budget: Duration) -> Vec<u8> {
    let stdout_path = directory.join(format!("{label}.stdout"));
    let stderr_path = directory.join(format!("{label}.stderr"));
    let stdout = fs::File::create_new(&stdout_path).expect("#1145 exclusive Node stdout log");
    let stderr = fs::File::create_new(&stderr_path).expect("#1145 exclusive Node stderr log");
    let mut child = Command::new(std::env::var_os("NODE22").unwrap_or_else(|| "node".into()))
        .args(args)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .expect("#1145 spawn selected Node");
    let mut deadline = Instant::now() + budget;
    let mut timeout = false;
    let status = loop {
        if let Some(status) = child.try_wait().expect("#1145 observe Node child exit") {
            break status;
        }
        if Instant::now() >= deadline {
            assert!(
                !timeout,
                "#1145 owned Node child did not reap within three seconds"
            );
            timeout = true;
            child
                .kill()
                .expect("#1145 terminate owned overdue Node child");
            // File redirection avoids descendant pipe retention. Reaping also
            // has its own bound instead of an unbounded wait after SIGKILL.
            deadline = Instant::now() + Duration::from_secs(3);
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stderr = fs::read_to_string(stderr_path).expect("#1145 read Node stderr log");
    assert!(!timeout, "#1145 bounded Node extraction: {label}: {stderr}");
    assert!(
        status.success(),
        "#1145 Node command succeeded: {label}: {stderr}"
    );
    fs::read(stdout_path).expect("#1145 read Node stdout log")
}

fn regenerates_identically(fixture: &str) {
    let scratch = Scratch::new();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    assert_eq!(
        run_node(
            &scratch.0,
            &["--version".as_ref()],
            "version",
            Duration::from_secs(20)
        ),
        b"v22.22.0\n",
        "#1145 selected Node must be v22.22.0"
    );
    let generated = scratch.0.join("extracted");
    run_node(
        &scratch.0,
        &[
            directory.join("generate.mjs").as_os_str(),
            generated.as_os_str(),
        ],
        "generate",
        Duration::from_secs(20),
    );
    assert_eq!(
        fs::read(generated.join("oracle.json")).expect("#1145 regenerated oracle exists"),
        fs::read(directory.join("oracle.json")).expect("#1145 recorded oracle exists"),
        "#1145 identical TS signal fixture"
    );
}

#[test]
#[ignore = "requires Node v22.22.0; extracts captured originals, does not execute TS"]
fn signal_oracle_regenerates_identically() {
    regenerates_identically("sdk-signals");
}

#[test]
#[ignore = "requires Node v22.22.0; adversarial capture-data validation"]
fn signal_capture_rejects_invalid_evidence() {
    let scratch = Scratch::new();
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sdk-signals/negative_controls.mjs");
    // 62 controls at two seconds each, plus startup/read overhead. The measured
    // full suite is under a second; keep a bounded allowance for busy hosts.
    run_node(
        &scratch.0,
        &[script.as_os_str()],
        "negative-controls",
        Duration::from_secs(62 * 2 + 12),
    );
}

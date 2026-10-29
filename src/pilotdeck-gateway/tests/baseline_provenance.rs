#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_protocol::parse_json;
use std::{
    fs,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

const ORACLE: &str = include_str!("fixtures/config-changes/oracle.json");
const SOURCE: &str = "src/pilot/config/classifyChanges.ts";
const COMMIT: &str = "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85";

fn provenance() -> serde_json::Value {
    // Fixture cases may contain lone UTF-16 surrogates; only its ASCII
    // provenance belongs in serde_json, not the JavaScript case payload.
    let oracle = parse_json(ORACLE).unwrap();
    let hash = oracle
        .get("sourceSha256")
        .unwrap()
        .get(SOURCE)
        .unwrap()
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap();
    serde_json::json!({"sourceCommit": COMMIT, "sourceSha256": {SOURCE: hash}})
}

#[test]
fn rejects_missing_empty_and_malformed_provenance() {
    baseline::oracle(ORACLE);
    let original = provenance();
    for (field, value) in [
        ("sourceCommit", serde_json::json!("wrong")),
        ("sourceSha256", serde_json::json!({})),
        ("sourceSha256", serde_json::json!({SOURCE: "not-a-digest"})),
        (
            "sourceSha256",
            serde_json::json!({"src/../README.md": "a".repeat(64)}),
        ),
    ] {
        let mut modified = original.clone();
        modified[field] = value;
        let parsed = parse_json(&modified.to_string()).unwrap();
        assert!(std::panic::catch_unwind(|| baseline::check(&parsed, None)).is_err());
    }
    for field in ["sourceCommit", "sourceSha256"] {
        let mut modified = original.clone();
        modified.as_object_mut().unwrap().remove(field);
        let parsed = parse_json(&modified.to_string()).unwrap();
        assert!(std::panic::catch_unwind(|| baseline::check(&parsed, None)).is_err());
    }
}

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn git(checkout: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command.arg("-C").arg(checkout).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "requires PDRS_BASELINE; clones a sparse, isolated local checkout"]
fn rejects_dirty_wrong_commit_forged_hash_and_hidden_source_changes() {
    let original = std::env::var("PDRS_BASELINE").expect("set PDRS_BASELINE");
    let scratch =
        Scratch(std::env::temp_dir().join(format!("pdrs-provenance-{}", uuid::Uuid::new_v4())));
    fs::create_dir(&scratch.0).unwrap();
    let checkout = scratch.0.join("baseline");
    git(
        &scratch.0,
        &[
            "clone",
            "--shared",
            "--no-checkout",
            &original,
            checkout.to_str().unwrap(),
        ],
    );
    git(
        &checkout,
        &[
            "sparse-checkout",
            "set",
            "--no-cone",
            &format!("/{SOURCE}"),
            "/README.md",
        ],
    );
    git(&checkout, &["checkout", "--detach", COMMIT]);
    let oracle = parse_json(ORACLE).unwrap();
    baseline::check(&oracle, Some(&checkout));
    fs::write(checkout.join("untracked.txt"), "allowed").unwrap();
    baseline::check(&oracle, Some(&checkout));
    let rejected = |value: &pilotdeck_protocol::JsValue| {
        assert!(std::panic::catch_unwind(|| baseline::check(value, Some(&checkout))).is_err())
    };
    let mut forged = provenance();
    forged["sourceSha256"][SOURCE] = serde_json::json!("0".repeat(64));
    rejected(&parse_json(&forged.to_string()).unwrap());
    fs::write(checkout.join("README.md"), "dirty unrelated tracked file").unwrap();
    rejected(&oracle);
    git(&checkout, &["add", "README.md"]);
    rejected(&oracle);
    git(
        &checkout,
        &["restore", "--staged", "--worktree", "README.md"],
    );
    let bytes = fs::read(checkout.join(SOURCE)).unwrap();
    for flag in ["assume-unchanged", "skip-worktree"] {
        git(&checkout, &["update-index", &format!("--{flag}"), SOURCE]);
        fs::write(checkout.join(SOURCE), "hidden change").unwrap();
        rejected(&oracle);
        fs::write(checkout.join(SOURCE), &bytes).unwrap();
        git(
            &checkout,
            &["update-index", &format!("--no-{flag}"), SOURCE],
        );
    }
    fs::remove_file(checkout.join(SOURCE)).unwrap();
    rejected(&oracle);
    git(&checkout, &["restore", "--worktree", SOURCE]);
    git(&checkout, &["checkout", "--detach", &format!("{COMMIT}^")]);
    rejected(&oracle);
}

#[cfg(unix)]
fn process_group_alive(pgid: i32) -> bool {
    // Signal 0 only probes group existence; it delivers nothing.
    rustix::process::Pid::from_raw(pgid)
        .is_some_and(|pid| rustix::process::test_kill_process_group(pid).is_ok())
}

/// Best-effort cleanup for a mutant that leaves the fake generator running.
#[cfg(unix)]
struct ProcessGroupGuard {
    pgid: i32,
    armed: bool,
}

#[cfg(unix)]
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if self.armed {
            // This is the fake generator's own group; it stays armed only
            // while an assertion is about to fail and unwind.
            if let Some(pid) = rustix::process::Pid::from_raw(self.pgid) {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
        }
    }
}

/// A descendant that inherits stdout/stderr keeps the pipes open after the
/// direct child exits or is killed, so the helper's deadline must terminate
/// the whole process group and bound its pipe drains.
#[cfg(unix)]
#[test]
fn generator_deadline_kills_descendants_that_hold_the_pipes() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("pdrs-generator-tree-{}", uuid::Uuid::new_v4())));
    fs::create_dir_all(&scratch.0).expect("#776 create fake generator scratch");
    // `/bin/sh` forks a sleeping descendant that inherits both pipes and
    // records both pids. `leader-waits` keeps the direct child alive until the
    // deadline; `leader-exits` makes it exit at once while the descendant
    // keeps the write ends open. Both are unbounded-drain scenarios.
    for (label, wait) in [("leader-waits", "; wait"), ("leader-exits", "")] {
        let pid_file = scratch.0.join(format!("{label}.pids"));
        let script = format!(
            "echo $$ > '{pids}'; sleep 120 & echo $! >> '{pids}'{wait}",
            pids = pid_file.display()
        );
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &script]);
        let started = Instant::now();
        let result = baseline::run_generator_with_deadline(
            &mut command,
            "fake-descendant-pipes",
            Duration::from_secs(1),
        );
        let elapsed = started.elapsed();
        let pids = fs::read_to_string(&pid_file)
            .expect("#776 fake generator records the leader and descendant pids");
        let pids: Vec<i32> = pids
            .split_whitespace()
            .map(|value| value.parse().expect("#776 pid must be numeric"))
            .collect();
        assert_eq!(
            pids.len(),
            2,
            "#776 fake generator records exactly two pids ({label})"
        );
        let mut guard = ProcessGroupGuard {
            pgid: pids[0],
            armed: process_group_alive(pids[0]),
        };
        let error = result.expect_err(&format!(
            "#776 [{label}] helper must fail at the total deadline instead of blocking on inherited pipes"
        ));
        assert!(
            error.contains("#280 fake-descendant-pipes: generator exceeded the 1s total deadline"),
            "#776 [{label}] deadline error must name the fixture and the total deadline: {error}"
        );
        let group_deadline = Instant::now() + Duration::from_secs(3);
        while process_group_alive(pids[0]) && Instant::now() < group_deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !process_group_alive(pids[0]),
            "#776 process group {} (descendant {}) survived the generator deadline ({label})",
            pids[0],
            pids[1]
        );
        guard.armed = false;
        assert!(
            elapsed < Duration::from_secs(6),
            "#776 [{label}] helper returned after {elapsed:?}, past the 1s deadline plus a bounded drain"
        );
    }
}

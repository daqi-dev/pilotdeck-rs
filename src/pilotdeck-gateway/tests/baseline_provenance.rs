#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_protocol::parse_json;
use std::{fs, path::Path, process::Command};

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

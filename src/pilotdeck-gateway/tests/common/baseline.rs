//! Provenance checks shared by unit and integration fixture replays.
use pilotdeck_protocol::{JsValue, parse_json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, process::Command};

const BASELINE_COMMIT: &str = "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85";

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("provenance string")
        .to_utf8()
        .unwrap()
}

pub(crate) fn oracle(json: &str) -> JsValue {
    let oracle = parse_json(json).expect("oracle.json must be valid JSON");
    let baseline = std::env::var_os("PDRS_BASELINE");
    check(&oracle, baseline.as_deref().map(Path::new));
    oracle
}

pub(crate) fn check(oracle: &JsValue, baseline: Option<&Path>) {
    // Older corpora use commit/baselineCommit and sources. Keep their bytes and
    // schema intact while applying the same checks to every replay.
    let commit = ["sourceCommit", "baselineCommit", "commit"]
        .into_iter()
        .find_map(|key| oracle.get(key))
        .expect("oracle is missing its baseline commit");
    assert_eq!(text(commit), BASELINE_COMMIT, "oracle baseline commit");
    let hashes = oracle
        .get("sourceSha256")
        .or_else(|| oracle.get("sources"))
        .expect("oracle is missing sourceSha256/sources")
        .as_object()
        .expect("oracle source hashes must be an object");
    assert!(!hashes.is_empty(), "oracle source hashes must not be empty");
    if let Some(baseline) = baseline {
        let head = git(baseline, &["rev-parse", "HEAD"]);
        assert_eq!(
            String::from_utf8_lossy(&head).trim(),
            BASELINE_COMMIT,
            "PDRS_BASELINE must be checked out at the fixed commit"
        );
        let tracked = git(baseline, &["status", "--porcelain", "--untracked-files=no"]);
        assert!(
            tracked.is_empty(),
            "PDRS_BASELINE has uncommitted changes to tracked files:\n{}",
            String::from_utf8_lossy(&tracked)
        );
    }
    for (source, recorded) in hashes.entries() {
        let source = source.to_utf8().expect("UTF-8 source path");
        assert!(
            source.starts_with("src/") && source.split('/').all(|p| p != ".." && !p.is_empty()),
            "expected a baseline-relative source path: {source}"
        );
        let recorded = text(recorded);
        assert!(
            recorded.len() == 64 && recorded.bytes().all(|b| b.is_ascii_hexdigit()),
            "missing or malformed hash for {source}"
        );
        if let Some(baseline) = baseline {
            let committed = git(baseline, &["show", &format!("{BASELINE_COMMIT}:{source}")]);
            let expected = format!("{:x}", Sha256::digest(committed));
            assert_eq!(
                recorded, expected,
                "oracle hash for {source} does not match {BASELINE_COMMIT}"
            );
            let working =
                fs::read(baseline.join(&source)).unwrap_or_else(|e| panic!("{source}: {e}"));
            assert_eq!(
                format!("{:x}", Sha256::digest(working)),
                expected,
                "baseline source {source} differs from {BASELINE_COMMIT}"
            );
        }
    }
}

fn git(baseline: &Path, args: &[&str]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .args(["--no-replace-objects", "-C"])
        .arg(baseline)
        .args(args)
        .output()
        .expect("run git against PDRS_BASELINE");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

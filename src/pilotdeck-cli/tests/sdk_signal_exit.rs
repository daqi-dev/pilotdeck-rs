//! #1145 P08: captured originals, provenance and lifecycle replay guards.
// Compile the real entry and its modules into the child driver: no copied CLI
// implementation, and the observation-only cfg(test) hooks remain available.
#[path = "../src/main.rs"]
#[allow(dead_code)]
mod product;
pub(crate) use product::{
    Failure, json_pretty, paths, skills, stderr_line, stdout_line, write_json_pretty_line,
};
#[path = "common/signal_replay.rs"]
mod signal_replay;
pub(crate) use product::test_negative_receipt;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const NEGATIVE_CONTROLS: u64 = 72;

fn fixture_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sdk-signals")
}

fn verify_hash_group(source: &Value, field: &str, root: &Path) {
    let hashes = source[field].as_object().expect("#1145 source hash object");
    let mut names = if field == "probeSha256" {
        vec![
            "probe.mjs",
            "hooks.mjs",
            "cases.json",
            "capture.py",
            "cleanup.mjs",
            "gap-source.json",
        ]
    } else {
        vec![
            "src/cli/sdkCli.ts",
            "packages/sdk/src/client.ts",
            "packages/sdk/src/transport.ts",
            "dist/src/cli/pilotdeck.js",
            "dist/src/cli/sdkCli.js",
            "packages/sdk/dist/client.js",
            "packages/sdk/dist/transport.js",
        ]
    };
    names.sort_unstable();
    assert_eq!(
        hashes.keys().map(String::as_str).collect::<Vec<_>>(),
        names,
        "#1145 exact producer identity paths"
    );
    assert_eq!(
        hashes.len(),
        if field == "probeSha256" { 6 } else { 7 },
        "#1145 exact producer hash denominator"
    );
    for (name, expected) in hashes {
        let expected = expected.as_str().expect("#1145 digest string");
        assert!(
            expected.len() == 64
                && expected
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "#1145 digest format"
        );
        let bytes = fs::read(root.join(name)).expect("#1145 recorded source file exists");
        assert_eq!(
            format!("{:x}", Sha256::digest(bytes)),
            expected,
            "#1145 source file SHA256 matches disk: {field}/{name}"
        );
    }
}

fn verify_all_files(source: &Value, directory: &Path) {
    let baseline = std::env::var_os("PDRS_BASELINE")
        .expect("#1145 PDRS_BASELINE required for complete TS provenance");
    verify_hash_group(source, "probeSha256", directory);
    verify_hash_group(source, "sourceSha256", Path::new(&baseline));
}

// Always runs without Node or the optional external baseline checkout. This
// prevents the ignored regeneration command from being the only golden guard.
#[test]
fn signal_oracle_matches_recorded_source_and_local_files() {
    verify_local_oracle(&fixture_directory());
}

fn verify_local_oracle(directory: &Path) {
    let bytes = fs::read(directory.join("source.json")).expect("#1145 captured source exists");
    let source: Value = serde_json::from_slice(&bytes).expect("#1145 captured source JSON");
    verify_hash_group(&source, "probeSha256", directory);
    let definitions: Value = serde_json::from_slice(
        &fs::read(directory.join("cases.json")).expect("#1145 definitions exist"),
    )
    .expect("#1145 definitions JSON");
    let definitions = definitions.as_array().expect("#1145 definitions array");
    assert_eq!(definitions.len(), 102, "#1145 registered matrix cells");
    assert_eq!(
        source["nodeVersion"], "v22.22.0",
        "#1145 source Node identity"
    );
    assert_eq!(
        source["sourceCommit"], "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85",
        "#1145 source commit identity"
    );
    let captures = source["captures"].as_array().expect("#1145 captures array");
    assert_eq!(captures.len(), 96, "#1145 active matrix cells");
    let gaps = source["gaps"].as_array().expect("#1145 gaps array");
    assert_eq!(gaps.len(), 6, "#1145 stdin gap denominator");
    let expected = json!({
        "sourceSha256": format!("{:x}", Sha256::digest(&bytes)),
        "sourceCommit": source["sourceCommit"], "nodeVersion": source["nodeVersion"],
        "kind": source["kind"],
        "normalization": "none; raw capture including stderr bootstrap warnings retained",
        "matrixCells": definitions.len(),
        "gaps": gaps.iter().map(|gap| json!({"id":gap["id"],"status":gap["status"],"reason":gap["reason"]})).collect::<Vec<_>>(),
        "cases": captures,
    });
    let oracle: Value = serde_json::from_slice(
        &fs::read(directory.join("oracle.json")).expect("#1145 oracle exists"),
    )
    .expect("#1145 oracle JSON");
    assert!(
        oracle == expected,
        "#1145 golden equals captured source projection"
    );
}

fn rejects_with_marker(action: impl FnOnce() + std::panic::UnwindSafe, marker: &str) {
    let error = std::panic::catch_unwind(action).expect_err("#1145 corrupt input must be rejected");
    let message = error
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| error.downcast_ref::<&str>().copied())
        .unwrap_or("");
    assert!(
        message.contains(marker),
        "#1145 negative must reach its bound assertion: {message}"
    );
}

#[test]
fn signal_local_replay_rejects_disk_drift() {
    let directory = fixture_directory();
    let source: Value = serde_json::from_slice(
        &fs::read(directory.join("source.json")).expect("#1145 local negative original"),
    )
    .expect("#1145 local negative JSON");
    for file in ["cases.json", "probe.mjs", "hooks.mjs"] {
        let scratch = Scratch::new();
        for name in source["probeSha256"]
            .as_object()
            .expect("#1145 producer names")
            .keys()
            .map(String::as_str)
            .chain(["source.json", "oracle.json"])
        {
            fs::copy(directory.join(name), scratch.0.join(name))
                .expect("#1145 scratch fixture copy");
        }
        verify_local_oracle(&scratch.0);
        let mut bytes = fs::read(scratch.0.join(file)).expect("#1145 scratch target");
        bytes.push(b'\n');
        fs::write(scratch.0.join(file), bytes).expect("#1145 scratch drift");
        rejects_with_marker(
            || verify_local_oracle(&scratch.0),
            &format!("#1145 source file SHA256 matches disk: probeSha256/{file}"),
        );
        fs::write(
            scratch.0.join(file),
            fs::read(directory.join(file)).expect("#1145 unchanged original"),
        )
        .expect("#1145 restore scratch input");
        verify_local_oracle(&scratch.0);
    }
}

#[test]
fn signal_provenance_rejects_replaced_identity_and_golden() {
    let directory = fixture_directory();
    let mut source: Value = serde_json::from_slice(
        &fs::read(directory.join("source.json")).expect("#1145 identity negative original"),
    )
    .expect("#1145 identity negative JSON");
    let hashes = source["probeSha256"]
        .as_object_mut()
        .expect("#1145 identity negative hashes");
    let digest = hashes
        .remove("hooks.mjs")
        .expect("#1145 original identity key");
    hashes.insert("unrelated.mjs".into(), digest);
    rejects_with_marker(
        || verify_hash_group(&source, "probeSha256", &directory),
        "#1145 exact producer identity paths",
    );
    let scratch = Scratch::new();
    for name in [
        "probe.mjs",
        "hooks.mjs",
        "cases.json",
        "capture.py",
        "cleanup.mjs",
        "gap-source.json",
        "source.json",
        "oracle.json",
    ] {
        fs::copy(directory.join(name), scratch.0.join(name)).expect("#1145 golden scratch copy");
    }
    verify_local_oracle(&scratch.0);
    let mut oracle: Value = serde_json::from_slice(
        &fs::read(scratch.0.join("oracle.json")).expect("#1145 golden negative original"),
    )
    .expect("#1145 golden negative JSON");
    oracle["cases"][0]["aborts"] = json!(99);
    fs::write(
        scratch.0.join("oracle.json"),
        serde_json::to_vec(&oracle).expect("#1145 corrupt golden JSON"),
    )
    .expect("#1145 corrupt golden file");
    rejects_with_marker(
        || verify_local_oracle(&scratch.0),
        "#1145 golden equals captured source projection",
    );
}

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
    // The supplemental state probe executes the same verified TS baseline;
    // its originals and producer identities are owned by sdk-signals.
    let source_directory = if fixture == "sdk-signal-abort-state" {
        fixture_directory()
    } else {
        directory.clone()
    };
    let source: Value = serde_json::from_slice(
        &fs::read(source_directory.join("source.json")).expect("#1145 complete source exists"),
    )
    .expect("#1145 complete source JSON");
    verify_all_files(&source, &source_directory);
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
#[ignore = "requires Node v22.22.0 and PDRS_BASELINE; executes actual TS SDK with WebSocket IO double"]
fn signal_abort_state_oracle_regenerates_identically() {
    regenerates_identically("sdk-signal-abort-state");
}

fn verify_abort_state_oracle(oracle: &Value) {
    let parent = fixture_directory();
    let directory = parent.with_file_name("sdk-signal-abort-state");
    let source_bytes =
        fs::read(parent.join("source.json")).expect("#1145 supplemental parent bytes");
    let source: Value =
        serde_json::from_slice(&source_bytes).expect("#1145 supplemental parent JSON");
    let generator =
        fs::read(directory.join("generate.mjs")).expect("#1145 supplemental producer bytes");
    let expected = json!({"sourceCommit":source["sourceCommit"],"nodeVersion":"v22.22.0",
        "generatorSha256":format!("{:x}",Sha256::digest(generator)),
        "signalSourceSha256":format!("{:x}",Sha256::digest(source_bytes)),
        "sourceSha256":source["sourceSha256"],"kind":"real-ts-sdk-state-with-websocket-io-double","matrixCells":21});
    for (key, value) in expected
        .as_object()
        .expect("#1145 supplemental identity fields")
    {
        assert_eq!(
            &oracle[key], value,
            "#1145 supplemental provenance matches disk: {key}"
        );
    }
    let rows = oracle["cases"].as_array().expect("#1145 supplemental rows");
    assert_eq!(rows.len(), 21, "#1145 fixed supplemental row denominator");
    let ids = rows
        .iter()
        .map(|row| row["id"].as_str().expect("#1145 supplemental cell id"))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 21, "#1145 unique supplemental cell identities");
}

#[test]
fn signal_abort_state_local_provenance() {
    verify_local_oracle(&fixture_directory());
    let oracle: Value =
        serde_json::from_str(include_str!("fixtures/sdk-signal-abort-state/oracle.json"))
            .expect("#1145 supplemental local oracle JSON");
    verify_abort_state_oracle(&oracle);
}

#[test]
fn signal_abort_state_rejects_provenance_and_denominator_drift() {
    let oracle: Value =
        serde_json::from_str(include_str!("fixtures/sdk-signal-abort-state/oracle.json"))
            .expect("#1145 supplemental negative original JSON");
    verify_abort_state_oracle(&oracle);
    for key in [
        "sourceCommit",
        "nodeVersion",
        "generatorSha256",
        "signalSourceSha256",
        "sourceSha256",
        "kind",
        "matrixCells",
    ] {
        let mut changed = oracle.clone();
        changed[key] = Value::Null;
        rejects_with_marker(
            || verify_abort_state_oracle(&changed),
            &format!("#1145 supplemental provenance matches disk: {key}"),
        );
    }
    let mut changed = oracle.clone();
    changed["cases"] = json!([]);
    rejects_with_marker(
        || verify_abort_state_oracle(&changed),
        "#1145 fixed supplemental row denominator",
    );
    let mut changed = oracle.clone();
    changed["cases"][1]["id"] = changed["cases"][0]["id"].clone();
    rejects_with_marker(
        || verify_abort_state_oracle(&changed),
        "#1145 unique supplemental cell identities",
    );
    verify_abort_state_oracle(&oracle);
}

#[test]
#[ignore = "requires Node v22.22.0; adversarial capture-data validation"]
fn signal_capture_rejects_invalid_evidence() {
    let scratch = Scratch::new();
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sdk-signals/negative_controls.mjs");
    // Registered controls at two seconds each, plus startup/read overhead. The measured
    // full suite is under a second; keep a bounded allowance for busy hosts.
    let output = run_node(
        &scratch.0,
        &[script.as_os_str()],
        "negative-controls",
        Duration::from_secs(NEGATIVE_CONTROLS * 2 + 12),
    );
    verify_control_summary(&output);
}

fn verify_control_summary(output: &[u8]) {
    let last = output
        .split(|byte| *byte == b'\n')
        .rfind(|line| !line.is_empty())
        .expect("#1145 negative summary line");
    let summary: Value = serde_json::from_slice(last).expect("#1145 negative summary JSON");
    assert_eq!(
        summary["total"], NEGATIVE_CONTROLS,
        "#1145 fixed negative total"
    );
    assert_eq!(
        summary["counts"]["KILLED"], NEGATIVE_CONTROLS,
        "#1145 every registered control killed"
    );
    assert_eq!(
        summary["counts"]["SURVIVED"], 0,
        "#1145 no surviving controls"
    );
    assert_eq!(summary["counts"]["INVALID"], 0, "#1145 no invalid controls");
}

#[test]
fn signal_negative_summary_rejects_missing_controls() {
    let valid = json!({"total":72,"counts":{"KILLED":72,"SURVIVED":0,"INVALID":0}});
    verify_control_summary(valid.to_string().as_bytes());
    for (key, value, marker) in [
        ("total", 0, "#1145 fixed negative total"),
        ("KILLED", 71, "#1145 every registered control killed"),
        ("SURVIVED", 1, "#1145 no surviving controls"),
        ("INVALID", 1, "#1145 no invalid controls"),
    ] {
        let mut corrupt = valid.clone();
        if key == "total" {
            corrupt[key] = json!(value);
        } else {
            corrupt["counts"][key] = json!(value);
        }
        rejects_with_marker(
            || verify_control_summary(corrupt.to_string().as_bytes()),
            marker,
        );
    }
}

#[test]
#[ignore = "requires PDRS_BASELINE; actual TS disk identity negative control"]
fn signal_ts_provenance_rejects_disk_drift() {
    let directory = fixture_directory();
    let source: Value = serde_json::from_slice(
        &fs::read(directory.join("source.json")).expect("#1145 TS negative source"),
    )
    .expect("#1145 TS negative JSON");
    let baseline = std::env::var_os("PDRS_BASELINE").expect("#1145 TS negative baseline required");
    let baseline = Path::new(&baseline);
    verify_all_files(&source, &directory);
    let scratch = Scratch::new();
    for name in source["sourceSha256"]
        .as_object()
        .expect("#1145 TS negative file names")
        .keys()
    {
        let target = scratch.0.join(name);
        fs::create_dir_all(target.parent().expect("#1145 TS negative parent"))
            .expect("#1145 TS negative directories");
        fs::copy(baseline.join(name), target).expect("#1145 TS negative scratch copy");
    }
    verify_hash_group(&source, "sourceSha256", &scratch.0);
    let target = scratch.0.join("src/cli/sdkCli.ts");
    let original = fs::read(&target).expect("#1145 TS scratch original");
    let mut corrupt = original.clone();
    corrupt.push(b'\n');
    fs::write(&target, corrupt).expect("#1145 TS scratch drift");
    rejects_with_marker(
        || verify_hash_group(&source, "sourceSha256", &scratch.0),
        "#1145 source file SHA256 matches disk: sourceSha256/src/cli/sdkCli.ts",
    );
    fs::write(target, original).expect("#1145 TS scratch restore");
    verify_hash_group(&source, "sourceSha256", &scratch.0);
}

// The exact child selection has no product effect in ordinary test runs.
#[test]
fn signal_driver() {
    signal_replay::driver();
}

mod signal_cases {
    macro_rules! observed {
        ($name:ident, $id:literal) => {
            #[test]
            #[ignore = "#1145 real TS baseline + 28999 process replay; run the registered 96-cell command"]
            fn $name() {
                super::signal_replay::run($id);
            }
        };
    }
    macro_rules! stdin_gap {
        ($name:ident, $id:literal) => {
            #[test]
            #[ignore = "#1145 stdin observation unresolved; prerequisite to #123 / P09"]
            fn $name() {
                panic!("#1145 {} explicit stdin gap: #123 / P09; forced ignored execution cannot report PASS", $id);
            }
        };
    }
    observed!(run_signal_sigint, "run--signal-sigint");
    stdin_gap!(
        run_stdin_open_after_response_sigint,
        "run--stdin-open-after-response-sigint"
    );
    observed!(run_query_hello_hold_sigint, "run--query-hello-hold-sigint");
    observed!(
        run_query_hello_release_sigint,
        "run--query-hello-release-sigint"
    );
    observed!(
        run_query_hello_reject_sigint,
        "run--query-hello-reject-sigint"
    );
    observed!(
        run_query_resume_hold_sigint,
        "run--query-resume-hold-sigint"
    );
    observed!(
        run_query_resume_release_sigint,
        "run--query-resume-release-sigint"
    );
    observed!(
        run_query_resume_reject_sigint,
        "run--query-resume-reject-sigint"
    );
    observed!(run_query_model_hold_sigint, "run--query-model-hold-sigint");
    observed!(
        run_query_model_release_sigint,
        "run--query-model-release-sigint"
    );
    observed!(
        run_query_model_reject_sigint,
        "run--query-model-reject-sigint"
    );
    observed!(run_abort_ok_sigint, "run--abort-ok-sigint");
    observed!(run_abort_error_sigint, "run--abort-error-sigint");
    observed!(run_abort_hold_sigint, "run--abort-hold-sigint");
    observed!(run_second_sigint, "run--second-sigint");
    observed!(run_second_sigint_sigterm, "run--second-sigint-sigterm");
    observed!(
        run_terminal_consumed_completed_sigint,
        "run--terminal-consumed-completed-sigint"
    );
    observed!(
        run_terminal_consumed_failed_sigint,
        "run--terminal-consumed-failed-sigint"
    );
    observed!(
        run_terminal_during_abort_open_sigint,
        "run--terminal-during-abort-open-sigint"
    );
    observed!(
        run_terminal_during_abort_final_sigint,
        "run--terminal-during-abort-final-sigint"
    );
    observed!(run_signal_sigterm, "run--signal-sigterm");
    stdin_gap!(
        run_stdin_open_after_response_sigterm,
        "run--stdin-open-after-response-sigterm"
    );
    observed!(
        run_query_hello_hold_sigterm,
        "run--query-hello-hold-sigterm"
    );
    observed!(
        run_query_hello_release_sigterm,
        "run--query-hello-release-sigterm"
    );
    observed!(
        run_query_hello_reject_sigterm,
        "run--query-hello-reject-sigterm"
    );
    observed!(
        run_query_resume_hold_sigterm,
        "run--query-resume-hold-sigterm"
    );
    observed!(
        run_query_resume_release_sigterm,
        "run--query-resume-release-sigterm"
    );
    observed!(
        run_query_resume_reject_sigterm,
        "run--query-resume-reject-sigterm"
    );
    observed!(
        run_query_model_hold_sigterm,
        "run--query-model-hold-sigterm"
    );
    observed!(
        run_query_model_release_sigterm,
        "run--query-model-release-sigterm"
    );
    observed!(
        run_query_model_reject_sigterm,
        "run--query-model-reject-sigterm"
    );
    observed!(run_abort_ok_sigterm, "run--abort-ok-sigterm");
    observed!(run_abort_error_sigterm, "run--abort-error-sigterm");
    observed!(run_abort_hold_sigterm, "run--abort-hold-sigterm");
    observed!(run_second_sigterm, "run--second-sigterm");
    observed!(run_second_sigterm_sigint, "run--second-sigterm-sigint");
    observed!(
        run_terminal_consumed_completed_sigterm,
        "run--terminal-consumed-completed-sigterm"
    );
    observed!(
        run_terminal_consumed_failed_sigterm,
        "run--terminal-consumed-failed-sigterm"
    );
    observed!(
        run_terminal_during_abort_open_sigterm,
        "run--terminal-during-abort-open-sigterm"
    );
    observed!(
        run_terminal_during_abort_final_sigterm,
        "run--terminal-during-abort-final-sigterm"
    );
    observed!(run_control_pending, "run--control-pending");
    stdin_gap!(run_stdin_pending, "run--stdin-pending");
    observed!(run_query_hello_pending, "run--query-hello-pending");
    observed!(run_query_resume_pending, "run--query-resume-pending");
    observed!(run_query_model_pending, "run--query-model-pending");
    observed!(run_complete_cleanup, "run--complete-cleanup");
    observed!(run_failed_cleanup, "run--failed-cleanup");
    observed!(run_unknown_cleanup, "run--unknown-cleanup");
    observed!(run_query_reject_cleanup, "run--query-reject-cleanup");
    observed!(run_response_error_cleanup, "run--response-error-cleanup");
    observed!(run_control_reject_cleanup, "run--control-reject-cleanup");
    observed!(resume_signal_sigint, "resume--signal-sigint");
    stdin_gap!(
        resume_stdin_open_after_response_sigint,
        "resume--stdin-open-after-response-sigint"
    );
    observed!(
        resume_query_hello_hold_sigint,
        "resume--query-hello-hold-sigint"
    );
    observed!(
        resume_query_hello_release_sigint,
        "resume--query-hello-release-sigint"
    );
    observed!(
        resume_query_hello_reject_sigint,
        "resume--query-hello-reject-sigint"
    );
    observed!(
        resume_query_resume_hold_sigint,
        "resume--query-resume-hold-sigint"
    );
    observed!(
        resume_query_resume_release_sigint,
        "resume--query-resume-release-sigint"
    );
    observed!(
        resume_query_resume_reject_sigint,
        "resume--query-resume-reject-sigint"
    );
    observed!(
        resume_query_model_hold_sigint,
        "resume--query-model-hold-sigint"
    );
    observed!(
        resume_query_model_release_sigint,
        "resume--query-model-release-sigint"
    );
    observed!(
        resume_query_model_reject_sigint,
        "resume--query-model-reject-sigint"
    );
    observed!(resume_abort_ok_sigint, "resume--abort-ok-sigint");
    observed!(resume_abort_error_sigint, "resume--abort-error-sigint");
    observed!(resume_abort_hold_sigint, "resume--abort-hold-sigint");
    observed!(resume_second_sigint, "resume--second-sigint");
    observed!(
        resume_second_sigint_sigterm,
        "resume--second-sigint-sigterm"
    );
    observed!(
        resume_terminal_consumed_completed_sigint,
        "resume--terminal-consumed-completed-sigint"
    );
    observed!(
        resume_terminal_consumed_failed_sigint,
        "resume--terminal-consumed-failed-sigint"
    );
    observed!(
        resume_terminal_during_abort_open_sigint,
        "resume--terminal-during-abort-open-sigint"
    );
    observed!(
        resume_terminal_during_abort_final_sigint,
        "resume--terminal-during-abort-final-sigint"
    );
    observed!(resume_signal_sigterm, "resume--signal-sigterm");
    stdin_gap!(
        resume_stdin_open_after_response_sigterm,
        "resume--stdin-open-after-response-sigterm"
    );
    observed!(
        resume_query_hello_hold_sigterm,
        "resume--query-hello-hold-sigterm"
    );
    observed!(
        resume_query_hello_release_sigterm,
        "resume--query-hello-release-sigterm"
    );
    observed!(
        resume_query_hello_reject_sigterm,
        "resume--query-hello-reject-sigterm"
    );
    observed!(
        resume_query_resume_hold_sigterm,
        "resume--query-resume-hold-sigterm"
    );
    observed!(
        resume_query_resume_release_sigterm,
        "resume--query-resume-release-sigterm"
    );
    observed!(
        resume_query_resume_reject_sigterm,
        "resume--query-resume-reject-sigterm"
    );
    observed!(
        resume_query_model_hold_sigterm,
        "resume--query-model-hold-sigterm"
    );
    observed!(
        resume_query_model_release_sigterm,
        "resume--query-model-release-sigterm"
    );
    observed!(
        resume_query_model_reject_sigterm,
        "resume--query-model-reject-sigterm"
    );
    observed!(resume_abort_ok_sigterm, "resume--abort-ok-sigterm");
    observed!(resume_abort_error_sigterm, "resume--abort-error-sigterm");
    observed!(resume_abort_hold_sigterm, "resume--abort-hold-sigterm");
    observed!(resume_second_sigterm, "resume--second-sigterm");
    observed!(
        resume_second_sigterm_sigint,
        "resume--second-sigterm-sigint"
    );
    observed!(
        resume_terminal_consumed_completed_sigterm,
        "resume--terminal-consumed-completed-sigterm"
    );
    observed!(
        resume_terminal_consumed_failed_sigterm,
        "resume--terminal-consumed-failed-sigterm"
    );
    observed!(
        resume_terminal_during_abort_open_sigterm,
        "resume--terminal-during-abort-open-sigterm"
    );
    observed!(
        resume_terminal_during_abort_final_sigterm,
        "resume--terminal-during-abort-final-sigterm"
    );
    observed!(resume_control_pending, "resume--control-pending");
    stdin_gap!(resume_stdin_pending, "resume--stdin-pending");
    observed!(resume_query_hello_pending, "resume--query-hello-pending");
    observed!(resume_query_resume_pending, "resume--query-resume-pending");
    observed!(resume_query_model_pending, "resume--query-model-pending");
    observed!(resume_complete_cleanup, "resume--complete-cleanup");
    observed!(resume_failed_cleanup, "resume--failed-cleanup");
    observed!(resume_unknown_cleanup, "resume--unknown-cleanup");
    observed!(resume_query_reject_cleanup, "resume--query-reject-cleanup");
    observed!(
        resume_response_error_cleanup,
        "resume--response-error-cleanup"
    );
    observed!(
        resume_control_reject_cleanup,
        "resume--control-reject-cleanup"
    );
}

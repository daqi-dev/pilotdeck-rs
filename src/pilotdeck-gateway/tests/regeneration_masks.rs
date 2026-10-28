//! #953 negative coverage for the run-registry / provider-timing masks.
//!
//! These cases stay separate from `generator_provenance.rs` (#743/#745 own
//! that file): they pin the two forward-looking diagnostics from #280 r2 —
//! a foreign owner PID must fail closed, and a truncation before a masked
//! scalar must name the damage instead of looking like a missing field.

#[allow(dead_code)]
#[path = "common/baseline.rs"]
mod baseline;

use baseline::regeneration_masks as masks;
use std::{fs, path::Path};

const RUN_REGISTRY_ORACLE: &[u8] = include_bytes!("fixtures/run-registry/oracle.json");
const RUN_REGISTRY_NORMAL_AFTER: &[u8] = include_bytes!("fixtures/run-registry/normal.after.json");
const PROVIDER_TIMING_ORACLE: &[u8] = include_bytes!("fixtures/provider-timing/oracle.json");

fn generator_pid() -> u32 {
    masks::pid(RUN_REGISTRY_ORACLE).expect("#953 committed oracle has a positive generator PID")
}

fn span_for(file: &[u8], pointer: &str) -> std::ops::Range<usize> {
    masks::scalar_spans(file)
        .into_iter()
        .find(|(path, _)| path == pointer)
        .unwrap_or_else(|| panic!("#953 fixture has a scalar at {pointer}"))
        .1
}

#[test]
fn run_registry_accepts_the_committed_owner_pid() {
    let pid = generator_pid();
    masks::normalized(
        "run-registry",
        "normal.after.json",
        RUN_REGISTRY_NORMAL_AFTER,
        Some(pid),
    )
    .expect("#953 committed owner PID is the generator PID");
}

#[test]
fn run_registry_rejects_a_foreign_owner_pid() {
    let pid = generator_pid();
    let foreign = pid + 7;
    let span = span_for(RUN_REGISTRY_NORMAL_AFTER, "/records/0/ownerPid");
    let mut changed = RUN_REGISTRY_NORMAL_AFTER.to_vec();
    changed.splice(span, foreign.to_string().into_bytes());
    let error = masks::normalized("run-registry", "normal.after.json", &changed, Some(pid))
        .expect_err("#953 a foreign owner PID must not be masked");
    assert!(
        error.contains("foreign owner PID")
            && error.contains(&foreign.to_string())
            && error.contains(&pid.to_string()),
        "#953 foreign owner PID diagnostic must name actual and expected: {error}"
    );
    let error = masks::compare(
        "run-registry",
        "normal.after.json",
        &changed,
        RUN_REGISTRY_NORMAL_AFTER,
        (Some(pid), Some(pid)),
    )
    .expect_err("#953 compare must propagate the foreign owner PID rejection");
    assert!(
        error.contains("foreign owner PID"),
        "#953 compare foreign owner PID diagnostic: {error}"
    );
}

#[test]
fn run_registry_rejects_mismatched_actual_and_committed_generator_pids() {
    let pid = generator_pid();
    let error = masks::compare(
        "run-registry",
        "normal.after.json",
        RUN_REGISTRY_NORMAL_AFTER,
        RUN_REGISTRY_NORMAL_AFTER,
        (Some(pid + 1), Some(pid)),
    )
    .expect_err("#953 the actual generator PID must match the masked owner PID");
    assert!(
        error.contains("foreign owner PID") && error.contains(&(pid + 1).to_string()),
        "#953 mismatched generator PID diagnostic: {error}"
    );
}

#[test]
fn truncation_before_a_masked_span_names_the_offset_and_path() {
    let rules = masks::rules("provider-timing", "oracle.json");
    let first = masks::scalar_spans(PROVIDER_TIMING_ORACLE)
        .into_iter()
        .find(|(path, _)| rules.contains_key(path.as_str()))
        .unwrap_or_else(|| panic!("#953 provider-timing has a masked scalar"))
        .1;
    let cut = (first.start / 2).max(1);
    let truncated = &PROVIDER_TIMING_ORACLE[..cut];
    let error = masks::normalized("provider-timing", "oracle.json", truncated, None)
        .expect_err("#953 truncation before a mask must fail closed");
    let location = format!("input ends at byte {cut}");
    assert!(
        (error.contains(&location) || error.contains("is malformed at byte"))
            && error.contains("before masked path")
            && error.contains("/cases/"),
        "#953 truncation diagnostic must be readable: {error}"
    );
    assert!(
        !error.contains("missing masked paths"),
        "#953 truncation must not be misreported as a missing field: {error}"
    );
}

#[test]
fn a_complete_document_missing_a_masked_field_still_says_missing() {
    let mut oracle: serde_json::Value =
        serde_json::from_slice(PROVIDER_TIMING_ORACLE).expect("#953 provider-timing oracle parses");
    oracle["cases"][0]["timers"][0]
        .as_object_mut()
        .expect("#953 timer is an object")
        .remove("start")
        .expect("#953 timer has a start");
    let bytes = serde_json::to_vec(&oracle).expect("#953 reserialize provider-timing oracle");
    let error = masks::normalized("provider-timing", "oracle.json", &bytes, None)
        .expect_err("#953 a complete document missing a mask must fail");
    assert!(
        error.contains("missing masked paths") && error.contains("/cases/0/timers/0/start"),
        "#953 missing masked path diagnostic: {error}"
    );
}

#[test]
fn bad_tail_after_all_masks_stays_byte_exact() {
    let pid = generator_pid();
    let mut changed = RUN_REGISTRY_NORMAL_AFTER.to_vec();
    let last = changed.last_mut().expect("#953 nonempty normal.after.json");
    *last = if *last == b'}' { b']' } else { b'}' };
    let error = masks::compare(
        "run-registry",
        "normal.after.json",
        &changed,
        RUN_REGISTRY_NORMAL_AFTER,
        (Some(pid), Some(pid)),
    )
    .expect_err("#953 a byte outside every mask must differ");
    assert!(
        error.contains("differs from committed bytes"),
        "#953 byte-exact tail diagnostic: {error}"
    );
}

/// #953 item 2: no real fixture generator may keep calling the old
/// `bindSourceTree` API directly. `_shared/baseline.mts` and
/// `_shared/generator_guard.mjs` are the only allowed entry points; both now
/// enforce the tracked-tree status and `ls-files -v` checks.
#[test]
fn fixture_generators_do_not_call_the_old_guard_api() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut checked = 0;
    for fixture in fs::read_dir(&fixtures).expect("#953 read fixtures directory") {
        let fixture = fixture.expect("#953 fixture entry");
        if !fixture.file_type().expect("#953 fixture type").is_dir() {
            continue;
        }
        for entry in fs::read_dir(fixture.path()).expect("#953 read fixture directory") {
            let entry = entry.expect("#953 fixture file");
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("generate") || !(name.ends_with(".mts") || name.ends_with(".mjs"))
            {
                continue;
            }
            let path = entry.path();
            let relative = path
                .strip_prefix(&fixtures)
                .expect("#953 relative generator path")
                .display();
            let text = fs::read_to_string(&path).expect("#953 read generator source");
            let code: String = text
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !code.contains("bindSourceTree"),
                "#953 {relative} still imports or calls the old guard API"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 190,
        "#953 generator inventory shrank unexpectedly: {checked}"
    );
}

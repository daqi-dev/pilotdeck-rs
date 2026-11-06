//! #1525: a permission-fault contract that could not be exercised must not be
//! reported as a passing test (#1263's three-state leak).
//!
//! Several tests assert behavior that only shows up when the filesystem really
//! refuses a write (mode bits / `EACCES`). On a privileged host (root,
//! `CAP_DAC_OVERRIDE`) the probe succeeds, so the test cannot run and used to
//! `return` — libtest still printed `ok`, and every consumer that looks at the
//! exit code treated the contract as exercised.
//!
//! `PDRS_REQUIRE_PERMISSION_FAULTS` flips that skip into a panic: CI and the
//! formal host recipes export the switch, local runs leave it unset and keep
//! the recognizable skip line. Shared by the integration tests that pull it in
//! with `#[path = "common/permission_faults.rs"]` (dump_request.rs,
//! edgeclaw_heartbeat_indexer.rs, tool_budget.rs, workspace_snapshot.rs,
//! write_tools.rs) and by the crate's own unit tests through
//! `crate::test_permission_faults` (lib.rs: dispatch, project_files,
//! runtime::cron, skills, storage) — do not copy the parser. An unset variable
//! means "off" only when it is *absent*: an empty value (the switch exported
//! with no spelling after `=`, which a missing shell expansion produces) is a
//! configuration error and panics with the other unrecognized spellings,
//! rather than quietly reading as off.
//!
//! #1900: every spelling that means "on" must actually arm the switch. The old
//! parser matched the exact literals `1` and `true`, so `True`, `YES` or `" 1"`
//! (a shell variable with a stray space, a YAML-normalized boolean) parsed as
//! *off* and put a privileged host straight back on "skip is fine" — the
//! fail-open #1263 exists to remove. The switch is now case-insensitive and
//! trimmed, and anything that is neither a recognized on nor a recognized off
//! token panics instead of guessing: a typo must not silently disarm the gate.

/// Accepted spellings, case-insensitive and trimmed. Everything else panics.
const ON: &[&str] = &["1", "true", "yes", "on"];
const OFF: &[&str] = &["0", "false", "no", "off"];

pub fn require_permission_faults() -> bool {
    match std::env::var("PDRS_REQUIRE_PERMISSION_FAULTS") {
        Err(std::env::VarError::NotPresent) => false,
        Err(std::env::VarError::NotUnicode(value)) => panic!(
            "#1900 PDRS_REQUIRE_PERMISSION_FAULTS is not valid UTF-8 ({}); refusing to guess \
             whether the permission-fault contract is required",
            value.to_string_lossy()
        ),
        Ok(value) => parse_permission_faults(&value),
    }
}

fn parse_permission_faults(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    if ON.contains(&normalized.as_str()) {
        true
    } else if OFF.contains(&normalized.as_str()) {
        false
    } else {
        panic!(
            "#1900 PDRS_REQUIRE_PERMISSION_FAULTS={value:?} is not a recognized on/off value; \
             use one of {ON:?} to require the permission-fault contract or {OFF:?} to leave it \
             to the host — an unrecognized spelling used to read as off and silently disarmed \
             the gate on privileged hosts"
        )
    }
}

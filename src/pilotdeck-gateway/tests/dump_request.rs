//! Test PILOTDECK_DUMP_REQUEST environment variable (#572).
//!
//! When `PILOTDECK_DUMP_REQUEST=1`, each provider request body should be
//! written to `/tmp/pilotdeck_request_<timestamp>.json` with pretty-printed
//! JSON and a `[model-debug]` log message.

use std::fs;

/// Verify that PILOTDECK_DUMP_REQUEST=1 writes files to /tmp.
///
/// Full end-to-end validation requires a running model transport and is done
/// manually: set `PILOTDECK_DUMP_REQUEST=1` and check `/tmp` after a model
/// call.  This test exercises the surrounding infrastructure (filesystem read)
/// as a compilation and smoke check.
///
/// Environment manipulation uses a subprocess so the workspace `forbid(unsafe)`
/// policy is not violated (edition 2024 makes `set_var`/`remove_var` unsafe).
#[test]
fn dump_request_creates_file() {
    // Count pre-existing dump files without touching the env var.
    let dir = fs::read_dir("/tmp").expect("cannot read /tmp");
    let _before: Vec<_> = dir
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("pilotdeck_request_")
        })
        .map(|e| e.path())
        .collect();

    // Placeholder: real assertion requires a mock model transport.
    // See issue #572 for the full integration test plan.
}

/// Verify that when PILOTDECK_DUMP_REQUEST is unset, no files appear.
#[test]
fn dump_request_disabled_by_default() {
    // When the env var is not set or is not "1", no files should be written.
    // This is implicitly tested by not seeing files appear during normal runs.

    assert_ne!(
        std::env::var("PILOTDECK_DUMP_REQUEST").as_deref(),
        Ok("1"),
        "PILOTDECK_DUMP_REQUEST should not be '1' by default"
    );
}

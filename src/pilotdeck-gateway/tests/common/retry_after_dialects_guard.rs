//! #360-2: the retry-after-dialects regeneration gate, shared by its two
//! test-binary twins (`model_stream.rs::
//! retry_after_dialects_regenerate_identically` and `generator_provenance.rs
//! ::retry_after_dialects_regenerates_identically`).
//!
//! The runtime provenance has two dimensions with different consequences
//! (regen-r2, decision card 2026-10-31). A different ICU build can change
//! `Date.parse` itself, so ICU 78.2 stays a hard pin and must fail. A
//! different bundled tzdata is value-neutral for this corpus (every
//! local-resolving case sits in Shanghai's flat +08:00 era; re-recording on
//! the homebrew ICU 78.2 / tzdata 2026a build reproduced all 62x2 values,
//! regen-r1 review), so a mismatch never fails the *values* — but since
//! regen-r3 (review P1) it fails this *guard*: the old println-and-return
//! skip left cargo green on a host whose byte comparison never ran, which
//! was indistinguishable from a real pass. A mismatched host now gets the
//! machine-checkable `#360-2 BLOCKED` panic (the same fail-not-skip shape
//! the #132a surrogate-collation twin already uses for its ICU pin);
//! re-verify the gold on the calibrated icu78 / tzdata 2025c Node.
//!
//! `local_tzdata_override` exists for the guard-semantics test and the
//! `blocked-branch-return` negative control: it injects a well-formed but
//! mismatched tzdata so the fail-not-skip branch is exercised on any host,
//! never only on a real mismatched machine.

/// Probe the host Node, hard-pin its version and ICU build, compare the
/// bundled tzdata against the gold-recorded one, then — only on a match —
/// re-run `generate.mts` against the real TS baseline and require the
/// byte-identical oracle (the `#[ignore]`d regeneration twins).
///
/// Uses `super::baseline` — every including test binary declares the shared
/// `common/baseline.rs` at its root, and loading it a second time through
/// this module would trip `clippy::duplicate_mod`.
pub(crate) fn assert_calibrated_runtime_and_regenerate(local_tzdata_override: Option<&str>) {
    let version = super::baseline::node_command()
        .arg("--version")
        .output()
        .expect("#360-2 Node version");
    assert!(
        version.status.success(),
        "#360-2 Node version command failed"
    );
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "v22.22.0",
        "#360-2 pinned Node runtime"
    );
    let probe = super::baseline::node_command()
        .args(["-p", "process.versions.icu + ' ' + process.versions.tz"])
        .output()
        .expect("#360-2 runtime provenance probe");
    assert!(
        probe.status.success(),
        "#360-2 runtime provenance probe failed"
    );
    let provenance = String::from_utf8_lossy(&probe.stdout);
    let (icu, tzdata) = provenance.trim().split_once(' ').unwrap_or_else(|| {
        panic!("#360-2 provenance probe must print 'icu tzdata', got {provenance:?}")
    });
    assert_eq!(
        icu, "78.2",
        "#360-2 pinned ICU runtime: the retry-after-dialects gold is recorded on the \
         icu78 calibrated chain (decision card 2026-10-31), and a different ICU build \
         must fail this guard rather than skip it"
    );
    let committed = serde_json::from_str::<serde_json::Value>(include_str!(
        "../fixtures/retry-after-dialects/oracle.json"
    ))
    .expect("#360-2 committed oracle must parse");
    let recorded = committed
        .get("tzdata")
        .and_then(serde_json::Value::as_str)
        .expect("#360-2 committed oracle records its tzdata provenance")
        .to_string();
    let local = local_tzdata_override.unwrap_or(tzdata);
    if local != recorded.as_str() {
        // regen-r3 (review P1): fail, never a green skip. The
        // `blocked-branch-return` negative control reverts exactly this
        // branch to the old println + return and must be killed by the
        // guard-semantics test on every host.
        panic!(
            "#360-2 BLOCKED: tzdata {local} (this Node) != {recorded} (gold-recorded); \
             the case bytes cannot be re-verified byte-wise on this host, so this \
             guard fails instead of silently skipping the byte comparison"
        );
    }
    super::baseline::regenerates_identically("retry-after-dialects");
}

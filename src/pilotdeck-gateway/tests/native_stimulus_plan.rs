//! #1368 independent TS input declaration vs native collection plan.
//!
//! #1408: the fixture is a Python child, so `Command::output()` parked the test
//! binary forever whenever the interpreter stopped making progress (the settle
//! wait is bounded inside the collector, but any regression, host stall or
//! deadlock above those bounds left `cargo test` waiting with no deadline and
//! no diagnostics). Launch it through the repository's shared bounded child
//! runner instead: the same `common/baseline.rs` deadline that already guards
//! every fixture generator (poll `try_wait` under one total deadline, kill and
//! reap the whole process group, bound both pipe drains). The `#280` marker in
//! that runner's timeout text is inherited from the shared helper, not new.
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

#[allow(dead_code)]
#[path = "common/baseline.rs"]
mod baseline;

/// Run one isolated fixture child under the shared native-collector backstop.
///
/// Reachability of the collector's own 930 s bound from *this* fixture is
/// measured, not assumed: `PlanTests.test_registry_free_settle_synthetic_control`
/// loads `native-witness/test_r11.py` and runs `R11Tests.test_collector_wait_wired`
/// / `test_collector_inapplicable_without_registry_file`, both of which call
/// `E2ETests._collect(real_settle=True)` → `_native_run(session_domain=True)`.
/// Instrumenting `_native_run` in a scratch copy recorded **two** entries during
/// that single control (`_tmp/20261107-issue-1408/07-reachability-probe.log`).
/// The 19 controls as a whole take 0.9 s
/// (`_tmp/20261107-issue-1408/01-baseline-full19.txt`); the #2396 adapter
/// controls add ~0.6 s of in-process scripted drives (same measurement run:
/// 28 controls, 1.4 s), so nothing legitimate comes anywhere near the backstop.
fn fixture_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/native-stimulus-plan/test_plan.py")
}

/// Run one isolated fixture child under a total deadline.
///
/// `Err` carries the launcher's timeout diagnostic or the fixture's own
/// stdout/stderr; the caller decides whether that is a test failure.
fn run_fixture(script: &Path, tests: &[&str], deadline: Duration) -> Result<(), String> {
    let mut command = Command::new("python3");
    command.arg("-I").arg(script).args(tests);
    let (status, stdout, stderr) =
        baseline::run_generator_with_deadline(&mut command, "native-stimulus-plan", deadline)?;
    if status.success() {
        return Ok(());
    }
    Err(format!(
        "#1368 stimulus plan controls failed\n{}\n{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    ))
}

fn controls(tests: &[&str]) {
    run_fixture(
        &fixture_script(),
        tests,
        baseline::native_collector_backstop(),
    )
    .unwrap_or_else(|error| panic!("#1408 launch isolated stimulus plan controls: {error}"));
}

#[test]
fn native_stimulus_plan_controls() {
    controls(&[
        "PlanTests.test_boundary_coverage",
        "PlanTests.test_manifest_rejects_gap",
        "PlanTests.test_manifest_rejects_duplicate",
        "PlanTests.test_unknown_plan_reason",
        "PlanTests.test_duplicate_plan_reason",
        "PlanTests.test_empty_boundary_not_witnessed",
        "PlanTests.test_measurement_unchecked_not_pass",
        "PlanTests.test_empty_order_not_witnessed",
        "PlanTests.test_measurement_rejects_empty_order",
        "PlanTests.test_measurement_empty_order_controls",
        "PlanTests.test_memory_runners_do_not_compile_tree",
        "PlanTests.test_phase_requires_prefix_before_wait",
        "PlanTests.test_fault_requires_entry_before_failure",
        "PlanTests.test_measurement_registered_denominator",
        "PlanTests.test_measurement_rejects_empty_repeats",
        "PlanTests.test_runner_target_environment",
        "PlanTests.test_registry_free_settle_synthetic_control",
        "PlanTests.test_unknown_and_detached_plan",
        "PlanTests.test_unwitnessed_is_not_run",
        "PlanTests.test_memory_rehearsal_holds_tree_lock",
        // #1528 fix-r1 (review DS sibling#1): #1530 added this pin test without
        // re-listing it here, so `cargo test` never ran it -- the same
        // anti-pattern as the #2396 block above (this list is the only
        // selective entry for the fixture).
        "PlanTests.test_hardcoded_counts_track_calibration",
        "AdapterTests.test_storage_arms_match_the_arming_table",
        "AdapterTests.test_driver_slow_status_choreography",
        "AdapterTests.test_driver_slow_append_two_barriers",
        "AdapterTests.test_driver_known_final_pending_release",
        "AdapterTests.test_driver_after_complete_fence",
        "AdapterTests.test_driver_unknown_first_timer_observation",
        "AdapterTests.test_adapter_precise_missing_per_target",
        "AdapterTests.test_adapter_cross_case_receipts_do_not_attest",
        "AdapterTests.test_adapter_registry_closure_requires_product_receipt",
        // The #2396 fix rounds added controls in the fixture without
        // re-listing them here, so `cargo test` never ran them (codex r3
        // sibling P2#3 — the fixture list is the only selective entry).
        "AdapterTests.test_case_budget_counts_every_armed_barrier",
        "AdapterTests.test_driver_bounded_release_receipt_does_not_attest",
        "AdapterTests.test_driver_cross_case_loop_final_does_not_attest",
        "AdapterTests.test_driver_foreign_request_final_is_ignored",
        "AdapterTests.test_driver_foreign_prefix_frame_does_not_attest_phase",
        "AdapterTests.test_driver_foreign_timeout_frame_keeps_timer_clear",
        "AdapterTests.test_plan_missing_request_id_attests_nothing",
        "AdapterTests.test_driver_order_edge_final_before_held_release",
        "AdapterTests.test_driver_receipt_without_seq_attests_no_order",
        "AdapterTests.test_plan_loop_receipt_without_seq_attests_no_phase_bracket",
        "AdapterTests.test_plan_status_block_without_seq_attests_no_rearm_edge",
        "AdapterTests.test_plan_missing_control_records_attest_no_rearm_edge",
        "AdapterTests.test_plan_special_variant_seq_edges_attest",
        "AdapterTests.test_plan_bool_seq_never_attests_order_edge",
        "AdapterTests.test_driver_pidless_loop_receipts_do_not_attest",
        "AdapterTests.test_driver_slow_append_out_of_order_receipts_do_not_attest",
    ]);
}

#[test]
#[ignore = "requires Node v22.22.0 / ICU 78.2; executes frozen TS preregistration"]
fn native_stimulus_plan_matches_preregistration() {
    controls(&["PlanTests.test_exact_registration"]);
}

/// #1408 negative control: prove the deadline, not the fixture, ends a blocking
/// child.
///
/// The wedge is self-bounded on purpose. A launcher that lost its deadline must
/// make this test *fail* in about 15 s (it would return `Ok` with the wedge's
/// exit status), never hang the binary -- the failure mode #1408 removes has to
/// stay observable rather than turn the control itself into the hang. Measured
/// with the deadline removed: `FAILED ... finished in 15.02s`
/// (`_tmp/20261107-issue-1408/05-remote-cb8-negctl.log`).
#[test]
fn blocking_fixture_control_reports_the_deadline() {
    let scratch = scratch_dir("wedged-fixture");
    let wedge = scratch.join("wedge.py");
    std::fs::write(&wedge, "import time\ntime.sleep(15)\n").expect("#1408 write wedge fixture");

    let started = Instant::now();
    let error = run_fixture(&wedge, &[], Duration::from_secs(1))
        .expect_err("#1408 a blocking fixture must fail at the total deadline");
    let elapsed = started.elapsed();
    let _ = std::fs::remove_dir_all(&scratch);

    assert!(
        error.contains("exceeded the 1s total deadline"),
        "#1408 the launcher must report its own deadline, not the fixture's:\n{error}"
    );
    // The deadline path also pays the shared runner's bounded grace chain
    // (kill helper + two reaps + two pipe joins, each capped at 2 s) in the
    // worst case, so this bound only has to separate "deadline fired" from
    // "deadline never existed" — the wedge itself is 15 s.
    assert!(
        elapsed < Duration::from_secs(15),
        "#1408 the launcher returned after {elapsed:?}; the 1s deadline did not bound the wait"
    );
}

/// #1408 review r1 (P2 #5): pin the headroom the doc comment claims.
///
/// The backstop only does its job while it stays *looser* than one collector
/// entry: `SESSION_LIMIT_SECONDS` plus the largest `gateway.close`. A future
/// `SESSION_LIMIT_SECONDS = 1800` would silently turn the launcher into a
/// preemptive kill that hides the collector's `TimeoutError`. Scrape the numbers
/// and recompute instead of trusting prose.
///
/// Scope, stated so the guard is not read as more than it is: this checks one
/// entry, not `entries × entry` (see `NATIVE_COLLECTOR_BACKSTOP`'s doc for why
/// multi-entry failing runs are allowed to be cut short).
///
/// Both `gateway.close` sites are scraped, one per consumer fixture: the same
/// backstop serves `native-session-e2e` (`test_e2e.py`) and the
/// `native-witness`/`native-stimulus-plan` fixtures (`test_r11.py`). Today the
/// witness side is the smaller one (3 vs 30), so dropping it would leave a
/// silent hole the day it grows.
#[test]
fn backstop_stays_looser_than_the_collector_bound() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let collector = std::fs::read_to_string(root.join("scripts/native-witness/collect.py"))
        .expect("#1408 read the collector");
    let e2e = std::fs::read_to_string(
        root.join("src/pilotdeck-gateway/tests/fixtures/native-session-e2e/test_e2e.py"),
    )
    .expect("#1408 read the e2e fixture");
    let r11 = std::fs::read_to_string(
        root.join("src/pilotdeck-gateway/tests/fixtures/native-witness/test_r11.py"),
    )
    .expect("#1408 read the R11 fixture");
    let capture = sole_number_after(&collector, "SESSION_LIMIT_SECONDS = ");
    let close = largest_number_after(&e2e, "gateway.close(")
        .max(largest_number_after(&r11, "gateway.close("));
    let inner = capture + close;
    let backstop = baseline::NATIVE_COLLECTOR_BACKSTOP;
    assert!(
        backstop.as_secs() > inner,
        "#1408 the {backstop:?} launch deadline is not looser than the collector's \
         own {inner}s bound ({capture}s capture + {close}s gateway close); it would preempt the \
         collector's diagnostic with an opaque kill"
    );
}

/// The headroom guard above is only as good as its parser, so pin the parser's
/// refusal modes: a silently *smaller* number is the one error it must never
/// make.
#[test]
#[should_panic(expected = "is not a plain integer literal")]
fn scraper_rejects_underscored_literals() {
    let _ = parse_bounded("1_5)", "gateway.close(");
}

#[test]
#[should_panic(expected = "is not followed by an integer literal")]
fn scraper_rejects_non_literal_arguments() {
    let _ = parse_bounded("timeout)", "gateway.close(");
}

/// The integer literal assigned to `marker`, which must appear exactly once.
///
/// A single occurrence is asserted rather than assumed: `split_once` would read
/// a stale docstring value that happens to sit above the real assignment — a
/// silently *smaller* bound, the one error this guard must never make.
fn sole_number_after(source: &str, marker: &str) -> u64 {
    let mut hits = source.match_indices(marker);
    let (at, _) = hits
        .next()
        .unwrap_or_else(|| panic!("#1408 `{marker}` not found in the fixture sources"));
    assert!(
        hits.next().is_none(),
        "#1408 `{marker}` appears more than once; the guard cannot tell which assignment is live"
    );
    parse_bounded(&source[at + marker.len()..], marker)
}

/// The largest integer argument among every `marker` call site.
fn largest_number_after(source: &str, marker: &str) -> u64 {
    let calls: Vec<(usize, &str)> = source.match_indices(marker).collect();
    assert!(
        !calls.is_empty(),
        "#1408 no `{marker}` call in the fixture sources"
    );
    calls
        .into_iter()
        .map(|(at, _)| parse_bounded(&source[at + marker.len()..], marker))
        .max()
        .expect("#1408 at least one call site was asserted above")
}

/// Parse the integer literal at the head of `rest`.
///
/// The literal must end where a plain integer argument ends (`,` or `)`, or a
/// newline for an assignment). Anything else — `0x1e`, `1_5`, a bare name like
/// `timeout`, or a docstring mention — panics instead of being filtered away.
/// A silently *smaller* bound is the one failure mode this guard cannot afford:
/// it would report headroom that does not exist.
fn parse_bounded(rest: &str, marker: &str) -> u64 {
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    assert!(
        !digits.is_empty(),
        "#1408 `{marker}` is not followed by an integer literal: {:?}",
        rest.chars().take(24).collect::<String>()
    );
    let tail = rest[digits.len()..].chars().next();
    assert!(
        matches!(tail, Some(',' | ')' | '\n' | '\r') | None),
        "#1408 `{marker}{digits}` is not a plain integer literal (next char {tail:?})"
    );
    digits
        .parse()
        .unwrap_or_else(|error| panic!("#1408 unparsable `{marker}{digits}`: {error}"))
}

/// A fresh directory: pid plus nanos, never pid alone (macOS reuses pids after a
/// killed run) and `create_dir` refuses a path that already exists.
fn scratch_dir(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let path =
        std::env::temp_dir().join(format!("pdrs-1408-{label}-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&path).unwrap_or_else(|error| panic!("#1408 create {path:?}: {error}"));
    path
}

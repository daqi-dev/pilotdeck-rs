#!/usr/bin/env python3
"""#1145 HEAD-only controls: --check-anchors | NEW_OUTPUT_DIR [mutation ...].

--check remains a compatibility alias for --check-anchors.

The catalog has exactly 79 controls. Each selected control runs its named
positive, compiles the mutant, runs the named negative, restores, and reruns the
positive. All verdicts are published only after the source guard has verified
restoration. New/dirty source and test files must first be committed by the
coordinator; this script never creates a second checkout or bypasses HEAD.
"""
from contextlib import contextmanager
import difflib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import negative_process
from negative_process import (
    SourceBaselineError,
    TreeLock,
    _head_bytes,
    exclusive_write,
    mutating_sources,
    new_output_directory,
    run_owned,
    wait_for_slot,
)
from negative_suite import (
    Case,
    classify,
    invalidate_completed,
    passed,
    publish_summary,
)

root = Path(__file__).resolve().parents[5]
turn = root / "src/pilotdeck-cli/src/sdk/turn.rs"
transport = root / "src/pilotdeck-cli/src/sdk/transport.rs"
signals = root / "src/pilotdeck-cli/src/sdk/signals.rs"
output = root / "src/pilotdeck-cli/src/sdk/output.rs"
replay = root / "src/pilotdeck-cli/tests/common/signal_replay.rs"
d1_ledger = root / "specs/gateway-parity/diffs/D1-SDK-SIGNAL-UNHANDLED-REJECTION-001.md"
close_order_ledger = root / "specs/gateway-parity/diffs/D1-SDK-WS-CLOSE-ORDER-001.md"
skills_tests = root / "src/pilotdeck-cli/src/skills.rs"
migrate_tests = root / "src/pilotdeck-cli/src/skills_migrate.rs"
wrapper_source = root / "src/pilotdeck-cli/tests/fixtures/sdk-signals/sigint_default.py"
fixture = root / "src/pilotdeck-cli/tests/fixtures/sdk-signals"
abort_state = root / "src/pilotdeck-cli/tests/fixtures/sdk-signal-abort-state"
# #1885: the only file under the fixture trees a control rewrites. Everything
# else there is a golden read by resident tests and by the replay children.
MUTABLE_FIXTURE_FILES = frozenset({wrapper_source})


def guarded_paths():
    """Exactly the files a control rewrites.

    #1885: `negative_process.mutating_sources` rewrites every guarded path on
    exit with a plain `write_bytes` and no atomic publish, so a SIGKILL inside
    that window truncates whatever the set carries. The whole-fixture globs
    that used to live at the call site put every golden (`source.json`,
    `oracle.json`, `capture.py`, `generate.mjs`, ...) at that risk for files no
    control ever touches. Leftover mutants stay detected without them:
    `_assert_workspace_sources` scans the whole `src/` tree against HEAD. Of
    the two `specs/` ledgers, only `d1_ledger` is rewritten by a control and so
    stays here; `close_order_ledger` is read-only and proves itself against
    HEAD in `assert_close_order_ledger_matches_head`.
    """
    return sorted({row[1] for row in mutations}, key=str)


EXPECTED_TOTAL = 79
# #1382: per-ID unordered-close controls are pinned to the registered
# close-order ledger IDs. These three whole-scope boundary controls are not
# one-per-ID rows, so they are excluded from that pin; a future boundary
# control must be added here or the pin below fails with an unregistered ID.
UNORDERED_CLOSE_PREFIX = "unordered-close-"
UNORDERED_CLOSE_BOUNDARY = (
    "unordered-close-overbroad",
    "unordered-close-unregistered-id",
    "unordered-close-non-array",
)
_UNORDERED_CLOSE_ID_TEST = (
    "signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics"
)
# #1382: the three whole-scope boundary rows are not one-per-ID, but their
# content is just as load-bearing. Swapping one boundary row's anchor, mutant
# value, target test or assertion for another's keeps every name check green
# while deleting the control, so pin the whole row. Keys must stay equal to
# UNORDERED_CLOSE_BOUNDARY (checked in check_catalog).
UNORDERED_CLOSE_BOUNDARY_EXPECTED = {
    "unordered-close-overbroad": (
        replay,
        "    if !UNORDERED_CLOSE_CASES.contains(&id) {",
        "    if false && !UNORDERED_CLOSE_CASES.contains(&id) {",
        _UNORDERED_CLOSE_ID_TEST,
        "#1252 stable close swap must fail:",
    ),
    "unordered-close-unregistered-id": (
        replay,
        'const UNORDERED_CLOSE_CASES: &[&str] = &[\n',
        'const UNORDERED_CLOSE_CASES: &[&str] = &[\n'
        '    "not-a-registered-signal-cell",\n',
        "signal_replay::comparison_tests::unordered_close_scope_equals_registration",
        "#1252 unordered close scope equals measured registration",
    ),
    "unordered-close-non-array": (
        replay,
        "    let Some(actual) = actual.as_array() else {\n        return false;\n    };",
        "    let Some(actual) = actual.as_array() else {\n        return true;\n    };",
        _UNORDERED_CLOSE_ID_TEST,
        "#1252 non-array closes must fail:",
    ),
}
BUILD_TIMEOUT = 900
TEST_TIMEOUT = 90

# Declarative whole-file anchors are understood by main's anchor_selfcheck.
# Every old fragment must occur exactly once; mutants are never cumulative.
mutations = [
    ('hello-ready-loop-starves-deadline', transport, '        let hello = loop {\n            let frame = self.receive(Some((deadline, message))).await?;\n            if string_field(&frame, "type").as_deref() == Some("hello_ok") {\n                break frame;\n            }\n        };', '        let hello = timeout_at(deadline, async {\n            loop {\n                let frame = self.receive(None).await?;\n                if string_field(&frame, "type").as_deref() == Some("hello_ok") {\n                    return Ok::<_, Failure>(frame);\n                }\n            }\n        }).await.map_err(|_| error(message))??;', 'product::sdk::transport::tests::hello_ready_frames_cannot_starve_deadline', '#1145 hello fixed deadline wins over ready frames'),
    ('request-ready-loop-starves-deadline', transport, '        loop {\n            let frame = self.receive(Some((deadline, &message))).await?;\n            if string_field(&frame, "type").as_deref() != Some("response")\n                || string_field(&frame, "id").as_deref() != Some(&id)\n            {\n                continue;\n            }\n            if frame.get("ok").and_then(JsValue::as_bool) == Some(true) {\n                return Ok(take_field(frame, "result"));\n            }\n            let message = frame\n                .get("error")\n                .and_then(|value| string_field(value, "message"));\n            return Err(error(\n                message.as_deref().unwrap_or("Gateway request failed."),\n            ));\n        }', '        timeout_at(deadline, async {\n        loop {\n            let frame = self.receive(None).await?;\n            if string_field(&frame, "type").as_deref() != Some("response")\n                || string_field(&frame, "id").as_deref() != Some(&id)\n            {\n                continue;\n            }\n            if frame.get("ok").and_then(JsValue::as_bool) == Some(true) {\n                return Ok(take_field(frame, "result"));\n            }\n            let message = frame\n                .get("error")\n                .and_then(|value| string_field(value, "message"));\n            return Err(error(\n                message.as_deref().unwrap_or("Gateway request failed."),\n            ));\n        }\n        }).await.map_err(|_| error(&message))?', 'product::sdk::transport::tests::request_ready_frames_cannot_starve_deadline', '#1145 request fixed deadline wins over ready frames'),
    (
        "abort-ready-starves-deadline",
        transport,
        "        Ok(result) if Instant::now() < deadline => result,",
        "        Ok(result) => result,",
        "product::sdk::turn::tests::abort_ready_frames_cannot_starve_deadline",
        "#1145 fixed abort deadline wins over ready frames",
    ),
    (
        "listener-installed-before-control",
        turn,
        '    let mut control = Transport::connect(connection, "pilotdeck-cli", true).await?;',
        '    let _early_signals = Signals::install()?;\n'
        '    let mut control = Transport::connect(connection, "pilotdeck-cli", true).await?;',
        "signal_cases::run_signal_sigint",
        "#1145 run--signal-sigint four surfaces, abort count, listeners and connections match TS",
    ),
    (
        "listeners-not-removed",
        signals,
        "    pub(super) fn remove(&mut self) {\n"
        "        if !self.removed {",
        "    pub(super) fn remove(&mut self) {\n"
        "        if false && !self.removed {",
        "signal_cases::run_complete_cleanup",
        "#1145 run--complete-cleanup four surfaces, abort count, listeners and connections match TS",
    ),
    (
        "abort-does-not-await-reply",
        turn,
        "        let next = next_query_frame(query, &initialized.request_id, &abort_ids, signals);",
        "        if !abort_ids.is_empty() {\n"
        "            return QueryOutcome::Closed(client_closed());\n"
        "        }\n"
        "        let next = next_query_frame(query, &initialized.request_id, &abort_ids, signals);",
        "signal_cases::run_abort_hold_sigint",
        "#1145 run--abort-hold-sigint four surfaces, abort count, listeners and connections match TS",
    ),
    (
        "held-predecessor-exits-early",
        turn,
        "            initialize_turn(query, argv, &session, &prompt, project).await",
        "            match tokio::time::timeout(\n"
        "                std::time::Duration::from_secs(2),\n"
        "                initialize_turn(query, argv, &session, &prompt, project),\n"
        "            ).await {\n"
        "                Ok(result) => result,\n"
        "                Err(_) => Err(Failure::Error(\"mutant predecessor deadline\".into())),\n"
        "            }",
        # #1252: this held model predecessor still passes strict close order.
        "signal_cases::run_query_model_hold_sigterm",
        "#1145 run--query-model-hold-sigterm four surfaces, abort count, listeners and connections match TS",
    ),
    (
        "completed-query-connection-retained",
        turn,
        "        QueryOutcome::Complete(output) => {\n"
        "            if let Some(query) = &mut query {\n"
        "                query.close().await;",
        "        QueryOutcome::Complete(output) => {\n"
        "            if let Some(query) = &mut query {\n"
        "                query.wait_peer_closed().await;\n"
        "                query.close().await;",
        "signal_cases::run_complete_cleanup",
        "#1145 run--complete-cleanup four surfaces, abort count, listeners and connections match TS",
    ),
    (
        # #1260: 16 measured STABLE-DIFF cells observe the query (2) close
        # before control (1) because TS drops the query at process exit before
        # its `finally { client.close() }`. Restoring the old control-first
        # order makes the named matrix cell report a "connection closures"
        # difference, so this binds the product order, not just the comparator.
        "initialization-query-close-before-control",
        turn,
        "            if let Some(mut query) = query.take() {\n"
        "                query.flush_pending().await;\n"
        "            }\n"
        "            control.close().await;",
        "            control.close().await;\n"
        "            if let Some(query) = &mut query {\n"
        "                query.flush_pending().await;\n"
        "            }",
        "signal_cases::run_query_hello_hold_sigint",
        "#1145 run--query-hello-hold-sigint four surfaces, abort count, listeners and connections match TS",
    ),
    (
        # #1178: mapping the stream-phase socket error with an abort in flight
        # back to "retain the query and await the abort reply" is the tempting
        # wrong alternative to the measured TS crash. The RST cell observes no
        # exit inside the 12s window (the abort deadline is 30s on a dead
        # socket), so status/natural surfaces differ and the cell must fail.
        "rst-abort-waits-for-reply",
        turn,
        "                return if abort_ids.is_empty() {\n"
        "                    QueryOutcome::Retain(error)\n"
        "                } else {\n"
        "                    QueryOutcome::InitializationFailed(error)\n"
        "                };",
        "                return if abort_ids.is_empty() {\n"
        "                    QueryOutcome::Retain(error)\n"
        "                } else {\n"
        "                    return retain_query(error, abort_ids, abort_deadline);\n"
        "                };",
        "signal_cases::run_abort_rst_sigint",
        "#1145 run--abort-rst-sigint four surfaces, abort count, listeners and connections match TS",
    ),
    (
        # #1178: a mock that swallows the reset keeps the Rust child alive,
        # so the RST cell degrades to an observation timeout instead of the
        # TS-measured exit -- the replay would stop measuring the mapping.
        "rst-mock-never-resets",
        replay,
        "                    Some(Action::Reset) => {\n"
        "                        let _ = socket.get_ref().set_linger(Some(Duration::ZERO));\n"
        "                        break;\n"
        "                    }",
        "                    Some(Action::Reset) => { continue; }",
        "signal_cases::run_abort_rst_sigint",
        "#1145 run--abort-rst-sigint four surfaces, abort count, listeners and connections match TS",
    ),
    (
        # #1260 r1: the 16-cell list is also pinned exhaustively against all
        # 96 captures. Broadening the shape predicate to accept any second
        # close code makes the measured set 25 cells, so the set equality has
        # to fail -- proving the exhaustiveness assertion is live rather than
        # decorative.
        "stable-order-pin-broadened",
        replay,
        '                    && closes[1]["code"].as_u64() == Some(1005)',
        '                    && closes[1]["code"].as_u64() != Some(0)',
        "signal_replay::comparison_tests::stable_query_before_control_cells_keep_concrete_order",
        "#1145 query-before-control shape is exactly the measured stable set",
    ),
    ('abort-stale-run-id', output, 'self.run_id = run_id.clone();', 'self.run_id = "requested-run".into();', 'product::sdk::turn::tests::abort_uses_consumed_run_id_without_utf16_loss', '#1145 abort uses latest lossless runId'),
    ('consumer-error-retains-query', output, 'return Err(EventError::Consumer(Failure::Error(\n                "event.type.includes is not a function".into(),', 'return Err(EventError::Iterator(Failure::Error(\n                "event.type.includes is not a function".into(),', 'product::sdk::turn::tests::output_failure_preserves_iterator_vs_consumer_lifetime', '#1145 event error matches TS iterator or consumer lifetime'),
    ('pending-abort-reader-finishes-early', turn, '    abort_ids: &[String],\n    deadline: Instant,\n) -> Result<(), Failure> {\n    loop {', '    abort_ids: &[String],\n    deadline: Instant,\n) -> Result<(), Failure> {\n    if abort_ids.len() == 1 { return Ok(()); }\n    loop {', 'product::sdk::turn::tests::pending_abort_keeps_query_until_reply', '#1145 abort cannot finish before its reply'),
    ('abort-send-extends-predecessor-deadline', transport, 'timeout_at(deadline, request)', 'timeout_at(deadline + REQUEST_TIMEOUT, request)', 'product::sdk::turn::tests::abort_send_respects_predecessor_deadline', '#1145 blocked second abort cannot extend first deadline'),
    ('node-version-required', replay, 'r"\\}\\n\\nNode\\.js v22\\.22\\.0\\n\\z",', 'r"\\}\\n\\n(?:Node\\.js v22\\.22\\.0\\n)?\\z",', 'signal_replay::comparison_tests::stderr_missing_node_version_is_not_normalized', '#1145 incomplete Node version cannot pass'),
    ('node-frames-required', replay, 'r"(?:    at [^ \\t\\r\\n{}][^\\r\\n{}]*\\n)*    at [^ \\t\\r\\n{}][^\\r\\n{}]* \\{\\n",', 'r"(?:(?:    at [^ \\t\\r\\n{}][^\\r\\n{}]*\\n)*    at [^ \\t\\r\\n{}][^\\r\\n{}]* \\{\\n)?",', 'signal_replay::comparison_tests::stderr_missing_stack_frames_is_not_normalized', '#1145 missing stack frames cannot pass'),
    ('node-business-message-consistent', replay, "if business.strip_suffix('\\n') == Some(message) {", 'if !message.is_empty() {', 'signal_replay::comparison_tests::stderr_changed_business_line_is_not_hidden', '#1145 mismatched business and exception remain literal'),
    ('rust-stderr-never-normalized', replay, 'ts_stderr(ts).as_bytes() == rust', 'ts_stderr(ts) == ts_stderr(std::str::from_utf8(rust).expect("#1145 mutant Rust UTF8"))', 'signal_replay::comparison_tests::stderr_rust_node_stack_is_not_normalized', '#1145 Rust Node stack cannot pass'),
    # #1252: preserve observer, exact-match and approved-reason order independently.
    ('close-observer-order', replay, '                self.closes.push(\n                    json!({"connection":connection,"code":code,"reason":reason,"cleanup":cleanup}),\n                );', '                self.closes.push(\n                    json!({"connection":connection,"code":code,"reason":reason,"cleanup":cleanup}),\n                );\n                self.closes.sort_by_key(|row| row["connection"].as_u64());', 'signal_replay::comparison_tests::close_events_keep_arrival_order', '#1252 close events retain arrival order and fields'),
    ('close-exact-order', replay, '    if close_rows_match(id, &closes, actual) {', '    closes.sort_by_key(|row| row["connection"].as_u64());\n    if close_rows_match(id, &closes, actual) {', 'signal_replay::comparison_tests::close_order_is_strict_including_reason_exception', '#1252 swapped actual closes must fail despite unchanged trace'),
    ('close-approved-order', replay, '            }\n        }\n    }\n    close_rows_match(id, &closes, actual)', '            }\n        }\n        closes.sort_by_key(|row| row["connection"].as_u64());\n    }\n    close_rows_match(id, &closes, actual)', 'signal_replay::comparison_tests::close_order_is_strict_including_reason_exception', '#1252 swapped actual closes must fail despite unchanged trace'),

    ('close-exception-scoped', replay, 'let approved = matches!(', 'let approved = true || matches!(', 'signal_replay::comparison_tests::close_reason_exception_is_exact_and_scoped', '#1145 unregistered close case cannot pass'),
    ('close-code-scoped', replay, '&& row["code"] == 4003', '&& !row["code"].is_null()', 'signal_replay::comparison_tests::close_reason_exception_is_exact_and_scoped', '#1145 close exception requires TS code'),
    ('close-connection-scoped', replay, 'if row["connection"] == 2', 'if !row["connection"].is_null()', 'signal_replay::comparison_tests::close_reason_exception_is_exact_and_scoped', '#1145 close exception requires TS connection'),
    ('close-cleanup-scoped', replay, '&& row["cleanup"] == false', '&& !row["cleanup"].is_null()', 'signal_replay::comparison_tests::close_reason_exception_is_exact_and_scoped', '#1145 close exception requires TS cleanup'),
    ('close-ts-reason-scoped', replay, '&& row["reason"] == ""', '&& row["reason"].is_string()', 'signal_replay::comparison_tests::close_reason_exception_is_exact_and_scoped', '#1145 close exception requires TS reason'),
    ('sigint-wrapper-resets-ignore', wrapper_source, 'signal.signal(signal.SIGINT, signal.SIG_DFL)', '# mutant: keep inherited SIGINT disposition', 'signal_replay::comparison_tests::signal_wrapper_restores_inherited_sigint_ignore', '#1145 wrapper restores SIGINT default before exec'),

    ('node-nonblank-source', replay, 'r" +[^ \\t\\r\\n][^\\r\\n]*\\n +\\^\\n\\n",', 'r" +[^\\r\\n]+\\n +\\^\\n\\n",', 'signal_replay::comparison_tests::stderr_blank_source_or_stack_body_is_not_normalized', '#1145 blank source excerpt cannot pass'),
    ('node-nonblank-stack', replay, 'r"(?:    at [^ \\t\\r\\n{}][^\\r\\n{}]*\\n)*    at [^ \\t\\r\\n{}][^\\r\\n{}]* \\{\\n",', 'r"(?:    at [^\\r\\n{}]+\\n)*    at [^\\r\\n{}]+ \\{\\n",', 'signal_replay::comparison_tests::stderr_blank_source_or_stack_body_is_not_normalized', '#1145 blank stack body cannot pass'),

    ('rejection-exception-ninth-cell', replay, '        _ => None,\n    }\n}\n\nfn stderr_matches_for_case', '        "run--terminal-during-abort-open-sigint" => Some(b"d1 fixture rejected\\n"),\n        _ => None,\n    }\n}\n\nfn stderr_matches_for_case', 'signal_replay::comparison_tests::unhandled_rejection_stderr_is_exact_and_scoped', '#1145 unregistered unhandled rejection cannot pass'),
    ('rejection-exact-rust-line', replay, "expected == rust && ts.split_inclusive('\\n').any(|line| line == exception)", "ts.split_inclusive('\\n').any(|line| line == exception)", 'signal_replay::comparison_tests::unhandled_rejection_stderr_is_exact_and_scoped', '#1145 unhandled rejection requires exact Rust line'),
    ('rejection-abort-error-line', replay, '| "resume--abort-error-sigterm" => Some(b"d1 fixture rejected\\n"),', '| "resume--abort-error-sigterm" => Some(b"wrong error\\n"),', 'signal_replay::comparison_tests::unhandled_rejection_stderr_is_exact_and_scoped', '#1145 approved unhandled rejection line must pass'),
    ('rejection-terminal-error-line', replay, '| "resume--terminal-during-abort-final-sigterm" => Some(b"Gateway client closed.\\n"),', '| "resume--terminal-during-abort-final-sigterm" => Some(b"wrong error\\n"),', 'signal_replay::comparison_tests::unhandled_rejection_stderr_is_exact_and_scoped', '#1145 approved unhandled rejection line must pass'),
    ('rejection-exit-remains-strict', replay, 'if actual[field] != expected[field] {', 'if field != "naturalExit" && actual[field] != expected[field] {', 'signal_replay::comparison_tests::unhandled_rejection_preserves_other_surfaces', '#1145 rejection exception cannot hide naturalExit'),
    ('rejection-stdout-remains-strict', replay, 'if stdout != expected_stdout.as_bytes() {', 'if approved_signal_stderr(id).is_none() && stdout != expected_stdout.as_bytes() {', 'signal_replay::comparison_tests::unhandled_rejection_preserves_other_surfaces', '#1145 rejection exception cannot hide stdout'),

    ('rejection-domain-ninth-distinct', replay, '        _ => None,\n    }\n}\n\nfn stderr_matches_for_case', '        "run--terminal-during-abort-open-sigint" => Some(b"distinct ninth error\\n"),\n        _ => None,\n    }\n}\n\nfn stderr_matches_for_case', 'signal_replay::comparison_tests::approved_stderr_match_domain_is_exact', '#1145 approved stderr match arms equal exactly eight IDs'),
    ('rejection-domain-outside-matrix', replay, '        _ => None,\n    }\n}\n\nfn stderr_matches_for_case', '        "not-a-registered-signal-cell" => Some(b"outside matrix error\\n"),\n        _ => None,\n    }\n}\n\nfn stderr_matches_for_case', 'signal_replay::comparison_tests::approved_stderr_match_domain_is_exact', '#1145 approved stderr match arms equal exactly eight IDs'),
    ('rejection-domain-missing-cell', replay, '        | "run--abort-error-sigterm"\n', '', 'signal_replay::comparison_tests::approved_stderr_match_domain_is_exact', '#1145 approved stderr match arms equal exactly eight IDs'),
    ('rejection-registered-difference-drift', d1_ledger, '- `run--abort-error-sigint`', '- `mutant-not-a-registered-cell`', 'signal_replay::comparison_tests::approved_stderr_match_domain_is_exact', '#1145 approved stderr IDs equal registered difference'),
    ('rejection-original-error-required', replay, "expected == rust && ts.split_inclusive('\\n').any(|line| line == exception)", 'expected == rust && !exception.is_empty()', 'signal_replay::comparison_tests::unhandled_rejection_requires_original_error', '#1145 rejection requires exact original TS error'),
    ('rejection-hooks-from-original', replay, '            let removed = expected["hooks"]\n                .as_array()\n                .expect("#1145 rejection surface TS hooks")', '            let removed = !expected["hooks"]\n                .as_array()\n                .expect("#1145 rejection surface TS hooks")', 'signal_replay::comparison_tests::unhandled_rejection_preserves_other_surfaces', '#1145 rejection all surfaces positive'),

    ('nested-marker-child-name', skills_tests, '"::marker_child_over_file_size_limit"', '"::marker_child_over_file_size_limit_renamed"', 'product::skills::tests::marker_write_reports_the_write_stage_like_node', '#1145 marker child emitted no WRITE_ERROR line'),
    ('nested-umask-child-name', migrate_tests, '"::copy_tree_preserves_modes_like_node_async_cp"', '"::copy_tree_preserves_modes_like_node_async_cp_renamed"', 'product::skills_migrate::tests::copy_tree_preserves_modes_like_node_async_cp', '#273 umask rerun matched no test'),

    # #1252 measured order policy: one independently bound control per ID.
    ('unordered-close-run--query-resume-hold-sigint', replay, '\n    "run--query-resume-hold-sigint",\n', '\n    "run--query-resume-hold-sigint-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: run--query-resume-hold-sigint'),
    ('unordered-close-run--query-model-hold-sigint', replay, '\n    "run--query-model-hold-sigint",\n', '\n    "run--query-model-hold-sigint-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: run--query-model-hold-sigint'),
    ('unordered-close-run--abort-error-sigint', replay, '\n    "run--abort-error-sigint",\n', '\n    "run--abort-error-sigint-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: run--abort-error-sigint'),
    ('unordered-close-run--second-sigint', replay, '\n    "run--second-sigint",\n', '\n    "run--second-sigint-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: run--second-sigint'),
    ('unordered-close-run--second-sigint-sigterm', replay, '\n    "run--second-sigint-sigterm",\n', '\n    "run--second-sigint-sigterm-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: run--second-sigint-sigterm'),
    ('unordered-close-run--terminal-consumed-failed-sigint', replay, '\n    "run--terminal-consumed-failed-sigint",\n', '\n    "run--terminal-consumed-failed-sigint-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: run--terminal-consumed-failed-sigint'),
    ('unordered-close-run--terminal-during-abort-final-sigint', replay, '\n    "run--terminal-during-abort-final-sigint",\n', '\n    "run--terminal-during-abort-final-sigint-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: run--terminal-during-abort-final-sigint'),
    ('unordered-close-run--query-resume-hold-sigterm', replay, '\n    "run--query-resume-hold-sigterm",\n', '\n    "run--query-resume-hold-sigterm-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: run--query-resume-hold-sigterm'),
    ('unordered-close-run--abort-error-sigterm', replay, '\n    "run--abort-error-sigterm",\n', '\n    "run--abort-error-sigterm-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: run--abort-error-sigterm'),
    ('unordered-close-run--second-sigterm-sigint', replay, '\n    "run--second-sigterm-sigint",\n', '\n    "run--second-sigterm-sigint-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: run--second-sigterm-sigint'),
    ('unordered-close-run--terminal-during-abort-final-sigterm', replay, '\n    "run--terminal-during-abort-final-sigterm",\n', '\n    "run--terminal-during-abort-final-sigterm-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: run--terminal-during-abort-final-sigterm'),
    ('unordered-close-run--query-resume-pending', replay, '\n    "run--query-resume-pending",\n', '\n    "run--query-resume-pending-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: run--query-resume-pending'),
    ('unordered-close-run--query-model-pending', replay, '\n    "run--query-model-pending",\n', '\n    "run--query-model-pending-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: run--query-model-pending'),
    ('unordered-close-resume--query-resume-hold-sigint', replay, '\n    "resume--query-resume-hold-sigint",\n', '\n    "resume--query-resume-hold-sigint-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--query-resume-hold-sigint'),
    ('unordered-close-resume--query-resume-release-sigint', replay, '\n    "resume--query-resume-release-sigint",\n', '\n    "resume--query-resume-release-sigint-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--query-resume-release-sigint'),
    ('unordered-close-resume--query-model-hold-sigint', replay, '\n    "resume--query-model-hold-sigint",\n', '\n    "resume--query-model-hold-sigint-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--query-model-hold-sigint'),
    ('unordered-close-resume--abort-error-sigint', replay, '\n    "resume--abort-error-sigint",\n', '\n    "resume--abort-error-sigint-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--abort-error-sigint'),
    ('unordered-close-resume--abort-hold-sigint', replay, '\n    "resume--abort-hold-sigint",\n', '\n    "resume--abort-hold-sigint-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--abort-hold-sigint'),
    ('unordered-close-resume--second-sigint', replay, '\n    "resume--second-sigint",\n', '\n    "resume--second-sigint-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--second-sigint'),
    ('unordered-close-resume--second-sigint-sigterm', replay, '\n    "resume--second-sigint-sigterm",\n', '\n    "resume--second-sigint-sigterm-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--second-sigint-sigterm'),
    ('unordered-close-resume--terminal-during-abort-final-sigint', replay, '\n    "resume--terminal-during-abort-final-sigint",\n', '\n    "resume--terminal-during-abort-final-sigint-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--terminal-during-abort-final-sigint'),
    ('unordered-close-resume--query-resume-hold-sigterm', replay, '\n    "resume--query-resume-hold-sigterm",\n', '\n    "resume--query-resume-hold-sigterm-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--query-resume-hold-sigterm'),
    ('unordered-close-resume--query-resume-release-sigterm', replay, '\n    "resume--query-resume-release-sigterm",\n', '\n    "resume--query-resume-release-sigterm-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--query-resume-release-sigterm'),
    ('unordered-close-resume--query-model-hold-sigterm', replay, '\n    "resume--query-model-hold-sigterm",\n', '\n    "resume--query-model-hold-sigterm-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--query-model-hold-sigterm'),
    ('unordered-close-resume--abort-error-sigterm', replay, '\n    "resume--abort-error-sigterm",\n', '\n    "resume--abort-error-sigterm-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--abort-error-sigterm'),
    ('unordered-close-resume--abort-hold-sigterm', replay, '\n    "resume--abort-hold-sigterm",\n', '\n    "resume--abort-hold-sigterm-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--abort-hold-sigterm'),
    ('unordered-close-resume--second-sigterm', replay, '\n    "resume--second-sigterm",\n', '\n    "resume--second-sigterm-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--second-sigterm'),
    ('unordered-close-resume--second-sigterm-sigint', replay, '\n    "resume--second-sigterm-sigint",\n', '\n    "resume--second-sigterm-sigint-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--second-sigterm-sigint'),
    ('unordered-close-resume--terminal-during-abort-final-sigterm', replay, '\n    "resume--terminal-during-abort-final-sigterm",\n', '\n    "resume--terminal-during-abort-final-sigterm-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--terminal-during-abort-final-sigterm'),
    ('unordered-close-resume--query-resume-pending', replay, '\n    "resume--query-resume-pending",\n', '\n    "resume--query-resume-pending-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--query-resume-pending'),
    ('unordered-close-resume--query-model-pending', replay, '\n    "resume--query-model-pending",\n', '\n    "resume--query-model-pending-unregistered",\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--query-model-pending'),
    ('unordered-close-overbroad', replay, '    if !UNORDERED_CLOSE_CASES.contains(&id) {', '    if false && !UNORDERED_CLOSE_CASES.contains(&id) {', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 stable close swap must fail:'),
    ('unordered-close-unregistered-id', replay, 'const UNORDERED_CLOSE_CASES: &[&str] = &[\n', 'const UNORDERED_CLOSE_CASES: &[&str] = &[\n    "not-a-registered-signal-cell",\n', 'signal_replay::comparison_tests::unordered_close_scope_equals_registration', '#1252 unordered close scope equals measured registration'),
    ('unordered-close-non-array', replay, '    let Some(actual) = actual.as_array() else {\n        return false;\n    };', '    let Some(actual) = actual.as_array() else {\n        return true;\n    };', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 non-array closes must fail:'),

]


class FrozenLedgerDrift(RuntimeError):
    """#1885 r7: a frozen ledger could not be proved against HEAD under the lock.

    Covers both a real drift and the proof's own infrastructure failure (git
    timeout / non-zero `git show`, unreadable file): to the caller both mean
    "this run has no verdict about the ledger", never "a control survived".
    A distinct type so the rc contract in ``main`` can route them without
    inspecting reason strings: `invalidate_completed` appends its reason to
    rows that were already recorded, so a prefix test only matched the
    restore phase (GLM + DeepSeek boundary r6, DeepSeek sibling r7).
    """


def _proven_ledger_bytes(path, label):
    """#1885 r8: prove one read-only ledger, or fail as a guard failure."""
    relative = path.relative_to(root).as_posix()
    try:
        data = path.read_bytes()
        head = _head_bytes(root, relative, "HEAD")
    except (OSError, SourceBaselineError) as error:
        raise FrozenLedgerDrift(
            f"#1885 {label} proof failed: {relative}") from error
    if head != data:
        raise FrozenLedgerDrift(
            f"#1885 read-only {label} must equal HEAD: {relative}")
    return data


def assert_close_order_ledger_matches_head():
    """#1885: this ledger is read but never mutated.

    It used to sit in ``guarded``, so ``mutating_sources`` proved it equalled
    HEAD on entry and wrote those bytes back on exit. The restore set now
    carries only files a control rewrites, and the workspace guard covers
    ``src/`` only, so nothing would notice an uncommitted edit moving the
    #1382 baseline behind our back. Keep the proof, drop the write-back: the
    frozen bytes are never put in the truncation window.

    Returns the bytes it verified so the caller parses exactly what was
    proved; ``_head_bytes`` is the repo's bounded git entry point (own
    session, group kill, ``GIT_TIMEOUT``) rather than a bare
    ``subprocess.run`` (GLM simplify/boundary + DeepSeek concurrency r2).
    """
    return _proven_ledger_bytes(close_order_ledger, "#1382 baseline")


def assert_frozen_ledgers_match_head():
    """#1885 r7: prove every read-only ledger, at both boundaries.

    ``d1_ledger`` leaves the restore set on a subset run that does not select
    the control rewriting it, so ``mutating_sources`` no longer re-reads it on
    exit -- while the replay children still parse it
    (``rejection_difference_ledger``). An edit landing mid-run would stand
    unnoticed, exactly the threat the close-order proof answers. Proving it
    unconditionally also covers the runs where it *is* selected: every case
    restores its own target before this runs.
    """
    data = assert_close_order_ledger_matches_head()
    _proven_ledger_bytes(d1_ledger, "D1 rejection ledger")
    return data


def registered_unordered_close_ids():
    """#1382: read the same close-order ledger block the Rust pin reads.

    No second hardcoded ID list lives here: the IDs come from the frozen
    D1-SDK-WS-CLOSE-ORDER-001 registration (read-only) exactly like
    ``unordered_close_scope_equals_registration`` in signal_replay.rs.
    """
    text = assert_close_order_ledger_matches_head().decode("utf-8")
    _, _, after_start = text.partition("<!-- close-order-ids:start -->")
    block, _, _ = after_start.partition("<!-- close-order-ids:end -->")
    fenced = block.strip()
    if not fenced.startswith("```json") or not fenced.endswith("```"):
        raise RuntimeError("#1382 close-order ledger block is not fenced JSON")
    ids = json.loads(fenced[len("```json"):-3])
    if (
        not isinstance(ids, list)
        or not ids
        or any(not isinstance(item, str) or not item for item in ids)
    ):
        raise RuntimeError("#1382 close-order ledger block is not a non-empty ID list")
    if len(ids) != len(set(ids)):
        raise RuntimeError("#1382 close-order ledger block has duplicate IDs")
    return ids


def check_catalog():
    names = [row[0] for row in mutations]
    # #1382: pin the per-ID unordered-close controls to the registered ledger
    # IDs in both directions. EXPECTED_TOTAL only pins the row count, so
    # raising it together with the ledger and UNORDERED_CLOSE_CASES while
    # skipping the per-ID mutation used to leave no failing check; this pin
    # must therefore stay a set comparison and never degenerate into count
    # arithmetic. It runs before the denominator check so a removed row is
    # reported as a missing ID, not as a stale total.
    stale_boundary = sorted(set(UNORDERED_CLOSE_BOUNDARY) - set(names))
    if stale_boundary:
        raise RuntimeError(
            "#1382 stale unordered-close boundary controls: "
            + ", ".join(stale_boundary)
        )
    registered = set(registered_unordered_close_ids())
    per_id = {
        name[len(UNORDERED_CLOSE_PREFIX):]
        for name in names
        if name.startswith(UNORDERED_CLOSE_PREFIX)
        and name not in UNORDERED_CLOSE_BOUNDARY
    }
    missing = sorted(registered - per_id)
    extra = sorted(per_id - registered)
    if missing or extra:
        raise RuntimeError(
            "#1382 unordered-close controls must equal the registered"
            f" close-order IDs; registered IDs without a mutation: {missing};"
            f" mutations without a registered ID: {extra}"
        )
    # #1382: matching names alone is not enough. A row copied from another ID
    # and renamed still satisfies the set comparison above while mutating that
    # other ID's anchor, so the registered ID would have no independent
    # control and the copy would run twice. For every per-ID row whose name
    # resolves to a registered ID, pin the anchor, the mutant value and the
    # bound assertion to that same ID. Boundary rows and renamed rows resolve
    # to an unregistered ID and are skipped here; they are governed by the set
    # comparison above.
    wrong_binding = []
    for name, path, old, new, test, assertion in mutations:
        if not name.startswith(UNORDERED_CLOSE_PREFIX):
            continue
        close_id = name[len(UNORDERED_CLOSE_PREFIX):]
        if close_id not in registered:
            continue
        if (
            path != replay
            or old != f'\n    "{close_id}",\n'
            or new != f'\n    "{close_id}-unregistered",\n'
            or test != _UNORDERED_CLOSE_ID_TEST
            or assertion != f'#1252 unordered close swap must pass: {close_id}'
        ):
            wrong_binding.append(close_id)
    if wrong_binding:
        raise RuntimeError(
            "#1382 unordered-close rows must bind their own ID;"
            " wrong content: " + ", ".join(sorted(wrong_binding))
        )
    # #1382: the whole-scope boundary rows get the same treatment — their names
    # are checked above, but a row whose content was swapped for a sibling's
    # would otherwise keep every name check green while the control is gone.
    if set(UNORDERED_CLOSE_BOUNDARY) != set(UNORDERED_CLOSE_BOUNDARY_EXPECTED):
        raise RuntimeError(
            "#1382 boundary name list and expected-content table disagree"
        )
    wrong_boundary = []
    for name, path, old, new, test, assertion in mutations:
        expected = UNORDERED_CLOSE_BOUNDARY_EXPECTED.get(name)
        if expected is None:
            continue
        if (path, old, new, test, assertion) != expected:
            wrong_boundary.append(name)
    if wrong_boundary:
        raise RuntimeError(
            "#1382 whole-scope boundary rows must keep their own content;"
            " wrong content: " + ", ".join(sorted(wrong_boundary))
        )
    if len(mutations) != EXPECTED_TOTAL:
        raise RuntimeError("#1145 fixed product mutation denominator")
    if len(set(names)) != EXPECTED_TOTAL:
        raise RuntimeError("#1145 unique product mutation identities")
    # #1885: the restore set must stay exactly the mutation targets. Restoring
    # a file no control rewrites re-enters the golden-truncation window without
    # buying any leftover detection, so pin the fixture-side membership: the
    # only file under the fixture trees a control may rewrite is the SIGINT
    # wrapper. Re-broadening `guarded_paths` to a fixture glob, or aiming a
    # mutation at a golden, fails here.
    strays = sorted(
        str(path.relative_to(root))
        for path in guarded_paths()
        # `in path.parents` rather than `path.parent in {...}` so a golden one
        # directory deeper (`fixture/sub/oracle.json`) is caught too.
        if any(tree in path.parents for tree in (fixture, abort_state))
        and path not in MUTABLE_FIXTURE_FILES
    )
    if strays:
        raise RuntimeError(
            "#1885 restore set must not carry fixture goldens: " + ", ".join(strays)
        )
    # #1885 r5: the stray filter above only covers the fixture trees. Aiming a
    # mutation at the read-only close-order ledger would slip it into
    # `guarded_paths()` and reopen the write-back window on a frozen ledger --
    # and the entry/exit HEAD proofs would still pass, because each case
    # restores its own target before they run. Pin it directly.
    if close_order_ledger in guarded_paths():
        raise RuntimeError(
            "#1885 the read-only close-order ledger must never be a mutation"
            " target (it is proved against HEAD, not written back)"
        )
    stale = []
    for name, path, old, new, test, assertion in mutations:
        if not old or old == new:
            stale.append(f"{name}: empty or unchanged mutation")
        count = path.read_text(encoding="utf-8").count(old)
        if count != 1:
            stale.append(f"{name}: expected one anchor, found {count}")
        if not test.startswith(("signal_cases::", "product::sdk::turn::tests::", "product::sdk::transport::tests::", "product::skills::tests::", "product::skills_migrate::tests::", "signal_replay::comparison_tests::")) or not assertion.startswith(("#1145 ", "#1252 ", "#273 ")):
            stale.append(f"{name}: invalid named-test/assertion binding")
    if stale:
        raise RuntimeError("#1145 stale product controls: " + "; ".join(stale))


def raw_text(value):
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return value or ""


def _require_host_lease(common, lock=None):
    """#2513 fix-r6 (MM concurrency P1): the host pre-hold, machine-checked.

    fix-r4's redirect also removed the old shared-lease 60 s TimeoutError as
    a side effect: nothing failed when the caller never pre-held
    /tmp/pdrs-parity.lock, so a run without the host's serialization could
    silently open a mutation window unguarded — the fail-open the review
    flagged. This gate runs before the window opens (from `redirected`,
    before the redirected lease is taken) and is fail-closed: the shared
    lease's owner.json must exist and name a live pid that is not this
    process itself, the README's "取不到租约即退出稍后重跑" contract.
    Otherwise one reason line goes to stderr and the run exits 2 — this
    file's existing refusal shape (a refusal, never a verdict). The owner
    and liveness reads come from the verified `common` module, so the gate
    cannot drift from the lease protocol it polices.
    """
    lock = common.LOCK if lock is None else lock
    previous = common.owner(lock)
    if previous is None:
        print(
            f"#2513 host pre-hold required: {lock / 'owner.json'} is missing "
            "or invalid; exit and rerun under the host hold",
            file=sys.stderr,
        )
        raise SystemExit(2)
    pid = previous["pid"]
    if not common.alive(pid):
        print(
            f"#2513 host pre-hold required: owner pid {pid} is not alive; "
            "exit and rerun under the host hold",
            file=sys.stderr,
        )
        raise SystemExit(2)
    if pid == os.getpid():
        print(
            "#2513 host pre-hold required: owner pid is this fixture itself "
            "— the pre-hold must be the host's, not the fixture's own",
            file=sys.stderr,
        )
        raise SystemExit(2)


def _self_check():
    """#2513 fix-r6: the host pre-hold gate's own four rows.

    (a) a missing owner.json, (b) a dead owner pid, (c) our own pid and
    (d) a live external pid — the first three must refuse with exit 2, the
    fourth must proceed. The scenarios use throwaway lock directories,
    never the real /tmp/pdrs-parity.lock.
    """
    spec = importlib.util.spec_from_file_location(
        "negative_selfcheck_parity_sdk_signals",
        root / "scripts/gateway-parity/common.py")
    common = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(common)

    def publish_owner(lock, pid):
        lock.mkdir(parents=True, exist_ok=True)
        (lock / "owner.json").write_text(json.dumps({
            "pid": pid, "timestamp": 1000000.0,
            "token": "self-check", "cleanupPending": True}))

    def refused(lock, label):
        try:
            _require_host_lease(common, lock)
        except SystemExit as error:
            if error.code != 2:
                raise RuntimeError(
                    f"#2513 self-check {label}: exit {error.code!r}, expected 2")
            return
        raise RuntimeError(
            f"#2513 self-check {label}: proceeded where the refusal was required")

    with tempfile.TemporaryDirectory() as held:
        base = Path(held)
        # (a) the lock directory exists but no owner was ever published.
        empty = base / "a-pdrs-parity.lock"
        empty.mkdir()
        refused(empty, "(a) missing owner.json")
        # (b) the owner is a dead process (a reaped child's pid).
        dead = subprocess.Popen(["/bin/sleep", "0"])
        dead.wait()
        dead_lock = base / "b-pdrs-parity.lock"
        publish_owner(dead_lock, dead.pid)
        refused(dead_lock, "(b) dead owner pid")
        # (c) the "owner" is this fixture process itself.
        own_lock = base / "c-pdrs-parity.lock"
        publish_owner(own_lock, os.getpid())
        refused(own_lock, "(c) own pid")
        # (d) the owner is a live external process: the gate proceeds.
        live = subprocess.Popen(["/bin/sleep", "30"])
        try:
            live_lock = base / "d-pdrs-parity.lock"
            publish_owner(live_lock, live.pid)
            _require_host_lease(common, live_lock)
        finally:
            live.terminate()
            live.wait()
    print("#2513 host pre-hold gate self-check: PASS (4 rows)")
    return 0


def _redirect_window_parity_lock(out):
    """#2513 fix-r4 (review 条2): redirect this family's window lease.

    This family's contract is a host-pre-held /tmp/pdrs-parity.lock: the host
    serializes every 28999 lane user and runs this fixture under that hold (see
    the sdk-signals README). Since fix-r3 the mutation window itself would take
    that same shared lease — against the host holding it for us — and fail after
    60 s with zero evidence (实测 pre-fix: `TimeoutError: parity lock wait
    exceeded 60s`, every row INVALID). The Python twin of the Rust families'
    PDRS_SDK_TEST_PARITY_LOCK redirect: the window's `common.parity_lock` call
    point is redirected to `out/parity-redirect.lock`, an unshared lease in
    this run's evidence directory, same owner/guard/stale-takeover protocol.
    Mutual exclusion against live rounds is not weakened — the host pre-hold
    provides it for the whole run, which is exactly the README contract.

    #2513 fix-r6: that contract is now machine-checked, not assumed —
    `_require_host_lease` runs before the redirected lease is taken and
    refuses (exit 2) unless the real shared lease names a live external
    pid, restoring the fail-closed side fix-r4 lost with the old 60 s
    TimeoutError.

    Returns the restore callable; the caller must invoke it in a `finally`.
    The pristine ruler module is still executed from verified snapshot bytes
    (never mutated disk code) and `common.py` still joins the guarded set, so
    the snapshot entry requirement is the original's, not a copy drifting
    behind negative_process.
    """
    original = negative_process._parity_mutation_lock

    @contextmanager
    def redirected(root, snapshot, timeout):
        path = root / "scripts/gateway-parity/common.py"
        if path not in snapshot:
            raise RuntimeError(
                "#166 parity lock requires common.py in guarded sources")
        # Stage the verified snapshot bytes, then load them through the same
        # importlib shape the other checked fixtures use — a bare exec would
        # re-open the anchor_selfcheck dynamic-write hole this family closed.
        staged = out / "pristine-common.py"
        staged.write_bytes(snapshot[path])
        spec = importlib.util.spec_from_file_location(
            "negative_pristine_parity_sdk_signals", staged)
        common = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(common)
        common.guard = lambda lock: TreeLock(
            lock.with_name(lock.name + ".guard"), timeout=timeout)
        # #2513 fix-r6: the window opens only under the host's pre-held
        # shared lease — checked here, before anything is mutated.
        _require_host_lease(common)
        restored = [False]
        with common.parity_lock(
                timeout=timeout, lock=out / "parity-redirect.lock"):
            try:
                yield restored
            finally:
                if not restored[0]:
                    # Same fail-closed marker as the unredirected original: an
                    # unverified restoration must not leave a reusable lease.
                    common.LIVE_GROUPS.add("source-restore-unverified")

    negative_process._parity_mutation_lock = redirected

    def restore():
        negative_process._parity_mutation_lock = original

    return restore


def main():
    # #1885 r8: the pre-lock catalog check proves the close-order ledger too
    # (`registered_unordered_close_ids`), so a drift here is the same guard
    # failure as the in-lock proofs. Uncaught it exited as an interpreter
    # traceback (rc 1, "a control survived"); route it to 2 like the rest
    # (GLM sibling r7, DeepSeek sibling r7).
    try:
        check_catalog()
    except FrozenLedgerDrift as error:
        print(
            "source guard or interruption:"
            f" {type(error).__name__}: {error}",
            file=sys.stderr,
        )
        return 2
    args = sys.argv[1:]
    # #1382: --check is deliberately a compatibility alias for --check-anchors.
    # Both spellings are handled by this one branch, so their behavior cannot
    # drift apart; any new flag must either join this same branch or carry its
    # own documented semantics.
    if args in (["--check"], ["--check-anchors"]):
        print(f"#1145 {EXPECTED_TOTAL} product anchors: PASS")
        return 0
    # #2513 fix-r6: --self-check is its own documented branch — the host
    # pre-hold gate's four rows (missing owner.json / dead pid / own pid /
    # live external pid). It opens no mutation window and needs no lease:
    # the scenarios run against throwaway lock directories.
    if args == ["--self-check"]:
        return _self_check()
    if not args:
        raise SystemExit(
            "usage: negative_controls.py --check-anchors (alias: --check) | --self-check | "
            "NEW_OUTPUT_DIR [mutation ...]"
        )

    selected_names = args[1:]
    if (
        len(selected_names) != len(set(selected_names))
        or any(name not in {row[0] for row in mutations} for name in selected_names)
    ):
        raise SystemExit("#1145 unknown or duplicate mutation selection")
    selected = [
        row for row in mutations
        if not selected_names or row[0] in selected_names
    ]
    if not selected:
        raise SystemExit("#1145 selected product denominator must be nonzero")

    env = os.environ.copy()
    if not env.get("CARGO_TARGET_DIR"):
        raise SystemExit("#1145 CARGO_TARGET_DIR must be supplied by the caller")
    if not env.get("PDRS_BASELINE"):
        raise SystemExit("#1145 PDRS_BASELINE must identify the real TS checkout")

    wrapper = Path(
        env.get(
            "PDRS_SIGINT_WRAPPER",
            str(root / "src/pilotdeck-cli/tests/fixtures/sdk-signals/sigint_default.py"),
        )
    ).resolve()
    if not wrapper.is_file():
        raise SystemExit(f"#1145 missing SIGINT default wrapper: {wrapper}")

    # No receipt state may leak from a caller's unrelated negative invocation.
    env.pop("PDRS_NEGATIVE_RECEIPTS", None)
    env.pop("PDRS_NEGATIVE_PARENT_TEST", None)
    env.pop("PDRS_SIGNAL_CHILD", None)
    env.pop("PDRS_SIGNAL_ARGV", None)
    env.pop("PDRS_SIGNAL_OBSERVER", None)

    out = new_output_directory(args[0])
    exclusive_write(
        out / "run.json",
        json.dumps(
            {
                "issue": 1145,
                "registeredTotal": EXPECTED_TOTAL,
                "selectedTotal": len(selected),
                "selected": [row[0] for row in selected],
                "cargoTargetDir": env["CARGO_TARGET_DIR"],
                "baseline": env["PDRS_BASELINE"],
                "sigintWrapper": str(wrapper),
            },
            indent=2,
        ) + "\n",
    )

    command = [
        sys.executable, "-I", "-B", str(wrapper),
        "cargo", "test", "--offline",
        "--manifest-path", "src/Cargo.toml",
        "-p", "pilotdeck-cli",
        "--test", "sdk_signal_exit",
    ]

    def run(label, test, compile_only=False):
        run_env = dict(env)
        # The replay owns its case directory exclusively. Each invocation gets
        # a different root, including positive and restored runs of one test.
        run_env["PDRS_SIGNAL_RESULTS"] = str(out / f"{label}.observations")
        # #1885: this process holds the exclusive tree lock for the whole run,
        # so a child asking for the shared read lease would block on its own
        # parent until BUILD_TIMEOUT. The runner is the writer for the bytes its
        # children read, which is exactly what the bypass asserts.
        run_env["PDRS_NEGATIVE_LEASE_HELD_ROOT"] = str(root)
        try:
            slot = wait_for_slot(
                root / "scripts/cargo-slots.sh",
                timeout=900,
                interval=15,
            )
            exclusive_write(
                out / f"{label}.slots.log", slot.stdout + slot.stderr
            )
            suffix = (
                ["--no-run"] if compile_only else
                [test, "--", "--exact", "--include-ignored", "--test-threads=1"]
            )
            result = run_owned(
                command + suffix,
                cwd=root,
                env=run_env,
                timeout=BUILD_TIMEOUT if compile_only else TEST_TIMEOUT,
            )
            exclusive_write(out / f"{label}.stdout", result.stdout)
            exclusive_write(out / f"{label}.stderr", result.stderr)
            return result
        except Exception as error:
            raw = ""
            if isinstance(error, subprocess.TimeoutExpired):
                raw = raw_text(error.stdout) + raw_text(error.stderr)
            exclusive_write(
                out / f"{label}.error.log",
                raw + f"\n{type(error).__name__}: {error}\n",
            )
            raise

    # #1885 r5: the restore set follows the *selection*, not the whole table.
    # `mutating_sources` rewrites every guarded path on exit, so carrying the
    # targets of controls that are not running would put untouched files (the
    # `d1_ledger` frozen ledger, the `src/` targets) back into the truncation
    # window on a subset run that never mutates them (DeepSeek sibling r5 P1).
    # No leftover detection is lost: `mutating_sources` proves the whole `src/`
    # tree against HEAD on entry, and the one `specs/` target is proved below.
    guarded = sorted({row[1] for row in selected}, key=str)
    results = []
    phase = "enter"
    interrupted = False
    guard_failure = False

    def record(name, test, assertion, status, reason, compiled):
        results.append(
            {
                "mutation": name,
                "test": test,
                "assertion": assertion,
                "status": status,
                "reason": reason,
                "compiled": compiled,
            }
        )
        print(f"{name}: {status}: {reason}", flush=True)

    # #2513 fix-r4 (review 条2): the window's own lease must not be the one
    # the host is pre-holding for this run — see
    # `_redirect_window_parity_lock` above for the contract argument.
    restore_window_parity_lock = _redirect_window_parity_lock(out)
    try:
        with mutating_sources(root, guarded) as originals:
            phase = "body"
            # #1885 r3: prove under the tree lock. `check_catalog()` already
            # proved the frozen #1382 ledger before this block, but acquiring
            # the source lock can wait (default 900 s while another runner
            # holds it), and an edit landing in that gap would be the very
            # bytes the replay then parses. Same shape as
            # `sdk-signals-close-order-pin` (GLM sibling + DeepSeek sibling r4).
            #
            # #1885 r7: both read-only ledgers, not just the close-order one.
            # `d1_ledger` leaves the restore set on a subset run that does not
            # select the control rewriting it, so `mutating_sources` no longer
            # re-reads it on exit; the replay children still parse it, so it
            # needs the same entry/exit proof (GLM boundary + DeepSeek
            # sibling/boundary r6).
            assert_frozen_ledgers_match_head()
            for name, path, old, new, test, assertion in selected:
                case = Case(name, path, old, new, test, assertion)
                status, reason, compiled = "INVALID", "not run", False
                stage = "anchor"
                try:
                    original = originals[path].decode("utf-8")
                    if original.count(old) != 1:
                        raise RuntimeError(
                            "#1145 bound mutation anchor must occur exactly once"
                        )
                    stage = "positive"
                    positive = run(name + "-positive", test)
                    if not passed(positive, test):
                        reason = "positive control did not pass its named test"
                    else:
                        changed = original.replace(old, new, 1)
                        exclusive_write(
                            out / f"{name}.patch",
                            "".join(
                                difflib.unified_diff(
                                    original.splitlines(True),
                                    changed.splitlines(True),
                                    fromfile=str(path.relative_to(root)),
                                    tofile=str(path.relative_to(root)),
                                )
                            ),
                        )
                        path.write_text(changed, encoding="utf-8")
                        stage = "compile"
                        build = run(name + "-compile", test, compile_only=True)
                        compiled = (
                            build.returncode == 0
                            and "could not compile" not in build.stdout + build.stderr
                        )
                        if not compiled:
                            reason = "mutant failed no-run compilation; not a kill"
                        else:
                            stage = "negative"
                            status, reason = classify(
                                run(name + "-negative", test), case
                            )
                except Exception as error:
                    status = "INVALID"
                    reason = f"{stage}: {type(error).__name__}: {error}"
                finally:
                    # Always restore this case before any successor. The outer
                    # guard independently restores and checks HEAD on exit.
                    phase = "restore"
                    path.write_bytes(originals[path])
                    if path.read_bytes() != originals[path]:
                        raise RuntimeError(
                            f"#1145 restoration mismatch: {path.relative_to(root)}"
                        )
                    phase = "body"

                try:
                    restored = run(name + "-restored", test)
                    if not passed(restored, test):
                        status = "INVALID"
                        reason += "; restored named positive did not pass"
                except Exception as error:
                    status = "INVALID"
                    reason += (
                        f"; restored: {type(error).__name__}: {error}"
                    )
                record(name, test, assertion, status, reason, compiled)
            # #1885 r3: re-prove on the way out, still under the tree lock.
            # Before #1885 this ledger sat in `guarded`, so `mutating_sources`
            # re-read it on exit; the frozen bytes are no longer written back,
            # so the exit-side verification has to be explicit or an edit made
            # during the run would stand unnoticed.
            phase = "exit"
            assert_frozen_ledgers_match_head()

    except BaseException as error:
        interrupted = not isinstance(error, Exception)
        reason = f"source guard or interruption: {type(error).__name__}: {error}"
        # #1885 r7: route the failure to 2 ("no verdict") by *where* it
        # happened and *what* it was, not by the shape of the reason strings.
        # `invalidate_completed` appends its reason to rows that were already
        # recorded, so the old `startswith` test only ever matched the restore
        # phase (it appends to rows that do not exist yet); an exit-phase
        # proof, or the entry-phase proof added in r3/r5, left every reason
        # prefixed by the original verdict and fell through to 1 -- reporting
        # a guard fault as "a control survived" (GLM + DeepSeek boundary r6).
        #
        # #1885 r8: `phase != "body"` rather than the restore/exit pair. The
        # `enter` phase is `mutating_sources.__enter__` -- tree-lock timeout,
        # `_assert_workspace_sources` on a leftover mutant -- and every way it
        # can escape is a guard, never a verdict about a control (DeepSeek
        # sibling r7). Entry-phase ledger proofs inside the body raise
        # `FrozenLedgerDrift`, which now also covers the proof's own git/file
        # failures (GLM boundary + concurrency r7).
        guard_failure = (
            interrupted
            or phase != "body"
            or isinstance(error, FrozenLedgerDrift)
        )
        # Completed kills become invalid if the guarded run cannot verify its
        # final state. Every unrun selected case is also explicitly accounted.
        invalidate_completed(results, reason)
        for name, _path, _old, _new, test, assertion in selected[len(results):]:
            record(name, test, assertion, "INVALID", reason, False)
    finally:
        restore_window_parity_lock()

    if len(results) != len(selected):
        raise RuntimeError("#1145 fixed selected result denominator")
    if {row["mutation"] for row in results} != {row[0] for row in selected}:
        raise RuntimeError("#1145 exact selected result identities")

    # The summary does not exist while product sources are mutated.
    publish_summary(out, results)
    counts = {
        state: sum(row["status"] == state for row in results)
        for state in ("KILLED", "SURVIVED", "INVALID")
    }
    print(
        json.dumps(
            {
                "registeredTotal": EXPECTED_TOTAL,
                "selectedTotal": len(selected),
                "total": len(results),
                "counts": counts,
            }
        ),
        flush=True,
    )
    if guard_failure:
        return 2
    return int(any(row["status"] != "KILLED" for row in results))


if __name__ == "__main__":
    sys.exit(main())

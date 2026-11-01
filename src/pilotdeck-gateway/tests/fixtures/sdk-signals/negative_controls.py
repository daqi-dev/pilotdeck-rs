#!/usr/bin/env python3
"""#1145 HEAD-only controls: --check-anchors | NEW_OUTPUT_DIR [mutation ...].

--check remains a compatibility alias for --check-anchors.

The catalog has exactly 75 controls. Each selected control runs its named
positive, compiles the mutant, runs the named negative, restores, and reruns the
positive. All verdicts are published only after the source guard has verified
restoration. New/dirty source and test files must first be committed by the
coordinator; this script never creates a second checkout or bypasses HEAD.
"""
import difflib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from negative_process import (
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
skills_tests = root / "src/pilotdeck-cli/src/skills.rs"
migrate_tests = root / "src/pilotdeck-cli/src/skills_migrate.rs"
wrapper_source = root / "src/pilotdeck-cli/tests/fixtures/sdk-signals/sigint_default.py"

EXPECTED_TOTAL = 75
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


def check_catalog():
    if len(mutations) != EXPECTED_TOTAL:
        raise RuntimeError("#1145 fixed product mutation denominator")
    names = [row[0] for row in mutations]
    if len(set(names)) != EXPECTED_TOTAL:
        raise RuntimeError("#1145 unique product mutation identities")
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


def main():
    check_catalog()
    args = sys.argv[1:]
    if args in (["--check"], ["--check-anchors"]):
        print(f"#1145 {EXPECTED_TOTAL} product anchors: PASS")
        return 0
    if not args:
        raise SystemExit(
            "usage: negative_controls.py --check-anchors (alias: --check) | NEW_OUTPUT_DIR [mutation ...]"
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

    fixture = root / "src/pilotdeck-cli/tests/fixtures/sdk-signals"
    guarded = [
        *sorted((root / "src/pilotdeck-cli/src").rglob("*.rs")),
        root / "src/pilotdeck-cli/Cargo.toml",
        root / "src/Cargo.toml",
        root / "src/Cargo.lock",
        root / "src/pilotdeck-cli/tests/sdk_signal_exit.rs",
        root / "src/pilotdeck-cli/tests/common/signal_replay.rs",
        root / "specs/gateway-parity/diffs/D1-SDK-WS-CLOSE-ORDER-001.md",
        root / "src/pilotdeck-cli/tests/common/negative_receipt.rs",
        root / "specs/gateway-parity/diffs/D1-SDK-SIGNAL-UNHANDLED-REJECTION-001.md",
        *sorted(path for path in fixture.iterdir() if path.is_file()),
        *sorted((fixture.parent / "sdk-signal-abort-state").glob("*")),
        Path(__file__).resolve(),
        Path(__file__).resolve().parents[1] / "negative_process.py",
        Path(__file__).resolve().parents[1] / "negative_suite.py",
    ]
    results = []
    phase = "enter"
    interrupted = False

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

    try:
        with mutating_sources(root, guarded) as originals:
            phase = "body"
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
            phase = "exit"

    except BaseException as error:
        interrupted = not isinstance(error, Exception)
        reason = f"source guard or interruption: {type(error).__name__}: {error}"
        # Completed kills become invalid if the guarded run cannot verify its
        # final state. Every unrun selected case is also explicitly accounted.
        invalidate_completed(results, reason)
        for name, _path, _old, _new, test, assertion in selected[len(results):]:
            record(name, test, assertion, "INVALID", reason, False)

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
    if interrupted or phase in ("restore", "exit") and any(
        row["status"] == "INVALID"
        and row["reason"].startswith("source guard or interruption:")
        for row in results
    ):
        return 2
    return int(any(row["status"] != "KILLED" for row in results))


if __name__ == "__main__":
    sys.exit(main())

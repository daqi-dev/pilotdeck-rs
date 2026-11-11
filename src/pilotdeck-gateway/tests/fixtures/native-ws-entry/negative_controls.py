#!/usr/bin/env python3
"""#749 compile-time switches with per-row compilation and nonce receipts.

No source replacement: use the shared tree lock + working-tree SHA contract.
Caller supplies CARGO_TARGET_DIR; all selected rows run before the summary exit.
"""
import argparse
import json
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[5]
FIXTURE = Path(__file__).resolve().parent
sys.path.insert(0, str(FIXTURE.parent))
from negative_process import tree_lock, run_owned, wait_for_slot
from negative_suite import ReceiptRun, bind_module_file, declares_test, judge, passed
from negative_source import assertion_marker_present
from _shared.mutant_switch import (preflight, clean_environment, record_sources,
                                   verify_unchanged, enforce_denominator)
SOURCE = ROOT / 'src/pilotdeck-gateway/src/native_ws.rs'
TESTS = ROOT / 'src/pilotdeck-gateway/src/native_ws/tests.rs'
PREFIX = 'native_ws::tests::'
# #1942: the failed-initialization teardown is witnessed by the turn-gateway
# suite, not the native-ws one; rows bound there name this module and are
# audited against its own file.
TURN_TESTS = 'runtime::turn::gateway::tests::'
TURN_TESTS_SOURCE = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/gateway_tests.rs'
TURN_GATEWAY = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/gateway.rs'
switches = [
    ('native-ws-shorten-eof-deadline', SOURCE, PREFIX + 'close_budget_covers_accepted_bytes', '#749 close budget covers accepted bytes'),
    ('native-ws-short-close-budget', SOURCE, PREFIX + 'accepted_output_flushes_after_close_or_eof', '#749 accepted output survives delayed flush'),
    ('native-ws-ignore-flush-bytes', SOURCE, PREFIX + 'close_budget_covers_accepted_bytes', '#749 close budget covers accepted bytes'),
    ('native-ws-cancel-leaks-permit', SOURCE, PREFIX + 'stalled_output_is_bounded_and_cancellable', '#749 blocked flush releases resources'),
    ('native-ws-close-oracle-missing', TESTS, PREFIX + 'close_flush_fixture_records_ts_lifecycle', '#749 TS close flush matrix is complete'),
    ('native-ws-large-oracle-empty', TESTS, PREFIX + 'large_output_matches_ts', '#749 large output fixture has exactly two required sizes'),
    ('native-ws-large-oracle-missing', TESTS, PREFIX + 'large_output_matches_ts', '#749 large output fixture has exactly two required sizes'),
    ('native-ws-large-oracle-size', TESTS, PREFIX + 'large_output_matches_ts', '#749 large output fixture has exactly two required sizes'),
    ('native-ws-large-oracle-content', TESTS, PREFIX + 'large_output_matches_ts', '#749 legal large output matches TS'),
    ('native-ws-full-oracle-before-count', TESTS, PREFIX + 'full_queue_eof_flushes_successfully', '#749 full queue EOF fixture lifecycle'),
    ('native-ws-full-oracle-before-finished', TESTS, PREFIX + 'full_queue_eof_flushes_successfully', '#749 full queue EOF fixture lifecycle'),
    ('native-ws-full-oracle-completed', TESTS, PREFIX + 'full_queue_eof_flushes_successfully', '#749 full queue EOF fixture lifecycle'),
    ('native-ws-full-oracle-finished', TESTS, PREFIX + 'full_queue_eof_flushes_successfully', '#749 full queue EOF fixture lifecycle'),
    ('native-ws-full-oracle-destroyed', TESTS, PREFIX + 'full_queue_eof_flushes_successfully', '#749 full queue EOF fixture lifecycle'),
    ('native-ws-drop-text', SOURCE, PREFIX + 'handshake_and_auth', '#749 text delivered'),
    ('native-ws-after-close', SOURCE, PREFIX + 'close_discards_following_frames', '#749 close discards following requests'),
    ('native-ws-drop-closed-read', SOURCE, PREFIX + 'close_tail_resumes_on_next_data', '#749 next data resumes buffered tail'),
    ('native-ws-drop-application-tail', SOURCE, PREFIX + 'application_close_preserves_same_batch_callbacks', '#749 application close same batch matches TS'),
    ('native-ws-skip-disconnect', SOURCE, PREFIX + 'pending_request_and_disconnect', '#749 disconnect drains pending submit'),
    ('native-ws-writer-blocks-read', SOURCE, PREFIX + 'read_continues_under_writer_backpressure', '#749 read under write backlog'),
    ('native-ws-cancel-leak', SOURCE, PREFIX + 'cancelled_server_stops_retained_host', '#749 cancelled server signals retained host'),
    ('native-ws-unbounded-cancel', SOURCE, PREFIX + 'cancelled_last_host_keeps_deadline', '#749 last host retains explicit deadline'),
    ('native-ws-bind-policy', SOURCE, PREFIX + 'native_configuration', '#749 native configuration refuses implicit authority'),
    ('native-ws-token-policy', SOURCE, PREFIX + 'native_configuration', '#749 native configuration refuses implicit authority'),
    ('native-ws-invalid-upgrade', SOURCE, PREFIX + 'invalid_upgrade', '#749 invalid upgrade rejected'),
    # #1292: the token route's loopback derivation is the TS literal
    # (host === "127.0.0.1" || host === "localhost"; probe b4-L1.out:
    # 127.0.0.2 + allowRemoteHost is a 404, b4-L2/b4-L3 serve the token);
    # the mutant relaxes the derivation back to the wide is_loopback()
    # class, so 127.0.0.2 and ::1 count as loopback.
    ('native-ws-loopback-literal', SOURCE, PREFIX + 'loopback_literal_matches_ts', '#1292 the loopback derivation is the TS 127.0.0.1/localhost literal, not the is_loopback() class'),
    # #1292: Node's TrackHeader span budget applies to upgrade heads like the
    # non-upgrade lane (probe b4-U1.out: the over-budget /ws upgrade is the
    # same 431 + close as b4-U2.out's /health); the mutant skips the check on
    # the diversion, so the over-budget upgrade head is upgraded to 101
    # instead of the 431.
    ('native-http-upgrade-budget-off', SOURCE, PREFIX + 'upgrade_head_respects_span_budget', '#1292 the over-budget upgrade head is the same 431 as the non-upgrade lane'),
    # #1351: the boundary golden replay — the mutant corrupts a recorded 431
    # cell in memory (the native-ws-large-oracle-content style), so the
    # per-cell byte comparison must fail on its own assertion.
    ('native-ws-upgrade-oracle-431', TESTS, PREFIX + 'upgrade_budget_boundary_matches_ts', "#1351 every boundary cell's response matches the recorded TS bytes"),
    # #1351 exemption (2026-11-06 review, #1757): the two standalone cell tests
    # upgrade_budget_16385_cell_431s_like_ts / upgrade_budget_20kib_cell_431s_like_ts
    # have no row here. This table binds one switch name to exactly one test, and a
    # per-cell switch would need its own site in the native_ws upgrade guard; the two
    # cells are covered here by the boundary replay above, and their independent
    # anchors are witnessed by hand-run PDRS_MUTANT=native-http-upgrade-budget-off
    # --exact runs (_tmp/20261106-issue-1351/negctl-newcells/, 2 KILLED / 0 SURVIVED
    # / 0 INVALID) which do not carry nonce receipts. Registering them is #1757.
    ('native-ws-retain-reader', SOURCE, PREFIX + 'socket_drops_before_business_drains', '#749 closed socket released before business drain'),
    ('native-ws-skip-host-cleanup', TURN_GATEWAY, PREFIX + 'host_cleanup_precedes_runtime_exit', '#749 owned cleanup called'),
    ('native-ws-error-skips-cleanup', TURN_GATEWAY, PREFIX + 'host_cleanup_runs_after_session_failure', '#749 cleanup runs after session failure'),
    # #1942: the failed-initialization teardown (#1746) shipped with only its
    # positive control, so `dispose_initialization` had no switch watching it.
    # The first two rows below are compiled there and bound to that one test's
    # two markers: the first skips the whole teardown (the coordinator and its
    # interactions stay live), the second skips only the embedding cleanup
    # poll while the synchronous disposals still run. The third (#1942 panic
    # chain) lets a panicking embedding cleanup escape the host thread
    # instead of being caught, so the disposals behind it never run and the
    # caller sees the panic, not the builder error. The fourth is the drain
    # twin: the same escape on the shutdown-drain poll of the same future,
    # so the drain never settles and the disposals behind it never run.
    ('native-ws-skip-initialization-cleanup', TURN_GATEWAY,
     TURN_TESTS + 'a_builder_error_disposes_the_coordinator_and_the_embedding_cleanup',
     '#1746 a builder error must still dispose the coordinator'),
    ('native-ws-skip-initialization-embedding-cleanup', TURN_GATEWAY,
     TURN_TESTS + 'a_builder_error_disposes_the_coordinator_and_the_embedding_cleanup',
     '#1746 a builder error must still poll the embedding cleanup'),
    ('native-ws-initialization-cleanup-panic-uncaught', TURN_GATEWAY,
     TURN_TESTS + 'a_panicking_cleanup_still_disposes_the_coordinator',
     '#1942 a panicking cleanup must still dispose the coordinator'),
    ('native-ws-drain-cleanup-panic-uncaught', TURN_GATEWAY,
     TURN_TESTS + 'a_panicking_drain_cleanup_still_disposes_the_coordinator',
     '#1942 a panicking drain cleanup must still dispose the coordinator'),
    ('native-ws-skip-drain', SOURCE, PREFIX + 'stop_accepting_drains_existing', '#749 drain waits for existing socket'),
    ('native-ws-input-boundary', ROOT / 'src/pilotdeck-gateway/src/frame.rs', PREFIX + 'input_limit_boundary_preserves_proxy', '#749 exact input boundary remains accepted'),
    ('native-ws-full-close-signal', SOURCE, PREFIX + 'output_slot_budget_and_full_close', '#749 full output queue preserves close frame'),
    ('native-ws-leak-output-permit', SOURCE, PREFIX + 'output_permit_released_after_write', '#749 written frame releases byte permit'),
    ('native-ws-retain-completed-requests', SOURCE, PREFIX + 'completed_writes_and_requests_release_capacity', '#749 completed writes and requests release capacity'),
    ('native-ws-buffer-limit', SOURCE, PREFIX + 'buffered_input_never_exceeds_frame_budget', '#749 buffered input capped before read allocation'),
    ('native-ws-reset-close-deadline', SOURCE, PREFIX + 'close_deadline_is_not_extended_by_eof', '#749 EOF preserves first close deadline'),
    ('native-ws-input-limit', SOURCE, PREFIX + 'oversized_header_closes_before_payload', '#749 input limit rejects header before allocation'),
    ('native-ws-input-close-code', SOURCE, PREFIX + 'oversized_header_closes_before_payload', '#749 input limit close code'),
    ('native-ws-output-bytes', SOURCE, PREFIX + 'output_byte_budget_includes_inflight_and_recovers', '#749 byte limit includes blocked writer'),
    ('native-ws-output-slots', SOURCE, PREFIX + 'output_slot_budget_and_full_close', '#749 full output queue preserves close frame'),
    ('native-ws-writer-deadline', SOURCE, PREFIX + 'stalled_output_is_bounded_and_cancellable', '#749 blocked flush releases resources'),
    ('native-ws-reader-deadline', SOURCE, PREFIX + 'close_deadline_reclaims_peer_without_fin', '#749 reader close deadline reclaims peer without FIN'),
    ('native-ws-pending-limit', SOURCE, PREFIX + 'pending_requests_are_bounded_before_enqueue', '#1123 pending cap rejects before host enqueue'),
    ('native-ws-drain-no-deadline', SOURCE, PREFIX + 'bounded_drain', '#749 bounded host exit'),
    ('native-ws-handshake-no-deadline', SOURCE, PREFIX + 'shutdown_cancels_handshake', '#749 half handshake bounded'),
    ('native-ws-handshake-leak', SOURCE, PREFIX + 'shutdown_cancels_handshake', '#749 half handshake reclaimed'),
    ('native-ws-drain-false-success', SOURCE, PREFIX + 'idle_socket_drain_reports_deadline', '#749 idle drain cannot claim success'),
    ('native-ws-small-output-limit', SOURCE, PREFIX + 'large_output_matches_ts', '#749 legal large output matches TS'),
    ('native-ws-drop-output-close', SOURCE, PREFIX + 'oversized_output_sends_close', '#749 oversized output sends close1009'),
    ('native-ws-drop-full-end', SOURCE, PREFIX + 'full_queue_eof_flushes_successfully', '#749 full queue EOF flushes successfully'),
    ('native-ws-writer-priority', SOURCE, PREFIX + 'completed_writer_wins_ready_deadline', '#749 completed writer wins ready deadline'),
    ('native-ws-buffer-close-code', SOURCE, PREFIX + 'buffer_capacity_close_uses_1009', '#749 saturated input admission sends close1009'),
    ('native-ws-server-header-budget', ROOT / 'src/pilotdeck-gateway/src/frame.rs', PREFIX + 'frame_header_budgets_match_encoding', '#749 header budgets cover actual wire encoding'),
    ('native-ws-client-header-budget', ROOT / 'src/pilotdeck-gateway/src/frame.rs', PREFIX + 'frame_header_budgets_match_encoding', '#749 header budgets cover actual wire encoding'),
    # #2153 fix-r1 (review r1): the budget replay's `content-length: 0` mask
    # is scoped to the oracle's 431 cells through `fn parser_admission`.
    # The drop mutant disables the predicate, so a 431 cell's replay fails
    # at its own byte comparison; the widening mutant fails the coverage
    # guard that reads the same predicate; the mask-always mutant makes
    # `budget_normalize` ignore its scope flag, so the injected-line unit
    # test fails; the two product mutants drop or grow the wire line the
    # raw pin holds in both directions.
    ('native-ws-budget-cl0-off', TESTS, PREFIX + 'upgrade_budget_boundary_matches_ts',
     "#1351 every boundary cell's response matches the recorded TS bytes"),
    ('native-ws-budget-cl0-wide', TESTS, PREFIX + 'parser_admission_cells_cover_exactly_the_oracle_431s',
     "#2153 the content-length exemption covers exactly the oracle's 431 cells"),
    ('native-ws-budget-cl0-mask-always', TESTS, PREFIX + 'only_the_registered_cells_mask_a_content_length_zero',
     '#2153 an injected content-length: 0 on a served cell must not normalize away'),
    # #2789 cluster 2: a stale entry in PARSER_ADMISSION_CELLS admits nothing
    # through the predicate, so only the direct set-comparison leg of the
    # coverage guard (the sorted constant against the sorted oracle 431s)
    # can catch it — the mutant appends one unrecorded id to that leg.
    ('native-ws-budget-cl0-stale', TESTS, PREFIX + 'parser_admission_cells_cover_exactly_the_oracle_431s',
     "#2153 the exemption list itself is exactly the oracle's 431 cells, so a stale entry points at the cell to re-check"),
    ('native-http-431-cl0-off', ROOT / 'src/pilotdeck-gateway/src/native_http.rs',
     PREFIX + 'budget_content_length_zero_stays_scoped_to_the_registered_431s',
     '#2153 pin: the registered 431 carries content-length: 0'),
    ('native-ws-101-cl0', ROOT / 'src/pilotdeck-gateway/src/acceptor.rs',
     PREFIX + 'budget_content_length_zero_stays_scoped_to_the_registered_431s',
     '#2153 pin: the served cell carries no content-length at all'),
]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    args = parser.parse_args()
    preflight([dict(name=n, path=p, test=t, anchor=a) for n, p, t, a in switches])
    # #1890: one list feeds both the binding and the file reads, so widening
    # `modules` cannot bind one file and check another (and cannot KeyError).
    # #1942: rows may bind a test outside `native_ws::tests` (the failed
    # initialization teardown lives in the turn-gateway suite), so the list
    # carries that module too and every row is audited against its own file.
    modules = ((PREFIX, TESTS), (TURN_TESTS, TURN_TESTS_SOURCE))
    sources = {path: path.read_text() for _, path in modules}
    # #2303 (条 4): every row is audited before the refusal, and a scanner
    # refusal (ValueError) becomes one named INVALID record per row instead
    # of a raw traceback on the first failure -- the session-factory /
    # session-drop-release audit shape.
    invalid = []
    for _, _, test, assertion in switches:
        # #1890: the same rule as the other audits -- exactly one module level
        # under the listed prefix, and a declaration that exists in the file
        # the binding returned. bind_module_file raises when a row names no
        # listed module, which carries the #749 "test binding names no audited
        # test module" guard (it also rejects an empty or extra-level short
        # name, so the binding stays exact).
        try:
            file, short = bind_module_file(test, modules)
            # #2303 (条 5): the assertion marker must live in code, never in
            # a comment -- the raw-text scan let a commented-out copy of the
            # assertion satisfy the binding after the real one was deleted.
            if (not declares_test(sources[file], short)
                    or not assertion_marker_present(sources[file], '"' + assertion + '"')):
                raise ValueError('stale test/assertion binding')
        except Exception as error:
            invalid.append(f'{test}: {type(error).__name__}: {error}')
    if invalid:
        raise RuntimeError('#749 binding audit INVALID rows: ' + '; '.join(invalid))
    if args.check:
        print(f'#749 {len(switches)} switch bindings valid')
        return 0
    if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
        raise RuntimeError('#749 output and caller CARGO_TARGET_DIR are required')
    names = args.selected or [row[0] for row in switches]
    if len(names) != len(set(names)) or set(names) - {row[0] for row in switches}:
        raise RuntimeError('#749 unknown or repeated selection')
    selected = [row for row in switches if row[0] in names]
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = dict(clean_environment(os.environ), CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only')
    for key in ('PDRS_MUTANT', ReceiptRun.ENV, ReceiptRun.PARENT_ENV, 'CARGO_TERM_QUIET'):
        env.pop(key, None)
    guarded = [SOURCE, SOURCE.with_suffix('') / 'tests.rs', ROOT / 'src/pilotdeck-gateway/src/runtime/turn/gateway.rs',
               ROOT / 'src/pilotdeck-gateway/src/runtime/turn/gateway_tests.rs', ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
               ROOT / 'src/pilotdeck-gateway/src/http_proxy.rs', ROOT / 'src/pilotdeck-gateway/src/frame.rs',
               ROOT / 'src/pilotdeck-gateway/src/acceptor.rs', ROOT / 'src/pilotdeck-gateway/src/lib.rs',
               ROOT / 'src/pilotdeck-gateway/tests/common/negative_receipt.rs',
               # #1351 review: the upgrade budget decision and the golden replay's
               # inputs live outside this directory — the upgrade diversion calls
               # native_http::over_header_budget / tracked_header_bytes, and
               # upgrade_budget_boundary_matches_ts reads the budget fixture via
               # include_str!, so a mid-run edit must be caught (native-http-basic
               # lists its own fixture files the same way).
               ROOT / 'src/pilotdeck-gateway/src/native_http.rs',
               FIXTURE.parent / 'native-ws-upgrade-budget' / 'oracle.json',
               FIXTURE.parent / 'native-ws-upgrade-budget' / 'generate.mts',
               FIXTURE.parent / 'native-ws-upgrade-budget' / 'README.md',
               FIXTURE.parent / 'negative_process.py', FIXTURE.parent / 'negative_suite.py',
               FIXTURE.parent / '_shared/mutant_switch.py']
    guarded += [path for path in FIXTURE.iterdir() if path.is_file()]
    rows = []

    def publish():
        counts = {state: sum(row['status'] == state for row in rows)
                  for state in ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED')}
        pending = output / 'summary.json.tmp'
        pending.write_text(json.dumps({'controls': rows, 'counts': counts}, indent=2) + '\n')
        pending.replace(output / 'summary.json')
        return counts

    def run(label, test, current, compile_only=False):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (output / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        command = ['cargo', 'test', '--offline', '--locked', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-gateway', '--lib', '--features', 'pdrs-mutants']
        command += ['--no-run'] if compile_only else [test, '--', '--exact', '--show-output']
        result = run_owned(command, cwd=ROOT, env=current, timeout=600)
        (output / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    try:
        with tree_lock(ROOT, timeout=900):
            hashes = record_sources(ROOT, output, guarded)
            for name, _, test, assertion in selected:
                row = dict(name=name, test=test, status='INVALID', reason='not run', compiled=False)
                try:
                    changed = dict(env, PDRS_MUTANT=name)
                    if run(name + '-compile', test, changed, True).returncode:
                        raise RuntimeError('mutant failed compilation; not a runtime kill')
                    row['compiled'] = True
                    if args.compile_only:
                        row.update(status='COMPILED', reason='feature branches compiled')
                    else:
                        if not passed(run(name + '-positive', test, env), test):
                            raise RuntimeError('positive bound test failed')
                        receipt = ReceiptRun(output, name, [(test, assertion)])
                        result = run(name + '-negative', test, receipt.environment(changed))
                        killed, reason, hits = judge(result, '', [(test, assertion)], receipt)
                        row.update(status='KILLED' if killed else ('SURVIVED' if passed(result, test) else 'INVALID'), reason=reason, hits=hits)
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                # Always restore the selector and verify, including failed
                # positives, compilation errors and receipt exceptions.
                if not args.compile_only:
                    try:
                        if not passed(run(name + '-restored', test, env), test):
                            raise RuntimeError('restored bound test failed')
                    except Exception as error:
                        row.update(status='INVALID', reason=row['reason'] + f'; restored: {error}')
                rows.append(row)
                publish()
                print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)
            verify_unchanged(ROOT, hashes)
            enforce_denominator(ROOT, Path(__file__), rows, args.selected)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        for row in rows:
            row.update(status='INVALID', reason=reason, compiled=False)
        for name, _, test, _ in selected[len(rows):]:
            rows.append(dict(name=name, test=test, status='INVALID', reason=reason, compiled=False))
    counts = publish()
    print(json.dumps(counts), flush=True)
    required = 'COMPILED' if args.compile_only else 'KILLED'
    return int(not rows or any(row['status'] != required for row in rows))


if __name__ == '__main__':
    raise SystemExit(main())

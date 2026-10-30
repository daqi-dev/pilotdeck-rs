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
from negative_suite import ReceiptRun, judge, passed
from _shared.mutant_switch import (preflight, clean_environment, record_sources,
                                   verify_unchanged, enforce_denominator)
SOURCE = ROOT / 'src/pilotdeck-gateway/src/native_ws.rs'
TESTS = ROOT / 'src/pilotdeck-gateway/src/native_ws/tests.rs'
PREFIX = 'native_ws::tests::'
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
    ('native-ws-retain-reader', SOURCE, PREFIX + 'socket_drops_before_business_drains', '#749 closed socket released before business drain'),
    ('native-ws-skip-host-cleanup', ROOT / 'src/pilotdeck-gateway/src/runtime/turn/gateway.rs', PREFIX + 'host_cleanup_precedes_runtime_exit', '#749 owned cleanup called'),
    ('native-ws-error-skips-cleanup', ROOT / 'src/pilotdeck-gateway/src/runtime/turn/gateway.rs', PREFIX + 'host_cleanup_runs_after_session_failure', '#749 cleanup runs after session failure'),
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
    ('native-ws-pending-limit', SOURCE, PREFIX + 'pending_requests_are_bounded_before_enqueue', '#749 pending cap rejects before host enqueue'),
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
]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    args = parser.parse_args()
    preflight([dict(name=n, path=p, test=t, anchor=a) for n, p, t, a in switches])
    tests = (ROOT / 'src/pilotdeck-gateway/src/native_ws/tests.rs').read_text()
    for _, _, test, assertion in switches:
        if 'fn ' + test.split('::')[-1] + '(' not in tests or '"' + assertion + '"' not in tests:
            raise RuntimeError('#749 stale test/assertion binding: ' + test)
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

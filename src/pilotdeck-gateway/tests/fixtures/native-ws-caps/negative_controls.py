#!/usr/bin/env python3
"""#1079 compile-time controls with an unchanged source=HEAD guard.

No source is edited: feature branches are compiled together. Each selector still
gets a no-run receipt and positive/negative/restored execution in fresh children.
Uncommitted sources are refused; the coordinator runs this after committing.
"""
import argparse
import json
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[5]
FIXTURE = Path(__file__).resolve().parent
sys.path.insert(0, str(FIXTURE.parent))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import ReceiptRun, bind_module_file, declares_test, judge, passed
from negative_source import assertion_marker_present
from _shared.mutant_switch import preflight, clean_environment, enforce_denominator

SOURCE = ROOT / 'src/pilotdeck-gateway/src/native_ws.rs'
TESTS = ROOT / 'src/pilotdeck-gateway/src/native_ws/caps_tests.rs'
HOST = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/gateway.rs'
HOST_TESTS = HOST.with_name('gateway_tests.rs')
PREFIX = 'native_ws::caps_tests::'
# #1885: these selectors flip compile-time feature branches, so no source or
# fixture byte is ever rewritten. `negative_process.mutating_sources` rewrites
# every guarded path on exit with a plain `write_bytes` and no atomic publish,
# so a SIGKILL inside that window truncates whatever the set carries; the old
# `FIXTURE.iterdir()` entry put `oracle.json` there while no control touched it.
# Leftovers are still caught: `mutating_sources` proves the whole `src/` input
# tree equals HEAD, independently of this list.
RESTORE_SET = ()
switches = [
    ('ws-caps-request-budget', SOURCE, PREFIX + 'complete_large_requests_share_aggregate_budget', '#1079 complete requests retain aggregate charge'),
    ('ws-caps-request-close-code', SOURCE, PREFIX + 'complete_large_requests_share_aggregate_budget', '#1079 pending aggregate rejection uses close1009'),
    ('ws-caps-request-lossy-length', SOURCE, PREFIX + 'lossy_request_reservation_matches_node_bytes', '#1079 lossy bytes charged after replacement'),
    ('ws-caps-request-lossy-replacement', SOURCE, PREFIX + 'lossy_request_reservation_matches_node_bytes', '#1079 lossy conversion matches Node'),
    ('ws-caps-lossy-empty-samples', TESTS, PREFIX + 'lossy_request_reservation_matches_node_bytes', '#1079 lossy oracle covers fixed valid and invalid sequences'),
    ('ws-caps-early-request-refund', HOST, PREFIX + 'completed_request_refunds_before_waiter_reap', '#1079 host owns pending request lease'),
    ('ws-caps-request-permit-leak', HOST, PREFIX + 'completed_request_refunds_before_waiter_reap', '#1079 host completion refunds without waiter poll'),
    ('ws-caps-admission-request-refund', HOST, 'runtime::turn::gateway::tests::native_ws_queued_input_outlives_cancelled_admission_waiter', '#1079 queued job retains lease after admission waiter cancellation'),
    ('ws-caps-cancel-request-leak', HOST, PREFIX + 'pending_requests_release_on_shutdown_and_abort', '#1079 cancelled host returns request input'),
    ('ws-caps-oracle-pending-requests', TESTS, PREFIX + 'baseline_observations', '#1079 TS complete requests survive socket close until business completion'),
    ('ws-caps-default-connections', SOURCE, PREFIX + 'defaults_are_applied_to_bound_server', '#1079 default bind applies both resource limits'),
    ('ws-caps-default-input', SOURCE, PREFIX + 'defaults_are_applied_to_bound_server', '#1079 default bind applies both resource limits'),
    ('ws-caps-split-input-refund', SOURCE, PREFIX + 'split_backing_stays_charged_during_growth', '#1079 split growth includes original backing'),
    ('ws-caps-unreaped-records', SOURCE, PREFIX + 'completed_records_reaped_before_next_accept', '#1079 completed connection records stay within cap'),
    ('ws-caps-connection-leak', SOURCE, PREFIX + 'connection_cap_covers_handshakes_and_recovers', '#1079 EOF releases half handshake slot'),
    ('ws-caps-refund-tail', SOURCE, PREFIX + 'consumed_frames_release_backing_after_tail', '#1079 tail retains backing charge'),
    ('ws-caps-connections', SOURCE, PREFIX + 'connection_cap_covers_handshakes_and_recovers', '#1079 connection budget bounds half handshakes'),
    ('ws-caps-early-connection-release', SOURCE, PREFIX + 'connection_cap_covers_handshakes_and_recovers', '#1079 connection budget bounds half handshakes'),
    ('ws-caps-input-budget', SOURCE, PREFIX + 'aggregate_overload_isolated_from_existing_peers', '#1079 aggregate pressure closes only contender'),
    ('ws-caps-input-close-code', SOURCE, PREFIX + 'aggregate_overload_isolated_from_existing_peers', '#1079 aggregate rejection uses close1009'),
    ('ws-caps-early-input-refund', SOURCE, PREFIX + 'growth_charges_old_and_new_backing', '#1079 growth counts old and new allocations'),
    ('ws-caps-retain-empty', SOURCE, PREFIX + 'consumed_frames_release_backing_after_tail', '#1079 empty decoder releases allocation and charge'),
    ('ws-caps-input-drop-leak', SOURCE, PREFIX + 'shutdown_and_abort_release_resources', '#1079 shutdown and cancellation release all resources'),
    ('ws-caps-retain-input-during-drain', SOURCE, PREFIX + 'eof_releases_input_before_pending_business', '#1079 EOF frees input before business drain'),
    ('ws-caps-oracle-version', TESTS, PREFIX + 'baseline_observations', '#1079 TS version'),
    ('ws-caps-oracle-connections', TESTS, PREFIX + 'baseline_observations', '#1079 TS accepts 65 upgrades beside half handshake'),
    ('ws-caps-oracle-aggregate', TESTS, PREFIX + 'baseline_observations', '#1079 TS retains 270 MiB without aggregate close'),
    ('ws-caps-oracle-consumed', TESTS, PREFIX + 'baseline_observations', '#1079 TS consumes completed frame'),
    ('ws-caps-oracle-cancelled', TESTS, PREFIX + 'baseline_observations', '#1079 TS observes cancellation'),
    ('ws-caps-oracle-replacement', TESTS, PREFIX + 'baseline_observations', '#1079 TS accepts replacement'),
    ('ws-caps-oracle-pending-stop', TESTS, PREFIX + 'baseline_observations', '#1079 TS stop waits for predecessor'),
    ('ws-caps-oracle-cancelled-stop', TESTS, PREFIX + 'baseline_observations', '#1079 TS stop after cancellation'),
]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    args = parser.parse_args()
    preflight([dict(name=n, path=p, test=t, anchor=a) for n, p, t, a in switches])
    # #1890: bind each row to the file that declares its module. The old scan
    # concatenated both test files, so a same-named short function in the other
    # module satisfied the binding.
    modules = ((PREFIX, TESTS), ('runtime::turn::gateway::tests::', HOST_TESTS))
    sources = {path: path.read_text() for _, path in modules}
    # #2303 (条 4): every row is audited before the refusal, and a scanner
    # refusal (ValueError) becomes one named INVALID record per row instead
    # of a raw traceback on the first failure -- the session-factory /
    # session-drop-release audit shape.
    invalid = []
    for _, _, test, assertion in switches:
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
        raise RuntimeError('#1079 binding audit INVALID rows: ' + '; '.join(invalid))
    if args.check:
        # #1885: prove-failable pin -- restoring the old guarded list (product
        # files, shared modules, `FIXTURE.iterdir()`) trips this, while the run
        # itself reads the same constant.
        if RESTORE_SET:
            raise RuntimeError(
                '#1079 feature switches rewrite no bytes; restore set must stay empty: '
                + ', '.join(str(path) for path in RESTORE_SET))
        print(f'#1079 {len(switches)} switch bindings valid')
        return 0
    if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
        raise RuntimeError('#1079 output and caller CARGO_TARGET_DIR are required')
    requested = args.selected or [row[0] for row in switches]
    if len(requested) != len(set(requested)) or set(requested) - {row[0] for row in switches}:
        raise RuntimeError('#1079 unknown or repeated selector')
    selected = [row for row in switches if row[0] in requested]
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = dict(clean_environment(os.environ), CARGO_PROFILE_DEV_DEBUG='line-tables-only',
               CARGO_PROFILE_TEST_DEBUG='line-tables-only')
    for key in ('CARGO_TERM_QUIET', ReceiptRun.ENV, ReceiptRun.PARENT_ENV):
        env.pop(key, None)
    guarded = list(RESTORE_SET)
    rows = []

    def run(label, test, current, compile_only=False):
        gate = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (output / (label + '.slots.log')).write_text(gate.stdout + gate.stderr)
        command = ['cargo', 'test', '--offline', '--locked', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-gateway', '--lib', '--features', 'pdrs-mutants']
        command += ['--no-run'] if compile_only else [test, '--', '--exact', '--show-output']
        result = run_owned(command, cwd=ROOT, env=current, timeout=600)
        (output / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    try:
        with mutating_sources(ROOT, guarded):
            for name, _, test, assertion in selected:
                row = dict(name=name, test=test, status='INVALID', reason='not run', compiled=False)
                try:
                    mutant = dict(env, PDRS_MUTANT=name)
                    if run(name + '-compile', test, mutant, True).returncode:
                        raise RuntimeError('mutant compilation failed; never a runtime kill')
                    row['compiled'] = True
                    if args.compile_only:
                        row.update(status='COMPILED', reason='selected feature branch compiled')
                    else:
                        if not passed(run(name + '-positive', test, env), test):
                            raise RuntimeError('positive bound test failed')
                        receipts = ReceiptRun(output, name, [(test, assertion)])
                        result = run(name + '-negative', test, receipts.environment(mutant))
                        killed, reason, hits = judge(result, '', [(test, assertion)], receipts)
                        row.update(status='KILLED' if killed else ('SURVIVED' if passed(result, test) else 'INVALID'),
                                   reason=reason, hits=hits)
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                if not args.compile_only:
                    try:
                        if not passed(run(name + '-restored', test, env), test):
                            raise RuntimeError('restored bound test failed')
                    except Exception as error:
                        row.update(status='INVALID', reason=row['reason'] + f'; restored: {error}')
                rows.append(row)
                print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)
            enforce_denominator(ROOT, Path(__file__), rows, args.selected)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        for row in rows:
            row.update(status='INVALID', reason=reason, compiled=False)
        for name, _, test, _ in selected[len(rows):]:
            rows.append(dict(name=name, test=test, status='INVALID', reason=reason, compiled=False))
        print(reason, file=sys.stderr)
    counts = {state: sum(row['status'] == state for row in rows)
              for state in ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED')}
    temporary = output / 'summary.json.tmp'
    temporary.write_text(json.dumps({'controls': rows, 'counts': counts}, indent=2) + '\n')
    temporary.replace(output / 'summary.json')
    print(json.dumps(counts), flush=True)
    required = 'COMPILED' if args.compile_only else 'KILLED'
    return int(not rows or any(row['status'] != required for row in rows))


if __name__ == '__main__':
    raise SystemExit(main())

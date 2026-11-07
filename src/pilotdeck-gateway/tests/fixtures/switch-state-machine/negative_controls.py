#!/usr/bin/env python3
"""#1353 A8.5-pr2 switch state machine: compile-time switches with per-row
compilation and nonce receipts.

No source replacement: use the shared tree lock + working-tree SHA contract.
Caller supplies CARGO_TARGET_DIR; all selected rows run before the summary exit.

The four rows cover the machine's own contract on the matrix §四 failure
semantics — the A2 halt branch, the A8 gate release point, the DC-1/A3 cap
clause of the shared timeout predicate — and the production wiring's A10 flush
precondition. Each row's anchor is the receipt-emitting assertion inside the
named test, so only that assertion's panic can kill its mutant.
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
from _shared.mutant_switch import (preflight, clean_environment, record_sources,
                                   verify_unchanged, enforce_denominator)

MACHINE = ROOT / 'src/pilotdeck-gateway/src/switch.rs'
MACHINE_TESTS = ROOT / 'src/pilotdeck-gateway/src/switch_tests.rs'
WIRING = ROOT / 'src/pilotdeck-gateway/src/native_process/switch_wiring.rs'
WIRING_TESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/switch_wiring_tests.rs'
MACHINE_PREFIX = 'switch_tests::'
WIRING_PREFIX = 'native_process::switch_wiring_tests::'

switches = [
    # A2: the halt branch in `SwitchMachine::bounded` deleted — the face's
    # rejection is swallowed and the machine walks on to the later faces, so
    # the machine-level anchor must fail on its leading receipt assertion.
    ('switch-halt-continues', MACHINE,
     MACHINE_PREFIX + 'drain_rejection_halts_the_switch',
     '#1353 A2: a rejecting face halts the switch with its own failure'),
    # A8: the gate release pulled ahead of the owner stop — the anchor asserts
    # the gate is still closed while its stop_owner face is running.
    ('switch-gate-early-open', MACHINE,
     MACHINE_PREFIX + 'gate_stays_closed_until_the_owner_stopped',
     '#1353 A8: the gate is closed while stop_owner is still running'),
    # A3/DC-1: the cap clause of the shared timeout predicate deleted, so the
    # actually-unbounded spellings (`u64::MAX` ms, `1e300`) pass as bounded.
    ('switch-unbounded-timeout', MACHINE,
     MACHINE_PREFIX + 'bounded_timeout_predicate_rejects_unbounded_values',
     '#1353 DC-1: the f64 spelling rejects the uncapped value'),
    # A10: the production flush face's precondition gate deleted, so the
    # half-drained flush proceeds instead of rejecting.
    ('switch-flush-before-drain', WIRING,
     WIRING_PREFIX + 'the_flush_face_rejects_until_the_entries_have_drained',
     '#1353 A10: flush rejects before the entry drain completed'),
]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    args = parser.parse_args()
    preflight([dict(name=n, path=p, test=t, anchor=a) for n, p, t, a in switches])
    # One list feeds both the binding and the file reads: a row cannot bind
    # one module and audit another (the #1890 rule).
    modules = ((MACHINE_PREFIX, MACHINE_TESTS), (WIRING_PREFIX, WIRING_TESTS))
    sources = {path: path.read_text() for _, path in modules}
    for _, _, test, assertion in switches:
        file, short = bind_module_file(test, modules)
        source = sources[file]
        if not declares_test(source, short) or '"' + assertion + '"' not in source:
            raise RuntimeError('#1353 stale test/assertion binding: ' + test)
    if args.check:
        print(f'#1353 {len(switches)} switch bindings valid')
        return 0
    if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
        raise RuntimeError('#1353 output and caller CARGO_TARGET_DIR are required')
    names = args.selected or [row[0] for row in switches]
    if len(names) != len(set(names)) or set(names) - {row[0] for row in switches}:
        raise RuntimeError('#1353 unknown or repeated selection')
    selected = [row for row in switches if row[0] in names]
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = dict(clean_environment(os.environ), CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only')
    for key in ('PDRS_MUTANT', ReceiptRun.ENV, ReceiptRun.PARENT_ENV, 'CARGO_TERM_QUIET'):
        env.pop(key, None)
    guarded = [
        MACHINE, MACHINE_TESTS, WIRING, WIRING_TESTS,
        ROOT / 'src/pilotdeck-gateway/src/lib.rs',
        ROOT / 'src/pilotdeck-gateway/src/native_process.rs',
        ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
        ROOT / 'src/pilotdeck-gateway/tests/common/negative_receipt.rs',
        FIXTURE.parent / 'negative_process.py', FIXTURE.parent / 'negative_suite.py',
        FIXTURE.parent / 'negative_panic.py',
        FIXTURE.parent / '_shared/mutant_switch.py',
    ]
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
    print(json.dumps(counts, sort_keys=True))
    return 0 if counts.get('SURVIVED', 0) == 0 and counts.get('INVALID', 0) == 0 else 1


if __name__ == '__main__':
    sys.exit(main())

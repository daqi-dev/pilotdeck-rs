#!/usr/bin/env python3
"""#956 #957 FIFO creation gate controls. Run after committing the candidate.

Usage: CARGO_TARGET_DIR=... python3 negative_controls.py OUT [mutation ...]
Every selected mutant compiles first; all results are reported before exit.
"""
import argparse
import json
import os
import subprocess
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/tests/background_tasks.rs'
oracle = root / 'src/pilotdeck-gateway/tests/fixtures/background-create-gate/oracle.json'
cases = [
    ('remove-create-gate', source,
     '                    let gate = CreateGate::new(&root, index);\n                    gate.configure(&mut env);',
     '                    let gate = CreateGate::new(&root, index);\n                    // mutant: original shell executes before assertion',
     'dispose_drains_replay', '#960 FIFO child exited before reader rendezvous:'),
    ('immediate-shell-exit', source,
     'include_str!("fixtures/background-create-gate/shell.sh")',
     '"#!/bin/bash\\nexit 0\\n"',
     'dispose_drains_replay', '#960 FIFO child exited before reader rendezvous:'),
    ('skip-release', source,
     '                gate.release();\n                until("#956 #957 released command completes",',
     '                drop(gate);\n                until("#956 #957 released command completes",',
     'creation_gate_lifecycle', '#956 #957 terminal status'),
    ('skip-stop', source,
     '                tokio::time::timeout(\n                    Duration::from_secs(8),\n                    tasks.stop(&id, &JsValue::Undefined, None),\n                )\n                .await\n                .expect("#956 #957 stop deadline")\n                .expect("#956 #957 stop blocked command");',
     '                // mutant: do not cancel the blocked predecessor',
     'creation_gate_lifecycle', '#956 #957 terminal status'),
    ('skip-dispose', source,
     '                tokio::time::timeout(Duration::from_secs(8), tasks.dispose())\n                    .await\n                    .expect("#956 #957 dispose deadline");',
     '                // mutant: do not dispose the blocked predecessor',
     'creation_gate_lifecycle', '#956 #957 terminal status'),
    ('old-eight-second-cap', source,
     'if waited >= Duration::from_secs(60) {',
     'if waited >= Duration::from_secs(8) {',
     'creation_gate_waits_for_live_delayed_reader', '#960 live delayed reader stays pending past 8s'),
    ('ignore-exited-child', source,
     'if !alive || task.ended_at.is_some() || task.status.as_str() != "running" {',
     'if false && (!alive || task.ended_at.is_some() || task.status.as_str() != "running") {',
     'creation_gate_rejects_exited_reader', '#960 terminal reader rejected before total limit'),
    ('ignore-cancelled-child', source,
     'if !alive || task.ended_at.is_some() || task.status.as_str() != "running" {',
     'if false && (!alive || task.ended_at.is_some() || task.status.as_str() != "running") {',
     'creation_gate_rejects_cancelled_reader', '#960 terminal reader rejected before total limit'),
    ('missing-total-limit', source,
     'if waited >= Duration::from_secs(60) {',
     'if waited >= Duration::from_secs(61) {',
     'creation_gate_bounds_missing_reader', '#960 watchdog must reject without polling delay'),
    ('missing-pid-diagnostic', source,
     '"pid={pid} status={} alive={alive} waited={}ms"',
     '"process={pid} status={} alive={alive} waited={}ms"',
     'creation_gate_rejects_exited_reader', '#960 watchdog diagnostic fields'),
    ('missing-watchdog-mode', oracle,
     '"mode": "never-reader"',
     '"mode": "delayed-reader"',
     'creation_gate_bounds_missing_reader', '#960 watchdog matrix coverage'),
    ('missing-matrix-mode', oracle,
     '"mode": "stop"',
     '"mode": "release"',
     'creation_gate_lifecycle', '#956 #957 lifecycle matrix coverage'),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('out', type=Path)
    parser.add_argument('mutations', nargs='*')
    args = parser.parse_args()
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be provided by the caller')
    unknown = set(args.mutations) - {row[0] for row in cases}
    if unknown:
        parser.error(f'unknown mutations: {sorted(unknown)}')
    selected = [row for row in cases if not args.mutations or row[0] in args.mutations]
    args.out.mkdir(parents=True, exist_ok=False)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--test', 'background_tasks']
    results = []

    def run(label, test, compile_only=False):
        try:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=300)
            (args.out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
            suffix = ['--no-run'] if compile_only else [test, '--', '--exact']
            result = run_owned(command + suffix, cwd=root, env=env, timeout=300)
            (args.out / f'{label}.log').write_text(result.stdout + result.stderr)
            return result
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                for value in (error.stdout, error.stderr):
                    raw += value.decode(errors='replace') if isinstance(value, bytes) else value or ''
            with (args.out / f'{label}.log').open('a') as log:
                log.write(raw + f'\n{type(error).__name__}: {error}\n')
            raise

    def record(name, test, assertion, status, reason):
        results.append(dict(mutation=name, test=test, assertion=assertion, status=status, reason=reason))
        print(f'{name}: {status}: {reason}', flush=True)

    try:
        with mutating_sources(root, [source, oracle]) as snapshots:
            for name, path, old, new, test, assertion in selected:
                status, reason = 'INVALID', 'not run'
                try:
                    original = snapshots[path].decode()
                    if original.count(old) != 1:
                        raise ValueError(f'{name}: expected one source anchor')
                    if not args.compile_only and not passed(run(name + '-positive', test), test):
                        reason = 'bound positive test did not pass'
                    else:
                        path.write_text(original.replace(old, new, 1))
                        if run(name + '-compile', test, compile_only=True).returncode != 0:
                            reason = 'mutant failed to compile'
                        elif args.compile_only:
                            status, reason = 'COMPILED', 'mutant cargo test --no-run passed'
                        else:
                            status, reason = classify(run(name + '-negative', test),
                                Case(name, path, old, new, test, assertion))
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(snapshots[path])
                try:
                    if not args.compile_only and not passed(run(name + '-restored', test), test):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {error}'
                record(name, test, assertion, status, reason)
    except Exception as error:
        reason = f'source guard: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for name, _, _, _, test, assertion in selected[len(results):]:
            record(name, test, assertion, 'INVALID', reason)
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED')}
    (args.out / 'summary.json').write_text(json.dumps(dict(controls=results, counts=counts), indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    success = 'COMPILED' if args.compile_only else 'KILLED'
    return int(not results or any(row['status'] != success for row in results))


if __name__ == '__main__':
    if sys.argv[1:] == ['--check-anchors']:
        stale = []
        for name, path, old, _new, _test, _assertion in cases:
            count = path.read_text().count(old)
            if count != 1:
                stale.append((name, count))
        if stale:
            raise SystemExit(f'STALE ANCHORS: {stale}')
        print(f'{len(cases)} anchors: ok')
        raise SystemExit(0)
    raise SystemExit(main())

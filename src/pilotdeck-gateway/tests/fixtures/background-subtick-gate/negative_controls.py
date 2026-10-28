#!/usr/bin/env python3
"""#957 HEAD-only gate controls: OUT [mutation ...], or --compile-only OUT.

Use the caller's CARGO_TARGET_DIR. Every selected row compiles before its
runtime test, restores before its positive retest, and contributes to summary.
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
oracle = Path(__file__).with_name('oracle.json')
cases = [
    ('remove-body-gate', source,
     'receiver.await.expect("#957 explicit body release");',
     'drop(receiver);',
     'subagent_gate_holds_until_release', '#957 release body held before release'),
    ('finish-on-cancel', source,
     'receiver.await.expect("#957 explicit body release");',
     'tokio::select! { _ = receiver => {}, () = signal.cancelled() => {} }',
     'subagent_gate_holds_until_release', '#957 cancelled body held before release'),
    ('skip-entry-handshake', source,
     'self.signal = Some(subagent_bounded(entered).await.expect("#957 body entered"));',
     'drop(entered);',
     'subagent_gate_lifecycle', '#957 entry signal captured'),
    ('wrong-failure', source,
     '"fail" => Err(JsString::from("boom")),',
     '"fail" => Err(JsString::from("wrong")),',
     'subagent_gate_lifecycle', '#957 lifecycle fail'),
    ('skip-cancel-release', source,
     '                                    gate.release();\n                                    stop.await',
     '                                    // mutant: leave cancelled body held\n                                    stop.await',
     'sdk_projections_replay', '#957 subagent event deadline'),
    ('skip-explicit-release', source,
     '            if behavior != "block" {\n                gate.release();\n            }',
     '            if behavior != "block" {\n                std::hint::black_box(&mut gate);\n            }',
     'subagent_gate_holds_until_release', '#957 subagent event deadline'),
    ('missing-matrix-behavior', oracle,
     '"behavior": "block"',
     '"behavior": "fail"',
     'subagent_gate_lifecycle', '#957 lifecycle matrix coverage'),
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
    names = {row[0] for row in cases}
    if set(args.mutations) - names or len(set(args.mutations)) != len(args.mutations):
        parser.error('unknown or duplicate mutations')
    selected = [row for row in cases if not args.mutations or row[0] in args.mutations]
    args.out.mkdir(parents=True, exist_ok=False)
    command = ['cargo', 'test', '--offline', '-q', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--test', 'background_tasks']
    results = []

    def write(path, value):
        with path.open('x') as stream:
            stream.write(value)

    def run(label, test, *, compile_only=False):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=300)
        write(args.out / f'{label}.slots.log', slot.stdout + slot.stderr)
        suffix = ['--no-run'] if compile_only else [test, '--', '--exact', '--show-output', '--format=pretty']
        try:
            result = run_owned(command + suffix, cwd=root, env=env, timeout=300)
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                for value in (error.stdout, error.stderr):
                    raw += value.decode(errors='replace') if isinstance(value, bytes) else value or ''
            write(args.out / f'{label}.error.txt', raw + f'\n{type(error).__name__}: {error}\n')
            raise
        write(args.out / f'{label}.stdout', result.stdout)
        write(args.out / f'{label}.stderr', result.stderr)
        return result

    # Guard the test, golden data, generator and verdict dependencies as well
    # as the mutation source. No dirty/new-file exception and no shadow repo.
    guarded = [source, oracle, Path(__file__).resolve(), Path(__file__).with_name('generate.mts'),
               Path(__file__).resolve().parents[1] / 'negative_process.py',
               Path(__file__).resolve().parents[1] / 'negative_suite.py']
    for name, path, old, new, test, assertion in selected:
        item = dict(mutation=name, test=test, assertion=assertion,
                    status='INVALID', compiled=False, restoredPass=False, reason='not run')
        try:
            with mutating_sources(root, guarded) as snapshots:
                original = snapshots[path].decode()
                try:
                    if original.count(old) != 1:
                        raise ValueError(f'{name}: stale anchor; expected exactly one match')
                    if not args.compile_only and not passed(run(name + '-positive', test), test):
                        raise RuntimeError('bound positive test did not pass')
                    path.write_text(original.replace(old, new, 1))
                    if run(name + '-compile', test, compile_only=True).returncode != 0:
                        raise RuntimeError('mutant did not compile; never a kill')
                    item['compiled'] = True
                    if args.compile_only:
                        item.update(status='COMPILED', reason='cargo test --no-run passed')
                    else:
                        status, reason = classify(run(name + '-negative', test),
                                                  Case(name, path, old, new, test, assertion))
                        item.update(status=status, reason=reason)
                except Exception as error:
                    item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    path.write_bytes(snapshots[path])
                    if path.read_bytes() != snapshots[path]:
                        raise RuntimeError('byte restoration failed')
                if not args.compile_only:
                    item['restoredPass'] = passed(run(name + '-restored', test), test)
                    if not item['restoredPass']:
                        item.update(status='INVALID', reason=item['reason'] + '; restored test failed')
        except Exception as error:
            item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        results.append(item)
        write(args.out / f'{name}.result.json', json.dumps(item, indent=2) + '\n')
        print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    counts = {status: sum(row['status'] == status for row in results)
              for status in ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED')}
    write(args.out / 'summary.json', json.dumps(dict(controls=results, counts=counts), indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    expected = 'COMPILED' if args.compile_only else 'KILLED'
    return int(not results or any(row['status'] != expected for row in results))


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

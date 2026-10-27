#!/usr/bin/env python3
"""#940 controls: compile each mutant, require its bound runtime assertion.

Run after committing the patch, with no other writer/build in this worktree.
Every selected case is accounted for, including stale anchors or HEAD refusals.
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
source = root / 'src/pilotdeck-gateway/src/runtime/model/cpu.rs'
tests = root / 'src/pilotdeck-gateway/tests/web_tools.rs'
fixture = Path(__file__).resolve().parent
admission = 'html_conversion_admission_survives_dropped_requests'
cases = [
    ('early-permit-release', source, '        let _permit = permit;', '        drop(permit);', admission),
    ('fifth-slot', source, 'Semaphore::new(4)', 'Semaphore::new(5)', admission),
    ('skip-detached-drain', tests,
     'while batch.active.load(Ordering::SeqCst) != 0 {',
     'while false && batch.active.load(Ordering::SeqCst) != 0 {', admission),
    ('active-never-drains', tests,
     'self.active.fetch_sub(1, Ordering::SeqCst);',
     'self.active.fetch_sub(usize::from(id == 0 || id >= 4), Ordering::SeqCst);', admission),
    ('raw-result-oracle', fixture / 'oracle.json',
     '"returnedLength": 9', '"returnedLength": 8',
     'html_conversion_parent_abort_keeps_ts_raw_success_and_async_progress'),
    ('missing-cases', fixture / 'oracle.json', '"cases": [', '"unusedCases": [',
     'html_conversion_parent_abort_keeps_ts_raw_success_and_async_progress'),
    ('missing-abort-case', fixture / 'oracle.json', '"abort": true', '"abort": false',
     'html_conversion_parent_abort_keeps_ts_raw_success_and_async_progress'),
    ('regeneration', fixture / 'generate.mts',
     "const html = '<p>converted</p>';", "const html = '<p>changed</p>';",
     'web_conversion_lifecycle_regenerates_identically'),
]
ANCHOR_COUNTS = {'raw-result-oracle': 2}
messages = {
    'early-permit-release': '#940 a fifth worker bypassed conversion admission',
    'fifth-slot': '#940 a fifth worker bypassed conversion admission',
    'skip-detached-drain': '#940 final active workers',
    'active-never-drains': '#940 detached workers must drain within the deadline',
    'raw-result-oracle': '#940 real TS raw conversion result',
    'missing-cases': '#940 normal and abort cases must both exist',
    'missing-abort-case': '#940 lifecycle cases must be normal then abort',
    'regeneration': 'web-conversion-lifecycle/oracle.json differs from the regenerated',
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    if not os.environ.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be supplied by the caller')
    selected = [case for case in cases if not args.selected or case[0] in args.selected]
    if not selected or set(args.selected) - {case[0] for case in cases}:
        parser.error('unknown or empty selection')
    if any(case[0] == 'regeneration' for case in selected) and not os.environ.get('PDRS_BASELINE'):
        parser.error('PDRS_BASELINE is required for regeneration')
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--test', 'web_tools']

    def run(label, arguments):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=120)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        try:
            result = run_owned(command + arguments, cwd=root, env=env, timeout=180)
        except subprocess.TimeoutExpired as error:
            raw = ''.join(value.decode(errors='replace') if isinstance(value, bytes) else value or ''
                          for value in (error.stdout, error.stderr))
            (out / (label + '.log')).write_text(raw + f'\n{type(error).__name__}: {error}\n')
            raise
        (out / (label + '.log')).write_text(result.stdout + result.stderr + f'\nexit={result.returncode}\n')
        return result

    results = []
    for name, path, before, after, test in selected:
        status, reason = 'INVALID', ''
        arguments = [test, '--', '--exact', '--include-ignored', '--nocapture']
        try:
            with mutating_sources(root, [path]) as originals:
                original = originals[path].decode()
                count = original.count(before)
                expected = ANCHOR_COUNTS.get(name, 1)
                if count != expected:
                    raise ValueError(f'ANCHOR_EXPIRED {name}: {count} matches, expected {expected}')
                if not passed(run(name + '-positive', arguments), test):
                    raise RuntimeError('positive control did not pass the bound test')
                path.write_text(original.replace(before, after))
                compiled = run(name + '-compile', ['--no-run'])
                if compiled.returncode != 0:
                    raise RuntimeError('mutant failed to compile; not a runtime kill')
                case = Case(name, path, before, after, test, messages[name])
                status, reason = classify(run(name + '-negative', arguments), case)
            if not passed(run(name + '-restored', arguments), test):
                status, reason = 'INVALID', 'restored control did not pass the bound test'
        except Exception as error:
            status, reason = 'INVALID', f'{type(error).__name__}: {error}'
        results.append({'mutation': name, 'test': test, 'assertion': messages[name],
                        'status': status, 'reason': reason})
        print(f'{name}: {status}: {reason}', flush=True)
        (out / 'summary.json').write_text(json.dumps(results, indent=2) + '\n')
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    return int(any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    sys.exit(main())

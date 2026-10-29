#!/usr/bin/env python3
# anchor-selfcheck: unsupported #953 mask diagnostics suite, rows are runtime cases
"""#953 mutation controls for run-registry mask diagnostics.

Run from the repository root after the work is committed:

    CARGO_TARGET_DIR=<absolute> python3 \
        src/pilotdeck-gateway/tests/fixtures/regeneration-masks/negative.py \
        _tmp/20261026-issue-953/negative-regeneration-masks-<attempt>

Each row runs the positive test, mutates `common/regeneration_masks.rs`, reruns
the same exact test, and restores the bytes before the next row. All rows are
collected before the summary; any non-KILLED row exits 1. Cargo uses the
caller's CARGO_TARGET_DIR and never `-q`.

The output directory must be missing or empty: reusing one that already holds
evidence would overwrite the previous run's original logs. Every log is created
exclusively, so a rerun needs a new directory.
"""
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (exclusive_write, mutating_sources,
                              new_output_directory, run_owned, wait_for_slot)
from negative_suite import invalidate_completed

ROOT = Path(__file__).resolve().parents[5]
MASKS = 'src/pilotdeck-gateway/tests/common/regeneration_masks.rs'

ROWS = [
    dict(
        name='foreign-pid-accepted',
        old='"pid" => generator_pid.is_some_and(|pid| token == pid.to_string().as_bytes()),',
        new='"pid" => true,',
        test='run_registry_rejects_a_foreign_owner_pid',
        anchors=['#953 a foreign owner PID must not be masked'],
    ),
    dict(
        name='foreign-pid-diagnostic-generic',
        old='"#280 foreign owner PID {actual} at {file}{pointer}; expected this run\'s generator PID {expected}"',
        new='"#280 invalid pid value at {file}{pointer}"',
        test='run_registry_rejects_a_foreign_owner_pid',
        anchors=['#953 foreign owner PID diagnostic must name actual and expected'],
    ),
    dict(
        name='truncation-reported-missing',
        old='        if !complete {',
        new='        if false && !complete {',
        test='truncation_before_a_masked_span_names_the_offset_and_path',
        anchors=['#953 truncation diagnostic must be readable'],
    ),
]


def command(test):
    return [
        'cargo', 'test', '--offline', '--locked',
        '--manifest-path', str(ROOT / 'src/Cargo.toml'),
        '-p', 'pilotdeck-gateway', '--test', 'regeneration_masks',
        test, '--', '--exact', '--nocapture',
    ]


def run(test, env, output, label):
    wait_for_slot(ROOT / 'scripts/cargo-slots.sh')
    result = run_owned(command(test), cwd=ROOT, env=env, timeout=900)
    exclusive_write(output / f'{label}.log', result.stdout + result.stderr)
    return result


def passed(result, test):
    log = result.stdout + result.stderr
    return (result.returncode == 0 and f'test {test} ... ok' in log
            and 'test result: ok. 1 passed;' in log)


def classify(result, test, anchors):
    log = result.stdout + result.stderr
    if passed(result, test):
        return 'SURVIVED', 'bound test still passed'
    if (result.returncode != 0 and 'test result: FAILED' in log
            and f'test {test} ... FAILED' in log and 'panicked at' in log
            and any(anchor in log for anchor in anchors)):
        return 'KILLED', 'bound test failed at its assertion'
    return 'INVALID', 'missing bound runtime failure (build error, wrong panic, or missing test)'


def main():
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    output = new_output_directory(sys.argv[1])
    env = dict(os.environ, CARGO_NET_OFFLINE='true')
    results = []
    try:
        with mutating_sources(ROOT, [MASKS]) as originals:
            for row in ROWS:
                status, reason = 'INVALID', ''
                try:
                    base = run(row['test'], env, output, row['name'] + '-positive')
                    if not passed(base, row['test']):
                        status, reason = 'INVALID', 'positive control did not pass the bound test'
                    else:
                        original = originals[Path(MASKS)].decode()
                        if original.count(row['old']) != 1:
                            status, reason = 'INVALID', 'mutation anchor is not unique'
                        else:
                            Path(ROOT / MASKS).write_bytes(
                                original.replace(row['old'], row['new'], 1).encode())
                            status, reason = classify(run(row['test'], env, output, row['name'] + '-negative'), row['test'], row['anchors'])
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    Path(ROOT / MASKS).write_bytes(originals[Path(MASKS)])
                try:
                    if not passed(run(row['test'], env, output, row['name'] + '-restored'), row['test']):
                        status, reason = 'INVALID', (reason + '; ' if reason else '') + 'restored positive control did not pass'
                except Exception as error:
                    status, reason = 'INVALID', (reason + '; ' if reason else '') + f'restored: {type(error).__name__}: {error}'
                results.append(dict(name=row['name'], test=row['test'], status=status, reason=reason))
                print(f"{row['name']}: {status}: {reason}", flush=True)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for row in ROWS[len(results):]:
            results.append(dict(name=row['name'], test=row['test'], status='INVALID', reason=reason))
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    exclusive_write(output / 'summary.json', json.dumps(
        {'controls': results, 'counts': counts}, indent=2) + '\n')
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()))
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())

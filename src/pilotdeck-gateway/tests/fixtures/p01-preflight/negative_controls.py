#!/usr/bin/env python3
"""#982 (#123 D1-P01) preflight negative controls for pilotdeck-cli sdk.rs.

Runs after the branch is committed: ``mutating_sources`` proves the mutated
sources equal HEAD before mutating, so these controls are executed by the
coordinator on the host (10-27 rule 11: this tree's fix is not in HEAD while the
branch is unmerged). Judgment is the shared ``negative_suite.classify``: a kill
needs the bound test's own ``FAILED`` line, a panic, and this case's exact
assertion message; a fixture/build failure or any other assertion (even another
``#982`` one) is INVALID, never KILLED. All selected mutants run to completion
before the summary, and any non-KILLED verdict exits non-zero.

Usage (CARGO_TARGET_DIR is mandatory; it is never created here):

    python3 -I negative_controls.py --check-anchors
    CARGO_TARGET_DIR=<dir> python3 -I negative_controls.py NEW_OUTPUT_DIR [mutation ...]
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[5] / 'src/pilotdeck-gateway/tests/fixtures'))
from negative_process import mutating_sources, run_owned, wait_for_slot  # noqa: E402
from negative_suite import Case, classify, passed  # noqa: E402

REPO = Path(__file__).resolve().parents[5]
SDK = REPO / 'src/pilotdeck-cli/src/sdk.rs'
# The bound test file is guarded too: the ruler must not run against a
# modified copy of the test it judges with.
TESTFILE = REPO / 'src/pilotdeck-cli/tests/sdk_preflight_exit.rs'

GATE = 'gateway_empty_fails_the_shared_gate_before_connecting'
TOKENS = 'token_source_errors_match_ts_bytes'

# (name, path, before, after, test, bound assertion). ``before`` must occur
# exactly once, and every mutant is bound to its own assertion message — two
# mutants never share one. ``settings`` without a leaf routes to the get branch,
# so removing the get gate fails the settings-default cell first.
CASES = [
    (
        'settings-get-url-check-removed',
        SDK,
        '        "get" => {\n'
        '            create_client(&connection)?;\n',
        '        "get" => {\n'
        '            let _ = &connection;\n',
        GATE,
        '#982 settings-default--gateway-empty: stderr',
    ),
    (
        'settings-set-url-check-removed',
        SDK,
        '            // above already precedes both (#982 settings-set--gateway-empty).\n'
        '            create_client(&connection)?;\n',
        '            // above already precedes both (#982 settings-set--gateway-empty).\n'
        '            let _ = &connection;\n',
        GATE,
        '#982 settings-set--gateway-empty: stderr',
    ),
    (
        'settings-url-check-before-subcommand',
        SDK,
        '    let connection = read_connection(argv, env, cwd)?;\n'
        '    match argv.first().map_or("get", String::as_str) {\n',
        '    let connection = read_connection(argv, env, cwd)?;\n'
        '    create_client(&connection)?;\n'
        '    match argv.first().map_or("get", String::as_str) {\n',
        GATE,
        '#982 settings-unknown--gateway-empty: stderr',
    ),
    (
        'sdk-token-empty-falls-through',
        SDK,
        '            .string("PILOTDECK_SDK_AUTH_TOKEN")\n'
        '            .or_else(|| env.string("PILOTDECK_TOKEN"))\n',
        '            .string("PILOTDECK_SDK_AUTH_TOKEN")\n'
        '            .filter(|value| !value.is_empty())\n'
        '            .or_else(|| env.string("PILOTDECK_TOKEN"))\n',
        TOKENS,
        '#982 run--token-sdk-empty: stderr',
    ),
]


def check_anchors():
    failures = []
    for name, path, old, _, _, _ in CASES:
        count = path.read_text().count(old)
        if count != 1:
            failures.append(f'{name}: expected 1 anchor in {path.name}, got {count}')
    if len({row[5] for row in CASES}) != len(CASES):
        failures.append('two mutants share one bound assertion message')
    print('\n'.join(failures) if failures else f'{len(CASES)} anchors OK')
    return int(bool(failures))


def main():
    if sys.argv[1:] == ['--check-anchors']:
        return check_anchors()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('mutations', nargs='*')
    args = parser.parse_args()
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be provided by the caller')
    if not args.mutations:
        selected = list(CASES)
    else:
        selected = [row for row in CASES if row[0] in set(args.mutations)]
        # Unknown or duplicated names must refuse instead of running a subset.
        if len(selected) != len(args.mutations):
            parser.error(f'unknown or duplicate mutations: {args.mutations}')
    args.out.mkdir(parents=True, exist_ok=False)
    results = []
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-cli', '--test', 'sdk_preflight_exit']

    def run(label, test, compile_only=False):
        try:
            slot = wait_for_slot(REPO / 'scripts/cargo-slots.sh', timeout=300)
            (args.out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
            suffix = ['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored', '--show-output']
            result = run_owned(command + suffix, cwd=REPO, env=env, timeout=600)
            (args.out / f'{label}.log').write_text(result.stdout + result.stderr)
            return result
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                for item in (error.stdout, error.stderr):
                    raw += item.decode(errors='replace') if isinstance(item, bytes) else item or ''
            with (args.out / f'{label}.log').open('a') as log:
                log.write(raw + f'\n{type(error).__name__}: {error}\n')
            raise

    def record(name, test, assertion, status, reason):
        results.append(dict(mutation=name, test=test, assertion=assertion,
                            status=status, reason=reason))
        print(f'{name}: {status}: {reason}', flush=True)

    try:
        with mutating_sources(REPO, sorted({row[1] for row in selected} | {TESTFILE})) as snapshots:
            for name, path, old, new, test, assertion in selected:
                status, reason = 'INVALID', 'not run'
                try:
                    original = snapshots[path].decode()
                    if original.count(old) != 1:
                        raise ValueError(f'{name}: expected one source anchor')
                    if not passed(run(name + '-positive', test), test):
                        reason = 'bound positive test did not pass'
                    else:
                        path.write_text(original.replace(old, new, 1))
                        if run(name + '-compile', test, compile_only=True).returncode != 0:
                            reason = 'mutant failed to compile'
                        else:
                            status, reason = classify(
                                run(name + '-negative', test),
                                Case(name, path, old, new, test, assertion))
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(snapshots[path])
                try:
                    if not passed(run(name + '-restored', test), test):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {error}'
                record(name, test, assertion, status, reason)
    except Exception as error:
        # A baseline/lock/restoration failure makes further mutation unsafe.
        # Still account for every selected case and never leave an all-green summary.
        reason = f'source guard: {type(error).__name__}: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for name, _, _, _, test, assertion in selected[len(results):]:
            record(name, test, assertion, 'INVALID', reason)
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    (args.out / 'summary.json').write_text(json.dumps(
        dict(controls=results, counts=counts), indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())

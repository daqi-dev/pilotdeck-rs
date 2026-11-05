#!/usr/bin/env python3
"""#1453: compile each guard-order mutant, kill it at its bound assertion, restore.

Run after the candidate is committed, with CARGO_TARGET_DIR supplied by the
caller: python3 negative_controls.py NEW_OUTPUT_DIR [mutation ...].
The shared source guard must refuse uncommitted workspace inputs.
"""
import argparse
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, invalidate_completed, passed, publish_summary

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/native_process/methods/user_dialog.rs'
TEST = 'native_process::methods::tests::user_dialog_respond_validates_result_before_lease_id'
ASSERTION = '#1453 guard-order error message:'
EARLY_LEASE = '''                    let _ = optional_nonempty_string(&params, "leaseId").map_err(|_| {
                        domain_error(
                            "INVALID_USER_DIALOG_RESPONSE",
                            "leaseId must be a non-empty string when provided.",
                        )
                    })?;
'''
cases = [
    ('lease-before-result', source,
     '                    let result_val = params.get("result").ok_or_else(|| {',
     EARLY_LEASE + '                    let result_val = params.get("result").ok_or_else(|| {',
     TEST, ASSERTION),
    ('lease-before-value', source,
     '                    let answer = if behavior.equals_str("answered") {\n',
     '                    let answer = if behavior.equals_str("answered") {\n' + EARLY_LEASE,
     TEST, ASSERTION),
    ('lease-before-reason', source,
     '                        let reason = match result_val.get("reason") {',
     EARLY_LEASE + '                        let reason = match result_val.get("reason") {',
     TEST, ASSERTION),
    ('skip-lease-validation', source,
     '''                    let lease_id = optional_nonempty_string(&params, "leaseId").map_err(|_| {
                        domain_error(
                            "INVALID_USER_DIALOG_RESPONSE",
                            "leaseId must be a non-empty string when provided.",
                        )
                    })?;''',
     '                    let lease_id = None::<JsString>;',
     TEST, '#1453 guard-order invalid input must fail'),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
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
    command = ['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']
    results = []

    def run(label, *, compile_only=False):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
        (args.out / f'{label}.slots.log').write_text(slots.stdout + slots.stderr)
        arguments = ['--no-run'] if compile_only else [TEST, '--', '--exact']
        try:
            result = run_owned(command + arguments, cwd=root, env=env, timeout=900)
        except subprocess.TimeoutExpired as error:
            raw = ''.join(value.decode(errors='replace') if isinstance(value, bytes)
                          else value or '' for value in (error.stdout, error.stderr))
            (args.out / f'{label}.log').write_text(raw + f'\n{error}\n')
            raise
        (args.out / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    def record(name, test, assertion, status, reason, compiled=False):
        results.append(dict(mutation=name, test=test, assertion=assertion,
                            status=status, reason=reason, compiled=compiled))
        print(f'{name}: {status}: {reason}', flush=True)

    try:
        with mutating_sources(root, [source]) as snapshots:
            for name, path, old, new, test, assertion in selected:
                status, reason, compiled = 'INVALID', 'not run', False
                try:
                    original = snapshots[path].decode()
                    count = original.count(old)
                    if count != 1:
                        raise ValueError(f'{name}: stale anchor: expected 1, found {count}')
                    if not passed(run(name + '-positive'), test):
                        reason = 'bound positive control did not pass'
                    else:
                        path.write_text(original.replace(old, new, 1))
                        build = run(name + '-compile', compile_only=True)
                        compiled = build.returncode == 0
                        if not compiled:
                            reason = 'mutant failed cargo test --no-run'
                        else:
                            status, reason = classify(
                                run(name + '-negative'),
                                Case(name, path, old, new, test, assertion),
                            )
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(snapshots[path])
                    if path.read_bytes() != snapshots[path]:
                        raise RuntimeError(f'byte restoration failed: {path}')
                try:
                    if not passed(run(name + '-restored'), test):
                        status, reason = 'INVALID', reason + '; restored control did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restore test: {error}'
                record(name, test, assertion, status, reason, compiled)
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, _path, _old, _new, test, assertion in selected[len(results):]:
            record(name, test, assertion, 'INVALID', reason)
    # Publish only after the guard has restored and verified source bytes.
    publish_summary(args.out, results)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    if sys.argv[1:] == ['--check-anchors']:
        stale = [(name, path.read_text().count(old))
                 for name, path, old, _new, _test, _assertion in cases
                 if path.read_text().count(old) != 1]
        if stale:
            raise SystemExit(f'STALE ANCHORS: {stale}')
        print(f'{len(cases)} anchors: ok')
        raise SystemExit(0)
    raise SystemExit(main())

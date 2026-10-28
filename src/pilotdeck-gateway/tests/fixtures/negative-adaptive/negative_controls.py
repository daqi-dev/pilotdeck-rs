#!/usr/bin/env python3
"""#994 shared runner meta-controls; commit first, then run against real HEAD.

No cargo is launched: each Python mutant is syntax-checked before its bound
unittest runs. Every control restores, reruns, and contributes to the summary.
"""
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
shared = fixtures / '_shared/negative_control.py'
process = fixtures / 'negative_process.py'
tests = fixtures / 'test_negative_adaptive.py'
cases = [
    ('floor-too-small', shared, 'MIN_RUN_SECONDS = 20', 'MIN_RUN_SECONDS = 1',
     'AdaptiveTests.test_floor_and_multiplier', '#994 minimum runtime'),
    ('multiplier-too-small', shared, 'RUN_FACTOR = 5', 'RUN_FACTOR = 1',
     'AdaptiveTests.test_floor_and_multiplier', '#994 runtime limit is baseline times five'),
    ('compile-wall-clock', shared, "command, timeout=None,\n                                 idle_timeout=COMPILE_IDLE_SECONDS",
     "command, timeout=20,\n                                 idle_timeout=COMPILE_IDLE_SECONDS",
     'CargoTests.test_compile_is_separate_and_runtime_uses_baseline', '#994 compile must not use test wall clock'),
    ('compile-runs-tests', shared, "'--no-run', '--message-format=json'", "'--message-format=json'",
     'CargoTests.test_compile_is_separate_and_runtime_uses_baseline', '#994 must compile without running tests'),
    ('runtime-fixed-budget', shared, 'else adaptive_timeout(self.baselines[key]))', 'else BASELINE_RUN_SECONDS)',
     'CargoTests.test_compile_is_separate_and_runtime_uses_baseline', '#994 exclude compile duration'),
    ('compile-error-accepted', shared, 'if compiled.returncode:', 'if False and compiled.returncode:',
     'CargoTests.test_compile_failure_never_runs_tests', '#994 compile failure is invalid'),
    ('skip-restored-rebuild', shared, "        slot = wait_for_slot(REPO / 'scripts/cargo-slots.sh', timeout=900)",
     "        if restored:\n            return subprocess.CompletedProcess([], 0, 'test result: ok.', '')\n        slot = wait_for_slot(REPO / 'scripts/cargo-slots.sh', timeout=900)",
     'CargoTests.test_restored_rebuilds_in_the_same_target_and_does_not_recalibrate', '#994 restored must rebuild'),
    ('stop-after-first', shared, 'for name, path, old, new, tests in rows:', 'for name, path, old, new, tests in rows[:1]:',
     'SuiteTests.test_all_mutations_restore_and_pass', '#994 complete accounting'),
    ('skip-scratch-restore', shared, 'copied.write_bytes(original)', 'copied.write_bytes(b"changed")',
     'SuiteTests.test_all_mutations_restore_and_pass', '#994 scratch restored'),
    ('allow-product-target', shared, 'if any(target == path.resolve() or path.resolve() in target.parents',
     'if False and any(target == path.resolve() or path.resolve() in target.parents',
     'AdaptiveTests.test_target_is_caller_owned_and_separate', '#994 reject product/disposable target'),
    ('nonfinite-idle-accepted', process,
     'if idle_timeout is not None and (not math.isfinite(idle_timeout) or idle_timeout <= 0):',
     'if False and idle_timeout is not None and (not math.isfinite(idle_timeout) or idle_timeout <= 0):',
     'ProgressTests.test_invalid_idle_deadline_fails_before_spawn', '#994 invalid idle deadline must not spawn'),
]


def main():
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    if len(sys.argv) != 2:
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT_DIR')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    records = []

    def run(label, command):
        # Fresh bytecode namespace prevents same-second, same-size Python
        # mutants from accidentally importing their predecessor's .pyc.
        env = dict(os.environ, PYTHONPYCACHEPREFIX=str(out / (label + '-cache')))
        result = run_owned(command, cwd=root, env=env, timeout=30)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    # wait_for_slot is intentionally unused: this suite launches only Python.
    for name, path, before, after, test, marker in cases:
        command = [sys.executable, str(tests), test]
        row = dict(name=name, status='INVALID', reason='not run', compiled=False, restoredPass=False)
        try:
            with mutating_sources(root, [path, tests]) as originals:
                original = originals[path].decode()
                try:
                    if original.count(before) != 1:
                        raise ValueError(f'stale anchor: {name}')
                    result = run(name + '-positive', command)
                    if result.returncode != 0:
                        raise RuntimeError('bound positive unittest did not pass')
                    path.write_text(original.replace(before, after, 1))
                    compiled = run(name + '-compile', [sys.executable, '-m', 'py_compile', str(path)])
                    if compiled.returncode != 0:
                        raise RuntimeError('Python mutant does not compile; never a kill')
                    row['compiled'] = True
                    result = run(name + '-negative', command)
                    log = result.stdout + result.stderr
                    failed = f'FAIL: {test.split(".")[-1]} ('
                    block = log.partition(failed)[2].split('\n====')[0]
                    if result.returncode and block and marker in block:
                        row.update(status='KILLED', reason='bound #994 unittest assertion failed')
                    elif result.returncode == 0:
                        row.update(status='SURVIVED', reason='bound unittest still passed')
                    else:
                        row.update(reason='unrelated error or missing named assertion')
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    path.write_bytes(originals[path])
                row['restoredPass'] = run(name + '-restored', command).returncode == 0
                if not row['restoredPass']:
                    row.update(status='INVALID', reason=row['reason'] + '; restored unittest failed')
        except Exception as error:
            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        records.append(row)
        print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)
    counts = {state: sum(row['status'] == state for row in records)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    (out / 'summary.json').write_text(json.dumps({'controls': records, 'counts': counts}, indent=2) + '\n')
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    return int(any(row['status'] != 'KILLED' for row in records))


if __name__ == '__main__':
    raise SystemExit(main())

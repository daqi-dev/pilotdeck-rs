#!/usr/bin/env python3
"""#1142 Python controls: commit first; never bypass the HEAD guard.

Usage: CARGO_TARGET_DIR=<caller-owned> python3 -I -B negative_controls.py OUTPUT
--check compiles all candidate replacements without writing a source file;
only a committed, guarded run can report KILLED. No cargo is needed for these
Python-only mutations. Each child has a 90s bound via the existing Python runner.
"""
import argparse
import importlib.util
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_process import exclusive_write, new_output_directory
from negative_suite import invalidate_completed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SOURCE = FIXTURES / 'negative_process.py'
TESTS = FIXTURES / 'test_negative_process.py'
JUDGE = FIXTURES / 'negative-output-directory/negative.py'

# Reuse the existing named-unittest/AssertionError judge and owned child runner.
# In particular, a Python ERROR, timeout or compilation failure is never a kill.
spec = importlib.util.spec_from_file_location('treelock_python_controls', JUDGE)
python_controls = importlib.util.module_from_spec(spec)
spec.loader.exec_module(python_controls)

# Identical injections intentionally bind different named tests: retaining an
# existing budget and delivering a caller deadline are separate obligations.
cases = [
    ('alarm-cleared', SOURCE,
     '    def _acquire(self, fd):\n',
     '    def _acquire(self, fd):\n        signal.setitimer(signal.ITIMER_REAL, 0)\n',
     'TreeLockBudget.test_alarm_survives_acquisition_and_nested_release',
     '#1142 caller timer must retain only its remaining budget'),
    ('wait-budget-restarted', SOURCE,
     '                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)\n                return',
     '                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)\n'
     '                signal.alarm(10)\n                return',
     'TreeLockBudget.test_alarm_keeps_elapsed_wait_when_holder_releases',
     '#1142 caller timer must retain only its remaining budget'),
    ('periodic-interval-cleared', SOURCE,
     '    def _acquire(self, fd):\n',
     '    def _acquire(self, fd):\n'
     '        remaining, _ = signal.getitimer(signal.ITIMER_REAL)\n'
     '        signal.setitimer(signal.ITIMER_REAL, remaining)\n',
     'TreeLockBudget.test_periodic_timer_survives',
     '#1142 caller timer interval must survive'),
    ('caller-deadline-cancelled', SOURCE,
     '    def _acquire(self, fd):\n',
     '    def _acquire(self, fd):\n        signal.setitimer(signal.ITIMER_REAL, 0)\n',
     'TreeLockBudget.test_outer_deadline_interrupts_wait_and_closes_fd',
     '#1142 caller deadline must interrupt a busy lock'),
    ('caller-handler-replaced', SOURCE,
     '    def _acquire(self, fd):\n',
     '    def _acquire(self, fd):\n        signal.signal(signal.SIGALRM, signal.SIG_IGN)\n',
     'TreeLockBudget.test_no_timer_is_created',
     '#1142 caller signal handler must survive'),
    ('idle-timer-armed', SOURCE,
     '    def _acquire(self, fd):\n',
     '    def _acquire(self, fd):\n        signal.setitimer(signal.ITIMER_REAL, 10)\n',
     'TreeLockBudget.test_no_timer_is_created',
     '#1142 idle acquisition must not arm a timer'),
    ('timeout-timer-cleared', SOURCE,
     '                if remaining <= 0:\n'
     '                    raise TimeoutError(f"source lock busy: {self.path}")',
     '                if remaining <= 0:\n'
     '                    signal.setitimer(signal.ITIMER_REAL, 0)\n'
     '                    raise TimeoutError(f"source lock busy: {self.path}")',
     'TreeLockBudget.test_lock_timeout_preserves_outer_alarm',
     '#1142 caller timer must retain only its remaining budget'),
    ('worker-deadline-ignored', SOURCE,
     '                if remaining <= 0:\n'
     '                    raise TimeoutError(f"source lock busy: {self.path}")',
     # Preserve TimeoutError, but delay it until the real holder releases so
     # only the pre-release completion check detects the unbounded worker wait.
     '                if remaining <= 0:\n'
     '                    if threading.current_thread() is not threading.main_thread():\n'
     '                        fcntl.flock(fd, fcntl.LOCK_EX)\n'
     '                    raise TimeoutError(f"source lock busy: {self.path}")',
     'TreeLockBudget.test_worker_wait_remains_bounded',
     '#1142 worker must respect lock deadline'),
]


def changed_source(original, name, old, new):
    count = original.count(old)
    if count != 1:
        raise ValueError(f'#1142 stale anchor {name}: count={count}')
    return original.replace(old, new, 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path, nargs='?')
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    if not os.environ.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be supplied by the caller')
    if args.check:
        for name, path, old, new, _test, _assertion in cases:
            compile(changed_source(path.read_text(), name, old, new), str(path), 'exec')
            print(f'{name}: anchor unique; Python candidate compile PASS (not executed)')
        return 0
    if args.output is None:
        parser.error('OUTPUT is required without --check')
    out = new_output_directory(args.output)
    results = []
    paths = [SOURCE, TESTS, Path(__file__).resolve(), JUDGE,
             FIXTURES / 'negative_suite.py', FIXTURES / 'negative_panic.py',
             FIXTURES / 'pending_reanchor.py']
    for name, path, old, new, test, assertion in cases:
        status, reason, compiled = 'INVALID', 'not run', False
        try:
            with mutating_sources(ROOT, paths) as originals:
                changed = changed_source(originals[path].decode(), name, old, new)
                if not python_controls.passed(python_controls.run(out, name + '-positive', test), test):
                    raise RuntimeError('#1142 bound positive test did not pass')
                path.write_text(changed)
                compile(path.read_bytes(), str(path), 'exec')
                compiled = True
                exclusive_write(out / (name + '-compile.log'), '#1142 on-disk Python compile PASS\n')
                status, reason = python_controls.classify(
                    python_controls.run(out, name + '-negative', test), test, assertion)
            with mutating_sources(ROOT, paths):
                if not python_controls.passed(python_controls.run(out, name + '-restored', test), test):
                    raise RuntimeError('#1142 bound restored test did not pass')
        except Exception as error:
            status, reason, compiled = 'INVALID', f'{type(error).__name__}: {error}', False
            invalidate_completed(results, '#1142 control/guard failure: ' + reason)
        results.append(dict(mutation=name, test=test, assertion=assertion,
                            status=status, reason=reason, compiled=compiled))
        print(f'{name}: {status}: {reason}', flush=True)
    publish_summary(out, results)
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()))
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())

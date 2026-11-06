#!/usr/bin/env python3
# anchor-selfcheck: unsupported meta-test of the shared runner's verdict guards, spawns -O subprocesses only
"""#1922: the shared runner's verdict guards must raise, not assert.

``_shared/negative_control.py`` decides KILLED vs PASS inside ``run()`` with
four ``raise`` guards (``running 1 test``, ``test result: FAILED`` /
``test result: ok. 1 passed``, the bound diagnostic, and the exit code).
Before #1666 they were ``assert`` statements, and ``python3 -O`` strips every
``assert``: a mutant that never compiled or never ran the bound test was then
recorded KILLED -- a ruler passing without measuring anything. The static
register in ``test_negative_optimize_guards.py`` only proves the *file* holds
no bare ``assert``; it does not prove these ``raise`` guards still fire under
``-O``. This file does: each case execs the real shared source inside a
``python3 -O`` subprocess and drives its ``run()`` with a fake ``cargo``
(first on the child PATH) that prints the chosen output and exits with the
chosen code. No cargo, slot or source mutation is involved: the shared slot
gate is stubbed with ``PDRS_CARGO_SLOT_BIN`` pointing at a non-executable
path, which ``scripts/cargo-slots.sh`` documents as "this host has no shared
quota to wait for" (exit 69) -- the one outcome ``run()`` accepts without
waiting.

Every refusal case's input lets all *other* guards pass, so reverting one
guard to ``assert`` leaves exactly that case with no exception under ``-O``
and turns it red. Two positive controls prove the harness reaches a real
verdict (PASS / KILLED printed, nothing raised).

Run from this directory: ``python3 -m unittest test_negative_run_guards``.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

FIXTURES = Path(__file__).resolve().parent
SHARED = FIXTURES / '_shared' / 'negative_control.py'
DRIVE_FLAG = '--drive-run-guard'
DIAGNOSTIC = 'boom-1922'
LABEL = 'case-1922'


def drive_run_guard():
    """Run inside ``python3 -O``: exec the real shared source, call ``run()``."""
    case = json.loads(sys.argv[2])
    namespace = {'__name__': 'driven_shared_negative_control_1922',
                 '__file__': str(SHARED)}
    exec(compile(SHARED.read_text(encoding='utf-8'), str(SHARED), 'exec'), namespace)
    # The child PATH puts the fake cargo first; the shared slot gate never
    # waits (PDRS_CARGO_SLOT_BIN is a non-executable path -> exit 69).
    child_env = {'PATH': case['bin'] + os.pathsep + os.environ.get('PATH', os.defpath),
                 'TMPDIR': case['tmp'],
                 'FAKE_OUTPUT': case['output'],
                 'FAKE_CODE': str(case['code'])}
    try:
        namespace['run'](LABEL, ['cargo', 'test', '--no-run'], child_env,
                         diagnostic=case['diagnostic'])
    except RuntimeError as error:
        print(f'VERDICT RAISED RuntimeError: {error}')
    except BaseException as error:
        print(f'VERDICT UNEXPECTED {type(error).__name__}: {error}')
    else:
        print('VERDICT NOT-RAISED')


class OptimizeStripsAssertHere(unittest.TestCase):
    """This file's red-on-revert logic only means anything if -O strips asserts."""

    def test_this_interpreter_really_strips_asserts(self):
        plain = subprocess.run([sys.executable, '-c', 'assert False'],
                               capture_output=True, text=True)
        optimized = subprocess.run([sys.executable, '-O', '-c', 'assert False'],
                                   capture_output=True, text=True)
        self.assertTrue(plain.returncode == 1,
                        '#1922 an `assert False` must fail without -O')
        self.assertTrue(optimized.returncode == 0,
                        '#1922 this interpreter does not strip asserts under -O; '
                        'the guard cases below would be untestable')


class SharedRunnerVerdictGuards(unittest.TestCase):
    """Four ``raise`` guards in ``run()`` decide KILLED vs PASS; prove they fire."""

    def drive(self, output, code, diagnostic):
        """Spawn ``python3 -O`` driving the real ``run()`` with a fake cargo."""
        with tempfile.TemporaryDirectory(prefix='negctl-1922-run-') as base:
            bin_dir = Path(base) / 'bin'
            bin_dir.mkdir()
            cargo = bin_dir / 'cargo'
            cargo.write_text('#!/bin/sh\nprintf \'%s\' "$FAKE_OUTPUT"\nexit "$FAKE_CODE"\n')
            os.chmod(cargo, 0o755)
            case = dict(label=LABEL, output=output, code=code,
                        diagnostic=diagnostic, bin=str(bin_dir), tmp=base)
            env = dict(os.environ)
            # An explicit non-executable gate means "no shared quota on this
            # host" to cargo-slots.sh (exit 69): the real gate is never queried.
            env['PDRS_CARGO_SLOT_BIN'] = str(Path(base) / 'absent-cargo-slot')
            return subprocess.run(
                [sys.executable, '-O', str(Path(__file__).resolve()), DRIVE_FLAG,
                 json.dumps(case)],
                cwd=str(FIXTURES), env=env, capture_output=True, text=True,
                timeout=180)

    def verdict(self, result):
        lines = [line for line in result.stdout.split('\n') if line.strip()]
        self.assertTrue(lines != [], '#1922 the -O driver printed no verdict')
        return lines[-1]

    def assertGuardRaised(self, result, needle):
        last = self.verdict(result)
        self.assertTrue(last.startswith('VERDICT RAISED RuntimeError: '),
                        f'#1922 the guard must raise RuntimeError under -O, got {last!r} '
                        f'(stderr={result.stderr!r})')
        self.assertIn(needle, last,
                      '#1922 the refusal must name its guard condition')

    def assertPassedThrough(self, result, outcome):
        last = self.verdict(result)
        self.assertTrue(last == 'VERDICT NOT-RAISED',
                        f'#1922 the clean run must not raise, got {last!r}')
        self.assertIn(f'{LABEL}: {outcome}', result.stdout,
                      f'#1922 the clean run must reach a {outcome} verdict')

    def test_missing_running_line_is_not_evidence(self):
        # No `running 1 test`: a compilation or zero-test run must not count
        # as a verdict. Reverting this guard to `assert` lets the case fall
        # through to PASS under -O (every other guard's input is satisfied).
        result = self.drive('test result: ok. 1 passed\n', 0, None)
        self.assertGuardRaised(result, 'compilation or zero-test failure is not evidence')

    def test_missing_pass_summary_is_not_a_pass(self):
        result = self.drive('running 1 test\n', 0, None)
        self.assertGuardRaised(result, "expected 'test result: ok. 1 passed' in the output")

    def test_missing_failed_summary_is_not_a_kill(self):
        # The diagnostic branch of the summary guard: the output names the
        # diagnostic but never prints the FAILED summary, so the expected
        # `test result: FAILED` line is what must refuse the verdict.
        result = self.drive(f'running 1 test\n{DIAGNOSTIC}\n', 101, DIAGNOSTIC)
        self.assertGuardRaised(result, "expected 'test result: FAILED' in the output")

    def test_zero_exit_with_diagnostic_is_not_a_kill(self):
        result = self.drive(f'running 1 test\ntest result: FAILED\n{DIAGNOSTIC}\n',
                            0, DIAGNOSTIC)
        self.assertGuardRaised(result, 'the bound test did not fail with the expected diagnostic')

    def test_missing_diagnostic_is_not_a_kill(self):
        result = self.drive('running 1 test\ntest result: FAILED\n', 101, DIAGNOSTIC)
        self.assertGuardRaised(result, 'the bound test did not fail with the expected diagnostic')

    def test_nonzero_exit_is_not_a_pass(self):
        result = self.drive('running 1 test\ntest result: ok. 1 passed\n', 3, None)
        self.assertGuardRaised(result, 'exited 3, expected 0')

    def test_clean_pass_and_clean_kill_reach_a_verdict(self):
        # Positive controls: with complete output the same driver reaches a
        # real verdict, so the refusals above are the guards firing, not a
        # harness that cannot run the shared code at all.
        result = self.drive('running 1 test\ntest result: ok. 1 passed\n', 0, None)
        self.assertPassedThrough(result, 'PASS')
        result = self.drive(f'running 1 test\ntest result: FAILED\n{DIAGNOSTIC}\n',
                            101, DIAGNOSTIC)
        self.assertPassedThrough(result, 'KILLED')


if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1] == DRIVE_FLAG:
        drive_run_guard()
    else:
        unittest.main()

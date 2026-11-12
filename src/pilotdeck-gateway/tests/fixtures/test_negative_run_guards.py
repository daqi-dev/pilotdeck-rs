#!/usr/bin/env python3
# anchor-selfcheck: unsupported meta-test of the shared runner's verdict guards, spawns -O subprocesses only
"""#1922: the shared runner's verdict guards must raise, not assert.

``_shared/negative_control.py`` decides KILLED vs PASS inside ``run()`` with
``raise`` guards (``running 1 test``, ``test result: ok. 1 passed``, the
``test result: FAILED`` summary, the ``negative_panic.named_test_killed``
kill criterion, and the exit code).
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

#1087 fix-r2: the diagnostic branch judges through the shared strict #813
criterion, so a kill transcript is only a real libtest shape -- the bound
test's own ``... FAILED`` status line plus the diagnostic inside that test's
own panic payload. The false-kill shapes the old whole-log substring check
accepted (the diagnostic echoed by the test's own stdout, a sibling thread's
panic) are pinned as refusal cases here.

#1087 fix-r3 (boundary #2): the strict judge alone accepts an aborted run --
a partial transcript that already printed the bound test's FAILED line and
its panic block but never reached libtest's summary. The restored
``test_missing_failed_summary_is_not_a_kill`` pins that refusal.

Every refusal case's input lets all *other* guards pass, so reverting one
guard to ``assert`` leaves exactly that case with no exception under ``-O``
and turns it red. Two positive controls prove the harness reaches a real
verdict (PASS / KILLED printed, nothing raised).

Scope boundary (review r1 sibling P2): this file covers ``run()`` only. The
sibling verdict guards in ``CargoTests.run`` / ``mutation_suite`` and the
``negative_suite`` path used by ``sdk-settings`` have no ``-O`` behaviour
case yet; they are protected by the static register in
``test_negative_optimize_guards.py`` and by review, not by this file.

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
TEST = 'guard_case_1922'
LABEL = 'case-1922'


def kill_transcript(payload):
    """A realistic one-test FAILED transcript whose panic body is `payload`."""
    return (
        'running 1 test\n'
        f'test {TEST} ... FAILED\n'
        '\n'
        'failures:\n'
        '\n'
        f'---- {TEST} stdout ----\n'
        f'thread \'{TEST}\' panicked at src/gateway.rs:10:5:\n'
        f'{payload}\n'
        'note: run with `RUST_BACKTRACE=1` environment variable to display '
        'a backtrace\n'
        '\n'
        'failures:\n'
        f'    {TEST}\n'
        '\n'
        'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; '
        '0 filtered out; finished in 0.01s\n'
    )


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
                         diagnostic=case['diagnostic'], test=case['test'])
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
    """The ``raise`` guards in ``run()`` decide KILLED vs PASS; prove they fire."""

    def drive(self, output, code, diagnostic):
        """Spawn ``python3 -O`` driving the real ``run()`` with a fake cargo."""
        with tempfile.TemporaryDirectory(prefix='negctl-1922-run-') as base:
            bin_dir = Path(base) / 'bin'
            bin_dir.mkdir()
            cargo = bin_dir / 'cargo'
            cargo.write_text('#!/bin/sh\nprintf \'%s\' "$FAKE_OUTPUT"\nexit "$FAKE_CODE"\n')
            os.chmod(cargo, 0o755)
            case = dict(label=LABEL, output=output, code=code,
                        diagnostic=diagnostic, test=TEST,
                        bin=str(bin_dir), tmp=base)
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
        # #1087 fix-r3 (boundary #2), restored for the strict judge: the
        # transcript carries everything the strict #813 judge needs (the
        # bound test's own FAILED status line and the diagnostic inside its
        # own panic block) but libtest never reached its summary -- an
        # aborted run is not a kill decision. Reverting this guard to
        # `assert` lets the aborted run fall through to KILLED under -O.
        transcript = kill_transcript(f'{DIAGNOSTIC} bound assertion message')
        aborted = transcript.split('test result: FAILED.')[0]
        result = self.drive(aborted, 101, DIAGNOSTIC)
        self.assertGuardRaised(result, "expected 'test result: FAILED' in the output")

    def test_zero_exit_with_diagnostic_is_not_a_kill(self):
        result = self.drive(kill_transcript(f'{DIAGNOSTIC} bound assertion message'), 0,
                            DIAGNOSTIC)
        self.assertGuardRaised(result, 'did not fail with the expected diagnostic')

    def test_missing_diagnostic_is_not_a_kill(self):
        # The bound test fails, but on an unrelated assertion whose payload
        # does not carry the diagnostic.
        result = self.drive(kill_transcript('assertion `left == right` failed'), 101,
                            DIAGNOSTIC)
        self.assertGuardRaised(result, 'did not fail with the expected diagnostic')

    def test_sibling_panic_is_not_a_kill(self):
        # #1087 fix-r2 false-kill shape: the diagnostic appears in a sibling
        # thread's panic while the bound test itself passes. The old
        # whole-log `diagnostic in output` recorded this KILLED.
        result = self.drive(
            'running 1 test\n'
            f'test {TEST} ... ok\n'
            "thread 'sibling-1922' panicked at src/gateway.rs:20:9:\n"
            f'{DIAGNOSTIC} leaked from a sibling thread\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display '
            'a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; '
            '0 filtered out; finished in 0.01s\n',
            101, DIAGNOSTIC)
        self.assertGuardRaised(result, 'did not fail with the expected diagnostic')

    def test_captured_stdout_is_not_a_kill(self):
        # #1087 fix-r2 false-kill shape: the test echoes the diagnostic on its
        # own stdout and then fails an unrelated assertion. The diagnostic is
        # in the whole log, but not in the bound test's own panic payload.
        result = self.drive(
            'running 1 test\n'
            f'test {TEST} ... FAILED\n'
            '\n'
            'failures:\n'
            '\n'
            f'---- {TEST} stdout ----\n'
            f'{DIAGNOSTIC} echoed by the test\'s own stdout\n'
            '\n'
            f"thread '{TEST}' panicked at src/gateway.rs:10:5:\n"
            'assertion `left == right` failed\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display '
            'a backtrace\n'
            '\n'
            'failures:\n'
            f'    {TEST}\n'
            '\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; '
            '0 filtered out; finished in 0.01s\n',
            101, DIAGNOSTIC)
        self.assertGuardRaised(result, 'did not fail with the expected diagnostic')

    def test_nonzero_exit_is_not_a_pass(self):
        result = self.drive('running 1 test\ntest result: ok. 1 passed\n', 3, None)
        self.assertGuardRaised(result, 'exited 3, expected 0')

    def test_clean_pass_and_clean_kill_reach_a_verdict(self):
        # Positive controls: with complete output the same driver reaches a
        # real verdict, so the refusals above are the guards firing, not a
        # harness that cannot run the shared code at all.
        result = self.drive('running 1 test\ntest result: ok. 1 passed\n', 0, None)
        self.assertPassedThrough(result, 'PASS')
        result = self.drive(kill_transcript(f'{DIAGNOSTIC} bound assertion message'),
                            101, DIAGNOSTIC)
        self.assertPassedThrough(result, 'KILLED')


if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1] == DRIVE_FLAG:
        drive_run_guard()
    else:
        unittest.main()

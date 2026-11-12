# anchor-selfcheck: unsupported unit tests of the replay kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r2 controls for the service-orchestration kill judge.

The delivered judge (``negative.py::classify``) is the shared #813 criterion:
the named test's own ``FAILED`` status line plus the mutant's anchor inside
that test's own panic payload. The controls below pin the positive shape and
the false-kill shapes the old judge (a whole-log ``anchor in log`` substring
plus a ``panicked at``-line filename scan) accepted — a sibling test's panic,
an anchor only in captured stdout, and an anchor inside build output. Every
log is a synthetic libtest transcript; no cargo run happens here (the
real-sample kill is proven by the coordinator's full run).

#1087 fix-r3 (boundary #3): the strict judge alone also accepts an aborted
run (a partial transcript with the FAILED line and the panic block but no
libtest summary) and a panic whose location is not the mutant's own file.
Both pre-fix-r2 constraints are restored on top of the judge and pinned as
refusal cases here.

#1087 fix-r3 (boundary #1) publication controls: the rows stay in memory
for the whole guarded window and one final ``summary.json`` is published by
a single rename only after ``mutating_sources`` restored and byte-verified
every guarded source. An abort inside the window — a cancellation, a restore
failure, the source guard — voids the recorded rows, publishes INVALID and
exits 3 (never 2: that is the "never started" usage refusal). Only the
process layer (cargo, slots, guard, source writes, sleep) is mocked; the
bookkeeping under test is the delivered code.

Run with:

    python3 -B edgeclaw-service-orchestration/test_negative.py
"""
import json
import os
import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

TEST = 'rust_matches_ts_oracle'
TEST_FILE = 'pilotdeck-gateway/tests/edgeclaw_service_orchestration.rs'


def _panic_log(test, payload, *, status='FAILED', thread=None, file=TEST_FILE,
               summary=True):
    counts = ('0 passed; 1 failed' if status == 'FAILED' else '1 passed; 0 failed')
    line = f'test result: {"FAILED" if status == "FAILED" else "ok"}. ' \
           f'{counts}; 0 ignored; 0 measured; 0 filtered out'
    return (
        '\nrunning 1 test\n'
        f'test {test} ... {status}\n'
        '\nfailures:\n'
        f"thread '{thread or test}' (2841) panicked at "
        f'{file}:12:5:\n'
        f'{payload}\n'
        'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
        + (line + '\n' if summary else '')
    )


def load_runner():
    entry = str(Path(__file__).resolve().parent)
    if entry not in sys.path:
        sys.path.insert(0, entry)
    import negative
    return negative


class ServiceOrchestrationJudgeControls(unittest.TestCase):
    """The orchestration kill judge must only accept the named test's anchor."""

    @classmethod
    def setUpClass(cls):
        cls.runner = load_runner()

    def mutant(self, anchor, test=TEST, test_file=TEST_FILE):
        return dict(name='sample', path=None, old='', new='',
                    test=test, anchor=anchor, test_file=test_file, suite='test')

    def result(self, log, code=101):
        return subprocess.CompletedProcess([], code, '', log)

    def judge(self, log, code, anchor, test=TEST, test_file=TEST_FILE):
        return self.runner.classify(self.result(log, code),
                                     self.mutant(anchor, test, test_file))

    def test_named_test_anchor_panic_kills(self):
        """The positive shape: the named test's own anchor panic."""
        anchor = '#776 scenario dream-stage-act: sqlite differs from the real TS'
        log = _panic_log(TEST, 'sqlite mismatch: ' + anchor)
        self.assertTrue(self.judge(log, 101, anchor)
                        == ('KILLED', 'bound test failed at its named assertion'),
                        "#1087 the named test's own anchor panic must kill")

    def test_captured_stdout_is_not_a_kill(self):
        """An anchor printed to captured stdout is not a kill."""
        anchor = '#776 scenario dream-stage-act: sqlite differs from the real TS'
        log = (
            '\nrunning 1 test\n'
            f'test {TEST} ... FAILED\n'
            '\nfailures:\n'
            f'---- {TEST} stdout ----\n'
            f'replay dump for debugging: {anchor}\n'
            f"thread '{TEST}' (2841) panicked at "
            f'{TEST_FILE}:44:9:\n'
            'called `Option::unwrap()` on a `None` value\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.judge(log, 101, anchor)[0] == 'INVALID',
                        '#1087 an anchor in captured stdout is not a kill')

    def test_sibling_panic_is_not_a_kill(self):
        """An anchor in a sibling test's panic is not a kill."""
        anchor = '#776 scenario dream-stage-act: sqlite differs from the real TS'
        sibling = 'flush_second_completes_when_first_never_does'
        log = (
            '\nrunning 2 tests\n'
            f'test {TEST} ... ok\n'
            f'test {sibling} ... FAILED\n'
            '\nfailures:\n'
            f"thread '{sibling}' (2841) panicked at "
            'pilotdeck-gateway/tests/edgeclaw_snapshots.rs:44:9:\n'
            f'flush count wrong: {anchor}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.judge(log, 101, anchor)[0] == 'INVALID',
                        "#1087 a sibling test's panic is not a kill")

    def test_build_output_anchor_is_not_a_kill(self):
        """An anchor inside build output is never a kill."""
        anchor = '#776 scenario dream-stage-act: sqlite differs from the real TS'
        log = (
            f'error[E0308]: mismatched types in `{anchor}`\n'
            f' --> {TEST_FILE}:12:5\n'
            'error: could not compile `pilotdeck-gateway` '
            '(test "edgeclaw_service_orchestration") due to 1 previous error\n'
        )
        self.assertTrue(self.judge(log, 101, anchor)[0] == 'INVALID',
                        '#1087 an anchor inside build output is never a kill')

    def test_missing_failed_summary_is_not_a_kill(self):
        """#1087 fix-r3 (boundary #3): an aborted run is not a kill.

        The transcript carries everything the strict judge needs (the bound
        test's own FAILED status line and the anchor inside its own panic
        block) but libtest never reached its summary.
        """
        anchor = '#776 scenario dream-stage-act: sqlite differs from the real TS'
        log = _panic_log(TEST, 'sqlite mismatch: ' + anchor, summary=False)
        self.assertTrue(self.judge(log, 101, anchor)[0] == 'INVALID',
                        '#1087 a run without the FAILED summary is not a kill')

    def test_panic_in_another_file_is_not_a_kill(self):
        """#1087 fix-r3 (boundary #3): the panic must sit in the mutant's file.

        The anchor is inside the bound test's own panic payload, but the
        panic location binds to a different file — the mutation and the kill
        are misattributed.
        """
        anchor = '#776 scenario dream-stage-act: sqlite differs from the real TS'
        log = _panic_log(
            TEST, 'sqlite mismatch: ' + anchor,
            file='pilotdeck-gateway/tests/edgeclaw_snapshots.rs')
        self.assertTrue(self.judge(log, 101, anchor)[0] == 'INVALID',
                        '#1087 a panic outside the mutant\'s bound file is not a kill')

    def test_green_run_survives(self):
        """A passing bound test is SURVIVED, never INVALID."""
        anchor = '#776 scenario dream-stage-act: sqlite differs from the real TS'
        log = _panic_log(TEST, 'no failure', status='ok')
        self.assertTrue(self.judge(log, 0, anchor) == ('SURVIVED', 'bound test still passed'),
                        '#1087 a green bound test must read SURVIVED')

    def test_every_mutant_has_a_bound_anchor(self):
        """Every mutant name must carry an anchor in the ANCHORS table."""
        missing = [mutant['name'] for mutant in self.runner.MUTANTS
                   if not mutant.get('anchor')]
        self.assertTrue(not missing,
                        f'#1087 every mutant must have a bound anchor: {missing}')


class PublicationControls(unittest.TestCase):
    """The orchestration runner must never keep a stale green summary."""

    ANCHOR = '#776 bound anchor for the twin'
    OLD = 'fn twin_original() {}\n'
    NEW = 'fn twin_mutated() {}\n'

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-orch-negctl-')
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        self.out = self.root / 'run'
        self.out.mkdir()
        self.runner = load_runner()
        self.source_a = self.root / 'twin-orchestration.rs'
        self.source_a.write_text('fn twin_original() {}\n')
        self.source_b = self.root / 'twin-dream-run.rs'
        self.source_b.write_text('unrelated dream run source\n')
        # main() refuses to run without a caller-owned build tree.
        self.saved_target = os.environ.get('CARGO_TARGET_DIR')
        os.environ['CARGO_TARGET_DIR'] = str(self.root / 'target')
        if self.saved_target is None:
            self.addCleanup(os.environ.pop, 'CARGO_TARGET_DIR', None)
        else:
            self.addCleanup(os.environ.__setitem__, 'CARGO_TARGET_DIR',
                            self.saved_target)
        self.sources = {
            self.source_a: self.source_a.read_bytes(),
            self.source_b: self.source_b.read_bytes(),
        }
        self.mutants = [
            dict(name='twin-a', path=self.source_a,
                 old=self.OLD, new=self.NEW, test='twin_a_matches_ts',
                 anchor=self.ANCHOR,
                 test_file='pilotdeck-gateway/tests/edgeclaw_service_orchestration.rs',
                 suite='test'),
            dict(name='twin-b', path=self.source_b,
                 old='unrelated dream run source', new='mutated dream run source',
                 test='twin_b_matches_ts', anchor=self.ANCHOR,
                 test_file='pilotdeck-gateway/tests/edgeclaw_dream_run.rs',
                 suite='test'),
        ]

    def _green_log(self, test):
        return (f'running 1 test\ntest {test} ... ok\n'
                'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; '
                '0 filtered out\n')

    def _kill_log(self, test, test_file):
        return _panic_log(test, f'oracle mismatch: {self.ANCHOR}', file=test_file)

    def _drive(self, *, fail_at=None, fail_with=None, restore_fails_at=None):
        """Run the real ``main`` with only the process layer mocked.

        ``fail_at`` is the ``run_owned`` call index (1-based, over the
        positive/negative/restored calls of every mutant in order) that
        raises ``fail_with``. ``restore_fails_at`` is the 1-based
        ``write_source_atomic`` call index that raises instead of recording.
        """
        state = {'runs': 0, 'writes': 0, 'summary_existed': False}
        per_test = {}
        # Local name is `twin_mutants`, not `mutants`: anchor_selfcheck's
        # exemption-signal scan reads any store of a table name as a
        # mutation-table declaration, and this file is exempt with no tables.
        out, sources, twin_mutants = self.out, self.sources, self.mutants

        def fake_run_owned(command, cwd, env, timeout):
            state['runs'] += 1
            if fail_at is not None and state['runs'] == fail_at:
                raise fail_with
            test = command[-2]
            seen = per_test.get(test, 0)
            per_test[test] = seen + 1
            mutant = next(m for m in twin_mutants if m['test'] == test)
            if seen == 1:
                log = self._kill_log(test, mutant['test_file'])
                return subprocess.CompletedProcess(command, 101, '', log)
            return subprocess.CompletedProcess(
                command, 0, '', self._green_log(test))

        class Guard:
            def __enter__(self):
                return dict(sources)

            def __exit__(self, *exc):
                return False

        def fake_mutating(root, paths, **kwargs):
            return Guard()

        def fake_slot(script, **kwargs):
            return types.SimpleNamespace(stdout='', stderr='')

        def fake_atomic(root, path, data, *, mode=None):
            state['writes'] += 1
            state['summary_existed'] = (state['summary_existed']
                                        or (out / 'summary.json').exists())
            if restore_fails_at is not None and state['writes'] == restore_fails_at:
                raise RuntimeError('twin restore failure')

        disk = {}

        class Model:
            real_write_text = Path.write_text
            real_replace = Path.replace

            def model_write_text(self, data, *args, **kwargs):
                if self.parent == out and self.name == 'summary.json':
                    disk['direct'] = data
                    return None
                if self.parent == out and self.name == 'summary.json.tmp':
                    disk['staged'] = data
                    return Model.real_write_text(self, data, *args, **kwargs)
                return Model.real_write_text(self, data, *args, **kwargs)

            def model_replace(self, destination, *args, **kwargs):
                if self.parent == out and self.name == 'summary.json.tmp':
                    disk['renames'] = disk.get('renames', 0) + 1
                    disk['summary'] = disk['staged']
                return Model.real_replace(self, destination, *args, **kwargs)

        source = Path(self.runner.__file__).resolve()
        argv = [str(source), str(out), 'twin-a', 'twin-b']
        with patch.multiple(
                self.runner,
                MUTANTS=self.mutants,
                mutating_sources=fake_mutating,
                run_owned=fake_run_owned,
                wait_for_slot=fake_slot,
                write_source_atomic=fake_atomic,
                time=types.SimpleNamespace(sleep=lambda seconds: None)), \
             patch.object(sys, 'argv', argv), \
             patch.object(Path, 'write_text', Model.model_write_text), \
             patch.object(Path, 'replace', Model.model_replace):
            try:
                code = self.runner.main()
            except BaseException as error:  # re-raised by the runner itself
                injected = fail_with if fail_at is not None else (
                    RuntimeError('twin restore failure'))
                if not isinstance(error, type(injected)) or (
                        isinstance(injected, SystemExit)
                        and not isinstance(error, SystemExit)):
                    raise
                code = error
        return code, state, disk, out

    def test_green_run_publishes_via_one_rename(self):
        """The final summary is one rename, never an in-place write."""
        code, state, disk, out = self._drive()
        message = '#1087 the final summary must be one rename, never an in-place write'
        self.assertTrue(code == 0, message + ' (the sample run must stay green)')
        self.assertTrue(disk.get('direct') is None,
                        message + ' (summary.json was written in place)')
        self.assertTrue(disk.get('renames') == 1,
                        message + ' (the summary was not published by exactly one rename)')
        payload = json.loads(disk['summary'])
        self.assertTrue([row['status'] for row in payload['controls']]
                        == ['KILLED', 'KILLED'],
                        message + ' (the final payload must keep every row)')

    def test_no_summary_is_readable_inside_the_guarded_window(self):
        code, state, disk, out = self._drive()
        message = '#1087 no summary may be readable before restore verification'
        self.assertTrue(code == 0, message + ' (the sample run must stay green)')
        self.assertTrue(state['summary_existed'] is False,
                        message + ' (a summary was readable inside the guarded window)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a temp file was left behind)')

    def test_keyboardinterrupt_voids_rows_and_propagates(self):
        """A mid-suite cancellation voids every row, then propagates."""
        code, state, disk, out = self._drive(
            fail_at=4, fail_with=KeyboardInterrupt('red run interrupted'))
        message = '#1087 a cancellation must void the rows and publish INVALID'
        self.assertIsInstance(code, KeyboardInterrupt,
                              message + ' (the cancellation must be re-raised)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue([row['status'] for row in payload['controls']]
                        == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')
        self.assertTrue(
            all('runner: KeyboardInterrupt' in row['reason']
                for row in payload['controls']),
            message + ' (the invalidation reason was not recorded)')
        self.assertTrue('voided' in payload['controls'][0]['reason'],
                        message + ' (the already-KILLED row must be voided too)')

    def test_systemexit_is_normalized_to_three(self):
        """An aborting SystemExit pairs with exit code 3, never 2 or the raw code."""
        code, state, disk, out = self._drive(fail_at=4, fail_with=SystemExit(130))
        message = '#1087 an INVALID summary must pair with exit code 3'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the SystemExit must propagate)')
        self.assertTrue(code.code == 3,
                        message + ' (the raw code must be normalized away from 2)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue([row['status'] for row in payload['controls']]
                        == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')

    def test_restore_failure_voids_rows_and_exits_three(self):
        """A guard failure voids the rows and exits 3."""
        code, state, disk, out = self._drive(restore_fails_at=2)
        message = '#1087 a guard failure must void the rows and exit 3'
        self.assertTrue(code == 3, message + ' (a guard failure must exit 3)')
        payload = json.loads((out / 'summary.json').read_text())
        self.assertTrue([row['status'] for row in payload['controls']]
                        == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')
        self.assertTrue(
            all('runner: RuntimeError' in row['reason']
                for row in payload['controls']),
            message + ' (the invalidation reason was not recorded)')


if __name__ == '__main__':
    unittest.main()

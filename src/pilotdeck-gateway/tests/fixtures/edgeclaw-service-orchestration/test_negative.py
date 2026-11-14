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
for the whole guarded window and one final per-run summary
(``summary.<token>.json``, #1087 fix-r9) is published by
a single rename only after ``mutating_sources`` restored and byte-verified
every guarded source. An abort inside the window — a cancellation, a restore
failure, the source guard — voids the recorded rows, publishes INVALID and
exits 3 (never 2: that is the "never started" usage refusal). Only the
process layer (cargo, slots, guard, source writes, sleep) is mocked; the
bookkeeping under test is the delivered code.

Run with:

    python3 -B edgeclaw-service-orchestration/test_negative.py
"""
import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import threading
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
        self.root = Path(self.tmp.name).resolve()  # claim_dir resolves; /var is a symlink on this host
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
            # #1087 fix-r9 (P1): the summary probe follows the per-run
            # summary shape — the fixed ``summary.json`` name is gone.
            state['summary_existed'] = (state['summary_existed']
                                        or bool(list(out.glob('summary.*.json'))))
            if restore_fails_at is not None and state['writes'] == restore_fails_at:
                raise RuntimeError('twin restore failure')

        disk = {}

        def is_summary_tmp(name):
            # #1087 fix-r5 (concurrency #3): the staging name is unique per
            # run; #1087 fix-r9 (P1): it prefixes the per-run summary name,
            # not the fixed ``summary.json.``.
            return (name.startswith('summary.')
                    and name.endswith('.tmp'))

        class Model:
            real_write_text = Path.write_text
            real_replace = Path.replace

            def model_write_text(self, data, *args, **kwargs):
                # #1087 fix-r9 (P1): the verdict is published under the
                # per-run name; a direct in-place write to any summary file
                # is the defect the one-rename rule exists to prevent.
                if (self.parent == out and self.name.startswith('summary.')
                        and self.name.endswith('.json')):
                    disk['direct'] = data
                    return None
                if self.parent == out and is_summary_tmp(self.name):
                    disk.setdefault('staged', {})[self.name] = data
                    return Model.real_write_text(self, data, *args, **kwargs)
                return Model.real_write_text(self, data, *args, **kwargs)

            def model_replace(self, destination, *args, **kwargs):
                if self.parent == out and is_summary_tmp(self.name):
                    disk['renames'] = disk.get('renames', 0) + 1
                    disk['summary'] = disk['staged'][self.name]
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
                        message + ' (a summary was written in place)')
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
        # #1087 fix-r9 (P1): the voided verdict lives at this run's per-run
        # summary name; the glob finds exactly that one file.
        summaries = list(out.glob('summary.*.json'))
        self.assertTrue(len(summaries) == 1,
                        message + ' (the aborted run must publish exactly one '
                        'summary)')
        payload = json.loads(summaries[0].read_text())
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
        summaries = list(out.glob('summary.*.json'))
        self.assertTrue(len(summaries) == 1,
                        message + ' (the aborted run must publish exactly one '
                        'summary)')
        payload = json.loads(summaries[0].read_text())
        self.assertTrue([row['status'] for row in payload['controls']]
                        == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')

    def test_restore_failure_voids_rows_and_exits_three(self):
        """A guard failure voids the rows and exits 3."""
        code, state, disk, out = self._drive(restore_fails_at=2)
        message = '#1087 a guard failure must void the rows and exit 3'
        self.assertTrue(code == 3, message + ' (a guard failure must exit 3)')
        summaries = list(out.glob('summary.*.json'))
        self.assertTrue(len(summaries) == 1,
                        message + ' (the aborted run must publish exactly one '
                        'summary)')
        payload = json.loads(summaries[0].read_text())
        self.assertTrue([row['status'] for row in payload['controls']]
                        == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')
        self.assertTrue(
            all('runner: RuntimeError' in row['reason']
                for row in payload['controls']),
            message + ' (the invalidation reason was not recorded)')

    def test_closed_stdout_does_not_lose_the_abort_summary(self):
        """#1087 fix-r4 (concurrency #1): a closed stdout must not eat the
        INVALID summary.

        A consumer that closed the pipe (`negative.py OUT a b | head -1`)
        turns record()'s announce print into a BrokenPipeError. The void
        handler appends the remaining rows without announcing and still
        publishes one INVALID summary, exiting 3.
        """
        class ClosedStdout:
            def write(self, data):
                raise BrokenPipeError('consumer closed the pipe')

            def flush(self):
                pass

        out = self.out
        sources = dict(self.sources)
        argv = [str(Path(self.runner.__file__).resolve()), str(out),
                'twin-a', 'twin-b']
        calls = {}

        def fake_run_owned(command, cwd, env, timeout):
            test = command[-2]
            mutant = next(m for m in self.mutants if m['test'] == test)
            seen = calls.get(test, 0)
            calls[test] = seen + 1
            if seen == 1:
                return subprocess.CompletedProcess(
                    command, 101, '',
                    self._kill_log(test, mutant['test_file']))
            return subprocess.CompletedProcess(
                command, 0, '', self._green_log(test))

        class Guard:
            def __enter__(self):
                return dict(sources)

            def __exit__(self, *exc):
                return False

        with patch.multiple(
                self.runner,
                MUTANTS=self.mutants,
                mutating_sources=lambda root, paths, **kw: Guard(),
                run_owned=fake_run_owned,
                wait_for_slot=lambda script, **kw: types.SimpleNamespace(
                    stdout='', stderr=''),
                write_source_atomic=lambda root, path, data, mode=None: None,
                time=types.SimpleNamespace(sleep=lambda seconds: None)), \
             patch.object(sys, 'argv', argv), \
             patch.object(sys, 'stdout', ClosedStdout()):
            try:
                code = self.runner.main()
            except BaseException as error:  # pragma: no cover - must not raise
                self.fail(f'#1087 a closed stdout must not raise past main: {error!r}')
        message = '#1087 the abort summary must survive a closed stdout'
        self.assertTrue(code == 3,
                        message + ' (the exit code must stay 3, not 1)')
        summaries = list(out.glob('summary.*.json'))
        self.assertTrue(len(summaries) == 1,
                        message + ' (the aborted run must publish exactly one '
                        'summary)')
        payload = json.loads(summaries[0].read_text())
        self.assertTrue(all(row['status'] == 'INVALID'
                            for row in payload['controls']),
                        message + ' (the rows must be voided to INVALID)')
        self.assertTrue(all('BrokenPipeError' in row['reason']
                            for row in payload['controls']),
                        message + ' (the abort reason must be recorded)')

    def test_closed_stderr_keeps_the_abort_exit_code(self):
        """#1087 fix-r4 (concurrency #2): a closed stderr must not replace
        the exit code.

        The summary is already published when the diagnostic print runs, so
        an unwritable stderr (a host that closed `2>&-`) must not turn the
        "aborted = 3" contract into an unhandled OSError exit 1.
        """
        class ClosedStderr:
            def write(self, data):
                raise OSError('stderr is closed')

            def flush(self):
                pass

        sources = dict(self.sources)

        class Guard:
            def __enter__(self):
                return dict(sources)

            def __exit__(self, *exc):
                return False

        out = self.out
        argv = [str(Path(self.runner.__file__).resolve()), str(out),
                'twin-a', 'twin-b']

        def fake_run_owned(command, cwd, env, timeout):
            raise SystemExit(130)

        with patch.multiple(
                self.runner,
                MUTANTS=self.mutants,
                mutating_sources=lambda root, paths, **kw: Guard(),
                run_owned=fake_run_owned,
                wait_for_slot=lambda script, **kw: types.SimpleNamespace(
                    stdout='', stderr=''),
                write_source_atomic=lambda root, path, data, mode=None: None,
                time=types.SimpleNamespace(sleep=lambda seconds: None)), \
             patch.object(sys, 'argv', argv), \
             patch.object(sys, 'stderr', ClosedStderr()):
            with self.assertRaises(SystemExit) as caught:
                self.runner.main()
        message = '#1087 a closed stderr must not replace the exit code'
        self.assertTrue(caught.exception.code == 3,
                        message + ' (the raw SystemExit(130) must still '
                        'normalize to 3)')
        summaries = list(out.glob('summary.*.json'))
        self.assertTrue(len(summaries) == 1,
                        message + ' (the aborted run must publish exactly one '
                        'summary)')
        payload = json.loads(summaries[0].read_text())
        self.assertTrue(all(row['status'] == 'INVALID'
                            for row in payload['controls']),
                        message + ' (the on-disk rows were not voided)')

    def test_a_refusal_in_a_reused_directory_never_touches_the_other_run(self):
        """#1087 fix-r9 (P1, review concurrency #1): a refused re-run leaves
        the other run's green verdict alone.

        The OUT_DIR is a plain reused mkdir, so a host re-run can land in
        the same path after a green run. With the per-run summary names the
        exit-2 "never started" refusal writes its own refused marker under
        its own token and the previous run's green summary stays
        byte-identical — run B can no longer destroy run A's verdict (the
        fix-r4 marker overwrote the shared fixed name, which is the exact
        defect this round closes). This replaces the fix-r4 twin that
        pinned the overwrite as the intended behavior.
        """
        # Run A: green, into this test's reused directory.
        code_a, _state_a, _disk_a, out = self._drive()
        message = '#1087 a refused re-run must not touch the other run\'s summary'
        self.assertTrue(code_a == 0,
                        message + ' (the first run into the directory must '
                        'stay green)')
        green = list(out.glob('summary.*.json'))
        self.assertTrue(len(green) == 1,
                        message + ' (the green run published exactly one '
                        f'summary; got {[p.name for p in green]})')
        green_payload = json.loads(green[0].read_text())
        self.assertTrue('controls' in green_payload,
                        message + ' (the green run\'s verdict must carry its '
                        'rows)')
        green_name = green[0].name
        green_bytes = green[0].read_bytes()
        # Run B: same directory, an unknown mutant — the exit-2 refusal.
        argv = [str(Path(self.runner.__file__).resolve()), str(out),
                'no-such-mutant']
        with patch.object(sys, 'argv', argv):
            code_b = self.runner.main()
        self.assertTrue(code_b == 2,
                        message + ' (the unknown-mutant refusal must stay '
                        'exit 2)')
        # The other run's green verdict is untouched — same name, same bytes.
        self.assertTrue(green[0].name == green_name,
                        message + ' (the green summary was renamed)')
        self.assertTrue(green[0].read_bytes() == green_bytes,
                        message + ' (the green summary was rewritten)')
        # This run's own refused marker exists, carries the reason, and no rows.
        marker = [path for path in out.glob('summary.*.json')
                  if path.name != green_name]
        self.assertTrue(len(marker) == 1,
                        message + ' (the refused run must leave its own marker)')
        payload = json.loads(marker[0].read_text())
        self.assertTrue('refused' in payload,
                        message + ' (the refused marker must carry the refusal)')
        self.assertTrue('unknown mutant' in payload['refused'],
                        message + ' (the refused marker must carry the reason)')
        self.assertTrue('controls' not in payload,
                        message + ' (the refused marker must carry no verdict '
                        'rows)')
        self.assertTrue(payload.get('runToken') != green_payload.get('runToken'),
                        message + ' (the marker must name a different run than '
                        'the green one)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (the refusal must not leave staging behind)')

    def test_refusal_leaves_a_fresh_directory_without_a_verdict(self):
        """A first-run refusal publishes no verdict: nothing was measured.

        #1087 fix-r9 (P1): the refusal now leaves its own refused marker
        under its per-run name (the record that this run refused), but no
        verdict rows may exist — "never started" is not a green summary.
        """
        out = self.out
        argv = [str(Path(self.runner.__file__).resolve()), str(out),
                'no-such-mutant']
        with patch.object(sys, 'argv', argv):
            code = self.runner.main()
        message = '#1087 a fresh refused directory must carry no verdict'
        self.assertTrue(code == 2, message + ' (the refusal must exit 2)')
        summaries = list(out.glob('summary.*.json'))
        # fix-r9 (review DS concurrency #2): an all() over an empty glob is
        # vacuously true — the marker's existence is itself the contract.
        self.assertTrue(len(summaries) == 1,
                        message + ' (the refusal must leave exactly its own '
                        'marker — an empty glob is not a refusal)')
        self.assertTrue(all('refused' in json.loads(path.read_text())
                            for path in summaries),
                        message + ' (a verdict summary was published for a '
                        'never-started run)')
        self.assertTrue(all('controls' not in json.loads(path.read_text())
                            for path in summaries),
                        message + ' (the refused marker must carry no verdict '
                        'rows)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a temp file was left behind)')

    def test_a_refusal_before_the_runners_mkdir_still_leaves_its_marker(self):
        """#1087 fix-r11 (review MM r10 boundary #1/#2, two lineages): a
        refusal before the runner's own ``mkdir`` still leaves its marker.

        ``main`` refuses unknown mutants before it creates the OUT_DIR, and
        the fix-r9 guard made that refusal leave nothing on disk while the
        adapter claimed "this run's refusal marker published". The refusal
        now creates the directory itself: exit 2, exactly one own marker,
        and the published claim is true.
        """
        out = self.root / 'not-created-yet'
        argv = [str(Path(self.runner.__file__).resolve()), str(out),
                'no-such-mutant']
        with patch.object(sys, 'argv', argv):
            code = self.runner.main()
        message = '#1087 a pre-mkdir refusal must still leave its marker'
        self.assertTrue(code == 2, message + ' (the refusal must exit 2)')
        summaries = list(out.glob('summary.*.json'))
        self.assertTrue(len(summaries) == 1,
                        message + ' (the refusal must create the directory '
                        'and leave exactly its own marker)')
        payload = json.loads(summaries[0].read_text())
        self.assertTrue('unknown mutant' in payload['refused'],
                        message + ' (the marker must carry the reason)')
        self.assertTrue('controls' not in payload,
                        message + ' (the refused marker must carry no '
                        'verdict rows)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a temp file was left behind)')

    def test_refusal_survives_a_failed_marker_write(self):
        """#1087 fix-r5 (boundary #3): a failing refusal-marker write must not
        turn the exit-2 refusal into a traceback (rc=1, "ran and was red").

        The other run's green summary keeps its bytes when the marker write
        fails, but the run still reports "never started". #1087 fix-r9 (P1):
        the stale green is a per-run-named file this run does not own, and
        the marker's own staging name is per-run prefixed.
        """
        out = self.out
        stale = json.dumps(
            {'controls': [{'name': 'twin-a', 'status': 'KILLED'}]}) + '\n'
        (out / 'summary.999.1.123.json').write_text(stale)
        argv = [str(Path(self.runner.__file__).resolve()), str(out),
                'no-such-mutant']
        real_write_text = Path.write_text

        def failing_write_text(self_path, data, *args, **kwargs):
            name = self_path.name
            if (self_path.parent == out and name.startswith('summary.')
                    and name.endswith('.tmp')):
                raise OSError('disk full while staging the refusal marker')
            return real_write_text(self_path, data, *args, **kwargs)

        with patch.object(sys, 'argv', argv), \
                patch.object(Path, 'write_text', failing_write_text):
            code = self.runner.main()
        message = '#1087 a failed marker write must not change the refusal code'
        self.assertTrue(code == 2,
                        message + ' (the refusal must still exit 2, not 1)')
        self.assertTrue((out / 'summary.999.1.123.json').read_text() == stale,
                        message + ' (the other run\'s green summary is unchanged '
                        'after a failed marker write)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a temp file was left behind)')

    def _write_pipe_child(self, out, mode):
        """A standalone driver that runs the delivered runner for a real pipe.

        Only the process layer is faked — and only on the real module, so
        ``unique_summary_tmp`` and ``detach_dead_streams`` stay the shipped
        ones. The child sleeps briefly so the parent's close always lands
        before the first announce print (measured race, see #1087 fix-r5).
        """
        fixtures = Path(self.runner.__file__).resolve().parents[1]
        runner = Path(self.runner.__file__).resolve()
        anchor = self.ANCHOR
        old, new = self.OLD, self.NEW
        test = 'twin_a_matches_ts'
        test_file = 'pilotdeck-gateway/tests/edgeclaw_service_orchestration.rs'
        source = self.source_a
        kill_log = (
            'running 1 test\n'
            f'test {test} ... FAILED\n'
            '\nfailures:\n'
            f"thread '{test}' (2841) panicked at {test_file}:12:5:\n"
            f'oracle mismatch: {anchor}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to '
            'display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; '
            '0 filtered out\n'
        )
        green_log = (f'running 1 test\ntest {test} ... ok\n'
                     'test result: ok. 1 passed; 0 failed; 0 ignored; '
                     '0 measured; 0 filtered out\n')
        driver = f'''\
import subprocess
import sys
import time
import types
from pathlib import Path

fixtures, runner, out, source = map(Path, sys.argv[1:5])
mode = sys.argv[5]
sys.path.insert(0, str(fixtures))
import negative_process as process


class Guard:
    def __init__(self, snapshots):
        self.snapshots = snapshots

    def __enter__(self):
        return dict(self.snapshots)

    def __exit__(self, *exc):
        return False


def fake_mutating(root, paths, **kwargs):
    return Guard({{path: path.read_bytes() for path in paths}})


def fake_slot(script, **kwargs):
    return types.SimpleNamespace(stdout='', stderr='')


def fake_atomic(root, path, data, mode=None):
    pass


seen = {{}}


def fake_run_owned(command, cwd, env, timeout):
    target = command[-2]
    count = seen.get(target, 0)
    seen[target] = count + 1
    log = {kill_log!r} if count == 1 else {green_log!r}
    return subprocess.CompletedProcess(command, 101 if count == 1 else 0,
                                       '', log)


process.mutating_sources = fake_mutating
process.wait_for_slot = fake_slot
process.write_source_atomic = fake_atomic
process.run_owned = fake_run_owned

namespace = {{'__name__': 'negative_pipe_child', '__file__': str(runner)}}
exec(compile(runner.read_text(), str(runner), 'exec'), namespace)
namespace['MUTANTS'] = [dict(
    name='sample', path=source, old={old!r}, new={new!r}, test={test!r},
    anchor={anchor!r}, test_file={test_file!r}, suite='test')]
namespace['ORCHESTRATION'] = source
namespace['DREAM_RUN'] = source
namespace['SNAPSHOTS'] = source
namespace['STORAGE'] = source
namespace['INDEXER'] = source
if mode == 'refuse':
    sys.argv = [str(runner), str(out), 'no-such-mutant']
else:
    sys.argv = [str(runner), str(out), 'sample']
time.sleep(0.3)
raise SystemExit(namespace['main']())
'''
        child = self.root / f'pipe-child-{mode}.py'
        child.write_text(driver)
        return child

    def test_real_broken_pipe_keeps_the_abort_exit_code(self):
        """#1087 fix-r5 (concurrency #2): the measured 120 defect, for real.

        A consumer that closes the pipe (`negative.py OUT a b | head -1`)
        turns the announce print into a BrokenPipeError. Without the detach
        the interpreter-shutdown flush rewrites the abort exit code to 120
        ("Exception ignored in: ..."), measured on this host. The shipped
        runner must keep 3 and still publish one INVALID summary.
        """
        out = self.root / 'pipe-run'
        out.mkdir()
        child = self._write_pipe_child(out, 'abort')
        process = subprocess.Popen(
            [sys.executable, '-I', str(child),
             str(Path(self.runner.__file__).resolve().parents[1]),
             str(Path(self.runner.__file__).resolve()), str(out),
             str(self.source_a), 'abort'],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        process.stdout.close()  # the read end is gone before the first print
        _stdout, stderr = process.communicate(timeout=120)
        message = '#1087 a broken stdout must keep the abort exit code 3'
        self.assertTrue(process.returncode == 3,
                        message + f' (got {process.returncode}, '
                        f'stderr={stderr!r})')
        self.assertTrue(b'Exception ignored' not in stderr,
                        message + ' (the shutdown flush still saw the dead '
                        'stream; the detach did not run)')
        self.assertTrue(b'voided' in stderr,
                        message + ' (the void diagnostic must still print)')
        payloads = [json.loads(path.read_text())
                    for path in out.glob('summary.*.json')]
        message = '#1087 a broken stdout must keep the abort exit code 3'
        self.assertTrue(len(payloads) == 1,
                        message + ' (the aborted run must publish exactly '
                        'one per-run summary)')
        payload = payloads[0]
        self.assertTrue(all(row['status'] == 'INVALID'
                            for row in payload['controls']),
                        message + ' (the rows must be voided to INVALID)')
        self.assertTrue(all('BrokenPipeError' in row['reason']
                            for row in payload['controls']),
                        message + ' (the abort reason must be recorded)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a temp file was left behind)')

    def test_real_closed_stderr_keeps_the_refusal_exit_code(self):
        """#1087 fix-r5 (boundary #1): a real ``2>&-`` refusal keeps exit 2.

        With fd 2 closed at exec time CPython sets ``sys.stderr = None`` and
        the old unguarded diagnostic write crashed with rc=1 instead of the
        "never started" code.
        """
        out = self.root / 'refuse-run'
        out.mkdir()
        child = self._write_pipe_child(out, 'refuse')
        fixtures = Path(self.runner.__file__).resolve().parents[1]
        result = subprocess.run(
            ['/bin/sh', '-c',
             f'exec {sys.executable} -I {child} {fixtures} '
             f'{Path(self.runner.__file__).resolve()} {out} '
             f'{self.source_a} refuse 2>&-'],
            capture_output=True, timeout=120)
        message = '#1087 a closed stderr must keep the refusal exit code 2'
        self.assertTrue(result.returncode == 2,
                        message + f' (got {result.returncode}, '
                        f'stdout={result.stdout!r})')

    def test_concurrent_runs_never_share_a_staging_file(self):
        """#1087 fix-r5 (concurrency #3): two runs on one directory.

        The staging name is unique per run (pid + serial), and since fix-r9
        the published name carries the run token too, so two concurrent
        runs on the same reused directory stage separately, each publishes
        by its own rename into its own per-run summary, and neither can
        truncate the other's temp — the fixed ``summary.json`` /
        ``summary.json.tmp`` pair was a shared mutable resource.
        """
        out = self.out
        sources = dict(self.sources)
        out_dir, out_arg = out, str(out)
        runner = self.runner
        twin_mutants = self.mutants
        disk = {'staged': {}, 'renames': 0}
        barrier = threading.Barrier(2, timeout=30)
        seen = threading.local()

        def is_summary_tmp(name):
            return (name.startswith('summary.')
                    and name.endswith('.tmp'))

        def fake_run_owned(command, cwd, env, timeout):
            # Per-thread call counters: positive (green) -> negative (kill)
            # -> restored (green) for every mutant of this run.
            counts = getattr(seen, 'counts', None)
            if counts is None:
                counts = seen.counts = {}
            test = command[-2]
            count = counts.get(test, 0)
            counts[test] = count + 1
            if count == 1:
                mutant = next(m for m in twin_mutants if m['test'] == test)
                log = self._kill_log(test, mutant['test_file'])
                return subprocess.CompletedProcess(command, 101, '', log)
            return subprocess.CompletedProcess(
                command, 0, '', self._green_log(test))

        class Guard:
            def __enter__(self):
                return dict(sources)

            def __exit__(self, *exc):
                return False

        real_write_text = Path.write_text
        real_replace = Path.replace

        def model_write_text(self, data, *args, **kwargs):
            if self.parent == out_dir and is_summary_tmp(self.name):
                first = self.name not in disk['staged']
                disk['staged'][self.name] = data
                if first:
                    # Hold both stagings open at once: the second run must
                    # still get its own name, not the first one's file.
                    barrier.wait()
                return real_write_text(self, data, *args, **kwargs)
            return real_write_text(self, data, *args, **kwargs)

        def model_replace(self, destination, *args, **kwargs):
            if self.parent == out_dir and is_summary_tmp(self.name):
                disk['renames'] += 1
            return real_replace(self, destination, *args, **kwargs)

        def run_once():
            argv = [str(Path(runner.__file__).resolve()), out_arg,
                    'twin-a', 'twin-b']
            with patch.multiple(
                    runner,
                    MUTANTS=twin_mutants,
                    mutating_sources=lambda root, paths, **kw: Guard(),
                    run_owned=fake_run_owned,
                    wait_for_slot=lambda script, **kw: types.SimpleNamespace(
                        stdout='', stderr=''),
                    write_source_atomic=lambda root, path, data, mode=None:
                        None,
                    time=types.SimpleNamespace(sleep=lambda seconds: None)), \
                    patch.object(sys, 'argv', argv), \
                    patch.object(Path, 'write_text', model_write_text), \
                    patch.object(Path, 'replace', model_replace), \
                    contextlib.redirect_stdout(io.StringIO()):
                return runner.main()

        with contextlib.redirect_stderr(io.StringIO()):
            first = threading.Thread(target=run_once)
            second = threading.Thread(target=run_once)
            first.start()
            second.start()
            first.join(timeout=60)
            second.join(timeout=60)
        message = '#1087 concurrent runs must not share a staging file'
        self.assertTrue(len(disk['staged']) == 2,
                        message + f' (two runs staged {len(disk["staged"])} '
                        'distinct temps; they shared one)')
        self.assertTrue(disk['renames'] == 2,
                        message + f' (each run must publish by its own rename, '
                        f'got {disk["renames"]})')
        summaries = sorted(out.glob('summary.*.json'),
                          key=lambda path: path.name)
        self.assertTrue(len(summaries) == 2,
                        message + f' (each run must publish its own per-run '
                        f'summary; got {len(summaries)})')
        for path in summaries:
            payload = json.loads(path.read_text())
            self.assertTrue(
                payload['counts'] == {'KILLED': 2, 'SURVIVED': 0, 'INVALID': 0},
                message + f' (the {path.name} summary must be a whole '
                'document)')
        self.assertTrue(len({path.name for path in summaries}) == 2
                        and all('.tmp' not in path.name
                                for path in summaries),
                        message + ' (the two summaries must be distinct '
                        'per-run names)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a temp file was left behind)')
        log_names = sorted(path.name for path in out.glob('*.log'))
        self.assertTrue(len(log_names) == 12,
                        message + f' (the label logs must not truncate each '
                        f'other; got {log_names}')
        # The shared run token is three components (pid.serial.time_ns), so
        # split it off the label before the .log suffix (#1087 fix-r8, S1-b).
        self.assertTrue(len(set(name.removesuffix('.log').rsplit('.', 3)[0]
                                for name in log_names)) == 6,
                        message + ' (the six labels keep their identity '
                        'across the two run tokens)')


if __name__ == '__main__':
    unittest.main()

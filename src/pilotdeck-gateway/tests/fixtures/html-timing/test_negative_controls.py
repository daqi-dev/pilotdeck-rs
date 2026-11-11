# anchor-selfcheck: unsupported unit tests of the timing runner's cancellation publish, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r1 controls for the timing runner's cancellation publication.

The delivered runner (``negative_controls.py``) keeps every row in memory for
the whole guarded window and publishes one final ``summary.json`` by a single
rename only after ``mutating_sources`` restored and byte-verified every mutated
source (#1087 fix-r1 / #1061). The controls below pin the contract:

* a green run publishes exactly once, by rename, never in place, and no
  summary is readable while the guarded window is open;
* a cancellation (KeyboardInterrupt / SystemExit) is re-raised after the void
  publish, and the SystemExit code is normalized to 2 (#1059 fix-r2);
* a guard failure voids the recorded rows and exits 1;
* every guarded-source write in the window is one atomic rename (#1903 rule).

Only the cargo timing runs and the source guard are mocked; the row
bookkeeping under test is the delivered code.

Run with:

    python3 -B html-timing/test_negative_controls.py
"""
import contextlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch


class TimingRunnerControls(unittest.TestCase):
    """The delivered timing runner must never keep a stale green summary."""

    ROW = ('src/fixture.rs', 'fn a() {}\n', 'fn a() { mutated }\n',
           ['--test', 'html_markdown', 't_one'],
           dict(all=('did not scale from',)))

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-timing-runner-')
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        self.source = self.root / 'src/fixture.rs'
        self.source.parent.mkdir(parents=True)
        self.source.write_text('fn a() {}\n')
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative_controls
        self.runner = negative_controls

    def _rows(self, names):
        return [(name, *self.ROW) for name in names]

    def _drive(self, *, names=('one', 'two'), fail_at=None, guard_error=None,
               disk=None):
        """Run the real ``main`` with only the guard and cargo mocked.

        ``fail_at`` is ``(call index, exception)`` in the per-row call order
        green/red/restored. ``disk`` turns on the read-tracking model over the
        real ``pathlib`` calls, keyed to the run's own output directory, so the
        only observable summary writers are ``write_summary``'s
        temp+rename channel and the pre-fix direct ``summary.json`` write.
        """
        out_arg = Path(tempfile.mkdtemp(prefix='out-', dir=self.root)) / 'run'
        out = out_arg.resolve()
        state = {'calls': 0, 'summary_existed': False}
        source = self.source

        def fake_timing(target, extra_env, timeout):
            state['calls'] += 1
            state['summary_existed'] = (state['summary_existed']
                                        or (out_arg / 'summary.json').exists())
            if fail_at is not None and state['calls'] == fail_at[0]:
                raise fail_at[1]
            if state['calls'] % 3 == 2:
                return subprocess.CompletedProcess(
                    [], 1, '',
                    'running 1 test\nt_one ... FAILED\n'
                    'did not scale from 1 MiB to 2 MiB #978\nfailures:\n')
            return subprocess.CompletedProcess([], 0, '',
                                              'running 1 test\nt_one ... ok\n')

        class Guard:
            def __enter__(self):
                return {source: source.read_bytes()}

            def __exit__(self, *exc):
                if guard_error is not None:
                    raise guard_error
                return False

        def mutating(root, paths, **kwargs):
            return Guard()

        def atomic(root_arg, path, data, *, mode=None):
            state.setdefault('writes', []).append((Path(path), bytes(data), mode))

        non_head = types.SimpleNamespace(
            gate=lambda root: ('head', 'abc123', 'abc123'),
            non_head_exit=lambda source_, sha, *, code=0, evidence=None: code)

        real_write_text = Path.write_text
        real_replace = Path.replace

        def model_write_text(self, data, *args, **kwargs):
            if self.parent == out and self.name == 'summary.json':
                disk['direct'] = data
                return None
            if self.parent == out and self.name == 'summary.json.tmp':
                disk['staged'] = data
                return None
            return real_write_text(self, data, *args, **kwargs)

        def model_replace(self, destination, *args, **kwargs):
            if self.parent == out and self.name == 'summary.json.tmp':
                disk['renames'] = disk.get('renames', 0) + 1
                disk['summary'] = disk['staged']
                return None
            return real_replace(self, destination, *args, **kwargs)

        names = tuple(names)
        with contextlib.ExitStack() as stack:
            stack.enter_context(patch.multiple(
                self.runner,
                root=self.root,
                mutations=self._rows(names),
                EXPECTED_NAMES=names,
                missing_target=lambda: False,
                refuse_unusable_target=lambda: None,
                non_head=non_head,
                run_timing_test=fake_timing,
                mutating_sources=mutating,
                write_source_atomic=atomic))
            stack.enter_context(patch.object(
                sys, 'argv',
                [str(Path(self.runner.__file__).resolve()), str(out_arg), *names]))
            if disk is not None:
                stack.enter_context(patch.object(Path, 'write_text',
                                                model_write_text))
                stack.enter_context(patch.object(Path, 'replace',
                                                model_replace))
            try:
                code = self.runner.main()
            except BaseException as error:
                # Only the injected error is observed; a real Ctrl-C (a
                # different exception object) must still abort the test. A
                # SystemExit is accepted by type because main() re-wraps it
                # as SystemExit(2) (#1059 fix-r2).
                injected = fail_at[1] if fail_at is not None else guard_error
                if error is not injected and not (
                        isinstance(injected, SystemExit)
                        and isinstance(error, SystemExit)):
                    raise
                code = error
        return code, out, state

    def test_green_run_publishes_via_one_rename(self):
        """The final summary is one rename, never an in-place write."""
        disk = {}
        code, out, _ = self._drive(disk=disk)
        message = '#1087 the final summary must be one rename, never an in-place write'
        self.assertTrue(code == 0, message + ' (the sample run must stay green)')
        self.assertTrue(disk.get('direct') is None,
                        message + ' (summary.json was written in place)')
        self.assertTrue(disk.get('renames') == 1,
                        message + ' (the summary was not published by exactly one rename)')
        self.assertTrue(isinstance(disk.get('summary'), str),
                        message + ' (no final payload was renamed into place)')
        rows = json.loads(disk['summary'])
        self.assertTrue([row['outcome'] for row in rows] == ['KILLED', 'KILLED'],
                        message + ' (the final payload must keep every row)')

    def test_green_run_leaves_no_summary_in_guarded_window(self):
        """No summary may be readable before restore verification."""
        code, out, state = self._drive()
        message = '#1087 no summary may be readable before restore verification'
        self.assertTrue(code == 0, message + ' (the sample run must stay green)')
        self.assertTrue(state['summary_existed'] is False,
                        message + ' (a summary was readable inside the guarded window)')
        summary = json.loads((out / 'summary.json').read_text())
        self.assertTrue([row['outcome'] for row in summary] == ['KILLED', 'KILLED'],
                        message + ' (the published rows must stay complete)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a temp file was left behind)')
        self.assertTrue(self.source.read_text() == 'fn a() {}\n',
                        message + ' (the guarded tree must end restored)')

    def test_keyboardinterrupt_voids_rows_and_propagates(self):
        """A mid-suite cancellation voids every row, then propagates."""
        code, out, _ = self._drive(
            fail_at=(5, KeyboardInterrupt('red run interrupted')))
        message = '#1087 a cancellation must void the rows and publish INVALID'
        self.assertIsInstance(code, KeyboardInterrupt,
                              message + ' (the cancellation must be re-raised)')
        path = out / 'summary.json'
        self.assertTrue(path.is_file(),
                        message + ' (no INVALID summary was published)')
        rows = json.loads(path.read_text())
        self.assertTrue([row['outcome'] for row in rows] == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')
        self.assertTrue(
            all('runner: KeyboardInterrupt' in row['reason'] for row in rows),
            message + ' (the invalidation reason was not recorded)')
        self.assertTrue('voided' in rows[0]['reason'],
                        message + ' (the already-KILLED row must be voided too)')

    def test_systemexit_is_normalized_to_two(self):
        """The INVALID publish must pair with exit code 2, never the raw code."""
        code, out, _ = self._drive(fail_at=(5, SystemExit(7)))
        message = '#1087 an INVALID summary must pair with exit code 2'
        self.assertIsInstance(code, SystemExit,
                              message + ' (the SystemExit must propagate)')
        self.assertTrue(code.code == 2,
                        message + ' (the raw code must be normalized)')
        rows = json.loads((out / 'summary.json').read_text())
        self.assertTrue([row['outcome'] for row in rows] == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')
        self.assertTrue(
            all('runner: SystemExit' in row['reason'] for row in rows),
            message + ' (the invalidation reason was not recorded)')

    def test_guard_failure_voids_rows_and_exits_one(self):
        """A guard failure after the last row must void it and exit 1."""
        code, out, _ = self._drive(
            guard_error=RuntimeError('restore verification failed'))
        message = '#1087 a guard failure must void the rows and exit 1'
        self.assertTrue(code == 1, message + ' (a guard failure must exit 1)')
        rows = json.loads((out / 'summary.json').read_text())
        self.assertTrue([row['outcome'] for row in rows] == ['INVALID', 'INVALID'],
                        message + ' (the on-disk rows were not voided)')
        self.assertTrue(
            all('runner: RuntimeError: restore verification failed' in row['reason']
                for row in rows),
            message + ' (the invalidation reason was not recorded)')

    def test_mutation_writes_are_atomic(self):
        """Every guarded-source write in the window is one atomic rename."""
        code, out, state = self._drive(names=('one',))
        message = '#1087 every guarded-source write must be one atomic rename'
        self.assertTrue(code == 0, message + ' (the sample run must stay green)')
        writes = state.get('writes') or []
        mutant_writes = [row for row in writes if row[1] == b'fn a() { mutated }\n']
        restore_writes = [row for row in writes if row[1] == b'fn a() {}\n']
        self.assertTrue(len(mutant_writes) == 1,
                        message + ' (the mutant must be applied through one rename)')
        self.assertTrue(len(restore_writes) == 1,
                        message + ' (the restore must go through one rename too)')
        mode = self.source.stat().st_mode & 0o7777
        self.assertTrue(all(row[0] == self.source and row[2] == mode
                            for row in writes),
                        message + ' (each write must keep the source path and mode)')
        self.assertTrue(self.source.read_text() == 'fn a() {}\n',
                        message + ' (the tree must end restored)')


if __name__ == '__main__':
    unittest.main()

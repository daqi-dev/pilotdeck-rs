# anchor-selfcheck: unsupported unit tests of the summary publication, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r1 controls for the spawn runner's summary publication.

The delivered runner (``negative_controls.py``) publishes the one final
``summary.json`` only after the last restore verification, and #1087 fix-r1
made that publish one atomic rename (``write_summary``): a truncating
``write_text`` killed mid-write leaves an empty or half-written summary. The
controls below pin both halves on a real run of ``main`` with only the cargo
runs and the source guard mocked:

* no summary is readable while any row still runs;
* the publish is exactly one rename over the temp+rename channel, never an
  in-place write, and leaves no temp file behind.

Run with:

    python3 -B mcp-hooks-spawn/test_negative_controls.py
"""
import contextlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch


class SpawnSummaryControls(unittest.TestCase):
    """The spawn runner must publish one intact summary by one rename."""

    ROW = ('src/fixture.rs', r'x = 0', 'x = 1', 't_one', 'anchor one')

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-spawn-runner-')
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        self.source = self.root / 'src/fixture.rs'
        self.source.parent.mkdir(parents=True)
        self.source.write_text('x = 0\n')
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative_controls
        self.runner = negative_controls

    def _drive(self, *, names=('one',), disk=None):
        """Run the real ``main`` with only the guard and cargo mocked.

        The fake cargo alternates the per-row negative run (the bound test
        fails at its anchor panic) with the post-restore run (green), so a
        row ends KILLED exactly like a real one. ``disk`` turns on the
        read-tracking model over the real ``pathlib`` calls, keyed to the
        run's own output directory, so the only observable summary writers
        are ``write_summary``'s temp+rename channel and the pre-fix direct
        ``summary.json`` write.
        """
        out_arg = Path(tempfile.mkdtemp(prefix='out-', dir=self.root)) / 'run'
        out = out_arg.resolve()
        state = {'calls': 0, 'summary_existed': False}
        source = self.source

        def fake_run_owned(command, **kwargs):
            state['calls'] += 1
            state['summary_existed'] = (state['summary_existed']
                                        or (out_arg / 'summary.json').exists())
            if state['calls'] % 2 == 1:
                return subprocess.CompletedProcess(
                    [], 1, '',
                    'running 1 test\ntest t_one ... FAILED\n'
                    "thread 't_one' (1234) panicked at src/fixture.rs:1:1:\n"
                    'anchor one\n'
                    'note: run with `RUST_BACKTRACE=1` environment variable to '
                    'display a backtrace\n'
                    'test result: FAILED. 0 passed; 1 failed; 0 ignored; '
                    '0 measured; 0 filtered out\n')
            return subprocess.CompletedProcess(
                [], 0, '',
                'running 1 test\ntest t_one ... ok\n'
                'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; '
                '0 filtered out\n')

        class Guard:
            def __enter__(self):
                return {source: b'x = 0\n'}

            def __exit__(self, *exc):
                # The real guard restores every byte on exit; the fake must
                # too, or the next row sees a stale pattern.
                source.write_bytes(b'x = 0\n')
                return False

        def mutating(root_arg, paths, **kwargs):
            return Guard()

        real_write_text = Path.write_text
        real_os_rename = os.rename

        def model_write_text(self, data, *args, **kwargs):
            if self.parent == out and self.name == 'summary.json':
                disk['direct'] = data
                return None
            if self.parent == out and self.name == 'summary.json.tmp':
                disk['staged'] = data
                return None
            return real_write_text(self, data, *args, **kwargs)

        def model_os_rename(src, dst, **kwargs):
            # The delivered publish renames via ``os.rename``; the model
            # intercepts exactly that channel, keyed to the run's own output
            # directory.
            src = Path(src)
            dst = Path(dst)
            if (src.parent == out and src.name == 'summary.json.tmp'
                    and dst.name == 'summary.json'):
                disk['renames'] = disk.get('renames', 0) + 1
                disk['summary'] = disk['staged']
                return None
            return real_os_rename(src, dst, **kwargs)

        with contextlib.ExitStack() as stack:
            stack.enter_context(patch.multiple(
                self.runner,
                root=self.root,
                mutants=[(name, *self.ROW) for name in names],
                wait_for_slot=lambda *a, **k: types.SimpleNamespace(
                    stdout='', stderr=''),
                run_owned=fake_run_owned,
                mutating_sources=mutating))
            stack.enter_context(patch.object(
                sys, 'argv',
                [str(Path(self.runner.__file__).resolve()), str(out_arg)]))
            if disk is not None:
                stack.enter_context(patch.object(Path, 'write_text',
                                                model_write_text))
                stack.enter_context(patch.object(os, 'rename',
                                                model_os_rename))
            code = self.runner.main()
        return code, out, state

    def test_summary_is_published_by_one_atomic_rename(self):
        """The one publish is a rename, never an in-place write."""
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
        self.assertTrue([row['status'] for row in rows] == ['KILLED'],
                        message + ' (the final payload must keep the row)')

    def test_no_summary_readable_while_rows_still_run(self):
        """No summary is readable before the last restore verification."""
        code, out, state = self._drive(names=('one', 'two'))
        message = '#1087 no summary may be readable before the last restore verification'
        self.assertTrue(code == 0, message + ' (the sample run must stay green)')
        self.assertTrue(state['summary_existed'] is False,
                        message + ' (a summary was readable while rows still ran)')
        summary = json.loads((out / 'summary.json').read_text())
        self.assertTrue([row['status'] for row in summary] == ['KILLED', 'KILLED'],
                        message + ' (the published rows must stay complete)')
        self.assertTrue(not list(out.glob('*.tmp')),
                        message + ' (a temp file was left behind)')
        self.assertTrue(self.source.read_text() == 'x = 0\n',
                        message + ' (the guarded tree must end restored)')


class SpawnJudgeControls(unittest.TestCase):
    """#1087 fix-r4 (sibling #2): the kill judge must only accept a completed
    run of the named test's own anchor panic."""

    @classmethod
    def setUpClass(cls):
        import importlib.util
        source = Path(__file__).with_name('negative_controls.py')
        spec = importlib.util.spec_from_file_location('spawn_negative', source)
        cls.runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.runner)

    TEST = 'start_spawn_failure_matrix'
    ANCHOR = 'failure clears cached attempt'
    FULL = 'mcp::hooks::tests::start_spawn_failure_matrix'

    @staticmethod
    def _result(log, returncode):
        return types.SimpleNamespace(returncode=returncode, stdout=log, stderr='')

    @classmethod
    def _panic_log(cls, payload, *, summary=None):
        if summary is None:
            summary = ('test result: FAILED. 0 passed; 1 failed; '
                       '0 ignored; 0 measured; 0 filtered out')
        return (
            '\nrunning 2 tests\n'
            f'test {cls.FULL} ... FAILED\n'
            'test mcp::hooks::tests::start_spawn_drop_matrix ... ok\n'
            '\nfailures:\n'
            f"thread '{cls.FULL}' (2841) panicked at "
            'src/pilotdeck-gateway/src/mcp/hooks.rs:44:9:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def test_named_test_anchor_panic_kills(self):
        """The positive shape: the named test's own anchor panic."""
        log = self._panic_log(f'cached attempt leaked: {self.ANCHOR}')
        self.assertEqual(
            self.runner.classify(self._result(log, 101), self.TEST, self.ANCHOR),
            ('KILLED', 'bound test failed at its assertion'),
            "#1087 the named test's own anchor panic must kill")

    def test_aborted_run_without_summary_is_not_a_kill(self):
        """A partial transcript is not a kill.

        The bound test already printed its FAILED line and its own payload
        (the ``--nocapture`` abort shape measured on a real libtest process
        killed by SIGKILL), but the run never reached libtest's own
        ``test result:`` summary. An aborted run is not a kill decision.
        """
        log = self._panic_log(f'cached attempt leaked: {self.ANCHOR}', summary='')
        self.assertEqual(
            self.runner.classify(self._result(log, 101), self.TEST, self.ANCHOR),
            ('INVALID', 'missing bound runtime failure (build error, '
             'wrong panic, or missing anchor)'),
            '#1087 an aborted run without the FAILED summary line is not a kill')

    def test_captured_stdout_is_not_a_kill(self):
        """An anchor printed to captured stdout is not a kill."""
        log = (
            '\nrunning 2 tests\n'
            f'test {self.FULL} ... FAILED\n'
            'test mcp::hooks::tests::start_spawn_drop_matrix ... ok\n'
            '\nfailures:\n'
            f'---- {self.FULL} stdout ----\n'
            f'replay dump for debugging: {self.ANCHOR}\n'
            f"thread '{self.FULL}' (2841) panicked at "
            'src/pilotdeck-gateway/src/mcp/hooks.rs:44:9:\n'
            'called `Option::unwrap()` on a `None` value\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertEqual(
            self.runner.classify(self._result(log, 101), self.TEST, self.ANCHOR),
            ('INVALID', 'missing bound runtime failure (build error, '
             'wrong panic, or missing anchor)'),
            '#1087 an anchor in captured stdout is not a kill')


if __name__ == '__main__':
    unittest.main()

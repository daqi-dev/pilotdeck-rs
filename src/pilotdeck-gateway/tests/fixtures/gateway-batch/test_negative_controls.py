# anchor-selfcheck: unsupported unit tests of the batch runner's cancellation publish, no product source mutations
#!/usr/bin/env python3
"""#1061 controls for the gateway-batch runner's cancellation publication.

The delivered runner (``negative_controls.py``) keeps every row in memory and
publishes one final ``summary.json`` only after ``mutating_sources`` restored
and re-verified every mutated byte. The controls below pin the four halves of
that contract, mirroring the #1059 lock-release shape:

* a sample run stays green and grounds the runner itself in the guarded set;
* a plain guard failure voids the recorded rows and publishes INVALID (exit 2);
* a cancellation (KeyboardInterrupt / SystemExit) is re-raised after the void
  publish, and the SystemExit code is normalized to 2 at a real process
  boundary (256/-256 truncate to a zero POSIX status, a string code exits 1);
* a real SIGINT raised mid void-publish leaves the pre-existing summary bytes
  untouched on every write channel — no all-KILLED file may become readable
  before restore verification (#1061).

Run with:

    python3 -B -m unittest -v gateway-batch/test_negative_controls.py
"""
import contextlib
import hashlib
import importlib.util
import json
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


class RunnerInvalidationControls(unittest.TestCase):
    """The delivered batch runner must never keep a stale green summary."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-gb-runner-')
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        self.source = self.root / 'mutant_source.py'
        self.source.write_text('x = 0\n')
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative_controls
        self.runner = negative_controls

    def _read_summary(self, out):
        """Read the final summary without an un-messaged failure point."""
        path = out / 'summary.json'
        if not path.is_file():
            return None
        try:
            summary = json.loads(path.read_text())
        except ValueError as error:
            self.fail(f'#1061 runner summary.json must be valid JSON: {error}')
        self.assertIsInstance(summary, list,
                              '#1061 the runner summary must stay a bare list')
        return summary

    def _run_owned(self):
        phase = {'n': 0}

        def run_owned(command, **kwargs):
            phase['n'] += 1
            step = (phase['n'] - 1) % 3
            if step == 0:
                return subprocess.CompletedProcess([], 0, '', 'Ran 1 test\n')
            if step == 1:
                return subprocess.CompletedProcess(
                    [], 1, '', 'FAIL: test_one\n#166 exclusive message\n')
            return subprocess.CompletedProcess([], 0, '', 'Ran 1 test\n')

        return run_owned

    def _drive(self, *, guard_error=None):
        """Run the real ``main`` with only the guard and subprocess mocked."""
        # A fresh (not yet existing) subdirectory: admission itself is the
        # untouched mkdir(exist_ok=False) refusal.
        out = Path(tempfile.mkdtemp(prefix='out-', dir=self.root)) / 'run'
        source = self.source
        captured = {'mutating_calls': 0}

        class Guard:
            def __enter__(self):
                return {source: b'x = 0\n'}

            def __exit__(self, *exc):
                if guard_error is not None:
                    raise guard_error
                return False

        def mutating(root, paths, **kwargs):
            captured['mutating_calls'] += 1
            captured['paths'] = list(paths)
            return Guard()

        with patch.multiple(self.runner,
                            MUTATIONS=[('one', source, 'x = 0', 'x = 1')],
                            CHECKS={'one': ('Safety.test_one',
                                            '#166 exclusive message')},
                            mutating_sources=mutating,
                            run_owned=self._run_owned()), \
                patch.object(sys, 'argv',
                             [str(Path(self.runner.__file__).resolve()), '--output',
                              str(out), '--only', 'one']):
            try:
                code = self.runner.main()
            except BaseException as error:
                # Only the injected cancellation is observed; a real Ctrl-C
                # (a different exception object) must still abort the test.
                # Every SystemExit is re-wrapped by main() as SystemExit(2)
                # (#1061 / #1059 fix-r2), so it is accepted by type, not
                # identity.
                if error is not guard_error and not (
                        isinstance(guard_error, SystemExit)
                        and isinstance(error, SystemExit)):
                    raise
                code = error
        return code, self._read_summary(out), captured.get('paths'), captured.get('mutating_calls')

    def test_sample_run_is_green_and_runner_grounded(self):
        """The sample run stays green and guards the runner itself."""
        code, summary, paths, mutating_calls = self._drive()
        self.assertTrue(code == 0, '#1061 the sample runner must finish green')
        self.assertTrue([row['status'] for row in summary] == ['KILLED'],
                        '#1061 the sample mutant must be killed')
        self.assertIn(self.runner.NEGATIVE, paths,
                      '#1061 the delivered runner must be protected for its own row')
        self.assertIn(self.runner.COMMON, paths,
                      '#1061 the shared verdict table must stay in the guard set')
        self.assertTrue(mutating_calls == 1,
                        '#1061 the guard must wrap the whole run exactly once')

    def test_final_guard_failure_rewrites_summary(self):
        """A guard/restoration failure after the last row must void it."""
        code, summary, _, _ = self._drive(
            guard_error=RuntimeError('restore failed'))
        self.assertTrue(code == 2, '#1061 a guard failure must exit 2')
        self.assertIsNotNone(
            summary, '#1061 a guard failure must publish an INVALID summary')
        self.assertTrue([row['status'] for row in summary] == ['INVALID'],
                        '#1061 the on-disk summary must not keep KILLED')
        self.assertIn('source guard', summary[0]['reason'],
                      '#1061 the invalidation reason must be recorded')

    def test_final_guard_interrupt_rewrites_summary(self):
        """A cancellation after the last row must void it, then propagate."""
        message = '#1061 a cancellation must void the rows and publish INVALID'
        for canceled in (KeyboardInterrupt('restore interrupted'),
                         SystemExit('restore exited'),
                         SystemExit(0), SystemExit(None)):
            with self.subTest(canceled=type(canceled).__name__):
                code, summary, _, _ = self._drive(guard_error=canceled)
                self.assertIs(type(code), type(canceled),
                              message + ' (the cancellation must be re-raised)')
                if isinstance(code, SystemExit):
                    # fix-r2: the INVALID summary is already on disk, so the
                    # exit is normalized regardless of the raw code.
                    self.assertTrue(
                        code.code == 2,
                        message + ' (an INVALID summary must pair with exit code 2)')
                self.assertIsNotNone(
                    summary, message + ' (no summary was published)')
                self.assertTrue([row['status'] for row in summary] == ['INVALID'],
                                message + ' (the on-disk rows were not voided)')
                self.assertIn('source guard', summary[0]['reason'],
                             message + ' (the reason was not recorded)')

    _EXIT_PROBE = '''
import importlib.util, json, sys

spec = importlib.util.spec_from_file_location('gateway_batch_exit_probe', sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

# Captured before main() rewrites sys.argv: the guard must inject this exact
# SystemExit, not whatever sys.argv holds once argparse has seen it.
code = json.loads(sys.argv[3])

class Guard:
    def __enter__(self):
        raise SystemExit(code)

    def __exit__(self, *exc):
        return False

module.mutating_sources = lambda *args, **kwargs: Guard()
sys.argv = [sys.argv[1], '--output', sys.argv[2], '--only', 'empty-aggregate-pass']
raise SystemExit(module.main())
'''

    def test_invalid_exit_normalizes_process_status(self):
        """The INVALID publish must exit 2, never the raw SystemExit code.

        A real child process loads the delivered runner from disk (no edit) and
        raises ``SystemExit(code)`` at the guard boundary, so the assertion
        observes the POSIX process status a coordinator would: 256/-256
        truncate to a zero status and a string code exits 1, which is why the
        raw code must never decide the exit (#1061 / #1059 fix-r2).
        """
        message = ('#1061 an INVALID summary must not pair with the raw '
                   'SystemExit process status')
        for code in (None, 0, 256, -256, 'msg'):
            with self.subTest(code=code):
                out = Path(tempfile.mkdtemp(prefix='exit-', dir=self.root)) / 'run'
                try:
                    result = subprocess.run(
                        [sys.executable, '-I', '-B', '-c', self._EXIT_PROBE,
                         str(self.runner.NEGATIVE), str(out), json.dumps(code)],
                        capture_output=True, text=True, timeout=120)
                except subprocess.TimeoutExpired as error:
                    self.fail(f'{message}: SystemExit({code!r}) probe timed out: {error}')
                self.assertEqual(
                    result.returncode, 2,
                    f'{message}: SystemExit({code!r}) exited {result.returncode}: '
                    f'{result.stdout}{result.stderr}')
                # The injected cancellation is the exercised path; a plain
                # Exception from the stubs also returns 2 and would make the
                # exit assertion vacuously green.
                self.assertIn('source guard: SystemExit', result.stdout,
                              f'{message}: SystemExit({code!r}) wrong path')
                try:
                    summary = json.loads((out / 'summary.json').read_text())
                except (OSError, ValueError) as error:
                    self.fail(f'{message}: SystemExit({code!r}) summary unreadable: {error}')
                self.assertIsInstance(summary, list,
                                      f'{message}: SystemExit({code!r}) bare-list shape')
                for row in summary:
                    self.assertIsInstance(row, dict,
                                          f'{message}: SystemExit({code!r}) row shape')
                self.assertTrue([row['status'] for row in summary] == ['INVALID'],
                                f'{message}: SystemExit({code!r}) on-disk rows')

    def test_interrupt_during_void_publish_leaves_no_killed_summary(self):
        """A SIGINT inside the void publish must not keep KILLED (#1061).

        The disk is a read-tracking model over the real ``pathlib`` calls: the
        only summary writers are ``write_summary``'s temp+rename channel and
        the pre-fix direct ``summary.json`` write, both keyed to the run's own
        output directory. The model starts with the pre-existing all-KILLED
        summary the #1061 shape left behind (an earlier record-era publish),
        and the SIGINT is a real Python signal raised from ``tmp.replace``
        while the INVALID payload is being published. Admission creates a
        fresh directory for real, so nothing else can write there first.
        """
        # Resolved once here: main() resolves argv the same way, and on macOS
        # the per-user tempdir is a /var -> /private/var symlink, so an
        # unresolved comparison would never match the runner's own paths.
        out_arg = (self.root / 'void-publish').resolve()
        stale = json.dumps(
            [{'mutation': 'one', 'status': 'KILLED',
              'reason': '#1061 stale row from an earlier record-era publish'}]) + '\n'
        disk = {'summary': stale, 'direct': stale}
        real_write_text = Path.write_text
        real_replace = Path.replace

        def model_write_text(self, data, *args, **kwargs):
            if self.parent == out_arg and self.name == 'summary.json':
                disk['direct'] = data
                return None
            if self.parent == out_arg and self.name == 'summary.json.tmp':
                disk['staged'] = data
                return None
            return real_write_text(self, data, *args, **kwargs)

        def model_replace(self, destination, *args, **kwargs):
            if self.parent == out_arg and self.name == 'summary.json.tmp':
                # The window: the INVALID temp is complete, and the SIGINT
                # lands before the rename.
                if any(row['status'] == 'INVALID' for row in json.loads(disk['staged'])):
                    signal.raise_signal(signal.SIGINT)
                disk['summary'] = disk['staged']
                return None
            return real_replace(self, destination, *args, **kwargs)

        source = self.source

        class Guard:
            def __enter__(self):
                return {source: b'x = 0\n'}

            def __exit__(self, *exc):
                raise RuntimeError('restore verification failed')

        def mutating(root, paths, **kwargs):
            return Guard()

        # raise_signal delivers KeyboardInterrupt only under the default
        # disposition; a batch parent can inherit SIG_IGN, so install it for
        # the duration of this test and restore whatever was there.
        previous = signal.signal(signal.SIGINT, signal.default_int_handler)
        self.addCleanup(signal.signal, signal.SIGINT, previous)
        with patch.multiple(self.runner,
                            MUTATIONS=[('one', source, 'x = 0', 'x = 1')],
                            CHECKS={'one': ('Safety.test_one',
                                            '#166 exclusive message')},
                            mutating_sources=mutating,
                            run_owned=self._run_owned()), \
                patch.object(Path, 'write_text', model_write_text), \
                patch.object(Path, 'replace', model_replace), \
                patch.object(sys, 'argv',
                             [str(Path(self.runner.__file__).resolve()), '--output',
                              str(out_arg), '--only', 'one']):
            try:
                code = self.runner.main()
            except BaseException as error:
                code = error
        message = ('#1061 no summary may be readable before restore verification')
        self.assertIsInstance(code, KeyboardInterrupt,
                             message + ' (the SIGINT must escape the runner)')
        # The rename channel (write_summary): the interrupted void publish
        # must leave the pre-existing bytes untouched — the only earlier
        # writer is a per-row publish that has no business existing.
        self.assertTrue(disk.get('summary') == stale,
                        message + ' (the rename channel was written before verification)')
        # The direct channel (the original #1061 per-row write_text): the
        # delivered runner never writes summary.json in place at all.
        self.assertTrue(disk.get('direct') == stale,
                        message + ' (the direct channel was written before verification)')

    def test_mutation_writes_are_atomic(self):
        """Every guarded-source write must be one atomic rename.

        A truncating ``write_text``/``write_bytes`` inside the mutation
        window leaves an empty or half-written source when the runner is
        killed mid-write; the runner is in its own guarded set, so a 0-byte
        runner would exit 0 without running any row and a half file could not
        even preflight (#1087 fix-r1; the #1903 rule shared by
        ``mutating_sources``).
        """
        message = '#1087 every guarded-source write must be one atomic rename'
        out = Path(tempfile.mkdtemp(prefix='atomic-', dir=self.root)) / 'run'
        source = self.source
        captured = []

        class Guard:
            def __enter__(self):
                return {source: b'x = 0\n'}

            def __exit__(self, *exc):
                return False

        def mutating(root, paths, **kwargs):
            return Guard()

        def atomic(root, path, data, *, mode=None):
            captured.append((path, bytes(data), mode))

        with patch.multiple(self.runner,
                            MUTATIONS=[('one', source, 'x = 0', 'x = 1')],
                            CHECKS={'one': ('Safety.test_one',
                                            '#166 exclusive message')},
                            mutating_sources=mutating,
                            run_owned=self._run_owned(),
                            write_source_atomic=atomic), \
                patch.object(sys, 'argv',
                             [str(Path(self.runner.__file__).resolve()), '--output',
                              str(out), '--only', 'one']):
            code = self.runner.main()
        self.assertTrue(code == 0, message + ' (the sample run must stay green)')
        mutant_writes = [row for row in captured if row[1] == b'x = 1\n']
        restore_writes = [row for row in captured if row[1] == b'x = 0\n']
        self.assertTrue(len(mutant_writes) == 1,
                        message + ' (the mutant must be applied through one rename)')
        self.assertTrue(len(restore_writes) >= 1,
                        message + ' (the restore must go through one rename too)')
        mode = source.stat().st_mode & 0o7777
        self.assertTrue(all(row[0] == source and row[2] == mode for row in captured),
                        message + ' (each write must keep the source path and mode)')
        self.assertTrue(source.read_text() == 'x = 0\n',
                        message + ' (the tree must end restored)')

    def test_existing_evidence_is_refused_byte_identical(self):
        """A pre-existing evidence directory is refused, never edited.

        Admission is the untouched ``mkdir(exist_ok=False)`` refusal: nothing
        is deleted or renamed before it, so a mistyped ``--output`` cannot
        destroy another run's result (#1061; admission behavior unchanged).
        """
        message = '#1061 a pre-existing evidence directory must be refused with every byte preserved'
        stale = json.dumps(
            [{'mutation': 'one', 'status': 'KILLED', 'reason': '#166 stale'}]) + '\n'
        for label, extra in (
                ('summary-only', {}),
                ('summary-and-evidence',
                 {'empty-aggregate-pass-positive.log': 'Ran 1 test\nOK\n'})):
            with self.subTest(case=label):
                folder = Path(tempfile.mkdtemp(prefix='reuse-', dir=self.root))
                (folder / 'summary.json').write_text(stale)
                for name, text in extra.items():
                    (folder / name).write_text(text)

                def snapshot():
                    return {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                            for path in folder.iterdir()}

                before = snapshot()

                def refuse(*_args, **_kwargs):
                    # Any admitted directory would start the run; fail loudly
                    # instead of touching this tree (#1061 message).
                    raise RuntimeError('#1061 mutation must not start')

                with patch.multiple(self.runner, mutating_sources=refuse), \
                        patch.object(sys, 'argv',
                                     [str(Path(self.runner.__file__).resolve()),
                                      '--output', str(folder),
                                      '--only', 'empty-aggregate-pass']):
                    try:
                        code = self.runner.main()
                    except BaseException as error:
                        code = error
                after = snapshot()
                self.assertTrue(after == before,
                                message + f' (the {label} directory was edited)')
                self.assertTrue(after.get('summary.json') == before['summary.json'],
                                message + f' (summary.json bytes changed, {label})')
                self.assertIsInstance(code, FileExistsError,
                                      message + f' (admission must refuse, {label})')


if __name__ == '__main__':
    unittest.main()

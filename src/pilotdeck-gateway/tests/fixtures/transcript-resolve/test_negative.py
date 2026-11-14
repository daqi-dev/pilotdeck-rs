# anchor-selfcheck: unsupported unit tests of the transcript-resolve kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r3 controls for the transcript-resolve kill judge.

The delivered judge (``negative_controls.py::classify``) is the shared strict
#813 criterion: the named test's own ``FAILED`` status line plus the bound
assertion's message inside that test's own panic payload, with a compile
failure explicitly excluded. The controls below pin the positive shape and
the false-kill shapes the old whole-log substring check
(``f'test {test} ... FAILED' in log`` plus ``assertion in log``) accepted --
a sibling test's panic, an assertion only in captured stdout, and an
assertion inside build output. Every log is a synthetic libtest transcript;
no cargo run happens here (the real-sample kill is proven by the
coordinator's full run).

Run with:

    python3 -B transcript-resolve/test_negative.py
"""
import importlib.util
import sys
from pathlib import Path
import unittest
from unittest.mock import patch


def load_runner():
    source = Path(__file__).with_name('negative_controls.py')
    spec = importlib.util.spec_from_file_location('transcript_resolve_negative',
                                                  source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)  # main() only runs under __main__
    return module


class TranscriptResolveJudgeControls(unittest.TestCase):
    """The transcript-resolve kill judge must only accept the named test's panic."""

    @classmethod
    def setUpClass(cls):
        cls.runner = load_runner()

    @staticmethod
    def _panic_log(test, payload, *, status='FAILED', thread=None):
        counts = ('0 passed; 1 failed' if status == 'FAILED' else '1 passed; 0 failed')
        summary = f'test result: {"FAILED" if status == "FAILED" else "ok"}. ' \
                  f'{counts}; 0 ignored; 0 measured; 0 filtered out'
        return (
            '\nrunning 1 test\n'
            f'test {test} ... {status}\n'
            '\nfailures:\n'
            f"thread '{thread or test}' (2841) panicked at "
            'tests/transcript_resolve.rs:12:5:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def judge(self, log, returncode, test, assertion):
        return self.runner.classify(returncode, log, test, assertion)

    TEST = 'relative_base_deleted_cwd'
    ASSERTION = 'transcript resolve matrix: relative-deleted'

    def test_named_test_assertion_panic_kills(self):
        """The positive shape, across the representative bound assertions."""
        # Named `samples`, not `cases`: anchor_selfcheck's exemption-signal
        # scan reads any store of a table name as a mutation-table
        # declaration, and this file is exempt with no tables.
        samples = (
            ('relative_base_deleted_cwd', 'transcript resolve matrix: relative-deleted'),
            ('relative_base_normal_cwd', 'transcript resolve matrix: join-relative-normal'),
            ('relative_base_deleted_cwd', 'list propagates cwd failure'),
            ('relative_base_deleted_cwd', 'rewind propagates cwd failure'),
        )
        for test, assertion in samples:
            with self.subTest(assertion=assertion):
                log = self._panic_log(test, f'{assertion} (bound assertion fired)')
                self.assertTrue(self.judge(log, 101, test, assertion),
                                "#1087 the named test's own assertion panic must kill")

    def test_captured_stdout_is_not_a_kill(self):
        """An assertion echoed by the test's own stdout is not a kill."""
        log = (
            '\nrunning 1 test\n'
            f'test {self.TEST} ... FAILED\n'
            '\nfailures:\n'
            f'---- {self.TEST} stdout ----\n'
            f'adapter dump for debugging: {self.ASSERTION}\n'
            f"thread '{self.TEST}' (2841) panicked at "
            'tests/transcript_resolve.rs:44:9:\n'
            'assertion `left == right` failed\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(not self.judge(log, 101, self.TEST, self.ASSERTION),
                        '#1087 an assertion in captured stdout is not a kill')

    def test_sibling_panic_is_not_a_kill(self):
        """An assertion in a sibling test's panic is not a kill."""
        sibling = 'absolute_base_deleted_cwd'
        log = (
            '\nrunning 2 tests\n'
            f'test {self.TEST} ... ok\n'
            f'test {sibling} ... FAILED\n'
            '\nfailures:\n'
            f"thread '{sibling}' (2841) panicked at "
            'tests/transcript_resolve.rs:44:9:\n'
            f'{self.ASSERTION}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(not self.judge(log, 101, self.TEST, self.ASSERTION),
                        "#1087 a sibling test's panic is not a kill")

    def test_build_output_assertion_is_not_a_kill(self):
        """An assertion inside build output is never a kill."""
        log = (
            f'error[E0308]: mismatched types at `{self.ASSERTION}`\n'
            ' --> tests/transcript_resolve.rs:12:5\n'
            'error: could not compile `pilotdeck-gateway` '
            '(test "transcript_resolve") due to 1 previous error\n'
        )
        self.assertTrue(not self.judge(log, 101, self.TEST, self.ASSERTION),
                        '#1087 an assertion inside build output is not a kill')

    def test_green_run_is_not_a_kill(self):
        """A passing bound test is never a kill."""
        log = self._panic_log(self.TEST, 'no failure', status='ok')
        self.assertTrue(not self.judge(log, 0, self.TEST, self.ASSERTION),
                        '#1087 a green bound test is not a kill')

class PublicationBehavior(unittest.TestCase):
    """#1087 fix-r5 (boundary #6): behavioral twins, not source literals.

    The old controls asserted substrings of the delivered source
    (``'--', '--exact'`` and the ``write_source_atomic`` call text). These
    controls execute the delivered ``main`` with only the process layer
    mocked and pin the behavior instead: every stage runs the one named
    test, the kill lands in the published summary, and the mutation publish
    goes through the shared single-rename helper while the guarded source
    is never written directly.
    """

    def setUp(self):
        import os
        import tempfile
        self._tempfile = tempfile.TemporaryDirectory(
            prefix='transcript-resolve-twin-')
        self.addCleanup(self._tempfile.cleanup)
        self.out = Path(self._tempfile.name).resolve() / 'run'
        self.runner = load_runner()
        # main() refuses to run without a caller-owned build tree.
        self.saved_target = os.environ.get('CARGO_TARGET_DIR')
        os.environ['CARGO_TARGET_DIR'] = str(self.out / 'target')
        if self.saved_target is None:
            self.addCleanup(os.environ.pop, 'CARGO_TARGET_DIR', None)
        else:
            self.addCleanup(os.environ.__setitem__, 'CARGO_TARGET_DIR',
                            self.saved_target)

    def _drive(self):
        import contextlib
        import subprocess
        import types
        state = {'seen': {}, 'atomic': [], 'direct_writes': []}
        runner, out = self.runner, self.out

        @contextlib.contextmanager
        def fake_mutating(root, paths, **kwargs):
            yield {path: path.read_bytes() for path in paths}

        def fake_slot(script, **kwargs):
            return types.SimpleNamespace(stdout='', stderr='')

        def fake_run_owned(command, cwd, env, timeout):
            # The command tail is ``<test> -- --exact``; the per-test call
            # order is positive (green) -> negative (kill) -> restored
            # (green), so the exact filter's effect is observable: the log
            # of every stage names exactly the one bound test.
            test = command[-3]
            count = state['seen'].get(test, 0)
            state['seen'][test] = count + 1
            if count == 1:
                payload = 'transcript resolve matrix: relative-deleted'
                log = (
                    'running 1 test\n'
                    f'test {test} ... FAILED\n'
                    '\nfailures:\n'
                    f"thread '{test}' (2841) panicked at "
                    'tests/transcript_resolve.rs:12:5:\n'
                    f'assertion `left == right` failed: {payload}\n'
                    'note: run with `RUST_BACKTRACE=1` environment variable '
                    'to display a backtrace\n'
                    'test result: FAILED. 0 passed; 1 failed; 0 ignored; '
                    '0 measured; 0 filtered out\n'
                )
                return subprocess.CompletedProcess(command, 101, log, '')
            log = (f'running 1 test\ntest {test} ... ok\n'
                   'test result: ok. 1 passed; 0 failed; 0 ignored; '
                   '0 measured; 0 filtered out\n')
            return subprocess.CompletedProcess(command, 0, log, '')

        def fake_atomic(root, path, data, *, mode=None):
            state['atomic'].append((path, bytes(data)))

        real_write_bytes = Path.write_bytes

        def model_write_bytes(self, data, *args, **kwargs):
            # The guarded product source may only be published through the
            # shared helper; a direct write_bytes is the truncating shape.
            state['direct_writes'].append(self)
            return real_write_bytes(self, data, *args, **kwargs)

        argv = [str(Path(runner.__file__).resolve()), str(out),
                'cwd-empty-fallback']
        with patch.object(runner, 'mutating_sources', fake_mutating), \
                patch.object(runner, 'wait_for_slot', fake_slot), \
                patch.object(runner, 'run_owned', fake_run_owned), \
                patch.object(runner, 'write_source_atomic', fake_atomic), \
                patch.object(sys, 'argv', argv), \
                patch.object(Path, 'write_bytes', model_write_bytes):
            runner.main()
        return state, out

    def test_every_stage_runs_the_named_test_behind_exact(self):
        """Behavioral ``--exact`` pin: one named test per process, always."""
        import json
        state, out = self._drive()
        message = '#1087 every cargo invocation must run the named test'
        self.assertTrue(list(state['seen']) == ['relative_base_deleted_cwd'],
                        message + f' (the runs used {list(state["seen"])})')
        self.assertTrue(state['seen']['relative_base_deleted_cwd'] == 3,
                        message + ' (positive/negative/restored all ran)')
        for label in ('cwd-empty-fallback-positive',
                      'cwd-empty-fallback-negative',
                      'cwd-empty-fallback-restored'):
            log = (out / f'{label}.log').read_text()
            self.assertTrue('running 1 test\n' in log,
                            message + f' ({label} did not run exactly one test)')
        summary = json.loads((out / 'summary.json').read_text())
        self.assertTrue(all(row['killed'] for row in summary),
                        message + ' (the killed rows must record the runs)')

    def test_mutation_publish_goes_through_the_shared_helper(self):
        """Behavioral rename pin: one atomic publish, never a direct write."""
        state, out = self._drive()
        message = '#1087 the mutation publish must be one shared-helper rename'
        self.assertTrue(len(state['atomic']) == 1,
                        message + f' (expected exactly one atomic publish, '
                        f'got {len(state["atomic"])})')
        path, data = state['atomic'][0]
        self.assertTrue(data != path.read_bytes(),
                        message + ' (the published bytes were not the mutant)')
        guarded = [p for p in state['direct_writes'] if p.suffix == '.rs']
        self.assertTrue(not guarded,
                        message + ' (the guarded source was written directly)')


if __name__ == '__main__':
    unittest.main()

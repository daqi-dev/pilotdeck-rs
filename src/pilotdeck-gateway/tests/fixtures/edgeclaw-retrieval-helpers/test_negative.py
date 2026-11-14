# anchor-selfcheck: unsupported unit tests of the replay kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r4 controls for the retrieval-helpers kill judge.

The delivered judge (``negative.py::assertion_failure``) is the shared #813
criterion plus the #1087 fix-r4 aborted-run guard: the bound test's own
``FAILED`` status line, the ``{fn} args=`` anchor inside that test's own panic
payload, and libtest's own ``test result: FAILED.`` summary line. The controls
below pin the positive shape and the false-kill shapes — a sibling test's
panic, an anchor only in captured stdout, and an aborted run that printed the
bound test's FAILED line and payload but never reached libtest's summary.

The runner is a script (its mutations run at module level on import), so the
twin extracts the delivered judge's own function bytes with ``ast`` instead of
importing the module. Every log is a synthetic libtest transcript; no cargo
run happens here (the real-sample kill is proven by the coordinator's full
run).

Run with:

    python3 -B edgeclaw-retrieval-helpers/test_negative.py
"""
import ast
import os
import sys
import tempfile
import types
from pathlib import Path
import unittest
from unittest.mock import patch

FIXTURES = Path(__file__).resolve().parents[1]
if str(FIXTURES) not in sys.path:
    sys.path.insert(0, str(FIXTURES))
import negative_process  # noqa: F401 - real module before the swap below
from negative_panic import named_test_killed


def load_judge():
    """Compile only ``assertion_failure`` out of the script-style runner."""
    source = Path(__file__).with_name('negative.py')
    tree = ast.parse(source.read_text())
    body = [node for node in tree.body
            if isinstance(node, ast.FunctionDef) and node.name == 'assertion_failure']
    if not body:
        raise AssertionError('assertion_failure is missing from negative.py')
    module = ast.Module(body=body, type_ignores=[])
    namespace = {'named_test_killed': named_test_killed}
    exec(compile(module, str(source), 'exec'), namespace)
    return namespace['assertion_failure']


class RetrievalHelpersJudgeControls(unittest.TestCase):
    """The retrieval-helpers kill judge must only accept a completed run of the
    named test's own anchor panic."""

    @classmethod
    def setUpClass(cls):
        cls.judge = staticmethod(load_judge())

    @staticmethod
    def _panic_log(test, payload, *, summary=None):
        if summary is None:
            summary = ('test result: FAILED. 0 passed; 1 failed; '
                       '0 ignored; 0 measured; 0 filtered out')
        return (
            '\nrunning 1 test\n'
            f'test {test} ... FAILED\n'
            '\nfailures:\n'
            f"thread '{test}' (2841) panicked at "
            'tests/edgeclaw_retrieval_helpers.rs:12:5:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def test_named_test_anchor_panic_kills(self):
        """The positive shape: the named test's own ``{fn} args=`` panic."""
        test = 'cjk_tokenization_matches_ts'
        anchor = 'expandCjkSearchToken args='
        log = self._panic_log(test, f'{anchor}["并发","检索"] mismatch after mutation')
        self.assertTrue(self.judge(log, test, anchor) is True,
                        "#1087 the named test's own anchor panic must kill")

    def test_aborted_run_without_summary_is_not_a_kill(self):
        """#1087 fix-r4 (sibling #2): a partial transcript is not a kill.

        The bound test already printed its FAILED line and its own payload
        (the ``--nocapture`` abort shape measured on a real libtest process
        killed by SIGKILL), but the run never reached libtest's own
        ``test result:`` summary. An aborted run is not a kill decision.
        """
        test = 'cjk_tokenization_matches_ts'
        anchor = 'expandCjkSearchToken args='
        log = self._panic_log(test, f'{anchor}["并发","检索"] mismatch after mutation',
                              summary='')
        self.assertTrue(self.judge(log, test, anchor) is False,
                        '#1087 an aborted run without the FAILED summary line is not a kill')

    def test_captured_stdout_is_not_a_kill(self):
        """An anchor printed to captured stdout is not a kill."""
        test = 'cjk_tokenization_matches_ts'
        anchor = 'expandCjkSearchToken args='
        log = (
            '\nrunning 1 test\n'
            f'test {test} ... FAILED\n'
            '\nfailures:\n'
            f'---- {test} stdout ----\n'
            f'replay dump for debugging: {anchor}["并发","检索"]\n'
            f"thread '{test}' (2841) panicked at "
            'tests/edgeclaw_retrieval_helpers.rs:44:9:\n'
            'called `Option::unwrap()` on a `None` value\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.judge(log, test, anchor) is False,
                        '#1087 an anchor in captured stdout is not a kill')

    def test_sibling_panic_is_not_a_kill(self):
        """An anchor in a sibling test's panic is not a kill."""
        test = 'cjk_tokenization_matches_ts'
        anchor = 'expandCjkSearchToken args='
        log = (
            '\nrunning 2 tests\n'
            f'test {test} ... ok\n'
            'test project_shortlist_matches_ts ... FAILED\n'
            '\nfailures:\n'
            "thread 'project_shortlist_matches_ts' (2841) panicked at "
            'tests/edgeclaw_retrieval_helpers.rs:44:9:\n'
            f'shortlist diverged: {anchor}["检索"]\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.judge(log, test, anchor) is False,
                        "#1087 a sibling test's panic is not a kill")


class RetrievalHelpersRefusalControls(unittest.TestCase):
    """#1087 fix-r7 batch 1 (P1b): the stale-anchor refusal must survive a dead stdout.

    ``1>&-`` or a closed stream makes the in-window STALE ANCHOR prints
    raise; the old unguarded prints turned the never-started exit-2 refusal
    into an unhandled OSError crash (rc=1, "ran and was red"). The delivered
    script is a top-level guarded run (no ``main``), so this control executes
    it a second time with only the process layer mocked — the guard
    snapshots bytes that contain none of the anchors, so the refusal fires
    before any cargo run.
    """

    def test_main_path_stale_refusal_survives_a_dead_stdout(self):
        class ClosedStdout:
            def write(self, data):
                raise OSError('stdout is closed')

            def flush(self):
                pass

        with tempfile.TemporaryDirectory(prefix='retrieval-helpers-refusal-') as tmp:
            out = Path(tmp).resolve() / 'run'
            source = Path(__file__).with_name('negative.py')
            text = source.read_text()

            def fake_mutating(root, paths, **kwargs):
                class Guard:
                    def __enter__(self):
                        return {path: b'fn unrelated() {}' for path in paths}

                    def __exit__(self, *exc):
                        return False

                return Guard()

            def fake_slot(script, **kwargs):
                return types.SimpleNamespace(stdout='', stderr='')

            fake = types.ModuleType('negative_process')
            fake.mutating_sources = fake_mutating
            fake.run_owned = lambda *args, **kwargs: None
            fake.wait_for_slot = fake_slot
            fake.write_source_atomic = lambda *args, **kwargs: None
            # #1087 fix-r7 b1-fix: the delivered refusal branch now calls the
            # shared detach helper; the fake keeps the shipped one.
            fake.detach_dead_streams = negative_process.detach_dead_streams
            # #1087 fix-r8 (S1-a/S1-b): the guarded prints and the per-run
            # token are shared originals; the fake keeps the shipped ones.
            fake.dead_stream_print = negative_process.dead_stream_print
            fake.new_run_token = negative_process.new_run_token
            saved_process = sys.modules.get('negative_process')
            saved_argv = sys.argv
            sys.modules['negative_process'] = fake
            sys.argv = [str(source), str(out)]
            try:
                with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(Path(tmp) / 'target'),
                                             'NEGATIVE_ONLY': '',
                                             'NEGATIVE_BASELINE': ''}), \
                     patch.object(sys, 'stdout', ClosedStdout()):
                    try:
                        exec(compile(text, str(source), 'exec'),
                             {'__name__': 'retrieval_helpers_refusal_twin',
                              '__file__': str(source)})
                    except BaseException as error:  # the script always exits
                        code = error
                    else:  # pragma: no cover - the script never falls through
                        code = None
            finally:
                sys.argv = saved_argv
                if saved_process is None:
                    sys.modules.pop('negative_process', None)
                else:
                    sys.modules['negative_process'] = saved_process
            message = '#1087 a dead stdout must keep the stale-anchor refusal at exit 2'
            self.assertIsInstance(code, SystemExit,
                                  message + ' (the refusal must exit through SystemExit)')
            self.assertTrue(code.code == 2,
                            message + ' (the refusal must stay exit 2, not crash rc=1)')
            # #1087 fix-r8 (S2-c): the report name is per-run
            # (``stale-anchors.<token>.log``).
            self.assertTrue(any(path.name.startswith('stale-anchors.') and path.name.endswith('.log')
                                for path in out.iterdir()),
                            message + ' (the refusal report must still reach the disk)')


if __name__ == '__main__':
    unittest.main()

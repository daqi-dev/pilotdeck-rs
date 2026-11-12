# anchor-selfcheck: unsupported unit tests of the replay kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r4 controls for the live-history kill judge.

The delivered judge (``negative.py::classify``) is the shared #813 criterion
plus the #1087 fix-r4 aborted-run guard: the bound test's own ``FAILED``
status line (resolved from the bare-suffix filter's module-qualified names),
the marker inside that test's own panic payload, and libtest's own
``test result: FAILED.`` summary line. The controls below pin the positive
shape and the false-kill shapes — a marker only in captured stdout and an
aborted run that printed the bound test's FAILED line and payload but never
reached libtest's summary.

The runner is a script (its mutations run at module level on import), so the
twin extracts the delivered judge's own function bytes with ``ast`` instead of
importing the module. Every log is a synthetic libtest transcript; no cargo
run happens here (the real-sample kill is proven by the coordinator's full
run).

Run with:

    python3 -B live-history/test_negative.py
"""
import ast
from pathlib import Path
import re
import sys
import types
import unittest

FIXTURES = Path(__file__).resolve().parents[1]
if str(FIXTURES) not in sys.path:
    sys.path.insert(0, str(FIXTURES))
from negative_panic import named_test_killed
from negative_suite import passed


def load_judge():
    """Compile only ``classify``/``full_names`` out of the script-style runner."""
    source = Path(__file__).with_name('negative.py')
    tree = ast.parse(source.read_text())
    wanted = {'classify', 'full_names'}
    body = [node for node in tree.body
            if isinstance(node, ast.FunctionDef) and node.name in wanted]
    found = {node.name for node in body}
    if found != wanted:
        raise AssertionError(f'judge functions missing from negative.py: {sorted(wanted - found)}')
    module = ast.Module(body=body, type_ignores=[])
    namespace = {'named_test_killed': named_test_killed, 'passed': passed, 're': re}
    exec(compile(module, str(source), 'exec'), namespace)
    return namespace['classify']


def run_result(log, returncode):
    """A duck-typed run result for ``classify``."""
    return types.SimpleNamespace(returncode=returncode, stdout=log, stderr='')


FULL = 'runtime::turn::live::tests::live_history_slot_matrix'


class LiveHistoryJudgeControls(unittest.TestCase):
    """The live-history kill judge must only accept a completed run of the
    named test's own marker panic."""

    @classmethod
    def setUpClass(cls):
        cls.classify = staticmethod(load_judge())

    TEST = 'live_history_slot_matrix'
    MARKER = 'history slot matrix must match real TS'

    @staticmethod
    def _panic_log(payload, *, summary=None):
        if summary is None:
            summary = ('test result: FAILED. 0 passed; 1 failed; '
                       '0 ignored; 0 measured; 0 filtered out')
        return (
            '\nrunning 4 tests\n'
            f'test {FULL} ... FAILED\n'
            'test runtime::turn::live::tests::live_dispose_matrix ... ok\n'
            'test runtime::turn::live::tests::live_cancel_matrix ... ok\n'
            'test runtime::turn::live::tests::live_reserve_matrix ... ok\n'
            '\nfailures:\n'
            f"thread '{FULL}' (2841) panicked at "
            'src/pilotdeck-gateway/src/runtime/turn/live.rs:44:9:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def test_named_test_marker_panic_kills(self):
        """The positive shape: the named test's own marker panic."""
        log = self._panic_log(f'live-history mismatch: {self.MARKER}')
        self.assertEqual(self.classify(run_result(log, 101), self.TEST, self.MARKER),
                         ('KILLED', 'bound test failed at its assertion'),
                         "#1087 the named test's own marker panic must kill")

    def test_aborted_run_without_summary_is_not_a_kill(self):
        """#1087 fix-r4 (sibling #2): a partial transcript is not a kill.

        The bound test already printed its FAILED line and its own payload
        (the ``--nocapture`` abort shape measured on a real libtest process
        killed by SIGKILL), but the run never reached libtest's own
        ``test result:`` summary. An aborted run is not a kill decision.
        """
        log = self._panic_log(f'live-history mismatch: {self.MARKER}', summary='')
        self.assertEqual(self.classify(run_result(log, 101), self.TEST, self.MARKER),
                         ('INVALID', 'missing bound runtime failure (build error, '
                          'wrong panic, or missing anchor)'),
                         '#1087 an aborted run without the FAILED summary line is not a kill')

    def test_captured_stdout_is_not_a_kill(self):
        """A marker printed to captured stdout is not a kill."""
        log = (
            '\nrunning 4 tests\n'
            f'test {FULL} ... FAILED\n'
            'test runtime::turn::live::tests::live_dispose_matrix ... ok\n'
            'test runtime::turn::live::tests::live_cancel_matrix ... ok\n'
            'test runtime::turn::live::tests::live_reserve_matrix ... ok\n'
            '\nfailures:\n'
            f'---- {FULL} stdout ----\n'
            f'replay dump for debugging: {self.MARKER}\n'
            f"thread '{FULL}' (2841) panicked at "
            'src/pilotdeck-gateway/src/runtime/turn/live.rs:44:9:\n'
            'called `Option::unwrap()` on a `None` value\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertEqual(self.classify(run_result(log, 101), self.TEST, self.MARKER),
                         ('INVALID', 'missing bound runtime failure (build error, '
                          'wrong panic, or missing anchor)'),
                         '#1087 a marker in captured stdout is not a kill')


if __name__ == '__main__':
    unittest.main()

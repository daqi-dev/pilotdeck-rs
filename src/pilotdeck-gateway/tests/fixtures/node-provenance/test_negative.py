# anchor-selfcheck: unsupported unit tests of the replay kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r4 controls for the node-provenance kill judge.

The delivered judge (``negative.py::classify``) is the shared #813 criterion
plus the #1087 fix-r4 aborted-run guard: the bound test's own ``FAILED``
status line, the marker inside that test's own panic payload, and libtest's
own ``test result: FAILED.`` summary line. The controls below pin the positive
shape and the false-kill shapes — a marker only in captured stdout and an
aborted run that printed the bound test's FAILED line and payload but never
reached libtest's summary. Every log is a synthetic libtest transcript; no
cargo run happens here (the real-sample kill is proven by the coordinator's
full run).

Run with:

    python3 -B node-provenance/test_negative.py
"""
import importlib.util
from pathlib import Path
import sys
import types
import unittest


def load_runner():
    source = Path(__file__).with_name('negative.py')
    spec = importlib.util.spec_from_file_location('node_provenance_negative', source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run_result(log, returncode):
    """A duck-typed run result for ``classify``."""
    return types.SimpleNamespace(returncode=returncode, stdout=log, stderr='')


class NodeProvenanceJudgeControls(unittest.TestCase):
    """The node-provenance kill judge must only accept a completed run of the
    named test's own marker panic."""

    @classmethod
    def setUpClass(cls):
        cls.runner = load_runner()

    TEST = 'model_cost::tests::model_cost_fixtures_regenerate_identically_from_ts'
    ANCHORS = ['#953 pinned Node runtime']

    def row(self):
        return {'runner': 'cargo', 'test': self.TEST, 'anchors': list(self.ANCHORS)}

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
            'tests/node_provenance.rs:12:5:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def test_named_test_marker_panic_kills(self):
        """The positive shape: the named test's own marker panic."""
        log = self._panic_log(self.TEST,
                              'fixture drifted under a foreign node: ' + self.ANCHORS[0])
        self.assertEqual(self.runner.classify(self.row(), run_result(log, 101)),
                         ('KILLED', 'bound test failed at its assertion'),
                         "#1087 the named test's own marker panic must kill")

    def test_aborted_run_without_summary_is_not_a_kill(self):
        """#1087 fix-r4 (sibling #2): a partial transcript is not a kill.

        The bound test already printed its FAILED line and its own payload
        (the ``--nocapture`` abort shape measured on a real libtest process
        killed by SIGKILL), but the run never reached libtest's own
        ``test result:`` summary. An aborted run is not a kill decision.
        """
        log = self._panic_log(self.TEST,
                              'fixture drifted under a foreign node: ' + self.ANCHORS[0],
                              summary='')
        self.assertEqual(self.runner.classify(self.row(), run_result(log, 101)),
                         ('INVALID', 'missing bound runtime failure (build error, '
                          'wrong panic, or missing test)'),
                         '#1087 an aborted run without the FAILED summary line is not a kill')

    def test_captured_stdout_is_not_a_kill(self):
        """A marker printed to captured stdout is not a kill."""
        log = (
            '\nrunning 1 test\n'
            f'test {self.TEST} ... FAILED\n'
            '\nfailures:\n'
            f'---- {self.TEST} stdout ----\n'
            f'replay dump for debugging: {self.ANCHORS[0]}\n'
            f"thread '{self.TEST}' (2841) panicked at "
            'tests/node_provenance.rs:44:9:\n'
            'called `Option::unwrap()` on a `None` value\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertEqual(self.runner.classify(self.row(), run_result(log, 101)),
                         ('INVALID', 'missing bound runtime failure (build error, '
                          'wrong panic, or missing test)'),
                         '#1087 a marker in captured stdout is not a kill')


if __name__ == '__main__':
    unittest.main()

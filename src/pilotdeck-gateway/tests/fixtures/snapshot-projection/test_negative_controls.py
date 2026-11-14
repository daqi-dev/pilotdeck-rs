# anchor-selfcheck: unsupported unit tests of the replay kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r4 controls for the snapshot-projection kill judge.

The delivered judge (``negative_controls.py::classify_bound_run`` — imported
from ``negative_panic`` since #1087 fix-r5 simplify #1, no local copy) is the
shared #813 criterion plus the #1087 fix-r4 aborted-run guard: the bound
test's own ``FAILED`` status line, the marker inside that test's own panic
payload, and libtest's own ``test result: FAILED.`` summary line. The controls
below pin the positive shape and the false-kill shapes — a marker only in
captured stdout, a sibling test's panic, and an aborted run that printed the
bound test's FAILED line and payload but never reached libtest's summary.
Every log is a synthetic libtest transcript; no cargo run happens here (the
real-sample kill is proven by the coordinator's full run).

Run with:

    python3 -B snapshot-projection/test_negative_controls.py
"""
import importlib.util
from pathlib import Path
import unittest


def load_runner():
    source = Path(__file__).with_name('negative_controls.py')
    spec = importlib.util.spec_from_file_location('snapshot_projection_negative', source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class SnapshotProjectionJudgeControls(unittest.TestCase):
    """The snapshot-projection kill judge must only accept a completed run of
    the named test's own marker panic."""

    @classmethod
    def setUpClass(cls):
        cls.runner = load_runner()

    TEST = "scenarios_match_ts_outcomes_and_trees"
    MARKER = "#322 workspace-snapshot corpus is pinned"

    @staticmethod
    def _panic_log(test, payload, *, summary=None):
        if summary is None:
            summary = ("test result: FAILED. 0 passed; 1 failed; "
                       "0 ignored; 0 measured; 0 filtered out")
        return (
            "\nrunning 1 test\n"
            f"test {test} ... FAILED\n"
            "\nfailures:\n"
            f"thread '{test}' (2841) panicked at "
            "tests/workspace_snapshot.rs:12:5:\n"
            f"{payload}\n"
            "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n"
            f"{summary}\n"
        )

    def judge(self, log, returncode=101):
        return self.runner.classify_bound_run(returncode, log, self.TEST, self.MARKER)

    def test_named_test_marker_panic_kills(self):
        """The positive shape: the named test's own marker panic."""
        log = self._panic_log(self.TEST,
                              f"scenario tree diverged: {self.MARKER}")
        self.assertEqual(self.judge(log),
                         ("KILLED", "named assertion failed"),
                         "#1087 the named test's own marker panic must kill")

    def test_aborted_run_without_summary_is_not_a_kill(self):
        """#1087 fix-r4 (sibling #2): a partial transcript is not a kill.

        The bound test already printed its FAILED line and its own payload
        (the ``--nocapture`` abort shape measured on a real libtest process
        killed by SIGKILL), but the run never reached libtest's own
        ``test result:`` summary. An aborted run is not a kill decision.
        """
        log = self._panic_log(self.TEST,
                              f"scenario tree diverged: {self.MARKER}",
                              summary="")
        self.assertEqual(self.judge(log),
                         ("INVALID", "no libtest FAILED summary line "
                          "(aborted run or harness error)"),
                         "#1087 an aborted run without the FAILED summary line is not a kill")

    def test_captured_stdout_is_not_a_kill(self):
        """A marker printed to captured stdout is not a kill."""
        log = (
            "\nrunning 1 test\n"
            f"test {self.TEST} ... FAILED\n"
            "\nfailures:\n"
            f"---- {self.TEST} stdout ----\n"
            f"replay dump for debugging: {self.MARKER}\n"
            f"thread '{self.TEST}' (2841) panicked at "
            "tests/workspace_snapshot.rs:44:9:\n"
            "called `Option::unwrap()` on a `None` value\n"
            "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n"
            "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n"
        )
        self.assertEqual(self.judge(log),
                         ("INVALID", "test failed outside the named assertion"),
                         "#1087 a marker in captured stdout is not a kill")

    def test_sibling_panic_is_not_a_kill(self):
        """A marker in a sibling test's panic is not a kill."""
        log = (
            "\nrunning 2 tests\n"
            f"test {self.TEST} ... ok\n"
            "test trees_match_ts_outcomes ... FAILED\n"
            "\nfailures:\n"
            "thread 'trees_match_ts_outcomes' (2841) panicked at "
            "tests/workspace_snapshot.rs:44:9:\n"
            f"tree diverged: {self.MARKER}\n"
            "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n"
            "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n"
        )
        self.assertEqual(self.judge(log),
                         ("INVALID", "test failed outside the named assertion"),
                         "#1087 a sibling test's panic is not a kill")

    def test_default_call_site_keeps_this_tables_survived_wording(self):
        """#1087 fix-r5 (simplify #1) must not loosen the table's own wording.

        The classifier moved to ``negative_panic`` whose default survived
        wording is memory-gateway's ("bound test stayed green"); this table's
        own "named test stayed green" must survive as the explicit call-site
        argument, and the shared classifier must honor it.
        """
        source = Path(__file__).with_name('negative_controls.py').read_text()
        self.assertIn('survived_reason="named test stayed green"', source,
                      "#1087 the default call site must keep this table's wording")
        status, reason = self.runner.classify_bound_run(
            0, "", self.TEST, self.MARKER,
            survived_reason="named test stayed green")
        self.assertEqual((status, reason), ("SURVIVED", "named test stayed green"),
                         "#1087 the shared classifier must honor the explicit wording")


if __name__ == "__main__":
    unittest.main()

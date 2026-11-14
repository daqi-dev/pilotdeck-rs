# anchor-selfcheck: unsupported unit tests of the replay kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r4 controls for the memory-gateway kill judge.

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

    python3 -B memory-gateway/test_negative_controls.py
"""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import unittest


def load_runner():
    source = Path(__file__).with_name('negative_controls.py')
    spec = importlib.util.spec_from_file_location('memory_gateway_negative', source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class MemoryGatewayJudgeControls(unittest.TestCase):
    """The memory-gateway kill judge must only accept a completed run of the
    named test's own marker panic."""

    @classmethod
    def setUpClass(cls):
        cls.runner = load_runner()

    TEST = "gateway_replays_real_ts_wrapper_oracle"
    MARKER = "#1214 memory_list must forward a requires_host boundary to the Node host"

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
            "tests/memory_gateway.rs:12:5:\n"
            f"{payload}\n"
            "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n"
            f"{summary}\n"
        )

    def judge(self, log, returncode=101):
        return self.runner.classify_bound_run(returncode, log, self.TEST, self.MARKER)

    def test_named_test_marker_panic_kills(self):
        """The positive shape: the named test's own marker panic."""
        log = self._panic_log(self.TEST,
                              f"replay diverged from the oracle: {self.MARKER}")
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
                              f"replay diverged from the oracle: {self.MARKER}",
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
            "tests/memory_gateway.rs:44:9:\n"
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
            "test memory_wipe_replays_real_ts_wrapper_oracle ... FAILED\n"
            "\nfailures:\n"
            "thread 'memory_wipe_replays_real_ts_wrapper_oracle' (2841) panicked at "
            "tests/memory_gateway.rs:44:9:\n"
            f"wipe diverged: {self.MARKER}\n"
            "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n"
            "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n"
        )
        self.assertEqual(self.judge(log),
                         ("INVALID", "test failed outside the named assertion"),
                         "#1087 a sibling test's panic is not a kill")


class RefusalProcessControls(unittest.TestCase):
    """#3094 item 1 (#3178): the never-started refusal under a dead stderr.

    The process-level twin the rc=2 class lacked at family level: the
    CARGO_TARGET_DIR refusal runs before the window and before the evidence
    directory is claimed, so a real subprocess with stderr closed at exec
    time (``2>&-`` makes CPython set ``sys.stderr = None``) must keep exit 2
    — the reason may not be silently re-routed to stdout while the host
    watches stderr, and the interpreter must not record an "Exception
    ignored" shutdown flush.
    """

    def test_missing_target_refusal_survives_a_dead_stderr(self):
        runner = Path(__file__).with_name("negative_controls.py").resolve()
        env = {name: value for name, value in os.environ.items()
               if name != "CARGO_TARGET_DIR"}
        result = subprocess.run(
            ["/bin/sh", "-c",
             f"exec {sys.executable} -I {runner} /nonexistent-out 2>&-"],
            capture_output=True, timeout=120, env=env)
        message = "#3094 the never-started refusal must keep exit 2 under 2>&-"
        self.assertEqual(result.returncode, 2,
                         message + f" (got rc={result.returncode}, "
                         f"stderr={result.stderr!r})")
        self.assertEqual(result.stdout, b"",
                         message + " (a dead stderr must not re-route the "
                         "refusal reason to stdout)")
        self.assertNotIn(b"Exception ignored", result.stderr,
                         message + " (the shutdown flush must not see the dead stderr)")


class RosterMismatchStreamControls(unittest.TestCase):
    """#3178 fix-r1 (boundary #4): the roster-mismatch reason is a mid-run
    refusal print — a dead stderr must not re-route it to stdout.

    The never-started refusals in this file already go through
    ``dead_stream_print``; the mid-run roster mismatch (a shrunken run, the
    #1456 guard) was the one surviving bare ``print(file=sys.stderr)``.
    Under ``2>&-`` CPython sets ``sys.stderr = None`` and ``print(file=None)``
    falls back to *stdout* — measured in the fix-r1 probe: the reason leaves
    via the stream the host is not watching. The twin drives the delivered
    ``main()`` in-process to a roster mismatch with ``sys.stderr = None`` and
    asserts the reason never reaches stdout. No cargo, no product source
    writes.
    """

    def test_mismatch_reason_is_not_rerouted_under_a_dead_stderr(self):
        import contextlib
        import io
        import tempfile
        from unittest.mock import patch
        runner = load_runner()
        with tempfile.TemporaryDirectory(prefix="pdrs-memory-roster-") as temp:
            out = Path(temp) / "run"
            out.mkdir()

            class Guard:
                def __enter__(self):
                    return {}

                def __exit__(self, *exc):
                    return False

            captured = io.StringIO()
            with contextlib.redirect_stdout(captured), \
                    patch.object(sys, "stderr", None), \
                    patch.dict(os.environ,
                               {"CARGO_TARGET_DIR": str(Path(temp) / "target")}), \
                    patch.object(sys, "argv",
                                 ["negative_controls.py", str(out)]), \
                    patch.object(runner, "claim_dir", lambda path: Path(path)), \
                    patch.object(runner, "mutating_sources",
                                 lambda root, paths, **kwargs: Guard()), \
                    patch.object(
                        runner, "run_bound_test",
                        # A failing baseline keeps the mutation loop from
                        # touching any source: the only rows recorded are the
                        # baseline diagnostics, and the verdict roster still
                        # mismatches the (intact) EXPECTED_NAMES.
                        lambda test: subprocess.CompletedProcess([], 1, "", "")), \
                    patch.object(runner, "registered", lambda *args, **kwargs: None), \
                    patch.object(runner, "mutations", []):
                code = runner.main()
            recorded = (out / "roster-mismatch.json").is_file()
        message = ("#3178 the roster-mismatch reason must not be re-routed to "
                  "stdout under a dead stderr (print(file=None) falls back to "
                  "the other stream)")
        self.assertEqual(code, 1, message)
        self.assertTrue(recorded,
                        message + " (the mismatch must still be recorded on disk)")
        self.assertNotIn("roster mismatch", captured.getvalue(), message)


if __name__ == "__main__":
    unittest.main()

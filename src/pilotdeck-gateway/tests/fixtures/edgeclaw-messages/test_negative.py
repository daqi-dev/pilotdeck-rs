# anchor-selfcheck: unsupported unit tests of the replay kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r1 controls for the messages replay kill judge.

The delivered judge (``negative.py::assertion_failure``) is the shared #813
criterion: the bound test's own ``FAILED`` status line plus one of the oracle's
``{group}/`` anchors inside that test's own panic payload (the replay helper
formats its message that way; the ``mentions-scale`` watchdog line is that
test's declared kill line). The controls below pin the positive shapes and the
false-kill shapes the old log-substring judge accepted — a sibling test's
panic, an anchor only in captured stdout, a bare ``assert_eq!`` line without
the group anchor, and an anchor inside build output. Every log is a synthetic
libtest transcript; no cargo run happens here (the real-sample kill is proven
by the coordinator's full run).

Run with:

    python3 -B edgeclaw-messages/test_negative.py
"""
import os
import subprocess
import sys
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch


class MessagesJudgeControls(unittest.TestCase):
    """The messages kill judge must only accept the named test's own anchor."""

    def setUp(self):
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative
        self.runner = negative

    @staticmethod
    def _panic_log(test, payload, *, status='FAILED', summary=None):
        """One synthetic libtest transcript around a single named test."""
        if summary is None:
            counts = ('0 passed; 1 failed' if status == 'FAILED'
                      else '1 passed; 0 failed')
            summary = f'test result: {"FAILED" if status == "FAILED" else "ok"}. ' \
                      f'{counts}; 0 ignored; 0 measured; 0 filtered out'
        return (
            '\nrunning 1 test\n'
            f'test {test} ... {status}\n'
            '\nfailures:\n'
            f"thread '{test}' (2841) panicked at "
            'tests/edgeclaw_messages.rs:12:5:\n'
            f'{payload}\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            f'{summary}\n'
        )

    def test_named_test_anchor_panic_kills(self):
        """The positive shape: the named test's own anchor panic."""
        test = 'extraction_matches_ts'
        log = self._panic_log(test, 'extract/ extractContent case=2: expected 2, got 3')
        self.assertTrue(self.runner.assertion_failure(log, test) is True,
                        "#1087 the named test's own anchor panic must kill")

    def test_mentions_scale_watchdog_kills(self):
        """The mentions-scale watchdog line stays the declared kill line."""
        test = 'leading_mentions_scale_is_linear'
        log = self._panic_log(
            test, 'mentions-scale/1-4 MiB input did not finish within the 60 s watchdog')
        self.assertTrue(self.runner.assertion_failure(log, test) is True,
                        '#1087 the mentions-scale watchdog line stays the declared kill line')

    def test_sibling_panic_is_not_a_kill(self):
        """An anchor in a sibling test's panic is not a kill."""
        test = 'extraction_matches_ts'
        log = (
            '\nrunning 2 tests\n'
            f'test {test} ... ok\n'
            'test plugin_matches_ts ... FAILED\n'
            '\nfailures:\n'
            "thread 'plugin_matches_ts' (2841) panicked at "
            'tests/edgeclaw_messages.rs:44:9:\n'
            'plugin/ isPluginLine case=1: expected true, got false\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.runner.assertion_failure(log, test) is False,
                        "#1087 a sibling test's panic is not a kill")

    def test_captured_stdout_is_not_a_kill(self):
        """An anchor printed to captured stdout is not a kill."""
        test = 'extraction_matches_ts'
        log = (
            '\nrunning 1 test\n'
            f'test {test} ... FAILED\n'
            '\nfailures:\n'
            f'---- {test} stdout ----\n'
            'extract/ replay dump for debugging\n'
            f"thread '{test}' (2841) panicked at "
            'tests/edgeclaw_messages.rs:44:9:\n'
            'called `Option::unwrap()` on a `None` value\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
        )
        self.assertTrue(self.runner.assertion_failure(log, test) is False,
                        '#1087 an anchor in captured stdout is not a kill')

    def test_bare_assert_eq_without_anchor_is_not_a_kill(self):
        """A bare ``assert_eq!`` line without the group anchor is not a kill."""
        test = 'extraction_matches_ts'
        log = self._panic_log(test, 'assertion `left == right` failed\n  left: 2\n right: 3')
        self.assertTrue(self.runner.assertion_failure(log, test) is False,
                        '#1087 a bare assert_eq! line without the group anchor is not a kill')

    def test_build_output_anchor_is_not_a_kill(self):
        """An anchor inside build output is never a kill."""
        test = 'extraction_matches_ts'
        log = (
            'error[E0308]: mismatched types in `extract/ extractContent`\n'
            ' --> tests/edgeclaw_messages.rs:12:5\n'
            'error: could not compile `pilotdeck-gateway` '
            '(test "edgeclaw_messages") due to 1 previous error\n'
        )
        self.assertTrue(self.runner.assertion_failure(log, test) is False,
                        '#1087 an anchor inside build output is never a kill')


class MessagesRefusalControls(unittest.TestCase):
    """#1087 fix-r7 batch 1 (P1b): the stale-anchor refusal must survive a dead stdout.

    ``1>&-`` or a closed stream makes the in-window STALE ANCHOR prints
    raise; the old unguarded prints turned the never-started exit-2 refusal
    into an unhandled OSError crash (rc=1, "ran and was red"). No cargo run
    happens here — the process layer is patched on the imported module.
    """

    def setUp(self):
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative
        self.runner = negative
        self.tmp = tempfile.TemporaryDirectory(prefix='messages-refusal-')
        self.addCleanup(self.tmp.cleanup)
        self.out = Path(self.tmp.name).resolve() / 'run'

    def test_main_path_stale_refusal_survives_a_dead_stdout(self):
        """A closed stdout keeps the in-window refusal at exit 2."""
        class ClosedStdout:
            def write(self, data):
                raise OSError('stdout is closed')

            def flush(self):
                pass

        def fake_mutating(root, paths, **kwargs):
            class Guard:
                def __enter__(self):
                    return {path: b'fn unrelated() {}' for path in paths}

                def __exit__(self, *exc):
                    return False

            return Guard()

        runner = self.runner
        with patch.dict(os.environ, {'CARGO_TARGET_DIR': self.tmp.name,
                                     'NEGATIVE_ONLY': '', 'NEGATIVE_BASELINE': ''}), \
             patch.object(runner, 'mutating_sources', fake_mutating), \
             patch.object(sys, 'stdout', ClosedStdout()):
            # The messages refusal is a plain `return 2` out of main, so the
            # twin asserts the returned contract code, not a SystemExit.
            code = runner.main([str(Path(__file__).with_name('negative.py')), str(self.out)])
        message = '#1087 a dead stdout must keep the stale-anchor refusal at exit 2'
        self.assertTrue(code == 2,
                        message + ' (the refusal must stay exit 2, not crash rc=1)')
        # #1087 fix-r8 (S2-c): the report name is per-run
        # (``stale-anchors.<token>.log``).
        self.assertTrue(any(path.name.startswith('stale-anchors.') and path.name.endswith('.log')
                            for path in self.out.iterdir()),
                        message + ' (the refusal report must still reach the disk)')


REFUSAL_CHILD = '''\
import os
import sys
import types
from pathlib import Path

fixtures, runner, out = (Path(argument) for argument in sys.argv[1:4])
sys.path.insert(0, str(fixtures))
import negative_process


class Guard:
    def __init__(self, paths):
        self.paths = paths

    def __enter__(self):
        return {path: b'fn unrelated() {}' for path in self.paths}

    def __exit__(self, *exc):
        return False


fake = types.ModuleType('negative_process')
fake.mutating_sources = lambda root, paths, **kwargs: Guard(paths)
fake.write_source_atomic = lambda *args, **kwargs: None
fake.run_owned = lambda *args, **kwargs: None
fake.wait_for_slot = lambda *args, **kwargs: None
# The guarded print, the detach and the token are the shipped ones — the
# twin must exercise the real helpers, only the process layer is faked.
fake.dead_stream_print = negative_process.dead_stream_print
fake.detach_dead_streams = negative_process.detach_dead_streams
fake.new_run_token = negative_process.new_run_token
sys.modules['negative_process'] = fake
sys.argv = [str(runner), str(out)]
os.environ.setdefault('CARGO_TARGET_DIR', str(out.parent / 'target'))
namespace = {'__name__': 'messages_refusal_child', '__file__': str(runner)}
exec(compile(runner.read_text(), str(runner), 'exec'), namespace)
raise SystemExit(namespace['main']())
'''


class MessagesRefusalProcessControls(unittest.TestCase):
    """#3094 item 1 (#3178): a real process, a dead stdout, the exit-2 refusal.

    The in-process twin above patches the stream objects inside this
    interpreter; this one runs the delivered runner in a real child process
    with stdout closed at exec time (``1>&-`` makes CPython set
    ``sys.stdout = None``), so the interpreter-shutdown flush is the real
    one. The faked window returns a snapshot whose bytes match no anchor, so
    the runner refuses before any cargo run: the stale-anchor refusal must
    keep exit 2, still write its report to the evidence directory, and
    neither re-route the reason to stderr nor record an "Exception ignored"
    flush.
    """

    def test_stale_anchor_refusal_survives_a_dead_stdout(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-msg-refusal-') as temp:
            out = Path(temp) / 'run'
            child = Path(temp) / 'child.py'
            child.write_text(REFUSAL_CHILD)
            result = subprocess.run(
                ['/bin/sh', '-c',
                 f'exec {sys.executable} -I {child} '
                 f'{Path(__file__).resolve().parents[1]} '
                 f'{Path(__file__).with_name("negative.py")} {out} 1>&-'],
                capture_output=True, timeout=120)
            report = [path.name for path in out.iterdir()
                      if path.name.startswith('stale-anchors.')]
        message = '#3094 the stale-anchor refusal must keep exit 2 under 1>&-'
        self.assertEqual(result.returncode, 2,
                         message + f' (got rc={result.returncode}, '
                         f'stderr={result.stderr!r})')
        self.assertNotIn(b'Exception ignored', result.stderr,
                         message + ' (the shutdown flush must not see the dead stdout)')
        self.assertNotIn(b'STALE ANCHORS', result.stderr,
                         message + ' (a dead stdout must not re-route the '
                         'refusal reason to stderr)')
        self.assertTrue(len(report) == 1,
                        message + ' (the refusal report must still reach the disk)')


if __name__ == '__main__':
    unittest.main()

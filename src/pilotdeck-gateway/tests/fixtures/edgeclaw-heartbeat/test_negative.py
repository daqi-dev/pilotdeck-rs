# anchor-selfcheck: unsupported unit tests of the heartbeat kill judge, no product source mutations
#!/usr/bin/env python3
"""#1087 fix-r7 batch 1 (P1b) refusal control for the heartbeat table.

The ``ANCHOR EXPIRED`` refusal (exit 2) printed to stdout unguarded; a
closed stdout (``1>&-``) used to turn the refusal into an unhandled
OSError crash (rc=1, "ran and was red"). The twin executes the delivered
script once with only the process layer mocked (no cargo run, no real
source write): the guard snapshots unrelated bytes for the product
sources, so the in-window anchor check raises ``AnchorExpired`` on the
first row and the refusal path runs to its exit.

#3178 fix-r5 (DS sibling#3): the empty-table twin below pins the family
tail verdict — an emptied cases table (plus the shared extension) used to
finish the run and fall through the handwritten ``if survived: sys.exit(1)``
green (rc=0); the migrated ``negative_selection.verdict_exit`` must read
the zero-row run as red.

Run with:

    python3 -B edgeclaw-heartbeat/test_negative.py
"""
import contextlib
import io
import os
import subprocess
import sys
import tempfile
import types
from pathlib import Path
import unittest
from unittest.mock import patch

_FIXTURES = Path(__file__).resolve().parents[1]
if str(_FIXTURES) not in sys.path:
    sys.path.insert(0, str(_FIXTURES))
_HERE = Path(__file__).resolve().parent
if str(_HERE) not in sys.path:
    sys.path.insert(0, str(_HERE))
import negative_process  # noqa: F401 - real module before the swap below
import negative_panic  # noqa: F401 - import before the read_text patch
import negative_shared  # noqa: F401 - heartbeat imports it from its own directory


class AnchorExpiredRefusalControls(unittest.TestCase):
    """The ANCHOR EXPIRED refusal must survive a dead stdout."""

    def test_anchor_expired_refusal_survives_a_dead_stdout(self):
        """#1087 fix-r7 batch 1 (P1b): a dead stdout keeps the refusal at exit 2.

        ``1>&-`` or a closed stream makes the ANCHOR EXPIRED print raise;
        the old unguarded print turned the exit-2 refusal into a crash
        (rc=1, "ran and was red") inside the restore path.
        """
        class ClosedStdout:
            def write(self, data):
                raise OSError('stdout is closed')

            def flush(self):
                pass

        with tempfile.TemporaryDirectory(prefix='heartbeat-refusal-') as tmp:
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

            def fake_run_owned(command, cwd, env, timeout):
                log = ('running 1 test\n'
                       'test result: ok. 1 passed; 0 failed; 0 ignored; '
                       '0 measured; 0 filtered out\n')
                return subprocess.CompletedProcess(command, 0, '', log)

            def fake_slot(script, **kwargs):
                return types.SimpleNamespace(stdout='', stderr='')

            fake = types.ModuleType('negative_process')
            fake.mutating_sources = fake_mutating
            fake.run_owned = fake_run_owned
            fake.wait_for_slot = fake_slot
            fake.write_source_atomic = lambda *args, **kwargs: None
            # #1087 fix-r7 b1-fix: the delivered refusal branch now calls the
            # shared detach helper; the fake keeps the shipped one.
            fake.detach_dead_streams = negative_process.detach_dead_streams
            # #1087 fix-r8 (S1-a/S1-b): the token and the guarded print are
            # shared originals too — the fake keeps the shipped ones so the
            # twin exercises the real helpers.
            fake.new_run_token = negative_process.new_run_token
            fake.dead_stream_print = negative_process.dead_stream_print
            saved_process = sys.modules.get('negative_process')
            saved_argv = sys.argv
            sys.modules['negative_process'] = fake
            sys.argv = [str(source), str(out)]
            try:
                with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(Path(tmp) / 'target'),
                                              'NEGATIVE_ONLY': '',
                                              'NEGATIVE_BASELINE': ''}), \
                     patch.object(Path, 'read_text', return_value='fn unrelated() {}'), \
                     patch.object(sys, 'stdout', ClosedStdout()):
                    try:
                        exec(compile(text, str(source), 'exec'),
                             {'__name__': 'heartbeat_refusal_twin',
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
            message = '#1087 a dead stdout must keep the anchor-expired refusal at exit 2'
            self.assertIsInstance(code, SystemExit,
                                  message + ' (the refusal must exit through SystemExit)')
            self.assertTrue(code.code == 2,
                            message + ' (the refusal must stay exit 2, not crash rc=1)')


class EmptyTableVerdictControls(unittest.TestCase):
    """#3178 fix-r5 (DS sibling#3): an emptied table must not exit green."""

    def test_an_emptied_table_exits_red_through_the_shared_verdict(self):
        """An emptied cases table must exit 1, never the rc=0 vacuum green.

        Before fix-r5 the handwritten ``if survived: sys.exit(1)`` tail
        read a zero-row run as all-KILLED (rc=0); the dream family
        migrated to ``negative_selection.verdict_exit`` and this twin
        pins the same contract here: no rows is not a green verdict. The
        process layer is the same fake as the refusal twin above (no
        cargo run, no real source write), with the restore-verify reads
        served from the fake snapshot.
        """
        recorded = {}

        def fake_mutating(root, paths, **kwargs):
            recorded.update({path: b'fn unrelated() {}' for path in paths})

            class Guard:
                def __enter__(self):
                    return dict(recorded)

                def __exit__(self, *exc):
                    return False

            return Guard()

        def fake_run_owned(command, cwd, env, timeout):
            log = ('running 1 test\n'
                   'test result: ok. 1 passed; 0 failed; 0 ignored; '
                   '0 measured; 0 filtered out\n')
            return subprocess.CompletedProcess(command, 0, '', log)

        def fake_slot(script, **kwargs):
            return types.SimpleNamespace(stdout='', stderr='')

        real_read_bytes = Path.read_bytes

        def fake_read_bytes(path):
            if path in recorded:
                return recorded[path]
            return real_read_bytes(path)

        source = Path(__file__).with_name('negative.py')
        text = source.read_text()
        for before, after in (
                ('cases = [', 'cases = []\n_twin_emptied_cases = ['),
                # rows also extend from negative_shared's shared_mutations;
                # both tables must be emptied or the run still has real rows.
                ('for name, path, before, after, test, anchor in shared_mutations)',
                 'for name, path, before, after, test, anchor in ())')):
            count = text.count(before)
            self.assertEqual(count, 1,
                             f'{source}: anchor {before!r} occurs {count} times')
            text = text.replace(before, after, 1)
        with tempfile.TemporaryDirectory(prefix='heartbeat-empty-') as tmp:
            out = Path(tmp).resolve() / 'run'
            fake = types.ModuleType('negative_process')
            fake.mutating_sources = fake_mutating
            fake.run_owned = fake_run_owned
            fake.wait_for_slot = fake_slot
            fake.write_source_atomic = lambda *args, **kwargs: None
            # The guarded print and the run token stay the shipped ones, the
            # same as the refusal twin above; verdict_exit itself comes from
            # the real negative_selection import inside the delivered script.
            fake.dead_stream_print = negative_process.dead_stream_print
            fake.new_run_token = negative_process.new_run_token
            saved_process = sys.modules.get('negative_process')
            saved_argv = sys.argv
            sys.modules['negative_process'] = fake
            sys.argv = [str(source), str(out)]
            output = io.StringIO()
            try:
                with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(Path(tmp) / 'target'),
                                             'NEGATIVE_ONLY': '',
                                             'NEGATIVE_BASELINE': ''}), \
                     patch.object(Path, 'read_bytes', fake_read_bytes), \
                     contextlib.redirect_stdout(output):
                    try:
                        exec(compile(text, str(source), 'exec'),
                             {'__name__': 'heartbeat_empty_table_twin',
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
        message = ('#3178 an emptied cases table must exit red through the '
                   'shared verdict (the old tail read the empty run as '
                   'all-KILLED, rc=0)')
        self.assertIsInstance(code, SystemExit,
                              message + ' (the run must exit through SystemExit)')
        self.assertTrue(code.code == 1,
                        message + ' (zero rows is not a green verdict)')
        self.assertIn('KILLED   0:', output.getvalue(),
                      message + ' (the summary lines stay)')


if __name__ == '__main__':
    unittest.main()

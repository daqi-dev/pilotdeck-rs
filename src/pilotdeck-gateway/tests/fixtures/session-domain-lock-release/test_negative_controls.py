# anchor-selfcheck: unsupported unit tests of the verdict tail, no product source mutations
#!/usr/bin/env python3
"""#3178 item 7 (#2973 sibling) + fix-r1 (boundary #3): the session-domain
verdict tail must refuse a zero-row run without publishing a summary.

The old tail ``0 if counts['KILLED'] == len(results) else 1`` is vacuously
true on an empty row set — a lost mutation table or a misparsed selection
finishes with ``0 == 0`` and exits 0, the all-green code. The family contract
(fixtures/README.md「零行永不返回 0」) is ``negative_selection.verdict_exit``:
empty is a failure. #3178 fix-r1 additionally pins the boundary #3 shape: an
empty selection is a never-started run — rc=2 and **no** ``summary.json`` at
all, because a summary that exists reads like a completed run to anyone who
only checks for the artifact. The twin drives the delivered ``main()``
in-process to an empty finish — ``MUTATIONS`` emptied, the source guard and
the output-directory admission faked (a real ``mutating_sources`` lease is the
coordinator's post-commit run, not a unit twin) — and asserts the exit is 2
and the evidence directory holds no summary. No cargo, no lease, no product
source writes.

Run with:

    python3 -B -m unittest -v session-domain-lock-release/test_negative_controls.py
"""
import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


def load_runner():
    source = Path(__file__).with_name('negative_controls.py')
    spec = importlib.util.spec_from_file_location(
        'session_domain_lock_release_negative', source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ZeroRowVerdictControls(unittest.TestCase):
    """A finished run with zero rows must exit 2 unpublished, never green."""

    def test_empty_run_exits_two_without_a_summary(self):
        runner = load_runner()
        with tempfile.TemporaryDirectory(prefix='pdrs-session-lock-zero-') as temp:
            out = Path(temp) / 'run'

            class Guard:
                def __enter__(self):
                    return {}

                def __exit__(self, *exc):
                    return False

            with patch.object(runner, 'MUTATIONS', []), \
                    patch.object(runner, 'mutating_sources',
                                 lambda root, paths, **kwargs: Guard()), \
                    patch.object(sys, 'argv',
                                 ['negative_controls.py', '--output', str(out)]):
                code = runner.main()
            # #3178 fix-r2 (DS concurrency#1): the real admission ran above
            # (the old fake_output_directory twin shape kept the .claim O_EXCL
            # path out of the twin), so the refusal's withdrawal is pinned on
            # the real claim: nothing may remain, and a re-run of the same
            # output path must be admitted again instead of refused on the
            # leftover never-ran directory (#1060/#953).
            leftover = sorted(path.name for path in out.iterdir()) \
                if out.exists() else []
            try:
                runner.new_output_directory(out)
                refused, reclaimed = None, (out / '.claim').is_file()
            except SystemExit as error:
                refused, reclaimed = str(error), False
        message = ('#3178 a zero-row run must exit 2 and publish no summary '
                   '(the old tail read 0 == 0 as green and an empty '
                   'summary.json read like a completed run)')
        self.assertEqual(code, 2, message)
        self.assertEqual(leftover, [],
                         message + ' (the never-started run must withdraw its '
                         'claim: the evidence directory must not exist or be '
                         'empty; a leftover .claim refused the same path on '
                         're-run)')
        self.assertIsNone(refused,
                           message + f' (a re-run of the same output path must '
                           f'be admitted, not refused: {refused})')
        self.assertTrue(reclaimed,
                        message + ' (the re-admitted evidence directory must '
                        'hold a fresh claim)')


CHECK_ANCHORS_CHILD = '''\
import sys
from pathlib import Path

fixtures, source = (Path(argument) for argument in sys.argv[1:3])
sys.path.insert(0, str(fixtures))
namespace = {'__name__': 'check_anchors_dead_stdout_child',
             '__file__': str(source)}
exec(compile(source.read_text(), str(source), 'exec'), namespace)
# A mutation table that lost every row: the delivered --check-anchors
# preflight must refuse (rc=2) instead of certifying zero anchors, on the
# real shared helpers -- and still refuse inside this child, whose stdout
# was closed before exec by the parent's preexec_fn (fix-r5 P2-a#2: the
# list-form spelling of the old ``1>&-`` shell redirect).
namespace['MUTATIONS'] = []
sys.argv = [source.name, '--check-anchors']
raise SystemExit(namespace['main']())
'''


class CheckAnchorsDeadStdoutControls(unittest.TestCase):
    """#3178 fix-r4 (DS sibling#4 + MM sibling#5), revised by fix-r5 (P2-a#2).

    The guarded prints this family routed through ``dead_stream_print``
    (the empty-selection refusal put ahead of the loop in fix-r4, the
    stale-anchor refusal in fix-r3) had no dead-stream twin.
    ``CHECK_ANCHORS_CHILD`` runs the delivered ``negative_controls.py``
    in a real child process — the real ``negative_process`` helpers,
    nothing faked, no cargo run — with an emptied table and
    ``--check-anchors``, stdout closed at exec time (fix-r5 P2-a#2: the
    list-form ``preexec_fn=lambda: os.close(1)`` replaces the unquoted
    ``/bin/sh -c`` spelling; the two are indistinguishable, pinned by
    fix-r5's probe-dead-stdout-semantics.log).

    Pinned semantics (same probe, correcting fix-r4's causal claim): a
    dead exec-time stdout makes CPython set ``sys.stdout = None`` and a
    bare ``print`` silently returns — the line is lost, never a crash and
    never re-routed; the intended exit code survives either way. So the
    refusal must still reach stderr and the exit must stay at 2.
    """

    def test_an_empty_selection_refuses_under_a_dead_stdout(self):
        import os

        with tempfile.TemporaryDirectory(prefix='pdrs-sdlr-check-') as temp:
            child = Path(temp) / 'child.py'
            child.write_text(CHECK_ANCHORS_CHILD)
            result = subprocess.run(
                [sys.executable, '-I', str(child),
                 str(Path(__file__).resolve().parents[1]),
                 str(Path(__file__).with_name('negative_controls.py'))],
                preexec_fn=lambda: os.close(1),
                capture_output=True, timeout=120)
        message = ('#3178 the --check-anchors refusal must keep exit 2 under '
                   'a dead stdout (the refusal reason must still reach '
                   'stderr; a lost stdout line is never a crash and never '
                   'a re-route)')
        self.assertEqual(result.returncode, 2,
                         message + f' (got rc={result.returncode}, '
                         f'stderr={result.stderr!r})')
        self.assertIn(b'EMPTY ANCHOR SELECTION', result.stderr,
                      message + ' (the refusal reason must still reach stderr)')
        self.assertNotIn(b'Exception ignored', result.stderr,
                         message + ' (the interpreter-shutdown flush must not '
                         'see the dead stdout)')


if __name__ == '__main__':
    unittest.main()

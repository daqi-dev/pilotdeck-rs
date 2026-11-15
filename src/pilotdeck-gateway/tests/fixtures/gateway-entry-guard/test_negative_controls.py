# anchor-selfcheck: unsupported dead-stdout unit twin, no product source mutations
#!/usr/bin/env python3
"""#3178 fix-r5 (DS sibling#1 + MM sibling#1), twin in the fix-r4 shape
revised by P2-a#2: the gateway-entry-guard ``--check-anchors`` preflight
must refuse an emptied table under a dead stdout.

Run with:

    python3 -B -m unittest -v gateway-entry-guard/test_negative_controls.py
"""
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

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
# list-form spelling of the old ``1>&-`` shell redirect). This family's
# check_anchors() reads the module-level list directly.
namespace['MUTATIONS'] = []
sys.argv = [source.name, '--check-anchors']
raise SystemExit(namespace['main']())
'''


class CheckAnchorsDeadStdoutControls(unittest.TestCase):
    """#3178 fix-r5 (DS sibling#1 + MM sibling#1), twin shape (P2-a#2).

    fix-r5 put the empty-table refusal into this runner's ``check_anchors``
    (the vacuum-green shape fix-r4 closed for the parity-lock family: an
    emptied table used to fall through the loop and print "0 anchors: ok"
    with rc=0; this table has no roster guard, so the refusal lives in the
    anchor gate itself). The twin pins the refusal under a dead stdout, the
    same shape the four fix-r4 twins got — revised by fix-r5 P2-a#2 after
    the dead-stdout semantics were pinned (probe-dead-stdout-semantics.log):
    ``CHECK_ANCHORS_CHILD`` runs the delivered ``negative_controls.py`` in a
    real child process — the real shared helpers, nothing faked, no cargo
    run — with an emptied table and ``--check-anchors``, stdout closed at
    exec time by the list-form ``preexec_fn=lambda: os.close(1)``.

    Pinned semantics: a dead exec-time stdout makes CPython set
    ``sys.stdout = None`` and a bare ``print`` silently returns — the line
    is lost, never a crash and never re-routed; the intended exit code
    survives either way. So the refusal must still reach stderr and the
    exit must stay at 2. Deleting the refusal instead lets the emptied
    table fall through to the green line and rc=0, and this twin goes red
    on the exit-code assertion.
    """

    def test_an_empty_selection_refuses_under_a_dead_stdout(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-geg-check-') as temp:
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

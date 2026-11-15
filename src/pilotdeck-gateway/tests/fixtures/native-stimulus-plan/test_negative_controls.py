# anchor-selfcheck: unsupported unit tests of the stimulus-plan empty-table refusal, no product source mutations
#!/usr/bin/env python3
"""#3178 fix-r2 (DS sibling#2) controls for the stimulus-plan runner's tail.

The delivered runner (``negative_controls.py``) demands ``python3 -I`` (an
isolated-interpreter guard at its top), so an in-process import twin is
impossible here: the control below drives the runner in a real child process
with the interpreter flag it requires. The child empties the case table and
calls ``main()`` on a fresh output path with CARGO_TARGET_DIR supplied, the
process layer stubbed to ``unreachable`` (no cargo, no slot wait, no source
guard may ever start for a table with nothing to verify).

The runner's tail used to be ``0 if len(results) == len(selected) and
all(...)`` — an emptied table gave ``0 == 0 and all([])``: rc=0, an empty
``summary.json`` published, and the evidence directory left occupied. The
refusal must come first: exit 2, no evidence directory at all, the same
output path re-runnable.

Run with:

    python3 -B -m unittest -v native-stimulus-plan/test_negative_controls.py
"""
import os
import subprocess
import sys
import tempfile
from pathlib import Path
import unittest


CHILD = '''\
import sys
from pathlib import Path

fixtures, runner, out = (Path(argument) for argument in sys.argv[1:4])
sys.path.insert(0, str(fixtures / 'native-stimulus-plan'))
import negative_controls


def unreachable(*_args, **_kwargs):
    raise AssertionError('#3178 the empty table must refuse before any '
                         'run, slot wait or source guard')


negative_controls.cases = []
negative_controls.run_owned = unreachable
negative_controls.wait_for_slot = unreachable
negative_controls.mutating_sources = unreachable
sys.argv = [str(runner), str(out)]
raise SystemExit(negative_controls.main())
'''


class StimulusEmptyTableControls(unittest.TestCase):
    """An emptied stimulus-plan case table is a refusal, not a vacuum green."""

    def test_an_empty_case_table_refuses_before_any_evidence(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-ns-empty-') as temp:
            out = Path(temp) / 'run'
            child = Path(temp) / 'child.py'
            child.write_text(CHILD)
            fixtures = Path(__file__).resolve().parents[1]
            runner = Path(__file__).with_name('negative_controls.py')
            env = dict(os.environ,
                       CARGO_TARGET_DIR=str(Path(temp) / 'target'))
            result = subprocess.run(
                [sys.executable, '-I', str(child), str(fixtures),
                 str(runner), str(out)],
                capture_output=True, timeout=120, env=env)
            listing = (sorted(path.name for path in out.iterdir())
                       if out.exists() else None)
        message = ('#3178 an empty case table must refuse before any evidence '
                   '(the old tail read the empty run as all-KILLED, rc=0, '
                   'and published an empty summary.json)')
        self.assertEqual(result.returncode, 2, message + f' (got rc='
                         f'{result.returncode}, stderr='
                         f'{result.stderr!r})')
        self.assertFalse(out.exists(),
                         message + ' (nothing may be created for a run that '
                         'never started; the same output path must stay '
                         're-runnable)')
        self.assertIsNone(listing,
                          message + ' (the evidence directory must not '
                          'exist, let alone hold a summary)')
        self.assertIn(b'EMPTY CASE TABLE', result.stderr,
                      message + ' (the refusal reason must reach stderr)')
        self.assertNotIn(b'summary.json', result.stdout + result.stderr,
                         message + ' (no summary may be published for a '
                         'run that never started)')


if __name__ == '__main__':
    unittest.main()

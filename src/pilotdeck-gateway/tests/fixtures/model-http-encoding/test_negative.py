# anchor-selfcheck: unsupported unit tests of the http-encoding empty-table refusal, no product source mutations
#!/usr/bin/env python3
"""#3178 fix-r3 (DS3 F6) controls for the http-encoding runner's tail.

The runner (``negative.py``) is host-driven with cargo, so these controls
patch the process layer on the imported module — no cargo run, no source
mutation, no slot wait happens here. What is pinned is the empty-table
contract:

The runner's tail used to be ``0 if all(row['status'] == 'KILLED' for row
in summary) else 1``, so an emptied ``CASES`` table produced ``summary ==
[]`` and exited 0 — read as all-KILLED (the vacuum green: no mutant
verified, the evidence directory left occupied). The ``--check-anchors``
green exit had the same shape ("#867 0 anchors valid"). Both must refuse
before the evidence directory exists: exit 2, no summary, and the output
path left untouched for a re-run.

Run with:

    python3 -B -m unittest -v model-http-encoding/test_negative.py
"""
import contextlib
import io
import os
import sys
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch


class EncodingEmptyTableControls(unittest.TestCase):
    """An emptied CASES table is a refusal, not a vacuum green."""

    def setUp(self):
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative
        self.runner = negative
        self.tmp = tempfile.TemporaryDirectory(prefix='encoding-empty-')
        self.addCleanup(self.tmp.cleanup)
        self.out = Path(self.tmp.name).resolve() / 'run'

    def _unreachable(self):
        def unreachable(*_args, **_kwargs):
            raise AssertionError('#3178 the empty table must refuse before '
                                 'any run, slot wait or source guard')
        return unreachable

    def test_an_empty_case_table_refuses_before_any_evidence(self):
        errors = io.StringIO()
        with patch.dict(os.environ, {'CARGO_TARGET_DIR': self.tmp.name}), \
             patch.object(self.runner, 'CASES', []), \
             patch.object(self.runner, 'mutating_sources', self._unreachable()), \
             patch.object(self.runner, 'run_owned', self._unreachable()), \
             patch.object(self.runner, 'wait_for_slot', self._unreachable()), \
             patch.object(sys, 'argv',
                          ['negative.py', str(self.out)]), \
             contextlib.redirect_stderr(errors):
            # The refusal is a plain `return 2` out of main, so the twin
            # asserts the returned contract code, not a SystemExit.
            code = self.runner.main()
        message = ('#3178 an empty case table must refuse before any evidence '
                   '(the old tail read the empty run as all-KILLED, rc=0)')
        self.assertEqual(code, 2, message)
        self.assertFalse(self.out.exists(),
                         message + ' (nothing may be created for a run that '
                         'never started; the same output path must stay '
                         're-runnable)')
        self.assertIn('EMPTY CASE TABLE', errors.getvalue(),
                      message + ' (the refusal reason must reach stderr)')

    def test_an_empty_case_table_fails_the_check_anchors_preflight(self):
        errors = io.StringIO()
        with patch.object(self.runner, 'CASES', []), \
             patch.object(sys, 'argv', ['negative.py', '--check-anchors']), \
             contextlib.redirect_stderr(errors):
            code = self.runner.main()
        message = ('#3178 an empty case table must fail --check-anchors '
                   '(the green exit used to print "#867 0 anchors valid" '
                   'and certify a table with nothing in it)')
        self.assertEqual(code, 2, message)
        self.assertIn('EMPTY CASE TABLE', errors.getvalue(),
                      message + ' (the refusal reason must reach stderr)')


if __name__ == '__main__':
    unittest.main()

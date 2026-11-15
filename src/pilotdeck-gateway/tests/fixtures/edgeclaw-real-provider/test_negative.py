# anchor-selfcheck: unsupported unit tests of the real-provider empty-table refusal, no product source mutations
#!/usr/bin/env python3
"""#3178 fix-r3 (DS3 F6) controls for the real-provider runner's tail.

The runner (``negative.py``) drives ``test_smoke.py`` in child processes
with cargo-derived environment, so these controls patch the process layer
on the imported module — no child run, no source mutation happens here.
What is pinned is the empty-table contract:

The runner's tail used to be ``0 if len(results) == len(mutants) and
all(r['status'] == 'KILLED' for r in results) else 1``, so an emptied
``mutants`` table gave ``0 == 0 and all([])``: rc=0, an empty
``summary.json`` published, and the evidence directory left occupied (the
vacuum green). The ``--check`` green exit had the same shape ("PASS: 0
mutants"). Both must refuse before the evidence directory exists: exit 2,
no summary, and the output path left untouched for a re-run.

Run with:

    python3 -B -m unittest -v edgeclaw-real-provider/test_negative.py
"""
import contextlib
import io
import os
import sys
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch


class RealProviderEmptyTableControls(unittest.TestCase):
    """An emptied mutants table is a refusal, not a vacuum green."""

    def setUp(self):
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative
        self.runner = negative
        self.tmp = tempfile.TemporaryDirectory(prefix='real-provider-empty-')
        self.addCleanup(self.tmp.cleanup)
        self.out = Path(self.tmp.name).resolve() / 'run'

    def _unreachable(self):
        def unreachable(*_args, **_kwargs):
            raise AssertionError('#3178 the empty table must refuse before '
                                 'any run, slot wait or source guard')
        return unreachable

    def test_an_empty_mutation_table_refuses_before_any_evidence(self):
        errors = io.StringIO()
        with patch.dict(os.environ, {'CARGO_TARGET_DIR': self.tmp.name}), \
             patch.object(self.runner, 'mutants', []), \
             patch.object(self.runner, 'mutating_sources', self._unreachable()), \
             patch.object(self.runner, 'run_owned', self._unreachable()), \
             patch.object(self.runner, 'wait_for_slot', self._unreachable()), \
             patch.object(sys, 'argv',
                          ['negative.py', str(self.out)]), \
             contextlib.redirect_stderr(errors):
            # The refusal is a plain `return 2` out of main, so the twin
            # asserts the returned contract code, not a SystemExit.
            code = self.runner.main()
        message = ('#3178 an empty mutation table must refuse before any '
                   'evidence (the old tail read the empty run as '
                   'all-KILLED, rc=0, and published an empty summary.json)')
        self.assertEqual(code, 2, message)
        self.assertFalse(self.out.exists(),
                         message + ' (nothing may be created for a run that '
                         'never started; the same output path must stay '
                         're-runnable)')
        self.assertIn('EMPTY MUTATION TABLE', errors.getvalue(),
                      message + ' (the refusal reason must reach stderr)')

    def test_an_empty_mutation_table_fails_the_check_preflight(self):
        errors = io.StringIO()
        with patch.object(self.runner, 'mutants', []), \
             patch.object(sys, 'argv', ['negative.py', '--check']), \
             contextlib.redirect_stderr(errors):
            code = self.runner.main()
        message = ('#3178 an empty mutation table must fail --check (the '
                   'green exit used to print "PASS: 0 mutants" and certify '
                   'a table with nothing in it)')
        self.assertEqual(code, 2, message)
        self.assertIn('EMPTY MUTATION TABLE', errors.getvalue(),
                      message + ' (the refusal reason must reach stderr)')


if __name__ == '__main__':
    unittest.main()

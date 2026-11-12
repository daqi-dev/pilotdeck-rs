#!/usr/bin/env python3
# anchor-selfcheck: unsupported unit tests of the fixture's own bound-test gate; no product source mutation table
"""#2692 fix-r9 (review sibling#3): the --check declares_test gate's red side.

fix-r6 added the gate (a bound test renamed in read_tests.rs used to keep the
roster green with the name living only in this string table), but only the
host's out-of-tree negative round ever showed it red. The first case runs the
real `--check` and pins its bound-test count; the second simulates the rename
(the shared `declares_test` never matches a `_renamed` spelling) and demands
the matching refusal. fix-r10 (review sibling#2 / boundary#1) adds the other
rejection legs -- stale anchor, no-op mutation, duplicate roster name -- and
the static-roster drift leg; the table-level cases splice the script's own
source in memory (`--check` runs at module level, so the only way to drift
the table without touching the tree is to exec modified source text).
Run from anywhere:

    python3 -I -B src/pilotdeck-gateway/tests/fixtures/turn-read/test_bound_test_gate.py
"""
import contextlib
import importlib.util
import io
import sys
import unittest
from pathlib import Path

FIXTURES = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(FIXTURES))
import negative_suite  # noqa: E402

SCRIPT = Path(__file__).resolve().with_name('negative.py')


def run_check(source_text=None):
    """Exec negative.py under argv ['negative.py', '--check'].

    The `--check` branch runs at module level and terminates the module with
    SystemExit, so the exit code, both streams and the half-built module (its
    mutants table) are the result. No cargo runs; the tree is only read.
    `source_text` (fix-r10) overrides the on-disk script so the table-level
    red sides can splice one row without mutating the tree.
    """
    code_text = SCRIPT.read_text() if source_text is None else source_text
    spec = importlib.util.spec_from_file_location('turn_read_negative_check', SCRIPT)
    module = importlib.util.module_from_spec(spec)
    out, err = io.StringIO(), io.StringIO()
    saved_argv = sys.argv
    sys.argv = ['negative.py', '--check']
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            exec(compile(code_text, str(SCRIPT), 'exec'), module.__dict__)
            raise AssertionError('#2692 fix-r9: --check must terminate the module')
    except SystemExit as stop:
        return stop.code, out.getvalue(), err.getvalue(), module
    finally:
        sys.argv = saved_argv


class BoundTestGate(unittest.TestCase):
    def test_check_passes_and_reports_bound_tests(self):
        code, out, err, module = run_check()
        self.assertTrue(code == 0,
                       f'#2692 fix-r9: --check must pass on the pristine tree: '
                       f'rc={code} err={err!r}')
        self.assertIn(f'{len(module.mutants)} bound tests declared', out,
                      '#2692 fix-r9: the bound-test declaration count must be reported')

    def test_check_rejects_a_renamed_bound_test(self):
        # The roster keeps the old spelling while the fn is renamed in
        # read_tests.rs, so the real declares_test matching runs and must come
        # back empty. negative.py binds the name from negative_suite at import
        # time, so the shared attribute is swapped around the exec.
        real = negative_suite.declares_test

        def renamed(source, short, macro=None):
            return real(source, short + '_renamed', macro)

        saved = negative_suite.declares_test
        negative_suite.declares_test = renamed
        try:
            code, out, err, _module = run_check()
        finally:
            negative_suite.declares_test = saved
        self.assertTrue(code == 2,
                       f'#2692 fix-r9: a renamed bound test must refuse --check: '
                       f'rc={code} out={out!r}')
        self.assertIn('bound test not declared in read_tests.rs', err,
                      '#2692 fix-r9: the refusal names the drifted binding')

    def _run_spliced(self, old, new, label):
        """Exec the script with exactly one verbatim fragment replaced."""
        text = SCRIPT.read_text()
        self.assertTrue(text.count(old) == 1,
                        f'#2692 fix-r10: {label}: the spliced fragment must '
                        f'appear exactly once, got {text.count(old)}')
        return run_check(source_text=text.replace(old, new, 1))

    def test_check_rejects_a_stale_anchor(self):
        # fix-r10 (review sibling#2): the count != 1 refusal leg had no
        # in-tree red side -- the gate could regress to "always 1" and only
        # the host's cargo round would notice.
        code, out, err, _module = self._run_spliced(
            "'pub const HISTORY_EPOCH_ATTEMPTS: usize = 3;', "
            "'pub const HISTORY_EPOCH_ATTEMPTS: usize = 2;',",
            "'pub const HISTORY_EPOCH_ATTEMPTS: usize = 30;', "
            "'pub const HISTORY_EPOCH_ATTEMPTS: usize = 2;',",
            'stale-anchor splice')
        self.assertTrue(code == 2,
                        f'#2692 fix-r10: a stale anchor must refuse --check: '
                        f'rc={code} out={out!r}')
        self.assertIn('attempts-3-to-2: stale anchor (expected 1, got 0)', err,
                      '#2692 fix-r10: the refusal names the row and the count')

    def test_check_rejects_a_no_op_mutation(self):
        # fix-r10 (review sibling#2): before == after used to stay green until
        # the host's cargo round reported SURVIVED.
        code, out, err, _module = self._run_spliced(
            "'pub const HISTORY_EPOCH_ATTEMPTS: usize = 2;',",
            "'pub const HISTORY_EPOCH_ATTEMPTS: usize = 3;',",
            'no-op splice')
        self.assertTrue(code == 2,
                        f'#2692 fix-r10: a no-op mutation must refuse --check: '
                        f'rc={code} out={out!r}')
        self.assertIn('attempts-3-to-2: no-op mutation (before == after)', err,
                      '#2692 fix-r10: the refusal names the row and the shape')

    def test_check_rejects_duplicate_roster_names(self):
        # fix-r10 (review sibling#2): two rows sharing a name silently
        # overwrite each other's evidence logs in the mutating run.
        code, out, err, _module = self._run_spliced(
            "('attempts-3-to-4', source,",
            "('attempts-3-to-2', source,",
            'duplicate-name splice')
        self.assertTrue(code == 2,
                        f'#2692 fix-r10: a duplicate roster name must refuse '
                        f'--check: rc={code} out={out!r}')
        self.assertIn('turn-read roster has duplicate mutation names', err,
                      '#2692 fix-r10: the refusal names the duplicate shape')

    def test_check_refuses_a_dropped_roster_row(self):
        # fix-r10 (review boundary#1): the printed anchor count and the count
        # pin above both used to derive from the same runtime table, so a
        # renamed row kept --check green while one control silently stopped
        # being audited; the static EXPECTED_NAMES roster must refuse it.
        code, out, err, _module = self._run_spliced(
            "('final-error-text', source,",
            "('final-error-text-renamed', source,",
            'roster-drift splice')
        self.assertTrue(code == 2,
                        f'#2692 fix-r10: a drifted roster must refuse --check: '
                        f'rc={code} out={out!r}')
        self.assertIn('turn-read mutation roster drifted', err,
                      '#2692 fix-r10: the refusal names the roster drift')
        self.assertIn("extra=['final-error-text-renamed']", err,
                      '#2692 fix-r10: the refusal names the drifted row')


if __name__ == '__main__':
    unittest.main()

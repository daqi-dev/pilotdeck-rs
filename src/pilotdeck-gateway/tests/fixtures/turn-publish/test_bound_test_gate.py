#!/usr/bin/env python3
# anchor-selfcheck: unsupported unit tests of the fixture's own bound-test gate; no product source mutation table
"""#2692 fix-r9 (review sibling#3): the --check declares_test gate's red side.

fix-r6 added the gate (a bound test renamed in publish_tests.rs used to keep
the roster green with the name living only in this string table), but only the
host's out-of-tree negative round ever showed it red. The first case runs the
real `--check` and pins its bound-test count; the second simulates the rename
(the shared `declares_test` never matches a `_renamed` spelling) and demands
the matching refusal. fix-r10 (review sibling#2 / boundary#1) adds the other
rejection legs -- stale anchor, no-op mutation, duplicate roster name -- and
the static-roster drift leg; the table-level cases splice the script's own
source in memory (`--check` runs at module level, so the only way to drift
the table without touching the tree is to exec modified source text).
Run from anywhere:

    python3 -I -B src/pilotdeck-gateway/tests/fixtures/turn-publish/test_bound_test_gate.py
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
    spec = importlib.util.spec_from_file_location('turn_publish_negative_check', SCRIPT)
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
        # publish_tests.rs, so the real declares_test matching runs and must
        # come back empty. negative.py binds the name from negative_suite at
        # import time, so the shared attribute is swapped around the exec.
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
        self.assertIn('bound test not declared in publish_tests.rs', err,
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
            "'            state.deferred.push(event);',",
            "'            state.deferred.push(nobody);',",
            'stale-anchor splice')
        self.assertTrue(code == 2,
                        f'#2692 fix-r10: a stale anchor must refuse --check: '
                        f'rc={code} out={out!r}')
        self.assertIn('duplicate-terminal-suppressed: stale anchor (expected 1, got 0)', err,
                      '#2692 fix-r10: the refusal names the row and the count')

    def test_check_rejects_a_no_op_mutation(self):
        # fix-r10 (review sibling#2): before == after used to stay green until
        # the host's cargo round reported SURVIVED.
        code, out, err, _module = self._run_spliced(
            "'            let conflict = false;', 'conflict_does_not_start_pump'),",
            "'            let conflict = error.code() == Some(\"conflict\");', "
            "'conflict_does_not_start_pump'),",
            'no-op splice')
        self.assertTrue(code == 2,
                        f'#2692 fix-r10: a no-op mutation must refuse --check: '
                        f'rc={code} out={out!r}')
        self.assertIn('conflict-treated-as-unknown: no-op mutation (before == after)', err,
                      '#2692 fix-r10: the refusal names the row and the shape')

    def test_check_rejects_duplicate_roster_names(self):
        # fix-r10 (review sibling#2): two rows sharing a name silently
        # overwrite each other's evidence logs in the mutating run.
        code, out, err, _module = self._run_spliced(
            "('accept-failure-as-conflict', source,",
            "('conflict-treated-as-unknown', source,",
            'duplicate-name splice')
        self.assertTrue(code == 2,
                        f'#2692 fix-r10: a duplicate roster name must refuse '
                        f'--check: rc={code} out={out!r}')
        self.assertIn('turn-publish roster has duplicate mutation names', err,
                      '#2692 fix-r10: the refusal names the duplicate shape')

    def test_check_refuses_a_dropped_roster_row(self):
        # fix-r10 (review boundary#1): the printed anchor count and the count
        # pin above both used to derive from the same runtime table, so a
        # renamed row kept --check green while one control silently stopped
        # being audited; the static EXPECTED_NAMES roster must refuse it.
        code, out, err, _module = self._run_spliced(
            "('drop-event', source,",
            "('drop-event-renamed', source,",
            'roster-drift splice')
        self.assertTrue(code == 2,
                        f'#2692 fix-r10: a drifted roster must refuse --check: '
                        f'rc={code} out={out!r}')
        self.assertIn('turn-publish mutation roster drifted', err,
                      '#2692 fix-r10: the refusal names the roster drift')
        self.assertIn("extra=['drop-event-renamed']", err,
                      '#2692 fix-r10: the refusal names the drifted row')

    def _run_surgered(self, surgery):
        """Exec the script with table surgery spliced in after the argv read.

        closure-r4 (MM sibling#1): the both-tables legs drift `mutants` AND
        `EXPECTED_NAMES` together, which a single-fragment splice cannot
        express; the surgery lines run at module level, right before the
        `--check` branch reads the tables.
        """
        text = SCRIPT.read_text()
        anchor = 'args = sys.argv[1:]'
        self.assertTrue(text.count(anchor) == 1,
                        '#2692 closure-r4: the surgery anchor must appear '
                        'exactly once')
        return run_check(source_text=text.replace(
            anchor, anchor + '\n' + surgery))

    def test_check_refuses_a_new_shared_test_row_left_unpinned(self):
        # closure-r4 (MM closure sibling#1, P1): the twin reuses `drop-event`'s
        # bound test name on both tables (the roster stays aligned, so
        # `registered()` passes), and the reverse check must refuse and name
        # exactly the twin. The denominator guards run before the per-row
        # audits, so the anchor text the twin also shares does not answer
        # first with a stale-anchor count.
        twin = 'drop-event-unpinned-twin'
        surgery = (
            "_row = next(row for row in mutants if row[0] == 'drop-event')\n"
            f"mutants = list(mutants) + [({twin!r},) + _row[1:]]\n"
            f"EXPECTED_NAMES = tuple(EXPECTED_NAMES) + ({twin!r},)\n")
        code, out, err, _module = self._run_surgered(surgery)
        self.assertTrue(code == 2,
                        f'#2692 closure-r4: an unpinned row reusing a shared '
                        f'bound test must refuse --check: rc={code} out={out!r}')
        self.assertIn('#2692 turn-publish shared-test rows missing from '
                      f'ROW_PINS: {twin}', err,
                      '#2692 closure-r4: the refusal names the family and '
                      'the unpinned twin')

    def test_check_refuses_a_row_dropped_from_both_tables(self):
        # closure-r4 (MM closure sibling#1, P1): a row whose bound test
        # another row also carries used to survive a both-tables drop --
        # `registered()` stayed aligned and the surviving sibling kept the
        # test bound, so --check stayed green while one control silently
        # stopped being audited. ROW_PINS is a third denominator outside
        # both tables; every pin in turn must refuse the drop (the
        # turn-submit / native-blankkey fix-r13 red-side shape).
        _code, _out, _err, module = run_check()
        for dropped in module.ROW_PINS:
            surgery = (
                f"mutants = [row for row in mutants if row[0] != {dropped!r}]\n"
                f"EXPECTED_NAMES = tuple(name for name in EXPECTED_NAMES "
                f"if name != {dropped!r})\n")
            code, out, err, _m = self._run_surgered(surgery)
            self.assertTrue(code == 2,
                            f'#2692 closure-r4: a both-tables drop of '
                            f'{dropped} must refuse --check: rc={code} '
                            f'out={out!r}')
            self.assertIn(f'#2692 row pin names a row that is gone: {dropped}',
                          err,
                          f'#2692 closure-r4: the refusal names the dropped '
                          f'row {dropped}')


if __name__ == '__main__':
    unittest.main()

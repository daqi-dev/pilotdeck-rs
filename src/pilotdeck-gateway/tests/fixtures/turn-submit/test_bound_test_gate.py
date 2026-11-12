#!/usr/bin/env python3
# anchor-selfcheck: unsupported unit tests of the fixture's own roster gate; no product source mutation table
"""#2692 fix-r12 (review MM sibling P1): the static-roster gate's red side.

turn-submit was the one turn fixture without `EXPECTED_NAMES` + the
`registered()` startup comparison (turn-read / turn-publish / native-config /
native-blankkey carry theirs since fix-r10), so a dropped or renamed row kept
--check green while one control silently stopped being audited (the #1666
shape). The cases below drift the table in memory (`build()` reads the module
global, so the tree is only read) and demand the matching refusal. Run from
anywhere:

    python3 -I -B src/pilotdeck-gateway/tests/fixtures/turn-submit/test_bound_test_gate.py
"""
import contextlib
import importlib.util
import io
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve()
SPEC = importlib.util.spec_from_file_location(
    'turn_submit_negative', HERE.with_name('negative.py'))
nc = importlib.util.module_from_spec(SPEC)
# The module name is never `__main__`, so the `if __name__` guard keeps main()
# inert here; loading only defines the table and the functions.
SPEC.loader.exec_module(nc)

DROPPED_ROW = 'permission-check-skipped'
RENAMED_ROW = 'run-id-check-skipped'
RENAMED_TO = 'run-id-check-skipped-renamed'


def run_check(rows=None, expected_names=None):
    """Call nc.main() under argv ['negative.py', '--check'].

    Returns (exit code, stdout, stderr); the refusal envelope exits 2 with the
    reason on stderr (`negative_selection.refuse`), which `main()`'s
    `except Exception` cannot swallow. No cargo runs; the tree is only read.
    `rows` / `expected_names` swap the module's two tables in memory so the
    red sides can drift one row without touching the tree.
    """
    out, err = io.StringIO(), io.StringIO()
    saved_argv, saved_rows = sys.argv, nc.mutants
    saved_names = nc.EXPECTED_NAMES
    sys.argv = ['negative.py', '--check']
    if rows is not None:
        nc.mutants = rows
    if expected_names is not None:
        nc.EXPECTED_NAMES = expected_names
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            try:
                code = nc.main()
            except SystemExit as stop:
                code = stop.code
    finally:
        nc.mutants = saved_rows
        nc.EXPECTED_NAMES = saved_names
        sys.argv = saved_argv
    return code, out.getvalue(), err.getvalue()


def renamed_rows(mutation, new_name):
    rows, patched = [], False
    for row in nc.mutants:
        row = list(row)
        if row[0] == mutation:
            patched = True
            row[0] = new_name
        rows.append(row)
    assert patched, f'#2692 fix-r12: the row to rename must exist: {mutation}'
    return rows


class RosterGate(unittest.TestCase):
    def test_check_passes_and_reports_the_mutation_count(self):
        code, out, err = run_check()
        self.assertTrue(code == 0,
                        f'#2692 fix-r12: --check must pass on the pristine '
                        f'tree: rc={code} err={err!r}')
        self.assertIn(f'{len(nc.EXPECTED_NAMES)} mutations + 1 baseline; anchors OK', out,
                      '#2692 fix-r12: the count is pinned against the static roster')

    def test_check_refuses_a_dropped_roster_row(self):
        rows = [row for row in nc.mutants if row[0] != DROPPED_ROW]
        code, out, err = run_check(rows=rows)
        self.assertTrue(code == 2,
                        f'#2692 fix-r12: a dropped roster row must refuse '
                        f'--check: rc={code} out={out!r}')
        self.assertIn('turn-submit mutation roster drifted', err,
                      '#2692 fix-r12: the refusal names the roster drift')
        self.assertIn(f'missing=[{DROPPED_ROW!r}]', err,
                      f'#2692 fix-r12: the refusal names the missing row: {err!r}')

    def test_check_refuses_a_renamed_roster_row(self):
        rows = renamed_rows(RENAMED_ROW, RENAMED_TO)
        code, out, err = run_check(rows=rows)
        self.assertTrue(code == 2,
                        f'#2692 fix-r12: a renamed roster row must refuse '
                        f'--check: rc={code} out={out!r}')
        self.assertIn('turn-submit mutation roster drifted', err,
                      '#2692 fix-r12: the refusal names the roster drift')
        self.assertIn(f'extra=[{RENAMED_TO!r}]', err,
                      f'#2692 fix-r12: the refusal names the renamed row: {err!r}')

    def test_check_rejects_duplicate_roster_names(self):
        # Two rows sharing a name silently overwrite each other's evidence
        # logs in the mutating run; `registered()` compares the runtime
        # table against the duplicate-free static roster, so the drift
        # refusal fires before any count is printed.
        rows = renamed_rows(RENAMED_ROW, DROPPED_ROW)
        code, out, err = run_check(rows=rows)
        self.assertTrue(code == 2,
                        f'#2692 fix-r12: a duplicate roster name must refuse '
                        f'--check: rc={code} out={out!r}')
        self.assertIn('turn-submit mutation roster drifted', err,
                      '#2692 fix-r12: the refusal names the roster drift')
        self.assertIn(f'missing=[{RENAMED_ROW!r}]', err,
                      f'#2692 fix-r12: the refusal names the shadowed row: {err!r}')

    def test_check_refuses_a_new_shared_row_left_unpinned(self):
        # closure-r2 (MM concurrency #1, P1): the forward pin loop only
        # catches a pinned row that disappeared. A row ADDED with a sibling's
        # assertion string and no ROW_PINS entry is the one drift nothing
        # guarded after the structural census left (closure-r1). The twin
        # reuses `timeout-status-skipped`'s marker on both tables (the roster
        # stays aligned, so `registered()` passes), and the reverse check must
        # refuse and name exactly the twin.
        source = next(row for row in nc.mutants
                     if row[0] == 'timeout-status-skipped')
        twin = list(source)
        twin[0] = 'timeout-status-skipped-unpinned-twin'
        rows = nc.mutants + [tuple(twin)]
        names = list(nc.EXPECTED_NAMES) + [twin[0]]
        code, out, err = run_check(rows=rows, expected_names=names)
        self.assertTrue(code == 2,
                        f'#2692 closure-r2: an unpinned row reusing a shared '
                        f'assertion must refuse --check: rc={code} out={out!r}')
        self.assertIn('#2692 turn-submit shared-assertion rows missing from '
                      f'ROW_PINS: {twin[0]}', err,
                      '#2692 closure-r2: the refusal names the family and '
                      'the unpinned twin')

    def test_check_refuses_a_row_dropped_from_both_tables(self):
        # #2692 fix-r13 (DS + MM sibling/boundary, both families): a row whose
        # assertion string another row also carries used to survive a
        # both-tables drop -- `registered()` stayed aligned and the surviving
        # sibling kept the shared #813 marker bound, so --check stayed green
        # while one control silently stopped being audited. ROW_PINS is a
        # third denominator outside both tables; every pin in turn must
        # refuse the drop (the native-blankkey fix-r12 red-side shape).
        for dropped in nc.ROW_PINS:
            rows = [row for row in nc.mutants if row[0] != dropped]
            names = [name for name in nc.EXPECTED_NAMES if name != dropped]
            assert rows and len(rows) == len(nc.mutants) - 1, dropped
            code, out, err = run_check(rows=rows, expected_names=names)
            self.assertTrue(code == 2,
                            f'#2692 fix-r13: a both-tables drop of {dropped} '
                            f'must refuse --check: rc={code} out={out!r}')
            self.assertIn(f'#2692 row pin names a row that is gone: {dropped}', err,
                          f'#2692 fix-r13: the refusal names the dropped row '
                          f'{dropped}')


if __name__ == '__main__':
    unittest.main()

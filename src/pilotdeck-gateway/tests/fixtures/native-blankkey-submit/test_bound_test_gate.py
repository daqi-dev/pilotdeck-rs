#!/usr/bin/env python3
# anchor-selfcheck: unsupported unit tests of the fixture's own bound-test gate; no product source mutation table
"""#2692 fix-r9 (review sibling#3): the --check bound-test gate's red side.

fix-r6 added the gate (a bound test renamed in the file its module path names
used to keep the roster green with the name living only in this string table),
but only the host's out-of-tree negative round ever showed it red. The first
case runs the real `--check` and pins its bound-test count; the second renames
the short name the shared binder returns and demands the matching refusal.
fix-r11 (review sibling#1 / sibling#2) adds the table-level red sides the same
review round asked for on the turn twins: the static-roster drift leg and
mutate()'s stale-anchor / no-op legs. fix-r12 (review MM boundary P1 +
sibling P2) adds the duplicate-name leg and the both-tables drift leg over
the shared-assertion ROW_PINS. The rows are drifted in memory (the
table is data here and `main()` reads the module global), so the tree is only
read. Run from anywhere:

    python3 -I -B src/pilotdeck-gateway/tests/fixtures/native-blankkey-submit/test_bound_test_gate.py
"""
import contextlib
import importlib.util
import io
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve()
SPEC = importlib.util.spec_from_file_location(
    'blankkey_negative_controls', HERE.with_name('negative_controls.py'))
nc = importlib.util.module_from_spec(SPEC)
# The module name is never `__main__`, so the `if __name__` guard keeps main()
# inert here; loading only defines the table and the functions.
SPEC.loader.exec_module(nc)

# Dropping this row keeps every pin bound through a sibling row
# (`registry-body-spelling` asserts the same `#2692 the record body keeps the
# raw JSON null`), so the drift test isolates the static-roster refusal, not
# a knock-on pin refusal -- the native-config DROPPED_ROW precedent.
DROPPED_ROW = 'submit-raw-spelling'
STALE_ROW = 'echo-capped-serializer'


def run_check(rows=None, expected_names=None):
    """Call nc.main() under argv ['negative_controls.py', '--check'].

    Returns (exit code, stdout, stderr); the refusal envelope exits 2 with the
    reason on stderr. No cargo runs; the tree is only read. `rows` (fix-r11)
    swaps the module's mutation table in memory so the table-level red sides
    can drift one row without touching the tree; `expected_names` (fix-r12)
    swaps the static roster too, so the both-tables drift leg can simulate a
    real deletion (a dropped row is normally removed from both sides at once
    because `registered()` forces alignment).
    """
    out, err = io.StringIO(), io.StringIO()
    saved_argv, saved_rows = sys.argv, nc.mutations
    saved_names = nc.EXPECTED_NAMES
    sys.argv = ['negative_controls.py', '--check']
    if rows is not None:
        nc.mutations = rows
    if expected_names is not None:
        nc.EXPECTED_NAMES = expected_names
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            try:
                code = nc.main()
            except SystemExit as stop:
                code = stop.code
    finally:
        nc.mutations = saved_rows
        nc.EXPECTED_NAMES = saved_names
        sys.argv = saved_argv
    return code, out.getvalue(), err.getvalue()


def patched_rows(mutation, old=None, new=None):
    """The table with one row's anchor fields replaced (in memory)."""
    rows, patched = [], False
    for row in nc.mutations:
        row = list(row)
        if row[0] == mutation:
            patched = True
            if old is not None:
                row[2] = old
            if new is not None:
                row[3] = new
        rows.append(row)
    assert patched, f'#2692 fix-r11: the row to patch must exist: {mutation}'
    return rows


class BoundTestGate(unittest.TestCase):
    def test_check_passes_and_reports_bound_tests(self):
        code, out, err = run_check()
        self.assertTrue(code == 0,
                       f'#2692 fix-r9: --check must pass on the pristine tree: '
                       f'rc={code} err={err!r}')
        self.assertIn(f'{len(nc.mutations)} bound tests declared', out,
                      '#2692 fix-r9: the bound-test declaration count must be reported')

    def test_check_rejects_a_renamed_bound_test(self):
        # The roster keeps the old spelling while the fn is renamed in the
        # file its module path names, so the real declares_test matching runs
        # and must come back empty.
        real = nc.bind_module_file

        def renamed(test, modules):
            path, short = real(test, modules)
            return path, short + '_renamed'

        saved_argv, saved_bind = sys.argv, nc.bind_module_file
        nc.bind_module_file = renamed
        out, err = io.StringIO(), io.StringIO()
        try:
            sys.argv = ['negative_controls.py', '--check']
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                with self.assertRaises(SystemExit,
                                       msg='#2692 fix-r9: a renamed bound test must '
                                           'refuse --check') as caught:
                    nc.main()
        finally:
            nc.bind_module_file = saved_bind
            sys.argv = saved_argv
        self.assertTrue(caught.exception.code == 2,
                       f'#2692 fix-r9: the refusal must be a clean rc 2: '
                       f'code={caught.exception.code!r} out={out.getvalue()!r}')
        self.assertIn('bound test not declared', err.getvalue(),
                      '#2692 fix-r9: the refusal names the drifted binding')

    def test_check_refuses_a_dropped_roster_row(self):
        # fix-r11 (review sibling#1): the printed anchor count and this
        # file's expectations all derive from the runtime table, so a dropped
        # row kept --check green while one control silently stopped being
        # audited; the static EXPECTED_NAMES roster must refuse the drift
        # before any count is printed (the #1666 shape; native-config /
        # turn-read / turn-publish already pin theirs).
        rows = [row for row in nc.mutations if row[0] != DROPPED_ROW]
        code, out, err = run_check(rows=rows)
        self.assertTrue(code == 2,
                        f'#2692 fix-r11: a dropped roster row must refuse --check: '
                        f'rc={code} out={out!r}')
        self.assertIn('mutation roster drifted', err,
                      '#2692 fix-r11: the refusal names the roster drift')
        self.assertIn(f'missing=[{DROPPED_ROW!r}]', err,
                      f'#2692 fix-r11: the refusal names the missing row: {err!r}')

    def test_check_rejects_a_stale_anchor(self):
        # fix-r11 (review sibling#2): mutate()'s count != 1 leg had no
        # in-tree red side -- the gate could regress to "always 1" and only
        # the host's cargo round would notice.
        rows = patched_rows(STALE_ROW,
                            old='pub(crate) const MAX_ARRAY_DEPTH: usize = 4242;')
        code, out, err = run_check(rows=rows)
        self.assertTrue(code == 2,
                        f'#2692 fix-r11: a stale anchor must refuse --check: '
                        f'rc={code} out={out!r}')
        self.assertIn(f'#2692 {STALE_ROW}: stale anchor (expected 1, got 0)', err,
                      '#2692 fix-r11: the refusal names the row and the count')

    def test_check_rejects_a_no_op_mutation(self):
        # fix-r11 (review sibling#2): before == after used to stay green
        # until the host's cargo round reported SURVIVED; blankkey folds the
        # leg into mutate()'s stale-anchor refusal, so the count reads 1.
        rows = patched_rows(STALE_ROW,
                            new='pub(crate) const MAX_ARRAY_DEPTH: usize = 2000;')
        code, out, err = run_check(rows=rows)
        self.assertTrue(code == 2,
                        f'#2692 fix-r11: a no-op mutation must refuse --check: '
                        f'rc={code} out={out!r}')
        self.assertIn(f'#2692 {STALE_ROW}: stale anchor (expected 1, got 1)', err,
                      '#2692 fix-r11: the refusal names the row and the no-op shape')

    def test_check_rejects_duplicate_roster_names(self):
        # fix-r12 (review sibling P2, the turn-read :126 / turn-publish :125
        # shape): two rows sharing a name silently overwrite each other's
        # evidence logs in the mutating run; the duplicate gets its own
        # refusal instead of the raw roster-diff list.
        rows = [list(row) for row in nc.mutations]
        for row in rows:
            if row[0] == DROPPED_ROW:
                row[0] = STALE_ROW
                break
        code, out, err = run_check(rows=rows)
        self.assertTrue(code == 2,
                        f'#2692 fix-r12: a duplicate roster name must refuse '
                        f'--check: rc={code} out={out!r}')
        self.assertIn('#2692 the mutation roster has duplicate names', err,
                      '#2692 fix-r12: the refusal names the duplicate shape')

    def test_check_refuses_a_new_shared_row_left_unpinned(self):
        # closure-r2 (MM concurrency #1, P1): the forward pin loop only
        # catches a pinned row that disappeared. A row ADDED with a sibling's
        # assertion string and no ROW_PINS entry is the one drift nothing
        # guarded after the structural census left (closure-r1). The twin
        # reuses `submit-owner-uses-template-key`'s assertion on both tables
        # (the roster stays aligned, so `registered()` passes), and the
        # reverse check must refuse and name exactly the twin.
        source = next(row for row in nc.mutations
                      if row[0] == 'submit-owner-uses-template-key')
        twin = list(source)
        twin[0] = 'submit-owner-uses-template-key-twin'
        rows = list(nc.mutations) + [tuple(twin)]
        names = tuple(nc.EXPECTED_NAMES) + (twin[0],)
        code, out, err = run_check(rows=rows, expected_names=names)
        self.assertTrue(code == 2,
                        f'#2692 closure-r2: an unpinned row reusing a shared '
                        f'assertion must refuse --check: rc={code} out={out!r}')
        self.assertIn('#2692 native-blankkey-submit shared-assertion rows '
                      f'missing from ROW_PINS: {twin[0]}', err,
                      '#2692 closure-r2: the refusal names the family and '
                      'the unpinned twin')

    def test_check_refuses_a_row_dropped_from_both_tables(self):
        # fix-r12 (review MM boundary P1): the fix-r11 drift leg only dropped
        # the runtime row, which `registered()` alone catches; a real
        # deletion edits both sides at once (the comparison forces
        # alignment). For the six shared-assertion rows that drift used to
        # stay green -- every pin stayed bound through the sibling row that
        # asserts the same verbatim string -- so the row pins must name the
        # row itself. `submit-raw-spelling` is asserted separately from its
        # string-mate `registry-body-spelling`, the pair the review named.
        for dropped in nc.ROW_PINS:
            rows = [row for row in nc.mutations if row[0] != dropped]
            names = tuple(name for name in nc.EXPECTED_NAMES if name != dropped)
            code, out, err = run_check(rows=rows, expected_names=names)
            self.assertTrue(code == 2,
                            f'#2692 fix-r12: dropping {dropped} from both '
                            f'tables must refuse --check: rc={code} out={out!r}')
            self.assertIn(f'#2692 row pin names a row that is gone: {dropped}', err,
                          f'#2692 fix-r12: the refusal names the dropped row: {err!r}')


if __name__ == '__main__':
    unittest.main()

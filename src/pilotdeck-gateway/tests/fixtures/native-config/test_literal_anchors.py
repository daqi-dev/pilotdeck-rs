#!/usr/bin/env python3
# anchor-selfcheck: unsupported unit tests of the fixture's own literal-anchor guard; no product source mutation table
"""#2065: red-side tests for the fixture's own literal-anchor guard.

`check_literal_anchors` refuses a run whose literal anchor is verbatim source
text yet unregistered, or a pin no row asserts; a guard with no red side is
itself unverified (same contract as `session-assembly-1316
--self-check-controls`: every guard must be shown to turn red). Each case below
breaks exactly one invariant of `check_literal_anchors` and demands the matching
refusal; the first case runs the real `--check` and pins its output counts.

Run from the repo root:

    python3 -m unittest -v src/pilotdeck-gateway/tests/fixtures/native-config/test_literal_anchors.py
"""
import dataclasses
import importlib.util
import os
import subprocess
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve()
SPEC = importlib.util.spec_from_file_location(
    'native_config_negative_controls', HERE.with_name('negative_controls.py'))
nc = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(nc)

ROWS = [nc.Case(*row) for row in nc.mutations]
BAD_ROW = 'permission-mode-gate-removed'
# Dropping this row leaves every pin bound through a sibling row (proven by
# simulation), so with the roster guard broken the refusal below would not
# come from `check_literal_anchors` either -- the drift test isolates the
# static-roster refusal, not a knock-on pin refusal.
DROPPED_ROW = 'transcript-live-bypass-config'


def check(cases=None, pins=None, bound_rows=None):
    """Run the guard with optional mutated rows / pinned table / binding rows."""
    nc.check_literal_anchors(ROWS if cases is None else cases, pins, bound_rows)


def replace_row(field, value, mutation=BAD_ROW):
    return [dataclasses.replace(case, **{field: value})
            if case.mutation == mutation else case for case in ROWS]


class LiteralAnchorGuard(unittest.TestCase):
    def assertRefused(self, *needles, **kwargs):
        with self.assertRaises(SystemExit) as caught:
            check(**kwargs)
        for needle in needles:
            self.assertIn(needle, str(caught.exception))

    def test_check_passes_and_reports_all_three_counts(self):
        done = subprocess.run(
            [sys.executable, '-I', str(HERE.with_name('negative_controls.py')), '--check'],
            cwd=nc.ROOT, capture_output=True, text=True)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn(f'#1457 {len(ROWS)} mutation anchors OK', done.stdout)
        self.assertIn(f'#2065 {len(nc.LITERAL_ANCHORS)} literal anchors OK', done.stdout)
        # #2692 fix-r9 (review boundary#2): pin the third count, which is now
        # what the bound-test gate actually passed -- deleting its loop or
        # truncating the rows it audits changes the printed count and this
        # positive goes red (it used to re-derive the count from the input
        # table, so only the rename leg caught a deletion).
        self.assertIn(f'{len(ROWS)} bound tests declared', done.stdout,
                      '#2692 fix-r9: the bound-test declaration count must be reported')

    def test_rejects_the_regression(self):
        # The exact #2065 shape: the row goes back to the invented suffix while
        # the table keeps the source-verbatim prefix.
        self.assertRefused('unbound to any row',
                           cases=replace_row('assertion',
                                             '#1457 set_permission_mode rejects unknown mode'))

    def test_rejects_pin_missing_from_source(self):
        pins = [(nc.CONFIG, '#1457 message nobody wrote')] + nc.LITERAL_ANCHORS[1:]
        self.assertRefused('not in config.rs', pins=pins)

    def test_rejects_pin_outside_the_scanned_set(self):
        pins = nc.LITERAL_ANCHORS + [(HERE.with_name('README.md'), '#1457 busy message')]
        self.assertRefused('outside SOURCE_TEXTS', pins=pins)

    def test_rejects_pin_without_a_row(self):
        pins = [(nc.CONFIG, '#1457 set_permission_mode rejects {label}')] + nc.LITERAL_ANCHORS[1:]
        self.assertRefused('unbound to any row', pins=pins)

    def test_rejects_unpinned_literal_anchor(self):
        pins = [pin for pin in nc.LITERAL_ANCHORS if pin[1] != '#1457 exact mtime applies']
        self.assertRefused('unpinned literal anchor', pins=pins)

    def test_rejects_unpinned_anchor_in_a_test_file(self):
        pins = [pin for pin in nc.LITERAL_ANCHORS if pin[0] is not nc.LIVE_TESTS]
        self.assertRefused('unpinned literal anchor', pins=pins)

    def test_rejects_dropped_second_registration(self):
        # `the transcript still loads` is verbatim in two scanned files; a pin
        # for one copy must not vouch for the other.
        pins = [pin for pin in nc.LITERAL_ANCHORS
                if pin != (nc.SESSION_DIR, '#1457 the transcript still loads')]
        self.assertRefused('unpinned literal anchor', 'session_dir.rs', pins=pins)

    def test_rejects_row_outside_the_scanned_set(self):
        self.assertRefused('outside SOURCE_TEXTS',
                           cases=replace_row('path', HERE.with_name('README.md')))

    def test_subset_is_bound_against_the_full_table(self):
        # A targeted rerun validates a few rows but still owns the whole table:
        # judging the subset alone would refuse every other pin as unbound.
        check(cases=[ROWS[0]], bound_rows=ROWS)
        self.assertRefused('unbound to any row', cases=[ROWS[0]])

    def test_main_check_refuses_a_dropped_roster_row(self):
        # fix-r10 (review boundary#1): the printed anchor count and this
        # file's expectations all used to derive from the same runtime table,
        # so a dropped row kept --check green while one control silently
        # stopped being audited; the static EXPECTED_NAMES roster must refuse
        # the drift before any count is printed.
        rows = [row for row in nc.mutations if row[0] != DROPPED_ROW]
        saved_argv, saved_rows = sys.argv, nc.mutations
        nc.mutations = rows
        try:
            sys.argv = ['negative_controls.py', '--check']
            with self.assertRaises(SystemExit,
                                   msg='#2692 fix-r10: a dropped roster row '
                                       'must refuse --check') as caught:
                nc.main()
        finally:
            nc.mutations = saved_rows
            sys.argv = saved_argv
        message = str(caught.exception)
        self.assertIn('mutation roster drifted', message,
                      '#2692 fix-r10: the refusal names the drift')
        self.assertIn(f'missing=[{DROPPED_ROW!r}]', message,
                      f'#2692 fix-r10: the refusal names the missing row: {message!r}')

    def test_check_row_pins_refuses_a_new_shared_row_left_unpinned(self):
        # closure-r2 (MM concurrency #1, P1): the forward pin loop only
        # catches a pinned row that disappeared. A row ADDED with a sibling's
        # assertion string and no ROW_PINS entry is the one drift nothing
        # guarded after the structural census left (closure-r1) -- the twin
        # below reuses `permission-stub-revert`'s assertion, so the reverse
        # check must refuse and name exactly the twin.
        source = next(case for case in ROWS
                      if case.mutation == 'permission-stub-revert')
        twin = dataclasses.replace(source,
                                   mutation='permission-stub-revert-twin')
        with self.assertRaises(SystemExit,
                               msg='#2692 closure-r2: an unpinned row '
                                   'reusing a shared assertion must '
                                   'refuse the run') as caught:
            nc.check_row_pins(ROWS + [twin])
        self.assertIn('#2692 native-config shared-assertion rows missing '
                      'from ROW_PINS: permission-stub-revert-twin',
                      str(caught.exception),
                      '#2692 closure-r2: the refusal names the family and '
                      'the unpinned twin')

    def test_check_row_pins_counts_a_list_assertion_per_element(self):
        # closure-r3 (DS sibling#1 + concurrency#1 + boundary#1, three-way
        # P2): the reverse check counted `case.assertion` directly, so a row
        # whose assertion is a list -- a shape `Case.assertion` supports and
        # `check_literal_anchors` already iterates per element -- aborted
        # with `TypeError: unhashable type: 'list'` instead of refusing, and
        # per-element sharing never judged. The twin below reuses
        # `permission-stub-revert`'s shared assertion as one list element;
        # the reverse check must refuse and name exactly the twin.
        source = next(case for case in ROWS
                      if case.mutation == 'permission-stub-revert')
        twin = dataclasses.replace(
            source, mutation='permission-stub-revert-list-twin',
            assertion=[source.assertion, '#1872 an unshared list rider'])
        with self.assertRaises(SystemExit,
                               msg='#2692 closure-r3: a list assertion '
                                   'sharing an element must refuse the '
                                   'run, not crash') as caught:
            nc.check_row_pins(ROWS + [twin])
        self.assertIn('#2692 native-config shared-assertion rows missing '
                      'from ROW_PINS: permission-stub-revert-list-twin',
                      str(caught.exception),
                      '#2692 closure-r3: the refusal names the family and '
                      'the unpinned list twin')

    def test_main_check_refuses_a_list_assertion_row_cleanly(self):
        # closure-r3: the same injection through the real --check envelope
        # must be a clean refusal -- stderr names the row, the process exits
        # nonzero, and no traceback reaches the operator. The roster stays
        # aligned (the twin joins EXPECTED_NAMES), so the refusal can only
        # come from check_row_pins, after the roster guard.
        # closure-r4 (DS simplify#2): the twin is built with
        # dataclasses.replace + astuple, never positional tuple indices --
        # the driver used to write twin[5] blind, so a field inserted before
        # `assertion` (Case fields are mutation,path,old,new,test,assertion,
        # scope; scope already carries a default) would mutate the wrong
        # column and this leg would stay green.
        driver = (
            "import dataclasses, importlib.util, sys\n"
            "spec = importlib.util.spec_from_file_location('nc', sys.argv[1])\n"
            "nc = importlib.util.module_from_spec(spec)\n"
            "spec.loader.exec_module(nc)\n"
            "source = next(row for row in nc.mutations\n"
            "              if row[0] == 'permission-stub-revert')\n"
            "case = nc.Case(*source)\n"
            "twin = dataclasses.replace(\n"
            "    case, mutation='permission-stub-revert-list-twin',\n"
            "    assertion=[case.assertion, '#1872 an unshared list rider'])\n"
            "nc.mutations = list(nc.mutations) + [dataclasses.astuple(twin)]\n"
            "nc.EXPECTED_NAMES = tuple(nc.EXPECTED_NAMES) + (twin.mutation,)\n"
            "sys.argv = ['negative_controls.py', '--check']\n"
            "nc.main()\n")
        done = subprocess.run(
            [sys.executable, '-I', '-c', driver,
             str(HERE.with_name('negative_controls.py'))],
            cwd=nc.ROOT, capture_output=True, text=True)
        # The family envelope raises SystemExit(str), which the interpreter
        # turns into rc 1 (native-config's documented pre-guard failure code,
        # `run_cases` shape) -- the blankkey/turn-submit refuse() rc 2 does
        # not apply to this family.
        self.assertEqual(done.returncode, 1,
                          f'#2692 closure-r3: the list twin must refuse '
                          f'--check: rc={done.returncode} out={done.stdout!r}')
        self.assertNotIn('Traceback', done.stderr,
                         '#2692 closure-r3: the refusal must be clean, '
                         'not a traceback')
        self.assertIn('permission-stub-revert-list-twin', done.stderr,
                      '#2692 closure-r3: the refusal names the list twin')
        self.assertIn('shared-assertion rows missing from ROW_PINS',
                      done.stderr,
                      '#2692 closure-r3: the refusal is the pin-table one')

    def test_main_check_refuses_an_empty_assertion_list_row(self):
        # closure-r4 (DS boundary#1, P2-a): after closure-r3 made every
        # consumer iterate per element, an `assertion=[]` row produced no
        # anchors -- it joined no count, no reverse pin and no literal-anchor
        # loop, so the real --check stayed green (rc 0, the row counted in;
        # the pre-fix false green is recorded in closure-r4's RESULT) while
        # the kill round could only ever read it INVALID. The accessor now
        # refuses the empty-list shape (and the non-str shapes that would
        # TypeError one line later) before any count prints. Same envelope
        # and family exit code as the list twin above.
        driver = (
            "import dataclasses, importlib.util, sys\n"
            "spec = importlib.util.spec_from_file_location('nc', sys.argv[1])\n"
            "nc = importlib.util.module_from_spec(spec)\n"
            "spec.loader.exec_module(nc)\n"
            "source = next(row for row in nc.mutations\n"
            "              if row[0] == 'permission-stub-revert')\n"
            "twin = dataclasses.replace(nc.Case(*source),\n"
            "                           mutation='permission-stub-revert-empty-twin',\n"
            "                           assertion=[])\n"
            "nc.mutations = list(nc.mutations) + [dataclasses.astuple(twin)]\n"
            "nc.EXPECTED_NAMES = tuple(nc.EXPECTED_NAMES) + (twin.mutation,)\n"
            "sys.argv = ['negative_controls.py', '--check']\n"
            "nc.main()\n")
        done = subprocess.run(
            [sys.executable, '-I', '-c', driver,
             str(HERE.with_name('negative_controls.py'))],
            cwd=nc.ROOT, capture_output=True, text=True)
        self.assertEqual(done.returncode, 1,
                         f'#2692 closure-r4: the empty-list twin must refuse '
                         f'--check: rc={done.returncode} out={done.stdout!r}')
        self.assertNotIn('Traceback', done.stderr,
                         '#2692 closure-r4: the refusal must be clean, '
                         'not a traceback')
        self.assertIn('#2692 empty assertion list: '
                      'permission-stub-revert-empty-twin',
                      done.stderr,
                      '#2692 closure-r4: the refusal names the empty-list twin')
        self.assertNotIn('mutation anchors OK', done.stdout,
                         '#2692 closure-r4: no count may print for a refused '
                         'row (the pre-fix false green printed 132)')

    def test_check_refuses_a_row_dropped_from_both_tables(self):
        # #2692 fix-r13 (DS + MM sibling/boundary, both families): a row whose
        # assertion string another row also carries used to survive a
        # both-tables drop -- `registered()` stayed aligned and the surviving
        # sibling kept every pin bound, so --check stayed green while one
        # control silently stopped being audited. ROW_PINS is a third
        # denominator outside both tables; every pin in turn must refuse the
        # drop (the native-blankkey fix-r12 red-side shape).
        for dropped in nc.ROW_PINS:
            rows = [row for row in nc.mutations if row[0] != dropped]
            names = [name for name in nc.EXPECTED_NAMES if name != dropped]
            saved_argv, saved_rows, saved_names = sys.argv, nc.mutations, nc.EXPECTED_NAMES
            nc.mutations, nc.EXPECTED_NAMES = rows, names
            try:
                sys.argv = ['negative_controls.py', '--check']
                with self.assertRaises(SystemExit,
                                      msg=f'#2692 fix-r13: a both-tables drop '
                                          f'of {dropped} must refuse --check') as caught:
                    nc.main()
            finally:
                nc.mutations, nc.EXPECTED_NAMES = saved_rows, saved_names
                sys.argv = saved_argv
            self.assertIn(f'#2692 row pin names a row that is gone: {dropped}',
                          str(caught.exception),
                          f'#2692 fix-r13: the refusal names the dropped row '
                          f'{dropped}')

    def test_main_binds_a_targeted_run_to_the_whole_table(self):
        seen = {}
        real = nc.check_literal_anchors

        def spy(cases, pins=None, bound_rows=None):
            seen['cases'] = list(cases)
            seen['bound'] = list(bound_rows or ())
            raise SystemExit('stop before cargo')

        saved_argv = sys.argv
        saved_target = os.environ.get('CARGO_TARGET_DIR')
        nc.check_literal_anchors = spy
        try:
            sys.argv = ['negative_controls.py', '/tmp/never-created', BAD_ROW]
            os.environ['CARGO_TARGET_DIR'] = '/tmp/never-target'
            with self.assertRaises(SystemExit):
                nc.main()
        finally:
            nc.check_literal_anchors = real
            sys.argv = saved_argv
            if saved_target is None:
                os.environ.pop('CARGO_TARGET_DIR', None)
            else:
                os.environ['CARGO_TARGET_DIR'] = saved_target
        self.assertEqual([case.mutation for case in seen['cases']], [BAD_ROW])
        self.assertEqual(len(seen['bound']), len(ROWS))

    def test_main_check_rejects_a_renamed_bound_test(self):
        # #2692 fix-r8 (review sibling#2): the --check bound-test gate's
        # in-tree red side. Renaming the short name simulates the fn being
        # renamed in the tests file (the roster keeps the old spelling), so
        # the real declares_test matching runs and must come back empty.
        real = nc.bind_module_file

        def renamed(test, modules):
            path, short = real(test, modules)
            return path, short + '_renamed'

        saved_argv = sys.argv
        nc.bind_module_file = renamed
        try:
            sys.argv = ['negative_controls.py', '--check']
            with self.assertRaises(SystemExit,
                                   msg='#2692 a renamed bound test must fail --check') as caught:
                nc.main()
        finally:
            nc.bind_module_file = real
            sys.argv = saved_argv
        self.assertIn('#1457 bound test not declared', str(caught.exception),
                      '#2692 fix-r8: the rename refusal message names the drifted binding')

    def test_main_check_prefixes_a_stale_anchor_once(self):
        # fix-r8 (concurrency#2): the --check envelope owns the `#1457` tag;
        # the stale-anchor ValueError used to carry its own and the refusal
        # printed the tag twice.
        rows = [list(row) for row in nc.mutations]
        rows[0][2] = 'anchor nobody ships in config.rs'
        saved_argv, saved_rows = sys.argv, nc.mutations
        nc.mutations = rows
        try:
            sys.argv = ['negative_controls.py', '--check']
            with self.assertRaises(SystemExit,
                                   msg='#2692 a stale anchor must refuse --check') as caught:
                nc.main()
        finally:
            nc.mutations = saved_rows
            sys.argv = saved_argv
        message = str(caught.exception)
        # fix-r9 (concurrency#2): the mutation name is read off the row this
        # test just corrupted -- reordering the table must not turn this into a
        # confusing false red naming a different mutation.
        self.assertIn(f'{rows[0][0]}: stale anchor (expected 1, got 0)', message,
                      '#2692 fix-r8: the stale-anchor refusal shape is unchanged')
        self.assertEqual(message.count('#1457'), 1,
                         f'concurrency#2: the tag must appear exactly once: {message!r}')

    def test_main_check_rejects_a_no_op_mutation(self):
        # fix-r12 (review sibling P2, the r11 blankkey shape): before ==
        # after used to stay green until the host's cargo round reported
        # SURVIVED; native-config folds the leg into mutate()'s
        # stale-anchor refusal, so the count reads 1.
        rows = [list(row) for row in nc.mutations]
        rows[0][3] = rows[0][2]
        saved_argv, saved_rows = sys.argv, nc.mutations
        nc.mutations = rows
        try:
            sys.argv = ['negative_controls.py', '--check']
            with self.assertRaises(SystemExit,
                                   msg='#2692 fix-r12: a no-op mutation must '
                                       'refuse --check') as caught:
                nc.main()
        finally:
            nc.mutations = saved_rows
            sys.argv = saved_argv
        message = str(caught.exception)
        self.assertIn(f'{rows[0][0]}: stale anchor (expected 1, got 1)', message,
                      '#2692 fix-r12: the refusal names the row and the '
                      'no-op shape (count reads 1)')
        self.assertEqual(message.count('#1457'), 1,
                         f'#2692 fix-r12: the envelope owns the tag exactly '
                         f'once: {message!r}')


if __name__ == '__main__':
    unittest.main()

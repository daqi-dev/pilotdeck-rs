#!/usr/bin/env python3
# anchor-selfcheck: unsupported unit tests of the fixture's own literal-anchor guard; no product source mutation table
"""#2392 fix-r12 (DS + MM review, same entry both lineages): red-side tests for
the fixture's own literal-anchor guard.

`check_literal_anchors` refuses a run whose literal anchor is verbatim source
text yet unregistered, or a pin no row asserts; until fix-r12 this guard had
no red side in this tree -- only the host's out-of-tree round ever showed it
red, so a regression that turned the guard into a no-op would have kept
--check green (the same contract native-blankkey / native-config carry:
every guard must be shown to turn red). Each case below breaks exactly one
invariant and demands the matching refusal. This fixture's signature is
`(cases, bound_rows)` with the pin table a module global, so the pin-level
cases swap `nc.LITERAL_ANCHORS` around the call instead of passing `pins`.
Run from anywhere:

    python3 -I -B src/pilotdeck-gateway/tests/fixtures/session-delete-2392/test_literal_anchors.py
"""
import contextlib
import dataclasses
import importlib.util
import io
import os
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve()
SPEC = importlib.util.spec_from_file_location(
    'delete_negative_controls', HERE.with_name('negative_controls.py'))
nc = importlib.util.module_from_spec(SPEC)
# The module name is never `__main__`, so the `if __name__` guard keeps main()
# inert here; loading only defines the tables and the functions.
SPEC.loader.exec_module(nc)

ROWS = [nc.Case(*row) for row in nc.mutations]
BAD_ROW = 'mcp-clear-removed'


def check(cases=None, bound_rows=None):
    """Run the guard with optional row subset / binding rows."""
    nc.check_literal_anchors(ROWS if cases is None else cases, bound_rows)


def replace_row(field, value, mutation=BAD_ROW):
    return [dataclasses.replace(case, **{field: value})
            if case.mutation == mutation else case for case in ROWS]


class LiteralAnchorGuard(unittest.TestCase):
    def assertRefused(self, *needles, pins=None, cases=None, bound_rows=None,
                      why='#2392 fix-r12: check_literal_anchors must refuse '
                          'the broken table'):
        """Run the guard expecting the shared refusal (rc 2, stderr text)."""
        err = io.StringIO()
        saved_pins = nc.LITERAL_ANCHORS
        if pins is not None:
            nc.LITERAL_ANCHORS = pins
        try:
            with contextlib.redirect_stderr(err):
                check(cases=cases, bound_rows=bound_rows)
        except SystemExit as stop:
            self.assertTrue(stop.code == 2,
                           f'#2392 fix-r12: the refusal must be a clean rc 2: '
                           f'code={stop.code!r} stderr={err.getvalue()!r}')
            for needle in needles:
                self.assertIn(needle, err.getvalue(),
                              f'#2392 fix-r12: the refusal must say {needle!r}: '
                              f'{err.getvalue()!r}')
            return
        finally:
            nc.LITERAL_ANCHORS = saved_pins
        raise AssertionError(f'{why} (no SystemExit raised)')

    def test_guard_passes_on_the_pristine_table(self):
        try:
            check()
        except SystemExit as stop:
            self.fail(f'#2392 fix-r12: the guard must pass on the pristine '
                      f'table: rc={stop.code}')

    def test_rejects_a_row_outside_the_scanned_set(self):
        self.assertRefused('row mutates a file outside SOURCE_TEXTS',
                           cases=replace_row('path', HERE.with_name('README.md')))

    def test_rejects_a_pin_outside_the_scanned_set(self):
        pins = list(nc.LITERAL_ANCHORS) + [(HERE.with_name('README.md'),
                                            '#2392 busy message')]
        self.assertRefused('literal anchor declared outside SOURCE_TEXTS',
                           pins=pins)

    def test_rejects_a_pin_missing_from_source(self):
        pins = [(nc.SESSION_DIR, '#2392 a pin nobody ships in session_dir.rs')]
        pins += list(nc.LITERAL_ANCHORS)[1:]
        self.assertRefused('literal anchor not in', pins=pins)

    def test_rejects_a_pin_unbound_to_any_row(self):
        # `clear_sdk_session_state` is verbatim in session_dir.rs (the wiring
        # the rows mutate) but no row asserts it, so the pin is in the source
        # yet unbound.
        pins = [(nc.SESSION_DIR, 'clear_sdk_session_state')]
        pins += list(nc.LITERAL_ANCHORS)[1:]
        self.assertRefused('literal anchor unbound to any row', pins=pins)

    def test_rejects_an_unpinned_row_assertion(self):
        # Drop the pin for BAD_ROW's assertion while the row keeps asserting
        # it: the completeness scan must refuse the now-unpinned verbatim.
        pin = (nc.SESSION_DIR,
               "#2392 delete drops the session's MCP permission overrides")
        pins = [item for item in nc.LITERAL_ANCHORS if item != pin]
        self.assertRefused('unpinned literal anchor', 'session_dir.rs', pins=pins)

    def test_subset_is_bound_against_the_full_table(self):
        # A targeted run validates a few rows but still owns the whole pin
        # table: judging the subset alone would refuse every other pin as
        # unbound (the #2065 r5 review P1 shape native-config pins too).
        try:
            check(cases=[ROWS[0]], bound_rows=ROWS)
        except SystemExit as stop:
            self.fail(f'#2392 fix-r12: a subset rerun must stay bound against '
                      f'the full table: rc={stop.code}')
        self.assertRefused('literal anchor unbound to any row', cases=[ROWS[0]])

    def assert_main_check_refused(self, rows, *needles):
        """Run main() --check over drifted rows and demand the rc 2 refusal."""
        err = io.StringIO()
        saved_argv, saved_rows = sys.argv, nc.mutations
        nc.mutations = rows
        try:
            sys.argv = ['negative_controls.py', '--check']
            with contextlib.redirect_stderr(err):
                with self.assertRaises(SystemExit,
                                       msg='#2392 the drifted table must '
                                           'refuse --check') as caught:
                    nc.main()
        finally:
            nc.mutations = saved_rows
            sys.argv = saved_argv
        self.assertTrue(caught.exception.code == 2,
                        f'#2392 fix-r13: the refusal must be a clean rc 2: '
                        f'code={caught.exception.code!r}')
        for needle in needles:
            self.assertIn(needle, err.getvalue(),
                          f'#2392 fix-r13: the refusal must say {needle!r}: '
                          f'{err.getvalue()!r}')
        return err.getvalue()

    def test_main_check_refuses_a_stale_anchor(self):
        # #2392 fix-r13 (review P2, the native-config fix-r8 shape): the
        # --check envelope's own legs. Until now only the guard-level cases
        # above existed -- a main() that stopped calling mutate() kept
        # --check green, and the tag is asserted exactly once because the
        # envelope used to print it a second time.
        rows = [list(row) for row in nc.mutations]
        rows[0][2] = '#2392 an anchor nobody ships in the scanned sources'
        message = self.assert_main_check_refused(
            rows, f'{rows[0][0]}: stale anchor (expected 1, got 0)')
        self.assertEqual(message.count('#2392'), 1,
                         f'fix-r13: the tag must appear exactly once: {message!r}')

    def test_main_check_rejects_a_no_op_mutation(self):
        # #2392 fix-r13 (review P2, the native-config fix-r12 shape):
        # before == after used to stay green until the host's cargo round
        # reported SURVIVED; the fixture folds the leg into mutate()'s
        # stale-anchor refusal, so the count reads 1.
        rows = [list(row) for row in nc.mutations]
        rows[0][3] = rows[0][2]
        message = self.assert_main_check_refused(
            rows, f'{rows[0][0]}: stale anchor (expected 1, got 1)')
        self.assertEqual(message.count('#2392'), 1,
                         f'fix-r13: the tag must appear exactly once: {message!r}')

    def test_main_check_refuses_an_empty_assertion_list_row(self):
        # closure-r7 (DS r5r6 boundary#2 + sibling#1 + concurrency#1, P2): the
        # closure-r5 sync added the three-shape `_assertions` guard to this
        # family but not one red leg -- the guard could be deleted wholesale
        # and this file stayed green, against the in-file contract above
        # (every guard must be shown to turn red). Each leg below breaks
        # exactly one shape through the real --check envelope, rewriting the
        # row-0 assertion column in place (the roster stays aligned, so the
        # refusal can only come from the accessor inside
        # check_literal_anchors; the blankkey closure-r5 needle caliber).
        # Empty list: the row joins no anchor loop, the pre-fix --check
        # counted the row in and stayed green while the kill round could
        # only ever read it INVALID.
        rows = [list(row) for row in nc.mutations]
        rows[0][5] = []
        self.assert_main_check_refused(
            rows, f'#2392 empty assertion list: {rows[0][0]}')

    def test_main_check_refuses_a_non_str_assertion_element_row(self):
        # closure-r7: the non-str element shape (the row's own assertion
        # plus an int rider). Pre-guard this raised a bare TypeError inside
        # `assertion in text` instead of the clean refusal.
        rows = [list(row) for row in nc.mutations]
        rows[0][5] = [rows[0][5], 3]
        self.assert_main_check_refused(
            rows, f'#2392 non-str assertion element: {rows[0][0]}')

    def test_main_check_refuses_a_non_str_assertion_scalar_row(self):
        # closure-r7: the third shape -- the assertion column replaced by a
        # non-str scalar. Same needle caliber as the two legs above.
        rows = [list(row) for row in nc.mutations]
        rows[0][5] = 3
        self.assert_main_check_refused(
            rows, f'#2392 non-str assertion: {rows[0][0]}')

    def test_main_binds_a_targeted_run_to_the_whole_table(self):
        # #2392 fix-r13 (review P2, the native-config wiring shape): the
        # guard-level cases above call check_literal_anchors directly, so a
        # main() that stopped wiring it in (or narrowed its bound_rows to the
        # selection) kept every one of them green. The spy records what main
        # actually passed and stops before any cargo; it must really have
        # been called -- a silently unwired main() fails the seen-assertions.
        seen = {}
        real = nc.check_literal_anchors

        def spy(cases, bound_rows=None):
            seen['cases'] = [case.mutation for case in cases]
            seen['bound'] = [case.mutation for case in (bound_rows or ())]
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
        self.assertTrue(seen, '#2392 fix-r13: main() never called the guard')
        self.assertEqual(seen['cases'], [BAD_ROW],
                         '#2392 fix-r13: the selection must reach the guard')
        self.assertEqual(seen['bound'], [case.mutation for case in ROWS],
                         '#2392 fix-r13: a targeted run must stay bound '
                         'against the whole table')


if __name__ == '__main__':
    unittest.main()

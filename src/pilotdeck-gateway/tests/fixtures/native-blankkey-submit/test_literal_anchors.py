#!/usr/bin/env python3
# anchor-selfcheck: unsupported unit tests of the fixture's own literal-anchor guard; no product source mutation table
"""#2692 fix-r10 (review sibling#1): red-side tests for the fixture's own literal-anchor guard.

`check_literal_anchors` refuses a run whose literal anchor is verbatim source
text yet unregistered, or a pin no row asserts; fix-r9 only gave the same
file's bound-test gate a red side, so this guard itself stayed unverified
(same contract as `native-config/test_literal_anchors.py`: every guard must be
shown to turn red). Each case below breaks exactly one invariant of
`check_literal_anchors` and demands the matching refusal. This fixture's
signature is `(cases, bound_rows)` with the pin table a module global, so the
pin-level cases swap `nc.LITERAL_ANCHORS` around the call instead of passing
`pins`. Run from anywhere:

    python3 -I -B src/pilotdeck-gateway/tests/fixtures/native-blankkey-submit/test_literal_anchors.py
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
    'blankkey_negative_controls', HERE.with_name('negative_controls.py'))
nc = importlib.util.module_from_spec(SPEC)
# The module name is never `__main__`, so the `if __name__` guard keeps main()
# inert here; loading only defines the tables and the functions.
SPEC.loader.exec_module(nc)

ROWS = [nc.Case(*row) for row in nc.mutations]
BAD_ROW = 'permission-gate-string-only'


def check(cases=None, bound_rows=None):
    """Run the guard with optional row subset / binding rows."""
    nc.check_literal_anchors(ROWS if cases is None else cases, bound_rows)


def replace_row(field, value, mutation=BAD_ROW):
    return [dataclasses.replace(case, **{field: value})
            if case.mutation == mutation else case for case in ROWS]


class LiteralAnchorGuard(unittest.TestCase):
    def assertRefused(self, *needles, pins=None, cases=None, bound_rows=None,
                      why='#2692 fix-r10: check_literal_anchors must refuse '
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
                           f'#2692 fix-r10: the refusal must be a clean rc 2: '
                           f'code={stop.code!r} stderr={err.getvalue()!r}')
            for needle in needles:
                self.assertIn(needle, err.getvalue(),
                              f'#2692 fix-r10: the refusal must say {needle!r}: '
                              f'{err.getvalue()!r}')
            return
        finally:
            nc.LITERAL_ANCHORS = saved_pins
        raise AssertionError(f'{why} (no SystemExit raised)')

    def test_guard_passes_on_the_pristine_table(self):
        try:
            check()
        except SystemExit as stop:
            self.fail(f'#2692 fix-r10: the guard must pass on the pristine '
                      f'table: rc={stop.code}')

    def test_rejects_a_row_outside_the_scanned_set(self):
        self.assertRefused('row mutates a file outside SOURCE_TEXTS',
                           cases=replace_row('path', HERE.with_name('README.md')))

    def test_rejects_a_pin_outside_the_scanned_set(self):
        pins = list(nc.LITERAL_ANCHORS) + [(HERE.with_name('README.md'),
                                            '#2692 busy message')]
        self.assertRefused('literal anchor declared outside SOURCE_TEXTS',
                           pins=pins)

    def test_rejects_a_pin_missing_from_source(self):
        pins = [(nc.GATES, '#2692 a pin nobody ships in config.rs')]
        pins += list(nc.LITERAL_ANCHORS)[1:]
        self.assertRefused('literal anchor not in', pins=pins)

    def test_rejects_a_pin_unbound_to_any_row(self):
        # `un-rekeyable record(s)` is verbatim in run_registry.rs (the fix-r5
        # stderr line) but no row asserts it, so the pin is in the source yet
        # unbound.
        pins = [(nc.REGISTRY, 'un-rekeyable record(s)')]
        pins += list(nc.LITERAL_ANCHORS)[1:]
        self.assertRefused('literal anchor unbound to any row', pins=pins)

    def test_rejects_an_unpinned_row_assertion(self):
        # Drop the pin for BAD_ROW's assertion while the row keeps asserting
        # it: the completeness scan must refuse the now-unpinned verbatim.
        pins = [pin for pin in nc.LITERAL_ANCHORS
                if pin != (nc.GATES, '#2692 permission cell F code')]
        self.assertRefused('unpinned literal anchor', 'config.rs', pins=pins)

    def test_rejects_dropped_second_registration(self):
        # fix-r11 (review boundary#2): `#2692 the record body keeps the raw
        # JSON null` is verbatim in two scanned files (config.rs and
        # run_registry_tests.rs); a pin for one copy must not vouch for the
        # other -- matching the anchor set alone would keep --check green
        # while the run_registry_tests.rs copy rode on config.rs's pin (the
        # native-config test_rejects_dropped_second_registration shape).
        pins = [pin for pin in nc.LITERAL_ANCHORS
                if pin != (nc.REGISTRY_TESTS,
                           '#2692 the record body keeps the raw JSON null')]
        self.assertRefused('unpinned literal anchor', 'run_registry_tests.rs',
                           pins=pins,
                           why='#2692 fix-r11: a dropped second registration '
                               'must refuse the run')

    def test_subset_is_bound_against_the_full_table(self):
        # A targeted run validates a few rows but still owns the whole pin
        # table: judging the subset alone would refuse every other pin as
        # unbound (the #2065 r5 review P1 shape native-config pins too).
        try:
            check(cases=[ROWS[0]], bound_rows=ROWS)
        except SystemExit as stop:
            self.fail(f'#2692 fix-r10: a subset rerun must stay bound against '
                      f'the full table: rc={stop.code}')
        self.assertRefused('literal anchor unbound to any row', cases=[ROWS[0]])

    def test_main_binds_a_targeted_run_to_the_whole_table(self):
        # fix-r12 (review sibling P2, the native-config :139 shape): main()'s
        # run path passes the selected rows as `cases` but the full table as
        # `bound_rows` -- a targeted rerun selects one row yet still owns the
        # whole pin table. This wiring had no red side here: passing
        # selected_rows as the binding too would only refuse on a targeted
        # run, which nothing in-tree exercised.
        seen = {}
        real = nc.check_literal_anchors

        def spy(cases, bound_rows=None):
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
        self.assertEqual([case.mutation for case in seen['cases']], [BAD_ROW],
                         '#2692 fix-r12: the targeted run validates only the '
                         'selected row')
        self.assertEqual(len(seen['bound']), len(ROWS),
                         '#2692 fix-r12: the targeted run is still bound '
                         'against the whole table')


if __name__ == '__main__':
    unittest.main()

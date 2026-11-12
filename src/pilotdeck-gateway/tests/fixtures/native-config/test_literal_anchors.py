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
        # #2692 fix-r8 (review sibling#2): pin the third count too -- without
        # it the bound-test gate could be deleted outright and this positive
        # stayed green.
        self.assertIn(f'{len(ROWS)} bound tests declared', done.stdout,
                      '#2692 fix-r8: the bound-test declaration count must be reported')

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
        self.assertIn('set-stub-revert: stale anchor (expected 1, got 0)', message,
                      '#2692 fix-r8: the stale-anchor refusal shape is unchanged')
        self.assertEqual(message.count('#1457'), 1,
                         f'concurrency#2: the tag must appear exactly once: {message!r}')


if __name__ == '__main__':
    unittest.main()

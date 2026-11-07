#!/usr/bin/env python3
# anchor-selfcheck: unsupported unit tests of the fixture's own literal-anchor guard; no product source mutation table
"""#2065: red-side tests for the fixture's own literal-anchor guard.

`--check` refuses a run whose literal anchor could never match, but a guard with
no red side is itself unverified (same contract as `session-assembly-1316
--self-check-controls`: every guard must be shown to turn red). Each case below
breaks exactly one invariant of `check_literal_anchors` and demands the matching
refusal; the first case runs the real `--check` and pins its output counts.

Run from the repo root:

    python3 -m unittest -v src/pilotdeck-gateway/tests/fixtures/native-config/test_literal_anchors.py
"""
import dataclasses
import importlib.util
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


def check(cases=None, pins=None):
    """Run the guard with optional mutated rows / pinned table."""
    nc.check_literal_anchors(ROWS if cases is None else cases, pins)


def replace_row(field, value, mutation=BAD_ROW):
    return [dataclasses.replace(case, **{field: value})
            if case.mutation == mutation else case for case in ROWS]


class LiteralAnchorGuard(unittest.TestCase):
    def assertRefused(self, needle, *args, **kwargs):
        with self.assertRaises(SystemExit) as caught:
            check(*args, **kwargs)
        self.assertIn(needle, str(caught.exception))

    def test_check_passes_and_reports_both_counts(self):
        done = subprocess.run(
            [sys.executable, '-I', str(HERE.with_name('negative_controls.py')), '--check'],
            cwd=nc.ROOT, capture_output=True, text=True)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn(f'#1457 {len(ROWS)} mutation anchors OK', done.stdout)
        self.assertIn(f'#2065 {len(nc.LITERAL_ANCHORS)} literal anchors OK', done.stdout)

    def test_rejects_the_regression(self):
        # The exact #2065 shape: the row goes back to the invented suffix while
        # the table keeps the source-verbatim prefix.
        self.assertRefused('unbound to any row', replace_row(
            'assertion', '#1457 set_permission_mode rejects unknown mode'))

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
        with self.assertRaises(SystemExit) as caught:
            check(pins=pins)
        self.assertIn('unpinned literal anchor', str(caught.exception))
        self.assertIn('session_dir.rs', str(caught.exception))

    def test_rejects_row_outside_the_scanned_set(self):
        self.assertRefused('outside SOURCE_TEXTS',
                           replace_row('path', HERE.with_name('README.md')))


if __name__ == '__main__':
    unittest.main()

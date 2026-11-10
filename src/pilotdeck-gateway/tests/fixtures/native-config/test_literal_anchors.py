#!/usr/bin/env python3
# anchor-selfcheck: unsupported unit tests of the fixture's own literal-anchor guard; no product source mutation table
"""#2065/#2739: red-side tests for the fixture's own guards.

`check_literal_anchors` refuses a run whose literal anchor is verbatim source
text yet unregistered, or a pin no row asserts; a guard with no red side is
itself unverified (same contract as `session-assembly-1316
--self-check-controls`: every guard must be shown to turn red). Each case below
breaks exactly one invariant of `check_literal_anchors` and demands the matching
refusal; the first case runs the real `--check` and pins its output counts.

The #2739 cases do the same for the two newer guards: `main()` must refuse to
start (rc 2) when the runtime table drifts off the static `EXPECTED_NAMES`
roster, and `check_manifest()` must report the README hand-copy it can no
longer vouch for (file count, mutation count, `SOURCE_TEXTS` enumeration).

Run from the repo root:

    python3 -m unittest -v src/pilotdeck-gateway/tests/fixtures/native-config/test_literal_anchors.py
"""
import contextlib
import dataclasses
import importlib.util
import io
import os
import subprocess
import sys
import tempfile
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

    def test_roster_drift_refuses_before_any_run(self):
        # #2739: a dropped row must be a startup refusal (rc 2, "never
        # started"), never a silently shorter run that still reports
        # "N/N killed" off a runtime denominator.
        saved_table = nc.mutations
        saved_argv = sys.argv
        nc.mutations = nc.mutations[:-1]
        stderr = io.StringIO()
        try:
            sys.argv = ['negative_controls.py', '--check']
            with contextlib.redirect_stderr(stderr):
                with self.assertRaises(SystemExit) as caught:
                    nc.main()
        finally:
            nc.mutations = saved_table
            sys.argv = saved_argv
        self.assertEqual(caught.exception.code, 2)
        self.assertIn('mutation roster drifted', stderr.getvalue())

    def test_check_refuses_a_drifted_readme_hand_copy(self):
        # #2739: each case drifts exactly one of the README's three
        # hand-maintained copies; `check_manifest` must name the drifted one.
        drifts = [
            ('只读 22 个文件', '只读 21 个文件', 'SOURCE_TEXTS has 22'),
            ('当前 74 条变异', '当前 73 条变异', 'the table has 74'),
            ('`src/pilotdeck-gateway/src/host/modules/model_port.rs`、'
             '`src/pilotdeck-gateway/src/host/tests/model.rs`',
             '`src/pilotdeck-gateway/src/host/modules/model_port.rs`',
             '锚点 enumeration != SOURCE_TEXTS'),
        ]
        text = nc.README.read_text()
        for find, replace, needle in drifts:
            with self.subTest(needle=needle):
                self.assertIn(find, text)
                with tempfile.TemporaryDirectory() as scratch:
                    drifted = Path(scratch) / 'README.md'
                    drifted.write_text(text.replace(find, replace))
                    saved = nc.README
                    nc.README = drifted
                    try:
                        errors = nc.check_manifest()
                    finally:
                        nc.README = saved
                self.assertTrue(errors)
                self.assertIn(needle, '; '.join(errors))


if __name__ == '__main__':
    unittest.main()

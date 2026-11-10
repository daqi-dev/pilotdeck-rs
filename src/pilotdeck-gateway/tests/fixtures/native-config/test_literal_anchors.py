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

    def test_main_refuses_the_repository_shared_target(self):
        # #1651: `CARGO_TARGET_DIR` equal to the repository's shared in-tree
        # `_build/cargo-target` is an isolation failure — this runner assumes a
        # caller-owned tree-external target — and must be refused before the
        # evidence directory is claimed. A tree-external value proceeds past
        # the check, which `test_main_binds_a_targeted_run_to_the_whole_table`
        # already proves with `/tmp/never-target`.
        saved_argv = sys.argv
        saved_target = os.environ.get('CARGO_TARGET_DIR')
        try:
            sys.argv = ['negative_controls.py', '/tmp/never-created-target-refusal',
                        BAD_ROW]
            os.environ['CARGO_TARGET_DIR'] = str(nc.ROOT / '_build' / 'cargo-target')
            with self.assertRaises(SystemExit) as caught:
                nc.main()
            self.assertIn('repository-shared', str(caught.exception))
            self.assertFalse(Path('/tmp/never-created-target-refusal').exists())
        finally:
            sys.argv = saved_argv
            if saved_target is None:
                os.environ.pop('CARGO_TARGET_DIR', None)
            else:
                os.environ['CARGO_TARGET_DIR'] = saved_target

    def test_main_refuses_subpath_and_relative_spellings_of_the_shared_target(self):
        # #1651: the refusal is not one exact spelling — a subpath of the
        # shared target still lands cargo's products inside it, and a relative
        # spelling is resolved on cargo's basis (cwd=ROOT), never the
        # caller's, so neither form can slip past on a different resolution
        # basis. The chdir proves the caller cwd is irrelevant to the refusal.
        saved_argv = sys.argv
        saved_target = os.environ.get('CARGO_TARGET_DIR')
        saved_cwd = os.getcwd()
        try:
            for value in (str(nc.ROOT / '_build' / 'cargo-target' / 'debug'),
                          '_build/cargo-target'):
                with tempfile.TemporaryDirectory() as outside:
                    os.chdir(outside)
                    sys.argv = ['negative_controls.py',
                                '/tmp/never-created-target-refusal', BAD_ROW]
                    os.environ['CARGO_TARGET_DIR'] = value
                    with self.assertRaises(SystemExit) as caught:
                        nc.main()
                    self.assertIn('repository-shared', str(caught.exception))
                    self.assertFalse(
                        Path('/tmp/never-created-target-refusal').exists())
        finally:
            os.chdir(saved_cwd)
            sys.argv = saved_argv
            if saved_target is None:
                os.environ.pop('CARGO_TARGET_DIR', None)
            else:
                os.environ['CARGO_TARGET_DIR'] = saved_target

    def test_shared_target_roots_cover_worktree_deployments(self):
        # #1651 (DS r2 P1): in a worktree, the shared target that can poison a
        # run is the *primary* checkout's `_build/cargo-target`, which never
        # equals the worktree-local path; the roots must cover both.
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            primary = tmp / 'primary'
            subprocess.run(['git', 'init', '-q', str(primary)], check=True)
            subprocess.run(['git', '-C', str(primary), '-c', 'user.email=t@t',
                            '-c', 'user.name=t', 'commit', '-q',
                            '--allow-empty', '-m', 'x'], check=True)
            worktree = tmp / 'wt'
            subprocess.run(['git', '-C', str(primary), 'worktree', 'add', '-q',
                            str(worktree)], check=True)
            roots = nc._shared_target_roots(worktree)
            # git reports resolved paths; compare both sides resolved.
            self.assertEqual(
                {r.resolve() for r in roots},
                {(worktree / '_build' / 'cargo-target').resolve(),
                 (primary / '_build' / 'cargo-target').resolve()})
            self.assertEqual(len(roots), 2)
            # The primary checkout itself dedupes to a single root.
            self.assertEqual(
                [r.resolve() for r in nc._shared_target_roots(primary)],
                [(primary / '_build' / 'cargo-target').resolve()])

    def test_shared_target_collision_sees_symlink_aliases(self):
        # #1651: samefile comparison — a symlink alias of the shared target
        # (or, on a case-insensitive filesystem, a re-spelling of it) is
        # refused even though it is not textually equal to the shared root.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            shared = root / '_build' / 'cargo-target'
            shared.mkdir(parents=True)
            alias = root / 'alias-target'
            alias.symlink_to(shared)
            self.assertEqual(
                nc._shared_target_collision(str(alias), root),
                Path(os.path.realpath(shared)))
            # An alias pointing *inside* the shared target normalizes onto it.
            into = root / 'into-shared'
            into.symlink_to(shared / 'debug', target_is_directory=True)
            self.assertEqual(
                nc._shared_target_collision(str(into), root),
                Path(os.path.realpath(shared)))
            # A `..` hop through a directory that does not exist yet is
            # normalized lexically onto the shared root.
            self.assertEqual(
                nc._shared_target_collision('_build/missing/../cargo-target',
                                            root),
                Path(os.path.realpath(shared)))
            # A tree-external target proceeds untouched.
            self.assertIsNone(
                nc._shared_target_collision(str(root / 'elsewhere'), root))


if __name__ == '__main__':
    unittest.main()

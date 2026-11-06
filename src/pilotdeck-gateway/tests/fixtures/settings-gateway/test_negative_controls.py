#!/usr/bin/env python3
# anchor-selfcheck: unsupported guard tests for the settings-gateway negative controls, exec modified source only
"""#1734: the settings-gateway control's startup guards and exit-code taxonomy.

The script refuses to run (exit 2) when its static roster, marker table or
oracle binding has drifted, and it must refuse the same way with ``assert``
stripped. Instead of duplicating the guard logic here, each case execs the real
script with one source-level edit injected -- the same shape an accidental edit
would take -- and checks the refusal message and exit code. Nothing here needs
cargo, a committed tree or a mutation: every case stops at a startup guard.

Run from this directory: ``python3 -m unittest settings-gateway/test_negative_controls.py``.
"""
import io
import os
from pathlib import Path
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from unittest.mock import patch

FIXTURES = Path(__file__).resolve().parents[1]
SCRIPT = FIXTURES / 'settings-gateway' / 'negative_controls.py'
sys.path.insert(0, str(FIXTURES))


def run(*argv, patch_source=None, environ=None, output_exists=False):
    """Exec the control script in-process; return (exit code, stdout, stderr).

    ``patch_source`` is one ``(before, after)`` edit applied to the real script
    text. ``output_exists`` makes argv[1] point at a directory that already
    exists, which is the only refusal that may sit next to files.
    """
    source = SCRIPT.read_text(encoding='utf-8')
    if patch_source is not None:
        before, after = patch_source
        if source.count(before) != 1:
            raise AssertionError(f'test anchor occurs {source.count(before)} times: {before!r}')
        source = source.replace(before, after, 1)
    base = Path(tempfile.mkdtemp(prefix='settings-gateway-1734-'))
    out = base / 'out'
    if output_exists:
        out.mkdir()
    env = {key: value for key, value in os.environ.items()
           if key not in ('CARGO_TARGET_DIR', 'PDRS_NEGATIVE_BASELINE')}
    env.update(environ or {})
    stdout, stderr = io.StringIO(), io.StringIO()
    code = 0
    with patch.dict(os.environ, env, clear=True), \
            patch.object(sys, 'argv', [str(SCRIPT), str(out), *argv]), \
            redirect_stdout(stdout), redirect_stderr(stderr):
        try:
            exec(compile(source, str(SCRIPT), 'exec'),
                 {'__name__': '__main__', '__file__': str(SCRIPT)})
        except SystemExit as error:
            code = error.code if isinstance(error.code, int) else 1
    return code, stdout.getvalue(), stderr.getvalue(), out


class RosterGuards(unittest.TestCase):
    """A table, roster, denominator or marker edit must refuse before running."""

    def assertRefused(self, result, needle):
        code, stdout, stderr, out = result
        self.assertEqual(code, 2, f'expected refusal, got {code}: {stderr!r}{stdout!r}')
        self.assertIn(needle, stderr)
        # The evidence directory is claimed after every startup guard, so a
        # "never started" refusal must not even create it: a host that finds
        # the directory reads it as this run's verdict.
        self.assertFalse(out.exists(),
                         'a refusal before the run must not create the evidence directory')

    def test_denominator_and_roster_disagree(self):
        self.assertRefused(
            run(patch_source=('EXPECTED_TOTAL = 7', 'EXPECTED_TOTAL = 8')),
            'denominator and roster disagree')

    def test_denominator_no_longer_matches_the_table(self):
        # Dropping a row from the frozen roster leaves the literal at 7.
        self.assertRefused(
            run(patch_source=("    'apply-close-skipped',\n", '')),
            'denominator and roster disagree')

    def test_renamed_row_breaks_the_frozen_roster(self):
        self.assertRefused(
            run(patch_source=("('apply-close-skipped', 'gateway.rs',",
                              "('apply-close-skipped-x', 'gateway.rs',")),
            'registered mutation roster must equal the frozen list')

    def test_marker_table_must_cover_every_mutation(self):
        self.assertRefused(
            run(patch_source=("    'apply-close-skipped': 'case apply-effort-low',\n", '')),
            'marker table must cover exactly the registered mutations')

    def test_marker_must_name_one_assertion(self):
        self.assertRefused(
            run(patch_source=("    'apply-close-skipped': 'case apply-effort-low',",
                              "    'apply-close-skipped': 'assertion',")),
            'does not name one assertion')

    def test_marker_cannot_be_drawn_from_the_kill_criterion(self):
        # `killed` also requires `'#1318' in evidence`; a marker that is part of
        # that literal would make the marker check vacuous.
        self.assertRefused(
            run(patch_source=("    'apply-close-skipped': 'case apply-effort-low',",
                              "    'apply-close-skipped': '#1318',")),
            'is part of the kill criterion itself')

    def test_markers_must_be_unique(self):
        self.assertRefused(
            run(patch_source=("    'apply-close-skipped': 'case apply-effort-low',",
                              "    'apply-close-skipped': 'case apply-busy',")),
            'every mutation needs its own marker')

    def test_case_marker_must_name_a_real_oracle_row(self):
        self.assertRefused(
            run(patch_source=("    'apply-close-skipped': 'case apply-effort-low',",
                              "    'apply-close-skipped': 'case no-such-row-1734',"),
                environ={'CARGO_TARGET_DIR': '/nonexistent-cargo-target'}),
            'names no oracle case id')


class ExitCodeTaxonomy(unittest.TestCase):
    """#1734: 2 means "never started"; 1 is reserved for a run that happened."""

    def test_unknown_mutation_is_refused(self):
        code, _stdout, stderr, out = run('no-such-mutation-1734')
        self.assertEqual(code, 2)
        self.assertIn('no-such-mutation-1734', stderr)
        self.assertFalse(out.exists())

    def test_existing_output_directory_is_refused(self):
        # The directory claim sits behind the startup guards, so this case
        # supplies the environment those guards require; otherwise it would
        # stop at CARGO_TARGET_DIR and never reach the claim.
        code, _stdout, stderr, out = run(
            output_exists=True, environ={'CARGO_TARGET_DIR': '/nonexistent-cargo-target'})
        self.assertEqual(code, 2)
        self.assertIn('output directory already exists', stderr)
        self.assertEqual(list(out.iterdir()), [])

    def test_missing_cargo_target_is_refused_after_the_table_is_accepted(self):
        # Positive control for the guards above: the unmodified table and roster
        # pass, and the run stops at the caller-owned build tree it needs.
        code, _stdout, stderr, out = run()
        self.assertEqual(code, 2)
        self.assertIn('CARGO_TARGET_DIR must be supplied by the caller', stderr)
        self.assertFalse(out.exists(),
                         'the CARGO_TARGET_DIR refusal happens before the directory claim')


class MarkerScopeShared(unittest.TestCase):
    """#1734: one implementation, used by both the legacy helper and the M6 kit."""

    def test_legacy_helper_and_contract_import_the_same_predicate(self):
        from negative_panic import marker_is_scoped as legacy
        from _negkit import contract
        import negative_scope
        self.assertIs(contract.marker_is_scoped, negative_scope.marker_is_scoped)
        self.assertIs(legacy, negative_scope.marker_is_scoped)

    def test_scoped_and_unscoped_markers(self):
        from negative_scope import marker_is_scoped
        for marker in ('#1318 apply-busy: the gate must fire',
                       'case apply-effort-low',
                       '#1318-r1 update-drop: dropping the future must detach'):
            with self.subTest(marker=marker):
                self.assertTrue(marker_is_scoped(marker))
        for marker in ('', '   ', 'assertion', 'assertion `left == right` failed',
                       '"case": ', '"case":', 'snapshot-success: step',
                       '#813 gateway watchdog: hang', '#813 timeout: slow',
                       'first line\nsecond line'):
            with self.subTest(marker=marker):
                self.assertFalse(marker_is_scoped(marker))


if __name__ == '__main__':
    unittest.main()

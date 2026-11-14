# anchor-selfcheck: unsupported unit tests of the shared subset-selection helper
"""Negative controls for the shared subset selection (#813 fix-r3).

Run from anywhere: ``python3 -I -B test_negative_selection.py`` — the import
path is self-bootstrapped so the CI preflight can launch it with ``-I`` from
any working directory (#1540 review r3 sibling#4).
``test_unknown_control_is_rejected_even_with_a_valid_name`` pins the defect the
old runners had: they only asserted the filtered list was non-empty, so a typo
next to a valid name looked like a successful subset and, worse, dropped the
baseline.
"""
import io
import os
from pathlib import Path
import sys
import tempfile
import unittest
from contextlib import redirect_stderr

sys.path.insert(0, str(Path(__file__).resolve().parent))

from negative_selection import (baseline_passed, claim_dir, refuse,
                                registered, select, verdict_exit,
                                withdraw_claim)

ROWS = [('baseline', 'pass'), ('mutant-a', 'kill'), ('mutant-b', 'kill')]


class SubsetSelection(unittest.TestCase):
    def test_subset_keeps_the_baseline_first(self):
        self.assertEqual(select(ROWS, ['mutant-b']),
                         [('baseline', 'pass'), ('mutant-b', 'kill')])

    def test_baseline_only_selection_is_allowed(self):
        self.assertEqual(select(ROWS, ['baseline']), [('baseline', 'pass')])

    def test_empty_selection_runs_every_control(self):
        self.assertEqual(select(ROWS, []), ROWS)

    def test_unknown_control_is_rejected_even_with_a_valid_name(self):
        # Old behaviour: `[row for row in rows if row[0] in {'mutant-a', 'typo'}]`
        # was non-empty, so `assert selected` passed and the baseline vanished.
        with self.assertRaisesRegex(ValueError, 'unknown controls: typo'):
            select(ROWS, ['mutant-a', 'typo'])

    def test_repeated_control_name_is_rejected(self):
        # #1666 / #1734: a `a a` typo used to fold into a one-row subset, print
        # `1/1 killed` and exit 0. The specification says rc 2 instead.
        with self.assertRaisesRegex(ValueError, 'repeated control name'):
            select(ROWS, ['mutant-a', 'mutant-a'])

    def test_missing_baseline_is_rejected(self):
        with self.assertRaisesRegex(ValueError, r'missing baseline control\(s\): baseline'):
            select([('mutant-a', 'kill')], [])
    def test_baseline_passed_requires_the_named_test(self):
        ok = ('running 1 test\n'
              'test pkg::tests::target ... ok\n'
              'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n')
        self.assertTrue(baseline_passed(ok, 'pkg::tests::target'))
        failed = ok.replace('... ok', '... FAILED').replace(
            'test result: ok.', 'test result: FAILED.')
        self.assertFalse(baseline_passed(failed, 'pkg::tests::target'))
        # A zero-test filter is not a baseline: cargo exits 0 but the target
        # never ran (the subset runner used to accept this as green).
        empty = ('running 0 tests\n'
                 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out\n')
        self.assertFalse(baseline_passed(empty, 'pkg::tests::target'))
        sibling = ok.replace('pkg::tests::target', 'pkg::tests::other')
        self.assertFalse(baseline_passed(sibling, 'pkg::tests::target'))

    def test_dict_rows_use_the_name_key(self):
        rows = [dict(name='baseline'), dict(name='mutant-a')]
        self.assertEqual(select(rows, ['mutant-a'], name=lambda row: row['name']),
                         rows)


class VerdictExit(unittest.TestCase):
    """#818 fix-r3 P0: a run with zero KILLED rows must never exit 0."""

    def test_all_killed_exits_zero(self):
        self.assertEqual(verdict_exit([{'verdict': 'KILLED'},
                                       {'verdict': 'KILLED'}]), 0)

    def test_zero_killed_is_never_success(self):
        for rows in (
            [],
            [{'verdict': 'SURVIVED'}],
            [{'verdict': 'INVALID'}],
            [{'verdict': 'SKIPPED'}],
            [{'verdict': 'KILLED'}, {'verdict': 'SKIPPED'}],
            [{'verdict': 'KILLED'}, {'verdict': 'INVALID'}],
        ):
            with self.subTest(rows=rows):
                self.assertEqual(verdict_exit(rows), 1,
                                 'zero KILLED (or any non-KILLED row) must fail')

    def test_snapshot_status_key(self):
        rows = [{'status': 'KILLED'}, {'status': 'KILLED'}]
        self.assertEqual(verdict_exit(rows, key='status'), 0)
        self.assertEqual(verdict_exit([], key='status'), 1)
        self.assertEqual(verdict_exit([{'status': 'SKIPPED'}], key='status'), 1)


class ClaimDir(unittest.TestCase):
    """#1734: an existing evidence directory is "never started" (2), not 1."""

    def test_existing_directory_is_a_refusal(self):
        with tempfile.TemporaryDirectory() as base:
            directory = Path(base) / 'out'
            self.assertEqual(claim_dir(directory), directory.resolve())
            with self.assertRaises(SystemExit) as caught:
                claim_dir(directory)
            self.assertEqual(caught.exception.code, 2)


class WithdrawClaim(unittest.TestCase):
    """#3143 fix-r2 (DS sibling#2): the paired controls for the withdrawal.

    The positive side pins why the helper exists: after an enter-phase
    refusal withdrew the claim, a host re-running the same output path must
    not be refused on a leftover directory. The negative sides pin the two
    failure shapes: a claim that already wrote logs still loses the whole
    tree (rename + rmtree, not a bare rmdir), and a rename that cannot
    happen (a `.withdrawn-<pid>` sibling left by a SIGKILLed withdrawal) is
    swallowed so the caller's rc 2 refusal is never masked.
    """

    def test_withdrawn_claim_lets_a_host_rerun_proceed(self):
        with tempfile.TemporaryDirectory() as base:
            directory = Path(base) / 'out'
            self.assertEqual(claim_dir(directory), directory.resolve())
            withdraw_claim(directory)
            self.assertFalse(directory.exists())
            # The rerun claims the same path without a refusal.
            self.assertEqual(claim_dir(directory), directory.resolve())

    def test_a_claim_that_already_wrote_logs_is_fully_removed(self):
        with tempfile.TemporaryDirectory() as base:
            directory = Path(base) / 'out'
            claim_dir(directory)
            (directory / 'run.log').write_text('partial run')
            withdraw_claim(directory)
            self.assertFalse(directory.exists())
            self.assertEqual(claim_dir(directory), directory.resolve())

    def test_rename_failure_is_swallowed_so_the_refusal_survives(self):
        # A SIGKILL between the rename and the rmtree leaves a
        # `.withdrawn-<pid>` sibling behind; a later withdrawal's rename
        # collides with it and must not raise over the caller's rc 2 path.
        with tempfile.TemporaryDirectory() as base:
            directory = Path(base) / 'out'
            claim_dir(directory)
            (directory / 'run.log').write_text('partial run')
            stale = directory.with_name(f'{directory.name}.withdrawn-{os.getpid()}')
            stale.mkdir()
            (stale / 'run.log').write_text('killed run')
            withdraw_claim(directory)  # must not raise
            self.assertTrue((directory / 'run.log').exists())


class StaticRoster(unittest.TestCase):
    """#1666: the denominator is a source literal, not ``len(runtime_table)``.

    With a runtime denominator a deleted row turns ``3/3 killed`` into
    ``2/2 killed`` and deleting every row prints ``0/0 killed`` and exits 0.
    These checks are ordinary raises, so ``python3 -O`` cannot strip them.
    """

    def test_matching_roster_is_returned(self):
        self.assertEqual(registered(ROWS, ['baseline', 'mutant-a', 'mutant-b']),
                         ['baseline', 'mutant-a', 'mutant-b'])

    def test_dropped_renamed_duplicated_or_reordered_rows_are_refused(self):
        for runtime, expected, reason in (
            (ROWS[:2], ['baseline', 'mutant-a', 'mutant-b'], 'dropped row'),
            ([ROWS[0], ('mutant-x', 'kill'), ROWS[2]],
             ['baseline', 'mutant-a', 'mutant-b'], 'renamed row'),
            ([ROWS[0], ROWS[1], ROWS[1]],
             ['baseline', 'mutant-a', 'mutant-b'], 'duplicated row'),
            (list(reversed(ROWS)), ['baseline', 'mutant-a', 'mutant-b'], 'reordered'),
            ([], ['baseline'], 'empty run'),
        ):
            with self.subTest(reason=reason):
                with self.assertRaises(ValueError):
                    registered(runtime, expected)

    def test_empty_expected_roster_is_refused(self):
        with self.assertRaisesRegex(ValueError, 'empty expected roster'):
            registered([], [])

    def test_duplicate_expectation_is_refused(self):
        with self.assertRaisesRegex(ValueError, 'duplicate names in expected roster'):
            registered(ROWS[:2], ['baseline', 'baseline'])

    def test_dict_rows_use_the_name_key(self):
        rows = [dict(name='baseline'), dict(name='mutant-a')]
        self.assertEqual(registered(rows, ['baseline', 'mutant-a'],
                                    name=lambda row: row['name']),
                         ['baseline', 'mutant-a'])


class Refuse(unittest.TestCase):
    """#1734: "never started" is exit 2, distinct from a failed run's exit 1."""

    def test_refuse_exits_two_with_the_reason_on_stderr(self):
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            with self.assertRaises(SystemExit) as raised:
                refuse('unknown mutation name: typo')
        # A SystemExit carrying a string would exit 1; the code must be the
        # explicit integer so a reader can tell a guard from a failed control.
        self.assertEqual(raised.exception.code, 2)
        self.assertEqual(stderr.getvalue(),
                         'INVALID: unknown mutation name: typo\n')


if __name__ == '__main__':
    unittest.main()

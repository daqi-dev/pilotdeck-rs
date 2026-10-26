"""Negative controls for the shared subset selection (#813 fix-r3).

Run from this directory: ``python3 -m unittest test_negative_selection.py``.
``test_unknown_control_is_rejected_even_with_a_valid_name`` pins the defect the
old runners had: they only asserted the filtered list was non-empty, so a typo
next to a valid name looked like a successful subset and, worse, dropped the
baseline.
"""
import unittest

from negative_selection import baseline_passed, select

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


if __name__ == '__main__':
    unittest.main()

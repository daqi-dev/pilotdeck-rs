"""Negative controls for the shared subset selection (#813 fix-r3).

Run from this directory: ``python3 -m unittest test_negative_selection.py``.
``test_unknown_control_is_rejected_even_with_a_valid_name`` pins the defect the
old runners had: they only asserted the filtered list was non-empty, so a typo
next to a valid name looked like a successful subset and, worse, dropped the
baseline.
"""
import unittest

from negative_selection import select

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

    def test_dict_rows_use_the_name_key(self):
        rows = [dict(name='baseline'), dict(name='mutant-a')]
        self.assertEqual(select(rows, ['mutant-a'], name=lambda row: row['name']),
                         rows)


if __name__ == '__main__':
    unittest.main()

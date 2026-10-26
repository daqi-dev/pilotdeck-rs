"""Negative controls for the #813 panic-block criterion.

Run from this directory: ``python3 -m unittest test_negative_panic.py``.
The first sample is the defect #813 describes: the named test passed, another
test panicked with the same marker. The old ``marker in raw`` accepted it; the
strict criterion must reject it.
"""
import unittest

from negative_panic import (marker_kills_named_test, named_test_failed,
                            named_test_killed, panic_evidence)

NAMED = 'pkg::tests::named'
OTHER = 'pkg::tests::other'
MARKER = 'the same mutation marker'


def log(named_status, other_status, named_panic='', other_panic=''):
    """Build a small but libtest-shaped log."""
    statuses = [f'test {NAMED} ... {named_status}']
    if other_status:
        statuses.append(f'test {OTHER} ... {other_status}')
    failures = [name for name, status in ((NAMED, named_status), (OTHER, other_status))
                if status == 'FAILED']
    details = []
    if other_panic:
        details.append(f"---- {OTHER} stdout ----\nthread '{OTHER}' panicked at src/lib.rs:1:1:\n"
                       f"{other_panic}")
    if named_panic:
        details.append(f"---- {NAMED} stdout ----\nthread '{NAMED}' panicked at src/lib.rs:2:2:\n"
                       f"{named_panic}")
    failure_list = '\n'.join(f'    {name}' for name in failures)
    return (
        f"running {1 + bool(other_status)} tests\n"
        + '\n'.join(statuses) + "\n\n"
        + ('\n'.join(details) + '\n\n' if details else '')
        + "failures:\n" + failure_list + "\n\n"
        + "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n"
    )


class PanicBlockCriterion(unittest.TestCase):
    def test_named_test_passed_while_other_test_panicked_is_rejected(self):
        raw = log('ok', 'FAILED', other_panic=MARKER)
        # The old global check is exactly this; it is why #813 exists.
        self.assertIn(MARKER, raw)
        self.assertFalse(named_test_failed(raw, NAMED))
        self.assertFalse(named_test_killed(raw, NAMED, MARKER))
        self.assertEqual(panic_evidence(raw, NAMED), '')

    def test_marker_in_named_panic_block_is_accepted(self):
        raw = log('FAILED', '', named_panic=MARKER)
        self.assertTrue(named_test_failed(raw, NAMED))
        self.assertTrue(marker_kills_named_test(raw, NAMED, MARKER))
        self.assertTrue(named_test_killed(raw, NAMED, MARKER))

    def test_marker_in_another_panic_block_is_rejected_even_when_named_failed(self):
        raw = log('FAILED', 'FAILED', named_panic='a different assertion', other_panic=MARKER)
        self.assertTrue(named_test_failed(raw, NAMED))
        self.assertFalse(marker_kills_named_test(raw, NAMED, MARKER))
        self.assertFalse(named_test_killed(raw, NAMED, MARKER))

    def test_panic_block_without_failed_line_is_rejected(self):
        raw = (
            f"running 1 test\ntest {NAMED} ... ok\n\n"
            f"thread '{NAMED}' panicked at src/lib.rs:1:1:\n{MARKER}\n"
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
        )
        self.assertTrue(marker_kills_named_test(raw, NAMED, MARKER))
        self.assertFalse(named_test_failed(raw, NAMED))
        self.assertFalse(named_test_killed(raw, NAMED, MARKER))

    def test_alternative_markers_are_each_scoped_to_the_named_block(self):
        raw = log('FAILED', 'FAILED', named_panic='should throw', other_panic='has overflowed its stack')
        self.assertTrue(named_test_killed(raw, NAMED, ['has overflowed its stack', 'should throw']))
        self.assertFalse(named_test_killed(raw, NAMED, ['has overflowed its stack']))

    def test_nested_test_name_belongs_to_the_named_module(self):
        nested = 'pkg::tests::named::nested'
        raw = (
            f"running 1 test\ntest {nested} ... FAILED\n\n"
            f"thread '{nested}' panicked at src/lib.rs:1:1:\n{MARKER}\n\n"
            "failures:\n"
            f"    {nested}\n\n"
            "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n"
        )
        self.assertTrue(named_test_killed(raw, 'pkg::tests', MARKER))
        self.assertFalse(named_test_killed(raw, 'pkg::tests::other', MARKER))


if __name__ == '__main__':
    unittest.main()

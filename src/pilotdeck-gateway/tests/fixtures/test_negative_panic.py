"""Negative controls for the #813 panic-block criterion.

Run from this directory: ``python3 -m unittest test_negative_panic.py``.
The first sample is the defect #813 describes: the named test passed, another
test panicked with the same marker. The old ``marker in raw`` accepted it; the
strict criterion must reject it.

``turn-gateway/real-logs`` holds four host-run logs from the #813 fix-r1
defect: a named test FAILED and the marker is either in an ``<unnamed>``
thread spawned by it (emit cases) or in the named test's own block (expiry
cases). They are replayed here so the shared judge cannot regress.
"""
from pathlib import Path
import re
import unittest

from negative_panic import (marker_kills_named_test, named_test_failed,
                            named_test_killed, panic_evidence,
                            single_failed_test_run)

NAMED = 'pkg::tests::named'
OTHER = 'pkg::tests::other'
MARKER = 'the same mutation marker'
HERE = Path(__file__).resolve().parent
RECORDED = HERE / 'turn-gateway' / 'real-logs'
GATEWAY = 'runtime::turn::gateway::tests::'
EMIT = GATEWAY + 'concurrent_external_emits_are_serialized_on_the_serial_host'
TIMER = GATEWAY + 'serial_timer_cancels_queued_callbacks_and_replacements'
RECORDED_CASES = (
    ('emit-bypasses-serial-host', EMIT),
    ('emit-deferred-after-return', EMIT),
    ('expiry-runs-on-clock-thread', TIMER),
    ('expiry-callback-dropped', TIMER),
)


def recorded_anchor(name):
    """Read the marker the live runner registers for ``name``.

    The four recorded logs must be judged with the same marker the runner
    uses; reading it here keeps the two from drifting apart silently.
    """
    runner = (HERE / 'turn-gateway' / 'negative.py').read_text()
    line = next(line for line in runner.splitlines()
                if line.startswith(f"    mutation('{name}'"))
    match = re.search(r", '([^']*)', test=", line)
    if match is None:
        raise AssertionError(f'cannot read the registered marker for {name}')
    return match.group(1)


def child_thread_log(running):
    """A real-shaped log whose assertion panics on a spawned ``<unnamed>`` thread."""
    if running == 1:
        statuses = [f'test {NAMED} ... FAILED']
        result = 'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out'
    else:
        statuses = [f'test {NAMED} ... FAILED', f'test {OTHER} ... ok']
        result = 'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out'
    return (
        f"running {running} {'test' if running == 1 else 'tests'}\n"
        + '\n'.join(statuses) + "\n\n"
        + "failures:\n" + f'    {NAMED}' + "\n\n"
        + result + "\n\n"
        + "thread '<unnamed>' (123) panicked at src/lib.rs:9:9:\n"
        + MARKER + "\n"
        + "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\n"
        + f"thread '{NAMED}' (122) panicked at src/lib.rs:10:10:\n"
        + "caller thread: Any { .. }\n"
    )


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
    total = 1 + bool(other_status)
    result = (f"test result: {'FAILED' if failures else 'ok'}. "
              f"{total - len(failures)} passed; {len(failures)} failed; "
              "0 ignored; 0 measured; 0 filtered out")
    return (
        f"running {total} {'test' if total == 1 else 'tests'}\n"
        + '\n'.join(statuses) + "\n\n"
        + ('\n'.join(details) + '\n\n' if details else '')
        + "failures:\n" + failure_list + "\n\n"
        + result + "\n"
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

    def test_unnamed_thread_assertion_in_single_test_run_is_accepted(self):
        raw = child_thread_log(1)
        self.assertTrue(single_failed_test_run(raw))
        self.assertIn("<unnamed>'", panic_evidence(raw, NAMED))
        self.assertTrue(named_test_killed(raw, NAMED, MARKER))

    def test_unnamed_thread_assertion_in_multi_test_run_is_rejected(self):
        raw = child_thread_log(2)
        self.assertTrue(named_test_failed(raw, NAMED))
        self.assertFalse(single_failed_test_run(raw))
        self.assertNotIn("thread '<unnamed>'", panic_evidence(raw, NAMED))
        self.assertFalse(named_test_killed(raw, NAMED, MARKER))

    def test_unnamed_thread_panic_without_named_failure_is_rejected(self):
        raw = (
            f"running 1 test\ntest {NAMED} ... ok\n\n"
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n\n"
            "thread '<unnamed>' (123) panicked at src/lib.rs:9:9:\n"
            f"{MARKER}\n"
        )
        self.assertFalse(named_test_failed(raw, NAMED))
        self.assertFalse(single_failed_test_run(raw))
        self.assertFalse(named_test_killed(raw, NAMED, MARKER))

    def test_recorded_host_logs_are_killed(self):
        for name, used_test in RECORDED_CASES:
            with self.subTest(log=name):
                raw = (RECORDED / f'{name}.log').read_text()
                marker = recorded_anchor(name)
                self.assertTrue(named_test_failed(raw, used_test))
                self.assertTrue(named_test_killed(raw, used_test, marker),
                                f'{name}: {marker!r} not in the attributable block')

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

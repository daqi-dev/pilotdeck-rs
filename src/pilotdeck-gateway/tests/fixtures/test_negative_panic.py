"""Negative controls for the #813 panic-block criterion.

Run from this directory: ``python3 -m unittest test_negative_panic.py``.
The first sample is the defect #813 describes: the named test passed, another
test panicked with the same marker. The old ``marker in raw`` accepted it; the
strict criterion must reject it.

fix-r4 narrows evidence to the panic *payload*. The thread name, the
``panicked at`` path and the backtrace are not assertions, so a marker that only
appears there must be rejected even when the named test is the FAILED one.

``turn-gateway/real-logs`` holds host-run logs from the #813 fix-r1 defect and
the fix-r2 expiry re-record: a named test FAILED and the marker is either in an
``<unnamed>`` thread spawned by it (emit cases) or in the named test's own
block (expiry cases). They are replayed here so the shared judge cannot
regress.
"""
import ast
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


def projector_named_markers():
    """Read marker -> test from the projector runner without executing it."""
    tree = ast.parse((HERE / 'agent-event-projector' / 'negative.py').read_text())
    for node in tree.body:
        if not isinstance(node, ast.Assign):
            continue
        if any(isinstance(target, ast.Name) and target.id == 'NAMED_TESTS'
               for target in node.targets):
            return ast.literal_eval(node.value)
    raise AssertionError('NAMED_TESTS not found')


def plugin_stranded_markers():
    """Read the plugin-runtime stranded markers without executing the script."""
    tree = ast.parse((HERE / 'plugin-runtime' / 'panic-negative.py').read_text())
    for node in ast.walk(tree):
        if not isinstance(node, ast.Assign):
            continue
        if any(isinstance(target, ast.Name) and target.id == 'STRANDED_MARKERS'
               for target in node.targets):
            return ast.literal_eval(node.value)
    raise AssertionError('STRANDED_MARKERS not found')


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
        evidence = panic_evidence(raw, NAMED)
        self.assertIn(MARKER, evidence)
        self.assertNotIn('<unnamed>', evidence)
        self.assertTrue(named_test_killed(raw, NAMED, MARKER))

    def test_unnamed_thread_assertion_in_multi_test_run_is_rejected(self):
        raw = child_thread_log(2)
        self.assertTrue(named_test_failed(raw, NAMED))
        self.assertFalse(single_failed_test_run(raw))
        self.assertNotIn('<unnamed>', panic_evidence(raw, NAMED))
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

    def test_marker_in_thread_name_only_is_rejected(self):
        # The marker is the test function name. The old helper returned the
        # whole segment, so the header alone satisfied it for any panic.
        marker = 'advisory_persist_returns_before_the_write_completes'
        used_test = 'projector::tests::' + marker
        raw = (
            f"running 1 test\ntest {used_test} ... FAILED\n\n"
            f"thread '{used_test}' panicked at src/tests.rs:1:1:\n"
            "advisory write started: Timeout\n"
            "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n"
        )
        self.assertTrue(named_test_failed(raw, used_test))
        self.assertNotIn(marker, panic_evidence(raw, used_test))
        self.assertFalse(named_test_killed(raw, used_test, marker))

    def test_marker_in_panic_location_path_is_rejected(self):
        marker = 'advisory_persist_returns_before_the_write_completes'
        raw = (
            f"running 1 test\ntest {NAMED} ... FAILED\n\n"
            f"thread '{NAMED}' panicked at src/{marker}.rs:1:1:\n"
            "a different assertion\n"
            "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n"
        )
        self.assertTrue(named_test_failed(raw, NAMED))
        self.assertFalse(named_test_killed(raw, NAMED, marker))

    def test_marker_in_backtrace_is_rejected(self):
        raw = (
            f"running 1 test\ntest {NAMED} ... FAILED\n\n"
            f"thread '{NAMED}' panicked at src/lib.rs:1:1:\n"
            "a different assertion\n"
            f"stack backtrace:\n   0: {MARKER}\n"
            "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n"
        )
        self.assertTrue(named_test_failed(raw, NAMED))
        self.assertNotIn(MARKER, panic_evidence(raw, NAMED))
        self.assertFalse(named_test_killed(raw, NAMED, MARKER))

    def test_multiline_payload_is_kept(self):
        raw = log('FAILED', '',
                  named_panic='assertion `left == right` failed\n  left: 1\n right: 2')
        evidence = panic_evidence(raw, NAMED)
        self.assertIn('assertion `left == right` failed', evidence)
        self.assertIn('left: 1', evidence)
        self.assertIn('right: 2', evidence)

    def test_projector_markers_are_assertion_messages_not_test_names(self):
        markers = projector_named_markers()
        self.assertEqual(len(markers), 15)
        tests_source = (HERE.parents[1] / 'src/runtime/turn/projector_tests.rs').read_text()
        test_names = set(markers.values())
        for marker, test in markers.items():
            with self.subTest(marker=marker):
                self.assertNotIn(marker, test_names)
                self.assertIn(marker, tests_source)
                self.assertIn(f'fn {test}(', tests_source)

    def test_plugin_runtime_generic_assert_eq_marker_is_rejected(self):
        # The first plugin-runtime repair added `assertion `left == right`
        # failed`, which accepts *any* assert_eq! in the same test. Only the
        # dedicated completion/deadline messages may count (#813 fix-r4).
        used_test = ('runtime::plugins::tests::'
                     'disposal_sync_panic_does_not_strand_other_waiters')
        raw = (
            f"running 1 test\ntest {used_test} ... FAILED\n\n"
            f"thread '{used_test}' panicked at src/plugins_tests.rs:1:1:\n"
            "assertion `left == right` failed\n  left: 1\n right: 2\n"
            "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n"
        )
        markers = plugin_stranded_markers()
        self.assertEqual(markers, (
            'sync panic stranded a waiter: dispose deadline',
            'sync panic stranded a waiter: completion count',
        ))
        tests_source = (HERE.parents[1] / 'src/runtime/plugins_tests.rs').read_text()
        for marker in markers:
            self.assertIn(marker, tests_source)
        self.assertFalse(named_test_killed(raw, used_test, markers))
        # The old generic marker accepted this very panic; the new one does not.
        self.assertTrue(named_test_killed(raw, used_test,
                                          'assertion `left == right` failed'))

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

    def test_nested_test_panic_is_not_evidence_for_the_named_parent(self):
        named = 'pkg::tests::named'
        nested = named + '::nested'
        raw = (
            f"running 2 tests\ntest {named} ... ok\ntest {nested} ... FAILED\n\n"
            f"---- {nested} stdout ----\n"
            f"thread '{nested}' panicked at src/lib.rs:1:1:\n{MARKER}\n\n"
            "failures:\n"
            f"    {nested}\n\n"
            "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n"
        )
        # Exact equality only: the parent passed, so its child's panic cannot
        # be promoted into the parent's evidence (#813 fix-r2 P1-1).
        self.assertFalse(named_test_failed(raw, named))
        self.assertFalse(marker_kills_named_test(raw, named, MARKER))
        self.assertFalse(named_test_killed(raw, named, MARKER))
        self.assertEqual(panic_evidence(raw, named), '')

    def test_module_path_argument_is_rejected(self):
        named = 'pkg::tests::named'
        nested = named + '::nested'
        raw = (
            f"running 1 test\ntest {nested} ... FAILED\n\n"
            f"thread '{nested}' panicked at src/lib.rs:1:1:\n{MARKER}\n"
            "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n"
        )
        # The caller used to pass a module path and rely on `::` prefixing.
        with self.assertRaisesRegex(ValueError, 'module path'):
            named_test_failed(raw, 'pkg::tests')
        with self.assertRaisesRegex(ValueError, 'module path'):
            named_test_killed(raw, 'pkg::tests', MARKER)
        # Also rejected when the output proves the argument is only a prefix.
        with self.assertRaisesRegex(ValueError, 'module path'):
            named_test_failed(raw.replace('pkg::tests::named', 'pkg::mod'), 'pkg::mod')

    def test_module_level_caller_passes_explicit_full_names(self):
        raw = (
            f"running 2 tests\ntest {NAMED} ... FAILED\ntest {OTHER} ... ok\n\n"
            f"thread '{NAMED}' panicked at src/lib.rs:1:1:\n{MARKER}\n\n"
            "failures:\n"
            f"    {NAMED}\n\n"
            "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n"
        )
        # A module-level caller lists the full names it owns; the helper never
        # expands a module path itself.
        self.assertTrue(named_test_killed(raw, [NAMED, OTHER], MARKER))
        self.assertFalse(named_test_killed(raw, [OTHER], MARKER))
        with self.assertRaisesRegex(ValueError, 'module path'):
            named_test_killed(raw, [NAMED, 'pkg::tests'], MARKER)

    def test_passing_list_member_cannot_lend_its_panic_block(self):
        raw = log('FAILED', 'ok', named_panic='a different assertion', other_panic=MARKER)
        # OTHER is listed but did not fail; its block must not satisfy the
        # marker requirement for the module-level judgement.
        self.assertFalse(named_test_killed(raw, [NAMED, OTHER], MARKER))
        self.assertFalse(named_test_killed(raw, [NAMED], MARKER))

    def test_libtest_reports_detached_panics_after_the_failed_line(self):
        # P2-2 constraint cannot be implemented: libtest streams the detached
        # thread's panic after `... FAILED` (here even after `test result:`),
        # so position cannot separate "inside the test body" from "after the
        # test returned". The module docstring records this limitation.
        raw = (RECORDED / 'emit-bypasses-serial-host.log').read_text()
        failed = raw.index(f'test {EMIT} ... FAILED')
        detached = raw.index("thread '<unnamed>'")
        self.assertLess(failed, detached)


if __name__ == '__main__':
    unittest.main()

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

fix-r8 closes the two remaining false-KILLED channels: a watchdog/timeout
payload is a hang, not an assertion, and a scenario prefix or a marker for a
different assertion is not the mutant's target. ``turn_gateway_markers`` and
``failure_guard_markers`` also fail closed on a missing or non-literal marker
and count every registration, so a deleted marker cannot hide behind the old
``>= 30`` slack.

fix-r13 binds every turn-submit step marker to the complete
``#813 <case>: step <index> "<step>": submit scenario must match`` message and
replays two real host logs of the same case at different steps
(``turn-submit/real-logs``): neither step may kill the other's mutant.
"""
import ast
import json
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
SUBMIT_RECORDED = HERE / 'turn-submit' / 'real-logs'
SUBMIT_REPLAY = 'runtime::turn::submit::tests::real_ts_submit_scenarios'
# fix-r13: two host-run logs, same case, different steps.
SUBMIT_SNAPSHOT_STEPS = {
    'snapshot-pre-skipped':
        '#813 snapshot-success: step 1 "accept": submit scenario must match',
    'snapshot-success-post-added':
        '#813 snapshot-success: step 3 "end": submit scenario must match',
}
GATEWAY = 'runtime::turn::gateway::tests::'
EMIT = GATEWAY + 'concurrent_external_emits_are_serialized_on_the_serial_host'
TIMER = GATEWAY + 'serial_timer_cancels_queued_callbacks_and_replacements'
RECORDED_CASES = (
    ('emit-bypasses-serial-host', EMIT),
    ('emit-deferred-after-return', EMIT),
    ('expiry-runs-on-clock-thread', TIMER),
    ('expiry-callback-dropped', TIMER),
)


def _module_constants(tree):
    values = {}
    for node in ast.walk(tree):
        if (isinstance(node, ast.Assign) and len(node.targets) == 1
                and isinstance(node.targets[0], ast.Name)):
            try:
                values.setdefault(node.targets[0].id, ast.literal_eval(node.value))
            except (ValueError, TypeError):
                pass
    return values


def _marker_value(node, constants):
    """Evaluate a marker registration that is a literal or a named builder.

    #813 fix-r8: a non-literal marker used to fall through every shape check
    (``ast.unparse`` produced neither a quote nor ``f'``). Fail closed instead:
    only constants and the three explicit builder calls are accepted.
    """
    if isinstance(node, ast.Constant) and isinstance(node.value, (str, int)):
        return node.value
    if isinstance(node, ast.Name) and node.id in constants:
        return constants[node.id]
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
        args = [_marker_value(arg, constants) for arg in node.args]
        if node.func.id == 'replay_marker':
            return f'#813 {args[0]}: step "{args[1]}": {args[2]}'
        if node.func.id == 'timer_marker':
            return f'#813 serial timer: {args[0]}'
        if node.func.id == 'submit_marker':
            return f'#813 {args[0]}: step {args[1]} "{args[2]}": submit scenario must match'
        if node.func.id == 'guard_trip_marker':
            return (f'#813 failure guard trip: {args[0]} action {args[1]}: '
                    'trip result bytes must match')
        if node.func.id == 'guard_message_marker':
            return (f'#813 failure guard message: {args[0]} action {args[1]}: '
                    'trip message bytes must match')
    raise AssertionError(f'marker must be a literal or named builder: {ast.unparse(node)}')


def _mutation_markers(path):
    tree = ast.parse(path.read_text())
    constants = _module_constants(tree)
    entries = []
    calls = 0
    dict_rows = 0
    for node in ast.walk(tree):
        if not (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)):
            continue
        if node.func.id == 'mutation':
            calls += 1
            values = {keyword.arg: keyword.value for keyword in node.keywords}
            marker = values.get('marker')
            if marker is None and len(node.args) >= 6:
                marker = node.args[5]
            if marker is None:
                raise AssertionError(f'mutation {ast.unparse(node.args[0])} has no marker')
            try:
                name = ast.literal_eval(node.args[0])
            except ValueError:
                name = ast.unparse(node.args[0])
            entries.append((name, _marker_value(marker, constants)))
        elif node.func.id == 'dict':
            values = {keyword.arg: keyword.value for keyword in node.keywords}
            name = values.get('name')
            if isinstance(name, ast.Constant) and name.value == 'notification-after-event':
                dict_rows += 1
                entries.append((name.value, _marker_value(values['marker'], constants)))
    if len(entries) != calls + dict_rows:
        raise AssertionError(f'{len(entries)} markers for {calls} mutation() calls '
                             f'+ {dict_rows} dict rows')
    return entries


def recorded_anchor(name):
    """Read the marker the live runner registers for ``name``.

    The four recorded logs must be judged with the same marker the runner
    uses; reading it here keeps the two from drifting apart silently.
    """
    for entry_name, marker in _mutation_markers(HERE / 'turn-gateway' / 'negative.py'):
        if entry_name == name:
            return marker
    raise AssertionError(f'cannot read the registered marker for {name}')


def turn_gateway_markers():
    """Read every turn-gateway mutant marker without executing the runner."""
    return [marker for _name, marker in
            _mutation_markers(HERE / 'turn-gateway' / 'negative.py')]


def failure_guard_markers():
    """Read every failure-guard mutant marker without executing the runner."""
    return [marker for _name, marker in
            _mutation_markers(HERE / 'failure-guard' / 'negative.py')]


def fence_markers():
    """Read the turn-gateway abort-fence marker registrations."""
    text = (HERE / 'turn-gateway' / 'negative-fence.py').read_text()
    markers = [f'#813 abort fence: {case}: results must match'
               for case in re.findall(r"fence_marker\('([^']+)'\)", text)]
    for literal in (
        '#813 abort-after-busy: step "abort": gateway frames, active and ended must match',
        '#813 transport observation: must wait for delivery before the receive completes',
    ):
        if literal in text:
            markers.append(literal)
    return markers


def turn_submit_registered_markers():
    """Read every turn-submit (mutant, marker) without executing the runner."""
    tree = ast.parse((HERE / 'turn-submit' / 'negative.py').read_text())
    for node in ast.walk(tree):
        if not (isinstance(node, ast.Assign) and len(node.targets) == 1
                and isinstance(node.targets[0], ast.Name)
                and node.targets[0].id == 'mutants'
                and isinstance(node.value, ast.List)):
            continue
        entries = []
        for row in node.value.elts:
            if not (isinstance(row, ast.Tuple) and len(row.elts) >= 5):
                continue
            name_node = row.elts[0]
            if not (isinstance(name_node, ast.Constant)
                    and isinstance(name_node.value, str)):
                raise AssertionError(
                    f'non-literal turn-submit mutant name: {ast.unparse(name_node)}')
            marker_node = row.elts[4]
            if isinstance(marker_node, ast.Constant) and marker_node.value is None:
                entries.append((name_node.value, None))
                continue
            if not (isinstance(marker_node, ast.Constant)
                    and isinstance(marker_node.value, str)):
                raise AssertionError(
                    f'non-literal turn-submit marker: {ast.unparse(marker_node)}')
            entries.append((name_node.value, marker_node.value))
        return entries
    raise AssertionError('turn-submit mutants table not found')


def turn_submit_markers():
    """Read every non-None turn-submit mutant marker."""
    return [marker for _name, marker in turn_submit_registered_markers()
            if marker is not None]


def panic_log(used_test, message):
    """A libtest-shaped log whose named test failed with ``message``."""
    return (
        f"running 1 test\ntest {used_test} ... FAILED\n\n"
        f"thread '{used_test}' (1) panicked at src/tests.rs:1:1:\n"
        f"{message}\n"
        "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n"
    )


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
        # #813 fix-r8: the generic diagnostic is rejected outright even when it
        # is exactly the payload; it names no assertion.
        self.assertIn('assertion `left == right` failed', raw)
        self.assertFalse(named_test_killed(raw, used_test,
                                           'assertion `left == right` failed'))

    def test_turn_gateway_markers_are_complete_target_assertions(self):
        # #813 fix-r8: a scenario prefix (`"<case>": `) accepted any step, any
        # other assertion, and the deadlock watchdog. Every mutant must now
        # register the complete message of the one assertion it must trip.
        entries = _mutation_markers(HERE / 'turn-gateway' / 'negative.py')
        self.assertEqual(len(entries), 33)
        tests_source = (HERE.parents[1] / 'src/runtime/turn/gateway_tests.rs').read_text()
        oracle = json.loads((HERE / 'turn-gateway' / 'oracle.json').read_text())
        steps = {case['name']: case['steps'] for case in oracle['cases']}
        markers = []
        for name, marker in entries:
            with self.subTest(mutant=name, marker=marker):
                self.assertTrue(marker.startswith('#813 '), marker)
                self.assertNotEqual(marker, '#813 assertion')
                self.assertNotRegex(marker, r'^"[^"]+": ')
                match = re.match(r'^#813 ([^:]+): step "([^"]+)": (.+)$', marker)
                if match:
                    case, step, meaning = match.groups()
                    self.assertIn(case, steps)
                    self.assertIn(step, steps[case])
                    self.assertIn(meaning, tests_source)
                else:
                    self.assertIn(marker.rsplit(': ', 1)[-1], tests_source)
                markers.append(marker)
        self.assertIn(
            '#813 permission-before-event-and-final: step "permission": '
            'gateway frames, active and ended must match', markers)
        # No marker is a strict prefix of another: a prefix is exactly how the
        # old scenario marker matched a different step or assertion.
        for short in markers:
            for longer in markers:
                if short != longer:
                    self.assertFalse(longer.startswith(short), (short, longer))

    def test_case_prefix_marker_is_not_killed(self):
        replay = GATEWAY + 'real_ts_gateway_frames_registry_replay_and_concurrency'
        # Old-style prefix payload: the marker is a literal substring, so only
        # the scoped-marker check can reject it (#813 fix-r8).
        target = ('"permission-before-event-and-final": step "observe": '
                  'gateway frames, active and ended must match')
        raw = panic_log(replay, target)
        marker = '"permission-before-event-and-final": '
        self.assertIn(marker, raw)
        self.assertFalse(named_test_killed(raw, replay, marker))
        self.assertFalse(named_test_killed(
            raw, replay, marker,
            '#813 permission-before-event-and-final: step "observe": '
            'gateway frames, active and ended must match'))

    def test_other_assertion_marker_is_not_killed(self):
        replay = GATEWAY + 'real_ts_gateway_frames_registry_replay_and_concurrency'
        target = ('#813 permission-before-event-and-final: step "end": '
                  'gateway frames, active and ended must match')
        other = ('#813 permission-before-event-and-final: step "observe": '
                 'transport observation did not arrive')
        raw = panic_log(replay, target)
        self.assertFalse(named_test_killed(raw, replay, other, other))
        # A prefix of the target message is not the complete target message.
        self.assertFalse(named_test_killed(
            raw, replay,
            '#813 permission-before-event-and-final: step "end"', target))
        self.assertTrue(named_test_killed(raw, replay, target, target))

    def test_watchdog_panic_is_not_killed(self):
        replay = GATEWAY + 'real_ts_gateway_frames_registry_replay_and_concurrency'
        marker = 'scenario permission-before-event-and-final did not finish'
        raw = panic_log(replay, '#813 gateway watchdog: ' + marker)
        self.assertIn(marker, panic_evidence(raw, replay))
        # The marker text is in the payload, but a watchdog is a hang, not an
        # assertion failure (#813 fix-r8).
        self.assertFalse(named_test_killed(raw, replay, marker))
        self.assertFalse(named_test_killed(
            raw, replay, '#813 gateway watchdog:', '#813 gateway watchdog:'))

    def test_failure_guard_markers_are_complete_target_assertions(self):
        entries = _mutation_markers(HERE / 'failure-guard' / 'negative.py')
        self.assertEqual(len(entries), 22)
        source = (HERE.parents[1] / 'src/runtime/turn/failure_guard_tests.rs').read_text()
        submit = (HERE.parents[1] / 'src/runtime/turn/submit_tests.rs').read_text()
        for name, marker in entries:
            with self.subTest(mutant=name, marker=marker):
                self.assertTrue(marker.startswith('#813 '), marker)
                self.assertNotIn(marker, ('#813 failure guard', '#813 assertion'))
                self.assertNotRegex(marker, r'^#813 failure guard: $')
                self.assertIn(marker.rsplit(': ', 1)[-1], source + submit)
        pure = 'runtime::turn::failure_guard::tests::real_ts_failure_guard_and_classify'
        marker = ('#813 failure guard trip: accumulate-success-does-not-reset-and-trip-latches '
                  'action 4: trip result bytes must match')
        other = '#813 failure guard classify: resolved unit and limit must match'
        self.assertFalse(named_test_killed(panic_log(pure, other), pure, marker, marker))
        self.assertTrue(named_test_killed(panic_log(pure, marker), pure, marker, marker))

    def test_fence_markers_are_scenario_assertions(self):
        oracle = json.loads((HERE / 'turn-gateway' / 'oracle.json').read_text())
        cases = {case['name'] for case in oracle['abortFenceCases']}
        source = (HERE.parents[1] / 'src/runtime/turn/gateway_tests.rs').read_text()
        markers = fence_markers()
        self.assertGreaterEqual(len(markers), 11)
        for marker in markers:
            with self.subTest(marker=marker):
                self.assertTrue(marker.startswith('#813 '), marker)
                self.assertNotEqual(marker, '#813 assertion')
                self.assertIn(marker.rsplit(': ', 1)[-1], source)
                match = re.match(r'^#813 abort fence: ([^:]+): results must match$', marker)
                if match:
                    self.assertIn(match.group(1), cases)

    def test_turn_submit_markers_are_scenario_bound(self):
        oracle = json.loads((HERE / 'turn-submit' / 'oracle.json').read_text())
        cases = {case['name']: case['steps'] for case in oracle['cases']}
        entries = turn_submit_registered_markers()
        self.assertGreaterEqual(len(entries), 50)
        step_bound = 0
        for name, marker in entries:
            if marker is None:
                continue
            with self.subTest(mutant=name, marker=marker):
                self.assertNotEqual(marker, 'assertion')
                self.assertFalse(marker.startswith('assertion `left == right`'))
                # fix-r13: `<case>: step` names the scenario, not the target
                # assertion; it matches every step of that case.
                self.assertFalse(marker.endswith(': step'), marker)
                match = re.match(
                    r'^#813 (.+): step (\d+) "([^"]+)": '
                    r'submit scenario must match$', marker)
                if match:
                    step_bound += 1
                    case, index, step = match.groups()
                    self.assertIn(case, cases, name)
                    index = int(index)
                    self.assertLess(index, len(cases[case]), name)
                    self.assertEqual(step, cases[case][index], name)
        # Every `: step` mutant was migrated to its complete message. Losing
        # one (back to a prefix, or to a non-step marker) is a regression.
        self.assertEqual(step_bound, 44)

    def test_turn_submit_same_case_steps_do_not_cross_kill(self):
        registered = dict(turn_submit_registered_markers())
        for log, own in SUBMIT_SNAPSHOT_STEPS.items():
            with self.subTest(log=log):
                self.assertEqual(registered[log], own)
                raw = (SUBMIT_RECORDED / f'{log}.log').read_text()
                # The real host log carries the same-case prefix, so the old
                # bare-prefix judge accepted it...
                self.assertIn('snapshot-success: step',
                              panic_evidence(raw, SUBMIT_REPLAY))
                self.assertTrue(marker_kills_named_test(
                    raw, SUBMIT_REPLAY, 'snapshot-success: step'))
                # ...but the complete message of the other step must not.
                self.assertTrue(named_test_failed(raw, SUBMIT_REPLAY))
                self.assertTrue(named_test_killed(raw, SUBMIT_REPLAY, own, own))
                for other_log, other in SUBMIT_SNAPSHOT_STEPS.items():
                    if other_log == log:
                        continue
                    self.assertFalse(
                        named_test_killed(raw, SUBMIT_REPLAY, other, other),
                        f'{log} was killed by {other_log}\'s step marker')

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

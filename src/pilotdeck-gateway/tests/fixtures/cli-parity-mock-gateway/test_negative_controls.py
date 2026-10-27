"""Unit tests of the #933 kill judge, on synthetic logs only.

The judge is the piece a forged log could otherwise fool, so every class of
forgery the fix-r2 review demonstrated gets its own case here: a marker echoed
in an ERROR traceback, a marker in another test's failure, a crash without a
unittest summary. The fix-r3 case runs main() end to end against source COPIES
with an injected final-restore failure: the persisted summary must be rewritten
INVALID, never left claiming kills whose sources were not verifiably restored.
No real source is mutated; nothing here runs cargo or node.
"""
from contextlib import contextmanager
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import negative_controls
from negative_controls import bound_failure_bodies, classify, unittest_blocks

TEST = 'ContractTests.test_occupied_mock_port_blocks_round'
MARKER = '#933-5: an occupied 28999 must make the owned mock exit and Block the round'

KILLED_LOG = """F
======================================================================
FAIL: test_occupied_mock_port_blocks_round (test_cli_parity.ContractTests.test_occupied_mock_port_blocks_round)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 10, in test_occupied_mock_port_blocks_round
    self.assertIn('exited before listening', str(error))
AssertionError: 'exited before listening' not found in 'mock gateway did not confirm listening within 10s' : #933-5: an occupied 28999 must make the owned mock exit and Block the round

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""

# The forged log from the review: the marker is only the source echo of an
# ERROR block; a "marker appears somewhere" judge would count this as a kill.
ERROR_ECHO_LOG = """E
======================================================================
ERROR: test_occupied_mock_port_blocks_round (test_cli_parity.ContractTests.test_occupied_mock_port_blocks_round)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 42, in test_occupied_mock_port_blocks_round
    self.fail('#933-5: an occupied 28999 must make the owned mock exit and Block the round')
    ^^^^^^^^^^^^^
KeyError: 'mockIsolation'

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (errors=1)
"""

# The bound test fails for an unrelated reason; the marker only shows up in a
# plain stdout line another test printed.
MARKER_ELSEWHERE_LOG = """marker check: #933-5: an occupied 28999 must make the owned mock exit and Block the round
F
======================================================================
FAIL: test_occupied_mock_port_blocks_round (test_cli_parity.ContractTests.test_occupied_mock_port_blocks_round)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 10, in test_occupied_mock_port_blocks_round
    self.assertIn('x', y)
AssertionError: 'x' not found in 'y'

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""

# A different test carries the exact marker; the bound test is green or fails
# for its own unrelated reason.
OTHER_TEST_LOG = """F
======================================================================
FAIL: test_gateway_something_else (test_cli_parity.ContractTests.test_gateway_something_else)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 10, in test_gateway_something_else
    self.assertIn('x', y)
AssertionError: #933-5: an occupied 28999 must make the owned mock exit and Block the round

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""

CRASH_LOG = """Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 1, in <module>
    import nothing
ModuleNotFoundError: No module named 'nothing'
"""

OK_LOG = """.
----------------------------------------------------------------------
Ran 1 test in 0.001s

OK
"""

# assertEqual puts long diffs before the message, on continuation lines of
# the exception: the marker lands after a newline, not on the AssertionError
# line itself. It is still the assertion's own message.
DIFF_CONTINUATION_LOG = """F
======================================================================
FAIL: test_mock_scenario (__main__.ContractTests.test_mock_scenario) (side='ts')
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 980, in test_mock_scenario
    self.assertEqual(compare(source, target, expected)['status'], 'PASS',
AssertionError: 'FAIL' != 'PASS'
- FAIL
+ PASS
 : #933-3: the untampered round must compare PASS; a broken profile binding rejects valid evidence

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""

# A subTest failure: the description follows the parenthesized qualname (the
# exact format of the Python this suite runs under; older Pythons used
# [description] and a bare class in the parentheses — both tolerated below).
SUBTEST_LOG = """F
======================================================================
FAIL: test_gateway_probe (__main__.ContractTests.test_gateway_probe) (probe='allowed')
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 10, in test_gateway_probe
    self.fail('#933-4:')
AssertionError: #933-4: probe tampering not detected

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""

# What the marker-truthy mutant really produces: some subTests fail at the
# pinned assertion while a sibling subTest errors out deeper in the apparatus.
# The run still completes with the standard failure exit code and a summary,
# so the pinned assertion firing is a kill; the sibling ERROR does not negate
# it (it is not an abnormal exit).
FAIL_PLUS_ERROR_LOG = """FE
======================================================================
ERROR: test_gateway_marker (__main__.ContractTests.test_gateway_marker) (bad='string')
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 42, in test_gateway_marker
    a, ac, ae = load_round(Path(left))
    ^^^^^^^^^^^^^
ValueError: not enough values to unpack (expected 3, got 0)

======================================================================
FAIL: test_gateway_marker (__main__.ContractTests.test_gateway_marker) (bad='yes')
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 12, in test_gateway_marker
    self.fail(f'#933-1: marker {bad!r} must be refused, not accepted')
AssertionError: #933-1: marker 'yes' must be refused, not accepted

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1, errors=1)
"""

# Older Python: the class alone in the parentheses, the method before them.
OLD_FORMAT_LOG = """F
======================================================================
FAIL: test_ping_sample (test_cli_parity.ContractTests)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 10, in test_ping_sample
    self.assertEqual(frame, pong)
AssertionError: #933-7: the mock closed the connection during the ping exchange

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""


def run(returncode, stdout, stderr=''):
    return subprocess.CompletedProcess(['judge-sample'], returncode, stdout, stderr)


class UnittestBlocksTests(unittest.TestCase):
    def test_killed_log_parses_the_fail_block_with_the_assertion(self):
        blocks = unittest_blocks(KILLED_LOG)
        self.assertEqual(len(blocks), 1)
        kind, method, qualname, body = blocks[0]
        self.assertEqual((kind, method, qualname),
                         ('FAIL', 'test_occupied_mock_port_blocks_round',
                          'test_cli_parity.ContractTests.test_occupied_mock_port_blocks_round'))
        self.assertIn('AssertionError: ', '\n'.join(body))

    def test_subtest_description_is_tolerated_after_the_qualname(self):
        blocks = unittest_blocks(SUBTEST_LOG)
        self.assertEqual([block[2] for block in blocks],
                         ['__main__.ContractTests.test_gateway_probe'])
        bodies = bound_failure_bodies(blocks, 'ContractTests.test_gateway_probe')
        self.assertEqual(len(bodies), 1)
        self.assertIn('#933-4:', '\n'.join(bodies[0]))

    def test_bound_bodies_reject_a_lookalike_test(self):
        bodies = bound_failure_bodies(unittest_blocks(OTHER_TEST_LOG),
                                      'ContractTests.test_occupied_mock_port_blocks_round')
        self.assertEqual(bodies, [])


class ClassifyTests(unittest.TestCase):
    def test_bound_test_failed_at_its_own_assertion_is_killed(self):
        status, reason = classify(run(1, KILLED_LOG), TEST, MARKER)
        self.assertEqual(status, 'KILLED', reason)

    def test_still_passing_test_is_survived(self):
        status, _reason = classify(run(0, OK_LOG), TEST, MARKER)
        self.assertEqual(status, 'SURVIVED')

    def test_error_block_with_a_marker_echo_is_invalid(self):
        status, reason = classify(run(1, ERROR_ECHO_LOG), TEST, MARKER)
        self.assertEqual(status, 'INVALID', reason)
        self.assertIn('no FAIL block', reason)

    def test_marker_on_a_diff_continuation_line_is_killed(self):
        status, reason = classify(run(1, DIFF_CONTINUATION_LOG),
                                  'ContractTests.test_mock_scenario', '#933-3:')
        self.assertEqual(status, 'KILLED', reason)

    def test_sibling_error_subtest_does_not_negate_the_kill(self):
        status, reason = classify(run(1, FAIL_PLUS_ERROR_LOG),
                                  'ContractTests.test_gateway_marker', '#933-1:')
        self.assertEqual(status, 'KILLED', reason)

    def test_older_python_qualname_format_is_tolerated(self):
        status, reason = classify(run(1, OLD_FORMAT_LOG),
                                  'ContractTests.test_ping_sample',
                                  '#933-7: the mock closed')
        self.assertEqual(status, 'KILLED', reason)

    def test_marker_only_outside_the_bound_block_is_invalid(self):
        status, reason = classify(run(1, MARKER_ELSEWHERE_LOG), TEST, MARKER)
        self.assertEqual(status, 'INVALID', reason)
        self.assertIn('wrong failure point', reason)

    def test_marker_in_another_test_is_invalid(self):
        status, reason = classify(run(1, OTHER_TEST_LOG), TEST, MARKER)
        self.assertEqual(status, 'INVALID', reason)
        self.assertIn('no FAIL block', reason)

    def test_crash_without_a_unittest_summary_is_invalid(self):
        status, reason = classify(run(1, CRASH_LOG), TEST, MARKER)
        self.assertEqual(status, 'INVALID', reason)
        self.assertIn('no unittest summary', reason)

    def test_signal_death_is_not_a_kill(self):
        status, reason = classify(run(-11, KILLED_LOG), TEST, MARKER)
        self.assertEqual(status, 'INVALID', reason)
        self.assertIn('unexpected exit code', reason)

    def test_subtest_failures_of_the_bound_test_can_be_killed(self):
        status, reason = classify(run(1, SUBTEST_LOG),
                                  'ContractTests.test_gateway_probe', '#933-4:')
        self.assertEqual(status, 'KILLED', reason)

    def test_alternative_fragments_are_all_accepted(self):
        alternatives = ('#933-7: a pong', '#933-7: the mock closed')
        self.assertEqual(classify(run(1, KILLED_LOG), TEST, alternatives)[0], 'INVALID')
        closed = KILLED_LOG.replace(MARKER, '#933-7: the mock closed the connection during the ping exchange')
        closed = closed.replace('test_occupied_mock_port_blocks_round', 'test_ping_sample')
        status, reason = classify(run(1, closed), 'ContractTests.test_ping_sample', alternatives)
        self.assertEqual(status, 'KILLED', reason)


class RestoreFailureTests(unittest.TestCase):
    """fix-r3 P1-3: the persisted summary is only trustworthy after a clean exit."""

    def test_final_restore_failure_invalidates_the_whole_summary(self):
        # The review's P1: every mutant finished (rows flushed as KILLED),
        # then mutating_sources raised on exit — and summary.json kept saying
        # KILLED while only the exit code told the truth. Run one real table
        # row against source COPIES with run_owned faked and the context exit
        # failing, and require the persisted rows to be rewritten INVALID.
        # fix-r4 P1-2: every file main() hands to mutating_sources gets its
        # own copy — isolation_probe.mjs included (the sixth since fix-r3) —
        # and every write must land inside the copy directory: the fake
        # context's write-back is exactly where an unpatched real path would
        # be rewritten outside the real source guard's lock.
        name, _path, old, new = negative_controls.mutations[0]
        assert name == 'allow-rule-drops-port'
        test = negative_controls.tests[name]
        fragment = negative_controls.failure_fragments[name]
        sources = (negative_controls.policy, negative_controls.common, negative_controls.compare,
                   negative_controls.run_round, negative_controls.mock_gateway,
                   negative_controls.isolation_probe)
        # The real probe source must survive this test byte-for-byte even if
        # the copy wiring regresses, so pin it before anything is patched.
        real_probe = negative_controls.isolation_probe
        real_probe_sha256 = hashlib.sha256(real_probe.read_bytes()).hexdigest()
        tmp = Path(tempfile.mkdtemp(prefix='negctl-restore-'))
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        copies = [tmp / source.name for source in sources]
        for source, copy in zip(sources, copies):
            copy.write_bytes(source.read_bytes())
        pristine = copies[0].read_bytes()
        written = []

        @contextmanager
        def fake_mutating_sources(_root, paths, *, baseline='HEAD'):
            snapshots = {path: path.read_bytes() for path in paths}
            try:
                yield snapshots
            finally:
                for path, data in snapshots.items():
                    path.write_bytes(data)
                    written.append(path)
                raise RuntimeError('injected final restore verification failure')

        fail_log = """F
======================================================================
FAIL: test_mock_profile_appends_only_the_loopback_allow (__main__.ContractTests.test_mock_profile_appends_only_the_loopback_allow) (side='ts', mock=True)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 947, in test_mock_profile_appends_only_the_loopback_allow
    self.assertEqual(
AssertionError: '162612cc65cda15e942dea86b4a2a73bd44b6cf0464bd4ef8178ef57c6e5fa20' != '0f1e2d3c4b5a69788695a4b3c2d1e0f9a8b7c6d5e4f30291a8b7c6d5e4f30291a' : #933-2: the mock TS profile changed

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""

        def fake_run_owned(command, **_kwargs):
            # The mutated copy is on disk exactly during the negative run.
            if new.encode() in copies[0].read_bytes():
                return subprocess.CompletedProcess(command, 1, fail_log, '')
            return subprocess.CompletedProcess(command, 0, OK_LOG, '')

        out = tmp / 'evidence'
        with patch.object(negative_controls, 'mutations', [(name, copies[0], old, new)]), \
             patch.object(negative_controls, 'policy', copies[0]), \
             patch.object(negative_controls, 'common', copies[1]), \
             patch.object(negative_controls, 'compare', copies[2]), \
             patch.object(negative_controls, 'run_round', copies[3]), \
             patch.object(negative_controls, 'mock_gateway', copies[4]), \
             patch.object(negative_controls, 'isolation_probe', copies[5]), \
             patch.object(negative_controls, 'mutating_sources', fake_mutating_sources), \
             patch.object(negative_controls, 'run_owned', fake_run_owned), \
             patch.object(sys, 'argv', ['negative_controls.py', str(out), name]):
            with self.assertRaises(SystemExit) as caught:
                negative_controls.main()
        self.assertEqual(caught.exception.code, 1,
                         'a restore failure must exit nonzero, never pass')
        # fix-r4 P1-2: nothing may write a real source — every mutating_sources
        # write-back target must be a copy under the test's own directory.
        self.assertTrue(written, 'the fake context must actually exercise its write-back')
        for path in written:
            self.assertTrue(path.is_relative_to(tmp),
                            f'a real source was written outside the copies: {path}')
        self.assertIn(copies[5], written,
                      'the sixth source copy (isolation_probe) must participate in the write-back')
        self.assertEqual(hashlib.sha256(real_probe.read_bytes()).hexdigest(), real_probe_sha256,
                         'the real isolation_probe.mjs must be byte-identical after the test')
        summary = json.loads((out / 'summary.json').read_text())
        self.assertEqual(summary['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                         'the flushed KILLED row must be rewritten INVALID after the restore failure')
        self.assertEqual([row['status'] for row in summary['controls']], ['INVALID'],
                         'no row may stay KILLED when the sources were not verifiably restored')
        self.assertIn('source guard', summary['controls'][0]['reason'])
        self.assertIn('injected final restore verification failure', summary['controls'][0]['reason'],
                      'the persisted reason must name the restore failure')
        self.assertEqual(copies[0].read_bytes(), pristine,
                         'the source copy must end up byte-identical to its original')
        # The fragment above must really be what the faked run is judged by.
        self.assertEqual(classify(run(1, fail_log), test, fragment)[0], 'KILLED')


if __name__ == '__main__':
    unittest.main(verbosity=2)

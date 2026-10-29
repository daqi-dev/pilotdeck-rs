# anchor-selfcheck: unsupported kill-judge and runner-publication unit tests; no product mutation table
"""Unit tests of the #1064 r1 negative-control apparatus, on synthetic logs only.

The judge is the piece a forged log could otherwise fool, so every forgery
class the sibling fixtures demonstrated gets its own case here: a marker
echoed in an ERROR traceback, a marker in another test's failure, a crash
without a unittest summary. The r1 P1-3 cases drive the delivered main()
end to end with only the source guard and the bound-test runner mocked: a
cancellation during the final restore verification must void the recorded
rows and publish INVALID (never an all-KILLED summary whose sources were not
verifiably restored), a SystemExit must be normalized to exit 2, a SIGINT
inside the void publish must leave no readable KILLED summary, and the runner
itself must sit in the guarded set. No real source is mutated; nothing here
runs cargo, node, or the cli-parity scripts.
"""
from contextlib import contextmanager
import hashlib
import json
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import MagicMock, patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import negative_controls
from negative_controls import bound_failure_bodies, classify, passed, unittest_blocks

TEST = 'ContractTests.test_interrupt_during_acquisition_leaves_no_lease'
MARKER = '#1064: an interrupted acquisition must not leave a lease'

KILLED_LOG = """F
======================================================================
FAIL: test_interrupt_during_acquisition_leaves_no_lease (test_cli_parity.ContractTests.test_interrupt_during_acquisition_leaves_no_lease)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 10, in test_interrupt_during_acquisition_leaves_no_lease
    self.assertFalse(lock.exists())
AssertionError: True is not false : #1064: an interrupted acquisition must not leave a lease

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""

# The marker is only the source echo of an ERROR block; a "marker appears
# somewhere" judge would count this as a kill.
ERROR_ECHO_LOG = """E
======================================================================
ERROR: test_interrupt_during_acquisition_leaves_no_lease (test_cli_parity.ContractTests.test_interrupt_during_acquisition_leaves_no_lease)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 42, in test_interrupt_during_acquisition_leaves_no_lease
    self.fail('#1064: an interrupted acquisition must not leave a lease')
    ^^^^^^^^^^^^^
KeyError: 'ownerIdentity'

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (errors=1)
"""

# The bound test fails for an unrelated reason; the marker only shows up in a
# plain stdout line another helper printed.
MARKER_ELSEWHERE_LOG = """probe: #1064: an interrupted acquisition must not leave a lease
F
======================================================================
FAIL: test_interrupt_during_acquisition_leaves_no_lease (test_cli_parity.ContractTests.test_interrupt_during_acquisition_leaves_no_lease)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 10, in test_interrupt_during_acquisition_leaves_no_lease
    self.assertIn('x', y)
AssertionError: 'x' not found in 'y'

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""

# A different test carries the exact marker; the bound test fails for its own
# unrelated reason.
OTHER_TEST_LOG = """F
======================================================================
FAIL: test_guard_wait_is_bounded (test_cli_parity.ContractTests.test_guard_wait_is_bounded)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 10, in test_guard_wait_is_bounded
    self.fail('#1064: an interrupted acquisition must not leave a lease')

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""

# A subTest failure: the header carries the description before the separator
# and the module prefix may be stripped to the class; the bound body must
# still be found.
SUBTEST_LOG = """F
======================================================================
FAIL: test_interrupt_during_acquisition_leaves_no_lease (test_cli_parity.ContractTests.test_interrupt_during_acquisition_leaves_no_lease) [owner-written]
----------------------------------------------------------------------
Traceback (most recent call last):
  File "scripts/cli-parity/test_cli_parity.py", line 10, in test_interrupt_during_acquisition_leaves_no_lease
    self.assertFalse(lock.exists())
AssertionError: True is not false : #1064: an interrupted acquisition must not leave a lease

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""

# Two consecutive failures separated by the report separator; the second
# block must not swallow the first one's tail.
TWO_BLOCK_LOG = """FF
======================================================================
FAIL: test_guard_wait_is_bounded (test_cli_parity.ContractTests.test_guard_wait_is_bounded)
----------------------------------------------------------------------
AssertionError: 1 != 2
======================================================================
FAIL: test_interrupt_during_acquisition_leaves_no_lease (test_cli_parity.ContractTests.test_interrupt_during_acquisition_leaves_no_lease)
----------------------------------------------------------------------
AssertionError: True is not false : #1064: an interrupted acquisition must not leave a lease

----------------------------------------------------------------------
Ran 1 test in 0.123s

FAILED (failures=1)
"""


def result(code, log):
    return subprocess.CompletedProcess([], code, log, '')


class UnittestBlocksTests(unittest.TestCase):
    def test_killed_log_yields_one_fail_block(self):
        blocks = unittest_blocks(KILLED_LOG)
        self.assertEqual(len(blocks), 1, '#1064 one FAIL block must be parsed')
        kind, name, qualname, body = blocks[0]
        self.assertEqual(kind, 'FAIL', '#1064 the block kind must be FAIL')
        self.assertEqual(name, TEST.split('.', 1)[1])
        self.assertIn('test_cli_parity.' + TEST, qualname)
        self.assertTrue(any(line.startswith('AssertionError: ') for line in body),
                        '#1064 the block body must hold the assertion')

    def test_error_block_is_parsed_and_kind_preserved(self):
        blocks = unittest_blocks(ERROR_ECHO_LOG)
        self.assertEqual([kind for kind, *_ in blocks], ['ERROR'],
                         '#1064 an ERROR block must be parsed with its kind')
        self.assertTrue(any('KeyError' in line for line in blocks[0][3]),
                        '#1064 the ERROR body must be captured')

    def test_consecutive_blocks_do_not_bleed(self):
        blocks = unittest_blocks(TWO_BLOCK_LOG)
        self.assertEqual([name for _kind, name, _q, _b in blocks],
                         ['test_guard_wait_is_bounded', TEST.split('.', 1)[1]],
                         '#1064 both blocks must be parsed')
        self.assertFalse(any('not false' in line for line in blocks[0][3]),
                          '#1064 the report separator must close a block')
        self.assertTrue(any('AssertionError: True is not false' in line
                             for line in blocks[1][3]),
                        '#1064 the second block body must be captured')

    def test_subtest_preamble_is_tolerated(self):
        blocks = unittest_blocks(SUBTEST_LOG)
        self.assertEqual(len(blocks), 1,
                         '#1064 a subTest description must not break parsing')


class BoundFailureBodiesTests(unittest.TestCase):
    def test_full_qualname_is_matched(self):
        blocks = unittest_blocks(KILLED_LOG)
        self.assertEqual(len(bound_failure_bodies(blocks, TEST)), 1,
                         '#1064 the dotted qualname must match the bound test')

    def test_old_style_qualname_is_matched(self):
        log = KILLED_LOG.replace(
            f'({TEST.replace("ContractTests.", "test_cli_parity.ContractTests.")})',
            '(test_cli_parity.ContractTests)')
        self.assertEqual(len(bound_failure_bodies(unittest_blocks(log), TEST)), 1,
                         '#1064 the bare-class qualname must match the bound test')

    def test_other_tests_are_excluded(self):
        self.assertEqual(bound_failure_bodies(unittest_blocks(OTHER_TEST_LOG), TEST), [],
                         '#1064 another test\'s failure must never count')

    def test_error_blocks_are_excluded(self):
        self.assertEqual(bound_failure_bodies(unittest_blocks(ERROR_ECHO_LOG), TEST), [],
                         '#1064 an ERROR block must never count as a kill')


class ClassifyTests(unittest.TestCase):
    def test_marker_in_assertion_kills(self):
        status, reason = classify(result(1, KILLED_LOG), TEST, MARKER)
        self.assertEqual(status, 'KILLED', '#1064 a marker inside the bound '
                         f'AssertionError must kill: {reason}')

    def test_subtest_assertion_kills(self):
        status, reason = classify(result(1, SUBTEST_LOG), TEST, MARKER)
        self.assertEqual(status, 'KILLED', '#1064 a subTest failure at the '
                         f'bound assertion must kill: {reason}')

    def test_passing_run_survives(self):
        log = 'Ran 1 test\n\nOK\n'
        status, _ = classify(result(0, log), TEST, MARKER)
        self.assertEqual(status, 'SURVIVED',
                         '#1064 a green bound run must be SURVIVED')

    def test_error_echo_is_invalid(self):
        status, _ = classify(result(1, ERROR_ECHO_LOG), TEST, MARKER)
        self.assertEqual(status, 'INVALID',
                         '#1064 a marker echoed in an ERROR traceback must be INVALID')

    def test_marker_elsewhere_is_invalid(self):
        status, _ = classify(result(1, MARKER_ELSEWHERE_LOG), TEST, MARKER)
        self.assertEqual(status, 'INVALID',
                         '#1064 a stdout marker echo without the bound assertion must be INVALID')

    def test_other_test_failure_is_invalid(self):
        status, _ = classify(result(1, OTHER_TEST_LOG), TEST, MARKER)
        self.assertEqual(status, 'INVALID',
                         '#1064 another test carrying the marker must be INVALID')

    def test_signal_death_is_invalid(self):
        status, _ = classify(result(2, 'Alarm clock\n'), TEST, MARKER)
        self.assertEqual(status, 'INVALID',
                         '#1064 a signal death (no unittest summary) must be INVALID')

    def test_wrong_failure_point_is_invalid(self):
        status, _ = classify(result(1, MARKER_ELSEWHERE_LOG), TEST, MARKER)
        self.assertEqual(status, 'INVALID',
                         '#1064 failing without the #1064 assertion must be INVALID')

    def test_passed_requires_the_ok_line(self):
        self.assertTrue(passed(result(0, 'Ran 1 test\n\nOK\n'), TEST),
                        '#1064 a green run must pass the judge')
        self.assertFalse(passed(result(0, 'Ran 1 test\n'), TEST),
                         '#1064 a run without OK must not pass the judge')
        self.assertFalse(passed(result(1, 'Ran 1 test\n\nOK\n'), TEST),
                         '#1064 a nonzero exit must not pass the judge')


class RunnerInvalidationControls(unittest.TestCase):
    """The delivered runner must never keep a stale green summary (#1064 r1 P1-3)."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-clb-runner-')
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        self.source = self.root / 'mutant_source.py'
        self.source.write_text('x = 0\n')
        self.runner = negative_controls

    def _read_summary(self, out):
        path = out / 'summary.json'
        if not path.is_file():
            return None
        try:
            summary = json.loads(path.read_text())
        except ValueError as error:
            self.fail(f'#1064 r1 P1-3 summary.json must be valid JSON: {error}')
        self.assertIsInstance(summary, dict,
                              '#1064 r1 P1-3 the summary must keep its shape')
        return summary

    def _run_test(self):
        """positive/negative/restored by label suffix, like the real phases."""
        def run_test(_out, label, _test):
            if label.endswith('-positive') or label.endswith('-restored'):
                return result(0, 'Ran 1 test\n\nOK\n')
            return result(1, KILLED_LOG)
        return run_test

    def _drive(self, *, guard_error=None):
        """Run the real ``main`` with only the guard and the bound runs mocked."""
        out = Path(tempfile.mkdtemp(prefix='out-', dir=self.root))
        source = self.source
        captured = {'mutating_calls': 0, 'paths': None}

        class Guard:
            def __enter__(self):
                return {source: b'x = 0\n'}

            def __exit__(self, *exc):
                if guard_error is not None:
                    raise guard_error
                return False

        def mutating(root, paths, **kwargs):
            captured['mutating_calls'] += 1
            captured['paths'] = list(paths)
            return Guard()

        with patch.multiple(self.runner,
                            mutations=[('one', source, 'x = 0', 'x = 1')],
                            tests={'one': TEST},
                            failure_fragments={'one': MARKER},
                            mutating_sources=mutating,
                            run_test=self._run_test(),
                            compile_check=lambda path: None), \
                patch.object(sys, 'argv',
                             ['negative_controls.py', str(out), 'one']):
            try:
                code = self.runner.main()
            except BaseException as error:
                # Only the injected cancellation is observed; a real Ctrl-C
                # (a different exception object) must still abort the test.
                # Every SystemExit from the failure handler is re-wrapped by
                # main() as SystemExit(2) (#1064 r1 P1-3), so it is accepted
                # by type, not identity.
                if error is not guard_error and not (
                        isinstance(guard_error, SystemExit)
                        and isinstance(error, SystemExit)):
                    raise
                code = error
        return code, self._read_summary(out), captured

    def test_sample_run_stays_green_and_guards_the_runner(self):
        code, summary, captured = self._drive()
        self.assertEqual(code, 0, '#1064 r1 the sample run must finish green')
        self.assertEqual([row['status'] for row in summary['controls']], ['KILLED'],
                         '#1064 r1 the sample mutant must be killed')
        self.assertEqual(summary['counts']['KILLED'], 1,
                         '#1064 r1 the counts block must match the rows')
        self.assertIn(self.runner.RUNNER, captured['paths'],
                      '#1064 r1 P1-3 the delivered runner must sit in the guarded set')
        self.assertEqual(captured['mutating_calls'], 1,
                          '#1064 r1 the guard must wrap the whole run exactly once')

    def test_final_guard_failure_publishes_invalid(self):
        code, summary, _ = self._drive(guard_error=RuntimeError('restore failed'))
        self.assertEqual(code, 2, '#1064 r1 P1-3 a guard failure must exit 2')
        self.assertIsNotNone(summary,
                              '#1064 r1 P1-3 a guard failure must publish an INVALID summary')
        self.assertEqual([row['status'] for row in summary['controls']], ['INVALID'],
                         '#1064 r1 P1-3 a guard failure must void the recorded rows')
        self.assertIn('source guard', summary['controls'][0]['reason'],
                      '#1064 r1 P1-3 the invalidation reason must be recorded')

    def test_final_guard_interrupt_voids_and_reraises(self):
        """r1 P1-3: a cancellation after the last row must void it, then propagate.

        This is the bound test of runner-baseexception-narrowed: under the
        mutant (``except Exception``), the KeyboardInterrupt skips the INVALID
        publication entirely and leaves no summary at all.
        """
        for canceled in (KeyboardInterrupt('restore interrupted'),
                         SystemExit('restore exited'),
                         SystemExit(0), SystemExit(None)):
            with self.subTest(canceled=type(canceled).__name__):
                code, summary, _ = self._drive(guard_error=canceled)
                self.assertIs(type(code), type(canceled),
                              '#1064 r1 P1-3 the cancellation must be re-raised')
                if isinstance(code, SystemExit):
                    self.assertEqual(
                        code.code, 2,
                        '#1064 r1 P1-3 an INVALID summary must pair with exit code 2')
                self.assertIsNotNone(
                    summary,
                    '#1064 r1 P1-3 a cancellation after the last row must not keep KILLED')
                self.assertEqual([row['status'] for row in summary['controls']],
                                 ['INVALID'],
                                 '#1064 r1 P1-3 a cancellation after the last row must not keep KILLED')
                self.assertIn('source guard', summary['controls'][0]['reason'],
                              '#1064 r1 P1-3 the invalidation reason must be recorded')

    def test_final_guard_systemexit_is_normalized_to_two(self):
        """r1 P1-3: a SystemExit from the guard must exit 2, never the raw code.

        This is the bound test of runner-exit-normalization-removed: under the
        mutant, SystemExit('restore exited') is re-raised with its string code
        (process status 1) instead of the normalized 2.
        """
        for canceled in (SystemExit('restore exited'), SystemExit(0),
                         SystemExit(None)):
            with self.subTest(code=repr(canceled.code)):
                code, summary, _ = self._drive(guard_error=canceled)
                self.assertIsInstance(code, SystemExit,
                                     '#1064 r1 P1-3 the SystemExit path must be exercised')
                self.assertEqual(code.code, 2,
                                 '#1064 r1 P1-3 a SystemExit from the guard must exit 2, never the raw code')
                self.assertEqual([row['status'] for row in summary['controls']],
                                 ['INVALID'],
                                 '#1064 r1 P1-3 the normalized exit must pair with INVALID')

    def test_interrupt_during_void_publish_leaves_no_killed_summary(self):
        """r1 P1-3: a SIGINT inside the void publish must not leave KILLED.

        This is the bound test of runner-publish-before-verify. File
        operations are mocks and the SIGINT is a real Python signal raised
        from the atomic rename while the INVALID payload is being published:
        under the fix, the only write so far was the in-memory record(), so
        no summary exists at all; under the mutant, record() had already
        published the all-KILLED file and it survives the interrupt.
        """
        disk = {}
        files = {}

        def entry(name):
            if name not in files:
                files[name] = MagicMock(name=name)
            return files[name]

        out = MagicMock(name='out')
        out.__truediv__.side_effect = entry
        tmp = entry('summary.json.tmp')
        tmp.write_text.side_effect = lambda payload: disk.update(staged=payload)

        def publish(_destination):
            # The window: the INVALID temp is complete, and the SIGINT lands
            # before the rename.
            if any(row['status'] == 'INVALID'
                   for row in json.loads(disk['staged'])['controls']):
                signal.raise_signal(signal.SIGINT)
            disk['summary'] = disk['staged']

        tmp.replace.side_effect = publish
        source = MagicMock(name='source')

        class Guard:
            def __enter__(self):
                return {source: b'x = 0\n'}

            def __exit__(self, *exc):
                raise RuntimeError('restore verification failed')

        def mutating(root, paths, **kwargs):
            return Guard()

        # raise_signal delivers KeyboardInterrupt only under the default
        # disposition; a batch parent can inherit SIG_IGN, so install it for
        # the duration of this test and restore whatever was there.
        previous = signal.signal(signal.SIGINT, signal.default_int_handler)
        self.addCleanup(signal.signal, signal.SIGINT, previous)
        with patch.multiple(
                self.runner,
                mutations=[('one', source, 'x = 0', 'x = 1')],
                tests={'one': TEST},
                failure_fragments={'one': MARKER},
                new_output_directory=lambda _: out,
                mutating_sources=mutating,
                run_test=self._run_test(),
                compile_check=lambda path: None), \
                patch.object(sys, 'argv',
                            ['negative_controls.py', str(self.root / 'void-publish'), 'one']):
            try:
                code = self.runner.main()
            except BaseException as error:
                code = error
        message = ('#1064 r1 P1-3 an interrupt during the void publish must not '
                   'leave a readable KILLED summary')
        try:
            summary = json.loads(disk['summary']) if 'summary' in disk else None
        except ValueError as error:
            self.fail(f'{message}: unreadable summary: {error}')
        self.assertIsInstance(code, KeyboardInterrupt, message)
        self.assertTrue(
            summary is None
            or [row['status'] for row in summary['controls']] != ['KILLED'],
            message)

    def test_existing_evidence_is_refused_byte_identical(self):
        """r1 P1-3: a pre-existing evidence directory is refused, never edited.

        Nothing may be deleted or renamed before admission: a mistyped
        ``output`` must destroy no byte of another run's evidence.
        """
        message = ('#1064 r1 P1-3 a pre-existing evidence directory must be '
                   'refused with every byte preserved')
        stale = json.dumps({'controls': [
            {'mutation': 'one', 'status': 'KILLED', 'reason': '#1064 stale'}],
            'counts': {'KILLED': 1}}) + '\n'
        for label, extra in (
                ('summary-only', {}),
                ('summary-and-evidence',
                 {'one-positive.log': 'Ran 1 test\n\nOK\n'})):
            with self.subTest(case=label):
                folder = Path(tempfile.mkdtemp(prefix='reuse-', dir=self.root))
                (folder / 'summary.json').write_text(stale)
                for name, text in extra.items():
                    (folder / name).write_text(text)

                def snapshot():
                    return {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                            for path in folder.iterdir()}

                before = snapshot()

                def refuse(*_args, **_kwargs):
                    raise RuntimeError('#1064 r1 P1-3 mutation must not start')

                with patch.multiple(self.runner,
                                    mutations=[('one', self.source, 'x = 0', 'x = 1')],
                                    mutating_sources=refuse), \
                        patch.object(sys, 'argv',
                                     ['negative_controls.py', str(folder), 'one']):
                    try:
                        code = self.runner.main()
                    except BaseException as error:
                        code = error
                after = snapshot()
                self.assertEqual(
                    after, before,
                    f'{message}: the {label} directory was edited')
                self.assertIsInstance(code, SystemExit, f'{message} ({label})')
                self.assertIn('#953 refusing to reuse a non-empty output directory',
                              str(code), f'{message} ({label})')


if __name__ == '__main__':
    unittest.main()

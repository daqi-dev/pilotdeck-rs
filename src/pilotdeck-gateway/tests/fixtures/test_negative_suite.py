# anchor-selfcheck: unsupported unit tests of the shared mutation suite runner
"""Harness fault injection only; no cargo or product source mutation here."""
import contextlib
import io
import json
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import negative_suite as suite

sys.path.insert(0, str(Path(__file__).resolve().parent / '_shared'))
import negative_sqlite_header as sqlite_header


TEST = 'runtime::fixture::bound_test'
ANCHOR = 'bound assertion message'


def output(kind):
    if kind == 'pass':
        return subprocess.CompletedProcess([], 0,
            f'test {TEST} ... ok\ntest result: ok. 1 passed; 0 failed;\n', '')
    if kind == 'kill':
        return subprocess.CompletedProcess([], 101,
            f'test {TEST} ... FAILED\ntest result: FAILED\n',
            f"thread '{TEST}' panicked at fixture.rs:1:1:\n{ANCHOR}\n")
    if kind == 'sibling':
        return subprocess.CompletedProcess([], 101,
            f'test {TEST} ... FAILED\ntest result: FAILED\n',
            f"thread 'other::test' panicked at fixture.rs:2:2:\n{ANCHOR}\n")
    if kind == 'stdout-only':
        return subprocess.CompletedProcess([], 101,
            f'test {TEST} ... FAILED\n{ANCHOR}\ntest result: FAILED\n', '')
    raise ValueError(kind)


class SuiteTests(unittest.TestCase):
    def exercise(self, first, *, positive=None, restored=None, bad_anchor=False, guard=None,
                 guard_exit=None, restore_failure=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'fixture.txt'
            source.write_bytes(b'original')
            out = root / 'logs'
            out.mkdir()
            cases = [suite.Case(name, source, 'absent' if bad_anchor and name == 'first' else 'original',
                                name, TEST, ANCHOR) for name in ('first', 'second')]
            calls = []
            write_bytes = Path.write_bytes

            def restore(path, data):
                if restore_failure and path == source and path.read_text() == 'first':
                    raise OSError('injected restoration failure')
                return write_bytes(path, data)

            def leave_guard(*args):
                write_bytes(source, b'original')
                if guard_exit:
                    raise guard_exit
                return False

            def run(command, **kwargs):
                self.assertEqual(command[-4:], [TEST, '--', '--exact', '--include-ignored'],
                                 'bound ignored tests must execute without --nocapture')
                calls.append(source.read_text())
                if source.read_text() == 'first':
                    if isinstance(first, Exception):
                        raise first
                    return first
                if source.read_text() == 'second':
                    return output('kill')
                if len(calls) == 1 and positive:
                    return positive
                if len(calls) == (2 if bad_anchor or positive else 3) and restored:
                    return restored
                return output('pass')

            def mutate(text, scope, replacements):
                old, new = replacements[0]
                if text.count(old) != 1:
                    raise AssertionError('missing anchor')
                return text.replace(old, new)

            with patch.object(suite, 'mutating_sources') as sources, \
                    patch.object(suite, 'wait_for_slot', return_value=output('pass')), \
                    patch.object(suite, 'run_owned', side_effect=run), \
                    patch.object(Path, 'write_bytes', new=restore), \
                    contextlib.redirect_stdout(io.StringIO()) as stdout:
                sources.return_value.__enter__.return_value = {source: b'original'}
                sources.return_value.__enter__.side_effect = guard
                sources.return_value.__exit__.side_effect = leave_guard
                code = suite.run_cases(root, cases, out, {'CARGO_TARGET_DIR': str(root / 'target')}, mutate)
            self.assertEqual(source.read_bytes(), b'original', 'each injected fault restores bytes')
            summary = json.loads((out / 'summary.json').read_text())
            self.assertEqual([r['mutation'] for r in summary['controls']], ['first', 'second'],
                             'all selected controls must be accounted for')
            if guard is None and not restore_failure:
                expected = 'INVALID' if guard_exit else 'KILLED'
                self.assertEqual(summary['controls'][1]['status'], expected, 'successor must run')
                self.assertEqual(calls[-3:], ['original', 'second', 'original'])
            if restore_failure:
                self.assertNotIn('second', calls, 'unsafe restoration must stop later mutations')
            return code, summary, stdout.getvalue()

    def test_all_killed_exits_zero(self):
        code, summary, text = self.exercise(output('kill'))
        self.assertEqual(code, 0)
        self.assertEqual(summary['counts'], {'KILLED': 2, 'SURVIVED': 0, 'INVALID': 0})
        self.assertIn('KILLED=2', text)

    def test_survivor_does_not_abort_successor(self):
        code, summary, _ = self.exercise(output('pass'))
        self.assertEqual(code, 1, 'survivor must not count as killed')
        self.assertEqual(summary['controls'][0]['status'], 'SURVIVED', 'survivor must not count as killed')

    def test_invalid_failures_never_count_as_kills_and_continue(self):
        failures = {
            'compile': subprocess.CompletedProcess([], 101, '', 'error[E0001]: compilation failed'),
            'wrong-test': subprocess.CompletedProcess([], 101, 'test other ... FAILED\ntest result: FAILED',
                                                       f'panicked at other.rs: {ANCHOR}'),
            'wrong-anchor': subprocess.CompletedProcess([], 101,
                f'test {TEST} ... FAILED\ntest result: FAILED', 'panicked at: unrelated assertion'),
            'zero-tests': subprocess.CompletedProcess([], 0, 'test result: ok. 0 passed; 0 failed;', ''),
            'timeout': subprocess.TimeoutExpired(['cargo'], 0.01, output=output('kill').stdout,
                                                 stderr=output('kill').stderr),
            'spawn-error': OSError('injected spawn failure'),
        }
        for name, result in failures.items():
            with self.subTest(name=name):
                code, summary, _ = self.exercise(result)
                self.assertEqual(code, 1)
                self.assertEqual(summary['controls'][0]['status'], 'INVALID')

    def test_bad_positive_and_restoration_continue(self):
        for phase in ('positive', 'restored'):
            with self.subTest(phase=phase):
                code, summary, _ = self.exercise(output('kill'), **{phase: output('kill')})
                self.assertEqual(code, 1)
                self.assertEqual(summary['controls'][0]['status'], 'INVALID')
                self.assertIn(phase, summary['controls'][0]['reason'])

    def test_bad_anchor_does_not_abort_successor(self):
        code, summary, _ = self.exercise(output('kill'), bad_anchor=True)
        self.assertEqual(code, 1)
        self.assertIn('anchor: AssertionError: missing anchor', summary['controls'][0]['reason'])

    def test_guard_refusal_accounts_for_every_case(self):
        # #1076 fix-r1: a guard that refuses to open never mutated anything, so
        # the pre-existing rc=1 contract stays (pending-reanchor test_controls
        # asserts the same value). Only a cancellation or an unverified restore
        # may use rc=2.
        code, summary, _ = self.exercise(output('kill'), guard=RuntimeError('baseline differs'))
        self.assertEqual(code, 1, '#1076 a HEAD guard refusal must keep exit code 1')
        self.assertEqual(summary['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 2},
                         '#1076 a guard refusal must account for every selected case')

    def test_guard_exit_invalidates_previous_kills(self):
        code, summary, _ = self.exercise(output('kill'), guard_exit=RuntimeError('guard restore failed'))
        self.assertEqual(code, 2, '#1076 guard restoration failure must exit 2')
        self.assertEqual(summary['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 2},
                         '#1076 guard restoration failure must void every recorded kill')
        self.assertTrue(all(row['status'] == 'INVALID' for row in summary['controls']),
                        '#1076 guard restoration failure invalidates every previous kill')

    def test_byte_restore_failure_stops_unsafe_mutation(self):
        code, summary, _ = self.exercise(output('kill'), restore_failure=True)
        self.assertEqual(code, 2, '#1076 an unverified byte restore must exit 2')
        self.assertEqual(summary['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 2},
                         '#1076 an unverified byte restore must void every row')
        self.assertIn('injected restoration failure', summary['controls'][0]['reason'],
                      '#1076 the restoration failure must be attached to the row')

    def drive_body_fault(self, label, body_error, guard_exit=None):
        """Drive one body-phase fault; return (exit code, published summary)."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'fixture.txt'
            source.write_bytes(b'original')
            out = root / 'logs'
            out.mkdir()
            case = suite.Case('one', source, 'original', 'mutant', TEST, ANCHOR)

            @contextlib.contextmanager
            def guard(_root, _paths, **_kwargs):
                try:
                    yield {source: b'original'}
                finally:
                    if guard_exit is not None:
                        raise guard_exit

            with patch.object(suite, 'mutating_sources', new=guard), \
                    patch.object(suite.pending, 'match', side_effect=body_error), \
                    patch.object(suite, 'wait_for_slot') as slot, \
                    patch.object(suite, 'run_owned') as cargo, \
                    contextlib.redirect_stdout(io.StringIO()):
                code = suite.run_cases(root, [case], out, {}, lambda *args: 'unused')
            summary = json.loads((out / 'summary.json').read_text())
            restored = source.read_bytes()
        self.assertFalse(slot.called or cargo.called,
                         f'{label} must not run cargo')
        self.assertEqual(restored, b'original',
                         f'{label} must leave the source untouched')
        return code, summary

    def test_in_window_refusal_keeps_exit_one(self):
        # #1076 fix-r1: a refusal after the guard opened but before any row
        # (expired anchor / malformed registration) is still a refusal -> rc=1;
        # only a restore that cannot be verified or a cancellation is rc=2.
        code, summary = self.drive_body_fault(
            '#1076 an in-window refusal', ValueError('expired anchor'))
        self.assertEqual(code, 1, '#1076 an in-window refusal must keep exit code 1')
        self.assertEqual(summary['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                         '#1076 an in-window refusal must account for every selected case')

    def test_body_refusal_with_guard_restore_failure_exits_two(self):
        # #1076 fix-r2 / Codex P1: the body already raised, then the guard's
        # own restore/re-verify fails while unwinding. ``phase`` still says
        # 'body', so the refusal branch must not win: this run is void -> rc=2.
        code, summary = self.drive_body_fault(
            '#1076 a body refusal plus an unverified guard exit',
            ValueError('expired anchor'),
            guard_exit=RuntimeError('restore verification failed'))
        self.assertEqual(code, 2,
                         '#1076 a body refusal plus an unverified guard exit must return 2')
        self.assertEqual(summary['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                         '#1076 a body refusal plus an unverified guard exit must void every row')
        self.assertIn('restore verification failed', summary['controls'][0]['reason'],
                      '#1076 the guard restore failure must be recorded on the voided row')

    def test_body_cancellation_keeps_exit_two(self):
        # #1076 fix-r2: a cancellation delivered inside the body is void even
        # when the guard restores cleanly; deleting ``not isinstance`` must
        # turn this case into rc=1.
        code, summary = self.drive_body_fault(
            '#1076 a body cancellation', KeyboardInterrupt('SIGINT in body'))
        self.assertEqual(code, 2,
                         '#1076 a body cancellation with a clean guard exit must return 2')
        self.assertEqual(summary['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                         '#1076 a body cancellation must publish an all-INVALID summary')
        self.assertIn('KeyboardInterrupt', summary['controls'][0]['reason'],
                      '#1076 a body cancellation must be recorded as INVALID')

    def test_sibling_panic_with_the_same_marker_is_rejected(self):
        # #818/#819/#930 defect channel: the marker lives in another test's
        # panic block. The named test's own FAILED line is not enough.
        case = suite.Case('test', None, '', '', TEST, ANCHOR)
        self.assertEqual(suite.classify(output('sibling'), case)[0], 'INVALID',
                         'a sibling panic must not kill the named test')

    def test_marker_in_named_stdout_is_rejected(self):
        case = suite.Case('test', None, '', '', TEST, ANCHOR)
        self.assertEqual(suite.classify(output('stdout-only'), case)[0], 'INVALID',
                         'stdout is not a panic payload')

    def test_alternative_markers_any_one_kills(self):
        # #818 fix-r4: a list means alternatives. live_negative's
        # live_factory_pre_aborted lists two assertions from different failure
        # points; a panic at the first never reaches the second.
        only_one = suite.Case('test', None, '', '', TEST, ['unreached alternative', ANCHOR])
        self.assertEqual(suite.classify(output('kill'), only_one)[0], 'KILLED',
                         'one bound alternative in the named payload must kill')
        none = suite.Case('test', None, '', '', TEST,
                          ['first alternative', 'second alternative'])
        self.assertEqual(suite.classify(output('kill'), none)[0], 'INVALID',
                         'no alternative in the named payload must stay invalid')
        self.assertEqual(suite.classify(output('sibling'), only_one)[0], 'INVALID',
                         'alternatives never widen evidence to a sibling panic')

    def test_each_failure_marker_is_required(self):
        case = suite.Case('test', None, '', '', TEST, ANCHOR)
        killed = output('kill')
        for marker in ('test result: FAILED', f'test {TEST} ... FAILED', 'panicked at', ANCHOR):
            with self.subTest(marker=marker):
                weakened = subprocess.CompletedProcess([], 101, killed.stdout.replace(marker, ''),
                                                        killed.stderr.replace(marker, ''))
                self.assertEqual(suite.classify(weakened, case)[0], 'INVALID',
                                 'every bound runtime marker is required')
        killed.returncode = 0
        self.assertEqual(suite.classify(killed, case)[0], 'INVALID')

    def test_positive_needs_exact_test_and_one_pass(self):
        passed = output('pass')
        for marker in (f'test {TEST} ... ok', 'test result: ok. 1 passed;'):
            with self.subTest(marker=marker):
                self.assertFalse(suite.passed(subprocess.CompletedProcess([], 0,
                                  passed.stdout.replace(marker, ''), ''), TEST),
                                 'positive needs exact test and one pass')


class CancellationTests(unittest.TestCase):
    """#1076: a cancellation in the guard exit must never keep an all-KILLED file."""

    def drive_run_cases(self, canceled):
        with tempfile.TemporaryDirectory(prefix='pdrs-1076-suite-') as directory:
            root = Path(directory)
            source = root / 'fixture.txt'
            source.write_bytes(b'original')
            out = root / 'logs'
            out.mkdir()
            case = suite.Case('one', source, 'original', 'mutant', TEST, ANCHOR)
            summary_present = []
            before_invalidate = []
            original_invalidate = suite.invalidate_completed

            def inject():
                if canceled == 'sigint':
                    signal.raise_signal(signal.SIGINT)
                raise SystemExit(256)

            @contextlib.contextmanager
            def guard(_root, _paths, **_kwargs):
                try:
                    yield {source: b'original'}
                finally:
                    summary_present.append((out / 'summary.json').exists())
                    inject()

            def capture(rows, reason):
                before_invalidate.append([row['status'] for row in rows])
                return original_invalidate(rows, reason)

            def run(command, **_kwargs):
                # #818 fix-r7 removed --nocapture from the shared judge's
                # command; the #1076 cancellation probe keeps asserting the
                # exact ignored binding under the merged command shape.
                self.assertEqual(command[-4:], [TEST, '--', '--exact', '--include-ignored'],
                                 '#1076 the cancellation probe must run the exact ignored test')
                if source.read_text() == 'mutant':
                    return output('kill')
                return output('pass')

            def mutate(text, _scope, replacements):
                old, new = replacements[0]
                if text.count(old) != 1:
                    raise ValueError('#1076 stale anchor in cancellation fixture')
                return text.replace(old, new, 1)

            previous = signal.signal(signal.SIGINT, signal.default_int_handler)
            self.addCleanup(signal.signal, signal.SIGINT, previous)
            with patch.object(suite, 'mutating_sources', new=guard), \
                    patch.object(suite, 'invalidate_completed', new=capture), \
                    patch.object(suite, 'wait_for_slot', return_value=output('pass')), \
                    patch.object(suite, 'run_owned', side_effect=run), \
                    contextlib.redirect_stdout(io.StringIO()):
                try:
                    code = suite.run_cases(root, [case], out,
                                           {'CARGO_TARGET_DIR': str(root / 'target')}, mutate)
                except BaseException as error:
                    code = error
            path = out / 'summary.json'
            summary = json.loads(path.read_text()) if path.is_file() else None
            return (code, summary, summary_present, before_invalidate,
                    source.read_bytes())

    def test_guard_sigint_voids_last_kill_and_exits_2(self):
        code, summary, present, before, restored = self.drive_run_cases('sigint')
        self.assertEqual(code, 2, '#1076 a SIGINT in the guard exit must return 2')
        self.assertEqual(present, [False],
                         '#1076 no summary may exist while the guard is unverified')
        self.assertEqual(before, [['KILLED']],
                         '#1076 the last row must be KILLED before the guard exit')
        self.assertEqual(restored, b'original',
                         '#1076 the cancellation fixture must restore its bytes')
        self.assertIsNotNone(summary, '#1076 the failed guard exit must publish summary.json')
        self.assertEqual([row['status'] for row in summary['controls']], ['INVALID'],
                         '#1076 a SIGINT must not leave the final row KILLED')
        self.assertIn('source guard: KeyboardInterrupt', summary['controls'][0]['reason'],
                      '#1076 the SIGINT invalidation reason must be recorded')
        self.assertEqual(summary['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                         '#1076 the published counts must be INVALID after SIGINT')

    def test_guard_systemexit_256_voids_last_kill_and_exits_2(self):
        code, summary, present, before, restored = self.drive_run_cases('systemexit')
        self.assertIsInstance(code, SystemExit,
                              '#1076 SystemExit(256) must be re-raised, not swallowed')
        self.assertEqual(code.code, 2, '#1076 SystemExit(256) must normalize to exit code 2')
        self.assertEqual(present, [False],
                         '#1076 no summary may exist while the guard is unverified')
        self.assertEqual(before, [['KILLED']],
                         '#1076 the last row must be KILLED before the guard exit')
        self.assertEqual(restored, b'original',
                         '#1076 the cancellation fixture must restore its bytes')
        self.assertIsNotNone(summary, '#1076 the failed guard exit must publish summary.json')
        self.assertEqual([row['status'] for row in summary['controls']], ['INVALID'],
                         '#1076 SystemExit(256) must not leave the final row KILLED')
        self.assertIn('source guard: SystemExit', summary['controls'][0]['reason'],
                      '#1076 the SystemExit invalidation reason must be recorded')
        self.assertEqual(summary['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                         '#1076 the published counts must be INVALID after SystemExit')

    def test_publish_summary_publishes_atomically(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1076-suite-atomic-') as directory:
            out = Path(directory)
            target = out / 'summary.json'
            target.write_text('previous\n')
            row = {'mutation': 'one', 'test': TEST, 'assertion': ANCHOR,
                   'status': 'INVALID', 'reason': '#1076 forced INVALID publish'}
            real_write = Path.write_text
            real_replace = Path.replace

            def interrupt_write(path, text, *args, **kwargs):
                if path.name == 'summary.json':
                    real_write(path, text[:5], *args, **kwargs)
                    signal.raise_signal(signal.SIGINT)
                return real_write(path, text, *args, **kwargs)

            def interrupt_replace(path, destination, *args, **kwargs):
                if Path(destination).name == 'summary.json':
                    signal.raise_signal(signal.SIGINT)
                return real_replace(path, destination, *args, **kwargs)

            previous = signal.signal(signal.SIGINT, signal.default_int_handler)
            try:
                with patch.object(Path, 'write_text', new=interrupt_write), \
                        patch.object(Path, 'replace', new=interrupt_replace), \
                        contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaises(
                            KeyboardInterrupt,
                            msg='#1076 the suite atomic publish interrupt must be delivered'):
                        suite.publish_summary(out, [row])
            finally:
                signal.signal(signal.SIGINT, previous)
            self.assertEqual(target.read_bytes(), b'previous\n',
                             '#1076 an interrupted publish must preserve the previous summary')


class SqliteHeaderExecutorTests(unittest.TestCase):
    """#979 P0: guard-exit failures must invalidate already-recorded kills."""

    def test_guard_refusal_keeps_exit_one(self):
        """#1076 fix-r1: a sqlite guard that refuses to open keeps rc=1."""
        @contextlib.contextmanager
        def guard(_root, _paths):
            raise RuntimeError('injected HEAD baseline refusal')
            yield  # pragma: no cover - keeps this a context manager

        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / 'evidence'
            with patch.object(sqlite_header, 'mutating_sources', new=guard), \
                    patch.object(sys, 'argv', ['negative_sqlite_header.py', str(out)]), \
                    patch.dict('os.environ', {
                        'PDRS_BASELINE': str(Path(directory)),
                        'CARGO_TARGET_DIR': str(Path(directory) / 'target')}), \
                    contextlib.redirect_stdout(io.StringIO()):
                code = sqlite_header.main()
            summary = out / 'summary.json'
            self.assertTrue(summary.is_file(),
                            '#1076 a refused sqlite guard must still account for every row')
            report = json.loads(summary.read_text())
        self.assertEqual(code, 1, '#1076 a sqlite HEAD guard refusal must keep exit code 1')
        self.assertEqual(report['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 2},
                         '#1076 a sqlite guard refusal must account for every selected row')

    def test_stale_anchor_refusal_keeps_exit_one(self):
        """#1076 fix-r1: an expired anchor refuses to run with rc=1, not 2."""
        @contextlib.contextmanager
        def guard(_root, _paths):
            # Empty snapshots are enough: the stale-anchor preflight fails
            # before any bytes are mutated, and this keeps the test independent
            # of where the fixture lives.
            yield {sqlite_header.GENERATOR: b'',
                   sqlite_header.STORAGE_OBSERVER: b''}

        rows = [(name, path, '#1076 no such anchor', after, test, assertion)
                for name, path, _before, after, test, assertion in sqlite_header.MUTANT_ROWS]
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / 'evidence'
            with patch.object(sqlite_header, 'MUTANT_ROWS', rows), \
                    patch.object(sqlite_header, 'mutating_sources', new=guard), \
                    patch.object(sys, 'argv', ['negative_sqlite_header.py', str(out)]), \
                    patch.dict('os.environ', {
                        'PDRS_BASELINE': str(Path(directory)),
                        'CARGO_TARGET_DIR': str(Path(directory) / 'target')}), \
                    contextlib.redirect_stdout(io.StringIO()):
                code = sqlite_header.main()
            report = json.loads((out / 'summary.json').read_text())
        self.assertEqual(code, 1, '#1076 a sqlite stale-anchor refusal must keep exit code 1')
        self.assertEqual(report['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 2},
                         '#1076 a sqlite stale-anchor refusal must void every row')
        self.assertIn('anchor expired', report['controls'][0]['reason'],
                      '#1076 the stale-anchor refusal reason must be recorded')

    def test_stale_anchor_with_guard_restore_failure_exits_two(self):
        """#1076 fix-r2 / Codex P1: a body refusal plus a failed guard exit is void."""
        @contextlib.contextmanager
        def guard(_root, _paths):
            try:
                yield {sqlite_header.GENERATOR: b'',
                       sqlite_header.STORAGE_OBSERVER: b''}
            finally:
                raise RuntimeError('restore verification failed')

        rows = [(name, path, '#1076 no such anchor', after, test, assertion)
                for name, path, _before, after, test, assertion in sqlite_header.MUTANT_ROWS]
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / 'evidence'
            with patch.object(sqlite_header, 'MUTANT_ROWS', rows), \
                    patch.object(sqlite_header, 'mutating_sources', new=guard), \
                    patch.object(sys, 'argv', ['negative_sqlite_header.py', str(out)]), \
                    patch.dict('os.environ', {
                        'PDRS_BASELINE': str(Path(directory)),
                        'CARGO_TARGET_DIR': str(Path(directory) / 'target')}), \
                    contextlib.redirect_stdout(io.StringIO()):
                code = sqlite_header.main()
            report = json.loads((out / 'summary.json').read_text())
        self.assertEqual(
            code, 2,
            '#1076 a sqlite stale anchor plus an unverified guard exit must return 2')
        self.assertEqual(
            report['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 2},
            '#1076 a sqlite stale anchor plus an unverified guard exit must void every row')
        self.assertIn('restore verification failed', report['controls'][0]['reason'],
                      '#1076 the sqlite guard restore failure must be recorded')

    def test_body_cancellation_keeps_exit_two(self):
        """#1076 fix-r2: a cancellation inside the body is void on a clean exit."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'generator.mts'
            source.write_text('original')
            out = root / 'evidence'
            saved = (sqlite_header.GENERATOR, sqlite_header.STORAGE_OBSERVER,
                     sqlite_header.MUTANT_ROWS)

            @contextlib.contextmanager
            def guard(_root, _paths):
                yield {source: b'original'}

            sqlite_header.GENERATOR = sqlite_header.STORAGE_OBSERVER = source
            sqlite_header.MUTANT_ROWS = [('one', source, 'original', 'mutant', TEST, ANCHOR)]
            try:
                with patch.object(sqlite_header, 'mutating_sources', new=guard), \
                        patch.object(sqlite_header, 'wait_for_slot',
                                     return_value=subprocess.CompletedProcess([], 0, '', '')), \
                        patch.object(sqlite_header, 'run_owned',
                                     side_effect=KeyboardInterrupt('SIGINT in body')), \
                        patch.object(sys, 'argv', ['negative_sqlite_header.py', str(out)]), \
                        patch.dict('os.environ', {
                            'PDRS_BASELINE': str(root),
                            'CARGO_TARGET_DIR': str(root / 'target')}), \
                        contextlib.redirect_stdout(io.StringIO()):
                    code = sqlite_header.main()
            finally:
                (sqlite_header.GENERATOR, sqlite_header.STORAGE_OBSERVER,
                 sqlite_header.MUTANT_ROWS) = saved
            report = json.loads((out / 'summary.json').read_text())
            restored = source.read_bytes()
        self.assertEqual(code, 2, '#1076 a sqlite body cancellation must return 2')
        self.assertEqual(report['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                         '#1076 a sqlite body cancellation must publish an all-INVALID summary')
        self.assertIn('KeyboardInterrupt', report['controls'][0]['reason'],
                      '#1076 the sqlite body cancellation must be recorded as INVALID')
        self.assertEqual(restored, b'original',
                         '#1076 the sqlite body cancellation must restore its bytes')

    def test_row_restore_failure_returns_two(self):
        """#1076 fix-r2: an unverified per-row restore must be rc=2."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'generator.mts'
            source.write_text('original')
            out = root / 'evidence'
            saved = (sqlite_header.GENERATOR, sqlite_header.STORAGE_OBSERVER,
                     sqlite_header.MUTANT_ROWS)
            real_write_bytes = Path.write_bytes

            def reject_restore(path, data):
                if path == source and data == b'original':
                    raise OSError('#1076 injected row restore failure')
                return real_write_bytes(path, data)

            @contextlib.contextmanager
            def guard(_root, _paths):
                yield {source: b'original'}

            sqlite_header.GENERATOR = sqlite_header.STORAGE_OBSERVER = source
            sqlite_header.MUTANT_ROWS = [('one', source, 'original', 'mutant', TEST, ANCHOR)]
            try:
                with patch.object(sqlite_header, 'mutating_sources', new=guard), \
                        patch.object(sqlite_header, 'wait_for_slot',
                                     return_value=subprocess.CompletedProcess([], 0, '', '')), \
                        patch.object(sqlite_header, 'run_owned',
                                     return_value=output('pass')), \
                        patch.object(Path, 'write_bytes', new=reject_restore), \
                        patch.object(sys, 'argv', ['negative_sqlite_header.py', str(out)]), \
                        patch.dict('os.environ', {
                            'PDRS_BASELINE': str(root),
                            'CARGO_TARGET_DIR': str(root / 'target')}), \
                        contextlib.redirect_stdout(io.StringIO()):
                    code = sqlite_header.main()
            finally:
                (sqlite_header.GENERATOR, sqlite_header.STORAGE_OBSERVER,
                 sqlite_header.MUTANT_ROWS) = saved
            report = json.loads((out / 'summary.json').read_text())
        self.assertEqual(code, 2, '#1076 an unverified sqlite row restore must return 2')
        self.assertEqual(report['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                         '#1076 an unverified sqlite row restore must void every row')
        self.assertIn('#1076 injected row restore failure', report['controls'][0]['reason'],
                      '#1076 the sqlite row restore failure must be recorded')

    def test_guard_restore_failure_invalidates_all_completed_kills(self):
        generator = sqlite_header.GENERATOR
        observer = sqlite_header.STORAGE_OBSERVER
        originals = {generator: generator.read_bytes(), observer: observer.read_bytes()}
        for name, path, before, _after, _test, _assertion in sqlite_header.MUTANT_ROWS:
            self.assertEqual(originals[path].decode().count(before), 1,
                             f'#979 {name} anchor must be unique before the simulation')

        @contextlib.contextmanager
        def guard(_root, _paths):
            try:
                yield originals
            finally:
                raise RuntimeError('injected source guard restore failure')

        outcomes = []
        anchors = {test: assertion for _name, _path, _before, _after, test, assertion
                   in sqlite_header.MUTANT_ROWS}
        snapshots = []
        write_summary = sqlite_header.write_summary

        def capture(output_dir, rows):
            snapshots.append([row['status'] for row in rows])
            return write_summary(output_dir, rows)

        def cargo(command, **_kwargs):
            test = command[-1]
            outcomes.append(test)
            if len(outcomes) in (2, 5):
                return subprocess.CompletedProcess(
                    command, 101,
                    f'test {test} ... FAILED\ntest result: FAILED\n',
                    # #818 fix-r5: the shared judge now reads the named test's
                    # own panic block, so the fixture must emit libtest's real
                    # `thread '<full name>' panicked at <file>:<line>:<col>:` header.
                    f"thread '{test}' panicked at fixture.rs:1:1:\n{anchors[test]}\n")
            return subprocess.CompletedProcess(
                command, 0,
                f'test {test} ... ok\ntest result: ok. 1 passed; 0 failed;\n', '')

        write_bytes = Path.write_bytes

        def keep_tree(path, data):
            # Never let the simulation write the real fixtures: the snapshots
            # already equal their committed bytes.
            if path in originals:
                self.assertEqual(path.read_bytes(), originals[path],
                                 '#979 simulated mutation must not touch the tree')
                return len(data)
            return write_bytes(path, data)

        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / 'evidence'
            with patch.object(sqlite_header, 'mutating_sources', new=guard), \
                    patch.object(sqlite_header, 'wait_for_slot',
                                 return_value=subprocess.CompletedProcess([], 0, '', '')), \
                    patch.object(sqlite_header, 'run_owned', side_effect=cargo), \
                    patch.object(sqlite_header, 'write_summary', new=capture), \
                    patch.object(Path, 'write_bytes', new=keep_tree), \
                    patch.object(sys, 'argv', ['negative_sqlite_header.py', str(out)]), \
                    patch.dict('os.environ', {
                        'PDRS_BASELINE': str(Path(directory)),
                        'CARGO_TARGET_DIR': str(Path(directory) / 'target')}), \
                    contextlib.redirect_stdout(io.StringIO()):
                code = sqlite_header.main()
            summary = out / 'summary.json'
            self.assertTrue(summary.is_file(), '#979 guard-failure run must write summary.json')
            try:
                report = json.loads(summary.read_text())
            except (OSError, ValueError) as error:
                self.fail(f'#979 summary.json must be readable JSON: {type(error).__name__}: {error}')
        self.assertEqual(len(outcomes), 6,
                         '#1076 both mutants must finish positive/negative/restored before the guard exits')
        self.assertEqual(report['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 2},
                         '#1076 a failed guard exit must invalidate every completed kill')
        self.assertEqual([row['status'] for row in report['controls']],
                         ['INVALID', 'INVALID'],
                         '#1076 no completed row may stay KILLED after a guard failure')
        self.assertEqual(snapshots, [['INVALID', 'INVALID']],
                         '#1076 the summary must be published once, only after the guard exit')
        self.assertNotIn(['KILLED', 'KILLED'], snapshots,
                         '#1076 no KILLED summary may be readable while the guard is unverified')
        for row in report['controls']:
            self.assertIn('injected source guard restore failure', row['reason'],
                          '#1076 the guard exception must be attached to every row')
        self.assertEqual(code, 2, '#1076 a failed guard exit must return exit code 2')


class SqliteHeaderCancellationTests(unittest.TestCase):
    """#1076: sqlite-header cancellation and atomic publication."""

    def drive_sqlite(self, canceled):
        with tempfile.TemporaryDirectory(prefix='pdrs-1076-sqlite-') as directory:
            root = Path(directory)
            source = root / 'generator.mts'
            source.write_text('original')
            out = root / 'evidence'
            summary_present = []
            before_invalidate = []
            original_invalidate = sqlite_header.invalidate_completed
            saved = (sqlite_header.GENERATOR, sqlite_header.STORAGE_OBSERVER,
                     sqlite_header.MUTANT_ROWS)

            def inject():
                if canceled == 'sigint':
                    signal.raise_signal(signal.SIGINT)
                raise SystemExit(256)

            @contextlib.contextmanager
            def guard(_root, _paths, **_kwargs):
                try:
                    yield {source: b'original'}
                finally:
                    summary_present.append((out / 'summary.json').exists())
                    inject()

            def capture(rows, reason):
                before_invalidate.append([row['status'] for row in rows])
                return original_invalidate(rows, reason)

            def cargo(command, **_kwargs):
                self.assertIn(command[-1], (TEST,),
                              '#1076 the sqlite cancellation probe must run the bound test')
                if source.read_text() == 'mutant':
                    return output('kill')
                return output('pass')

            sqlite_header.GENERATOR = source
            sqlite_header.STORAGE_OBSERVER = source
            sqlite_header.MUTANT_ROWS = [('one', source, 'original', 'mutant', TEST, ANCHOR)]
            previous = signal.signal(signal.SIGINT, signal.default_int_handler)
            try:
                with patch.object(sqlite_header, 'mutating_sources', new=guard), \
                        patch.object(sqlite_header, 'invalidate_completed', new=capture), \
                        patch.object(sqlite_header, 'wait_for_slot',
                                     return_value=subprocess.CompletedProcess([], 0, '', '')), \
                        patch.object(sqlite_header, 'run_owned', side_effect=cargo), \
                        patch.object(sys, 'argv', ['negative_sqlite_header.py', str(out)]), \
                        patch.dict('os.environ', {
                            'PDRS_BASELINE': str(root),
                            'CARGO_TARGET_DIR': str(root / 'target')}), \
                        contextlib.redirect_stdout(io.StringIO()):
                    try:
                        code = sqlite_header.main()
                    except BaseException as error:
                        code = error
            finally:
                signal.signal(signal.SIGINT, previous)
                (sqlite_header.GENERATOR, sqlite_header.STORAGE_OBSERVER,
                 sqlite_header.MUTANT_ROWS) = saved
            path = out / 'summary.json'
            summary = json.loads(path.read_text()) if path.is_file() else None
            return (code, summary, summary_present, before_invalidate,
                    source.read_text())

    def test_sqlite_guard_sigint_voids_last_kill_and_exits_2(self):
        code, summary, present, before, restored = self.drive_sqlite('sigint')
        self.assertEqual(code, 2, '#1076 a sqlite SIGINT in the guard exit must return 2')
        self.assertEqual(present, [False],
                         '#1076 no summary may exist while the sqlite guard is unverified')
        self.assertEqual(before, [['KILLED']],
                         '#1076 the last sqlite row must be KILLED before the guard exit')
        self.assertEqual(restored, 'original',
                         '#1076 the sqlite cancellation fixture must restore its bytes')
        self.assertIsNotNone(summary, '#1076 the failed sqlite guard exit must publish summary.json')
        self.assertEqual([row['status'] for row in summary['controls']], ['INVALID'],
                         '#1076 a sqlite SIGINT must not leave the final row KILLED')
        self.assertIn('source guard: KeyboardInterrupt', summary['controls'][0]['reason'],
                      '#1076 the sqlite SIGINT invalidation reason must be recorded')
        self.assertEqual(summary['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                         '#1076 the sqlite counts must be INVALID after SIGINT')

    def test_sqlite_guard_systemexit_256_voids_last_kill_and_exits_2(self):
        code, summary, present, before, restored = self.drive_sqlite('systemexit')
        self.assertIsInstance(code, SystemExit,
                              '#1076 sqlite SystemExit(256) must be re-raised')
        self.assertEqual(code.code, 2,
                         '#1076 sqlite SystemExit(256) must normalize to exit code 2')
        self.assertEqual(present, [False],
                         '#1076 no summary may exist while the sqlite guard is unverified')
        self.assertEqual(before, [['KILLED']],
                         '#1076 the last sqlite row must be KILLED before the guard exit')
        self.assertEqual(restored, 'original',
                         '#1076 the sqlite cancellation fixture must restore its bytes')
        self.assertIsNotNone(summary, '#1076 the failed sqlite guard exit must publish summary.json')
        self.assertEqual([row['status'] for row in summary['controls']], ['INVALID'],
                         '#1076 sqlite SystemExit(256) must not leave the final row KILLED')
        self.assertIn('source guard: SystemExit', summary['controls'][0]['reason'],
                      '#1076 the sqlite SystemExit invalidation reason must be recorded')
        self.assertEqual(summary['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                         '#1076 the sqlite counts must be INVALID after SystemExit')

    def test_write_summary_publishes_atomically(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1076-atomic-') as directory:
            out = Path(directory)
            target = out / 'summary.json'
            target.write_text('previous\n')
            row = {'mutation': 'one', 'test': TEST, 'status': 'INVALID',
                   'reason': '#1076 forced INVALID publish'}
            real_write = Path.write_text
            real_replace = Path.replace

            def interrupt_write(path, text, *args, **kwargs):
                if path.name == 'summary.json':
                    # The non-atomic mutant would overwrite the live summary
                    # with this prefix before the cancellation lands.
                    real_write(path, text[:5], *args, **kwargs)
                    signal.raise_signal(signal.SIGINT)
                return real_write(path, text, *args, **kwargs)

            def interrupt_replace(path, destination, *args, **kwargs):
                if Path(destination).name == 'summary.json':
                    signal.raise_signal(signal.SIGINT)
                return real_replace(path, destination, *args, **kwargs)

            previous = signal.signal(signal.SIGINT, signal.default_int_handler)
            try:
                with patch.object(Path, 'write_text', new=interrupt_write), \
                        patch.object(Path, 'replace', new=interrupt_replace), \
                        contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaises(
                            KeyboardInterrupt,
                            msg='#1076 the atomic publish interrupt must be delivered'):
                        sqlite_header.write_summary(out, [row])
            finally:
                signal.signal(signal.SIGINT, previous)
            self.assertEqual(target.read_bytes(), b'previous\n',
                             '#1076 an interrupted publish must preserve the previous summary')


class AnchorPreflightTests(unittest.TestCase):
    def test_scope_drift_and_duplicate_fail_without_skipping_successor(self):
        import sys
        sys.path.insert(0, str(Path(__file__).resolve().parent / 'turn-runner'))
        from _negative_common import check_mutations
        # Mirror the #741k failure: a clone changes one anchor and a new
        # select duplicates another. Neither may be accepted or hide later rows.
        original = ("    fn factory() { recorder.clone(); biased; biased; }\n"
                    "    fn client() { recorder.clone(); }\n")
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / 'fixture.rs'
            source.write_text(original)
            rows = [
                ('stale', source, 'factory', [('recorder;', 'bad;')]),
                ('duplicate', source, 'factory', [('biased;', 'bad;')]),
                ('noop', source, 'factory', [('recorder.clone();', 'recorder.clone();')]),
                ('successor', source, 'factory', [('recorder.clone();', 'bad;')]),
            ]
            with contextlib.redirect_stdout(io.StringIO()) as output:
                code = check_mutations(rows)
            summary = json.loads(output.getvalue())
            self.assertEqual(code, 1, '#741k drift must fail preflight')
            self.assertEqual([row['status'] for row in summary['controls']],
                             ['INVALID', 'INVALID', 'INVALID', 'CHECKED'],
                             '#741k invalid anchors must not skip the successor')
            self.assertEqual(source.read_text(), original, '#741k preflight is read-only')
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(check_mutations(rows[-1:]), 0,
                                 '#741k function-scoped unique anchor must pass')
                self.assertEqual(check_mutations([]), 1, '#741k empty preflight must fail')

class ReceiptTests(unittest.TestCase):
    PREFIX = 'metadata::from_env_tests::'
    CELL = 'relative_deleted_cwd'
    ASSERTION = 'metadata from_env matrix: relative-deleted'

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.out = Path(self.temp.name)
        self.targets = [(self.CELL, self.ASSERTION)]
        self.key = (self.PREFIX + self.CELL, self.ASSERTION)
        self.run = suite.ReceiptRun(self.out, 'current', [self.key])
        self.result = subprocess.CompletedProcess([], 101,
            f'test {self.key[0]} ... FAILED\n\nfailures:\n    {self.key[0]}\n\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored;\n', '')

    def judge(self, result=None, targets=None, **kwargs):
        return suite.judge(result or self.result, self.PREFIX,
                           self.targets if targets is None else targets, self.run, **kwargs)

    def receipt(self, value=None):
        self.run.paths[self.key].write_text(self.run.nonce if value is None else value)

    def test_named_failure_and_exact_nonce_are_both_required(self):
        self.assertFalse(self.judge()[0])
        self.receipt()
        self.assertTrue(self.judge()[0])
        for code, stdout in ((0, self.result.stdout), (1, self.result.stdout),
                             (-9, self.result.stdout), (101, ''),
                             (101, self.result.stdout.replace(self.key[0], 'other'))):
            self.assertFalse(self.judge(subprocess.CompletedProcess([], code, stdout, ''))[0])
        # stderr and diagnostic substrings cannot substitute for the stdout line.
        self.assertFalse(self.judge(subprocess.CompletedProcess([], 101, '', self.result.stdout))[0])
        self.assertFalse(self.judge(subprocess.CompletedProcess([], 101,
                         'diagnostic ' + self.result.stdout, ''))[0])

    def test_fresh_run_paths_and_nonce_reject_previous_run(self):
        previous = suite.ReceiptRun(self.out, 'previous', [self.key])
        previous.paths[self.key].write_text(previous.nonce)
        self.assertNotEqual(self.run.nonce, previous.nonce)
        self.assertNotEqual(self.run.paths[self.key], previous.paths[self.key])
        self.assertFalse(self.judge()[0])
        self.receipt(previous.nonce)
        self.assertFalse(self.judge()[0])
        with self.assertRaises(FileExistsError):
            suite.ReceiptRun(self.out, 'current', [self.key])

    def test_corrupt_receipts_are_not_kills(self):
        for value in ('', 'old', self.run.nonce[:-1], self.run.nonce + '\n',
                      self.run.nonce + 'x' * 100000):
            self.receipt(value)
            self.assertFalse(self.judge()[0], repr(value[:66]))
        self.run.paths[self.key].write_bytes(b'\xff' * 64)
        self.assertFalse(self.judge()[0])

    def test_nonregular_and_unreadable_receipts_are_not_kills(self):
        import os
        path = self.run.paths[self.key]
        path.mkdir()
        self.assertFalse(self.judge()[0])
        path.rmdir()
        other = self.out / 'other'
        other.write_text(self.run.nonce)
        path.symlink_to(other)
        self.assertFalse(self.judge()[0])
        path.unlink()
        os.mkfifo(path)
        self.assertFalse(self.judge()[0], 'FIFO must not block')
        path.unlink()
        with patch('os.open', side_effect=PermissionError('denied')):
            self.assertFalse(self.judge()[0])

    def test_test_and_assertion_identity_are_both_required(self):
        self.receipt()
        for targets in ([], [(self.CELL, 'other assertion')],
                        [('other_cell', self.ASSERTION)], self.targets * 2,
                        self.targets + [(self.CELL, 'other assertion')]):
            self.assertFalse(self.judge(targets=targets)[0])
            self.assertFalse(self.judge(targets=targets, any_cell=True)[0])

    def test_all_and_explicit_any_cell(self):
        second = ('sibling', 'sibling assertion')
        targets = self.targets + [second]
        self.receipt()
        self.assertFalse(self.judge(targets=targets)[0])
        self.assertTrue(self.judge(targets=targets, any_cell=True)[0])
        sibling_failed = subprocess.CompletedProcess([], 101, self.result.stdout +
            f'test {self.PREFIX}sibling ... FAILED\n', '')
        self.assertFalse(self.judge(sibling_failed, targets=targets, any_cell=True)[0],
                         'any_cell never accepts an unrelated declared failure')
        self.run.paths[self.key].unlink()
        self.assertFalse(self.judge(targets=targets, any_cell=True)[0])

    def test_empty_and_duplicate_receipt_targets_are_refused(self):
        for index, targets in enumerate(([], [self.key, self.key],
                                         [self.key, (self.key[0], 'other assertion')])):
            with self.assertRaises(ValueError):
                suite.ReceiptRun(self.out, f'bad-{index}', targets)

    def test_environment_clears_spoofed_parent_and_overrides_old_config(self):
        env = self.run.environment({self.run.ENV: 'old', self.run.PARENT_ENV: 'sibling', 'HOME': 'kept'})
        self.assertNotIn(self.run.PARENT_ENV, env)
        self.assertEqual(json.loads(env[self.run.ENV]), self.run.config)
        self.assertEqual(env['HOME'], 'kept')

    def test_issue889_r2_r3_r4_replays_never_kill_without_current_receipt(self):
        # Rewritten from review-r{2,3,4}/codex.log, including both echo toggles.
        # Real failure remains setup unwrap; only the diagnostic prose changes.
        worker = self.PREFIX + 'environment_worker'
        setup = 'called Result::unwrap() on an Err value: PermissionDenied'
        old_panic = (f"thread '{worker}' panicked at regression.rs:64:5:\n"
                     f'assertion `left == right` failed: {self.ASSERTION}\n')
        summary = 'test result: FAILED. 0 passed; 1 failed; 0 ignored;\n\n'
        setup_panic = f"thread '{worker}' panicked at regression.rs:64:5:\n{setup}\n"
        parent = (f"thread '{self.key[0]}' panicked at test_process.rs:32:37:\n"
                  f'{worker}/relative-deleted: expected one passed test and receipt relative-deleted: '
                  'exit status: 101\nrunning 1 test\n')
        diagnostics = {
            'r2-no-echo': setup_panic,
            'r2-ordinary-marker': f'diagnostic: checking {self.ASSERTION}\n' + setup_panic,
            'r3-no-echo': setup_panic,
            'r3-old-stdout-panic': old_panic + summary + setup_panic,
            'r4-conflicting': setup_panic + old_panic,
            'r4-old-summary-after-real': setup_panic + summary + old_panic,
            'r4-old-summary-before-real': summary + old_panic + setup_panic,
        }
        for name, diagnostic in diagnostics.items():
            with self.subTest(name=name):
                result = subprocess.CompletedProcess([], 101, self.result.stdout, parent + diagnostic)
                self.assertFalse(self.judge(result)[0])
                # Echoing even this run's public nonce in either stream is not a file receipt.
                result.stderr += self.run.nonce
                self.assertFalse(self.judge(result)[0])
        # Positive pair: arbitrary prose cannot veto authentic assertion evidence.
        self.receipt()
        self.assertTrue(self.judge()[0])


class MatrixRunnerTests(unittest.TestCase):
    def exercise(self, fault=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'source.rs'
            source.write_text('original')
            rows = [('first', source, 'original', 'first', [('bound', 'assertion')]),
                    ('second', source, 'original', 'second', [('bound', 'assertion')])]
            if fault == 'anchor':
                rows[0] = ('first', source, 'absent', 'first', [('bound', 'assertion')])
            phases = []
            original = b'original'
            guards = []
            restores = []
            write_bytes = Path.write_bytes
            def restore(path, data):
                if fault == 'restore' and path == source and path.read_text() == 'first' and not restores:
                    restores.append(True)
                    raise OSError('injected matrix restore failure')
                return write_bytes(path, data)
            @contextlib.contextmanager
            def guard(*args, **kwargs):
                self.assertNotIn('baseline', kwargs, 'must use HEAD')
                guards.append(True)
                if fault == 'guard-enter' and len(guards) == 1:
                    raise RuntimeError('injected guard entry refusal')
                self.assertEqual(source.read_bytes(), original, 'guard rechecks restored source')
                try:
                    yield {source: original}
                finally:
                    source.write_bytes(original)
                if fault == 'guard-exit' and len(guards) == 1:
                    raise RuntimeError('injected guard exit refusal')
            def run(command, **kwargs):
                current = source.read_text()
                compile = '--no-run' in command
                phases.append((current, compile))
                if compile:
                    return subprocess.CompletedProcess([], int(fault == 'compile' and current == 'first'), '', '')
                if current == 'first' and fault == 'timeout':
                    raise subprocess.TimeoutExpired(command, 900)
                if current == 'original' or (current == 'first' and fault == 'survive'):
                    return subprocess.CompletedProcess([], 0,
                        'test result: ok. 1 passed; 0 failed;\nPDRS_VERIFIED_CHILD bound\n', '')
                config = json.loads(kwargs['env'][suite.ReceiptRun.ENV])
                if current != 'first' or fault != 'missing-receipt':
                    Path(config['targets'][0]['path']).write_text(config['nonce'])
                return subprocess.CompletedProcess([], 101, 'test prefix::bound ... FAILED\n', '')
            with patch.object(suite, 'mutating_sources', side_effect=guard), \
                    patch.object(suite, 'run_owned', side_effect=run), \
                    patch.object(Path, 'write_bytes', new=restore), \
                    patch.object(suite, 'wait_for_slot', return_value=output('pass')), \
                    patch.dict('os.environ', {'CARGO_TARGET_DIR': str(root / 'target')}), \
                    contextlib.redirect_stdout(io.StringIO()):
                code = suite.matrix_main(root, root / 'runner.py', 'prefix::', rows,
                    passed_count=1, child_count=1, paths=[source], args=[str(root / 'out')])
            self.assertEqual(source.read_bytes(), original)
            report = json.loads((root / 'out/summary.json').read_text())
            self.assertEqual(report['controls'][1]['status'], 'KILLED', 'must run successor')
            self.assertEqual(phases[-3:], [('second', True), ('second', False), ('original', False)])
            return code, report, phases

    def test_each_mutant_compiles_and_restores(self):
        code, report, phases = self.exercise()
        self.assertEqual(code, 0)
        self.assertEqual(report['counts']['KILLED'], 2)
        self.assertEqual(phases[:4], [('original', False), ('first', True), ('first', False), ('original', False)])

    def test_faults_continue_and_never_kill(self):
        for fault in ('compile', 'timeout', 'survive', 'missing-receipt',
                      'restore', 'guard-enter', 'guard-exit', 'anchor'):
            with self.subTest(fault=fault):
                code, report, phases = self.exercise(fault)
                self.assertEqual(code, 1)
                self.assertEqual(report['controls'][0]['status'], 'SURVIVED' if fault == 'survive' else 'INVALID')
                if fault == 'compile':
                    self.assertNotIn(('first', False), phases, 'uncompiled mutant must not execute')


if __name__ == '__main__':
    unittest.main(verbosity=2)

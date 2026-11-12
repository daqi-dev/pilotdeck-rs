# anchor-selfcheck: unsupported unit tests of the shared mutation suite runner
"""Harness fault injection only; no cargo or product source mutation here."""
import contextlib
import errno
import io
import json
from pathlib import Path
import re
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
            publish_source = suite.write_source_atomic

            def publish(root, path, data, *, mode=None):
                # #1087 fix-r2: run_cases publishes every guarded-source write
                # through write_source_atomic (one rename, #1903), so the
                # fault injection wraps that entry point — the byte level under
                # it never runs a truncating write anymore.
                if restore_failure and path == source and path.read_text() == 'first':
                    raise OSError('injected restoration failure')
                return publish_source(root, path, data, mode=mode)

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
                    patch.object(suite, 'write_source_atomic', new=publish), \
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


class EvidenceLabelTests(unittest.TestCase):
    """#1365 fix-r3 P1: a row that binds several tests keeps one log per Case."""

    FIRST = 'runtime::owner::tests::alpha_drain'
    SECOND = 'runtime::owner::tests::beta_drain'
    SOLO = 'runtime::owner::tests::solo_drain'

    def result_for(self, test, kind):
        if kind == 'pass':
            return subprocess.CompletedProcess(
                [], 0, f'test {test} ... ok\ntest result: ok. 1 passed; 0 failed;\n', '')
        return subprocess.CompletedProcess(
            [], 101, f'test {test} ... FAILED\ntest result: FAILED\n',
            f"thread '{test}' panicked at fixture.rs:1:1:\n{ANCHOR}\n")

    def drive(self, specs):
        """Run ``(mutation, test, marker)`` specs through the shared runner."""
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        source = root / 'fixture.rs'
        source.write_text('original')
        out = root / 'logs'
        out.mkdir()
        cases = [suite.Case(mutation, source, 'original', 'mutant', test, marker)
                 for mutation, test, marker in specs]

        def run(command, **_kwargs):
            self.assertEqual(command[-3:], ['--', '--exact', '--include-ignored'],
                             '#1365 the evidence probe must run the exact bound test')
            return self.result_for(command[-4],
                                   'kill' if source.read_text() == 'mutant' else 'pass')

        def mutate(text, _scope, replacements):
            old, new = replacements[0]
            self.assertEqual(text.count(old), 1, '#1365 stale label fixture anchor')
            return text.replace(old, new, 1)

        with patch.object(suite, 'mutating_sources') as sources, \
                patch.object(suite, 'wait_for_slot',
                             return_value=self.result_for(self.FIRST, 'pass')), \
                patch.object(suite, 'run_owned', side_effect=run), \
                contextlib.redirect_stdout(io.StringIO()) as stdout:
            sources.return_value.__enter__.return_value = {source: b'original'}
            sources.return_value.__exit__.return_value = False
            code = suite.run_cases(root, cases, out,
                                   {'CARGO_TARGET_DIR': str(root / 'target')}, mutate)
        self.assertEqual(source.read_text(), 'original', '#1365 evidence probe restores bytes')
        return code, out, stdout.getvalue()

    def test_multi_test_row_keeps_per_case_logs_and_labels(self):
        code, out, text = self.drive([
            ('row', self.FIRST, ANCHOR),
            ('row', self.SECOND, ANCHOR),
            ('solo', self.SOLO, ANCHOR),
        ])
        self.assertEqual(code, 0, '#1365 every bound Case must still be killed')
        names = sorted(path.name for path in out.iterdir())
        for test, other in ((self.FIRST, self.SECOND), (self.SECOND, self.FIRST)):
            short = suite.short_test(test)
            for stage in ('positive', 'negative', 'restored'):
                path = out / f'row-{short}-{stage}.log'
                self.assertTrue(path.is_file(),
                                f'#1365 {path.name} must exist; got {names}')
                self.assertIn(f'test {test} ', path.read_text(),
                              f'#1365 {path.name} must hold its own bound test output')
                self.assertNotIn(other, path.read_text(),
                                 f'#1365 {path.name} must not hold the sibling Case output')
        self.assertIn('solo-positive.log', names,
                      '#1365 single-test rows keep their historical log names')
        for label in (f'row-{suite.short_test(self.FIRST)}',
                      f'row-{suite.short_test(self.SECOND)}', 'solo'):
            self.assertIn(f'{label}: KILLED', text,
                          '#1365 every Case must be traceable to its test on the console')
        summary = json.loads((out / 'summary.json').read_text())
        self.assertEqual([row['mutation'] for row in summary['controls']],
                         ['row', 'row', 'solo'],
                         '#1365 the summary keeps the real mutation name, not the log stem')
        self.assertEqual([row['test'] for row in summary['controls']],
                         [self.FIRST, self.SECOND, self.SOLO])

    def test_same_test_bound_twice_gets_ordinal_labels(self):
        code, out, text = self.drive([
            ('row', self.FIRST, ANCHOR),
            ('row', self.FIRST, ANCHOR),
        ])
        self.assertEqual(code, 0)
        names = sorted(path.name for path in out.iterdir())
        self.assertIn('row-runtime.owner.tests.alpha_drain-positive.log', names,
                      '#1365 a repeated Case falls back to the whole test identity')
        self.assertIn('row-runtime.owner.tests.alpha_drain-2-positive.log', names,
                      '#1365 a repeated label gains an ordinal instead of overwriting')
        self.assertIn('row-runtime.owner.tests.alpha_drain: KILLED', text,
                      '#1365 the console label must match the disambiguated log stem')

    def test_cross_group_stem_collision_keeps_single_row_historical_name(self):
        """#1413 fix-r4: a derived multi-Case stem must not take a row's name."""
        colliding = f'row-{suite.short_test(self.FIRST)}'
        code, out, text = self.drive([
            ('row', self.FIRST, ANCHOR),
            ('row', self.SECOND, ANCHOR),
            (colliding, self.SOLO, ANCHOR),
        ])
        self.assertEqual(code, 0, '#1413 every bound Case must still be killed')
        names = sorted(path.name for path in out.iterdir())
        # The single-Case row keeps its historical bare stem and its evidence.
        for stage in ('positive', 'negative', 'restored'):
            path = out / f'{colliding}-{stage}.log'
            self.assertIn(path.name, names,
                          f'#1413 {path.name} must keep its historical name; got {names}')
            self.assertIn(f'test {self.SOLO} ', path.read_text(),
                          '#1413 the bare stem must hold the single-Case row output')
        # The multi-Case row falls back to its whole test identity.
        fallback = f'row-{self.FIRST.replace("::", ".")}'
        self.assertIn(f'{fallback}-positive.log', names,
                      '#1413 the colliding derived stem falls back to the full identity')
        self.assertIn(f'{fallback}: KILLED', text,
                      '#1413 the console label must match the disambiguated stem')
        self.assertIn(f'{colliding}: KILLED', text)
        summary = json.loads((out / 'summary.json').read_text())
        self.assertEqual([row['mutation'] for row in summary['controls']],
                         ['row', 'row', colliding],
                         '#1413 the summary keeps the real mutation names')


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

    def test_exact_requires_every_declared_target_to_run(self):
        # #1579 P1: a row whose second declared cell never reached libtest must
        # not inherit the first cell's kill. This is the judge half of the
        # control; the runner half is
        # MatrixRunnerTests.test_exact_multi_target_runs_each_cell.
        second = ('sibling', 'sibling assertion')
        targets = self.targets + [second]
        self.receipt()
        self.assertFalse(
            self.judge(targets=targets, any_cell=True, require_all_ran=True)[0],
            'a declared cell without a named result was never run')
        ran = subprocess.CompletedProcess([], 101, self.result.stdout +
            f'test {self.PREFIX}sibling ... ok\n', '')
        killed, _reason, hits = self.judge(
            ran, targets=targets, any_cell=True, require_all_ran=True)
        self.assertTrue(killed)
        self.assertEqual(hits, [self.CELL])
        cells = suite.cell_states(ran, self.PREFIX, targets, self.run)
        self.assertEqual({cell['test']: cell['status'] for cell in cells},
                         {self.CELL: 'KILLED', 'sibling': 'OK'})

    def test_judge_keeps_its_three_value_contract(self):
        # Sixteen fixtures unpack `killed, reason, hits = judge(...)`; per-cell
        # evidence travels through cell_states() so that never breaks (#1579).
        self.receipt()
        judged = self.judge()
        self.assertEqual(len(judged), 3, judged)
        self.assertEqual(suite.cell_states(self.result, self.PREFIX,
                                           self.targets, self.run),
                         [{'test': self.CELL, 'status': 'KILLED'}])

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
            publish_source = suite.write_source_atomic
            def publish(root, path, data, *, mode=None):
                # Same #1087 fix-r2 re-anchoring as SuiteTests: matrix_main
                # publishes through write_source_atomic, so the injected
                # restore failure must come from that entry point.
                if fault == 'restore' and path == source and path.read_text() == 'first' and not restores:
                    restores.append(True)
                    raise OSError('injected matrix restore failure')
                return publish_source(root, path, data, mode=mode)
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
                    patch.object(suite, 'write_source_atomic', new=publish), \
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

    def test_exact_multi_target_runs_each_cell(self):
        # #1579 P1 negative control: if the runner reverts to a single filter
        # built from targets[0], the second declared cell never executes; the
        # filter assertion below goes red before the verdict is even reached.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'source.rs'
            source.write_text('original')
            targets = [('cell_one', 'assertion one'), ('cell_two', 'assertion two')]
            rows = [('multi', source, 'original', 'mutated', targets)]
            commands = []
            @contextlib.contextmanager
            def guard(*args, **kwargs):
                yield {source: b'original'}
            def run(command, **kwargs):
                commands.append(list(command))
                if '--no-run' in command:
                    return subprocess.CompletedProcess([], 0, '', '')
                filters = [arg for arg in command if arg.startswith('prefix::')]
                env = kwargs['env']
                if suite.ReceiptRun.ENV in env:
                    config = json.loads(env[suite.ReceiptRun.ENV])
                    for test in filters:
                        entry = next(target for target in config['targets']
                                     if target['test'] == test)
                        Path(entry['path']).write_text(config['nonce'])
                    return subprocess.CompletedProcess(
                        [], 101, ''.join(f'test {test} ... FAILED\n' for test in filters)
                        + f'test result: FAILED. 0 passed; {len(filters)} failed;\n', '')
                return subprocess.CompletedProcess(
                    [], 0, ''.join(f'test {test} ... ok\n' for test in filters)
                    + f'test result: ok. {len(filters)} passed; 0 failed;\n', '')
            with patch.object(suite, 'mutating_sources', side_effect=guard), \
                    patch.object(suite, 'run_owned', side_effect=run), \
                    patch.object(suite, 'wait_for_slot', return_value=output('pass')), \
                    patch.dict('os.environ', {'CARGO_TARGET_DIR': str(root / 'target')}), \
                    contextlib.redirect_stdout(io.StringIO()):
                code = suite.matrix_main(root, root / 'runner.py', 'prefix::', rows,
                    passed_count=1, child_count=0, paths=[source], exact=True,
                    args=[str(root / 'out')])
            filters = [[arg for arg in command if arg.startswith('prefix::')]
                       for command in commands if '--no-run' not in command]
            for command_filters in filters:
                self.assertEqual(command_filters,
                                 ['prefix::cell_one', 'prefix::cell_two'],
                                 'every declared target must reach libtest')
            self.assertEqual(len(filters), 3, 'positive, negative and restored')
            report = json.loads((root / 'out/summary.json').read_text())
            self.assertEqual(code, 0)
            row = report['controls'][0]
            self.assertEqual(row['status'], 'KILLED')
            # Both declared cells are recorded as hits — the registered column
            # that carries per-cell evidence (#1579 P1). A `cells` column here
            # made `_negkit/compare_verdicts.py` reject the whole summary.
            self.assertEqual(row['hits'], ['cell_one', 'cell_two'])
            self.assertEqual(set(row), set(suite.RESULT_ROW_FIELDS))


class PublishJsonTests(unittest.TestCase):
    def test_publish_is_atomic_and_refuses_to_overwrite_evidence(self):
        # #1579 r2 P2: `summary.json` is a cross-process contract; publishing
        # it must never expose a half-written document nor clobber a previous
        # run's evidence (the OUT directory is already exclusive, this keeps
        # the old `open('x')` refusal alongside the temp-and-rename).
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            target = out / 'summary.json'
            payload = {'controls': [], 'counts': {'KILLED': 0}}
            suite._publish_json(target, payload)
            self.assertEqual(json.loads(target.read_text()), payload)
            self.assertEqual(sorted(p.name for p in out.iterdir()), ['summary.json'],
                             'the hidden temp file must not linger')
            with self.assertRaises(FileExistsError):
                suite._publish_json(target, {'controls': ['rewritten']})
            self.assertEqual(json.loads(target.read_text()), payload,
                             'refused publish must leave the old evidence intact')

    def test_publish_never_unlinks_another_runs_temp_file(self):
        # #1579 r5 P1: the temp name used to be the fixed `<name>.tmp`, so two
        # runs publishing into one directory shared it — the loser installed
        # the winner's half-written bytes via `replace()` and then unlinked the
        # winner's temp. The per-pid/serial name must leave a foreign
        # fixed-name temp untouched even on the refusing path.
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            target = out / 'summary.json'
            target.write_text('{"controls": []}\n')
            foreign = out / 'summary.json.tmp'
            foreign.write_text('half')
            with self.assertRaises(FileExistsError):
                suite._publish_json(target, {'controls': ['x']})
            self.assertEqual(foreign.read_text(), 'half',
                             'another run\'s temp file must survive our publish')
            self.assertEqual(sorted(p.name for p in out.iterdir()),
                             ['summary.json', 'summary.json.tmp'])

    def test_publish_names_the_reason_when_hard_links_are_unsupported(self):
        # #1579 r6 P2-a (minimax concurrency): the refusing path installs with
        # `os.link`, which raises ENOTSUP/EPERM on exFAT/SMB/container volumes
        # instead of `FileExistsError`; that used to abort the matrix with a
        # bare OSError. It must stay fail-closed but say what happened.
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            target = out / 'summary.json'
            unsupported = OSError(errno.EOPNOTSUPP, 'Operation not supported')
            with patch.object(suite.os, 'link', side_effect=unsupported):
                with self.assertRaises(OSError) as caught:
                    suite._publish_json(target, {'controls': []})
            self.assertIn('hard link', str(caught.exception))
            self.assertIn(str(target), str(caught.exception))
            self.assertFalse(target.exists(), 'a refused publish leaves no evidence file')
            self.assertEqual(sorted(p.name for p in out.iterdir()), [],
                             'the temporary file must not linger')


class MatrixMainStartupTests(unittest.TestCase):
    """#1734/#1666: every `matrix_main` startup refusal is rc 2 and leaves no dir.

    The function used to raise `SystemExit('<string>')` for a usage error and a
    mistyped selection (rc 1, "ran and did not pass") and let an existing output
    directory surface as `FileExistsError` (also rc 1). None of those runs
    measured anything, so they share the migrated entries' "never started" code
    -- and none of them had a test, because every other caller supplies the
    target and a fresh directory.
    """

    def call(self, root, *, args, env):
        """Run `matrix_main` on one row; return (SystemExit, stderr text)."""
        source = root / 'fixture.py'
        source.write_text('original')
        rows = [('first', source, 'original', 'changed', ['cell'])]
        errors = io.StringIO()
        with patch.dict('os.environ', env, clear=True), \
                contextlib.redirect_stderr(errors):
            with self.assertRaises(SystemExit) as caught:
                suite.matrix_main(root, source, 'prefix::', rows, passed_count=1,
                                  child_count=1, paths=[source], args=args)
        return caught.exception, errors.getvalue()

    def test_usage_and_unknown_selection_are_never_started(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            for args, needle in (([], 'usage:'),
                                 ([str(root / 'out'), 'no-such-mutation'],
                                  'unknown/duplicate')):
                with self.subTest(args=args):
                    refusal, stderr = self.call(
                        root, args=args, env={'CARGO_TARGET_DIR': str(root / 'target')})
                    self.assertEqual(refusal.code, 2,
                                     '#1734 a refusal that ran nothing must exit 2')
                    self.assertIn(needle, stderr)
                    self.assertFalse((root / 'out').exists(),
                                     '#1734 a refused run leaves no evidence directory')

    def test_unusable_target_is_never_started(self):
        # `dot` joined the matrix in #1666 fix-r10: `matrix_main` used to pass
        # it straight through to `run_owned`, which compiled inside the repo's
        # own `src` after the evidence directory had already been claimed.
        for mode, value, needle in (('missing', None, 'CARGO_TARGET_DIR'),
                                    ('empty', '', 'CARGO_TARGET_DIR'),
                                    ('blank', '   ', 'CARGO_TARGET_DIR'),
                                    ('dot', '.', 'target')):
            with tempfile.TemporaryDirectory() as name:
                root = Path(name)
                env = {} if value is None else {'CARGO_TARGET_DIR': value}
                out = root / 'out'
                with self.subTest(mode=mode):
                    refusal, stderr = self.call(root, args=[str(out)], env=env)
                    self.assertEqual(refusal.code, 2,
                                     f'#1734 a {mode} target never started')
                    self.assertIn(needle, stderr)
                    self.assertFalse(out.exists(),
                                     '#1734 a refused run leaves no evidence directory')

    def test_existing_output_directory_is_never_started(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            out = root / 'out'
            out.mkdir()
            refusal, stderr = self.call(
                root, args=[str(out)], env={'CARGO_TARGET_DIR': str(root / 'target')})
            self.assertEqual(refusal.code, 2, '#1734 a taken directory never started')
            self.assertIn('already exists', stderr)
            self.assertEqual(list(out.iterdir()), [],
                             '#1734 nothing written into a taken directory')


class ResultRowSchemaTests(unittest.TestCase):
    """#1579 P1: `matrix_main` rows must stay inside the registered schema.

    `_negkit/compare_verdicts.py` rejects unknown row fields with exit 2, so a
    row that grows a column turns every summary un-comparable (false FAIL).
    These tests run the real comparator over a real row shape.
    """

    @staticmethod
    def comparator():
        import importlib.util
        path = Path(__file__).resolve().parent / '_negkit' / 'compare_verdicts.py'
        spec = importlib.util.spec_from_file_location('pdrs_compare_verdicts', path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    @staticmethod
    def load(comparator, row):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'summary.json'
            path.write_text(json.dumps({'controls': [row],
                                        'counts': {'KILLED': 1, 'SURVIVED': 0, 'INVALID': 0}}))
            return comparator.load_table(path)

    def test_matrix_row_is_accepted_by_the_comparator(self):
        comparator = self.comparator()
        row = suite.result_row('rejection-silenced', status='KILLED',
                               reason='named assertions produced current nonce receipts',
                               compiled=True, restored_pass=True,
                               targets=[('rejects_unmigrated_qq', '#1386 marker')],
                               hits=['rejects_unmigrated_qq'])
        self.assertEqual(set(row), set(suite.RESULT_ROW_FIELDS))
        table = self.load(comparator, row)
        self.assertEqual(table[0]['case'], 'rejection-silenced')
        self.assertEqual(table[0]['status'], 'KILLED')
        self.assertEqual(table[0]['hits'], ['rejects_unmigrated_qq'])

    def test_unregistered_row_field_is_rejected(self):
        # Negative control: the schema guard must actually fire. Re-adding the
        # removed `cells` column reproduces the #1579 failure.
        comparator = self.comparator()
        row = suite.result_row('rejection-silenced', status='KILLED',
                               targets=[('rejects_unmigrated_qq', '#1386 marker')],
                               hits=['rejects_unmigrated_qq'])
        row['cells'] = [{'test': 'rejects_unmigrated_qq', 'status': 'KILLED'}]
        with self.assertRaises(comparator.InputError):
            self.load(comparator, row)

    def test_unjudged_row_keeps_hits_absent(self):
        # A row that never reached `judge` must not carry `hits: []`: the
        # comparator normalizes a missing `hits` to null and treats null != [],
        # so the same unjudged row would compare as NOT EQUIVALENT across the
        # old (no key) and new (empty list) runners — a false FAIL.
        comparator = self.comparator()
        row = suite.result_row('rejection-silenced',
                               reason='RuntimeError: positive matrix did not pass',
                               targets=[('rejects_unmigrated_qq', '#1386 marker')])
        self.assertNotIn('hits', row)
        self.assertLessEqual(set(row), set(suite.RESULT_ROW_FIELDS))
        # The comparator accepts the absent-key form; the empty list is a
        # different table, which is exactly why the key must stay absent.
        self.load(comparator, row)
        with_hits = dict(row, hits=[])
        self.load(comparator, with_hits)
        self.assertNotEqual(self.load(comparator, row), self.load(comparator, with_hits))


class AuditBindingTests(unittest.TestCase):
    """#1890: the shared `--audit` binding helpers must never match loosely."""

    MODULES = (('runtime::a::tests::', 'a.rs'), ('runtime::b::tests::', 'b.rs'))

    def test_binds_to_the_declaring_module_file(self):
        self.assertEqual(suite.bind_module_file('runtime::a::tests::case_a', self.MODULES),
                         ('a.rs', 'case_a'))
        self.assertEqual(suite.bind_module_file('runtime::b::tests::case_b', self.MODULES),
                         ('b.rs', 'case_b'))

    def test_empty_short_name_is_refused(self):
        # #1890-5: a row whose test id equals the prefix leaves `short == ''`,
        # and the old matcher then hit the bare `=>` text spread over the file.
        with self.assertRaises(ValueError, msg='#1890 an empty short name must be refused'):
            suite.bind_module_file('runtime::a::tests::', self.MODULES)

    def test_extra_module_level_is_refused(self):
        with self.assertRaises(ValueError, msg='#1890 an extra module level must be refused'):
            suite.bind_module_file('runtime::a::tests::nested::case_a', self.MODULES)

    def test_unaudited_module_is_refused(self):
        with self.assertRaises(ValueError, msg='#1890 an unaudited module must be refused'):
            suite.bind_module_file('runtime::c::tests::case_c', self.MODULES)

    def test_declares_test_accepts_every_existing_form(self):
        self.assertTrue(suite.declares_test('fn case_a() {', 'case_a'),
                        '#2064 a bare fn declaration must satisfy the fn form')
        self.assertTrue(suite.declares_test('async fn case_a() {', 'case_a'),
                        '#2064 an async fn declaration must satisfy the fn form')
        self.assertTrue(suite.declares_test('pub fn case_a() {', 'case_a'),
                        '#2064 a pub fn declaration must satisfy the fn form')
        self.assertTrue(suite.declares_test('unsafe extern "C" fn case_a() {', 'case_a'),
                        '#2064 an extern fn declaration must satisfy the fn form')
        self.assertTrue(suite.declares_test('matrix_tests! { case_a=>"row" }', 'case_a'),
                        '#2064 a table row must satisfy the table form')
        self.assertTrue(suite.declares_test('case!(case_a,);', 'case_a', 'case!'),
                        '#2064 a macro table row must satisfy the macro form')

    def test_declares_test_empty_short_is_refused(self):
        # #2064: an empty short name degenerated the table form to a bare `=>`
        # matcher that any arrow at a boundary satisfied (a tuple match arm),
        # so a mistyped binding audited nothing while --check stayed green.
        with self.assertRaises(
                ValueError, msg='#2064 declares_test must refuse an empty short name'):
            suite.declares_test('match pair { (1, 2) => 1 }', '')

    def test_declares_test_ignores_comment_and_string_carriers(self):
        # r10: a binding that a comment or a string literal satisfies audits
        # nothing; every noise carrier is blanked before matching.
        self.assertFalse(suite.declares_test('// fn case_a() {', 'case_a'),
                         '#2064 a line comment must not satisfy the fn form')
        self.assertFalse(suite.declares_test('/* /* nested */ fn case_a() { */', 'case_a'),
                         '#2064 a block comment must not satisfy the fn form')
        self.assertFalse(suite.declares_test('let s = "fn case_a() {";', 'case_a'),
                         '#2064 a string literal must not satisfy the fn form')
        self.assertFalse(suite.declares_test('let s = r#"fn case_a() {"#;', 'case_a'),
                         '#2064 a raw string literal must not satisfy the fn form')
        # A char literal holding a quote must not open a string either: with
        # the char branch gone, that quote would swallow code up to the next
        # string's opening quote and turn it back into un-stripped code.
        # (r3 order note: kept above the C-string carriers so the char-branch
        # mutant dies at this bound assertion first -- the cr carrier below
        # would otherwise die earlier as an unterminated-literal error.)
        self.assertFalse(
            suite.declares_test("let q = '\"'; let s = \"fn case_a() {\"; let r = '\"';",
                               'case_a'),
            '#2064 a quote char literal must not open a string')
        # r11: C-string prefixes. Without ``cr`` in the prefix set the inner
        # quote closes a bogus plain string and the carried ``fn`` text
        # returns to the code view, so the binding audits a carrier.
        self.assertFalse(suite.declares_test('let s = c"fn case_a() {";', 'case_a'),
                         '#2064 a C string literal must not satisfy the fn form')
        self.assertFalse(
            suite.declares_test('let s = cr#"x"fn case_a(){"#; let q = \'"\';', 'case_a'),
            '#2064 a raw C string literal must not satisfy the fn form')
        # r4: a raw hash delimiter with no ``r``-bearing prefix (``c#"…"#``)
        # is not legal Rust (rustc 1.99.0: ``error: prefix `c` is unknown``);
        # the scanner must refuse it instead of guessing a plain string whose
        # inner quote would return the carried text to the code view.
        # (Kept after the cr carrier: with the refusal disabled the carrier
        # dies as an unterminated string, so the bound kill is this message
        # check, not the assertRaises around it.)
        with self.assertRaises(ValueError) as raised:
            suite.declares_test('let s = c#"x"fn case_a(){"#; fn case_a() {}',
                               'case_a')
        self.assertIn('invalid Rust string prefix', str(raised.exception),
                      '#2064 a hash delimiter without an r prefix must fail closed')
        self.assertFalse(suite.declares_test('// case_a => "row"', 'case_a'),
                         '#2064 a line comment must not satisfy the table form')
        self.assertFalse(suite.declares_test('let s = "case_a => 1";', 'case_a'),
                         '#2064 a string literal must not satisfy the table form')

    def test_rust_code_only_blanks_escape_char_literals(self):
        # #2064 r11: ``'\u{…}'`` / ``'\xNN'`` are single-char literals; the
        # old _CHAR_START could not close them, so their quotes leaked into
        # the code view and could pair into bogus ``'x'`` matches that ate
        # neighbouring code. Blanking must take the whole literal.
        self.assertEqual(
            suite._rust_code_only(r"let c = '\u{1F600}'; let d = '\x41';"),
            'let c =  ; let d =  ;',
            '#2064 an escaped char literal must be blanked whole')

    def test_declares_test_unterminated_literal_is_refused(self):
        # #449 / #1166: a scanner that cannot close a literal must fail closed
        # instead of guessing where it ends.
        with self.assertRaises(
                ValueError, msg='#2064 an unterminated literal must fail closed'):
            suite.declares_test('let s = "fn case_a() {', 'case_a')

    def test_declares_test_needs_the_exact_name(self):
        # #1890: every assertion carries the message its mutant is bound to; a
        # bare assertFalse makes the kill indistinguishable from an unrelated
        # failure (the shared-runner audit classified it INVALID, not KILLED).
        message = '#1890 a missing declaration must be refused'
        # A declaration that merely starts with the short name must not count.
        self.assertFalse(suite.declares_test('fn case_ab() {', 'case_a'), message)
        # Nor may a longer table-row name that merely ends with it.
        self.assertFalse(
            suite.declares_test('xcase_a => 1,', 'case_a'),
            '#1890 a longer name ending in the short name must not count')
        # A longer identifier that merely embeds `fn` + the short name is not a
        # declaration either; the boundary is enforced on every form.
        self.assertFalse(
            suite.declares_test('let x = run_fncase_a();', 'case_a'),
            '#1890 a longer identifier must not satisfy the fn form')
        self.assertFalse(
            suite.declares_test('mycase!(case_a,);', 'case_a', 'case!'),
            '#1890 a longer macro name must not satisfy the table form')
        # The macro form is only accepted when the caller names the macro.
        self.assertFalse(suite.declares_test('case!(case_a,);', 'case_a'), message)
        self.assertFalse(suite.declares_test('case_other!(case_a,);', 'case_a', 'case!'), message)
        # r10: a qualifier-looking suffix glued inside a longer identifier is
        # not a qualifier; the old head.endswith() accepted both shapes.
        self.assertFalse(
            suite.declares_test('let x = runasyncfncase_a();', 'case_a'),
            '#2064 a qualifier glued onto a longer identifier must not satisfy the fn form')
        self.assertFalse(
            suite.declares_test('let x = xpubfncase_a();', 'case_a'),
            '#2064 a qualifier suffix inside a longer identifier must not satisfy the fn form')


BASELINE_DEFAULTS = (
    re.compile(r"\.get\(\s*['\"][A-Z_]*BASELINE['\"]\s*,\s*['\"]HEAD['\"]\s*\)"),
    re.compile(r"sys\.argv\[2\]\s+if\s+len\(sys\.argv\)\s*>\s*2\s+else\s+['\"]HEAD['\"]"),
)


def baseline_scanned_files(root):
    """#1890: the runner files the baseline-default scan actually reads.

    Both runner families count: ``negative*.py`` at any depth and the
    ``*_negative.py`` spelling used by ``turn-runner`` and its siblings
    (``input_negative.py``, ``recording_negative.py``, ...). ``test_*.py`` is
    excluded on purpose: the meta suite (``test_negative_suite.py``,
    ``test_negative_panic.py``, ``test_negative_optimize_guards.py`` …) carries
    the literal ``'HEAD'`` probe strings below, so scanning it would report the
    scan itself. ``negative-restore/test_restore.py`` is out of the glob by
    name (it does not contain ``negative`` in its file name); the ``test_``
    filter is what keeps the meta files out.
    """
    return [path for path in sorted(Path(root).rglob('*negative*.py'))
            if not path.name.startswith('test_')]


def unpinned_baseline_reads(root):
    """#1890: report every runner that reads a baseline with a literal default.

    ``os.environ.get(NAME, 'HEAD')`` and ``sys.argv[2] … else 'HEAD'`` treat an
    exported-but-empty variable as a real revision instead of "unset". What an
    empty revspec then does was measured, not assumed: ``git show ':path'``
    resolves it to the **index** (``git show ':src/.../lib.rs'`` prints the
    staged blob), so a guard that read it would grade a staged tree against
    itself. The sink refuses first — ``mutating_sources(..., baseline='')``
    dies in ``_assert_workspace_sources`` with
    ``git diff --name-only -z -- src …: fatal: bad revision ''`` — so the
    current failure mode is a loud refusal, not a false PASS. ``get(NAME) or
    'HEAD'`` (and the argv form folded the same way) keeps the documented
    "unset or empty means HEAD" contract instead of turning an empty variable
    into that refusal; this scan is what keeps the copies from drifting apart.

    Deliberately out of scan scope: the argparse spelling
    ``add_argument('--baseline', default='HEAD')``. That value is explicit
    caller input rather than an ambient default, and an explicitly empty
    ``--baseline ''`` reaches the same loud refusal above.
    """
    found = []
    for path in baseline_scanned_files(root):
        for number, line in enumerate(path.read_text().splitlines(), 1):
            if any(pattern.search(line) for pattern in BASELINE_DEFAULTS):
                found.append(f'{path.relative_to(root)}:{number}')
    return found


class BaselineDefaultTests(unittest.TestCase):
    def test_fixtures_fall_back_to_head_on_an_empty_baseline(self):
        self.assertEqual(
            unpinned_baseline_reads(Path(__file__).resolve().parent),
            [],
            '#1890 an empty baseline must fall back to HEAD, not be read verbatim')

    def test_scan_catches_a_literal_head_default(self):
        with tempfile.TemporaryDirectory() as root:
            env_probe = Path(root) / 'edgeclaw-probe' / 'negative.py'
            env_probe.parent.mkdir()
            env_probe.write_text("baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')\n")
            argv_probe = Path(root) / 'argv-probe' / 'negative_controls.py'
            argv_probe.parent.mkdir()
            argv_probe.write_text("baseline = sys.argv[2] if len(sys.argv) > 2 else 'HEAD'\n")
            # #1890 review: the `turn-runner` family is named `*_negative.py`, a
            # spelling `negative*.py` never matches; the probe keeps the wider
            # glob honest.
            suffix_probe = Path(root) / 'turn-runner-probe' / 'input_negative.py'
            suffix_probe.parent.mkdir()
            suffix_probe.write_text("baseline = env.get('PDRS_NEGATIVE_BASELINE', 'HEAD')\n")
            # Metrics probes: the meta suite's own probe strings and the
            # reverse-mutation payload in negative-restore must stay excluded,
            # or the scan reports itself.
            meta_probe = Path(root) / 'meta-probe' / 'test_negative_probe.py'
            meta_probe.parent.mkdir()
            meta_probe.write_text("baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')\n")
            self.assertEqual(
                unpinned_baseline_reads(Path(root)),
                ['argv-probe/negative_controls.py:1', 'edgeclaw-probe/negative.py:1',
                 'turn-runner-probe/input_negative.py:1'],
                '#1890 the scan must see all three literal HEAD default spellings')

    def test_scan_covers_both_runner_families(self):
        # #1890 review: an empty result is also what a scan that matches nothing
        # returns, so pin the real denominator instead of only asserting `[]`.
        root = Path(__file__).resolve().parent
        scanned = {path.relative_to(root).as_posix()
                   for path in baseline_scanned_files(root)}
        for name in ('turn-runner/input_negative.py',
                     'turn-runner/recording_negative.py',
                     'turn-runner/runner_negative.py',
                     'turn-runner/integrity_negative.py',
                     'turn-runner/forward_negative.py',
                     'turn-runner/live_negative.py',
                     'session-factory/negative_controls.py',
                     'negative_suite.py'):
            self.assertIn(name, scanned, '#1890 the baseline scan must read ' + name)
        # Measured at 261 files; the prefix-only glob this replaced yields 239,
        # so the bound sits between them instead of at a floor that a
        # half-broken glob would still clear.
        self.assertGreaterEqual(len(scanned), 250,
                                '#1890 the baseline scan must keep the *_negative.py family')


if __name__ == '__main__':
    unittest.main(verbosity=2)

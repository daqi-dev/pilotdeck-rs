#!/usr/bin/env python3
# anchor-selfcheck: unsupported contract unit tests use only disposable files and in-memory modules
"""Isolated, bounded tests of each M6 contract clause."""
from contextlib import contextmanager, nullcontext
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import contract as C
import compare_verdicts


class Stop(BaseException):
    pass


def observation(case, status='FAILED', *, code=None, payload=None):
    failed = status == 'FAILED'
    if code is None:
        code = 101 if failed else 0
    raw = f'running 1 test\ntest {case.test} ... {status}\n'
    if failed:
        raw += f"thread '{case.test}' panicked at fixture.rs:1:1:\n"
        raw += (case.assertion if payload is None else payload) + '\n'
    raw += 'test result: ' + ('FAILED. 0 passed; 1 failed;' if failed else 'ok. 1 passed; 0 failed;')
    raw += ' 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n'
    return subprocess.CompletedProcess([], code, raw, '')


class ContractTests(unittest.TestCase):
    longMessage = True

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='negkit-contract-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.output = self.root / 'evidence'
        self.env = {'CARGO_HOME': str(self.root), 'CARGO_TARGET_DIR': str(self.root / 'target'),
                    'PDRS_BASELINE': str(self.root)}
        self.cases = [C.Case('first', 'suite::first', '#1107 first value'),
                      C.Case('second', 'suite::second', '#1107 second value')]

    def run_contract(self, **kwargs):
        options = dict(guard=nullcontext, execute=lambda case, env: observation(case),
                       oracle=[{'expected': 1}], environ=self.env)
        options.update(kwargs)
        cases = options.pop('cases', self.cases)
        output = options.pop('output', self.output)
        return C.run(cases, output, **options)

    def test_publication_after_restore(self):
        events = []
        raw_replace = Path.replace

        @contextmanager
        def guard():
            events.append('enter')
            yield
            self.assertFalse((self.output / 'summary.json').exists(), '#1107 no summary before verification')
            events.append('verified')

        def replace(path, target):
            events.append('replace')
            self.assertEqual(events, ['enter', 'verified', 'replace'], '#1107 publication follows verified restore')
            self.assertEqual(path.name, 'summary.json.tmp', '#1107 summary uses a temporary file')
            document = json.loads(path.read_text())
            self.assertEqual(len(document['controls']), 2, '#1107 temporary JSON is complete')
            return raw_replace(path, target)

        with patch.object(Path, 'replace', replace):
            result = self.run_contract(guard=guard)
        self.assertEqual(result.code, 0, '#1107 healthy run exits zero')
        self.assertEqual(events.count('replace'), 1, '#1107 summary is published exactly once')
        self.assertEqual(json.loads((self.output / 'summary.json').read_text()), result.document,
                         '#1107 published summary matches returned verdicts')

    def test_fatal_revokes_completed_and_pending(self):
        for offset, error in enumerate([Stop('stop'), KeyboardInterrupt(), SystemExit(0),
                                        SystemExit(256), SystemExit(-256), SystemExit('stop')]):
            calls = []

            def execute(case, env):
                calls.append(case.mutation)
                if len(calls) == 2:
                    raise error
                return observation(case)

            output = self.root / str(offset)
            result = self.run_contract(output=output, execute=execute,
                                       cases=self.cases + [C.Case('third', 'suite::third', '#1107 third value')])
            self.assertEqual(result.code, 2, '#1107 post-entry BaseException exits two')
            self.assertEqual([row['status'] for row in result.document['controls']], ['INVALID'] * 3,
                             '#1107 fatal exit revokes every completed and pending row')
            self.assertEqual([row['hits'] for row in result.document['controls']], [[], [], []],
                             '#1107 invalidation clears prior hits')
            self.assertFalse((output / 'summary.json').exists(), '#1107 cancelled runs publish no summary')

    def test_guard_suppression_cannot_hide_cancellation(self):
        @contextmanager
        def guard():
            try:
                yield
            except BaseException:
                pass

        def execute(case, env):
            raise SystemExit(0)

        result = self.run_contract(guard=guard, execute=execute)
        self.assertEqual(result.code, 2, '#1107 suppressed cancellation remains failure')
        self.assertFalse((self.output / 'summary.json').exists(), '#1107 suppressed cancellation has no summary')

    def test_pre_entry_errors_exit_one(self):
        for offset, error in enumerate([Stop(), KeyboardInterrupt(), SystemExit(0), OSError('enter')]):
            @contextmanager
            def guard():
                raise error
                yield

            result = self.run_contract(output=self.root / str(offset), guard=guard)
            self.assertEqual(result.code, 1, '#1107 pre-entry failures exit one by entered flag')
            self.assertEqual([row['status'] for row in result.document['controls']], ['INVALID'] * 2,
                             '#1107 guard entry failure retains invalid rows')

    def test_restore_failure_exits_two(self):
        for offset, error in enumerate([Stop('verify'), KeyboardInterrupt(), SystemExit(0), OSError('restore')]):
            @contextmanager
            def guard():
                yield
                raise error

            output = self.root / str(offset)
            result = self.run_contract(output=output, guard=guard)
            self.assertEqual(result.code, 2, '#1107 restore failures exit two by entered flag')
            self.assertFalse((output / 'summary.json').exists(), '#1107 failed restoration has no summary')
            self.assertTrue(all(row['status'] == 'INVALID' for row in result.document['controls']),
                            '#1107 restore failure revokes completed verdicts')

    def test_publication_failure_is_closed(self):
        for index, stage in enumerate(('dump', 'rename-before', 'rename-after')):
            output = self.root / str(index)
            original = Path.replace

            def replace(path, target):
                if stage == 'rename-after':
                    original(path, target)
                raise Stop('publication')

            context = (patch.object(C.json, 'dump', side_effect=Stop('serialize')) if stage == 'dump'
                       else patch.object(Path, 'replace', replace))
            with context:
                result = self.run_contract(output=output)
            self.assertEqual(result.code, 2, '#1107 publication failure exits two')
            self.assertFalse((output / 'summary.json').exists(), '#1107 failed publication leaves no green summary')
            self.assertFalse((output / 'summary.json.tmp').exists(), '#1107 failed publication removes owned temp')
            self.assertTrue(all(row['status'] == 'INVALID' for row in result.document['controls']),
                            '#1107 publication failure revokes completed verdicts')

    def test_cancellation_after_publisher_returns(self):
        original = C.publish_summary
        def publish(*args, **kwargs):
            original(*args, **kwargs)
            raise Stop('after publisher returned')
        with patch.object(C, 'publish_summary', publish):
            result = self.run_contract()
        self.assertEqual(result.code, 2, '#1107 late cancellation exits two')
        self.assertFalse((self.output / 'summary.json').exists(), '#1107 outer finalizer revokes published summary')
        self.assertTrue(all(row['status'] == 'INVALID' for row in result.document['controls']),
                        '#1107 late cancellation revokes all rows')

    def test_publication_does_not_delete_foreign_inode(self):
        def replace(path, target):
            target.write_text('foreign evidence')
            raise Stop('publication')

        with patch.object(Path, 'replace', replace):
            result = self.run_contract()
        self.assertEqual(result.code, 2, '#1107 foreign publication failure exits two')
        self.assertTrue((self.output / 'summary.json').exists(),
                        '#1107 publication rollback only removes its own inode')
        self.assertEqual((self.output / 'summary.json').read_text(), 'foreign evidence',
                         '#1107 publication rollback only removes its own inode')

    def test_empty_cases_refused(self):
        with self.assertRaisesRegex(ValueError, 'empty case table', msg='#1107 empty case table has its own admission guard'):
            C.select_cases([], None, [1])
        result = self.run_contract(cases=[])
        self.assertEqual(result.code, 1, '#1107 empty case table refuses before guard')
        self.assertFalse(self.output.exists(), '#1107 case admission precedes output creation')

    def test_empty_oracle_refused(self):
        result = self.run_contract(oracle=iter(()))
        self.assertEqual(result.code, 1, '#1107 zero-row oracle refuses before guard')
        self.assertFalse(self.output.exists(), '#1107 oracle admission precedes output creation')

    def test_selection_validation(self):
        for selected in ([], ['unknown'], ['first', 'first']):
            result = self.run_contract(selected=selected)
            self.assertEqual(result.code, 1, '#1107 invalid selection refuses before guard')
        with self.assertRaisesRegex(ValueError, 'duplicate case name', msg='#1107 duplicate cases have their own admission guard'):
            C.select_cases(self.cases + self.cases, None, [1])
        result = self.run_contract(cases=self.cases + self.cases)
        self.assertEqual(result.code, 1, '#1107 duplicate case names refuse before guard')
        self.assertFalse(self.output.exists(), '#1107 bad selections never claim output')

    def test_sample_metadata(self):
        result = self.run_contract(selected=['second'])
        self.assertEqual(result.code, 0, '#1107 selected healthy controls pass')
        self.assertEqual((result.document['mode'], result.document['selected'], result.document['total']),
                         ('sample', ['second'], 2), '#1107 sample reports exact scope')
        self.assertEqual([row['mutation'] for row in result.document['controls']], ['second'],
                         '#1107 sample executes only its selected controls')

    def test_full_metadata(self):
        result = self.run_contract(selected=['second', 'first'])
        self.assertEqual((result.document['mode'], result.document['selected'], result.document['total']),
                         ('full', ['first', 'second'], 2), '#1107 full metadata follows declaration order')

    def test_nonempty_output_untouched(self):
        self.output.mkdir()
        for name in ('summary.json', 'summary.json.tmp', 'evidence.txt'):
            (self.output / name).write_text('prior ' + name)
        before = {path.name: (path.read_bytes(), path.stat().st_ino) for path in self.output.iterdir()}
        result = self.run_contract()
        after = {path.name: (path.read_bytes(), path.stat().st_ino) for path in self.output.iterdir()}
        self.assertEqual(result.code, 1, '#1107 nonempty output refuses before guard')
        self.assertEqual(after, before, '#1107 refusal never deletes or renames old evidence')

    def test_output_file_untouched(self):
        self.output.write_text('prior file')
        result = self.run_contract()
        self.assertEqual(result.code, 1, '#1107 existing output file refuses before guard')
        self.assertEqual(self.output.read_text(), 'prior file', '#1107 refusal preserves existing file')

    def test_claim_race_is_exclusive(self):
        self.output.mkdir()
        original = Path.open
        injected = []

        def open_file(path, mode='r', *args, **kwargs):
            if path == self.output / '.claim' and not injected:
                injected.append(True)
                with original(path, 'x') as stream:
                    stream.write('competitor')
            return original(path, mode, *args, **kwargs)

        with patch.object(Path, 'open', open_file):
            result = self.run_contract()
        self.assertEqual(result.code, 1, '#1107 claim race has a single winner')
        self.assertEqual((self.output / '.claim').read_text(), 'competitor', '#1107 loser preserves winner claim')

    def test_permanent_claim_refuses_successor(self):
        @contextmanager
        def guard():
            raise Stop('predecessor cancelled before entry')
            yield

        first = self.run_contract(guard=guard)
        second = self.run_contract()
        self.assertEqual((first.code, second.code), (1, 1), '#1107 cancelled predecessor claim is never reclaimed')
        self.assertTrue((self.output / '.claim').is_file(), '#1107 claim persists as evidence')

    def test_environment_required(self):
        for key in self.env:
            for value in (None, '', 'relative/path'):
                env = dict(self.env)
                env[key] = value
                result = self.run_contract(environ=env)
                self.assertEqual(result.code, 1, '#1107 environment locations are explicit absolute paths')
        for key in ('CARGO_HOME', 'PDRS_BASELINE'):
            env = dict(self.env, **{key: str(self.root / 'missing')})
            result = self.run_contract(environ=env)
            self.assertEqual(result.code, 1, '#1107 environment roots must exist')
        self.assertFalse(self.output.exists(), '#1107 environment is checked before claim')

    def test_environment_preserved(self):
        seen = []
        def execute(case, env):
            seen.append(env)
            return observation(case)
        result = self.run_contract(execute=execute)
        self.assertEqual(result.code, 0, '#1107 valid environment passes')
        self.assertEqual(seen[0]['CARGO_TARGET_DIR'], self.env['CARGO_TARGET_DIR'],
                         '#1107 caller target directory is preserved')
        self.assertEqual(seen[0]['CARGO_HOME'], self.env['CARGO_HOME'], '#1107 caller cargo home is preserved')
        self.assertEqual(seen[0]['PDRS_BASELINE'], self.env['PDRS_BASELINE'], '#1107 caller baseline is preserved')
        self.assertEqual(seen[0]['CARGO_PROFILE_TEST_DEBUG'], 'line-tables-only', '#1107 test debug default is bounded')
        self.assertEqual(seen[0]['CARGO_PROFILE_DEV_DEBUG'], 'line-tables-only', '#1107 dev debug default is bounded')
        custom = C.environment(dict(self.env, CARGO_PROFILE_TEST_DEBUG='0'))
        self.assertEqual(custom['CARGO_PROFILE_TEST_DEBUG'], '0', '#1107 explicit debug override survives')

    def test_sigint_default_and_restore(self):
        before = signal.signal(signal.SIGINT, signal.SIG_IGN)
        self.addCleanup(signal.signal, signal.SIGINT, before)
        seen = []
        def execute(case, env):
            seen.append(signal.getsignal(signal.SIGINT))
            return observation(case)
        result = self.run_contract(execute=execute)
        self.assertEqual(result.code, 0, '#1107 sigint environment passes')
        self.assertEqual(seen, [signal.default_int_handler] * 2, '#1107 runner uses sigint-default')
        self.assertEqual(signal.getsignal(signal.SIGINT), signal.SIG_IGN, '#1107 prior signal handler is restored')

    def test_sigint_restore_failure_does_not_publish(self):
        original = signal.signal
        before = signal.getsignal(signal.SIGINT)
        count = []
        def install(sig, handler):
            count.append(handler)
            if len(count) == 2:
                raise Stop('handler restore')
            return original(sig, handler)
        try:
            with patch.object(signal, 'signal', install):
                result = self.run_contract()
        finally:
            original(signal.SIGINT, before)
        self.assertEqual(result.code, 2, '#1107 signal cleanup failure exits two')
        self.assertFalse((self.output / 'summary.json').exists(), '#1107 cleanup failure cannot publish completed rows')

    def test_sigint_during_publication(self):
        before = signal.signal(signal.SIGINT, signal.SIG_IGN)
        self.addCleanup(signal.signal, signal.SIGINT, before)
        original = Path.replace
        for stage in ('before', 'after'):
            output = self.root / stage
            def replace(path, target):
                if stage == 'after':
                    original(path, target)
                signal.raise_signal(signal.SIGINT)
                return original(path, target) if stage == 'before' else target
            with patch.object(Path, 'replace', replace):
                result = self.run_contract(output=output)
            self.assertEqual(result.code, 2, '#1107 publication still obeys sigint-default')
            self.assertFalse((output / 'summary.json').exists(), '#1107 interrupted publication is revoked')
            self.assertEqual(signal.getsignal(signal.SIGINT), signal.SIG_IGN, '#1107 cancelled publication restores prior handler')

    def test_multiline_and_unbound_markers_refused(self):
        for marker in ('', 'message', '#1107 line\ncontinuation', '#1107 line\r', '#1107 line\x00'):
            result = self.run_contract(cases=[C.Case('bad', 'suite::test', marker)])
            self.assertEqual(result.code, 1, '#1107 assertion must be an issue-specific single line')
        for test in ('', 'suite test', 'suite\ntest'):
            result = self.run_contract(cases=[C.Case('bad', test, '#1107 good marker')])
            self.assertEqual(result.code, 1, '#1107 test binding is an exact nonempty name')

    def test_named_normal_failure_kills(self):
        for payload in (None, "assertion `left == right` failed: " + self.cases[0].assertion):
            status, _, hits = C.classify(observation(self.cases[0], payload=payload), self.cases[0])
            self.assertEqual((status, hits), ('KILLED', ['suite::first']), '#1107 only named normal assertion kills')

    def test_normal_pass_survives(self):
        self.assertEqual(C.classify(observation(self.cases[0], 'ok'), self.cases[0])[0], 'SURVIVED',
                         '#1107 named normal pass survives')
        result = self.run_contract(execute=lambda case, env: observation(case, 'ok'))
        self.assertEqual(result.code, 2, '#1107 survived controls cannot exit zero')

    def test_abnormal_exit_invalid(self):
        for code in (-2, -9, -15, 1, 124, 137, 143):
            self.assertEqual(C.classify(observation(self.cases[0], code=code), self.cases[0])[0], 'INVALID',
                             '#1107 signal and abnormal exits never kill')

    def test_named_test_required(self):
        case = self.cases[0]
        for before, after in [('running 1 test', 'running 0 tests'),
                              ('running 1 test', 'running 2 tests'),
                              (f'test {case.test} ... FAILED', 'test other ... FAILED'),
                              (f'test {case.test} ... FAILED', ''),
                              ('FAILED. 0 passed; 1 failed;', 'FAILED. 1 passed; 0 failed;')]:
            result = observation(case)
            result.stdout = result.stdout.replace(before, after)
            self.assertEqual(C.classify(result, case)[0], 'INVALID', '#1107 kill requires exactly one named failure')
        result = observation(case)
        result.stdout += result.stdout
        self.assertEqual(C.classify(result, case)[0], 'INVALID', '#1107 concatenated runs are not evidence')

    def test_exclusive_panic_marker_required(self):
        case = self.cases[0]
        for payload in ('unrelated', 'unrelated\n' + case.assertion, 'path/' + case.assertion,
                        'stack backtrace: ' + case.assertion, case.assertion + ' extra'):
            self.assertEqual(C.classify(observation(case, payload=payload), case)[0], 'INVALID',
                             '#1107 marker belongs to the first assertion payload line')
        result = observation(case)
        result.stdout = result.stdout.replace(f"thread '{case.test}'", "thread 'other'")
        self.assertEqual(C.classify(result, case)[0], 'INVALID', '#1107 marker belongs to the named panic')
        result = observation(case, payload='unrelated')
        result.stdout += case.assertion + '\n'
        self.assertEqual(C.classify(result, case)[0], 'INVALID', '#1107 stdout markers cannot kill')
        result = observation(case)
        result.stdout += "thread 'watchdog' panicked at other.rs:1:1:\nwatchdog expired\n"
        self.assertEqual(C.classify(result, case)[0], 'INVALID', '#1107 additional panics invalidate attribution')

    def test_backtrace_is_not_panic_payload(self):
        case = self.cases[0]
        for tail in ('stack backtrace:\n#813 timeout: frame data',
                     'note: diagnostics\n#813 timeout: note data', 'failures:\n#813 timeout: report data'):
            result = observation(case, payload=case.assertion + '\n' + tail)
            self.assertEqual(C.classify(result, case)[0], 'KILLED', '#1107 backtrace and report text are not panic payload')

    def test_counts_match_rows(self):
        third = C.Case('third', 'suite::third', '#1107 third value')
        def execute(case, env):
            if case.mutation == 'third':
                return observation(case, code=-9)
            return observation(case, 'ok' if case.mutation == 'second' else 'FAILED')
        result = self.run_contract(cases=self.cases + [third], execute=execute)
        self.assertEqual(result.document['counts'], {'KILLED': 1, 'SURVIVED': 1, 'INVALID': 1},
                         '#1107 counts exactly reflect every verdict')
        self.assertEqual(result.code, 2, '#1107 mixed verdicts do not pass')

    def test_timeout_watchdog_invalid(self):
        case = self.cases[0]
        result = observation(case, payload=case.assertion + '\n#813 timeout: deadline expired')
        self.assertEqual(C.classify(result, case)[0], 'INVALID',
                         '#1107 timeout watchdog panic never kills')
        for prefix in ('#813 timeout:', '#813 gateway watchdog:'):
            case = C.Case('deadline', 'suite::deadline', prefix + ' deadline expired')
            self.assertEqual(C.classify(observation(case), case)[0], 'INVALID',
                             '#1107 timeout watchdog panic never kills')

    def test_assertion_data_can_contain_timeout_words(self):
        case = C.Case('data', 'suite::data', '#1107 timeout error is retained')
        payload = ('assertion `left == right` failed: ' + case.assertion
                   + '\n  left: "timeout"\n right: "watchdog"')
        self.assertEqual(C.classify(observation(case, payload=payload), case)[0], 'KILLED',
                         '#1107 assertion data is not a watchdog diagnostic')

    def test_exceptions_continue_all_cases(self):
        for index, error in enumerate([FileNotFoundError('spawn'), subprocess.TimeoutExpired('test', 1)]):
            calls = []
            def execute(case, env):
                calls.append(case.mutation)
                if len(calls) == 1:
                    raise error
                return observation(case)
            result = self.run_contract(output=self.root / str(index), execute=execute)
            self.assertEqual(calls, ['first', 'second'], '#1107 ordinary case errors do not abort remaining cases')
            self.assertEqual(result.code, 2, '#1107 launch timeout errors cannot pass')
            self.assertEqual([row['status'] for row in result.document['controls']], ['INVALID', 'KILLED'],
                             '#1107 launch timeout errors stay invalid')

    def test_canonical_schema_and_equivalence(self):
        result = self.run_contract()
        for row in result.document['controls']:
            self.assertEqual(set(row), {'mutation', 'test', 'assertion', 'status', 'reason', 'hits'},
                             '#1107 summary rows use the canonical comparator schema')
        other = self.run_contract(output=self.root / 'other')
        self.assertEqual(other.code, 0, '#1107 repeated run passes')
        report = compare_verdicts.compare(compare_verdicts.load_table(self.output / 'summary.json'),
                                          compare_verdicts.load_table(self.root / 'other/summary.json'))
        self.assertTrue(report['equivalent'], '#1107 independent canonical runs compare equivalent')


class ToyTests(unittest.TestCase):
    def test_real_process_exits_and_equivalence(self):
        script = Path(__file__).with_name('toy_runner.py')
        with tempfile.TemporaryDirectory(prefix='negkit-toy-test-') as directory:
            root = Path(directory)
            for scenario, code in [('pass', 0), ('refuse', 1), ('restore-fail', 2), ('cancel', 2)]:
                output = root / scenario
                result = subprocess.run([sys.executable, '-I', '-B', str(script), str(output),
                                         '--scenario', scenario], capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, code, '#1107 toy returns the lifecycle exit code: ' + result.stdout + result.stderr)
                self.assertEqual((output / 'summary.json').exists(), scenario == 'pass', '#1107 toy publication follows verified restore')
                if scenario != 'refuse':
                    self.assertIn('guard: restored; summary=False', result.stdout, '#1107 toy restored before publication')
            second = root / 'again'
            result = subprocess.run([sys.executable, '-I', '-B', str(script), str(second)],
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, '#1107 second toy succeeds: ' + result.stdout + result.stderr)
            report = compare_verdicts.compare(compare_verdicts.load_table(root / 'pass/summary.json'),
                                              compare_verdicts.load_table(second / 'summary.json'))
            self.assertTrue(report['equivalent'], '#1107 toy summaries compare EQUIVALENT')


if __name__ == '__main__':
    unittest.main(verbosity=2)

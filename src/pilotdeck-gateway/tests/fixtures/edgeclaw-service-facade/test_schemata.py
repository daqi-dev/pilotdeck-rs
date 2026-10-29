#!/usr/bin/env python3
# anchor-selfcheck: unsupported tests of the precompiled runner, no product source mutations
"""#988 failure-path tests; no cargo or product source writes."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import schemata as runner


def result(test, state='ok', anchor='', file='pilotdeck-gateway/src/../tests/edgeclaw_service_facade.rs'):
    return SimpleNamespace(returncode=0 if state == 'ok' else 101,
        stdout=f'test {runner.PREFIX}{test} ... {state}\n'
               + ('test result: ok. 1 passed;' if state == 'ok' else 'test result: FAILED'),
        # Real libtest header: the shared judge reads the named test's own
        # `thread '<name>' panicked at` block (#818), never a bare line.
        stderr='' if state == 'ok' else f"thread '{runner.PREFIX}{test}' panicked at {file}:1:1:\n{anchor}")


def binding(name, witness=None):
    return dict(name=name, test=name, anchor=f'#988 {name}', witness=witness, include_ignored=False)


class SchemataTests(unittest.TestCase):
    def test_legacy_phase_timeouts_restore_and_allow_successors(self):
        # Only temporary text is changed: this does not bypass a product HEAD guard.
        with tempfile.TemporaryDirectory() as temp, patch.object(runner.legacy.time, 'sleep'):
            source = Path(temp) / 'fixture.txt'
            original = b'original\n'
            row = dict(binding('sample', 'witness'), path=source, old='original', new='changed')
            for phase in ('positive', 'witness-positive', 'compile', 'witness-negative', 'negative', 'restored'):
                with self.subTest(phase=phase):
                    source.write_bytes(original)
                    calls = []
                    def invoke(label, test, include_ignored=False, compile_only=False):
                        calls.append(label)
                        if label == 'sample-' + phase:
                            raise subprocess.TimeoutExpired(['fixture'], 1)
                        bad = label.endswith('-negative') and test == 'sample'
                        answer = result(test, 'FAILED' if bad else 'ok', row['anchor'], runner.legacy.TEST_FILE)
                        # The legacy runner launches the test binary directly, so
                        # its libtest output carries no facade prefix in either the
                        # status line or the thread header (#818).
                        answer.stdout = answer.stdout.replace(runner.PREFIX, '')
                        answer.stderr = answer.stderr.replace(runner.PREFIX, '')
                        return answer
                    self.assertEqual(runner.legacy.run_mutant(row, original, invoke)[0],
                                     'INVALID',
                                     '#818 an interrupted legacy phase must invalidate, not kill')
                    self.assertEqual(source.read_bytes(), original,
                                     '#818 every legacy phase must restore the source bytes')
                    self.assertIn('sample-restored', calls,
                                  '#818 the successor phase must still run after an interruption')
                    successor = dict(row, name='second')
                    self.assertEqual(runner.legacy.run_mutant(successor, original, invoke)[0],
                                     'KILLED',
                                     '#818 a later valid mutant must stay KILLED after an interruption')
                    self.assertEqual(source.read_bytes(), original,
                                     '#818 mutating and restored phases must leave the source intact')

    def test_complete_suite_and_restore_after_failure(self):
        bindings = [binding('first'), binding('second')]
        calls = []
        def invoke(label, test, selector, row):
            calls.append((label, selector))
            if selector == 'first':
                raise subprocess.TimeoutExpired(['test'], 1)
            return result(test, 'FAILED' if selector else 'ok', row['anchor'])
        rows = list(runner.execute_suite(bindings, invoke, prefix=runner.PREFIX))
        self.assertEqual([r['status'] for r in rows], ['INVALID', 'KILLED'])
        self.assertIn(('first-restored', None), calls)
        self.assertIn(('second-negative', 'second'), calls)

    def test_positive_restore_and_witness_fail_closed(self):
        for failed in ('sample-positive', 'sample-restored', 'sample-witness-positive', 'sample-witness-negative'):
            with self.subTest(failed=failed):
                def invoke(label, test, selector, row):
                    return result(test, 'FAILED' if label == failed or (selector and test == 'sample') else 'ok', row['anchor'])
                rows = list(runner.execute_suite([binding('sample', 'witness')], invoke,
                                                 prefix=runner.PREFIX))
                self.assertEqual(rows[0]['status'], 'INVALID')

    def test_wrong_panic_missing_test_and_survivor(self):
        row = binding('sample')
        for response, expected in (
            (result('sample'), 'SURVIVED'),
            (result('other', 'FAILED', row['anchor']), 'INVALID'),
            (result('sample', 'FAILED', 'unrelated'), 'INVALID'),
            (result('sample', 'FAILED', row['anchor'], 'other.rs'), 'INVALID'),
            (SimpleNamespace(returncode=101, stdout='could not compile', stderr=''), 'INVALID'),
            (result('sample', 'FAILED', row['anchor']), 'KILLED'),
        ):
            with self.subTest(expected=expected, response=response):
                def invoke(label, test, selector, _row):
                    return response if selector else result(test)
                # run() keeps this fixture's stricter panic-file verdict, with
                # the library inclusion's explicit ../ path.
                def verdict(response_, bound):
                    return runner.legacy.classify(
                        response_, bound,
                        source_file='pilotdeck-gateway/src/../tests/edgeclaw_service_facade.rs')
                rows = list(runner.execute_suite([row], invoke, prefix=runner.PREFIX,
                                                 verdict=verdict))
                self.assertEqual(rows[0]['status'], expected,
                                 f'#818 synthetic libtest log verdict must be {expected}')

    def test_bare_panic_line_is_not_evidence(self):
        # #818 negative control: the exact bound marker without the named
        # test's own libtest thread header must stay INVALID, never KILLED.
        row = binding('sample')
        response = SimpleNamespace(
            returncode=101,
            stdout=f'test {runner.PREFIX}sample ... FAILED\ntest result: FAILED',
            stderr='panicked at pilotdeck-gateway/src/../tests/'
                   f"edgeclaw_service_facade.rs:1:1:\n{row['anchor']}")

        def invoke(label, test, selector, _row):
            return response if selector else result(test)

        def verdict(response_, bound):
            return runner.legacy.classify(
                response_, bound,
                source_file='pilotdeck-gateway/src/../tests/edgeclaw_service_facade.rs')

        rows = list(runner.execute_suite([row], invoke, prefix=runner.PREFIX, verdict=verdict))
        self.assertEqual(rows[0]['status'], 'INVALID',
                         '#818 a bare panic line without the thread header is not evidence')

    def test_environment_and_request_validation(self):
        env = runner.clean_environment({'PDRS_MUTANT': 'bad', 'PDRS_FACADE_ENCODER_MUTANT': 'bad', 'KEEP': 'yes'})
        self.assertEqual(env, {'KEEP': 'yes'})
        self.assertEqual(len(runner.selected_rows([])), 28)
        for request in (['missing'], ['default-root-dir', 'default-root-dir']):
            with self.assertRaises(ValueError):
                runner.selected_rows(request)

    def test_stale_anchor_missing_switch_and_duplicate_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / 'source.rs'
            row = dict(binding('sample'), path=source, old='original', new='changed')
            for content in ('missing', 'original original', 'original'):
                source.write_text(content)
                with patch.object(runner.legacy, 'MUTANTS', [row]), self.assertRaises(ValueError):
                    runner.preflight()
            source.write_text('original crate::mutants::on("sample")')
            with patch.object(runner.legacy, 'MUTANTS', [row]):
                runner.preflight()
            with patch.object(runner.legacy, 'MUTANTS', [row, row]), self.assertRaises(ValueError):
                runner.preflight()

    def test_exact_cargo_artifact_required(self):
        with tempfile.TemporaryDirectory() as temp, patch.dict(os.environ, {'CARGO_TARGET_DIR': temp}):
            binary = Path(temp) / 'test-binary'
            binary.write_text('fixture')
            item = dict(reason='compiler-artifact', target={'name': 'pilotdeck_gateway', 'kind': ['lib']},
                        profile={'test': True}, features=['pdrs-mutants'], executable=str(binary))
            def build(data, code=0):
                return SimpleNamespace(returncode=code, stdout=json.dumps(data), stderr='')
            self.assertEqual(runner.artifact(build(item)), binary.resolve())
            for bad in (dict(item, features=[]), dict(item, profile={'test': False}),
                        dict(item, target={'name': 'other', 'kind': ['lib']}), dict(item, executable='/missing')):
                with self.assertRaises(RuntimeError): runner.artifact(build(bad))
            with self.assertRaises(RuntimeError): runner.artifact(build(item, 101))

    def test_node_failure_does_not_skip_rust_controls(self):
        rust = next(row for row in runner.legacy.MUTANTS if row['name'] == 'default-root-dir')
        ts = next(row for row in runner.legacy.MUTANTS if row['include_ignored'])
        def process(command, **kwargs):
            if command[:2] == ['git', 'rev-parse']:
                return SimpleNamespace(returncode=0, stdout='test-head', stderr='')
            if command[0] == 'cargo':
                return SimpleNamespace(returncode=0, stdout='build fixture', stderr='')
            if command[0] == 'node':
                return SimpleNamespace(returncode=0, stdout='v26.0.0', stderr='')
            selected = kwargs['env'].get('PDRS_MUTANT')
            self.assertIn('--exact', command)
            self.assertIn('--include-ignored', command)
            return result(rust['test'], 'FAILED' if selected else 'ok', rust['anchor'])
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)
            with patch.object(runner, 'run_owned', side_effect=process), \
                 patch.object(runner, 'wait_for_slot', return_value=SimpleNamespace(stdout='', stderr='')), \
                 patch.object(runner, 'artifact', return_value=Path(__file__).resolve()):
                self.assertEqual(runner.run(out, [ts, rust], {},
                                            requested=[ts['name'], rust['name']]), 1)
            rows = json.loads((out / 'summary.json').read_text())['controls']
            self.assertEqual([row['status'] for row in rows], ['INVALID', 'KILLED'])
            self.assertIn('Node v22.22.0', rows[0]['reason'])

    def test_denominator_drift_fails_the_whole_suite(self):
        # #996 fix-r5 review P1: the facade entry has its own run(), so the r4
        # denominator check inside the shared run() never saw it; a main()
        # that drops a declared row at runtime used to publish the shrunken
        # suite as an all-KILLED success. The declared denominator is the
        # checker's static extraction of negative.py, never the runtime
        # legacy.MUTANTS the executed rows could already have shrunk.
        full = list(runner.legacy.MUTANTS)
        by_name = {row['name']: row for row in full}
        dropped = full[0]['name']

        def process(command, **kwargs):
            if command[:2] == ['git', 'rev-parse']:
                return SimpleNamespace(returncode=0, stdout='test-head\n', stderr='')
            if command[0] == 'cargo':
                return SimpleNamespace(returncode=0, stdout='build fixture', stderr='')
            if command[0] == 'node':
                return SimpleNamespace(returncode=0, stdout='v22.22.0\n', stderr='')
            selector = kwargs['env'].get('PDRS_MUTANT')
            named = command[1][len(runner.PREFIX):]
            if selector is None:
                return result(named, 'ok')
            row = by_name[selector]
            # The bound test dies on its own anchor; a witness stays green.
            return result(named, 'FAILED' if named == row['test'] else 'ok', row['anchor'])

        with tempfile.TemporaryDirectory() as temp, \
                patch.object(runner, 'run_owned', side_effect=process), \
                patch.object(runner, 'wait_for_slot',
                             return_value=SimpleNamespace(stdout='', stderr='')), \
                patch.object(runner, 'artifact', return_value=Path(__file__).resolve()):
            def suite(selected, **kwargs):
                out = Path(temp) / kwargs.pop('label')
                out.mkdir()
                with patch.object(runner.legacy, 'MUTANTS', selected):
                    code = runner.run(out, selected, {}, **kwargs)
                return code, json.loads((out / 'summary.json').read_text())

            # The review's exact shape: the runtime table loses its first row.
            code, document = suite(full[1:], label='missing')
            self.assertEqual(code, 1, '#996 r5 a dropped declared row must fail '
                                      'the whole facade suite')
            self.assertEqual(document['counts'],
                             {'KILLED': 0, 'SURVIVED': 0, 'INVALID': len(full) - 1},
                             '#996 r5 the drifted run must count every row INVALID')
            reasons = [row['reason'] for row in document['controls']]
            self.assertTrue(all('switch denominator drift' in reason for reason in reasons),
                            '#996 r5 every drifted row must name the denominator check')
            self.assertTrue(any(f'missing=[{dropped!r}]' in reason for reason in reasons),
                            f'#996 r5 the drift must list the missing {dropped}')
            # #996 fix-r6 review P0: the table emptied whole at runtime leaves
            # no executed row, and any() over an empty row list reads False, so
            # an entrance without the ``not rows`` guard could exit 0 with an
            # all-zero summary. The suite-level row must decide the exit code
            # and stay in the summary here too.
            code, document = suite([], label='emptied')
            self.assertEqual(code, 1, '#996 r6 the emptied runtime table must fail '
                                      'the whole facade suite')
            self.assertEqual(document['counts'],
                             {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                             '#996 r6 the emptied run must keep a suite-level INVALID row')
            reason = document['controls'][0]['reason']
            self.assertIn('switch denominator drift', reason,
                          '#996 r6 the suite-level row must name the denominator check')
            self.assertIn(dropped, reason,
                          '#996 r6 the drift must still list the declared names')
            # A matching full denominator keeps the whole suite green.
            code, document = suite(full, label='clean')
            self.assertEqual(code, 0, '#996 r5 a matching denominator must stay green')
            self.assertEqual(document['counts'],
                             {'KILLED': len(full), 'SURVIVED': 0, 'INVALID': 0},
                             '#996 r5 a matching denominator must keep every row KILLED')
            # An explicit selection is its own denominator.
            code, document = suite(full[:1], requested=[full[0]['name']], label='subset')
            self.assertEqual(code, 0, '#996 r5 an explicit selection is its own denominator')
            self.assertEqual(document['counts'], {'KILLED': 1, 'SURVIVED': 0, 'INVALID': 0},
                             '#996 r5 an explicit selection must stay KILLED')
            # A requested name beyond the declared table cannot hide behind it.
            code, document = suite(full[:1], requested=['forged'], label='beyond')
            self.assertEqual(code, 1, '#996 r5 a requested name outside the declaration '
                                      'must fail the whole suite')
            self.assertIn("extra=['forged']", document['controls'][0]['reason'],
                          '#996 r5 the beyond-declaration drift must list the switch')

            # main() must wire the user's requested subset into the check.
            out = Path(temp) / 'main-subset'
            argv = [str(Path(runner.__file__).resolve()), str(out), full[0]['name'],
                    full[1]['name']]
            with patch.object(sys, 'argv', argv), \
                    patch.dict(os.environ, {'CARGO_TARGET_DIR': str(temp)}):
                code = runner.main()
            document = json.loads((out / 'summary.json').read_text())
            self.assertEqual(code, 0, '#996 r5 main() must pass the requested subset '
                                      'to the denominator check')
            self.assertEqual(document['counts'], {'KILLED': 2, 'SURVIVED': 0, 'INVALID': 0},
                             '#996 r5 a two-name request must run exactly two rows')

    def test_summary_is_complete_and_atomic(self):
        import time
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)
            with patch.object(Path, 'replace', autospec=True, wraps=Path.replace) as publish:
                runner.publish_summary(out, [{'status': 'INVALID'}], time.monotonic(), 1.0)
                publish.assert_called_once_with(out / 'summary.json.tmp', out / 'summary.json')
            runner.publish_summary(out, [{'status': 'KILLED'}, {'status': 'INVALID'}], time.monotonic(), 1.0)
            self.assertEqual(json.loads((out / 'summary.json').read_text())['counts'],
                             {'KILLED': 1, 'SURVIVED': 0, 'INVALID': 1})
            self.assertFalse((out / 'summary.json.tmp').exists())


if __name__ == '__main__':
    unittest.main()

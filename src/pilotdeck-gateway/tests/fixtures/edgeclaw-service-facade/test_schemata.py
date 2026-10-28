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
        stderr='' if state == 'ok' else f'panicked at {file}:1:1:\n{anchor}')


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
                        answer.stdout = answer.stdout.replace(runner.PREFIX, '')
                        return answer
                    self.assertEqual(runner.legacy.run_mutant(row, original, invoke)[0], 'INVALID')
                    self.assertEqual(source.read_bytes(), original)
                    self.assertIn('sample-restored', calls)
                    successor = dict(row, name='second')
                    self.assertEqual(runner.legacy.run_mutant(successor, original, invoke)[0], 'KILLED')
                    self.assertEqual(source.read_bytes(), original)

    def test_complete_suite_and_restore_after_failure(self):
        bindings = [binding('first'), binding('second')]
        calls = []
        def invoke(label, test, selector, row):
            calls.append((label, selector))
            if selector == 'first':
                raise subprocess.TimeoutExpired(['test'], 1)
            return result(test, 'FAILED' if selector else 'ok', row['anchor'])
        rows = list(runner.execute_suite(bindings, invoke))
        self.assertEqual([r['status'] for r in rows], ['INVALID', 'KILLED'])
        self.assertIn(('first-restored', None), calls)
        self.assertIn(('second-negative', 'second'), calls)

    def test_positive_restore_and_witness_fail_closed(self):
        for failed in ('sample-positive', 'sample-restored', 'sample-witness-positive', 'sample-witness-negative'):
            with self.subTest(failed=failed):
                def invoke(label, test, selector, row):
                    return result(test, 'FAILED' if label == failed or (selector and test == 'sample') else 'ok', row['anchor'])
                rows = list(runner.execute_suite([binding('sample', 'witness')], invoke))
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
                self.assertEqual(list(runner.execute_suite([row], invoke))[0]['status'], expected)

    def test_environment_and_request_validation(self):
        env = runner.clean_environment({'PDRS_MUTANT': 'bad', 'PDRS_FACADE_ENCODER_MUTANT': 'bad', 'KEEP': 'yes'})
        self.assertEqual(env, {'KEEP': 'yes'})
        self.assertEqual(len(runner.selected_rows([])), 24)
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
                self.assertEqual(runner.run(out, [ts, rust], {}), 1)
            rows = json.loads((out / 'summary.json').read_text())['controls']
            self.assertEqual([row['status'] for row in rows], ['INVALID', 'KILLED'])
            self.assertIn('Node v22.22.0', rows[0]['reason'])

    def test_summary_is_complete_and_atomic(self):
        import time
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)
            with patch.object(Path, 'replace', autospec=True, wraps=Path.replace) as publish:
                runner.write_summary(out, [{'status': 'INVALID'}], time.monotonic(), 1.0)
                publish.assert_called_once_with(out / 'summary.json.tmp', out / 'summary.json')
            runner.write_summary(out, [{'status': 'KILLED'}, {'status': 'INVALID'}], time.monotonic(), 1.0)
            self.assertEqual(json.loads((out / 'summary.json').read_text())['counts'],
                             {'KILLED': 1, 'SURVIVED': 0, 'INVALID': 1})
            self.assertFalse((out / 'summary.json.tmp').exists())


if __name__ == '__main__':
    unittest.main()

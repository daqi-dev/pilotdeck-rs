#!/usr/bin/env python3
# anchor-selfcheck: unsupported tests of the shared switch executor, no product source mutations
"""#996 failure-path tests; no cargo or product source writes."""
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
import mutant_switch as shared


def result(test, state='ok', anchor='', file='pilotdeck-gateway/src/fixture.rs'):
    return SimpleNamespace(returncode=0 if state == 'ok' else 101,
        stdout=f'test {test} ... {state}\n'
               + ('test result: ok. 1 passed;' if state == 'ok' else 'test result: FAILED'),
        stderr='' if state == 'ok' else f'panicked at {file}:1:1:\n{anchor}')


def binding(name, witness=None, path=None):
    return dict(name=name, test=name, anchor=f'#996 {name}', witness=witness,
                path=path or Path('/fixture.rs'))


class MutantSwitchTests(unittest.TestCase):
    def test_environments_never_inherit_stale_protocols(self):
        stale = {shared.SELECTOR: 'bad', 'PDRS_FACADE_ENCODER_MUTANT': 'bad',
                 'PDRS_NEGATIVE_RECEIPTS': 'bad', 'PDRS_NEGATIVE_PARENT_TEST': 'bad',
                 'KEEP': 'yes'}
        self.assertEqual(shared.clean_environment(stale), {'KEEP': 'yes'})
        self.assertEqual(shared.selector_environment(stale, 'sample'),
                         {'KEEP': 'yes', shared.SELECTOR: 'sample'})
        # Control phases must run without any selector at all.
        self.assertNotIn(shared.SELECTOR, shared.selector_environment(stale, None))

    def test_raw_timeout_decodes_partial_output(self):
        error = subprocess.TimeoutExpired(['fixture'], 1)
        error.stdout, error.stderr = b'partial stdout ', 'partial stderr '
        self.assertEqual(shared.raw_timeout(error),
                         'partial stdout partial stderr \nTIMEOUT: Command '
                         "'['fixture']' timed out after 1 seconds\n")

    def test_selection_must_be_known_and_unique(self):
        rows = [binding('first'), binding('second')]
        self.assertEqual(shared.selected(rows, []), rows)
        self.assertEqual([row['name'] for row in shared.selected(rows, ['second'])], ['second'])
        for request in (['missing'], ['first', 'first']):
            with self.assertRaises(ValueError):
                shared.selected(rows, request)

    def test_preflight_requires_unique_switch_and_bindings(self):
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / 'source.rs'
            row = binding('sample', path=source)
            for content in ('missing switch', 'mutants::on("sample") twice mutants::on("sample")'):
                source.write_text(content)
                with self.assertRaises(ValueError):
                    shared.preflight([row])
            source.write_text('crate::mutants::on("sample")')
            shared.preflight([row])
            row['anchor'] = ['#996 first outcome', '#996 second outcome']
            shared.preflight([row])
            row['anchor'] = []
            with self.assertRaises(ValueError):
                shared.preflight([row])
            row['anchor'] = 'bound'
            with self.assertRaises(ValueError):
                shared.preflight([row, dict(row)])
            del row['anchor']
            with self.assertRaises(ValueError):
                shared.preflight([row])

    def test_exact_cargo_artifact_required(self):
        with tempfile.TemporaryDirectory() as temp, patch.dict(os.environ, {'CARGO_TARGET_DIR': temp}):
            binary = Path(temp) / 'test-binary'
            binary.write_text('fixture')
            item = dict(reason='compiler-artifact', target={'name': 'pilotdeck_gateway', 'kind': ['lib']},
                        profile={'test': True}, features=['pdrs-mutants'], executable=str(binary))
            def build(data, code=0):
                return SimpleNamespace(returncode=code, stdout=json.dumps(data), stderr='')
            self.assertEqual(shared.artifact(build(item)), binary.resolve())
            for bad in (dict(item, features=[]), dict(item, profile={'test': False}),
                        dict(item, target={'name': 'other', 'kind': ['lib']}),
                        dict(item, executable='/missing')):
                with self.assertRaises(RuntimeError):
                    shared.artifact(build(bad))
            with self.assertRaises(RuntimeError):
                shared.artifact(build(item, 101))

    def test_summary_counts_are_complete_and_atomic(self):
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)
            with patch.object(Path, 'replace', autospec=True, wraps=Path.replace) as publish:
                shared.publish_summary(out, [{'status': 'INVALID'}], 0.0, 1.0)
                publish.assert_called_once_with(out / 'summary.json.tmp', out / 'summary.json')
            shared.publish_summary(out, [{'status': 'KILLED'}, {'status': 'INVALID'}], 0.0, 1.0)
            document = json.loads((out / 'summary.json').read_text())
            self.assertEqual(document['counts'], {'KILLED': 1, 'SURVIVED': 0, 'INVALID': 1})
            self.assertIn('total_seconds', document['timing'])
            self.assertFalse((out / 'summary.json.tmp').exists())

    def test_complete_suite_and_restore_after_failure(self):
        rows = [binding('first'), binding('second')]
        calls = []
        def invoke(label, test, selector, row):
            calls.append((label, selector))
            if selector == 'first':
                raise subprocess.TimeoutExpired(['test'], 1)
            return result(test, 'FAILED' if selector else 'ok', row['anchor'])
        produced = list(shared.execute_suite(rows, invoke))
        self.assertEqual([row['status'] for row in produced], ['INVALID', 'KILLED'])
        self.assertIn(('first-restored', None), calls)
        self.assertIn(('second-negative', 'second'), calls)

    def test_positive_witness_and_restore_fail_closed(self):
        for failed in ('sample-positive', 'sample-restored', 'sample-witness-positive',
                      'sample-witness-negative'):
            with self.subTest(failed=failed):
                def invoke(label, test, selector, row):
                    return result(test, 'FAILED' if label == failed or (selector and test == 'sample')
                                  else 'ok', row['anchor'])
                produced = list(shared.execute_suite([binding('sample', 'witness')], invoke))
                self.assertEqual(produced[0]['status'], 'INVALID')

    def test_default_verdict_binds_named_test_and_panic(self):
        row = binding('sample')
        for response, expected in (
            (result('sample'), 'SURVIVED'),
            (result('other', 'FAILED', row['anchor']), 'INVALID'),
            (result('sample', 'FAILED', 'unrelated'), 'INVALID'),
            (SimpleNamespace(returncode=101, stdout='could not compile', stderr=''), 'INVALID'),
            (result('sample', 'FAILED', row['anchor']), 'KILLED'),
        ):
            with self.subTest(expected=expected):
                def invoke(label, test, selector, _row):
                    return response if selector else result(test)
                self.assertEqual(
                    list(shared.execute_suite([row], invoke))[0]['status'], expected)

    def test_verdict_and_passed_use_the_prefixed_test(self):
        seen = []
        def verdict(value, bound):
            seen.append(bound['test'])
            return 'KILLED', 'fixture'
        def invoke(label, test, selector, row):
            self.assertIn(test, ('sample', 'witness'))
            # Only the bound test fails under mutation; the witness stays green.
            return result('prefixed-' + test,
                          'FAILED' if selector and test == 'sample' else 'ok', row['anchor'])
        produced = list(shared.execute_suite([binding('sample', 'witness')], invoke,
                                             verdict=verdict, prefix='prefixed-'))
        self.assertEqual(produced[0]['status'], 'KILLED')
        self.assertEqual(seen, ['prefixed-sample'])

    def test_run_end_to_end_without_source_writes(self):
        rows = [binding('first'), binding('second')]
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp) / 'out'
            out.mkdir()
            guarded = [Path(temp) / 'guarded.rs']
            guarded[0].write_text('fixture\n')
            def process(command, **kwargs):
                if command[:2] == ['git', 'rev-parse']:
                    return SimpleNamespace(returncode=0, stdout='test-head\n', stderr='')
                if command[0] == 'cargo':
                    return SimpleNamespace(returncode=0, stdout='build fixture', stderr='')
                selector = kwargs['env'].get(shared.SELECTOR)
                self.assertIn('--exact', command)
                self.assertTrue(command[0].endswith('test-binary') or Path(command[0]).is_file())
                return result(command[1], 'FAILED' if selector else 'ok',
                              '#996 ' + command[1], 'fixture.rs')
            binary = Path(temp) / 'test-binary'
            binary.write_bytes(b'fixture binary\n')
            with patch.object(shared, 'run_owned', side_effect=process), \
                 patch.object(shared, 'wait_for_slot',
                              return_value=SimpleNamespace(stdout='', stderr='')), \
                 patch.object(shared, 'artifact', return_value=binary):
                self.assertEqual(shared.run(Path(temp), out, rows, {}, guarded=guarded), 0)
            document = json.loads((out / 'summary.json').read_text())
            self.assertEqual(document['counts'], {'KILLED': 2, 'SURVIVED': 0, 'INVALID': 0})
            self.assertEqual([row['name'] for row in document['controls']], ['first', 'second'])
            self.assertEqual(json.loads((out / 'source.json').read_text())['head'], 'test-head')
            self.assertIn('not a claim of committed HEAD equivalence',
                          (out / 'source.json').read_text())
            self.assertEqual(guarded[0].read_text(), 'fixture\n')
            self.assertTrue((out / 'first-negative.json').exists())

    def test_run_marks_remaining_rows_when_precompile_fails(self):
        rows = [binding('first'), binding('second')]
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp) / 'out'
            out.mkdir()
            def process(command, **kwargs):
                if command[:2] == ['git', 'rev-parse']:
                    return SimpleNamespace(returncode=0, stdout='test-head\n', stderr='')
                raise RuntimeError('slots exhausted')
            with patch.object(shared, 'run_owned', side_effect=process), \
                 patch.object(shared, 'wait_for_slot',
                              return_value=SimpleNamespace(stdout='', stderr='')):
                self.assertEqual(shared.run(Path(temp), out, rows, {}, guarded=[]), 1)
            document = json.loads((out / 'summary.json').read_text())
            self.assertEqual(document['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 2})
            self.assertTrue(all('runner:' in row['reason'] for row in document['controls']))

    def test_denominator_drift_fails_the_whole_suite(self):
        # #996 fix-r4 review P1: the static whitelist and the import-time audit
        # both stop at module scope, so a main() that shrinks (or grows) the
        # table after those gates used to run a different suite than the
        # declared one. The run now finishes with the checker's own extraction
        # as the denominator: any drift marks every row INVALID with the
        # difference and exits nonzero.
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            fixtures = root / 'src/pilotdeck-gateway/tests/fixtures/sample'
            fixtures.mkdir(parents=True)
            source = root / 'src/example.rs'
            source.write_text('crate::mutants::on("first")\ncrate::mutants::on("second")\n')
            script = fixtures / 'schemata.py'
            script.write_text(
                'from pathlib import Path\n'
                'root = Path(__file__).resolve().parents[5]\n'
                'source = root / "src/example.rs"\n'
                "switches = [('first', source, 'first_test', '#996 first'),"
                " ('second', source, 'second_test', '#996 second')]\n")
            binary = Path(temp) / 'test-binary'
            binary.write_bytes(b'fixture binary\n')

            def process(command, **kwargs):
                if command[:2] == ['git', 'rev-parse']:
                    return SimpleNamespace(returncode=0, stdout='test-head\n', stderr='')
                selector = kwargs['env'].get(shared.SELECTOR)
                return result(command[1], 'FAILED' if selector else 'ok',
                              '#996 ' + command[1], 'fixture.rs')

            def suite(rows, **kwargs):
                out = Path(temp) / kwargs.pop('label')
                out.mkdir()
                with patch.object(shared, 'run_owned', side_effect=process), \
                     patch.object(shared, 'wait_for_slot',
                                  return_value=SimpleNamespace(stdout='', stderr='')), \
                     patch.object(shared, 'artifact', return_value=binary):
                    code = shared.run(root, out, rows, {}, guarded=[], script=script,
                                      **kwargs)
                return code, json.loads((out / 'summary.json').read_text())

            # A main() that drops a declared row is reported whole-suite.
            code, document = suite([binding('first')], label='missing')
            self.assertEqual(code, 1, '#996 r4 a dropped declared switch must '
                                      'fail the whole suite')
            self.assertEqual(document['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                             '#996 r4 the drifted run must count every row INVALID')
            reason = document['controls'][0]['reason']
            self.assertIn('switch denominator drift', reason,
                          '#996 r4 the drifted row must name the denominator check')
            self.assertIn("missing=['second']", reason,
                          '#996 r4 the drift must list the missing declared switch')
            # A main() that adds an undeclared row is reported the same way.
            code, document = suite([binding('first'), binding('second'), binding('forged')],
                                   label='extra')
            self.assertEqual(code, 1, '#996 r4 an undeclared executed switch must '
                                      'fail the whole suite')
            self.assertIn("extra=['forged']",
                          document['controls'][0]['reason'],
                          '#996 r4 the drift must list the undeclared switch')
            # Without drift, and for an explicit selection, nothing is reported.
            code, document = suite([binding('first'), binding('second')], label='clean')
            self.assertEqual(code, 0, '#996 r4 a matching denominator must keep the suite green')
            self.assertEqual(document['counts'], {'KILLED': 2, 'SURVIVED': 0, 'INVALID': 0},
                             '#996 r4 a matching denominator must keep both rows KILLED')
            code, document = suite([binding('first')], requested=['first'], label='subset')
            self.assertEqual(code, 0, '#996 r4 an explicit selection is its own denominator')
            self.assertEqual(document['counts'], {'KILLED': 1, 'SURVIVED': 0, 'INVALID': 0},
                             '#996 r4 an explicit selection must stay KILLED')
            # A selection beyond the declared table cannot hide behind requested.
            code, document = suite([binding('forged')], requested=['forged'], label='beyond')
            self.assertEqual(code, 1, '#996 r4 a requested name outside the declaration '
                                      'must fail the whole suite')
            self.assertIn("extra=['forged']", document['controls'][0]['reason'],
                          '#996 r4 the beyond-declaration drift must list the switch')

    def test_declared_denominator_covers_legacy_tables(self):
        # #996 fix-r5: entries whose declaration is a legacy mutation table
        # (the edgeclaw-service-facade runner and boundary control) supply the
        # checker's static extraction as the declared denominator; drift then
        # invalidates the whole suite through the same shared check.
        rows = [binding('first'), binding('second')]
        drift = shared.denominator_drift(Path(tempfile.gettempdir()), None, rows, (),
                                         declared={'first', 'second', 'dropped'})
        self.assertIn("missing=['dropped']", drift,
                      '#996 r5 a legacy-table drop must be reported as drift')
        self.assertIsNone(shared.denominator_drift(Path(tempfile.gettempdir()), None, rows,
                                                   ['first'], declared={'first', 'second'}),
                          '#996 r5 an explicit selection stays its own denominator')
        enforced = [dict(binding('first'), status='KILLED', reason='')]
        with tempfile.TemporaryDirectory() as temp, patch('sys.stderr') as quiet:
            quiet.write = lambda *_: None
            self.assertTrue(shared.enforce_denominator(Path(temp), None, enforced, (),
                                                        declared={'second'}),
                            '#996 r5 drift must be returned to the caller')
        self.assertEqual([row['status'] for row in enforced], ['INVALID'],
                         '#996 r5 drift must invalidate every executed row')
        self.assertIn('switch denominator drift', enforced[0]['reason'],
                      '#996 r5 the invalidated row must name the check')


if __name__ == '__main__':
    unittest.main()

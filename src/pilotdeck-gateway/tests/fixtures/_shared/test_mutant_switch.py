#!/usr/bin/env python3
# anchor-selfcheck: unsupported tests of the shared switch executor, no product source mutations
"""#996 failure-path tests; no cargo or product source writes.

The #1037 tests prove the real entrypoint and the shared executor revoke every
already completed row when an exit check fails; ``--negative-controls`` no-ops
the shared invalidation in memory and requires those tests to die.  The
#996-pr2 fix-r2 tests prove every turn-family entry passes its declaring
script and the requested selection to the shared run, so the executor-side
denominator check sees a runtime-shrunk or emptied table.
"""
import contextlib
import importlib.util
import io
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

FIXTURES = Path(__file__).resolve().parents[1]


def load_entry(fixture='turn-runner-tail'):
    """Load a real schemata entry, as a runner would."""
    spec = importlib.util.spec_from_file_location(
        fixture.replace('-', '_') + '_schemata', FIXTURES / fixture / 'schemata.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


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

    def test_list_tests_run_every_cell_and_align_anchors(self):
        row = dict(name='cells', test=['first_cell', 'second_cell'],
                   anchor=['#996 first_cell anchor', '#996 second_cell anchor'],
                   path=Path('/fixture.rs'))
        seen = []
        judged = []

        def verdict(value, bound):
            judged.append(bound['anchor'])
            return shared.classify(value, SimpleNamespace(
                test=bound['test'], assertion=bound['anchor']))

        def invoke(label, test, selector, bound):
            seen.append((label, test, selector))
            if selector is None:
                return result(test)
            return result(test, 'FAILED', f'#996 {test} anchor')

        produced = list(shared.execute_suite([row], invoke, verdict=verdict))
        self.assertEqual(produced[0]['status'], 'KILLED')
        self.assertEqual(produced[0]['reason'], 'bound test failed at its assertion')
        # positive + restored phases run both cells without a selector
        self.assertEqual([entry[1] for entry in seen if entry[2] is None],
                         ['first_cell', 'second_cell', 'first_cell', 'second_cell'])
        # negatives use indexed labels and the verdict sees one aligned anchor
        # per cell, so no cell is judged by another cell's anchor.
        negatives = [entry for entry in seen if entry[2] == 'cells']
        self.assertEqual([entry[0] for entry in negatives],
                         ['cells-negative-1of2', 'cells-negative-2of2'])
        self.assertEqual(judged, ['#996 first_cell anchor', '#996 second_cell anchor'])

    def test_list_tests_fail_closed_when_one_cell_survives(self):
        row = dict(name='cells', test=['first_cell', 'second_cell'],
                   anchor=['#996 first_cell anchor', '#996 second_cell anchor'],
                   path=Path('/fixture.rs'))

        def invoke(label, test, selector, bound):
            if selector is None:
                return result(test)
            state = 'FAILED' if test == 'first_cell' else 'ok'
            return result(test, state, f'#996 {test} anchor')

        produced = list(shared.execute_suite([row], invoke))
        self.assertEqual(produced[0]['status'], 'SURVIVED')
        self.assertIn('second_cell', produced[0]['reason'])

    def test_preflight_requires_aligned_anchors_for_list_tests(self):
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / 'source.rs'
            source.write_text('crate::mutants::on("cells")')
            row = dict(name='cells', test=['first_cell', 'second_cell'],
                       anchor='#996 single anchor', path=source)
            with self.assertRaises(ValueError):
                shared.preflight([row])
            row['anchor'] = ['#996 first', '#996 second']
            shared.preflight([row])

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

    def test_run_invalidates_completed_kills_when_exit_checks_fail(self):
        # #1037: this executor never writes sources, so the mutating guard's
        # restore/verify exit has two analogs that run after every row:
        # verify_unchanged and the denominator check. When either raises after
        # all rows already completed KILLED, the shared invalidate_completed
        # (#979) must revoke them — an all-KILLED summary with exit 0 would
        # publish a false pass.
        for fault in ('verify', 'denominator'):
            with tempfile.TemporaryDirectory() as temp:
                # No subTest: a failure must stay attributable to this method's
                # name for the negative control that no-ops the invalidation.
                rows = [binding('first'), binding('second')]
                out = Path(temp) / 'out'
                out.mkdir()
                guarded = [Path(temp) / 'guarded.rs']
                guarded[0].write_text('fixture\n')
                binary = Path(temp) / 'test-binary'
                binary.write_bytes(b'fixture binary\n')

                def process(command, **kwargs):
                    if command[:2] == ['git', 'rev-parse']:
                        return SimpleNamespace(returncode=0, stdout='test-head\n', stderr='')
                    if command[0] == 'cargo':
                        return SimpleNamespace(returncode=0, stdout='build fixture', stderr='')
                    selector = kwargs['env'].get(shared.SELECTOR)
                    return result(command[1], 'FAILED' if selector else 'ok',
                                  '#996 ' + command[1], 'fixture.rs')

                with patch.object(shared, 'run_owned', side_effect=process), \
                        patch.object(shared, 'wait_for_slot',
                                     return_value=SimpleNamespace(stdout='', stderr='')), \
                        patch.object(shared, 'artifact', return_value=binary), \
                        patch.object(shared, 'verify_unchanged' if fault == 'verify'
                                     else 'enforce_denominator',
                                     side_effect=RuntimeError('#1037 injected exit failure')):
                    code = shared.run(Path(temp), out, rows, {}, guarded=guarded,
                                      script=Path(temp) / 'schemata.py'
                                      if fault == 'denominator' else None)
                self.assertEqual(code, 1,
                                 '#1037 a failing exit check must exit nonzero')
                document = json.loads((out / 'summary.json').read_text())
                self.assertEqual(document['counts'],
                                 {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 2},
                                 '#1037 a failing exit check must revoke every '
                                 'completed kill')
                for row in document['controls']:
                    self.assertEqual(row['status'], 'INVALID',
                                     '#1037 every completed row must be invalidated')
                    self.assertTrue(row['reason'].startswith(
                        'bound test failed at its assertion; runner: RuntimeError: '
                        '#1037 injected exit failure'),
                        '#1037 invalidation must preserve the completed diagnostic')

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

    def test_denominator_drift_on_an_emptied_table(self):
        # #996 fix-r6 review P0: a table emptied whole at runtime leaves no
        # executed row, and any() over an empty row list reads False, so the
        # drift used to reach neither the exit code nor the summary of the
        # callers without a ``not rows`` guard. The shared check must keep a
        # suite-level INVALID row for every entrance.
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
                return result(command[1])

            out = Path(temp) / 'out'
            out.mkdir()
            with patch.object(shared, 'run_owned', side_effect=process), \
                    patch.object(shared, 'wait_for_slot',
                                 return_value=SimpleNamespace(stdout='', stderr='')), \
                    patch.object(shared, 'artifact', return_value=binary), \
                    patch('sys.stderr') as quiet:
                quiet.write = lambda *_: None
                code = shared.run(root, out, [], {}, guarded=[], script=script)
            self.assertNotEqual(code, 0, '#996 r6 a runtime table emptied whole '
                                          'must fail the whole run')
            document = json.loads((out / 'summary.json').read_text())
            self.assertEqual(document['counts'],
                             {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                             '#996 r6 the emptied run must keep a suite-level INVALID row')
            reason = document['controls'][0]['reason']
            self.assertIn('switch denominator drift', reason,
                          '#996 r6 the suite-level row must name the denominator check')
            self.assertIn("missing=['first', 'second']", reason,
                          '#996 r6 the drift must list every declared switch')

            # The same shape at the shared check itself: an empty executed
            # table is never a green run, with or without entry-side guards.
            enforced = []
            with patch('sys.stderr') as quiet:
                quiet.write = lambda *_: None
                drift = shared.enforce_denominator(root, None, enforced, (),
                                                   declared={'first'})
            self.assertTrue(drift, '#996 r6 drift must still be returned to the caller')
            self.assertEqual([row['status'] for row in enforced], ['INVALID'],
                             '#996 r6 an empty executed table must keep a '
                             'suite-level INVALID row')
            self.assertIn('switch denominator drift', enforced[0]['reason'],
                          '#996 r6 the suite-level row must name the check')

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


class SchemataEntryTests(unittest.TestCase):
    """#1037: the real turn-runner-tail entry must revoke its kills on failure."""

    @classmethod
    def setUpClass(cls):
        cls.entry = load_entry()

    def test_schemata_entry_invalidates_completed_kills(self):
        # #1037: exercise the real entrypoint end to end (argparse, preflight,
        # the shared run) with every owned process replaced: all declared rows
        # complete KILLED, then the exit verification fails. The published
        # summary must show only INVALID rows and main() must return nonzero;
        # the revocation goes through the shared negative_suite.
        # invalidate_completed (#979), never a second copy.
        entry = self.entry
        module = entry.mutant_switch
        # One synthetic panic containing every declared anchor: the default
        # verdict must still bind each row to its own assertion.
        anchors = ' '.join(row[3] for row in entry.switches)
        selectors = []
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp) / 'out'
            binary = Path(temp) / 'test-binary'
            binary.write_bytes(b'fixture binary\n')

            def process(command, **kwargs):
                if command[:2] == ['git', 'rev-parse']:
                    return SimpleNamespace(returncode=0, stdout='test-head\n', stderr='')
                if command[0] == 'cargo':
                    return SimpleNamespace(returncode=0, stdout='build fixture', stderr='')
                selector = kwargs['env'].get(module.SELECTOR)
                selectors.append(selector)
                return result(command[1], 'FAILED' if selector else 'ok', anchors,
                              'fixture.rs')

            with patch.object(module, 'run_owned', side_effect=process), \
                    patch.object(module, 'wait_for_slot',
                                 return_value=SimpleNamespace(stdout='', stderr='')), \
                    patch.object(module, 'artifact', return_value=binary), \
                    patch.object(module, 'verify_unchanged',
                                 side_effect=RuntimeError(
                                     '#1037 injected verification failure')), \
                    patch.dict(os.environ, {'CARGO_TARGET_DIR': str(Path(temp) / 'unused')}), \
                    patch.object(sys, 'argv', ['schemata.py', str(out)]):
                code = entry.main()
            self.assertEqual(code, 1, '#1037 the real entry must exit nonzero '
                                       'when the exit verification fails')
            document = json.loads((out / 'summary.json').read_text())
            self.assertEqual(len(document['controls']), len(entry.switches),
                             '#1037 every declared switch must stay accounted for')
            self.assertEqual(document['counts']['KILLED'], 0,
                             '#1037 no completed kill may stay green after the '
                             'exit failure')
            self.assertEqual(document['counts']['INVALID'], len(entry.switches),
                             '#1037 every completed kill must be revoked as INVALID')
            for row in document['controls']:
                self.assertIn('#1037 injected verification failure', row['reason'],
                             '#1037 every row must retain the injected exit failure')
            self.assertEqual(selectors.count(None), 2 * len(entry.switches),
                             '#1037 positive and restored phases must really '
                             'complete for every row')
            self.assertEqual(len(selectors), 3 * len(entry.switches),
                             '#1037 the suite must really complete before the '
                             'exit check')


class SchemataDenominatorTests(unittest.TestCase):
    """#996-pr2 fix-r2: every turn-family entry wires the denominator check.

    The five entries whose negative controls became compile-time switches in
    this PR called ``mutant_switch.run`` without ``script`` / ``requested``,
    so the shared executor skipped the fix-r4 denominator check: a ``main()``
    the static audit cannot see could shrink the executed table and still
    publish rc=0.  Each entry must therefore finish its run through the real
    ``enforce_denominator`` with its own declaring script.
    """

    ENTRIES = ('turn-runner-tail', 'turn-gates', 'turn-prompt-hooks',
               'turn-start-notices', 'tool-budget-cwd')

    @classmethod
    def setUpClass(cls):
        cls.entries = [load_entry(name) for name in cls.ENTRIES]

    def entry_run(self, entry, argv, shrink=None):
        """Run the real main() with every owned process replaced.

        Every declared row completes KILLED (one synthetic panic carries all
        anchors), exactly like the review's memory-only reproduction; the
        exit code and the published summary are the witnesses.  ``shrink``
        rewrites the runtime selection, the shape of drift the import-time
        audit cannot see.
        """
        module = entry.mutant_switch
        # All-cells rows bind a list of anchors; one synthetic panic still
        # carries every one of them.
        anchors = ' '.join(' '.join(row[3]) if isinstance(row[3], list) else row[3]
                           for row in entry.switches)
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp) / 'out'
            binary = Path(temp) / 'test-binary'
            binary.write_bytes(b'fixture binary\n')

            def process(command, **kwargs):
                if command[:2] == ['git', 'rev-parse']:
                    return SimpleNamespace(returncode=0, stdout='test-head\n', stderr='')
                if command[0] == 'cargo':
                    return SimpleNamespace(returncode=0, stdout='build fixture', stderr='')
                selector = kwargs['env'].get(module.SELECTOR)
                return result(command[1], 'FAILED' if selector else 'ok', anchors,
                              'fixture.rs')

            selected, patches = module.selected, [
                patch.object(module, 'run_owned', side_effect=process),
                patch.object(module, 'wait_for_slot',
                             return_value=SimpleNamespace(stdout='', stderr='')),
                patch.object(module, 'artifact', return_value=binary)]
            if shrink is not None:
                def shrunk(rows, names, _selected=selected, _shrink=shrink):
                    return _shrink(_selected(rows, names))
                patches.append(patch.object(module, 'selected', side_effect=shrunk))
            with contextlib.ExitStack() as stack:
                for item in patches:
                    stack.enter_context(item)
                stack.enter_context(patch.dict(
                    os.environ, {'CARGO_TARGET_DIR': str(Path(temp) / 'unused')}))
                stack.enter_context(patch.object(
                    sys, 'argv', ['schemata.py', str(out), *argv]))
                stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
                stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
                code = entry.main()
                document = json.loads((out / 'summary.json').read_text())
        return code, document

    def test_entries_report_runtime_shrunk_tables(self):
        # A main() that shrinks the executed table to one switch must lose the
        # whole suite: nonzero exit, the executed row INVALID with the drift
        # difference, and the last declared switch (tail-regeneration for the
        # tail entry) named as missing.  No subTest: a failure must stay
        # attributable to this method's name for the negative control.
        for entry in self.entries:
            name = Path(entry.__file__).parent.name
            code, document = self.entry_run(entry, [], shrink=lambda rows: rows[:1])
            self.assertNotEqual(
                code, 0, f'#996-pr2 {name}: a runtime-shrunk switch table '
                         'must exit nonzero')
            self.assertEqual(
                document['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                f'#996-pr2 {name}: the drifted run must count the executed '
                'row INVALID')
            reason = document['controls'][0]['reason']
            self.assertIn('switch denominator drift', reason,
                          f'#996-pr2 {name}: the row must name the denominator check')
            self.assertIn(f"'{entry.switches[-1][0]}'", reason,
                          f'#996-pr2 {name}: the drift must list the last '
                          'declared switch as missing')

    def test_entries_report_emptied_tables(self):
        # A main() that empties the executed table whole leaves no row to
        # invalidate; the entry must still publish one suite-level
        # switch-denominator INVALID row and exit nonzero.
        for entry in self.entries:
            name = Path(entry.__file__).parent.name
            code, document = self.entry_run(entry, [], shrink=lambda rows: [])
            self.assertNotEqual(
                code, 0, f'#996-pr2 {name}: a runtime-emptied switch table '
                         'must exit nonzero')
            self.assertEqual(
                document['counts'], {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                f'#996-pr2 {name}: an emptied table must keep a suite-level '
                'INVALID row')
            self.assertEqual(
                document['controls'][0]['name'], 'switch-denominator',
                f'#996-pr2 {name}: the suite-level row must be the '
                'denominator check')
            self.assertIn(f"'{entry.switches[-1][0]}'",
                          document['controls'][0]['reason'],
                          f'#996-pr2 {name}: the drift must list every '
                          'declared switch')

    def test_entries_explicit_subset_stays_green(self):
        # The fix must not overreach: an explicitly requested legal subset is
        # its own denominator and must keep the suite green.
        for entry in self.entries:
            name = Path(entry.__file__).parent.name
            code, document = self.entry_run(entry, [entry.switches[0][0]])
            self.assertEqual(
                code, 0, f'#996-pr2 {name}: an explicit legal subset must '
                         'stay green')
            self.assertEqual(
                document['counts'], {'KILLED': 1, 'SURVIVED': 0, 'INVALID': 0},
                f'#996-pr2 {name}: the requested switch must stay KILLED')


def negative_controls():
    """No-op the shared exit checks in memory, then every guarded test must
    die at its named assertion (no source edits, no cargo).

    #1037: ``invalidate_completed`` no-opped must fail the exit-failure tests
    at their ``#1037`` assertions.  #996-pr2 fix-r2: ``enforce_denominator``
    no-opped (the in-memory shape of an entry that stops passing ``script`` /
    ``requested``, so the shared run skips the check) must fail the drift
    tests at their ``#996-pr2`` assertions.
    """
    def noop_invalidation(module):
        return patch.object(module, 'invalidate_completed', new=lambda *_a, **_k: None)

    def noop_denominator(module):
        return patch.object(module, 'enforce_denominator', new=lambda *_a, **_k: None)

    def bound_module(test_class):
        """The switch module whose exit check the mutation no-ops."""
        for attribute in ('entry', 'entries'):
            if attribute in test_class.__dict__:
                value = test_class.__dict__[attribute]
                return (value[0] if isinstance(value, list) else value).mutant_switch
        if test_class is not MutantSwitchTests:
            test_class.setUpClass()
            return bound_module(test_class)
        return shared

    targets = (
        (MutantSwitchTests, 'test_run_invalidates_completed_kills_when_exit_checks_fail',
         'invalidation-noop', noop_invalidation, '#1037'),
        (SchemataEntryTests, 'test_schemata_entry_invalidates_completed_kills',
         'invalidation-noop', noop_invalidation, '#1037'),
        (SchemataDenominatorTests, 'test_entries_report_runtime_shrunk_tables',
         'denominator-noop', noop_denominator, '#996-pr2'),
        (SchemataDenominatorTests, 'test_entries_report_emptied_tables',
         'denominator-noop', noop_denominator, '#996-pr2'),
    )
    results = []
    for test_class, method, mutation, patcher, marker in targets:
        label = method + '-' + mutation
        try:
            with patcher(bound_module(test_class)):
                stream = io.StringIO()
                outcome = unittest.TextTestRunner(stream=stream, verbosity=2).run(
                    unittest.defaultTestLoader.loadTestsFromName(method, test_class))
            print(stream.getvalue(), end='')
            observed = {test._testMethodName for test, _ in
                        outcome.failures + outcome.errors}
            traces = [trace for _, trace in outcome.failures + outcome.errors]
            killed = (outcome.testsRun > 0 and not outcome.skipped
                      and observed == {method}
                      and all(marker in trace for trace in traces))
            results.append({'mutation': label,
                            'status': 'KILLED' if killed else 'SURVIVED'
                            if outcome.wasSuccessful() else 'INVALID',
                            'reason': f'observed failures={sorted(observed)}'})
        except Exception as error:
            results.append({'mutation': label, 'status': 'INVALID',
                            'reason': f'{type(error).__name__}: {error}'})
    print(json.dumps(results, indent=2))
    return int(any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    if sys.argv[1:] == ['--negative-controls']:
        raise SystemExit(negative_controls())
    if sys.argv[1:]:
        raise SystemExit('usage: test_mutant_switch.py [--negative-controls]')
    unittest.main()

#!/usr/bin/env python3
# anchor-selfcheck: unsupported in-memory unittest coverage; no source replacements
"""#1190 binding, read-only listing and evidence regressions.

The historical filename remains for callers; the document-scanning meta
control runner and its tests have been removed. Function variants below run
only in memory, compile before testing, and bind exact unittest diagnostics.
"""
import contextlib
import copy
import io
import inspect
import json
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import MagicMock, Mock, patch

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import negative_controls as gate


class DriftTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.expectations = gate.panel_table('EXPECTATION_CASES')
        cls.calibration = gate.panel_table('CALIBRATION_CASES')

    def test_binding_corruptions_are_rejected(self):
        state = 'drop-registry-state-require'
        row = next(row for row in gate.MUTATIONS if row[0] == state)
        scenarios = (
            ('deleted', f'BOUND names unknown mutation {state}'),
            ('empty', f'BOUND[{state}] is empty'),
            ('malformed', f'{state}: BOUND entry is not a (case, rule) pair'),
            ('non-sequence', f'BOUND[{state}] is not a sequence'),
            ('short-row', 'MUTATIONS row is not a (name, source, old, new) row'),
            ('duplicate', f'MUTATIONS names are not unique: {state}'),
        )
        for name, marker in scenarios:
            with self.subTest(name=name):
                require_rows, bound = copy.deepcopy(gate.MUTATIONS), copy.deepcopy(gate.BOUND)
                self.assertFalse(gate.check_bindings(self.expectations, require_rows, bound, gate.UNAFFECTED),
                                 '#1190 bindings: unchanged tables must pass')
                if name == 'deleted':
                    require_rows.remove(row)
                elif name == 'empty':
                    bound[state] = ()
                elif name == 'malformed':
                    bound[state] = (('expectations-rust-registry-state',),)
                elif name == 'non-sequence':
                    bound[state] = 42
                elif name == 'short-row':
                    require_rows[require_rows.index(row)] = row[:-1]
                else:
                    require_rows.append(row)
                errors = gate.check_bindings(self.expectations, require_rows, bound, gate.UNAFFECTED)
                self.assertTrue(any(error.startswith(marker) for error in errors),
                                '#1190 bindings: corruption must have its bound diagnostic')

    def test_wrong_rule_binding_is_rejected(self):
        state = 'drop-registry-state-require'
        for table in ('BOUND', 'UNAFFECTED'):
            with self.subTest(table=table):
                bound, unaffected = copy.deepcopy(gate.BOUND), copy.deepcopy(gate.UNAFFECTED)
                self.assertFalse(gate.check_bindings(self.expectations, gate.MUTATIONS, bound, unaffected),
                                 '#1190 wrong-rule: unchanged bindings must pass')
                case, rule = bound[state][0] if table == 'BOUND' else unaffected[state]
                wrong = rule + '_WRONG'
                if table == 'BOUND':
                    bound[state] = ((case, wrong),)
                else:
                    unaffected[state] = (case, wrong)
                errors = gate.check_bindings(self.expectations, gate.MUTATIONS, bound, unaffected)
                marker = f'{state}: {case} is claimed as {wrong} but registered as {rule}'
                self.assertTrue(marker in errors,
                                '#1190 wrong-rule: registered rule mismatch must be rejected')

    def test_list_reports_live_tables_and_counts(self):
        for extended in (False, True):
            with self.subTest(extended=extended):
                expectations, calibration = dict(self.expectations), dict(self.calibration)
                require_rows = list(gate.MUTATIONS)
                bound, unaffected = copy.deepcopy(gate.BOUND), copy.deepcopy(gate.UNAFFECTED)
                if extended:
                    expectations['listing-probe'] = 'EXPECTATION_LISTING_PROBE'
                    expectations['listing-derivation-probe'] = 'EXPECTATION_RUST_DERIVATION'
                    calibration['listing-probe'] = 'CALIBRATION_LISTING_PROBE'
                    require_rows.append(('listing-probe', gate.SESSION, 'old', 'new'))
                    bound['listing-probe'] = (
                        ('listing-probe', 'EXPECTATION_LISTING_PROBE'),
                        ('listing-derivation-probe', 'EXPECTATION_RUST_DERIVATION'),
                    )
                    unaffected['listing-probe'] = ('listing-derivation-probe',
                                                  'EXPECTATION_RUST_DERIVATION')
                tables = {'EXPECTATION_CASES': expectations, 'CALIBRATION_CASES': calibration}
                with patch.object(sys, 'argv', ['negative_controls.py', '--list']), \
                        patch.object(gate, 'panel_table', side_effect=tables.__getitem__), \
                        patch.object(gate, 'MUTATIONS', require_rows), \
                        patch.object(gate, 'BOUND', bound), patch.object(gate, 'UNAFFECTED', unaffected), \
                        contextlib.redirect_stdout(io.StringIO()) as output:
                    self.assertTrue(gate.main() == 0, '#1190 list: valid tables must pass')
                try:
                    listing = json.loads(output.getvalue())
                except ValueError as error:
                    self.fail(f'#1190 list: expected JSON: {error}')
                self.assertTrue(listing.get('EXPECTATION_CASES') == expectations,
                                '#1190 list: expectation names and rules must come from the live table')
                self.assertTrue(listing.get('CALIBRATION_CASES') == calibration,
                                '#1190 list: calibration names and rules must come from the live table')
                self.assertTrue(listing.get('MUTATIONS') == [row[0] for row in require_rows],
                                '#1190 list: mutation names must come from the live table')
                self.assertTrue(listing.get('counts', {}).get('derivationExpectationCases')
                                == (10 if extended else 9),
                                '#1190 list: derivation probe must raise count from 9 to 10')
                self.assertTrue(listing.get('counts') == {
                    'expectationCases': len(expectations),
                    'derivationExpectationCases': sum(rule == 'EXPECTATION_RUST_DERIVATION'
                                                     for rule in expectations.values()),
                    'calibrationCases': len(calibration), 'mutationRows': len(require_rows)},
                    '#1190 list: counts must be derived from the live tables')

    def test_list_rejects_invalid_only(self):
        known = gate.MUTATIONS[0][0]
        for names in (['bogus'], [known, known]):
            with self.subTest(names=names), \
                    patch.object(sys, 'argv', ['negative_controls.py', '--list'] +
                                 [arg for name in names for arg in ('--only', name)]), \
                    contextlib.redirect_stdout(io.StringIO()) as output, \
                    contextlib.redirect_stderr(io.StringIO()) as errors:
                self.assertTrue(gate.main() == 2,
                                '#1190 list-only: invalid selection must reject listing')
                self.assertTrue('unknown or duplicate --only name' in errors.getvalue()
                                and not output.getvalue(),
                                '#1190 list-only: diagnostic must precede listing')

    def test_list_runs_anchor_checks(self):
        selected = gate.MUTATIONS[:1]
        with patch.object(sys, 'argv', ['negative_controls.py', '--list', '--check-anchors',
                                      '--only', selected[0][0]]), \
                patch.object(gate, 'check_anchors', wraps=gate.check_anchors) as anchors, \
                contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertTrue(gate.main() == 0,
                            '#1190 list-anchors: valid anchors and listing must pass')
        self.assertTrue(anchors.call_count == 1 and anchors.call_args.args == (selected,),
                        '#1190 list-anchors: selected anchors must execute before listing')
        report, separator, payload = output.getvalue().partition('\n')
        self.assertTrue(report == '1 anchors: ok' and bool(separator),
                        '#1190 list-anchors: anchor report must precede listing')
        try:
            listing = json.loads(payload)
        except ValueError as error:
            self.fail(f'#1190 list-anchors: JSON listing must follow anchor report: {error}')
        self.assertTrue(listing.get('MUTATIONS') == [row[0] for row in gate.MUTATIONS],
                        '#1190 list-anchors: combined invocation must also list live tables')

    def test_list_rejects_failed_anchors(self):
        with patch.object(sys, 'argv', ['negative_controls.py', '--list', '--check-anchors']), \
                patch.object(gate, 'check_anchors', return_value=2) as anchors, \
                contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertTrue(gate.main() == 2,
                            '#1190 list-anchors: failed anchors must reject listing')
        self.assertTrue(anchors.call_count == 1 and not output.getvalue(),
                        '#1190 list-anchors: failed anchors must not print listing')

    def test_list_is_read_only(self):
        active = False

        def audit(event, args):
            if not active:
                return
            if event == 'open':
                _path, mode, flags = args
                writable = (isinstance(mode, str) and any(char in mode for char in 'wax+')) or (
                    isinstance(flags, int) and flags & (os.O_WRONLY | os.O_RDWR | os.O_CREAT |
                                                       os.O_TRUNC | os.O_APPEND))
                if not writable:
                    return
            elif event not in {'os.mkdir', 'os.rename', 'os.remove', 'os.rmdir',
                               'os.symlink', 'os.link', 'os.truncate', 'os.chmod',
                               'os.chown', 'os.utime', 'subprocess.Popen', 'os.system'}:
                return
            raise AssertionError(f'#1190 list: filesystem writes forbidden: {event}')

        sys.addaudithook(audit)
        with patch.object(sys, 'argv', ['negative_controls.py', '--list',
                                      '--output', '/unused', '--source', '/unused']), \
                contextlib.redirect_stdout(io.StringIO()):
            try:
                active = True
                self.assertTrue(gate.main() == 0, '#1190 list: read-only invocation must pass')
            finally:
                active = False

    def test_list_rejects_binding_drift(self):
        bound = copy.deepcopy(gate.BOUND)
        bound['drop-registry-state-require'] = (('expectations-rust-registry-state', 'WRONG'),)
        with patch.object(sys, 'argv', ['negative_controls.py', '--list']), \
                patch.object(gate, 'BOUND', bound), \
                contextlib.redirect_stdout(io.StringIO()) as output, \
                contextlib.redirect_stderr(io.StringIO()) as errors:
            self.assertTrue(gate.main() == 2, '#1190 list: binding drift must reject CLI')
        self.assertTrue('BINDING DRIFT:' in errors.getvalue() and not output.getvalue(),
                        '#1190 list: binding diagnostic must precede listing')

    def test_early_errors_have_fresh_evidence_and_continue(self):
        source = Mock()
        rows = [('first', source, 'absent', ''), ('second', source, 'pass', 'invalid !')]
        recorded = []
        with patch.object(sys, 'argv', ['negative_controls.py', '--output', '/unused', '--source', '/unused']), \
                patch.object(gate, 'MUTATIONS', rows), \
                patch.object(gate, 'BOUND', {name: (('case', 'rule'),) for name, *_ in rows}), \
                patch.object(gate, 'UNAFFECTED', {name: ('case', 'rule') for name, *_ in rows}), \
                patch.object(gate, 'check_bindings', return_value=[]), \
                patch.object(gate, 'new_output_directory', return_value=MagicMock()), \
                patch.object(gate, 'mutating_sources', return_value=contextlib.nullcontext({source: b'pass\n'})), \
                patch.object(gate, 'publish_summary', side_effect=lambda _out, data: recorded.extend(data)), \
                contextlib.redirect_stdout(io.StringIO()):
            try:
                code = gate.main()
            except Exception as error:
                self.fail(f'#1190 evidence: early errors escaped: {type(error).__name__}: {error}')
            self.assertTrue(code == 1, '#1190 evidence: early errors must stay row-local')
        self.assertTrue(len(recorded) == 2, '#1190 evidence: must continue after early error')
        expected = {'baseline': [], 'mutant': [], 'unaffected': None, 'restored': None}
        self.assertTrue(all(row['evidence'] == expected for row in recorded),
                        '#1190 evidence: early errors need initialized empty evidence')
        self.assertTrue(recorded[0]['evidence'] is not recorded[1]['evidence'],
                        '#1190 evidence: never carry previous row evidence')
        self.assertTrue('anchor expired' in recorded[0]['reason'] and 'SyntaxError' in recorded[1]['reason'],
                        '#1190 evidence: exercise anchor and compile failures')


def negative_checks():
    """Compile each function variant, run the bound test, restore, then summarize all."""
    variants = (
        ('skip-binding-checks', 'check_bindings', '    errors = []', '    return []',
         'test_binding_corruptions_are_rejected',
         '#1190 bindings: corruption must have its bound diagnostic'),
        ('skip-registered-rule', 'check_bindings', 'if registered != rule:', 'if False:',
         'test_wrong_rule_binding_is_rejected',
         '#1190 wrong-rule: registered rule mismatch must be rejected'),
        ('omit-expectation-list', 'main', "'EXPECTATION_CASES': expectation_cases",
         "'EXPECTATION_CASES': {}", 'test_list_reports_live_tables_and_counts',
         '#1190 list: expectation names and rules must come from the live table'),
        ('omit-calibration-list', 'main', "'CALIBRATION_CASES': calibration_cases",
         "'CALIBRATION_CASES': {}", 'test_list_reports_live_tables_and_counts',
         '#1190 list: calibration names and rules must come from the live table'),
        ('omit-mutation-list', 'main', "'MUTATIONS': [row[0] for row in MUTATIONS]",
         "'MUTATIONS': []", 'test_list_reports_live_tables_and_counts',
         '#1190 list: mutation names must come from the live table'),
        ('wrong-mutation-count', 'derived_counts', 'len(mutations)', 'len(mutations) + 1',
         'test_list_reports_live_tables_and_counts',
         '#1190 list: counts must be derived from the live tables'),
        ('constant-derivation-count', 'derived_counts',
         "sum(rule == 'EXPECTATION_RUST_DERIVATION'\n"
         "                    for rule in expectation_cases.values())", '9',
         'test_list_reports_live_tables_and_counts',
         '#1190 list: derivation probe must raise count from 9 to 10'),
        ('skip-only-validation', 'main',
         'if args.only and (set(args.only) - known or len(args.only) != len(set(args.only))):',
         'if False:', 'test_list_rejects_invalid_only',
         '#1190 list-only: invalid selection must reject listing'),
        ('skip-list-anchors', 'main', 'if args.check_anchors:',
         'if args.check_anchors and not args.list:', 'test_list_runs_anchor_checks',
         '#1190 list-anchors: selected anchors must execute before listing'),
        ('return-before-listing', 'main', 'if anchor_status != 0 or not args.list:',
         'if True:', 'test_list_runs_anchor_checks',
         '#1190 list-anchors: JSON listing must follow anchor report'),
        ('ignore-anchor-failure', 'main', 'if anchor_status != 0 or not args.list:',
         'if not args.list:', 'test_list_rejects_failed_anchors',
         '#1190 list-anchors: failed anchors must reject listing'),
        ('write-during-list', 'main', '    if args.list:',
         "    if args.list:\n        sys.audit('os.mkdir', '/unused', 511, -1)",
         'test_list_is_read_only', '#1190 list: filesystem writes forbidden'),
        ('skip-cli-bindings', 'main',
         'check_bindings(expectation_cases, MUTATIONS, BOUND, UNAFFECTED)', '[]',
         'test_list_rejects_binding_drift', '#1190 list: binding drift must reject CLI'),
        ('late-evidence', 'main',
         "                evidence = {'baseline': [], 'mutant': [], 'unaffected': None,\n"
         "                            'restored': None}\n", '',
         'test_early_errors_have_fresh_evidence_and_continue',
         '#1190 evidence: early errors must stay row-local'),
    )
    DriftTests.setUpClass()
    results = []
    for name, function, before, after, method, marker in variants:
        row = {'mutation': name, 'test': f'test_meta_controls.DriftTests.{method}',
               'assertion': marker, 'status': 'INVALID', 'compiled': False}
        try:
            module = gate
            text = inspect.getsource(getattr(module, function))
            if text.count(before) != 1:
                raise ValueError(f'#1190 {name}: stale in-memory anchor')
            namespace = {}
            exec(compile(text.replace(before, after, 1), f'<#1190 {name}>', 'exec'),
                 module.__dict__, namespace)
            changed = namespace[function]
            row['compiled'] = True
            positive = unittest.TestResult()
            DriftTests(method).run(positive)
            if not positive.wasSuccessful():
                raise ValueError('#1190 positive failed: ' + str(positive.errors + positive.failures))
            negative = unittest.TestResult()
            with patch.object(module, function, changed), contextlib.redirect_stdout(io.StringIO()):
                DriftTests(method).run(negative)
            failures = negative.errors + negative.failures
            row['raw'] = [detail for _test, detail in failures]
            if negative.wasSuccessful():
                row.update(status='SURVIVED', reason='named test passed under variant')
            elif (not negative.errors and negative.failures and
                  all(any('AssertionError' in line and marker in line for line in detail.splitlines())
                      for _test, detail in negative.failures)):
                row.update(status='KILLED', reason='named test failed with its exclusive assertion')
            else:
                row['reason'] = 'unrelated exception or missing bound assertion'
            restored = unittest.TestResult()
            DriftTests(method).run(restored)
            if not restored.wasSuccessful():
                raise ValueError('#1190 restored test failed')
        except Exception as error:
            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        results.append(row)
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print(json.dumps({'controls': results, 'counts': counts}, indent=2))
    return 0 if all(row['status'] == 'KILLED' for row in results) else 1


if __name__ == '__main__':
    if sys.argv[1:] == ['--negative']:
        raise SystemExit(negative_checks())
    unittest.main(verbosity=2)

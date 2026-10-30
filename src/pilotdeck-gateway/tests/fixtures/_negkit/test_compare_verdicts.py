#!/usr/bin/env python3
# anchor-selfcheck: unsupported comparator CLI tests, only temporary JSON inputs
"""Positive and negative inputs exercise the real CLI under python -I.

No product/source mutation, runner import, cargo invocation or HEAD bypass.
"""
import copy
import json
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import unittest
from types import SimpleNamespace


SCRIPT = Path(__file__).with_name('compare_verdicts.py')
TESTDATA = Path(__file__).with_name('testdata')
REAL_SUMMARIES = (
    'run-cases.json', 'matrix-main.json', 'negative-output-directory.json',
    'gateway-entry-guard.json', 'sdk-run.json',
)
SECTIONS = {
    'missing_cases': 'P0 MISSING CASES (case loss)',
    'status_changes': 'STATUS CHANGES',
    'test_changes': 'TEST BINDING CHANGES',
    'assertion_changes': 'ASSERTION CHANGES',
    'duplicate_keys': 'DUPLICATE CASE KEYS',
    'empty_tables': 'EMPTY TABLES',
}


def summary():
    return {'controls': [
        {'mutation': 'alpha', 'test': 'suite::alpha', 'assertion': '#1100 alpha',
         'status': 'KILLED', 'reason': 'bound assertion failed'},
        {'mutation': 'beta', 'test': 'suite::beta', 'assertion': ['#1100 beta', '#1100 fallback'],
         'status': 'INVALID', 'reason': 'test did not run'},
    ], 'counts': {'KILLED': 1, 'INVALID': 1}}


class CompareVerdictsTests(unittest.TestCase):
    def test_negative_runner_requires_the_declared_assertion_message(self):
        judge = runpy.run_path(str(SCRIPT.with_name('negative_compare_verdicts.py')))[
            'bound_failure_matches']
        marker = '#1100 target assertion'
        full_test = 'test_compare_verdicts.CompareVerdictsTests.test_case_keys_preserve_case'
        for actual, message, expected in ((0, marker, True),
                                          (marker, '#1100 earlier stderr guard', False)):
            with self.subTest(message=message):
                try:
                    self.assertEqual(actual, 1, message)
                except AssertionError as error:
                    result = SimpleNamespace(testsRun=1, errors=[],
                                             bound_failures=[(full_test, (type(error), error, None))])
                self.assertEqual(judge(result, full_test, marker), expected,
                                 '#1100 actual-value marker is not the declared assertion message')
                self.assertFalse(judge(result, 'another.test', marker),
                                 '#1100 negative runner must bind the complete test name')

    def invoke(self, old, new, *, raw=False, json_output=True):
        with tempfile.TemporaryDirectory(prefix='compare-verdicts-') as directory:
            root = Path(directory)
            paths = [root / 'old.json', root / 'new.json']
            for path, document in zip(paths, (old, new)):
                path.write_bytes(document if raw else json.dumps(document).encode())
            contents = {path.name: path.read_bytes() for path in root.iterdir()}
            result = subprocess.run(
                [sys.executable, '-I', '-B', str(SCRIPT), *map(str, paths)]
                + (['--json'] if json_output else []),
                capture_output=True, text=True, timeout=10, cwd=root)
            self.assertEqual(result.stderr, '', '#1100 CLI must report without traceback/stderr')
            self.assertEqual({path.name: path.read_bytes() for path in root.iterdir()}, contents,
                             '#1100 comparator must leave both inputs and directory untouched')
            return result, json.loads(result.stdout) if json_output else result.stdout

    def test_real_runner_summaries(self):
        for filename in REAL_SUMMARIES:
            with self.subTest(filename=filename):
                raw = (TESTDATA / filename).read_bytes()
                old = json.loads(raw)
                result, report = self.invoke(raw, raw, raw=True)
                self.assertEqual(result.returncode, 0, '#1100 real summary must be accepted unchanged')
                self.assertEqual(report['same'], len(old['controls']),
                                 '#1100 every real summary row must be compared')
                for change, category in (('delete', 'missing_cases'), ('status', 'status_changes'),
                                         ('test', 'test_changes'), ('assertion', 'assertion_changes')):
                    with self.subTest(change=change):
                        new = copy.deepcopy(old)
                        row = new['controls'][0]
                        if change == 'delete':
                            new['controls'].pop(0)
                        elif change == 'status':
                            row['status'] = 'SURVIVED' if row['status'] != 'SURVIVED' else 'KILLED'
                        elif 'targets' in row:
                            row['targets'][0][0 if change == 'test' else 1] = '#1100 changed binding'
                        else:
                            row[change] = '#1100 changed binding'
                        self.different(old, new, category)

    def test_missing_assertion_is_explicit_empty_marker(self):
        for identity in ('mutation', 'name'):
            with self.subTest(identity=identity):
                document = {'controls': [{identity: 'alpha', 'status': 'KILLED',
                                          'test': 'suite::alpha', 'compiled': True}]}
                result, report = self.invoke(document, document)
                self.assertEqual(result.returncode, 0, '#1100 absent marker schema is supported')
                self.assertEqual(report['table'][0]['old'][0]['bindings'],
                                 [{'test': 'suite::alpha', 'assertion': ''}],
                                 '#1100 absent assertion normalizes to explicit empty marker')
                marked = copy.deepcopy(document)
                marked['controls'][0]['assertion'] = '#1100 present marker'
                for old, new in ((document, marked), (marked, document)):
                    self.different(old, new, 'assertion_changes')
                other = {'controls': [{'name' if identity == 'mutation' else 'mutation': 'alpha',
                                       'status': 'KILLED', 'test': 'suite::alpha'}]}
                result, _ = self.invoke(document, other)
                self.assertEqual(result.returncode, 0, '#1100 missing marker forms normalize equally')

    def test_missing_assertion_does_not_hide_unknown_bindings(self):
        for schema in ('mutation', 'name', 'mutation-marker', 'name-marker', 'targets'):
            for extra in ('anchor', 'message', 'markers', 'assertions', 'unknown'):
                with self.subTest(schema=schema, extra=extra):
                    identity = 'mutation' if schema.startswith('mutation') else 'name'
                    row = {identity: 'alpha', 'status': 'KILLED', extra: '#1100 other marker'}
                    if schema == 'targets':
                        row['targets'] = [['suite::alpha', '#1100 marker']]
                    else:
                        row['test'] = 'suite::alpha'
                        if schema.endswith('-marker'):
                            row['assertion'] = '#1100 marker'
                    document = {'controls': [row]}
                    self.invalid(document, document, 'unsupported row fields')

    def test_scalar_schemas_keep_other_fields_strict(self):
        for identity in ('mutation', 'name'):
            for has_marker in (False, True):
                base = {identity: 'alpha', 'status': 'KILLED', 'test': 'suite::alpha'}
                if has_marker:
                    base['assertion'] = '#1100 marker'
                for field, values in ((identity, [None, '', '  ', 1, [], {}]),
                                      ('test', [None, '', '  ', 1, [], {}]),
                                      ('status', [None, '', 'killed', 1, [], {}])):
                    for value in values:
                        with self.subTest(identity=identity, has_marker=has_marker,
                                          field=field, value=value):
                            row = {**base, field: value}
                            self.invalid({'controls': [row]}, {'controls': [row]}, field)
                for field in (identity, 'test', 'status'):
                    with self.subTest(identity=identity, has_marker=has_marker, missing=field):
                        row = dict(base)
                        del row[field]
                        self.invalid({'controls': [row]}, {'controls': [row]}, 'controls[0]')
                for marker in (None, '', '  ', 1, {}, [], [''], [1], [['nested']]):
                    with self.subTest(identity=identity, has_marker=has_marker, marker=marker):
                        row = {**base, 'assertion': marker}
                        self.invalid({'controls': [row]}, {'controls': [row]}, 'assertion')

    def strict_case_keys(self, before, after):
        for schema in ('mutation', 'name', 'targets'):
            with self.subTest(schema=schema):
                identity = 'mutation' if schema == 'mutation' else 'name'
                row = {identity: before, 'status': 'KILLED'}
                if schema == 'targets':
                    row['targets'] = [['suite::alpha', '#1100 alpha']]
                else:
                    row['test'] = 'suite::alpha'
                old = {'controls': [row]}
                new = copy.deepcopy(old)
                new['controls'][0][identity] = after
                report = self.different(old, new, 'missing_cases')
                self.assertEqual(report['differences']['missing_cases'], sorted([
                    {'case': before, 'missing_from': 'new', 'severity': 'P0'},
                    {'case': after, 'missing_from': 'old', 'severity': 'P0'},
                ], key=lambda item: item['case']), '#1100 exact case keys require two missing rows')
                human, text = self.invoke(old, new, json_output=False)
                self.assertEqual(human.returncode, 1, '#1100 strict key human verdict')
                section = text.split(SECTIONS['missing_cases'] + '\n', 1)[1].split('\n\n', 1)[0]
                for key in (before, after):
                    self.assertIn(json.dumps(key), section,
                                  '#1100 both exact case keys appear in human missing section')

    def test_case_keys_preserve_case(self):
        self.strict_case_keys('alpha', 'Alpha')

    def test_case_keys_preserve_whitespace(self):
        self.strict_case_keys('alpha', 'alpha ')

    def test_case_keys_preserve_unicode(self):
        self.strict_case_keys('caf\u00e9', 'cafe\u0301')

    def different(self, old, new, category):
        result, report = self.invoke(old, new)
        self.assertEqual(result.returncode, 1, '#1100 recognized table difference must return 1')
        self.assertFalse(report['equivalent'], '#1100 a differing table must never pass')
        self.assertTrue(report['differences'][category], '#1100 expected difference category')
        self.assertEqual(report['errors'], [], '#1100 table differences are not parse errors')
        human, text = self.invoke(old, new, json_output=False)
        self.assertEqual(human.returncode, 1, '#1100 output format cannot change verdict')
        self.assertIn('NOT EQUIVALENT', text, '#1100 human verdict')
        section = text.split(SECTIONS[category] + '\n', 1)[1].split('\n\n', 1)[0]
        self.assertNotEqual(section, '(none)\n', '#1100 human difference section populated')
        self.assertNotEqual(section, '(none)', '#1100 human difference section populated')
        return report

    def invalid(self, old, new, fragment, *, raw=False):
        result, report = self.invoke(old, new, raw=raw)
        self.assertEqual(result.returncode, 2, '#1100 unknown/unreadable input must fail closed')
        self.assertFalse(report['equivalent'], '#1100 malformed input must never pass')
        self.assertIsNone(report['totals'], '#1100 partial tables must not have totals')
        self.assertIsNone(report['differences'], '#1100 parse errors must not look like case loss')
        self.assertEqual(report['table'], [], '#1100 partial tables must not be reported')
        self.assertEqual(report['same'], 0, '#1100 partial comparison must not count same cases')
        self.assertIn(fragment, ' '.join(e['reason'] for e in report['errors']),
                      '#1100 error must identify the rejected field or shape')
        return report

    def test_identical_and_reordered_tables(self):
        old = summary()
        new = copy.deepcopy(old)
        new['controls'].reverse()
        # Both shared runner forms may add reason/compiled/counts metadata.
        new['controls'][0].update(reason='different host', compiled=False)
        new['counts'] = {'ignored': 999}
        result, report = self.invoke(old, new)
        self.assertEqual(result.returncode, 0, '#1100 identical verdict tables must pass')
        self.assertTrue(report['equivalent'])
        self.assertEqual(report['totals'], {'old': 2, 'new': 2, 'union': 2})
        self.assertEqual(report['same'], 2)
        self.assertTrue(all(not value for value in report['differences'].values()))
        self.assertEqual([row['case'] for row in report['table']], ['alpha', 'beta'])
        human, text = self.invoke(old, new, json_output=False)
        self.assertEqual(human.returncode, 0)
        self.assertTrue(text.startswith('EQUIVALENT old=2 new=2 union=2 same=2'))
        self.assertIn('CASE TABLE', text)
        for title in SECTIONS.values():
            self.assertIn(title + '\n(none)', text)

    def test_all_four_exact_statuses(self):
        for status in ('KILLED', 'SURVIVED', 'INVALID', 'PASS'):
            with self.subTest(status=status):
                document = summary()
                document['controls'][0]['status'] = status
                result, report = self.invoke(document, document)
                self.assertEqual(result.returncode, 0, '#1100 equivalence is not success')
                self.assertEqual(report['table'][0]['old'][0]['status'], status)

    def test_deleted_case_is_p0(self):
        old, new = summary(), summary()
        new['controls'].pop()
        report = self.different(old, new, 'missing_cases')
        self.assertEqual(report['differences']['missing_cases'], [
            {'case': 'beta', 'missing_from': 'new', 'severity': 'P0'}])
        self.assertEqual(report['same'], 1)
        self.assertEqual(report['totals'], {'old': 2, 'new': 1, 'union': 2})

    def test_added_case_is_p0(self):
        old, new = summary(), summary()
        old['controls'].pop()
        report = self.different(old, new, 'missing_cases')
        self.assertEqual(report['differences']['missing_cases'], [
            {'case': 'beta', 'missing_from': 'old', 'severity': 'P0'}])

    def test_status_flip(self):
        old, new = summary(), summary()
        new['controls'][0]['status'] = 'SURVIVED'
        report = self.different(old, new, 'status_changes')
        self.assertEqual(report['differences']['status_changes'], [
            {'case': 'alpha', 'old': 'KILLED', 'new': 'SURVIVED'}])
        self.assertEqual(report['same'], 1)

    def test_test_binding_change(self):
        old, new = summary(), summary()
        new['controls'][0]['test'] = 'suite::wrong'
        report = self.different(old, new, 'test_changes')
        self.assertEqual(report['differences']['test_changes'], [
            {'case': 'alpha', 'old': ['suite::alpha'], 'new': ['suite::wrong']}])

    def test_assertion_marker_changes_and_shape_are_preserved(self):
        for marker in ('#1100 wrong', ['#1100 alpha']):
            with self.subTest(marker=marker):
                old, new = summary(), summary()
                new['controls'][0]['assertion'] = marker
                report = self.different(old, new, 'assertion_changes')
                self.assertEqual(report['differences']['assertion_changes'], [
                    {'case': 'alpha', 'old': ['#1100 alpha'], 'new': [marker]}])
        old, new = summary(), summary()
        new['controls'][1]['assertion'].reverse()
        self.different(old, new, 'assertion_changes')

    def test_duplicates_never_overwrite_or_count_same(self):
        for sides in (('old',), ('new',), ('old', 'new')):
            with self.subTest(sides=sides):
                documents = {'old': summary(), 'new': summary()}
                for side in sides:
                    documents[side]['controls'].append(copy.deepcopy(documents[side]['controls'][0]))
                report = self.different(documents['old'], documents['new'], 'duplicate_keys')
                self.assertEqual(report['differences']['duplicate_keys'], [
                    {'side': side, 'case': 'alpha', 'rows': [1, 3]} for side in sides])
                self.assertEqual(report['same'], 1, '#1100 duplicates cannot count as same')
                self.assertEqual(len(report['table'][0][sides[0]]), 2)

    def test_empty_tables_never_pass(self):
        for sides in (('old',), ('new',), ('old', 'new')):
            with self.subTest(sides=sides):
                documents = {'old': summary(), 'new': summary()}
                for side in sides:
                    documents[side]['controls'] = []
                report = self.different(documents['old'], documents['new'], 'empty_tables')
                self.assertEqual(report['differences']['empty_tables'], list(sides))
                self.assertEqual(report['same'], 0)

    def test_all_differences_are_reported_together(self):
        old, new = summary(), summary()
        new['controls'].pop()
        new['controls'][0].update(status='PASS', test='other', assertion='#1100 other')
        result, report = self.invoke(old, new)
        self.assertEqual(result.returncode, 1)
        for category in ('missing_cases', 'status_changes', 'test_changes', 'assertion_changes'):
            self.assertEqual(len(report['differences'][category]), 1)

    def test_matrix_and_regular_runner_schemas(self):
        old = summary()
        new = {'controls': [
            {'name': row['mutation'], 'status': row['status'],
             'targets': [[row['test'], row['assertion']]], 'compiled': True,
             'restoredPass': True} for row in old['controls']]}
        result, report = self.invoke(old, new)
        self.assertEqual(result.returncode, 0, '#1100 known runner schemas normalize equally')
        self.assertEqual(report['same'], 2)
        new['controls'][1]['targets'][0][1] = '#1100 changed matrix assertion'
        self.different(old, new, 'assertion_changes')

    def test_matrix_keeps_multiple_test_assertion_pairs(self):
        old = {'controls': [{'name': 'matrix', 'status': 'KILLED',
                            'targets': [['test::one', '#1100 one'], ['test::two', '#1100 two']]}]}
        result, _ = self.invoke(old, old)
        self.assertEqual(result.returncode, 0)
        new = copy.deepcopy(old)
        new['controls'][0]['targets'][0][1] = '#1100 two'
        new['controls'][0]['targets'][1][1] = '#1100 one'
        self.different(old, new, 'assertion_changes')
        new = copy.deepcopy(old)
        new['controls'][0]['targets'].pop()
        report = self.different(old, new, 'test_changes')
        self.assertTrue(report['differences']['assertion_changes'])

    def test_unknown_statuses_are_not_guessed(self):
        for status in ('COMPILED', 'SKIPPED(pending-reanchor)', 'CONTROL-OK', 'killed', '', None, [], 1):
            with self.subTest(status=status):
                document = summary()
                document['controls'][0]['status'] = status
                report = self.invalid(document, document, 'unknown status')
                self.assertEqual([e['side'] for e in report['errors']], ['old', 'new'])

    def test_missing_required_fields(self):
        for field in ('mutation', 'status', 'test'):
            with self.subTest(field=field):
                document = summary()
                del document['controls'][0][field]
                self.invalid(summary(), document, 'controls[0]')

    def test_invalid_fields_and_marker_lists(self):
        for field, values in (
            ('mutation', [None, '', '  ', 1, [], {}]),
            ('test', [None, '', '  ', 1, [], {}]),
            ('assertion', [None, '', '  ', 1, {}, [], [''], [1], [['nested']]]),
        ):
            for value in values:
                with self.subTest(field=field, value=value):
                    document = summary()
                    document['controls'][0][field] = value
                    self.invalid(summary(), document, field)

    def test_unknown_containers_and_nonobject_rows(self):
        for document in ([], None, {}, {'cases': []}, {'controls': {}},
                         {'controls': None}, {'controls': [None]}, {'controls': [[]]}):
            with self.subTest(document=document):
                self.invalid(summary(), document, 'expected')

    def test_unknown_and_mixed_row_schemas(self):
        variants = [
            {'name': 'alpha', 'status': 'KILLED', 'assertion': 'marker'},
            {**summary()['controls'][0], 'name': 'other'},
            {**summary()['controls'][0], 'targets': [['test', 'marker']]},
            {'name': 'alpha', 'status': 'KILLED', 'targets': [['test', 'marker']], 'test': 'other'},
            {'name': 'alpha', 'status': 'KILLED', 'targets': [['test', 'marker']], 'assertion': 'other'},
        ]
        for row in variants:
            with self.subTest(row=row):
                self.invalid(summary(), {'controls': [row]}, 'controls[0]')

    def test_bad_matrix_targets(self):
        for targets in (None, [], {}, [None], ['string'], [[]], [['only-test']],
                        [['test', 'marker', 'extra']], [['', 'marker']], [['test', []]]):
            with self.subTest(targets=targets):
                document = {'controls': [{'name': 'matrix', 'status': 'KILLED', 'targets': targets}]}
                self.invalid(summary(), document, 'targets')
        self.invalid(summary(), {'controls': [{'name': '', 'status': 'PASS',
                                              'targets': [['test', 'marker']]}]}, '.name')

    def test_bad_json_and_encoding(self):
        for data, message in ((b'{', 'Expecting'), (b'\xff', 'utf-8'),
                              (b'{"controls": [], "controls": []}', 'duplicate JSON member'),
                              (b'{"controls": [], "unused": NaN}', 'non-JSON numeric'),
                              (b'{"controls": [], "unused": Infinity}', 'non-JSON numeric')):
            with self.subTest(data=data):
                self.invalid(json.dumps(summary()).encode(), data, message, raw=True)
        valid = json.dumps(summary()).encode()
        duplicate = valid.replace(b'"status": "KILLED"', b'"status": "INVALID", "status": "KILLED"')
        self.invalid(valid, duplicate, 'duplicate JSON member', raw=True)
        human, text = self.invoke(valid, b'{', raw=True, json_output=False)
        self.assertEqual(human.returncode, 2)
        self.assertTrue(text.startswith('INPUT ERROR (exit 2)'))
        self.assertIn('new:', text)

    def test_missing_file_and_directory(self):
        with tempfile.TemporaryDirectory(prefix='compare-missing-') as directory:
            root = Path(directory)
            for path in (root / 'absent.json', root):
                with self.subTest(path=path):
                    result = subprocess.run([sys.executable, '-I', '-B', str(SCRIPT),
                                             str(path), str(path), '--json'],
                                            capture_output=True, text=True, timeout=10)
                    self.assertEqual(result.returncode, 2, '#1100 unreadable inputs return 2')
                    self.assertEqual(result.stderr, '')
                    self.assertEqual(len(json.loads(result.stdout)['errors']), 2)

    def test_human_table_cannot_inject_lines(self):
        document = summary()
        document['controls'][0]['mutation'] = 'alpha\nP0 MISSING CASES\tforged'
        result, text = self.invoke(document, document, json_output=False)
        self.assertEqual(result.returncode, 0)
        self.assertIn(r'alpha\nP0 MISSING CASES\tforged', text)
        self.assertNotIn('\nP0 MISSING CASES\tforged', text)


if __name__ == '__main__':
    unittest.main()

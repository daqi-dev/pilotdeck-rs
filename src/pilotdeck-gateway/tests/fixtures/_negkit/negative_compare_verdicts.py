#!/usr/bin/env python3
# anchor-selfcheck: unsupported disposable Python comparator mutations, no product source writes
"""Exercise bound unittest assertions against disposable comparator copies.

No Rust/source-tree mutation or cargo process. Compile every Python variant,
require the named test's structured AssertionError (never log matching), run all
selected variants, then summarize. Usage: script.py OUT [variant ...].
"""
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest


# -I alone still lets importlib write beside the imported test module.
sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
VARIANTS = (
    ('reject-missing-marker', "return {'test': test, 'assertion': reason_marker(row.get('reason'))}",
     "return binding(test, None, field)", 'test_missing_assertion_is_explicit_empty_marker',
     '#1100 absent marker schema is supported'),
    ('invent-missing-marker', "return {'test': test, 'assertion': reason_marker(row.get('reason'))}",
     "return {'test': test, 'assertion': 'invented'}", 'test_missing_assertion_is_explicit_empty_marker',
     '#1100 absent assertion normalizes to explicit empty marker'),
    ('reject-name-test', "elif 'name' in row and 'test' in row:",
     "elif False and 'name' in row and 'test' in row:", 'test_real_runner_summaries',
     '#1100 real summary must be accepted unchanged'),
    ('erase-present-marker', "return binding(row.get('test'), row['assertion'], field)",
     "return {'test': required_string(row.get('test'), field + '.test'), 'assertion': ''}",
     'test_missing_assertion_is_explicit_empty_marker',
     '#1100 recognized table difference must return 1'),
    ('ignore-unknown-marker', 'if unknown:', 'if False and unknown:',
     'test_missing_assertion_does_not_hide_unknown_bindings',
     '#1100 unknown/unreadable input must fail closed'),
    ('default-missing-test', "test = required_string(row.get('test'), field + '.test')",
     "test = required_string(row.get('test', 'suite::fallback'), field + '.test')",
     'test_scalar_schemas_keep_other_fields_strict',
     '#1100 unknown/unreadable input must fail closed'),
    ('accept-empty-marker', "required_string(assertion, field + '.assertion')",
     "if assertion != '': required_string(assertion, field + '.assertion')",
     'test_scalar_schemas_keep_other_fields_strict',
     '#1100 unknown/unreadable input must fail closed'),
    ('mixed-target-binding', "if 'test' in row or 'assertion' in row:",
     "if False and ('test' in row or 'assertion' in row):", 'test_unknown_and_mixed_row_schemas',
     '#1100 unknown/unreadable input must fail closed'),
    ('case-folded', "return {'case': key,", "return {'case': key.lower(),",
     'test_case_keys_preserve_case', '#1100 recognized table difference must return 1'),
    ('case-trimmed', "return {'case': key,", "return {'case': key.strip(),",
     'test_case_keys_preserve_whitespace', '#1100 recognized table difference must return 1'),
    ('case-unicode-normalized', "return {'case': key,",
     "return {'case': __import__('unicodedata').normalize('NFC', key),",
     'test_case_keys_preserve_unicode', '#1100 recognized table difference must return 1'),
    ('ignore-reason-marker', "'assertion': reason_marker(row.get('reason'))",
     "'assertion': ''", 'test_real_reason_marker_changes',
     '#1100 recognized table difference must return 1'),
    ('compare-free-reason', "return {'test': test, 'assertion': reason_marker(row.get('reason'))}",
     "return {'test': test, 'assertion': row.get('reason')}",
     'test_unrecognized_reason_is_reported_unbound',
     '#1100 free reason text must not affect equivalence'),
    ('keep-diagnostic-tail', 'return re.split(', 'return reason or re.split(',
     'test_reason_diagnostics_do_not_change_binding',
     '#1100 producer diagnostics are not assertion bindings'),
    ('truncate-marker-semicolon', 'reason, maxsplit=1)[0]',
     "reason, maxsplit=1)[0].split('; ', 1)[0]",
     'test_reason_diagnostics_do_not_change_binding',
     '#1100 semicolon inside the real marker is preserved'),
    ('find-marker-inside-free-text', 'not re.match(', 'not re.search(',
     'test_unrecognized_reason_is_reported_unbound',
     '#1100 free reason text must not affect equivalence'),
    ('omit-unbound-report', "if bound['assertion'] == ''", "if False and bound['assertion'] == ''",
     'test_unrecognized_reason_is_reported_unbound',
     '#1100 missing binding is explicit in JSON report'),
    ('omit-unbound-human', "lines += [cell(item) for item in report['unbound_markers']] or ['(none)']",
     "lines += ['(none)']", 'test_unrecognized_reason_is_reported_unbound',
     '#1100 missing binding is explicit in human report'),
    ('reason-overrides-assertion', "if 'assertion' in row:",
     "if 'assertion' in row and 'reason' not in row:",
     'test_explicit_assertion_takes_priority_over_reason',
     '#1100 explicit assertion takes priority over reason'),
    ('invalid-assertion-falls-back', "if 'assertion' in row:", "if row.get('assertion'):",
     'test_invalid_explicit_assertion_cannot_fall_back_to_reason',
     '#1100 unknown/unreadable input must fail closed'),
    ('erase-scalar-schema', "schema = 'mutation/test'", "schema = 'name/test'",
     'test_mutation_schema_cannot_collapse_to_name', '#1110 mutation schema must remain distinct'),
    ('erase-unmarked-schema', "schema = 'mutation/test'", "schema = 'name/test'",
     'test_schema_drift_without_assertions', '#1100 recognized table difference must return 1'),
    ('erase-target-schema', "schema = 'name/targets'", "schema = 'name/test'",
     'test_target_schema_cannot_collapse_to_name', '#1110 target schema must remain distinct'),
    ('ignore-hits', "'hits': observed_hits(row, field)", "'hits': None",
     'test_real_hits_are_compared', '#1100 recognized table difference must return 1'),
    ('missing-hits-as-empty', "if 'hits' not in row:\n        return None",
     "if 'hits' not in row:\n        return []", 'test_hits_absence_and_empty_are_distinct',
     '#1100 recognized table difference must return 1'),
    ('accept-bad-hit-container', 'if not isinstance(hits, list):',
     'if not isinstance(hits, (list, str)):', 'test_hits_types_are_strict',
     '#1100 unknown/unreadable input must fail closed'),
    ('accept-bad-hit-member', "required_string(hit, field + '.hits[]')", 'pass',
     'test_hits_types_are_strict', '#1100 unknown/unreadable input must fail closed'),
    ('require-all-hits', 'return hits',
     "return [target[0] for target in row.get('targets', [])]",
     'test_partial_hits_are_valid_evidence', '#1100 normalized table keeps actual hits'),
)


class BoundResult(unittest.TextTestResult):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.bound_failures = []

    def addFailure(self, test, err):
        super().addFailure(test, err)
        self.bound_failures.append((test.id(), err))

    def addSubTest(self, test, subtest, err):
        super().addSubTest(test, subtest, err)
        if err is not None:
            self.bound_failures.append((test.id(), err))


def bound_failure_matches(result, test, marker):
    # unittest appends its explicit msg after the rendered actual values.
    # A marker printed by the CLI inside an earlier guard's actual value is
    # not the guard's declared msg and must never qualify as a bound failure.
    return (result.testsRun == 1 and not result.errors and bool(result.bound_failures)
            and all(identity == test and issubclass(error[0], AssertionError)
                    and str(error[1]).endswith(' : ' + marker)
                    for identity, error in result.bound_failures))


def main():
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_compare_verdicts.py OUT [variant ...]')
    selected = set(sys.argv[2:]) or {row[0] for row in VARIANTS}
    unknown = selected - {row[0] for row in VARIANTS}
    if unknown:
        raise SystemExit(f'unknown variants: {sorted(unknown)}')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    spec = importlib.util.spec_from_file_location('test_compare_verdicts', HERE / 'test_compare_verdicts.py')
    tests = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(tests)
    original_script = tests.SCRIPT
    source = original_script.read_text()
    records = []

    def run(test, script, label):
        tests.SCRIPT = script
        stream = io.StringIO()
        result = unittest.TextTestRunner(stream=stream, verbosity=2, resultclass=BoundResult).run(
            unittest.TestSuite([tests.CompareVerdictsTests(test)]))
        with (out / (label + '.log')).open('x') as log:
            log.write(stream.getvalue())
        return result

    for name, before, after, test, marker in VARIANTS:
        if name not in selected:
            continue
        full_test = f'test_compare_verdicts.CompareVerdictsTests.{test}'
        item = dict(name=name, test=full_test, assertion=marker, status='INVALID',
                    reason='not run', compiled=False)
        try:
            if source.count(before) != 1:
                raise ValueError('variant anchor must occur exactly once')
            changed = source.replace(before, after, 1)
            compile(changed, name, 'exec')
            item['compiled'] = True
            positive = run(test, original_script, name + '-positive')
            if not positive.wasSuccessful() or positive.testsRun != 1:
                raise ValueError('bound positive test did not pass')
            with tempfile.TemporaryDirectory(prefix='compare-verdicts-mutant-') as directory:
                script = Path(directory) / 'compare_verdicts.py'
                with script.open('x') as stream:
                    stream.write(changed)
                negative = run(test, script, name + '-negative')
            restored = run(test, original_script, name + '-restored')
            if not restored.wasSuccessful() or restored.testsRun != 1:
                raise ValueError('bound restored test did not pass')
            if negative.wasSuccessful():
                item.update(status='SURVIVED', reason='bound test still passed')
            elif bound_failure_matches(negative, full_test, marker):
                item.update(status='KILLED', reason='bound test raised only the declared AssertionError')
            else:
                item['reason'] = 'unrelated failure/error or missing bound AssertionError'
        except Exception as error:
            item['reason'] = f'{type(error).__name__}: {error}'
        finally:
            tests.SCRIPT = original_script
        records.append(item)
        print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    counts = {status: sum(row['status'] == status for row in records)
              for status in ('KILLED', 'SURVIVED', 'INVALID')}
    temporary = out / 'summary.json.tmp'
    with temporary.open('x') as stream:
        json.dump({'controls': records, 'counts': counts}, stream, indent=2)
        stream.write('\n')
    temporary.replace(out / 'summary.json')
    print(json.dumps(counts), flush=True)
    return int(any(row['status'] != 'KILLED' for row in records))


if __name__ == '__main__':
    sys.exit(main())

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
    ('reject-missing-marker', "return {'test': test, 'assertion': ''}",
     "return binding(test, None, field)", 'test_missing_assertion_is_explicit_empty_marker',
     '#1100 absent marker schema is supported'),
    ('invent-missing-marker', "return {'test': test, 'assertion': ''}",
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

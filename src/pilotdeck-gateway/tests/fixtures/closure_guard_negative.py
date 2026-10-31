#!/usr/bin/env python3
# anchor-selfcheck: unsupported in-process meta-control, no cargo mutations
"""#1139 fix-r1: neutralized closure/criterion guards must kill their bound tests.

Mutates the two count runners in place inside the shared source guard and runs
the bound ``test_import_closure.py`` tests in-process (same thread, reentrant
tree lock, no cargo anywhere): positive -> negative -> restored per row, every
row recorded before the summary, any non-KILLED row exits nonzero. Run after
committing, on a clean tree, like the suites it guards.
"""
import importlib.util
import io
import json
import sys
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
FIXTURES = Path(__file__).resolve().parent
REPO = FIXTURES.parents[3]
TESTS = FIXTURES / 'test_import_closure.py'
sys.path.insert(0, str(FIXTURES / '_shared'))
from negative_process import mutating_sources

RECORDER = FIXTURES / 'recorder/fix3-negative.py'
RUN_REGISTRY = FIXTURES / 'run-registry/count-negative.py'

# (probe, runner, bound test, pristine anchor, mutant anchor, kill marker)
GUARD_PROBES = [
    ('recorder-closure-accept-unrecognized', RECORDER,
     'ClosureTests.test_unrecognized_dynamic_import_is_rejected',
     """        raise ValueError(
            f'#1139 unrecognized dynamic import argument in {path}: {argument}')""",
     '        return []  # mutated: unrecognized dynamic import accepted',
     '#1139 fix-r1: unrecognized dynamic import must fail closed'),
    ('run-registry-closure-accept-unrecognized', RUN_REGISTRY,
     'ClosureTests.test_unrecognized_dynamic_import_is_rejected',
     """        raise ValueError(
            f'#1129 unrecognized dynamic import argument in {path}: {argument}')""",
     '        return []  # mutated: unrecognized dynamic import accepted',
     '#1139 fix-r1: unrecognized dynamic import must fail closed'),
    ('recorder-closure-skip-absolute', RECORDER,
     'ClosureTests.test_absolute_dynamic_import_outside_repository_is_rejected',
     """    raise ValueError(
        f'#1139 absolute dynamic import outside the repository in {path}: {argument}')""",
     '    return []  # mutated: absolute dynamic import skipped',
     '#1139 fix-r1: out-of-root absolute dynamic import must fail closed'),
    ('run-registry-closure-skip-absolute', RUN_REGISTRY,
     'ClosureTests.test_absolute_dynamic_import_outside_repository_is_rejected',
     """    raise ValueError(
        f'#1129 absolute dynamic import outside the repository in {path}: {argument}')""",
     '    return []  # mutated: absolute dynamic import skipped',
     '#1139 fix-r1: out-of-root absolute dynamic import must fail closed'),
    ('recorder-closure-drop-literal-dynamic', RECORDER,
     'ClosureTests.test_literal_dynamic_imports_join_the_closure',
     """        for argument in _dynamic_import_arguments(text):
            pending.extend(_dynamic_import_targets(path, argument, source_root))""",
     '        for argument in _dynamic_import_arguments(text):\n'
     '            pass  # mutated: literal dynamic imports dropped',
     '#1139 fix-r1: literal dynamic imports must join the closure'),
    ('recorder-closure-reject-baseline-loader', RECORDER,
     'ClosureTests.test_baseline_loader_dynamic_import_is_allowed',
     """        if re.search(r'\\bbaseline\\b', argument):
            return []""",
     """        if re.search(r'\\bbaseline\\b', argument):
            raise ValueError('mutated: baseline loader rejected')""",
     '#1139 fix-r1: baseline loader dynamic import must stay allowed'),
    ('recorder-guard-drop-rust-criterion', RECORDER,
     'GuardedInputTests.test_dirty_rust_criterion_is_refused_before_positive',
     """               gateway / 'tests/recorder.rs',
               gateway / 'tests/common/baseline.rs',
               gateway / 'tests/common/regeneration_masks.rs',
               fixture_root / '_shared/regeneration-masks.tsv',
               gateway / 'Cargo.toml']""",
     '               ]',
     '#1139 fix-r1: dirty Rust criterion input must be refused: '),
]


def run_bound_test(test):
    """Run one bound test from a freshly loaded module; return (result, text)."""
    spec = importlib.util.spec_from_file_location('import_closure_under_probe', TESTS)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    stream = io.StringIO()
    outcome = unittest.TextTestRunner(stream=stream, verbosity=0).run(
        unittest.TestLoader().loadTestsFromName(test, module))
    return outcome, stream.getvalue()


def judge(outcome, output, marker):
    if outcome.wasSuccessful() and not outcome.skipped:
        return 'SURVIVED', 'bound test still passed'
    if outcome.errors:
        return 'INVALID', 'bound test errored instead of failing at its assertion'
    failures = ''.join(traceback for _test, traceback in outcome.failures)
    if marker in failures:
        return 'KILLED', 'bound test failed at its mutant-binding assertion'
    return 'INVALID', 'bound test failed without its mutant-binding marker'


def main():
    results = []
    with mutating_sources(REPO, [RECORDER, RUN_REGISTRY]):
        for probe, path, test, pristine, mutant, marker in GUARD_PROBES:
            row = dict(probe=probe, test=f'test_import_closure.py::{test}',
                       assertion=marker, status='INVALID', reason='not run')
            try:
                source = path.read_text()
                if source.count(pristine) != 1:
                    raise RuntimeError(f'#1139 stale probe anchor: {probe}')
                outcome, _output = run_bound_test(test)
                if not outcome.wasSuccessful():
                    raise RuntimeError('#1139 bound test failed before mutation')
                try:
                    path.write_text(source.replace(pristine, mutant))
                    outcome, output = run_bound_test(test)
                    row['status'], row['reason'] = judge(outcome, output, marker)
                finally:
                    path.write_text(source)
            except Exception as error:
                row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
            try:
                outcome, _output = run_bound_test(test)
                if not outcome.wasSuccessful():
                    raise RuntimeError('restored bound test failed')
            except Exception as error:
                row.update(status='INVALID',
                           reason=row['reason'] + f'; restored: {error}')
            results.append(row)
    for row in results:
        print(json.dumps(row), flush=True)
    counts = {status: sum(row['status'] == status for row in results)
              for status in ('KILLED', 'SURVIVED', 'INVALID')}
    print('SUMMARY: ' + ' '.join(f'{status}={count}' for status, count in counts.items()),
          flush=True)
    if len(results) != len(GUARD_PROBES) or any(row['status'] != 'KILLED' for row in results):
        return 1
    print(f'PASS: {len(GUARD_PROBES)} / {len(GUARD_PROBES)} closure guard mutants killed',
          flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())

#!/usr/bin/env python3
# anchor-selfcheck: unsupported in-process meta-control, no cargo mutations
"""#1139: neutralized closure/criterion guards must kill their bound tests.

Mutates the two count runners in place inside the shared source guard and runs
the bound ``test_import_closure.py`` tests in-process (same thread, reentrant
tree lock, no cargo anywhere): positive -> negative -> restored per row, every
row recorded before the summary, any non-KILLED row exits nonzero. On this
base the guard's shared #1166 scanner refuses any dirty workspace input, so a
dirty criterion is refused before the first positive control; every probe's
kill comes from its own bound assertion, never from the runner self-check.
SIGTERM/SIGHUP unwind through the finally clauses so a killed run still
restores the mutated sources. Run after committing, on a clean tree, like the
suites it guards. ``--check-anchors`` only verifies that every anchor is
unique and every mutant still parses.
"""
import ast
import importlib.util
import io
import json
import signal
import sys
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
FIXTURES = Path(__file__).resolve().parent
REPO = FIXTURES.parents[3]
TESTS = FIXTURES / 'test_import_closure.py'
# negative_process.py sits in the fixtures root; only negative_control.py,
# imported first by the runners, would otherwise put it on the path.
sys.path.insert(0, str(FIXTURES / '_shared'))
sys.path.insert(0, str(FIXTURES))
from negative_process import mutating_sources

RECORDER = FIXTURES / 'recorder/fix3-negative.py'
RUN_REGISTRY = FIXTURES / 'run-registry/count-negative.py'

# (probe, runner, bound test, pristine anchor, mutant anchor, kill marker)
GUARD_PROBES = [
    ('recorder-closure-accept-unrecognized', RECORDER,
     'ClosureTests.test_unrecognized_dynamic_import_is_rejected',
     """        raise ValueError(
            f'#1139 unrecognized dynamic import argument in {path}: {original}')""",
     '        return []  # mutated: unrecognized dynamic import accepted',
     '#1139: unrecognized dynamic import must fail closed'),
    ('run-registry-closure-accept-unrecognized', RUN_REGISTRY,
     'ClosureTests.test_unrecognized_dynamic_import_is_rejected',
     """        raise ValueError(
            f'#1129 unrecognized dynamic import argument in {path}: {original}')""",
     '        return []  # mutated: unrecognized dynamic import accepted',
     '#1139: unrecognized dynamic import must fail closed'),
    ('recorder-closure-widen-baseline-whitelist', RECORDER,
     'ClosureTests.test_unrecognized_dynamic_import_is_rejected',
     """    else:
        raise ValueError(
            f'#1139 unrecognized dynamic import argument in {path}: {original}')""",
     """    else:
        if re.search(r'\\bbaseline\\b', argument):
            return []  # mutated: any baseline mention accepted
        raise ValueError(
            f'#1139 unrecognized dynamic import argument in {path}: {original}')""",
     '#1139: unrecognized dynamic import must fail closed'),
    ('run-registry-closure-widen-baseline-whitelist', RUN_REGISTRY,
     'ClosureTests.test_unrecognized_dynamic_import_is_rejected',
     """    else:
        raise ValueError(
            f'#1129 unrecognized dynamic import argument in {path}: {original}')""",
     """    else:
        if re.search(r'\\bbaseline\\b', argument):
            return []  # mutated: any baseline mention accepted
        raise ValueError(
            f'#1129 unrecognized dynamic import argument in {path}: {original}')""",
     '#1139: unrecognized dynamic import must fail closed'),
    ('recorder-closure-skip-absolute', RECORDER,
     'ClosureTests.test_absolute_dynamic_import_outside_repository_is_rejected',
     """    raise ValueError(
        f'#1139 absolute dynamic import outside the repository in {path}: {original}')""",
     '    return []  # mutated: absolute dynamic import skipped',
     '#1139: out-of-root absolute dynamic import must fail closed'),
    ('run-registry-closure-skip-absolute', RUN_REGISTRY,
     'ClosureTests.test_absolute_dynamic_import_outside_repository_is_rejected',
     """    raise ValueError(
        f'#1129 absolute dynamic import outside the repository in {path}: {original}')""",
     '    return []  # mutated: absolute dynamic import skipped',
     '#1139: out-of-root absolute dynamic import must fail closed'),
    ('recorder-closure-skip-file-url', RECORDER,
     'ClosureTests.test_file_url_dynamic_imports_follow_absolute_rules',
     """    scheme = re.match(r'[A-Za-z][A-Za-z0-9+.\\-]*:', specifier)
    if scheme is not None:""",
     """    scheme = re.match(r'[A-Za-z][A-Za-z0-9+.\\-]*:', specifier)
    if False:  # mutated: file: URLs skipped""",
     '#1139: in-root file: URL dynamic import must join the closure'),
    ('run-registry-closure-skip-file-url', RUN_REGISTRY,
     'ClosureTests.test_file_url_dynamic_imports_follow_absolute_rules',
     """    scheme = re.match(r'[A-Za-z][A-Za-z0-9+.\\-]*:', specifier)
    if scheme is not None:""",
     """    scheme = re.match(r'[A-Za-z][A-Za-z0-9+.\\-]*:', specifier)
    if False:  # mutated: file: URLs skipped""",
     '#1139: in-root file: URL dynamic import must join the closure'),
    ('recorder-closure-drop-literal-dynamic', RECORDER,
     'ClosureTests.test_literal_dynamic_imports_join_the_closure',
     """        for argument in _dynamic_import_arguments(text):
            pending.extend(_dynamic_import_targets(path, argument, source_root))""",
     '        for argument in _dynamic_import_arguments(text):\n'
     '            pass  # mutated: literal dynamic imports dropped',
     '#1139: literal dynamic imports must join the closure'),
    ('run-registry-closure-drop-literal-dynamic', RUN_REGISTRY,
     'ClosureTests.test_literal_dynamic_imports_join_the_closure',
     """        for argument in _dynamic_import_arguments(text):
            pending.extend(_dynamic_import_targets(path, argument, source_root))""",
     '        for argument in _dynamic_import_arguments(text):\n'
     '            pass  # mutated: literal dynamic imports dropped',
     '#1139: literal dynamic imports must join the closure'),
    ('recorder-closure-drop-trailing-strip', RECORDER,
     'ClosureTests.test_literal_dynamic_imports_join_the_closure',
     """    original = argument
    argument = _argument_without_trailing(argument, path)""",
     '    original = argument  # mutated: trailing comment and comma kept',
     '#1139: literal dynamic imports must join the closure'),
    ('run-registry-closure-drop-trailing-strip', RUN_REGISTRY,
     'ClosureTests.test_literal_dynamic_imports_join_the_closure',
     """    original = argument
    argument = _argument_without_trailing(argument, path)""",
     '    original = argument  # mutated: trailing comment and comma kept',
     '#1139: literal dynamic imports must join the closure'),
    ('recorder-closure-drop-static-edge-forms', RECORDER,
     'ClosureTests.test_static_import_edge_forms_join_the_closure',
     """            index = _skip_spacing(text, found.end())
            if index >= len(text) or text[index] not in '\\'"':""",
     "            index = found.end()  # mutated: comment-separated static imports dropped\n"
     "            if index >= len(text) or text[index] not in '\\\"':",
     '#1139: static import edge forms must join the closure'),
    ('run-registry-closure-drop-static-edge-forms', RUN_REGISTRY,
     'ClosureTests.test_static_import_edge_forms_join_the_closure',
     """            index = _skip_spacing(text, found.end())
            if index >= len(text) or text[index] not in '\\'"':""",
     "            index = found.end()  # mutated: comment-separated static imports dropped\n"
     "            if index >= len(text) or text[index] not in '\\\"':",
     '#1139: static import edge forms must join the closure'),
    ('recorder-closure-reject-baseline-loader', RECORDER,
     'ClosureTests.test_baseline_loader_dynamic_import_is_allowed',
     """    elif re.sub(r'\\s+', ' ', argument) in BASELINE_LOADERS:
        return []""",
     """    elif re.sub(r'\\s+', ' ', argument) in BASELINE_LOADERS:
        raise ValueError('mutated: baseline loader rejected')""",
     '#1139: baseline loader dynamic imports must stay allowed'),
    ('run-registry-closure-reject-baseline-loader', RUN_REGISTRY,
     'ClosureTests.test_baseline_loader_dynamic_import_is_allowed',
     """    elif re.sub(r'\\s+', ' ', argument) in BASELINE_LOADERS:
        return []""",
     """    elif re.sub(r'\\s+', ' ', argument) in BASELINE_LOADERS:
        raise ValueError('mutated: baseline loader rejected')""",
     '#1139: baseline loader dynamic imports must stay allowed'),
    ('recorder-closure-helpers-drift', RECORDER,
     'ClosureTests.test_runner_closure_helpers_stay_synchronized',
     """    'pathToFileURL(join(baseline, source)).href',""",
     """    'pathToFileURL(join(baseline, sources)).href',""",
     "#1139: the runners' closure helpers must stay synchronized"),
    ('recorder-criterion-anchor-dropped', RECORDER,
     'CriterionCoverageTests.test_criterion_entry_outside_scanner_inputs_fails_closed',
     """    if CRITERION_ENTRY not in inputs:
        raise RuntimeError(
            f'#1139 Rust criterion entry is not a #1166 scanner input: '
            f'{CRITERION_ENTRY}')
    return inputs""",
     '    return inputs  # mutated: criterion entry anchor dropped',
     'RuntimeError not raised'),
]


def run_bound_test(test):
    """Run one bound test from a freshly loaded module; return its outcome."""
    spec = importlib.util.spec_from_file_location('import_closure_under_probe', TESTS)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    stream = io.StringIO()
    return unittest.TextTestRunner(stream=stream, verbosity=0).run(
        unittest.TestLoader().loadTestsFromName(test, module))


def judge(outcome, marker):
    if outcome.wasSuccessful() and not outcome.skipped:
        return 'SURVIVED', 'bound test still passed'
    if outcome.errors:
        return 'INVALID', 'bound test errored instead of failing at its assertion'
    failures = ''.join(traceback for _test, traceback in outcome.failures)
    if marker in failures:
        return 'KILLED', 'bound test failed at its mutant-binding assertion'
    return 'INVALID', 'bound test failed without its mutant-binding marker'


def check_anchors():
    """Validate every anchor and mutant without entering the tree lock."""
    for probe, path, _test, pristine, mutant, _marker in GUARD_PROBES:
        source = path.read_text()
        if source.count(pristine) != 1:
            raise SystemExit(f'#1139 stale probe anchor: {probe}')
        ast.parse(source.replace(pristine, mutant))
    print(f'PASS: {len(GUARD_PROBES)} probe anchors unique, mutants parse', flush=True)
    return 0


def main():
    if '--check-anchors' in sys.argv[1:]:
        return check_anchors()
    # An outer timeout's SIGTERM/SIGHUP must unwind through the finally
    # clauses instead of dying with the sources left mutated (#1139).
    def terminate(signum, _frame):
        raise SystemExit(128 + signum)

    for received in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(received, terminate)
    results = []
    # The guard covers the two mutated runners and the bound test file; the
    # shared #1166 scanner inside it refuses any dirty workspace input, so
    # results generated from a dirty tree prove nothing about this HEAD.
    with mutating_sources(REPO, [RECORDER, RUN_REGISTRY, TESTS]):
        for probe, path, test, pristine, mutant, marker in GUARD_PROBES:
            row = dict(probe=probe, test=f'test_import_closure.py::{test}',
                       assertion=marker, status='INVALID', reason='not run')
            try:
                source = path.read_text()
                if source.count(pristine) != 1:
                    raise RuntimeError(f'#1139 stale probe anchor: {probe}')
                baseline = run_bound_test(test)
                if not baseline.wasSuccessful() or baseline.skipped:
                    raise RuntimeError('#1139 bound test failed or skipped before mutation')
                try:
                    path.write_text(source.replace(pristine, mutant))
                    row['status'], row['reason'] = judge(run_bound_test(test), marker)
                finally:
                    path.write_text(source)
            except Exception as error:
                row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
            try:
                restored = run_bound_test(test)
                if not restored.wasSuccessful() or restored.skipped:
                    raise RuntimeError('restored bound test failed or skipped')
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

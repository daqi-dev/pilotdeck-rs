#!/usr/bin/env python3
# anchor-selfcheck: unsupported #1037 temporary-file runner fault injection, no product mutations
"""Exercise the real three suite entrypoints without cargo, Node, or Git writes.

--negative-controls replaces each imported invalidate_completed with a no-op
in memory, runs every mutant, and requires the same regression tests to fail.
The simulated source guard only owns TemporaryDirectory files; it is never a
replacement for the HEAD guard when running product mutations.
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
import unittest
from unittest.mock import patch

FIXTURES = Path(__file__).resolve().parents[1]
sys.dont_write_bytecode = True
sys.path.insert(0, str(FIXTURES))


def load_suite(name):
    spec = importlib.util.spec_from_file_location(
        name.replace('-', '_'), FIXTURES / name / 'negative.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


MODULES = {name: load_suite(name) for name in
           ('mcp-http-encoding', 'regeneration-masks', 'node-provenance')}


class RestoreTests(unittest.TestCase):
    def exercise(self, fault):
        module = MODULES[self.suite_name]
        compile_only = self.suite_name == 'mcp-http-encoding'
        guard_failure = fault in ('enter', 'exit-restore', 'exit-verify', 'mid-restore')
        with tempfile.TemporaryDirectory(prefix='pdrs-1037-') as directory, \
                contextlib.ExitStack() as stack:
            root = Path(directory)
            output = root / 'evidence'
            snapshots = {}
            selected = []
            if compile_only:
                selected = [(name, root / f'source-{index}.rs', old, new, test, assertion)
                            for index, (name, _, old, new, test, assertion)
                            in enumerate(module.controls)]
                for _, path, old, _, _, _ in selected:
                    snapshots[path] = old.encode()
                stack.enter_context(patch.object(module, 'root', root))
                stack.enter_context(patch.object(module, 'controls', selected))
                names = [row[0] for row in selected]
                paths = [row[1] for row in selected]
            else:
                stack.enter_context(patch.object(module, 'ROOT', root))
                selected = module.ROWS
                names = [row['name'] for row in selected]
                paths = [root / row.get('path', getattr(module, 'MASKS', ''))
                         for row in selected]
                for row, path in zip(selected, paths):
                    snapshots[path] = snapshots.get(path, b'') + row['old'].encode() + b'\n'
            for path, data in snapshots.items():
                self.assertTrue(path.is_relative_to(root), '#1037 only temporary sources may be written')
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)

            calls = []
            completed = []
            injected = False
            write_bytes = Path.write_bytes
            invalidate = module.invalidate_completed

            def invalidate_and_capture(report_rows, reason):
                completed.extend(dict(row) for row in report_rows)
                invalidate(report_rows, reason)

            @contextlib.contextmanager
            def guard(actual_root, requested):
                self.assertEqual(actual_root, root, '#1037 guard must own the temporary root')
                self.assertEqual({root / path for path in requested}, set(snapshots),
                                 '#1037 guard must own exactly the temporary sources')
                if fault == 'enter':
                    raise RuntimeError('#1037 injected guard enter failure')
                try:
                    yield {Path(path): snapshots[root / path] for path in requested}
                finally:
                    for path, data in snapshots.items():
                        write_bytes(path, data)
                    if fault == 'exit-restore':
                        raise OSError('#1037 injected guard restore failure')
                    if fault == 'exit-verify':
                        raise RuntimeError('#1037 injected guard verify failure')

            def restore(path, data):
                nonlocal injected
                if (fault == 'mid-restore' and not injected and path == paths[1]
                        and data == snapshots[path] and len(calls) == (2 if compile_only else 5)):
                    injected = True
                    raise OSError('#1037 injected mid restore failure')
                return write_bytes(path, data)

            def process(index, stage):
                row, path = selected[index], paths[index]
                old, new = (row[2], row[3]) if compile_only else (row['old'], row['new'])
                expected = snapshots[path]
                if stage == 'negative':
                    expected = expected.replace(old.encode(), new.encode(), 1)
                self.assertEqual(path.read_bytes(), expected,
                                 '#1037 simulated process must observe the actual mutation phase')
                calls.append((index, stage))
                if compile_only:
                    return subprocess.CompletedProcess(
                        [], 1 if fault == 'compile-failure' and index == 0 else 0,
                        'temporary compile result\n', '')
                test = row['test']
                if row.get('runner', 'cargo') == 'node':
                    log = (f"not ok - {test}\n{row['anchors'][0]}\n" if stage == 'negative'
                           else f'ok - {test}\n')
                elif stage == 'negative':
                    log = (f'test {test} ... FAILED\ntest result: FAILED\n'
                           f"panicked at fixture: {row['anchors'][0]}\n")
                else:
                    log = f'test {test} ... ok\ntest result: ok. 1 passed; 0 failed;\n'
                return subprocess.CompletedProcess([], 101 if stage == 'negative' else 0, log, '')

            if compile_only:
                stack.enter_context(patch.object(module, 'run_owned',
                                                 side_effect=lambda *_a, **_k: process(len(calls), 'negative')))
                stack.enter_context(patch.object(module, 'wait_for_slot',
                                                 return_value=subprocess.CompletedProcess([], 0, '', '')))
            elif self.suite_name == 'regeneration-masks':
                def run(test, _env, _output, label):
                    index = next(i for i, row in enumerate(selected) if label.startswith(row['name'] + '-'))
                    self.assertEqual(test, selected[index]['test'], '#1037 exact bound test must be preserved')
                    return process(index, label.rsplit('-', 1)[1])
                stack.enter_context(patch.object(module, 'run', side_effect=run))
            else:
                stack.enter_context(patch.object(module, 'run', side_effect=lambda row, _env, _out, stage:
                                                 process(selected.index(row), stage)))
            stack.enter_context(patch.object(module, 'mutating_sources', new=guard))
            stack.enter_context(patch.object(module, 'invalidate_completed', new=invalidate_and_capture))
            stack.enter_context(patch.object(Path, 'write_bytes', new=restore))
            stack.enter_context(patch.dict(os.environ, {
                'CARGO_TARGET_DIR': str(root / 'unused-target'), 'PDRS_BASELINE': str(root / 'unused-baseline')}))
            args = ['negative.py', '--compile-only', str(output)] if compile_only else ['negative.py', str(output)]
            stack.enter_context(patch.object(sys, 'argv', args))
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            code = module.main()
            summary = output / ('compile-summary.json' if compile_only else 'summary.json')
            report = json.loads(summary.read_text())
            report_rows = report if compile_only else report['controls']
            self.assertEqual([row.get('mutation', row.get('name')) for row in report_rows], names,
                             '#1037 every selected row must be accounted for in order')
            expected_calls = (0 if fault == 'enter' else (2 if compile_only else 5)
                              if fault == 'mid-restore' else len(selected) * (1 if compile_only else 3))
            self.assertEqual(len(calls), expected_calls, '#1037 fault must occur at the requested execution point')
            for path, data in snapshots.items():
                self.assertEqual(path.read_bytes(), data, '#1037 temporary sources must be restored')
            success = 'COMPILED' if compile_only else 'KILLED'
            expected_statuses = (['INVALID'] * len(selected) if guard_failure else
                                 ['INVALID'] + [success] * (len(selected) - 1)
                                 if fault == 'compile-failure' else [success] * len(selected))
            self.assertEqual([row['status'] for row in report_rows],
                             expected_statuses,
                             '#1037 row status must preserve only valid completed results')
            self.assertEqual(code, int(bool(fault)), '#1037 guard failure must return nonzero; healthy must pass')
            if compile_only:
                self.assertEqual([row['compiled'] for row in report_rows],
                                 [status == 'COMPILED' for status in expected_statuses],
                                 '#1037 guard failure must revoke every compiled flag')
            else:
                self.assertEqual(report['counts'], {
                    'KILLED': 0 if fault else len(selected), 'SURVIVED': 0,
                    'INVALID': len(selected) if fault else 0}, '#1037 counts must match the final rows')
            if guard_failure:
                expected_completed = 0 if fault == 'enter' else 1 if fault == 'mid-restore' else len(selected)
                self.assertEqual(len(completed), expected_completed, '#1037 completed rows must precede guard failure')
                self.assertTrue(all(row['status'] == success for row in completed),
                                '#1037 completed rows must be green before invalidation')
                detail = {'enter': 'guard enter', 'exit-restore': 'guard restore',
                          'exit-verify': 'guard verify', 'mid-restore': 'mid restore'}[fault]
                for row in report_rows:
                    self.assertIn(f'#1037 injected {detail} failure', row['reason'],
                                  '#1037 every row must retain the guard failure reason')
                for before, after in zip(completed, report_rows):
                    self.assertTrue(after['reason'].startswith(before['reason'] + '; source guard:'),
                                    '#1037 invalidation must preserve the prior diagnostic')

    def test_healthy(self):
        self.exercise(None)

    def test_guard_enter(self):
        self.exercise('enter')

    def test_guard_exit_restore(self):
        self.exercise('exit-restore')

    def test_guard_exit_verify(self):
        self.exercise('exit-verify')

    def test_mid_restore(self):
        self.exercise('mid-restore')


class McpRestoreTests(RestoreTests):
    suite_name = 'mcp-http-encoding'

    def test_compile_failure_continues(self):
        self.exercise('compile-failure')


class MasksRestoreTests(RestoreTests):
    suite_name = 'regeneration-masks'


class NodeRestoreTests(RestoreTests):
    suite_name = 'node-provenance'


TEST_CLASSES = (McpRestoreTests, MasksRestoreTests, NodeRestoreTests)


def tests(test_class):
    return unittest.defaultTestLoader.loadTestsFromTestCase(test_class)


def negative_controls():
    results = []
    variants = [(test_class, False) for test_class in TEST_CLASSES] + [(McpRestoreTests, True)]
    for test_class, status_mutant in variants:
        name = test_class.suite_name
        mutation = name + ('-compile-status-always-green' if status_mutant else '-invalidation-noop')
        try:
            # Python-only mutants: all replacements stay in memory, no source edits.
            source = (FIXTURES / name / 'negative.py').read_text()
            expected = {'test_guard_exit_restore', 'test_guard_exit_verify', 'test_mid_restore'}
            if status_mutant:
                anchor = "status='COMPILED' if run.returncode == 0 else 'INVALID'"
                if source.count(anchor) != 1:
                    raise ValueError('#1037 stale compile-status anchor; expected exactly one')
                source = source.replace(anchor, "status='COMPILED'", 1)
                mutant = load_suite(name)
                exec(compile(source, name, 'exec'), mutant.__dict__)
                replacement = patch.dict(MODULES, {name: mutant})
                expected = {'test_compile_failure_continues'}
            else:
                compile(source, name, 'exec')
                replacement = patch.object(MODULES[name], 'invalidate_completed', new=lambda *_: None)
            stream = io.StringIO()
            with replacement:
                result = unittest.TextTestRunner(stream=stream, verbosity=2).run(tests(test_class))
            print(stream.getvalue(), end='')
            observed = {test._testMethodName for test, _ in result.failures}
            killed = (result.testsRun == tests(test_class).countTestCases() and not result.errors and observed == expected
                      and all('#1037 row status must preserve only valid completed results' in trace
                              for _, trace in result.failures))
            results.append({'mutation': mutation,
                            'status': 'KILLED' if killed else 'SURVIVED' if result.wasSuccessful() else 'INVALID',
                            'reason': f'expected failures={sorted(expected)}, observed={sorted(observed)}'})
        except Exception as error:
            results.append({'mutation': mutation, 'status': 'INVALID',
                            'reason': f'{type(error).__name__}: {error}'})
    print(json.dumps(results, indent=2))
    return int(any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    if sys.argv[1:] == ['--negative-controls']:
        raise SystemExit(negative_controls())
    if sys.argv[1:]:
        raise SystemExit('usage: test_restore.py [--negative-controls]')
    result = unittest.TextTestRunner(verbosity=2).run(
        unittest.TestSuite(tests(test_class) for test_class in TEST_CLASSES))
    raise SystemExit(not result.wasSuccessful())

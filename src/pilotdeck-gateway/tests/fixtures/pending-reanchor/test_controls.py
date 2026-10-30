#!/usr/bin/env python3
# anchor-selfcheck: unsupported synthetic pending-reanchor registry and executor tests
"""#1048: disposable text inputs and mocked cargo; never a product baseline."""
from contextlib import contextmanager, redirect_stdout
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import anchor_selfcheck as checker
import pending_reanchor as pending
import negative_suite as suite
from _shared import negative_control as adaptive

LABEL = '#1048'
TEST = 'bound_test'


def output(killed=False):
    # #818: the killed shape must be the libtest output the shared classifier
    # reads: a `running 1 test` header, the named test's own FAILED line, and
    # its `thread '<name>' panicked at` block carrying the assertion message.
    text = (f'running 1 test\ntest {TEST} ... FAILED\n\nfailures:\n'
            f'---- {TEST} stdout ----\n'
            f"thread '{TEST}' panicked at source.rs:1:1:\n"
            '#1048 bound assertion\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
            '\nfailures:\n'
            f'    {TEST}\n'
            '\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured;\n'
            if killed else
            f'test {TEST} ... ok\ntest result: ok. 1 passed; 0 failed;\n')
    return subprocess.CompletedProcess([], 101 if killed else 0, text, '')


class Controls(unittest.TestCase):
    def setUp(self):
        scratch = Path(__file__).resolve().parents[5] / '_tmp'
        scratch.mkdir(exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(prefix='pdrs-pending-reanchor-', dir=scratch)
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name).resolve()
        self.fixtures = self.root / pending.FIXTURES
        self.fixtures.mkdir(parents=True)
        self.script = self.fixtures / 'sample/negative.py'
        self.script.parent.mkdir()
        self.source = self.root / 'src/example.rs'
        self.source.write_text('before\n')
        self.script.write_text("from pathlib import Path\n"
            "from negative_suite import run_cases\n"
            "root = Path(__file__).resolve().parents[5]\n"
            "source = root / 'src/example.rs'\n"
            "mutants = [('sample', source, 'before', 'after')]\n"
            "if __name__ == '__main__':\n    run_cases()\n")
        self.registry = self.fixtures / pending.REGISTRY
        hosts = patch.object(pending, 'TEXT_HOSTS', pending.TEXT_HOSTS | {'sample/negative.py'})
        hosts.start()
        self.addCleanup(hosts.stop)

    def register(self, name='sample', fixture='sample/negative.py'):
        self.registry.write_text(f'{fixture} · {name} · #1031 · #1048 · 2026-10-29\n')

    def scan(self):
        with patch.dict(checker.UNSUPPORTED_SIGNALS, {}, clear=True), \
                patch.object(checker, 'tracked_fixtures', return_value=set()), \
                redirect_stdout(io.StringIO()) as stream:
            code = checker.check(self.root, anchors_only=True, environ={})
        return code, stream.getvalue()

    def test_scanner_four_cells(self):
        for registered in (False, True):
            for drift in (False, True):
                with self.subTest(registered=registered, drift=drift):
                    self.registry.write_text('')
                    if registered:
                        self.register()
                    self.source.write_text('drift\n' if drift else 'before\n')
                    code, text = self.scan()
                    self.assertEqual(code, int(drift and not registered), f'{LABEL} downgrade\n{text}')
                    self.assertIn(f'pending-reanchor={int(registered)}', text, f'{LABEL} count')
                    self.assertEqual('WARN ' in text, registered, f'{LABEL} warning')
                    self.assertEqual('ERROR ' in text, drift and not registered, f'{LABEL} error')
                    if registered and not drift:
                        self.assertIn('remove registration', text, f'{LABEL} stale registration')

    def test_wildcard_and_fixture_boundary(self):
        self.register('*')
        self.source.write_text('drift\n')
        code, text = self.scan()
        self.assertEqual(code, 0, f'{LABEL} wildcard: {text}')
        entries = pending.load(self.root)
        self.assertTrue(entries[0].matches('sample/negative.py', 'anything'), f'{LABEL} wildcard name')
        self.assertFalse(entries[0].matches('other/negative.py', 'sample'), f'{LABEL} fixture boundary')
        self.assertEqual(pending.for_script(self.root, self.fixtures / 'other.py'), [], f'{LABEL} host boundary')
        with self.assertRaises(ValueError, msg=f'{LABEL} ambiguous host identity'):
            pending.for_script(self.root, self.root / 'outside.py')
        with patch.object(sys, 'argv', [str(self.script)]):
            self.assertEqual(pending.for_script(self.root), entries, f'{LABEL} CLI identity')

    def test_multiple_anchors_count_once_and_warn_only_when_all_match(self):
        self.register()
        self.script.write_text(self.script.read_text().replace(
            "mutants = [('sample', source, 'before', 'after')]",
            "mutants = [('sample', source, 'before', 'after'), "
            "('sample', source, 'second', 'changed')]"))
        for text, healthy in (('before second', True), ('before drift', False), ('before before second', False)):
            self.source.write_text(text)
            code, report = self.scan()
            self.assertEqual(code, 0, f'{LABEL} registered multi-anchor downgrade\n{report}')
            self.assertIn('pending-reanchor=1', report, f'{LABEL} count mutations not anchors')
            self.assertEqual('remove registration' in report, healthy, f'{LABEL} all anchors must match')
        self.script.write_text(self.script.read_text().replace("('sample', source, 'second'", "('second', source, 'second'"))
        self.register('*')
        code, report = self.scan()
        self.assertEqual(code, 0, f'{LABEL} wildcard covers both mutations')
        self.assertIn('pending-reanchor=2', report, f'{LABEL} wildcard counts each mutation')

    def test_bad_registry_fails_closed(self):
        self.register()
        original = self.registry.read_text()
        broken = [original.replace('sample/negative.py', '../negative.py'),
                  original.replace('sample/negative.py', '/tmp/negative.py'),
                  original.replace('sample/negative.py', 'sample//negative.py'),
                  original.replace('sample/negative.py', 'missing.py'),
                  original.replace('#1031', '1031'), original.replace('#1048', '#0'),
                  original.replace('2026-10-29', '2026-02-30'),
                  original.replace(' · sample · ', ' ·  · '), original + original,
                  original + original.replace(' · sample · ', ' · * · '),
                  original.replace(' · sample · ', ' · bad*name · ')]
        for value in broken:
            with self.subTest(value=value):
                self.registry.write_text(value)
                code, text = self.scan()
                self.assertEqual(code, 1, f'{LABEL} malformed registration\n{text}')
                self.assertIn('ERROR pending-reanchor.txt:', text, f'{LABEL} registry diagnostic')
        self.registry.write_text(original)
        self.assertEqual(self.scan()[0], 0, f'{LABEL} restored registry')

    def test_unknown_mutation_and_switches_are_errors(self):
        self.register('unknown')
        self.assertEqual(self.scan()[0], 1, f'{LABEL} unknown mutation')
        self.register('*')
        self.script.write_text(self.script.read_text().replace(
            "mutants = [('sample', source, 'before', 'after')]",
            "switches = [('sample', source, 'bound_test', '#1048 assertion')]"))
        for source in ('drift', 'crate::mutants::on("sample")'):
            self.source.write_text(source)
            with patch.object(checker, 'audit_runtime_switches'):
                code, text = self.scan()
            self.assertEqual(code, 1, f'{LABEL} compiled switch never exempt\n{text}')
            self.assertIn('does not apply to compiled switches', text, f'{LABEL} switch diagnostic')
            self.assertIn('pending-reanchor=0', text, f'{LABEL} switch count')

    def test_unwired_host_cannot_downgrade(self):
        self.register()
        self.source.write_text('drift')
        # A dead call or copied import cannot enroll an arbitrary host.
        with patch.object(pending, 'TEXT_HOSTS', set()):
            code, report = self.scan()
            self.assertEqual(code, 1, f'{LABEL} unwired host fails closed\n{report}')
            self.assertIn('requires a shared skip entry', report, f'{LABEL} unwired diagnostic')
            self.assertIn('pending-reanchor=0', report, f'{LABEL} unwired not pending')
        for relative in ('mcp-http-encoding/negative.py', 'skills-external/negative_controls.py',
                         'pump-order-cases/negative_controls.py', 'turn-gates/negative.py',
                         'turn-runner-tail/negative_controls.py', 'interaction-bus/negative_controls.py',
                         'tool-budget-cwd/negative_controls.py', 'tool-budget-cwd/session_negative.py',
                         'turn-prompt-hooks/negative_controls.py', 'turn-start-notices/negative_controls.py'):
            self.assertFalse(pending.supports_host(relative), f'{LABEL} earlier host path')
        self.assertEqual(self.scan()[0], 0, f'{LABEL} shared host restored')

    @contextmanager
    def guard(self, root, paths, **kwargs):
        # Only synthetic scratch inputs. Production runners still use the real HEAD guard.
        self.assertEqual(root, self.root, f'{LABEL} guard root')
        originals = {path: path.read_bytes() for path in paths if path.is_file()}
        try:
            yield originals
        finally:
            for path, contents in originals.items():
                self.assertEqual(path.read_bytes(), contents, f'{LABEL} restoration {path}')

    def run_cases(self, registered, drift, *, mixed=False, guard=None):
        if registered:
            self.register()
        self.source.write_text('drift' if drift else 'before')
        out = self.root / 'logs'
        out.mkdir()
        rows = [suite.Case('sample', self.source, 'before', 'after', TEST, '#1048 bound assertion')]
        if mixed:
            rows.append(suite.Case('second', self.source, self.source.read_text(), 'second', TEST,
                                   '#1048 bound assertion'))
        calls = []

        def invoke(*args, **kwargs):
            calls.append(self.source.read_text())
            return output(self.source.read_text() in ('after', 'second'))

        def replace(text, scope, replacements):
            old, new = replacements[0]
            if text.count(old) != 1:
                raise ValueError('#1048 stale anchor')
            return text.replace(old, new)

        with patch.object(suite, 'mutating_sources', side_effect=guard or self.guard) as guarded, \
                patch.object(suite, 'wait_for_slot', return_value=output()), \
                patch.object(suite, 'run_owned', side_effect=invoke), redirect_stdout(io.StringIO()):
            code = suite.run_cases(self.root, rows, out, {}, replace, script=self.script)
        self.assertTrue(guarded.called, f'{LABEL} HEAD guard retained')
        summary = json.loads((out / 'summary.json').read_text())
        return code, summary, calls

    def test_host_four_cells(self):
        for registered in (False, True):
            for drift in (False, True):
                with self.subTest(registered=registered, drift=drift):
                    # A fresh scratch root/output for each independent cell.
                    self.setUp()
                    code, summary, calls = self.run_cases(registered, drift)
                    expected = pending.SKIPPED if registered else ('INVALID' if drift else 'KILLED')
                    self.assertEqual(summary['controls'][0]['status'], expected, f'{LABEL} skip status')
                    self.assertEqual(code, int(registered or drift), f'{LABEL} skip exit')
                    self.assertEqual(summary['counts']['KILLED'], int(not registered and not drift),
                                     f'{LABEL} skipped never killed')
                    if registered:
                        self.assertEqual(calls, [], f'{LABEL} skip before mutation and cargo')
                        self.assertEqual(summary['counts']['pending-reanchor'], 1, f'{LABEL} skip count')

    def test_mixed_suite_continues_and_fails(self):
        code, summary, calls = self.run_cases(True, True, mixed=True)
        self.assertEqual(code, 1, f'{LABEL} mixed exit')
        self.assertEqual(summary['counts'], {'KILLED': 1, 'INVALID': 0, 'SURVIVED': 0,
                                            'pending-reanchor': 1}, f'{LABEL} mixed counts')
        self.assertEqual(calls, ['drift', 'second', 'drift'], f'{LABEL} successor executes')

    def test_skip_cannot_bypass_head_guard(self):
        def rejected(*args, **kwargs):
            raise RuntimeError('source differs from HEAD')
        code, summary, calls = self.run_cases(True, True, guard=rejected)
        self.assertEqual(code, 1, f'{LABEL} HEAD failure exit')
        self.assertEqual(summary['controls'][0]['status'], 'INVALID', f'{LABEL} HEAD still mandatory')
        self.assertEqual(calls, [], f'{LABEL} rejected guard no cargo')

    def test_matrix_skips_without_compilation_and_compile_only_fails(self):
        self.register('*')
        self.source.write_text('drift')
        for compile_only, denied in ((False, False), (True, False), (False, True), (True, True)):
            out = self.root / f'matrix-{compile_only}-{denied}'
            rows = [('sample', self.source, 'before', 'after', TEST, '#1048 assertion')]
            effect = RuntimeError('HEAD rejected') if denied else self.guard
            with patch.object(suite, 'mutating_sources', side_effect=effect) as guarded, \
                    patch.object(suite, 'run_owned', side_effect=AssertionError('#1048 skipped cargo')) as run, \
                    patch.dict(os.environ, {'CARGO_TARGET_DIR': str(self.root / 'target')}), \
                    redirect_stdout(io.StringIO()):
                code = suite.matrix_main(self.root, self.script, '', rows, passed_count=1,
                    child_count=0, paths=[self.source], args=[str(out)] + (['--compile-only'] if compile_only else []))
            self.assertTrue(guarded.called, f'{LABEL} matrix HEAD guard retained')
            self.assertEqual(code, 1, f'{LABEL} matrix skip exit')
            self.assertFalse(run.called, f'{LABEL} skipped matrix cargo')
            document = json.loads((out / 'summary.json').read_text())
            self.assertEqual(document['counts'].get('pending-reanchor', 0), int(not denied),
                             f'{LABEL} matrix skip count')
            self.assertEqual(document['controls'][0]['status'], 'INVALID' if denied else pending.SKIPPED,
                             f'{LABEL} matrix skip status')
            self.assertFalse(document['controls'][0]['compiled'], f'{LABEL} skipped not compiled')

    def test_adaptive_skips_without_build_or_mutation(self):
        self.register()
        self.source.write_text('drift')
        rows = [('sample', self.source, 'before', 'after', [TEST])]
        @contextmanager
        def scratch(*args, **kwargs):
            yield self.root
        for denied in (False, True):
            out = self.root / f'adaptive-{denied}'
            effect = RuntimeError('HEAD rejected') if denied else self.guard
            with patch.object(adaptive, 'REPO', self.root), \
                    patch.object(adaptive, 'mutating_sources', side_effect=effect) as guarded, \
                    patch.object(adaptive, 'scratch', new=scratch), \
                    patch.object(adaptive, 'persistent_environment', return_value={'CARGO_TARGET_DIR': 'test-double'}), \
                    patch.object(adaptive, 'source_copy', return_value=self.root), \
                    patch.object(adaptive.CargoTests, 'run', side_effect=AssertionError('#1048 skipped build')) as run, \
                    redirect_stdout(io.StringIO()):
                code = adaptive.mutation_suite('test', rows, out, None, None, None, script=self.script)
            self.assertTrue(guarded.called, f'{LABEL} adaptive HEAD guard retained')
            self.assertEqual(code, 1, f'{LABEL} adaptive skip exit')
            self.assertFalse(run.called, f'{LABEL} skipped adaptive cargo')
            document = json.loads((out / 'summary.json').read_text())
            self.assertEqual(document['controls'][0]['status'], 'INVALID' if denied else pending.SKIPPED,
                             f'{LABEL} adaptive skip status')
            self.assertEqual(document['counts'].get('pending-reanchor', 0), int(not denied),
                             f'{LABEL} adaptive skip count')


if __name__ == '__main__':
    unittest.main(verbosity=2)

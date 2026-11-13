# anchor-selfcheck: unsupported isolated roster protocol test, no source replacement table
"""#1642 fix-r2: the frozen roster refuses with rc 2 (never started).

A drifted EXPECTED_NAMES no longer exits with the bare `raise SystemExit(str)`
rc 1: the shared negative_selection.registered + refuse pair keeps roster
mismatch on the refusal side of the four-way exit-code table.

#1642 fix-r3 additions: the guard-failure phases (a guard that never started
refuses with rc 2 and no evidence; a guard that fails after the body ran is
a red run with every row revoked) and the startup refusals the four-way
table promises (unknown name, existing output directory, unusable
CARGO_TARGET_DIR) are pinned the same way native-management pins them.
"""
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from contextlib import contextmanager, redirect_stderr
from pathlib import Path
from unittest.mock import patch

FIXTURES = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('registry_switch_controls',
                                              Path(__file__).with_name('negative_controls.py'))
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class Roster(unittest.TestCase):
    def test_roster_drift_refuses_with_rc2(self):
        source = Path(RUNNER.__file__).read_text()
        pattern = "'registry-receipt-not-published',\n)"
        self.assertTrue(source.count(pattern) == 1,
                        '#1642 fix-r2 the roster drift probe must replace exactly one EXPECTED_NAMES entry')
        drifted = source.replace(pattern, "'registry-receipt-not-published-renamed',\n)", 1)
        # #1642 fix-r3 (P1-1): the drifted copy sits beside the original
        # negative_controls.py, not in a shallow TemporaryDirectory. The copy
        # runs the module-level `ROOT = Path(__file__).resolve().parents[5]`
        # at import time, and a CI /tmp path (3 levels) died with an
        # IndexError before the roster refusal could even raise (job
        # 131861169897); beside the original the depth shape is identical by
        # construction. mkstemp keeps the name out of the tree's scans, the
        # finally unlinks it, and bytecode caching is off so the fixtures
        # directory gains no __pycache__ from the copy.
        descriptor, copy_name = tempfile.mkstemp(dir=Path(RUNNER.__file__).parent,
                                                 prefix='drifted-1642-', suffix='.py')
        copy = Path(copy_name)
        stderr = io.StringIO()
        previous = sys.dont_write_bytecode
        sys.dont_write_bytecode = True
        try:
            with os.fdopen(descriptor, 'w') as handle:
                handle.write(drifted)
            spec = importlib.util.spec_from_file_location('drifted_registry_switch_controls', copy)
            module = importlib.util.module_from_spec(spec)
            with redirect_stderr(stderr):
                code = None
                try:
                    spec.loader.exec_module(module)
                except SystemExit as refusal:
                    code = refusal.code
        finally:
            sys.dont_write_bytecode = previous
            copy.unlink(missing_ok=True)
        self.assertTrue(code == 2,
                        '#1642 fix-r2 roster drift is a refusal (rc 2, never started), '
                        'never a failed control (rc 1) or a silent pass')
        self.assertTrue('registry-receipt-not-published-renamed' in stderr.getvalue()
                        and 'roster' in stderr.getvalue(),
                        '#1642 fix-r2 the roster refusal must name the drift')

    # #1642 fix-r3 (review P1-3 + b): the phase rig for the guard-failure
    # paths. The run is --compile-only over a fake mutation whose bound test
    # body is handed in directly (the guarded-paths table main() hashes must
    # stay the real tree's, so the fake paths never enter it), so no product
    # source is touched and no ReceiptRun harness is needed to reach the
    # guarded window.
    def drive(self, guard_factory):
        with tempfile.TemporaryDirectory(prefix='pdrs-1642-roster-protocol-') as temporary:
            root = Path(temporary)
            source = root / 'fake-source.txt'
            source.write_text('original')
            out = root / 'result'

            def run_owned(command, **_kwargs):
                return subprocess.CompletedProcess(command, 0, '', '')

            # The in-fixture self-check main() runs first calls test_body with
            # its own synthetic texts; those calls see the real implementation,
            # and only the stale loop's real-tree read is faked away.
            real_body = RUNNER.test_body

            def fake_body(test, texts=None):
                if texts is not None:
                    return real_body(test, texts=texts)
                return 'fn fake() {\n    assert!(false, "#1642 bound");\n}\n'

            # The guard is built from drive's own source path, so the fake
            # originals mapping covers the selected path main() restores from.
            guard = guard_factory(source)

            fake_row = [('fake-registry-mutation', source, 'original', 'changed',
                         'native_process::tests::fake', '#1642 bound')]
            argv = ['negative_controls.py', '--compile-only', str(out),
                    'fake-registry-mutation']
            patches = [patch.object(RUNNER, 'mutations', fake_row),
                       patch.object(RUNNER, 'test_body', fake_body),
                       patch.object(RUNNER, 'mutating_sources', guard),
                       patch.object(RUNNER, 'run_owned', run_owned),
                       patch.object(RUNNER, 'wait_for_slot',
                                    return_value=subprocess.CompletedProcess([], 0, '', '')),
                       patch.object(sys, 'argv', argv),
                       patch.dict(os.environ, CARGO_TARGET_DIR=str(root / 'unused-target'))]
            for applied in patches:
                applied.start()
            try:
                try:
                    code = RUNNER.main()
                except SystemExit as refusal:
                    # A refusal (rc 2) or a delayed signal (rc 128+signum)
                    # leaves main() through SystemExit; the code is the
                    # process exit shape.
                    code = refusal.code
            finally:
                for applied in patches:
                    applied.stop()
            # The evidence is read inside the temporary directory's lifetime
            # (this with-block deletes the tree on return): `claimed` is the
            # enter-refusal question (was the evidence directory withdrawn?),
            # `summary` the publish question.
            claimed = out.exists()
            summary = None
            if claimed and (out / 'summary.json').is_file():
                summary = json.loads((out / 'summary.json').read_text())['controls']
            return code, claimed, summary

    def entered_guard(self, source, *, fail_exit=False, fail_enter=False):
        @contextmanager
        def guard(_root, paths):
            if fail_enter:
                raise RuntimeError('source lock busy')
            # main() hashes every guarded path and restores every selected
            # path out of this one mapping, so the guard hands back bytes
            # for both without reading the real tree.
            originals = {path: b'guarded-bytes' for path in paths}
            originals[source] = b'original'
            try:
                yield originals
            finally:
                if fail_exit:
                    raise RuntimeError('guard exit failed')
        return guard

    @contextmanager
    def signal_guard(self, source, _root, paths):
        originals = {path: b'guarded-bytes' for path in paths}
        originals[source] = b'original'
        yield originals
        raise SystemExit(143)

    def test_enter_phase_failure_is_a_refusal(self):
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            code, claimed, summary = self.drive(
                lambda source: self.entered_guard(source, fail_enter=True))
        self.assertTrue(code == 2,
                        '#1642 fix-r3 a guard that never started refuses with rc 2, '
                        'not the flat rc 1 the phase-less handler used to return')
        self.assertTrue('guard refused to start' in stderr.getvalue(),
                        '#1642 fix-r3 the enter-phase refusal must name the guard failure')
        self.assertFalse(claimed,
                        '#1642 fix-r3 an enter-phase refusal must withdraw the claimed '
                        'evidence directory (no summary, no leftover claim shape)')
        self.assertTrue(summary is None,
                        '#1642 fix-r3 an enter-phase refusal must not publish a summary')

    def test_exit_phase_failure_is_a_red_run(self):
        code, claimed, summary = self.drive(
            lambda source: self.entered_guard(source, fail_exit=True))
        self.assertTrue(code == 1,
                        '#1642 fix-r3 a guard that fails after the body ran is a red run (rc 1)')
        self.assertTrue(claimed,
                        '#1642 fix-r3 a red run keeps its evidence directory')
        self.assertTrue(all(row['status'] == 'INVALID' for row in summary),
                        '#1642 fix-r3 a failed guard revokes every row it recorded')
        self.assertTrue(all('source guard' in row['reason']
                            and 'guard exit failed' in row['reason'] for row in summary),
                        '#1642 fix-r3 the rows are revoked by the guard failure itself, '
                        'not by some earlier per-case error')

    # #1642 fix-r3 (review a9 shape, DS r2c#2): a SIGTERM deferred through the
    # restore window re-enters as SystemExit(128+signum) and must survive the
    # handler untouched -- no invalidate, no summary, no rewrite to rc 1.
    def test_deferred_signal_survives_the_guard_handler(self):
        with redirect_stderr(io.StringIO()):
            code, claimed, summary = self.drive(
                lambda source: lambda *arguments: self.signal_guard(source, *arguments))
        self.assertTrue(code == 143,
                        '#1642 fix-r3 a SIGTERM deferred through the restore window '
                        'exits 128+signum, not the rewritten rc 1')
        self.assertTrue(summary is None,
                        '#1642 fix-r3 a deferred signal publishes no summary over the '
                        'restored tree')

    # #1642 fix-r3 (review b, item 4): the startup refusals the four-way table
    # promises, driven in-process over the real mutation table.
    def refuse_with(self, argv, environment):
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            with patch.object(sys, 'argv', argv), \
                 patch.dict(os.environ, environment):
                with self.assertRaises(SystemExit) as raised:
                    RUNNER.main()
        return raised.exception.code, stderr.getvalue()

    def test_unknown_name_refuses_with_rc2(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1642-roster-protocol-') as temporary:
            out = Path(temporary) / 'result'
            code, stderr = self.refuse_with(
                ['negative_controls.py', str(out), 'no-such-mutation-1642'],
                {'CARGO_TARGET_DIR': str(Path(temporary) / 't')})
            self.assertTrue(code == 2,
                            '#1642 fix-r3 an unknown mutation name refuses with rc 2')
            self.assertTrue('no-such-mutation-1642' in stderr and 'unknown' in stderr,
                            '#1642 fix-r3 the refusal must name the unknown mutation')
            self.assertFalse(out.exists(),
                            '#1642 fix-r3 the refusal must not claim the evidence directory')

    def test_existing_output_directory_refuses_with_rc2(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1642-roster-protocol-') as temporary:
            out = Path(temporary) / 'result'
            out.mkdir()
            code, stderr = self.refuse_with(
                ['negative_controls.py', '--compile-only', str(out)],
                {'CARGO_TARGET_DIR': str(Path(temporary) / 't')})
            self.assertTrue(code == 2,
                            '#1642 fix-r3 an existing output directory refuses with rc 2, '
                            'not the bare FileExistsError traceback it used to raise')
            self.assertTrue('already exists' in stderr,
                            '#1642 fix-r3 the refusal must say the directory is taken')
            self.assertEqual(list(out.iterdir()), [],
                            '#1642 fix-r3 the refusal must not write into another run\'s directory')

    def test_missing_target_refuses_with_rc2(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1642-roster-protocol-') as temporary:
            out = Path(temporary) / 'result'
            code, stderr = self.refuse_with(
                ['negative_controls.py', '--compile-only', str(out)],
                {'CARGO_TARGET_DIR': ''})
            self.assertTrue(code == 2,
                            '#1642 fix-r3 a blank CARGO_TARGET_DIR refuses with rc 2')
            self.assertTrue('CARGO_TARGET_DIR' in stderr,
                            '#1642 fix-r3 the refusal must name the unusable target')
            self.assertFalse(out.exists(),
                            '#1642 fix-r3 the refusal must not claim the evidence directory')

    # #1642 fix-r4 (review P1-E): test_body now matches on the shared
    # literal-blanked structure view and requires a `#[test]` family
    # attribute, so the three carriers that are not real code must each fail
    # the binding -- the same discrimination the in-fixture
    # check_binding_scope pins, mirrored here because --check reads it on
    # every run and this file is the one CI drives.
    def test_binding_scope_refuses_non_code_carriers(self):
        RUNNER.check_binding_scope()
        fixture = ('#[test]\n'
                    'fn target() {\n'
                    '    panic!("#1369 own marker");\n'
                    '    // panic!("#1369 comment carrier");\n'
                    '}\n')
        plain = ('fn target() {\n'
                 '    panic!("#1369 plain non-test carrier");\n'
                 '}\n')
        literal = ('#[test]\n'
                    'fn sibling() {\n'
                    '    assert!(false, "#1369 sibling marker");\n'
                    '}\n'
                    'fn documented() {\n'
                    '    let sample = r#"\n'
                    '#[test]\n'
                    'fn target() {\n'
                    '    panic!("#1369 literal carrier");\n'
                    '}\n'
                    '"#;\n'
                    '    let _ = sample;\n'
                    '}\n')

        def body(text):
            return RUNNER.test_body('native_process::tests::target', {RUNNER.TESTS: text})

        self.assertTrue('#1369 own marker' in body(fixture),
                        '#1642 fix-r4 the binding checker must keep its own assertion')
        self.assertTrue('#1369 comment carrier' not in body(fixture),
                        '#1642 fix-r4 a comment inside the body must not anchor the assertion')
        self.assertTrue(body(plain) == '',
                        '#1642 fix-r4 a same-named plain function must not stand in '
                        'for the deleted bound test')
        self.assertTrue(body(literal) == '',
                        '#1642 fix-r4 a `#[test] fn` only a raw string carries must '
                        'not stand in for the deleted bound test')


if __name__ == '__main__':
    unittest.main(verbosity=2)

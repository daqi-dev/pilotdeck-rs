# anchor-selfcheck: unsupported isolated runner protocol tests; no product source mutations
"""Exercise runner lease/error handling with temporary fake sources and processes."""
from contextlib import contextmanager
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('native_dispatch_controls', Path(__file__).with_name('negative_controls.py'))
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class Protocol(unittest.TestCase):
    def exercise(self, *, guard_error=False, compile_error=False, cancel=False, unrelated=False,
                 compile_only=False):
        with tempfile.TemporaryDirectory(prefix='pdrs-1305-protocol-') as temporary:
            root = Path(temporary)
            source = root / 'fake-source.txt'
            source.write_text('original')
            out = root / 'result'
            rows = [(name, source, 'original', 'changed', 'bound_test', '#1305 bound')
                    for name in ('first', 'second')]
            active = False
            restored = 0
            classified = 0
            compilations = 0
            pending_restore = False

            @contextmanager
            def guard(_root, _paths):
                nonlocal active
                active = True
                try:
                    yield {source: b'original'}
                finally:
                    # A failed runner must still attempt its per-case restore.
                    self.assertTrue(source.read_text() == 'original', '#1305 protocol restores source')
                    active = False
                    if guard_error:
                        raise RuntimeError('#1305 injected guard exit failure')

            def run(command, **kwargs):
                nonlocal restored, compilations, pending_restore
                self.assertTrue(active, '#1305 protocol cargo stays inside lease')
                self.assertFalse(any(key in kwargs['env'] for key in
                                     ('PDRS_NEGATIVE_RECEIPTS', 'PDRS_NEGATIVE_PARENT_TEST', 'PDRS_MUTANT')),
                                 '#1305 protocol discards inherited receipts')
                if '--no-run' in command:
                    compilations += 1
                    pending_restore = True
                    if cancel and compilations == 2:
                        raise KeyboardInterrupt('#1305 injected cancellation')
                    return subprocess.CompletedProcess(command, int(compile_error and compilations == 1), '', '')
                if source.read_text() == 'original' and pending_restore:
                    restored += 1
                    pending_restore = False
                return subprocess.CompletedProcess(command, 0,
                    'test bound_test ... ok\n\ntest result: ok. 1 passed; 0 failed;\n', '')

            def classify(_result, _case):
                nonlocal classified
                classified += 1
                return ('INVALID', 'wrong panic') if unrelated and classified == 1 else ('KILLED', 'bound assertion')

            argv = ['controls', str(out)] + (['--compile-only'] if compile_only else [])
            with patch.object(RUNNER, 'mutations', rows), \
                 patch.object(RUNNER, 'mutating_sources', guard), \
                 patch.object(RUNNER, 'run_owned', run), \
                 patch.object(RUNNER, 'wait_for_slot', return_value=subprocess.CompletedProcess([], 0, '', '')), \
                 patch.object(RUNNER, 'classify', classify), \
                 patch.object(sys, 'argv', argv), \
                 patch.dict(os.environ, CARGO_TARGET_DIR=str(root / 'unused-target'),
                            PDRS_NEGATIVE_RECEIPTS='stale', PDRS_NEGATIVE_PARENT_TEST='stale', PDRS_MUTANT='stale'):
                code = RUNNER.main()
            return code, json.loads((out / 'summary.json').read_text())['controls'], restored, compilations

    def test_restored_runs_inside_lease(self):
        code, rows, restored, compilations = self.exercise()
        self.assertTrue(code == 0 and all(row['status'] == 'KILLED' for row in rows),
                        '#1305 protocol successful batch')
        self.assertTrue(restored == 2 and compilations == 2, '#1305 protocol compiles and restores every case')

    def test_guard_exit_invalidates_completed(self):
        code, rows, _, _ = self.exercise(guard_error=True)
        self.assertTrue(code == 2 and len(rows) == 2, '#1305 protocol failed exit is nonzero and complete')
        self.assertTrue(all(row['status'] == 'INVALID' and not row['compiled'] for row in rows),
                        '#1305 protocol failed exit revokes every kill')

    def test_compile_failure_continues(self):
        code, rows, _, compilations = self.exercise(compile_error=True)
        self.assertTrue(code == 1 and compilations == 2, '#1305 protocol compile error continues batch')
        self.assertTrue([row['status'] for row in rows] == ['INVALID', 'KILLED'],
                        '#1305 protocol compile error is never a kill')

    def test_unrelated_panic_is_invalid(self):
        code, rows, _, _ = self.exercise(unrelated=True)
        self.assertTrue(code == 1 and [row['status'] for row in rows] == ['INVALID', 'KILLED'],
                        '#1305 protocol preserves shared wrong-panic verdict')

    def test_cancellation_invalidates_completed(self):
        code, rows, _, _ = self.exercise(cancel=True)
        self.assertTrue(code == 2 and len(rows) == 2, '#1305 protocol cancellation accounts for all cases')
        self.assertTrue(all(row['status'] == 'INVALID' and not row['compiled'] for row in rows),
                        '#1305 protocol cancellation revokes earlier kill')

    def test_compile_only_is_not_a_kill(self):
        code, rows, restored, compilations = self.exercise(compile_only=True)
        self.assertTrue(code == 0 and compilations == 2 and restored == 0,
                        '#1305 protocol compile-only compiles every case')
        self.assertTrue(all(row['status'] == 'COMPILED' for row in rows),
                        '#1305 protocol compile-only never claims kill')

    def test_duplicate_names_are_rejected(self):
        with patch.object(sys, 'argv', ['controls', 'unused', 'omit-runs', 'omit-runs']), \
             patch.dict(os.environ, CARGO_TARGET_DIR='unused'):
            with self.assertRaises(SystemExit, msg='#1305 protocol duplicate selection rejected') as raised:
                RUNNER.main()
        self.assertTrue(str(raised.exception) == '#1305 unknown/duplicate mutation name',
                        '#1305 protocol duplicate selection diagnostic')


if __name__ == '__main__':
    unittest.main(verbosity=2)

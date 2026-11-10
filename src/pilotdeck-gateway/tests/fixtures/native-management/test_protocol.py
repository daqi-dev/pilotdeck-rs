# anchor-selfcheck: unsupported isolated runner protocol tests; no product source mutations
"""Exercise the #1440 restored-inside-lease window with temporary fake sources."""
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

SPEC = importlib.util.spec_from_file_location('native_management_controls',
                                              Path(__file__).with_name('negative_controls.py'))
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class Protocol(unittest.TestCase):
    def exercise(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1440-protocol-') as temporary:
            root = Path(temporary)
            source = root / 'fake-source.txt'
            source.write_text('original')
            out = root / 'result'
            rows = [(name, source, 'original', 'changed', 'bound_test', '#1231 bound')
                    for name in ('first', 'second')]
            active = False
            restored = 0
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
                    self.assertTrue(source.read_text() == 'original', '#1440 protocol restores source')
                    active = False

            def run(command, **kwargs):
                nonlocal restored, compilations, pending_restore
                self.assertTrue(active, '#1440 protocol cargo stays inside the lease')
                if '--no-run' in command:
                    compilations += 1
                    pending_restore = True
                    return subprocess.CompletedProcess(command, 0, '', '')
                if source.read_text() == 'original' and pending_restore:
                    restored += 1
                    pending_restore = False
                return subprocess.CompletedProcess(command, 0,
                    'test bound_test ... ok\n\ntest result: ok. 1 passed; 0 failed;\n', '')

            def classify(_result, _case):
                return ('KILLED', 'bound assertion')

            argv = ['controls', str(out)]
            with patch.object(RUNNER, 'mutations', rows), \
                 patch.object(RUNNER, 'mutating_sources', guard), \
                 patch.object(RUNNER, 'run_owned', run), \
                 patch.object(RUNNER, 'wait_for_slot', return_value=subprocess.CompletedProcess([], 0, '', '')), \
                 patch.object(RUNNER, 'classify', classify), \
                 patch.object(sys, 'argv', argv), \
                 patch.dict(os.environ, CARGO_TARGET_DIR=str(root / 'unused-target')):
                code = RUNNER.main()
            return code, json.loads((out / 'summary.json').read_text())['controls'], restored, compilations

    def test_restored_runs_inside_lease(self):
        code, rows, restored, compilations = self.exercise()
        self.assertTrue(code == 0 and all(row['status'] == 'KILLED' for row in rows),
                        '#1440 protocol successful batch')
        self.assertTrue(restored == 2 and compilations == 2,
                        '#1440 protocol compiles and restores every case inside the lease')


if __name__ == '__main__':
    unittest.main(verbosity=2)

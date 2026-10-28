# anchor-selfcheck: unsupported unit tests of the mutation runner
"""Exercise scratch ownership and cleanup without compiling mutant artifacts."""
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


class NegativeLifecycleTests(unittest.TestCase):
    def exercise(self, failure=None):
        script = Path(__file__).with_name('negative.py')
        spec = importlib.util.spec_from_file_location('background_negative', script)
        negative = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(negative)
        scratch_paths = []
        processes = []
        temporary_directory = tempfile.TemporaryDirectory
        previous_handler = signal.getsignal(signal.SIGTERM)
        with temporary_directory() as fixture:
            fixture = Path(fixture)
            launcher_target = fixture / 'launcher-target'
            launcher_target.mkdir()
            sentinel = launcher_target / 'clean-artifact'
            sentinel.write_bytes(b'clean')

            def make_scratch(**kwargs):
                directory = temporary_directory(dir=fixture, **kwargs)
                scratch_paths.append(Path(directory.name))
                return directory

            def copy_sources(source, destination, **kwargs):
                # Real source anchors, only the two files needed by this mutation.
                for relative in ('pilotdeck-gateway/src/runtime/background.rs',
                                 'pilotdeck-gateway/src/runtime/background/output.rs'):
                    target = destination / relative
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes((source / relative).read_bytes())
                if failure == 'copy':
                    raise OSError('injected copy failure')

            class Process:
                pid = 123456

                def __init__(self, command, *, cwd, env, stdout, **kwargs):
                    target = Path(env['CARGO_TARGET_DIR'])
                    self.owner.assertEqual(target, Path(cwd).parent / 'target')
                    target.mkdir(exist_ok=True)
                    (target / 'mutant-artifact').write_bytes(b'mutant')
                    self.calls = 0
                    self.index = len(processes)
                    processes.append(self)
                    stdout.write('test result: FAILED\n' if self.index == 1 else 'test result: ok\n')

                def wait(self, timeout=None):
                    self.calls += 1
                    if self.index == 1 and self.calls == 1:
                        if failure == 'sigterm':
                            os.kill(os.getpid(), signal.SIGTERM)
                        if failure == 'interrupt':
                            raise KeyboardInterrupt()
                        if failure == 'wait':
                            raise RuntimeError('injected wait failure')
                    return 101 if self.index == 1 else 0

            Process.owner = self
            expected = OSError if failure == 'copy' else KeyboardInterrupt if failure in ('sigterm', 'interrupt') else RuntimeError
            with patch.object(sys, 'argv', [str(script), str(fixture / 'logs'), 'threshold-minus-one']), \
                 patch.dict(os.environ, {'CARGO_TARGET_DIR': str(launcher_target)}), \
                 patch.object(negative.tempfile, 'TemporaryDirectory', side_effect=make_scratch), \
                 patch.object(negative.shutil, 'copytree', side_effect=copy_sources), \
                 patch.object(negative.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, '', '')), \
                 patch.object(negative.subprocess, 'Popen', Process), \
                 patch.object(negative.os, 'killpg') as killpg, \
                 contextlib.redirect_stdout(io.StringIO()):
                if failure:
                    with self.assertRaises(expected):
                        negative.main()
                else:
                    negative.main()
            self.assertEqual(len(scratch_paths), 1)
            self.assertFalse(scratch_paths[0].exists(), 'scratch and mutant target must be deleted')
            self.assertEqual(list(launcher_target.iterdir()), [sentinel])
            self.assertEqual(sentinel.read_bytes(), b'clean')
            self.assertEqual(signal.getsignal(signal.SIGTERM), previous_handler)
            if failure in ('wait', 'sigterm', 'interrupt'):
                killpg.assert_called_once_with(Process.pid, signal.SIGKILL)
                self.assertEqual(processes[-1].calls, 2, 'interrupted child must be reaped')

    def test_success(self):
        self.exercise()

    def test_copy_exception(self):
        self.exercise('copy')

    def test_child_exception(self):
        self.exercise('wait')

    def test_sigterm(self):
        self.exercise('sigterm')

    def test_keyboard_interrupt(self):
        self.exercise('interrupt')


if __name__ == '__main__':
    unittest.main()

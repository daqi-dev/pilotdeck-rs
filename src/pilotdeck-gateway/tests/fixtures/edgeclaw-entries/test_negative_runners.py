#!/usr/bin/env python3
"""Exercise both real runners' slot retry, owned timeout and byte restoration.

Replaces the Cargo payload with a bounded child tree; no build or helper edit.
The runners still execute their first real mutation and their finally blocks.
"""
import errno
import os
from pathlib import Path
import runpy
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import negative_process

FIXTURES = Path(__file__).resolve().parent
ROOT = FIXTURES.parents[4]


class RunnerControls(unittest.TestCase):
    def check_runner(self, name):
        source = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw'
        originals = {p: p.read_bytes() for p in [source/'file_memory.rs',
                     source/'storage/entries.rs', source/'storage/accessors.rs']}
        owned = negative_process.run_owned
        wait = negative_process.wait_for_slot
        before_cwd = Path.cwd()
        calls = []
        with tempfile.TemporaryDirectory(prefix='pdrs-entry-runner-') as temp:
            temp = Path(temp)
            marker, pids_file = temp/'slot', temp/'pids'
            gate = temp/'gate.sh'
            gate.write_text(f'if [ ! -f "{marker}" ]; then touch "{marker}"; echo full; exit 1; fi\necho free\n')

            def wait_slot(script):
                self.assertEqual(Path(script), ROOT/'scripts/cargo-slots.sh')
                result = wait(gate, timeout=5, interval=0.02)
                self.assertEqual(result.stdout, 'full\nfree\n')
                calls.append('slot')
                return result

            child = "import time; print('ready',flush=True); time.sleep(20)"
            leader = ("import os,pathlib,subprocess,sys,time; "
                      f"p=subprocess.Popen([sys.executable,'-c',{child!r}],stdout=subprocess.PIPE,text=True); "
                      "assert p.stdout.readline().strip()=='ready'; "
                      "pathlib.Path(sys.argv[1]).write_text(str(os.getpid())+' '+str(p.pid)); "
                      "print('owned-ready',flush=True); time.sleep(20)")

            def payload(command, **kwargs):
                self.assertEqual(command[0], 'cargo')
                self.assertEqual(calls, ['slot'])
                calls.append('cargo')
                return owned([sys.executable, '-c', leader, str(pids_file)], timeout=1)

            try:
                with patch.object(sys, 'argv', [name, '--out', str(temp/'logs')]), \
                     patch.object(negative_process, 'wait_for_slot', side_effect=wait_slot), \
                     patch.object(negative_process, 'run_owned', side_effect=payload), \
                     patch.object(subprocess, 'run', side_effect=AssertionError('unowned subprocess.run')):
                    # wait_for_slot itself delegates to run_owned. Its native gate
                    # command must stay owned while the Cargo payload is replaced.
                    def dispatch(command, **kwargs):
                        return owned(command, **kwargs) if command[0] == 'bash' else payload(command, **kwargs)
                    with patch.object(negative_process, 'run_owned', side_effect=dispatch):
                        with self.assertRaises(subprocess.TimeoutExpired) as caught:
                            runpy.run_path(str(FIXTURES/name), run_name='__main__')
                self.assertIn('owned-ready', caught.exception.output)
                self.assertEqual(calls, ['slot', 'cargo'])
                pids = [int(p) for p in pids_file.read_text().split()]
                for pid in pids:
                    deadline = time.monotonic() + 5
                    while True:
                        try:
                            os.kill(pid, 0)
                        except OSError as error:
                            self.assertEqual(error.errno, errno.ESRCH)
                            break
                        self.assertLess(time.monotonic(), deadline, f'orphan PID {pid}')
                        time.sleep(0.02)
                print(f'{name}: retried full slot; owned PIDs {pids} gone (ESRCH); bytes restored', flush=True)
            finally:
                if pids_file.exists():
                    for pid in reversed([int(p) for p in pids_file.read_text().split()]):
                        try:
                            os.kill(pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                os.chdir(before_cwd)
                for path, content in originals.items():
                    self.assertEqual(path.read_bytes(), content, f'{name} left a mutation in {path}')

    def test_negative(self):
        self.check_runner('negative.py')

    def test_negative_fix1(self):
        self.check_runner('negative_fix1.py')


if __name__ == '__main__':
    unittest.main(verbosity=2)

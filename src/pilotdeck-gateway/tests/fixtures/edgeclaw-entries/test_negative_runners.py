#!/usr/bin/env python3
# anchor-selfcheck: unsupported unit tests of the mutation runners
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
        with negative_process.mutating_sources(ROOT, [source/'file_memory.rs',
                source/'storage/entries.rs', source/'storage/accessors.rs',
                source/'storage/overview.rs']) as snapshots:
            originals = {p: snapshots[p] for p in [source/'file_memory.rs',
                         source/'storage/entries.rs', source/'storage/accessors.rs',
                         source/'storage/overview.rs']}
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
                    self.assertTrue(Path(script) == ROOT/'scripts/cargo-slots.sh',
                                    '#1166 the slot gate must be the tree cargo-slots script')
                    result = wait(gate, timeout=5, interval=0.02)
                    self.assertTrue(result.stdout == 'full\nfree\n',
                                    '#1166 the slot gate must report full then free')
                    calls.append('slot')
                    return result

                child = "import time; print('ready',flush=True); time.sleep(20)"
                leader = ("import os,pathlib,subprocess,sys,time; "
                          f"p=subprocess.Popen([sys.executable,'-c',{child!r}],stdout=subprocess.PIPE,text=True); "
                          "assert p.stdout.readline().strip()=='ready'; "
                          "pathlib.Path(sys.argv[1]).write_text(str(os.getpid())+' '+str(p.pid)); "
                          "print('owned-ready',flush=True); time.sleep(20)")

                def payload(command, **kwargs):
                    self.assertTrue(command[0] == 'cargo',
                                    '#1166 the cargo test must be the payload command; got '
                                    + repr(command[0]))
                    self.assertTrue(calls == ['scanner', 'slot'],
                                    '#1166 the workspace scanner must run before slot reservation and cargo')
                    calls.append('cargo')
                    return owned([sys.executable, '-c', leader, str(pids_file)], timeout=1)

                try:
                    with patch.object(sys, 'argv', [name, '--out', str(temp/'logs')]), \
                         patch.object(negative_process, 'wait_for_slot', side_effect=wait_slot), \
                         patch.object(negative_process, 'run_owned', side_effect=payload), \
                         patch.object(subprocess, 'run', side_effect=AssertionError('unowned subprocess.run')):
                        # wait_for_slot itself delegates to run_owned. Its native gate
                        # command must stay owned while the Cargo payload is replaced.
                        # #1166: mutating_sources first runs the prebuilt workspace
                        # scanner through run_owned too; it is identified by its
                        # executable name and must precede the slot and cargo calls.
                        def dispatch(command, **kwargs):
                            if command[0] == 'bash':
                                return owned(command, **kwargs)
                            if Path(command[0]).name == 'workspace_inputs':
                                calls.append('scanner')
                                return owned(command, **kwargs)
                            return payload(command, **kwargs)
                        with patch.object(negative_process, 'run_owned', side_effect=dispatch):
                            with self.assertRaises(subprocess.TimeoutExpired) as caught:
                                runpy.run_path(str(FIXTURES/name), run_name='__main__')
                    self.assertIn('owned-ready', caught.exception.output,
                                  '#1166 the owned payload leader must report readiness')
                    self.assertTrue(calls == ['scanner', 'slot', 'cargo'],
                                    '#1166 the workspace scanner must run before slot reservation and cargo')
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

    def test_negative_ids(self):
        self.check_runner('negative_ids.py')

    def test_tree_lock_serializes(self):
        # A second runner must not read or rewrite the source while the first
        # one owns the tree lock. A marker file stands in for entries.rs so the
        # probe never touches production bytes.
        child = """\
import pathlib, sys, time
sys.path.insert(0, sys.argv[1])
from negative_process import tree_lock
p = pathlib.Path(sys.argv[2])
with tree_lock(sys.argv[3]):
    p.write_text('mutated')
    print('mutated', flush=True)
    time.sleep(1.2)
    p.write_text('clean')
"""
        with tempfile.TemporaryDirectory(prefix='pdrs-entry-lock-') as temp:
            marker = Path(temp)/'source'
            marker.write_text('clean')
            root = Path(temp)/'tree'
            root.mkdir()
            holder = subprocess.Popen(
                [sys.executable, '-c', child, str(FIXTURES.parent), str(marker), str(root)],
                stdout=subprocess.PIPE, text=True, start_new_session=True)
            try:
                self.assertEqual(holder.stdout.readline().strip(), 'mutated')
                start = time.monotonic()
                with negative_process.tree_lock(root, timeout=5):
                    self.assertEqual(marker.read_text(), 'clean')
                self.assertGreaterEqual(time.monotonic()-start, 0.5)
                holder.wait(timeout=5)
            finally:
                if holder.poll() is None:
                    holder.kill()
                holder.wait(timeout=5)
                holder.stdout.close()
                # The lock is a persistent flock file. Never unlink it here:
                # a live external holder may still own that inode, and a new
                # opener would otherwise create a second "exclusive" lock.


if __name__ == '__main__':
    unittest.main(verbosity=2)

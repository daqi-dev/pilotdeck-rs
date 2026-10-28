#!/usr/bin/env python3
"""#209 A1 lock-release controls: signals, proof, and retained leases.

Offline only: no sidecar, no full parity round, no shared /tmp lock.  Each
test uses a temporary directory as the lease root, so the real
/tmp/pdrs-parity.lock is never touched.
"""
import sys

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution; without it, exit before loading any apparatus module.
if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

import tempfile
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-parity-lock-pyc-')
sys.dont_write_bytecode = True

from contextlib import redirect_stdout
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import time
import unittest
from unittest.mock import patch

_HERE = Path(__file__).resolve().parent
_ROOT = _HERE.parent


def _load(name, path):
    """Load an apparatus module by explicit path; never resolve via sys.path."""
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module


G = _load('g03_compare', _ROOT / 'g03_compare.py')
R = _load('round', _ROOT / 'round.py')


class LockReleaseControls(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-a1-lock-', dir=G.WORKTREE / '_tmp')
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        self.lock = self.root / 'parity.lock'

    def tearDown(self):
        R.LIVE_GROUPS.clear()

    def test_signals_deferred_across_spawn(self):
        def mask():
            return signal.pthread_sigmask(signal.SIG_BLOCK, set())

        self.assertFalse(mask() & R.DEFERRED, '#209 signals must start unblocked')
        with R.deferred_signals():
            self.assertEqual(mask() & R.DEFERRED, R.DEFERRED,
                             '#209 SIGINT/SIGTERM must be blocked during spawn/cleanup')
        self.assertFalse(mask() & R.DEFERRED, '#209 the previous mask must be restored')

    def test_acquisition_interrupt_releases_empty_lock(self):
        real_publish = R.publish

        def interrupt_after_publish(path, value):
            real_publish(path, value)
            os.kill(os.getpid(), signal.SIGINT)

        with patch.object(R, 'publish', interrupt_after_publish):
            with self.assertRaises(KeyboardInterrupt,
                                   msg='#209 acquisition interrupt must be delivered'):
                with R.parity_lock(self.lock):
                    self.fail('#209 acquisition signal was lost')
        self.assertFalse(self.lock.exists(),
                         '#209 cancelled acquisition must release an empty lock')

    def test_acquisition_publishes_pending(self):
        with R.parity_lock(self.lock):
            self.assertIs(R.read_owner(self.lock).get('cleanupPending'), True,
                          '#209 acquisition must publish cleanupPending before spawn')

    def test_retains_lock_when_group_unproven(self):
        with patch.object(R, 'LIVE_GROUPS', {424242}):
            with R.parity_lock(self.lock):
                pass
        self.assertTrue(self.lock.exists(),
                        '#209 an unproven group must retain the shared lock')
        retained = R.read_owner(self.lock)
        self.assertEqual(retained.get('cleanupPending'), [424242],
                         '#209 retained groups must be explicit in owner.json')
        with patch.object(R, 'pid_alive', return_value=False), \
                patch.object(R.time, 'sleep', side_effect=RuntimeError('waited')):
            # The refusal itself is the bound assertion: a mutant that takes
            # the retained lease over makes assertRaises fail with this message.
            with self.assertRaises(RuntimeError,
                                   msg='#209 cleanupPending lock must be refused'):
                with R.parity_lock(self.lock):
                    pass

    def test_cleanup_error_is_unproven(self):
        class FakeProc:
            pid = 424242
            returncode = None

            def wait(self, timeout=None):
                return 0

        def killpg(pid, sig):
            # The group is already gone (final ESRCH probe succeeds) but the
            # TERM/KILL delivery itself failed: errors must still deny proof.
            if sig == 0:
                raise ProcessLookupError(pid)
            raise PermissionError('injected #209')

        R.LIVE_GROUPS.add(424242)
        self.addCleanup(R.LIVE_GROUPS.discard, 424242)
        with patch.object(R.os, 'killpg', side_effect=killpg):
            audit = R.stop_group(FakeProc())
        self.assertFalse(audit['allStopped'],
                         '#209 kill/wait OSError must leave the group unproven')
        self.assertTrue(audit['errors'], '#209 the cleanup error must be recorded')
        self.assertFalse(audit['remaining'],
                         '#209 the final ESRCH probe must prove the group gone')
        self.assertIn(424242, R.LIVE_GROUPS,
                      '#209 unproven cleanup must stay in LIVE_GROUPS')
        with R.parity_lock(self.lock):
            pass
        self.assertTrue(self.lock.exists(),
                        '#209 unproven cleanup must retain the shared lock')
        self.assertEqual(R.read_owner(self.lock).get('cleanupPending'), [424242],
                         '#209 retained cleanupPending must be explicit in owner.json')

    def test_cleanup_pending_cannot_be_stolen(self):
        self.lock.mkdir()
        original = {'pid': os.getpid(), 'timestamp': time.time(),
                    'token': 'retained-owner', 'cleanupPending': [424242]}
        (self.lock / 'owner.json').write_text(json.dumps(original) + '\n')
        with patch.object(R, 'pid_alive', return_value=False), \
                patch.object(R.time, 'sleep', side_effect=RuntimeError('waited')):
            # The refusal itself is the bound assertion: a mutant that steals
            # this lease makes assertRaises fail with this exact message.
            with self.assertRaises(RuntimeError,
                                   msg='#209 cleanupPending lock must be refused'):
                with R.parity_lock(self.lock):
                    pass
        self.assertEqual(R.read_owner(self.lock), original,
                         '#209 failed takeover must preserve the owner record')

    def test_interrupt_marks_blocked_and_releases_after_reap(self):
        baseline = self.root / 'baseline'
        binary = baseline / 'dist/src/cli/pilotdeck-agent-loop-sidecar.js'
        binary.parent.mkdir(parents=True)
        binary.write_text('interrupt-control')
        worktree = self.root / 'worktree'
        (worktree / '_tmp').mkdir(parents=True)
        out_root = worktree / '_tmp/parity'
        real_popen = subprocess.Popen
        real_lock = R.parity_lock
        spawned = []

        class Interrupting:
            def __init__(self, proc):
                self._proc = proc
                self.pid = proc.pid
                self.stdout = proc.stdout
                self.stderr = proc.stderr

            @property
            def returncode(self):
                return self._proc.returncode

            def communicate(self, timeout=None):
                raise KeyboardInterrupt('injected #209 SIGINT')

            def wait(self, timeout=None):
                return self._proc.wait(timeout)

        def interrupting_popen(cmd, **kwargs):
            proc = real_popen([sys.executable, '-c',
                               'import time; print("alive", flush=True); time.sleep(30)'], **kwargs)
            spawned.append(proc)
            return Interrupting(proc)

        def isolated_lock(*args, **kwargs):
            return real_lock(self.lock, **kwargs)

        argv = ['round.py', '--side', 'ts', '--label', 'interrupt', '--scenario', 'pure_text']
        stdout = io.StringIO()
        with patch.object(sys, 'argv', argv), \
                patch.dict(os.environ, {'PDRS_OUT': str(out_root)}), \
                patch.object(G, 'WORKTREE', worktree), patch.object(G, 'BASELINE', baseline), \
                patch.object(G, 'scenario_catalog', return_value={'pure_text': {}}), \
                patch.object(R.shutil, 'which', return_value=sys.executable), \
                patch.object(R.subprocess, 'check_output', return_value='v-control\n'), \
                patch.object(R.subprocess, 'Popen', side_effect=interrupting_popen), \
                patch.object(R, 'parity_lock', side_effect=isolated_lock), \
                redirect_stdout(stdout):
            self.assertEqual(R.main(), 1, '#209 interrupt must exit BLOCKED/1')
        run = json.loads((out_root / 'interrupt/run.json').read_text())
        self.assertEqual(run['status'], 'BLOCKED', '#209 interrupted round must be BLOCKED')
        self.assertEqual(run['ownedGroupsRemaining'], [],
                         '#209 reaped group must not be reported as residual')
        self.assertTrue(run['cleanup']['allStopped'], '#209 cleanup must prove absence')
        self.assertIn('interrupted by KeyboardInterrupt', run['blocked'][0],
                      '#209 blocked reason must name the interrupt')
        self.assertFalse(self.lock.exists(),
                         '#209 proven-stopped group must allow lock release')
        self.assertIsNotNone(spawned[0].poll(), '#209 injected child group must be reaped')
        self.assertEqual(json.loads(stdout.getvalue())['status'], 'BLOCKED',
                         '#209 printed summary must be BLOCKED')


if __name__ == '__main__':
    unittest.main()

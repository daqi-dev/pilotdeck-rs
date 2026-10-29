# anchor-selfcheck: unsupported positive-only lock-release controls; the source mutation table lives in negative_controls.py
#!/usr/bin/env python3
"""#209 E1 positive controls for the gated shared-lease release.

Offline only: each test uses a temporary directory as the lease root, so the
real /tmp/pdrs-parity.lock is never touched.  ``run_fixture`` is driven with a
fake child; no fixture, sidecar or full calibration is launched.

The source-mutation controls live in ``negative_controls.py`` and run after the
coordinator commits the patch: ``mutating_sources`` refuses a working tree that
differs from HEAD.  This file is the restored positive they bind to.
"""
import sys

if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

import tempfile

sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-e1-lock-pyc-')
sys.dont_write_bytecode = True

import contextlib
import importlib.util
import json
from pathlib import Path
import shutil
import signal
import subprocess
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[5]
SESSION = ROOT / 'scripts/gateway-parity/session_domain.py'
DEFERRED = {signal.SIGINT, signal.SIGTERM}


def _load(name, path):
    """Load the apparatus entry by explicit path; never resolve via sys.path."""
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    try:
        spec.loader.exec_module(module)
    except BaseException:
        sys.modules.pop(name, None)
        raise
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module


S = _load('session_domain_lock_release', SESSION)
C = S.common


class FakeChild:
    """The small Popen surface run_fixture's cleanup path actually uses."""

    def __init__(self, pid, returncode=0):
        self.pid = pid
        self.returncode = returncode
        self.wait_timeouts = 0

    def poll(self):
        return self.returncode

    def wait(self, timeout=None):
        if self.returncode is None:
            # A successful bounded wait reaps the leader and records its code.
            self.returncode = 0
        return self.returncode


class LockReleaseControls(unittest.TestCase):
    PID = 505050

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-e1-lock-')
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        self.lock = self.root / 'parity.lock'
        C.LIVE_GROUPS.clear()
        self.addCleanup(C.LIVE_GROUPS.clear)
        self.masks = {}
        self.registered_at_cleanup = None
        self.run_number = 0

    def _run(self, probe, *, wait_states=('EXITED',), child=None,
             kill_leader=None, acquire_lock=False):
        """Drive the real run_fixture cleanup gate with a fake child."""
        child = child if child is not None else FakeChild(self.PID)
        self.run_number += 1
        folder = self.root / f'run-{self.run_number}'
        folder.mkdir()
        states = iter(wait_states)

        def wait_state(pid, timeout):
            return next(states, wait_states[-1])

        def spawn(*args, **kwargs):
            self.masks['spawn'] = signal.pthread_sigmask(signal.SIG_BLOCK, set())
            return child

        def probe_group(actual):
            self.masks['cleanup'] = signal.pthread_sigmask(signal.SIG_BLOCK, set())
            self.registered_at_cleanup = self.PID in C.LIVE_GROUPS
            if isinstance(probe, BaseException):
                raise probe
            return dict(probe)

        stack = contextlib.ExitStack()
        stack.enter_context(patch.object(S.subprocess, 'Popen', side_effect=spawn))
        stack.enter_context(patch.object(S, 'wait_exit_unreaped', side_effect=wait_state))
        stack.enter_context(patch.object(S, 'probe_process_group', side_effect=probe_group))
        stack.enter_context(patch.object(S.os, 'killpg', return_value=None))
        stack.enter_context(patch.object(S.os, 'kill', return_value=None))
        if kill_leader is not None:
            stack.enter_context(patch.object(S.HeldProcessGroup, 'kill_leader',
                                             return_value=kill_leader))
        try:
            if acquire_lock:
                with C.parity_lock(timeout=2, lock=self.lock):
                    return S.run_fixture(folder, ROOT, command=['stub'])
            return S.run_fixture(folder, ROOT, command=['stub'])
        finally:
            stack.close()

    def test_verified_cleanup_releases_lock(self):
        audit = self._run({'status': S.CLEANUP_VERIFIED, 'pgid': self.PID},
                          acquire_lock=True)
        self.assertNotIn(self.PID, C.LIVE_GROUPS,
                         '#209 verified cleanup must discard the owned group')
        self.assertIs(audit.get('allStopped'), True,
                      '#209 verified cleanup must report allStopped')
        self.assertEqual(audit.get('ownedGroupsRemaining'), [],
                         '#209 verified cleanup must leave no owned group')
        self.assertFalse(self.lock.exists(),
                         '#209 verified cleanup must release the shared lock')

    def test_unverified_cleanup_retains_lock(self):
        audit = self._run({'status': S.CLEANUP_GROUP_NOT_EMPTY, 'pgid': self.PID},
                          acquire_lock=True)
        self.assertIn(self.PID, C.LIVE_GROUPS,
                      '#209 unverified cleanup must keep the owned group registered')
        self.assertIs(audit.get('allStopped'), False,
                      '#209 unverified cleanup must not report allStopped')
        self.assertEqual(audit.get('processTreeCleanup'), S.CLEANUP_GROUP_NOT_EMPTY,
                         '#209 the group finding must stay visible')
        self.assertTrue(self.lock.exists(),
                        '#209 unverified cleanup must retain the shared lock')
        owner = json.loads((self.lock / 'owner.json').read_text())
        self.assertEqual(owner.get('cleanupPending'), [self.PID],
                         '#209 retained groups must be explicit in owner.json')

    def test_takeover_pending_refused(self):
        self.lock.mkdir()
        retained = {'pid': 424242, 'timestamp': 0, 'token': 'retained',
                    'cleanupPending': [424242]}
        (self.lock / 'owner.json').write_text(json.dumps(retained))
        self.addCleanup(shutil.rmtree, self.lock, True)
        with self.assertRaises(TimeoutError,
                               msg='#209 cleanupPending lock must be refused'):
            with C.parity_lock(timeout=0.3, lock=self.lock):
                pass
        self.assertEqual(json.loads((self.lock / 'owner.json').read_text()), retained,
                         '#209 refused acquisition must preserve the owner record')

    def test_probe_oserror_retains_lock(self):
        audit = self._run(OSError('injected #209 probe'), acquire_lock=True)
        self.assertEqual(audit.get('processTreeCleanup'), S.CLEANUP_NOT_VERIFIED,
                         '#209 probe OSError must leave cleanup NOT_VERIFIED')
        self.assertIn('injected #209 probe', audit.get('cleanupBlocked') or '',
                      '#209 probe OSError must be recorded')
        self.assertIn(self.PID, C.LIVE_GROUPS,
                      '#209 probe OSError must keep the owned group registered')
        self.assertTrue(self.lock.exists(),
                        '#209 probe OSError must retain the shared lock')

    def test_leader_not_reaped_retains_lock(self):
        child = FakeChild(self.PID, returncode=None)
        audit = self._run({'status': S.CLEANUP_VERIFIED, 'pgid': self.PID},
                          wait_states=('REAPED',), child=child,
                          kill_leader=('sent', 'RUNNING'), acquire_lock=True)
        self.assertEqual(audit.get('processTreeCleanup'), S.CLEANUP_NOT_VERIFIED,
                         '#209 a leader that was not reaped must not be verified')
        self.assertIn(self.PID, C.LIVE_GROUPS,
                      '#209 a leader that was not reaped must stay registered')
        self.assertTrue(self.lock.exists(),
                        '#209 a leader that was not reaped must retain the lock')

    def test_leader_kill_refusal_retains_lock(self):
        child = FakeChild(self.PID, returncode=None)
        audit = self._run({'status': S.CLEANUP_VERIFIED, 'pgid': self.PID},
                          wait_states=('EXITED', 'REAPED'), child=child,
                          acquire_lock=True)
        self.assertIn('leader kill refused', audit.get('cleanupBlocked') or '',
                      '#209 a refused leader kill must be recorded')
        self.assertEqual(audit.get('processTreeCleanup'), S.CLEANUP_NOT_VERIFIED,
                         '#209 a cleanup error must deny verification')
        self.assertIn(self.PID, C.LIVE_GROUPS,
                      '#209 a cleanup error must keep the owned group registered')
        self.assertTrue(self.lock.exists(),
                        '#209 a cleanup error must retain the shared lock')

    def test_spawn_defers_signals(self):
        self._run({'status': S.CLEANUP_VERIFIED, 'pgid': self.PID})
        self.assertEqual(self.masks.get('spawn', set()) & DEFERRED, DEFERRED,
                         '#209 spawn and registration must block SIGINT/SIGTERM')
        self.assertFalse(signal.pthread_sigmask(signal.SIG_BLOCK, set()) & DEFERRED,
                         '#209 the signal mask must be restored after run_fixture')

    def test_cleanup_defers_signals(self):
        self._run({'status': S.CLEANUP_GROUP_NOT_EMPTY, 'pgid': self.PID})
        self.assertEqual(self.masks.get('cleanup', set()) & DEFERRED, DEFERRED,
                         '#209 cleanup must block SIGINT/SIGTERM')
        self.assertFalse(signal.pthread_sigmask(signal.SIG_BLOCK, set()) & DEFERRED,
                         '#209 the signal mask must be restored after run_fixture')

    def test_group_registered_before_cleanup(self):
        self._run({'status': S.CLEANUP_VERIFIED, 'pgid': self.PID})
        self.assertIs(self.registered_at_cleanup, True,
                      '#209 the owned group must be registered before cleanup')

    def test_real_clean_group_releases_lock(self):
        folder = self.root / 'real-clean'
        folder.mkdir()
        with C.parity_lock(timeout=2, lock=self.lock):
            audit = S.run_fixture(folder, ROOT,
                                  command=[sys.executable, '-I', '-c', 'pass'])
        self.assertEqual(audit.get('processTreeCleanup'), S.CLEANUP_VERIFIED,
                         '#209 a real empty group must be VERIFIED on this host')
        self.assertIs(audit.get('allStopped'), True,
                      '#209 a real empty group must be reported allStopped')
        self.assertNotIn(audit.get('pgid'), C.LIVE_GROUPS,
                         '#209 a real empty group must be discarded from the registry')
        self.assertFalse(self.lock.exists(),
                         '#209 a real empty group must release the shared lock')

    def test_existing_output_directory_is_not_reused(self):
        folder = self.root / 'existing-output'
        folder.mkdir()
        with self.assertRaises(FileExistsError,
                               msg='#209 output directory reuse must be refused'):
            folder.mkdir(parents=False, exist_ok=False)


if __name__ == '__main__':
    unittest.main()

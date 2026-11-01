#!/usr/bin/env python3
# anchor-selfcheck: unsupported lease lifecycle tests using runtime hooks, no source mutations
"""#1221 lease lifecycle controls; scoring and checkout guards stay separate."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import importlib.util
from pathlib import Path
import signal
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[5]
path = ROOT / 'scripts/gateway-parity/session_domain.py'
spec = importlib.util.spec_from_file_location('lease_session', path)
s = importlib.util.module_from_spec(spec)
exec(compile(path.read_bytes(), str(path), 'exec'), s.__dict__)
c = s.common


class LeaseTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='pdrs-1221-')
        self.addCleanup(temporary.cleanup)
        self.folder = Path(temporary.name)
        self.lock = self.folder / 'lock'

    def test_blocked_fixture_releases_lease(self):
        marker = '#1221 BLOCKED empty fixture releases lease for successor'
        # The real child/cleanup/lease run on this host. Only the scoring platform
        # is selected deterministically: non-Darwin must remain BLOCKED.
        with patch.object(c, 'LIVE_GROUPS', set()), patch.object(s.sys, 'platform', 'linux'):
            try:
                with c.parity_lock(lock=self.lock):
                    audit = s.run_fixture(self.folder, self.folder,
                                          [sys.executable, '-I', '-c', 'pass'])
                    self.assertTrue(audit['processTreeCleanup'] == s.CLEANUP_BLOCKED, marker)
                    self.assertTrue(s.cleanup_status(audit) != 'PASS', marker)
                    self.assertTrue(audit['allStopped'] and not audit['ownedGroupsRemaining'], marker)
                self.assertFalse(self.lock.exists(), marker)
                with c.parity_lock(timeout=.1, lock=self.lock):
                    self.assertTrue(c.owner(self.lock)['pid'] == s.os.getpid(), marker)
            except Exception as error:
                self.fail(marker + ': ' + repr(error))

    def test_failed_and_cancelled_fixture_release(self):
        marker = '#1221 non-PASS cleanup releases only a settled empty group'
        for event in ('failure', 'cancel', 'deadline'):
            with self.subTest(event=event), patch.object(c, 'LIVE_GROUPS', set()), \
                    patch.object(s.sys, 'platform', 'linux'):
                output = self.folder / event
                output.mkdir()
                original_wait = s.wait_exit_unreaped
                first = True

                def wait(pid, timeout):
                    nonlocal first
                    if first and event != 'failure':
                        first = False
                        if event == 'cancel':
                            raise KeyboardInterrupt('#1221 cancellation')
                        raise TimeoutError('#1221 launcher deadline')
                    return original_wait(pid, timeout)

                caught = None
                try:
                    with c.parity_lock(lock=self.lock), patch.object(s, 'wait_exit_unreaped', wait):
                        s.run_fixture(output, output, [sys.executable, '-I', '-c',
                                      'raise SystemExit(7)' if event == 'failure'
                                      else 'import time; time.sleep(5)'])
                except (KeyboardInterrupt, TimeoutError) as error:
                    caught = error
                except Exception as error:
                    self.fail(marker + ': ' + repr(error))
                self.assertTrue((caught is None) == (event == 'failure'), marker)
                self.assertFalse(c.LIVE_GROUPS or self.lock.exists(), marker)
                audit = c.json.loads((output / 'launch.json').read_text())
                self.assertTrue(audit['allStopped'], marker)
                self.assertTrue(audit['processTreeCleanup'] == s.CLEANUP_BLOCKED, marker)

    def test_lease_probe_is_fail_closed(self):
        marker = '#1221 lease proof requires held reaped leader and final ESRCH'
        for event in ('absent', 'live', 'denied', 'error', 'unreaped', 'identity-lost',
                      'probe-not-reaped', 'cleanup-error', 'cleanup-raised', 'unregistered',
                      'wrong-pgid', 'truthy-unreaped', 'truthy-reaped'):
            with self.subTest(event=event):
                child = SimpleNamespace(pid=424242, returncode=None if event == 'unreaped' else 0)
                audit = {'processTreeCleanup': s.CLEANUP_BLOCKED,
                         'processGroupProbe': {'pgid': 1 if event == 'wrong-pgid' else child.pid,
                                               'leaderUnreaped': event != 'identity-lost',
                                               'leaderReaped': event != 'probe-not-reaped'}}
                if event == 'truthy-unreaped':
                    audit['processGroupProbe']['leaderUnreaped'] = 1
                if event == 'truthy-reaped':
                    audit['processGroupProbe']['leaderReaped'] = 1
                if event == 'cleanup-error':
                    audit['cleanupBlocked'] = 'denied'
                if event == 'cleanup-raised':
                    audit['cleanupRaised'] = 'interrupted'
                before = audit['processTreeCleanup']
                error = (None if event == 'live' else PermissionError('denied') if event == 'denied'
                         else OSError('probe failed') if event == 'error' else ProcessLookupError())
                with patch.object(c, 'LIVE_GROUPS', set() if event == 'unregistered' else {child.pid}), \
                        patch.object(s.os, 'killpg', side_effect=error) as probe, \
                        patch.object(s.time, 'monotonic', side_effect=(0, 3)):
                    stopped = s.fixture_lease_stopped(child, audit)
                self.assertTrue(stopped == (event == 'absent'), marker)
                self.assertTrue(audit['processTreeCleanup'] == before, marker)
                self.assertTrue(all(call.args == (child.pid, 0) for call in probe.call_args_list), marker)
                if event in ('unreaped', 'identity-lost', 'probe-not-reaped', 'cleanup-error',
                             'cleanup-raised', 'unregistered', 'wrong-pgid',
                             'truthy-unreaped', 'truthy-reaped'):
                    self.assertTrue(probe.call_count == 0, marker)

    def test_lease_probe_waits_boundedly(self):
        marker = '#1221 lease probe waits for ESRCH within bounded deadline'
        child = SimpleNamespace(pid=424242, returncode=0)
        audit = {'processTreeCleanup': s.CLEANUP_BLOCKED,
                 'processGroupProbe': {'pgid': child.pid, 'leaderUnreaped': True, 'leaderReaped': True}}
        with patch.object(c, 'LIVE_GROUPS', {child.pid}), \
                patch.object(s.os, 'killpg', side_effect=[None, ProcessLookupError()]) as probe, \
                patch.object(s.time, 'monotonic', side_effect=(0, 1)), \
                patch.object(s.time, 'sleep') as sleep:
            self.assertTrue(s.fixture_lease_stopped(child, audit), marker)
            self.assertTrue(probe.call_count == 2 and sleep.call_count == 1, marker)


if __name__ == '__main__':
    signal.signal(signal.SIGINT, signal.default_int_handler)
    unittest.main()

# anchor-selfcheck: unsupported exercises the inline calibrate.sh supervisor; no product mutation table
#!/usr/bin/env python3
"""#209 B2 controls for the inline calibrate.sh lock/signal supervisor.

The shell file owns the gate; this test extracts exactly the heredoc Python it
executes (no copy, no second implementation) and drives it with a temporary
lease and stub "calibrate" scripts. The real /tmp/pdrs-parity.lock is never
touched.
"""
import sys

if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

import tempfile
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-b2-lock-pyc-')
sys.dont_write_bytecode = True

import json
import os
from pathlib import Path
import signal
import subprocess
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[5]
CALIBRATE = ROOT / 'scripts/ui-server-parity/calibrate.sh'


def load_inline():
    text = CALIBRATE.read_text()
    marker = "<<'PY'\n"
    start = text.index(marker) + len(marker)
    end = text.index('\nPY\n', start)
    source = text[start:end] + '\n'
    namespace = {'__name__': 'calibrate_inline_supervisor', '__file__': str(CALIBRATE)}
    exec(compile(source, str(CALIBRATE) + ':inline', 'exec'), namespace)
    return namespace


L = load_inline()


class LockGateControls(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-b2-lock-')
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        self.lock = self.root / 'parity.lock'

    def tearDown(self):
        L['LIVE_GROUPS'].clear()

    def test_signals_deferred_across_spawn(self):
        def mask():
            return signal.pthread_sigmask(signal.SIG_BLOCK, set())

        deferred = L['DEFERRED']
        self.assertFalse(mask() & deferred, '#209 signals must start unblocked')
        with L['deferred_signals']():
            self.assertEqual(mask() & deferred, deferred,
                             '#209 SIGINT/SIGTERM must be blocked during spawn/cleanup')
        self.assertFalse(mask() & deferred, '#209 the previous mask must be restored')

    def test_acquisition_interrupt_releases_empty_lock(self):
        stub = self.root / 'stub.py'
        stub.write_text('import sys\nsys.exit(0)\n')
        real_publish = L['publish']

        def interrupt_after_publish(path, value):
            real_publish(path, value)
            os.kill(os.getpid(), signal.SIGINT)

        with patch.dict(L, {'publish': interrupt_after_publish}):
            with self.assertRaises(KeyboardInterrupt,
                                   msg='#209 acquisition interrupt must be delivered'):
                L['run'](stub, [], self.lock)
        self.assertFalse(self.lock.exists(),
                         '#209 cancelled acquisition must release an empty lock')

    def test_acquisition_publishes_pending(self):
        mine = L['acquire'](self.lock)
        self.assertEqual(L['read_owner'](self.lock).get('cleanupPending'), True,
                         '#209 acquisition must publish cleanupPending before spawn')
        self.assertTrue(L['release'](self.lock, mine),
                        '#209 empty acquisition must release')
        self.assertFalse(self.lock.exists(), '#209 released lease must be removed')

    def test_existing_cleanup_pending_cannot_be_stolen(self):
        self.lock.mkdir()
        retained = {'pid': 424242, 'timestamp': 0, 'token': 'retained',
                    'cleanupPending': [424242]}
        (self.lock / 'owner.json').write_text(json.dumps(retained) + '\n')
        with self.assertRaises(FileExistsError, msg='#209 B2 never steals a lease'):
            L['acquire'](self.lock)
        self.assertEqual(L['read_owner'](self.lock), retained,
                         '#209 refused acquisition must preserve the owner record')

    def test_retains_lock_when_group_unproven(self):
        mine = L['acquire'](self.lock)
        with patch.dict(L, {'LIVE_GROUPS': {424242}}):
            self.assertFalse(L['release'](self.lock, mine),
                             '#209 an unproven group must retain the lease')
        self.assertTrue(self.lock.exists(),
                        '#209 retained lease must still exist')
        self.assertEqual(L['read_owner'](self.lock).get('cleanupPending'), [424242],
                         '#209 retained groups must be explicit in owner.json')

    def test_cleanup_error_is_unproven(self):
        class FakeProc:
            pid = 424242
            returncode = None

            def wait(self, timeout=None):
                raise subprocess.TimeoutExpired(str(self.pid), timeout)

        with patch.object(L['os'], 'killpg', side_effect=PermissionError('injected #209')):
            audit = L['_stop_group'](FakeProc())
        self.assertFalse(audit['allStopped'],
                         '#209 kill/wait OSError must leave the group unproven')
        self.assertTrue(audit['errors'], '#209 the cleanup error must be recorded')

    def test_interrupt_reaps_group_and_releases(self):
        marker = self.root / 'grandchild.pid'
        stub = self.root / 'stub.py'
        stub.write_text(
            'import subprocess, sys, time\n'
            'child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])\n'
            'open(sys.argv[1], "w").write(str(child.pid))\n'
            'time.sleep(60)\n')

        def alarm(signum, frame):
            raise KeyboardInterrupt(f'signal {signum}')

        previous = signal.signal(signal.SIGALRM, alarm)
        signal.setitimer(signal.ITIMER_REAL, 0.4)
        try:
            code = L['run'](stub, [str(marker)], self.lock)
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
            signal.signal(signal.SIGALRM, previous)
        self.assertEqual(code, 130, '#209 SIGINT after proof must return 130')
        self.assertFalse(self.lock.exists(),
                         '#209 proven-stopped group must allow lease release')
        pid = int(marker.read_text())
        with self.assertRaises(ProcessLookupError,
                               msg='#209 the spawned session group must be reaped'):
            os.kill(pid, 0)

    def test_normal_exit_preserves_calibrate_code(self):
        stub = self.root / 'stub.py'
        stub.write_text('import sys\nsys.exit(2)\n')
        self.assertEqual(L['run'](stub, [], self.lock), 2,
                         '#209 a normal calibrate exit code must be preserved')
        self.assertFalse(self.lock.exists(), '#209 normal exit must release the lease')

    def test_shell_has_no_unconditional_release(self):
        text = CALIBRATE.read_text()
        self.assertNotIn("trap 'rmdir /tmp/pdrs-parity.lock' EXIT", text,
                         '#209 calibrate.sh must not release on every EXIT')
        self.assertIn("<<'PY'", text,
                      '#209 calibrate.sh must execute the gated supervisor')


if __name__ == '__main__':
    unittest.main()

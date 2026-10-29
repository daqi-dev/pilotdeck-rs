# anchor-selfcheck: unsupported exercises the inline calibrate.sh supervisor; no product mutation table
#!/usr/bin/env python3
"""#209 B2 controls for the inline calibrate.sh lock/signal supervisor.

The shell file owns the gate; this test extracts exactly the heredoc Python it
executes (no copy, no second implementation) and drives it with a temporary
lease and stub "calibrate" scripts. The real /tmp/pdrs-parity.lock is never
touched.
"""
import sys
if sys.version_info < (3, 11) or 'os' not in sys.modules:
    raise SystemExit(f'os must be preloaded from CPython 3.11+ frozen modules; run with -I: python3 -I {__file__}')
import os

# #1023 fix-r2 P2-1: -I alone is not enough. unittest discovery reinserts the
# fixture directory, and a caller can also insert the frozen B2 apparatus
# directory; with either on sys.path a same-named stdlib module beside it would
# execute before the first non-boot import. Reject every spelling of this
# fixture, its tests/fixtures parent, and scripts/ui-server-parity by
# (device, inode), not by string: a case-insensitive alias shares the inode
# while realpath keeps the spellings distinct. ``os`` has been frozen since
# CPython 3.11, so importing it here cannot be shadowed from that directory.
_boot_dir = os.path.dirname(os.path.realpath(__file__))
_boot_parent = os.path.dirname(_boot_dir)
_boot_root = os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.dirname(_boot_parent))))
_boot_roots = (_boot_dir, _boot_parent,
               os.path.join(_boot_root, 'scripts', 'ui-server-parity'),
               os.path.join(_boot_root, 'scripts', 'ui-server-parity', 'controls'))


def _boot_reinserted(entries):
    for entry in entries:
        candidate = entry or os.curdir
        if not os.path.isdir(candidate):
            continue
        for root in _boot_roots:
            try:
                if os.path.samefile(candidate, root):
                    return True
            except OSError:
                continue
    return False


if not sys.flags.isolated or _boot_reinserted(sys.path):
    raise SystemExit(f'run with -I: python3 -I {__file__}')

import tempfile
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-b2-lock-pyc-')
sys.dont_write_bytecode = True

import ast
import contextlib
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import unittest
from unittest.mock import MagicMock, patch

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
                return 0

        def killpg(pid, sig):
            # The group is already gone (final ESRCH probe succeeds) but the
            # TERM/KILL delivery itself failed: errors must still deny proof.
            if sig == 0:
                raise ProcessLookupError(pid)
            raise PermissionError('injected #209')

        mine = L['acquire'](self.lock)
        L['LIVE_GROUPS'].add(424242)
        with patch.object(L['os'], 'killpg', side_effect=killpg):
            audit = L['stop_group'](FakeProc())
        self.assertFalse(audit['allStopped'],
                         '#209 kill/wait OSError must leave the group unproven')
        self.assertTrue(audit['errors'], '#209 the cleanup error must be recorded')
        self.assertFalse(audit['remaining'],
                         '#209 the final ESRCH probe must prove the group gone')
        self.assertIn(424242, L['LIVE_GROUPS'],
                      '#209 unproven cleanup must stay in LIVE_GROUPS')
        self.assertFalse(L['release'](self.lock, mine),
                         '#209 an unproven group must retain the lease')
        self.assertTrue(self.lock.exists(),
                        '#209 retained lease must still exist')
        self.assertEqual(L['read_owner'](self.lock).get('cleanupPending'), [424242],
                         '#209 retained cleanupPending must be explicit in owner.json')

    def _run_fake_cleanup(self, fake, killpg):
        """Drive run()'s real post-exit cleanup gate with a fake child."""
        def fake_spawn(*args, **kwargs):
            L['LIVE_GROUPS'].add(fake.pid)
            return fake

        with patch.dict(L, {'spawn': fake_spawn}), \
                patch.object(L['os'], 'killpg', side_effect=killpg):
            return L['run'](self.root / 'stub.py', [], self.lock)

    def test_wait_timeout_returns_blocked_and_retains_lease(self):
        class FakeProc:
            pid = 505053
            returncode = 0

            def __init__(self):
                self.waits = 0

            def wait(self, timeout=None):
                # The first wait is run()'s normal leader wait; the cleanup
                # waits time out, which alone must force BLOCKED.
                self.waits += 1
                if self.waits == 1:
                    return 0
                raise subprocess.TimeoutExpired(str(self.pid), timeout)

        def killpg(pid, sig):
            # TERM/KILL are delivered; the bounded leader waits fail and only
            # the final probe sees the group gone.
            if sig == 0:
                raise ProcessLookupError(pid)
            return None

        fake = FakeProc()
        code = self._run_fake_cleanup(fake, killpg)
        self.assertEqual(code, L['BLOCKED'],
                         '#209 wait timeout must return BLOCKED')
        self.assertTrue(self.lock.exists(),
                        '#209 wait timeout must retain the lease')
        owner = L['read_owner'](self.lock)
        self.assertIsNotNone(owner, '#209 retained lease must keep an owner record')
        self.assertEqual(owner.get('cleanupPending'), [fake.pid],
                         '#209 wait timeout must publish cleanupPending')
        self.assertIn(fake.pid, L['LIVE_GROUPS'],
                      '#209 wait timeout must stay in LIVE_GROUPS')

    def test_probe_error_returns_blocked_and_retains_lease(self):
        class FakeProc:
            pid = 505054
            returncode = 0

            def wait(self, timeout=None):
                return 0

        def killpg(pid, sig):
            # TERM/KILL succeed and the leader is reaped; only the final
            # absence probe fails. That error must deny proof, not read as gone.
            if sig == 0:
                raise PermissionError('probe #209')
            return None

        fake = FakeProc()
        code = self._run_fake_cleanup(fake, killpg)
        self.assertEqual(code, L['BLOCKED'],
                         '#209 probe OSError must return BLOCKED')
        self.assertTrue(self.lock.exists(),
                        '#209 probe OSError must retain the lease')
        owner = L['read_owner'](self.lock)
        self.assertIsNotNone(owner, '#209 retained lease must keep an owner record')
        self.assertEqual(owner.get('cleanupPending'), [fake.pid],
                         '#209 probe OSError must publish cleanupPending')
        self.assertIn(fake.pid, L['LIVE_GROUPS'],
                      '#209 probe OSError must stay in LIVE_GROUPS')

    def test_cleanup_error_returns_blocked_and_retains_lease(self):
        stub = self.root / 'stub.py'
        stub.write_text('import sys\nsys.exit(0)\n')

        def killpg(pid, sig):
            # The leader has exited, so the final ESRCH probe succeeds; the
            # TERM/KILL delivery fails, which alone must force BLOCKED.
            if sig == 0:
                raise ProcessLookupError(pid)
            raise PermissionError('injected #209')

        with patch.object(L['os'], 'killpg', side_effect=killpg):
            code = L['run'](stub, [], self.lock)
        self.assertEqual(code, L['BLOCKED'],
                         '#209 cleanup OSError must return BLOCKED')
        self.assertTrue(self.lock.exists(),
                        '#209 cleanup OSError must retain the lease')
        retained = L['read_owner'](self.lock)
        self.assertEqual(len(retained.get('cleanupPending') or []), 1,
                         '#209 blocked cleanup must publish the residual group')

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


GUARD_RAISE = "    raise SystemExit(f'run with -I: python3 -I {__file__}')"


def guard_prefix(disable=False):
    """The real entry guard bytes, up to the first non-boot import."""
    text = Path(__file__).read_text()
    prefix = text[:text.index('\nimport tempfile\n')]
    if disable:
        if prefix.count(GUARD_RAISE) != 1:
            raise AssertionError('#1023-10: the guard raise anchor is stale')
        prefix = prefix.replace(GUARD_RAISE, '    pass  # control: guard disabled', 1)
    return prefix


def without_preflight(source):
    """The same guard bytes with the os-preload preflight block removed.

    Paired control for the forged-os probes: if a mutant already disabled the
    block, the source is returned unchanged, so its next ``import os`` still
    reaches the forge directory.
    """
    tree = ast.parse(source)
    node = next((statement for statement in tree.body
                 if isinstance(statement, ast.If)
                 and 'version_info' in ast.unparse(statement.test)), None)
    if node is None:
        return source
    lines = source.splitlines(keepends=True)
    del lines[node.lineno - 1:node.end_lineno]
    return ''.join(lines)


def guarded_probe(disable=False):
    return (guard_prefix(disable)
            + '\nimport unittest\n\n\nclass Probe(unittest.TestCase):\n'
              '    def test_runs(self):\n'
              '        pass\n\n\n'
              "if __name__ == '__main__':\n"
              '    unittest.main()\n')


class EntryGuardTests(unittest.TestCase):
    """#1023 fix-r2 P2-1: discovery/reinsertion is rejected before tempfile."""

    here = Path(__file__).resolve().parent

    def run_discover(self, directory):
        return subprocess.run(
            [sys.executable, '-I', '-B', '-m', 'unittest', 'discover',
             '-s', str(directory), '-p', 'test_zz_probe.py'],
            capture_output=True, text=True, timeout=60)

    def run_reinserted(self, path_dir, entry):
        probe = ('import runpy, sys\n'
                 'sys.path.insert(0, sys.argv[1])\n'
                 'runpy.run_path(sys.argv[2], run_name="probe")\n')
        return subprocess.run(
            [sys.executable, '-I', '-B', '-c', probe, str(path_dir), str(entry)],
            capture_output=True, text=True, timeout=60)

    def test_discovery_reinsertion_is_rejected_before_tempfile(self):
        marker = '#1023-10: discovery reinsertion must be rejected by the entry guard'
        with tempfile.TemporaryDirectory(prefix='b2-guard-') as temp:
            probe = Path(temp) / 'probe'
            probe.mkdir()
            target = probe / 'test_zz_probe.py'
            target.write_text(guarded_probe())
            closed = self.run_discover(probe)
            self.assertEqual(closed.returncode, 1, marker + f': rc={closed.returncode}')
            self.assertIn('run with -I', closed.stdout + closed.stderr, marker)
            # Paired control: with just the raise disabled the identical probe
            # must import and run, proving the rejection was the guard.
            target.write_text(guarded_probe(disable=True))
            opened = self.run_discover(probe)
            self.assertEqual(opened.returncode, 0,
                             marker + ' control: '
                             + (opened.stdout + opened.stderr)[-500:])
            self.assertIn('Ran 1 test', opened.stdout + opened.stderr,
                          marker + ' control: the probe itself must run')

    def test_guard_requires_preloaded_os(self):
        """fix-r4 GLM P1: import os may only run after its preload is proved.

        fix-r5 Codex P1-1: the forged os.py must be provably reachable. The
        probe unloads ``os`` and puts the real forge directory on sys.path
        under ``-X frozen_modules=off``; the paired control removes exactly the
        preflight from the same guard bytes and requires the marker. Keep this
        probe and control pair in sync with ``scripts/cli-parity/
        test_cli_parity.py`` (#701) and ``scripts/ui-server-parity/
        test_frozen.py`` (#1023-11).
        """
        marker = '#1023-10: the guard must reject an unfrozen or unloaded os before importing it'
        preload = 'os must be preloaded from CPython 3.11+ frozen modules'
        forged = ('import sys\n'
                  'with open(sys.argv[2], "w") as stream:\n'
                  '    stream.write("forged os executed")\n'
                  'raise RuntimeError("forged os module executed")\n')
        # sys.argv: [1] entry, [2] marker, [3] forge. ``os`` is unloaded only
        # after the source was read and compiled, then the real forge directory
        # is put on sys.path: a guard that imports os first executes forge/os.py.
        probe = ('import sys\n'
                 'entry, marker, forge = sys.argv[1], sys.argv[2], sys.argv[3]\n'
                 'source = open(entry, "rb").read()\n'
                 'code = compile(source, entry, "exec")\n'
                 'del sys.modules["os"]\n'
                 'sys.path.insert(0, forge)\n'
                 'exec(code, {"__file__": entry, "__name__": "__main__"})\n')
        version_probe = ('import sys\n'
                         'entry, marker, forge = sys.argv[1], sys.argv[2], sys.argv[3]\n'
                         'source = open(entry, "rb").read()\n'
                         'code = compile(source, entry, "exec")\n'
                         'sys.version_info = (3, 10, 0, "final", 0)\n'
                         'del sys.modules["os"]\n'
                         'sys.path.insert(0, forge)\n'
                         'exec(code, {"__file__": entry, "__name__": "__main__"})\n')

        with tempfile.TemporaryDirectory(prefix='b2-osguard-') as temp:
            holder = Path(temp)
            staged = holder / 'entry.py'
            staged.write_text(guard_prefix() + "\nprint('REACHED')\n")
            control = holder / 'control.py'
            control.write_text(without_preflight(staged.read_text()))
            forge = holder / 'forge'
            forge.mkdir()
            (forge / 'os.py').write_text(forged)
            marker_path = holder / 'forged.marker'
            scenarios = (('frozen-modules-off', probe),
                         ('version-below-3-11', version_probe))
            for label, case_probe in scenarios:
                base = [sys.executable, '-I', '-X', 'frozen_modules=off',
                        '-c', case_probe]
                with self.subTest(probe=label):
                    marker_path.unlink(missing_ok=True)
                    result = subprocess.run(
                        base + [str(staged), str(marker_path), str(forge)],
                        capture_output=True, text=True, timeout=60)
                    output = result.stdout + result.stderr
                    self.assertEqual(
                        result.returncode, 1,
                        marker + f' [{label}] rc={result.returncode}: ' + output[-300:])
                    self.assertIn(
                        preload, output,
                        marker + f' [{label}] the preflight must reject before '
                        'importing os: ' + output[-300:])
                    self.assertFalse(
                        marker_path.exists(),
                        marker + f' [{label}]: the forged os.py executed')
                    # Paired control: removing exactly the preflight from the same
                    # bytes must execute the same forged module; without this arm
                    # the marker check above could stay vacuously true.
                    marker_path.unlink(missing_ok=True)
                    control_result = subprocess.run(
                        base + [str(control), str(marker_path), str(forge)],
                        capture_output=True, text=True, timeout=60)
                    control_output = control_result.stdout + control_result.stderr
                    self.assertTrue(
                        marker_path.exists(),
                        marker + f' [{label}] control: removing the preflight must '
                        'execute the forged os.py: ' + control_output[-300:])
                    self.assertIn(
                        'forged os module executed', control_output,
                        marker + f' [{label}] control: the forged module must be the '
                        'failure: ' + control_output[-300:])

    def test_mixed_spelling_reinsertion_is_rejected(self):
        marker = '#1023-10: a mixed-spelling reinsertion must be rejected'
        with tempfile.TemporaryDirectory(prefix='b2-guard-') as temp:
            holder = Path(temp) / 'a' / 'b' / 'fixture'
            holder.mkdir(parents=True)
            entry = holder / 'entry.py'
            entry.write_text(guard_prefix() + "\nprint('REACHED')\n")
            link = Path(temp) / 'linked'
            link.symlink_to(holder, target_is_directory=True)
            spellings = [('sys.path real, entry symlink', holder, link / 'entry.py')]
            alias = holder.with_name(holder.name.upper())
            if alias.is_dir():
                try:
                    same = os.path.samefile(alias, holder)
                except OSError:
                    same = False
                if same:
                    spellings.append(('sys.path case alias, entry real', alias, entry))
                    spellings.append(('sys.path real, entry case alias', holder,
                                      alias / 'entry.py'))
            for label, path_dir, entry_path in spellings:
                with self.subTest(case=label):
                    result = self.run_reinserted(path_dir, entry_path)
                    self.assertEqual(result.returncode, 1,
                                     marker + f' [{label}] rc={result.returncode}')
                    self.assertIn('run with -I', result.stderr, marker + f' [{label}]')
            real = Path(__file__).resolve()
            controls_dir = self.here.parents[4] / 'scripts/ui-server-parity/controls'
            controls_link = Path(temp) / 'controls-link'
            controls_link.symlink_to(controls_dir, target_is_directory=True)
            matrix = [
                ('fixture directory', self.here),
                ('B2 apparatus directory',
                 self.here.parents[4] / 'scripts/ui-server-parity'),
                ('B2 controls directory', controls_dir),
                ('B2 controls symlink', controls_link),
            ]
            controls_alias = controls_dir.with_name(controls_dir.name.upper())
            if controls_alias.is_dir():
                try:
                    same = os.path.samefile(controls_alias, controls_dir)
                except OSError:
                    same = False
                if same:
                    matrix.append(('B2 controls case alias', controls_alias))
            for label, path_dir in matrix:
                with self.subTest(case=label):
                    result = self.run_reinserted(path_dir, real)
                    self.assertEqual(result.returncode, 1, marker + f' [{label}]')
                    self.assertIn('run with -I', result.stderr, marker + f' [{label}]')
            # Paired control: the same alias probe must run once the raise is
            # gone, so the assertions above measure the guard, not runpy.
            entry.write_text(guard_prefix(disable=True) + "\nprint('REACHED')\n")
            opened = self.run_reinserted(holder, link / 'entry.py')
            self.assertEqual(opened.returncode, 0,
                             marker + ' control: ' + opened.stderr[-500:])
            self.assertIn('REACHED', opened.stdout,
                          marker + ' control: the alias must reach the file')


FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
# fix-r5 GLM P2-2/P2-3: every fixture guard must prove its os preload and must
# reject the apparatus directory it exercises. The entries are also the
# boot-time import surfaces a caller can reinsert.
FIXTURE_BOOTSTRAPS = (
    ('cli-parity-isolation/negative_controls.py', 'scripts/cli-parity'),
    ('cli-parity-isolation/test_negative_controls.py', 'scripts/cli-parity'),
    ('cli-parity-mock-gateway/negative_controls.py', 'scripts/cli-parity'),
    ('cli-parity-mock-gateway/test_negative_controls.py', 'scripts/cli-parity'),
    ('parity-lock-release/test_lock_release.py', 'scripts/ui-server-parity'),
    ('ui-server-parity-isolation/negative_controls.py', 'scripts/ui-server-parity'),
)


class FixtureBootstrapTests(unittest.TestCase):
    """fix-r5 GLM P2-2/P2-3: each fixture guard is proved, not assumed."""

    def test_fixture_drivers_require_preloaded_os(self):
        marker = ('#1023-12: every fixture driver must reject an unloaded os '
                  'before importing it')
        preload = 'os must be preloaded from CPython 3.11+ frozen modules'
        forger = ('import sys\n'
                  'with open(sys.argv[2], "w") as stream:\n'
                  '    stream.write("forged os executed")\n'
                  'raise RuntimeError("forged os module executed")\n')
        probe = ('import sys\n'
                 'entry, marker, forge = sys.argv[1], sys.argv[2], sys.argv[3]\n'
                 'source = open(entry, "rb").read()\n'
                 'code = compile(source, entry, "exec")\n'
                 'del sys.modules["os"]\n'
                 'sys.path.insert(0, forge)\n'
                 'exec(code, {"__file__": entry, "__name__": "__main__"})\n')
        with tempfile.TemporaryDirectory(prefix='fixture-osguard-') as temp:
            holder = Path(temp)
            for relative, _apparatus in FIXTURE_BOOTSTRAPS:
                entry = FIXTURES / relative
                label = relative.replace('/', '__')
                control = holder / ('control-' + label)
                control.write_text(without_preflight(entry.read_text()))
                forge = holder / ('forge-' + label)
                forge.mkdir()
                (forge / 'os.py').write_text(forger)
                marker_path = holder / (label + '.marker')
                base = [sys.executable, '-I', '-X', 'frozen_modules=off', '-c', probe]
                with self.subTest(fixture=relative):
                    marker_path.unlink(missing_ok=True)
                    result = subprocess.run(
                        base + [str(entry), str(marker_path), str(forge)],
                        capture_output=True, text=True, timeout=60)
                    output = result.stdout + result.stderr
                    self.assertEqual(
                        result.returncode, 1,
                        marker + f' [{relative}] rc={result.returncode}: '
                        + output[-300:])
                    self.assertIn(
                        preload, output,
                        marker + f' [{relative}] the preflight must reject '
                        'before importing os: ' + output[-300:])
                    self.assertFalse(
                        marker_path.exists(),
                        marker + f' [{relative}] the forged os.py executed')
                    # Paired control: removing exactly the preflight from the
                    # same bytes must execute the same forged module.
                    marker_path.unlink(missing_ok=True)
                    control_result = subprocess.run(
                        base + [str(control), str(marker_path), str(forge)],
                        capture_output=True, text=True, timeout=60)
                    control_output = control_result.stdout + control_result.stderr
                    self.assertTrue(
                        marker_path.exists(),
                        marker + f' [{relative}] control: removing the preflight '
                        'must execute the forged os.py: ' + control_output[-300:])
                    self.assertIn(
                        'forged os module executed', control_output,
                        marker + f' [{relative}] control: the forged module must '
                        'be the failure: ' + control_output[-300:])

    def test_fixture_drivers_reject_a_reinserted_apparatus_directory(self):
        marker = ('#1023-13: a reinserted apparatus directory must be rejected '
                  'by the fixture guard')
        probe = ('import sys\n'
                 'entry, path_dir = sys.argv[1], sys.argv[2]\n'
                 'source = open(entry, "rb").read()\n'
                 'sys.path.insert(0, path_dir)\n'
                 'sys.argv = [entry, "--help"]\n'
                 'exec(compile(source, entry, "exec"), '
                 '{"__file__": entry, "__name__": "__main__"})\n')
        with tempfile.TemporaryDirectory(prefix='fixture-reinsert-') as temp:
            neutral = Path(temp)
            for relative, apparatus in FIXTURE_BOOTSTRAPS:
                entry = FIXTURES / relative
                with self.subTest(fixture=relative):
                    result = subprocess.run(
                        [sys.executable, '-I', '-c', probe, str(entry),
                         str(ROOT / apparatus)],
                        capture_output=True, text=True, timeout=30)
                    self.assertEqual(
                        result.returncode, 1,
                        marker + f' [{relative}] rc={result.returncode}: '
                        + result.stderr[-300:])
                    rejection = ('unsafe import path', 'run with -I')
                    self.assertTrue(
                        any(text in result.stderr for text in rejection),
                        marker + f' [{relative}] the guard itself must reject: '
                        + result.stderr[-300:])
                    # Paired control: a neutral directory must pass the guard,
                    # proving the rejection above is the apparatus root.
                    control = subprocess.run(
                        [sys.executable, '-I', '-c', probe, str(entry),
                         str(neutral)],
                        capture_output=True, text=True, timeout=30)
                    self.assertFalse(
                        any(text in control.stderr for text in rejection),
                        marker + f' [{relative}] control: a neutral path was rejected')


class RunnerInvalidationControls(unittest.TestCase):
    """The delivered mutation runner must never keep a stale green summary."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-plr-runner-')
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        self.source = self.root / 'mutant_source.py'
        self.source.write_text('x = 0\n')
        entry = str(Path(__file__).resolve().parent)
        if entry not in sys.path:
            sys.path.insert(0, entry)
            self.addCleanup(sys.path.remove, entry)
        import negative_controls
        self.runner = negative_controls

    def _read_summary(self, out):
        """Read the final summary without an un-messaged failure point."""
        path = out / 'summary.json'
        if not path.is_file():
            return None
        try:
            summary = json.loads(path.read_text())
        except ValueError as error:
            self.fail(f'#1059 runner summary.json must be valid JSON: {error}')
        self.assertIsInstance(summary, list,
                              '#1059 the runner summary must stay a bare list')
        return summary

    def _run_owned(self):
        phase = {'n': 0}

        def run_owned(command, **kwargs):
            phase['n'] += 1
            step = (phase['n'] - 1) % 3
            if step == 0:
                return subprocess.CompletedProcess([], 0, '', 'Ran 1 test\n')
            if step == 1:
                return subprocess.CompletedProcess(
                    [], 1, '', 'FAIL: test_one\n#209 exclusive message\n')
            return subprocess.CompletedProcess([], 0, '', 'Ran 1 test\n')

        return run_owned

    def _drive(self, *, guard_error=None):
        """Run the real ``main`` with only the guard and subprocess mocked."""
        out = Path(tempfile.mkdtemp(prefix='out-', dir=self.root))
        source = self.source
        captured = {'mutating_calls': 0}

        class Guard:
            def __enter__(self):
                return {source: b'x = 0\n'}

            def __exit__(self, *exc):
                if guard_error is not None:
                    raise guard_error
                return False

        def mutating(root, paths, **kwargs):
            captured['mutating_calls'] += 1
            captured['paths'] = list(paths)
            return Guard()

        with patch.multiple(self.runner,
                            MUTATIONS=[('one', source, 'x = 0', 'x = 1')],
                            CHECKS={'one': ('Controls.test_one',
                                            '#209 exclusive message')},
                            parity_lease=lambda: contextlib.nullcontext(),
                            mutating_sources=mutating,
                            run_owned=self._run_owned()), \
                patch.object(sys, 'argv',
                             [str(Path(self.runner.__file__)), '--output',
                              str(out), '--only', 'one']):
            try:
                code = self.runner.main()
            except BaseException as error:
                # Only the injected cancellation is observed; a real Ctrl-C
                # (a different exception object) must still abort the test.
                # A zero/None-coded SystemExit is the one re-wrapping main()
                # legitimately performs (SystemExit(2)), so it is accepted by
                # type, not identity.
                if error is not guard_error and not (
                        isinstance(guard_error, SystemExit)
                        and isinstance(error, SystemExit)):
                    raise
                code = error
        return code, self._read_summary(out), captured.get('paths'), captured['mutating_calls']

    def test_runner_guards_round_and_shell_without_outer_lease(self):
        """The sample run stays green and guards the runner itself."""
        code, summary, paths, mutating_calls = self._drive()
        self.assertEqual(code, 0, '#1059 the sample runner must finish green')
        self.assertEqual([row['status'] for row in summary], ['KILLED'],
                         '#1059 the sample mutant must be killed')
        self.assertIn(self.runner.RUNNER, paths,
                      '#1059 the delivered runner must be protected for its own row')
        self.assertEqual(mutating_calls, 1,
                         '#1059 the runner lease must not wrap a second guard')

    def test_final_guard_failure_rewrites_summary(self):
        """P0-2: a guard/restoration failure after the last row must void it."""
        code, summary, _, _ = self._drive(
            guard_error=RuntimeError('restore failed'))
        self.assertEqual(code, 2, '#1059 P0-2 guard failure must exit 2')
        self.assertIsNotNone(
            summary, '#1059 P0-2 a guard failure must publish an INVALID summary')
        self.assertEqual([row['status'] for row in summary], ['INVALID'],
                         '#1059 P0-2 the on-disk summary must not keep KILLED')
        self.assertIn('source guard', summary[0]['reason'],
                      '#1059 P0-2 invalidation reason must be recorded')

    def test_final_guard_interrupt_rewrites_summary(self):
        """P0-2: a cancellation after the last row must void it, then propagate."""
        for canceled in (KeyboardInterrupt('restore interrupted'),
                         SystemExit('restore exited'),
                         SystemExit(0), SystemExit(None)):
            with self.subTest(canceled=type(canceled).__name__):
                code, summary, _, _ = self._drive(guard_error=canceled)
                self.assertIs(type(code), type(canceled),
                              '#1059 P0-2 the cancellation must be re-raised')
                if isinstance(code, SystemExit):
                    # fix-r1 P2: the INVALID summary is already on disk, so a
                    # zero/None SystemExit code would read as success.
                    self.assertNotIn(code.code, (None, 0),
                                      '#1059 an INVALID summary must not exit as success')
                self.assertIsNotNone(
                    summary,
                    '#1059 P0-2 a cancellation after the last row must not keep KILLED')
                self.assertEqual([row['status'] for row in summary], ['INVALID'],
                                 '#1059 P0-2 a cancellation after the last row must not keep KILLED')
                self.assertIn('source guard', summary[0]['reason'],
                              '#1059 P0-2 invalidation reason must be recorded')

    def test_interrupt_during_void_publish_leaves_no_killed_summary(self):
        """P0-3: a SIGINT inside the void publish must not keep KILLED.

        The publish side is the review's read-only in-memory model: file
        operations are mocks and the SIGINT is a real Python signal raised
        from ``tmp.replace`` while the INVALID payload is being published.
        Admission and pre-existing evidence are deliberately out of scope
        here: ``new_output_directory`` is replaced by the model output, and
        the target-directory lifecycle is pinned by
        ``test_existing_evidence_is_refused_byte_identical`` (#1059 fix-r1).
        """
        disk = {}
        files = {}

        def entry(name):
            if name not in files:
                files[name] = MagicMock(name=name)
            return files[name]

        out = MagicMock(name='out')
        out.__truediv__.side_effect = entry
        tmp = entry('summary.json.tmp')
        tmp.write_text.side_effect = lambda payload: disk.update(staged=payload)

        def publish(_destination):
            # The window: the INVALID temp is complete, and the SIGINT lands
            # before the rename.
            if any(row['status'] == 'INVALID' for row in json.loads(disk['staged'])):
                signal.raise_signal(signal.SIGINT)
            disk['summary'] = disk['staged']

        tmp.replace.side_effect = publish
        source = MagicMock(name='source')

        class Guard:
            def __enter__(self):
                return {source: b'x = 0\n'}

            def __exit__(self, *exc):
                raise RuntimeError('restore verification failed')

        def mutating(root, paths, **kwargs):
            return Guard()

        # raise_signal delivers KeyboardInterrupt only under the default
        # disposition; a batch parent can inherit SIG_IGN, so install it for
        # the duration of this test and restore whatever was there.
        previous = signal.signal(signal.SIGINT, signal.default_int_handler)
        self.addCleanup(signal.signal, signal.SIGINT, previous)
        with patch.multiple(
                self.runner,
                MUTATIONS=[('one', source, 'x = 0', 'x = 1')],
                CHECKS={'one': ('Controls.test_one',
                                '#209 exclusive message')},
                new_output_directory=lambda _: out,
                parity_lease=lambda: contextlib.nullcontext(),
                mutating_sources=mutating,
                run_owned=self._run_owned()), \
                patch.object(sys, 'argv',
                             [str(Path(self.runner.__file__)), '--output',
                              str(self.root / 'void-publish')]):
            try:
                code = self.runner.main()
            except BaseException as error:
                code = error
        message = ('#1059 P0-3 an interrupt during the void publish must not '
                   'leave a readable KILLED summary')
        try:
            summary = json.loads(disk['summary']) if 'summary' in disk else None
        except ValueError as error:
            self.fail(f'{message}: unreadable summary: {error}')
        self.assertIsInstance(code, KeyboardInterrupt, message)
        self.assertTrue(
            summary is None
            or [row['status'] for row in summary] != ['KILLED'],
            message)

    def test_existing_evidence_is_refused_byte_identical(self):
        """fix-r1 P0: a pre-existing evidence directory is refused, never edited.

        The runner used to delete ``summary.json`` before admission, so a
        mistyped ``--output`` destroyed another run's result and only then got
        the #953 refusal. Both shapes are pinned here — the old summary alone
        and the old summary next to other evidence — and every byte (sha256)
        of the directory must survive. The bound mutant puts the pre-admission
        unlink back.
        """
        message = ('#1059 P0-4 a pre-existing evidence directory must be refused '
                   'with every byte preserved')
        stale = json.dumps(
            [{'mutation': 'one', 'status': 'KILLED', 'reason': '#209 stale'}]) + '\n'
        for label, extra in (
                ('summary-only', {}),
                ('summary-and-evidence',
                 {'a1-release-ungated-positive.log': 'Ran 1 test\nOK\n'})):
            with self.subTest(case=label):
                folder = Path(tempfile.mkdtemp(prefix='reuse-', dir=self.root))
                (folder / 'summary.json').write_text(stale)
                for name, text in extra.items():
                    (folder / name).write_text(text)

                def snapshot():
                    return {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                            for path in folder.iterdir()}

                before = snapshot()

                def refuse(*_args, **_kwargs):
                    # Any admitted directory would start the run; fail loudly
                    # instead of touching this tree (#1059 P0-4 message).
                    raise RuntimeError('#1059 P0-4 mutation must not start')

                with patch.multiple(self.runner,
                                    mutating_sources=refuse,
                                    parity_lease=lambda: contextlib.nullcontext()), \
                        patch.object(sys, 'argv',
                                     [str(Path(self.runner.__file__)),
                                      '--output', str(folder),
                                      '--only', 'a1-release-ungated']):
                    try:
                        code = self.runner.main()
                    except BaseException as error:
                        code = error
                after = snapshot()
                self.assertEqual(
                    after, before,
                    f'{message}: the {label} directory was edited')
                self.assertEqual(
                    after.get('summary.json'), before['summary.json'],
                    f'{message}: summary.json bytes changed ({label})')
                self.assertIsInstance(code, SystemExit, f'{message} ({label})')
                self.assertIn('#953 refusing to reuse a non-empty output directory',
                              str(code), f'{message} ({label})')


if __name__ == '__main__':
    unittest.main()

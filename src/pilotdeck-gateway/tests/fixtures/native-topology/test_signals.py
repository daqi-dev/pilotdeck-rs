#!/usr/bin/env python3
# anchor-selfcheck: unsupported #1168 real-signal phase children, no source mutation table
"""#1168 real-signal matrix for the A5.3 runner's publish/cleanup windows.

Each cell replays the real run.py main() in a disposable child process: real
subprocess groups, a real loopback provider and a real os.kill signal. Only
the baseline source check and the shared lease path are redirected into a
throwaway directory; the lifecycle logic under test is the fixture's own code.
The companion negative_signals.py removes one guarantee at a time and
requires the targeted cell below to fail with its own #1168 message.
"""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
from contextlib import ExitStack
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import threading
import time
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[5]
HERE = Path(__file__).resolve().parent
REAL_SLEEP = time.sleep

spec = importlib.util.spec_from_file_location('native_topology_run', HERE / 'run.py')
matrix = importlib.util.module_from_spec(spec)
spec.loader.exec_module(matrix)
common = matrix.common  # run.py's bootstrap loaded it; LIVE_GROUPS is one identity.

PHASES = ('lease-wait', 'probe-pending', 'probe-spawn-window', 'probe-running', 'probe-cleanup',
          'provider-preflight', 'provider-bound', 'provider-thread-started', 'rounds-running',
          'cleanup-shutdown', 'cleanup-join', 'cleanup-close', 'release',
          'cleanup-residue', 'window-residue')
RESIDUE_PHASES = ('cleanup-residue', 'window-residue')

PROBE_SCRIPT = """#!/usr/bin/env python3
# #1168 stand-in probe: records its inherited signal mask, then sleeps.
import json, signal, sys, time
from pathlib import Path
blocked = sorted(name for name in ('SIGINT', 'SIGTERM')
                 if getattr(signal, name) in signal.pthread_sigmask(signal.SIG_BLOCK, []))
Path(sys.argv[1]).with_name('probe-observer.json').write_text(json.dumps({'blocked': blocked}))
time.sleep(__DURATION__)
"""


def write_probe(folder, duration):
    name = 'probe-long.sh' if duration > 1 else 'probe-short.sh'
    probe = folder / name
    probe.write_text(PROBE_SCRIPT.replace('__DURATION__', repr(duration)))
    probe.chmod(0o755)
    return probe


def signal_child(folder, phase, signum):
    """Run the real main() under real signals; report what survived."""
    if phase not in PHASES:
        raise SystemExit(f'unknown phase: {phase}')
    folder = Path(folder)
    lock = folder / 'parity.lock'
    out = folder / 'out'
    long_probe = phase in ('probe-spawn-window', 'probe-running', 'probe-cleanup')
    probe = write_probe(folder, 30.0 if long_probe else 0.05)
    baseline = folder / 'baseline'
    (baseline / 'src/cli').mkdir(parents=True)
    (baseline / 'src/cli/pilotdeck-agent-loop-sidecar.ts').write_text('// #1168 signal-phase stand-in\n')
    sidecar = folder / 'sidecar.sh'
    sidecar.write_text('#!/bin/sh\nexit 0\n')
    sidecar.chmod(0o755)
    # A background (non-interactive) parent leaves SIGINT ignored, which POSIX
    # shells cannot undo; restore the default here like sigint-default.py so
    # every delivered signal is real.
    signal.signal(signal.SIGINT, signal.SIG_DFL)
    signal.signal(signal.SIGTERM, signal.SIG_DFL)
    previous = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}

    events = []
    fired = [False]
    spawned = []
    counts = {}
    shutdowns, closes, joins = [0], [0], [0]
    predecessor = None
    if phase == 'lease-wait':
        predecessor = {'pid': os.getpid(), 'timestamp': time.time(),
                       'token': 'predecessor', 'cleanupPending': True}
        lock.mkdir()
        common.save(lock / 'owner.json', predecessor)

    def fire():
        os.kill(os.getpid(), signum)
        # Force a signal-check point: an unshielded window must fail here
        # deterministically, not by winning the next bytecode race.
        REAL_SLEEP(0.02)

    def hit(name):
        events.append(name)
        if name == phase:
            fired[0] = True
            fire()
            if name.startswith('cleanup-') or name == 'probe-cleanup':
                fire()

    def counted(name):
        counts[name] = counts.get(name, 0) + 1
        return counts[name]

    def wait_any_observer():
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            found = next(out.rglob('probe-observer.json'), None) if out.is_dir() else None
            if found is not None:
                return found
            REAL_SLEEP(0.01)
        return None

    real_popen = matrix.subprocess.Popen

    def popen(*args, **kwargs):
        child = real_popen(*args, **kwargs)
        if kwargs.get('start_new_session'):
            spawned.append(child.pid)
            if phase == 'probe-spawn-window':
                wait_any_observer()
                hit('probe-spawn-window')
            elif phase == 'window-residue' and counted('window-spawn') == 1:
                hit('window-residue')
        return child

    real_collect = matrix.collect_descendants

    def collect(pid, owned):
        real_collect(pid, owned)
        if counted('collect') == 2 and phase in ('probe-running', 'probe-cleanup'):
            wait_any_observer()
            hit(phase)

    real_cleanup_owned = matrix.cleanup_owned

    def cleanup_owned(child, owned):
        if phase == 'probe-cleanup':
            hit('probe-cleanup')
        if phase in RESIDUE_PHASES:
            # A cleanup that never proves absence is the honest residue shape:
            # the group must stay registered and the lease marked.
            raise ValueError('CLEANUP: simulated residue #1168')
        return real_cleanup_owned(child, owned)

    real_run_probe = matrix.run_probe

    def run_probe(args, config, directory):
        number = counted('run_probe')
        if phase == 'rounds-running' and number == 3:
            hit('rounds-running')
        if phase in RESIDUE_PHASES and number == 2:
            hit(phase)
        return real_run_probe(args, config, directory)

    real_save = matrix.save

    def save(path, value):
        real_save(path, value)
        if phase == 'probe-pending' and Path(path).name == 'config.json':
            hit('probe-pending')

    def baseline_identity(_baseline):
        if phase == 'provider-preflight':
            hit('provider-preflight')
        return {}

    class HookedProvider(matrix.Provider):
        def __init__(self):
            super().__init__()
            if phase == 'provider-bound':
                hit('provider-bound')

        def shutdown(self):
            if phase == 'cleanup-shutdown':
                hit('cleanup-shutdown')
            shutdowns[0] += 1
            super().shutdown()

        def server_close(self):
            if phase == 'cleanup-close':
                hit('cleanup-close')
            closes[0] += 1
            super().server_close()

    class HookedThread(threading.Thread):
        def start(self):
            super().start()
            if phase == 'provider-thread-started':
                hit('provider-thread-started')

        def join(self, *args, **kwargs):
            if phase == 'cleanup-join':
                hit('cleanup-join')
            joins[0] += 1
            return super().join(*args, **kwargs)

    real_rmtree = common.shutil.rmtree

    def rmtree(path, *args, **kwargs):
        if phase == 'release' and Path(path) == lock:
            hit('release')
        return real_rmtree(path, *args, **kwargs)

    def waiting_sleep(seconds):
        hit('lease-wait')
        return REAL_SLEEP(seconds)

    argv = ['run.py', '--baseline', str(baseline), '--node', sys.executable,
            '--sidecar', str(sidecar), '--probe', str(probe), '--output', str(out)]
    exit_code, caught = None, None
    matrix.REPEATS = 1  # 8 stub cases reach the terminal matrix failure fast
    with ExitStack() as stack:
        stack.enter_context(mock.patch.object(matrix, 'parity_lock',
                                              lambda *args, **kwargs: common.parity_lock(lock=lock)))
        stack.enter_context(mock.patch.object(matrix, 'baseline_identity', baseline_identity))
        stack.enter_context(mock.patch.object(matrix, 'Provider', HookedProvider))
        stack.enter_context(mock.patch.object(matrix.threading, 'Thread', HookedThread))
        stack.enter_context(mock.patch.object(matrix, 'save', save))
        stack.enter_context(mock.patch.object(matrix.subprocess, 'Popen', popen))
        stack.enter_context(mock.patch.object(matrix, 'collect_descendants', collect))
        stack.enter_context(mock.patch.object(matrix, 'cleanup_owned', cleanup_owned))
        stack.enter_context(mock.patch.object(matrix, 'run_probe', run_probe))
        stack.enter_context(mock.patch.object(common.shutil, 'rmtree', rmtree))
        if phase == 'lease-wait':
            stack.enter_context(mock.patch.object(common.time, 'sleep', waiting_sleep))
        try:
            with mock.patch.object(sys, 'argv', argv):
                exit_code = matrix.main()
        except BaseException as error:
            caught = type(error).__name__

    owner = None
    if lock.exists():
        try:
            owner = json.loads((lock / 'owner.json').read_text())
        except (OSError, ValueError):
            owner = 'unreadable'
    probe_pid = spawned[0] if spawned else None
    probe_alive = None
    if probe_pid is not None:
        probe_alive = subprocess.run(['ps', '-p', str(probe_pid), '-o', 'pid='],
                                     capture_output=True, text=True).returncode == 0
    observer = next(out.rglob('probe-observer.json'), None) if out.is_dir() else None
    mask_blocked = json.loads(observer.read_text())['blocked'] if observer is not None else None
    first_launch = out / 'Rust-sidecar-ts' / '0' / 'model-stream' / 'launch.json'
    first_launch_status = None
    if first_launch.is_file():
        first_launch_status = json.loads(first_launch.read_text()).get('cleanupStatus')
    print(json.dumps({'phase': phase, 'signum': signum, 'fired': fired[0], 'events': events,
                      'error': caught, 'exitCode': exit_code, 'lease': owner,
                      'predecessorUnchanged': (owner == predecessor) if phase == 'lease-wait' else None,
                      'shutdowns': shutdowns[0], 'closes': closes[0], 'joins': joins[0],
                      'restored': all(signal.getsignal(sig) == handler for sig, handler in previous.items()),
                      'probePid': probe_pid, 'probeAlive': probe_alive,
                      'probeMaskBlocked': mask_blocked,
                      'firstLaunchCleanupStatus': first_launch_status}, ensure_ascii=False))
    return 0


class NativeTopologySignalTests(unittest.TestCase):
    """One #1168 message per guarantee; negative_signals.py binds to these."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='pdrs-1168-')
        self.addCleanup(self.temp.cleanup)
        self.folder = Path(self.temp.name)

    def cell(self, phase, sig, timeout=90):
        folder = self.folder / f'{phase}-{sig}'
        folder.mkdir()
        result = subprocess.run([sys.executable, '-I', str(Path(__file__).resolve()),
                                 '--signal-child', str(folder), phase, str(sig)],
                                capture_output=True, text=True, timeout=timeout)
        self.assertTrue(result.returncode == 0,
                        f'#1168 signal child completes: {phase}/{sig}: rc={result.returncode} {result.stderr[-500:]}')
        self.assertTrue(bool(result.stdout.strip()),
                        f'#1168 signal child publishes its result: {phase}/{sig}')
        return json.loads(result.stdout)

    def check_cancelled(self, value, phase, sig):
        self.assertTrue(value['fired'], f'#1168 the hook reached the target phase: {phase}/{sig}')
        self.assertTrue(value['error'] is None,
                        f'#1168 cancellation unwinds through main: {phase}/{sig}: {value["error"]}')
        self.assertTrue(value['exitCode'] == 1,
                        f'#1168 a cancelled capture exits 1: {phase}/{sig}: {value["exitCode"]}')
        self.assertTrue(value['restored'], f'#1168 cancellation handlers are restored: {phase}/{sig}')

    def check_released(self, value, phase, sig):
        self.assertTrue(value['lease'] is None, f'#1168 a cleaned lease is released: {phase}/{sig}')

    def check_probe_reaped(self, value, phase, sig):
        self.assertTrue(value['probePid'] is not None, f'#1168 the probe was spawned: {phase}/{sig}')
        self.assertTrue(value['probeAlive'] is False,
                        f'#1168 cleanup is not interruptible, the probe group is reaped: {phase}/{sig}')
        self.assertTrue(value['probeMaskBlocked'] == [],
                        f'#1168 the probe inherits an unblocked signal mask: {phase}/{sig}')

    def check_provider_cleaned(self, value, phase, sig):
        self.assertTrue(value['closes'] == value['joins'] == 1,
                        f'#1168 provider cleanup is not interruptible: {phase}/{sig}: '
                        f"closes={value['closes']} joins={value['joins']}")

    def check_residue_marked(self, value, phase, sig):
        pending = (value['lease'] or {}).get('cleanupPending') if isinstance(value['lease'], dict) else None
        self.assertTrue(value['lease'] is not None,
                        f'#1168 residue keeps the lease marked cleanupPending: {phase}/{sig}')
        self.assertTrue(pending and value['probePid'] in pending,
                        f'#1168 residue keeps the lease marked cleanupPending: {phase}/{sig}: {pending!r}')
        self.assertTrue(value['firstLaunchCleanupStatus'] == 'FAIL',
                        f'#1168 residue is reported in the launch audit: {phase}/{sig}')

    def test_lease_wait(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(sig=sig):
                value = self.cell('lease-wait', sig)
                self.check_cancelled(value, 'lease-wait', sig)
                self.assertTrue(value['predecessorUnchanged'],
                                '#1168 a cancelled waiter leaves the predecessor lease untouched: lease-wait')
                self.assertTrue(value['lease'] is not None,
                                '#1168 a cancelled waiter never acquired the lease: lease-wait')
                self.assertTrue(value['probePid'] is None,
                                '#1168 a cancelled waiter spawns nothing: lease-wait')

    def test_probe_pending(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(sig=sig):
                value = self.cell('probe-pending', sig)
                self.check_cancelled(value, 'probe-pending', sig)
                self.check_released(value, 'probe-pending', sig)
                self.assertTrue(value['probePid'] is None,
                                '#1168 a pre-publish cancellation spawns no probe: probe-pending')
                self.check_provider_cleaned(value, 'probe-pending', sig)

    def test_probe_spawn_window(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(sig=sig):
                value = self.cell('probe-spawn-window', sig)
                self.check_cancelled(value, 'probe-spawn-window', sig)
                self.check_released(value, 'probe-spawn-window', sig)
                self.check_probe_reaped(value, 'probe-spawn-window', sig)
                self.assertTrue(value['firstLaunchCleanupStatus'] == 'PASS',
                                '#1168 the publish-window cleanup audit is published: probe-spawn-window')

    def test_probe_running(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(sig=sig):
                value = self.cell('probe-running', sig)
                self.check_cancelled(value, 'probe-running', sig)
                self.check_released(value, 'probe-running', sig)
                self.check_probe_reaped(value, 'probe-running', sig)
                self.assertTrue(value['firstLaunchCleanupStatus'] == 'PASS',
                                '#1168 the running-probe cleanup audit is published: probe-running')

    def test_probe_cleanup(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(sig=sig):
                value = self.cell('probe-cleanup', sig)
                self.check_cancelled(value, 'probe-cleanup', sig)
                self.check_released(value, 'probe-cleanup', sig)
                self.check_probe_reaped(value, 'probe-cleanup', sig)
                self.assertTrue(value['firstLaunchCleanupStatus'] == 'PASS',
                                '#1168 cleanup is not interruptible, the audit is published: probe-cleanup')

    def test_provider_preflight(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(sig=sig):
                value = self.cell('provider-preflight', sig)
                self.check_cancelled(value, 'provider-preflight', sig)
                self.check_released(value, 'provider-preflight', sig)
                self.assertTrue(value['probePid'] is None,
                                '#1168 a pre-publish cancellation spawns no probe: provider-preflight')
                self.assertTrue(value['closes'] == value['joins'] == 0,
                                '#1168 a pre-publish cancellation never creates the provider: provider-preflight')

    def test_provider_bound(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(sig=sig):
                value = self.cell('provider-bound', sig)
                self.check_cancelled(value, 'provider-bound', sig)
                self.check_released(value, 'provider-bound', sig)
                self.assertTrue(value['probePid'] is None,
                                '#1168 a publish-window cancellation spawns no probe: provider-bound')
                self.check_provider_cleaned(value, 'provider-bound', sig)

    def test_provider_thread_started(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(sig=sig):
                value = self.cell('provider-thread-started', sig)
                self.check_cancelled(value, 'provider-thread-started', sig)
                self.check_released(value, 'provider-thread-started', sig)
                self.assertTrue(value['probePid'] is None,
                                '#1168 a publish-window cancellation spawns no probe: provider-thread-started')
                self.check_provider_cleaned(value, 'provider-thread-started', sig)

    def test_rounds_running(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(sig=sig):
                value = self.cell('rounds-running', sig)
                self.check_cancelled(value, 'rounds-running', sig)
                self.check_released(value, 'rounds-running', sig)
                self.check_probe_reaped(value, 'rounds-running', sig)
                self.check_provider_cleaned(value, 'rounds-running', sig)

    def test_cleanup_shutdown(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(sig=sig):
                value = self.cell('cleanup-shutdown', sig)
                self.check_cancelled(value, 'cleanup-shutdown', sig)
                self.check_released(value, 'cleanup-shutdown', sig)
                self.check_probe_reaped(value, 'cleanup-shutdown', sig)
                self.check_provider_cleaned(value, 'cleanup-shutdown', sig)

    def test_cleanup_join(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(sig=sig):
                value = self.cell('cleanup-join', sig)
                self.check_cancelled(value, 'cleanup-join', sig)
                self.check_released(value, 'cleanup-join', sig)
                self.check_probe_reaped(value, 'cleanup-join', sig)
                self.check_provider_cleaned(value, 'cleanup-join', sig)

    def test_cleanup_close(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(sig=sig):
                value = self.cell('cleanup-close', sig)
                self.check_cancelled(value, 'cleanup-close', sig)
                self.check_released(value, 'cleanup-close', sig)
                self.check_probe_reaped(value, 'cleanup-close', sig)
                self.check_provider_cleaned(value, 'cleanup-close', sig)

    def test_release(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(sig=sig):
                value = self.cell('release', sig)
                self.check_cancelled(value, 'release', sig)
                self.check_released(value, 'release', sig)
                self.check_probe_reaped(value, 'release', sig)
                self.check_provider_cleaned(value, 'release', sig)

    def test_cleanup_residue(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(sig=sig):
                value = self.cell('cleanup-residue', sig)
                self.check_cancelled(value, 'cleanup-residue', sig)
                self.check_residue_marked(value, 'cleanup-residue', sig)
                self.check_provider_cleaned(value, 'cleanup-residue', sig)

    def test_window_residue(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(sig=sig):
                value = self.cell('window-residue', sig)
                self.check_cancelled(value, 'window-residue', sig)
                self.check_residue_marked(value, 'window-residue', sig)
                self.check_provider_cleaned(value, 'window-residue', sig)


def main():
    if len(sys.argv) == 5 and sys.argv[1] == '--signal-child':
        return signal_child(sys.argv[2], sys.argv[3], int(sys.argv[4]))
    unittest.main()


if __name__ == '__main__':
    sys.exit(main())

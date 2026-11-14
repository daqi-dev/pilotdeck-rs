#!/usr/bin/env python3
# anchor-selfcheck: unsupported #1713 verdict-chain self-tests plus the #1695
# handoff-window and #1705 kill-tree behavior probes: the two chains run on
# fake inputs and the two subprocess behaviors run against stub children; no
# product source is ever replaced, so there is no replacement table here.
"""#1555-family control self-tests; --negative runs in-memory Python mutants.

Issue #1713 asked for exactly this: the two verdict chains in
`dirty_tree_control_1555.py` (`lock_verdict`, `dirty_verdict`) used to live
inline in `lock_check` / `dirty_check` and were only ever exercised through
whole cargo runs, so a mis-ordered branch could ship without any test seeing
it. They are pure functions now, and this file drives every branch of each
with fake inputs -- no cargo, no product checkout, seconds.

Two behavior tests cover the fixes that share this fixture:

* #1695 -- the lock handoff is atomic. A sibling process polls the shared
  lock file with `flock(LOCK_NB)` every millisecond across the whole window
  while a stub driver holds the handed-over descriptor; it must never win
  (`test_handoff_window_is_atomic`). The unlock-at-handoff mutant (the
  pre-#1695 shape) lets the sibling take the lock with the marker on disk,
  which is the false `src/ is dirty` the issue was about.
* #1705 -- a timeout kill must reach the driver's setsid descendants. A stub
  driver starts a `start_new_session` grandchild, and `_kill_tree` must kill
  both (`test_kill_tree_reaches_setsid_descendants`); the leader-group-only
  mutant (the pre-#1705 shape) leaves the grandchild alive.

`--negative` compiles one in-memory Python mutant of the control module at a
time and requires the bound test to fail with its single-line marker
(the #1128 `jsobject-density/test_controls.py` pattern). Every kill is bound
to a full unittest name; the run never stops early, and any SURVIVED or
INVALID row makes the exit code non-zero.
"""
from contextlib import redirect_stderr, redirect_stdout
import fcntl
import io
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import types
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
fixture = Path(__file__).resolve().parent
sys.path.insert(0, str(fixture))
import dirty_tree_control_1555

# The sibling runner of the #1695 matrix: a real process hammering the shared
# lock with LOCK_NB for a fixed window. Exit 7 the moment the lock is ever won
# (the marker would be on disk at that instant), exit 0 if it never was.
SIBLING_PROBE = '''
import fcntl, os, sys, time
path, window = sys.argv[1], float(sys.argv[2])
deadline = time.monotonic() + window
while time.monotonic() < deadline:
    fd = os.open(path, os.O_RDWR)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        os.close(fd)
        sys.exit(7)
    except BlockingIOError:
        os.close(fd)
    time.sleep(0.001)
sys.exit(0)
'''

# The stub driver of the handoff window: it holds the inherited descriptor
# (its only duty) well past the sibling's window, then releases by closing.
STUB_DRIVER = '''
import os, time
time.sleep(2.0)
os.close(int(os.environ['PDRS_NEGCTL_LOCK_FD']))
'''

# The stub driver of the kill tree: it reports one start_new_session child
# (its own session, its own group -- the #1705 orphan shape) and then sleeps.
TREE_DRIVER = '''
import subprocess, sys, time
child = subprocess.Popen([sys.executable, '-I', '-c',
                         'import time; time.sleep(120)'], start_new_session=True)
with open(sys.argv[1], 'w') as handle:
    handle.write(str(child.pid))
time.sleep(120)
'''

# The dead-leader stub (fix-r1, concurrency P2): same shape, but it exits on
# its own right after reporting the child, so the census sees a zombie leader
# whose child was already reparented -- the race the state column answers.
TREE_DRIVER_EXITING = '''
import os, subprocess, sys
child = subprocess.Popen([sys.executable, '-I', '-c',
                         'import time; time.sleep(120)'], start_new_session=True)
with open(sys.argv[1], 'w') as handle:
    handle.write(str(child.pid))
os._exit(0)
'''


def lock_cell(**over):
    """One `lock_verdict` call over the green defaults of the lock-held row."""
    inputs = dict(
        early='starting\nlock-requested 5.0\n',
        full='starting\nlock-requested 5.0\nlock-acquired\nprovenance x\n',
        returncode=0, driver_status='OK', positive_ok=True, positive_error='',
        positive_rc=0, positive_status='OK', positive_minima={'baseline': 1},
        blocked=5.0, progressed=False, failure='', early_exit=False)
    inputs.update(over)
    return dirty_tree_control_1555.lock_verdict(**inputs)


def dirty_cell(**over):
    """One `dirty_verdict` call over the green defaults of the dirty-src row."""
    inputs = dict(
        restored=True, mode_lost=False, complaint='', failure='',
        provenance={'src_dirty': True, 'negctl_restore': 'restored under the tree lock'},
        returncode=2, driver_status='INVALID', failures=[], built=False,
        refused=True)
    inputs.update(over)
    return dirty_tree_control_1555.dirty_verdict(**inputs)


class VerdictGridTests(unittest.TestCase):
    """(#1713) Every verdict cell, asserted on its exact reason string."""

    def test_lock_verdict_cells(self):
        status, reason = lock_cell(early='lock-acquired during the hold\n')
        self.assertTrue(status == 'SURVIVED' and reason ==
                        'the driver acquired the shared lock while the control held it',
                        '#1713 lock cell 1: acquiring the shared lock under the hold is SURVIVED')
        status, reason = lock_cell(progressed=True)
        self.assertTrue(status == 'SURVIVED' and reason ==
                        'the driver built while the control held the lock '
                        '(build.log or summary.json appeared during the hold)',
                        '#1713 lock cell 2: building under the hold is SURVIVED')
        status, reason = lock_cell(failure='TimeoutError: source lock busy: /lock')
        self.assertTrue(status == 'INVALID' and reason ==
                        'TimeoutError: source lock busy: /lock',
                        '#1713 lock cell 3: a control-side failure is INVALID')
        status, reason = lock_cell(positive_ok=False, positive_error='boom',
                                   positive_rc=None, positive_status=None,
                                   positive_minima=None)
        self.assertTrue(status == 'INVALID' and reason ==
                        'clean-tree positive control failed: boom rc=None status=None '
                        'minima=None',
                        '#1713 lock cell 4: a failed positive control is INVALID')
        status, reason = lock_cell(early='starting\n')
        self.assertTrue(status == 'INVALID' and reason ==
                        "the driver never asked for the lock during the hold "
                        "(early='starting', early_exit=False)",
                        '#1713 lock cell 5: never asking for the lock is INVALID')
        status, reason = lock_cell(full='starting\nlock-requested 5.0\nprovenance x\n',
                                   returncode=1, driver_status='FAILED')
        self.assertTrue(status == 'SURVIVED' and reason ==
                        'the driver asked for the shared lock, never printed '
                        'lock-acquired, and went on to print provenance -- it ran '
                        "without the lock (rc=1 status='FAILED')",
                        '#1713 lock cell 6: provenance without lock-acquired is SURVIVED')
        status, reason = lock_cell(full='starting\nlock-requested 5.0\n',
                                   returncode=0)
        self.assertTrue(status == 'SURVIVED' and reason ==
                        'the driver exited cleanly (rc=0) without ever printing '
                        'lock-acquired or provenance -- it ran without the lock',
                        '#1713 lock cell 7: a clean exit without the lock is SURVIVED')
        status, reason = lock_cell(full='starting\nlock-requested 5.0\n',
                                   returncode=2, driver_status=None)
        self.assertTrue(status == 'INVALID' and reason ==
                        'the driver never reached the lock: no lock-acquired and no '
                        'provenance in its output (rc=2 status=None)',
                        '#1713 lock cell 8: dying before the lock is INVALID')
        status, reason = lock_cell(blocked=0.5)
        self.assertTrue(status == 'INVALID' and reason ==
                        'the request appeared 0.50s before the release, shorter '
                        'than the 2.0s needed to prove a wait',
                        '#1713 lock cell 9: a too-short block is INVALID')
        status, reason = lock_cell(returncode=3)
        self.assertTrue(status == 'INVALID' and reason ==
                        'driver exit 3 after the release',
                        '#1713 lock cell 10: a non-zero driver exit is INVALID')
        status, reason = lock_cell(driver_status='FAILED')
        self.assertTrue(status == 'INVALID' and reason ==
                        "driver status 'FAILED' after the release",
                        '#1713 lock cell 11: a non-OK driver status is INVALID')
        status, reason = lock_cell()
        self.assertTrue(status == 'KILLED' and reason == '',
                        '#1713 lock cell 12: the green row is KILLED with no reason')

    def test_dirty_verdict_cells(self):
        status, reason = dirty_cell(restored=False,
                                    complaint='the restore did not read back as HEAD bytes')
        self.assertTrue(status == 'INVALID' and reason ==
                        'the restore did not read back as HEAD bytes',
                        '#1713 dirty cell 1: unrestored bytes are INVALID')
        status, reason = dirty_cell(mode_lost=True,
                                    complaint='restored the bytes but not the file mode')
        self.assertTrue(status == 'INVALID' and reason ==
                        'restored the bytes but not the file mode',
                        '#1713 dirty cell 2: a lost file mode is INVALID')
        status, reason = dirty_cell(provenance={
            'src_dirty': True, 'negctl_restore_mode_lost': True,
            'negctl_restore': 'restored the bytes but not the file mode (stat or chmod failed)'})
        self.assertTrue(status == 'INVALID' and reason ==
                        "the driver restored the marker bytes but not the file mode: "
                        "'restored the bytes but not the file mode (stat or chmod failed)'",
                        '#1713 dirty cell 3: the driver losing the mode is INVALID')
        status, reason = dirty_cell(failure='TimeoutExpired: driver did not exit')
        self.assertTrue(status == 'INVALID' and reason ==
                        'TimeoutExpired: driver did not exit',
                        '#1713 dirty cell 4: a control-side failure is INVALID')
        status, reason = dirty_cell(provenance={}, returncode=0)
        self.assertTrue(status == 'SURVIVED' and reason ==
                        'the driver exited cleanly (rc=0) without ever printing '
                        'provenance -- it ran to the end without reaching the dirty check',
                        '#1713 dirty cell 5: a clean exit without provenance is SURVIVED')
        status, reason = dirty_cell(provenance={}, returncode=2,
                                    driver_status=None)
        self.assertTrue(status == 'INVALID' and reason ==
                        'the driver never reached the dirty check (no provenance in '
                        'its summary: rc=2 status=None); a crash before the lock '
                        'says nothing about the gate',
                        '#1713 dirty cell 6: dying before the dirty check is INVALID')
        status, reason = dirty_cell()
        self.assertTrue(status == 'KILLED' and reason == '',
                        '#1713 dirty cell 7: the refusal verdict is KILLED')
        status, reason = dirty_cell(refused=False, returncode=0,
                                    driver_status='OK',
                                    provenance={'src_dirty': False}, built=True)
        self.assertTrue(status == 'SURVIVED' and reason ==
                        "rc=0 status='OK' src_dirty=False failures=[] built=True",
                        '#1713 dirty cell 8: anything but the refusal is SURVIVED')


class HandoffTests(unittest.TestCase):
    """(#1695) The lock handoff never leaves the marker exposed."""

    def test_handoff_lock_blocks_a_second_taker(self):
        with tempfile.TemporaryDirectory(prefix='1695-exclusive-') as directory:
            work = Path(directory)
            fd = dirty_tree_control_1555.handoff_lock(work)
            try:
                with self.assertRaises(TimeoutError,
                                       msg='#1695 a second handoff of the same lock must time out'):
                    dirty_tree_control_1555.handoff_lock(work, timeout=0.3)
            finally:
                os.close(fd)

    def test_handoff_window_is_atomic(self):
        # Matrix cells 2-6 of the #1695 plan: the marker is written under the
        # control's descriptor, the driver is started with that descriptor in
        # `pass_fds`, the control closes its copy without unlocking, and a
        # sibling polls LOCK_NB across the whole window. It must never win.
        with tempfile.TemporaryDirectory(prefix='1695-handoff-') as directory:
            work = Path(directory)
            marker = work / 'marked-source.rs'
            marker.write_bytes(b'one dirtied tracked byte\n')
            fd = dirty_tree_control_1555.handoff_lock(work)
            process = None
            sibling = None
            try:
                sibling = subprocess.Popen(
                    [sys.executable, '-I', '-c', SIBLING_PROBE,
                     str(work / '.negative-source.lock'), '1.0'],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

                def stub_launch(out, env, name, log, pass_fds=()):
                    # The real `launch` shape, but with the stub driver: the
                    # handoff itself must go through the real code path.
                    return subprocess.Popen(
                        [sys.executable, '-I', '-c', STUB_DRIVER], cwd=str(out),
                        env=env, stdout=log, stderr=subprocess.STDOUT,
                        text=True, start_new_session=True,
                        pass_fds=tuple(pass_fds))

                driver_env = {'PDRS_NEGCTL_LOCK_FD': str(fd)}
                log = (work / 'probe-dirty.log').open('w')
                try:
                    with patch.object(dirty_tree_control_1555, 'launch', stub_launch):
                        process = dirty_tree_control_1555.launch_under_lock(
                            work, driver_env, 'probe-dirty', log, fd)
                    fd = None  # the stub's copy is now the only reference
                finally:
                    log.close()
                sibling.wait(timeout=15)
                process.wait(timeout=15)
                self.assertTrue(sibling.returncode == 0,
                                '#1695 the sibling never acquired the tree lock '
                                'while the marker was on disk')
                self.assertTrue(process.returncode == 0,
                                '#1695 the stub driver exited cleanly after its hold')
                released = os.open(work / '.negative-source.lock', os.O_RDWR)
                try:
                    try:
                        fcntl.flock(released, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        free = True
                    except BlockingIOError:
                        free = False
                finally:
                    os.close(released)
                self.assertTrue(free,
                                '#1695 the lock is free again once the driver exited')
            finally:
                if fd is not None:
                    os.close(fd)
                for child in (sibling, process):
                    if child is not None and child.poll() is None:
                        child.kill()
                        child.wait(timeout=5)


    def test_handoff_lock_returns_a_non_stdio_fd(self):
        # fix-r1 (boundary P3): a daemonized caller with stdio closed can see
        # the lock descriptor land on 0/1/2, where the child's stdio dup2
        # would clobber it; the handoff must move it out of that range and
        # still hold the real lock.
        with tempfile.TemporaryDirectory(prefix='1695-lowfd-') as directory:
            work = Path(directory)
            saved = os.dup(0)
            try:
                os.close(0)  # the next open() inside handoff_lock returns 0
                fd = dirty_tree_control_1555.handoff_lock(work)
                try:
                    self.assertTrue(fd > 2,
                                    '#1695 the handoff descriptor never shares '
                                    'a number with stdio')
                    probe = os.open(work / '.negative-source.lock', os.O_RDWR)
                    try:
                        try:
                            fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
                            free = True
                        except BlockingIOError:
                            free = False
                    finally:
                        os.close(probe)
                    self.assertTrue(not free,
                                    '#1695 the moved descriptor still holds '
                                    'the real lock')
                finally:
                    os.close(fd)
            finally:
                os.dup2(saved, 0)
                os.close(saved)


class KillTreeTests(unittest.TestCase):
    """(#1705) A timeout kill must reach the setsid descendants."""

    def test_descendant_groups_grid(self):
        census = [(1, 0, 1, 'S'), (10, 1, 10, 'S'), (20, 10, 20, 'S'),
                  (30, 20, 30, 'Z'), (40, 30, 10, 'S'), (50, 1, 50, 'S')]
        # 10 leads to 20 (own group) to 30 (own group, a zombie -- still
        # enumerated, signalling it is a no-op); 40 sits in the leader's
        # group; 50 is not a descendant. All of 10's side must come
        # back, and nothing else.
        groups = dirty_tree_control_1555.descendant_groups(10, census)
        self.assertTrue(groups == {10, 20, 30},
                        '#1705 every descendant group at or below the leader '
                        'is enumerated')
        self.assertTrue(dirty_tree_control_1555.descendant_groups(99, census) == set(),
                        '#1705 an unknown leader enumerates nothing')

    def test_kill_tree_reaches_setsid_descendants(self):
        with tempfile.TemporaryDirectory(prefix='1705-killtree-') as directory:
            work = Path(directory)
            pid_file = work / 'grandchild.pid'
            process = subprocess.Popen(
                [sys.executable, '-I', '-c', TREE_DRIVER, str(pid_file)],
                start_new_session=True, stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL)
            grandchild = None
            try:
                deadline = time.monotonic() + 15
                while (not pid_file.exists() and time.monotonic() < deadline
                       and process.poll() is None):
                    time.sleep(0.05)
                self.assertTrue(pid_file.exists(),
                                '#1705 the stub driver reported its setsid child')
                grandchild = int(pid_file.read_text().strip())
                dirty_tree_control_1555._kill_tree(process)
                process.wait(timeout=15)
                gone = False
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    try:
                        os.kill(grandchild, 0)
                        time.sleep(0.05)
                    except ProcessLookupError:
                        gone = True
                        break
                self.assertTrue(gone,
                                '#1705 the setsid descendant died with the driver')
            finally:
                if grandchild is not None:
                    try:
                        os.killpg(grandchild, signal.SIGKILL)
                    except (ProcessLookupError, PermissionError):
                        pass
                if process.poll() is None:
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except (ProcessLookupError, PermissionError):
                        pass
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        pass

    def _sleeper(self):
        return subprocess.Popen(
            [sys.executable, '-I', '-c', 'import time; time.sleep(120)'],
            start_new_session=True, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL)

    def _reap(self, process):
        if process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass

    def test_kill_tree_swallows_census_timeout(self):
        # fix-r1 (sibling#1 / concurrency#1 / boundary#2, promoted P1): `ps`
        # timing out raises subprocess.TimeoutExpired, a SubprocessError that
        # is NOT an OSError; the cleanup must fall back to the leader-group
        # kill and never raise out of _kill_tree.
        process = self._sleeper()
        killed = []
        try:
            with patch.object(dirty_tree_control_1555, 'process_census',
                              side_effect=subprocess.TimeoutExpired('ps', 30)), \
                 patch.object(dirty_tree_control_1555, '_kill_group',
                              lambda proc: killed.append(proc.pid)), \
                 redirect_stderr(io.StringIO()) as captured:
                raised = None
                try:
                    dirty_tree_control_1555._kill_tree(process)
                except Exception as error:
                    raised = error
            self.assertTrue(raised is None,
                            '#1705 a ps timeout during the census never '
                            'escapes the cleanup')
            self.assertTrue(killed == [process.pid],
                            '#1705 a ps timeout falls back to the driver '
                            'group kill')
            self.assertTrue('#1705 ps census failed (TimeoutExpired' in
                            captured.getvalue(),
                            '#1705 a ps timeout is reported, not silent')
        finally:
            self._reap(process)

    def test_kill_tree_swallows_group_kill_permission_error(self):
        # fix-r1 (boundary P2): negative_process._kill_group re-raises its
        # PermissionError after the reap-and-retry (the macOS EPERM on an
        # unreaped exited leader); the cleanup must print it, not raise it.
        process = self._sleeper()
        try:
            census = [(process.pid, os.getpid(), process.pid, 'S')]
            with patch.object(dirty_tree_control_1555, 'process_census',
                              return_value=census), \
                 patch.object(dirty_tree_control_1555, '_kill_group',
                              side_effect=PermissionError('EPERM: leader group')), \
                 redirect_stderr(io.StringIO()) as captured:
                raised = None
                try:
                    dirty_tree_control_1555._kill_tree(process)
                except Exception as error:
                    raised = error
            self.assertTrue(raised is None,
                            '#1705 a group-kill PermissionError never escapes '
                            'the cleanup')
            self.assertTrue('#1705 the driver group kill failed too '
                            '(PermissionError' in captured.getvalue(),
                            '#1705 a group-kill PermissionError is reported, '
                            'not silent')
        finally:
            self._reap(process)

    def test_kill_tree_reports_a_dead_leader(self):
        # fix-r1 (concurrency#2): the caller's poll()/wait() liveness check and
        # the ps census are two instants; a leader that exits in between is a
        # zombie whose children are already reparented, so the ppid walk would
        # enumerate nobody. The census's state column must turn that into a
        # visible leader-group fallback, not a silent empty kill.
        with tempfile.TemporaryDirectory(prefix='1705-dead-') as directory:
            work = Path(directory)
            pid_file = work / 'grandchild.pid'
            process = subprocess.Popen(
                [sys.executable, '-I', '-c', TREE_DRIVER_EXITING, str(pid_file)],
                start_new_session=True, stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL)
            grandchild = None
            killed = []
            try:
                deadline = time.monotonic() + 15
                while (not pid_file.exists() and time.monotonic() < deadline
                       and process.poll() is None):
                    time.sleep(0.05)
                self.assertTrue(pid_file.exists(),
                                '#1705 the stub driver reported its setsid child')
                grandchild = int(pid_file.read_text().strip())
                # Wait for the exit WITHOUT reaping (WNOWAIT): the census must
                # see the zombie row, the exact race shape from the review.
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    if os.waitid(os.P_PID, process.pid,
                                 os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None:
                        break
                    time.sleep(0.05)
                with patch.object(dirty_tree_control_1555, '_kill_group',
                                  lambda proc: killed.append(proc.pid)), \
                     redirect_stderr(io.StringIO()) as captured:
                    dirty_tree_control_1555._kill_tree(process)
                self.assertTrue(killed == [process.pid],
                                '#1705 a dead leader still gets its group kill')
                self.assertTrue('#1705 the driver exited before the ps census'
                                in captured.getvalue(),
                                '#1705 a leader that died before the census is '
                                'reported, not silently skipped')
                # The reparented setsid grandchild is by design unreachable
                # from here; the note above is what keeps that visible.
            finally:
                if grandchild is not None:
                    try:
                        os.killpg(grandchild, signal.SIGKILL)
                    except (ProcessLookupError, PermissionError):
                        pass
                process.wait(timeout=5)


class EnvHygieneTests(unittest.TestCase):
    """(#1695 fix-r1) Control-injection variables never reach the driver env."""

    def test_sanitized_env_drops_injection_variables(self):
        with patch.dict(os.environ, {'PDRS_NEGCTL_LOCK_FD': '9',
                                     'PDRS_NEGCTL_RESTORE':
                                         'src/x.rs\x1fa\x1fb'}):
            env = dirty_tree_control_1555.sanitized_env()
        self.assertTrue('PDRS_NEGCTL_LOCK_FD' not in env
                        and 'PDRS_NEGCTL_RESTORE' not in env,
                        '#1695 inherited control-injection variables are '
                        'dropped from the driver environment')


class EarlyLinesTests(unittest.TestCase):
    """(#1695 fix-r1) An empty early window stays an empty list."""

    def test_early_lines_shape(self):
        self.assertTrue(dirty_tree_control_1555.early_lines('') == [],
                        "#1695 an empty early window is an empty list, not ['']")
        self.assertTrue(dirty_tree_control_1555.early_lines('  \n  \n') == [],
                        '#1695 a whitespace-only early window is an empty list')
        lines = dirty_tree_control_1555.early_lines(
            'starting\nlock-requested 5.0\nlock-acquired\nprovenance\n')
        self.assertTrue(lines == ['starting', 'lock-requested 5.0',
                                  'lock-acquired'],
                        '#1695 early_lines keeps the first three whole lines')


# These are Python-only branch mutations of the control module, compiled in
# memory (the #1128 jsobject-density pattern). No product source, no index
# change, no cargo run. Each row names the one test and the one marker line
# that must kill it.
IN_MEMORY_MUTANTS = [
    ('handoff-unlocks-at-the-seam', dirty_tree_control_1555,
     '    process = launch(out, env, name, log, pass_fds=(fd,))\n'
     '    os.close(fd)\n'
     '    return process',
     '    process = launch(out, env, name, log, pass_fds=(fd,))\n'
     '    fcntl.flock(fd, fcntl.LOCK_UN)\n'
     '    os.close(fd)\n'
     '    return process',
     HandoffTests, 'test_handoff_window_is_atomic',
     '#1695 the sibling never acquired the tree lock while the marker was on disk'),
    ('kill-tree-leader-group-only', dirty_tree_control_1555,
     '    for pgid in sorted(descendant_groups(process.pid, census)):',
     '    for pgid in ():',
     KillTreeTests, 'test_kill_tree_reaches_setsid_descendants',
     '#1705 the setsid descendant died with the driver'),
    ('descendants-never-walked', dirty_tree_control_1555,
     '            if child not in found:',
     '            if False:',
     KillTreeTests, 'test_descendant_groups_grid',
     '#1705 every descendant group at or below the leader is enumerated'),
    ('lock-verdict-ignores-a-passed-lock', dirty_tree_control_1555,
     '    if not acquired and passed_lock:',
     '    if False:',
     VerdictGridTests, 'test_lock_verdict_cells',
     '#1713 lock cell 6: provenance without lock-acquired is SURVIVED'),
    ('lock-verdict-ignores-a-clean-exit', dirty_tree_control_1555,
     '    if not acquired and returncode == 0:',
     '    if False:',
     VerdictGridTests, 'test_lock_verdict_cells',
     '#1713 lock cell 7: a clean exit without the lock is SURVIVED'),
    ('lock-verdict-ignores-control-failure', dirty_tree_control_1555,
     '    if failure:\n'
     "        # A control-side failure (lock timeout, launch error) dominates: it",
     '    if False:\n'
     "        # A control-side failure (lock timeout, launch error) dominates: it",
     VerdictGridTests, 'test_lock_verdict_cells',
     '#1713 lock cell 3: a control-side failure is INVALID'),
    ('dirty-verdict-ignores-the-refusal', dirty_tree_control_1555,
     '    if refused:',
     '    if False:',
     VerdictGridTests, 'test_dirty_verdict_cells',
     '#1713 dirty cell 7: the refusal verdict is KILLED'),
    # fix-r1 cluster 1 (three review roles hit the same hole): a ps timeout is
    # a SubprocessError, not an OSError, and _kill_group's EPERM-after-retry
    # must not escape either -- cleanup never masks the timeout it answers.
    ('kill-tree-raises-on-census-timeout', dirty_tree_control_1555,
     '    except (OSError, ValueError, subprocess.SubprocessError) as error:',
     '    except (OSError, ValueError) as error:',
     KillTreeTests, 'test_kill_tree_swallows_census_timeout',
     '#1705 a ps timeout during the census never escapes the cleanup'),
    ('kill-tree-raises-on-group-kill-eperm', dirty_tree_control_1555,
     '        _kill_group(process)\n    except OSError as error:',
     '        _kill_group(process)\n    except ValueError as error:',
     KillTreeTests, 'test_kill_tree_swallows_group_kill_permission_error',
     '#1705 a group-kill PermissionError never escapes the cleanup'),
    # fix-r1 concurrency#2: a leader that died before the census must be
    # reported and answered with the leader-group fallback, not silently
    # walked as an empty tree.
    ('kill-tree-censuses-a-dead-leader', dirty_tree_control_1555,
     "    if leader_state is None or leader_state.startswith('Z'):",
     '    if False:',
     KillTreeTests, 'test_kill_tree_reports_a_dead_leader',
     '#1705 a leader that died before the census is reported, not silently '
     'skipped'),
    # fix-r1 boundary#4: the handoff descriptor must never share a number with
    # the child's stdio.
    ('handoff-keeps-a-stdio-descriptor', dirty_tree_control_1555,
     '        if fd <= 2:',
     '        if False:',
     HandoffTests, 'test_handoff_lock_returns_a_non_stdio_fd',
     '#1695 the handoff descriptor never shares a number with stdio'),
    # fix-r1 sibling#3: the restore spec is as much a control-injection
    # variable as the lock fd.
    ('env-hygiene-keeps-the-restore-spec', dirty_tree_control_1555,
     "    env.pop('PDRS_NEGCTL_RESTORE', None)",
     '    pass',
     EnvHygieneTests, 'test_sanitized_env_drops_injection_variables',
     '#1695 inherited control-injection variables are dropped from the driver '
     'environment'),
    # fix-r1 boundary#3 / simplify#6: an empty early window must stay [].
    ('early-lines-keeps-empty-strings', dirty_tree_control_1555,
     "    return [line for line in text.strip().split('\\n') if line][:limit]",
     "    return text.strip().split('\\n')[:limit]",
     EarlyLinesTests, 'test_early_lines_shape',
     "#1695 an empty early window is an empty list, not ['']"),
    # fix-r1 sibling#6: --negative previously pinned only 3 of 12 lock units
    # and 4 of 8 dirty units; the remaining branch heads get their own
    # mutants so every grid assertion is provably falsifiable.
    ('lock-verdict-ignores-a-passed-lock-during-hold',
     dirty_tree_control_1555,
     "    if 'lock-acquired' in early:",
     '    if False:',
     VerdictGridTests, 'test_lock_verdict_cells',
     '#1713 lock cell 1: acquiring the shared lock under the hold is SURVIVED'),
    ('lock-verdict-ignores-progress-under-hold', dirty_tree_control_1555,
     '    if progressed:',
     '    if False:',
     VerdictGridTests, 'test_lock_verdict_cells',
     '#1713 lock cell 2: building under the hold is SURVIVED'),
    ('lock-verdict-ignores-the-min-block-band', dirty_tree_control_1555,
     '    if blocked < MIN_BLOCK_SECONDS:',
     '    if False:',
     VerdictGridTests, 'test_lock_verdict_cells',
     '#1713 lock cell 9: a too-short block is INVALID'),
    # The restore branch is one head for two cells, so each half is mutated on
    # its own to keep the kill bound to exactly one marker.
    ('dirty-verdict-ignores-unrestored-bytes', dirty_tree_control_1555,
     '    if not restored or mode_lost:',
     '    if mode_lost:',
     VerdictGridTests, 'test_dirty_verdict_cells',
     '#1713 dirty cell 1: unrestored bytes are INVALID'),
    ('dirty-verdict-ignores-a-lost-mode', dirty_tree_control_1555,
     '    if not restored or mode_lost:',
     '    if not restored:',
     VerdictGridTests, 'test_dirty_verdict_cells',
     '#1713 dirty cell 2: a lost file mode is INVALID'),
    ('dirty-verdict-ignores-driver-mode-loss', dirty_tree_control_1555,
     "    if provenance.get('negctl_restore_mode_lost'):",
     '    if False:',
     VerdictGridTests, 'test_dirty_verdict_cells',
     '#1713 dirty cell 3: the driver losing the mode is INVALID'),
    ('dirty-verdict-ignores-control-failure', dirty_tree_control_1555,
     '    if failure:\n'
     '        # A control-side failure (handoff timeout, launch error, watchdog\n',
     '    if False:\n'
     '        # A control-side failure (handoff timeout, launch error, watchdog\n',
     VerdictGridTests, 'test_dirty_verdict_cells',
     '#1713 dirty cell 4: a control-side failure is INVALID'),
    ('dirty-verdict-ignores-never-reached', dirty_tree_control_1555,
     '    if not provenance:',
     '    if False:',
     VerdictGridTests, 'test_dirty_verdict_cells',
     '#1713 dirty cell 6: dying before the dirty check is INVALID'),
]


def bound_run(cls, method, log):
    with redirect_stdout(log):
        return unittest.TextTestRunner(stream=log, verbosity=2).run(cls(method))


def run_negative():
    counts = dict(KILLED=0, SURVIVED=0, INVALID=0)
    for name, module, old, new, cls, method, marker in IN_MEMORY_MUTANTS:
        status, reason = 'INVALID', 'not run'
        log = io.StringIO()
        try:
            source = Path(module.__file__).read_text()
            if source.count(old) != 1:
                raise ValueError('stale Python mutation anchor')
            if not bound_run(cls, method, log).wasSuccessful():
                raise ValueError('positive bound test failed')
            candidate = types.ModuleType(module.__name__)
            candidate.__file__ = module.__file__
            exec(compile(source.replace(old, new, 1), module.__file__, 'exec'),
                 candidate.__dict__)
            with patch.dict(globals(), {module.__name__: candidate}):
                result = bound_run(cls, method, log)
            failures = result.failures
            if result.wasSuccessful():
                status, reason = 'SURVIVED', 'bound test passed'
            elif not result.errors and failures and all(
                    getattr(test, 'test_case', test).id() ==
                    f'{cls.__module__}.{cls.__name__}.{method}' and
                    any(line.startswith('AssertionError:') and marker in line
                        for line in trace.split('\n')) for test, trace in failures):
                status, reason = 'KILLED', marker
            else:
                reason = 'missing bound assertion (unexpected error or different failure)'
            if not bound_run(cls, method, log).wasSuccessful():
                status, reason = 'INVALID', 'restored bound test failed'
        except Exception as error:
            reason = f'{type(error).__name__}: {error}'
        counts[status] += 1
        print(log.getvalue(), end='')
        print(f'{name}: {status}: {cls.__module__}.{cls.__name__}.{method}: '
              f'{reason}', flush=True)
    print(f'#1695 Python negative controls: {counts}', flush=True)
    return int(counts['SURVIVED'] != 0 or counts['INVALID'] != 0)


if __name__ == '__main__':
    if sys.argv[1:] == ['--negative']:
        raise SystemExit(run_negative())
    unittest.main(verbosity=2)

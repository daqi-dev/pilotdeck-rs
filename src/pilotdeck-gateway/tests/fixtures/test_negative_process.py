# anchor-selfcheck: unsupported unit tests of the shared process/source guard
"""Run directly with Python; no network or product fixtures needed; the shared guard may bootstrap its Rust scanner offline.

The source-window cases create a throwaway git repository under the system
temporary directory, so they exercise the real baseline check without touching
this worktree.
"""
import errno
import io
import json
from contextlib import contextmanager, nullcontext
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from types import ModuleType
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from negative_process import (SourceBaselineError, exclusive_write,
                              mutating_sources, new_output_directory,
                              node_version, published_module, run_owned,
                              tree_lock, wait_for_slot)
import negative_process


SIGKILL_RESIDUE = r"""
import pathlib, sys, time
sys.path.insert(0, sys.argv[1])
from negative_process import mutating_sources
root, target, control = map(pathlib.Path, sys.argv[2:5])
with mutating_sources(root, [target]):
    target.write_text("mutant-residue\n")
    (control / "mutated").write_text("1")
    time.sleep(60)
"""


SERIAL_WRITER = r"""
import pathlib, sys, time
sys.path.insert(0, sys.argv[1])
from contextlib import nullcontext
from negative_process import tree_lock
root, source, control = map(pathlib.Path, sys.argv[2:5])
name = sys.argv[5]
use_lock = sys.argv[6] == "lock"
control.mkdir(parents=True, exist_ok=True)
(control / ("start-" + name)).write_text("")
while not (control / "start-a").exists() or not (control / "start-b").exists():
    time.sleep(0.005)
context = tree_lock(root, timeout=20) if use_lock else nullcontext()
with context:
    observed = source.read_text()
    (control / ("observed-" + name)).write_text(observed)
    source.write_text("mutated-" + name)
    window = control / ("window-" + name)
    window.write_text("")
    other = "b" if name == "a" else "a"
    deadline = time.monotonic() + 1.0
    while time.monotonic() < deadline:
        if (control / ("window-" + other)).exists():
            (control / ("overlap-" + name)).write_text("")
            break
        time.sleep(0.005)
    source.write_text(observed)
    window.unlink()
"""


READER_PROBE = r"""
import pathlib, sys
sys.path.insert(0, sys.argv[1])
from contextlib import nullcontext
from negative_process import tree_lock
root, source, control = map(pathlib.Path, sys.argv[2:5])
(control / "reader-started").write_text("")
with (tree_lock(root, timeout=20) if sys.argv[5] == "lock" else nullcontext()):
    (control / "observed").write_text(source.read_text())
"""


HOLDER_PROBE = r"""
import pathlib, sys, time
sys.path.insert(0, sys.argv[1])
from contextlib import nullcontext
from negative_process import tree_lock
root, source, control = map(pathlib.Path, sys.argv[2:5])
with (tree_lock(root, timeout=20) if sys.argv[5] == "lock" else nullcontext()):
    original = source.read_text()
    source.write_text("mutated")
    # Publish atomically: the parent polls for existence, then reads.
    (control / "mutated.tmp").write_text(original)
    (control / "mutated.tmp").replace(control / "mutated")
    while not (control / "release").exists():
        time.sleep(0.005)
    source.write_text(original)
"""


SIGKILL_HOLDER = r"""
import pathlib, sys, time
sys.path.insert(0, sys.argv[1])
from negative_process import tree_lock
root, source, control = map(pathlib.Path, sys.argv[2:5])
with tree_lock(root, timeout=20):
    (control / "held").write_text(source.read_text())
    time.sleep(60)
"""


class ProcessControls(unittest.TestCase):
    def test_exited_leader_permission_race_retries_group_signal(self):
        communicate = subprocess.Popen.communicate
        calls = []

        def completed_at_deadline(process, *args, **kwargs):
            stdout, stderr = communicate(process, *args, **kwargs)
            if not calls:
                calls.append(process.pid)
                raise subprocess.TimeoutExpired(process.args, 2, stdout, stderr)
            return stdout, stderr

        attempts = []

        def permission_then_retry(pid, sig):
            self.assertEqual(sig, signal.SIGKILL)
            attempts.append(pid)
            if len(attempts) == 1:
                raise PermissionError(errno.EPERM, "exited leader sandbox race")
            # Exercise the retry contract deterministically. Real group KILL
            # is verified by the owned-tree integration test; this sandbox can
            # report EPERM even after an empty group's leader has been reaped.
            raise ProcessLookupError(errno.ESRCH, "group gone after reap")

        with patch("negative_process.subprocess.Popen.communicate", completed_at_deadline), \
                patch("negative_process.os.killpg", permission_then_retry):
            with self.assertRaises(subprocess.TimeoutExpired) as caught:
                run_owned([sys.executable, "-c", "print('exited')"], timeout=2)
        self.assertEqual(caught.exception.output, "exited\n")
        self.assertEqual(attempts, calls * 2, "must retry the actual group after reaping")

    def test_timeout_kills_child_and_grandchild(self):
        # The group leader records readiness only after its own child is alive.
        # Give the descendant its own pipes so killing only the leader returns
        # promptly and the PID assertion independently detects the orphan.
        child = "import time; print('child-ready', flush=True); time.sleep(60)"
        leader = (
            "import os, pathlib, subprocess, sys, time; "
            f"p = subprocess.Popen([sys.executable, '-c', {child!r}], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True); "
            "assert p.stdout.readline().strip() == 'child-ready'; "
            "pathlib.Path(sys.argv[1]).write_text(str(os.getpid()) + ' ' + str(p.pid)); "
            "print('tree-ready', flush=True); time.sleep(60)"
        )
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-process-") as temp:
            pids_file = Path(temp) / "pids"
            pids = []
            try:
                with self.assertRaises(subprocess.TimeoutExpired) as caught:
                    run_owned([sys.executable, "-c", leader, str(pids_file)], timeout=2)
                self.assertIn("tree-ready", caught.exception.output)
                pids = [int(pid) for pid in pids_file.read_text().split()]
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    if all(self.gone(pid) for pid in pids):
                        break
                    time.sleep(0.02)
                self.assertTrue(all(self.gone(pid) for pid in pids), f"surviving owned PIDs: {pids}")
                try:
                    ps = subprocess.run(["ps", "-o", "pid=,ppid=,pgid=,stat=", "-p",
                                         ",".join(map(str, pids))], capture_output=True, text=True)
                except PermissionError as error:
                    print(f"owned PIDs {pids}; kill(pid, 0)=ESRCH; ps denied: {error}; "
                          "coordinator must repeat ps verification", flush=True)
                    return
                print(f"owned PIDs {pids}; kill(pid, 0)=ESRCH; ps exit={ps.returncode}\n"
                      + ps.stdout + ps.stderr, flush=True)
                if ps.returncode in (0, 1) and not ps.stderr:
                    self.assertFalse(ps.stdout.strip(), "ps found an orphan")
            finally:
                # Also clean the tree when deliberately mutating killpg to kill.
                if not pids and pids_file.exists():
                    pids = [int(pid) for pid in pids_file.read_text().split()]
                for pid in reversed(pids):
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass

    def test_setsid_grandchild_cannot_extend_cleanup_past_its_bound(self):
        # A descendant that calls setsid() leaves the owned process group and
        # keeps the inherited pipes open. Killpg therefore cannot make EOF
        # arrive; cleanup must give up on the pipes after kill_timeout instead
        # of waiting for the 900 s command budget (#528). The descendant exits
        # on its own after 15 s so deliberately removing the bound fails the
        # 10 s assertion instead of leaving an orphan behind.
        grandchild = "import time; print('escaped-ready', flush=True); time.sleep(15)"
        leader = (
            "import os, pathlib, subprocess, sys, time; "
            f"p = subprocess.Popen([sys.executable, '-c', {grandchild!r}], "
            "start_new_session=True); "
            "pathlib.Path(sys.argv[1]).write_text(str(os.getpid()) + ' ' + str(p.pid)); "
            "print('leader-ready', flush=True); time.sleep(60)"
        )
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-escape-") as temp:
            pids_file = Path(temp) / "pids"
            pids = []
            try:
                started = time.monotonic()
                with self.assertRaises(subprocess.TimeoutExpired) as caught:
                    run_owned([sys.executable, "-c", leader, str(pids_file)],
                              timeout=1, kill_timeout=1)
                elapsed = time.monotonic() - started
                self.assertIn("leader-ready", caught.exception.output)
                self.assertLess(
                    elapsed, 10,
                    f"cleanup waited on an escaped pipe holder for {elapsed:.1f}s")
                pids = [int(pid) for pid in pids_file.read_text().split()]
            finally:
                # The escaped grandchild survives the group signal by design.
                if not pids and pids_file.exists():
                    pids = [int(pid) for pid in pids_file.read_text().split()]
                for pid in pids:
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass

    @staticmethod
    def gone(pid):
        try:
            os.kill(pid, 0)
        except OSError as error:
            if error.errno == errno.ESRCH:
                return True
            raise
        return False

    def test_slot_wait_has_deadline(self):
        # Isolate the wait budget from subprocess startup: the last retry may
        # have < 1 ms left, which cannot reliably start bash under CI load.
        # Real subprocess timeout/cleanup is covered by the tree and hung gate.
        full = subprocess.CompletedProcess(["gate"], 1, "full\n", "")
        attempts = 0

        def still_full(*_args, **_kwargs):
            nonlocal attempts
            attempts += 1
            self.assertLessEqual(attempts, 100, "slot wait ignored its total budget")
            return full

        started = time.monotonic()
        with patch("negative_process.run_owned", side_effect=still_full) as gate:
            with self.assertRaises(subprocess.TimeoutExpired) as caught:
                wait_for_slot("full.sh", timeout=0.2, interval=0.02)
            self.assertIn("full", caught.exception.output)
            self.assertGreaterEqual(gate.call_count, 2, "must retry a full gate")
            self.assertLess(time.monotonic() - started, 5)

    def test_hung_gate_is_also_killed(self):
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-gate-") as temp:
            script = Path(temp) / "hung.sh"
            script.write_text("echo gate-ready; exec sleep 60\n")
            with self.assertRaises(subprocess.TimeoutExpired) as caught:
                wait_for_slot(script, timeout=0.2, interval=0.02)
            self.assertIn("gate-ready", caught.exception.output)

    def test_slot_success_and_errors(self):
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-gate-") as temp:
            script = Path(temp) / "gate.sh"
            script.write_text("echo available; exit 0\n")
            self.assertEqual(wait_for_slot(script, timeout=2).stdout, "available\n")
            marker = Path(temp) / "retried"
            script.write_text(f'if [ ! -f "{marker}" ]; then touch "{marker}"; '
                              'echo full; exit 1; fi; echo available; exit 0\n')
            self.assertEqual(wait_for_slot(script, timeout=5, interval=0.02).stdout,
                             "full\navailable\n")
            script.write_text("exit 2\n")
            with self.assertRaises(subprocess.CalledProcessError):
                wait_for_slot(script, timeout=2)


    @staticmethod
    def wait_path(path, timeout=15):
        deadline = time.monotonic() + timeout
        while not path.exists():
            if time.monotonic() >= deadline:
                raise AssertionError(f"timed out waiting for {path}")
            time.sleep(0.005)

    def test_tree_lock_serializes_two_writers(self):
        # Both children start from one barrier, then try to enter the mutation
        # window. With flock one must finish before the other enters; the
        # no-lock run is the negative control for this same harness.
        fixtures = str(Path(__file__).resolve().parent)
        result = {}
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-flock-") as temp:
            temp = Path(temp)
            root, source = temp / "tree", temp / "source"
            root.mkdir()
            for mode in ("lock", "nolock"):
                source.write_text("clean")
                control = temp / f"control-{mode}"
                control.mkdir()
                children = [
                    subprocess.Popen(
                        [sys.executable, "-c", SERIAL_WRITER, fixtures, str(root),
                         str(source), str(control), name, mode],
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                    for name in ("a", "b")
                ]
                for child in children:
                    stdout, stderr = child.communicate(timeout=30)
                    self.assertEqual(child.returncode, 0, stdout + stderr)
                result[mode] = {
                    "observed": {name: (control / f"observed-{name}").read_text()
                                 for name in ("a", "b")},
                    "overlap": sorted(p.name for p in control.glob("overlap-*")),
                }
        self.assertEqual(result["lock"]["observed"], {"a": "clean", "b": "clean"})
        self.assertEqual(result["lock"]["overlap"], [])
        # The first writer to check sees the other's mutation window and can
        # unlink before the second check; one overlap already proves both were
        # inside the critical section at once.
        self.assertGreaterEqual(len(result["nolock"]["overlap"]), 1,
                                f"no-lock writers did not overlap: {result}")
        print(f"flock serialization: {result}", flush=True)

    def test_sigkill_releases_lock_and_next_holder_reads_clean(self):
        fixtures = str(Path(__file__).resolve().parent)
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-sigkill-") as temp:
            temp = Path(temp)
            root, source, control = temp / "tree", temp / "source", temp / "control"
            root.mkdir()
            control.mkdir()
            source.write_text("clean")
            holder = subprocess.Popen(
                [sys.executable, "-c", SIGKILL_HOLDER, fixtures, str(root),
                 str(source), str(control)],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                self.wait_path(control / "held")
                self.assertEqual((control / "held").read_text(), "clean")
                holder.kill()  # SIGKILL: no Python finally block runs
                holder.wait(timeout=10)
                started = time.monotonic()
                with tree_lock(root, timeout=5) as lock:
                    observed = source.read_text()
                    self.assertTrue(Path(lock.path).is_file(),
                                    "flock is a persistent file, not a directory")
                elapsed = time.monotonic() - started
            finally:
                if holder.poll() is None:
                    holder.kill()
                holder.communicate(timeout=10)
        self.assertLess(elapsed, 5, "kernel did not release the SIGKILLed holder's lock")
        self.assertEqual(observed, "clean")
        print(f"sigkill lock release: elapsed={elapsed:.3f}s observed={observed!r}", flush=True)

    def test_without_lock_reader_captures_mutated_original(self):
        # Deterministic probe: the holder announces its mutation and waits for
        # an explicit release file. With the lock the reader cannot observe the
        # mutated window; without it the reader is released only after it has
        # read, so it always captures the mutant as its "original".
        fixtures = str(Path(__file__).resolve().parent)
        observed = {}
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-reader-") as temp:
            temp = Path(temp)
            root, source, control = temp / "tree", temp / "source", temp / "control"
            root.mkdir()
            for mode in ("lock", "nolock"):
                control = temp / f"control-{mode}"
                control.mkdir()
                source.write_text("clean")
                holder = subprocess.Popen(
                    [sys.executable, "-c", HOLDER_PROBE, fixtures, str(root),
                     str(source), str(control), mode],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                reader = None
                try:
                    self.wait_path(control / "mutated")
                    self.assertEqual((control / "mutated").read_text(), "clean")
                    reader = subprocess.Popen(
                        [sys.executable, "-c", READER_PROBE, fixtures, str(root),
                         str(source), str(control), mode],
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                    if mode == "nolock":
                        # Reader must finish while the mutation is live.
                        self.wait_path(control / "observed")
                    else:
                        # Wait until the reader is about to call flock; the
                        # holder cannot release before the release file exists,
                        # so the outcome is clean for any scheduling order.
                        self.wait_path(control / "reader-started")
                    (control / "release").write_text("")
                    holder.communicate(timeout=30)
                    reader.communicate(timeout=30)
                    self.assertEqual(holder.returncode, 0)
                    self.assertEqual(reader.returncode, 0)
                    observed[mode] = (control / "observed").read_text()
                finally:
                    (control / "release").write_text("")
                    for process in (holder, reader):
                        if process is not None and process.poll() is None:
                            process.kill()
                            process.communicate(timeout=10)
        self.assertEqual(observed, {"lock": "clean", "nolock": "mutated"},
                         f"lock did not hide the mutated original: {observed}")
        print(f"without-lock capture: {observed}", flush=True)

    def test_lock_wait_has_total_deadline(self):
        fixtures = str(Path(__file__).resolve().parent)
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-timeout-") as temp:
            temp = Path(temp)
            root, source, control = temp / "tree", temp / "source", temp / "control"
            root.mkdir()
            control.mkdir()
            source.write_text("clean")
            holder = subprocess.Popen(
                [sys.executable, "-c", SIGKILL_HOLDER, fixtures, str(root),
                 str(source), str(control)],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                self.wait_path(control / "held")
                started = time.monotonic()
                with self.assertRaises(TimeoutError):
                    with tree_lock(root, timeout=0.2):
                        pass
                elapsed = time.monotonic() - started
            finally:
                holder.kill()
                holder.communicate(timeout=10)
        self.assertLess(elapsed, 5, f"lock timeout waited {elapsed:.1f}s")


class TreeLockBudget(unittest.TestCase):
    """#1142: the caller owns SIGALRM/ITIMER_REAL, even while flock is busy."""

    def setUp(self):
        self.assertTrue(signal.getitimer(signal.ITIMER_REAL) == (0.0, 0.0),
                        '#1142 tests require an isolated process without a timer')
        self.previous_handler = signal.getsignal(signal.SIGALRM)
        self.addCleanup(signal.signal, signal.SIGALRM, self.previous_handler)
        self.addCleanup(signal.setitimer, signal.ITIMER_REAL, 0)

        def expired(_signum, _frame):
            raise SystemExit('#1142 caller deadline')

        self.handler = expired
        signal.signal(signal.SIGALRM, self.handler)

    @contextmanager
    def held_lock(self, root):
        # A distinct open-file description holds the real kernel flock. No
        # source mutation or alternate Git baseline is involved in this probe.
        fd = os.open(Path(root) / '.negative-source.lock', os.O_RDWR | os.O_CREAT, 0o600)
        try:
            negative_process.fcntl.flock(fd, negative_process.fcntl.LOCK_EX)
            yield fd
        finally:
            os.close(fd)

    def arm_timer(self, budget, interval=None):
        before = time.monotonic()
        if interval is None:
            signal.alarm(budget)
        else:
            signal.setitimer(signal.ITIMER_REAL, budget, interval)
        return before, time.monotonic()

    def assert_budget(self, budget, started, interval=0.0):
        before = time.monotonic()
        remaining, repeat = signal.getitimer(signal.ITIMER_REAL)
        after = time.monotonic()
        # Bracket both syscalls so a descheduled test cannot turn time between
        # monotonic/getitimer samples into a spurious budget mismatch.
        lower = budget - (after - started[0])
        upper = budget - (before - started[1])
        self.assertTrue(remaining > 0 and lower - 0.01 <= remaining <= upper + 0.01,
                        '#1142 caller timer must retain only its remaining budget')
        self.assertTrue(repeat == interval, '#1142 caller timer interval must survive')
        self.assertIs(signal.getsignal(signal.SIGALRM), self.handler,
                      '#1142 caller signal handler must survive')
        print(f'#1142 budget={budget} elapsed={after - started[1]:.6f} remaining={remaining:.6f} '
              f'interval={repeat}', flush=True)

    def test_no_timer_is_created(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1142-idle-') as root:
            with tree_lock(root):
                self.assertTrue(signal.getitimer(signal.ITIMER_REAL) == (0.0, 0.0),
                                '#1142 idle acquisition must not arm a timer')
                self.assertIs(signal.getsignal(signal.SIGALRM), self.handler,
                              '#1142 caller signal handler must survive')
            self.assertTrue(signal.getitimer(signal.ITIMER_REAL) == (0.0, 0.0),
                            '#1142 idle release must not arm a timer')

    def test_alarm_survives_acquisition_and_nested_release(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1142-alarm-') as root:
            started = self.arm_timer(10)
            with tree_lock(root):
                with tree_lock(root):
                    time.sleep(0.2)
                    self.assert_budget(10, started)
                self.assert_budget(10, started)
            self.assert_budget(10, started)

    def test_periodic_timer_survives(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1142-periodic-') as root:
            started = self.arm_timer(10, interval=2)
            with tree_lock(root):
                self.assert_budget(10, started, interval=2)
            self.assert_budget(10, started, interval=2)

    def test_alarm_keeps_elapsed_wait_when_holder_releases(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1142-release-') as root:
            with self.held_lock(root) as held:
                attempted = threading.Event()
                flock = negative_process.fcntl.flock

                def release():
                    # Even a broken waiter cannot strand this helper thread.
                    attempted.wait(5)
                    time.sleep(0.2)
                    flock(held, negative_process.fcntl.LOCK_UN)

                def acquire(fd, operation):
                    attempted.set()
                    return flock(fd, operation)

                holder = threading.Thread(target=release)
                holder.start()
                try:
                    started = self.arm_timer(10)
                    with patch.object(negative_process.fcntl, 'flock', acquire):
                        with tree_lock(root, timeout=2):
                            self.assert_budget(10, started)
                    self.assert_budget(10, started)
                finally:
                    attempted.set()
                    holder.join(6)
                self.assertFalse(holder.is_alive(), '#1142 releasing holder must finish')

    def test_lock_timeout_preserves_outer_alarm(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1142-timeout-') as root:
            with self.held_lock(root):
                started = self.arm_timer(10)
                with self.assertRaises(TimeoutError, msg='#1142 busy lock must time out'):
                    with tree_lock(root, timeout=0.1):
                        self.fail('#1142 busy lock must not be acquired')
                self.assert_budget(10, started)

    def test_outer_deadline_interrupts_wait_and_closes_fd(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1142-cancel-') as root:
            with self.held_lock(root):
                closed = []
                close = negative_process.os.close
                acquire = negative_process.TreeLock._acquire

                def close_fd(fd):
                    closed.append(fd)
                    close(fd)

                def arm_and_acquire(lock, fd):
                    # Enter __enter__'s protected acquisition window before
                    # arming: even a long scheduler pause must close this fd.
                    signal.setitimer(signal.ITIMER_REAL, 0.05)
                    return acquire(lock, fd)

                outcome = None
                with patch.object(negative_process.os, 'close', close_fd), \
                        patch.object(negative_process.TreeLock, '_acquire', arm_and_acquire):
                    try:
                        with tree_lock(root, timeout=0.5):
                            outcome = 'acquired'
                    except (SystemExit, TimeoutError) as error:
                        outcome = (type(error), str(error))
                self.assertTrue(outcome == (SystemExit, '#1142 caller deadline'),
                                '#1142 caller deadline must interrupt a busy lock')
                self.assertTrue(len(closed) == 1, '#1142 interrupted acquisition must close its fd')
                self.assertNotIn(negative_process._held_key(Path(root) / '.negative-source.lock'),
                                 negative_process._held_state()['held'],
                                 '#1142 cancelled acquisition must not own the lock')
            with tree_lock(root, timeout=0.1):
                pass

    def test_worker_wait_remains_bounded(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1142-worker-') as root:
            with self.held_lock(root):
                outcome = []

                def wait():
                    try:
                        with tree_lock(root, timeout=0.1):
                            outcome.append('acquired')
                    except BaseException as error:
                        outcome.append(type(error))

                worker = threading.Thread(target=wait)
                worker.start()
                worker.join(2)
                finished_while_held = not worker.is_alive()
            # Releasing the real lock also bounds cleanup of a broken waiter.
            worker.join(2)
            self.assertFalse(worker.is_alive(), '#1142 worker waiter must finish')
            self.assertTrue(finished_while_held and outcome == [TimeoutError],
                            '#1142 worker must respect lock deadline')
            self.assertIs(signal.getsignal(signal.SIGALRM), self.handler,
                          '#1142 worker must not change caller signal handler')


class NodeVersionProbe(unittest.TestCase):
    """#818 fix-r3 P1/P2: the probe is evidence, never a silent skip."""

    @staticmethod
    def result(returncode=0, stdout='v22.22.0\n', stderr=''):
        return subprocess.CompletedProcess(['node', '--version'], returncode,
                                           stdout, stderr)

    def test_only_the_first_stdout_line_counts(self):
        with patch.object(negative_process, 'run_owned',
                          return_value=self.result(stderr='(node:1) ExperimentalWarning\n')):
            self.assertEqual(node_version({}), ('v22.22.0', None),
                             'stderr warnings must not change a matching host')

    def test_empty_stdout_is_an_error(self):
        with patch.object(negative_process, 'run_owned',
                          return_value=self.result(stdout='')):
            version, error = node_version({})
            self.assertIsNone(version)
            self.assertIn('no version on stdout', error)

    def test_nonzero_exit_is_an_error_not_a_version(self):
        with patch.object(negative_process, 'run_owned',
                          return_value=self.result(returncode=9, stdout='',
                                                   stderr='node: bad option\n')):
            version, error = node_version({})
            self.assertIsNone(version)
            self.assertIn('exited 9', error)
            self.assertIn('bad option', error)

    def test_missing_node_is_an_error(self):
        with patch.object(negative_process, 'run_owned',
                          side_effect=OSError('node not found')):
            version, error = node_version({})
            self.assertIsNone(version)
            self.assertIn('node unavailable', error)

    def test_hanging_probe_is_an_error(self):
        with patch.object(negative_process, 'run_owned',
                          side_effect=subprocess.TimeoutExpired(['node'], 60)):
            version, error = node_version({})
            self.assertIsNone(version)
            self.assertIn('timed out', error)


class MutatingSources(unittest.TestCase):
    """The source mutation window must start from HEAD, never from residue."""

    @staticmethod
    def wait_path(path, timeout=15):
        deadline = time.monotonic() + timeout
        while not path.exists():
            if time.monotonic() >= deadline:
                raise AssertionError(f"timed out waiting for {path}")
            time.sleep(0.005)

    @staticmethod
    def repo(temp):
        root = Path(temp).resolve()
        root.mkdir(parents=True, exist_ok=True)
        subprocess.run(["git", "init", "-q"], cwd=root, check=True)
        subprocess.run(["git", "config", "user.email", "negative@example.invalid"],
                       cwd=root, check=True)
        subprocess.run(["git", "config", "user.name", "negative control"],
                       cwd=root, check=True)
        target = root / "source.rs"
        target.write_text("// head\n")
        subprocess.run(["git", "add", "source.rs"], cwd=root, check=True)
        subprocess.run(["git", "commit", "-qm", "fixture"], cwd=root, check=True)
        return root, target

    def test_residue_is_refused_and_left_for_its_owner(self):
        # A killed predecessor can leave non-HEAD bytes. The context must not
        # adopt them as its original, and must not overwrite them either.
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-residue-") as temp:
            root, target = self.repo(temp)
            target.write_text("// residue\n")
            before = target.read_bytes()
            with self.assertRaises(SourceBaselineError) as caught:
                with mutating_sources(root, [target]):
                    self.fail("residue must not enter the mutation window")
            message = str(caught.exception)
            self.assertIn("source.rs", message)
            self.assertIn("git checkout -- source.rs", message)
            self.assertEqual(target.read_bytes(), before, "refusal wrote to disk")
            target.write_text("// head\n")

    def test_other_product_source_residue_is_refused(self):
        # A runner names only its own files, but a killed runner from another
        # group can leave any workspace input mutated (full matrix in
        # workspace-pristine/test_controls.py). The global product check
        # must refuse before the new runner compiles a polluted tree.
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-other-") as temp:
            root, target = self.repo(temp)
            area = root / "src/pilotdeck-gateway/src"
            area.mkdir(parents=True)
            other = area / "other.rs"
            other.write_text("// head\n")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "product area"], cwd=root, check=True)
            other.write_text("// residue\n")
            with self.assertRaises(SourceBaselineError) as caught:
                with mutating_sources(root, [target]):
                    self.fail("unrelated product residue must abort the window")
            self.assertIn("other.rs", str(caught.exception))
            self.assertEqual(other.read_text(), "// residue\n")
            other.write_text("// head\n")

    def test_without_global_product_check_other_residue_is_not_seen(self):
        # Negative control for the global check above: patch it out and the
        # unrelated residue no longer aborts the window, so a runner would
        # compile a tree polluted by another group. The per-path check alone
        # cannot see it (target is clean).
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-other-no-") as temp:
            root, target = self.repo(temp)
            area = root / "src/pilotdeck-gateway/src"
            area.mkdir(parents=True)
            other = area / "other.rs"
            other.write_text("// head\n")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "product area"], cwd=root, check=True)
            other.write_text("// residue\n")
            with patch("negative_process._assert_product_pristine"), \
                    patch("negative_process._assert_workspace_sources"):
                with mutating_sources(root, [target]) as snapshot:
                    self.assertEqual(snapshot[target], b"// head\n")
                    self.assertEqual(other.read_text(), "// residue\n",
                                     "control should enter despite other residue")
            self.assertEqual(other.read_text(), "// residue\n")
            other.write_text("// head\n")

    def test_sigkill_residue_is_refused_by_the_next_instance(self):
        # The kernel releases flock on SIGKILL, so the next instance acquires
        # the lock. Baseline validation, not the lock, is what stops it from
        # snapshotting the dead runner's mutant as its "original".
        fixtures = str(Path(__file__).resolve().parent)
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-killed-") as temp:
            root, target = self.repo(Path(temp) / "repo")
            control = Path(temp) / "control"
            control.mkdir()
            corpse = subprocess.Popen(
                [sys.executable, "-c", SIGKILL_RESIDUE, fixtures, str(root),
                 str(target), str(control)],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                self.wait_path(control / "mutated")
                self.assertEqual(target.read_text(), "mutant-residue\n")
                corpse.kill()
                corpse.communicate(timeout=10)
                with self.assertRaises(SourceBaselineError):
                    with mutating_sources(root, [target]) as snapshot:
                        self.fail(f"accepted mutant as original: {snapshot}")
                self.assertEqual(target.read_text(), "mutant-residue\n",
                                 "the next instance overwrote the residue")
            finally:
                if corpse.poll() is None:
                    corpse.kill()
                corpse.communicate(timeout=10)
            target.write_text("// head\n")

    def test_without_baseline_check_the_same_residue_is_a_false_green(self):
        # Negative control for the check itself: dropping it makes the exact
        # scenario above snap the mutant and restore the mutant, i.e. a runner
        # would report success. Patch only the baseline reader (not the write
        # path) so the real context still performs snapshot and restore.
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-nobase-") as temp:
            root, target = self.repo(temp)
            target.write_text("mutant-residue\n")
            try:
                with patch("negative_process._head_bytes",
                           side_effect=lambda root, relative, baseline: (
                               Path(root) / relative).read_bytes()):
                    with mutating_sources(root, [target]) as snapshot:
                        self.assertEqual(snapshot[target], b"mutant-residue\n",
                                         "no-baseline control did not adopt residue")
                        target.write_text("second-mutant\n")
                self.assertEqual(target.read_text(), "mutant-residue\n",
                                 "no-baseline control did not report false success")
            finally:
                target.write_text("// head\n")
            self.assertEqual(target.read_text(), "// head\n")

    def test_normal_path_restores_head_bytes(self):
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-clean-") as temp:
            root, target = self.repo(temp)
            with mutating_sources(root, [target]) as snapshot:
                self.assertEqual(snapshot[target], b"// head\n")
                target.write_text("// mutant\n")
            self.assertEqual(target.read_bytes(), b"// head\n")

    def test_same_thread_nested_context_is_reentrant(self):
        # round6 mutates a runner and executes it in-process; the runner takes
        # the same lock again. A second flock on another fd would deadlock.
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-nested-") as temp:
            root, target = self.repo(temp)
            with mutating_sources(root, [target]):
                with mutating_sources(root, [target]) as inner:
                    self.assertEqual(inner[target], b"// head\n")
                target.write_text("// outer mutant\n")
            self.assertEqual(target.read_bytes(), b"// head\n")

    def test_published_module_copy_reenters_the_held_tree_lock(self):
        # #1166 r9: runner_control compiles an in-memory candidate
        # negative_process and publishes it as the sys.modules entry while
        # the original module already owns the tree lock. The copy must share
        # the reentrancy registry: with its own empty registry the copy's
        # tree_lock opens a second fd and flocks against its own process
        # until the lock deadline (cb8: round6 waited the full 900 s for
        # "source lock busy"). lock_timeout bounds this regression to seconds
        # when that regression is present.
        module_path = Path(negative_process.__file__)
        candidate = module_path.read_bytes().decode()
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-published-") as temp:
            root, target = self.repo(temp)
            with mutating_sources(root, [target]) as outer:
                self.assertTrue(outer[target] == b"// head\n",
                                "#1166 r9 outer window must snapshot HEAD bytes")
                with published_module("negative_process", module_path,
                                      candidate) as copy:
                    started = time.monotonic()
                    try:
                        with copy.mutating_sources(root, [target],
                                                  lock_timeout=15) as inner:
                            self.assertTrue(inner[target] == b"// head\n",
                                            "#1166 r9 copy must snapshot HEAD bytes")
                            target.write_text("// inner mutant\n")
                    except TimeoutError as error:
                        self.fail("#1166 r9 published copy must reenter the held "
                                  "tree lock instead of flocking against its own "
                                  "process: " + str(error))
                    self.assertLess(time.monotonic() - started, 10,
                                    "#1166 r9 published copy must reenter the held "
                                    "tree lock within seconds, not the lock deadline")
                self.assertTrue(target.read_bytes() == b"// head\n",
                                "#1166 r9 copy must restore its own mutation")


class ParityMutationLock(unittest.TestCase):
    """#166 real ruler lock, isolated paths and deterministic source IO hooks."""

    @contextmanager
    def fixture(self):
        with tempfile.TemporaryDirectory(prefix="pdrs-166-parity-") as temp:
            root = Path(temp).resolve()
            source = root / "scripts/gateway-parity/common.py"
            source.parent.mkdir(parents=True)
            original = (Path(__file__).resolve().parents[4]
                        / "scripts/gateway-parity/common.py").read_bytes()
            self.assertEqual(original.count(b"Path('/tmp/pdrs-parity.lock')"), 1,
                             "#166 fixture must bind the real parity lock path")
            lock = root / "parity.lock"
            source.write_bytes(original.replace(b"Path('/tmp/pdrs-parity.lock')",
                                               f"Path({str(lock)!r})".encode()))
            target = root / "scripts/gateway-parity/probe.py"
            target.write_bytes(b"# original\n")
            snapshot = {source: source.read_bytes(), target: target.read_bytes()}
            common = ModuleType("parity_probe")
            common.__file__ = str(source)
            exec(compile(snapshot[source], str(source), "exec"), common.__dict__)
            with patch.object(negative_process, "_assert_product_pristine"), \
                    patch.object(negative_process, "_assert_workspace_sources"), \
                    patch.object(negative_process, "_head_bytes",
                                 side_effect=lambda _root, path, _base: snapshot[root / path]):
                yield root, source, target, snapshot, common

    def contender(self, root, source):
        probe = root / "contender.py"
        probe.write_text(
            "from pathlib import Path\nfrom types import ModuleType\nimport sys\n"
            "m = ModuleType('contender'); m.__file__ = sys.argv[1]\n"
            "exec(compile(Path(sys.argv[1]).read_bytes(), sys.argv[1], 'exec'), m.__dict__)\n"
            "try:\n"
            "    with m.parity_lock(timeout=0.05): print('ACQUIRED')\n"
            "except TimeoutError:\n    print('REFUSED')\n")
        result = run_owned([sys.executable, "-I", "-B", str(probe), str(source)], timeout=5)
        self.assertEqual(result.returncode, 0, "#166 contender must finish without unrelated errors")
        return result.stdout.strip()

    def test_lock_held_through_restore_and_head_verification(self):
        for event in (None, RuntimeError, KeyboardInterrupt):
            with self.subTest(event=event), self.fixture() as (root, source, target, snapshot, common):
                raw_head = negative_process._head_bytes
                checks = []
                readbacks = 0

                def head(_root, path, baseline):
                    nonlocal readbacks
                    if root / path == target:
                        readbacks += 1
                        if readbacks == 2:
                            checks.append(self.contender(root, source))
                            self.assertEqual(target.read_bytes(), snapshot[target],
                                             "#166 verification must observe restored bytes")
                    return raw_head(_root, path, baseline)

                expected = self.assertRaises(event) if event else nullcontext()
                with patch.object(negative_process, "_head_bytes", side_effect=head), expected:
                    with mutating_sources(root, [source, target], lock_timeout=0.1):
                        self.assertEqual(self.contender(root, source), "REFUSED",
                                         "#166 second parity acquirer must be refused during mutation")
                        target.write_bytes(b"# mutant\n")
                        if event:
                            raise event("#166 controlled cancellation")
                self.assertEqual(checks, ["REFUSED"],
                                 "#166 parity lock must remain held through HEAD verification")
                self.assertEqual(self.contender(root, source), "ACQUIRED",
                                 "#166 verified restoration must release parity lock")

    def test_busy_lease_and_guard_refuse_before_any_source_write(self):
        for holder in ("lease", "guard"):
            with self.subTest(holder=holder), self.fixture() as (root, source, target, snapshot, common):
                context = (common.parity_lock(timeout=0.1) if holder == "lease"
                           else common.guard(common.LOCK))
                with context, patch.object(Path, "write_bytes") as write:
                    with self.assertRaises(TimeoutError, msg="#166 busy parity lock must time out"):
                        with mutating_sources(root, [source, target], lock_timeout=0.05):
                            self.fail("#166 busy parity lock must refuse before mutation")
                    self.assertEqual(write.call_count, 0, "#166 refused lock must not write source bytes")
                self.assertEqual(target.read_bytes(), snapshot[target], "#166 busy lock preserves original bytes")

    def test_failed_restore_retains_cleanup_pending_lease(self):
        with self.fixture() as (root, source, target, snapshot, common):
            raw_write = Path.write_bytes

            def fail_restore(path, data):
                if path == target and data == snapshot[target]:
                    raise OSError("#166 injected restore failure")
                return raw_write(path, data)

            try:
                with self.assertRaisesRegex(OSError, "#166 injected restore failure"), \
                        patch.object(Path, "write_bytes", fail_restore):
                    with mutating_sources(root, [source, target], lock_timeout=0.1):
                        target.write_bytes(b"# mutant\n")
                self.assertEqual(self.contender(root, source), "REFUSED",
                                 "#166 failed restoration must retain parity lock")
                self.assertEqual(common.owner(common.LOCK)["cleanupPending"],
                                 ["source-restore-unverified"], "#166 failed restoration must record reason")
            finally:
                target.write_bytes(snapshot[target])

    def test_without_parity_guard_the_bound_check_fails(self):
        # Execute the exact positive test with the acquisition bypassed. An
        # unrelated exception/timeout cannot satisfy this dedicated assertion.
        with patch.object(negative_process, "_parity_mutation_lock", return_value=nullcontext()):
            case = ParityMutationLock("test_lock_held_through_restore_and_head_verification")
            result = unittest.TestResult()
            case.run(result)
        self.assertEqual(result.errors, [], "#166 bypass control must fail only at bound assertions")
        self.assertTrue(result.failures, "#166 bypass control must kill the missing lock")
        self.assertTrue(all("#166 second parity acquirer must be refused during mutation" in failure
                            for _, failure in result.failures), "#166 bypass control must bind the lock assertion")


class OutputDirectoryGuard(unittest.TestCase):
    """#953: a reused evidence directory must never overwrite earlier logs."""

    def test_missing_and_empty_directories_are_accepted(self):
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-out-") as temp:
            missing = Path(temp) / "fresh"
            self.assertEqual(new_output_directory(missing), missing.resolve(),
                             "#953 a missing output directory must be created")
            self.assertTrue(missing.is_dir(),
                            "#953 the output directory must exist after preparation")
            empty = Path(temp) / "empty"
            empty.mkdir()
            self.assertEqual(new_output_directory(empty), empty.resolve(),
                             "#953 an existing empty output directory must be accepted")

    def test_nonempty_directory_is_refused(self):
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-out-") as temp:
            output = Path(temp) / "reused"
            output.mkdir()
            previous = output / "positive.log"
            previous.write_text("#953 earlier run evidence\n")
            with self.assertRaises(SystemExit,
                                   msg="#953 non-empty output directory must refuse reuse") as caught:
                new_output_directory(output)
            self.assertIn("#953 refusing to reuse a non-empty output directory",
                          str(caught.exception),
                          "#953 refusal must name the non-empty output directory")
            self.assertEqual(previous.read_text(), "#953 earlier run evidence\n",
                             "#953 refusal must leave the earlier evidence untouched")

    def test_file_at_output_path_is_refused(self):
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-out-") as temp:
            output = Path(temp) / "evidence"
            output.write_text("#953 not a directory\n")
            with self.assertRaises(SystemExit,
                                   msg="#953 output path that is a file must refuse") as caught:
                new_output_directory(output)
            self.assertIn("#953 output path exists and is not a directory",
                          str(caught.exception),
                          "#953 refusal must name the non-directory output path")
            self.assertEqual(output.read_text(), "#953 not a directory\n",
                             "#953 refusal must leave a non-directory output path untouched")

    def test_logs_are_created_exclusively(self):
        with tempfile.TemporaryDirectory(prefix="pdrs-negative-out-") as temp:
            log = Path(temp) / "positive.log"
            exclusive_write(log, "first run\n")
            with self.assertRaises(FileExistsError,
                                   msg="#953 existing log must not be overwritten"):
                exclusive_write(log, "second run\n")
            self.assertEqual(log.read_text(), "first run\n",
                             "#953 exclusive log creation must preserve the first run bytes")


class OutputDirectoryClaim(unittest.TestCase):
    """#1060: only the atomic claim admits a runner, even across pauses."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='pdrs-1060-')
        self.addCleanup(self.temp.cleanup)
        self.control = Path(self.temp.name)
        self.output = self.control / 'output'

    def child(self, name, phase='none'):
        probe = Path(__file__).resolve().parent / 'negative-output-directory/process_probe.py'
        process = subprocess.Popen(
            [sys.executable, '-I', '-B', '-X',
             'pycache_prefix=' + str(self.control / 'unused-pycache'),
             str(probe), str(self.output),
             str(self.control), name, phase],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

        def cleanup():
            if process.poll() is None:
                process.terminate()
            try:
                process.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.communicate(timeout=5)

        self.addCleanup(cleanup)
        return process

    def ready(self, process, name, phase):
        ready = self.control / (name + '.ready')
        deadline = time.monotonic() + 10
        while not ready.exists():
            self.assertIsNone(process.poll(), '#1060 child exited before pause')
            self.assertLess(time.monotonic(), deadline, '#1060 child did not reach pause')
            time.sleep(0.01)
        self.assertEqual(ready.read_text(), phase, '#1060 atomic ready signal identifies pause')

    def release(self, name):
        path = self.control / (name + '.release')
        temporary = path.with_suffix('.tmp')
        temporary.write_text('release')
        temporary.replace(path)

    def completed(self, process):
        try:
            stdout, stderr = process.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            self.fail('#1060 child did not exit within 10 seconds')
        return process.returncode, stdout + stderr

    def no_work(self, name):
        for kind in ('cargo', 'mutation'):
            self.assertFalse((self.control / (name + '.' + kind)).exists(),
                             '#1060 refused runner must not start ' + kind)

    def test_missing_and_empty_paths_have_persistent_metadata(self):
        for existing in (False, True):
            with self.subTest(existing=existing):
                output = self.control / str(existing)
                if existing:
                    output.mkdir()
                started = time.time()
                self.assertEqual(new_output_directory(output), output.resolve(),
                                 '#1060 successful claim returns resolved output')
                claim = output / '.claim'
                self.assertTrue(claim.is_file(), '#1060 admission requires a persistent claim')
                raw = claim.read_bytes()
                metadata = json.loads(raw)
                self.assertEqual(metadata['pid'], os.getpid(), '#1060 claim identifies owner pid')
                # List assertEqual adds multiline diffs before the marker.
                # Keep the bound marker on the AssertionError first line.
                self.assertTrue(metadata['argv'] == sys.argv, '#1060 claim records invocation')
                self.assertGreaterEqual(metadata['started_at'], started, '#1060 claim records start time')
                self.assertLessEqual(metadata['started_at'], time.time(), '#1060 claim time is not future')
                with self.assertRaises(SystemExit, msg='#1060 completed claim cannot be reused'):
                    new_output_directory(output)
                self.assertEqual(claim.read_bytes(), raw, '#1060 refusal preserves claim bytes')

    def mkdir_race(self, phase):
        if phase == 'iterdir':
            self.output.mkdir()
        first = self.child('a', phase)
        self.ready(first, 'a', phase)
        second = self.child('b')
        rc, log = self.completed(second)
        self.assertEqual(rc, 0, '#1060 unclaimed directory admits one winner: ' + log)
        self.release('a')
        rc, log = self.completed(first)
        self.assertNotEqual(rc, 0, '#1060 ' + phase + ' loser must be rejected before work')
        self.assertIn('#1060 output directory already claimed', log,
                      '#1060 loser must fail at the exclusive claim')
        self.no_work('a')
        for kind in ('cargo', 'mutation'):
            self.assertTrue((self.control / ('b.' + kind)).exists(),
                            '#1060 winner must reach the work sites')
        claim = json.loads((self.output / '.claim').read_text())
        self.assertEqual(claim['pid'], second.pid, '#1060 loser must not replace winner metadata')

    def test_mkdir_pause_has_exactly_one_winner(self):
        self.mkdir_race('mkdir')

    def test_empty_snapshot_has_exactly_one_winner(self):
        self.mkdir_race('iterdir')

    def test_incomplete_and_cancelled_claims_are_not_reused(self):
        first = self.child('a', 'open')
        self.ready(first, 'a', 'open')
        self.assertEqual((self.output / '.claim').read_bytes(), b'',
                         '#1060 hook must pause before claim content is written')
        for name in ('b', 'c'):
            if name == 'c':
                first.terminate()
                rc, _ = self.completed(first)
                self.assertNotEqual(rc, 0, '#1060 predecessor must be cancelled')
            contender = self.child(name)
            rc, log = self.completed(contender)
            self.assertNotEqual(rc, 0, '#1060 incomplete or cancelled claim must reject contender')
            self.assertIn('output directory', log, '#1060 refusal must identify evidence directory')
            self.no_work(name)
            self.assertEqual((self.output / '.claim').read_bytes(), b'',
                             '#1060 cancellation must retain the original claim')
        self.no_work('a')

    def test_claim_open_and_write_errors_prevent_admission(self):
        with patch.object(Path, 'open', side_effect=PermissionError('#1060 injected open denial')):
            with self.assertRaisesRegex(PermissionError, '#1060 injected open denial',
                                        msg='#1060 claim open errors must prevent admission'):
                new_output_directory(self.output)
        with patch.object(negative_process.json, 'dump',
                          side_effect=OSError('#1060 injected write failure')):
            with self.assertRaisesRegex(OSError, '#1060 injected write failure',
                                        msg='#1060 claim write errors must prevent admission'):
                new_output_directory(self.output)
        self.assertTrue((self.output / '.claim').is_file(),
                        '#1060 failed claim write must retain reservation')
        with self.assertRaises(SystemExit, msg='#1060 failed claim cannot be reused'):
            new_output_directory(self.output)


def output_directory_runner():
    path = Path(__file__).resolve().parent / 'negative-output-directory/negative.py'
    module = ModuleType('output_directory_negative')
    module.__file__ = str(path)
    exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
    return module


class OutputDirectoryRunnerTests(unittest.TestCase):
    """#1060 exercise real unittest output and the runner's failure evidence."""

    TEST = 'OutputDirectoryClaim.test_missing_and_empty_paths_have_persistent_metadata'
    MESSAGE = '#1060 claim records invocation'
    # Independent expectations captured with local CPython 3.14 / 3.9 probes;
    # do not derive these from the function being tested.
    DESCRIPTIONS = (
        'test_missing_and_empty_paths_have_persistent_metadata '
        '(__main__.OutputDirectoryClaim.test_missing_and_empty_paths_have_persistent_metadata)',
        'test_missing_and_empty_paths_have_persistent_metadata (__main__.OutputDirectoryClaim)',
    )

    def setUp(self):
        self.runner = output_directory_runner()

    @staticmethod
    def capture(case):
        stream = io.StringIO()
        result = unittest.TextTestRunner(stream=stream, verbosity=2).run(case)
        return subprocess.CompletedProcess([], int(not result.wasSuccessful()), '', stream.getvalue())

    def metadata_mutant_result(self):
        row = next(row for row in self.runner.cases if row[0] == 'missing-argv')
        _, path, old, new, test, assertion = row
        source = path.read_text()
        self.assertEqual(source.count(old), 1, '#1060 argv sample must use the unique runner anchor')
        mutant = ModuleType('claim_metadata_mutant')
        exec(compile(source.replace(old, new, 1), str(path), 'exec'), mutant.__dict__)
        # Execute the actual bound test, with only its helper replaced in memory.
        with patch.dict(globals(), {'new_output_directory': mutant.new_output_directory}), \
                patch.object(OutputDirectoryClaim, '__module__', '__main__'):
            result = self.capture(OutputDirectoryClaim(test.rsplit('.', 1)[-1]))
        return test, assertion, result

    def test_metadata_mutant_has_classifiable_first_line(self):
        test, assertion, result = self.metadata_mutant_result()
        failures = [line for line in result.stderr.splitlines() if line.startswith('AssertionError: ')]
        self.assertEqual(len(failures), 2, '#1060 argv sample must fail both real path subtests')
        self.assertTrue(all(line.endswith(assertion) for line in failures),
                        '#1060 bound marker must be on every AssertionError first line')
        self.assertEqual(self.runner.classify(result, test, assertion)[0], 'KILLED',
                         '#1060 real argv failure must classify as KILLED')
        for log in self.formatted_logs(result.stderr):
            formatted = subprocess.CompletedProcess([], 1, '', log)
            self.assertEqual(self.runner.classify(formatted, test, assertion)[0], 'KILLED',
                             '#1060 both observed failure descriptions must kill')

    def test_wrong_failure_identity_or_message_is_invalid(self):
        test, assertion, result = self.metadata_mutant_result()
        for log in self.formatted_logs(result.stderr):
            for old, new in [('OutputDirectoryClaim', 'OtherClass'),
                             (test.rsplit('.', 1)[-1], 'test_other'), ('__main__', 'another_module'),
                             ('AssertionError: False is not true : ' + assertion, 'AssertionError: unrelated'),
                             ('FAIL:', 'ERROR:')]:
                with self.subTest(replacement=new):
                    wrong = subprocess.CompletedProcess([], 1, assertion + '\n', log.replace(old, new))
                    self.assertEqual(self.runner.classify(wrong, test, assertion)[0], 'INVALID',
                                     '#1060 only the bound test assertion may kill')

    def formatted_logs(self, log):
        for description in self.DESCRIPTIONS:
            formatted = log
            for current in self.DESCRIPTIONS:
                formatted = formatted.replace(current, description)
            yield formatted

    def probe(self):
        def bound(case):
            for existing in (False, True):
                with case.subTest(existing=existing):
                    case.assertTrue(True, '#1060 format probe passes')
        cls, method = self.TEST.rsplit('.', 1)
        probe = type(cls, (unittest.TestCase,), {'__module__': '__main__', method: bound})
        return self.capture(probe(method))

    def test_real_and_legacy_subtest_pass_formats(self):
        result = self.probe()
        self.assertTrue(self.runner.passed(result, self.TEST), '#1060 real subtest success must pass')
        for log in self.formatted_logs(result.stderr):
            formatted = subprocess.CompletedProcess([], 0, '', log)
            self.assertTrue(self.runner.passed(formatted, self.TEST),
                            '#1060 both observed unittest descriptions must pass')

    def test_wrong_identity_skips_and_bad_exit_do_not_pass(self):
        result = self.probe()
        for old, new in [('OutputDirectoryClaim', 'OtherClass'),
                         ('test_missing_and_empty_paths_have_persistent_metadata', 'test_other'),
                         ('__main__', 'another_module'), ('Ran 1 test', 'Ran 2 tests'),
                         ('... ok', "... skipped 'reason'"), ('\nOK\n', '\nNOT OK\n')]:
            with self.subTest(replacement=new):
                wrong = subprocess.CompletedProcess([], 0, '', result.stderr.replace(old, new))
                self.assertFalse(self.runner.passed(wrong, self.TEST),
                                 '#1060 success requires the exact unskipped bound test')
        result.returncode = 1
        self.assertFalse(self.runner.passed(result, self.TEST), '#1060 success requires zero exit')

    def test_run_preserves_real_completed_output(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1060-runner-') as temp:
            out = Path(temp)
            result = self.runner.run(out, 'positive', self.TEST)
            self.assertTrue(self.runner.passed(result, self.TEST), '#1060 real runner positive must pass')
            self.assertTrue((out / 'positive.log').is_file(), '#1060 completed run must retain a log')
            self.assertEqual((out / 'positive.log').read_text(), result.stdout + result.stderr,
                             '#1060 completed output must be preserved')
            self.assertTrue((out / 'positive.exit').is_file(), '#1060 completed run must retain an exit record')
            self.assertEqual((out / 'positive.exit').read_text(), '0\n',
                             '#1060 completed exit must be preserved')

    def test_timeout_preserves_partial_output_and_invalid_exit(self):
        for stdout, stderr in [(b'partial stdout\n', b'partial stderr\n'),
                               ('partial stdout\n', 'partial stderr\n'), (None, None)]:
            with self.subTest(stdout=stdout), \
                    tempfile.TemporaryDirectory(prefix='pdrs-1060-timeout-') as temp:
                out = Path(temp)
                error = subprocess.TimeoutExpired(['probe'], 90, stdout, stderr)
                with patch.object(self.runner, 'run_owned', side_effect=error):
                    with self.assertRaises(subprocess.TimeoutExpired,
                                           msg='#1060 timeout must still propagate') as caught:
                        self.runner.run(out, 'timeout', self.TEST)
                self.assertIs(caught.exception, error, '#1060 original timeout must propagate')
                self.assertTrue((out / 'timeout.log').is_file(), '#1060 timeout must retain a log')
                raw = 'partial stdout\npartial stderr\n' if stdout else ''
                self.assertEqual((out / 'timeout.log').read_text(), raw + f'\nTimeoutExpired: {error}\n',
                                 '#1060 timeout must preserve both partial streams and diagnostic')
                self.assertTrue((out / 'timeout.exit').is_file(), '#1060 timeout must retain an exit record')
                self.assertEqual((out / 'timeout.exit').read_text(), 'INVALID: TimeoutExpired\n',
                                 '#1060 timeout must not invent a completed return code')

    def test_start_error_preserves_diagnostic_and_invalid_exit(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-1060-start-') as temp:
            out = Path(temp)
            error = OSError('#1060 injected spawn failure')
            with patch.object(self.runner, 'run_owned', side_effect=error):
                with self.assertRaises(OSError, msg='#1060 spawn error must still propagate') as caught:
                    self.runner.run(out, 'start', self.TEST)
            self.assertIs(caught.exception, error, '#1060 original spawn error must propagate')
            self.assertTrue((out / 'start.log').is_file(), '#1060 spawn error must retain a log')
            self.assertEqual((out / 'start.log').read_text(), f'\nOSError: {error}\n',
                             '#1060 spawn error must preserve its diagnostic')
            self.assertTrue((out / 'start.exit').is_file(), '#1060 spawn error must retain an exit record')
            self.assertEqual((out / 'start.exit').read_text(), 'INVALID: OSError\n',
                             '#1060 spawn error must not invent a completed return code')


if __name__ == "__main__":
    unittest.main(verbosity=2)

"""Run directly with Python; no Cargo, network, baseline or fixtures needed."""
import errno
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

from negative_process import run_owned, wait_for_slot


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


if __name__ == "__main__":
    unittest.main(verbosity=2)

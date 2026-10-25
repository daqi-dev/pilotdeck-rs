"""Run directly with Python; no Cargo, network or product fixtures needed.

The source-window cases create a throwaway git repository under the system
temporary directory, so they exercise the real baseline check without touching
this worktree.
"""
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

from negative_process import (SourceBaselineError, mutating_sources, run_owned,
                              tree_lock, wait_for_slot)


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
    (control / "mutated").write_text(original)
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
        # group can leave any gateway source mutated. The global product check
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
            with patch("negative_process._assert_product_pristine"):
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


if __name__ == "__main__":
    unittest.main(verbosity=2)

# anchor-selfcheck: unsupported unit tests of the shared process/source guard
"""Run directly with Python; no network or product fixtures needed; the shared guard may bootstrap its Rust scanner offline.

The source-window cases create a throwaway git repository under the system
temporary directory, so they exercise the real baseline check without touching
this worktree.
"""
import errno
import io
import json
import re
from contextlib import contextmanager, nullcontext
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
import tokenize
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
            # 69 (EX_UNAVAILABLE) = no unified gate on this machine (CI has
            # none): there is no shared quota to wait for, so wait_for_slot
            # proceeds instead of failing. Every other unexpected status --
            # including 2 (a broken cargo-slots.sh: bash exits 2 on syntax
            # errors) and 4 (gate present but unusable) -- stays a hard error.
            script.write_text("exit 69\n")
            self.assertEqual(wait_for_slot(script, timeout=2).returncode, 0)
            # gate_required=True: the caller already proved a gate exists, so
            # 69 can only mean it vanished mid-wait → hard error instead of
            # running cargo with the per-host quota skipped (#589 r2 P1).
            with self.assertRaises(subprocess.CalledProcessError) as caught:
                wait_for_slot(script, timeout=2, gate_required=True)
            self.assertEqual(caught.exception.returncode, 69)
            for broken in ("exit 2\n", "exit 3\n", "exit 4\n", "exit 64\n"):
                script.write_text(broken)
                with self.assertRaises(subprocess.CalledProcessError):
                    wait_for_slot(script, timeout=2)


    def sample_caps(self, wait):
        """Drive one hung gate and return the per-sample timeouts handed to run_owned."""
        caps = []

        def hung(_command, **kwargs):
            caps.append(kwargs.get('timeout'))
            time.sleep(0.002)
            raise subprocess.TimeoutExpired(_command, kwargs.get('timeout'))

        with patch.dict(wait.__globals__, {'run_owned': hung}):
            with self.assertRaises(subprocess.TimeoutExpired):
                wait('gate.sh', timeout=1.0, interval=0.01, sample_timeout=0.05)
        self.assertTrue(caps, '#1692 the gate was never sampled')
        return caps

    def assert_sample_bounds(self, wait):
        # #1692 §2: one hung sample must not eat the caller's whole budget, but
        # the gate's documented cold bootstrap still gets one longer sample.
        caps = self.sample_caps(wait)
        self.assertLessEqual(caps[0], 0.05,
                             '#1692 one sample is bounded by sample_timeout, not the total budget')
        long_caps = [cap for cap in caps if cap > 0.05]
        self.assertEqual(len(long_caps), 1,
                         '#1692 exactly one cold-bootstrap sample: a hung gate must not keep '
                         'getting 300 s samples for the rest of the budget')
        self.assertLessEqual(max(caps), 1.0,
                             '#1692 no sample may exceed the total wait budget')

    def test_wait_for_slot_bounds_each_sample(self):
        self.assert_sample_bounds(wait_for_slot)

        def mutant(old, new):
            source = Path(negative_process.__file__).read_text()
            self.assertEqual(source.count(old), 1, f'#1692 unique anchor: {old!r}')
            module = ModuleType('negative_process_mutant')
            module.__file__ = negative_process.__file__
            exec(compile(source.replace(old, new, 1), negative_process.__file__, 'exec'),
                 module.__dict__)
            return module.wait_for_slot

        # 删掉单次采样上界：第一个样本直接拿总预算 → 断言红。
        broad = mutant(
            'cap = min(remaining, CARGO_SLOTS_CALL_SECONDS if use_long else sample_timeout)',
            'cap = remaining')
        with self.assertRaises(AssertionError, msg='#1692 deleting the sample bound must be caught'):
            self.assert_sample_bounds(broad)
        print('#1692 wait_for_slot mutant sample-bound-removed: KILLED', flush=True)
        # 删掉冷启动逃生口：首样本超时后仍只给 sample_timeout，冷闸永远过不去
        # （#1556 回归）→ 断言红。
        no_bootstrap = mutant(
            'CARGO_SLOTS_CALL_SECONDS if use_long else sample_timeout',
            'sample_timeout')
        with self.assertRaises(AssertionError, msg='#1692 dropping the cold bootstrap sample must be caught'):
            self.assert_sample_bounds(no_bootstrap)
        print('#1692 wait_for_slot mutant cold-bootstrap-removed: KILLED', flush=True)
        # 冷启动长样本只给一次：去掉「只一次」闸，长样本超时后仍反复拿 300 s，
        # 挂着不返回的闸门会一直躲在长样本里 → 断言红。
        repeat_long = mutant(
            'use_long = cold_bootstrap and long_sample_available',
            'use_long = cold_bootstrap')
        with self.assertRaises(AssertionError,
                               msg='#1692 repeating the long cold-bootstrap sample must be caught'):
            self.assert_sample_bounds(repeat_long)
        print('#1692 wait_for_slot mutant long-sample-repeated: KILLED', flush=True)

    def test_wait_for_slot_rejects_bad_sample_timeout(self):
        # A non-positive/infinite cap would recreate the unbounded sample (§2).
        for value in (0, -1, float('inf'), float('nan')):
            with self.subTest(sample_timeout=value), \
                    self.assertRaises(ValueError):
                wait_for_slot('gate.sh', timeout=1, sample_timeout=value)

    def test_cargo_slots_gate_resolution(self):
        # #1556 r2 P1：把「闸路径解析不出来」当成「本机没有闸」会让名额检查静默消失。
        # HOME 未设 = 无法定位 → 必须 fail-closed 4，不能是 69（放行）。
        script = Path(__file__).resolve().parents[4] / "scripts" / "cargo-slots.sh"
        env = {k: v for k, v in os.environ.items() if k != "PDRS_CARGO_SLOT_BIN"}
        env.pop("HOME", None)
        result = subprocess.run(["bash", str(script)], capture_output=True,
                                text=True, timeout=60, env=env)
        self.assertEqual(result.returncode, 4, result.stdout + result.stderr)
        # 显式 PDRS_CARGO_SLOT_BIN 指向不存在的闸 = 调用方声明「本机不用共享名额」→ 69。
        env = dict(os.environ, PDRS_CARGO_SLOT_BIN=str(
            Path(tempfile.gettempdir()) / "no-such-gate-1556"))
        env.pop("HOME", None)
        result = subprocess.run(["bash", str(script)], capture_output=True,
                                text=True, timeout=60, env=env)
        self.assertEqual(result.returncode, 69, result.stdout + result.stderr)
        # HOME 设了但不是目录（服务账号 / 拼错）= 同样无法定位，只对「未设」fail-closed
        # 会把这条静默成 69（#1556 r3 boundary P2）。
        env = {k: v for k, v in os.environ.items() if k != "PDRS_CARGO_SLOT_BIN"}
        env["HOME"] = str(Path(tempfile.gettempdir()) / "no-such-home-1556")
        result = subprocess.run(["bash", str(script)], capture_output=True,
                                text=True, timeout=60, env=env)
        self.assertEqual(result.returncode, 4, result.stdout + result.stderr)

    def test_cargo_slots_runner_tools_fallback(self):
        # #589 收尾：远端 runner 宿主没有 ~/ws/pdrs-ops，同一个实现装在
        # ~/ws/pdrs-runner-tools/bin/cargo。回退必须真的被用上——找不到闸时这里
        # 是 4/69（fail-closed 或「无共享名额，继续」），都会让名额被跳过。
        # 桩闸报 999/999（满）→ rc 1 是「确实用上了这个闸」的可判定证据。
        script = Path(__file__).resolve().parents[4] / "scripts" / "cargo-slots.sh"
        report = "cargo-slot x host=h 名额 999/999  磁盘 1.0 GiB / 190（cache）"
        with tempfile.TemporaryDirectory(prefix="pdrs-cargo-slots-runner-") as temp:
            temp = Path(temp)
            home = temp / "home"
            gate = home / "ws/pdrs-runner-tools/bin/cargo"
            gate.parent.mkdir(parents=True)
            gate.write_text(f'#!/bin/bash\necho "{report}"\n')
            gate.chmod(0o755)
            env = {k: v for k, v in os.environ.items() if k != "PDRS_CARGO_SLOT_BIN"}
            env.update(HOME=str(home),
                       PDRS_CARGO_SLOTS_CACHE_DIR=str(temp / "cache"),
                       # 真机上的 /private/tmp/pdrs-cargo-gate 本身就是「共享池迹象」，
                       # 会让下面两步无论有没有新判定都退 4；指到不存在的路径才能
                       # 让「runner-tools 目录在」「双装」成为唯一的 4 的理由。
                       PDRS_CARGO_SLOTS_POOL_DIR=str(temp / "no-such-pool"))
            result = subprocess.run(["bash", str(script)], capture_output=True,
                                    text=True, timeout=120, env=env)
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertNotIn("没有统一闸", result.stderr, result.stdout + result.stderr)
            # 装了但坏了：runner-tools 目录在、闸不可执行 → fail-closed 4，不是 69。
            gate.unlink()
            result = subprocess.run(["bash", str(script)], capture_output=True,
                                    text=True, timeout=60, env=env)
            self.assertEqual(result.returncode, 4, result.stdout + result.stderr)
            self.assertIn("pdrs-runner-tools", result.stderr)
            # 双装宿主：ops 目录在但闸坏了，runner 副本完好 → 仍是 4，不许静默换闸
            # （两份 cargo_gate.py 的 SLOT_DEFAULTS 可各自漂移，用旧 cap 放行）。
            (home / "ws/pdrs-ops").mkdir(parents=True)
            gate.write_text(f'#!/bin/bash\necho "{report}"\n')
            gate.chmod(0o755)
            result = subprocess.run(["bash", str(script)], capture_output=True,
                                    text=True, timeout=60, env=env)
            self.assertEqual(result.returncode, 4, result.stdout + result.stderr)

    def test_cargo_slots_judges_by_gate_used(self):
        # #1556 r3 P1：只看本机 ps 数出的 cargo 会假放行——shim 先取槽位、后做冷 du，那几十秒
        # ps 只看到 python。闸报 999/999 时必须判满，哪怕本机一个 cargo 都没有；闸报 0/999
        # 时也不能因为 ps 而误判满。
        script = Path(__file__).resolve().parents[4] / "scripts" / "cargo-slots.sh"
        report = "cargo-slot x host=h 名额 {}  磁盘 1.0 GiB / 190（cache）"
        with tempfile.TemporaryDirectory(prefix="pdrs-cargo-slots-used-") as temp:
            temp = Path(temp)
            for name, slots, expected in (("full", "999/999", 1), ("free", "0/999", 0)):
                gate = temp / f"gate-{name}.sh"
                gate.write_text(f'#!/bin/bash\necho "{report.format(slots)}"\n')
                gate.chmod(0o755)
                env = dict(os.environ,
                           PDRS_CARGO_SLOT_BIN=str(gate),
                           PDRS_CARGO_SLOTS_CACHE_DIR=str(temp / f"cache-{name}"))
                result = subprocess.run(["bash", str(script)], capture_output=True,
                                        text=True, timeout=120, env=env)
                self.assertEqual(result.returncode, expected,
                                 f"{slots}: " + result.stdout + result.stderr)

    def test_cargo_slots_used_only_when_fresh(self):
        # #1556 r4（两家评审）：`used` 是快变量，缓存里那份是写缓存那一刻的快照——旧的 used=0 会假放行，
        # 旧的 used=cap 会让整段 TTL 恒判「满」。所以只有最近 USED_TTL 内真的从闸拿到的 used 才参与判定。
        # 这里预置一份「闸报 999/999」的缓存：lastok 很旧 → 只按 ps 判（本机无 cargo → 0）；
        # lastok 是现在 → 判满（1）。两条都走新鲜缓存路径，不碰闸，所以结果确定。
        script = Path(__file__).resolve().parents[4] / "scripts" / "cargo-slots.sh"
        now = int(time.time())
        with tempfile.TemporaryDirectory(prefix="pdrs-cargo-slots-used-ttl-") as temp:
            temp = Path(temp)
            gate = temp / "gate.sh"
            gate.write_text("#!/bin/bash\necho 'cargo-slot x host=h 名额 0/999  磁盘 1.0 GiB / 190（cache）'\n")
            gate.chmod(0o755)
            for name, lastok, expected in (("stale", now - 600, 0), ("live", now, 1)):
                cache = temp / f"cache-{name}"
                env = dict(os.environ,
                           PDRS_CARGO_SLOT_BIN=str(gate),
                           PDRS_CARGO_SLOTS_CACHE_DIR=str(cache))
                subprocess.run(["bash", str(script)], capture_output=True, text=True,
                               timeout=60, env=env, check=False)
                files = list(cache.glob("cap-cache.*"))
                self.assertEqual(len(files), 1, f"{name}: 第一次调用应写出缓存：{files}")
                files[0].write_text(f"999 999 {now} {lastok}\n")
                result = subprocess.run(["bash", str(script)], capture_output=True,
                                        text=True, timeout=60, env=env)
                self.assertEqual(result.returncode, expected,
                                 f"{name}: " + result.stdout + result.stderr)
                self.assertIn("闸内", result.stdout)

    def test_cargo_slots_slow_gate_sample_is_not_trusted(self):
        # #1556 r6 concurrency P1：闸的 --status 是「先探 slot 算 used → 再做磁盘 du（冷缓存
        # 31.8–42.2 s）→ 最后才打印」，所以 used 的测量时刻最早是这次调用的起点。时间戳必须取
        # 问闸**之前**的时刻——用答复时刻会把整段 du 抹掉，让陈旧的 used 冒充「0s 前取自闸」，
        # 而 du 期间正是「shim 刚占走最后一个槽、ps 还看不到 cargo」的假放行窗口。
        # 两格：闸秒答 → used 新鲜、参与判定（判满 1）；闸睡 4 s 而 USED_TTL=2 → used 年龄 ≥4 s，
        # 当未知、只按 ps 判（本机无 cargo → 0），输出里写明「不计入判定」。
        script = Path(__file__).resolve().parents[4] / "scripts" / "cargo-slots.sh"
        with tempfile.TemporaryDirectory(prefix="pdrs-cargo-slots-stamp-") as temp:
            temp = Path(temp)
            gate = temp / "gate.sh"
            cache = temp / "cache"
            env = dict(os.environ,
                       PDRS_CARGO_SLOT_BIN=str(gate),
                       PDRS_CARGO_SLOTS_CACHE_DIR=str(cache),
                       PDRS_CARGO_SLOTS_REFRESH_TIMEOUT="20",
                       PDRS_CARGO_SLOTS_USED_TTL="2")

            def seed_and_run(body):
                gate.write_text("#!/bin/bash\n" + body +
                                "echo 'cargo-slot x host=h 名额 999/999  磁盘 1.0 GiB / 190（cache）'\n")
                gate.chmod(0o755)
                # 先跑一次让脚本写出缓存（键是 cksum，不猜文件名），再把它改成「已过期」，
                # 强制走限时刷新分支。
                subprocess.run(["bash", str(script)], capture_output=True, text=True,
                               timeout=60, env=env, check=False)
                files = list(cache.glob("cap-cache.*"))
                self.assertEqual(len(files), 1, f"第一次调用应写出缓存：{files}")
                now = int(time.time())
                files[0].write_text(f"999 0 {now - 600} {now - 600}\n")
                return subprocess.run(["bash", str(script)], capture_output=True,
                                      text=True, timeout=120, env=env)

            fast = seed_and_run("")
            self.assertEqual(fast.returncode, 1, fast.stdout + fast.stderr)
            self.assertIn("前取自闸", fast.stdout)

            slow = seed_and_run("sleep 4\n")
            self.assertEqual(slow.returncode, 0, slow.stdout + slow.stderr)
            self.assertIn("不计入判定", slow.stdout)

    def test_cargo_slots_bootstrap_and_refresh_stamp_sample_time(self):
        # #1556 r7（两家评审同点）：三条写缓存路径里只有「缓存过期→问闸」按口径取问闸之前的
        # 时刻；冷启动与 --refresh-cap 仍用答复之后的 date +%s。冷启动正是 du 最冷、耗时最长
        # 的一次，这样打戳会让一份最长已 240 s 旧的 used 冒充「0s 前取自闸」，并在随后
        # USED_TTL 内被缓存读者继续当真。两格都断言：慢闸下 used 不参与判定，且 lastok 记的是
        # 问闸**之前**的时刻。
        script = Path(__file__).resolve().parents[4] / "scripts" / "cargo-slots.sh"
        with tempfile.TemporaryDirectory(prefix="pdrs-cargo-slots-boot-") as temp:
            temp = Path(temp)
            gate = temp / "gate.sh"
            gate.write_text("#!/bin/bash\nsleep 4\n"
                            "echo 'cargo-slot x host=h 名额 999/999  磁盘 1.0 GiB / 190（cache）'\n")
            gate.chmod(0o755)
            env = dict(os.environ,
                       PDRS_CARGO_SLOT_BIN=str(gate),
                       PDRS_CARGO_SLOTS_CACHE_DIR=str(temp / "cache"),
                       PDRS_CARGO_SLOTS_USED_TTL="2")

            before = int(time.time())
            boot = subprocess.run(["bash", str(script)], capture_output=True,
                                  text=True, timeout=120, env=env)
            self.assertEqual(boot.returncode, 0, boot.stdout + boot.stderr)
            self.assertIn("不计入判定", boot.stdout)
            files = list((temp / "cache").glob("cap-cache.*"))
            self.assertEqual(len(files), 1, files)
            lastok = int(files[0].read_text().split()[3])
            self.assertLessEqual(lastok, before + 1, files[0].read_text())

            # --refresh-cap 是人工恢复路径，写出的 lastok 同样不能是答复时刻。
            before = int(time.time())
            refresh = subprocess.run(["bash", str(script), "--refresh-cap"], capture_output=True,
                                     text=True, timeout=120, env=env)
            self.assertEqual(refresh.returncode, 0, refresh.stdout + refresh.stderr)
            lastok = int(files[0].read_text().split()[3])
            self.assertLessEqual(lastok, before + 1, files[0].read_text())

    def test_cargo_slots_rejects_bad_time_knobs(self):
        # 0 / 前导零 / 非数字都必须 fail-closed 4：0 让 MAX_STALE、FAT_REFRESH_AFTER 变成恒真，
        # 前导零在 $(( )) 里按八进制解析（09 非法 → 算术错误退 1 = 「已满」），非数字在 set -u
        # 下退 1（#1556 r3 P2）。校验先于闸定位，所以这里连 PDRS_CARGO_SLOT_BIN 都不用给。
        script = Path(__file__).resolve().parents[4] / "scripts" / "cargo-slots.sh"
        for knob, value in (("PDRS_CARGO_SLOTS_MAX_STALE", "0"),
                            ("PDRS_CARGO_SLOTS_CAP_TTL", "08"),
                            ("PDRS_CARGO_SLOTS_REFRESH_CAP_TIMEOUT", "abc")):
            env = dict(os.environ, **{knob: value})
            env.pop("PDRS_CARGO_SLOT_BIN", None)
            result = subprocess.run(["bash", str(script)], capture_output=True,
                                    text=True, timeout=60, env=env)
            self.assertEqual(result.returncode, 4,
                             f"{knob}={value}: " + result.stdout + result.stderr)

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


class ParityGuardedPredicate(unittest.TestCase):
    """#2513 fix-r2→fix-r3 (DS sibling#1 + MM boundary#1): one shared spelling.

    ``parity_guarded`` decides whose mutation window holds
    /tmp/pdrs-parity.lock. Six families take that lease through their own
    copies of the protocol; the fix-r1 inline predicate covered only two,
    fix-r2 widened it to four, and fix-r3 added native-witness (collect.py)
    and the pilotdeck-cli Rust tests (the sdk ParityLock copies), whose
    windows previously mutated real lease holders without holding the lease.
    The consumers (mutating_sources' parity branch, parity-lock-release's
    shared copy, cli-parity-mock-gateway's inner-lock stand-down and
    cli-parity-lock-bounded's --check) all read this one definition, so the
    family set is pinned here: a seventh family must be a deliberate change,
    not a missed one — and ParityLockHolderReconciliation below reds on any
    lock reference outside the family and the registered-mentions table.
    """

    def test_the_family_set_is_pinned(self):
        self.assertEqual(
            negative_process.PARITY_GUARDED_PREFIXES,
            ("scripts/gateway-parity/", "scripts/parity/",
             "scripts/cli-parity/", "scripts/ui-server-parity/",
             "scripts/native-witness/",
             "src/pilotdeck-cli/tests/"))

    def test_all_six_lock_families_are_guarded(self):
        for relative in ("scripts/gateway-parity/common.py",
                         "scripts/gateway-parity/run_round.py",
                         "scripts/parity/round.py",
                         "scripts/parity/controls/lock_release.py",
                         "scripts/cli-parity/common.py",
                         "scripts/cli-parity/run_round.py",
                         "scripts/ui-server-parity/calibrate.py",
                         "scripts/ui-server-parity/calibrate.sh",
                         "scripts/native-witness/collect.py",
                         "src/pilotdeck-cli/tests/sdk_run_exit.rs",
                         "src/pilotdeck-cli/tests/sdk_settings_exit.rs",
                         "src/pilotdeck-cli/tests/sdk_sessions_list_exit.rs",
                         "src/pilotdeck-cli/tests/sdk_resume_exit.rs",
                         "src/pilotdeck-cli/tests/sdk_format_exit.rs",
                         "src/pilotdeck-cli/tests/sdk_sessions_messages_exit.rs"):
            self.assertTrue(negative_process.parity_guarded(relative),
                            f'{relative} takes /tmp/pdrs-parity.lock '
                            'and must be in the lease family')

    def test_lookalikes_outside_the_family_are_not_guarded(self):
        for relative in ("", "scripts/parity", "scripts/round.py",
                         "scripts/gateway-parityx/common.py",
                         "scripts/native-witnessx/collect.py",
                         "src/pilotdeck-cli/src/sdk.rs",
                         "src/pilotdeck-cli/test/x.rs",
                         "src/pilotdeck-gateway/tests/fixtures/negative_process.py",
                         "specs/parity/README.md"):
            self.assertFalse(negative_process.parity_guarded(relative),
                             f'{relative} must not take the shared lease')


class BaselineTracksRuler(unittest.TestCase):
    """#2513 fix-r4 (review 条3): ruler absence must be resolvable, not assumed.

    ``_baseline_tracks_ruler`` used to fold every ``SourceBaselineError`` —
    git timeout, sabotaged environment, unreadable blob — into "no lease
    consumer" and silently drop the shared lease for windows mutating guarded
    sources. Absence is now proved by a resolvable ``git ls-tree``; any git
    failure re-raises (fail-closed); a ``--baseline`` predating the lease
    protocol on a ruler-tracking checkout is refused without the explicit
    ``allow_unleased`` opt-in.
    """

    RULER = "scripts/gateway-parity/common.py"
    GUARDED = "scripts/cli-parity/probe.py"

    @staticmethod
    def repo(temp, ruler):
        """Two commits: pre-lease A, then HEAD; the ruler only joins at HEAD."""
        root = Path(temp).resolve()
        root.mkdir(parents=True, exist_ok=True)

        def git(*args):
            subprocess.run(["git", *args], cwd=root, check=True)

        git("init", "-q")
        git("config", "user.email", "negative@example.invalid")
        git("config", "user.name", "negative control")
        probe = root / BaselineTracksRuler.GUARDED
        probe.parent.mkdir(parents=True, exist_ok=True)
        probe.write_text("# original\n")
        git("add", ".")
        git("commit", "-qm", "pre-lease baseline")
        before = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root,
                                 check=True, capture_output=True,
                                 text=True).stdout.strip()
        if ruler:
            ruler_path = root / BaselineTracksRuler.RULER
            ruler_path.parent.mkdir(parents=True, exist_ok=True)
            ruler_path.write_text("# ruler stub\n")
            git("add", ".")
            git("commit", "-qm", "ruler arrives")
        return root, probe, before

    def test_resolvable_absence_is_the_only_exemption(self):
        # The synthetic-mirror case: a tree whose baseline genuinely has no
        # ruler cannot host a live lease consumer, so no lease. The answer is
        # a resolvable ls-tree away from HEAD, never an assumed one.
        with tempfile.TemporaryDirectory(prefix="pdrs-2513-ruler-") as temp:
            root, _probe, _before = self.repo(temp, ruler=False)
            self.assertFalse(negative_process._baseline_tracks_ruler(root, "HEAD"),
                             "#2513 a resolvable ruler absence is the only exemption")
            self.assertFalse(negative_process._baseline_has_file(root, self.RULER, "HEAD"),
                             "#2513 a resolvable absence reads as absence")
            # A fresh subdirectory: re-running repo() in the same tree would
            # leave nothing for the "ruler arrives" commit to record.
            root, _probe, _before = self.repo(str(Path(temp) / "with-ruler"), ruler=True)
            self.assertTrue(negative_process._baseline_tracks_ruler(root, "HEAD"),
                             "#2513 a tracked ruler reads as tracked")

    def test_pre_lease_baseline_requires_explicit_opt_in(self):
        with tempfile.TemporaryDirectory(prefix="pdrs-2513-ruler-") as temp:
            root, _probe, before = self.repo(temp, ruler=True)
            with self.assertRaises(SourceBaselineError,
                                   msg="#2513 a pre-lease baseline must not silently drop the lease") as caught:
                negative_process._baseline_tracks_ruler(root, before)
            self.assertIn("allow-unleased", str(caught.exception),
                          "#2513 the refusal names the opt-in switch")
            self.assertFalse(negative_process._baseline_tracks_ruler(root, before, allow_unleased=True),
                             "#2513 the explicit opt-in keeps the mirror exemption")

    def test_git_failure_fails_closed(self):
        with tempfile.TemporaryDirectory(prefix="pdrs-2513-ruler-") as temp:
            root, _probe, _before = self.repo(temp, ruler=True)
            # Fake GIT_DIR: git itself refuses to answer; the old code folded
            # this into the exemption and mutated guarded sources unleased.
            with patch.dict(os.environ, {"GIT_DIR": str(root / "nowhere.git")}):
                with self.assertRaises(SourceBaselineError,
                                       msg="#2513 a git failure must not read as ruler absence"):
                    negative_process._baseline_tracks_ruler(root, "HEAD")
            # PATH sabotage: git cannot even be launched — still a refusal,
            # never an exemption.
            with patch.dict(os.environ, {"PATH": ""}):
                with self.assertRaises(OSError,
                                       msg="#2513 a missing git must not read as ruler absence"):
                    negative_process._baseline_tracks_ruler(root, "HEAD")

    def test_tracked_but_unreadable_blob_fails_closed(self):
        with tempfile.TemporaryDirectory(prefix="pdrs-2513-ruler-") as temp:
            root, _probe, _before = self.repo(temp, ruler=True)
            denial = SourceBaselineError(root, self.RULER,
                                         "cannot read HEAD bytes: simulated")
            with patch.object(negative_process, "_head_bytes", side_effect=denial), \
                    patch.object(negative_process, "_git",
                                 side_effect=lambda *_args: (self.RULER + "\n").encode()):
                with self.assertRaises(SourceBaselineError,
                                       msg="#2513 a tracked ruler that cannot be read must not read as absent") as caught:
                    negative_process._baseline_tracks_ruler(root, "HEAD")
                self.assertIs(caught.exception, denial,
                              "#2513 the original read failure is re-raised, not reinterpreted")

    def test_window_refuses_pre_lease_baseline_and_opt_in_runs_unleased(self):
        with tempfile.TemporaryDirectory(prefix="pdrs-2513-ruler-") as temp:
            root, probe, before = self.repo(temp, ruler=True)
            original = probe.read_bytes()
            with patch.object(negative_process, "_assert_product_pristine"):
                with self.assertRaises(SourceBaselineError,
                                       msg="#2513 guarded mutation on a pre-lease baseline must refuse to start"):
                    with mutating_sources(root, [probe], baseline=before, lock_timeout=1):
                        self.fail("#2513 a refused window must not yield")
                self.assertEqual(probe.read_bytes(), original,
                                 "#2513 the refused window writes nothing")
                with mutating_sources(root, [probe], baseline=before, lock_timeout=1,
                                      allow_unleased=True):
                    self.assertEqual(probe.read_bytes(), original,
                                     "#2513 the opted-in window starts from pristine bytes")
                    probe.write_text("# mutant\n")
                self.assertEqual(probe.read_bytes(), original,
                                 "#2513 the opted-in window still restores bytes")


class ParityLockHolderReconciliation(unittest.TestCase):
    """#2513 fix-r3 (DS sibling#1 + MM boundary#1): machine-checked holder list.

    The four-prefix predicate of fix-r2 still missed scripts/native-witness
    (collect.py) and src/pilotdeck-cli/tests (the Rust ParityLock copies),
    so their mutation windows never held /tmp/pdrs-parity.lock. Widening the
    predicate alone would leave the next family one review away, so this
    check re-derives the holder inventory from the tree itself: every code
    file that names the lease path or calls parity_lock must live under a
    guarded prefix or carry a registered reason here. A seventh family, or a
    stray copy of the protocol, fails closed instead of shipping silently.
    """

    # The one lease path and the one protocol name every reference uses.
    # #2513 fix-r4 (review boundary#3): matching the bare filename, not only
    # the full path, catches the concatenated '/tmp/' + 'pdrs-parity.lock'
    # spelling, and PEP 701 f-strings tokenize their literal parts as
    # FSTRING_MIDDLE — a bare-STRING scan would skip an f-string spelling.
    LOCK_PATH = '/tmp/pdrs-parity.lock'
    LOCK_FILE = 'pdrs-parity.lock'
    LOCK_NAME = re.compile(r'\bparity_lock\b')
    # Code files only; .md/.txt are documentation, and the raw grep over the
    # whole tree (RESULT, fix-r3) shows every non-code mention is a doc.
    SUFFIXES = frozenset({'.py', '.rs', '.mjs', '.js', '.sh', '.bash'})
    SKIP_DIRS = frozenset({'.git', '_build', '_tmp', '__pycache__',
                           'node_modules', 'target', '.venv'})

    # Out-of-family files that reference the lease, each with the reason it
    # is safe: temp-lock redirects, protocol copies exercised on temporary
    # paths, or the lease machinery and its own tests. Every entry must keep
    # referencing the lease (test_registered_mentions_still_reference_the_lease)
    # or the registration is stale and reds too.
    REGISTERED_MENTIONS = {
        'src/pilotdeck-gateway/tests/fixtures/negative_process.py':
            'the predicate and the mutation-window lease machinery itself',
        'src/pilotdeck-gateway/tests/fixtures/test_negative_process.py':
            'this reconciliation check, the family pin and the '
            'ParityMutationLock mechanism tests',
        'src/pilotdeck-gateway/tests/fixtures/cli-parity-lock-bounded/'
        'negative_controls.py':
            'the docstring documents the shared lease; --check consumes the '
            'one parity_guarded predicate',
        'src/pilotdeck-gateway/tests/fixtures/cli-parity-mock-gateway/'
        'negative_controls.py':
            'the inner machine lock loads the guarded scripts/cli-parity/'
            'common.py and stands down while its own window holds the lease',
        'src/pilotdeck-gateway/tests/fixtures/cli-parity-mock-gateway/'
        'test_negative_controls.py':
            'machine-lock tests acquire temporary leases only',
        'src/pilotdeck-gateway/tests/fixtures/native-session-e2e/test_e2e.py':
            'cells redirect parity_lock to per-case temporary leases '
            '(lock=...); one string is a documented payload',
        'src/pilotdeck-gateway/tests/fixtures/native-topology/run.py':
            'real consumer through gateway common; its mutation window '
            '(negative_signals.py) only runs cells that redirect to a '
            'temporary lease (test_signals.py), so no in-window real acquirer',
        'src/pilotdeck-gateway/tests/fixtures/native-topology/test_signals.py':
            'replays run.py with parity_lock patched to a temporary lease',
        'src/pilotdeck-gateway/tests/fixtures/native-witness/'
        'test_authentication.py':
            'cells acquire temporary leases only (lock=...)',
        'src/pilotdeck-gateway/tests/fixtures/sdk-signals/'
        'negative_controls.py':
            'the mutation window redirects its own common.parity_lock '
            'call to a per-batch evidence-dir lease (fix-r4); the shared '
            'lease stays host-pre-held per the lane contract',
        'src/pilotdeck-gateway/tests/fixtures/native-witness/test_witness.py':
            'cells patch parity_lock to temporary leases or nullcontext',
        'src/pilotdeck-gateway/tests/fixtures/parity-lock-release/'
        'negative_controls.py':
            'release-protocol copy exercised on temporary lock paths',
        'src/pilotdeck-gateway/tests/fixtures/parity-lock-release/'
        'test_lock_release.py':
            'release-protocol tests on temporary lock paths',
        'src/pilotdeck-gateway/tests/fixtures/session-domain-lock-release/'
        'test_lock_release.py':
            'release-protocol tests on temporary lock paths',
        'src/pilotdeck-gateway/tests/fixtures/session-lease/test_lease.py':
            'lease-protocol tests on temporary paths',
    }

    @classmethod
    def references(cls, path):
        """The reference kinds one code file makes to the shared lease.

        Python is scanned with tokenize so comments do not count and
        docstrings do; the other code files get the equivalent raw match.
        The path match is on the bare filename (#2513 fix-r4 boundary#3): a
        concatenated ``'/tmp/' + 'pdrs-parity.lock'`` holds no single token
        with the full path, and an f-string spelling lives in FSTRING_MIDDLE.
        """
        text = path.read_text(errors='replace')
        found = set()
        if path.suffix == '.py':
            for token in tokenize.generate_tokens(io.StringIO(text).readline):
                if token.type in (tokenize.STRING, tokenize.FSTRING_MIDDLE) \
                        and cls.LOCK_FILE in token.string:
                    found.add('path')
                elif token.type == tokenize.NAME and cls.LOCK_NAME.fullmatch(token.string):
                    found.add('name')
        else:
            if cls.LOCK_FILE in text:
                found.add('path')
            if cls.LOCK_NAME.search(text):
                found.add('name')
        return found

    @classmethod
    def unregistered(cls, root):
        """(relative, kinds) for every lock reference outside family + table."""
        violations = []
        for path in sorted(root.rglob('*')):
            if not path.is_file() or path.suffix not in cls.SUFFIXES:
                continue
            relative = path.relative_to(root)
            if any(part in cls.SKIP_DIRS or part.startswith('.')
                   for part in relative.parts[:-1]):
                continue
            found = cls.references(path)
            if found and not (negative_process.parity_guarded(relative.as_posix())
                              or relative.as_posix() in cls.REGISTERED_MENTIONS):
                violations.append((relative.as_posix(), '+'.join(sorted(found))))
        return violations

    def test_every_lock_reference_is_guarded_or_registered(self):
        violations = self.unregistered(Path(__file__).resolve().parents[4])
        self.assertTrue(not violations,
                        '#2513 out-of-family lock references: '
                        + '; '.join(f'{rel} ({kinds})' for rel, kinds in violations))

    def test_registered_mentions_still_reference_the_lease(self):
        root = Path(__file__).resolve().parents[4]
        stale = [relative for relative in self.REGISTERED_MENTIONS
                 if not self.references(root / relative)]
        self.assertTrue(not stale,
                        '#2513 stale registered mentions (no reference left): '
                        + ', '.join(sorted(stale)))

    def test_a_new_out_of_family_holder_fails_closed(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-2513-reconcile-') as temp:
            root = Path(temp).resolve()
            holder = root / 'src/pilotdeck-gateway/tests/fixtures/nowhere/oracle.py'
            holder.parent.mkdir(parents=True)
            holder.write_text('from somewhere import parity_lock\n')
            family = root / 'scripts/native-witness/collect.py'
            family.parent.mkdir(parents=True)
            family.write_text('parity_lock = common.parity_lock\n')
            self.assertEqual([row[0] for row in self.unregistered(root)],
                             [holder.relative_to(root).as_posix()],
                             '#2513 an unregistered out-of-family holder must '
                             'red the reconciliation')

    def test_a_family_member_alone_stays_green(self):
        with tempfile.TemporaryDirectory(prefix='pdrs-2513-reconcile-') as temp:
            root = Path(temp).resolve()
            family = root / 'src/pilotdeck-cli/tests/sdk_run_exit.rs'
            family.parent.mkdir(parents=True)
            family.write_text(
                'Self::acquire_at(Path::new("/tmp/pdrs-parity.lock"), '
                'Duration::from_secs(180))\n')
            self.assertTrue(not self.unregistered(root),
                            '#2513 a guarded-prefix holder needs no '
                            'registration to stay green')

    def test_concatenated_lock_spelling_is_still_caught(self):
        # #2513 fix-r4 (review boundary#3): the path check must survive the
        # '/tmp/' + 'pdrs-parity.lock' spelling — no single token carries the
        # full path, so the pre-fix STRING-only, LOCK_PATH-only scan saw
        # nothing and the holder shipped without a registration.
        with tempfile.TemporaryDirectory(prefix='pdrs-2513-reconcile-') as temp:
            root = Path(temp).resolve()
            holder = root / 'src/pilotdeck-gateway/tests/fixtures/nowhere/oracle.py'
            holder.parent.mkdir(parents=True)
            holder.write_text("LOCK = '/tmp/' + 'pdrs-parity.lock'\n")
            self.assertEqual([row[0] for row in self.unregistered(root)],
                             [holder.relative_to(root).as_posix()],
                             '#2513 a concatenated lease-path spelling must '
                             'still count as a lock reference')

    def test_fstring_lock_spelling_is_still_caught(self):
        # PEP 701 tokenizes the literal part of an f-string as FSTRING_MIDDLE,
        # not STRING: a bare-STRING scan would skip this spelling entirely.
        with tempfile.TemporaryDirectory(prefix='pdrs-2513-reconcile-') as temp:
            root = Path(temp).resolve()
            holder = root / 'src/pilotdeck-gateway/tests/fixtures/nowhere/oracle.py'
            holder.parent.mkdir(parents=True)
            holder.write_text("base = '/tmp'\n"
                              "LOCK = f'{base}/pdrs-parity.lock'\n")
            self.assertEqual([row[0] for row in self.unregistered(root)],
                             [holder.relative_to(root).as_posix()],
                             '#2513 an f-string lease-path spelling must '
                             'still count as a lock reference')

    def test_raw_fragment_spelling_is_still_caught(self):
        # Non-Python code files get the raw text match; its path predicate
        # must accept the bare filename too, or a fragment spelling outside
        # Python is the same hole one file suffix away.
        with tempfile.TemporaryDirectory(prefix='pdrs-2513-reconcile-') as temp:
            root = Path(temp).resolve()
            holder = root / 'src/pilotdeck-gateway/tests/fixtures/nowhere/run.sh'
            holder.parent.mkdir(parents=True)
            holder.write_text('exec flock "${TMPDIR}pdrs-parity.lock" "$@"\n')
            self.assertEqual([row[0] for row in self.unregistered(root)],
                             [holder.relative_to(root).as_posix()],
                             '#2513 a raw filename fragment must still count '
                             'as a lock reference')


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
                # #1903：还原改走 `write_source_atomic`（临时文件 + os.replace），
                # 只钉 `Path.write_bytes` 会漏掉这条写入路径。
                with context, patch.object(Path, "write_bytes") as write, \
                        patch.object(negative_process, "write_source_atomic",
                                     side_effect=AssertionError(
                                         "#166 refused lock must not publish source bytes")) as publish:
                    with self.assertRaises(TimeoutError, msg="#166 busy parity lock must time out"):
                        with mutating_sources(root, [source, target], lock_timeout=0.05):
                            self.fail("#166 busy parity lock must refuse before mutation")
                    self.assertEqual(write.call_count, 0, "#166 refused lock must not write source bytes")
                    self.assertEqual(publish.call_count, 0, "#166 refused lock must not publish source bytes")
                    # #1903 r2：与 test_metadata_io_driver 的两处 #166 探针同形接线判据 ——
                    # 还原必须按 negative_process 的**模块全局**解析，否则上面那条
                    # `publish.call_count == 0` 永远成立（探针空转）。默认参绑定与函数内
                    # `from negative_process import …` 都会落进 `co_varnames`，一并抓。
                    restore = getattr(negative_process.mutating_sources, "__wrapped__",
                                      negative_process.mutating_sources)
                    # r3（zai boundary P3 #2）：三条与 driver 的 `restore_wiring_problems`
                    # 对齐 —— 少的 `__globals__` 那条挡的是「`mutating_sources` 被
                    # `from other import mutating_sources` 带进来」：它的 `co_names` 仍含
                    # `write_source_atomic`（other 模块的全局），探针照盲。
                    self.assertIs(restore.__globals__, vars(negative_process),
                                  "#1903 the restore must stay defined in negative_process")
                    self.assertIn("write_source_atomic", restore.__code__.co_names,
                                  "#1903 the restore must resolve write_source_atomic as a global")
                    self.assertNotIn("write_source_atomic", restore.__code__.co_varnames,
                                     "#1903 a parameter or local binding would blind this probe")
                self.assertEqual(target.read_bytes(), snapshot[target], "#166 busy lock preserves original bytes")

    def test_failed_restore_retains_cleanup_pending_lease(self):
        with self.fixture() as (root, source, target, snapshot, common):
            raw_replace = os.replace

            # #1903：还原的落盘是 `write_source_atomic` 的 `os.replace`，旧的
            # `Path.write_bytes` 注入已经不再经过还原路径。
            def fail_restore(actual, destination, *args, **kwargs):
                if Path(destination) == target:
                    raise OSError("#166 injected restore failure")
                return raw_replace(actual, destination, *args, **kwargs)

            try:
                with self.assertRaisesRegex(OSError, "#166 injected restore failure"), \
                        patch.object(negative_process.os, "replace", fail_restore):
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


RESTORE_WINDOW_CHILD = r"""
import os, pathlib, signal, sys, time
sys.path.insert(0, sys.argv[1])
from negative_process import restore_critical, take_deferred_signal
sentinel = pathlib.Path(sys.argv[2])
names = ("SIGTERM", "SIGINT") if sys.argv[3] == "double" else (sys.argv[3],)
signals_ = [getattr(signal, name) for name in names]
disposition = sys.argv[4]
for which in signals_:
    if disposition == "default":
        # A background-launched parent inherits SIGINT=SIG_IGN; reset it so the
        # case exercises the shield and not the inherited disposition.
        signal.signal(which, signal.SIG_DFL)
    elif disposition == "fixture":
        def handler(signum, _frame):
            raise SystemExit(128 + signum)
        signal.signal(which, handler)
    elif disposition == "ignore":
        signal.signal(which, signal.SIG_IGN)
with restore_critical():
    for which in signals_:
        os.kill(os.getpid(), which)
    time.sleep(0.05)
    sentinel.write_text("restore finished\n", encoding="utf-8")
pending = take_deferred_signal()
if pending is not None:
    raise SystemExit(128 + pending)
"""


class RestoreWindowSignals(unittest.TestCase):
    """#892 r19: the restore window shields itself, not just one consumer.

    ``restore_critical`` used to rely on ``terminate_cleanly`` calling
    ``defer_signal_exit``, so every other fixture handler -- and a plain
    Ctrl-C, for which no handler was installed at all -- still raised
    ``SystemExit`` on an arbitrary bytecode boundary inside the write-back
    loop and left a mutant behind (MiniMax sibling r18 P1-2 / GLM sibling r18
    P1-3).
    """

    def _run(self, signame, disposition):
        fixtures = str(Path(__file__).resolve().parent)
        with tempfile.TemporaryDirectory(prefix="pdrs-restore-window-") as temp:
            sentinel = Path(temp) / "sentinel"
            cp = subprocess.run(
                [sys.executable, "-I", "-c", RESTORE_WINDOW_CHILD, fixtures,
                 str(sentinel), signame, disposition],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            return cp, sentinel.exists()

    def test_sigterm_from_a_fixture_handler_still_finishes_the_restore(self):
        cp, sentinel = self._run("SIGTERM", "fixture")
        self.assertTrue(sentinel, "SIGTERM unwound inside the window: " + cp.stderr)
        self.assertEqual(cp.returncode, 128 + signal.SIGTERM)

    def test_sigint_with_the_default_handler_still_finishes_the_restore(self):
        cp, sentinel = self._run("SIGINT", "default")
        self.assertTrue(sentinel, "SIGINT unwound inside the window: " + cp.stderr)
        self.assertEqual(cp.returncode, 128 + signal.SIGINT)

    def test_sigint_from_a_fixture_handler_still_finishes_the_restore(self):
        cp, sentinel = self._run("SIGINT", "fixture")
        self.assertTrue(sentinel, "SIGINT unwound inside the window: " + cp.stderr)
        self.assertEqual(cp.returncode, 128 + signal.SIGINT)

    def test_sighup_with_the_default_handler_still_finishes_the_restore(self):
        cp, sentinel = self._run("SIGHUP", "default")
        self.assertTrue(sentinel, "SIGHUP unwound inside the window: " + cp.stderr)
        self.assertEqual(cp.returncode, 128 + signal.SIGHUP)

    def test_sigquit_with_the_default_handler_still_finishes_the_restore(self):
        # ``^\`` is catchable and deferrable like the other three; a restore
        # killed by a core-dumping SIGQUIT would leave a mutant behind (GLM
        # sibling r20 P2-4).  The case itself is the runtime probe of the
        # disposition the reviewer asked to verify.
        cp, sentinel = self._run("SIGQUIT", "default")
        self.assertTrue(sentinel, "SIGQUIT unwound inside the window: " + cp.stderr)
        self.assertEqual(cp.returncode, 128 + signal.SIGQUIT)

    def test_the_first_of_two_signals_inside_one_window_wins(self):
        # The window keeps one deferred signum: the *second* signal is the one
        # dropped, so the first never gets overwritten (anthropic concurrency
        # r20 P2-1 / boundary r20 P2-2).
        cp, sentinel = self._run("double", "default")
        self.assertTrue(sentinel, "the second signal unwound inside the window: "
                        + cp.stderr)
        self.assertEqual(cp.returncode, 128 + signal.SIGTERM)

    def test_an_ignored_signal_stays_ignored(self):
        cp, sentinel = self._run("SIGTERM", "ignore")
        self.assertTrue(sentinel, "an ignored SIGTERM must not end the window: " + cp.stderr)
        self.assertEqual(cp.returncode, 0)

    def test_a_failing_body_does_not_leak_the_deferred_signal(self):
        """#892 r20: the recorded signum may not outlive a window that raised.

        ``take_deferred_signal`` sits after the ``with`` block, so a body that
        raises never reaches it; without the clear-on-raise the stale signum
        would be taken by the next window in the same process and exit 128+n
        for a signal that arrived much earlier (MiniMax boundary r19 P2).
        """
        boom = RuntimeError("restore body failed")
        with self.assertRaises(RuntimeError) as caught:
            with negative_process.restore_critical():
                self.assertTrue(negative_process.defer_signal_exit(signal.SIGTERM),
                                "the window must be open when the signal lands")
                raise boom
        self.assertIs(caught.exception, boom)
        self.assertIsNone(negative_process.take_deferred_signal(),
                          "a failed body must not leave a signum for the next window")

    def test_handlers_are_put_back_after_the_window(self):
        previous = signal.getsignal(signal.SIGTERM)

        def handler(signum, _frame):
            pass

        try:
            signal.signal(signal.SIGTERM, handler)
            with negative_process.restore_critical():
                shielded = signal.getsignal(signal.SIGTERM)
                self.assertTrue(callable(shielded), "the shield must install a handler")
                self.assertIsNot(shielded, handler, "the consumer handler must be swapped out")
            self.assertIs(signal.getsignal(signal.SIGTERM), handler,
                          "the consumer handler must come back")
            with negative_process.restore_critical():
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
            self.assertIs(signal.getsignal(signal.SIGTERM), signal.SIG_IGN,
                          "a handler installed inside the window must survive it")
        finally:
            # getsignal returns None when a non-Python handler holds the
            # signal; signal.signal refuses None, so map that back to the
            # default disposition (same guard as the #2552 r9 legs in
            # test_safety.py, DS r10 sibling#2).
            signal.signal(signal.SIGTERM, previous if previous is not None
                          else signal.SIG_DFL)


TERMINATE_HANDLER_CHILD = r"""
import os, pathlib, signal, sys, time
sys.path.insert(0, sys.argv[1])
import negative_controls
which = getattr(signal, sys.argv[3])
# #892 r24: a background / ``nohup`` parent hands SIGQUIT (and SIGINT) down as
# SIG_IGN, and the child inherits that disposition.  The control arm must still
# measure the *default* disposition, so it resets the tested signal instead of
# relying on the caller's shell (memory: background inherits SIGINT ignored);
# SIGINT is restored to the interpreter's own handler for the same reason.
if which != signal.SIGINT:
    signal.signal(signal.SIGINT, signal.default_int_handler)
if sys.argv[4] == "registered":
    negative_controls._install_terminate_handlers()
else:
    signal.signal(which, signal.SIG_DFL)
sentinel = pathlib.Path(sys.argv[2])
try:
    os.kill(os.getpid(), which)
    time.sleep(10)
finally:
    sentinel.write_text("restored\n", encoding="utf-8")
"""


class TerminateHandlerRegistration(unittest.TestCase):
    """#892 r23: the harness registers SIGQUIT too, not only SIGTERM/SIGHUP.

    ``_SHIELDED_SIGNALS`` defers SIGQUIT only inside the restore window;
    outside it the default disposition is kill-with-core, so a ``^\\`` landing
    between two mutations would leave a mutant in the tree with no finally --
    the exact SIGTERM hazard ``terminate_cleanly`` exists for (anthropic
    sibling r22 P1 / GLM boundary r22 P2, taken at the higher grade).  The
    second case is the control: without the registration the same signal
    kills in place, which is the only difference the registration makes.
    """

    def _run(self, signame, registered):
        cleanup = str(Path(__file__).resolve().parent / "session-domain-cleanup")
        with tempfile.TemporaryDirectory(prefix="pdrs-terminate-handler-") as temp:
            sentinel = Path(temp) / "sentinel"
            cp = subprocess.run(
                [sys.executable, "-I", "-c", TERMINATE_HANDLER_CHILD, cleanup,
                 str(sentinel), signame, registered],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            return cp, sentinel.exists()

    def test_sigquit_outside_the_window_unwinds_through_finally(self):
        cp, sentinel = self._run("SIGQUIT", "registered")
        self.assertTrue(sentinel,
                        "SIGQUIT outside the window must unwind: " + cp.stderr)
        self.assertEqual(cp.returncode, 128 + signal.SIGQUIT)

    def test_the_same_signal_without_the_registration_kills_in_place(self):
        cp, sentinel = self._run("SIGQUIT", "default")
        self.assertFalse(sentinel,
                          "no registration means no finally: " + cp.stderr)
        self.assertEqual(cp.returncode, -signal.SIGQUIT)


if __name__ == "__main__":
    unittest.main(verbosity=2)

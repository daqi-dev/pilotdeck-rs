#!/usr/bin/env python3
# anchor-selfcheck: unsupported #2981 sdk-signals abort-accounting meta-control on a temporary tree, no product mutation table
"""#2981: the sdk-signals runner's hand-written abort accounting, pinned.

`sdk-signals/negative_controls.py` is a self-managed loop (it never calls
``negative_suite.run_cases``), so its ``except BaseException`` branch —
revoke every recorded KILLED through the shared ``invalidate_completed``,
record every unrun control as INVALID, still publish the summary, return 2 —
is hand-written, and until this file it had no in-repo test or meta-control
(the #1800 pin covers only the ``sdk-signals-close-order-pin`` sibling). A
regression would surface only as a false PASS with green rows over an
unverified tree (#979 P0), or as a guard fault misread as a verdict.

Every case drives the real runner source in a subprocess whose ``__file__``
points at an identical path inside a temporary tree, so the runner's
self-computed ``root``, its whole mutation table (the anchor preflight
``check_catalog`` must pass over the copy) and both frozen ledgers all land
in the temporary tree. The shared ``negative_suite.invalidate_completed``
and ``publish_summary`` are never mocked — half of the pinned behavior is
that the abort accounting routes through the shared revocation — and only
the ``negative_process`` boundaries are patched in memory, exactly like the
issue-2495 / close-order-pin precedents:

- healthy: the guard is a snapshot/restore stub and ``run_owned`` answers
  the phases from a queue (positive passes, the compile leg is green, each
  control's negative fails at its own bound assertion inside a real-shape
  libtest panic block, the restored run passes). The selection must come out
  all KILLED at rc 0 — the positive control that makes every revoke
  assertion below non-vacuous;
- busy-lock: a real subprocess holds ``<tree>/.negative-source.lock`` through
  the real ``tree_lock``, the runner runs the real ``mutating_sources`` with
  its wait shortened to 2 s (the issue-2495 GUARD_LOCK_FAST shape — this
  runner passes no lock timeout of its own, so the fast bound rides its one
  call site), and the lock timeout must land in the abort accounting: every
  selected row INVALID with the lock path in the reason, rc 2. Unlike the
  close-order pin, ``check_catalog`` runs (and proves the frozen ledgers)
  before the guard opens, so the git read is served from the temporary
  tree's own bytes in this mode too;
- mid-restore-fault: the runner's own restore read-back fails on the last
  control (its ``write_bytes`` restore is skipped, the mutant stays on disk
  and the ``#1145 restoration mismatch`` check fires) — the already recorded
  row must be revoked with its prior diagnostic kept as a prefix, the unrun
  row recorded INVALID, the guard still restores the tree, rc 2;
- guard-exit-fault: the stub guard raises after the body completed — the
  revoke-everything half of the same accounting (#979);
- ledger-drift: ``_head_bytes`` fails on the entry-side frozen-ledger proof
  inside the guard, where ``phase`` is already ``'body'`` — so only the
  ``isinstance(error, FrozenLedgerDrift)`` term of the guard-failure
  predicate can route this run to rc 2. Dropping that term turns the same
  run into an all-INVALID verdict-path rc 1, which this test must catch;
- cancel: ``run_owned`` raises KeyboardInterrupt during the last control's
  negative run — the deterministic shape of a Ctrl-C, which is a
  BaseException the per-control ``except Exception`` must not swallow;
- preflight-drift: ``_head_bytes`` fails on ``check_catalog``'s own ledger
  proof, before any argument parsing — the ``FrozenLedgerDrift`` route in
  ``main`` that prints a diagnostic and returns 2 without creating any
  evidence directory;
- survived: ``run_owned`` answers every phase green — the verdict edge none
  of the abort modes can reach: every row is SURVIVED and the run exits 1;
- invalid-verdict: the negative run fails *without* the bound assertion —
  the shape a stale anchor produces on real hardware — so the row is
  INVALID through the verdict path, not the abort path;
- exit-ledger-drift: the real exit-side ``assert_frozen_ledgers_match_head``
  (the ``phase = "exit"`` proof after the last control) fails — every
  completed row must still land in the abort accounting, its diagnostic
  kept as a prefix, rc 2. This is the use case for those lines: deleting
  the exit-side proof turns this test red (rc 0 != the asserted 2), not
  green, and no earlier case can (the ledger-drift case faults the
  entry-side proof only, and guard-exit-fault raises from the guard's
  cleanup, not from the proof).

The three verdict-delivery legs of an otherwise healthy run are pinned
as they behave today (fix-r2, MM concurrency + DS sibling both flagged the
missing coverage — the close-order pin has had the same three since its
fix-r2/r3/r4). This runner has no exit handling of its own at any of them:
the prints write straight through, ``publish_summary`` is called bare, and
an escaping exception is finished by the interpreter. Each leg therefore
pins the current behavior, with the exit-code contract collection
registered as #1866 (a deliberate change there changes this class):

- stdout-pipe-fault: the run is read through a real ``... | head -1``
  subprocess; the reader takes the first record() line and exits, and the
  next record() print raises BrokenPipeError — ``results.append`` precedes
  that print, so the row is already recorded and the abort accounting still
  revokes it and publishes the all-INVALID summary; only the final counts
  line's own BrokenPipeError escapes ``main``, leaving a nonzero rc chosen
  by the interpreter and no counts line delivered;
- stdout-write-fault: the final counts line fails with the measured
  /dev/full shape (OSError ENOSPC, never a pipe error). The summary is
  already on disk at that point (the publish precedes the counts print),
  so the verdict survives on disk while the stdout delivery fails;
- summary-publish-fault: the summary rename itself fails (the
  ``Path.replace`` injection over the publish temp file) after the rows
  completed: no summary.json, no leftover temp, and the record lines
  still on stdout.

fix-r4（MM concurrency#1）adds the driver's own failure leg: a runner hung
after its first delivered line must be bounded and reaped by
``run_runner_piped`` — the old unbounded ``stderr.read()`` hung the whole
module until the CI step killed it, taking all eight modules' verdicts with
it.

Both non-abort verdict edges pin this runner's current exit contract
(``return int(any(row != KILLED))``): the verdict path's rc 1, distinct from
the abort path's rc 2. That contract differs from the close-order pin's
os._exit tri-state on purpose — the exit-code contract follow-up is
registered as #1866, and when it changes this file changes with it.

The pre-guard refusals (an unknown selection, a missing CARGO_TARGET_DIR, a
missing PDRS_BASELINE) exit 1 with no summary — those run the real,
unmodified script directly, because nothing needs patching before the
guard. Run from this directory:
``python3 -I sdk-signals/test_negative_controls.py``.
"""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

FIXTURES = Path(__file__).resolve().parents[1]
RUNNER_SCRIPT = FIXTURES / 'sdk-signals' / 'negative_controls.py'
REAL_ROOT = Path(__file__).resolve().parents[5]

# The first per-ID row and a later per-ID row of the registered table: both
# target the shared replay harness, so the mid-restore fault has exactly one
# file whose restore write can be skipped, and both carry the #1252 criterion
# family the close-order pin already keeps registered.
SELECTION = ('unordered-close-run--query-resume-hold-sigint',
             'unordered-close-resume--second-sigterm')
REPLAY_RELATIVE = 'src/pilotdeck-cli/tests/common/signal_replay.rs'
CLOSE_ORDER_RELATIVE = 'specs/gateway-parity/diffs/D1-SDK-WS-CLOSE-ORDER-001.md'
HEALTHY_REASON = 'bound test failed at its assertion'
ABORT_PREFIX = 'source guard or interruption: '
ENTRY_DRIFT = (ABORT_PREFIX + 'FrozenLedgerDrift: #1885 #1382 baseline proof'
               ' failed: ' + CLOSE_ORDER_RELATIVE)


def _load_catalog():
    """The runner's own module, loaded for its table and constants.

    Loading the real script (its module level is constants and functions
    only — ``check_catalog`` runs inside ``main``) keeps the temporary tree's
    file set single-sourced: every mutation target plus the read-only
    close-order ledger. A row whose target file is not copied fails the
    temp-tree ``check_catalog`` on the next run, so the copy set cannot
    drift behind the table.
    """
    import importlib.util
    spec = importlib.util.spec_from_file_location(
        'pdrs_2981_sdk_signals_catalog', str(RUNNER_SCRIPT))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


_CATALOG = _load_catalog()

RELATIVE_TARGETS = tuple(sorted(
    path.relative_to(_CATALOG.root).as_posix()
    for path in {row[1] for row in _CATALOG.mutations}
    | {_CATALOG.close_order_ledger}))

# fix-r2（DS sibling#3）：把故障注入前提钉在表上——SELECTION 的每一行都必须
# 指向同一份回放 harness（REPLAY_RELATIVE）。mid-restore-fault 只跳过最后
# 一个控件对该文件的第 len(names) 次恢复写；每个用例的还原断言也只对这份
# 字节采样。表里哪一行改指到别的文件，注入不再触发 / 断言采错对象，而
# 本文件其余部分照绿——先在这里红，把漂移挡在选择处。
_BY_NAME = {row[0]: row for row in _CATALOG.mutations}
# fix-r3（MM concurrency P2）：守卫写成显式 raise，不依赖 assert——`python3
# -O` / PYTHONOPTIMIZE=1 会整条剥离 assert（fixtures README 三条硬要求之
# 三），剥离后本前提钉静默消失；raise 在任何解释器档位都拒跑。
if not all(
        _BY_NAME[name][1].relative_to(_CATALOG.root).as_posix()
        == REPLAY_RELATIVE
        for name in SELECTION):
    raise RuntimeError(
        '#2981 every selected row must target the shared replay harness '
        + REPLAY_RELATIVE)

# The subprocess driver. It execs the real runner source with ``__file__``
# inside the temporary tree and patches only the negative_process boundaries
# the busy-lock case must keep real. Mode is the first argv element.
HARNESS_SOURCE = """\
import contextlib
import subprocess
import sys
import time
from pathlib import Path

mode = sys.argv[1]
runner_path = Path(sys.argv[2]).resolve()
real_fixtures = Path(sys.argv[3]).resolve()
tree = Path(sys.argv[4]).resolve()
out = Path(sys.argv[5])
names = sys.argv[6:]

# #2981 fix-r5（MM concurrency#2 的姊妹 #7，DS simplify#2）：hang 模式的
# 自释放常量——此前它与用例侧的墙钟上界同为 60，纯靠巧合同值维系判别
# 力。解耦成独立大常量：用例上界（驱动超时 ×4）必须严格小于自释放，
# 才能区分「驱动的超时腿先到」与「孤儿等自己醒」；即使驱动的回收被
# 变异掉，孤儿也会在此时限内自退，不把宿主钉成无限泄漏。
ORPHAN_RELEASE_S = 300

sys.path.insert(0, str(real_fixtures))
import negative_process

# The catalog's own table, loaded from the tree copy before exec: the
# exec'd module only populates its globals while it runs, but the mid-restore
# fault must resolve its target file before the body starts. Importing the
# module is safe — its module level is constants and function defs only
# (check_catalog runs inside main, not at import).
import importlib.util
catalog_spec = importlib.util.spec_from_file_location(
    'pdrs_2981_harness_catalog', str(runner_path))
catalog = importlib.util.module_from_spec(catalog_spec)
catalog_spec.loader.exec_module(catalog)
table = {row[0]: row for row in catalog.mutations}

source = runner_path.read_text(encoding='utf-8')

if mode in ('busy-lock', 'preflight-drift'):
    # Neither mode reaches a fake guard: busy-lock aborts inside the real
    # one, preflight-drift inside check_catalog before it. Both still run
    # check_catalog() first (busy-lock: the ledger proof and the whole
    # 79-anchor preflight; preflight-drift: the ledger proof only), and the
    # temporary tree is not a git checkout, so the git read is served from
    # the tree's own bytes.
    if mode == 'busy-lock':
        # Real guard, real flock timeout: only the wait bound is shortened
        # (the issue-2495 GUARD_LOCK_FAST shape; this runner passes no lock
        # timeout of its own, so the fast bound rides its one call site).
        # The timeout fires before any git work inside the guard, so the
        # temporary tree needs no repository.
        old = 'with mutating_sources(root, guarded) as originals:'
        # #2981 fix-r4（MM sibling#2）：前提守卫不许用裸 assert——``python3 -O``
        # 剥离后锚点漂移静默放行（fixtures README 三条硬要求之三）；raise 在
        # 任何解释器档位都拒跑，与本文件 fix-r3 已改的 SELECTION 前提钉同形。
        if source.count(old) != 1:
            raise RuntimeError('#2981 stale mutating_sources call anchor')
        source = source.replace(
            old,
            'with mutating_sources(root, guarded, lock_timeout=2)'
            ' as originals:',
            1)

        def fake_head_bytes(actual_root, relative, _baseline):
            return (Path(actual_root) / relative).read_bytes()
    else:
        def fake_head_bytes(actual_root, relative, _baseline):
            raise OSError('#2981 injected ledger drift')
    negative_process._head_bytes = fake_head_bytes
    runner_globals = {'__file__': str(runner_path), '__name__': '__main__'}
else:
    @contextlib.contextmanager
    def fake_guard(actual_root, requested, **_kwargs):
        # #2981 fix-r5（DS boundary#1 点名，四路同撞升 P1）：入口置位之外
        # 补 finally 复位——出场注入的守卫项靠复位才有判别力（证明被挪到
        # with 之后时 guard 已回 False，注入不再误触发，用例转红）。
        state['guard'] = True
        snapshots = {Path(path): Path(path).read_bytes() for path in requested}
        try:
            yield snapshots
        finally:
            for path, data in snapshots.items():
                negative_process.write_source_atomic(actual_root, path, data)
            state['guard'] = False
            if mode == 'guard-exit-fault':
                raise RuntimeError('#2981 injected guard exit failure')

    # #2981 fix-r4（MM simplify#1）：注入按相位绑定，不按 _head_bytes 调用
    # 序号——旧的 ``index >= 1`` / ``index >= 3`` 把「check_catalog 恰读一本、
    # 每次证明恰读两本」写死，runner 增删账本或证明次数，注入就会落到错误
    # 的相位（或不再触发），绿灯从此证明不了「钉的是哪条证明」。相位以
    # runner 的可见行为绑定（实测）：check_catalog 的读在守卫打开之前
    # （head 0 / guard 未开）；入场证明在守卫内、任何 run_owned 之前
    # （head 1-2 / runs 0）；出场证明在全部 run_owned 之后（head 3-4 /
    # runs 8 = 4×len(names)）。
    # #2981 fix-r5（DS boundary#1 点名，升 P1）：入场侧 r4 已绑守卫窗口，
    # 本轮出场侧同样补上（state['guard']），且 fake_guard 的 finally 补
    # 复位——否则置位后恒 True，出场注入的守卫项形同虚设：证明被挪到
    # with 之外时注入照触发，用例照绿（假 PASS，NC 审计结论被污染）。
    calls = {'run': 0}
    state = {'guard': False, 'exit_served': False}

    def fake_slot(script, **_kwargs):
        return subprocess.CompletedProcess([], 0, '', '')

    def _row(control):
        return table[names[control]]

    def _ok_log(test):
        return (
            'running 1 test\\n'
            'test ' + test + ' ... ok\\n\\n'
            'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;'
            ' 0 filtered out; finished in 0.01s\\n')

    def _failed_log(test, message):
        # The exact libtest shape negative_suite.classify demands: the named
        # FAILED line, the panic payload inside the test's own thread block,
        # and a FAILED summary (negative_panic accepts nothing weaker).
        return (
            'running 1 test\\n'
            'test ' + test + ' ... FAILED\\n\\n'
            'failures:\\n\\n'
            '---- ' + test + ' stdout ----\\n'
            "thread '" + test + "' panicked at"
            ' src/pilotdeck-cli/tests/common/signal_replay.rs:1:1:\\n'
            + message + '\\n'
            'note: run with `RUST_BACKTRACE=1` environment variable to'
            ' display a backtrace\\n\\n'
            'failures:\\n    ' + test + '\\n\\n'
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured;'
            ' 0 filtered out; finished in 0.01s\\n\\n'
            'error: test failed, to rerun pass ' + repr(test))

    def fake_run_owned(command, **_kwargs):
        # The cargo command carries no phase label, so the phases are served
        # from the runner's own deterministic order: positive, compile,
        # negative and restored per selected control (the SessionRunnerTests
        # stage-queue shape).
        if mode == 'stdout-pipe':
            # The real ``... | head -1`` reader exits as soon as it has the
            # first record() line; give it time to close the pipe before
            # the next print, so the BrokenPipeError lands deterministically
            # during the run (never at the final counts line).
            time.sleep(0.2)
        index = calls['run']
        calls['run'] += 1
        # Serving the last of the runner's deterministic phases (4 per
        # control) marks the exit phase: every later _head_bytes read can
        # only be the exit-side proof (see the state comment above).
        if calls['run'] == 4 * len(names):
            state['exit_served'] = True
        if mode == 'hang' and index >= 4:
            # #2981 fix-r4（MM concurrency#1）：确定性的「写完首行后卡死」——
            # 第一条 record() 行已到达 reader（head 取行退出），第二个控制的
            # 首相位起 runner 永不再前进。自释放改用 ORPHAN_RELEASE_S
            # （见其定义处的 fix-r5 注释：与用例墙钟上界解耦）。
            time.sleep(ORPHAN_RELEASE_S)
        row = _row(index // 4)
        if mode == 'survived':
            # The verdict edge no abort mode can reach: every phase green,
            # so the negative run still passes and the row survives.
            return subprocess.CompletedProcess(command, 0, _ok_log(row[4]), '')
        phase = index % 4
        if phase in (0, 3):
            return subprocess.CompletedProcess(command, 0, _ok_log(row[4]), '')
        if phase == 1:
            return subprocess.CompletedProcess(command, 0, '', '')
        if mode == 'cancel' and index // 4 == len(names) - 1:
            raise KeyboardInterrupt()
        if mode == 'invalid-verdict':
            # The verdict path's INVALID row: the negative run fails without
            # the bound assertion (the stale-anchor shape), so the runner
            # records INVALID and the exit comes from the verdict path,
            # never the abort branch.
            return subprocess.CompletedProcess(
                command, 1, _failed_log(row[4], 'an unrelated assertion'), '')
        return subprocess.CompletedProcess(
            command, 1, _failed_log(row[4], row[5]), '')

    def fake_head_bytes(actual_root, relative, _baseline):
        # #2981 fix-r4：相位判定见 state 上方注释（MM simplify#1）；fix-r5
        # （升 P1）：出场注入补 state['guard'] —— 出场证明必须在守卫窗口
        # 内触发，被挪到 with 之外时注入不再误触发、用例转红而不是照绿。
        if mode == 'ledger-drift' and state['guard'] and calls['run'] == 0:
            # The entry-side proof inside the guard, where `phase` is already
            # 'body' and only the FrozenLedgerDrift term of the
            # guard-failure predicate can route the run to rc 2.
            # check_catalog's own proof runs before the guard opens and must
            # stay green here — that read is the preflight-drift mode's
            # territory.
            raise OSError('#2981 injected ledger drift')
        if mode == 'exit-ledger-drift' and state['guard'] and state['exit_served']:
            # The exit-side proof after the last control, where `phase` is
            # 'exit' and the raise comes out of the runner's own
            # assert_frozen_ledgers_match_head() on its way out of the
            # guard. Failing there and nowhere else is what makes this the
            # use case for that proof: delete it and this case goes red
            # (rc 0 != the asserted 2), not green.
            raise OSError('#2981 injected exit ledger drift')
        return (Path(actual_root) / relative).read_bytes()

    negative_process.mutating_sources = fake_guard
    negative_process.run_owned = fake_run_owned
    negative_process.wait_for_slot = fake_slot
    negative_process._head_bytes = fake_head_bytes

    if mode == 'mid-restore-fault':
        # The runner's own restore check: skip the last control's write-back
        # so the mutant stays on disk and the read-back comparison raises.
        target = _row(len(names) - 1)[1]
        raw_write_bytes = Path.write_bytes
        restore_writes = {'count': 0}

        def skip_last_restore(self, data):
            if self == target:
                restore_writes['count'] += 1
                if restore_writes['count'] == len(names):
                    return len(data)
            return raw_write_bytes(self, data)

        Path.write_bytes = skip_last_restore

    if mode == 'summary-publish-fault':
        # The publish step itself: publish_summary writes its temp file
        # (`.summary.json.<pid>.<serial>.tmp`) and renames it onto
        # summary.json, so fail exactly that rename after an otherwise
        # healthy run. The guard's restore goes through
        # write_source_atomic (os.replace) and is untouched.
        raw_replace = Path.replace

        def fail_summary_publish(self, target):
            if self.name.startswith('.summary.json.'):
                raise OSError('#2981 injected summary publish failure')
            return raw_replace(self, target)

        Path.replace = fail_summary_publish

    if mode == 'stdout-write-fault':
        # The final counts line fails with the measured /dev/full shape —
        # OSError(ENOSPC), never a pipe error (the sibling's host
        # evidence). Only that one line fails: the per-control record
        # lines still reach the reader, so the run itself completed and
        # only the stdout delivery broke. The swap is in-process (nothing
        # inherited from the environment is consulted) and stderr keeps
        # its real stream, so the traceback the interpreter prints for the
        # escape stays observable.
        real_stdout = sys.stdout

        class FullDiskStdout:
            def write(self, text):
                if '"counts"' in text:
                    raise OSError(28, 'No space left on device')
                return real_stdout.write(text)

            def flush(self):
                return real_stdout.flush()

            def close(self):
                return real_stdout.close()

        sys.stdout = FullDiskStdout()

    runner_globals = {'__file__': str(runner_path), '__name__': '__main__'}

sys.argv = [str(runner_path), str(out), *names]
exec(compile(source, str(runner_path), 'exec'), runner_globals)
"""

# A real subprocess holds the temporary tree's source lock; the ready file is
# published atomically once the lock is held (the issue-2495 holder shape).
LOCK_HOLDER_SOURCE = (
    'import sys, time\n'
    'from pathlib import Path\n'
    'sys.path.insert(0, sys.argv[1])\n'
    'from negative_process import tree_lock\n'
    'with tree_lock(sys.argv[2], timeout=10):\n'
    '    Path(sys.argv[3] + ".tmp").write_text("held")\n'
    '    Path(sys.argv[3] + ".tmp").replace(sys.argv[3])\n'
    '    time.sleep(30)\n'
)

_BASES = []


class _RunnerDriver:
    """The shared subprocess driver: the real runner over a temporary tree.

    Not a TestCase — both the abort class and the verdict class below need
    ``run_runner`` / ``counts_line``, and neither can inherit the other's
    tests.
    """

    @staticmethod
    def _new_base(prefix):
        base = Path(tempfile.mkdtemp(prefix=prefix))
        _BASES.append(base)
        return base

    @staticmethod
    def _stage(base):
        """Stage the temporary tree, the redirected runner and the harness.

        Returns ``(tree, tree_runner, out, target_before, env)``; the
        replay-harness copy is taken before any run so every case can also
        prove the tree was restored whatever the abort did.
        """
        tree = base / 'tree'
        for relative in RELATIVE_TARGETS:
            path = tree / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes((REAL_ROOT / relative).read_bytes())
        # The runner itself is redirected too: the subprocess execs the real
        # source but with ``__file__`` pointing at the identical path inside
        # the temporary tree, so the runner's self-computed root (and with it
        # the whole mutation table, both frozen ledgers and the source lock)
        # lands in the temporary tree — a driving bug that left ``__file__``
        # on the real script would aim the real guard's snapshot/restore at
        # the real worktree.
        tree_runner = tree / RUNNER_SCRIPT.relative_to(REAL_ROOT)
        tree_runner.parent.mkdir(parents=True, exist_ok=True)
        tree_runner.write_bytes(RUNNER_SCRIPT.read_bytes())
        harness = base / 'harness.py'
        harness.write_text(HARNESS_SOURCE, encoding='utf-8')
        out = base / 'out'
        before = (tree / REPLAY_RELATIVE).read_bytes()
        env = {**os.environ,
               'CARGO_TARGET_DIR': str(base / 'unused-target'),
               'PDRS_BASELINE': str(base / 'unused-baseline')}
        return tree, tree_runner, out, before, env

    def run_runner(self, mode, names=SELECTION, base=None):
        """Drive the real runner source in a subprocess over a temporary tree.

        Returns ``(process, summary-or-None, tree, target_before, out)``.
        ``base`` lets a caller reuse a tree it prepared (the busy-lock case
        must lock the very tree the runner then runs over).
        """
        if base is None:
            base = self._new_base('pdrs-2981-pin-')
        tree, tree_runner, out, before, env = self._stage(base)
        process = subprocess.run(
            [sys.executable, '-I', '-B', str(base / 'harness.py'), mode,
             str(tree_runner), str(FIXTURES), str(tree), str(out), *names],
            capture_output=True, text=True, timeout=120, env=env)
        summary = None
        if (out / 'summary.json').is_file():
            summary = json.loads((out / 'summary.json').read_text(encoding='utf-8'))
        return process, summary, tree, before, out

    def run_runner_piped(self, mode, names=SELECTION, timeout=120):
        """Drive the runner with its stdout feeding a real ``head -1`` reader.

        The measured ``... | head -1`` delivery shape: the reader takes the
        first record() line and exits, so the run's next print hits a closed
        pipe instead of a captured buffer. Returns ``(process, head_stdout,
        stderr_text, tree, target_before, out)``; the runner's stdout belongs
        to the reader (nothing to capture on the process itself), its stderr
        is drained into ``stderr_text``, and the process is fully waited
        before returning. The parent never closes ``reader.stdin`` before
        ``communicate()``: CPython's ``Popen._communicate`` flushes that stream
        first, and on a closed file 3.9-3.12 raise ``ValueError`` (only 3.13+
        swallow it — the CI's ubuntu-24.04 python is 3.12, so the close would
        ERROR the case there). ``communicate()`` itself flushes and closes the
        empty-input stdin, same semantics on every version.

        fix-r4（MM concurrency#1）: both legs of the failure path are bounded.
        The old ``process.stderr.read()`` after the reader's communicate had
        no timeout — stderr only EOFs when the runner exits, so a runner
        stuck after its first line hung the whole module until the CI step's
        ``timeout-minutes`` killed all eight modules' verdicts together, and
        a timeout on either leg leaked the runner itself. The runner's stderr
        is now drained through ``process.communicate(timeout)`` (its stdout
        already belongs to the reader's pipe, so communicate does not touch
        it), and the finally group-kills and reaps both children (the
        ``_kill_holder`` shape; each runs in its own session, so the group
        kill cannot reach this process group). A TimeoutExpired is re-raised
        with the reaped runner attached as ``.process``, so a caller can
        prove the driver never returns with the runner still alive.

        fix-r5（MM concurrency#2 + 其姊妹 #1）：回收腿本身不再抛。旧的裸
        ``child.wait(timeout=30)`` 在 finally 里可能再抛一个
        TimeoutExpired，顶掉刚挂好 ``.process`` 的原始异常（调用者从此拿
        不到「runner 是否还活着」的载体）；``os.killpg`` 的 except 也只认
        ProcessLookupError，而组领导已被收尸时整个 OSError 家族都在失败
        窗内。TimeoutExpired 现在随异常带走三样东西：``.process``（fix-r4
        已有）、``.head_stdout``（reader 已交付的首行——「卡死发生在首行
        送达之后」从此是用例可断言的前提，不再是注释里的口头声明）、
        ``.reaped``（回收腿的逐子确认结果）。
        """
        base = self._new_base('pdrs-2981-pipe-')
        tree, tree_runner, out, before, env = self._stage(base)
        reader = subprocess.Popen(
            ['head', '-1'], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            text=True, start_new_session=True)
        process = subprocess.Popen(
            [sys.executable, '-I', '-B', str(base / 'harness.py'), mode,
             str(tree_runner), str(FIXTURES), str(tree), str(out), *names],
            stdout=reader.stdin, stderr=subprocess.PIPE, text=True, env=env,
            start_new_session=True)
        head_stdout = None
        try:
            head_stdout, _reader_stderr = reader.communicate(timeout=timeout)
            _process_stdout, stderr_text = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired as error:
            error.process = process
            error.head_stdout = head_stdout
            error.reaped = self._reap_piped_children(process, reader)
            raise
        finally:
            # fix-r5（MM concurrency#2）：回收腿不抛（见 _reap_piped_children
            # 的 docstring）；在 except 之外兜底其余异常形态与正常路径——
            # 幂等，重复调用不会二次杀已收尸的孩子。
            self._reap_piped_children(process, reader)
        return process, head_stdout, stderr_text, tree, before, out

    @staticmethod
    def _reap_piped_children(*children):
        """fix-r5（MM concurrency#2）：piped 驱动的回收腿，杀完、有界等、不抛。

        旧 finally 里裸 ``child.wait(timeout=30)`` 可能再抛一个
        TimeoutExpired 顶掉原始异常（调用者挂好的 ``.process`` 载体随之丢
        失）；``os.killpg`` 只吞 ProcessLookupError，而组领导已被收尸时
        ``getpgid``/``killpg`` 抛的是整个 OSError 家族（ProcessLookupError
        只是其一）。现在 killpg 放宽到 OSError、wait 超时静默吞掉，返回值
        逐子回答「确认已死」（挂到 TimeoutExpired.reaped 上）。幂等：
        poll() 已答的孩子跳过 kill，重复调用无副作用。
        """
        reaped = []
        for child in children:
            if child.poll() is None:
                try:
                    os.killpg(os.getpgid(child.pid), signal.SIGKILL)
                except OSError:
                    # ProcessLookupError 是其子类；组已消散不是错误。
                    pass
            try:
                child.wait(timeout=30)
            except subprocess.TimeoutExpired:
                pass
            reaped.append(child.poll() is not None)
        return reaped

    @staticmethod
    def counts_line(stdout):
        lines = [line for line in stdout.split('\n') if line.strip()]
        return json.loads(lines[-1]) if lines else None


class AbortAccountingTests(_RunnerDriver, unittest.TestCase):
    """Every abort lands in the same accounting: all INVALID, summary, rc 2."""

    def assert_rows(self, summary, reasons):
        self.assertIsNotNone(summary,
                             '#2981 an aborting run must still publish its summary')
        rows = summary['controls']
        self.assertTrue([row['mutation'] for row in rows] == list(SELECTION),
                        '#2981 every selected control must stay accounted for,'
                        ' got ' + repr([row['mutation'] for row in rows]))
        self.assertTrue([row['status'] for row in rows]
                        == ['INVALID'] * len(SELECTION),
                        '#2981 an aborting run must revoke every recorded'
                        ' kill, got '
                        + repr([row['status'] for row in rows]))
        self.assertTrue([row['reason'] for row in rows] == reasons,
                        '#2981 abort reasons must keep the revoked prefix and'
                        ' name the failure, got '
                        + repr([row['reason'] for row in rows]))
        self.assertTrue([row['compiled'] for row in rows]
                        == [False] * len(SELECTION),
                        '#2981 an aborting run must revoke compilation'
                        ' receipts too, got '
                        + repr([row['compiled'] for row in rows]))
        self.assertTrue(summary['counts']
                        == {'KILLED': 0, 'SURVIVED': 0, 'INVALID': len(SELECTION)},
                        '#2981 an aborting run must count every row INVALID,'
                        ' got ' + repr(summary['counts']))

    def test_healthy_selection_publishes_killed_rows(self):
        process, summary, tree, before, _out = self.run_runner('healthy')
        self.assertEqual(process.returncode, 0,
                         '#2981 a healthy selection must exit 0: '
                         f'{process.stderr!r}{process.stdout!r}')
        self.assertIsNotNone(summary,
                             '#2981 a healthy run must publish its summary')
        rows = summary['controls']
        self.assertTrue([row['status'] for row in rows]
                        == ['KILLED'] * len(SELECTION),
                        '#2981 the driving harness must really produce green'
                        ' rows, or every revoke assertion below is vacuous,'
                        ' got ' + repr([row['status'] for row in rows]))
        self.assertTrue([row['reason'] for row in rows]
                        == [HEALTHY_REASON] * len(SELECTION),
                        '#2981 a healthy kill carries the classify reason,'
                        ' got ' + repr([row['reason'] for row in rows]))
        self.assertTrue([row['compiled'] for row in rows]
                        == [True] * len(SELECTION),
                        '#2981 a healthy kill records its compilation'
                        ' receipt, got '
                        + repr([row['compiled'] for row in rows]))
        self.assertTrue(summary['counts']
                        == {'KILLED': len(SELECTION), 'SURVIVED': 0,
                            'INVALID': 0},
                        '#2981 a healthy run counts every row KILLED, got '
                        + repr(summary['counts']))
        self.assertTrue(self.counts_line(process.stdout)['counts']
                        == {'KILLED': len(SELECTION), 'SURVIVED': 0,
                            'INVALID': 0},
                        '#2981 the final stdout counts line must agree with'
                        ' the summary, got '
                        + repr(self.counts_line(process.stdout)))
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 a healthy run must restore its mutation target'
                        ' byte for byte')

    def test_busy_source_lock_records_every_row_invalid(self):
        base = Path(tempfile.mkdtemp(prefix='pdrs-2981-lock-'))
        _BASES.append(base)
        tree = base / 'tree'
        tree.mkdir(parents=True)
        ready = base / 'held'
        holder = subprocess.Popen(
            [sys.executable, '-I', '-B', '-c', LOCK_HOLDER_SOURCE,
             str(FIXTURES), str(tree), str(ready)],
            start_new_session=True)
        self.addCleanup(self._kill_holder, holder)
        deadline = time.monotonic() + 30
        while not ready.exists():
            self.assertIsNone(
                holder.poll(),
                '#2981 the lock holder died before holding the tree lock')
            self.assertLess(time.monotonic(), deadline,
                            '#2981 the lock holder never published its ready'
                            ' file')
            time.sleep(0.05)
        process, summary, tree_back, before, _out = self.run_runner(
            'busy-lock', base=base)
        self.assertEqual(
            process.returncode, 2,
            '#2981 a busy source lock must abort at rc 2, got '
            f'{process.returncode}: {process.stderr!r}{process.stdout!r}')
        busy_reason = (ABORT_PREFIX + 'TimeoutError: source lock busy: '
                      + str((tree / '.negative-source.lock').resolve()))
        # resolve()：runner 从已解析的 __file__ 自算 root（macOS 上 mkdtemp 给
        # /var/folders、resolve 后是 /private/var/folders，不解析即假红）
        self.assert_rows(summary, [busy_reason] * len(SELECTION))
        self.assertTrue(self.counts_line(process.stdout)['counts']['INVALID']
                        == len(SELECTION),
                        '#2981 the final stdout counts line must agree with'
                        ' the INVALID summary, got '
                        + repr(self.counts_line(process.stdout)))
        self.assertTrue((tree_back / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 an entry-phase abort never wrote the mutation'
                        ' target')

    @staticmethod
    def _kill_holder(holder):
        # Group kill, not a bare kill(): the holder runs in its own session
        # (the issue-2495 cleanup shape). A vanished group is not an error.
        if holder.poll() is None:
            try:
                os.killpg(os.getpgid(holder.pid), signal.SIGKILL)
            except ProcessLookupError:
                pass
        holder.wait(timeout=30)

    def test_mid_run_restore_failure_revokes_and_completes_accounting(self):
        # The #2981 core: a restore failure after a green row must revoke the
        # kill (keeping its diagnostic as the prefix, the #979 shape) and
        # still record the unrun control — never a green row over an
        # unverified tree.
        process, summary, tree, before, _out = self.run_runner(
            'mid-restore-fault')
        self.assertEqual(process.returncode, 2,
                         '#2981 a mid-run restore failure must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        failure = (ABORT_PREFIX + 'RuntimeError: #1145 restoration mismatch: '
                   + REPLAY_RELATIVE)
        self.assert_rows(summary, [HEALTHY_REASON + '; ' + failure, failure])
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 the guard must still restore the tree its'
                        ' runner failed to')

    def test_guard_exit_failure_revokes_every_completed_row(self):
        process, summary, tree, before, _out = self.run_runner(
            'guard-exit-fault')
        self.assertEqual(process.returncode, 2,
                         '#2981 a guard exit failure must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        failure = ABORT_PREFIX + 'RuntimeError: #2981 injected guard exit failure'
        self.assert_rows(summary, [HEALTHY_REASON + '; ' + failure,
                                   HEALTHY_REASON + '; ' + failure])
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 a guard-exit fault still restores before it'
                        ' raises')

    def test_entry_ledger_drift_revokes_every_row_on_the_body_phase(self):
        # The one abort whose `phase` is 'body': the guard opened, the loop
        # never ran, and the entry-side frozen-ledger proof raised
        # FrozenLedgerDrift. Only the isinstance term of the guard-failure
        # predicate can route it to rc 2 — dropping that term turns the same
        # run into the verdict path's rc 1 over revoked rows.
        process, summary, tree, before, _out = self.run_runner('ledger-drift')
        self.assertEqual(process.returncode, 2,
                         '#2981 a body-phase FrozenLedgerDrift must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        self.assert_rows(summary, [ENTRY_DRIFT, ENTRY_DRIFT])
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 a body-phase ledger drift never wrote the'
                        ' mutation target')

    def test_exit_ledger_drift_revokes_every_row_on_the_exit_phase(self):
        # fix-r2（MM boundary#2）：runner 自己的出场侧证明——最后一个控制
        # 完成后的 assert_frozen_ledgers_match_head()（phase = "exit"）——
        # 此前无用例：ledger-drift 只打断 entry 侧（相位 body，靠
        # isinstance 项路由），guard-exit-fault 的异常来自守卫清理而非
        # 这条证明。本例让真正的出场侧证明失败（相位化注入：全部 run_owned
        # 相位服务后，见 harness）。fix-r4（MM boundary#1）修正本注释的判别
        # 力方向：删掉那两行本用例转红（rc 0 ≠ 断言的 2），正是它的判别
        # 力所在。
        process, summary, tree, before, _out = self.run_runner(
            'exit-ledger-drift')
        self.assertEqual(process.returncode, 2,
                         '#2981 an exit-phase ledger drift must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        self.assert_rows(summary, [HEALTHY_REASON + '; ' + ENTRY_DRIFT,
                                   HEALTHY_REASON + '; ' + ENTRY_DRIFT])
        self.assertTrue(self.counts_line(process.stdout)['counts']['INVALID']
                        == len(SELECTION),
                        '#2981 the final stdout counts line must agree with'
                        ' the INVALID summary, got '
                        + repr(self.counts_line(process.stdout)))
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 an exit-phase ledger drift happens after every'
                        ' control restored its own target')

    def test_cancellation_voids_recorded_kills_and_unrun_rows(self):
        # KeyboardInterrupt is a BaseException the per-control ``except
        # Exception`` must not swallow: the accounting sees it as a
        # cancellation of the whole run.
        process, summary, tree, before, _out = self.run_runner('cancel')
        self.assertEqual(process.returncode, 2,
                         '#2981 a cancellation must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        cancel_reason = ABORT_PREFIX + 'KeyboardInterrupt: '
        self.assert_rows(summary,
                         [HEALTHY_REASON + '; ' + cancel_reason,
                          cancel_reason])
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 a cancelled run must still restore the tree')


class VerdictEdgeTests(_RunnerDriver, unittest.TestCase):
    """The verdict edges no abort can reach: non-KILLED rows exit 1.

    This runner's verdict path is ``return int(any(row != KILLED))`` — both
    SURVIVED and INVALID rows share rc 1, distinct from the abort path's
    rc 2. The exit-code contract follow-up is registered as #1866; this pins
    the current behavior so any change there is deliberate.
    """

    def test_survived_negative_exits_one_and_keeps_the_tree_restored(self):
        process, summary, tree, before, _out = self.run_runner('survived')
        self.assertEqual(process.returncode, 1,
                         '#2981 a surviving selection must exit 1, not 0 (a'
                         ' coordinator would read a red device as green)'
                         f': {process.stderr!r}{process.stdout!r}')
        self.assertIsNotNone(summary,
                             '#2981 a surviving run is not an abort: the'
                             ' summary must still be published')
        rows = summary['controls']
        self.assertTrue([row['status'] for row in rows]
                        == ['SURVIVED'] * len(SELECTION),
                        '#2981 a green negative run is SURVIVED, never'
                        ' KILLED or INVALID, got '
                        + repr([row['status'] for row in rows]))
        self.assertTrue([row['reason'] for row in rows]
                        == ['bound test still passed'] * len(SELECTION),
                        '#2981 a surviving row carries the classify reason,'
                        ' got ' + repr([row['reason'] for row in rows]))
        self.assertTrue(summary['counts']
                        == {'KILLED': 0, 'SURVIVED': len(SELECTION),
                            'INVALID': 0},
                        '#2981 a surviving run counts every row SURVIVED,'
                        ' got ' + repr(summary['counts']))
        self.assertTrue(self.counts_line(process.stdout)['counts']
                        == {'KILLED': 0, 'SURVIVED': len(SELECTION),
                            'INVALID': 0},
                        '#2981 the final stdout counts line must agree with'
                        ' the summary, got '
                        + repr(self.counts_line(process.stdout)))
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 a surviving run must still restore the tree')

    def test_invalid_negative_exits_one_on_the_verdict_path(self):
        # The negative run fails without the bound assertion (the
        # stale-anchor shape), so the rows are INVALID through the verdict
        # path — rc 1, the code this runner reserves for "the device ran and
        # produced a non-kill", never the abort path's rc 2.
        process, summary, tree, before, _out = self.run_runner(
            'invalid-verdict')
        self.assertEqual(process.returncode, 1,
                         '#2981 a verdict-path INVALID run must exit 1: '
                         f'{process.stderr!r}{process.stdout!r}')
        self.assertIsNotNone(summary,
                             '#2981 a verdict-path INVALID run is not an'
                             ' abort: the summary must still be published')
        rows = summary['controls']
        self.assertTrue([row['status'] for row in rows]
                        == ['INVALID'] * len(SELECTION),
                        '#2981 a negative run without the bound assertion is'
                        ' INVALID, never KILLED or SURVIVED, got '
                        + repr([row['status'] for row in rows]))
        self.assertTrue([row['reason'] for row in rows]
                        == ['missing bound runtime failure (build error,'
                            ' wrong panic, or missing test)'] * len(SELECTION),
                        '#2981 an INVALID verdict row carries the classify'
                        ' reason, got '
                        + repr([row['reason'] for row in rows]))
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 an INVALID-verdict run must still restore the'
                        ' tree')


class VerdictDeliveryTests(_RunnerDriver, unittest.TestCase):
    """The three output legs of a healthy run, pinned as they behave today.

    fix-r2（MM concurrency#2 + DS sibling#2 两家同撞，取高 P1）：每控
    record() 行、summary 落盘、最终 counts 行三条出口腿此前零仓内覆盖
    （close-order pin 自其 fix-r2/r3/r4 起三形齐备）。本 runner 三处都
    无自身出口处理——print 直写、publish_summary 裸调、异常一律逃出
    main 由解释器收尾；本类钉**现状**，出口契约的收口归 #1866，契约
    变更时本类随之改。
    """

    def test_closed_pipe_after_the_first_record_loses_the_verdict(self):
        # ``... | head -1`` 真子进程读端：第一条 record() 行被取走后
        # reader 退出，第二条 record() 的 print 抛 BrokenPipeError——
        # results.append 先于该 print，行已入账，异常落进 except
        # BaseException 的中止记账：撤销成全 INVALID（诊断作前缀）、
        # summary 照常发布；随后 counts 行自己的 BrokenPipeError 从
        # main 逃出，rc 由解释器自选（本机 3.14 实测 120——关闭管道上的
        # 终局 flush 失败）。钉现状：非零 rc + 无 counts 行送达 +
        # summary 为中止记账形态；契约收口归 #1866。
        process, head_stdout, stderr_text, tree, before, out = (
            self.run_runner_piped('stdout-pipe'))
        self.assertNotEqual(process.returncode, 0,
                            '#2981 a run whose stdout pipe closes mid-run'
                            ' must not exit 0 (a coordinator would read a'
                            ' half-delivered run as a clean verdict): rc='
                            f'{process.returncode}, stderr={stderr_text!r}')
        self.assertIn('BrokenPipeError', stderr_text,
                      '#2981 the pipe error must escape the runner by name,'
                      ' not be swallowed by the abort accounting')
        self.assertIn(': KILLED: ', head_stdout,
                      '#2981 the reader must really have taken the first'
                      ' record line, or the pipe fault fired too early'
                      ' (before any verdict existed) and pins nothing')
        self.assertNotIn('"counts"', head_stdout,
                         '#2981 the counts line must never reach the'
                         ' reader: the pipe closed at the second record'
                         ' line')
        summary = None
        if (out / 'summary.json').is_file():
            summary = json.loads((out / 'summary.json').read_text(
                encoding='utf-8'))
        self.assertIsNotNone(
            summary,
            '#2981 the abort accounting runs before the escaping counts'
            ' print: the summary must still be published')
        rows = summary['controls']
        self.assertTrue([row['mutation'] for row in rows] == list(SELECTION),
                        '#2981 every selected control must stay accounted'
                        ' for, got ' + repr([row['mutation'] for row in rows]))
        self.assertTrue([row['status'] for row in rows]
                        == ['INVALID'] * len(SELECTION),
                        '#2981 the pipe fault lands in the abort accounting:'
                        ' every recorded kill must be revoked, got '
                        + repr([row['status'] for row in rows]))
        pipe_failure = ABORT_PREFIX + 'BrokenPipeError: [Errno 32] Broken pipe'
        self.assertTrue([row['reason'] for row in rows]
                        == [HEALTHY_REASON + '; ' + pipe_failure]
                        * len(SELECTION),
                        '#2981 the revoked rows keep their diagnostics as'
                        ' prefixes and name the pipe error, got '
                        + repr([row['reason'] for row in rows]))
        self.assertTrue(summary['counts']
                        == {'KILLED': 0, 'SURVIVED': 0,
                            'INVALID': len(SELECTION)},
                        '#2981 the pipe-fault run counts every row INVALID,'
                        ' got ' + repr(summary['counts']))
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 a pipe-fault run must still restore the tree')

    def test_counts_line_write_failure_keeps_the_summary_on_disk(self):
        # 最终 counts 行写失败——/dev/full 实测形态 OSError(ENOSPC)，非
        # 管道错误。publish 在 counts 打印之前（negative_controls.py 的
        # publish_summary 先于最终 print），所以判决已在盘上、只有 stdout
        # 送达失败：钉现状——rc 1（未处理异常）+ stderr traceback 点名
        # OSError + summary 完整保留 KILLED 行。
        process, summary, tree, before, _out = self.run_runner(
            'stdout-write-fault')
        self.assertEqual(process.returncode, 1,
                         '#2981 an escaped stdout write failure is an'
                         ' unhandled exception: the interpreter exits 1,'
                         f' got {process.returncode}: {process.stderr!r}'
                         f'{process.stdout!r}')
        self.assertIn('OSError: [Errno 28] No space left on device',
                      process.stderr,
                      '#2981 the escaped write failure must name itself on'
                      ' stderr (a silent nonzero rc is unauditable)')
        self.assertIsNotNone(summary,
                             '#2981 only the stdout line failed: the'
                             ' summary must still be published')
        rows = summary['controls']
        self.assertTrue([row['status'] for row in rows]
                        == ['KILLED'] * len(SELECTION),
                        '#2981 the rows completed and were recorded: only'
                        ' the stdout delivery failed, got '
                        + repr([row['status'] for row in rows]))
        self.assertTrue([row['reason'] for row in rows]
                        == [HEALTHY_REASON] * len(SELECTION),
                        '#2981 a write-fault kill keeps its classify reason,'
                        ' got ' + repr([row['reason'] for row in rows]))
        self.assertNotIn('"counts"', process.stdout,
                         '#2981 the counts line never reached the reader:'
                         ' the write failed at it')
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 a write-fault run must still restore the tree')

    def test_summary_publish_failure_leaves_no_summary(self):
        # summary 发布失败（publish 的 temp rename 注入，close-order pin
        # fix-r2 的 Path.replace 形态）：行已跑完并打到 stdout，publish
        # 失败后 counts 打印到不了——钉现状：rc 1 + 无 summary.json +
        # 无残留 temp + stdout 仍带完整 KILLED 行（rc 只可能来自发布
        # 分支，绝不是中止）。
        process, summary, tree, before, out = self.run_runner(
            'summary-publish-fault')
        self.assertEqual(process.returncode, 1,
                         '#2981 an escaped publish failure is an unhandled'
                         ' exception: the interpreter exits 1, got '
                         f'{process.returncode}: {process.stderr!r}'
                         f'{process.stdout!r}')
        self.assertIn(': KILLED: ', process.stdout,
                      '#2981 the publish-fault run must have completed its'
                      ' rows: only the summary rename fails, so the rc can'
                      ' only come from the publish branch, never an abort')
        self.assertNotIn('"counts"', process.stdout,
                         '#2981 the counts print never ran past the failed'
                          ' publish')
        self.assertIn('#2981 injected summary publish failure',
                      process.stderr,
                      '#2981 the escaped publish failure must name itself'
                      ' on stderr')
        self.assertIsNone(summary,
                           '#2981 an unpublished summary must leave no'
                           ' summary.json behind')
        self.assertEqual(list(out.glob('summary.json*')), [],
                         '#2981 a failed publish must not leave a summary'
                         ' file a later reader could mistake for evidence')
        self.assertEqual(list(out.glob('.summary.json.*')), [],
                         '#2981 a failed publish must drop its temp file')
        self.assertTrue((tree / REPLAY_RELATIVE).read_bytes() == before,
                        '#2981 a publish-fault run must still restore the'
                        ' tree')

    def test_a_hung_runner_is_bounded_and_reaped(self):
        # fix-r4（MM concurrency#1）：``run_runner_piped`` 的失败路径此前有
        # 两条无界腿——reader 侧 communicate 超时后，runner stderr 的裸
        # ``read()`` 无超时（stderr 到子进程退出才 EOF），且超时路径不回收
        # runner：写完首行后卡死的 runner 让整个模块永不返回，只能靠 CI 步
        # 的 timeout-minutes 整体杀掉，八个模块的结论一起丢。本例钉修复后
        # 的两条性质：有限时间内以 TimeoutExpired 返回（墙钟下界），且驱动
        # 返回时 runner 必须已死（不回收的孤儿持着临时树与管道写端）。
        started = time.monotonic()
        timeout = 5
        with self.assertRaises(subprocess.TimeoutExpired) as caught:
            self.run_runner_piped('hang', timeout=timeout)
        # fix-r5（MM concurrency#2 的姊妹 #7，DS simplify#2）：墙钟上界挂
        # 钩驱动超时（×4 留调度余量），不再与 harness 自释放常量
        # ORPHAN_RELEASE_S 巧合同值——上界必须严格小于自释放，本用例才能
        # 区分「驱动的超时腿先到」与「孤儿自己醒了」。
        self.assertLess(time.monotonic() - started, timeout * 4,
                        '#2981 the piped driver must bound a hung runner '
                        'instead of waiting for the CI step to kill the '
                        'whole module')
        error = caught.exception
        # fix-r5（MM concurrency#1）：「reader 已交付首行后才卡死」是用例
        # 前提的一部分，不再只是 harness 注释里的口头声明（照姊妹用例
        # :891 的 assertIn 形态）——否则 runner 丢 flush 时 reader 腿超时
        # 照样全绿，而无界 stderr 腿从未被走到。
        self.assertIn(': KILLED: ', error.head_stdout,
                      '#2981 the reader must really have taken the first '
                      'record line before the hang: the pinned fault is '
                      '"hung after its first delivered line", not "never '
                      'delivered anything"')
        process = error.process
        self.assertIsNotNone(process.poll(),
                             '#2981 the hung runner must not outlive the '
                             'piped driver: a leaked child holds the '
                             'temporary tree and the pipe write end')
        # fix-r5（MM concurrency#2）：回收腿不抛、结果挂回异常——两个子
        # 进程（runner 与 reader）都必须确认已死。
        self.assertEqual(error.reaped, [True, True],
                          '#2981 the reap leg must confirm both children '
                          'dead without throwing: a raising wait() would '
                          'replace the TimeoutExpired the caller inspects')


class PreflightLedgerDriftTests(_RunnerDriver, unittest.TestCase):
    """check_catalog's own FrozenLedgerDrift route: rc 2, no evidence at all."""

    def test_preflight_ledger_drift_exits_two_without_evidence(self):
        process, _summary, _tree, _before, out = self.run_runner(
            'preflight-drift')
        self.assertEqual(process.returncode, 2,
                         '#2981 a preflight ledger drift must exit 2, got '
                         f'{process.returncode}: {process.stderr!r}'
                         f'{process.stdout!r}')
        self.assertIn(ENTRY_DRIFT, process.stderr,
                      '#2981 the preflight failure must name itself on'
                      ' stderr before returning 2')
        self.assertFalse(out.exists(),
                         '#2981 a preflight guard failure must not create an'
                         ' evidence directory a later reader could mistake'
                         ' for a run')


class PreGuardRefusalTests(unittest.TestCase):
    """The other side of the exit contract: pre-guard refusals exit 1."""

    def refusal(self, argv, env):
        base = Path(tempfile.mkdtemp(prefix='pdrs-2981-refusal-'))
        _BASES.append(base)
        out = base / 'out'
        result = subprocess.run(
            [sys.executable, '-I', '-B', str(RUNNER_SCRIPT), str(out), *argv],
            capture_output=True, text=True, timeout=60, env=env)
        return result, out

    def test_unknown_selection_exits_one_without_a_summary(self):
        result, out = self.refusal(
            ['no-such-control-2981'],
            {**os.environ, 'CARGO_TARGET_DIR': '/nonexistent-cargo-target'})
        self.assertEqual(result.returncode, 1,
                         f'#2981 an unknown selection is a pre-guard refusal'
                         f' (rc 1): {result.stderr!r}')
        self.assertIn('#1145 unknown or duplicate mutation selection',
                      result.stderr)
        self.assertFalse(out.exists(),
                         '#2981 a pre-guard refusal must not create evidence')

    def test_missing_cargo_target_exits_one_without_a_summary(self):
        result, out = self.refusal(
            [SELECTION[0]],
            {key: value for key, value in os.environ.items()
             if key != 'CARGO_TARGET_DIR'})
        self.assertEqual(result.returncode, 1,
                         f'#2981 a missing CARGO_TARGET_DIR is a pre-guard'
                         f' refusal (rc 1): {result.stderr!r}')
        self.assertIn('#1145 CARGO_TARGET_DIR must be supplied', result.stderr)
        self.assertFalse(out.exists())

    def test_missing_baseline_exits_one_without_a_summary(self):
        env = {key: value for key, value in os.environ.items()
               if key not in ('CARGO_TARGET_DIR', 'PDRS_BASELINE')}
        env['CARGO_TARGET_DIR'] = '/nonexistent-cargo-target'
        result, out = self.refusal([SELECTION[0]], env)
        self.assertEqual(result.returncode, 1,
                         f'#2981 a missing PDRS_BASELINE is a pre-guard'
                         f' refusal (rc 1): {result.stderr!r}')
        self.assertIn('#1145 PDRS_BASELINE must identify the real TS checkout',
                      result.stderr)
        self.assertFalse(out.exists())


def tearDownModule():
    for base in _BASES:
        shutil.rmtree(base, ignore_errors=True)


if __name__ == '__main__':
    unittest.main()

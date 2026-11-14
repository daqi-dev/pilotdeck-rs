#!/usr/bin/env python3
# anchor-selfcheck: unsupported #1800 close-order pin abort-accounting meta-control on a temporary tree, no product mutation table
"""#1800/#3043: the close-order pin runner's hand-written abort accounting, pinned.

`sdk-signals-close-order-pin/negative_controls.py` is a self-managed loop (it
never calls ``negative_suite.run_cases``), so its ``except BaseException``
branch — revoke every recorded KILLED, record every unrun control as INVALID,
still publish the summary, exit 2 (#1508 / #1795) — is hand-written, and until
this file it had no in-repo test or meta-control: a regression would surface
only as a false PASS with green rows over an unverified tree.

Every exit of that runner is ``os._exit``, so it cannot be exec'd in-process
the way the issue-2495 meta-controls drive their scripts; each case here
instead spawns a real subprocess whose ``__file__`` points at an identical
path inside a temporary tree, so the runner's self-computed ``root``, catalog
and frozen ledgers all land in the temporary copy (the catalog file is a byte
copy of the real one, keeping every mutation anchor present exactly once).
The shared ``negative_suite.invalidate_completed`` is never mocked — half of
the pinned behavior is that the abort accounting routes through the shared
revocation — and only ``negative_process`` boundaries are patched in memory,
exactly like the issue-2495 / SessionRunnerTests precedents.

#3043 simplify#1: the driver's single ``mode`` name used to squeeze the
verdict/abort shape and the stdout-reader shape into one axis, and the pipe
variants had to be enumerated in two non-adjacent tuples — adding a grid
cell meant editing both, and missing one stayed green or went red on the
wrong cell. The driver now takes two orthogonal argv axes instead: ``flow``
(what the run does) and ``reader`` (how stdout behaves); every case below is
one combination, and a new grid cell is a new combination, not a new name.

- (killed, plain): the guard is a snapshot/restore stub and ``run_owned``
  answers the check phases from a queue (positive passes, each control's
  negative fails at the dedicated #1382 marker plus its own criterion, the
  restored run passes). The selection must come out all KILLED at rc 0 — the
  positive control that makes every revoke assertion below non-vacuous;
- (busy-lock, plain): a real subprocess holds ``<tree>/.negative-source.lock``
  through the real ``tree_lock``, the runner runs the real ``mutating_sources``
  with its wait shortened 60 s -> 2 s (the issue-2495 GUARD_LOCK_FAST shape),
  and the lock timeout must land in the abort accounting: every selected row
  INVALID with ``run aborted: TimeoutError: source lock busy``, rc 2;
- (mid-restore, plain): the runner's own restore read-back fails on the last
  control (its ``write_bytes`` restore is skipped, the mutant stays on disk
  and the ``#1382 restore failed`` check fires) — the already recorded row
  must be revoked with its prior diagnostic kept as a prefix, the unrun row
  recorded INVALID, the guard still restores the catalog, rc 2;
- (guard-exit, plain): the stub guard raises after the body completed — the
  revoke-everything half of the same accounting (#979);
- (cancel, plain): ``run_owned`` raises KeyboardInterrupt during the last
  control's negative run — the deterministic shape of a Ctrl-C, which is a
  BaseException the per-control ``except Exception`` must not swallow;
- (survived, plain): ``run_owned`` answers every phase green — the verdict
  edge none of the abort modes can reach: the mutated catalog's preflight
  still passes, so every row is SURVIVED and the run exits 1, the code the
  contract reserves for "the device ran and a control survived". Routing
  that path to rc 0 (a coordinator reads a red device as green) or to rc 2
  (it reads "the device never ran") must both land here as a red test;
- (invalid, plain): the negative run fails *without* the dedicated #1382
  marker — the shape a stale anchor produces on real hardware — so the row
  is INVALID through the verdict path, not the abort path. Every INVALID
  row the abort modes above produce comes from the ``aborted`` branch, and
  the runner checks that branch *before* ``if invalid: os._exit(2)``, so
  the two exit-2 routes are different code: routing the verdict one to
  rc 0 (a coordinator reads "the device never produced a verdict" as all
  green) or to rc 1 (it reads it as "a control survived") must both land
  here as a red test;
- (summary-publish, plain): a healthy all-KILLED run whose summary rename
  fails — no summary may survive a failed publish (a stale or half-written
  one would read as evidence), and rc 2 must say the device produced no
  verdict.
- (killed, full): a healthy all-KILLED run whose final counts line cannot
  be written — the write fails with the measured /dev/full shape (OSError
  ENOSPC, not a pipe error), so it must not be swallowed as ``... | head``:
  rc 2 with a stderr diagnostic naming the failure, while the summary on
  disk still carries the completed verdict rows.
- (killed, pipe): the mirror of the write fault — the final counts line
  fails with a BrokenPipeError, the measured shape of ``... | head``
  closing the pipe. That one the runner must swallow: the exit code
  routes the reader, not stdout, so the healthy verdict code (rc 0) must
  survive and no "#1382 stdout summary write failed" diagnostic may
  appear — pinning that the pipe branch stays ahead of the OSError
  branch instead of both folding into one refusal.
- (invalid, pipe): the third leg of that pairing — a verdict-path INVALID
  run (no abort, the summary carries the INVALID rows) read through
  ``... | head``. Swallowing the pipe must not swallow the verdict: the
  run still exits 2 with the "#1382 INVALID controls" diagnostic, so a
  folded-in ``os._exit(0)`` on the pipe branch cannot pass a no-verdict
  run off as green.
- (survived, pipe): the last fake-reader cell of the same 3-state-by-2
  grid — a SURVIVED run (every phase green, rc 1 is the only signal a
  red device gives through ``... | head``) with the counts line hitting a
  closed pipe. Swallowing the pipe must not collapse the tri-state: the
  run still exits 1, so a regression folding the verdict codes to
  ``os._exit(2 if invalid else 0)`` (dropping the SURVIVED code, a red
  device read as rc 0 = all green) cannot pass a pipe-read run off.
- (cancel, head): #3043 concurrency#1 + boundary#1 — the fourth cell of
  the state-by-reader grid, and the one no text-injection fake can
  express: a real ``head -1`` subprocess reads the runner's stdout
  through a real OS pipe, so the EPIPE lands on a ``record()`` line (the
  fakes above can only break the final counts line — a record line never
  contains '"counts"'). On an aborting run that means the abort
  accounting's own record print raises BrokenPipeError inside the
  ``except BaseException`` handler, whose contract (#1508 fix-r2) is to
  fail closed: stderr diagnostic, rc 2, and no summary at all. Until
  #3043 this outcome had no pin anywhere in the repo.
- the roster precondition of that same cell (#3043 fix-r2, pinned by
  fix-r3): the runner only records at the end of each loop pass, so a
  single selected control gives a head reader nothing to read before the
  cancel point — the harness refuses such an invocation at its entrance
  with the reserved rc 96, a ``[harness-roster]`` stderr diagnostic, no
  summary, and the tree untouched. #3043 fix-r4 (MM boundary#2): the
  precondition belongs to that cell alone — it now refuses only when
  ``flow == 'cancel' and reader == 'head'`` — and both narrowing
  directions are pinned by width cases: a single-control (killed, head)
  run is a healthy run that must reach its rc 0 verdict (dropping the
  flow condition reds it), and a single-control (cancel, plain) run is an
  ordinary abort at rc 2 (dropping the reader condition reds it).
- #3043 fix-r4 (MM boundary#1): the two axes are closed enums. An unknown
  flow or reader value used to fall silently into the default healthy
  branch — a typo like ``hed`` was indistinguishable from ``killed`` —
  so the harness entrance now refuses it with the #1382 sibling shape
  (``[harness-axis] #1382 unknown or duplicate selection``, rc 1, no
  summary, tree untouched), pinned in both directions.

Both senses of exit 1 are pinned: the verdict path's 1 above, and the
pre-guard refusals' 1 (an unknown selection, a missing CARGO_TARGET_DIR)
with no summary — those run the real, unmodified script directly, because
nothing needs patching before the guard. Run from this directory:
``python3 -I sdk-signals-close-order-pin/test_negative_controls.py``.
"""
import contextlib
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
PIN_SCRIPT = FIXTURES / 'sdk-signals-close-order-pin' / 'negative_controls.py'
CATALOG_SCRIPT = FIXTURES / 'sdk-signals' / 'negative_controls.py'

# fix-r9 (MM sibling P1): make the fixtures importable regardless of the
# working directory the suite is started from (CI runs it from ``fixtures/``
# with ``-m unittest``, where the directory is already on ``sys.path``).
if str(FIXTURES) not in sys.path:
    sys.path.insert(0, str(FIXTURES))
from negative_process import tree_lock

# The tree the guarded sources live in: the same ``<root>/.negative-source.lock``
# a real ``mutating_sources`` round flocks. The pin runner derives its own
# ROOT from ``Path(__file__).resolve().parents[5]`` at the same fixture
# nesting this file sits at, so the same derivation names the same lock
# (the roster_guards twin, one level shallower at ``fixtures/`` itself, uses
# ``parents[4]`` for the same root).
REPO_ROOT = Path(__file__).resolve().parents[5]
# fix-r10 (P2#4, DS simplify#1 ≡ MM simplify#1): the depth constant is
# self-proved against FIXTURES. A relocated or flattened fixture tree would
# otherwise derive some other *existing* directory, and ``tree_lock``
# (``os.open(..., O_CREAT)``) would silently create a stranger's lock file
# there -- the window would stay green while locking nothing at all.
assert (REPO_ROOT / 'src/pilotdeck-gateway/tests/fixtures').resolve() == FIXTURES


@contextlib.contextmanager
def guarded_source_window(*, timeout=2):
    """Read real guarded sources only inside the tree lock (fix-r9).

    ``PIN_SCRIPT`` is the ``close-order-reader-failclosed`` control's own
    mutation target and ``CATALOG_SCRIPT`` is this fixture's catalog -- both
    carry a mutant on disk for the whole bound-test window of a sibling
    round. Reading them unlocked can stage mutant bytes as pristine and red
    every downstream anchor expectation (TreeLock: whoever owns the lock
    reads the pristine bytes *after* acquisition). The wait budget is
    explicit and short (the roster_guards ``fast_tree_lock`` precedent): a
    holder outliving it fails closed with a holder-naming diagnostic instead
    of TreeLock's silent 900 s default outlasting the CI step's own timeout.

    fix-r10 (P1, four-way review: DS sibling#1/concurrency#1, MM
    sibling#1/boundary#1): the close-order-reader round runs this file's
    bound test as a subprocess *inside* its own ``mutating_sources``
    window, so the descendant's lock can only ever wait on its own
    ancestor -- no amount of waiting resolves (the Rust read-lease
    precedent, ``tests/common/read_lease.rs:61-96``). The ancestor is also
    the byte writer for the very sources this window guards, which is
    exactly what the bypass asserts (the ``sdk-signals:650`` export shape):
    the mutation leg must read the parent-published mutant off the disk,
    and the pristine/restored legs read the restored pristine -- reading
    straight through yields the correct bytes on all three legs.
    """
    if os.environ.get('PDRS_NEGATIVE_LEASE_HELD_ROOT') == str(REPO_ROOT):
        yield
        return
    try:
        with tree_lock(REPO_ROOT, timeout=timeout):
            yield
    except TimeoutError as error:
        raise AssertionError(
            '#3043 the guarded source was busy: a sibling negative-control '
            f'round holds {REPO_ROOT}/.negative-source.lock for over '
            f'{timeout} s and mutant bytes would be read as pristine; '
            f'refusing: {error}') from error


# fix-r10 (P2#1, DS sibling#2 ≡ MM concurrency#2, 双撞): the pin runner's
# fixture import closure -- its own ``from negative_selection import …`` /
# ``from negative_suite import …`` plus the two modules those pull in
# transitively (negative_suite imports pending_reanchor and negative_panic;
# negative_panic imports negative_scope). A staged runner that resolves any
# of these from the *real* fixtures directory reads them unlocked, and
# ``negative_suite.py`` is the shared-runner round's own mutation target --
# the import channel leaked exactly the TOCTOU the staging exists to close.
# ``negative_process.py`` is deliberately absent from the harness-flow set:
# the harness imports the real module first to patch its guard boundaries
# (the runner's from-import then hits ``sys.modules``), while the direct-run
# flows below stage it alongside.
PIN_RUNNER_SHARED_SOURCES = (
    'negative_suite.py',
    'negative_selection.py',
    'pending_reanchor.py',
    'negative_panic.py',
    'negative_scope.py',
)
DIRECT_RUNNER_SHARED_SOURCES = ('negative_process.py',) + PIN_RUNNER_SHARED_SOURCES


def stage_shared_sources(fixtures_root, names):
    """Copy shared fixture modules into a temp fixtures tree (fix-r10 P2#1).

    Only ever called inside ``guarded_source_window()``: the real bytes are
    read while the repo tree lock is held, exactly like the staged runner and
    catalog themselves, so a sibling round's on-disk mutant cannot travel.
    """
    for name in names:
        (fixtures_root / name).write_bytes((FIXTURES / name).read_bytes())

# The first per-ID row and the first boundary row of the registered table: one
# of each criterion family, in table order.
SELECTION = ('unordered-close-row-removed', 'unordered-close-boundary-dropped')
PIN_MARKER = '#1382 unordered-close rows must bind their own ID'
HEALTHY_REASON = 'preflight failed at its #1382 pin'

# The subprocess driver. It execs the real runner source with ``__file__``
# inside the temporary tree and patches only the negative_process boundaries
# the busy-lock case must keep real. ``flow`` (argv[1]) and ``reader``
# (argv[2]) are the two orthogonal axes of the grid (#3043 simplify#1): the
# flow names the run's verdict/abort shape, the reader names how stdout
# behaves, and every case in this file is one combination — never a mode
# name that two distant tuples both have to learn before a new cell exists.
HARNESS_SOURCE = """\
import contextlib
import os
import select
import subprocess
import sys
from pathlib import Path

flow = sys.argv[1]
reader = sys.argv[2]
pin_path = Path(sys.argv[3])
real_fixtures = Path(sys.argv[4])
tree = Path(sys.argv[5]).resolve()
out = Path(sys.argv[6])
names = sys.argv[7:]

# #3043 fix-r4（a2，MM boundary#1）：两根轴是闭合枚举。拼错的值此前一律
# 静默落进默认分支的健康路径——'hed' 与 'killed' 在 (hed, plain) 上完全
# 同形（rc 0 全绿），调用方拿不到任何「这不是一个格」的信号。入口照
# #1382 先例拒（同族「unknown or duplicate selection」文本与 rc 1，不自
# 造新码）。
FLOWS = ('killed', 'busy-lock', 'mid-restore', 'guard-exit', 'cancel',
         'survived', 'invalid', 'summary-publish')
READERS = ('plain', 'full', 'pipe', 'head')
if flow not in FLOWS or reader not in READERS:
    sys.exit('[harness-axis] #1382 unknown or duplicate selection: '
             f'flow {flow!r} x reader {reader!r} is not a cell of the '
             'driver grid; flows: ' + ', '.join(FLOWS)
             + '; readers: ' + ', '.join(READERS))

# #3043 fix-r2（concurrency#1）→ fix-r4（a3，MM boundary#2）：名册前提钉在
# harness 入口，且只属 cancel×head 一格——离开屏障只在取消点存在。该格
# 依赖「至少 2 个控制」：runner 只在每轮循环末 record()，单控制时第一条
# record 行就是中止记账自己的那条，head 读者在取消点之前收不到任何行、
# 永不离开——屏障只能等满 5 s 后以误导性的 ``unexpected poll result: []``
# 退出（改前实测：rc 97 / 5.045 s），把调用方错误伪装成平台故障。入口
# 直接以可识别诊断拒绝（96 与屏障自身的 97 区分）。fix-r4 前该守卫对全
# 网格无条件生效而立论只属此格：健康的 killed×head 单控制运行与根本没有
# 离开屏障的 plain 读者被同一条 96 拒掉（MM 实测）——现收窄回立论所属
# 的格，两个收窄方向各由一条宽度用例钉住。
if flow == 'cancel' and reader == 'head' and len(names) < 2:
    print('[harness-roster] the grid driver needs at least 2 selected '
          f'controls, got {len(names)}: the runner only records at the end '
          'of each loop pass, so a single control gives a head reader '
          'nothing to read before the cancel point',
          file=sys.stderr, flush=True)
    os._exit(96)

sys.path.insert(0, str(real_fixtures))
import negative_process

runner_path = tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals-close-order-pin/negative_controls.py'
# fix-r9 (MM sibling P1): pin_path is a *staged* copy the parent read inside
# the tree lock -- a live holder of <repo>/.negative-source.lock (the
# close-order-reader control mutating the real driver) cannot leak mutant
# bytes into this read, so no lock is needed here.
source = pin_path.read_text(encoding='utf-8')

head_pipe_write = None
if reader == 'head':
    # #3043 (concurrency#1 + boundary#1): the fourth grid cell needs a REAL
    # pipe fault — a real ``head -1`` subprocess reading a real OS pipe —
    # because the shape it pins is the EPIPE landing on a ``record()`` line,
    # which the counts-line fakes below structurally cannot express (a
    # record line never contains '"counts"'). The harness's own stdout is
    # moved onto the pipe's write end so every runner print reaches the real
    # reader; head inherits the original stdout (the test's capture pipe)
    # and echoes back exactly the one line it reads before exiting. The
    # write fd is kept for the departure wait in fake_run_owned.
    read_fd, head_pipe_write = os.pipe()
    subprocess.Popen(['head', '-1'], stdin=read_fd)
    os.close(read_fd)
    os.dup2(head_pipe_write, 1)

if flow == 'busy-lock':
    # Real guard, real flock timeout: only the wait bound is shortened (the
    # issue-2495 GUARD_LOCK_FAST shape). The timeout fires before any git
    # work, so the temporary tree needs no repository.
    old = 'LOCK_TIMEOUT_SECONDS = 60'
    assert source.count(old) == 1, 'stale LOCK_TIMEOUT anchor'
    source = source.replace(old, 'LOCK_TIMEOUT_SECONDS = 2', 1)
else:
    catalog = tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals/negative_controls.py'
    runner_globals = {'__file__': str(runner_path), '__name__': '__main__'}
    exit_fault = flow == 'guard-exit'

    @contextlib.contextmanager
    def fake_guard(actual_root, requested, **_kwargs):
        snapshots = {Path(path): Path(path).read_bytes() for path in requested}
        try:
            yield snapshots
        finally:
            for path, data in snapshots.items():
                negative_process.write_source_atomic(actual_root, path, data)
            if exit_fault:
                raise RuntimeError('#1800 injected guard exit failure')

    calls = {'count': 0}

    def fake_run_owned(command, **_kwargs):
        # The check command carries no phase label, so the phases are served
        # from the runner's own deterministic order: positive-before, then
        # negative/restored per selected control (the SessionRunnerTests
        # stage-queue shape).
        index = calls['count']
        calls['count'] += 1
        if flow == 'survived':
            # The verdict edge no abort mode can reach: every phase green, so
            # the negative run's preflight still passes and the row survives.
            return subprocess.CompletedProcess(
                command, 0, 'product anchors: PASS\\n', '')
        if index == 0:
            return subprocess.CompletedProcess(
                command, 0, 'product anchors: PASS\\n', '')
        control = (index - 1) // 2
        if (index - 1) % 2 == 0:
            if flow == 'cancel' and control == len(names) - 1:
                if reader == 'head':
                    # #3043 fix-r1（concurrency P2）：等待必须真等出结果。
                    # darwin 实测（fix-r1/probe-poll-revents-v3.log）：真
                    # ``head -1`` 读完一行退出后 ~1–2 ms，写端 poll 报
                    # POLLHUP（0x10）——但只在该位被注册进掩码时才报；只注
                    # 册 POLLERR 则永远等满超时且返回空表（POSIX「ERR/HUP/
                    # NVAL 无视注册无条件报」在 darwin 管道上不成立，探针
                    # A/B/C 三变体钉死）。旧代码正因此死等 30 s 后把返回值
                    # 整个丢弃：屏障失效无人看见。现改为：注册
                    # ERR|HUP|NVAL，断言 revents 命中任一位，否则 stderr 落
                    # 可识别诊断并以 97 退出（用例红在屏障本身，不再静默
                    # 放行）；超时 5000 ms 对实测 ~2 ms 的一行读走余量
                    # 2500×。
                    departure = select.poll()
                    departure.register(head_pipe_write, select.POLLERR
                                       | select.POLLHUP
                                       | select.POLLNVAL)
                    ready = departure.poll(5000)
                    if (not ready
                            or not ready[0][1] & (select.POLLERR
                                                  | select.POLLHUP
                                                  | select.POLLNVAL)):
                        print('[reader-departure-barrier] unexpected '
                              f'poll result: {ready!r}',
                              file=sys.stderr, flush=True)
                        os._exit(97)
                raise KeyboardInterrupt()
            if flow == 'invalid':
                # The verdict path's INVALID row: the negative run fails
                # without any dedicated #1382 marker (the stale-anchor shape),
                # so the runner's else branch records INVALID and the exit
                # must come from the verdict path, never the abort branch.
                return subprocess.CompletedProcess(
                    command, 1, 'preflight failed elsewhere\\n', '')
            criterion = runner_globals['mutations'][control][4]
            return subprocess.CompletedProcess(
                command, 1, __PIN_MARKER__ + '\\n' + criterion + '\\n', '')
        return subprocess.CompletedProcess(
            command, 0, 'product anchors: PASS\\n', '')

    def fake_head_bytes(actual_root, relative, _baseline):
        return (Path(actual_root) / relative).read_bytes()

    negative_process.mutating_sources = fake_guard
    negative_process.run_owned = fake_run_owned
    negative_process._head_bytes = fake_head_bytes

    if flow == 'mid-restore':
        # The runner's own restore check: skip the last control's write-back
        # so the mutant stays on disk and the read-back comparison raises.
        # fix-r8 (sibling#1): the runner's publish/restore moved onto
        # write_source_atomic, so the skip hook follows it there. Only the
        # restore is shaped -- it rewrites the pristine snapshot bytes,
        # while the publish writes the mutant -- and the guard's own exit
        # restore (the (len(names)+1)-th pristine write) must still go
        # through, or the tree the runner failed to restore would stay
        # dirty and the case's byte-for-byte assertion below would red.
        raw_write_atomic = negative_process.write_source_atomic
        pristine = catalog.read_bytes()
        restore_writes = {'count': 0}

        def skip_last_restore(actual_root, path, data, **kwargs):
            if Path(path) == catalog and data == pristine:
                restore_writes['count'] += 1
                if restore_writes['count'] == len(names):
                    return
            return raw_write_atomic(actual_root, path, data, **kwargs)

        negative_process.write_source_atomic = skip_last_restore

    if flow == 'summary-publish':
        # The publish step itself: the temp summary file renames onto
        # summary.json, so fail exactly that rename after an otherwise
        # healthy run. write_source_atomic (the guard's restore) goes
        # through os.replace and is untouched.
        raw_replace = Path.replace

        def fail_summary_publish(self, target):
            if self.name == 'summary.json.tmp':
                raise OSError('#1800 injected summary publish failure')
            return raw_replace(self, target)

        Path.replace = fail_summary_publish

    if reader == 'pipe':
        # #1800 fix-r4（DS sibling#2 / boundary#1，两家同撞）：管道分支与
        # 两个 except 的先后次序此前零仓内覆盖——合并或调序成统一的
        # `except OSError` 后，健康的 `... | head` 运行会从「吞管道、按判
        # 决码退」变成 rc 2 + 诊断（协调者把健康读成无判决，假 FAIL），
        # 而 Ran 11 照旧全绿。形态对照 /dev/full 实测的反面：管道错误
        # 是 BrokenPipeError（EPIPE），必须走吞掉 + close stdout 的分支。
        # #3043：这一轴与 flow 正交，任何判决态都能配上该读者故障。
        real_stdout = sys.stdout

        class ClosedPipeStdout:
            def write(self, text):
                if '"counts"' in text:
                    raise BrokenPipeError('broken pipe')
                return real_stdout.write(text)

            def flush(self):
                return real_stdout.flush()

            def close(self):
                return real_stdout.close()

        sys.stdout = ClosedPipeStdout()

    if reader == 'full':
        # #1800 fix-r3 (comment follow-up 1): the final counts line fails
        # with the measured /dev/full shape — OSError(ENOSPC), never a pipe
        # error (the review's premise, reproduced by this file's host
        # evidence). Only that one line fails: the per-control record lines
        # still reach the reader, so the run itself completed and only the
        # stdout delivery broke. The swap is in-process (nothing inherited
        # from the environment is consulted) and stderr keeps its real
        # stream, so the diagnostic the fix must print stays observable.
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
sys.argv = [str(runner_path), str(out), *names]
exec(compile(source, str(runner_path), 'exec'), runner_globals if flow != 'busy-lock' else {'__file__': str(runner_path), '__name__': '__main__'})
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


class _PinDriver:
    """The shared subprocess driver: the real runner over a temporary tree.

    Not a TestCase — both the abort class and the verdict class below need
    ``run_pin`` / ``counts_line``, and neither can inherit the other's
    tests.
    """

    def run_pin(self, flow, reader='plain', names=SELECTION, base=None):
        """Drive the real runner source in a subprocess over a temporary tree.

        ``flow`` and ``reader`` are the two orthogonal grid axes (#3043
        simplify#1): the verdict/abort shape and the stdout-reader shape.
        Returns ``(process, summary-or-None, tree, catalog_before)``; the
        catalog copy is taken before the run so every case can also prove the
        tree was restored whatever the abort did. ``base`` lets a caller reuse
        a tree it prepared (the busy-lock case must lock the very tree the
        runner then runs over). fix-r9 (MM sibling P1): the staged catalog
        and pin-driver bytes are both read inside the tree lock (they are
        real guarded sources a sibling round can hold mutants on), so
        ``catalog_before`` is HEAD-shaped by construction.
        """
        if base is None:
            base = Path(tempfile.mkdtemp(prefix='pdrs-1800-pin-'))
            _BASES.append(base)
        tree = (base / 'tree').resolve()
        catalog = tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals/negative_controls.py'
        catalog.parent.mkdir(parents=True)
        # fix-r9 (MM sibling P1, the roster_guards twin's lock shape): both
        # staged files are real guarded sources -- the catalog is this
        # fixture's own mutation target (RESTORE_SET), and the pin driver is
        # the close-order-reader control's -- so a sibling round can keep a
        # mutant on disk for the whole bound-test window. The reads happen
        # inside the tree lock, and only the staged copies travel: the
        # harness below reads ``staged_pin`` (a private temp file), so the
        # subprocess itself never needs the lock.
        staged_pin = base / 'staged-pin.py'
        with guarded_source_window():
            staged_pin.write_bytes(PIN_SCRIPT.read_bytes())
            catalog.write_bytes(CATALOG_SCRIPT.read_bytes())
            # fix-r10 (P2#1, DS sibling#2 ≡ MM concurrency#2): stage the
            # runner's fixture import closure next to the staged catalog, so
            # the staged runner's own ``sys.path.insert(parents[1])`` (the
            # temp tree) resolves every shared module first. Before this the
            # temp tree resolved none of them and the imports fell back to
            # the real fixtures directory, where a sibling round can hold a
            # mutant on disk -- the same TOCTOU the staging exists for,
            # leaking through the import channel.
            stage_shared_sources(catalog.parent, PIN_RUNNER_SHARED_SOURCES)
        for ledger in ('D1-SDK-WS-CLOSE-ORDER-001.md',
                       'D1-SDK-SIGNAL-UNHANDLED-REJECTION-001.md'):
            path = tree / 'specs/gateway-parity/diffs' / ledger
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('#1800 temporary ledger placeholder\n', encoding='utf-8')
        harness = base / 'harness.py'
        # The dedicated #1382 marker is pinned in this module's constants, so
        # it reaches the subprocess as a literal rather than a second copy.
        harness.write_text(
            HARNESS_SOURCE.replace('__PIN_MARKER__', repr(PIN_MARKER)),
            encoding='utf-8')
        out = base / 'out'
        before = catalog.read_bytes()
        env = {**os.environ, 'CARGO_TARGET_DIR': str(base / 'unused-target')}
        process = subprocess.run(
            [sys.executable, '-I', str(harness), flow, reader, str(staged_pin),
             str(FIXTURES), str(tree), str(out), *names],
            capture_output=True, text=True, timeout=120, env=env)
        summary = None
        if (out / 'summary.json').is_file():
            summary = json.loads((out / 'summary.json').read_text(encoding='utf-8'))
        return process, summary, tree, before

    @staticmethod
    def counts_line(stdout):
        lines = [line for line in stdout.split('\n') if line.strip()]
        return json.loads(lines[-1]) if lines else None

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


class AbortAccountingTests(_PinDriver, unittest.TestCase):
    """Every abort lands in the same accounting: all INVALID, summary, rc 2."""

    def assert_rows(self, summary, reasons):
        self.assertIsNotNone(summary, 'an aborting run must still publish its summary')
        self.assertEqual([row['mutation'] for row in summary['results']],
                         list(SELECTION),
                         '#1800 every selected control must stay accounted for')
        self.assertEqual([row['reason'] for row in summary['results']], reasons)
        self.assertEqual(summary['counts'],
                         {'KILLED': 0, 'SURVIVED': 0, 'INVALID': len(SELECTION)},
                         '#1800 an aborting run must revoke every kill and count '
                         'every row INVALID')

    def test_healthy_selection_publishes_killed_rows(self):
        process, summary, _tree, before = self.run_pin('killed')
        self.assertEqual(process.returncode, 0,
                         f'a healthy selection must exit 0: {process.stderr!r}')
        self.assertIsNotNone(summary)
        self.assertEqual([row['status'] for row in summary['results']],
                         ['KILLED'] * len(SELECTION),
                         '#1800 the driving harness must really produce green '
                         'rows, or every revoke assertion below is vacuous')
        self.assertEqual([row['reason'] for row in summary['results']],
                         [HEALTHY_REASON] * len(SELECTION))
        self.assertEqual(summary['counts'],
                         {'KILLED': len(SELECTION), 'SURVIVED': 0, 'INVALID': 0})
        self.assertEqual(self.counts_line(process.stdout)['counts'],
                         {'KILLED': len(SELECTION), 'SURVIVED': 0, 'INVALID': 0})
        catalog = (_tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#1800 a healthy run must restore the catalog byte for byte')

    def test_busy_source_lock_records_every_row_invalid(self):
        base = Path(tempfile.mkdtemp(prefix='pdrs-1800-lock-'))
        _BASES.append(base)
        tree = (base / 'tree').resolve()
        tree.mkdir(parents=True)
        ready = base / 'held'
        holder = subprocess.Popen(
            [sys.executable, '-I', '-c', LOCK_HOLDER_SOURCE,
             str(FIXTURES), str(tree), str(ready)],
            start_new_session=True)
        self.addCleanup(self._kill_holder, holder)
        deadline = time.monotonic() + 30
        while not ready.exists():
            self.assertIsNone(holder.poll(),
                             'the lock holder died before holding the tree lock')
            self.assertLess(time.monotonic(), deadline,
                            'the lock holder never published its ready file')
            time.sleep(0.05)
        process, summary, tree_back, before = self.run_pin('busy-lock', base=base)
        self.assertEqual(
            process.returncode, 2,
            f'#1800 a busy source lock must abort at rc 2, got '
            f'{process.returncode}: {process.stderr!r}{process.stdout!r}')
        lock = tree_back / '.negative-source.lock'
        # The busy-lock branch appends its holder hint to the abort reason
        # itself, so the summary carries it too.
        busy_reason = ('run aborted: TimeoutError: source lock busy: ' + str(lock)
                       + '; the other holder is most likely the #1145'
                         ' sdk-signals runner (it keeps .negative-source.lock'
                         ' for its whole case list) — rerun this pin after it'
                         ' finishes')
        self.assert_rows(summary, [busy_reason] * len(SELECTION))
        self.assertIn('#1382 run did not complete', process.stderr)
        # The busy-lock branch names the likely holder; a generic abort would
        # not produce this sentence.
        self.assertIn('most likely the #1145 sdk-signals runner', process.stderr)
        self.assertEqual(self.counts_line(process.stdout)['counts']['INVALID'],
                         len(SELECTION))
        catalog = (tree_back / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#1800 an entry-phase abort never wrote the catalog')

    def test_mid_run_restore_failure_revokes_and_completes_accounting(self):
        # The #1800 core: a restore failure after a green row must revoke the
        # kill (keeping its diagnostic as the prefix, the #979 shape) and
        # still record the unrun control — never a green row over an
        # unverified tree.
        process, summary, tree, before = self.run_pin('mid-restore')
        self.assertEqual(process.returncode, 2,
                         f'#1800 a mid-run restore failure must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        failure = 'run aborted: RuntimeError: #1382 restore failed: ' \
                  'src/pilotdeck-gateway/tests/fixtures/sdk-signals/negative_controls.py'
        self.assert_rows(summary, [HEALTHY_REASON + '; ' + failure, failure])
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#1800 the guard must still restore the tree its '
                         'runner failed to')

    def test_guard_exit_failure_revokes_every_completed_row(self):
        process, summary, tree, before = self.run_pin('guard-exit')
        self.assertEqual(process.returncode, 2,
                         f'#1800 a guard exit failure must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        failure = 'run aborted: RuntimeError: #1800 injected guard exit failure'
        self.assert_rows(summary, [HEALTHY_REASON + '; ' + failure,
                                   HEALTHY_REASON + '; ' + failure])
        self.assertIn('#1382 run did not complete', process.stderr)

    def test_cancellation_voids_recorded_kills_and_unrun_rows(self):
        # KeyboardInterrupt is a BaseException the per-control ``except
        # Exception`` must not swallow: the accounting sees it as a
        # cancellation of the whole run.
        process, summary, tree, before = self.run_pin('cancel')
        self.assertEqual(process.returncode, 2,
                         f'#1800 a cancellation must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        self.assert_rows(summary,
                         [HEALTHY_REASON + '; run aborted: KeyboardInterrupt: ',
                          'run aborted: KeyboardInterrupt: '])
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#1800 a cancelled run must still restore the tree')

    def test_a_single_control_on_the_plain_cancel_cell_is_not_refused(self):
        # #3043 fix-r4（a3，MM boundary#2）：名册守卫的另一半宽度钉——
        # cancel×plain 格根本没有离开屏障（plain 读者不碰 stdout 管道），
        # 单控制的取消就是一次普通中止：该行未及 record，由中止记账补记
        # INVALID、summary 落盘、rc 2。把守卫收窄条件去掉 reader 一半
        # （只剩 ``flow == 'cancel'``）本用例即红（96 拒绝）——这是 reader
        # 收窄方向的钉。
        process, summary, tree, before = self.run_pin(
            'cancel', names=(SELECTION[0],))
        self.assertEqual(process.returncode, 2,
                         '#3043 the roster precondition belongs to the '
                         'cancel×head cell alone — a plain-reader '
                         'single-control cancellation is an ordinary abort, '
                         'not a 96 refusal: '
                         f'rc={process.returncode}, stderr={process.stderr!r}')
        self.assertIn('#1382 run did not complete', process.stderr,
                      '#3043 the single-control cancellation must keep the '
                      'normal abort diagnostic, not a harness refusal')
        self.assertIsNotNone(
            summary,
            '#3043 an ordinary abort still publishes its summary: the '
            'single-control plain cancellation must be accounted')
        self.assertEqual([row['mutation'] for row in summary['results']],
                         [SELECTION[0]],
                         '#3043 the unrun control must still be recorded by '
                         'the abort accounting')
        self.assertEqual([row['reason'] for row in summary['results']],
                         ['run aborted: KeyboardInterrupt: '])
        self.assertEqual(summary['counts'],
                         {'KILLED': 0, 'SURVIVED': 0, 'INVALID': 1},
                         '#3043 the cancelled single control must count as '
                         'one INVALID row through the abort path')
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#3043 a plain single-control cancellation must '
                         'still restore the tree')


class RealPipeReaderTests(_PinDriver, unittest.TestCase):
    """#3043: the abort cell of the state-by-reader grid, through a real pipe.

    The counts-line fakes pin the reader axis for the three verdict states,
    but they can only ever break the final print: ``ClosedPipeStdout`` keys on
    ``'"counts"'``, and a record line never contains it. A real ``... | head``
    reader closes the pipe after the first record line, so on an aborting run
    the EPIPE lands inside the abort accounting — the ``record()`` print for
    the unrun row — where the runner's contract (#1508 fix-r2) is to fail
    closed: the nested handler names the failure on stderr, exits 2, and
    leaves no summary at all. Until #3043 that cell had no pin anywhere in the
    repo, and no text-injection fake could express it.
    """

    def test_cancelled_run_read_by_a_real_head_fails_closed_in_the_accounting(self):
        # #3043（concurrency#1 + boundary#1）：三态×读者的第四格——中止运行
        # （cancel：末控负跑抛 KeyboardInterrupt）被真 ``head -1`` 读者读到。
        # 读者收完第一条 record 行即退出、关管；中止记账为未跑行补记的
        # record print 必中 EPIPE。runner 的契约（#1508 fix-r2 嵌套 handler）
        # 是 fail closed：stderr 报「abort accounting failed」、rc 2、不留
        # summary（无 summary＝装置没出判决）。harness 在取消点先以
        # ERR|HUP|NVAL 掩码 poll 写端、断言 revents 命中（#3043 fix-r1：
        # darwin 上只注册 POLLERR 永远不报事件，死等超时）等读者真离开，
        # EPIPE 落点确定，不靠 head 退出与下一次写之间的竞态。
        base = Path(tempfile.mkdtemp(prefix='pdrs-3043-real-pipe-'))
        _BASES.append(base)
        out = base / 'out'
        process, summary, tree, before = self.run_pin(
            'cancel', reader='head', base=base)
        self.assertEqual(process.returncode, 2,
                         '#3043 an aborting run whose accounting record line '
                         'hits a closed pipe must still exit 2 — the '
                         'accounting failure is a no-verdict exit, never a '
                         f'green or survived code: rc={process.returncode}, '
                         f'stderr={process.stderr!r}')
        self.assertIn(
            '#1382 abort accounting failed: BrokenPipeError',
            process.stderr,
            '#3043 the EPIPE on the abort accounting record line must be '
            'diagnosed on stderr, not swallowed: the reader leaving is not '
            'the run completing')
        self.assertNotIn(
            '#1382 run did not complete', process.stderr,
            '#3043 the accounting failure must fail closed before the tail: '
            'the run never completed, so the tail abort diagnostic must not '
            'appear in its place')
        self.assertIsNone(
            summary,
            '#3043 a failed abort accounting must publish no summary at all '
            '— no summary is the fail-closed signal, never evidence')
        self.assertFalse(
            (out / 'summary.json').exists(),
            '#3043 the aborted accounting must not leave a summary.json a '
            'later reader could mistake for a verdict')
        self.assertFalse(
            (out / 'summary.json.tmp').exists(),
            '#3043 the aborted accounting must not leave a summary temp file '
            'behind either')
        self.assertTrue(
            process.stdout == f'{SELECTION[0]}: KILLED: {HEALTHY_REASON}\n',
            '#3043 the real reader must have received exactly the first '
            'record line — the pipe closed at a record print, not the '
            f'counts line: stdout={process.stdout!r}')
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertTrue(catalog.read_bytes() == before,
                        '#3043 a cancelled run read through a real pipe must '
                        'still restore the catalog byte for byte')

    def test_a_single_control_roster_is_refused_at_96_before_the_runner(self):
        # #3043 fix-r3（DS fix-r2 增量评审 P2a）：fix-r2 钉在 harness 入口的
        # 名册前提（len(names) < 2 → [harness-roster] + os._exit(96)）此前无
        # 任何用例触达——删掉守卫或改回旧形状，CI 仍全绿。单控制
        # （names=(SELECTION[0],)）在 harness 入口即拒：rc 96（与屏障自身的
        # 97 区分）、stderr 落可识别诊断、无 summary、runner 未起、树未被碰。
        base = Path(tempfile.mkdtemp(prefix='pdrs-3043-roster-96-'))
        _BASES.append(base)
        out = base / 'out'
        process, summary, tree, before = self.run_pin(
            'cancel', reader='head', names=(SELECTION[0],), base=base)
        self.assertEqual(process.returncode, 96,
                         '#3043 the grid driver must refuse a single-control '
                         'roster at its entrance with the reserved code 96 — '
                         'not the barrier\'s 97 and not a runner exit: rc='
                         f'{process.returncode}, stderr={process.stderr!r}')
        self.assertIn('[harness-roster]', process.stderr,
                      '#3043 the refusal must carry the recognizable '
                      '[harness-roster] diagnostic, not a bare traceback')
        self.assertEqual(process.stdout, '',
                         '#3043 the refusal happens before the runner starts, '
                         'so no record line may reach the reader first')
        self.assertIsNone(
            summary,
            '#3043 the roster refusal must publish no summary — it is a '
            'misconfigured invocation, never a verdict')
        self.assertFalse((out / 'summary.json').exists(),
                         '#3043 the roster refusal must not leave a '
                         'summary.json a later reader could mistake for a '
                         'verdict')
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertTrue(catalog.read_bytes() == before,
                        '#3043 the roster refusal fires before the guard, so '
                        'the tree must be untouched byte for byte')

    def test_a_single_control_on_the_healthy_head_cell_is_not_refused(self):
        # #3043 fix-r4（a3，MM boundary#2）：名册守卫的立论只属 cancel×head
        # 一格（离开屏障只在取消点存在），fix-r4 前却在全网格生效——本格
        # （killed×head，单控制）与健康 plain 运行同形，却被同一条 96 拒掉
        # （MM 实测）。收窄后必须跑到判决：head 读走唯一一条 record 行，
        # counts 行 EPIPE 走 runner 的吞管道分支，summary 落盘、rc 0。把守
        # 卫放宽回全网格（或只留 ``reader == 'head'`` 一个条件）本用例即红
        # ——这是 flow 收窄方向的钉。
        base = Path(tempfile.mkdtemp(prefix='pdrs-3043-width-kh-'))
        _BASES.append(base)
        process, summary, tree, before = self.run_pin(
            'killed', reader='head', names=(SELECTION[0],), base=base)
        self.assertEqual(process.returncode, 0,
                         '#3043 the roster precondition belongs to the '
                         'cancel×head cell alone — a healthy single-control '
                         'run read by a real head must reach its verdict, not '
                         f'a 96 refusal: rc={process.returncode}, '
                         f'stderr={process.stderr!r}')
        self.assertEqual(
            process.stdout, f'{SELECTION[0]}: KILLED: {HEALTHY_REASON}\n',
            '#3043 the real reader must have received exactly the single '
            'record line — the pipe closed at a record print, and the counts '
            'line was swallowed by the pipe branch: '
            f'stdout={process.stdout!r}')
        self.assertIsNotNone(
            summary,
            '#3043 a healthy single-control head-read run is a verdict, not '
            'a refusal: the summary must be published')
        self.assertEqual([row['status'] for row in summary['results']],
                         ['KILLED'],
                         '#3043 the single selected control must be recorded '
                         'KILLED on the healthy head cell')
        self.assertEqual(summary['counts'],
                         {'KILLED': 1, 'SURVIVED': 0, 'INVALID': 0},
                         '#3043 the healthy head-cell run must count 1/0/0')
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertTrue(catalog.read_bytes() == before,
                        '#3043 a healthy single-control head-read run must '
                        'still restore the catalog byte for byte')


class SurvivedVerdictTests(_PinDriver, unittest.TestCase):
    """The verdict edge no abort can reach: a surviving control exits 1."""

    def test_survived_negative_exits_one_and_keeps_the_tree_restored(self):
        # #1800 fix-r1（PR #2979 boundary P2，升 b 类）：负控回放此前恒为
        # 「rc!=0+marker → KILLED」或 abort，runner 判决路径的 SURVIVED→rc 1
        # 零覆盖——回归把它路由成 rc 0（协调者读成绿）或 rc 2（读成「装置
        # 未跑」），本文件仍全绿，而 runner 自己的契约明写这两个码不得共用。
        process, summary, tree, before = self.run_pin('survived')
        self.assertEqual(process.returncode, 1,
                         f'#1800 a surviving selection must exit 1: '
                         f'{process.stderr!r}{process.stdout!r}')
        self.assertIsNotNone(summary)
        self.assertEqual([row['status'] for row in summary['results']],
                         ['SURVIVED'] * len(SELECTION),
                         '#1800 a green negative run is SURVIVED, never '
                         'KILLED or INVALID')
        self.assertEqual([row['reason'] for row in summary['results']],
                         ['preflight still passed'] * len(SELECTION))
        self.assertEqual(summary['counts'],
                         {'KILLED': 0, 'SURVIVED': len(SELECTION), 'INVALID': 0})
        self.assertEqual(self.counts_line(process.stdout)['counts'],
                         {'KILLED': 0, 'SURVIVED': len(SELECTION), 'INVALID': 0})
        self.assertIn('#1382 SURVIVED controls', process.stderr)
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#1800 a surviving run must still restore the catalog')

    def test_survived_with_a_closed_pipe_still_exits_one(self):
        # #1800 fix-r6（DS sibling P2，升 b 类）：三态×管道矩阵的最后一格
        # ——SURVIVED 运行（全相位绿，rc 1 是红装置经 ``... | head`` 给出的
        # 唯一信号）读到 counts 行时管道已关。吞管道必须不吞三态：回归把
        # 判决段塌成两态（如 `os._exit(2 if invalid else 0)`，丢掉 rc 1）
        # 时，健康用例（rc 0）与 INVALID 用例（rc 2）照绿，唯独本腿会把
        #「有控制存活」读成 rc 0（协调者读成全绿＝假 PASS）——本用例钉 rc 1。
        process, summary, tree, before = self.run_pin('survived', reader='pipe')
        self.assertEqual(process.returncode, 1,
                         '#1800 a SURVIVED run read through a closed pipe '
                         'must still exit 1 — swallowing the pipe error must '
                         'not collapse the tri-state verdict codes: '
                         f'rc={process.returncode}, stderr={process.stderr!r}')
        self.assertIn('#1382 SURVIVED controls', process.stderr,
                      '#1800 the SURVIVED diagnostic must still reach stderr '
                      'after the pipe branch')
        self.assertNotIn('#1382 stdout summary write failed', process.stderr,
                         '#1800 the pipe error is swallowed, not diagnosed')
        self.assertNotIn('#1382 run did not complete', process.stderr,
                         '#1800 rc 1 is the verdict path, never an abort')
        self.assertNotIn('"counts"', process.stdout,
                         '#1800 the counts line never reached the reader: '
                         'the pipe closed at it')
        self.assertIsNotNone(summary,
                            '#1800 a SURVIVED run is not an abort: the '
                            'summary must still be published')
        self.assertEqual([row['status'] for row in summary['results']],
                         ['SURVIVED'] * len(SELECTION),
                         '#1800 the rows are SURVIVED on the verdict path, '
                         'with the pipe fault layered on top')
        self.assertEqual(summary['counts'],
                         {'KILLED': 0, 'SURVIVED': len(SELECTION),
                          'INVALID': 0})
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#1800 a SURVIVED pipe run must still restore the '
                         'catalog')


class InvalidVerdictTests(_PinDriver, unittest.TestCase):
    """The verdict path's INVALID edge: no verdict, never rc 0 or 1."""

    def test_invalid_negative_exits_two_on_the_verdict_path(self):
        # #1800 fix-r2（PR #2979 boundary P1）：三态契约里 INVALID 的判决路径
        # （非中止）此前零覆盖——本文件每一条 INVALID 行都来自中止分支，而
        # runner 的判决顺序是 abort 判断先于 `if invalid: os._exit(2)`，两条
        # 是不同代码路径。把判决路径的退出码删掉（→rc 0，协调者把「装置没出
        # 判决」读成全绿＝假 PASS）或改成 1（→读成「有控制存活」＝审计误
        # 判），本文件此前全绿；本用例把它钉成 rc 2。
        process, summary, tree, before = self.run_pin('invalid')
        self.assertEqual(process.returncode, 2,
                         f'#1800 a verdict-path INVALID run must exit 2: '
                         f'{process.stderr!r}{process.stdout!r}')
        self.assertIsNotNone(summary,
                            '#1800 a verdict-path INVALID run is not an '
                            'abort: the summary must still be published')
        self.assertEqual([row['status'] for row in summary['results']],
                         ['INVALID'] * len(SELECTION),
                         '#1800 a negative run without the dedicated #1382 '
                         'marker is INVALID, never KILLED or SURVIVED')
        self.assertEqual([row['reason'] for row in summary['results']],
                         ['preflight failed without the dedicated #1382 '
                          'message'] * len(SELECTION))
        self.assertEqual(summary['counts'],
                         {'KILLED': 0, 'SURVIVED': 0, 'INVALID': len(SELECTION)})
        self.assertEqual(self.counts_line(process.stdout)['counts'],
                         {'KILLED': 0, 'SURVIVED': 0, 'INVALID': len(SELECTION)})
        self.assertIn('#1382 INVALID controls', process.stderr)
        # The rc 2 must come from the verdict path, not the abort branch —
        # the discriminator that keeps the two exit-2 routes from sharing
        # this case's coverage.
        self.assertNotIn('#1382 run did not complete', process.stderr)
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#1800 an INVALID-verdict run must still restore '
                         'the catalog')

    def test_invalid_verdict_with_a_closed_pipe_still_exits_two(self):
        # #1800 fix-r5（DS boundary P2a）：三态配对的第三腿——判决路径
        # INVALID（非中止）的运行被 ``... | head`` 读到 counts 行。吞掉管
        # 道错误后必须落到判决段的 `if invalid: os._exit(2)`，不能把
        #「装置没出判决」折进管道分支的退出。坏改法（`except
        # BrokenPipeError: ... os._exit(0)` 或判决段挪进 try 提前退出）
        # 会让 head 读法的 INVALID 运行 rc 0——协调者把无判决读成全绿
        # （假 PASS），而健康用例（rc 0）、ENOSPC 用例（不抛
        # BrokenPipeError）、abort 用例（不装管道故障）全照绿。
        process, summary, tree, before = self.run_pin('invalid', reader='pipe')
        self.assertEqual(process.returncode, 2,
                         '#1800 a verdict-path INVALID run read through a '
                         'closed pipe must still exit 2 — swallowing the '
                         'pipe error must not swallow the verdict: '
                         f'rc={process.returncode}, stderr={process.stderr!r}')
        self.assertIn('#1382 INVALID controls', process.stderr,
                      '#1800 the INVALID diagnostic must still reach stderr '
                      'after the pipe branch')
        self.assertNotIn('#1382 stdout summary write failed', process.stderr,
                         '#1800 the pipe error is swallowed, not diagnosed')
        self.assertNotIn('#1382 run did not complete', process.stderr,
                         '#1800 the rc 2 is the verdict path, not an abort')
        self.assertNotIn('"counts"', process.stdout,
                         '#1800 the counts line never reached the reader: '
                         'the pipe closed at it')
        self.assertIsNotNone(summary,
                            '#1800 a verdict-path INVALID run is not an '
                            'abort: the summary must still be published')
        self.assertEqual([row['status'] for row in summary['results']],
                         ['INVALID'] * len(SELECTION),
                         '#1800 the rows are INVALID on the verdict path, '
                         'with the pipe fault layered on top')
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#1800 an INVALID-verdict pipe run must still '
                         'restore the catalog')

    def test_unpublished_summary_exits_two_without_a_summary(self):
        # #1800 fix-r2（PR #2979 boundary P1）：summary 发布失败→rc 2 那条
        # 路径（temp 写入 + rename，失败须 unlink 且不得留半份/旧份 summary）
        # 此前同样零覆盖——本用例让一次全 KILLED 的健康运行只在 rename 一步
        # 失败，钉住「无 summary＝装置没出判决＝rc 2」。
        base = Path(tempfile.mkdtemp(prefix='pdrs-1800-unpublished-'))
        _BASES.append(base)
        out = base / 'out'
        process, summary, tree, before = self.run_pin(
            'summary-publish', base=base)
        self.assertEqual(process.returncode, 2,
                         f'#1800 a run that cannot publish its summary must '
                         f'exit 2: {process.stderr!r}{process.stdout!r}')
        # The rows themselves completed and were recorded as KILLED, so the
        # rc 2 can only come from the publish branch, never an abort.
        self.assertIn(': KILLED: ', process.stdout,
                      '#1800 the publish-fault run must have completed its '
                      'rows: only the summary rename fails')
        self.assertNotIn('#1382 run did not complete', process.stderr)
        self.assertIn('#1382 summary not published: OSError: '
                      '#1800 injected summary publish failure',
                      process.stderr)
        self.assertIsNone(summary,
                           '#1800 an unpublished summary must leave no '
                           'summary.json behind')
        self.assertFalse((out / 'summary.json').exists(),
                         '#1800 a failed publish must not leave a summary '
                         'file a later reader could mistake for evidence')
        self.assertFalse((out / 'summary.json.tmp').exists(),
                         '#1800 a failed publish must drop its temp file')
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#1800 a publish-fault run must still restore '
                         'the catalog')


class StdoutWriteFaultTests(_PinDriver, unittest.TestCase):
    """#1800 fix-r3: a non-EPIPE stdout write failure fails closed, loudly."""

    def test_stdout_write_failure_exits_two_with_a_stderr_diagnostic(self):
        # #1800 fix-r3（评论追加 1）：最终 counts 行的 stdout 写失败——
        # /dev/full 实测抛 OSError(ENOSPC) 而非 BrokenPipeError（实验 A/B
        # 原始输出贴 RESULT）——原 except 只认管道，异常落进尾段 catch-all：
        # rc 2 不变、stderr 0 字节（连 abort 路径的「run did not complete」
        # 都被跳过），即评审所说的「静默 rc 2」。回归两个方向都要红：吞掉
        # 诊断（stderr 无该行），或放行成 rc 0（协调者把没送达的判决读成
        # 全绿）。
        process, summary, tree, before = self.run_pin('killed', reader='full')
        self.assertEqual(process.returncode, 2,
                         '#1800 a run whose final stdout line cannot be '
                         'written must exit 2 — the verdict line never '
                         f'reached the reader: rc={process.returncode}, '
                         f'stderr={process.stderr!r}')
        self.assertIn(
            '#1382 stdout summary write failed: OSError: '
            '[Errno 28] No space left on device',
            process.stderr,
            '#1800 a swallowed non-EPIPE stdout write failure must name '
            'itself on stderr before failing closed')
        self.assertIsNotNone(
            summary,
            '#1800 only the stdout line failed: the summary must still be '
            'published')
        self.assertTrue(
            [row['status'] for row in summary['results']]
            == ['KILLED'] * len(SELECTION),
            '#1800 the rows completed and were recorded: only the stdout '
            'delivery failed, got '
            + repr([row['status'] for row in summary['results']]))
        self.assertIn(': KILLED: ', process.stdout,
                      '#1800 the record lines must still have reached the '
                      'reader: the fault is the final counts line alone')
        self.assertNotIn('"counts"', process.stdout,
                         '#1800 the counts line must not be readable from '
                         'stdout: its write failed')
        self.assertNotIn('#1382 run did not complete', process.stderr,
                         '#1800 the rc 2 must come from the stdout write '
                         'branch, not the abort branch — the run completed')
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertTrue(catalog.read_bytes() == before,
                        '#1800 a stdout-fault run must still restore the '
                        'catalog byte for byte')

    def test_stdout_pipe_fault_keeps_the_healthy_exit_code(self):
        # #1800 fix-r4（DS sibling#2 与 boundary#1 两家同撞）：counts 行抛
        # BrokenPipeError —— ``... | head`` 关管道的实测形态。except 的先后
        # 顺序是本契约的承重墙：BrokenPipeError 分支必须先于 OSError 分支
        # 吞掉管道错误（退出码路由读者，不由 stdout 决定），健康的全 KILLED
        # 运行仍按判决码 rc 0 退出、不落 #1382 诊断。把两分支合并成一个
        # `except OSError`（或调换次序）会让健康的 head 读法变成 rc 2 + 诊
        # 断——协调者把全绿读成「无判决」，假 FAIL；而 Ran 11 的闸门照旧
        # 全绿，所以这条必须自己钉。
        process, summary, tree, before = self.run_pin('killed', reader='pipe')
        self.assertEqual(process.returncode, 0,
                         '#1800 a broken pipe on the counts line is the '
                         'reader leaving (`... | head`): the exit code '
                         'routes the reader, so the healthy verdict code '
                         'must survive it — got '
                         f'rc={process.returncode}, stderr='
                         f'{process.stderr!r}')
        self.assertNotIn('#1382 stdout summary write failed', process.stderr,
                         '#1800 the pipe branch must swallow the error '
                         'ahead of the OSError branch: a healthy head '
                         'read is not a write failure to diagnose')
        self.assertNotIn('#1382 run did not complete', process.stderr,
                         '#1800 the run completed; only the reader left')
        self.assertIn(': KILLED: ', process.stdout,
                      '#1800 the record lines before the counts line must '
                      'still have reached the reader')
        self.assertNotIn('"counts"', process.stdout,
                         '#1800 the counts line must not be readable from '
                         'stdout: the pipe closed at it')
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertTrue(catalog.read_bytes() == before,
                        '#1800 a pipe-fault run must still restore the '
                        'catalog byte for byte')


class HarnessAxisValidationTests(_PinDriver, unittest.TestCase):
    """#3043 fix-r4: the driver grid's two axes are closed enums, refused."""

    def test_an_unknown_flow_is_refused_at_the_harness_entrance(self):
        # #3043 fix-r4（a2，MM boundary#1）：拼错的 flow（'hed'）此前静默落
        # 进 else 分支的健康路径，与 (killed, plain) 完全同形（rc 0 全绿），
        # 调用方拿不到「这不是一个格」的信号。入口照 #1382 先例拒：rc 1 +
        # 「unknown or duplicate selection」+ 无 summary + 树未被碰。
        process, summary, tree, before = self.run_pin('hed')
        self.assertEqual(process.returncode, 1,
                         '#3043 an unknown flow value must be a pre-run '
                         'refusal at the harness entrance with the #1382 '
                         'sibling code 1, never a silent fall into the '
                         f'healthy default branch: rc={process.returncode}, '
                         f'stderr={process.stderr!r}')
        self.assertIn('[harness-axis]', process.stderr,
                      '#3043 the refusal must carry the recognizable '
                      '[harness-axis] diagnostic, not a bare traceback')
        self.assertIn('#1382 unknown or duplicate selection', process.stderr,
                      '#3043 the refusal text must match the #1382 sibling '
                      'shape, not a newly invented code')
        self.assertEqual(process.stdout, '',
                         '#3043 the refusal happens before the runner starts, '
                         'so no record line may exist')
        self.assertIsNone(summary,
                          '#3043 an axis refusal is a misconfigured '
                          'invocation, never a verdict: no summary')
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertTrue(catalog.read_bytes() == before,
                        '#3043 the axis refusal fires before the guard, so '
                        'the tree must be untouched byte for byte')

    def test_an_unknown_reader_is_refused_at_the_harness_entrance(self):
        # #3043 fix-r4（a2，MM boundary#1）另一半：reader 轴同形——拼错的
        # reader（'hed'）此前同样静默落 plain，健康的 killed 流照常跑绿，
        # 调用方以为测了 head 读者。同一 #1382 形状拒。
        process, summary, tree, before = self.run_pin('killed', reader='hed')
        self.assertEqual(process.returncode, 1,
                         '#3043 an unknown reader value must be refused at '
                         'the harness entrance with the #1382 sibling code 1 '
                         '— a typo must not silently run as the plain '
                         f'reader: rc={process.returncode}, '
                         f'stderr={process.stderr!r}')
        self.assertIn('[harness-axis]', process.stderr,
                      '#3043 the refusal must carry the recognizable '
                      '[harness-axis] diagnostic, not a bare traceback')
        self.assertIn('#1382 unknown or duplicate selection', process.stderr,
                      '#3043 the refusal text must match the #1382 sibling '
                      'shape, not a newly invented code')
        self.assertEqual(process.stdout, '',
                         '#3043 the refusal happens before the runner starts, '
                         'so no record line may exist')
        self.assertIsNone(summary,
                          '#3043 an axis refusal is a misconfigured '
                          'invocation, never a verdict: no summary')
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertTrue(catalog.read_bytes() == before,
                        '#3043 the axis refusal fires before the guard, so '
                        'the tree must be untouched byte for byte')


class PreGuardRefusalTests(unittest.TestCase):
    """The three-state contract's other edge: pre-guard refusals exit 1."""

    def refusal(self, argv, env):
        base = Path(tempfile.mkdtemp(prefix='pdrs-1800-refusal-'))
        _BASES.append(base)
        fixtures = base / 'src/pilotdeck-gateway/tests/fixtures'
        staged_pin = (fixtures / 'sdk-signals-close-order-pin'
                      / 'negative_controls.py')
        staged_pin.parent.mkdir(parents=True)
        # fix-r10 (P2#5, MM sibling#2): this used to exec the real PIN_SCRIPT
        # directly, unlocked -- the one remaining execution of a guarded
        # source outside a window in this file, and PIN_SCRIPT is exactly
        # the close-order-reader round's RUNNER, so a sibling round's mutant
        # could rewrite the refusal contract under it. Take a staged copy
        # inside the window (the run_pin shape) and run the staged bytes; the
        # tree shape keeps the runner's own ``parents[1]`` sys.path derivation
        # inside the temp tree, and the whole import closure is staged with
        # it (P2#1 -- a direct run imports negative_process itself).
        with guarded_source_window():
            staged_pin.write_bytes(PIN_SCRIPT.read_bytes())
            stage_shared_sources(fixtures, DIRECT_RUNNER_SHARED_SOURCES)
        out = base / 'out'
        result = subprocess.run(
            [sys.executable, '-I', str(staged_pin), str(out), *argv],
            capture_output=True, text=True, timeout=60, env=env)
        return result, out

    def test_unknown_selection_exits_one_without_a_summary(self):
        result, out = self.refusal(
            ['no-such-control-1800'],
            {**os.environ, 'CARGO_TARGET_DIR': '/nonexistent-cargo-target'})
        self.assertEqual(result.returncode, 1,
                         f'#1800 an unknown selection is a pre-guard refusal '
                         f'(rc 1): {result.stderr!r}')
        self.assertIn('#1382 unknown or duplicate selection', result.stderr)
        self.assertFalse(out.exists(),
                         '#1800 a pre-guard refusal must not create evidence')

    def test_missing_cargo_target_exits_one_without_a_summary(self):
        result, out = self.refusal(
            [SELECTION[0]],
            {key: value for key, value in os.environ.items()
             if key != 'CARGO_TARGET_DIR'})
        self.assertEqual(result.returncode, 1,
                         f'#1800 a missing CARGO_TARGET_DIR is a pre-guard '
                         f'refusal (rc 1): {result.stderr!r}')
        self.assertIn('#1382 CARGO_TARGET_DIR must be supplied', result.stderr)
        self.assertFalse(out.exists())


# #3043 fix-r5 (sibling#2): the startup roster pin is driven through a real
# subprocess like the classes above, but nothing needs patching — only the
# module-level checks matter, so the copy runs with the real shared helpers
# (the bootstrap pre-inserts the real fixtures root on sys.path; the copy's
# own parents[1] insert lands in the temporary tree and resolves nothing).
ROSTER_BOOT_SOURCE = """\
import sys
copy, out = sys.argv[1], sys.argv[2]
sys.argv = ['negative_controls.py', out]
sys.path.insert(0, __REAL_FIXTURES__)
import runpy
runpy.run_path(copy, run_name='__main__')
"""

# Table rows carry an 8-space indent, EXPECTED_NAMES entries a 4-space one,
# and each anchor is newline-anchored so it names exactly one site — never a
# substring of the other indent, never the docstring bullets.
_TABLE_ROW = '\n        "unordered-close-id-renamed",\n'
_ROSTER_ENTRY = '\n    "unordered-close-id-renamed",\n'
_TABLE_ROW_MOVED = '\n        "unordered-close-row-removed",\n'
_TABLE_ROW_NEXT = '\n        "unordered-close-boundary-dropped",\n'


class StaticRosterTests(unittest.TestCase):
    """#3043 fix-r5: the static name roster refuses same-count drift (rc 1).

    EXPECTED_TOTAL already blocks a deleted row and a duplicated name, but a
    rename, a reorder or a same-count substitution kept every prior check
    green (measured pre-fix: a renamed row reached the CARGO_TARGET_DIR
    refusal). The EXPECTED_NAMES + registered() pair closes that gap per
    specs/negative-controls/README.md invariant 1. The refusal keeps this
    runner's pinned rc-1 pre-run contract, and no evidence directory may
    appear for any of them. The pristine width direction proves the negative
    results come from the roster, not the bootstrap: an unmutated copy of the
    same driver reaches the next pre-run refusal instead.
    """

    def run_copy(self, source, with_target=False):
        """Run a staged copy of the driver; return (result, out).

        ``with_target=True`` supplies a dummy ``CARGO_TARGET_DIR`` so the
        refusal cases below stay falsifiable (fix-r8 boundary#1, the
        shared-runner twin's fix-r6 shape): the module-level roster and
        count refusals fire before ``main()`` ever runs, and with the
        variable stripped the copy's next stop was the ``CARGO_TARGET_DIR``
        refusal -- also before ``new_output_directory`` -- so no path ever
        created ``out`` and the bare ``assertFalse(out.exists())`` held
        vacuously. With the dummy target, a pre-run refusal that stops
        firing lets the copy proceed to ``new_output_directory(sys.argv[1])``
        and create ``out`` (the temp tree stages no catalog, so the run
        then dies at its guard's baseline proof without touching any real
        source). The pristine case keeps the strip: the next refusal it
        pins is exactly the ``CARGO_TARGET_DIR`` check itself.
        """
        base = Path(tempfile.mkdtemp(prefix='pdrs-3043-roster-'))
        _BASES.append(base)
        tree = base / ('src/pilotdeck-gateway/tests/'
                       'fixtures/sdk-signals-close-order-pin')
        tree.mkdir(parents=True)
        copy = tree / 'negative_controls.py'
        copy.write_text(source, encoding='utf-8')
        # fix-r10 (P2#1, the same family as run_pin's staging): the copy's
        # own ``sys.path.insert(parents[1])`` puts the temp fixtures dir
        # first, so stage the runner's whole import closure there inside the
        # tree lock (a direct run imports negative_process itself) --
        # otherwise every shared module still resolves from the real
        # fixtures directory, where a sibling round can hold a mutant on
        # disk (``negative_suite.py`` is shared-runner's SUITE).
        with guarded_source_window():
            stage_shared_sources(tree.parent, DIRECT_RUNNER_SHARED_SOURCES)
        boot = base / 'boot.py'
        boot.write_text(
            ROSTER_BOOT_SOURCE.replace('__REAL_FIXTURES__',
                                       repr(str(FIXTURES))),
            encoding='utf-8')
        out = base / 'out'
        env = {key: value for key, value in os.environ.items()
               if key != 'CARGO_TARGET_DIR'}
        if with_target:
            env['CARGO_TARGET_DIR'] = str(base / 'unused-target')
        result = subprocess.run(
            [sys.executable, '-B', str(boot), str(copy), str(out)],
            capture_output=True, text=True, timeout=60, env=env)
        return result, out

    def test_a_renamed_row_is_refused_without_evidence(self):
        with guarded_source_window():
            source = PIN_SCRIPT.read_text(encoding='utf-8')
        self.assertEqual(source.count(_TABLE_ROW), 1)
        result, out = self.run_copy(
            source.replace(_TABLE_ROW,
                           _TABLE_ROW.replace('id-renamed', 'id-renamed-x')),
            with_target=True)
        self.assertEqual(result.returncode, 1,
                         f'#3043 a renamed row is a pre-guard refusal '
                         f'(rc 1): {result.stderr!r}')
        self.assertIn('#1382 mutation roster drifted from the static inventory',
                      result.stderr)
        self.assertIn('unordered-close-id-renamed', result.stderr)
        self.assertIn('unordered-close-id-renamed-x', result.stderr)
        self.assertFalse(out.exists())

    def test_a_reordered_row_is_refused_without_evidence(self):
        with guarded_source_window():
            source = PIN_SCRIPT.read_text(encoding='utf-8')
        self.assertEqual(source.count(_TABLE_ROW), 1)
        self.assertEqual(source.count(_TABLE_ROW_MOVED), 1)
        swapped = (source
                   .replace(_TABLE_ROW, '\n        "__swap__",\n')
                   .replace(_TABLE_ROW_MOVED, _TABLE_ROW)
                   .replace('\n        "__swap__",\n', _TABLE_ROW_MOVED))
        result, out = self.run_copy(swapped, with_target=True)
        self.assertEqual(result.returncode, 1,
                         f'#3043 a reordered row is a pre-guard refusal '
                         f'(rc 1): {result.stderr!r}')
        self.assertIn('#1382 mutation roster drifted from the static inventory',
                      result.stderr)
        self.assertFalse(out.exists())

    def test_a_row_deleted_with_its_roster_entry_still_shrinks_the_denominator(self):
        # The double edit registered() alone cannot see (both sides shrink
        # together) is exactly why the EXPECTED_TOTAL count literal stays: the
        # pair is two-sided, and this cell pins the count side of it.
        # fix-r9 (MM sibling P1, same family as run_pin's staging): the
        # driver text is read inside the tree lock -- a sibling round holding
        # a mutant on the real driver would otherwise stage mutant bytes here
        # and red the anchor-count expectations below as a product-looking
        # failure.
        with guarded_source_window():
            source = PIN_SCRIPT.read_text(encoding='utf-8')
        self.assertEqual(source.count(_TABLE_ROW), 1)
        self.assertEqual(source.count(_ROSTER_ENTRY), 1)
        index = source.index(_TABLE_ROW)
        # The anchor's leading newline is also the tuple-opening line's
        # terminator, so the row's own ``    (\n`` spans [index-4, index+1)
        # and the rfind end must be index + 1 to include it (fix-r8
        # boundary#2, the shared-runner twin's geometry): with end=index
        # the search landed on the *previous* row's opening, the deletion
        # below swallowed that row too, and the executed edit was two rows
        # minus one roster entry -- not the double edit this cell claims.
        start = source.rfind('    (\n', 0, index + 1)
        end = source.index('    (\n', index + 1)
        deleted = source[start:end]
        # Surgical, and pinned as such: exactly one tuple opening and the
        # renamed row's own name, nothing from either neighbouring row
        # (the pre-fix geometry also deleted the row-removed row, which
        # these assertions go red on).
        self.assertEqual(deleted.count('    (\n'), 1)
        self.assertIn('\n        "unordered-close-id-renamed",\n', deleted)
        self.assertNotIn(_TABLE_ROW_MOVED, deleted)
        without_row = source[:start] + source[end:]
        self.assertNotIn(_TABLE_ROW, without_row)
        self.assertEqual(without_row.count(_TABLE_ROW_NEXT), 1)
        # Both sides shrink 16 -> 15 together: one table row, one roster
        # entry -- the shape that keeps registered() quiet and leaves the
        # EXPECTED_TOTAL count literal as the only side that can notice.
        # (The entry removal glues its two neighbours onto one line, so the
        # roster side counts the block's quoted names, not line starts.)
        edited = without_row.replace(_ROSTER_ENTRY, '')
        self.assertEqual(without_row.count('    (\n'), 15)
        names_at = edited.index('EXPECTED_NAMES = (')
        self.assertEqual(
            edited[names_at:edited.index('\n)', names_at)]
            .count('"unordered-close-'),
            15)
        result, out = self.run_copy(edited, with_target=True)
        self.assertEqual(result.returncode, 1,
                         f'#3043 a double-deleted row still shrinks the '
                         f'denominator (rc 1): {result.stderr!r}')
        self.assertIn('#1506 fixed product mutation denominator', result.stderr)
        self.assertFalse(out.exists())

    def test_a_pristine_copy_reaches_the_next_refusal(self):
        with guarded_source_window():
            pristine_source = PIN_SCRIPT.read_text(encoding='utf-8')
        result, out = self.run_copy(pristine_source)
        self.assertEqual(result.returncode, 1,
                         f'#3043 the pristine copy must clear the roster and '
                         f'stop at the next pre-guard refusal: '
                         f'{result.stderr!r}')
        self.assertIn('#1382 CARGO_TARGET_DIR must be supplied', result.stderr)
        self.assertFalse(out.exists())


class GuardedSourceWindowTests(_PinDriver, unittest.TestCase):
    """fix-r10 (P1 + P2#2): the repo-tree lock window's two observable legs.

    P1 (four-way review, coordinator-measured red: the close-order-reader
    round went INVALID=6 because the bound test's guarded window waited
    2 s on its own ancestor): the bypass leg needs a regression pin -- a
    sibling process holds the very repo lock a real round would hold, the
    ``PDRS_NEGATIVE_LEASE_HELD_ROOT`` bypass is exported exactly as
    ``run_bound`` now exports it, and the bound-shaped staged read must
    stay green. P2#2 (DS sibling#3): the busy branch itself had no paired
    control anywhere -- nothing held the *repo* lock and asserted the
    named diagnostic within the budget.
    """

    def _hold_repo_lock(self):
        # The real repo lock, held by a sibling process in the round's own
        # shape (LOCK_HOLDER_SOURCE: acquire, publish the ready file
        # atomically, sleep). Positive control is the same window acquiring
        # cleanly once the holder is gone.
        base = Path(tempfile.mkdtemp(prefix='pdrs-3043-repolock-'))
        _BASES.append(base)
        ready = base / 'held'
        holder = subprocess.Popen(
            [sys.executable, '-I', '-c', LOCK_HOLDER_SOURCE,
             str(FIXTURES), str(REPO_ROOT), str(ready)],
            start_new_session=True)
        self.addCleanup(self._kill_holder, holder)
        deadline = time.monotonic() + 30
        while not ready.exists():
            self.assertIsNone(
                holder.poll(),
                'the repo lock holder died before holding the tree lock')
            self.assertLess(time.monotonic(), deadline,
                            'the repo lock holder never published its '
                            'ready file')
            time.sleep(0.05)
        return holder

    def test_bound_reads_stay_green_inside_an_ancestors_lock_window(self):
        # The P1 regression pin: without the env bypass this exact shape
        # waited 2 s on its own ancestor and failed closed (the round's
        # whole table INVALID). With it, the staged read runs to its green
        # verdict while the ancestor still holds the lock.
        self._hold_repo_lock()
        saved = os.environ.get('PDRS_NEGATIVE_LEASE_HELD_ROOT')
        os.environ['PDRS_NEGATIVE_LEASE_HELD_ROOT'] = str(REPO_ROOT)
        try:
            process, summary, tree, before = self.run_pin('killed')
        finally:
            if saved is None:
                del os.environ['PDRS_NEGATIVE_LEASE_HELD_ROOT']
            else:
                os.environ['PDRS_NEGATIVE_LEASE_HELD_ROOT'] = saved
        self.assertEqual(process.returncode, 0,
                         '#3043 a bound read inside the ancestor lock window '
                         f'must stay green: {process.stderr!r}'
                         f'{process.stdout!r}')
        self.assertEqual([row['status'] for row in summary['results']],
                         ['KILLED'] * len(SELECTION))
        self.assertEqual(summary['counts'],
                         {'KILLED': len(SELECTION), 'SURVIVED': 0,
                          'INVALID': 0})
        catalog = (tree / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals'
                   '/negative_controls.py')
        self.assertEqual(catalog.read_bytes(), before,
                         '#3043 the bypassed window must still stage and '
                         'restore the catalog byte for byte')

    def test_a_busy_repo_lock_fails_closed_within_the_budget(self):
        # P2#2 (DS sibling#3): the busy branch of guarded_source_window had
        # no paired control. The holder sleeps far longer than the budget,
        # so the refusal landing at ~2 s proves the window really waited
        # its budget out and then failed closed with the named diagnostic
        # -- never outlasting the holder, never refusing instantly.
        holder = self._hold_repo_lock()
        saved = os.environ.pop('PDRS_NEGATIVE_LEASE_HELD_ROOT', None)
        try:
            started = time.monotonic()
            with self.assertRaises(AssertionError) as raised:
                with guarded_source_window():
                    PIN_SCRIPT.read_bytes()
            elapsed = time.monotonic() - started
        finally:
            if saved is not None:
                os.environ['PDRS_NEGATIVE_LEASE_HELD_ROOT'] = saved
        message = str(raised.exception)
        self.assertIn('#3043 the guarded source was busy', message)
        self.assertIn('.negative-source.lock', message)
        self.assertGreaterEqual(
            elapsed, 1.0,
            'the window must really wait its 2 s budget out before '
            'refusing, not fail on some earlier error')
        self.assertLessEqual(
            elapsed, 5.0,
            'the refusal must land at the 2 s budget (50 ms poll), never '
            'outlast it toward the holder\'s sleep horizon')
        # Positive control: once the holder is gone the very same window
        # acquires and reads the guarded bytes cleanly.
        self._kill_holder(holder)
        with guarded_source_window():
            self.assertTrue(PIN_SCRIPT.read_bytes(),
                            'the window must read the guarded source once '
                            'the holder is gone')


def tearDownModule():
    for base in _BASES:
        shutil.rmtree(base, ignore_errors=True)


if __name__ == '__main__':
    unittest.main()

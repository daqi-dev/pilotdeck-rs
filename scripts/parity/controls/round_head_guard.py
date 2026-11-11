#!/usr/bin/env python3
"""#2513 fix-r6 control: round.py's post-lease apparatus guard, made observable.

Offline only: no sidecar, no full parity round, and the real shared
/tmp/pdrs-parity.lock is never taken.  The guard lives inside round.py's
parity_lock on the ``lock == LOCK`` branch (controls redirect the lock to a
temp path and are therefore exempt by construction — the host_env_isolation
precedent), so a plain control can never execute it.  This control stages a
scratch repository under the worktree's gitignored _tmp/ holding
scripts/parity/{round.py,g03_compare.py} copied from the worktree, with only
the LOCK constant rewritten to the scratch's own lease path: the ``lock ==
LOCK`` branch — the guard's only home — is then live without ever touching
the shared lease a live round may hold.  The scratch round.py's ``_REPO_ROOT``
walk-up lands on the scratch repository, so ``_head_source`` inside the guard
compares against the scratch's own HEAD — the executing-entry contract the
fix-r2 guard pins, replayed against a lease this control owns.

Three phases pin the guard:
  1. entry refusal — the probe module is loaded from bytes that differ from
     HEAD and the disk is restored before the lease is taken (exactly the
     mutation-window interleaving the guard exists for: CPython compiles the
     entry inside the window, the window restores the file, the round takes
     the lease).  parity_lock must refuse with the single-line #2513
     ValueError naming round.py, never reach the yield, and release the
     redirected lease;
  2. entry acceptance — a module whose executing bytes equal HEAD passes the
     guard, holds the redirected lease across the yield and releases it
     cleanly (the comparison is not a vacuous always-refuse);
  3. sibling refusal — a module whose g03_compare was loaded from mutated
     bytes (disk likewise restored) is refused with the #2513 ValueError
     naming g03_compare.py, the second comparison the guard performs.

A mutant that deletes the guard's call site fails phase 1: no ValueError is
raised where the refusal is required, the probe scratch yields instead, and
the bound markered assertion goes red.

#2513 fix-r7 (boundary P1 + sibling P2#2) adds two phases pinning *where*
``_head_source`` reads the bytes it compares against — the committed object
of this scratch, never the worktree file and never a decoy repository an
inherited ``GIT_DIR`` steers ``git -C`` at (measured pre-fix:
``_tmp/20261026-issue-2513/fix-r7/evidence-a-head-source-experiment.txt``):

  4. acceptance with the worktree and the environment pointing elsewhere —
     the executing bytes equal scratch HEAD, but the disk file is mutated
     after the load and ``GIT_DIR`` is steered at a decoy repository whose
     HEAD pins exactly those mutated bytes.  The guard must still ACCEPT.
     A mutant that reads the worktree file, or one that inherits the
     environment (and therefore the decoy HEAD), refuses here and this
     phase's markered assertion goes red;
  5. refusal under environment steering — the executing bytes differ from
     scratch HEAD (the mutation-window interleaving) and ``GIT_DIR`` is
     steered at a decoy whose HEAD pins the executing bytes: the pre-fix
     bare-``git`` shape false-passed exactly here.  The hardened
     ``/usr/bin/git`` with a fixed environment must refuse with the
     standard #2513 ValueError; an environment-inheriting mutant accepts
     and the bound assertRaises goes red.
"""
import sys

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution; without it exit before loading any apparatus module.
if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

if sys.version_info < (3, 11) or 'os' not in sys.modules:
    raise SystemExit('os must be preloaded from CPython 3.11+ frozen modules')
import os

_boot_dir = os.path.dirname(os.path.realpath(__file__))
_boot_dir = os.path.dirname(_boot_dir) if os.path.basename(_boot_dir) == 'controls' else _boot_dir
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (#1033). Nonexistent sys.path entries skip; an empty entry means cwd.
if any(
        os.path.isdir(entry or os.curdir)
        and any(os.path.isdir(root)
                and os.path.samefile(entry or os.curdir, root)
                for root in (_boot_dir, os.path.join(_boot_dir, 'controls')))
        for entry in sys.path):
    raise SystemExit('apparatus directory on sys.path; run the entry file directly with -I')

import tempfile
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-parity-roundguard-pyc-')
sys.dont_write_bytecode = True

import importlib.util
from pathlib import Path
import shutil
import subprocess
import unittest

_HERE = Path(__file__).resolve().parent          # scripts/parity/controls
_PARITY = _HERE.parent                            # scripts/parity
_REPO = _PARITY.parent.parent                     # the worktree root

# The one edit the scratch carries: LOCK points at the scratch's own lease
# path, so the ``lock == LOCK`` branch — the guard's only home — is live
# without ever touching the shared /tmp/pdrs-parity.lock.
_LOCK_ANCHOR = "LOCK = Path('/tmp/pdrs-parity.lock')"
_G03_ENTRY = 'g03_compare.py'


def _discover_baseline():
    """Resolve the PilotDeck baseline exactly as g03_compare does (#2398)."""
    configured = os.environ.get('PDRS_BASELINE')
    if configured:
        candidate = Path(configured)
        if not candidate.is_dir():
            raise RuntimeError(f'PDRS_BASELINE={configured} is not a directory')
        return candidate.resolve()
    candidates = [_REPO / 'resources/PilotDeck']
    # Same git isolation as g03_compare's own discovery — never inherit
    # GIT_*, never read a global gitconfig, always bounded.
    env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
    env['GIT_CONFIG_GLOBAL'] = os.devnull
    common = subprocess.run(['git', '--no-optional-locks', '-c', 'core.fsmonitor=false',
                             '-c', 'core.hooksPath=/dev/null', '-c', 'safe.directory=*',
                             '-C', str(_REPO), 'rev-parse',
                             '--path-format=absolute', '--git-common-dir'],
                            capture_output=True, text=True, env=env, timeout=30)
    if common.returncode == 0 and common.stdout.strip():
        candidates.append(Path(common.stdout.strip()).parent / 'resources/PilotDeck')
    for candidate in candidates:
        if candidate.is_dir():
            return candidate.resolve()
    raise RuntimeError(f'PilotDeck baseline not found at {candidates}; set PDRS_BASELINE')


# The scratch g03_compare resolves its baseline at import time and the scratch
# repository has no resources/ — pin the discovery to the worktree's own
# baseline before any scratch module loads.
os.environ['PDRS_BASELINE'] = str(_discover_baseline())


def _load(name, path):
    """Load an apparatus module by explicit path; never resolve via sys.path."""
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module


def _stage_round(tag):
    """Stage a scratch repository whose HEAD pins the scratch's own bytes.

    Layout: ``<holder>/scripts/parity/{round.py,g03_compare.py}``, so the
    scratch round.py's ``_HERE.parent.parent`` (its ``_REPO_ROOT``) and the
    scratch g03_compare's ``parents[2]`` both land on the holder — a fresh
    git repository whose ``HEAD:scripts/parity/...`` objects are the pristine
    scratch bytes.  ``_head_source`` inside the guard therefore compares
    against a HEAD this control owns, never the worktree's objects, and the
    git subprocesses stay inside the gitignored scratch.
    """
    # _tmp/ is gitignored, so a fresh checkout has no such directory yet;
    # TemporaryDirectory(dir=...) refuses to create the parent itself.
    (_REPO / '_tmp').mkdir(parents=True, exist_ok=True)
    holder = tempfile.TemporaryDirectory(prefix=f'pdrs-2513-roundguard-{tag}-',
                                         dir=_REPO / '_tmp')
    scratch = Path(holder.name)
    entry_dir = scratch / 'scripts' / 'parity'
    entry_dir.mkdir(parents=True)
    shutil.copy2(_PARITY / 'g03_compare.py', entry_dir / _G03_ENTRY)
    text = (_PARITY / 'round.py').read_text(encoding='utf-8')
    if text.count(_LOCK_ANCHOR) != 1:
        raise RuntimeError(f'round.py LOCK anchor occurs {text.count(_LOCK_ANCHOR)} times')
    text = text.replace(_LOCK_ANCHOR,
                       f"LOCK = Path({str(scratch / 'parity.lock')!r})")
    (entry_dir / 'round.py').write_text(text, encoding='utf-8')
    for args in (['init', '-q'],
                 ['add', '-A'],
                 ['-c', 'user.name=pdrs-2513-roundguard', '-c', 'user.email=pdrs-2513@invalid',
                  'commit', '-q', '-m', 'pdrs-2513 fix-r6 round.py guard control scratch']):
        subprocess.run(['git', '-C', str(scratch), *args], check=True, timeout=30)
    return holder, entry_dir / 'round.py', entry_dir / _G03_ENTRY, scratch / 'parity.lock'


def _stage_decoy(tag, round_bytes):
    """Stage a decoy repository whose HEAD pins the given round.py bytes.

    The steering half of the #2513 fix-r7 phases: a bare, environment-
    inheriting ``git -C <scratch> show HEAD:…`` reads this repository's
    object when ``GIT_DIR`` names it (measured pre-fix, evidence-a), while
    the hardened ``/usr/bin/git`` with its fixed environment never sees the
    variable.  g03_compare.py is committed alongside (pristine) so a mutant
    that passes the round.py comparison still reads a well-formed decoy
    instead of failing on a missing object.
    """
    (_REPO / '_tmp').mkdir(parents=True, exist_ok=True)
    holder = tempfile.TemporaryDirectory(prefix=f'pdrs-2513-roundguard-decoy-{tag}-',
                                         dir=_REPO / '_tmp')
    decoy = Path(holder.name)
    entry_dir = decoy / 'scripts' / 'parity'
    entry_dir.mkdir(parents=True)
    (entry_dir / 'round.py').write_bytes(round_bytes)
    shutil.copy2(_PARITY / 'g03_compare.py', entry_dir / _G03_ENTRY)
    for args in (['init', '-q'],
                 ['add', '-A'],
                 ['-c', 'user.name=pdrs-2513-roundguard', '-c', 'user.email=pdrs-2513@invalid',
                  'commit', '-q', '-m', 'pdrs-2513 fix-r7 round.py guard control decoy']):
        subprocess.run(['git', '-C', str(decoy), *args], check=True, timeout=30)
    return holder, decoy


class RoundHeadGuardControls(unittest.TestCase):
    def setUp(self):
        self.holders = []

    def tearDown(self):
        # A scratch round that raised through parity_lock released its own
        # lease in the contextmanager's finally (LIVE_GROUPS stays empty
        # here); the holder reaps the whole scratch repository.
        for holder in self.holders:
            holder.cleanup()

    def test_guard_refuses_entry_bytes_compiled_inside_a_mutation_window(self):
        # Phase 1: load the probe from bytes that differ from HEAD, then
        # restore the disk before taking the lease — the interleaving a
        # negative-control mutation window produces. The guard must refuse
        # right after the redirected lease is acquired, never reach the yield.
        holder, entry, sibling, lock = _stage_round('probe')
        self.holders.append(holder)
        pristine = entry.read_bytes()
        entry.write_bytes(pristine + b'\n_2513_guard_probe = 1\n')
        probe = _load('round_guard_probe', entry)
        entry.write_bytes(pristine)
        try:
            with self.assertRaises(ValueError, msg='#2513 round.py 取锁后装置核验必须'
                                                   '拒绝与 HEAD 不同的执行字节（守卫未被调用）') as caught:
                with probe.parity_lock():
                    raise AssertionError('#2513 the probe scratch must be refused '
                                        'before parity_lock yields')
        finally:
            probe.LIVE_GROUPS.clear()
        self.assertEqual(str(caught.exception),
                         '#2513 loaded apparatus differs from HEAD: scripts/parity/round.py',
                         '#2513 round.py 装置核验拒绝消息必须逐字节是既有的单行 #2513 消息')
        self.assertFalse(lock.exists(),
                         '#2513 拒绝路径必须把重定向租约完整释放，不留 owner 残留')

    def test_guard_accepts_entry_bytes_equal_to_head(self):
        # Phase 2: a module whose executing bytes equal HEAD passes the guard
        # (the comparison is not a vacuous always-refuse), holds the
        # redirected lease across the yield and releases it cleanly.
        holder, entry, sibling, lock = _stage_round('pristine')
        self.holders.append(holder)
        pristine = _load('round_guard_pristine', entry)
        try:
            with pristine.parity_lock():
                self.assertTrue(lock.exists(),
                                '#2513 pristine scratch must hold its redirected lease')
            self.assertFalse(lock.exists(),
                             '#2513 pristine scratch must release its redirected lease')
        finally:
            pristine.LIVE_GROUPS.clear()

    def test_guard_refuses_mutated_sibling_module(self):
        # Phase 3: the entry's own bytes equal HEAD, but its g03_compare was
        # loaded from mutated bytes (disk likewise restored before the lease).
        # The guard must refuse naming g03_compare.py.
        holder, entry, sibling, lock = _stage_round('sibling')
        self.holders.append(holder)
        pristine = sibling.read_bytes()
        sibling.write_bytes(pristine + b'\n_2513_guard_probe = 1\n')
        module = _load('round_guard_sibling', entry)
        sibling.write_bytes(pristine)
        try:
            with self.assertRaises(ValueError, msg='#2513 round.py 取锁后装置核验必须'
                                                   '同时核对 g03_compare 的执行字节（核对被删除）') as caught:
                with module.parity_lock():
                    raise AssertionError('#2513 the sibling-mutated scratch must be '
                                        'refused before parity_lock yields')
        finally:
            module.LIVE_GROUPS.clear()
        self.assertEqual(str(caught.exception),
                         '#2513 loaded apparatus differs from HEAD: scripts/parity/g03_compare.py',
                         '#2513 g03_compare 装置核验拒绝消息必须逐字节是既有的单行 #2513 消息')
        self.assertFalse(lock.exists(),
                         '#2513 拒绝路径必须把重定向租约完整释放，不留 owner 残留')

    def test_guard_accepts_when_worktree_and_git_dir_point_elsewhere(self):
        # Phase 4 (#2513 fix-r7, boundary P1 + sibling P2#2): the executing
        # bytes equal scratch HEAD, but after the load the disk file is
        # mutated AND GIT_DIR is steered at a decoy repository whose HEAD pins
        # exactly those mutated bytes. The guard must still ACCEPT — the
        # hardened _head_source reads the committed object of this scratch
        # only. A mutant that reads the worktree file, or inherits the
        # environment (and with it the decoy HEAD), refuses here and this
        # phase's markered assertion goes red.
        holder, entry, sibling, lock = _stage_round('worktree')
        self.holders.append(holder)
        module = _load('round_guard_worktree', entry)
        different = entry.read_bytes() + b'\n_2513_guard_probe = 1\n'
        entry.write_bytes(different)
        decoy_holder, decoy = _stage_decoy('worktree', different)
        self.holders.append(decoy_holder)
        held = False
        try:
            os.environ['GIT_DIR'] = str(decoy / '.git')
            try:
                with module.parity_lock():
                    held = lock.exists()
            except ValueError as error:
                raise AssertionError(
                    '#2513 _head_source 必须只读提交对象：工作区被改且 GIT_DIR 被引导时，'
                    '执行字节==HEAD 的轮次被拒绝（读到了工作区文件或诱饵 HEAD）: '
                    + str(error)) from error
        finally:
            os.environ.pop('GIT_DIR', None)
            module.LIVE_GROUPS.clear()
        self.assertTrue(held,
                        '#2513 工作区≠HEAD 且 GIT_DIR 被引导时，守卫必须仍接受执行字节==HEAD 的轮次'
                        '并持有重定向租约')
        self.assertFalse(lock.exists(),
                         '#2513 接受路径在 yield 结束后必须释放重定向租约')

    def test_guard_refuses_when_git_dir_steers_at_executing_bytes(self):
        # Phase 5 (#2513 fix-r7): the executing bytes differ from scratch HEAD
        # (the mutation-window interleaving), the disk is restored before the
        # lease, and GIT_DIR is steered at a decoy whose HEAD pins the
        # executing bytes — the pre-fix bare-git shape false-passed exactly
        # here (measured pre-fix, evidence-a). The hardened _head_source must
        # still refuse: its fixed environment never sees GIT_DIR. A mutant
        # that inherits the environment accepts and the bound assertRaises
        # goes red.
        holder, entry, sibling, lock = _stage_round('gitdir')
        self.holders.append(holder)
        pristine = entry.read_bytes()
        executing = pristine + b'\n_2513_guard_probe = 1\n'
        entry.write_bytes(executing)
        module = _load('round_guard_gitdir', entry)
        entry.write_bytes(pristine)
        decoy_holder, decoy = _stage_decoy('gitdir', executing)
        self.holders.append(decoy_holder)
        try:
            os.environ['GIT_DIR'] = str(decoy / '.git')
            with self.assertRaises(ValueError, msg='#2513 _head_source 必须不继承环境：'
                                                   'GIT_DIR 指向装着执行字节的诱饵 HEAD 时守卫假放行') as caught:
                with module.parity_lock():
                    # The canary carries the same dedicated marker as the
                    # assertRaises msg: an exception of the wrong type (this
                    # raise, the false-pass shape) bypasses the msg parameter,
                    # and the kill row's judge greps this exact marker.
                    raise AssertionError('#2513 _head_source 必须不继承环境：'
                                        'GIT_DIR 指向装着执行字节的诱饵 HEAD 时守卫假放行'
                                        '（parity_lock 在引导环境下让步，未抛 ValueError 即到达 yield）')
            self.assertTrue(str(caught.exception) ==
                            '#2513 loaded apparatus differs from HEAD: scripts/parity/round.py',
                            '#2513 round.py 装置核验拒绝消息必须逐字节是既有的单行 #2513 消息')
            self.assertFalse(lock.exists(),
                             '#2513 拒绝路径必须把重定向租约完整释放，不留 owner 残留')
        finally:
            os.environ.pop('GIT_DIR', None)
            module.LIVE_GROUPS.clear()


if __name__ == '__main__':
    unittest.main()

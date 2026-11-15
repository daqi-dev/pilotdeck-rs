#!/usr/bin/env python3
"""#2513 host-environment isolation control: the env round.py spawns run.py with.

Offline only: no sidecar, no full parity round, no shared /tmp lock.  The test
drives round.main()'s real spawn path with a fake child (same wait-timeout
BLOCKED shape as lock_release.py) and captures the env= argument passed to
R.spawn.  The host environment is patched to carry PILOTDECK_GATEWAY_MODE plus
the six pre-existing PILOTDECK_* deployment variables (#1354 family), so the
assertions prove the cleanup tuple strips all seven from the child env while
everything round.py itself sets still arrives.  The real /tmp/pdrs-parity.lock
is never touched: parity_lock is redirected to a temporary lease root.
"""
import sys

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution; without it, exit before loading any apparatus module.
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
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-parity-hostenv-pyc-')
sys.dont_write_bytecode = True

from contextlib import redirect_stdout
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

_HERE = Path(__file__).resolve().parent
_ROOT = _HERE.parent


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


G = _load('g03_compare', _ROOT / 'g03_compare.py')
R = _load('round', _ROOT / 'round.py')

# The #1354 deployment family the round host must not leak into the run.py
# child environment.  #2513 added PILOTDECK_GATEWAY_MODE to the tuple.
HOST_PILOTDECK_VARS = ('PILOTDECK_CONFIG_PATH', 'PILOTDECK_STORAGE_ROOT',
                       'PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND',
                       'PILOTDECK_AGENT_LOOP_SIDECAR_PATH',
                       'PILOTDECK_AGENT_LOOP_TCP_HOST',
                       'PILOTDECK_AGENT_LOOP_TCP_PORT')


class HostEnvIsolationControls(unittest.TestCase):
    def setUp(self):
        # _tmp/ is gitignored, so a fresh checkout has no such directory yet;
        # TemporaryDirectory(dir=...) refuses to create the parent itself.
        (G.WORKTREE / '_tmp').mkdir(parents=True, exist_ok=True)
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-2513-hostenv-', dir=G.WORKTREE / '_tmp')
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        self.lock = self.root / 'parity.lock'

    def tearDown(self):
        R.LIVE_GROUPS.clear()

    def test_spawn_env_strips_host_deployment_vars(self):
        baseline = self.root / 'baseline'
        binary = baseline / 'dist/src/cli/pilotdeck-agent-loop-sidecar.js'
        binary.parent.mkdir(parents=True)
        binary.write_text('hostenv-control')
        worktree = self.root / 'worktree'
        (worktree / '_tmp').mkdir(parents=True)
        out_root = worktree / '_tmp/parity'
        real_lock = R.parity_lock
        argv = ['round.py', '--side', 'ts', '--label', 'host-env', '--scenario', 'pure_text']
        captured = []

        class FakeProc:
            # wait-timeout BLOCKED shape from lock_release.py: communicate
            # succeeds, both bounded leader waits time out, only the final
            # ESRCH probe sees the group gone.  The env capture happens at
            # R.spawn time, before any of this cleanup matters.
            pid = 505053
            returncode = 0

            def wait(self, timeout=None):
                raise subprocess.TimeoutExpired(str(self.pid), timeout)

            def communicate(self, timeout=None):
                return 'stdout', 'stderr'

        def capture_spawn(*args, **kwargs):
            R.LIVE_GROUPS.add(FakeProc.pid)
            captured.append(dict(kwargs['env']))
            return FakeProc()

        def killpg(pid, sig):
            if sig == 0:
                raise ProcessLookupError(pid)
            return None

        def isolated_lock(*args, **kwargs):
            return real_lock(self.lock, **kwargs)

        host = {'PILOTDECK_GATEWAY_MODE': 'native',
                'PILOTDECK_CONFIG_PATH': '/p2513-dummy/pilotdeck.yaml',
                'PILOTDECK_STORAGE_ROOT': '/p2513-dummy/storage',
                'PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND': '/p2513-dummy/sidecar-command',
                'PILOTDECK_AGENT_LOOP_SIDECAR_PATH': '/p2513-dummy/sidecar-path',
                'PILOTDECK_AGENT_LOOP_TCP_HOST': 'p2513.invalid',
                'PILOTDECK_AGENT_LOOP_TCP_PORT': '62513',
                # The probe carries the family prefix without being part of
                # the deployment family the cleanup tuple names: a prefix-
                # wide strip would remove it and turn this assertion red, so
                # the enumeration below is discriminated, not assumed
                # (#2513 fix-r1 review P2#5 — the probe used to be
                # P2513_HOST_PROBE, which both strips survived).
                'PILOTDECK_HOST_PROBE': '1'}
        with patch.object(sys, 'argv', argv), \
                patch.dict(os.environ, {'PDRS_OUT': str(out_root), **host}), \
                patch.object(G, 'WORKTREE', worktree), patch.object(G, 'BASELINE', baseline), \
                patch.object(G, 'scenario_catalog', return_value={'pure_text': {}}), \
                patch.object(R.shutil, 'which', return_value=sys.executable), \
                patch.object(R.subprocess, 'check_output', return_value='v-control\n'), \
                patch.object(R, 'spawn', side_effect=capture_spawn), \
                patch.object(R.os, 'killpg', side_effect=killpg), \
                patch.object(R, 'parity_lock', side_effect=isolated_lock), \
                redirect_stdout(io.StringIO()):
            self.assertEqual(R.main(), 1,
                             '#2513 the control round must end BLOCKED/1 after the spawn-env capture')
            self.assertEqual(len(captured), 1,
                             '#2513 exactly one spawn must carry the run.py child environment')
            env = captured[0]
            # The seven PILOTDECK_* deployment variables must be absent.
            self.assertFalse('PILOTDECK_GATEWAY_MODE' in env,
                             '#2513 PILOTDECK_GATEWAY_MODE 泄漏：宿主导出的 '
                             'PILOTDECK_GATEWAY_MODE 到达了 run.py 子进程环境'
                             '（round.py 宿主环境清理元组缺项）')
            for key in HOST_PILOTDECK_VARS:
                self.assertFalse(key in env,
                                 f'#2513 宿主环境清理元组既有项泄漏：{key} '
                                 '到达了 run.py 子进程环境')
            # Positive bindings: what round.py itself sets must still arrive,
            # proving the capture is the env round.py constructed.
            self.assertTrue(env.get('PDRS_SIDE') == 'ts',
                            '#2513 PDRS_SIDE 未按 round.py 构造传入子进程环境')
            self.assertTrue(env.get('PDRS_BASELINE') == str(baseline),
                            '#2513 PDRS_BASELINE 未按 round.py 构造传入子进程环境')
            self.assertTrue(env.get('PILOTDECK_AGENT_LOOP_TRANSPORT') == 'stdio',
                            '#2513 PILOTDECK_AGENT_LOOP_TRANSPORT 未按 round.py 构造传入子进程环境')
            out = out_root / 'host-env'
            self.assertTrue(env.get('PILOT_HOME') == str(out / 'pilot-home'),
                            '#2513 PILOT_HOME 未按 round.py 构造传入子进程环境')
            self.assertTrue(env.get('TMPDIR') == str(out / 'tmp'),
                            '#2513 TMPDIR 未按 round.py 构造传入子进程环境')
            # Enumerated isolation, not a prefix-wide strip: a host variable
            # outside the named tuple but inside the PILOTDECK_ prefix must
            # survive into the child environment (the prefix is what makes
            # this discriminating, #2513 fix-r1 review P2#5).
            self.assertTrue(env.get('PILOTDECK_HOST_PROBE') == '1',
                            '#2513 无关宿主变量 PILOTDECK_HOST_PROBE 被剥离：'
                            '隔离被改成了整前缀剥离（非本单语义）')
            # The pop only edits the child env copy; round's own process
            # environment must still carry the host export.
            self.assertTrue(os.environ.get('PILOTDECK_GATEWAY_MODE') == 'native',
                            '#2513 round 自身进程环境被改动：pop 只应作用于子进程 env 副本')


if __name__ == '__main__':
    unittest.main()

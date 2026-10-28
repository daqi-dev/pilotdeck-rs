#!/usr/bin/env python3
"""E1 draft apparatus. Calibration is TS-only; A7 native execution is NOT_RUN.

No A2 proxy round is relabelled native. Native audit functions below consume
independent launcher/exec/network evidence; their mutation tests are apparatus
tests, never evidence that a Rust host has run or retired the TS host.
"""
# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)
# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  It must run before every other standard-library import: a
# shadowed argparse/pathlib would otherwise execute before this check.  ``sys``
# is already initialised by the interpreter, so it is the only safe import.
import sys

if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

import importlib.util
from pathlib import Path
import tempfile

# -B only stops cache writes, so redirect the bytecode prefix too (fix-r5).
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')
sys.dont_write_bytecode = True
# CPython has already compiled the CLI entry before it can acquire a lease.
# Capture that executing object, not a later read of the restored entry file.
_apparatus_code = sys._getframe().f_code

# Load the one guard by explicit path, never sys.path: a same-named package or
# extension module beside the source would win before the scan (fix-r6 P1).
# Inline on purpose: importing the guard is what this block defends.
_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
_guard_path = _apparatus / '_guard.py'
_guard = sys.modules.get('_guard')
if not (getattr(_guard, '__file__', None)
        and Path(_guard.__file__).resolve() == _guard_path
        and isinstance(getattr(_guard, '_apparatus_source', None), bytes)):
    _spec = importlib.util.spec_from_file_location('_guard', _guard_path)
    _guard = importlib.util.module_from_spec(_spec)
    sys.modules['_guard'] = _guard
    _guard_source = _guard_path.read_bytes()
    try:
        exec(compile(_guard_source, str(_guard_path), 'exec'), _guard.__dict__)
    except BaseException:
        sys.modules.pop('_guard', None)
        raise
    _guard._apparatus_source = _guard_source
if Path(_guard.__file__).resolve() != _guard_path:
    raise RuntimeError(f'_guard: loaded {_guard.__file__}, expected {_guard_path}')

_load = _guard.load
_has_git_checkout = _guard._has_git_checkout
_verify_cli_imports = _guard.verify_cli_imports
run_entry = _guard.run_entry
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap
import argparse
from collections import Counter
import ctypes
import ctypes.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time


ROOT = common.ROOT
SCRIPT = common.SCRIPT
apparatus_hashes = common.apparatus_hashes
baseline_dependency_hashes = common.baseline_dependency_hashes
digest = common.digest
install_shutdown = common.install_shutdown
parity_lock = common.verified_parity_lock
save = common.save
compare_module = _load('compare', common.SCRIPT / 'compare.py')
wire_frame = compare_module.wire_frame
observer_module = _load('observer', common.SCRIPT / 'observer.py')
Decoder = observer_module.Decoder

# 显式 --baseline > PDRS_BASELINE > 本 checkout 的 resources/PilotDeck（#187：不再写死主仓绝对路径）
BASELINE = Path(os.environ.get('PDRS_BASELINE') or ROOT / 'resources/PilotDeck')
BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
# scripts/a5-19/six_cells.mts 直接导入的 TS 源码入口（E1 不经 dist）
E1_SOURCES = ('src/gateway/client/InProcessGateway.ts', 'src/gateway/SessionRouter.ts', 'src/gateway/run/RunRegistry.ts',
              'src/session/transcript/JsonlTranscriptWriter.ts', 'src/session/events/JsonlSessionEventStore.ts',
              'src/gateway/util/AsyncQueue.ts', 'src/gateway/client/GatewayTurnCompletionFence.ts',
              'src/agent/session/AgentSession.ts', 'src/agent/turn/TurnRunner.ts',
              'src/agent/modules/transport/operationLedger.ts')
CELLS = ('no-registry-fast', 'no-registry-slow', 'no-registry-failure',
         'registry-fast', 'registry-slow', 'registry-failure')
# Cleanup verification is now the launcher-owned process group probe, not the
# ps census: the child is started with start_new_session, so its pid is the pgid.
CLEANUP_VERIFIED = 'VERIFIED'
CLEANUP_GROUP_NOT_EMPTY = 'GROUP_NOT_EMPTY'
CLEANUP_BLOCKED = 'BLOCKED'
CLEANUP_NOT_VERIFIED = 'NOT_VERIFIED'
CLEANUP_UNVERIFIED_STATUS = 'PASS_WITH_CLEANUP_UNVERIFIED'
CLEANUP_UNVERIFIED_FREEZE = 'BLOCKED_CLEANUP_UNVERIFIED'
MATRIX = tuple((host, sidecar) for host in ('ts', 'rust') for sidecar in ('ts', 'rust'))
REPEATS = 5
FIXTURE_NAMES = ('run.py', 'six_cells.mts', 'observation_rules.mjs',
                 'append-observer.mjs', 'cleanup-observer.mjs')


class EvidenceError(ValueError):
    def __init__(self, rule, detail):
        super().__init__(detail)
        self.rule = rule


class ApparatusVerificationError(EvidenceError):
    """The instrument is untrusted; this is not a parity result."""

    def __init__(self, detail):
        super().__init__('APPARATUS_IMPORTS', detail)


def require(condition, rule, detail):
    if not condition:
        raise EvidenceError(rule, detail)


def read(path):
    return json.loads(Path(path).read_text())


def hashes():
    # E1 includes its transitive observation rules and the reused A2 primitives.
    paths = [SCRIPT / name for name in ('session_domain.py', 'session_observations.mjs',
             'controls/session_domain.py', 'fixtures/session-calibration.json',
             'common.py', 'compare.py', 'driver.mts', 'observer.py')]
    paths += [ROOT / 'scripts/a5-19' / name for name in FIXTURE_NAMES]
    return {**apparatus_hashes(), **{p.relative_to(ROOT).as_posix(): digest(p) for p in paths}}


def frozen_check():
    process = subprocess.run([sys.executable, '-I', str(SCRIPT / 'check_frozen.py')],
                             capture_output=True, text=True, timeout=15)
    return {'exitCode': process.returncode, 'stdout': process.stdout, 'stderr': process.stderr}


def process_tree(root_pid):
    """Read only our descendants. An inaccessible census is not an empty tree."""
    try:
        result = subprocess.run(['ps', '-axo', 'pid=,ppid=,command='],
                                capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.SubprocessError) as error:
        return {'status': 'BLOCKED', 'reason': str(error), 'processes': []}
    if result.returncode:
        return {'status': 'BLOCKED', 'reason': result.stderr.strip(), 'processes': []}
    rows = []
    for line in result.stdout.splitlines():
        fields = line.strip().split(None, 2)
        if len(fields) == 3:
            rows.append({'pid': int(fields[0]), 'ppid': int(fields[1]), 'argvText': fields[2]})
    owned = {root_pid}
    while True:
        children = {row['pid'] for row in rows if row['ppid'] in owned}
        if children <= owned:
            break
        owned |= children
    return {'status': 'SNAPSHOT_ONLY', 'processes': [r for r in rows if r['pid'] in owned],
            'limitation': 'sampling cannot prove absence of short-lived helpers; native requires exec witness'}


def wait_exit_unreaped(pid, timeout):
    """Wait for *pid* to exit without reaping it (POSIX ``waitid`` WNOWAIT).

    While the unreaped zombie still owns its pid, neither that pid nor the
    matching pgid can be reused, so a later ``killpg()`` still names our group.
    Returns ``EXITED`` once the child is a zombie, ``REAPED`` if somebody else
    already reaped it (ownership is no longer provable), ``RUNNING`` on timeout,
    ``UNSUPPORTED`` where waitid/WNOWAIT is unavailable (never signal then).
    """
    if not hasattr(os, 'waitid') or not hasattr(os, 'WNOWAIT'):
        return 'UNSUPPORTED'
    deadline = time.monotonic() + timeout
    while True:
        try:
            info = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOWAIT | os.WNOHANG)
        except ChildProcessError:
            return 'REAPED'
        if info is not None:
            return 'EXITED'
        if time.monotonic() >= deadline:
            return 'RUNNING'
        time.sleep(.02)


_PROC_LIBRARY = None
_PROC_BUFFER_SLACK = 4096
_PROC_PGRP_ONLY = 2  # <sys/proc_info.h>; typeinfo is the process group id
_CENSUS_ATTEMPTS = 3


def _darwin_group_pids(pgid, library=None):
    """Exact pids in *pgid* from libproc's ``PROC_PGRP_ONLY``.

    A single kernel call returns the group's current members (including an
    unreaped zombie leader), so there is no "global pid snapshot then per-pid
    getpgid" window in which a member can fork a descendant and exit unseen.
    ``proc_listpids`` copies min(capacity, members) and returns the bytes
    copied: a returned count equal to the capacity is truncation, not a
    complete census.  Allocate slack, accept only a strictly short, 4-byte
    aligned write, and grow while the buffer is full.  ``None`` means the
    group could not be enumerated completely.  ``library`` is a test seam.
    """
    global _PROC_LIBRARY
    try:
        if library is None:
            if _PROC_LIBRARY is None:
                _PROC_LIBRARY = ctypes.CDLL(ctypes.util.find_library('proc') or 'libproc.dylib',
                                            use_errno=True)
                _PROC_LIBRARY.proc_listpids.restype = ctypes.c_int
                _PROC_LIBRARY.proc_listpids.argtypes = [ctypes.c_uint32, ctypes.c_uint32,
                                                        ctypes.c_void_p, ctypes.c_int]
            library = _PROC_LIBRARY
        needed = library.proc_listpids(_PROC_PGRP_ONLY, pgid, None, 0)
        if needed <= 0:
            return None
        for _ in range(4):
            capacity = needed + _PROC_BUFFER_SLACK
            buffer = (ctypes.c_uint32 * ((capacity + 3) // 4))()
            written = library.proc_listpids(_PROC_PGRP_ONLY, pgid, buffer, capacity)
            if written <= 0:
                return None
            if written < capacity and written % 4 == 0:
                return {pid for pid in buffer[:written // 4] if pid}
            # A full buffer is truncated (or the count is malformed): grow and
            # retry, never return a partial group.
            needed = written
    except (OSError, AttributeError, ValueError):
        return None
    return None


def _group_snapshot(pgid):
    """One complete group enumeration, or ``None`` when completeness is unproven.

    Only macOS libproc gives a direct, exact group census on this apparatus.
    Linux ``/proc`` is mount-dependent: ``hidepid`` and dumpable/permission
    filters can omit live members from ``iterdir``/``getpgid`` without any error,
    so a visible set is not a complete set and an empty visible set is not an
    empty group.  There is no portable proof of completeness here, so every
    non-darwin platform stays unproven and ``probe_process_group`` fails closed
    before reading a census or sending any signal.
    """
    if sys.platform == 'darwin':
        return _darwin_group_pids(pgid)
    return None


def live_group_members(pgid):
    """Live (and unreaped) pids in *pgid*, independently of ps/signal permission.

    ``killpg(pgid, 0)`` returning EPERM only proves "no member this process may
    signal": XNU filters the unreaped zombie in the group path, and a live
    descendant with different credentials is denied too.  On macOS the group is
    enumerated directly with ``PROC_PGRP_ONLY``; two consecutive complete
    snapshots must agree, otherwise a member appeared/disappeared while the
    census was taken and the result is unproven.  Truncation, enumeration
    failure, and a changing group all return ``None``; callers must fail closed
    instead of guessing "empty".
    """
    for _ in range(_CENSUS_ATTEMPTS):
        first = _group_snapshot(pgid)
        if first is None:
            continue
        second = _group_snapshot(pgid)
        if second is None:
            continue
        if first != second:
            # Membership changed between complete snapshots: a descendant may
            # exist that neither snapshot can vouch for.
            continue
        return second
    return None


class HeldProcessGroup:
    """The only entry allowed to signal this run's process group.

    It holds the direct child and refuses every actual signal unless
    ``waitid(WNOWAIT)`` proves the leader is still ours: ``RUNNING`` (live,
    unreaped direct child) or ``EXITED`` (zombie, pid/pgid still reserved).
    ``REAPED``/``UNSUPPORTED`` means the pid/pgid may already name another
    process group, so the request is refused and the caller records
    NOT_VERIFIED/BLOCKED rather than VERIFIED.  Signal-0 observations are
    exposed as ``observe``/``probe_*`` and never claim ownership.
    """

    def __init__(self, child):
        self.child = child
        self.pid = child.pid

    def _state(self):
        return wait_exit_unreaped(self.pid, 0)

    def _signal(self, sig, *, group, states):
        state = self._state()
        if state not in states:
            return 'refused', state
        try:
            if group:
                os.killpg(self.pid, sig)
            else:
                os.kill(self.pid, sig)
        except ProcessLookupError:
            return 'missing', state
        except PermissionError as error:
            return 'denied', str(error)
        except OSError as error:
            return 'error', f'{type(error).__name__}: {error}'
        return ('alive' if sig == 0 else 'sent'), state

    def send(self, sig):
        """Deliver *sig* to the held group. ``sig`` must be a real signal."""
        if sig == 0:
            raise ValueError('use probe_group() for signal 0')
        return self._signal(sig, group=True, states=('RUNNING', 'EXITED'))

    def probe_group(self):
        return self._signal(0, group=True, states=('RUNNING', 'EXITED'))

    def probe_leader(self):
        return self._signal(0, group=False, states=('RUNNING', 'EXITED'))

    def kill_leader(self, sig):
        return self._signal(sig, group=False, states=('RUNNING',))

    @classmethod
    def observe(cls, pgid):
        """Signal-0 observation of a bare group; never delivers a signal."""
        try:
            os.killpg(pgid, 0)
            return 'alive'
        except ProcessLookupError:
            return 'empty'
        except OSError:
            return 'blocked'

    @classmethod
    def observe_pid(cls, pid):
        """Three-state signal-0 observation; never delivers a signal.

        ``missing`` is the only state that proves the pid is gone.  ``blocked``
        (EPERM/EACCES or any other OS refusal) proves only that this process may
        not signal it, so callers must treat it as "cannot exclude alive", never
        as "cleaned up".
        """
        try:
            os.kill(pid, 0)
            return 'alive'
        except ProcessLookupError:
            return 'missing'
        except OSError:
            return 'blocked'


def group_probe_state(held):
    """State of our group while its leader is an unreaped zombie.

    macOS returns EPERM to ``killpg(pgid, 0)`` when the only signalable member
    is gone (measured 2026-10-26: Python 3.14.3 and Node v22.22.0 both EPERM;
    a live same-uid grandchild returns success/``alive``).  EPERM alone is not
    proof of emptiness: a live descendant we may not signal also yields EPERM.
    Only an independent member census that finds nothing besides this run's
    unreaped leader may be reported as ``empty``.  A census that cannot be
    completed (ESRCH race, truncation, enumeration failure), a changing census,
    or a denied leader probe is ``blocked`` and gets no signal.
    """
    outcome, detail = held.probe_group()
    if outcome == 'refused':
        return 'blocked', f'group probe refused: leader state {detail}'
    if outcome == 'missing':
        return 'empty', 'killpg0=ESRCH'
    if outcome == 'error':
        return 'blocked', detail
    if outcome == 'alive':
        return 'alive', None
    # outcome == 'denied' (EPERM): EPERM is not emptiness.
    live = live_group_members(held.pid)
    if live is None:
        return 'blocked', ('killpg0=EPERM; independent group census unavailable '
                           f'or incomplete: {detail}')
    live.discard(held.pid)
    leader, leader_detail = held.probe_leader()
    if leader == 'refused':
        return 'blocked', f'{detail}; leader probe refused: {leader_detail}'
    if leader != 'alive':
        return 'blocked', f'{detail}; leader probe: {leader} {leader_detail}; census={sorted(live)}'
    if live:
        return 'alive', f'killpg0=EPERM but census found live members {sorted(live)}: {detail}'
    return 'empty', (f'killpg0=EPERM with the unreaped zombie leader present '
                     f'and an empty live-member census: {detail}')


def probe_process_group(child, wait=5):
    """Probe the group owned by *child* without ever using a reaped pid.

    ``child`` was started with ``start_new_session``, so its pid is the pgid.
    The leader must be an unreaped zombie: its pid/pgid cannot be reused then,
    so ``killpg(pgid, ...)`` can only reach this run's descendants (plus the
    zombie itself).  Every actual signal goes through ``HeldProcessGroup``,
    which re-checks that ownership immediately before sending; signals happen
    before ``wait()`` and reaping is the last step.  A leader that is still
    running or was already reaped has no provable ownership, so it is reported
    NOT_VERIFIED and gets no signal.  A group whose signal probe is EPERM is
    only called empty after a complete, stable live-member census
    (``group_probe_state``) finds nothing but this run's unreaped leader; an
    unavailable/incomplete census is BLOCKED.  A live group is a finding
    (``GROUP_NOT_EMPTY``), not something to hide after remediation: record it,
    SIGTERM only that group, and never rewrite the status to VERIFIED just
    because our own SIGTERM eventually emptied it.
    """
    held = HeldProcessGroup(child)
    pgid = held.pid
    result = {'pgid': pgid, 'probe': 'waitid-WNOWAIT+killpg0',
              'signal': None, 'leaderUnreaped': False, 'leaderReaped': False}
    if wait_exit_unreaped(pgid, wait) != 'EXITED':
        result.update(status=CLEANUP_NOT_VERIFIED,
                      reason='leader is not an unreaped exited child; '
                             'pgid ownership unprovable, no signal sent')
        return result
    result['leaderUnreaped'] = True
    if sys.platform != 'darwin':
        # A filtered /proc can hide a live same-group descendant (hidepid,
        # EACCES/EPERM on a pid, a changing read) with no observable error.
        # Neither killpg0=EPERM nor killpg0=ESRCH can then prove the group is
        # empty, and Linux has no libproc-equivalent exact group census.  Do not
        # read a census and do not signal: unproven is BLOCKED, never VERIFIED.
        result.update(status=CLEANUP_BLOCKED,
                      reason='group membership completeness cannot be proven on '
                             + sys.platform + ' (filtered /proc); no census read, '
                             'no signal sent')
        try:
            child.wait(timeout=5)
            result['leaderReaped'] = True
        except subprocess.TimeoutExpired:
            result['leaderReaped'] = False
        return result
    try:
        state, detail = group_probe_state(held)
        if state == 'empty':
            result.update(status=CLEANUP_VERIFIED, reason=detail)
        elif state == 'blocked':
            result.update(status=CLEANUP_BLOCKED, reason=detail)
        else:
            result['status'] = CLEANUP_GROUP_NOT_EMPTY
            for sig in (signal.SIGTERM, signal.SIGKILL):
                outcome, info = held.send(sig)
                if outcome == 'sent':
                    result['signal'] = sig.name
                elif outcome == 'missing':
                    result['emptyAfterSignal'] = True
                    break
                elif outcome == 'refused':
                    result['signalRefused'] = info
                    result.update(status=CLEANUP_NOT_VERIFIED,
                                  reason='group signal refused after ownership was '
                                         f'lost: leader state {info}')
                    break
                else:
                    result['signalError'] = info
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    state, detail = group_probe_state(held)
                    if state == 'empty':
                        result['emptyAfterSignal'] = True
                        break
                    if state == 'blocked':
                        result['probeError'] = detail
                        break
                    time.sleep(.05)
                if result.get('emptyAfterSignal'):
                    break
            result.setdefault('emptyAfterSignal', False)
    finally:
        try:
            child.wait(timeout=5)
            result['leaderReaped'] = True
        except subprocess.TimeoutExpired:
            result['leaderReaped'] = False
    return result


def cleanup_status(audit):
    """A top-level result may never be rosier than the cleanup sub-check."""
    return ('PASS' if audit.get('processTreeCleanup') == CLEANUP_VERIFIED
            else CLEANUP_UNVERIFIED_STATUS)


def run_fixture(output, baseline, command=None):
    # ``command`` is a test seam for the cleanup controls; production always
    # launches the frozen A5.2a fixture.
    command = command or [sys.executable, '-I', str(ROOT / 'scripts/a5-19/run.py'),
                          '--baseline', str(baseline), '--output', str(output / 'six-cells')]
    audit = {'command': command, 'cwd': str(ROOT), 'launcherPid': os.getpid(),
             'processSnapshots': [], 'status': 'RUNNING', 'launcherAndFixtureReaped': False,
             'processTreeCleanup': CLEANUP_NOT_VERIFIED}
    save(output / 'launch.json', audit)
    child = None
    leader_state = 'RUNNING'
    try:
        with (output / 'launcher.log').open('w') as log:
            child = subprocess.Popen(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT,
                                     start_new_session=True)
            audit['pid'] = child.pid
            audit['pgid'] = child.pid
            deadline = time.monotonic() + 1250

            def sample():
                if not audit['processSnapshots'] or audit['processSnapshots'][-1]['status'] != 'BLOCKED':
                    audit['processSnapshots'].append(process_tree(child.pid))

            while True:
                # WNOWAIT leaves an exited leader a zombie: its pid/pgid stay
                # reserved for the group probe and signal steps below.  Polling
                # with child.poll()/wait() here would reap it and lose that proof.
                leader_state = wait_exit_unreaped(child.pid, 0)
                if leader_state == 'UNSUPPORTED':
                    # No WNOWAIT on this platform: still wait for the fixture,
                    # but never claim group ownership or signal a reaped pgid.
                    audit['leaderReapState'] = 'WNOWAIT_UNSUPPORTED'
                    while child.poll() is None:
                        sample()
                        if time.monotonic() >= deadline:
                            raise TimeoutError('A5.2a launcher exceeded 1250 seconds')
                        time.sleep(.2)
                    leader_state = 'REAPED'
                    break
                if leader_state != 'RUNNING':
                    break
                sample()
                if time.monotonic() >= deadline:
                    raise TimeoutError('A5.2a launcher exceeded 1250 seconds')
                time.sleep(.2)
            if leader_state == 'REAPED' and 'leaderReapState' not in audit:
                audit['leaderReapState'] = 'ALREADY_REAPED'
    finally:
        if child is not None:
            held = HeldProcessGroup(child)
            if leader_state == 'RUNNING':
                # The leader is still alive, so its pid is the pgid directly.
                # Every signal goes through the single ownership entry: it
                # re-checks waitid(WNOWAIT) immediately before sending and
                # refuses once the pid may have been reused.  Keep waiting
                # without reaping until the probe has run on a zombie.
                outcome, detail = held.send(signal.SIGTERM)
                if outcome == 'refused':
                    audit['cleanupBlocked'] = f'group signal refused: leader state {detail}'
                elif outcome == 'error':
                    audit['cleanupBlocked'] = detail
                leader_state = wait_exit_unreaped(child.pid, 5)
                if leader_state == 'RUNNING':
                    outcome, detail = held.send(signal.SIGKILL)
                    if outcome == 'refused':
                        audit['cleanupBlocked'] = f'group signal refused: leader state {detail}'
                    elif outcome == 'error':
                        audit['cleanupBlocked'] = detail
                    leader_state = wait_exit_unreaped(child.pid, 5)
                if leader_state == 'RUNNING':
                    audit['cleanupBlocked'] = 'launcher did not exit after SIGKILL'
            if leader_state == 'EXITED':
                # ps is only a census; the group probe is the independent cleanup
                # proof and the only input to processTreeCleanup.  It reaps the
                # leader last, after every signal has been sent.
                try:
                    audit['processGroupProbe'] = probe_process_group(child)
                except (OSError, subprocess.SubprocessError) as error:
                    audit['cleanupBlocked'] = f'group probe raised: {error}'
                    audit['processGroupProbe'] = {
                        'pgid': child.pid, 'status': CLEANUP_NOT_VERIFIED,
                        'reason': f'group probe raised: {type(error).__name__}: {error}'}
                audit['processTreeCleanup'] = audit['processGroupProbe']['status']
            else:
                audit['processTreeCleanup'] = CLEANUP_NOT_VERIFIED
                audit.setdefault('processGroupProbe', {
                    'pgid': child.pid, 'status': CLEANUP_NOT_VERIFIED,
                    'reason': 'leader was not an unreaped exited child; '
                              'ownership unprovable, no signal sent'})
            if any(s['status'] == 'BLOCKED' for s in audit['processSnapshots']):
                audit['psCensus'] = 'BLOCKED'
            elif audit['processSnapshots']:
                audit['psCensus'] = 'SAMPLED'
            else:
                audit['psCensus'] = 'NOT_SAMPLED'
            try:
                # Fixture evidence is read only after the group was probed and
                # reaped; a truncated/missing launch.json must not skip cleanup.
                # Never signal a bare pid after the leader's identity was lost.
                # The entry re-checks waitid(WNOWAIT) and refuses unless the
                # leader is still RUNNING, so the reap-state guard lives in one
                # place (the signal entry) instead of every caller.
                if child.returncode is None:
                    outcome, detail = held.kill_leader(signal.SIGKILL)
                    if outcome == 'error':
                        audit['cleanupBlocked'] = detail
                    elif outcome == 'refused':
                        audit['cleanupBlocked'] = f'leader kill refused: state {detail}'
                if child.returncode is None:
                    try:
                        child.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        audit['cleanupBlocked'] = 'launcher survived SIGKILL'
                    except ChildProcessError:
                        audit['cleanupBlocked'] = 'launcher was reaped by someone else'
                if child.returncode is not None:
                    audit['exitCode'] = child.returncode
                    audit['status'] = 'DONE' if child.returncode == 0 else 'FAIL'
                launch = output / 'six-cells/launch.json'
                audit['launcherAndFixtureReaped'] = False
                try:
                    if child.poll() is not None and launch.is_file():
                        fixture = read(launch)
                        audit['launcherAndFixtureReaped'] = fixture.get('childReaped') is True
                        fixture_command = fixture.get('command')
                        if (isinstance(fixture_command, list) and fixture_command
                                and all(isinstance(part, str) for part in fixture_command)):
                            audit['fixtureProcess'] = {
                                'pid': fixture.get('childPid'), 'ppid': child.pid,
                                'argv': fixture_command,
                                'executableSha256': digest(Path(fixture_command[0]))}
                        else:
                            audit['fixtureReadError'] = 'launch.json command missing/invalid'
                    elif not launch.is_file():
                        audit['fixtureReadError'] = 'launch.json missing'
                except (OSError, ValueError, KeyError, TypeError) as error:
                    audit['launcherAndFixtureReaped'] = False
                    audit['fixtureReadError'] = f'{type(error).__name__}: {error}'
            finally:
                save(output / 'launch.json', audit)
    return audit


def observation_check(payload, output, name='observation-check'):
    """Node bridge imports the original observation oracle, including pair checks."""
    source = output / (name + '.input.json')
    save(source, payload)
    proc = subprocess.run(['node', str(SCRIPT / 'session_observations.mjs'), str(source)],
                          capture_output=True, text=True, timeout=60)
    (output / (name + '.stdout')).write_text(proc.stdout)
    (output / (name + '.stderr')).write_text(proc.stderr)
    try:
        result = json.loads(proc.stdout)
    except ValueError as error:
        raise EvidenceError('OBSERVER_EXECUTION', proc.stderr or str(error)) from error
    require(proc.returncode in (0, 1) and result.get('status') in ('PASS', 'FAIL'),
            'OBSERVER_EXECUTION', 'observation bridge did not finish')
    if proc.returncode or result['status'] != 'PASS':
        raise EvidenceError(result.get('rule', 'OBSERVER_EXECUTION'), result.get('detail', 'observer failed'))
    return result


def calibration_coverage(registration, manifest):
    """Check candidate bookkeeping against our independently versioned case list."""
    contract = read(SCRIPT / 'fixtures/session-calibration.json')
    require([c['name'] for c in registration['cells']] == list(CELLS),
            'COVERAGE', 'all six preregistered cells required')
    require(all(c.get('registry') is c['name'].startswith('registry-') for c in registration['cells']),
            'CAPABILITY', 'registry configuration must match the fixed six-cell topology')
    require(registration['phases'] == ['model', 'tool'] and registration['repeats'] == REPEATS,
            'COVERAGE', 'model/tool x five repetitions required')
    expected = [f"{cell['name']}/{variant['name']}/{phase}"
                for cell in registration['cells'] for variant in cell['variants'] for phase in registration['phases']]
    require(expected == contract['cases'] and all(c['variants'] for c in registration['cells']),
            'COVERAGE', 'preregistration must match independent E1 case manifest; no denominator shrink')
    require(len(expected) == len(set(expected)) and expected,
            'COVERAGE', 'nonempty unique cases required')
    require(manifest['expectedCases'] == expected and manifest['executedCases'] == expected,
            'COVERAGE', 'missing/duplicate/extra cases')
    return expected


def validate_calibration(folder, output, baseline):
    raw = folder / 'raw'
    registration, manifest = read(raw / 'preregistration.json'), read(raw / 'manifest.json')
    expected = calibration_coverage(registration, manifest)
    require(manifest['baselineCommit'] == BASELINE_COMMIT and manifest['repeats'] == REPEATS,
            'IDENTITY', 'wrong baseline or repetition count')
    fixture_hashes = {f'scripts/a5-19/{name}': digest(ROOT / 'scripts/a5-19' / name) for name in FIXTURE_NAMES}
    require({row['path']: row['sha256'] for row in manifest['fixtureFiles']} == fixture_hashes,
            'IDENTITY', 'fixture sources differ from the measured manifest')
    require(bool(manifest['importedSources']), 'IDENTITY', 'empty baseline import manifest')
    for row in manifest['importedSources']:
        path = (baseline / row['path']).resolve()
        require(path.is_relative_to(baseline) and digest(path) == row['sha256'],
                'IDENTITY', 'imported baseline changed: ' + row['path'])
    observations, groups = [], []
    for cell in registration['cells']:
        data = read(raw / (cell['name'] + '.json'))
        case_ids = [case for case in expected if case.startswith(cell['name'] + '/')]
        expected_ids = [f'{case}/{i}' for case in case_ids for i in range(1, REPEATS + 1)]
        rows = data['observations']
        require(bool(rows) and data['cell'] == cell['name'] and data['baselineCommit'] == BASELINE_COMMIT
                and [o['id'] for o in rows] == expected_ids, 'COVERAGE', cell['name'] + ': missing/duplicate measurements')
        for observation, expected_id in zip(rows, expected_ids):
            _, variant, phase, repetition = expected_id.split('/')
            require(type(observation.get('repetition')) is int and observation['repetition'] == int(repetition),
                    'COVERAGE', expected_id + ': repetition does not match preregistered ID')
            require(observation.get('phase') == phase,
                    'COVERAGE', expected_id + ': phase does not match preregistered ID')
            require(isinstance(observation.get('variant'), dict) and observation['variant'].get('name') == variant,
                    'COVERAGE', expected_id + ': variant does not match preregistered ID')
            require(bool(observation['frames']) and bool(observation['timeline']),
                    'EMPTY_OBSERVER', observation['id'])
            require(observation['registry']['applicable'] is cell['registry'],
                    'CAPABILITY', observation['id'] + ': forged registry unavailable')
            require(any(r['kind'] == 'loop.final' for r in observation['timeline']),
                    'INDEPENDENT_ORACLE', observation['id'] + ': no loop final witness')
        observations.extend(rows)
        groups.append({'id': cell['name'], 'status': 'OBSERVED', 'measurements': len(rows),
                       'path': str(raw / (cell['name'] + '.json'))})
    require(manifest['totalMeasurements'] == len(observations), 'COVERAGE', 'total measurement mismatch')
    rounds = [{'repetition': i, 'status': 'OBSERVED',
               'observations': [o['id'] for o in observations if o['repetition'] == i]}
              for i in range(1, REPEATS + 1)]
    require(not observations or all(round_['observations'] for round_ in rounds),
            'COVERAGE', 'nonempty observations require nonempty repetition ledgers')
    oracle = observation_check({'observations': observations, 'controls': True}, output)
    controls = manifest['checks']['observerControls']
    require(len(controls) == 9 and len({c['name'] for c in controls}) == 9
            and all(c['detected'] is True and c['hitRule'] == c['expectedRule'] for c in controls),
            'CONTROLS', 'A5.2a original observation controls missing/failed')
    require(oracle['controls'] == controls, 'CONTROLS', 'recorded controls differ from independently rerun original controls')
    launch = read(folder / 'launch.json')
    require(launch.get('exitCode') == 0 and launch.get('childReaped') is True,
            'CLEANUP', 'A5.2a did not finish/reap its child')
    return {'groups': groups, 'expectedCases': expected, 'executedCases': manifest['executedCases'],
            'repeats': REPEATS, 'measurements': len(observations), 'oracle': oracle,
            'rounds': rounds,
            'controls': controls, 'unstableGroups': manifest['unstableGroups'],
            'sidecar': {'status': 'NOT_APPLICABLE', 'reason': 'controlled in-process TS loop; no sidecar binary'},
            'connections': {'count': 0, 'reason': 'A5.2a prohibits listen; in-process queue observation'},
            'surfaces': {'output': 'frames', 'disk': ['durableStatus', 'registry'],
                         'http': {'status': 'NOT_APPLICABLE', 'reason': 'A5.2a in-process scope'}},
            'sourceManifest': str(raw / 'manifest.json'),
            'evidenceSha256': {p.name: digest(p) for p in sorted(raw.glob('*.json'))}}


def audit_native(evidence, contract):
    """Fail-closed contract for future A7 recorder integration, not an executor.

    contract is prepared by the launcher BEFORE execution, never copied from a
    candidate's report. execEvents must include exited children, and calls must
    come from the independent egress observer. A ps snapshot alone is insufficient.
    Current CLI deliberately cannot certify a native run; tests use labelled fixtures.
    """
    require(evidence['topology'] == 'native' and contract['host'] == 'rust',
            'NATIVE_TOPOLOGY', 'Rust host required for a native declaration')
    processes = evidence['processes']
    require(bool(processes), 'PROCESS_WITNESS', 'empty process tree')
    # Inspect the recorded argv as well as the launch identity. Labels alone cannot allow a process.
    for process in processes:
        argv = process['argv']
        require(isinstance(argv, list) and argv and all(isinstance(a, str) for a in argv),
                'PROCESS_WITNESS', 'missing argv')
        if process.get('kind') == 'ts-gateway' or any('gateway-cli' in a or 'host-helper' in a for a in argv):
            raise EvidenceError('TS_HOST', 'TS gateway/internal helper recorded in native topology')
    require(evidence['execWitness']['complete'] is True and evidence['execWitness']['source'] == 'exec-monitor',
            'PROCESS_WITNESS', 'complete exec/exit witness required, not sampled ps')
    actual_pids = [p['pid'] for p in processes]
    require(all(type(pid) is int and pid > 0 for pid in actual_pids)
            and len(set(actual_pids)) == len(actual_pids), 'PROCESS_WITNESS', 'invalid/duplicate PID')
    roles = Counter(p['role'] for p in processes)
    require(roles == Counter(contract['processRoles']), 'NATIVE_PROCESS', 'unexpected/missing process roles')
    for process in processes:
        allowed = contract['binaries'][process['role']]
        require(process['argv'] == allowed['argv'] and process['sha256'] == allowed['sha256']
                and process['executable'] == allowed['path']
                and digest(Path(allowed['path'])) == allowed['sha256'],
                'NATIVE_PROCESS', 'exec identity/argv/hash differs from pre-launch identity')
        expected_parent = contract['launcherPid'] if process['role'] == 'host' else next(p['pid'] for p in processes if p['role'] == 'host')
        require(process['ppid'] == expected_parent, 'PROCESS_WITNESS', 'wrong parent PID')
    live, started, exited = set(), set(), set()
    by_pid = {p['pid']: p for p in processes}
    for row in evidence['execWitness']['events']:
        pid = row['pid']
        require(pid in by_pid and row['event'] in ('exec', 'exit'),
                'PROCESS_WITNESS', 'unknown process/event, including short-lived children')
        if row['event'] == 'exec':
            require(pid not in started and all(row.get(k) == by_pid[pid][k]
                    for k in ('ppid', 'argv', 'executable', 'sha256')),
                    'PROCESS_WITNESS', 'exec identity disagrees with recorded launch identity')
            require(row['ppid'] == contract['launcherPid'] or row['ppid'] in live,
                    'PROCESS_WITNESS', 'child exec before parent or after parent exit')
            live.add(pid)
            started.add(pid)
        else:
            require(pid in live, 'PROCESS_WITNESS', 'exit before exec or duplicate exit')
            live.remove(pid)
            exited.add(pid)
    require(not live and started == exited == set(actual_pids), 'PROCESS_WITNESS', 'missing exec/exit records')
    require(evidence['networkWitness']['complete'] is True, 'NETWORK_WITNESS', 'egress observer incomplete')
    for call in evidence['networkWitness']['calls']:
        require(call['target'] in contract['externalEndpoints'] and call['ownerPid'] in actual_pids,
                'HIDDEN_HELPER', 'undeclared helper/endpoint invocation')
    require(evidence['expectedCases'] == contract['cases'] and evidence['executedCases'] == contract['cases']
            and len(set(contract['cases'])) == len(contract['cases']) and contract['cases'],
            'COVERAGE', 'missing/duplicate/extra native groups')
    require(evidence['capabilities'] == contract['capabilities'], 'CAPABILITY', 'configured capability forged unavailable')
    require(evidence['cleanup']['allStopped'] is True, 'CLEANUP', 'owned processes not stopped')
    require(set(evidence['surfaces']) == {'output', 'disk', 'http'}, 'SURFACES', 'output/disk/HTTP accounting required')
    for name, surface in evidence['surfaces'].items():
        require(surface['status'] == 'RECORDED' and surface['artifacts'], 'SURFACES', name + ' missing')
        for artifact in surface['artifacts']:
            require(digest(Path(artifact['path'])) == artifact['sha256'], 'SURFACES', name + ' artifact changed')
    observer = evidence['observer']
    require(bool(observer['rows']), 'EMPTY_OBSERVER', 'empty independent observer')
    require(observer['connections'] == len({r['connection'] for r in observer['rows']})
            and observer['connections'] == contract['connections'], 'CONNECTIONS', 'connection count mismatch')
    require(evidence['binding'] == contract['binding'], 'BINDING', 'wrong/old interaction binding')
    # Validate bytes using the existing independent TCP decoder and wire checker.
    for row in observer['rows']:
        wire_frame(row)
        decoded = []
        decoder = Decoder(decoded.append, row['direction'])
        decoder.feed(b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n' + bytes.fromhex(row['wireHex']))
        require(len(decoded) == 1 and decoded[0].get('frame') == row['frame'],
                'OBSERVER_WIRE', 'wire observation disagrees with recorder')
    hellos = [r['frame'] for r in observer['rows'] if r['frame'].get('type') == 'hello_ok']
    require(bool(hellos), 'BINDING', 'actual server hello/binding missing')
    for frame in hellos:
        binding = frame.get('interactionBinding', {})
        require(frame.get('protocolVersion') == '1.2' and type(binding.get('generation')) is int
                and binding['generation'] >= 1 and isinstance(binding.get('connectionId'), str)
                and bool(binding['connectionId']) and binding == contract['binding'],
                'BINDING', 'actual server hello has invalid/wrong/old binding')
    require(observer['rows'] == contract['observerRows'], 'OBSERVER_DIFFERENCE', 'observer differs from independent driver')


def deadline_report():
    cells = []
    for host, sidecar in MATRIX:
        cells.append({'id': f'{host}-host/{sidecar}-sidecar', 'host': host, 'sidecar': sidecar,
                      'topology': 'native' if host == 'rust' else 'ts-reference',
                      'status': 'NOT_RUN', 'expectedGroups': list(CELLS),
                      'expectedCases': read(SCRIPT / 'fixtures/session-calibration.json')['cases'], 'executedCases': [],
                      'repeats': REPEATS, 'rounds': [], 'processes': [], 'connections': None,
                      'sidecarSha256': None, 'output': None, 'disk': None, 'http': None,
                      'oracle': 'NOT_RUN', 'cleanup': 'NOT_RUN'})
    result = {'status': 'NOT_RUN', 'structureStatus': 'PASS', 'cells': cells,
            'reason': 'A7 native host/driver/exec+egress witnesses and A5.4 frozen expectations are not ready',
            'executionStatus': 'NOT_RUN', 'acceptanceStatus': 'NOT_RUN'}
    require([(c['host'], c['sidecar']) for c in cells] == list(MATRIX)
            and all(c['status'] == 'NOT_RUN' and not c['executedCases'] and c['expectedCases'] for c in cells),
            'DEADLINE_STRUCTURE', 'four non-executed matrix cells required')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suite', choices=('calibrate', 'deadline'), required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--baseline', type=Path, default=BASELINE)
    args = parser.parse_args()
    takeovers = []
    install_shutdown()
    try:
        with parity_lock(timeout=30, takeovers=takeovers):
            missing = [name for name in E1_SOURCES if not (args.baseline / name).is_file()] if args.suite == 'calibrate' else []
            if missing:
                # 缺 baseline 是前置条件不满足：BLOCKED 且不建输出目录（#187）。按 E1 实际导入的源码入口核对，不要求 dist
                print(json.dumps({'suite': args.suite, 'status': 'BLOCKED',
                                  'error': f'baseline at {args.baseline.resolve()} lacks E1 sources {missing[:3]}; '
                                           'pass --baseline or set PDRS_BASELINE'}, ensure_ascii=False))
                return 2
            output = args.output.resolve()
            # E1 drives the baseline TS source through the baseline runtime; bind the
            # bytes of tsx/ws (and their declared dependencies) so a swapped loader or
            # ws cannot certify a calibration (#85 85.1).
            dependencies = None
            if args.suite == 'calibrate':
                try:
                    dependencies = baseline_dependency_hashes(args.baseline.resolve())
                except ValueError as error:
                    print(json.dumps({'suite': args.suite, 'status': 'BLOCKED', 'error': str(error)},
                                     ensure_ascii=False))
                    return 2
            output.mkdir(parents=True, exist_ok=False)
            report = {'suite': args.suite, 'scope': 'A5.3 draft apparatus', 'status': 'RUNNING',
                      'acceptanceStatus': 'NOT_RUN', 'apparatusSha256': hashes(), 'frozen': frozen_check()}
            if dependencies is not None:
                report['baselineDepsSha256'] = dependencies
            save(output / 'report.json', report)
            code = 0
            try:
                if args.suite == 'deadline':
                    report.update(deadline_report())
                    _, code = common.aggregate_status(report['status'])
                else:
                    report['lockTakeovers'] = takeovers
                    audit = run_fixture(output, args.baseline.resolve())
                    report['launch'] = audit
                    require(audit['exitCode'] == 0, 'CALIBRATION_EXECUTION', 'see launcher.log and six-cells/run.log')
                    require(audit['launcherAndFixtureReaped'], 'CLEANUP', 'launcher/fixture cleanup incomplete')
                    report.update(validate_calibration(output / 'six-cells', output, args.baseline.resolve()))
                    # Imported lazily to keep the independent negative-control CLI usable.
                    try:
                        controls = _load('session_controls', SCRIPT / 'controls/session_domain.py')
                        _verify_cli_imports()  # Late imports must be checked before controls execute.
                    except (Exception, KeyboardInterrupt) as error:
                        raise ApparatusVerificationError(str(error)) from error
                    report['e1Controls'] = controls.run_all(output / 'controls', output / 'six-cells')
                    controls_status = report['e1Controls']['status']
                    require(controls_status in ('PASS', 'PASS_WITH_SKIPS'),
                            'CONTROLS', 'E1 mutation control failed')
                    overall = cleanup_status(audit)
                    if controls_status == 'PASS_WITH_SKIPS':
                        # A skipped positive control is not proof of the batch.
                        # Keep the skipped names visible and never report PASS:
                        # BLOCKED / exit 2 is the apparatus' non-PASS convention.
                        report['controlsSkipped'] = (
                            report['e1Controls'].get('cleanupSkipped', [])
                            + [row['case'] for row in report['e1Controls'].get('notRun', [])])
                        report['failure'] = {
                            'rule': 'CONTROLS_SKIPPED',
                            'detail': 'E1 controls skipped platform-specific positives; not PASS'}
                        overall = 'BLOCKED'
                    report['status'] = overall
                    report['executionStatus'] = overall
                    report['cleanupStatus'] = audit['processTreeCleanup']
                    report['topology'] = 'ts-in-process-controlled'
                    report['acceptanceStatus'] = 'NOT_RUN'
                    report['calibrationStatus'] = 'OBSERVED'
                    # Cleanup unverified is not a freeze candidate even though the
                    # measurements completed; downstream only-PASS consumers must
                    # see a non-PASS string, not a fresh DRAFT_AWAITING_A5_4.
                    report['freezeStatus'] = ('DRAFT_AWAITING_A5_4' if overall == 'PASS'
                                              else CLEANUP_UNVERIFIED_FREEZE)
                    # Single shared exit-code mapping: only PASS maps to 0; a domain
                    # status outside the batch order (PASS_WITH_CLEANUP_UNVERIFIED)
                    # is BLOCKED, never an optimistic 0.
                    _, code = common.aggregate_status(
                        overall if overall in common.BATCH_STATUS_ORDER else 'BLOCKED')
                require(report['apparatusSha256'] == hashes(), 'APPARATUS_CHANGED', 'apparatus changed while collecting evidence')
                if dependencies is not None:
                    require(baseline_dependency_hashes(args.baseline.resolve()) == dependencies,
                            'BASELINE_DEPS_CHANGED', 'baseline runtime dependencies changed while collecting evidence')
                require(report['frozen'] == frozen_check(), 'FROZEN_CHANGED', 'frozen inputs changed during suite')
                if args.suite == 'calibrate' and report['frozen']['exitCode'] != 0:
                    report['status'], code = common.aggregate_status('BLOCKED')
                    report['freezeStatus'] = 'BLOCKED'
                    report['failure'] = {'rule': 'FROZEN', 'detail': 'measurements completed; see frozen check output; no acceptance PASS'}
            except (EvidenceError, OSError, ValueError, KeyError, TypeError, StopIteration,
                    subprocess.SubprocessError, TimeoutError, KeyboardInterrupt) as error:
                report['status'] = 'BLOCKED' if isinstance(error, (ApparatusVerificationError, PermissionError, TimeoutError, KeyboardInterrupt)) else 'FAIL'
                report['failure'] = {'rule': getattr(error, 'rule', 'EVIDENCE'), 'detail': str(error)}
                code = common.aggregate_status(report['status'])[1]
            save(output / 'report.json', report)
            print(json.dumps({'status': report['status'], 'suite': args.suite, 'report': str(output / 'report.json'),
                              'failure': report.get('failure'), 'acceptanceStatus': report['acceptanceStatus']}, ensure_ascii=False))
            return code
    except (TimeoutError, KeyboardInterrupt, ValueError, OSError, subprocess.SubprocessError) as error:
        print(json.dumps({'suite': args.suite, 'status': 'BLOCKED', 'error': str(error)}, ensure_ascii=False))
        return 2


if __name__ == '__main__':
    run_entry(main, 'session')

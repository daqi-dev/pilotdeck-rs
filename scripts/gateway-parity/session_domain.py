#!/usr/bin/env python3
"""E1 draft apparatus. Calibration is TS-only; A7 native scoring is wired (#1140).

The deadline suite scores one native witness capture (rust host + rust
sidecar) against the A19-HOST expectations registered in
fixtures/session-expectations.json; matrix cells without witness evidence
stay NOT_RUN and the shared aggregation cannot report PASS. No A2 proxy
round is relabelled native: audit functions consume independent launcher/
exec/network evidence, and their mutation tests are apparatus tests, never
evidence that a Rust host has run or retired the TS host.
"""
# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)
# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  It must run before every other standard-library import: a
# shadowed argparse/pathlib would otherwise execute before this check.  ``sys``
# is builtin; os is imported only after the frozen-module preflight below.
import sys

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
import re
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request


ROOT = common.ROOT
SCRIPT = common.SCRIPT
apparatus_hashes = common.apparatus_hashes
baseline_dependency_hashes = common.baseline_dependency_hashes
digest = common.digest
install_shutdown = common.install_shutdown
parity_lock = common.verified_parity_lock
save = common.save
# #209 E1: the shared lease is released only after this run's process groups
# are proven stopped.  Reuse the A2 primitives instead of a second registry.
deferred_signals = common.deferred_signals
unblock_child_signals = common.unblock_child_signals
compare_module = _load('compare', common.SCRIPT / 'compare.py')
wire_frame = compare_module.wire_frame
observer_module = _load('observer', common.SCRIPT / 'observer.py')
Decoder = observer_module.Decoder
native_evidence = _load('native_evidence', common.SCRIPT / 'native_evidence.py')

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
# A19-HOST per-case expectation registration (issue #1177): the labels the D4-B
# canonical outputs are built from.  S(T)/E(T) are the single timeout status and
# error set; U is the result_unknown responsibility record that replaces a failed
# registry append on the real-time stream.
A19_TIMEOUT_STATUS = 'agent_status:turn_timeout'
A19_TIMEOUT_ERROR = 'error:turn_timeout'
A19_RESULT_UNKNOWN = 'error:result_unknown'
# Closed class set of the D4-B derivation.  ``ts-equal`` covers every non
# timeout-first ordering; the other classes name the T-side write that failed.
A19_CLASSES = ('ts-equal',
               'no-registry-timeout-final',
               'no-registry-timeout-status-failed',
               'registry-timeout-final',
               'registry-timeout-status-failed',
               'registry-timeout-error-append-failed',
               'registry-timeout-status-append-failed')
# Canonical rust output per class, derived from the human-approved D4-B wording
# (state/decisions-20261014.md, 2026-10-15) plus the TS dropped-enqueue evidence
# in exp014: rust publishes the frames TS dropped, minus both gateway_submit_failed
# events, minus a result_unknown caused by a failed G-side append.  ``eventLabels``
# are anchored to the TS-side first after-prefix registry seq (model 4 / tool 5);
# ``None`` marks the two no-registry classes.
A19_CANONICAL_RUST = {
    'no-registry-timeout-final': {
        'frames': (A19_TIMEOUT_STATUS, A19_TIMEOUT_ERROR),
        'durableStatus': ('turn_timeout',), 'eventLabels': None},
    'no-registry-timeout-status-failed': {
        'frames': (A19_TIMEOUT_ERROR,),
        'durableStatus': (), 'eventLabels': None},
    'registry-timeout-final': {
        'frames': (A19_TIMEOUT_STATUS, A19_TIMEOUT_ERROR),
        'durableStatus': ('turn_timeout',),
        'eventLabels': (A19_TIMEOUT_STATUS, A19_TIMEOUT_ERROR),
        'state': 'failed', 'result': A19_TIMEOUT_ERROR},
    'registry-timeout-status-failed': {
        'frames': (A19_TIMEOUT_ERROR,),
        'durableStatus': (),
        'eventLabels': (A19_TIMEOUT_ERROR,),
        'state': 'failed', 'result': A19_TIMEOUT_ERROR},
    'registry-timeout-error-append-failed': {
        'frames': (A19_TIMEOUT_STATUS, A19_RESULT_UNKNOWN),
        'durableStatus': ('turn_timeout',),
        'eventLabels': (A19_TIMEOUT_STATUS,),
        'state': 'running', 'result': None},
    'registry-timeout-status-append-failed': {
        'frames': (A19_RESULT_UNKNOWN, A19_TIMEOUT_ERROR),
        'durableStatus': ('turn_timeout',),
        'eventLabels': (A19_TIMEOUT_ERROR,),
        'state': 'failed', 'result': A19_TIMEOUT_ERROR},
}
# Surfaces where the registered rust expectation may differ from the registered
# ts measurement, per class.  This is the machine scope of the A19-HOST-001 diff
# entry; anything differing outside it is out of the approved difference.
A19_ALLOWED_SURFACES = {
    'ts-equal': (),
    'no-registry-timeout-final': ('durableStatus',),
    'no-registry-timeout-status-failed': ('durableStatus',),
    'registry-timeout-final': ('frames', 'durableStatus', 'registry.events', 'registry.result'),
    'registry-timeout-status-failed': ('frames', 'durableStatus', 'registry.events', 'registry.result'),
    'registry-timeout-error-append-failed': ('frames', 'durableStatus', 'registry.events',
                                             'registry.state', 'registry.result'),
    'registry-timeout-status-append-failed': ('frames', 'durableStatus', 'registry.events', 'registry.result'),
}


class EvidenceError(ValueError):
    def __init__(self, rule, detail):
        super().__init__(detail)
        self.rule = rule


class ApparatusVerificationError(EvidenceError):
    """The instrument is untrusted; this is not a parity result."""

    def __init__(self, detail):
        super().__init__('APPARATUS_IMPORTS', detail)


class RetirementLaunchError(EvidenceError):
    """A retirement round never started; retryable on the alternate port set.

    A4 (#1355 fix-r2): a launch-phase failure (bind TOCTOU between the
    pre-check and the real bind, readiness timeout, early process exit) used
    to escape as a bare RuntimeError that main's except tuple did not catch,
    skipping the report write and leaving report.json at RUNNING.
    """

    def __init__(self, detail):
        super().__init__('ROUND_LAUNCH', detail)


# ② (#1355 fix-r3): a TimeoutExpired is a declared-budget bound, not a scored
# FAIL — subprocess.TimeoutExpired is a SubprocessError subclass (main's except
# tuple catches it), but it is not a TimeoutError, so the old BLOCKED
# classification tuple let it fall through to status=FAIL.
BLOCKED_APPARATUS_ERRORS = (ApparatusVerificationError, PermissionError, TimeoutError,
                            KeyboardInterrupt, subprocess.TimeoutExpired)


def require(condition, rule, detail):
    if not condition:
        raise EvidenceError(rule, detail)


def read(path):
    return json.loads(Path(path).read_text())


def hashes():
    # E1 includes its transitive observation rules and the reused A2 primitives.
    # Executable dependencies come solely from common.A2_SCRIPTS.
    # R2 (#1355 fix-r2): the retirement fixture is an apparatus input, not just
    # a DATA_FILES whitelist row — it feeds apparatusSha256, so an in-flight
    # params edit can no longer slip past APPARATUS_CHANGED.
    paths = [SCRIPT / name for name in ('fixtures/session-calibration.json',
                                        'fixtures/session-expectations.json',
                                        'fixtures/retirement-methods.json')]
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


def _cleanup_fixture(child, audit, leader_state, output):
    """Attempt every cleanup step; the caller proves absence before release.

    Runs inside ``run_fixture``'s deferred-signal window.  It never discards
    the group registration itself: only the caller, after this returns and
    ``child.returncode``/``cleanupBlocked`` show the leader is settled, may
    remove the pgid from ``common.LIVE_GROUPS`` (#209 E1).
    """
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


def fixture_lease_stopped(child, audit):
    """Prove lease release separately from the frozen cleanup score (#1221).

    A non-Darwin census stays BLOCKED even after a clean exit. Like A2's
    stop_owned, only final ESRCH after reaping our held leader proves that its
    group is gone. This is a signal-0 observation, never TERM/KILL after reap;
    PID reuse can only retain the lease conservatively. No stale lease takeover
    or scoring field is changed. Unknown ownership/cleanup errors fail closed.
    """
    probe = audit.get('processGroupProbe', {})
    if (child.pid not in common.LIVE_GROUPS
            or child.returncode is None
            or probe.get('pgid') != child.pid
            or probe.get('leaderUnreaped') is not True
            or probe.get('leaderReaped') is not True
            or audit.get('cleanupBlocked')
            or audit.get('cleanupRaised')):
        return False
    deadline = time.monotonic() + 2
    while True:
        try:
            os.killpg(child.pid, 0)
        except ProcessLookupError:
            audit['leaseGroupProbe'] = 'ESRCH after held leader reap'
            return True
        except OSError as error:
            audit['leaseGroupProbe'] = f'unverified: {type(error).__name__}: {error}'
            return False
        if time.monotonic() >= deadline:
            audit['leaseGroupProbe'] = 'group still exists after 2 seconds'
            return False
        time.sleep(.02)


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
            # Spawn and registration share one deferred-signal window (#209 E1):
            # a signal delivered between Popen and the in-memory ownership record
            # must not leave a group the shared lock cannot account for.
            with deferred_signals():
                child = subprocess.Popen(command, cwd=ROOT, stdout=log,
                                         stderr=subprocess.STDOUT, start_new_session=True,
                                         preexec_fn=unblock_child_signals)
                common.LIVE_GROUPS.add(child.pid)
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
        # Cleanup is the one window where an interrupt must not abandon the
        # remaining groups: defer SIGINT/SIGTERM until the audit is settled.
        with deferred_signals():
            try:
                if child is not None:
                    _cleanup_fixture(child, audit, leader_state, output)
            except BaseException as error:
                # A cleanup helper failed before it could publish a status.  Keep
                # the group registered and record the failure in a lock-lifecycle
                # field only: the frozen scoring fields are never rewritten here
                # (#209 E1 P0-1).  Re-raise so main()'s existing failure contract
                # decides FAIL/BLOCKED instead of publishing an optimistic PASS.
                audit['cleanupRaised'] = f'{type(error).__name__}: {error}'
                raise
            finally:
                if child is not None:
                    if child.returncode is not None:
                        audit['exitCode'] = child.returncode
                        audit['status'] = 'DONE' if child.returncode == 0 else 'FAIL'
                    # Lock lifecycle only.  ``processTreeCleanup`` keeps exactly
                    # the value the frozen scoring block wrote; a verified probe
                    # still needs a reaped leader and no cleanup error before the
                    # shared lease may be released (#209 E1 P0-1).
                    verified = (audit.get('processTreeCleanup') == CLEANUP_VERIFIED
                                and child.returncode is not None
                                and not audit.get('cleanupBlocked')
                                and not audit.get('cleanupRaised'))
                    # BLOCKED/GROUP_NOT_EMPTY still describe the observation,
                    # while an independently proven empty group may release its
                    # lease. Do not promote the frozen score after remediation.
                    verified = verified or fixture_lease_stopped(child, audit)
                    audit['allStopped'] = verified
                    if verified:
                        common.LIVE_GROUPS.discard(child.pid)
                    audit['ownedGroupsRemaining'] = sorted(common.LIVE_GROUPS)
                else:
                    audit['allStopped'] = (not common.LIVE_GROUPS
                                           and not audit.get('cleanupRaised'))
                    audit['ownedGroupsRemaining'] = sorted(common.LIVE_GROUPS)
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


SESSION_PREFIX_TYPES = frozenset(('turn_started', 'input_accepted', 'model_request_started',
                                 'tool_call_started'))


def _session_label(event):
    """Mirror of the fixture's ``label``: type plus the first present detail."""
    detail = event.get('event')
    if detail is None:
        detail = event.get('code')
    if detail is None:
        detail = event.get('finishReason')
    if detail is None:
        detail = ''
    return f"{event['type']}:{detail}"


def project_observation(observation):
    """Independent Python projection of one raw observation onto the three
    registered surfaces, mirroring the fixture's ``summarize`` (frames after
    prefix, durable statuses, registry events/state/result after prefix)."""
    frames = [_session_label(event) for event in observation['frames']
              if event['type'] not in SESSION_PREFIX_TYPES]
    durable = [entry['event'] for entry in observation['durableStatus']['entries']]
    registry = observation['registry']
    if registry.get('applicable'):
        events = [f"{event['seq']}:{_session_label(event['event'])}"
                  for event in registry['events']
                  if event['event']['type'] not in SESSION_PREFIX_TYPES]
        record = registry['record']
        result = record.get('result')
        # A missing state is an observed surface mismatch for this case, never
        # a batch-level KeyError (#1140 fix-r1): the projection scores it on
        # registry.state so every other case keeps its verdict.
        projected = {'applicable': True, 'events': events, 'state': record.get('state'),
                    'result': None if result is None else _session_label(result)}
    else:
        projected = {'applicable': False}
    return {'frames': frames, 'durableStatus': durable, 'registry': projected}


def expectation_class(cell, variant):
    """D4-B class of a preregistered variant, from the variant's fault config."""
    if variant.get('order', 'timeout-first') != 'timeout-first':
        return 'ts-equal'
    if cell.startswith('registry-'):
        if variant.get('failStatus') == 'turn_timeout':
            return 'registry-timeout-status-failed'
        if variant.get('failAppend') == 'error:turn_timeout':
            return 'registry-timeout-error-append-failed'
        if variant.get('failAppend') == 'agent_status:turn_timeout':
            return 'registry-timeout-status-append-failed'
        return 'registry-timeout-final'
    return ('no-registry-timeout-status-failed'
            if variant.get('failStatus') == 'turn_timeout' else 'no-registry-timeout-final')


def _valid_registry_event(item):
    """A registered registry event is a ``<seq>:<label>`` string with a non-empty label."""
    if not isinstance(item, str):
        return False
    head, separator, label = item.partition(':')
    return separator == ':' and head.isdigit() and bool(label)


def _expectation_structure(row):
    """Shape of one registered case; None when healthy, else the offending field."""
    if not isinstance(row, dict) or set(row) != {'case', 'a19Class', 'ts', 'rust'}:
        return 'case keys'
    if row['a19Class'] not in A19_CLASSES:
        return 'a19Class'
    for side in ('ts', 'rust'):
        value = row[side]
        if not isinstance(value, dict) or set(value) != {'frames', 'durableStatus', 'registry'}:
            return side + ' keys'
        if not (isinstance(value['frames'], list)
                and all(isinstance(item, str) and item for item in value['frames'])):
            return side + ' frames'
        if not (isinstance(value['durableStatus'], list)
                and all(isinstance(item, str) and item for item in value['durableStatus'])):
            return side + ' durableStatus'
        registry = value['registry']
        if not isinstance(registry, dict):
            return side + ' registry'
        if registry.get('applicable') is False:
            if set(registry) != {'applicable'}:
                return side + ' registry keys'
        elif registry.get('applicable') is True:
            if set(registry) != {'applicable', 'events', 'state', 'result'}:
                return side + ' registry keys'
            if not (isinstance(registry['events'], list)
                    and all(_valid_registry_event(item) for item in registry['events'])):
                return side + ' registry events'
            if not isinstance(registry['state'], str) or not registry['state']:
                return side + ' registry state'
            if registry['result'] is not None and not isinstance(registry['result'], str):
                return side + ' registry result'
        else:
            return side + ' registry applicable'
    if row['ts']['registry']['applicable'] != row['rust']['registry']['applicable']:
        return 'registry applicability'
    return None


def _expectation_diff_surfaces(ts, rust):
    """Surfaces on which the registered rust expectation differs from the ts one.

    #2039: these are JSON values, so the comparisons go through the strict
    ``same_json`` walk (#1372), never Python's ``==``: a boolean that drifted
    into a number must surface as a difference instead of passing silently.
    """
    surfaces = []
    if not native_evidence.same_json(ts['frames'], rust['frames']):
        surfaces.append('frames')
    if not native_evidence.same_json(ts['durableStatus'], rust['durableStatus']):
        surfaces.append('durableStatus')
    expected, registered = ts['registry'], rust['registry']
    if not native_evidence.same_json(expected.get('applicable'), registered.get('applicable')):
        surfaces.append('registry.applicable')
    elif expected.get('applicable'):
        for field in ('events', 'state', 'result'):
            if not native_evidence.same_json(expected.get(field), registered.get(field)):
                surfaces.append('registry.' + field)
    return surfaces


def validate_expectations(registration, observations):
    """A5.4 structural registration check for the A19-HOST expectations.

    Never scores a native run: it only pins the registration itself.  (1) the
    case set is one-to-one with the calibration manifest, (2) every registered
    ts expectation equals the projected measurement for all repetitions,
    (3) rust differs from ts only on the surfaces the A19-HOST entry lists,
    (4) rust equals the D4-B canonical output derived from the variant's fault
    configuration, so an edit inside the allowed scope is still caught.
    """
    contract = read(SCRIPT / 'fixtures/session-calibration.json')
    document = read(SCRIPT / 'fixtures/session-expectations.json')
    require(isinstance(document, dict)
            and isinstance(document.get('cases'), list)
            and document.get('baselineCommit') == BASELINE_COMMIT
            and document.get('repeats') == REPEATS
            and document.get('phases') == ['model', 'tool'],
            'EXPECTATION_STRUCTURE', 'expectation document header')
    rows = document['cases']
    require([row.get('case') if isinstance(row, dict) else None for row in rows] == contract['cases'],
            'EXPECTATION_COVERAGE',
            'expectation cases must equal the independent E1 case manifest one-to-one')
    for row in rows:
        problem = _expectation_structure(row)
        require(problem is None, 'EXPECTATION_STRUCTURE', f"{row['case']}: {problem}")
    variants = {(cell['name'], variant['name']): variant
                for cell in registration['cells'] for variant in cell['variants']}
    registered = {row['case']: row for row in rows}
    for case, row in registered.items():
        cell, variant, _phase = case.split('/')
        require(row['a19Class'] == expectation_class(cell, variants[(cell, variant)]),
                'EXPECTATION_CLASS', case + ': a19Class disagrees with the preregistered fault configuration')
    measured = {}
    for observation in observations:
        case = '/'.join(observation['id'].split('/')[:3])
        projection = project_observation(observation)
        if case in measured:
            require(measured[case] == projection, 'EXPECTATION_TS_DRIFT',
                    case + ': repetitions disagree with each other')
        else:
            measured[case] = projection
    require(set(measured) == set(registered), 'EXPECTATION_COVERAGE',
            'measured cases must equal the registered expectation cases')
    for case, row in registered.items():
        require(row['ts'] == measured[case], 'EXPECTATION_TS_DRIFT',
                case + ': ts expectation drifted from the calibrate measurement')
        surfaces = _expectation_diff_surfaces(row['ts'], row['rust'])
        require(all(surface in A19_ALLOWED_SURFACES[row['a19Class']] for surface in surfaces),
                'EXPECTATION_SCOPE',
                case + ': rust differs from ts outside the A19-HOST listed surfaces: '
                + ','.join(surfaces))
        if row['a19Class'] == 'ts-equal':
            require(row['rust'] == row['ts'], 'EXPECTATION_RUST_DERIVATION',
                    case + ': non-timeout-first ordering must match TS exactly')
            continue
        canonical = A19_CANONICAL_RUST[row['a19Class']]
        require(tuple(row['rust']['frames']) == canonical['frames'], 'EXPECTATION_RUST_DERIVATION',
                case + ': rust frames differ from the D4-B canonical output')
        require(tuple(row['rust']['durableStatus']) == canonical['durableStatus'],
                'EXPECTATION_RUST_DERIVATION',
                case + ': rust durable statuses differ from the D4-B canonical output')
        registry = row['rust']['registry']
        if canonical['eventLabels'] is None:
            require(registry == {'applicable': False}, 'EXPECTATION_RUST_DERIVATION',
                    case + ': rust registry must be inapplicable without a run registry')
        else:
            first = int(row['ts']['registry']['events'][0].split(':', 1)[0])
            expected_events = [f'{first + index}:{label}'
                               for index, label in enumerate(canonical['eventLabels'])]
            require(registry['applicable'] is True and registry['events'] == expected_events,
                    'EXPECTATION_RUST_DERIVATION',
                    case + ': rust registry events differ from the D4-B canonical output')
            require(registry['state'] == canonical['state'], 'EXPECTATION_RUST_DERIVATION',
                    case + ': rust registry state differs from the D4-B canonical output')
            require(registry['result'] == canonical['result'], 'EXPECTATION_RUST_DERIVATION',
                    case + ': rust registry result differs from the D4-B canonical output')
    return {'status': 'PASS', 'cases': len(rows), 'measurements': len(observations),
            'a19DiffCases': sum(1 for row in rows if _expectation_diff_surfaces(row['ts'], row['rust']))}


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
    expectations = validate_expectations(registration, observations)
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
            'expectations': expectations,
            'rounds': rounds,
            'controls': controls, 'unstableGroups': manifest['unstableGroups'],
            'sidecar': {'status': 'NOT_APPLICABLE', 'reason': 'controlled in-process TS loop; no sidecar binary'},
            'connections': {'count': 0, 'reason': 'A5.2a prohibits listen; in-process queue observation'},
            'surfaces': {'output': 'frames', 'disk': ['durableStatus', 'registry'],
                         'http': {'status': 'NOT_APPLICABLE', 'reason': 'A5.2a in-process scope'}},
            'sourceManifest': str(raw / 'manifest.json'),
            'evidenceSha256': {p.name: digest(p) for p in sorted(raw.glob('*.json'))}}


def audit_native(evidence, contract):
    """Fail-closed contract for A7 recorder integration, not an executor.

    contract is prepared by the launcher BEFORE execution, never copied from a
    candidate's report; the binding is the one exception because it exists only
    at runtime: the independent observer records it at hello and it is bound
    to the contract afterwards (#1140). execEvents must include exited children,
    and calls must come from the independent egress observer. A ps snapshot
    alone is insufficient; the deadline CLI scores only captures that pass here.
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
    # #1140 fix-r2: no capabilities equality here -- load_native_capture
    # overwrites the audited capabilities with the contract's own before this
    # audit runs, so the comparison was self-vs-self on every real path and
    # could only fire on synthetic fixtures built with two copies.
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
    # #1140 fix-r2: no evidence/contract binding equality here -- load_native_
    # capture fills the audited binding from the same contract, so this
    # comparison was self-vs-self on every real path.  The real binding gates
    # are in the loader: _observer_binding's shape/consistency requires, the
    # driver-recorded hello anchor (fix-r3; a value rewritten in the observer
    # rows alone cannot audit), and the pre-registration rejection.  The
    # bindingSource require below is defense in depth for callers other than
    # the loader, which overwrites that field on its own real path.
    # #1140: the binding is a runtime record.  It must have been recorded by
    # the independent observer at hello and bound to the contract afterwards;
    # a pre-registered or SYNTHETIC binding can never audit a native run.
    require(contract.get('bindingSource') == 'observer-hello', 'BINDING',
            'binding must be recorded by the independent observer at hello and bound '
            'to the contract afterwards; pre-registered/SYNTHETIC bindings are rejected')
    # Validate bytes using the existing independent TCP decoder and wire checker.
    # #1140 fix-r1: every recorder row must carry wire metadata, and the decoder
    # cross-check is direction-aware -- a sent row's frame is the payload text
    # (compare.wire_frame's A4 contract) while only received frames are recorded
    # decoded, so the sent text is compared against the decoder's raw view.
    _audit_observer_rows(observer['rows'])
    hellos = [r['frame'] for r in observer['rows']
              if isinstance(r['frame'], dict) and r['frame'].get('type') == 'hello_ok']
    require(bool(hellos), 'BINDING', 'actual server hello/binding missing')
    for frame in hellos:
        binding = frame.get('interactionBinding', {})
        require(frame.get('protocolVersion') == '1.2' and type(binding.get('generation')) is int
                and binding['generation'] >= 1 and isinstance(binding.get('connectionId'), str)
                and bool(binding['connectionId']) and binding == contract['binding'],
                'BINDING', 'actual server hello has invalid/wrong/old binding')
    if contract['cases'] == read(SCRIPT / 'fixtures/session-calibration.json')['cases']:
        try:
            native_evidence.session_profile(contract, contract['cases'])
            native_evidence.session_windows(evidence, contract['cases'], evidence['transcriptPaths'])
        except native_evidence.Incomplete as error:
            raise EvidenceError('NATIVE_PROCESS', str(error)) from error


def _audit_observer_rows(rows):
    for row in rows:
        require(isinstance(row, dict), 'OBSERVER_WIRE', 'observer row must be an object')
        require(isinstance(row.get('frame'), str if row['direction'] == 'send' else dict),
                'OBSERVER_WIRE', 'observer JSON frame must be an object (sent rows retain text)')
        require(isinstance(row.get('wire'), dict), 'OBSERVER_WIRE',
                'observer rows must carry wire metadata '
                '(opcode/fin/rsv/masked/lengthCode/payloadLength)')
        wire_frame(row)
        try:
            native_evidence.ws_profile(row['direction'], row['wire'])
        except native_evidence.Incomplete as error:
            raise EvidenceError('OBSERVER_WIRE', str(error)) from error
        decoded = []
        decoder = Decoder(decoded.append, row['direction'])
        decoder.feed(b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n' + bytes.fromhex(row['wireHex']))
        require(len(decoded) == 1
                and decoded[0].get('raw' if row['direction'] == 'send' else 'frame') == row['frame'],
                'OBSERVER_WIRE', 'wire observation disagrees with recorder')


def _observer_binding(rows):
    """The interaction binding the independent observer recorded at hello.

    Paired with native-witness/witness.observer_binding and its parity test.
    A runtime record cannot be pre-registered: the binding exists only once
    the gateway has answered hello.  Every hello the observer saw must agree.
    """
    hellos = [row for row in rows
              if isinstance(row, dict) and isinstance(row.get('frame'), dict)
              and row['frame'].get('type') == 'hello_ok']
    require(bool(hellos), 'BINDING', 'observer recorded no server hello/binding')
    bindings = [row['frame'].get('interactionBinding') for row in hellos]
    require(all(isinstance(binding, dict) for binding in bindings),
            'BINDING', 'observer hello binding malformed')
    require(all(json.dumps(binding, sort_keys=True) == json.dumps(bindings[0], sort_keys=True)
                for binding in bindings), 'BINDING', 'observer hello records disagree on the binding')
    return bindings[0]


def load_native_capture(folder):
    """Formal scoring rejects all explicitly synthetic controls before replay."""
    launch = read(Path(folder) / 'capture/launch.json')
    evidence = read(Path(folder) / 'capture/evidence.json')
    require(launch.get('scope') != 'SYNTHETIC_CONTROL_ONLY'
            and evidence.get('scope') != 'SYNTHETIC_CONTROL_ONLY', 'NATIVE_CAPTURE',
            'synthetic control is not formal run evidence')
    return _load_native_artifacts(folder)


def _load_native_artifacts(folder):
    """Shared strict artifact authentication, also exercised by labelled controls."""
    folder = Path(folder).resolve()
    require((folder / 'capture').is_dir() and (folder / 'session-domain').is_dir(),
            'NATIVE_CAPTURE', 'not a native witness capture directory: ' + str(folder))
    try:
        raw, launch, evidence = native_evidence.load_capture(folder / 'capture')
        evidence = native_evidence.checked_evidence(raw, launch, evidence)
    except native_evidence.BinaryUnavailable as error:
        raise ApparatusVerificationError(str(error)) from error
    except native_evidence.Incomplete as error:
        raise EvidenceError('NATIVE_CAPTURE', str(error)) from error
    receipt = read(folder / 'capture/receipt.json')
    for name, key in native_evidence.receipt_artifacts('session-domain').items():
        artifact = folder / 'session-domain' / name
        require(artifact.is_file() and digest(artifact) == receipt.get(key),
                'NATIVE_CAPTURE', 'missing/altered capture artifact: session-domain/' + name)
    prelaunch = read(folder / 'capture/prelaunch.json')
    for key in ('binding', 'observerRows', 'bindingSource'):
        require(key not in launch and key not in prelaunch, 'BINDING',
                'runtime binding/observer records must not be pre-registered before launch')
    observer = read(folder / 'session-domain/observer.json')
    require(isinstance(observer, dict) and isinstance(observer.get('rows'), list)
            and isinstance(observer.get('connections'), int), 'EMPTY_OBSERVER', 'observer document malformed')
    require(bool(observer['rows']), 'EMPTY_OBSERVER', 'empty independent observer')
    contract = dict(launch, binding=_observer_binding(observer['rows']),
                    observerRows=observer['rows'], bindingSource='observer-hello')
    # #1140/#1206: compare two records from the same collector. This detects
    # an observer-only binding rewrite (including wire/receipt re-pinning),
    # not an adversarial writer rewriting driver, observer and receipt together.
    # The driver record is a consistency copy, not an independently signed anchor.
    driver = read(folder / 'session-domain/driver.json')
    require(isinstance(driver, dict) and driver.get('binding') == contract['binding'], 'BINDING',
            'driver-recorded hello binding disagrees with the observer hello record')
    cases_document, provider = read(folder / 'session-domain/cases.json'), read(folder / 'session-domain/provider.json')
    expected = read(SCRIPT / 'fixtures/session-calibration.json')['cases']
    require(contract.get('cases') == expected, 'COVERAGE',
            'the native capture must declare the full E1 case manifest; no denominator shrink')
    rows = cases_document.get('cases') if isinstance(cases_document, dict) else None
    require(isinstance(rows, list) and rows, 'OBSERVATION_STRUCTURE', 'case rows malformed')
    named = [row.get('case') if isinstance(row, dict) else None for row in rows]
    require(all(isinstance(case, str) and case for case in named)
            and len(set(named)) == len(named) and set(named) <= set(expected),
            'OBSERVATION_STRUCTURE', 'case rows must be unique names inside the manifest')
    require(named == expected, 'COVERAGE', 'case rows must cover the full manifest in order')
    requests = provider.get('requests') if isinstance(provider, dict) else None
    require(isinstance(requests, list), 'NETWORK_WITNESS', 'provider request ledger malformed')
    require(isinstance(provider.get('endpoint'), str), 'NETWORK_WITNESS', 'provider endpoint missing')
    targets = {call.get('target') for call in evidence.get('networkWitness', {}).get('calls', [])}
    require(not requests or provider['endpoint'] in targets, 'NETWORK_WITNESS',
            'driver-observed provider traffic missing from the egress witness')
    _audit_observer_rows(observer['rows'])
    for row in rows:
        observation = row.get('observation')
        problem = _native_observation_structure(observation) if observation is not None else None
        require(problem is None, 'OBSERVATION_STRUCTURE', row['case'] + ': ' + str(problem))
    try:
        sources = read(folder / 'session-domain/sources.json')
        paths = native_evidence.session_chain(contract, rows, observer, provider, driver, sources, expected)
    except native_evidence.Incomplete as error:
        raise EvidenceError('NATIVE_CHAIN', str(error)) from error
    # The output/disk/http surface names are the audit's fixed accounting set;
    # #1140 maps the session-domain capture trio onto them as placeholders
    # (observer/cases/provider).  The audit verifies artifact hashes only, so
    # the names carry no scoring semantics here.
    surfaces = {}
    for surface, name in (('output', 'observer.json'), ('disk', 'cases.json'), ('http', 'provider.json')):
        path = folder / 'session-domain' / name
        surfaces[surface] = {'status': 'RECORDED', 'artifacts': [{'path': str(path), 'sha256': digest(path)}]}
    audited = dict(evidence, expectedCases=expected, executedCases=named, transcriptPaths=paths,
                   capabilities=contract.get('capabilities'), surfaces=surfaces,
                   observer=observer, binding=contract['binding'])
    return {'folder': folder, 'contract': contract, 'evidence': audited, 'rows': rows,
            'expected': expected, 'executed': named,
            'providerCases': native_evidence.provider_cases(requests, expected)}


def _native_observation_structure(observation):
    """Shape of one observed native case; None when healthy, else the field.

    #1140 fix-r1: the shapes the projection dereferences are checked here, so
    a malformed event is a rejected capture (OBSERVATION_STRUCTURE) rather than
    a batch-level KeyError.  A missing run-record state is deliberately not
    structural: the projection scores it as that case's registry.state mismatch.
    """
    if not isinstance(observation, dict) or set(observation) != {'frames', 'durableStatus', 'registry'}:
        return 'observation keys'
    if not (isinstance(observation['frames'], list)
            and all(isinstance(event, dict) and isinstance(event.get('type'), str)
                    for event in observation['frames'])):
        return 'frames'
    durable = observation['durableStatus']
    if not (isinstance(durable, dict) and isinstance(durable.get('entries'), list)
            and all(isinstance(entry, dict) and isinstance(entry.get('event'), str)
                    for entry in durable['entries'])):
        return 'durableStatus'
    registry = observation['registry']
    if not isinstance(registry, dict) or not isinstance(registry.get('applicable'), bool):
        return 'registry'
    if registry['applicable'] is False and (registry.get('events') or registry.get('record')):
        return 'inapplicable registry records'
    if registry['applicable'] and not (isinstance(registry.get('events'), list)
                                       and all(isinstance(event, dict) and isinstance(event.get('seq'), int)
                                               and isinstance(event.get('event'), dict)
                                               and isinstance(event['event'].get('type'), str)
                                               for event in registry['events'])
                                       and isinstance(registry.get('record'), dict)
                                       and (registry['record'].get('result') is None
                                            or (isinstance(registry['record'].get('result'), dict)
                                                and isinstance(registry['record']['result'].get('type'), str)))):
        return 'registry events/record'
    return None


def score_native_cases(rows, expected, provider_cases):
    """Compare observed three surfaces per case with the registered rust expectation.

    No denominator shrink (#1140): every case of the frozen manifest is scored.
    A row the capture did not execute stays NOT_RUN and can never count as
    PASS; a differing surface is a per-case FAIL, never hidden in an aggregate.
    """
    document = read(SCRIPT / 'fixtures/session-expectations.json')
    registered = {row['case']: row['rust'] for row in document['cases']}
    required = native_evidence.required_provider_cases(document)
    covered = set(provider_cases)
    require(set(registered) == set(expected), 'EXPECTATION_COVERAGE',
            'expectation cases must equal the E1 case manifest one-to-one')
    # A case row whose observation key is absent scores that case NOT_RUN,
    # never a batch-level KeyError (#1140 fix-r2).
    observed = {row['case']: row.get('observation') for row in rows}
    reasons = {row['case']: row.get('reason') for row in rows}
    verdicts = []
    for case in expected:
        observation = observed.get(case)
        if observation is None:
            verdicts.append({'case': case, 'status': 'NOT_RUN', 'rule': 'COVERAGE',
                             'surfaces': ['observation'], 'detail': reasons.get(case) or 'observation missing'})
            continue
        problem = _native_observation_structure(observation)
        require(problem is None, 'OBSERVATION_STRUCTURE', case + ': ' + (problem or 'malformed'))
        surfaces = _expectation_diff_surfaces(project_observation(observation), registered[case])
        if case in required and case not in covered:
            surfaces.append('provider.requests')
        verdicts.append({'case': case, 'status': 'FAIL' if surfaces else 'PASS',
                         'rule': 'EXPECTATION_MISMATCH' if surfaces else None,
                         'surfaces': surfaces})
    return verdicts


def native_deadline_evidence(folder):
    """Audit and per-case score one native witness capture (#1140 wiring).

    Any audit rejection fails the native cell with the rejecting rule, and a
    case whose observed surfaces differ from the registered rust expectation
    is a per-case FAIL, never hidden.  This certifies nothing about the other
    matrix cells: without witness evidence they stay NOT_RUN.
    """
    capture = load_native_capture(folder)
    return _native_deadline_result(capture)


def _native_deadline_result(capture):
    audit_native(capture['evidence'], capture['contract'])
    verdicts = score_native_cases(capture['rows'], capture['expected'], capture['providerCases'])
    evidence = capture['evidence']
    return {'verdicts': verdicts, 'expected': capture['expected'], 'providerCases': capture['providerCases'],
            'processes': evidence['processes'],
            'connections': evidence['observer']['connections'],
            'cleanup': 'PASS' if evidence['cleanup']['allStopped'] else 'FAIL',
            'surfaces': {name: [row['path'] for row in surface['artifacts']]
                         for name, surface in evidence['surfaces'].items()},
            'sidecarSha256': next((p['sha256'] for p in evidence['processes'] if p['role'] == 'sidecar'), None),
            'witness': {'status': 'PASS', 'auditRule': None, 'capture': str(capture['folder']),
                        'observerRows': len(evidence['observer']['rows'])}}


def deadline_report(native_capture=None):
    native = native_deadline_evidence(native_capture) if native_capture is not None else None
    return _deadline_report(native)


def _deadline_report(native):
    cells = []
    for host, sidecar in MATRIX:
        cell = {'id': f'{host}-host/{sidecar}-sidecar', 'host': host, 'sidecar': sidecar,
                'topology': 'native' if host == 'rust' else 'ts-reference',
                'status': 'NOT_RUN', 'expectedGroups': list(CELLS),
                'expectedCases': read(SCRIPT / 'fixtures/session-calibration.json')['cases'], 'executedCases': [],
                'repeats': REPEATS, 'rounds': [], 'processes': [], 'connections': None,
                'sidecarSha256': None, 'output': None, 'disk': None, 'http': None,
                'oracle': 'NOT_RUN', 'cleanup': 'NOT_RUN'}
        if native is not None and host == 'rust' and sidecar == 'rust':
            # One observed turn per case per capture; the TS calibration's
            # five-repetition registration stays the calibrate suite's own.
            cell['cases'] = native['verdicts']
            cell['expectedCases'] = native['expected']
            # Only cases the capture actually scored count as executed; a
            # NOT_RUN verdict is the absence of evidence, not an execution
            # (#1140 fix-r1).
            cell['executedCases'] = [row['case'] for row in native['verdicts']
                                     if row['status'] != 'NOT_RUN']
            cell['repeats'] = 1
            cell['processes'] = native['processes']
            cell['connections'] = native['connections']
            cell['sidecarSha256'] = native['sidecarSha256']
            cell['surfaces'] = native['surfaces']
            cell['output'] = native['surfaces']['output']
            cell['disk'] = native['surfaces']['disk']
            cell['http'] = native['surfaces']['http']
            cell['oracle'] = 'EXPECTATION_COMPARISON'
            cell['cleanup'] = native['cleanup']
            cell['witness'] = native['witness']
            cell['status'], _ = common.aggregate_status(*(row['status'] for row in native['verdicts']))
        cells.append(cell)
    statuses = [cell['status'] for cell in cells]
    status = common.aggregate_status(*statuses)[0]
    result = {'status': status, 'structureStatus': 'PASS', 'cells': cells,
            'reason': 'A5.4 wiring (#1140): the native rust-host/rust-sidecar cell is scored '
                      'against the registered A19-HOST expectations when a witness capture is '
                      'supplied; matrix cells without witness evidence stay NOT_RUN and cannot '
                      'make the batch PASS',
            'executionStatus': status if native is not None else 'NOT_RUN',
            'acceptanceStatus': 'NOT_RUN'}
    executed = [cell for cell in cells if cell['executedCases']]
    require([(c['host'], c['sidecar']) for c in cells] == list(MATRIX)
            and all(c['expectedCases'] == cells[0]['expectedCases'] for c in cells)
            and all(c['status'] == 'NOT_RUN' and not c['executedCases']
                    for c in cells if not c['executedCases'])
            and len(executed) <= 1
            and all(c['host'] == 'rust' and c['sidecar'] == 'rust' for c in executed)
            and all(len(set(c['executedCases'])) == len(c['executedCases'])
                    and set(c['executedCases']) <= set(c['expectedCases']) for c in executed),
            'DEADLINE_STRUCTURE', 'matrix cells intact; only the native cell may carry evidence')
    return result


# ---------------------------------------------------------------------------
# E1 retirement suite (#1355, TS calibration slice).  The executed method set
# is collected from real TS-side gateway rounds driven through the official
# client; pass-through or uniform-unavailable padding must FAIL.  Thresholds
# (k rounds, the rule classes below) are registered here from the TS reference
# only: Rust retirement results are not consulted when setting them
# (frozen-procedure iron rule).  cold-restart/rollback stays an explicit
# BLOCKED placeholder until #1354.
RETIREMENT_ROUNDS = 5
RETIREMENT_METHODS_FIXTURE = SCRIPT / 'fixtures/retirement-methods.json'
RETIREMENT_DRIVER = SCRIPT / 'retirement_driver.mts'
RETIREMENT_PORT_SETS = ((28761, 28762, 28763), (28771, 28772, 28773))
# A6 (#1355 fix-r2): the round budget is derived from the worst legal driver
# run — every method may legitimately take its full per-method timeout — so a
# dead gateway still reaches a per-method timeout verdict instead of only the
# top-level round failure.  RETIREMENT_PER_METHOD_TIMEOUT_S must equal
# PER_METHOD_TIMEOUT_MS in retirement_driver.mts.
# ② (#1355 fix-r3): the budget is the sum of its legal worst-case components,
# not a single magic number — the old 101×15+90 silently dropped the driver's
# tail (12 HTTP probes × 10s, 5s socket close) and sat below the worst legal
# path (1730s): a maximally slow but legal round would hit the round budget
# before its own per-method timeouts, collapsing to a top-level failure.
RETIREMENT_METHOD_COUNT = 101
RETIREMENT_PER_METHOD_TIMEOUT_S = 15
RETIREMENT_HTTP_PROBE_COUNT = 12
RETIREMENT_HTTP_PROBE_TIMEOUT_S = 10
RETIREMENT_SOCKET_CLOSE_TIMEOUT_S = 5
RETIREMENT_READINESS_TIMEOUT_S = 45
RETIREMENT_CONNECT_MARGIN_S = 30
RETIREMENT_ROUND_TIMEOUT = (RETIREMENT_METHOD_COUNT * RETIREMENT_PER_METHOD_TIMEOUT_S
                            + RETIREMENT_HTTP_PROBE_COUNT * RETIREMENT_HTTP_PROBE_TIMEOUT_S
                            + RETIREMENT_SOCKET_CLOSE_TIMEOUT_S
                            + 2 * RETIREMENT_READINESS_TIMEOUT_S
                            + RETIREMENT_CONNECT_MARGIN_S)
# The audit's question is whether a TS gateway/helper runs outside the declared
# owned set.  The census pattern is the round's own baseline path plus the TS CLI
# entry: `ps` lines of unrelated installs (the operator's own running PilotDeck,
# other checkouts) must not be counted, and are never touched — only processes
# running this round's exact baseline CLI entry are in scope.
RETIREMENT_CENSUS_SUFFIX = 'dist/src/cli/pilotdeck.js'
RETIREMENT_OUTCOME_STATUSES = ('ok', 'error', 'timeout')
# R8 (#1355 fix-r2): the baseline the calibration ran against must be recorded
# and pinned, or the acceptance evidence cannot be reproduced.
RETIREMENT_BASELINE_REVISION = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
# R6 (#1355 fix-r2): the entry parser has a completeness floor.  Without it the
# parsed set can silently shrink (a pattern that stops matching, a moved
# source file) and the entry inventory passes on whatever was still found.
# Registered from the real TS baseline parse (13 http routes / 4 channel
# listeners, both verified live in the r1 calibration rounds).
RETIREMENT_REQUIRED_ROUTES = (
    ('/health', 'absolute'), ('/auth/local-token', 'absolute'), ('/feishu/webhook', 'absolute'),
    ('/api/web/', 'prefix'), ('/ws', 'absolute'), ('/api/web/projects', 'absolute'),
    ('/files/tree', 'subpath'), ('/files/read', 'subpath'), ('/files/write', 'subpath'),
    ('/api/module-host/describe', 'absolute'), ('/api/module-host/call', 'absolute'),
    ('/api/module-host/approvals/status', 'absolute'), ('/api/module-host/approvals/resume', 'absolute'),
)
RETIREMENT_REQUIRED_LISTENERS = (
    'src/adapters/channel/api-server/ApiServerChannel.ts', 'src/adapters/channel/sms/SmsChannel.ts',
    'src/adapters/channel/webhook/WebhookChannel.ts',
    'src/adapters/channel/wecom-callback/WeComCallbackChannel.ts',
)
# R7 (#1355 fix-r2): entries that are config/credential-gated in the baseline
# can never be hit inside a round (the round config carries no credentials), so
# they are reported as GATED with the gating evidence instead of silently
# blocking or silently passing.  A gate whose evidence no longer holds (the
# baseline stopped guarding the route, or the round config grew the key) falls
# back to the ordinary HIT/NO_HIT evaluation — fail-closed, never a pass.
RETIREMENT_GATED_ENTRIES = {
    '/feishu/webhook': {
        'gate': 'options.feishuWebhook',
        'source': 'src/gateway/server/GatewayServer.ts:113',
        'roundConfigKey': 'feishu',
        'reason': 'pilotdeckServer.ts:271 always injects the feishuWebhook handler, but without an '
                  'active Feishu channel (credentials) handleWebhook returns false and the route 404s; '
                  'the round config carries no feishu credentials',
    },
}


def _retirement_expected(baseline):
    """The 101-method expected set with per-source provenance (no invented rows).

    Sources: the baseline wire union, the gateway coverage manifest, the frozen
    method inventory table, and the retirement fixture.  A missing or
    disagreeing source is a provenance gap and refuses to score.
    """
    sources = []
    frames_path = baseline / 'src/gateway/protocol/frames.ts'
    require(frames_path.is_file(), 'EXPECTED_PROVENANCE', f'baseline frames.ts missing: {frames_path}')
    union = re.search(r'type WsGatewayMethod =([^;]*);', frames_path.read_text())
    require(union is not None, 'EXPECTED_PROVENANCE', 'WsGatewayMethod union not found in baseline frames.ts')
    wire = re.findall(r'"([a-z0-9_]+)"', union.group(1))
    sources.append({'source': 'baseline frames.ts WsGatewayMethod union',
                    'path': frames_path.as_posix(), 'count': len(wire)})
    manifest_path = ROOT / 'specs/gateway-coverage/coverage-manifest.json'
    require(manifest_path.is_file(), 'EXPECTED_PROVENANCE', f'gateway coverage manifest missing: {manifest_path}')
    manifest = read(manifest_path)
    require(isinstance(manifest.get('methods'), list), 'EXPECTED_PROVENANCE',
            'coverage manifest has no methods list')
    covered = [row.get('method') for row in manifest['methods'] if isinstance(row, dict)]
    sources.append({'source': 'gateway coverage manifest (MAP.md §1.1 canonical)',
                    'path': manifest_path.as_posix(), 'count': len(covered)})
    inventory_path = ROOT / 'docs/research/005_phase-a-gateway/method-inventory.md'
    require(inventory_path.is_file(), 'EXPECTED_PROVENANCE', f'method inventory missing: {inventory_path}')
    inventoried = re.findall(r'^\|\s*\d+\s*\|\s*`([a-z0-9_]+)`', inventory_path.read_text(), re.M)
    sources.append({'source': 'frozen method inventory table (plan 005 A-phase denominator)',
                    'path': inventory_path.as_posix(), 'count': len(inventoried)})
    fixture = read(RETIREMENT_METHODS_FIXTURE)
    require(isinstance(fixture.get('methods'), list), 'EXPECTED_PROVENANCE',
            'retirement fixture has no methods list')
    fixtured = [row.get('method') for row in fixture['methods'] if isinstance(row, dict)]
    sources.append({'source': 'retirement driver fixture', 'path': RETIREMENT_METHODS_FIXTURE.as_posix(),
                    'count': len(fixtured)})
    sets = [wire, covered, inventoried, fixtured]
    require(all(len(rows) == 101 for rows in sets), 'EXPECTED_PROVENANCE',
            f'expected-method source counts disagree with 101: {[len(rows) for rows in sets]}')
    ordered = sorted(sets[0])
    for source, rows in zip(sources, sets):
        require(sorted(rows) == ordered, 'EXPECTED_PROVENANCE',
                f"source disagrees with the baseline union: {source['source']}")
    return ordered, sources


def _retirement_census(gateway_pid, baseline):
    """Snapshot of processes running this round's TS gateway CLI entry.

    The declared set is exactly the round's owned gateway pid; anything else
    matching the entry pattern is an undeclared TS host.  The pattern pins the
    round's own baseline path, so unrelated installs on the host are neither
    counted nor touched.
    """
    pattern = f'{baseline}/{RETIREMENT_CENSUS_SUFFIX}'
    result = subprocess.run(['ps', '-axo', 'pid=,command='], capture_output=True, text=True, timeout=5)
    require(result.returncode == 0, 'HOST_CENSUS', f'ps failed: {result.stderr.strip()}')
    # R3 (#1355 fix-r2): liveness is proven at snapshot time, while the gateway
    # is still running — the verdict is computed after cleanup, so it consumes
    # this recorded proof instead of re-probing a dead pid.
    declared_alive = True
    try:
        os.kill(gateway_pid, 0)
    except OSError:
        declared_alive = False
    rows, skipped = [], []
    for line in result.stdout.splitlines():
        if pattern not in line:
            continue
        fields = line.strip().split(None, 1)
        if len(fields) != 2:
            continue
        # A7 (#1355 fix-r2): whole-command substring matching counted
        # `grep -rn <entry>` / `tail -f <entry>` as undeclared TS hosts.  A row
        # is a TS host only when a node/tsx binary executes this round's
        # baseline CLI entry as its own argv token; anything else is recorded
        # as skipped (read-the-code inference, never silently dropped).
        argv = fields[1].split()
        if not argv or not argv[0].endswith(('node', 'tsx')) or pattern not in argv:
            skipped.append({'pid': int(fields[0]), 'argvText': fields[1][:300],
                            'reason': 'command line mentions the baseline entry but is not a '
                                      'node/tsx process executing it'})
            continue
        rows.append({'pid': int(fields[0]), 'argvText': fields[1][:300]})
    return {'rows': rows, 'skipped': skipped, 'declared': [gateway_pid],
            'declaredAlive': declared_alive, 'pattern': pattern}


def _census_unobserved(census):
    """#2519: the one shared test for "this round recorded no process census".

    A census that is not a dict or whose sampled rows are not a list is an
    *unobserved* host set, never an empty one (R3 #1355 fix-r2).  The
    criterion used to be written once per face — the report face's guard and
    the control face's rows derivation — so the two could drift apart (#2519,
    from the #1355 r1 harvest review); every face reads this one function
    now.  Whether the declared owned set actually contains a sampled pid
    stays with the verdict: an emptied declared set is the TS_HOST control's
    injection and must score FAIL, never read as unobserved.
    """
    return not isinstance(census, dict) or not isinstance(census.get('rows'), list)


def _retirement_census_verdict(census):
    """R3 (#1355 fix-r2): an unobservable census is not an empty host set.

    The declared gateway pid must be present in the sampled rows (and alive at
    sampling time) before "no undeclared rows" can mean PASS; an empty sample
    or a dead declared pid is a non-PASS observation, mirroring the
    process_tree rule that an inaccessible census is not an empty tree.
    #2519: an *unobserved* census (no dict, no rows list) blocks here via the
    same `_census_unobserved` predicate the TS_HOST control reads, instead of
    raising on ``census['rows']``.
    """
    if _census_unobserved(census):
        return {'status': 'BLOCKED', 'rule': 'HOST_CENSUS', 'undeclared': [],
                'problems': ['the round recorded no process census; an unobserved census is '
                             'not an empty host set']}
    rows, declared = census['rows'], census.get('declared', [])
    undeclared = [row for row in rows if row['pid'] not in declared]
    missing_declared = [pid for pid in declared if pid not in {row['pid'] for row in rows}]
    problems = []
    if undeclared:
        problems.append(f'{len(undeclared)} undeclared TS host rows: '
                        + ', '.join(str(row['pid']) for row in undeclared))
    if missing_declared:
        problems.append(f'declared pids absent from the census rows: {missing_declared} '
                        f'(rows sampled: {len(rows)})')
    if census.get('declaredAlive') is False:
        problems.append('declared gateway pid was not alive at census time')
    return {'status': 'FAIL' if problems else 'PASS', 'rule': 'HOST_CENSUS',
            'undeclared': undeclared, 'problems': problems}


def _retirement_report_census(round_row):
    """The round's census verdict for the report face.

    P1 (#1355 fix-r4e, minimax boundary #2; graded P2 by anthropic sibling #2,
    higher wins): the report path indexed ``row['census']`` /
    ``row['connections']`` / ``row['listeners']`` directly, so a round that
    recorded no observation raised KeyError inside ``retirement_report`` and
    main's fallback dropped all 16 scoring faces — the collapse this PR exists
    to remove.  A missing census is an *unobserved* host set, never an empty
    one, so it blocks instead of passing.  #2519: the unobserved criterion
    itself lives in `_retirement_census_verdict` (via `_census_unobserved`)
    now, so this report face and the verdict cannot drift apart again; this
    function stays the report face's tolerant key read.
    """
    return _retirement_census_verdict(round_row.get('census'))


def _retirement_connections(pid):
    """Established TCP peers of the gateway while it is still running."""
    result = subprocess.run(['lsof', '-nP', '-a', '-p', str(pid), '-iTCP'],
                            capture_output=True, text=True, timeout=10)
    # R3 (#1355 fix-r2): a failed probe is not an empty connection set.  lsof
    # exiting non-zero (or erroring with no parseable rows) must surface as a
    # blocked observation, never as "all peers loopback".
    if result.returncode != 0 or (result.stderr.strip() and not result.stdout.strip()):
        return {'rows': [], 'unparsed': [], 'commandStatus': 'failed',
                'reason': f'lsof exited {result.returncode}: {result.stderr.strip() or "(no stderr)"}'}
    rows, unparsed = [], []
    for line in result.stdout.splitlines()[1:]:
        if 'ESTABLISHED' not in line:
            continue
        peer = None
        match = re.search(r'(\S+)->(\S+)', line)
        if match:
            peer = match.group(2)
        row = {'peer': peer, 'raw': line[:300]}
        (rows if peer is not None else unparsed).append(row)
    return {'rows': rows, 'unparsed': unparsed, 'commandStatus': 'ok'}


def _retirement_network_verdict(connections):
    """Every established peer must be loopback; anything else is a fallback.

    R3 (#1355 fix-r2): the verdict takes the whole connections observation.
    An empty sample, a failed lsof, or unparsed peer lines cannot prove
    loopback-only, so they block instead of passing.
    """
    rows = connections.get('rows', [])
    undeclared = [row['peer'] for row in rows if row.get('peer') is not None
                  and not (row['peer'].startswith('127.0.0.1:') or row['peer'].startswith('localhost:')
                           or row['peer'].startswith('::1:') or row['peer'].startswith('[::1]:'))]
    unparsed = [row for row in rows if row.get('peer') is None] + connections.get('unparsed', [])
    if connections.get('commandStatus') == 'failed':
        return {'status': 'BLOCKED', 'rule': 'NETWORK_FALLBACK', 'undeclared': undeclared,
                'unparsed': unparsed, 'reason': connections.get('reason')}
    if undeclared:
        return {'status': 'FAIL', 'rule': 'NETWORK_FALLBACK', 'undeclared': undeclared, 'unparsed': unparsed}
    if unparsed or not rows:
        return {'status': 'BLOCKED', 'rule': 'NETWORK_FALLBACK', 'undeclared': [],
                'unparsed': unparsed,
                'reason': 'no parseable established-peer sample; an empty or unparsed sample is '
                          'not proof of loopback-only'}
    return {'status': 'PASS', 'rule': 'NETWORK_FALLBACK', 'undeclared': [], 'unparsed': []}


def _retirement_network_observable(connections):
    """Whether the round recorded a network sample a control can be judged on.

    P1 (#1355 fix-r4f, anthropic sibling #1): the control used to ask the
    *verdict* whether the round's sample was BLOCKED, but
    `_retirement_network_verdict` returns FAIL on `undeclared` before it
    reaches the `unparsed or not rows` check — a round with a pre-existing
    undeclared peer (the real r2 round's provider egress, #1478) *and* an
    unparsed line read as observable, and the injected peer was credited
    against it.  Observability is the shape of the observation: a successful
    probe, at least one parsed row, no unparsed lines, and no row without a
    parsed peer.
    """
    if not isinstance(connections, dict):
        return False
    rows = connections.get('rows')
    if (connections.get('commandStatus') != 'ok' or not isinstance(rows, list) or not rows
            or connections.get('unparsed')):
        return False
    return all(isinstance(row, dict) and row.get('peer') for row in rows)


def _retirement_listener_verdict(listeners, gateway_port):
    """R9 (#1355 fix-r2): loopbackOnly is judged, not just recorded.

    The gateway must listen on exactly 127.0.0.1:<gateway_port>; a non-loopback
    or unexpected binding is a LISTENER_EXPOSURE FAIL that participates in the
    host audit aggregation (exp003/exp008 treat this field as a direct FAIL).
    """
    listeners = listeners if isinstance(listeners, dict) else {}
    addresses = listeners.get('addresses')
    expected = [f'127.0.0.1:{gateway_port}']
    if listeners.get('loopbackOnly') is not True or addresses != expected:
        return {'status': 'FAIL', 'rule': 'LISTENER_EXPOSURE', 'addresses': addresses,
                'expected': expected, 'loopbackOnly': listeners.get('loopbackOnly'),
                'detail': f'gateway listeners {addresses} are not the loopback-only '
                          f'binding {expected}'}
    return {'status': 'PASS', 'rule': 'LISTENER_EXPOSURE', 'addresses': addresses, 'expected': expected}


def _retirement_gateway_pid(round_row):
    """The gateway pid a round recorded, or None when the probe recorded nothing.

    P1 (#1355 fix-r4d, anthropic boundary #1): ``row.get('listeners', {})``
    only defaults a *missing* key; a round that recorded ``listeners: null``
    (JSON null) still yields None and ``None.get('pid')`` raised
    AttributeError, collapsing the whole 16-face report into the 5-key stub
    this PR exists to remove.  Same isinstance guard as
    _retirement_listener_verdict.
    """
    listeners = round_row.get('listeners')
    return listeners.get('pid') if isinstance(listeners, dict) else None


def _retirement_evidence_pairs(verdict):
    """The (method, problem) pairs a scoring verdict reports as EVIDENCE_BACKING."""
    pairs = set()
    for problem in verdict.get('rules') or []:
        if problem.get('rule') != 'EVIDENCE_BACKING':
            continue
        detail = problem.get('detail')
        if isinstance(detail, dict):
            detail = [detail]
        for item in detail or []:
            if isinstance(item, dict):
                pairs.add((item.get('method'), item.get('problem')))
    return pairs


def _retirement_wire_backing(frames):
    """Map methods to their request ids and the responses/events that answered."""
    sends, responses, events, final_events = {}, {}, {}, set()
    for row in frames:
        frame = row.get('frame')
        if not isinstance(frame, dict):
            continue
        identifier = frame.get('id')
        if row.get('direction') == 'send':
            if frame.get('type') == 'request':
                sends[frame.get('method')] = identifier
            continue
        kind = frame.get('type')
        if kind == 'response':
            responses[identifier] = frame
        elif kind == 'event':
            events[identifier] = events.get(identifier, 0) + 1
            if frame.get('final') is True:
                final_events.add(identifier)
    return sends, responses, events, final_events


def score_retirement_round(row, expected):
    """Score one TS round against the anti-faking rules; never raises.

    Rules: COVERAGE (executed must equal expected), DUPLICATE_EXECUTION (an
    executed method reported twice), UNIFORM_UNAVAILABLE (all answers
    CAPABILITY_UNAVAILABLE cannot pad coverage), EVIDENCE_BACKING (every
    outcome needs its wire frame; a timeout outcome is never executed and by
    definition lacks wire backing — any timeout fails the round, fail-closed;
    a stream outcome of either ok or error kind needs at least one receive
    frame under the same id, R1 #1355 fix-r2; a stream ok additionally needs a
    `final:true` event frame under the same id, ① #1355 fix-r3 — a stream
    that merely ended never passes as ok),
    MISSING_GROUP / MISSING_FIELD / ZERO_TESTS / UNKNOWN_SKIP (structure),
    ROUND_EXECUTION (driver exit).
    """
    verdict = {'round': row['label'], 'status': 'FAIL', 'rules': [], 'executed': [],
               'methodVerdicts': {}, 'driverExitCode': row['driverExitCode']}
    problems = []
    for group in ('outcomes', 'frames', 'http', 'processes'):
        if not row.get(group):
            problems.append({'rule': 'MISSING_GROUP', 'detail': group})
    if problems:
        verdict['rules'] = problems
        return verdict
    if row['driverExitCode'] != 0:
        problems.append({'rule': 'ROUND_EXECUTION', 'detail': f"driver exited {row['driverExitCode']}"})
    outcomes = row['outcomes']
    if not outcomes:
        problems.append({'rule': 'ZERO_TESTS', 'detail': 'no method outcome rows'})
        verdict['rules'] = problems
        return verdict
    executed, unknown, missing_fields = [], [], []
    for outcome_row in outcomes:
        for field in ('method', 'outcome', 'shape'):
            if field not in outcome_row:
                missing_fields.append({'field': field, 'row': outcome_row})
        method, status = outcome_row.get('method'), outcome_row.get('outcome')
        if status not in RETIREMENT_OUTCOME_STATUSES:
            unknown.append(method)
            continue
        verdict['methodVerdicts'][method] = status
        if status == 'timeout':
            # F1 (#1355 fix-r1, fail-closed): a timeout is not an executed
            # method — it counts as missing for COVERAGE, and the wire-backing
            # pass below flags it so any single timeout fails the round.
            continue
        executed.append(method)
    if missing_fields:
        problems.append({'rule': 'MISSING_FIELD', 'detail': missing_fields})
    if unknown:
        problems.append({'rule': 'UNKNOWN_SKIP', 'detail': unknown})
    if len(executed) != len(set(executed)):
        counts: dict = {}
        for method in executed:
            counts[method] = counts.get(method, 0) + 1
        problems.append({'rule': 'DUPLICATE_EXECUTION',
                         'detail': sorted(method for method, n in counts.items() if n > 1)})
    expected_set, executed_set = set(expected), set(executed)
    missing = [method for method in expected if method not in executed_set]
    extra = [method for method in executed if method not in expected_set]
    if missing:
        problems.append({'rule': 'COVERAGE', 'detail': {'missing': missing}})
    if extra:
        problems.append({'rule': 'COVERAGE', 'detail': {'unexpected': extra}})
    if outcomes and all(outcome_row.get('outcome') == 'error'
                        and outcome_row.get('errorCode') == 'CAPABILITY_UNAVAILABLE'
                        for outcome_row in outcomes):
        problems.append({'rule': 'UNIFORM_UNAVAILABLE',
                         'detail': 'every executed method answered CAPABILITY_UNAVAILABLE'})
    sends, responses, events, final_events = _retirement_wire_backing(row['frames'])
    violations = []
    for outcome_row in outcomes:
        method = outcome_row.get('method')
        status = outcome_row.get('outcome')
        if status == 'timeout':
            # F1 (#1355 fix-r1): a gateway that takes requests and never answers
            # produces exactly this outcome with zero wire backing.
            violations.append({'method': method, 'problem': 'timeout lacks wire backing'})
            continue
        if method not in sends:
            violations.append({'method': method, 'problem': 'send frame missing'})
            continue
        identifier = sends[method]
        if outcome_row.get('shape') == 'stream':
            if status == 'ok':
                # ① (#1355 fix-r3): a stream ok must be backed by a terminal
                # `final:true` event frame under the same id — receive frames
                # alone prove activity, not completion.  The pre-fix scorer
                # passed a stream that merely ended (the arbitration's P0).
                if identifier not in final_events:
                    violations.append({'method': method,
                                       'problem': 'stream ok without a final event frame'})
            elif status == 'error':
                # R1 (#1355 fix-r2): a stream error outcome must be backed by at
                # least one receive frame (event or response) under the same
                # id.  The old branch only checked ok outcomes, so an error
                # outcome with zero received frames still counted as executed.
                if events.get(identifier, 0) < 1 and identifier not in responses:
                    violations.append({'method': method,
                                       'problem': 'stream error without any receive frame'})
        elif status == 'ok':
            response = responses.get(identifier)
            if response is None or response.get('ok') is not True:
                violations.append({'method': method, 'problem': 'outcome ok without an ok response frame'})
        elif status == 'error':
            response = responses.get(identifier)
            if response is None or response.get('ok') is not False:
                violations.append({'method': method, 'problem': 'outcome error without an error response frame'})
    if violations:
        problems.append({'rule': 'EVIDENCE_BACKING', 'detail': violations})
    verdict['executed'] = sorted(executed_set)
    verdict['rules'] = problems
    verdict['status'] = 'FAIL' if problems else 'PASS'
    return verdict


def _retirement_strip_wire(mutated, method):
    """Remove every wire frame of *method* while keeping its outcome row."""
    identifiers = set()
    for row in mutated['frames']:
        frame = row.get('frame')
        if row.get('direction') == 'send' and isinstance(frame, dict) and frame.get('method') == method:
            identifiers.add(frame.get('id'))
    mutated['frames'] = [row for row in mutated['frames']
                         if not (isinstance(row.get('frame'), dict) and row['frame'].get('id') in identifiers)]
    return mutated


def _retirement_strip_receive(mutated, method):
    """Remove every receive frame of *method*, keeping its send frame (R1)."""
    identifiers = set()
    for row in mutated['frames']:
        frame = row.get('frame')
        if row.get('direction') == 'send' and isinstance(frame, dict) and frame.get('method') == method:
            identifiers.add(frame.get('id'))
    mutated['frames'] = [row for row in mutated['frames']
                         if not (row.get('direction') == 'receive'
                                 and isinstance(row.get('frame'), dict)
                                 and row['frame'].get('id') in identifiers)]
    return mutated


def _retirement_controls(round_row, expected):
    """Six mutation classes over the real round evidence; each must FAIL.

    The controls only prove the scorer catches injected fakes; they are never
    evidence of retirement (memory ruler-real-data).  R4 (#1355 fix-r2): each
    class must be caught by its *own* registered rule — a mutation survived by
    being failed for an unrelated reason is a failed control, not a pass.
    P1 (#1355 fix-r4): a class a legal degraded round cannot establish (e.g.
    no ok stream outcome to mutate) is reported NOT_APPLICABLE with its
    reason, never raised as EvidenceError — raising collapsed the whole
    16-face report into a 5-key stub.
    P1 (#1355 fix-r4c): an injection that changes nothing on this round proves
    nothing either — the round's own pre-existing violation would be credited
    to the control as a PASS.  Every mutating class therefore requires its
    mutation to land on the dimension it injects (wire frame count, outcome
    rows, declared pids, network sample); otherwise the class is
    NOT_APPLICABLE.  The census class needs its gateway pid in the census.
    P1 (#1355 fix-r4d): landing is judged on the *injected dimension*, and
    attribution on the *injected object*, not on a rule name or a whole-round
    diff.  A round can change on one dimension (a frame's ok flag) while the
    injected property (every outcome CAPABILITY_UNAVAILABLE) is untouched, and
    a rule can fire for a violation the round already had on another method or
    another row; either way a PASS would report the round's own violation as a
    control success.  Frame classes attribute to their (method, problem) pair,
    the census class to the gateway pid inside the mutated declared set, the
    network class to the injected peer.
    """
    controls = []
    original_evidence = _retirement_evidence_pairs(
        score_retirement_round(json.loads(json.dumps(round_row)), expected))

    def record(name, injection, expected_rule, verdict, detail, require_pair=None, caused=None):
        if require_pair is not None and require_pair in original_evidence:
            # P1 (#1355 fix-r4d, minimax boundary): the control round already
            # reports this exact violation, so a FAIL here belongs to the
            # round, not to the injection.  Never PASS.
            unavailable(name, injection, expected_rule,
                        f'the control round already reports {require_pair[1]!r} for '
                        f'{require_pair[0]!r}, so the injected violation cannot be told apart '
                        'from the pre-existing one')
            return
        fired = ([problem['rule'] for problem in verdict['rules']] if 'rules' in verdict
                 else [verdict.get('rule')])
        attributed = expected_rule in fired
        if attributed and require_pair is not None:
            # A rule name is not attribution: another method may be the one
            # missing its frame.  The injected (method, problem) must be the
            # pair the scorer reports.
            attributed = require_pair in _retirement_evidence_pairs(verdict)
        if attributed and caused is not None:
            attributed = bool(caused(verdict))
        controls.append({'class': name, 'injection': injection, 'expected': 'FAIL',
                         'rule': expected_rule,
                         'status': 'PASS' if verdict.get('status') == 'FAIL' and attributed else 'FAIL',
                         'detail': {**detail, 'firedRules': fired}})

    def unavailable(name, injection, expected_rule, reason):
        # P1 (#1355 fix-r4): never PASS — the controls face aggregates an
        # unavailable class to BLOCKED so the total verdict cannot relax.
        controls.append({'class': name, 'injection': injection, 'expected': 'FAIL',
                         'rule': expected_rule, 'status': 'NOT_APPLICABLE', 'reason': reason})

    # 1. 故意偷偷回退 TS / 假 local 实透传: an executed method with no wire frame.
    mutated = json.loads(json.dumps(round_row))
    victim = next((row.get('method') for row in mutated['outcomes']
                   if row.get('outcome') == 'ok' and row.get('method')), None)
    injection = 'executed method with every wire frame stripped while its ok outcome is kept'
    if victim is None:
        # P1 (#1355 fix-r4): same unestablishable-control rule as class 6 — a
        # legal degraded round (every method errored/timed out, or the ok rows
        # carry no method name) leaves nothing to strip.  Raising here also
        # collapsed the whole 16-face report into the 5-key stub.
        unavailable('TS_FALLBACK', injection, 'EVIDENCE_BACKING',
                    'no ok outcome carrying a method name in the control round: every method '
                    'errored or timed out, so the wire-frame mutation cannot be established')
    else:
        before = len(mutated['frames'])
        mutated = _retirement_strip_wire(mutated, victim)
        if len(mutated['frames']) == before:
            # P1 (#1355 fix-r4b, both families): the victim has no send frame
            # to strip, so the injection is a no-op and the round's own
            # pre-existing "send frame missing" violation would be credited to
            # this control as a PASS.  An unestablished control is never PASS.
            unavailable('TS_FALLBACK', injection, 'EVIDENCE_BACKING',
                        f"executed method {victim!r} has no send frame in the control round, "
                        'so stripping its frames removes nothing and the injection cannot be '
                        'established')
        else:
            verdict = score_retirement_round(mutated, expected)
            record('TS_FALLBACK', f"strip all wire frames of executed method '{victim}' while keeping its ok outcome",
                   'EVIDENCE_BACKING', verdict,
                   {'detail': next((p['detail'] for p in verdict['rules']
                                    if p['rule'] == 'EVIDENCE_BACKING'), None)},
                   require_pair=(victim, 'send frame missing'))
    # 2. 隐藏 TS 宿主: the census still shows the gateway pid, but it is undeclared.
    census = round_row.get('census') or {}
    # #2519: the same `_census_unobserved` predicate the report face blocks on;
    # an unobserved census reads as no rows here, so the class is unestablished
    # instead of raising on a garbage-shaped observation.
    census_rows = [] if _census_unobserved(census) else census['rows']
    gateway_pid = _retirement_gateway_pid(round_row)
    if gateway_pid is None or not any(row.get('pid') == gateway_pid for row in census_rows):
        # P2a (#1355 fix-r4c, anthropic sibling #2): the injection is "the
        # gateway pid stays in the census but leaves the declared set".  With
        # the pid absent from the census the mutation proves nothing — it
        # either fails the class on a pre-existing undeclared row (false PASS)
        # or on an empty census (false FAIL).  Report it as unestablished.
        unavailable('TS_HOST', 'gateway pid present in the census but removed from the declared owned set',
                    'HOST_CENSUS',
                    f"gateway listener pid {gateway_pid!r} is absent from the census rows "
                    f'({len(census_rows)} sampled), so the undeclared-host mutation cannot be '
                    'established')
    else:
        # P1 (#1355 fix-r4d, minimax boundary #3): emptying the declared set
        # makes every row undeclared, so a leftover undeclared row the round
        # already had (a failed launch attempt's process) would fail the class
        # on its own.  Remove only the gateway pid from the declared set and
        # attribute the verdict to that pid.
        declared_original = census.get('declared') or []
        if gateway_pid not in declared_original:
            unavailable('TS_HOST', 'gateway pid present in the census but removed from the declared owned set',
                        'HOST_CENSUS',
                        f'gateway pid {gateway_pid} is not in the round\'s declared owned set '
                        f'{declared_original}, so removing it changes nothing and the undeclared-host '
                        'mutation cannot be established')
        else:
            declared = [pid for pid in declared_original if pid != gateway_pid]
            original_census = _retirement_census_verdict(
                {'rows': census_rows, 'declared': declared_original,
                 'declaredAlive': census.get('declaredAlive')})
            undeclared_before = {row.get('pid') for row in original_census['undeclared']}
            census_verdict = _retirement_census_verdict(
                {'rows': census_rows, 'declared': declared,
                 'declaredAlive': census.get('declaredAlive')})
            record('TS_HOST', 'gateway pid present in the census but removed from the declared owned set',
                   'HOST_CENSUS', census_verdict, {},
                   # Attribution is the declared-set delta: exactly the gateway
                   # pid becomes undeclared.  A leftover row the round already
                   # had is in undeclared_before and cannot carry the class.
                   caused=lambda verdict: (
                       {row.get('pid') for row in verdict.get('undeclared') or []}
                       == undeclared_before | {gateway_pid}))
    # 3. 全走 unavailable: every outcome becomes CAPABILITY_UNAVAILABLE, wire kept consistent.
    mutated = json.loads(json.dumps(round_row))
    for outcome_row in mutated['outcomes']:
        outcome_row['outcome'] = 'error'
        outcome_row['errorCode'] = 'CAPABILITY_UNAVAILABLE'
    for row in mutated['frames']:
        frame = row.get('frame')
        if isinstance(frame, dict) and frame.get('type') == 'response' and frame.get('ok') is True:
            frame['ok'] = False
            frame.pop('result', None)
            frame['error'] = {'code': 'CAPABILITY_UNAVAILABLE', 'message': 'controlled mutation'}
    if not round_row['outcomes']:
        # P1 (#1355 fix-r4d, minimax boundary #2): a legal degraded round may
        # record zero method outcomes.  The scorer then stops at ZERO_TESTS and
        # UNIFORM_UNAVAILABLE can never fire, so scoring the mutation reported
        # the class FAIL — the earlier whole-round diff only masked this when
        # the round happened to carry no ok:true frame.
        unavailable('UNIFORM_UNAVAILABLE',
                    'every outcome rewritten to error CAPABILITY_UNAVAILABLE with matching wire',
                    'UNIFORM_UNAVAILABLE',
                    'the control round recorded no method outcome, so the uniform-unavailable '
                    'rewrite changes nothing and the injection cannot be established')
    elif mutated['outcomes'] == round_row['outcomes']:
        # P1 (#1355 fix-r4c, anthropic sibling #1; narrowed in fix-r4d by
        # minimax boundary #1/#2): landing is judged on the injected dimension
        # — the outcomes — not on the whole round.  A contradictory round
        # (outcomes already uniform while the wire still carries an ok:true
        # frame) changes only on the frames, and crediting the round's own
        # UNIFORM_UNAVAILABLE violation to this control is a false PASS.
        unavailable('UNIFORM_UNAVAILABLE',
                    'every outcome rewritten to error CAPABILITY_UNAVAILABLE with matching wire',
                    'UNIFORM_UNAVAILABLE',
                    'every outcome already answered error CAPABILITY_UNAVAILABLE, so the rewrite '
                    'changes no outcome and the injection cannot be established')
    else:
        verdict = score_retirement_round(mutated, expected)
        record('UNIFORM_UNAVAILABLE', 'every outcome rewritten to error CAPABILITY_UNAVAILABLE with matching wire',
               'UNIFORM_UNAVAILABLE', verdict, {})
    # 4. 假 local 实透传: an undeclared non-loopback established peer appears.
    injected_peer = '203.0.113.7:443'
    injection = 'undeclared non-loopback established peer added to the gateway connections'
    connections = round_row.get('connections')
    # P1 (#1355 fix-r4e, anthropic sibling #1 = minimax boundary #1):
    # observability is a property of the round's *own* sample, not of the
    # mutated one.  Judging the mutated verdict let an empty or unparsed
    # sample count as landing — `_retirement_network_verdict` checks
    # `undeclared` before `unparsed or not rows`, so the injected row turned
    # the round's BLOCKED network face into FAIL and the class recorded PASS.
    # P1 (#1355 fix-r4f, anthropic sibling #1): asking the *verdict* about the
    # round's own sample has the same masking — a round with a pre-existing
    # undeclared peer (the real r2 round's provider egress, #1478) *and* an
    # unparsed line reads FAIL, not BLOCKED, and was credited as establishable.
    # So observability is the shape of the observation, never a verdict status.
    # A round that already reports the injected peer itself cannot attribute
    # the injection either (P1 #1355 fix-r4f, anthropic boundary #1): the set
    # delta would be empty and the injection would change nothing.
    original_network = (_retirement_network_verdict(connections)
                        if _retirement_network_observable(connections) else None)
    if not isinstance(connections, dict):
        unavailable('FORWARDING_MISLABELED', injection, 'NETWORK_FALLBACK',
                    'the control round recorded no network observation, so adding a non-loopback '
                    'peer cannot be established')
    elif original_network is None:
        unavailable('FORWARDING_MISLABELED', injection, 'NETWORK_FALLBACK',
                    "the control round's network observation is unobservable (failed lsof or an "
                    'empty / partly unparsed sample), so adding a non-loopback peer cannot be '
                    'established')
    elif injected_peer in (original_network.get('undeclared') or []):
        unavailable('FORWARDING_MISLABELED', injection, 'NETWORK_FALLBACK',
                    f'the control round already reports {injected_peer!r} as an undeclared peer, '
                    'so the injected peer cannot be told apart from the pre-existing one')
    else:
        undeclared_before = set(original_network.get('undeclared') or [])
        mutated_connections = json.loads(json.dumps(connections))
        mutated_connections['rows'] = (mutated_connections.get('rows') or []) + [
            {'peer': injected_peer, 'raw': 'controlled mutation'}]
        network_verdict = _retirement_network_verdict(mutated_connections)
        if network_verdict.get('status') == 'BLOCKED':
            unavailable('FORWARDING_MISLABELED', injection, 'NETWORK_FALLBACK',
                        f"the mutated sample is unobservable ({network_verdict.get('reason')}), "
                        'so adding a non-loopback peer cannot be established')
        else:
            record('FORWARDING_MISLABELED', injection, 'NETWORK_FALLBACK', network_verdict, {},
                   caused=lambda verdict: (set(verdict.get('undeclared') or [])
                                           == undeclared_before | {injected_peer}))
    # 5. R1 (#1355 fix-r2): a stream method recorded as an error outcome with
    # every receive frame stripped (send kept) must still fail EVIDENCE_BACKING
    # — the pre-fix scorer passed exactly this shape.
    mutated = json.loads(json.dumps(round_row))
    stream_row = next((row for row in mutated['outcomes']
                       if row.get('shape') == 'stream' and row.get('method')), None)
    injection = ('stream method rewritten to error with every receive frame stripped while '
                 'keeping its send frame')
    if stream_row is None:
        # P1 (#1355 fix-r4): an unestablishable class is reported, never raised.
        unavailable('STREAM_ERROR_NO_FRAME', injection, 'EVIDENCE_BACKING',
                    'no stream outcome carrying a method name in the control round, so the '
                    'error-without-receive-frame mutation cannot be established')
    else:
        stream_method = stream_row['method']
        stream_row['outcome'] = 'error'
        stream_row.pop('errorCode', None)
        before = len(mutated['frames'])
        mutated = _retirement_strip_receive(mutated, stream_method)
        if len(mutated['frames']) == before:
            # P1 (#1355 fix-r4b, both families): with no send frame (or no
            # receive frame under it) nothing is stripped, so the round was
            # already the failing shape and the PASS would be credited to that
            # pre-existing violation instead of a landed injection.
            unavailable('STREAM_ERROR_NO_FRAME', injection, 'EVIDENCE_BACKING',
                        f"stream method {stream_method!r} has no receive frame under a recorded "
                        'send frame in the control round, so the error-without-receive-frame '
                        'mutation cannot be established')
        else:
            verdict = score_retirement_round(mutated, expected)
            record('STREAM_ERROR_NO_FRAME', f"stream method '{stream_method}' rewritten to error with every "
                                           'receive frame stripped while keeping its send frame',
                   'EVIDENCE_BACKING', verdict, {},
                   require_pair=(stream_method, 'stream error without any receive frame'))
    # 6. ① (#1355 fix-r3): a stream ok outcome whose terminal event frame was
    # downgraded to final:false (the stream merely ended) must still fail
    # EVIDENCE_BACKING — the pre-fix scorer passed exactly this shape.
    mutated = json.loads(json.dumps(round_row))
    stream_ok = next((row for row in mutated['outcomes']
                      if row.get('shape') == 'stream' and row.get('outcome') == 'ok'
                      and row.get('method')), None)
    injection = 'stream ok outcome whose terminal event frame is downgraded to final:false'
    if stream_ok is None:
        # P1 (#1355 fix-r4): a legal degraded round (the only stream method
        # errored or timed out) leaves no ok stream to mutate.  This used to
        # raise EvidenceError and replace the whole 16-face report with a
        # 5-key BLOCKED stub — the very KeyError-on-face shape ② removed.
        unavailable('STREAM_OK_NON_FINAL', injection, 'EVIDENCE_BACKING',
                    'no ok stream outcome in the control round: the stream method errored or '
                    'timed out, so the terminal-frame mutation cannot be established')
    else:
        stream_method = stream_ok['method']
        sends, _responses, _events, _finals = _retirement_wire_backing(mutated['frames'])
        identifier = sends.get(stream_method)
        if identifier is None:
            # Same P1: without the send frame there is no id to locate the
            # terminal event; a KeyError here escaped even retirement_report's
            # EvidenceError handler and fell through to main's stub.
            unavailable('STREAM_OK_NON_FINAL', injection, 'EVIDENCE_BACKING',
                        f"stream ok method {stream_method!r} has no recorded send frame, "
                        'so its terminal event frame cannot be located')
        else:
            flipped = 0
            for frame_row in mutated['frames']:
                frame = frame_row.get('frame')
                if (isinstance(frame, dict) and frame.get('type') == 'event'
                        and frame.get('final') is True and frame.get('id') == identifier):
                    frame['final'] = False
                    flipped += 1
            if flipped == 0:
                # P1 (#1355 fix-r4): the round itself is the "stream ok
                # without a final:true frame" shape the scorer already FAILs,
                # so the downgrade mutation cannot be established and must not
                # collapse the report into the 5-key stub.  Duplicated terminal
                # frames (>=2) are all downgraded and scored normally: the
                # injection does land, and calling it unestablishable would
                # pin the controls face to BLOCKED on a legal round (P2a,
                # anthropic boundary #2).
                unavailable('STREAM_OK_NON_FINAL', injection, 'EVIDENCE_BACKING',
                            f"no terminal final:true event frame for {stream_method!r} in the "
                            'control round, so the downgrade mutation cannot be established')
            else:
                verdict = score_retirement_round(mutated, expected)
                record('STREAM_OK_NON_FINAL', f"stream method '{stream_method}' keeps its ok outcome while "
                                              'its terminal event frame is downgraded to final:false',
                       'EVIDENCE_BACKING', verdict, {},
                       require_pair=(stream_method, 'stream ok without a final event frame'))
    # The face status is a batch summary; it is computed by the registered
    # caller (_retirement_report) with the shared aggregator, never here.
    return {'classes': controls}


def _retirement_http_routes(baseline):
    """Parse the production HTTP route set from the baseline sources.

    Every row carries file:line provenance from the parsed source; the route
    set is never hand-listed.  One line can guard several routes
    (`path !== '/a' && path !== '/b'`), so every match on the line is
    collected; the same route under the same kind is never counted twice
    (first occurrence wins), so prefix/subPath aliases do not inflate the set.

    R6 (#1355 fix-r2): the parse has a registered completeness floor —
    RETIREMENT_REQUIRED_ROUTES must all be found, or the entry face is BLOCKED
    instead of silently passing on a shrunken denominator.  The baseline's
    `PROJECT_RE` literal is written with escaped slashes (a backslash before
    each separator in `/api/web/projects`), which the old unescaped pattern
    never matched; that pattern is removed and the projects prefix is covered
    by the `/api/web/` prefix (GatewayServer.ts) plus the absolute
    `/api/web/projects` (httpRouter.ts), both parsed and live-probed.
    """
    routes = []
    seen_kinds = set()

    def scan(relative, patterns):
        path = baseline / relative
        require(path.is_file(), 'ENTRY_SOURCE', f'baseline source missing: {path}')
        for lineno, line in enumerate(path.read_text().splitlines(), 1):
            line_seen = set()
            for pattern, kind in patterns:
                for route in re.findall(pattern, line):
                    if (route, kind) in line_seen:
                        continue
                    line_seen.add((route, kind))
                    if (route, kind) in seen_kinds:
                        continue
                    seen_kinds.add((route, kind))
                    routes.append({'route': route, 'kind': kind, 'source': f'{relative}:{lineno}',
                                   'line': line.strip()[:200]})

    scan('src/gateway/server/GatewayServer.ts',
         [(r'url\.pathname === "(/[^"]*)"', 'absolute'), (r'url\.pathname\.startsWith\("(/[^"]*)"', 'prefix'),
          (r'url\.pathname !== "(/[^"]*)"', 'absolute')])
    scan('src/adapters/web/httpRouter.ts',
         [(r'url\.pathname === "(/[^"]*)"', 'absolute'), (r'subPath === "(/[^"]*)"', 'subpath')])
    scan('src/composition/publicHostHttpTransport.ts', [(r"path !== '(/[^']*)'", 'absolute')])
    scan('src/composition/publicApprovalHttpTransport.ts', [(r"path !== '(/[^']*)'", 'absolute')])
    parsed = {(route['route'], route['kind']) for route in routes}
    require(set(RETIREMENT_REQUIRED_ROUTES) <= parsed, 'ENTRY_SOURCE',
            f'parsed route set lost registered entries: {sorted(set(RETIREMENT_REQUIRED_ROUTES) - parsed)}; '
            'the parser no longer finds every production HTTP entry')
    return routes


def _retirement_channel_listeners(baseline):
    """Channel adapters that own a listener socket, parsed from the baseline.

    A5 (#1355 fix-r2): provenance is the path relative to the baseline root —
    `path.name` dropped the adapter subdirectory, so all four real listeners
    recorded the wrong directory.
    """
    channel_dir = baseline / 'src/adapters/channel'
    require(channel_dir.is_dir(), 'ENTRY_SOURCE', f'baseline channel dir missing: {channel_dir}')
    entries = []
    for path in sorted(channel_dir.rglob('*.ts')):
        for lineno, line in enumerate(path.read_text().splitlines(), 1):
            if 'createServer(' in line:
                entries.append({'file': f'{path.relative_to(baseline).as_posix()}:{lineno}',
                                'line': line.strip()[:200]})
    files = {entry['file'].rsplit(':', 1)[0] for entry in entries}
    require(set(RETIREMENT_REQUIRED_LISTENERS) <= files, 'ENTRY_SOURCE',
            f'parsed channel-listener set lost registered entries: '
            f'{sorted(set(RETIREMENT_REQUIRED_LISTENERS) - files)}')
    return entries


def _retirement_gate_evidence(gate, route, round_row):
    """R7 (#1355 fix-r2): prove an entry is config-gated for this round.

    The gate holds only when the baseline still guards the route with the
    registered condition and the round config still lacks the credential key.
    Anything else returns False and the entry falls back to the ordinary
    HIT/NO_HIT evaluation (fail-closed).
    """
    if route['source'] != gate['source'] or gate['gate'] not in route['line']:
        return False
    try:
        config = Path(round_row['runtime']['pilotHome'], 'pilotdeck.yaml').read_text()
    except OSError:
        return False
    return gate['roundConfigKey'] not in config


def _retirement_entry_inventory(baseline, round_row):
    """Per-entry hit list; any unhit entry BLOCKS instead of silently passing.

    A2 (#1355 fix-r2): the cron/background HIT check counts only methods that
    were actually answered (ok/error), matching the scorer's executed set — a
    never-answered method must not satisfy an entry.  R7 (#1355 fix-r2):
    config/credential-gated entries that cannot be hit inside a round are
    reported as GATED with their gating evidence instead of blocking as
    unreachable; GATED entries still keep the face non-PASS (BLOCKED), never a
    silent pass.
    """
    http_probes = round_row['http']
    executed = {row['method'] for row in round_row['outcomes']
                if row.get('outcome') in ('ok', 'error')}
    # R6: the completeness floor is re-asserted at the consumer, so a parser
    # that silently returns less than the registered entry set (or a stubbed
    # one) cannot shrink the inventory's denominator.
    parsed_routes = _retirement_http_routes(baseline)
    parsed_listeners = _retirement_channel_listeners(baseline)
    require(set(RETIREMENT_REQUIRED_ROUTES) <= {(r['route'], r['kind']) for r in parsed_routes},
            'ENTRY_SOURCE', 'parsed route set lost registered entries: '
            f'{sorted(set(RETIREMENT_REQUIRED_ROUTES) - {(r["route"], r["kind"]) for r in parsed_routes})}')
    require({entry['file'].rsplit(':', 1)[0] for entry in parsed_listeners}
            >= set(RETIREMENT_REQUIRED_LISTENERS), 'ENTRY_SOURCE',
            'parsed channel-listener set lost registered entries: '
            f'{sorted(set(RETIREMENT_REQUIRED_LISTENERS) - {e["file"].rsplit(":", 1)[0] for e in parsed_listeners})}')

    def route_probes(route):
        """Live probes that exercised this parsed route, by parsed route kind."""
        name, kind = route['route'], route['kind']
        if name == '/ws':
            return []
        if kind == 'prefix':
            return [p for p in http_probes if p['path'] == name or p['path'].startswith(name.rstrip('/') + '/')]
        if kind == 'subpath':
            # subPath routes parsed from httpRouter.ts ('/files/tree', ...) live
            # under /api/web/projects/<key>; match by tail.
            return [p for p in http_probes if p['path'].split('?')[0].endswith(name)]
        return [p for p in http_probes if p['path'].split('?')[0] == name]

    entries, gated = [], []
    for route in _retirement_http_routes(baseline):
        name = route['route']
        matched = route_probes(route)
        gate = RETIREMENT_GATED_ENTRIES.get(name)
        if gate is not None and name != '/ws' and _retirement_gate_evidence(gate, route, round_row):
            gated.append(name)
            entries.append({'entry': name, 'kind': 'http', 'source': route['source'],
                            'probes': [{'path': p['path'], 'status': p['status']} for p in matched],
                            'status': 'GATED',
                            'reason': f"config/credential-gated ({gate['gate']}) and 404 inside this round; "
                                      f"{gate['reason']}"})
            continue
        if name == '/ws':
            # The whole driver session is one live upgrade; wire frames prove it.
            hit, reason = bool(round_row['frames']), (None if round_row['frames'] else 'no websocket frames recorded')
        else:
            hit = any(p['status'] not in (None, 404) for p in matched)
            reason = None if hit else 'route answered 404 or was unreachable'
        entries.append({'entry': name, 'kind': 'http', 'source': route['source'],
                        'probes': [{'path': p['path'], 'status': p['status']} for p in matched],
                        'status': 'HIT' if hit else 'NO_HIT', 'reason': reason})
    for listener in _retirement_channel_listeners(baseline):
        entries.append({'entry': listener['file'], 'kind': 'channel-listener', 'source': listener['file'],
                        'probes': [], 'status': 'STATIC_VERIFIED',
                        'reason': 'createServer call verified in the baseline channel adapter'})
    cron_executed = any(method.startswith('cron_') for method in executed)
    entries.append({'entry': 'cron', 'kind': 'background', 'source': 'home/pilotdeck.yaml cron.enabled + cron_* methods',
                    'probes': sorted(method for method in executed if method.startswith('cron_')),
                    'status': 'HIT' if cron_executed else 'NO_HIT',
                    'reason': None if cron_executed else 'no cron method executed'})
    background_executed = any(method in ('background_tasks', 'background_task_stop') for method in executed)
    entries.append({'entry': 'background', 'kind': 'background', 'source': 'background_tasks/background_task_stop methods',
                    'probes': sorted(method for method in executed if method.startswith('background')),
                    'status': 'HIT' if background_executed else 'NO_HIT',
                    'reason': None if background_executed else 'no background method executed'})
    unhit = [row['entry'] for row in entries if row['status'] == 'NO_HIT']
    # GATED entries keep the face BLOCKED (aggregate_status semantics: a
    # face that could not observe one of its entries is not PASS); they are
    # listed separately from unhit so "unhit" keeps meaning "should have been
    # reachable and was not".
    return {'status': 'BLOCKED' if unhit or gated else 'PASS',
            'unhit': unhit, 'gated': gated, 'entries': entries}


def _retirement_entry_inventory_aggregate(per_round):
    """④ (#1355 fix-r3): merge the per-round entry inventories into one face.

    An entry is NO_HIT when any round failed to hit it (fail-closed — an entry
    that answered in round 1 but 404'd in round 2 must not be hidden by round
    1, the pre-fix face only read rounds[0]); GATED keeps its own semantics
    (never a silent pass) but never outranks a NO_HIT.  Probes accumulate so
    every round's live answer stays visible, and each entry carries its
    per-round statuses.
    """
    status_precedence = ('NO_HIT', 'GATED', 'HIT', 'STATIC_VERIFIED')
    merged, order = {}, []
    for label, inventory in per_round:
        for entry in inventory['entries']:
            key = (entry['entry'], entry['kind'])
            if key not in merged:
                merged[key] = {'entry': entry['entry'], 'kind': entry['kind'],
                               'source': entry['source'], 'probes': [], 'rounds': []}
                order.append(key)
            merged[key]['probes'].extend(entry.get('probes', []))
            merged[key]['rounds'].append({'round': label, 'status': entry['status']})
    entries = []
    for key in order:
        row = merged[key]
        statuses = [round_row['status'] for round_row in row['rounds']]
        status = next((candidate for candidate in status_precedence if candidate in statuses), statuses[-1])
        entries.append({'entry': row['entry'], 'kind': row['kind'], 'source': row['source'],
                        'probes': row['probes'], 'rounds': row['rounds'], 'status': status})
    unhit = [row['entry'] for row in entries if row['status'] == 'NO_HIT']
    gated = [row['entry'] for row in entries if row['status'] == 'GATED']
    return {'status': 'BLOCKED' if unhit or gated else 'PASS',
            'unhit': unhit, 'gated': gated, 'entries': entries}


def _retirement_js_host_census():
    """Read-only census of JS host boundary call sites in the Rust tree.

    #132 (isolated UTF-16 surrogate sorting) and #1265 (outputSchema regex):
    the census lists every JS_HOST_REQUIRED site under src/pilotdeck-gateway/src
    so the Rust-side retirement round scores wiring against a recorded list
    instead of a fresh grep.
    """
    root = ROOT / 'src/pilotdeck-gateway/src'
    rows = []
    if root.is_dir():
        for path in sorted(root.rglob('*.rs')):
            for lineno, line in enumerate(path.read_text(errors='replace').splitlines(), 1):
                if 'JS_HOST_REQUIRED' in line:
                    rows.append({'file': path.relative_to(ROOT).as_posix(), 'line': lineno,
                                 'text': line.strip()[:200]})
    return {'status': 'CENSUS_RECORDED', 'count': len(rows), 'entries': rows,
            'note': 'static census; the wired-to-production verdict (#132/#1265) belongs to the '
                    'Rust-side retirement round'}


def _retirement_ports_available(ports):
    try:
        for port in ports:
            with socket.socket() as sock:
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                sock.bind(('127.0.0.1', port))
        return True
    except OSError:
        return False


def _retirement_round(index, output, baseline, ports, timeout):
    """One real TS-side gateway round: mock model + baseline CLI + driver.

    Production always drives the frozen 101-entry fixture
    (RETIREMENT_METHODS_FIXTURE); coverage scoring enforces the full expected
    set regardless of what the driver ran.
    """
    run_round_module = _load('run_round', common.SCRIPT / 'run_round.py')
    mock_port, gateway_port, webui_port = ports
    folder = output / f'round-{index:02d}'
    if folder.exists():
        raise EvidenceError('ROUND_OVERWRITE', f'{folder} already exists')
    folder.mkdir(parents=True)
    home, project = folder / 'home', folder / 'project'
    for path in (home, project, folder / 'tmp'):
        path.mkdir()
    registration = home / 'projects/gateway-parity-fixture'
    registration.mkdir(parents=True)
    (registration / '.cwd').write_text(str(project) + '\n')
    (project / 'note.txt').write_text('gateway parity fixture\n')
    save(home / 'permissions.json', {'version': 1, 'allowedTools': [], 'askTools': ['bash'],
                                      'disallowedTools': [], 'skipPermissions': False})
    token_path = home / 'server-token'
    token_path.write_text(run_round_module.TOKEN + '\n')
    token_path.chmod(0o600)
    (home / 'pilotdeck.yaml').write_text(f'''schemaVersion: 1
agent:
  model: parity/deterministic
  maxContextTokens: 65536
  maxOutputTokens: 8192
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:{mock_port}/v1
      apiKey: gateway-parity-mock-only
      models:
        deterministic:
          capabilities:
            supportsToolUse: true
            maxContextTokens: 65536
            maxOutputTokens: 8192
telemetry:
  enabled: false
cron:
  enabled: true
  timezone: UTC
webui:
  runtime:
    host: 127.0.0.1
    serverPort: {webui_port}
    databasePath: {json.dumps(str(home / 'auth.db'))}
    workspacesRoot: {json.dumps(str(folder / 'workspaces'))}
''')
    (folder / 'workspaces').mkdir()
    env = {key: val for key, val in os.environ.items() if not key.startswith(('PILOTDECK_', 'EXP003_', 'GP_'))
           and key not in ('PILOT_HOME', 'DATABASE_PATH', 'NODE_OPTIONS', 'WORKSPACES_ROOT')}
    env.update(PDRS_BASELINE=str(baseline), PILOT_HOME=str(home), GP_PROJECT=str(project),
               DATABASE_PATH=str(home / 'auth.db'), TMPDIR=str(folder / 'tmp'), WORKSPACES_ROOT=str(folder / 'workspaces'),
               PILOTDECK_GATEWAY_PORT=str(gateway_port), PILOTDECK_GATEWAY_HOST='127.0.0.1',
               PILOTDECK_GATEWAY_TOKEN=run_round_module.TOKEN,
               PILOTDECK_GATEWAY_TOKEN_PATH=str(token_path),
               GP_POLL_WATCH='1', PILOTDECK_SKIP_BROWSER_OPEN='1', GP_MOCK_PORT=str(mock_port),
               GP_MOCK_URL=f'http://127.0.0.1:{mock_port}', GP_MODEL_LOG=str(folder / 'model.jsonl'))
    processes = []
    row = {'label': f'round-{index:02d}', 'status': 'RUNNING',
           'ports': {'mock': mock_port, 'gateway': gateway_port},
           'runtime': {'pilotHome': str(home), 'cwd': str(project)}, 'driverExitCode': None}
    start_time = time.monotonic()

    def launch(name, argv, port):
        log = (folder / f'{name}.log').open('w')
        entry = {'name': name, 'port': port, 'argv': [str(item) for item in argv]}
        proc = run_round_module.start_owned(processes, log, entry, entry['argv'], cwd=project,
                                            env=env, stdout=log, stderr=subprocess.STDOUT)
        save(folder / 'processes.json', [item[2] for item in processes])
        return proc

    def ready(port, proc, deadline):
        while time.monotonic() < deadline:
            try:
                code = run_round_module.wait_owned(proc, 0)
            except subprocess.TimeoutExpired:
                pass
            else:
                raise RuntimeError(f'owned process exited {code}')
            try:
                with urllib.request.urlopen(f'http://127.0.0.1:{port}/health', timeout=.5) as response:
                    if response.status == 200:
                        return
            except (OSError, urllib.error.URLError):
                time.sleep(.1)
        raise TimeoutError(f'port {port} readiness timeout')

    try:
        try:
            deadline = start_time + timeout
            mock = launch('mock', ['node', SCRIPT / 'mock.mjs'], mock_port)
            ready(mock_port, mock, min(deadline, time.monotonic() + RETIREMENT_READINESS_TIMEOUT_S))
            gateway = launch('gateway', ['node', '--import', SCRIPT / 'watcher.mjs',
                                         baseline / 'dist/src/cli/pilotdeck.js', 'server'], gateway_port)
            ready(gateway_port, gateway, min(deadline, time.monotonic() + RETIREMENT_READINESS_TIMEOUT_S))
        except (RuntimeError, TimeoutError, OSError) as error:
            # A4 (#1355 fix-r2): convert the launch failure into a retryable
            # signal so _retirement_report can move to the alternate port set.
            raise RetirementLaunchError(f'{type(error).__name__}: {error}') from error
        listener_probe = subprocess.run(['lsof', '-nP', '-a', '-p', str(gateway.pid), '-iTCP', '-sTCP:LISTEN', '-Fn'],
                                         capture_output=True, text=True, timeout=5)
        addresses = [line[1:] for line in listener_probe.stdout.splitlines() if line.startswith('n')]
        row['listeners'] = {'pid': gateway.pid, 'addresses': addresses,
                            'loopbackOnly': addresses == [f'127.0.0.1:{gateway_port}']}
        driver = launch('driver-retirement', ['node', '--import', baseline / 'node_modules/tsx/dist/loader.mjs',
                                              RETIREMENT_DRIVER, '--url', f'ws://127.0.0.1:{gateway_port}/ws',
                                              '--token', run_round_module.TOKEN, '--methods', RETIREMENT_METHODS_FIXTURE,
                                              '--out', folder / 'driver'], None)
        row['driverExitCode'] = run_round_module.wait_owned(
            driver, max(.1, deadline - time.monotonic()))
        row['census'] = _retirement_census(gateway.pid, baseline)
        row['connections'] = _retirement_connections(gateway.pid)
        require((folder / 'driver' / 'methods.json').is_file(), 'ROUND_EVIDENCE', 'driver methods.json missing')
        require((folder / 'driver' / 'frames.jsonl').is_file(), 'ROUND_EVIDENCE', 'driver frames.jsonl missing')
        require((folder / 'driver' / 'http.json').is_file(), 'ROUND_EVIDENCE', 'driver http.json missing')
        row['outcomes'] = read(folder / 'driver' / 'methods.json')['methods']
        row['frames'] = [json.loads(line) for line in
                         (folder / 'driver' / 'frames.jsonl').read_text().splitlines() if line.strip()]
        row['http'] = read(folder / 'driver' / 'http.json')['probes']
        row['status'] = 'DONE'
    finally:
        with deferred_signals():
            row['audit'] = run_round_module.cleanup_owned(processes)
            row['elapsedSeconds'] = time.monotonic() - start_time
            row['processes'] = [item[2] for item in processes]
            save(folder / 'processes.json', [item[2] for item in processes])
            save(folder / 'round.json', {key: row[key] for key in
                                         ('label', 'status', 'ports', 'runtime', 'driverExitCode',
                                          'listeners', 'census', 'connections', 'processes',
                                          'audit', 'elapsedSeconds')
                                         if key in row})
    require(_cleanup_stopped(row['audit']), 'CLEANUP',
            f"round {index}: owned process-group cleanup unverified: {row['audit']}")
    return row


def _cleanup_stopped(audit):
    """#2519: the one shared read of a round audit's stopped verdict.

    ``audit.allStopped`` is the cleanup criterion the round face's CLEANUP
    require and the report face's cleanupAudit aggregation both judge on
    (A8.6 cleanup audit); each face used to keep its own read, so the two
    could drift (#2519, from the #1355 r1 harvest review).  Deliberately a
    truthiness read, not ``is True``: both call sites judged truthiness
    before, real rounds record a bool from cleanup_owned, and tightening
    here would change the registered scoring semantics.
    """
    return bool(audit['allStopped'])


def _retirement_baseline_revision(baseline):
    """R8 (#1355 fix-r2): pin and record the baseline revision the round ran.

    A report without the baseline path/revision cannot be reproduced, so the
    registered revision is asserted before any round starts.  A baseline that
    is not a git checkout is reported BLOCKED (rule BASELINE_REVISION), never
    bypassed.
    """
    try:
        result = subprocess.run(['git', '-C', str(baseline), 'rev-parse', 'HEAD'],
                                capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError) as error:
        raise EvidenceError('BASELINE_REVISION', f'git rev-parse failed: {error}') from error
    require(result.returncode == 0, 'BASELINE_REVISION',
            f'git rev-parse exited {result.returncode}: {result.stderr.strip() or "(no stderr)"}')
    revision = result.stdout.strip()
    require(revision == RETIREMENT_BASELINE_REVISION, 'BASELINE_REVISION',
            f'baseline revision {revision or "(empty)"} != registered {RETIREMENT_BASELINE_REVISION}')
    return revision


def _retirement_report(output, baseline):
    """TS calibration slice: k real TS rounds, scoring, five controls, audits."""
    expected, provenance = _retirement_expected(baseline)
    revision = _retirement_baseline_revision(baseline)
    rounds, port_attempts = [], []
    for index in range(1, RETIREMENT_ROUNDS + 1):
        for attempt, ports in enumerate(RETIREMENT_PORT_SETS):
            if not _retirement_ports_available(ports):
                port_attempts.append({'round': index, 'ports': list(ports),
                                      'outcome': 'AddrInUse: retried on the alternate port set'})
                continue
            # DS P2-11 (#1355 fix-r1): portAudit records the successful port
            # set too, not only the skipped ones.
            port_attempts.append({'round': index, 'ports': list(ports),
                                  'outcome': 'bound: round executed on this port set'})
            try:
                rounds.append(_retirement_round(index, output, baseline, ports, RETIREMENT_ROUND_TIMEOUT))
                break
            except RetirementLaunchError as error:
                # A4 (#1355 fix-r2): retry the launch on the alternate port
                # set; the failed attempt's evidence folder is kept on disk,
                # never deleted.
                archive = output / f'round-{index:02d}-launch-failed-{attempt + 1}'
                (output / f'round-{index:02d}').rename(archive)
                port_attempts.append({'round': index, 'ports': list(ports),
                                      'outcome': f'launch failed; evidence kept in {archive.name}: {error}'})
        else:
            raise EvidenceError('PORT_CONFLICT',
                                f'round {index}: every port set was in use or failed to launch')
    verdicts = [score_retirement_round(row, expected) for row in rounds]
    coverage_status = common.aggregate_status(*(verdict['status'] for verdict in verdicts))[0]
    # controls read rounds[0] by design (data mutation controls; the
    # arbitration keeps this rounds[0] use exempt from ④).
    controls = _retirement_controls(rounds[0], expected)
    # P1 (#1355 fix-r4): a control a legal degraded round cannot establish is
    # reported NOT_APPLICABLE and aggregates to BLOCKED (never PASS), so the
    # controls face and the total verdict stay fail-closed.  The batch status
    # comes from the one shared aggregator (aggregation-audit).
    controls_status = common.aggregate_status(
        *('BLOCKED' if row['status'] == 'NOT_APPLICABLE' else row['status']
          for row in controls['classes']))[0]
    controls = {**controls, 'status': controls_status}
    unavailable = [row for row in controls['classes'] if row['status'] == 'NOT_APPLICABLE']
    if unavailable:
        controls['reason'] = ('positive mutation control(s) could not be established on this round: '
                              + '; '.join(f"{row['class']}: {row['reason']}" for row in unavailable))
    # ④ (#1355 fix-r3): the inventory face reads every round, not round 1 —
    # an entry that answered in round 1 but 404'd in round 2 is NO_HIT.
    inventory = _retirement_entry_inventory_aggregate(
        [(row['label'], _retirement_entry_inventory(baseline, row)) for row in rounds])
    census_verdicts = [_retirement_report_census(row) for row in rounds]
    # R5 (#1355 fix-r2): the network face reads as what it is — one
    # post-driver sample of the gateway pid's own established peers (provider
    # egress included); it does not cover other TS hosts.  The rule itself is
    # deliberately not relaxed: a non-loopback peer still FAILs, and the
    # structural provider-egress FAIL is registered as a known boundary (#1478).
    network_verdicts = []
    for row in rounds:
        connections = row.get('connections')
        verdict = _retirement_network_verdict(connections if isinstance(connections, dict) else {})
        verdict.update({'samplingScope': 'gateway-pid-only',
                        'gatewayPid': _retirement_gateway_pid(row),
                        'peerOwnership': "every sampled peer belongs to this round's own gateway pid "
                                         '(provider egress included); other TS hosts are not covered '
                                         'by this sample'})
        network_verdicts.append(verdict)
    listener_verdicts = [_retirement_listener_verdict(row.get('listeners'), row['ports']['gateway'])
                         for row in rounds]
    host_status = common.aggregate_status(*(verdict['status'] for verdict in
                                            census_verdicts + network_verdicts + listener_verdicts))[0]
    cold_restart = {'status': 'BLOCKED', 'rule': 'COLD_RESTART_PENDING',
                    'reason': 'pending #1354; explicit placeholder, never a silent skip'}
    overall = common.aggregate_status(coverage_status, inventory['status'], host_status,
                                      controls['status'], cold_restart['status'])[0]
    return {
        'status': overall,
        'reason': 'E1 retirement TS calibration slice (#1355): expected/executed collected from real '
                  'TS gateway rounds; cold-restart/rollback stays BLOCKED pending #1354 and the frozen '
                  'hash re-registration is the coordinator receive step',
        'baseline': {'path': str(baseline), 'revision': revision},
        'expected': {'methods': expected, 'count': len(expected), 'provenance': provenance},
        'executed': {verdict['round']: {'count': len(verdict['executed']), 'methods': verdict['executed']}
                     for verdict in verdicts},
        'rounds': [{'round': verdict['round'], 'status': verdict['status'], 'rules': verdict['rules'],
                    'driverExitCode': verdict['driverExitCode'], 'executedCount': len(verdict['executed']),
                    'audit': row.get('audit'), 'listeners': row.get('listeners'),
                    'folder': (output / row['label']).as_posix()} for verdict, row in zip(verdicts, rounds)],
        'methodCoverage': {'status': coverage_status, 'k': RETIREMENT_ROUNDS, 'rounds': verdicts},
        'entryInventory': inventory,
        'hostAudit': {'status': host_status, 'processCensus': census_verdicts, 'network': network_verdicts,
                      'listeners': listener_verdicts,
                      'jsHostRequired': _retirement_js_host_census(),
                      'nativeBinConfig': {'status': 'NOT_RUN',
                                          'reason': 'native bin TS-upstream config audit belongs to the '
                                                    'Rust-side retirement round'}},
        'controls': controls,
        'coldRestartRollback': cold_restart,
        'topology': {'host': 'ts-reference', 'sidecar': 'ts-mock-model', 'entry': 'official GatewayWsClient driver',
                     # DS P2-13 (#1355 fix-r1): each round's actual ports, not
                     # round 1's set silently standing in for all of them.
                     'ports': [{'round': row['label'], 'mock': row['ports']['mock'],
                                'gateway': row['ports']['gateway']} for row in rounds]},
        'portAudit': port_attempts,
        'processAudit': [row['audit'] for row in rounds],
        'cleanupAudit': {'status': 'PASS' if all(_cleanup_stopped(row['audit']) for row in rounds) else 'FAIL'},
        # R9 (#1355 fix-r2): executionStatus is the overall verdict; the
        # coverage face lives in methodCoverage.status.  A BLOCKED report must
        # never read as an executed PASS.
        'executionStatus': overall,
        'acceptanceStatus': 'NOT_RUN',
    }


def _retirement_budget_blocked(baseline, error):
    """② (#1355 fix-r3): a round that exceeded the declared budget reports
    BLOCKED with every scoring face present (the A8.6 consumer list:
    methodCoverage/entryInventory/hostAudit/controls/coldRestartRollback/
    executionStatus/expected/executed/rounds/topology/…), so a consumer never
    KeyErrors on a budget-blocked report.  Faces carry no measurements — they
    are BLOCKED/NOT_RUN, never an optimistic read."""
    return {
        'status': 'BLOCKED',
        'reason': 'retirement TS calibration exceeded its declared round budget before a '
                  'certifiable result; the faces below carry no measurements',
        'failure': {'rule': 'ROUND_BUDGET', 'detail': f'{type(error).__name__}: {error}'},
        'baseline': {'path': str(baseline), 'revision': None},
        'expected': {'methods': [], 'count': 0, 'provenance': []},
        'executed': {},
        'rounds': [],
        'methodCoverage': {'status': 'BLOCKED', 'k': RETIREMENT_ROUNDS, 'rounds': []},
        'entryInventory': {'status': 'BLOCKED', 'unhit': [], 'gated': [], 'entries': []},
        'hostAudit': {'status': 'BLOCKED', 'processCensus': [], 'network': [], 'listeners': [],
                      'jsHostRequired': {'status': 'NOT_RUN',
                                         'reason': 'round budget exceeded before the static census ran'},
                      'nativeBinConfig': {'status': 'NOT_RUN',
                                          'reason': 'native bin TS-upstream config audit belongs to the '
                                                    'Rust-side retirement round'}},
        'controls': {'status': 'NOT_RUN', 'classes': []},
        'coldRestartRollback': {'status': 'NOT_RUN', 'rule': 'COLD_RESTART_PENDING',
                                'reason': 'pending #1354; not reached before the budget block'},
        'topology': {'host': 'ts-reference', 'sidecar': 'ts-mock-model',
                     'entry': 'official GatewayWsClient driver'},
        'portAudit': [],
        'processAudit': [],
        'cleanupAudit': {'status': 'NOT_RUN'},
        'executionStatus': 'BLOCKED',
        'acceptanceStatus': 'NOT_RUN',
    }


def retirement_report(output, baseline):
    """BLOCKED wrapper: a structural refusal is never a scored FAIL."""
    try:
        return _retirement_report(output, baseline)
    except EvidenceError as error:
        return {'status': 'BLOCKED', 'failure': {'rule': error.rule, 'detail': str(error)},
                'executionStatus': 'NOT_RUN', 'acceptanceStatus': 'NOT_RUN',
                'reason': 'retirement TS calibration could not produce a certifiable result'}
    except (subprocess.TimeoutExpired, TimeoutError) as error:
        # ② (#1355 fix-r3): a TimeoutExpired from the round driver (wait_owned,
        # census/connections probes) escaped to main and left a 9-key report
        # whose scoring faces are all missing; it is a budget-bound signal and
        # reports BLOCKED with the full face set instead.
        return _retirement_budget_blocked(baseline, error)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suite', choices=('calibrate', 'deadline', 'retirement'), required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--baseline', type=Path, default=BASELINE)
    parser.add_argument('--native-capture', type=Path,
                        help='#1140 native witness capture directory; the deadline suite '
                             'audits it and scores its rust-host/rust-sidecar cell against '
                             'the registered A19-HOST expectations')
    args = parser.parse_args()
    takeovers = []
    # Preconditions are checked before any output directory exists (#187); they
    # stay above install_shutdown so the #166 lease/shutdown anchors in the
    # gateway-batch source suite keep their healthy text.
    if args.suite != 'deadline' and args.native_capture is not None:
        print(json.dumps({'suite': args.suite, 'status': 'BLOCKED',
                          'error': '--native-capture applies to the deadline suite only'},
                         ensure_ascii=False))
        return 2
    if args.suite == 'deadline' and args.native_capture is not None:
        capture = args.native_capture.resolve()
        if not ((capture / 'capture').is_dir() and (capture / 'session-domain').is_dir()):
            print(json.dumps({'suite': args.suite, 'status': 'BLOCKED',
                              'error': 'native witness capture directory lacks capture/ or '
                                       f'session-domain/: {capture}'}, ensure_ascii=False))
            return 2
    install_shutdown()
    try:
        with parity_lock(timeout=30, takeovers=takeovers):
            if args.suite == 'retirement':
                # Preconditions before any output directory exists (#187).
                missing = [str(path) for path in (RETIREMENT_METHODS_FIXTURE, RETIREMENT_DRIVER)
                           if not path.is_file()]
                if not (args.baseline / 'dist/src/cli/pilotdeck.js').is_file():
                    missing.append(f'{args.baseline.resolve()}/dist/src/cli/pilotdeck.js')
                if missing:
                    print(json.dumps({'suite': args.suite, 'status': 'BLOCKED',
                                      'error': 'retirement preconditions missing: ' + '; '.join(missing[:4])
                                               + '; pass --baseline'}, ensure_ascii=False))
                    return 2
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
            if args.suite in ('calibrate', 'retirement'):
                try:
                    dependencies = baseline_dependency_hashes(args.baseline.resolve())
                except ValueError as error:
                    print(json.dumps({'suite': args.suite, 'status': 'BLOCKED', 'error': str(error)},
                                     ensure_ascii=False))
                    return 2
            output.mkdir(parents=True, exist_ok=False)
            # A8 (#1355 fix-r2): the retirement suite reports its own scope
            # instead of inheriting the calibrate slice's A5.3 label.
            report = {'suite': args.suite,
                      'scope': 'A8.6 draft apparatus' if args.suite == 'retirement' else 'A5.3 draft apparatus',
                      'status': 'RUNNING',
                      'acceptanceStatus': 'NOT_RUN', 'apparatusSha256': hashes(), 'frozen': frozen_check()}
            if dependencies is not None:
                report['baselineDepsSha256'] = dependencies
            save(output / 'report.json', report)
            code = 0
            try:
                if args.suite == 'deadline':
                    if report['frozen']['exitCode'] != 0:
                        raise ApparatusVerificationError('FROZEN: deadline requires successful frozen check')
                    report.update(deadline_report(args.native_capture))
                    _, code = common.aggregate_status(report['status'])
                elif args.suite == 'retirement':
                    # TS calibration slice (#1355): the frozen re-registration is the
                    # coordinator receive step, so measurements are recorded and the
                    # overall status stays BLOCKED while frozen exitCode != 0.
                    report['lockTakeovers'] = takeovers
                    report.update(retirement_report(output, args.baseline.resolve()))
                    _, code = common.aggregate_status(
                        report['status'] if report['status'] in common.BATCH_STATUS_ORDER else 'BLOCKED')
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
                    if audit.get('ownedGroupsRemaining'):
                        report['cleanupPending'] = audit['ownedGroupsRemaining']
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
                if report['frozen']['exitCode'] != 0:
                    report['status'], code = common.aggregate_status('BLOCKED')
                    report['freezeStatus'] = 'BLOCKED'
                    report['failure'] = {'rule': 'FROZEN', 'detail': 'measurements completed; see frozen check output; no acceptance PASS'}
            except (EvidenceError, OSError, ValueError, KeyError, TypeError, AttributeError, IndexError, StopIteration,
                    subprocess.SubprocessError, TimeoutError, RuntimeError, KeyboardInterrupt) as error:
                report['status'] = 'BLOCKED' if isinstance(error, BLOCKED_APPARATUS_ERRORS) else 'FAIL'
                if args.suite == 'deadline':
                    report.pop('cells', None)  # No consumable partial score after failed final verification.
                    report['executionStatus'] = 'NOT_RUN'
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

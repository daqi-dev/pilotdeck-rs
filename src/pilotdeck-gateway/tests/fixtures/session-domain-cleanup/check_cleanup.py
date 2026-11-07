#!/usr/bin/env python3
# anchor-selfcheck: unsupported runtime process-census/probe monkeypatch controls; snapshots is an iterator of PID sets, not source bytes; no source replacement anchors
"""Behavior controls for the ps-free process-group cleanup probe.

Run with ``python3 -I check_cleanup.py --output <new directory>``.  This file
does not mutate source; the post-commit source mutation controls are in
``negative_controls.py`` beside it.

Modes:
  clean  - reaped leader, empty group -> VERIFIED / PASS
  leak   - reaped leader, sleep grandchild -> GROUP_NOT_EMPTY / non-PASS
  ignore - grandchild ignores SIGTERM (bounded ready handshake) -> GROUP_NOT_EMPTY /
           non-PASS, then SIGKILL
  blocked - killpg and leader kill denied -> BLOCKED / non-PASS
  eperm-live-member - only the group probe is denied; a live grandchild must make
           the census report GROUP_NOT_EMPTY instead of VERIFIED
  identity-lost - the leader was already reaped: NOT_VERIFIED and no signal
  census-unavailable - the live-member census is missing: BLOCKED, not empty
  census-snapshot - a member appears between two group snapshots: GROUP_NOT_EMPTY
  census-descendant - a stable pair including the descendant reports it
  census-unstable - two complete scans disagree: no stable census
  census-truncated - libproc reports a full buffer: no complete enumeration
  census-enumeration - enumeration failure: BLOCKED, not empty
  pid-blocked - kill(pid, 0) EPERM is 'blocked', never 'gone'
  linux-filtered-proc - hidepid-style visible /proc subset on Linux: BLOCKED,
                       no census read, never VERIFIED/PASS
  signal-entry - the single signal entry refuses a reaped leader, sends while held
  reap-state - the leader was reported REAPED: no bare kill, NOT_VERIFIED
  status - status mapping for VERIFIED / GROUP_NOT_EMPTY / BLOCKED / NOT_VERIFIED
  wiring - drive session_domain.main()'s real cleanup tail with a non-empty
           group; pin status / executionStatus / freezeStatus / exit 2
  runtime-negative - replace probe at runtime with VERIFIED; the leaky fixture must
                     then look accepted (old behavior), and the real probe must
                     still report GROUP_NOT_EMPTY afterwards
  batch-aggregation - batch status order and exit codes: PASS_WITH_SKIPS /
                     BLOCKED / NOT_RUN can never become top PASS or exit 0;
                     calibrate maps a skipped controls batch to BLOCKED/exit 2
  all    - every mode above (default)

Darwin-only positives (clean, leak, ignore, eperm-live-member, runtime-negative)
are recorded as ``passed: null`` + ``skipped`` on other platforms: the non-Darwin
probe is BLOCKED before any census, so their positive expectation is unprovable
there.  Skips are never counted as passes; the summary is PASS_WITH_SKIPS.
"""
import argparse
import ast
import contextlib
import copy
import importlib.util
import atexit
import io
import json
import os
from pathlib import Path
from types import SimpleNamespace
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from unittest.mock import patch

# Loading the apparatus by explicit path is still an import: stop bytecode
# writes before the loader reads the source, otherwise a __pycache__ appears in
# the frozen apparatus directory and check_frozen (correctly) rejects it.
# #892 r6/r9: every bytecode prefix this process creates lives in one private
# root, so a batch of mutants (one interpreter each) leaves nothing behind.
# Adopting prefixes one at a time was not enough: the frozen apparatus rewrites
# the process-global ``sys.pycache_prefix`` at import time in every module it
# loads (``session_domain.py:44``, ``controls/session_domain.py:35``,
# ``common.py`` ...), so the earlier directory of each load still leaked
# (MiniMax P2 r6, MiniMax sibling r7 P2-2).  Pointing ``tempfile`` itself at
# the root catches them all without ever touching a directory another process
# owns.
_PYCACHE_ROOT = tempfile.mkdtemp(prefix='pdrs-session-cleanup-pycroot-')
tempfile.tempdir = _PYCACHE_ROOT
sys.pycache_prefix = _PYCACHE_ROOT
atexit.register(shutil.rmtree, _PYCACHE_ROOT, ignore_errors=True)
sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[5]
LAUNCHER = Path(__file__).resolve().with_name('launcher.py')
SESSION_DOMAIN = ROOT / 'scripts/gateway-parity/session_domain.py'
CONTROLS = ROOT / 'scripts/gateway-parity/controls/session_domain.py'
md = None


def load_session_domain():
    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)
    module = importlib.util.module_from_spec(spec)
    sys.modules['session_domain'] = module
    source = Path(spec.origin).read_bytes()
    exec(compile(source, spec.origin, 'exec'), module.__dict__)
    module._apparatus_source = source
    return module


def load_controls():
    spec = importlib.util.spec_from_file_location('session_cleanup_controls', CONTROLS)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    source = Path(spec.origin).read_bytes()
    exec(compile(source, spec.origin, 'exec'), module.__dict__)
    module._apparatus_source = source
    return module


def platform_skip(mode):
    """Reason a Darwin-only positive cannot be proven on this platform."""
    if sys.platform == 'darwin':
        return None
    return (f'{mode} requires complete Darwin group enumeration; '
            'the non-darwin probe is BLOCKED before any census')


def skipped_result(mode, reason):
    return {'mode': mode, 'skipped': reason, 'detail': reason, 'passed': None}


def summarize_results(results):
    """Pessimistic fixture summary: a skip is never a pass.

    A row with ``passed=None`` must carry the explicit ``skipped`` marker;
    otherwise the missing result is malformed and fails closed.  The final
    status and exit code come from the one shared common.aggregate_status().
    """
    executed = [row for row in results if row.get('passed') is not None]
    skipped = [row for row in results
               if row.get('passed') is None and row.get('skipped')]
    malformed = [row for row in results
                 if row.get('passed') is None and not row.get('skipped')]
    if (malformed or not executed
            or any(row.get('passed') is not True for row in executed)):
        # ``is False`` let a malformed ``passed=0`` / ``passed='no'`` count as
        # a non-failing row and the run report PASS; only a literal ``True``
        # passes (MiniMax boundary r19 P2-3).
        return md.common.aggregate_status('FAIL')[0], skipped
    status, _ = md.common.aggregate_status(
        'PASS', 'PASS_WITH_SKIPS' if skipped else None)
    return status, skipped


def pid_state(pid):
    """Three-state pid observation; ``blocked`` never means gone."""
    return md.HeldProcessGroup.observe_pid(pid)


def grandchild_view(pid):
    """Derive the leftover assertion from the three-state observation.

    Only a proven ``missing`` pid may be reported as gone.  ``blocked``
    (EPERM/EACCES) proves only that this process cannot signal it, so it must
    stay "cannot exclude alive" and make the leftover check fail.
    """
    if pid is None:
        return {'grandchildState': None, 'grandchildAlive': False,
                'grandchildGone': False}
    state = pid_state(pid)
    return {'grandchildState': state, 'grandchildAlive': state != 'missing',
            'grandchildGone': state == 'missing'}


def group_empty(pgid):
    # Signal-0 observation through the same entry; never a delivered signal.
    return md.HeldProcessGroup.observe(pgid) == 'empty'


def wait_group_empty(pgid, timeout=5):
    """Observe only. Never signal: after run_fixture the leader is reaped, so a
    bare pid/pgid has no ownership proof any more.  A residual group is a test
    failure, not something this helper is allowed to clean up."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if group_empty(pgid):
            return True
        time.sleep(.05)
    return group_empty(pgid)


def run_fixture(md, output, mode):
    reason = platform_skip(mode)
    if reason:
        return skipped_result(mode, reason)
    folder = output / mode
    folder.mkdir(parents=True, exist_ok=False)
    pid_file = folder / 'grandchild.pid'
    command = [sys.executable, '-I', str(LAUNCHER), '--output', str(folder / 'six-cells'),
               '--mode', mode, '--child-pid-file', str(pid_file)]
    audit = md.run_fixture(folder, ROOT, command=command)
    grandchild = int(pid_file.read_text()) if pid_file.is_file() else None
    # No signal here. The probe already had the only provable window (leader was
    # an unreaped zombie) and reaped it last; anything still alive now cannot be
    # named safely, so it is reported as a failure instead of force-killed.
    group_empty_after = wait_group_empty(audit['pgid'])
    result = {
        'mode': mode,
        'processTreeCleanup': audit['processTreeCleanup'],
        'topLevel': md.cleanup_status(audit),
        'psCensus': audit.get('psCensus'),
        'probe': audit.get('processGroupProbe'),
        'grandchildPid': grandchild,
        **grandchild_view(grandchild),
        'groupEmptyAfterRun': group_empty_after,
        'launcherExitCode': audit.get('exitCode'),
        'launcherStatus': audit.get('status'),
        'leaderReapState': audit.get('leaderReapState'),
        'fixtureReadError': audit.get('fixtureReadError'),
    }
    if mode == 'clean':
        expected = audit['processTreeCleanup'] == md.CLEANUP_VERIFIED
        expected = expected and result['topLevel'] == 'PASS' and group_empty_after
    else:
        expected = audit['processTreeCleanup'] == md.CLEANUP_GROUP_NOT_EMPTY
        expected = expected and result['topLevel'] == md.CLEANUP_UNVERIFIED_STATUS
        expected = expected and group_empty_after
        expected = expected and not result['grandchildAlive']
        expected = expected and result['probe'].get('emptyAfterSignal') is True
        expected = expected and result['probe'].get('signal') == (
            'SIGTERM' if mode == 'leak' else 'SIGKILL')
    result['passed'] = bool(expected)
    return result


def wiring_control(md, output):
    """Drive main()'s real cleanup tail with an unverified group.

    Only the expensive collection steps are replaced (fixture run, calibration
    validation, controls, hashes, frozen check).  The tail that maps
    ``processTreeCleanup`` to ``status`` / ``executionStatus`` / ``freezeStatus``
    and chooses the exit code runs unchanged, so a regression at
    ``session_domain.py`` ``overall = cleanup_status(audit)`` turns this red
    instead of only breaking a helper-level unit test.
    """
    script = output / 'wiring-apparatus'
    (script / 'controls').mkdir(parents=True, exist_ok=False)
    (script / 'controls/session_domain.py').write_text(
        "def run_all(*_args, **_kwargs):\n"
        "    return {'status': 'PASS'}\n")
    folder = output / 'wiring-run'
    audit = {'exitCode': 0, 'launcherAndFixtureReaped': True,
             'processTreeCleanup': md.CLEANUP_GROUP_NOT_EMPTY,
             'pid': 1, 'pgid': 1}
    patched = {
        'SCRIPT': script,
        'E1_SOURCES': (),
        'run_fixture': lambda *_a, **_k: copy.deepcopy(audit),
        'validate_calibration': lambda *_a, **_k: {
            'groups': [], 'expectedCases': [], 'executedCases': [],
            'repeats': md.REPEATS, 'measurements': 0, 'oracle': {},
            'rounds': [], 'controls': [], 'unstableGroups': [],
            'sidecar': {}, 'connections': {}, 'surfaces': {},
            'sourceManifest': '', 'evidenceSha256': {}},
        'hashes': lambda: {'wiring': 'fixed'},
        'frozen_check': lambda: {'exitCode': 0, 'stdout': '', 'stderr': ''},
        '_verify_cli_imports': lambda: None,  # Synthetic controls; provenance tested by test_safety.
        'baseline_dependency_hashes': lambda _baseline: {},
        'install_shutdown': lambda: None,
        'parity_lock': lambda **_kwargs: contextlib.nullcontext(),
    }
    saved = {name: getattr(md, name) for name in patched}
    captured = io.StringIO()
    try:
        for name, value in patched.items():
            setattr(md, name, value)
        with patch.object(sys, 'argv', [
                'session_domain.py', '--suite', 'calibrate',
                '--output', str(folder), '--baseline', str(folder / 'baseline')]), \
                contextlib.redirect_stdout(captured):
            code = md.main()
        report = json.loads((folder / 'report.json').read_text())
    finally:
        for name, value in saved.items():
            setattr(md, name, value)
    expected = {
        'status': md.CLEANUP_UNVERIFIED_STATUS,
        'executionStatus': md.CLEANUP_UNVERIFIED_STATUS,
        'cleanupStatus': md.CLEANUP_GROUP_NOT_EMPTY,
        'freezeStatus': md.CLEANUP_UNVERIFIED_FREEZE,
        'returncode': 2,
    }
    observed = {key: report.get(key) for key in expected if key != 'returncode'}
    observed['returncode'] = code
    result = {'mode': 'wiring', **observed, 'passed': observed == expected,
              'stdout': captured.getvalue()}
    return result


def runtime_negative(md, output):
    """Show the branch is load-bearing without editing source.

    Replacing only the probe *result* with VERIFIED makes the leaky fixture look
    clean (this is what the old run_fixture effectively did after wait).  The
    replacement runs inside run_fixture's zombie window, before child.wait(), so
    the group signal it uses is still owned; no later bare-pid signal is needed.
    The real probe then has to detect the same kind of group.  The grandchild is
    bounded by the fixture itself, so an unkilled leak cannot linger forever and
    is never signalled after the leader was reaped.
    """
    reason = platform_skip('runtime-negative')
    if reason:
        return skipped_result('runtime-negative', reason)
    bypass = output / 'runtime-negative-bypass'
    bypass.mkdir(parents=True, exist_ok=False)
    old_probe = md.probe_process_group
    audit = None
    bypass_accepted = False
    bypass_result = {}
    cleanup_notes = []

    def bypass_probe(child):
        # run_fixture calls this while the leader is an unreaped zombie and
        # before child.wait(): -child.pid is provably this run's group.
        outcome, detail = md.HeldProcessGroup(child).send(signal.SIGKILL)
        if outcome == 'sent':
            cleanup_notes.append('group-signalled-while-leader-unreaped')
        elif outcome == 'missing':
            cleanup_notes.append('group-already-gone')
        else:
            cleanup_notes.append(f'group-signal-{outcome}: {detail}')
        return {'pgid': child.pid if hasattr(child, 'pid') else child,
                'probe': 'runtime-bypass',
                'status': md.CLEANUP_VERIFIED}

    md.probe_process_group = bypass_probe
    try:
        audit = md.run_fixture(bypass, ROOT, command=[
            sys.executable, '-I', str(LAUNCHER), '--output', str(bypass / 'six-cells'),
            '--mode', 'leak', '--child-pid-file', str(bypass / 'grandchild.pid')])
        bypass_accepted = (audit['processTreeCleanup'] == md.CLEANUP_VERIFIED
                           and md.cleanup_status(audit) == 'PASS')
        bypass_result = {'processTreeCleanup': audit['processTreeCleanup'],
                         'topLevel': md.cleanup_status(audit)}
    finally:
        md.probe_process_group = old_probe
        # Do not signal anything now: run_fixture reaped the leader, so identity
        # is gone.  The fixture's own grandchild is short-lived and bounded.
        if audit is not None:
            wait_group_empty(audit['pgid'])
    real = run_fixture(md, output / 'runtime-negative-real', 'leak')
    result = {'mode': 'runtime-negative',
              'bypassAcceptedLeak': bypass_accepted,
              'bypass': bypass_result,
              'bypassCleanup': cleanup_notes,
              'realDetectsLeak': real['processTreeCleanup'] == md.CLEANUP_GROUP_NOT_EMPTY
                                 and real['topLevel'] == md.CLEANUP_UNVERIFIED_STATUS,
              'real': {'processTreeCleanup': real['processTreeCleanup'],
                       'topLevel': real['topLevel']}}
    result['passed'] = bool(bypass_accepted and result['realDetectsLeak'])
    return result


def blocked_control(md):
    """An inaccessible probe is BLOCKED and maps to the non-PASS status.

    Two denial shapes matter: a ``PermissionError`` (macOS sandbox / EPERM) and
    a plain ``OSError`` (the generic fail-closed branch).  Both must end in
    BLOCKED, never PASS and never a crash-only pass.
    """
    cases = []
    error = None
    try:
        for name, killpg_error, kill_error in (
                ('permission-denied', PermissionError('denied'), PermissionError('denied')),
                ('oserror-denied', OSError('denied'), None)):
            probe = _blocked_case(md, killpg_error, kill_error)
            status = probe.get('status')
            top = md.cleanup_status({'processTreeCleanup': status})
            cases.append({'case': name, 'processTreeCleanup': status, 'topLevel': top,
                          'passed': status == md.CLEANUP_BLOCKED
                                    and top == md.CLEANUP_UNVERIFIED_STATUS})
    except Exception as exc:  # a removed BLOCKED branch must be visible, not a crash-only pass
        error = f'{type(exc).__name__}: {exc}'
    passed = error is None and len(cases) == 2 and all(row['passed'] for row in cases)
    return {'mode': 'blocked',
            'processTreeCleanup': cases[0]['processTreeCleanup'] if cases else None,
            'topLevel': cases[0]['topLevel'] if cases else None,
            'error': error, 'cases': cases, 'passed': passed}


def eperm_live_member_control(md, output):
    """EPERM alone must not be read as "only the zombie leader remains".

    A real live grandchild stays in the group; only ``killpg`` is denied, the
    exact shape the review injected in memory.  The independent live-member
    census must find the grandchild, so the probe reports GROUP_NOT_EMPTY and
    the top level is non-PASS, never VERIFIED.  Cleanup is observation-only: the
    fixture's grandchild is short-lived, so nothing is signalled after reaping.
    """
    reason = platform_skip('eperm-live-member')
    if reason:
        return skipped_result('eperm-live-member', reason)
    folder = output / 'eperm-live-member'
    folder.mkdir(parents=True, exist_ok=False)
    marker = folder / 'grandchild.pid'
    code = ("import subprocess,sys;"
            "p=subprocess.Popen(['sleep','12'],stdout=subprocess.DEVNULL,"
            "stderr=subprocess.DEVNULL);"
            "open(sys.argv[1],'w').write(str(p.pid))")
    child = subprocess.Popen([sys.executable, '-I', '-c', code, str(marker)],
                             start_new_session=True)
    grandchild = None
    probe = None
    error = None
    try:
        if md.wait_exit_unreaped(child.pid, 10) == 'EXITED':
            grandchild = int(marker.read_text()) if marker.is_file() else None
            try:
                with patch.object(md.os, 'killpg',
                                  side_effect=PermissionError('sandbox denies group signals')):
                    probe = md.probe_process_group(child)
            except Exception as exc:  # a crash must not count as the expected finding
                error = f'{type(exc).__name__}: {exc}'
        else:
            error = 'leader did not exit as an unreaped zombie'
    finally:
        if child.returncode is None:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
    emptied = wait_group_empty(child.pid, timeout=15)
    status = (probe or {}).get('status')
    result = {
        'mode': 'eperm-live-member',
        'processTreeCleanup': status,
        'topLevel': md.cleanup_status({'processTreeCleanup': status}),
        'signalError': (probe or {}).get('signalError'),
        'grandchildPid': grandchild,
        'groupEmptyAfterWait': emptied,
        'error': error,
    }
    result['passed'] = bool(
        error is None and probe is not None
        and status == md.CLEANUP_GROUP_NOT_EMPTY
        and result['topLevel'] == md.CLEANUP_UNVERIFIED_STATUS
        and emptied)
    return result


def identity_lost_control(md):
    """An already-reaped leader gets NOT_VERIFIED and provably no signal.

    ``killpg`` is replaced by a raising stub: a regression that signals after
    the identity is gone becomes an error instead of a lucky pass.
    """
    child = _zombie_child()
    signals = []
    probe = None
    error = None

    def deny(*args, **_kwargs):
        signals.append(args)
        raise AssertionError('no signal may be sent after the leader was reaped')

    try:
        with patch.object(md, 'wait_exit_unreaped', return_value='REAPED'), \
                patch.object(md.os, 'killpg', side_effect=deny):
            probe = md.probe_process_group(child)
    except Exception as exc:
        error = f'{type(exc).__name__}: {exc}'
    finally:
        if child.returncode is None:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
    status = (probe or {}).get('status')
    result = {'mode': 'identity-lost', 'processTreeCleanup': status,
              'topLevel': md.cleanup_status({'processTreeCleanup': status}),
              'signalCalls': len(signals), 'error': error}
    result['passed'] = bool(error is None and probe is not None
                            and status == md.CLEANUP_NOT_VERIFIED
                            and not signals)
    return result


def census_unavailable_control(md):
    """An unavailable census must be BLOCKED, never read as empty.

    The client process is a real unreaped zombie with an empty group, so a
    missing census is the only reason the probe cannot distinguish "only the
    zombie" from a live member it may not signal.
    """
    child = _zombie_child()
    probe = None
    error = None
    try:
        with patch.object(md, 'live_group_members', return_value=None):
            probe = md.probe_process_group(child)
    except Exception as exc:  # a crash must not count as the expected finding
        error = f'{type(exc).__name__}: {exc}'
    finally:
        if child.returncode is None:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
    status = (probe or {}).get('status')
    result = {'mode': 'census-unavailable', 'processTreeCleanup': status,
              'topLevel': md.cleanup_status({'processTreeCleanup': status}),
              'error': error}
    result['passed'] = bool(error is None and probe is not None
                            and status == md.CLEANUP_BLOCKED
                            and result['topLevel'] == md.CLEANUP_UNVERIFIED_STATUS)
    return result


def census_snapshot_race_control(md):
    """A member appears between two complete group snapshots.

    ``PROC_PGRP_ONLY`` enumerates the group directly, so there is no per-pid
    getpgid snapshot window.  The remaining race is a fork between two
    snapshots: the first sees only the zombie leader, the second sees the
    descendant.  Accepting either snapshot alone would report empty; the
    stable pair must report GROUP_NOT_EMPTY and never PASS.
    """
    child = _zombie_child()
    leader = child.pid
    race = {'status': None}
    delivered = []
    error = None
    snapshot_pids = iter([[leader], [leader, 424244], [leader, 424244],
                          [leader, 424244], [leader], [leader]])

    def next_snapshot(*_args):
        return set(next(snapshot_pids, [leader]))

    def record_group(*args, **_kwargs):
        if len(args) > 1 and args[1] != 0:
            delivered.append(args)
        raise PermissionError('sandbox denies group signals')

    try:
        with patch.object(md, 'sys', SimpleNamespace(platform='darwin')), \
                patch.object(md, '_group_snapshot', side_effect=next_snapshot), \
                patch.object(md.os, 'killpg', side_effect=record_group):
            race = md.probe_process_group(child)
    except Exception as exc:  # a crash must not count as the expected finding
        error = f'{type(exc).__name__}: {exc}'
    finally:
        if child.returncode is None:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
    status = (race or {}).get('status')
    result = {'mode': 'census-snapshot', 'processTreeCleanup': status,
              'topLevel': md.cleanup_status({'processTreeCleanup': status}),
              'deliveredSignalCalls': len(delivered), 'error': error}
    result['passed'] = bool(
        error is None and race is not None
        and status == md.CLEANUP_GROUP_NOT_EMPTY
        and result['topLevel'] == md.CLEANUP_UNVERIFIED_STATUS
        and delivered)
    return result


def census_descendant_control(md):
    """A stable pair that includes the descendant must report it."""
    snapshots = iter([[900], [900, 424244], [900, 424244], [900, 424244], [900, 424244]])

    def next_members(*_args):
        return set(next(snapshots, [900, 424244]))

    try:
        with patch.object(md, '_group_snapshot', side_effect=next_members):
            members = md.live_group_members(900)
        error = None
    except Exception as exc:
        members = None
        error = f'{type(exc).__name__}: {exc}'
    result = {'mode': 'census-descendant',
              'members': None if members is None else sorted(members), 'error': error}
    result['passed'] = bool(error is None and members == {900, 424244})
    return result


def census_unstable_control(md):
    """Two complete snapshots that disagree cannot prove a stable membership."""
    alternating = iter([[900, 424244], [900, 424245]] * 4)

    def next_unstable(*_args):
        return set(next(alternating, [900, 424245]))

    try:
        with patch.object(md, '_group_snapshot', side_effect=next_unstable):
            members = md.live_group_members(900)
        error = None
    except Exception as exc:
        members = None
        error = f'{type(exc).__name__}: {exc}'
    result = {'mode': 'census-unstable',
              'members': None if members is None else sorted(members), 'error': error}
    result['passed'] = bool(error is None and members is None)
    return result


def census_truncated_control(md):
    """A full libproc buffer is truncation; never a complete group."""
    class FullBufferLibrary:
        def __init__(self):
            self.calls = []

        def proc_listpids(self, kind, typeinfo, buffer, size):
            self.calls.append((kind, typeinfo, size))
            if buffer is None:
                return 2832
            buffer[0] = 424242
            return size

    library = FullBufferLibrary()
    try:
        returned = md._darwin_group_pids(900, library=library)
        error = None
    except Exception as exc:
        returned = None
        error = f'{type(exc).__name__}: {exc}'
    result = {'mode': 'census-truncated',
              'returnedPids': None if returned is None else len(returned),
              'calls': library.calls, 'error': error}
    result['passed'] = bool(
        error is None and returned is None and library.calls[:1] == [(2, 900, 0)])
    return result


def census_enumeration_failure_control(md):
    """An unavailable enumeration is not an empty group."""
    child = _zombie_child()
    try:
        with patch.object(md, 'sys', SimpleNamespace(platform='darwin')), \
                patch.object(md, '_group_snapshot', return_value=None), \
                patch.object(md.os, 'killpg', side_effect=PermissionError('denied')):
            probe = md.probe_process_group(child)
        error = None
    except Exception as exc:
        probe = {'status': None}
        error = f'{type(exc).__name__}: {exc}'
    finally:
        if child.returncode is None:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
    status = probe.get('status')
    result = {'mode': 'census-enumeration', 'processTreeCleanup': status,
              'topLevel': md.cleanup_status({'processTreeCleanup': status}), 'error': error}
    result['passed'] = bool(error is None and status == md.CLEANUP_BLOCKED
                            and result['topLevel'] == md.CLEANUP_UNVERIFIED_STATUS)
    return result


def signal_entry_control(md):
    """The single entry refuses a reaped leader and still sends while held."""
    child = subprocess.Popen([sys.executable, '-I', '-c', 'pass'])
    child.wait(timeout=10)
    held = md.HeldProcessGroup(child)
    calls = []

    def deny(*args, **_kwargs):
        calls.append(args)
        raise PermissionError('denied')

    try:
        with patch.object(md.os, 'killpg', side_effect=deny), \
                patch.object(md.os, 'kill', side_effect=deny):
            group_outcome = held.send(signal.SIGTERM)
            leader_outcome = held.kill_leader(signal.SIGKILL)
        error = None
    except Exception as exc:
        group_outcome = leader_outcome = (None, None)
        error = f'{type(exc).__name__}: {exc}'

    held_child = subprocess.Popen(['sleep', '30'], start_new_session=True)
    try:
        held_outcome = md.HeldProcessGroup(held_child).send(signal.SIGTERM)
        held_child.wait(timeout=10)
    except Exception as exc:
        held_outcome = (None, None)
        error = error or f'{type(exc).__name__}: {exc}'
    finally:
        if held_child.returncode is None:
            try:
                held_child.kill()
                held_child.wait(timeout=5)
            except (OSError, subprocess.TimeoutExpired):
                pass
    result = {'mode': 'signal-entry',
              'groupOutcome': group_outcome[0], 'leaderOutcome': leader_outcome[0],
              'killCalls': len(calls), 'heldOutcome': held_outcome[0],
              'heldReaped': held_child.returncode is not None, 'error': error}
    result['passed'] = bool(
        error is None and group_outcome[0] == 'refused'
        and leader_outcome[0] == 'refused' and not calls
        and held_outcome[0] == 'sent' and held_child.returncode is not None)
    return result


def reap_state_control(md, output):
    """A leader reported REAPED must not get a bare ``child.kill()``.

    ``run_fixture`` is forced to see ``wait_exit_unreaped`` return REAPED while
    the real child is still ours; the fixed code skips the kill and only waits.
    A regression that signals the bare pid turns this red via the raising stub.
    """
    folder = output / 'reap-state'
    folder.mkdir(parents=True, exist_ok=False)
    calls = []

    def deny(*args, **_kwargs):
        calls.append(args)
        raise AssertionError('no signal after the leader was reported REAPED')

    audit = None
    error = None
    try:
        with patch.object(md, 'wait_exit_unreaped', return_value='REAPED'), \
                patch.object(md.os, 'kill', side_effect=deny), \
                patch.object(md.os, 'killpg', side_effect=deny):
            audit = md.run_fixture(folder, ROOT,
                                   command=[sys.executable, '-I', '-c', 'pass'])
    except Exception as exc:  # a crash must not count as the expected finding
        error = f'{type(exc).__name__}: {exc}'
    process = audit or {}
    result = {'mode': 'reap-state',
              'processTreeCleanup': process.get('processTreeCleanup'),
              'leaderReapState': process.get('leaderReapState'),
              'killCalls': len(calls), 'error': error}
    result['passed'] = bool(
        error is None and audit is not None
        and process.get('processTreeCleanup') == md.CLEANUP_NOT_VERIFIED
        and process.get('leaderReapState') == 'ALREADY_REAPED'
        and not calls)
    return result


def pid_blocked_control(md):
    """kill(pid, 0)=EPERM is 'blocked', not 'gone'.

    Denying the real ``os.kill`` exercises ``observe_pid`` itself.  A call site
    that collapses ``blocked`` to a two-state bool (or a source change that
    returns ``missing`` on OSError) makes ``grandchildGone`` true and this
    leftover assertion red.
    """
    with patch.object(md.os, 'kill', side_effect=PermissionError('denied')):
        view = grandchild_view(os.getpid())
    result = {'mode': 'pid-blocked', **view}
    result['passed'] = bool(view['grandchildState'] == 'blocked'
                            and view['grandchildAlive'] is True
                            and view['grandchildGone'] is False)
    return result


def linux_filtered_proc_control(md):
    """A filtered /proc on Linux is BLOCKED, never a complete group.

    ``/proc`` can hide a live same-group descendant (hidepid, EACCES/EPERM, a
    changing read) without an error.  The visible set below contains only the
    leader; reading it as complete would return empty/VERIFIED/PASS.  The
    non-darwin gate must return BLOCKED before the census is read or a signal
    is sent.
    """
    child = _zombie_child()
    leader = child.pid
    visible = iter([[leader]] * 8)
    calls = []

    def visible_only(*args):
        calls.append(args)
        return set(next(visible, [leader]))

    probe = None
    error = None
    try:
        with patch.object(md, 'sys', SimpleNamespace(platform='linux')), \
                patch.object(md, '_group_snapshot', side_effect=visible_only), \
                patch.object(md.os, 'killpg', side_effect=PermissionError('EPERM')):
            probe = md.probe_process_group(child)
    except Exception as exc:  # a crash must not count as the expected finding
        error = f'{type(exc).__name__}: {exc}'
    finally:
        if child.returncode is None:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
    status = (probe or {}).get('status')
    result = {'mode': 'linux-filtered-proc', 'processTreeCleanup': status,
              'topLevel': md.cleanup_status({'processTreeCleanup': status}),
              'censusCalls': len(calls), 'error': error}
    result['passed'] = bool(error is None and probe is not None
                            and status == md.CLEANUP_BLOCKED
                            and result['topLevel'] == md.CLEANUP_UNVERIFIED_STATUS
                            and not calls)
    return result


def _blocked_case(md, killpg_error, kill_error=None):
    child = _zombie_child()
    patches = [patch.object(md.os, 'killpg', side_effect=killpg_error)]
    if kill_error is not None:
        patches.append(patch.object(md.os, 'kill', side_effect=kill_error))
    try:
        with contextlib.ExitStack() as stack:
            for item in patches:
                stack.enter_context(item)
            return md.probe_process_group(child)
    finally:
        if child.returncode is None:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass


def _zombie_child():
    """Spawn a new-session child and leave it an unreaped zombie."""
    child = subprocess.Popen([sys.executable, '-I', '-c', 'pass'], start_new_session=True)
    if md_wait(child.pid, 10) != 'EXITED':
        child.wait(timeout=5)
        raise RuntimeError('fixture leader did not exit')
    return child


def md_wait(pid, timeout):
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


def status_controls(md):
    cases = {
        'VERIFIED': 'PASS',
        'GROUP_NOT_EMPTY': md.CLEANUP_UNVERIFIED_STATUS,
        'BLOCKED': md.CLEANUP_UNVERIFIED_STATUS,
        'NOT_VERIFIED': md.CLEANUP_UNVERIFIED_STATUS,
    }
    results = []
    for cleanup, expected in cases.items():
        observed = md.cleanup_status({'processTreeCleanup': cleanup})
        results.append({'processTreeCleanup': cleanup, 'expected': expected,
                        'observed': observed, 'passed': observed == expected})
    return results


# Static enumeration of every top-level status/exit-code computation in the
# E1 apparatus.  A registered point must call common.aggregate_status(); a new
# point that does not is unregistered and fails closed.  Functions that look
# like status/exit code but are not a batch summary carry an explicit reason;
# the enumeration test fails on any new key, so a new summary cannot be added
# without updating this table.  #892: the enumeration also covers summary
# functions that directly return a status dict (``return-report-status``), and
# each registered point's every return path must route its status/exit
# computation through the shared function (``unshared_status_slots``).
AGGREGATION_POINTS = {
    ('scripts/gateway-parity/_guard.py', '_failure'): 'guard-failure',
    ('scripts/gateway-parity/controls/session_domain.py', 'cleanup_controls'): 'controls-cleanup',
    ('scripts/gateway-parity/controls/session_domain.py', 'run_all'): 'controls-run-all',
    ('scripts/gateway-parity/controls/session_domain.py', 'main'): 'controls-main',
    ('scripts/gateway-parity/session_domain.py', 'main'): 'calibrate-main',
    ('scripts/gateway-parity/session_domain.py', '_deadline_report'): 'deadline-report',
    ('scripts/gateway-parity/session_domain.py', '_retirement_report'): 'retirement-report',
    ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py',
     'summarize_results'): 'fixture-summarize',
    ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py',
     'main'): 'fixture-main',
    ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/negative_controls.py',
     'main'): 'negative-main',
}
NON_AGGREGATION_POINTS = {
    ('scripts/gateway-parity/test_safety.py', 'test_deadline_not_run_has_dedicated_exit_code'):
        'test-only shared exit mapping and main-body contract, not a batch',
    ('scripts/gateway-parity/common.py', 'aggregate_status'):
        'the single shared implementation (definition, not a call site)',
    ('scripts/gateway-parity/check_frozen.py', 'main'):
        'single frozen-hash gate; own PASS/FAIL and exit contract, no child batch',
    ('scripts/gateway-parity/check_ruler.py', 'main'):
        'A2 ruler gate; PENDING_REVIEW/exit 2 contract is outside the E1 batch order',
    ('scripts/gateway-parity/compare.py', 'main'):
        'single comparison result; no skip/tri-state children',
    ('scripts/gateway-parity/compare.py', 'compare'):
        'builds the single comparison result main returns; scenarios are not tri-state children',
    ('scripts/gateway-parity/controls/prerequisites.py', 'run_prerequisites'):
        'boolean prerequisite suite; per-comparison status labels are not a top-level batch',
    ('scripts/gateway-parity/session_domain.py', 'probe_process_group'):
        'one process-group domain status (VERIFIED/BLOCKED), not a batch summary',
    ('scripts/gateway-parity/session_domain.py', 'score_native_cases'):
        'per-case scoring rows for one capture, not a batch summary',
    ('scripts/gateway-parity/session_domain.py', 'validate_calibration'):
        'one calibration report face; the batch status is aggregated by main',
    ('scripts/gateway-parity/session_domain.py', 'deadline_report'):
        'public wrapper returning _deadline_report(...); the single point _deadline_report audits the slots',
    ('scripts/gateway-parity/session_domain.py', '_retirement_controls'):
        'per-round control verdict recorder; the batch status is aggregated by _retirement_report',
    ('scripts/gateway-parity/session_domain.py', 'record'):
        'nested in _retirement_controls: records one control verdict, not a batch',
    ('scripts/gateway-parity/session_domain.py', 'unavailable'):
        'nested in _retirement_controls: records one unavailable control, not a batch',
    ('scripts/gateway-parity/session_domain.py', 'cleanup_status'):
        'domain mapping VERIFIED->PASS else PASS_WITH_CLEANUP_UNVERIFIED; one cleanup status, not a child summary',
    ('scripts/gateway-parity/session_domain.py', 'run_fixture'):
        'launcher audit status DONE/FAIL for one run, not a batch',
    ('scripts/gateway-parity/session_domain.py', 'score_retirement_round'):
        'per-round rule verdict from a problem list, not a batch summary',
    ('scripts/gateway-parity/session_domain.py', 'process_tree'):
        'one ps census face; its BLOCKED is a single read, not a batch summary',
    ('scripts/gateway-parity/session_domain.py', 'validate_expectations'):
        'A19-HOST registration verdict face; consumed by _deadline_report shared calls, not a batch',
    ('scripts/gateway-parity/session_domain.py', '_retirement_census_verdict'):
        'per-face HOST_CENSUS rule verdict; child of _retirement_report shared calls',
    ('scripts/gateway-parity/session_domain.py', '_retirement_report_census'):
        'error-shape HOST_CENSUS face for an unreadable round record, not a batch',
    ('scripts/gateway-parity/session_domain.py', '_retirement_network_verdict'):
        'per-face NETWORK_FALLBACK rule verdict; child of _retirement_report shared calls',
    ('scripts/gateway-parity/session_domain.py', '_retirement_listener_verdict'):
        'per-face LISTENER_EXPOSURE rule verdict; child of _retirement_report shared calls',
    ('scripts/gateway-parity/session_domain.py', '_retirement_entry_inventory'):
        'per-round entry inventory face; own NO_HIT/GATED/HIT vocabulary, not batch statuses',
    ('scripts/gateway-parity/session_domain.py', '_retirement_entry_inventory_aggregate'):
        'merges per-round inventories into one face with own precedence words; child of the shared aggregation',
    ('scripts/gateway-parity/session_domain.py', '_retirement_js_host_census'):
        'static census note (CENSUS_RECORDED); no children, not a batch summary',
    ('scripts/gateway-parity/session_domain.py', '_retirement_budget_blocked'):
        'error-shape face for an exceeded round budget, not a batch summary',
    ('scripts/gateway-parity/session_domain.py', '_retirement_round'):
        'one calibration round; its RUNNING/DONE row is the round state, not a batch verdict',
    ('scripts/gateway-parity/session_domain.py', 'retirement_report'):
        'error-shape face wrapping _retirement_budget_blocked, not a batch summary',
    ('scripts/gateway-parity/test_safety.py', 'test_control_attribution_requires_the_injected_object'):
        'test-only mutated fixture copy, not a batch summary',
    ('scripts/gateway-parity/test_safety.py', 'drifted_preexisting'):
        'test-only drifted fixture copy, not a batch summary',
    ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py',
     'batch_aggregation_control'):
        'test-only; calls the shared function to assert the order',
    ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py',
     'aggregation_audit'):
        'the audit itself; test-only',
    ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py',
     'runtime_negative'):
        'test-only control; the bypass stub below carries the probe verdict, not a batch',
    ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py',
     'bypass_probe'):
        'test-only runtime replacement stub returning one probe verdict, not a batch',
    ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py',
     'replay'):
        'test-only batch replay returning observed fields, not a batch summary',
    # #892 r3: assigning a clean status to an arbitrary name now counts as a
    # summary marker; these five build per-case / synthetic verdicts that never
    # claim to summarise a child batch.
    ('scripts/gateway-parity/run_round.py', 'run_round'):
        'runtime driver tracking its own RUNNING/DONE/ERROR round lifecycle, not a batch verdict',
    ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py',
     'census_enumeration_failure_control'):
        'test-only synthetic census payload ({status: None}); the control asserts the fixture fails closed',
    # #892 r16: the raise family now enumerates every ``raise SystemExit(...)``
    # like the exit-call family, so the three exit paths below -- each of which
    # leaves the process without aggregating a batch -- need an explicit reason.
    ('scripts/gateway-parity/_guard.py', 'run_entry'):
        'guard exit path: relays the guarded entry point own exit code (2 for an apparatus error, the child code otherwise); no child batch is aggregated here',
    ('scripts/gateway-parity/native_evidence.py', 'main'):
        'library entry guard: refuses to run native_evidence.py as a script; no batch',
    ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/negative_controls.py',
     'terminate_cleanly'):
        'signal-driven abort path: raises SystemExit to unwind after restoring the mutated tree, not a batch status',
    ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py',
     'census_snapshot_race_control'):
        'test-only synthetic census snapshot used to force the race; not a batch summary',
    ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py',
     'negative_restored_replays'):
        'test-only replay rows carrying deliberately restored statuses, not a batch summary',
    ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py',
     'wiring_control'):
        'test-only wiring probe whose dict is the injected stub payload, not a batch summary',
}


STATUS_WORDS = frozenset(('PASS', 'PASS_WITH_SKIPS', 'NOT_RUN', 'BLOCKED', 'FAIL',
                          'PASS_WITH_CLEANUP_UNVERIFIED'))


def _status_words():
    return STATUS_WORDS


def _folded_string(node):
    """The string *node* builds out of constants, or None (#892 r20).

    ``'PA' + 'SS'`` and ``f'{"PA"}{"SS"}'`` are the same literal as ``'PASS'``
    to the interpreter but were not a ``Constant`` to the checker, so every
    rule that keys on a status word walked straight past them: a write of
    ``report['status'] = 'PA' + 'SS'`` was not an inline clean status and
    ``sys.exit('PA' + 'SS')`` was not a status literal (GLM boundary r19 P1-2).
    Only fully constant expressions fold; anything with a runtime operand
    returns None and keeps its own verdict.
    """
    if isinstance(node, ast.Constant):
        return node.value if isinstance(node.value, str) else None
    if isinstance(node, ast.JoinedStr):
        parts = []
        for part in node.values:
            if isinstance(part, ast.Constant) and isinstance(part.value, str):
                parts.append(part.value)
            elif (isinstance(part, ast.FormattedValue) and part.format_spec is None
                  and isinstance(part.value, ast.Constant)
                  and isinstance(part.value.value, (str, int))):
                parts.append(str(part.value.value))
            else:
                return None
        return ''.join(parts)
    if isinstance(node, ast.BinOp):
        left = _folded_string(node.left)
        right = _folded_string(node.right)
        if isinstance(node.op, ast.Add) and left is not None and right is not None:
            return left + right
        if isinstance(node.op, ast.Mod) and left is not None and right is not None:
            try:
                return left % right
            except (TypeError, ValueError):
                return None
        if (isinstance(node.op, ast.Mult) and left is not None
                and isinstance(node.right, ast.Constant)
                and isinstance(node.right.value, int)):
            return left * node.right.value
    return None


def _status_literals(node):
    words = _status_words()
    folded = _folded_string(node)
    if folded is not None and folded in words:
        return True
    return any(isinstance(n, ast.Constant) and n.value in words for n in ast.walk(node))


def _has_passed_aggregate(node):
    return any(isinstance(n, ast.Call) and isinstance(n.func, ast.Name)
               and n.func.id in ('all', 'any') and 'passed' in ast.unparse(n)
               for n in ast.walk(node))


def _is_status_target(target):
    if isinstance(target, (ast.Tuple, ast.List)):
        # ``report['status'], code = 'PASS', 0`` writes the same slot as the
        # plain subscript form one element at a time, and the slot layer has
        # expanded tuple targets since fix-r9; the enumeration did not, so the
        # writing function was never enumerated and never audited (MiniMax
        # simplify r14 P2-2).  Element order does not matter here: the payload
        # is judged by presence of status evidence, not by position.
        return any(_is_status_target(element) for element in target.elts)
    if isinstance(target, ast.Name):
        # Only the ``status`` variable: ``verdict = ...`` is an ordinary local
        # name in several per-case helpers, not a batch status key.
        return target.id == 'status'
    if isinstance(target, ast.Subscript):
        return _status_key(target.slice)
    if isinstance(target, ast.Attribute):
        # The enumeration must see the attribute spelling for the same reason
        # the slot layer does (MiniMax sibling r17 P1-1): otherwise the only
        # evidence of a status write is invisible and the function is never
        # even enumerated.
        return target.attr in _STATUS_KEYS
    return False


def _is_report_status(target, value):
    """A name assigned a dict that carries a status key (#892 r3/r6).

    The old three-name whitelist (``report``/``summary``/``result``) let
    ``payload = {'status': 'PASS'}`` stay invisible, so a summary built under
    any other name never became an aggregation point at all (MiniMax sibling
    P1-1 / DeepSeek sibling P2-2).  The return side already accepts any name;
    the assignment side now does too.  The key test is ``_dict_has_status``,
    so a ``verdict`` spelling or a ``{**literal}`` expansion is a marker too:
    the harder-coded ``'status'`` check left ``payload = {'verdict': 'PASS'}``
    out of the enumeration entirely (GLM sibling P1-1).
    """
    return (isinstance(target, ast.Name) and target.id != 'status'
            and isinstance(value, ast.Dict) and _dict_has_status(value))


def _returned_summary_dicts(value):
    """Directly returned summary dicts, including tuple elements (#892).

    A constant ``status`` key is enough; the value may be a computed relay, so
    unlike the assignment shapes this does not require status-word literals.
    A conditional or boolean arm hands back the same dict a plain ``return``
    would, so every arm is expanded: ``return {'status': 'PASS'} if cond else
    report`` used to read as "no dict returned", and the whole return path went
    unaudited (GLM r6 boundary P0-1).
    """
    if isinstance(value, ast.IfExp):
        arms = (value.body, value.orelse)
    elif isinstance(value, ast.BoolOp):
        arms = value.values
    elif isinstance(value, ast.Dict):
        return [value] if _dict_has_status(value) else []
    elif isinstance(value, ast.Tuple):
        arms = value.elts
    else:
        return []
    dicts = [item for arm in arms for item in _returned_summary_dicts(arm)]
    return [item for item in dicts if _dict_has_status(item)]


def _dict_pairs(summary):
    """Every ``(key, value)`` pair, expanding ``{**literal}`` in place (#892 r3).

    A ``**{'status': 'PASS'}`` entry has ``key=None``; the previous code only
    looked at constant ``'status'`` keys, so the whole expansion was invisible
    (DeepSeek boundary P2-4).
    """
    for key, value in zip(summary.keys, summary.values):
        if key is None and isinstance(value, ast.Dict):
            yield from _dict_pairs(value)
        else:
            yield key, value


# #892 r3: the keys that claim a batch verdict.  ``verdict`` is the spelling a
# summary could use instead of ``status``; a sub-status such as
# ``structureStatus`` is a per-cell value, not a batch claim.
_STATUS_KEYS = frozenset({'status', 'verdict'})


def _status_key(node):
    """A status/verdict key whatever constant shape built it (#892 r21).

    ``report['sta' + 'tus']`` writes the very slot ``report['status']`` does,
    but the key is not a ``Constant`` node, and the r20 constant folding
    reached the value side only: every key-side judgment still walked past
    the folded spelling, so a summary keyed ``{'sta' + 'tus': 'PASS'}`` was
    not a summary at all and the whole function escaped enumeration
    (GLM sibling r20 P1-1).  All key judgments go through this one helper
    so the two halves cannot drift apart again.
    """
    return _folded_string(node) in _STATUS_KEYS


def _dict_has_status(summary):
    return any(_status_key(key) for key, _value in _dict_pairs(summary))


def _status_slot_values(summary):
    """Every value bound to a constant ``'status'`` key (#892 r3).

    All of them, not just the first: Python keeps the *last* duplicate key at
    runtime, so ``{'status': 'BLOCKED', 'status': 'PASS'}`` would otherwise be
    read as fail-closed while the process reports PASS (DeepSeek boundary P2-2).
    """
    return [value for key, value in _dict_pairs(summary)
            if _status_key(key)]


def _loads_shared_module(value, name, loaders=frozenset(), rebound=frozenset()):
    """True when *value* is the apparatus loader call for module *name*.

    The audited tree binds the shared module as
    ``common = _load('common', _apparatus / 'common.py')`` (or ``load(...)`` in
    ``_guard.py``), so the call names the module it loads.  Any other binding --
    a namespace stub, a local dict, a helper that returns something else -- does
    not name a module and therefore does not earn the trust.  #892 r23: the
    callee itself must resolve to the guard loader; a ``def _load(name, path)``
    inserted after ``_load = _guard.load`` spells the call the same way while
    returning a stub, so a first-argument constant alone no longer suffices
    (GLM boundary r22 P1-2).  #892 r24: a loader name rebound inside the calling
    function (``_load = <stub>`` in the body, or a parameter of that name)
    shadows the module alias exactly like a same-named ``def`` does at module
    scope, so *rebound* withdraws the trust in that function.
    """
    return (isinstance(value, ast.Call) and bool(value.args)
            and isinstance(value.args[0], ast.Constant)
            and value.args[0].value == name
            and isinstance(value.func, ast.Name)
            and value.func.id in loaders
            and value.func.id not in rebound)


# #892 r23: the file that defines the apparatus loader itself.  ``load`` there
# is a plain ``def``, not a ``<module>.load`` attribute, so ``_guard.py``'s own
# ``run_entry`` call keeps the loader role while exactly one definition exists
# and nothing else binds the name.
GUARD_REL = 'scripts/gateway-parity/_guard.py'


def _loader_names(tree, rel=None):
    """Module-scope names that still denote the guard loader (#892 r23/r24).

    ``_load = _guard.load`` earned every ``_load('common', ...)`` call the
    loader's trust without asking who ``_load`` is.  A name keeps the role
    only while *every* module-scope binding of it is the ``<name>.load``
    attribute **and** that base name itself resolves on the module binding
    graph to a real module (``_proven_module_names``); ``_guard = <stub>``
    then ``_load = _guard.load`` would otherwise spell a loader while calling
    a stub (#892 r24, same class as the ``_proven_source`` P0).  ``_guard.py``
    itself is the one sanctioned ``def load``.
    """
    if not isinstance(tree, ast.Module):
        return frozenset()
    bindings = _module_scope_binding_map(tree)
    modules = _proven_module_names(tree)
    loaders = {name for name, values in bindings.items()
               if values and all(
                   isinstance(value, ast.Attribute) and value.attr == 'load'
                   and isinstance(value.value, ast.Name)
                   and value.value.id in modules
                   for value in values)}
    if rel == GUARD_REL and bindings.get('load') == [None]:
        # #892 r26: the sanctioned ``def load`` is only itself while nothing
        # replaces the function object at definition time; ``@_swap`` keeps the
        # single ``[None]`` binding but hands the call a stub (GLM sibling r25
        # #5 / boundary r25 P1-7).  Same rule ``_proven_source_names`` already
        # applies to the proven sources.
        definitions = [node for node in _module_level_defs(tree)
                       if node.name == 'load']
        # #892 r27: a ``class load`` is not the loader, and a dynamic write
        # replaces the function object the single-def rule just accepted (GLM
        # simplify r26 P1-4 / DeepSeek concurrency r26 P1-8 / boundary r26 P0-2).
        dynamic = _dynamic_write_names(tree)
        if (len(definitions) == 1 and not definitions[0].decorator_list
                and isinstance(definitions[0], (ast.FunctionDef,
                                                ast.AsyncFunctionDef))
                and '*' not in dynamic and 'load' not in dynamic):
            loaders.add('load')
    return frozenset(loaders)


def _module_level_defs(tree):
    """Every ``def``/``class`` that binds a name at module scope (#892 r25).

    ``_own_nodes`` deliberately prunes definition subtrees, so a
    ``def`` inside ``if``/``try``/``for`` at module level was invisible to the
    binding map: the name kept its single top-level ``[None]`` binding and the
    proven-source trust survived while the runtime binding was the conditional
    one (GLM boundary r24 P0-1).  This walk descends through every control
    block and stops at definition bodies, so each module-scope definition is
    one binding whatever block it sits in.
    """
    found = []
    stack = [tree]
    while stack:
        node = stack.pop()
        for child in ast.iter_child_nodes(node):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef,
                                  ast.ClassDef)):
                found.append(child)
                continue
            if isinstance(child, ast.Lambda):
                continue
            stack.append(child)
    return found


def _nested_definition_names(function):
    """Names a nested ``def``/``class`` binds inside *function* (#892 r25).

    A nested definition is a local binding of the enclosing function exactly
    like an assignment, so it shadows a module-level proven source or loader
    for the whole body (GLM concurrency r24 P1-2: ``def main(): global md; def
    load_session_domain(): ...; md = load_session_domain()``).
    """
    return {node.name for node in _module_level_defs(function)}


def _scoped_nodes(scope):
    """Descendants of *scope* outside nested definition/lambda bodies (#892 r26).

    ``ast.walk`` dives into every nested ``def``/``lambda``, whose bindings
    belong to *their* scope, not this one: a walrus inside an inner function
    would withdraw the outer function's trust for nothing (GLM simplify r25
    P3-9).  Statements and expressions of the scope itself -- including the
    ``if``/``while`` tests and comprehension elements where a walrus lives --
    are still visited.
    """
    stack = [scope]
    while stack:
        current = stack.pop()
        yield current
        for child in ast.iter_child_nodes(current):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef,
                                  ast.ClassDef, ast.Lambda)):
                # #892 r27: the *header* of a definition -- decorators, default
                # values, bases, annotations -- is evaluated in the enclosing
                # scope, so a walrus there rebinds the enclosing name even
                # though the body belongs to the nested scope (GLM concurrency
                # r26 P1-2 / DeepSeek boundary r26 P1-6).
                stack.extend(_definition_header_nodes(child))
                continue
            stack.append(child)


def _definition_header_nodes(node):
    """Expressions a definition evaluates in its *enclosing* scope (#892 r27)."""
    nodes = list(getattr(node, 'decorator_list', ()))
    if isinstance(node, ast.ClassDef):
        nodes.extend(node.bases)
        nodes.extend(keyword.value for keyword in node.keywords)
        return nodes
    arguments = node.args
    nodes.extend(arguments.defaults)
    nodes.extend(default for default in arguments.kw_defaults
                 if default is not None)
    for parameter in (*getattr(arguments, 'posonlyargs', ()),
                      *arguments.args, *arguments.kwonlyargs,
                      arguments.vararg, arguments.kwarg):
        if parameter is not None and parameter.annotation is not None:
            nodes.append(parameter.annotation)
    if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
        if node.returns is not None:
            nodes.append(node.returns)
    return nodes


def _walrus_names(scope):
    """``(name := value)`` targets inside *scope* (#892 r25).

    A walrus binds in the enclosing function *or module* scope, not the
    comprehension, so ``if (load_session_domain := _stub): ...`` shadows the
    definition for the whole scope while no ``Assign`` statement exists to
    record it (GLM concurrency r24 P1-2).  #892 r26: module scope has no
    enclosing function, so the module-level spelling was invisible to every
    consumer (GLM concurrency r25 P0-1 / boundary r25 P0-2).
    """
    return {node.target.id for node in _scoped_nodes(scope)
            if isinstance(node, ast.NamedExpr)
            and isinstance(node.target, ast.Name)}


def _match_capture_names(scope):
    """Names a ``match`` statement captures inside *scope* (#892 r26).

    ``case [load_session_domain]:`` and ``case {'k': load_session_domain}``
    bind like an assignment; ``_local_bound_names`` has no ``Match`` branch, so
    the capture never reached the binding graph (GLM simplify r25 P1-4).
    """
    names = set()
    for node in _scoped_nodes(scope):
        if isinstance(node, (ast.MatchAs, ast.MatchStar)) and node.name:
            names.add(node.name)
        elif isinstance(node, ast.MatchMapping) and node.rest:
            names.add(node.rest)
    return names


def _default_bound_parameters(function):
    """Parameter names whose default value is supplied by the function itself.

    ``def summarize_results(results, md=<stub>):`` binds a stub the caller
    never passed; the parameter is not the caller's contract for that name
    (GLM boundary r25 P1-4).  Only parameters *with* a default are reported --
    a caller-supplied argument stays the contract.
    """
    arguments = function.args
    positional = list(getattr(arguments, 'posonlyargs', [])) + list(arguments.args)
    names = set()
    for parameter in positional[len(positional) - len(arguments.defaults):]:
        names.add(parameter.arg)
    for parameter, default in zip(arguments.kwonlyargs, arguments.kw_defaults):
        if default is not None:
            names.add(parameter.arg)
    return names


def _function_local_names(function):
    """Every name bound inside *function* (#892 r26).

    Parameters, nested definitions, walrus targets, ``match`` captures and
    every statement binding ``_own_nodes`` reports.  Four copies of this union
    used to be spelled out by hand; the r24/r25 rounds each fixed one binding
    form in some of them, so they are one helper now (GLM simplify r25 P2-6).
    """
    names = set(_parameter_names(function))
    names.update(_nested_definition_names(function))
    names.update(_walrus_names(function))
    names.update(_match_capture_names(function))
    for node in _own_nodes(function):
        bound, _value = _local_bound_names(node)
        names.update(bound)
    return names


def _file_local_names(tree):
    """Every name bound as a local inside any function of *tree* (#892 r25).

    Parameters, assignments, ``for``/``with``/``except`` targets, imports,
    nested definitions, walrus targets and ``match`` captures all count: each
    one shadows a module binding for the whole body, and a module-scope scan
    cannot tell which call site it belongs to.  #892 r26: a *module-scope*
    walrus or ``match`` capture replaces the module binding outright and is
    reported by ``_module_extra_bindings`` -- the caller adds both.
    """
    names = set()
    for function in ast.walk(tree):
        if not isinstance(function, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        names.update(_function_local_names(function))
    return names


def _module_extra_bindings(tree):
    """Names bound at module scope by a walrus or a ``match`` capture (#892 r26).

    ``_module_scope_binding_map`` reads *statements*, so ``if (name := stub):``
    and ``match [stub]: case [name]:`` rebind the module name while the graph
    still holds the single ``def`` binding (GLM concurrency r25 P0-1 / boundary
    r25 P0-2, both graded P0: the runtime name is the stub).
    """
    if not isinstance(tree, ast.Module):
        return frozenset()
    return frozenset(_walrus_names(tree) | _match_capture_names(tree))


def _tainted_global_rebinds(tree, loaders):
    """Names a ``global`` rebind lands on through a shadowed loader (#892 r25).

    ``global common; common = _load(...)`` is accepted as a real load, but when
    the same function also binds ``_load`` locally the call returns whatever
    that local returns and the module binding is replaced by a stub.  The
    module-scope walk cannot see the shadowing, so any name declared global in
    such a function loses the trust outright (GLM sibling r24 P1-1).
    """
    tainted = set()
    for function in ast.walk(tree):
        if not isinstance(function, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        global_names = {name for node in ast.walk(function)
                        if isinstance(node, ast.Global) for name in node.names}
        if not global_names:
            continue
        local_names = _function_local_names(function)
        if local_names & set(loaders):
            tainted |= global_names
    return tainted


def _dynamic_write_names(tree):
    """Names replaced by a dynamic write into the module namespace (#892 r25).

    ``globals()['load_session_domain'] = stub`` and a literal
    ``exec("load_session_domain = stub")`` mutate a module binding with no
    statement the binding graph can see (GLM boundary r24 P1-4).  #892 r26
    closes the spellings one hop away from the first two: an alias of
    ``globals()``/``vars()`` (``g = globals(); g['x'] = ...``),
    ``globals().update({...})``, ``setattr(<module object>, 'x', ...)``, and an
    ``exec`` whose source is not a literal at all.  The apparatus' own
    ``exec(compile(...))`` bootstrap stays allowed: a ``compile`` call is the
    one spelling whose source the audit can attribute (GLM boundary r25 P1-3 /
    concurrency r25 P1-5 / simplify r25 P1-2).

    ``'*'`` means any name could move; the whole file fails closed.
    """
    kinds = _object_kinds(tree)
    names = set()
    # #892 r26: writes to the module namespace land in module scope wherever
    # they are spelled -- ``global md`` plus ``globals()['md'] = ...`` inside a
    # function replaces the module binding -- so this scan walks the whole file
    # rather than pruning definition bodies.
    for node in ast.walk(tree):
        if isinstance(node, (ast.Assign, ast.AnnAssign, ast.AugAssign)):
            targets = (node.targets if isinstance(node, ast.Assign)
                       else [node.target])
            for target in targets:
                # #892 r32: ``globals()['common'], x = None, None`` replaces the
                # name through an unpacked target; the leaf walk reaches it
                # (codex boundary r31 P0-7).
                for leaf in _leaf_targets([target]):
                    if isinstance(leaf, ast.Name):
                        # #892 r30: ``g = globals(); g |= {'common': <stub>}``
                        # replaces the namespace through an alias (GLM sibling
                        # r29 P1-3/P1-5).
                        if (isinstance(node, ast.AugAssign)
                                and _is_namespace_object(leaf, kinds)):
                            names.add('*')
                        continue
                    if not isinstance(leaf, ast.Subscript):
                        continue
                    if not _is_namespace_object(leaf.value, kinds):
                        continue
                    names |= _write_key_names(leaf.slice, tree)
        elif isinstance(node, ast.Call):
            func = node.func
            written = _mapping_method_call(node, kinds, tree)
            if written is not None and _is_namespace_object(written[0], kinds):
                # ``globals().update(...)``, ``dict.update(globals(), ...)``,
                # ``globals().pop(...)``/``clear()`` -- one decoder for the
                # bound and the unbound spelling (#892 r30).
                keys = written[1]
                names |= {'*'} if '*' in keys else set(keys)
                continue
            helper = _callable_name(func, kinds)
            if helper == 'eval' and node.args:
                # #892 r30: ``eval("setattr(md, 'common', None)")`` writes with
                # no statement the graph can see (GLM concurrency r29 P0-5).
                names.add('*')
            if _setattr_arguments(node, kinds) is not None:
                names |= _setattr_module_keys(node, kinds, tree)
            elif helper == 'exec' and node.args:
                # ``exec(source, namespace[, namespace])`` writes *that*
                # namespace; the apparatus executes committed bytes into a
                # foreign module's ``__dict__`` and touches nothing here.  Only
                # an ``exec`` that can land in this module's namespace fails
                # the file closed.
                namespaces = [node.args[1]] if len(node.args) >= 2 else []
                if len(node.args) >= 3:
                    namespaces.append(node.args[2])
                if not namespaces:
                    names.add('*')
                elif any(_is_namespace_object(n, kinds)
                         or _is_module_object(n, kinds) for n in namespaces):
                    names.add('*')
    return frozenset(names)


# #892 r27: the builtin callables whose rewritten call is the same write.  The
# r26 rules all matched ``ast.Name`` with a literal id, so ``sa = setattr; sa(md,
# 'common', stub)``, ``builtins.setattr(...)`` and ``bi.globals()`` were the same
# write under another spelling (GLM sibling r26 P1-4 / simplify r26 P1-2 /
# DeepSeek concurrency r26 P1-6).  One alias resolver now feeds every rule.
_CALLABLE_KINDS = ('setattr', 'setitem', 'exec', 'eval', 'globals', 'vars',
                   'locals', 'getattr')
_KIND_CACHE = {}


def _value_elements(node, simple=None, depth=0):
    """*node*'s tuple elements with a Starred over a literal expanded (#892 r33).

    ``*(md,), None`` unpacks to ``md, None``; a Starred over anything else has
    no static shape, so the caller falls back to the whole-value binding.
    #892 r34: a star over a *name* bound to one tuple literal has that shape
    (``xs = (md,); m, x = (*xs, None)``), and reading it as unknown lost the
    receiver (codex boundary r33 P0-3).
    """
    if depth > 8:
        return None
    elements = []
    for element in node.elts:
        if not isinstance(element, ast.Starred):
            elements.append(element)
            continue
        inner = element.value
        if simple is not None and isinstance(inner, ast.Name):
            values = simple.get(inner.id) or ()
            if len(values) == 1:
                inner = values[0]
        if not isinstance(inner, (ast.Tuple, ast.List)):
            return None
        nested = _value_elements(inner, simple, depth + 1)
        if nested is None:
            return None
        elements.extend(nested)
    return elements


def _target_value_pairs(target, value, simple=None):
    """``(name, value)`` pairs a binding gives, unpacking tuples (#892 r32).

    ``m, other = md, None`` binds ``m`` to ``md``; r31 bound ``m`` to the whole
    right-hand tuple, whose kinds are empty, so a later ``m.common = <stub>``
    never withdrew the receiver (codex boundary r31 P0-5).
    """
    if isinstance(value, ast.Starred):
        # #892 r33: ``m, x = *(md,), None`` stores the segment behind a star;
        # pairing ``m`` with the Starred node gave it no kinds at all, so the
        # receiver was lost (GLM boundary r32 P1-3).
        return _target_value_pairs(target, value.value, simple)
    if isinstance(target, (ast.Tuple, ast.List)) and isinstance(value, (ast.Tuple, ast.List)):
        # #892 r33: ``m, x = *(md,), None`` expands the star before the
        # positional pairing; reading the Starred node as one element gave
        # ``m`` no kinds and lost the receiver (GLM boundary r32 P1-3).
        expanded = _value_elements(value, simple)
        if expanded is None:
            return [(name, value) for name in _target_names(target)]
        value = ast.Tuple(elts=expanded, ctx=ast.Load())
        stars = [index for index, element in enumerate(target.elts)
                 if isinstance(element, ast.Starred)]
        if len(stars) == 1:
            # ``m, *rest = (md, None)``: the elements around the star keep
            # their positions; the star target itself is handed the whole
            # right-hand side, which fails closed on any later write.
            index = stars[0]
            trailing = len(target.elts) - index - 1
            if len(value.elts) >= index + trailing:
                pairs = []
                for element, element_value in zip(target.elts[:index], value.elts[:index]):
                    pairs.extend(_target_value_pairs(element, element_value, simple))
                if trailing:
                    for element, element_value in zip(target.elts[index + 1:],
                                                      value.elts[len(value.elts) - trailing:]):
                        pairs.extend(_target_value_pairs(element, element_value, simple))
                # #892 r34: a star target holds the *list* of the elements it
                # covered, so ``rest[0]`` may be any of them.  Binding the name
                # to the whole tuple named no element and lost the receiver
                # (``a, *rest = (None, md); rest[0].common = <stub>``; codex
                # boundary r33 P0-3).
                for element_value in value.elts:
                    pairs.extend(_target_value_pairs(target.elts[index].value,
                                                     element_value, simple))
                return pairs
        elif not stars and len(target.elts) == len(value.elts):
            pairs = []
            for element, element_value in zip(target.elts, value.elts):
                pairs.extend(_target_value_pairs(element, element_value, simple))
            return pairs
    return [(name, value) for name in _target_names(target)]


_CONTAINER_WRAPPERS = frozenset((
    'iter', 'next', 'tuple', 'list', 'sorted', 'reversed', 'set', 'frozenset',
))


def _iter_literal_elements(node):
    """Elements of ``iter((a, b))`` / ``tuple([...])`` (#892 r33).

    ``for m in iter((md,)): m.common = <stub>`` iterates a literal the loop
    rule did not read, so the receiver was lost (codex boundary r32 P0-3).

    #1953 fix-r1 round 7 (GLM sibling r3 P2): the wrapper names live in one
    constant now -- this decoder and :func:`_container_callee` had drifted
    (``sorted``/``next`` missing here, ``set``/``frozenset`` missing there).
    """
    if (isinstance(node, ast.Call) and len(node.args) == 1
            and not node.keywords and isinstance(node.args[0], (ast.Tuple, ast.List))
            and isinstance(node.func, ast.Name)
            and node.func.id in _CONTAINER_WRAPPERS):
        return node.args[0].elts
    return []


def _shadowed_parameters(scope):
    """Parameter names of *scope* (#1953 fix-r1 round 9, codex r5 P1-8).

    A module-level ``items = (md,)`` says nothing about a function whose
    ``items`` is a parameter: ``def main(items): for m in items: ...`` iterates
    whatever the caller passed.  The literal decoders take these names as
    unresolvable, which is the difference between a false FAIL and the truth.
    """
    if isinstance(scope, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
        return set(_argument_names(scope.args))
    return set()


def _bound_literal_elements(node, simple, seen=None, shadowed=()):
    """Elements of a literal iterable, its wrapper, or a name bound to one.

    #1953 fix-r1 round 8 (GLM sibling r4 P1-1): ``items = (md,)`` followed by
    ``for m in items: m.common = <stub>`` names the same elements through a
    name, and only the comprehension branch of the receiver rules followed the
    name -- the three other decode sites (the alias table, the scoped alias
    table and the comprehension-target rule) read the literal alone, so
    aggregation-audit stayed green on a write it had already learned to see in
    one spelling.
    """
    if isinstance(node, (ast.Tuple, ast.List, ast.Set)):
        return node.elts
    elements = _iter_literal_elements(node)
    if elements:
        return elements
    if isinstance(node, ast.Name):
        if node.id in shadowed:
            return []
        seen = set() if seen is None else seen
        if node.id in seen:
            return []
        seen.add(node.id)
        values = simple.get(node.id) or ()
        # Round 9 (GLM boundary r5 P2-5): two bindings of the same name used to
        # answer "no elements", which made the loop target unbound and hid the
        # write.  Take the union -- an ambiguous binding may hold any of them,
        # so the conservative answer is every element of every binding.
        elements = []
        for value in values:
            for element in _bound_literal_elements(
                    value, simple, set(seen), shadowed):
                if element not in elements:
                    elements.append(element)
        return elements
    return []


def _visible_binding_table(simple, binding_scopes, chain, before=None,
                           certain=None):
    """``name -> values`` as the scope chain *chain* sees them (#1953 r10).

    ``_simple_bindings`` is file-wide, so a same-named local of an unrelated
    function answered for this one: ``other()`` binding ``items = (md,)`` made
    a clean ``main()`` with ``items = ()`` withdraw the receiver (codex r6
    P1-6).  A name resolves in the nearest scope of *chain* that binds it, and
    within that scope a later literal supersedes the earlier bindings -- an
    earlier value that is not a literal stays in the union, so an ambiguous
    binding still fails closed (GLM boundary r5 P2-5).

    #1953 fix-r1 round 11 (codex r7 P0-1 / P0-2): both of those filters were
    too eager.  A later literal only supersedes when its assignment provably
    runs -- ``items = (md,); if flag: items = ()`` still iterates ``md``
    whenever the flag is false, and the round-10 floor dropped it.  And the
    use's line number says nothing about a binding in an *enclosing* scope:
    the inner function resolves it when it is called, which may be long after
    the use's own line, so only the innermost scope is filtered by line.
    """
    table = {}
    for name, values in simple.items():
        for index, level in enumerate(chain):
            live = [value for value in values
                    if binding_scopes.get(id(value)) is level
                    and (before is None or index > 0
                         or getattr(value, 'lineno', 0) <= before)]
            if not live:
                continue
            literals = [getattr(value, 'lineno', 0) for value in live
                        if isinstance(value, (ast.Tuple, ast.List, ast.Set,
                                              ast.Dict, ast.Constant))
                        and (certain is None or id(value) in certain)]
            # #1953 fix-r1 round 12 (codex r8 P0-1): the supersede rule is
            # about *source order inside one scope*, and even there only about
            # what the use's own scope has executed so far.  An enclosing
            # scope is resolved when the nested function is *called*, which
            # may be any point between its first and last rebinding:
            # ``items = (md,); ...; items = ()`` still hands ``md`` to a
            # nested reader that runs in between, so every literal binding of
            # an outer scope stays a candidate (fail-closed).
            if literals and index == 0:
                floor = max(literals)
                live = [value for value in live
                        if getattr(value, 'lineno', 0) >= floor]
            table[name] = live
            break
    return table


def _simple_bindings(tree):
    """``name -> values`` for one-target assignments (#892 r33)."""
    simple = {}
    for node in ast.walk(tree):
        if (isinstance(node, ast.Assign) and len(node.targets) == 1
                and isinstance(node.targets[0], ast.Name)):
            simple.setdefault(node.targets[0].id, []).append(node.value)
        elif (isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name)
              and node.value is not None):
            simple.setdefault(node.target.id, []).append(node.value)
    return simple


def _context_argument(node, simple, seen=None):
    """The object a ``with`` manager was handed (#892 r33).

    ``with C(md) as m`` binds the value ``C`` returns, which the graph cannot
    name; ``C`` handed exactly one positional argument, so that argument is the
    bound object for the shapes the apparatus uses (``nullcontext``,
    ``contextlib.suppress``).  A manager held in a name is resolved first; the
    walk is bounded by its own chain through *seen* rather than by a hop cap
    (#892 r34).
    """
    seen = set() if seen is None else seen
    if isinstance(node, ast.Call) and len(node.args) == 1 and not node.keywords:
        return node.args[0]
    if isinstance(node, ast.Name):
        if node.id in seen:
            return None
        seen.add(node.id)
        values = simple.get(node.id) or ()
        if len(values) == 1:
            return _context_argument(values[0], simple, seen)
    return None


def _alias_assignments(tree):
    """``(name, value)`` pairs a plain binding gives a bare name (#892 r27)."""
    pairs = []
    simple = _simple_bindings(tree)
    scopes = {id(node): scope for scope, node in _walk_scoped(tree)}
    for node in ast.walk(tree):
        if isinstance(node, ast.Assign):
            for target in node.targets:
                pairs.extend(_target_value_pairs(target, node.value, simple))
        elif isinstance(node, ast.AnnAssign) and node.value is not None:
            for name in _target_names(node.target):
                pairs.append((name, node.value))
        elif isinstance(node, ast.NamedExpr) and isinstance(node.target, ast.Name):
            pairs.append((node.target.id, node.value))
        elif isinstance(node, (ast.ListComp, ast.SetComp, ast.GeneratorExp,
                               ast.DictComp)):
            # #1953 fix-r1: a comprehension target is a binding like a ``for``
            # target (``[h(md, 'common', <stub>) for h in [operator.setattr]]``),
            # and the kinds table is what the receiver rules consult.
            #
            # Round 9 (GLM sibling r5 P3-4): a filter that can never hold
            # yields nothing, so the target is never bound to an element.
            if _emptied_comprehension(node):
                continue
            for generator in node.generators:
                elements = (generator.iter.elts
                            if isinstance(generator.iter, (ast.List, ast.Tuple,
                                                           ast.Set))
                            and generator.iter.elts else [generator.iter])
                for element in elements:
                    pairs.extend(_target_value_pairs(generator.target, element,
                                                     simple))
        elif isinstance(node, ast.Match):
            # #1953 fix-r1: ``match operator.setattr: case h:`` binds the
            # capture to (a part of) the subject, exactly like a ``for``
            # target binds to its element.  ``_name_bindings`` learned this
            # for the status rule; the kinds table is what the *receiver*
            # rules consult, so the setattr spelling stayed invisible there
            # (same shape as codex boundary r35 P0-3).
            for name in _match_capture_names(node):
                pairs.append((name, node.subject))
        elif isinstance(node, (ast.For, ast.AsyncFor)):
            # #892 r32: ``for m in (md,): m.common = <stub>`` binds the loop
            # target to an element the literal iterable names (codex boundary
            # r31 P0-5).
            elements = _bound_literal_elements(
                node.iter, simple, shadowed=_shadowed_parameters(scopes.get(id(node))))
            for element in elements:
                pairs.extend(_target_value_pairs(node.target, element, simple))
        elif isinstance(node, (ast.With, ast.AsyncWith)):
            for item in node.items:
                if item.optional_vars is None:
                    continue
                context = item.context_expr
                # ``with nullcontext(md) as m:`` -- the bound object is the one
                # the manager was handed.  #892 r33: the manager may be held in
                # a name first (``h = nullcontext(md); with h as m:``), so the
                # argument is resolved through the simple binding table before
                # the fallback (codex boundary r32 P0-3).
                argument = _context_argument(context, simple)
                if argument is not None:
                    pairs.extend(_target_value_pairs(item.optional_vars, argument, simple))
                else:
                    pairs.extend(_target_value_pairs(item.optional_vars, context, simple))
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
            # #892 r32: ``def f(m=md): m.common = <stub>`` binds the parameter
            # to its default (codex boundary r31 P0-5).
            arguments = node.args
            positional = list(arguments.posonlyargs) + list(arguments.args)
            defaults = list(arguments.defaults)
            for argument, default in zip(positional[len(positional) - len(defaults):],
                                         defaults):
                pairs.append((argument.arg, default))
            for argument, default in zip(arguments.kwonlyargs, arguments.kw_defaults):
                if default is not None:
                    pairs.append((argument.arg, default))
    return pairs


def _expr_is_sys_modules(node, kinds):
    """True when *node* evaluates to the ``sys.modules`` dict (#892 r27)."""
    if isinstance(node, ast.Name):
        return 'sysmods' in kinds.get(node.id, ())
    if isinstance(node, ast.Call):
        # #892 r32: ``getattr(sys, 'modules')`` names the same dict as the
        # attribute spelling; the alias form already resolved through
        # ``_expr_kinds``, the direct one did not (GLM sibling r31 P1-1).
        return 'sysmods' in _expr_kinds(node, kinds)
    return (isinstance(node, ast.Attribute) and node.attr == 'modules'
            and 'sys' in _expr_kinds(node.value, kinds))


def _import_literal(node):
    """``'sys'`` / ``'builtins'`` when *node* imports one by a literal (#892 r28).

    ``__import__('sys')`` and ``importlib.import_module('builtins')`` name the
    same modules as ``import sys as s``; r27 resolved only the statement form,
    so ``__import__('sys').modules[__name__].__setattr__(...)`` stayed green.
    """
    func = node.func
    if not ((isinstance(func, ast.Name) and func.id in ('__import__', 'import_module'))
            or (isinstance(func, ast.Attribute) and func.attr == 'import_module')):
        return None
    if len(node.args) != 1:
        return None
    argument = node.args[0]
    if isinstance(argument, ast.Constant) and argument.value in ('sys', 'builtins'):
        return argument.value
    return None


def _getattr_literal(node, kinds):
    """The attribute a two-argument ``getattr(obj, '<literal>')`` reads (#892 r30).

    ``getattr(md, '__dict__')['common'] = <stub>`` and
    ``getattr(md, '__dict__').update(common=<stub>)`` name the same mapping as
    ``md.__dict__``; the call spelling reached no mapping root, so the relay
    stayed trusted (MiniMax simplify r29 P0-4, third spelling).

    #892 r31: the callee goes through the shared alias resolver, so
    ``from builtins import getattr as _g`` / ``builtins.getattr`` /
    ``__builtins__['getattr']`` name the same builtin as the bare spelling
    (the r30 fix judged only the literal name ``getattr``).
    """
    if not (isinstance(node, ast.Call) and len(node.args) in (2, 3)
            and _callable_name(node.func, kinds) == 'getattr'):
        return None
    literal = node.args[1]
    if isinstance(literal, ast.Constant) and isinstance(literal.value, str):
        return literal.value
    return None


def _expr_kinds(node, kinds):
    """Object kinds *node* can evaluate to under the alias map (#892 r27).

    One shared resolver feeds every rule that has to know whether an
    expression is this module's namespace, a module object, ``sys``,
    ``sys.modules`` or a builtin callable.  The r24/r25 rounds each taught one
    rule one more spelling (``import sys as _s``, ``from sys import modules``,
    ``g = globals()``) and the other rules stayed one spelling behind (GLM
    concurrency r26 P1-1 / DeepSeek sibling r26 P1-5).
    """
    if isinstance(node, ast.Name):
        return kinds.get(node.id, frozenset())
    if isinstance(node, ast.Attribute):
        if node.attr == 'modules':
            if 'sys' in _expr_kinds(node.value, kinds) \
                    or 'sysmods' in _expr_kinds(node.value, kinds):
                return frozenset({'sysmods'})
            return frozenset()
        if node.attr == '__dict__':
            inner = _expr_kinds(node.value, kinds)
            if 'module' in inner or 'namespace' in inner:
                return frozenset({'namespace'})
            return frozenset()
        if node.attr in _CALLABLE_KINDS:
            # #892 r32: no early return -- ``op.setitem`` hits the builtin
            # table first and used to fall out here, so ``put = op.setitem``
            # lost the callable (codex sibling r31 P0-1).
            if 'builtins' in _expr_kinds(node.value, kinds):
                return frozenset({'fn:' + node.attr})
        # #892 r30: ``ft.partial`` / ``operator.setitem`` / ``getattr`` reach the
        # same callables as their ``from ... import`` spellings.
        if node.attr in ('partial', 'setitem', 'setattr', 'getattr', 'itemgetter',
                         'attrgetter'):
            inner = _expr_kinds(node.value, kinds)
            if inner & {'functools', 'operator', 'builtins'}:
                return frozenset({'fn:' + node.attr})
        if node.attr in _MANAGED_WRITE_ATTRS:
            # #892 r32: a bound managed method is a write handle.  The alias
            # graph records it so ``f = md.__setattr__; f('common', <stub>)``
            # withdraws the receiver instead of dropping the write (codex
            # sibling r31 P0-2).
            # #1953 fix-r1 round 16 (codex r15 P0-1/P0-4): the receiver stays
            # in the kind.  The call of such a name is the *bound* spelling --
            # ``f('common', <stub>)`` writes ``md`` -- and the untagged kind
            # made ``_write_call_target`` read it as the unbound builtin, whose
            # first argument is the target: the literal ``'common'`` named no
            # receiver and the write vanished.
            if isinstance(node.value, ast.Name):
                return frozenset({'fn:write-handle:' + node.value.id})
            return frozenset({'fn:write-handle'})
        return frozenset()
    if isinstance(node, ast.Subscript):
        if _expr_is_sys_modules(node.value, kinds):
            return frozenset({'module'})
        # #892 r32: ``__builtins__['getattr']`` / ``__builtins__['partial']``
        # evaluate to the builtin callable the name denotes, so the alias graph
        # can hand a handle on to ``_unbounded_write`` (fail-closed) instead of
        # dropping it (GLM sibling r31 P1-3).
        literal = _builtins_subscript_literal(node, kinds)
        if literal in _CALLABLE_KINDS + ('partial', 'itemgetter'):
            return frozenset({'fn:' + literal})
        return frozenset()
    if isinstance(node, ast.Call):
        imported = _import_literal(node)
        if imported is not None:
            return frozenset({imported})
        # #892 r30: ``p = functools.partial(setattr, md)`` keeps a write handle
        # whose target the alias table cannot name -- ``p('common', None)``
        # still writes ``md.common`` (MiniMax boundary r29 P0-3).
        if 'fn:partial' in _expr_kinds(node.func, kinds):
            return frozenset({'fn:partial'})
        # #892 r30: ``getattr(md, '__dict__')`` evaluates to whatever
        # ``md.__dict__`` evaluates to -- the kinds rule has to decode the call
        # spelling, not only the attribute one.
        literal = _getattr_literal(node, kinds)
        if literal is not None:
            return _expr_kinds(ast.Attribute(value=node.args[0], attr=literal,
                                             ctx=ast.Load()), kinds)
        func = node.func
        # #892 r32: ``op.attrgetter('__setattr__')(md)`` is the bound managed
        # method; the outer call is a write handle with no nameable target
        # (codex sibling r31 P0-3).
        if 'fn:attrgetter' in _expr_kinds(func, kinds) and node.args:
            literal = node.args[0]
            if (isinstance(literal, ast.Constant)
                    and literal.value in _MANAGED_WRITE_ATTRS):
                return frozenset({'fn:write-handle'})
        # #892 r32: one resolver, so ``__builtins__['globals']()`` and the
        # aliased/attribute spellings agree (GLM sibling r31 P1-3).
        if _callable_name(func, kinds) in ('globals', 'vars', 'locals'):
            return frozenset({'namespace'})
        return frozenset()
    return frozenset()


def _object_kinds(tree):
    """name -> the object kinds it can denote anywhere in *tree* (#892 r27).

    A fixpoint over every alias binding form (``Assign``/``AnnAssign``/walrus),
    so ``g = globals()``, ``h = g``, ``sm = sys.modules``, ``s = sys``,
    ``sa = setattr`` and ``bi = builtins`` all resolve the way the direct
    spelling does.
    """
    cached = _KIND_CACHE.get(id(tree))
    if cached is not None and cached[0] is tree:
        return cached[1]
    kinds = {}

    def add(name, kind):
        bucket = kinds.setdefault(name, set())
        if kind in bucket:
            return False
        bucket.add(kind)
        return True

    for helper in _CALLABLE_KINDS:
        add(helper, 'fn:' + helper)
    # #892 r30: ``__builtins__`` is the builtins module in a module body, so
    # ``bi = __builtins__; bi.dict.update(...)`` names the same type (GLM
    # sibling r29 P1-4).
    add('__builtins__', 'builtins')
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name == 'sys':
                    add(alias.asname or 'sys', 'sys')
                elif alias.name == 'builtins':
                    add(alias.asname or 'builtins', 'builtins')
                elif alias.name in ('functools', 'operator'):
                    add(alias.asname or alias.name, alias.name)
        elif isinstance(node, ast.ImportFrom) and node.module in ('sys', 'builtins',
                                                                  'functools',
                                                                  'operator'):
            for alias in node.names:
                if node.module == 'sys' and alias.name == 'modules':
                    add(alias.asname or 'modules', 'sysmods')
                elif node.module == 'builtins' and alias.name in _CALLABLE_KINDS:
                    # #892 r27: ``from builtins import setattr as _sa`` names
                    # the same callable as the bare spelling (DeepSeek boundary
                    # r26 P1-2).
                    add(alias.asname or alias.name, 'fn:' + alias.name)
                elif node.module == 'builtins' and alias.name in _BUILTIN_TYPES:
                    # #892 r30: ``from builtins import dict as _bd``
                    add(alias.asname or alias.name, 'type:' + alias.name)
                elif node.module in ('functools', 'operator') and alias.name in (
                        'partial', 'setitem', 'setattr', 'getattr', 'update'):
                    # #892 r30: ``from functools import partial`` /
                    # ``from operator import setitem as put``
                    add(alias.asname or alias.name, 'fn:' + alias.name)
    pairs = _alias_assignments(tree)
    changed = True
    while changed:
        changed = False
        for name, value in pairs:
            for kind in _expr_kinds(value, kinds):
                if add(name, kind):
                    changed = True
    _KIND_CACHE[id(tree)] = (tree, kinds)
    return kinds


def _builtins_subscript_literal(node, kinds):
    """``'<name>'`` for ``__builtins__['<name>']`` or an alias, else ``None``.

    #892 r32: the fixture runs as ``__main__``, where ``__builtins__`` is a
    dict, so the subscript and the attribute spellings reach the same builtin.
    Three decoders each carried their own copy of the test and drifted (GLM
    simplify r31 P2-1), and the alias ``bi = __builtins__; bi['setattr']`` was
    read by none of them (GLM sibling r31 P3-5).
    """
    if not isinstance(node, ast.Subscript):
        return None
    literal = node.slice
    if (isinstance(literal, ast.Constant) and isinstance(literal.value, str)
            and 'builtins' in _expr_kinds(node.value, kinds)):
        return literal.value
    return None


def _callable_name(func, kinds):
    """The builtin a call target denotes, or ``None`` (#892 r27)."""
    if isinstance(func, ast.Name):
        for helper in _CALLABLE_KINDS + ('getattr', 'partial', 'itemgetter'):
            if 'fn:' + helper in kinds.get(func.id, ()):
                return helper
        return None
    if isinstance(func, ast.Attribute) and func.attr in _CALLABLE_KINDS:
        if 'builtins' in _expr_kinds(func.value, kinds):
            return func.attr
        return None
    if isinstance(func, ast.Attribute):
        # #892 r30: ``ft.partial`` / ``operator.setitem``
        if 'fn:' + func.attr in _expr_kinds(func, kinds):
            return func.attr
    if isinstance(func, ast.Subscript):
        # #892 r31: the fixture runs as ``__main__``, where ``__builtins__`` is
        # a dict, so ``__builtins__['getattr'](md, '__dict__')`` is the builtin
        # call.  r30 taught the write-target rule this spelling but not the
        # shared resolver, so the read side stayed one spelling behind.
        # #892 r32: one decoder for all three rules, alias included.
        literal = _builtins_subscript_literal(func, kinds)
        if literal in _CALLABLE_KINDS + ('partial', 'itemgetter'):
            return literal
        return None
    return None


def _is_namespace_object(node, kinds):
    """True when *node* evaluates to this module's namespace dict (#892 r26)."""
    return 'namespace' in _expr_kinds(node, kinds)


def _is_module_object(node, kinds):
    """True when *node* evaluates to a module object (#892 r26).

    ``sys.modules[__name__]`` / ``sys.modules['__main__']`` are this module or
    the entry module; ``setattr`` on them writes the module globals.  #892 r27:
    ``....__dict__`` is the same mapping (DeepSeek boundary r26 P1-5).
    """
    return 'module' in _expr_kinds(node, kinds)


def _write_key_names(expr, tree):
    """Names a subscript/``setattr`` key can spell (#892 r27).

    A literal string is itself; a name whose every binding is a literal string
    (``_HELD_STATE = "_pdrs_negative_process_held"``) or a literal dict
    iteration resolves to those spellings; anything else is unbounded and
    withdraws the whole file (GLM boundary r26 P0-4 / DeepSeek sink r26 P1-4).
    """
    found = _constant_string_values(expr, tree)
    return {'*'} if found is None else found


# #892 r30: every attribute spelling of the same write.  ``setitem`` is the
# ``operator``/``builtins`` form of ``__setitem__`` -- the status ruler
# ``_call_written_values`` already read it, the receiver rule did not (GLM
# concurrency r29 P0-2, MiniMax concurrency r29 P0-3).
_MANAGED_WRITE_ATTRS = ('__setattr__', '__setitem__', 'setattr', 'setitem')
# #892 r35: a *read* that hands over the receiver's mapping or a method that
# can reach it.  ``_MANAGED_WRITE_ATTRS`` holds the write sides only, so
# ``getattr(md, '__getattribute__')('__dict__')['common'] = <stub>`` wrote
# through an attribute name no rule decoded (codex boundary r34 P0-6).
_HANDOVER_ATTRS = ('__dict__', '__getattribute__')


def _managed_write_kind(name):
    """``'setitem'`` for the item spellings, ``'setattr'`` otherwise (#892 r30)."""
    return 'setitem' if name.lstrip('_').startswith('setitem') else 'setattr'


def _write_call_target(func, kinds, depth=0):
    """``(kind, receiver, bound)`` a write call target denotes, else ``None``.

    The receiver is the object a *bound* spelling writes (``md.__setattr__``,
    ``getattr(md, '__setattr__')``); ``bound`` are the leading arguments a
    ``partial`` already fixed.  Covered spellings (#892 r30): the attribute
    forms, a bare/aliased builtin, ``getattr(<obj>, '<literal>')`` and
    ``functools.partial(<managed>[, <obj>])``.
    """
    if depth > 50:
        return None
    if isinstance(func, ast.Attribute) and func.attr in _MANAGED_WRITE_ATTRS:
        return _managed_write_kind(func.attr), func.value, ()
    if isinstance(func, ast.Name):
        for candidate in _MANAGED_WRITE_ATTRS:
            if 'fn:' + candidate in kinds.get(func.id, ()):
                return _managed_write_kind(candidate), None, ()
        # #1953 fix-r1 round 16 (codex r15 P0-1/P0-4): a *bound* handle names
        # the receiver it was taken from -- ``_f = md.__setattr__`` /
        # ``_f = getattr(md, '__setattr__')`` then ``_f('common', <stub>)``
        # writes ``md``, not the literal key.  Read as the unbound spelling the
        # call named no receiver and the write disappeared.
        handle_kinds = kinds.get(func.id, ())
        for kind in sorted(handle_kinds):
            if kind.startswith('fn:write-handle:') and 'fn:partial' not in handle_kinds:
                return 'setitem', ast.Name(id=kind.split(':', 2)[2],
                                           ctx=ast.Load()), ()
        # #1953 fix-r1 (codex r1b P1): a name the kinds table only knows as
        # *some* write handle (a comprehension target bound to
        # ``operator.setitem``, ``h = getattr(md, <name>)``) is the unbound
        # spelling: it names its target first, exactly like the builtin.
        # Without the decode ``_unbounded_write`` answered "no target" and
        # withdrew every receiver in the file, which turned the harmless
        # ``[h for h in (operator.setitem,)]; h = lambda *a: None;
        # h(report, 'status', 'PASS')`` into a false FAIL, while the real
        # ``h(md, 'common', <stub>)`` spelling kept no named target at all.
        # A ``partial`` pre-binds its leading arguments, so that shape stays
        # undecoded here and keeps failing closed.
        if 'fn:write-handle' in handle_kinds and 'fn:partial' not in handle_kinds:
            return 'setitem', None, ()
        return None
    if isinstance(func, ast.Subscript):
        # #892 r30: the fixture runs as ``__main__``, where ``__builtins__`` is
        # a dict, so ``__builtins__['setattr'](md, 'common', <stub>)`` is the
        # builtin call (GLM simplify r29 P0-4, fourth spelling).
        # #892 r32: the same decoder the read side uses, alias included.
        literal = _builtins_subscript_literal(func, kinds)
        if literal in _MANAGED_WRITE_ATTRS:
            return _managed_write_kind(literal), None, ()
        return None
    if not isinstance(func, ast.Call):
        return None
    callee = _callable_name(func.func, kinds)
    # #892 r32: the three-argument spelling carries a default that a present
    # attribute never reaches -- ``getattr(md, '__setattr__', None)(...)``
    # still writes ``md`` (GLM boundary r31 P0-1).
    if callee == 'getattr' and len(func.args) in (2, 3):
        literal = func.args[1]
        if (isinstance(literal, ast.Constant) and literal.value in _MANAGED_WRITE_ATTRS):
            return _managed_write_kind(literal.value), func.args[0], ()
        return None
    if (callee == 'partial' or _expr_kinds(func.func, kinds) & {'fn:partial'}) \
            and func.args:
        inner = _write_call_target(func.args[0], kinds, depth + 1)
        if inner is None:
            return None
        kind, receiver, bound = inner
        return kind, receiver, tuple(bound) + tuple(func.args[1:])
    return None


def _attrgetter_is_dynamic(call):
    """True when ``op.attrgetter(<expr>)`` fixes its attribute at run time.

    A literal managed name is already a ``fn:write-handle``; anything else can
    only be judged by the object the handle is taken from (#892 r33).
    """
    if not call.args:
        return True
    literal = call.args[0]
    return not (isinstance(literal, ast.Constant)
                and literal.value in _MANAGED_WRITE_ATTRS)


def _constant_int(node):
    """The int literal *node* spells, including a negative one (#1953 r4).

    ``x[-1]`` parses as ``UnaryOp(USub, Constant(1))``; ``bool`` is excluded so
    ``x[True]`` is not folded into a claim about ``x[1]``.
    """
    if (isinstance(node, ast.Constant) and isinstance(node.value, int)
            and not isinstance(node.value, bool)):
        return node.value
    if (isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.USub)
            and isinstance(node.operand, ast.Constant)
            and isinstance(node.operand.value, int)
            and not isinstance(node.operand.value, bool)):
        return -node.operand.value
    return None


def _constant_string(node, simple):
    """The literal string *node* denotes, through one local binding (#892 r34).

    ``name = '__doc__'; getattr(md, name)`` is a read of a known attribute, not
    an unknown write handle; without the lookup the r33 scan withdrew the
    receiver for it (codex boundary r33 P1-8).

    #1953 fix-r1: a constant element of a constant sequence is the same read.
    ``keys = ('__doc__',); getattr(md, keys[0])`` is the file's own docstring
    read, and failing it closed was a regression this round introduced (codex
    boundary r35 P1-5); ``keys = ('__setattr__',)`` still names the write
    (``calibrate-getattr-const-subscript-write``).
    """
    seen = set()
    while True:
        if isinstance(node, ast.Name) and simple is not None:
            if node.id in seen:
                return None
            seen.add(node.id)
            values = simple.get(node.id) or ()
            if len(values) != 1:
                return None
            node = values[0]
            continue
        if isinstance(node, ast.Subscript):
            sequence = node.value
            index = node.slice
            if isinstance(sequence, ast.Name) and simple is not None:
                if sequence.id in seen:
                    return None
                seen.add(sequence.id)
                values = simple.get(sequence.id) or ()
                if len(values) != 1:
                    return None
                sequence = values[0]
            # #1953 fix-r1 (codex r1b P0): only an immutable tuple is folded.
            # ``keys = ['__doc__']; keys[0] = '__setattr__'; getattr(md,
            # keys[0])(...)`` writes the list before the read, so folding a
            # list literal told the gate the name was still ``__doc__`` and
            # the write handle went out trusted.  A list is mutable, so its
            # element is never a proven constant.
            position = _constant_int(index)
            if (isinstance(sequence, ast.Tuple) and position is not None):
                # #1953 fix-r1 round 4 (GLM boundary r2 P2): a negative index
                # is a position from the end, not an unknown name --
                # ``keys = ('__doc__',); getattr(md, keys[-1])`` is the same
                # pure read and was failing closed.  A negative literal is
                # ``UnaryOp(USub, Constant)``, not ``Constant(-1)``.
                #
                # #1953 fix-r1 round 7 (GLM boundary r3 P1): a starred element
                # makes the literal length differ from the runtime one
                # (``('__doc__', *more)``), so neither a negative position nor
                # a positive one can be folded -- the element read may be the
                # attacker's ``'__setattr__'``.
                if any(isinstance(item, ast.Starred)
                       for item in sequence.elts):
                    return None
                if position < 0:
                    position += len(sequence.elts)
                if 0 <= position < len(sequence.elts):
                    node = sequence.elts[position]
                    continue
            return None
        break
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    return None


def _handle_escapes(call, parents):
    """True when a handle value reaches a call, a name or a return (#892 r34).

    ``h = getattr(md, name)`` / ``h: object = ...`` / ``invoke(getattr(...))`` /
    ``return getattr(...)`` all let the handle be used as a writer later; a
    value read into a dict or a list (the save half of a save/restore) does
    not.
    """
    parent = parents.get(call)
    if isinstance(parent, ast.Call):
        return True
    if isinstance(parent, (ast.Assign, ast.AnnAssign, ast.NamedExpr, ast.Return)):
        return True
    if isinstance(parent, ast.Starred):
        return True
    # #892 r35: a container literal or a generator is the same escape -- the
    # handle can be taken back out and called (``hs = [getattr(md, n)];
    # hs[0]('common', <stub>)``; codex boundary r34 P0-5).  A dict
    # *comprehension* stays clean on purpose: the apparatus's own save loop
    # (``{name: getattr(md, name) for name in patched}``) only replays the
    # value as the restore operand.
    return isinstance(parent, (ast.List, ast.Tuple, ast.Set, ast.Dict,
                               ast.Yield, ast.YieldFrom))


def _dynamic_getattr(call, kinds, simple=None):
    """True when *call* is ``getattr(obj, <name>)`` with a write-sized name.

    The name counts as unknown unless it is spelled out -- inline or through a
    single string binding -- and a spelled name still counts when it may write
    (``__setattr__``) or hand over the mapping (``__dict__``).  A plain read
    such as ``getattr(md, '__doc__')`` does not withdraw anything (#892 r34,
    codex boundary r33 P0-4 / P1-8).
    """
    if _callable_name(call.func, kinds) != 'getattr' or len(call.args) < 2:
        return False
    literal = _getattr_literal(call, kinds)
    if literal is not None:
        # The inline spelling is already covered by the kinds/write-handle and
        # namespace-dict rules; only the through-a-name spelling is new here.
        # #892 r35: ``__getattribute__`` is the exception -- it is a read, so
        # none of those rules decode it, and it hands the mapping over.
        return literal == '__getattribute__'
    name = _constant_string(call.args[1], simple)
    if name is None:
        return True
    return name in _MANAGED_WRITE_ATTRS or name in _HANDOVER_ATTRS


def _getattribute_pure_read(call, kinds, simple, parents):
    """True when ``getattr(obj, '__getattribute__')(<plain attr>)`` only reads.

    ``__getattribute__`` hands back a method that can return any attribute, so
    it is a hand-over only when what the *outer* call asks for is a write
    spelling or the mapping itself.  ``getattr(md, '__getattribute__')
    ('__doc__')`` is a plain read and withdrew the receiver for nothing --
    a regression this round introduced (#1953 fix-r1, codex boundary r35
    P1-4).  A handle held in a name, called with an unknown name, or asked for
    ``__dict__`` is still a hand-over
    (``calibrate-getattribute-write-attribute``).
    """
    if _callable_name(call.func, kinds) != 'getattr' or len(call.args) < 2:
        return False
    literal = _getattr_literal(call, kinds)
    name = literal if literal is not None else _constant_string(call.args[1],
                                                                simple)
    if name != '__getattribute__':
        return False
    outer = parents.get(call)
    if not (isinstance(outer, ast.Call) and outer.func is call and outer.args):
        return False
    read = _constant_string(outer.args[0], simple)
    return (read is not None and read not in _MANAGED_WRITE_ATTRS
            and read not in _HANDOVER_ATTRS)


def _emptied_comprehension(node):
    """True for a comprehension whose filter can never hold (#1953 r8).

    ``[getattr(md, k) for k in (n,) if False]`` yields nothing, so the
    container is empty and nothing is ever handed out.  r7 taught the receiver
    branch this (codex r3 P1) but left the same question unasked at every
    other consumer -- the container registry, the relay decoder and the
    comprehension-target rule all still poisoned the receiver, which is a
    false FAIL (codex r4 P1-8 / GLM boundary r4 P1-2 / GLM sibling r4 P2-3).
    """
    if not isinstance(node, (ast.ListComp, ast.SetComp, ast.DictComp,
                             ast.GeneratorExp)):
        return False
    return any(_statically_false(condition)
               for generator in node.generators
               for condition in generator.ifs)


def _container_handle_roots(value, kinds, simple):
    """Receiver expressions whose handles a comprehension result carries.

    ``hs = [getattr(md, k) for k in (n,)]`` stores the same handle a container
    literal does, and the escape test deliberately left comprehensions alone;
    the container therefore has to be judged by how it is *used*
    (``hs[0]('common', <stub>)``), not where the handle was stored (#1953
    fix-r1, codex boundary r35 P0-1).  The apparatus's own save loop
    (``{name: getattr(md, name) for name in patched}``) reads the values back
    as restore operands and never calls them, so it stays clean.
    """
    roots = []
    for node in ast.walk(value):
        if not isinstance(node, (ast.ListComp, ast.SetComp, ast.DictComp,
                                 ast.GeneratorExp)):
            continue
        if _emptied_comprehension(node):
            continue
        parts = _comprehension_parts(node)
        for part in parts:
            for inner in ast.walk(part):
                if (isinstance(inner, ast.Call) and inner.args
                        and _dynamic_getattr(inner, kinds, simple)):
                    roots.append(inner.args[0])
    return roots


def _container_base(node):
    """The container name *node* evaluates to, through wrappers (#1953 r7)."""
    if isinstance(node, ast.Subscript):
        return _container_base(node.value)
    if isinstance(node, ast.Call):
        func = node.func
        if (isinstance(func, ast.Name) and func.id in _CONTAINER_WRAPPERS
                and len(node.args) == 1):
            return _container_base(node.args[0])
        if (isinstance(func, ast.Attribute) and func.attr == 'copy'
                and isinstance(func.value, ast.Name)):
            return func.value.id
        return None
    return node.id if isinstance(node, ast.Name) else None


def _container_callee(func):
    """The container name whose element *func* takes out and calls (#1953)."""
    if isinstance(func, ast.Subscript):
        return _container_base(func.value)
    if isinstance(func, ast.Call):
        func_id = func.func.id if isinstance(func.func, ast.Name) else None
        if func_id in _CONTAINER_WRAPPERS and len(func.args) == 1:
            return _container_base(func.args[0])
        if (isinstance(func.func, ast.Attribute) and func.func.attr == 'copy'
                and isinstance(func.func.value, ast.Name)):
            return func.func.value.id
    return None


def _unbounded_write(node, kinds):
    """True when a call writes a managed slot whose target cannot be named.

    ``p = functools.partial(setattr, md); p('common', <stub>)``,
    ``f = getattr(md, '__setattr__'); f('common', <stub>)`` and
    ``op.attrgetter('__setattr__')(md)('common', <stub>)`` keep a write handle
    the alias graph cannot name; the receiver then fails closed instead of
    being trusted (#892 r30/r32).

    #892 r32: ``setattr(*(md, 'common', <stub>))`` hides the target in a star
    argument and ``def f(*a, **kw): setattr(*a, **kw)`` forwards one, so
    neither call names its target; ``getattr(obj, <dynamic>)(...)`` may be any
    bound method at all (codex boundary r31 P0-6).
    """
    if not isinstance(node, ast.Call):
        return False
    func = node.func
    decoded = _write_call_target(func, kinds)
    found = _expr_kinds(func, kinds)
    # #892 r33: an unresolved attribute name can be ``__setattr__`` /
    # ``__setitem__``, but the write lands on the object the handle was taken
    # from -- the caller judges that one receiver instead of the whole file
    # (codex boundary r32 P1-1).
    managed = (decoded is not None or 'fn:partial' in found
               or 'fn:write-handle' in found)
    if not managed:
        return False
    if any(isinstance(argument, ast.Starred) for argument in node.args):
        return True
    if any(keyword.arg is None for keyword in node.keywords):
        return True
    return decoded is None


def _setattr_arguments(node, kinds):
    """``(target, key)`` a setattr/setitem spelling writes, else ``None`` (#892 r28).

    r27 resolved the builtin's aliases but still read only calls *named*
    ``setattr``; ``md.__setattr__('common', <stub>)``, ``object.__setattr__(md,
    'common', <stub>)``, ``type(md).__setattr__(md, 'common', <stub>)``,
    ``mapping.__setitem__('common', <stub>)`` and ``dict.__setitem__(mapping,
    'common', <stub>)`` are the same write.  The three-argument forms name the
    target first; the two-argument forms write their own receiver.
    """
    if not isinstance(node, ast.Call):
        return None
    decoded = _write_call_target(node.func, kinds)
    if decoded is None:
        return None
    _kind, receiver, bound = decoded
    args = list(bound) + list(node.args)
    # #892 r30: ``operator.setitem(mapping, key, value)`` is the same write as
    # the ``__setitem__`` spelling the class already read, and so is any
    # ``getattr(<obj>, '<managed>')`` / ``partial(<managed>, ...)`` call target
    # (GLM concurrency r29 P0-2/P0-3/P0-4, MiniMax boundary r29 P0-3).
    if len(args) == 3:
        return args[0], args[1]
    if len(args) == 2 and receiver is not None:
        return receiver, args[0]
    return None


# #892 r29: the unbound spelling of a mapping method names its target first
# (``dict.update(mapping, ...)``), and any helper module's ``setattr`` wrapper
# is the builtin's.  r28 read only the bound ``receiver.method(...)`` spelling.
_BUILTIN_TYPES = ('dict', 'type', 'object', 'list', 'set', 'str', 'bytes')
_MAPPING_METHODS = ('update', 'setdefault', 'pop', 'popitem', 'clear', '__ior__',
                    '__setitem__', 'setitem')


def _builtin_type_name(node, kinds):
    """The builtin type *node* names, else ``None`` (#892 r29)."""
    if isinstance(node, ast.Name):
        # #892 r30: ``from builtins import dict as _bd`` names the same type.
        for kind in kinds.get(node.id, ()):
            if kind.startswith('type:'):
                return kind.split(':', 1)[1]
        # A name the file rebinds is still read as the builtin: treating it as
        # "not the builtin" let ``dict = sys; dict.update(md.__dict__, ...)``
        # pass, and the conservative direction is a false FAIL (GLM sibling
        # r29 P1-4).
        return node.id if node.id in _BUILTIN_TYPES else None
    if isinstance(node, ast.Attribute) and node.attr in _BUILTIN_TYPES:
        if 'builtins' in _expr_kinds(node.value, kinds):
            return node.attr
    if isinstance(node, ast.Subscript):
        # #892 r32: ``__builtins__['dict'].update(md.__dict__, ...)`` names the
        # same type as the attribute spelling; the read side stayed one
        # spelling behind the r31 callee rule (GLM sibling r31 P1-2).
        literal = node.slice
        if (isinstance(literal, ast.Constant)
                and isinstance(literal.value, str)
                and literal.value in _BUILTIN_TYPES
                and 'builtins' in _expr_kinds(node.value, kinds)):
            return literal.value
    return None


def _mapping_method_call(node, kinds, tree):
    """``(target, keys)`` a mapping-method spelling writes, else ``None`` (#892 r29).

    ``dict.update(md.__dict__, common=<stub>)``,
    ``dict.setdefault(md.__dict__, 'common', <stub>)`` and
    ``dict.__setitem__(md.__dict__, 'common', <stub>)`` write the same mapping
    as the bound ``md.__dict__.update(...)`` the r28 rule reads.
    """
    if not (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)):
        return None
    attr = node.func.attr
    if attr not in _MAPPING_METHODS:
        return None
    receiver = node.func.value
    if attr == 'setitem' or _builtin_type_name(receiver, kinds) is not None:
        # #892 r30: ``operator.setitem(mapping, key, value)`` names its target
        # first, like the unbound ``dict.__setitem__`` form.
        if not node.args:
            return None
        target, args, keywords = node.args[0], node.args[1:], node.keywords
    else:
        target, args, keywords = receiver, node.args, node.keywords
    if attr == 'update':
        keys = {key for key, _value in _updated_items_parts(args, keywords)}
    elif attr in ('setdefault', 'pop', '__setitem__', 'setitem'):
        keys = _write_key_names(args[0], tree) if args else {'*'}
    else:
        keys = {'*'}
    return target, keys


def _dict_replacement_hits(value, shared):
    """Whether a dict literal assigned to a namespace can spell *shared*."""
    if (isinstance(value, ast.Dict)
            and all(isinstance(key, ast.Constant) and isinstance(key.value, str)
                    for key in value.keys)):
        return bool({key.value for key in value.keys} & set(shared))
    return True


def _updated_items(node):
    """``(key, value)`` pairs a dict-update call writes, ``('*', node)`` if open."""
    return _updated_items_parts(node.args, node.keywords)


def _updated_items_parts(args, keywords):
    """``(key, value)`` pairs an ``update`` writes, ``('*', None)`` if open."""
    items = []
    for argument in args:
        if not isinstance(argument, ast.Dict):
            return [('*', None)]
        for key, value in zip(argument.keys, argument.values):
            if not (isinstance(key, ast.Constant) and isinstance(key.value, str)):
                return [('*', None)]
            items.append((key.value, value))
    for keyword in keywords:
        if keyword.arg is None:
            return [('*', None)]
        items.append((keyword.arg, keyword.value))
    return items


def _updated_keys(node):
    """Keys a ``.update(...)`` call can add, ``{'*'}`` when unbounded (#892 r27)."""
    return {key for key, _value in _updated_items(node)}


def _setdefault_key(node):
    """Key a ``.setdefault`` call writes, ``{'*'}`` when unbounded (#892 r27)."""
    if (node.args and isinstance(node.args[0], ast.Constant)
            and isinstance(node.args[0].value, str)):
        return {node.args[0].value}
    return {'*'}


def _setattr_module_keys(node, kinds, tree):
    """Names a ``setattr`` spelling on a module object can replace (#892 r26).

    Only a module-object target is collected here: the apparatus itself calls
    ``setattr(md, name, value)`` with a non-literal key on a plain object, and
    that is a receiver attribute write, judged by ``_withdrawn_receivers``
    instead of withdrawing every name in the file.  #892 r28: the dunder
    spellings (``sys.modules[__name__].__setattr__(...)``,
    ``object.__setattr__(globals(), ...)``) go through the same resolver.
    """
    written = _setattr_arguments(node, kinds)
    if written is None:
        return set()
    target, key = written
    if not (_is_namespace_object(target, kinds) or _is_module_object(target, kinds)):
        return set()
    return _write_key_names(key, tree)


def _sys_aliases(tree):
    """Names that denote the ``sys`` module (#892 r25/r27)."""
    kinds = _object_kinds(tree)
    return frozenset(name for name, found in kinds.items() if 'sys' in found)


def _sys_modules_names(tree):
    """Names bound to the ``sys.modules`` dict itself (#892 r26/r27).

    ``from sys import modules as _m`` binds the dict, not the module, so
    ``_m['_guard'] = stub`` is a pre-seed with no ``.modules`` attribute to
    match (GLM concurrency r25 P1-4 / boundary r25 P1-6); ``sm = sys.modules``
    is the same binding through an alias (GLM boundary r26 P1-6).
    """
    kinds = _object_kinds(tree)
    return frozenset(name for name, found in kinds.items() if 'sysmods' in found)


def _sys_modules_write_values(tree, kinds=None):
    """``sys.modules['<literal>'] = value`` pairs (#892 r25).

    The apparatus bootstrap legitimately writes ``sys.modules['_guard'] =
    _guard`` after building the module, so the write alone is not poison.  The
    ``sys.modules.get`` arm must not trust a key that is also *assigned from
    something other than a proven module* -- that is exactly the
    ``sys.modules['_guard'] = <stub>`` pre-seed (GLM concurrency r24 P1-3).
    #892 r26: the dict can be reached through ``from sys import modules`` or an
    alias of ``sys``, and ``.update``/``.setdefault`` write it as a call (GLM
    boundary r25 P1-6 / concurrency r25 P1-4).
    """
    if kinds is None:
        kinds = _object_kinds(tree)
    writes = {}

    def record(key, value):
        writes.setdefault(key, []).append(value)

    def record_key(key, value, loops, scope):
        """Record the key a subscript write spells (#892 r27).

        A name is resolved through the ``for`` loop it runs in, not the whole
        file: the harness restores ``sys.modules[name] = previous`` inside
        ``for name, previous in (('common', saved), ...)``, and a file-wide
        lookup sees every unrelated ``name`` binding.

        #892 r30: the value is the *actual* right-hand side unless it is the
        loop's own value variable -- ``for name, previous in (...):
        sys.modules[name] = object()`` writes a stub, while
        ``sys.modules[name] = previous`` republishes the saved module (GLM
        concurrency r29 P0-7 / MiniMax concurrency r29 P0-5).  A key the loop
        body rebinds is unbounded, exactly as in the receiver rule.
        """
        if isinstance(key, ast.Constant) and isinstance(key.value, str):
            record(key.value, _resolve_scope_value(value, scope))
            return
        key_name = key.id if isinstance(key, ast.Name) else None
        if key_name is not None:
            for loop in reversed(loops):
                if _first_target_name(loop.target) == key_name:
                    if _rebound_in_loop(loop, key_name):
                        record('*', value)
                        return
                    found = _iterable_dict_keys(loop.iter, tree)
                    if found is None:
                        record('*', value)
                        return
                    items = _iterable_dict_items(loop.iter)
                    pair_value = None
                    if (isinstance(loop.target, ast.Tuple)
                            and len(loop.target.elts) > 1
                            and isinstance(loop.target.elts[1], ast.Name)):
                        pair_value = loop.target.elts[1].id
                    # #892 r32: a value variable rebound in the loop body no
                    # longer carries the pair's saved module, so the write is
                    # unknown, not a legal re-install -- the mirror of the key
                    # rule above (GLM boundary r31 P0-2).
                    if pair_value is not None and _rebound_in_loop(loop, pair_value):
                        record('*', value)
                        return
                    relayed = (items is not None
                               and isinstance(value, ast.Name)
                               and value.id == pair_value)
                    for spelling in found:
                        record(spelling, _resolve_scope_value(
                            value if not relayed
                            else items.get(spelling, value),
                            scope))
                    return
        record('*', value)

    def visit(node, loops, scope):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef,
                             ast.Lambda, ast.ClassDef)):
            scope = node
        if isinstance(node, (ast.Assign, ast.AnnAssign, ast.AugAssign)):
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            for target in targets:
                if (isinstance(node, ast.AugAssign)
                        and _expr_is_sys_modules(target, kinds)):
                    # #892 r32: ``sys.modules |= {'_guard': <stub>}`` updates
                    # the mapping in place; the key/value are not read from a
                    # literal pair table, so the write stays unknown (codex
                    # boundary r31 P0-10).
                    record('*', node.value)
                    continue
                if not isinstance(target, ast.Subscript):
                    continue
                if not _expr_is_sys_modules(target.value, kinds):
                    continue
                record_key(target.slice, node.value, loops, scope)
        elif isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
            attr = node.func.attr
            written = _mapping_method_call(node, kinds, tree)
            if written is not None and _expr_is_sys_modules(written[0], kinds):
                # #892 r30: ``dict.update(sys.modules, {...})`` /
                # ``operator.setitem(sys.modules, 'k', stub)`` name the mapping
                # first; r29 read only the bound spelling (GLM sibling r29
                # P1-6, MiniMax concurrency r29 P0-7).
                unbound = (attr == 'setitem'
                           or _builtin_type_name(node.func.value, kinds) is not None)
                args = node.args[1:] if unbound else node.args
                if attr == 'update':
                    for key, value in _updated_items_parts(args, node.keywords):
                        record(key, value)
                elif attr in ('setitem', '__setitem__', 'setdefault'):
                    if len(args) >= 2:
                        record_key(args[0], args[1], loops, scope)
                    else:
                        record('*', node)
                # ``pop`` / ``popitem`` / ``clear`` only remove entries; a
                # removal cannot install a stub, and recording it as an open
                # write fails every file closed (r30 first cut did exactly
                # that on the harness's own ``sys.modules.pop``).
            elif attr in ('update', 'setdefault') \
                    and _expr_is_sys_modules(node.func.value, kinds):
                if attr == 'setdefault':
                    if (len(node.args) >= 2
                            and isinstance(node.args[0], ast.Constant)
                            and isinstance(node.args[0].value, str)):
                        record(node.args[0].value, node.args[1])
                    else:
                        record('*', node)
                else:
                    for key, value in _updated_items(node):
                        record(key, value)
        elif isinstance(node, ast.Call):
            # #892 r32: ``put = operator.setitem; put(sys.modules, 'k', stub)``
            # is the same write as the attribute spelling the branch above
            # reads (codex boundary r31 P0-10).
            decoded = _write_call_target(node.func, kinds)
            if decoded is not None and decoded[0] == 'setitem':
                _kind, receiver, bound = decoded
                args = list(bound) + list(node.args)
                if len(args) >= 3:
                    if _expr_is_sys_modules(args[0], kinds):
                        record_key(args[1], args[2], loops, scope)
                elif len(args) == 2 and receiver is not None:
                    if _expr_is_sys_modules(receiver, kinds):
                        record_key(args[0], args[1], loops, scope)
                else:
                    record('*', node)
        for child in ast.iter_child_nodes(node):
            visit(child, loops + (node,) if isinstance(node, ast.For) else loops,
                  scope)

    visit(tree, (), tree)
    return writes


def _resolve_scope_value(value, scope):
    """*value*, or the single expression it is bound to in *scope* (#892 r27).

    ``saved = sys.modules.get('common')`` is how the harness saves an entry, so
    restoring from ``saved`` republishes that same module.  Only a binding that
    is unique inside the scope is substituted; anything ambiguous stays the
    bare name and the caller keeps failing closed.
    """
    if not isinstance(value, ast.Name):
        return value
    bindings = _scope_value_bindings(scope, value.id)
    if len(bindings) == 1 and isinstance(bindings[0], ast.Call):
        return bindings[0]
    return value


def _scope_value_bindings(scope, name):
    """Every value *name* is bound to directly in *scope* (#892 r27).

    Nested definitions are skipped: their bindings belong to another scope and
    must not make an outer name look resolved.
    """
    values = []
    stack = [scope]
    while stack:
        node = stack.pop()
        for child in ast.iter_child_nodes(node):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef,
                                  ast.ClassDef, ast.Lambda)):
                continue
            stack.append(child)
        if isinstance(node, ast.Assign):
            if any(name in _target_names(target) for target in node.targets):
                values.append(node.value)
        elif isinstance(node, ast.AnnAssign) and node.value is not None:
            if name in _target_names(node.target):
                values.append(node.value)
        elif isinstance(node, ast.NamedExpr):
            if name in _target_names(node.target):
                values.append(node.value)
        elif isinstance(node, (ast.AugAssign, ast.For, ast.AsyncFor)):
            target = node.target
            if name in _target_names(target):
                values.append(None)
        elif isinstance(node, (ast.With, ast.AsyncWith)):
            for item in node.items:
                if item.optional_vars is not None \
                        and name in _target_names(item.optional_vars):
                    values.append(None)
    return values


def _module_scope_binding_map(tree):
    """Every module-scope binding in *tree*: name -> list of values (#892 r23).

    Module-level statements are read through ``_own_nodes`` (which descends
    into ``if``/``try``/``for`` bodies but not into nested definitions), the
    module's top-level ``def``/``class`` names count as a ``None`` binding, and
    so does every binding that lands at module scope at runtime through a
    ``global name`` declaration inside a function -- that is exactly where a
    module binding is replaced behind the module walk's back.  ``None`` also
    marks non-expression bindings (imports, loop and ``with`` targets).
    """
    bindings = {}

    def add(name, value):
        bindings.setdefault(name, []).append(value)

    for node in _own_nodes(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef,
                             ast.ClassDef)):
            add(node.name, None)
            continue
        bound, value = _local_bound_names(node)
        for name in bound:
            add(name, value)
    for node in _module_level_defs(tree):
        add(node.name, None)
    # #892 r26: a module-scope walrus or ``match`` capture rebinds the name
    # outside every statement shape ``_local_bound_names`` knows, so it is
    # recorded as one more binding -- the name then no longer has the single
    # ``[None]`` binding a definition needs (GLM concurrency r25 P0-1).
    for name in _module_extra_bindings(tree):
        add(name, None)
    for function in ast.walk(tree):
        if not isinstance(function, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        global_names = {name for node in ast.walk(function)
                        if isinstance(node, ast.Global)
                        for name in node.names}
        if not global_names:
            continue
        for node in _own_nodes(function):
            bound, value = _local_bound_names(node)
            for name in bound:
                if name in global_names:
                    add(name, value)
        # #892 r33: a walrus on a ``global``-declared name rewrites the module
        # binding with no statement shape ``_local_bound_names`` reads, so
        # ``global md; (md := object())`` kept every ``md.common`` call trusted
        # (codex boundary r32 P0-1).
        for name in _walrus_names(function) & global_names:
            add(name, None)
    return bindings


def _module_imported_names(tree):
    """Module-scope names bound by an ``import`` statement (#892 r24).

    ``_module_scope_binding_map`` records an import as a bare ``None`` binding,
    the same marker it uses for a top-level ``def`` and for loop/``with``
    targets.  Only the import form proves the name denotes a real module, so the
    import names are collected here instead of being inferred from ``None``.
    #892 r26: an import inside ``if TYPE_CHECKING:`` never runs, so it binds
    nothing at runtime and must not seed the proven set (GLM concurrency r25
    P2-7).
    """
    return set(_module_import_binding_counts(tree))


def _module_import_binding_counts(tree):
    """name -> how many import statements bind it (#892 r26).

    ``sys`` may legitimately be imported twice; the binding map records one
    ``None`` per import statement, so "every binding is an import" has to count
    them instead of demanding a single one.  An import inside
    ``if TYPE_CHECKING:`` never runs, so it binds nothing at runtime and must
    not seed the proven set (GLM concurrency r25 P2-7).
    """
    counts = {}

    def visit(node):
        for child in ast.iter_child_nodes(node):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef,
                                  ast.ClassDef)):
                continue
            if isinstance(child, ast.If) and _is_type_checking_test(child.test):
                for statement in child.orelse:
                    visit(statement)
                continue
            if isinstance(child, (ast.Import, ast.ImportFrom)):
                for alias in child.names:
                    name = alias.asname or alias.name.split('.')[0]
                    counts[name] = counts.get(name, 0) + 1
                continue
            visit(child)

    visit(tree)
    return counts


def _module_import_sources(tree):
    """name -> the modules an import binds it from (#892 r27).

    ``_module_import_binding_counts`` proves a name *is* an import; this walk
    proves *which* module it comes from, so a registered cross-file source
    cannot be imported from a stub module of the same name (DeepSeek sibling
    r26 P1-2).
    """
    sources = {}

    def visit(node):
        for child in ast.iter_child_nodes(node):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef,
                                  ast.ClassDef)):
                continue
            if isinstance(child, ast.If) and _is_type_checking_test(child.test):
                for statement in child.orelse:
                    visit(statement)
                continue
            if isinstance(child, (ast.Import, ast.ImportFrom)):
                for alias in child.names:
                    name = alias.asname or alias.name.split('.')[0]
                    if isinstance(child, ast.ImportFrom):
                        module = child.module or ''
                    else:
                        module = alias.name
                    sources.setdefault(name, set()).add(module)
                continue
            visit(child)

    visit(tree)
    return sources


def _is_type_checking_test(test):
    """True for the ``TYPE_CHECKING`` guard spelling."""
    return ((isinstance(test, ast.Name) and test.id == 'TYPE_CHECKING')
            or (isinstance(test, ast.Attribute) and test.attr == 'TYPE_CHECKING'))


def _module_load_call(value, proven, sys_poisoned=frozenset(), kinds=None):
    """True when *value* is a call that returns a loaded module (#892 r24).

    The apparatus bootstrap binds ``_guard`` through
    ``sys.modules.get('_guard')`` / ``importlib.util.module_from_spec(_spec)``.
    The call is accepted only when the name it hangs off is itself proven
    (an import or another proven module load), so ``_guard = stub.fake_load()``
    does not hand ``_guard.load`` a loader role; ``importlib.import_module`` is
    the one import-by-name spelling.
    """
    if not isinstance(value, ast.Call) or not isinstance(value.func, ast.Attribute):
        return False
    func = value.func
    if func.attr in ('module_from_spec', 'import_module'):
        return _root_name(func.value) in proven
    if func.attr == 'get':
        base = func.value
        if kinds is None:
            kinds = {}
        return (isinstance(base, ast.Attribute) and base.attr == 'modules'
                and isinstance(base.value, ast.Name)
                # #892 r26/r27: read side resolves through the same alias map
                # the write side uses, so ``import sys as _s; _s.modules.get(
                # '_guard')`` is not a legal spelling that fails closed for no
                # reason (GLM sibling r25 #8).
                and 'sys' in kinds.get(base.value.id, ())
                and base.value.id in proven
                # #892 r25: the key must be a literal, so the binding names the
                # module it loads instead of ``sys.modules.get(<anything>)``
                # (GLM boundary r24 P2-5).
                and bool(value.args)
                and isinstance(value.args[0], ast.Constant)
                and isinstance(value.args[0].value, str)
                # #892 r25: a key pre-seeded with a non-name value is a stub
                # install, not the module the bootstrap published.
                and value.args[0].value not in sys_poisoned)
    return False


def _root_name(node):
    """Leftmost ``Name`` of an attribute chain, or ``None``."""
    while isinstance(node, ast.Attribute):
        node = node.value
    return node.id if isinstance(node, ast.Name) else None


def _proven_module_names(tree):
    """Module-scope names whose every binding loads a real module (#892 r24).

    An imported name is proven outright; a name assigned from a module-load call
    joins once the name that call hangs off is proven, computed as a fixpoint so
    ``_guard = importlib.util.module_from_spec(...)`` is accepted while
    ``_guard = SimpleNamespace(load=...)`` is not.  Anything unproven stays out,
    and ``_loader_names`` fails closed on it.
    """
    if not isinstance(tree, ast.Module):
        return frozenset()
    if _dynamic_write_names(tree) & {'*'}:
        # Same fail-closed rule as ``_proven_source_names``: a non-literal
        # dynamic write can move any binding.
        return frozenset()
    bindings = _module_scope_binding_map(tree)
    # #892 r26: an imported name is only a module while *every* binding of it
    # loads a module.  ``import sys`` then ``sys = SimpleNamespace(...)``, a
    # ``try: import x / except: x = stub`` fallback and a walrus rebind all left
    # the name in the seed set that never gets re-checked (GLM sibling r25 #1 /
    # concurrency r25 P1-3 / simplify r25 P1-1).
    imports = _module_import_binding_counts(tree)
    imported = {name for name, count in imports.items()
                if bindings.get(name) == [None] * count}
    imported -= _module_extra_bindings(tree)
    kinds = _object_kinds(tree)
    writes = _sys_modules_write_values(tree, kinds)
    if any(key == '*' for key in writes):
        # #892 r27: a non-literal key can pre-seed any entry, so the sentinel
        # is a real verdict instead of dead data (GLM boundary r26 P0-3 /
        # DeepSeek sibling r26 P1-3).
        return frozenset()
    # A Name is not automatically the module the bootstrap published --
    # ``stub = SimpleNamespace(...); sys.modules['_guard'] = stub`` is a stub
    # one assignment away (GLM simplify r25 P1-3).  #892 r27: the poisoning
    # pass is a fixpoint, because a key pre-seeded from another pre-seeded
    # key's *read* laundered itself in one pass (GLM concurrency r26 P1-1).
    poisoned = frozenset()
    while True:
        proven = _proven_modules_from(tree, bindings, imported, poisoned, kinds)
        grown = frozenset(
            key for key, values in writes.items()
            if not all((isinstance(value, ast.Name) and value.id in proven)
                       or _module_load_call(value, proven, poisoned, kinds)
                       for value in values))
        if grown <= poisoned:
            return frozenset(proven)
        poisoned |= grown


def _proven_modules_from(tree, bindings, imported, sys_poisoned, kinds=None):
    """Fixpoint of module-load bindings given the poisoned ``sys.modules`` keys."""
    proven = {name for name in imported if name not in sys_poisoned}
    changed = True
    while changed:
        changed = False
        for name, values in bindings.items():
            if name in proven or not values:
                continue
            if all(_module_load_call(value, proven, sys_poisoned, kinds)
                   for value in values):
                proven.add(name)
                changed = True
    return proven


def _leaf_targets(targets):
    """Every leaf of a (possibly tuple/nested) assignment target (#892 r30)."""
    leaves = []
    for target in targets:
        if isinstance(target, (ast.Tuple, ast.List)):
            leaves.extend(_leaf_targets(target.elts))
        elif isinstance(target, ast.Starred):
            leaves.extend(_leaf_targets([target.value]))
        else:
            leaves.append(target)
    return leaves


def _walk_scoped(tree):
    """``(scope, node)`` for every node, *scope* being the enclosing function.

    #892 r35: a decorator, a default value and an annotation are evaluated
    where the ``def`` sits, not inside the function they belong to.  Treating
    the decorator expression as part of the decorated body made a nested
    lambda there resolve its capture against the inner function's unrelated
    local (``x = object()``) and drop the ``setattr`` written through ``x =
    md`` (codex boundary r34 P0-3).
    """
    stack = [(None, tree)]
    while stack:
        scope, node = stack.pop()
        yield scope, node
        inner = node if isinstance(
            node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)) else scope
        header = set()
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            # ``args`` carries both defaults and parameter annotations; the
            # whole node belongs to the enclosing scope.
            header = {id(item) for item in node.decorator_list}
            header.add(id(node.args))
            if node.returns is not None:
                header.add(id(node.returns))
        elif isinstance(node, ast.Lambda):
            header = {id(node.args)}
        for child in ast.iter_child_nodes(node):
            stack.append((scope if id(child) in header else inner, child))


def _scope_parents(tree):
    """function node -> the function that encloses it (``None`` at module)."""
    return {node: scope for scope, node in _walk_scoped(tree)
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda))}


def _scoped_alias_assignments(tree):
    """``(scope, name, value, value_scope)`` a binding gives a bare name (#892 r33).

    *scope* is the enclosing function, ``None`` at module scope; a ``global``
    declaration lands at module scope and a ``nonlocal`` one in the enclosing
    function.  The file-wide table merged ``_x = md`` in one function with
    ``_x = <stub>`` in another, so an unrelated helper withdrew a clean file's
    relay (codex boundary r32 P1-2).  #892 r34: the *binding* scope and the
    scope the right-hand side is evaluated in differ -- a ``global`` hand-off
    reads the function's own locals (``global g; y = md; g = y``) and a
    parameter default is evaluated where the ``def`` sits
    (``def inner(x=x)``), so both are carried separately (codex boundary r33
    P0-2).
    """
    parents = _scope_parents(tree)
    declared = {}
    for scope, node in _walk_scoped(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
            continue
        globals_, nonlocals_ = set(), set()
        for child in ast.walk(node):
            if isinstance(child, ast.Global):
                globals_.update(child.names)
            elif isinstance(child, ast.Nonlocal):
                nonlocals_.update(child.names)
        declared[node] = (globals_, nonlocals_)

    def binding_scope(scope, name):
        while scope is not None:
            globals_, nonlocals_ = declared.get(scope, ((), ()))
            if name in globals_:
                return None
            if name in nonlocals_:
                scope = parents.get(scope)
                continue
            return scope
        return None

    def add(scope, target, value):
        for name in _target_names(target):
            pairs.append((binding_scope(scope, name), name, value, scope))

    pairs = []
    simple = _simple_bindings(tree)
    binding_scopes = {id(item): owner for owner, item in _walk_scoped(tree)}

    def visible(scope, before):
        chain = []
        level = scope
        while True:
            chain.append(level)
            if level is None:
                break
            level = parents.get(level)
        return _visible_binding_table(simple, binding_scopes, chain, before,
                                      _certain_value_ids(tree))

    for scope, node in _walk_scoped(tree):
        if isinstance(node, ast.Assign):
            for target in node.targets:
                for name, value in _target_value_pairs(target, node.value, simple):
                    pairs.append((binding_scope(scope, name), name, value, scope))
        elif isinstance(node, ast.AnnAssign) and node.value is not None:
            add(scope, node.target, node.value)
        elif isinstance(node, ast.NamedExpr) and isinstance(node.target, ast.Name):
            pairs.append((binding_scope(scope, node.target.id), node.target.id,
                          node.value, scope))
        elif isinstance(node, (ast.For, ast.AsyncFor)):
            elements = _bound_literal_elements(
                node.iter, visible(scope, node.lineno),
                shadowed=_shadowed_parameters(scope))
            for element in elements:
                for name, value in _target_value_pairs(node.target, element, simple):
                    pairs.append((binding_scope(scope, name), name, value, scope))
            if not elements:
                # #892 r34: an iterable with no static shape still binds the
                # loop target *in this scope*, shadowing a module-level name of
                # the same spelling.  Without the shadow a local loop over
                # unrelated objects resolved to the outer receiver and withdrew
                # it (codex boundary r33 P1-9).
                for name in _target_names(node.target):
                    pairs.append((binding_scope(scope, name), name,
                                  ast.Constant(value=None), scope))
        elif isinstance(node, ast.Match):
            # #1953 fix-r1 round 2 (GLM sibling r1b P1): ``match md: case h:``
            # binds the capture to (a part of) the subject, but this is the
            # table the *receiver* rules read -- the same branch added to
            # ``_alias_assignments`` alone left ``h.common = <stub>`` trusted.
            for name in _match_capture_names(node):
                pairs.append((binding_scope(scope, name), name, node.subject,
                              scope))
        elif isinstance(node, (ast.With, ast.AsyncWith)):
            for item in node.items:
                if item.optional_vars is None:
                    continue
                argument = _context_argument(item.context_expr, simple)
                context = argument if argument is not None else item.context_expr
                for name, value in _target_value_pairs(item.optional_vars, context, simple):
                    pairs.append((binding_scope(scope, name), name, value, scope))
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
            # Defaults are evaluated in the enclosing scope but land in the
            # parameter, so the binding is scoped to the function itself and
            # the value to where the ``def`` sits (#892 r34).
            parent = parents.get(node)
            arguments = node.args
            positional = list(arguments.posonlyargs) + list(arguments.args)
            defaults = list(arguments.defaults)
            for argument, default in zip(positional[len(positional) - len(defaults):],
                                         defaults):
                pairs.append((node, argument.arg, default, parent))
            for argument, default in zip(arguments.kwonlyargs, arguments.kw_defaults):
                if default is not None:
                    pairs.append((node, argument.arg, default, parent))
    return pairs


def _loop_target_used_as_handle(body, name):
    """True when ``name`` is called or written through inside *body* (#1953 r3).

    ``for h in hs:`` only hands the container's elements to ``h``; the handles
    are used where ``h`` is *called* or where a mapping/attribute is stored
    through it.  Iterating alone (``for h in hs: print(h)``) is a pure read, so
    withdrawing on the iteration made clean code fail (codex r2 P1).
    """
    for node in body:
        for child in ast.walk(node):
            if (isinstance(child, ast.Call) and isinstance(child.func, ast.Name)
                    and child.func.id == name):
                return True
            if (isinstance(child, (ast.Subscript, ast.Attribute))
                    and isinstance(child.value, ast.Name)
                    and child.value.id == name
                    and isinstance(getattr(child, 'ctx', None), ast.Store)):
                return True
    return False


#: Calls that only *read* their argument.  Handing a write handle to one of
#: them does not let it reach the receiver, so the after-loop scan stays quiet
#: (``for _h in _hs: ...`` followed by ``print(_h)`` is a pure read).  Anything
#: else -- ``nullcontext(h)``, ``Exception(h)``, a user helper -- is treated as
#: a hand-over: enumerating the escaping calls is what let five separate
#: escapes through in r7 (codex r4 P0), so the *safe* names are listed instead.
_READ_ONLY_CALLS = frozenset((
    'print', 'len', 'repr', 'bool', 'id', 'hash', 'ascii', 'type',
    'isinstance', 'issubclass', 'format',
))


def _argument_names(arguments):
    """Parameter names of an ``ast.arguments`` node (#1953 r8)."""
    names = [arg.arg for arg in list(getattr(arguments, 'posonlyargs', ()))
             + list(arguments.args) + list(arguments.kwonlyargs)]
    if arguments.vararg is not None:
        names.append(arguments.vararg.arg)
    if arguments.kwarg is not None:
        names.append(arguments.kwarg.arg)
    return names


def _class_body_statements(tree):
    """Ids of the statements executed in a class body (#1953 fix-r1 round 9).

    A class body runs in its own namespace: ``class C: print = 1`` does not
    shadow the enclosing function's ``print`` (codex r5 P1-7), so the names it
    binds must not be read as the enclosing scope's locals.
    """
    ids = set()

    def visit(statements):
        for statement in statements:
            if id(statement) in ids:
                continue
            ids.add(id(statement))
            for block in _statement_blocks(statement):
                visit(block)
            if isinstance(statement, ast.ClassDef):
                visit(statement.body)

    for node in ast.walk(tree):
        if isinstance(node, ast.ClassDef):
            visit(node.body)
    return ids


def _class_local_names(class_def):
    """Names a class body binds in its own namespace (#1953 fix-r1 round 10).

    ``class C: print = <writer>`` shadows the builtin for the rest of that
    class body only -- it neither shadows the enclosing scope nor leaks out of
    the class (codex r6 P0-5).  Nested function and class bodies are their own
    scopes and are not entered.
    """
    names = set()

    def visit(statements):
        for statement in statements:
            if isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef,
                                      ast.ClassDef)):
                names.add(statement.name)
                continue
            if isinstance(statement, ast.stmt):
                bound, _ = _local_bound_names(statement)
                names.update(bound)
                for block in _statement_blocks(statement):
                    visit(block)

    visit(class_def.body)
    return names


def _scope_local_names(tree, owner):
    """Names bound directly in the scope *owner* (``None`` = module).

    Nested scopes are a different scope and stay out: a nested ``def`` is seen
    at *owner* (it binds its own name there), but its body is not.

    #1953 fix-r1 round 9 (codex r5 P0-4 / P1-7): the *parameters* of a nested
    ``def``/``lambda`` belong to that nested scope, so listing them here made
    ``def inner(): def unused(h): pass; h('common', <stub>)`` look like a
    shadowed name -- the closure escape walked straight through.  The owner's
    own parameters, by contrast, really are its locals.
    """
    names = set()
    if isinstance(owner, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
        names.update(_argument_names(owner.args))
    class_body = _class_body_statements(tree)
    for scope, node in _walk_scoped(tree):
        if scope is not owner:
            continue
        # A ``def``/``class`` written in a class body binds its name in the
        # *class* namespace, not in the enclosing function: the guard has to
        # come before the definition branch or ``class C: def _h(): ...``
        # made ``_h`` look like a local of the enclosing function and the
        # closure escape walked straight through it (GLM boundary r6 P0-3).
        if id(node) in class_body:
            continue
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef,
                             ast.ClassDef, ast.Lambda)):
            if not isinstance(node, ast.Lambda):
                names.add(node.name)
            continue
        if isinstance(node, ast.NamedExpr):
            names.update(_target_names(node.target))
        elif isinstance(node, ast.match_case):
            names.update(_match_capture_names(node.pattern))
        elif isinstance(node, ast.stmt):
            bound, _ = _local_bound_names(node)
            names.update(bound)
    return names


def _nested_scope_uses(node, tracked, outer_bound=()):
    """True when a nested ``def``/``lambda`` reads a tracked name it does not bind.

    #1953 fix-r1 round 8 (codex r4 P0-2 / GLM boundary r4 P0): r6 skipped every
    nested scope on the theory that its ``h`` is another object.  That holds
    only when the nested scope *binds* the name -- a closure reading the outer
    loop variable holds the very same handle, so ``def _inner(): _h(...)``
    followed by ``_inner()`` writes the receiver.  ``def inner(_h): _h(...)``
    stays clean because the parameter shadows the outer name.

    The body goes through the same safe-position walk as the enclosing scope,
    so ``def _inner(): print(_h)`` is still only a read.

    #1953 fix-r1 round 9 (GLM boundary r5 P1-2): the read-only call table is
    checked against *both* binding sets.  ``print = _sink`` at module level and
    ``lambda: print(_h)`` inside a function must not treat ``print`` as the
    builtin just because the lambda does not bind it.
    """
    if isinstance(node, ast.Lambda):
        bound = set(_argument_names(node.args))
        body = [ast.Expr(value=node.body)]
        # A default evaluates where the lambda is *created*, not in the
        # lambda's own scope: ``lambda _x=_h('common', <stub>): None`` writes
        # right there (GLM boundary r6 P0-2).  The parameters do not shadow
        # that position, so the outer bound set is the one to check against.
        for default in node.args.defaults:
            if _handle_use(default, tracked, set(outer_bound), set()):
                return True
        for default in node.args.kw_defaults:
            if default is not None and _handle_use(
                    default, tracked, set(outer_bound), set()):
                return True
    else:
        bound = set(_argument_names(node.args)) | _scope_local_names(node, node)
        body = node.body
    scope_bound = set(outer_bound) | bound
    free = tracked - bound
    if not free:
        return False
    return _statements_use(body, free, scope_bound)


def _statements_use(statements, tracked, scope_bound):
    """True when any statement in the block, or in a nested block, uses a handle.

    #1953 fix-r1 round 14 (GLM sibling r10 P1-1): ``_f = _h`` inside a nested
    function hands the handle to ``_f`` exactly as it does in a flat scope, so
    the aliases a statement produces stay tracked for the statements after it.
    Dropping them left ``def _g(): _f = _h; _f('common', <stub>)`` green -- the
    third copy of the same miss, next to the flat scan and the class body.
    """
    used, _aliases = _statements_scan(statements, tracked, scope_bound)
    return used


def _statements_scan(statements, tracked, scope_bound):
    """``(used, aliases)`` for one statement list inside a nested scope (r14).

    A block that may not run still hands its aliases to the statements after
    it -- fail-closed, the same rule the class body and the flat scanner use.
    """
    live = set(tracked)
    produced = set()
    for statement in statements:
        used, aliases, fresh = _handle_use_in_statement(
            statement, live, scope_bound)
        if used:
            return True, set()
        live |= aliases
        produced |= aliases
        # #1953 fix-r1 round 15 (codex r14 P1): a name certainly rebound to a
        # value that holds no handle stops being an alias -- ``_f = _h`` then
        # ``_f = lambda *a: None`` then ``_f('common', <stub>)`` calls the
        # fresh object.  A conditional rebinding is not in *fresh* and keeps
        # the alias, exactly like the flat scanner.
        live -= fresh
        produced -= fresh
        if isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef,
                                  ast.ClassDef)):
            live.discard(statement.name)
            produced.discard(statement.name)
        for block in _statement_blocks(statement):
            block_used, inner = _statements_scan(block, live, scope_bound)
            if block_used:
                return True, set()
            live |= inner
            produced |= inner
    return False, produced


_FRESH_CALLS = frozenset((
    'object', 'list', 'dict', 'set', 'tuple', 'frozenset', 'str', 'bytes',
    'bytearray', 'int', 'float', 'complex', 'bool', 'range', 'enumerate',
))


def _fresh_value(node, scope_bound=()):
    """True when *node* provably holds no write handle (#1953 r5).

    A literal, a comprehension result or a lambda is fresh.  A call to a
    builtin constructor is fresh too: ``h = object(); h()`` is a new object,
    not the handle the loop just handed over (codex r3 caught the first
    version withdrawing it).

    #1953 fix-r1 round 8 (codex r4 P0-3): only when the name really still is
    the builtin.  ``object = lambda: hs[0]; h = object()`` hands the handle
    straight back, so a spelling the scope rebinds is not fresh.

    Round 9 (GLM boundary r5 P1-3): the rebinding may sit in *any* enclosing
    scope.  A module-level ``object = lambda: _hs[0]`` shadowed the builtin
    for a loop inside a function just as well, so the caller passes the whole
    enclosing chain (``_enclosing_bound_names``), not the innermost scope.
    """
    if isinstance(node, ast.Call):
        return (isinstance(node.func, ast.Name)
                and node.func.id in _FRESH_CALLS
                and node.func.id not in scope_bound)
    return isinstance(node, (ast.Constant, ast.List, ast.Tuple, ast.Set,
                             ast.Dict, ast.ListComp, ast.SetComp, ast.DictComp,
                             ast.GeneratorExp, ast.Lambda, ast.JoinedStr))


def _statically_true(node):
    """True for a literal that is always truthy (#1953 r8, codex r4 P1)."""
    return isinstance(node, ast.Constant) and bool(node.value)


def _statically_false(node):
    """True for a literal that is always falsy (#1953 r7, codex r3 P1)."""
    return isinstance(node, ast.Constant) and not node.value


def _assignment_effect(statement, tracked, scope_bound):
    """``(aliases, fresh, copies)`` an assignment introduces (#1953 r9).

    ``_f = _h`` does not hand the handle anywhere by itself -- the copy is
    followed instead, so ``_f = _h`` + ``print(_f)`` stays a read while
    ``_f = _h`` + ``_f('common', <stub>)`` is the write (GLM boundary r3 P1).
    Chained targets (``_f = _g = _h``) alias every leaf (GLM boundary r4 P2).
    Only plain-name targets qualify: ``d[0] = _h`` stores the handle into a
    container and is a hand-over, judged by the ordinary expression walk.

    A target bound to a provably fresh value stops holding the handle
    (``_h = object()``) -- but only when the value really is fresh
    (:func:`_fresh_value`).

    #1953 fix-r1 round 11 (codex r7 P0-3): ``copies`` holds the *elements* of
    the right-hand side that are pure copies of the handle.  Round 10 skipped
    the whole right-hand side once one alias turned up, so
    ``f, x = h, [h('common', <stub>)]`` lost the call that sits in the other
    element.  Only the copy itself may be skipped; every other element is
    still an ordinary expression.
    """
    if isinstance(statement, ast.Assign):
        targets, value = statement.targets, statement.value
    elif isinstance(statement, ast.AnnAssign):
        targets, value = [statement.target], statement.value
    else:
        return set(), set(), set()
    if value is None:
        return set(), set(), set()
    if len(targets) == 1 and isinstance(targets[0], (ast.Tuple, ast.List)):
        # ``_f, _g = _h, _h`` copies the handle into two names; the positional
        # pairing keeps them aliases instead of reading the bare names in the
        # right-hand tuple as an immediate hand-over (GLM boundary r6 P2-6).
        pairs = _target_value_pairs(targets[0], value)
        element_wise = True
    else:
        names = [target.id for target in targets if isinstance(target, ast.Name)]
        if len(names) != len(targets):
            return set(), set(), set()
        pairs = [(name, value) for name in names]
        element_wise = False
    aliases = set()
    fresh = set()
    copies = set()
    # #1953 fix-r1 round 16 (codex r15 P0-1): a duplicated target is written
    # twice and the *last* element decides.  ``_f, _f = None, _h`` aliases the
    # handle even though the earlier element is fresh; the one-pass version put
    # the name in both sets and the caller's ``live -= fresh`` dropped it.
    last_write = {}
    for index, (name, _item) in enumerate(pairs):
        last_write[name] = index
    for index, (name, item) in enumerate(pairs):
        if isinstance(item, ast.Name) and item.id in tracked:
            # #1953 fix-r1 round 17 (boundary r16 P1-3): the *final* binding
            # decides whether the name still aliases the handle, but a
            # duplicated target's earlier element is still a pure copy on the
            # right-hand side -- ``_f, _f = _h, None`` only reads ``_h``.  The
            # last-write skip used to drop this registration with the alias,
            # and the bare read was then judged a hand-over (false FAIL).
            copies.add(id(item))
        if last_write[name] != index:
            continue
        if isinstance(item, ast.Name):
            if item.id in tracked:
                aliases.add(name)
            continue
        if _fresh_value(item, scope_bound):
            fresh.add(name)
            continue
        # Anything else keeps the conservative answer: no alias, no fresh, and
        # the right-hand side is walked as the ordinary expression it is.
        return set(), set(), set()
    if aliases and not element_wise:
        # ``_f = _h``: the single right-hand side *is* the copy.
        copies = {id(value)}
    return aliases, fresh, copies


# Attributes whose value still is the handle (or the object behind it), so a
# load of one is a hand-over rather than a read (#1953 fix-r1 round 10, GLM
# boundary r6 P1-4).  ``__name__``, ``__doc__`` and the other metadata dunders
# stay reads.
_HANDLE_FORWARD_ATTRIBUTES = frozenset(('__call__', '__self__', '__func__'))


def _handle_use(node, tracked, scope_bound, aliases, read_only=False):
    """True when *node* hands a tracked handle somewhere it can be written from.

    #1953 fix-r1 round 8 (codex r4): r7 enumerated the *escaping* spellings
    (a call, a store, a ``return``) and four more turned up in review.  This
    inverts the test -- a tracked name is a use unless it sits in one of the
    few positions that provably only read it: a comparison / boolean / unary
    operand, an argument to a read-only builtin, or an alias assignment.  A
    nested ``def``/``lambda`` that does not bind the name counts as a use.
    Anything unclassified fails closed, per the ruler's rule for every other
    rule in this file.

    ``read_only`` is set while walking such a position, so a bare name there
    is a read (``return _h is None``) while a compound expression is still
    inspected (``_h() == 1``, ``_h[0] is None``).

    #1953 fix-r1 round 9 (codex r5 P0-1/P0-2/P0-3/P1-5/P1-6):

    * an assignment's right-hand side is walked *before* the alias bookkeeping,
      so ``x = [_h('common', <stub>)]`` is the write it looks like;
    * ``read_only`` stops at a call whose callee is not a read-only builtin --
      ``print(helper(_h))`` hands the handle to ``helper``;
    * a bare name in a truth test (``if _h:``) is a read, an attribute *load*
      (``_h.__name__``) is a read of the handle, and ``del _h`` ends the name
      instead of using it.
    """
    if isinstance(node, ast.Name):
        if isinstance(node.ctx, (ast.Store, ast.Del)):
            return False
        return node.id in tracked and not read_only
    if isinstance(node, (ast.Lambda, ast.FunctionDef, ast.AsyncFunctionDef)):
        return _nested_scope_uses(node, tracked, scope_bound)
    if isinstance(node, ast.Attribute):
        # ``_h.common = <stub>`` writes *through* the handle; ``name =
        # _h.__name__`` only reads the bound method (codex r5 P1-6).  A load
        # that still *is* the handle can never count as a read: ``__call__``
        # invokes the write (codex r6 P0-1), ``__self__`` is the bound object
        # itself and ``__func__`` the plain function, so both hand the write
        # on (GLM boundary r6 P1-4).
        if isinstance(node.ctx, (ast.Store, ast.Del)):
            return _handle_use(node.value, tracked, scope_bound, aliases, False)
        if node.attr in _HANDLE_FORWARD_ATTRIBUTES:
            return _handle_use(node.value, tracked, scope_bound, aliases, False)
        return _handle_use(node.value, tracked, scope_bound, aliases, True)
    if isinstance(node, ast.NamedExpr):
        if (isinstance(node.target, ast.Name) and isinstance(node.value, ast.Name)
                and node.value.id in tracked):
            aliases.add(node.target.id)
            return False
        return _handle_use(node.value, tracked, scope_bound, aliases, read_only)
    if isinstance(node, ast.Compare):
        return (_handle_use(node.left, tracked, scope_bound, aliases, True)
                or any(_handle_use(item, tracked, scope_bound, aliases, True)
                       for item in node.comparators))
    if isinstance(node, ast.BoolOp):
        return any(_handle_use(item, tracked, scope_bound, aliases, True)
                   for item in node.values)
    if isinstance(node, ast.UnaryOp):
        return _handle_use(node.operand, tracked, scope_bound, aliases, True)
    if isinstance(node, ast.IfExp):
        return (_handle_use(node.test, tracked, scope_bound, aliases, True)
                or _handle_use(node.body, tracked, scope_bound, aliases, read_only)
                or _handle_use(node.orelse, tracked, scope_bound, aliases, read_only))
    if isinstance(node, ast.Expr):
        return _handle_use(node.value, tracked, scope_bound, aliases, read_only)
    if isinstance(node, ast.Call):
        callee = node.func
        pure = (isinstance(callee, ast.Name)
                and callee.id in _READ_ONLY_CALLS
                and callee.id not in scope_bound)
        # The callee position is never a read: ``print(_h())`` calls the
        # handle.  And ``read_only`` must not survive into the arguments of a
        # call that is not read-only -- ``print(helper(_h))`` hands the handle
        # to ``helper`` (codex r5 P0-3 / GLM boundary r5 P0-1).
        if isinstance(callee, ast.Attribute):
            # ``_h.anything(<stub>)`` dispatches through the handle, so the
            # attribute's receiver is a callee, not a read (codex r6 P0-1).
            if _handle_use(callee.value, tracked, scope_bound, aliases, False):
                return True
        elif _handle_use(callee, tracked, scope_bound, aliases, False):
            return True
        for argument in node.args:
            if _handle_use(argument, tracked, scope_bound, aliases, pure):
                return True
        for keyword in node.keywords:
            if _handle_use(keyword.value, tracked, scope_bound, aliases, pure):
                return True
        return False
    if isinstance(node, (ast.ListComp, ast.SetComp, ast.GeneratorExp,
                         ast.DictComp)):
        return _comprehension_handle_use(node, tracked, scope_bound, aliases)
    return any(_handle_use(child, tracked, scope_bound, aliases, read_only)
               for child in ast.iter_child_nodes(node)
               if not isinstance(child, ast.stmt))


def _comprehension_handle_use(node, tracked, scope_bound, aliases):
    """Whether a comprehension hands a tracked handle somewhere (#1953 r10).

    Round 9 skipped the whole comprehension once a filter could never hold,
    but only what runs *after* that filter is dead: the iterable and the
    filters before it are evaluated, and
    ``[m for m in (md,) if setattr(m, 'common', <stub>) if False]`` still
    writes the receiver (codex r6 P0-2).

    The comprehension's own targets live in its own scope, so a target of the
    tracked name shadows the outer handle in the element and in the filters
    (``[h('common', <stub>) for h in (print,)]`` calls ``print``, codex r6
    P1-7); the first iterable is evaluated before that binding exists and is
    still inspected.  Anything else fails closed, as everywhere in this file.

    #1953 fix-r1 round 11 (codex r7 P0-4): only the shadowed *names* leave the
    tracked set.  Round 10 dropped every name as soon as one target shadowed
    one of them, so ``f = h; [f('common', <stub>) for h in (None,)]`` looked
    clean even though ``f`` still is the handle.
    """
    targets = set()
    for generator in node.generators:
        active = tracked - targets
        if active and _handle_use(generator.iter, active, scope_bound,
                                  aliases, False):
            return True
        targets |= set(_target_names(generator.target))
        stopped = False
        for condition in generator.ifs:
            active = tracked - targets
            if active and _handle_use(condition, active, scope_bound,
                                      aliases, True):
                return True
            if _statically_false(condition):
                stopped = True
                break
        if stopped:
            return False
    active = tracked - targets
    if not active:
        return False
    parts = ([node.key, node.value] if isinstance(node, ast.DictComp)
             else [node.elt])
    return any(_handle_use(part, active, scope_bound, aliases, False)
               for part in parts)


def _definition_header(statement):
    """Expressions a ``def``/``class`` evaluates in the enclosing scope."""
    header = list(getattr(statement, 'decorator_list', ()))
    arguments = getattr(statement, 'args', None)
    if arguments is not None:
        header.extend(arguments.defaults)
        header.extend(item for item in arguments.kw_defaults if item is not None)
        for argument in (list(getattr(arguments, 'posonlyargs', ()))
                         + list(arguments.args) + list(arguments.kwonlyargs)
                         + [item for item in (arguments.vararg, arguments.kwarg)
                            if item is not None]):
            if argument.annotation is not None:
                header.append(argument.annotation)
    if getattr(statement, 'returns', None) is not None:
        header.append(statement.returns)
    header.extend(getattr(statement, 'bases', ()))
    header.extend(keyword.value for keyword in getattr(statement, 'keywords', ()))
    return header


def _non_copy_use(node, copies, tracked, scope_bound, aliases):
    """True when a value expression hands on anything but the known copies.

    ``copies`` holds the ids of the right-hand-side nodes that are pure
    copies, at any depth of a literal unpack (``f, (g, x) = h, (h, None)``).
    Everything else in the expression is still judged as an ordinary
    expression (#1953 fix-r1 round 12, codex r8 P1-5).
    """
    if id(node) in copies:
        return False
    if isinstance(node, (ast.Tuple, ast.List)):
        for element in node.elts:
            inner = element.value if isinstance(element, ast.Starred) else element
            if _non_copy_use(inner, copies, tracked, scope_bound, aliases):
                return True
        return False
    return _handle_use(node, tracked, scope_bound, aliases)


def _handle_use_in_statement(statement, tracked, scope_bound):
    """``(used, aliases, fresh)`` for one statement's own expressions."""
    aliases = set()
    if isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
        # ``iter_child_nodes`` would skip the body, so a nested definition that
        # closes over the loop target never reached ``_nested_scope_uses``
        # (#1953 fix-r1 round 8, codex r4 P0-2).  The header, by contrast, is
        # evaluated right here: ``def inner(x=_h('common', <stub>))`` writes at
        # definition time (codex r5 P0-2).
        for item in _definition_header(statement):
            if _handle_use(item, tracked, scope_bound, aliases):
                return True, set(), set()
            # A lambda in the header is created here and runs later, against
            # the enclosing function chain (codex r9 P0-4).
            for node in ast.walk(item):
                if isinstance(node, ast.Lambda) and _nested_scope_uses(
                        node, tracked, scope_bound):
                    return True, set(), set()
        if isinstance(statement, ast.ClassDef):
            # The class body resolves a name against its own namespace first:
            # ``class C: print = <writer>`` then ``print(_h)`` inside the same
            # body calls the class attribute, not the builtin (codex r6
            # P0-5).
            return (_class_body_use(statement.body, tracked,
                                    scope_bound | _class_local_names(statement),
                                    nested_bound=scope_bound),
                    set(), set())
        return _nested_scope_uses(statement, tracked, scope_bound), set(), set()
    aliases = set()
    fresh = set()
    skip = None
    if isinstance(statement, (ast.Assign, ast.AnnAssign)):
        # The right-hand side runs first: ``x = [_h('common', <stub>)]`` is a
        # write even though the value is a fresh container (codex r5 P0-1).
        # Only the pure copy ``_f = _h`` skips its own value -- the copy is
        # followed through ``aliases`` instead.
        aliases, fresh, copies = _assignment_effect(
            statement, tracked, scope_bound)
        if copies:
            skip = id(statement.value)
            if isinstance(statement.value, (ast.Tuple, ast.List)):
                # Round 11 (codex r7 P0-3): only the copies leave the
                # expression.  ``f, x = h, [h('common', <stub>)]`` calls the
                # handle from the element that is not a copy, so the whole
                # right-hand side may never be skipped.
                # Round 12 (codex r8 P1-5): a copy nested one level deeper is
                # still a copy -- ``f, (g, x) = h, (h, None)`` binds ``g`` to
                # the handle and only prints it, which is a read.
                if _non_copy_use(statement.value, copies, tracked,
                                 scope_bound, aliases):
                    return True, set(), set()
    # A truth test reads the handle: ``if _h:`` / ``while _h:`` / ``assert _h``
    # are not hand-overs (codex r5 P1-5).  Calls inside the test are still
    # inspected -- ``read_only`` stops at a non-read-only callee.
    truth_tests = set()
    if isinstance(statement, (ast.If, ast.While)):
        truth_tests.add(id(statement.test))
    elif isinstance(statement, ast.Assert):
        truth_tests.add(id(statement.test))
        # #1953 fix-r1 round 11 (codex r7 P0-5): the message is *not* a read.
        # Round 10 believed it was only formatted on failure, but the failing
        # assertion keeps the object in ``AssertionError.args``:
        # ``try: assert False, _h except AssertionError as e:
        # e.args[0]('common', <stub>)`` writes the receiver.  The message stays
        # on the ordinary fail-closed path.
    for child in ast.iter_child_nodes(statement):
        if isinstance(child, ast.stmt) or id(child) == skip:
            continue
        if _handle_use(child, tracked, scope_bound, aliases,
                       id(child) in truth_tests):
            return True, set(), set()
    return False, aliases, fresh


def _class_body_use(statements, tracked, scope_bound, outer=None,
                    nested_bound=None):
    """Whether a class body hands a tracked handle on (#1953 fix-r1 r11).

    #1953 fix-r1 round 11 (codex r7 P1): the class namespace shadows an outer
    name for every statement *after* the one that binds it, so
    ``class C: _h = lambda *a: None; _h('common', <stub>)`` calls the class
    attribute and is not a hand-over.  Round 10 passed the outer tracked set
    through unchanged and turned that clean shape red.  Only a statement
    written directly in the body rebinds here -- one inside ``if``/``try`` may
    not run, so the outer handle stays live (fail-closed, exactly like the
    flat scope scan in :func:`_loop_target_used_after`).

    #1953 fix-r1 round 12 (codex r8 P0-2 / P0-3): two things the first version
    missed.  A compound statement written in the body runs *in* the class
    body, so its blocks are scanned too -- ``class C: if flag: h('common',
    <stub>)`` writes the receiver.  And the class namespace is not a closure
    scope: a method or lambda created in the body resolves its free names in
    the enclosing chain, so ``class C: h = lambda: None; def m(self):
    h('common', <stub>)`` still calls the outer handle.  *outer* carries the
    unshadowed set for exactly those nested scopes, and *nested_bound* the
    bound names of the enclosing function -- the class namespace must not leak
    into a method's free-name resolution (codex r9 P1-7).

    Round 12 (codex r9 P0-3): ``_f = _h`` written in the body hands the handle
    to ``_f`` exactly as it does in a function, so the aliases a statement
    produces stay tracked for the statements after it.
    """
    used, _aliases = _class_body_scan(statements, tracked, scope_bound,
                                      outer, nested_bound)
    return used


def _class_body_scan(statements, tracked, scope_bound, outer=None,
                     nested_bound=None):
    """``(used, aliases)`` for a class-body statement list (round 14).

    #1953 fix-r1 round 14 (codex r10 P0-4): round 12 followed ``_f = _h``
    written directly in the body, but an alias created inside a compound
    statement was dropped, so ``class C: if True: f = _h`` then ``f(...)``
    after the block stayed green.  Every block a compound statement runs is
    scanned and the aliases it may produce are handed to the statements after
    it -- fail-closed, because a block that does not run cannot hide a write.
    """
    outer = set(tracked) if outer is None else outer
    if nested_bound is None:
        nested_bound = scope_bound
    live = set(tracked)
    produced = set()
    for statement in statements:
        used, aliases = _class_body_use_in_statement(
            statement, live, outer, scope_bound, nested_bound)
        if used:
            return True, set()
        live |= aliases
        produced |= aliases
        dropped = _class_body_rebindings(statement, scope_bound)
        live -= dropped
        produced -= dropped
    return False, produced


def _class_body_use_in_statement(statement, live, outer, scope_bound,
                                 nested_bound):
    """``(used, aliases)`` for one class-body statement and the scopes it runs."""
    if isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef,
                              ast.ClassDef)):
        for item in _definition_header(statement):
            if _handle_use(item, live, scope_bound, set()):
                return True, set()
            # A lambda in a def/class header is created here but *runs* later,
            # so its body resolves free names against the enclosing function,
            # not this class namespace (codex r9 P0-4).
            for node in ast.walk(item):
                if isinstance(node, ast.Lambda) and _nested_scope_uses(
                        node, outer, nested_bound):
                    return True, set()
        if isinstance(statement, ast.ClassDef):
            # A nested class body is another class namespace: its statements
            # resolve against the enclosing function chain, not this one.
            return _class_body_use(statement.body, outer,
                                   nested_bound | _class_local_names(statement),
                                   outer, nested_bound), set()
        return _nested_scope_uses(statement, outer, nested_bound), set()
    used, aliases, _fresh = _handle_use_in_statement(
        statement, live, scope_bound)
    if used:
        return True, set()
    for node in ast.walk(statement):
        # A lambda written in the class body is a nested scope with the same
        # free-name rule as a method (codex r8 P0-3).
        if isinstance(node, ast.Lambda) and _nested_scope_uses(
                node, outer, nested_bound):
            return True, set()
    block_aliases = set()
    for block in _statement_blocks(statement):
        # #1953 fix-r1 round 14 (codex r10 P0-4): the aliases a block creates
        # reach the statements after the whole compound statement, exactly as a
        # direct ``_f = _h`` does.
        block_used, inner_aliases = _class_body_scan(
            block, live | aliases, scope_bound, outer, nested_bound)
        if block_used:
            return True, set()
        block_aliases |= inner_aliases
    return False, aliases | block_aliases


def _class_body_rebindings(statement, scope_bound=()):
    """Names a class-body statement provably rebinds to a fresh value."""
    if isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef,
                              ast.ClassDef)):
        # A definition always replaces the class namespace entry.
        return {statement.name}
    if isinstance(statement, ast.Assign):
        targets, value = statement.targets, statement.value
    elif isinstance(statement, ast.AnnAssign):
        targets, value = [statement.target], statement.value
    else:
        return set()
    # #1953 fix-r1 round 12 (codex r8 P0-4): *scope_bound* decides whether a
    # constructor call is still the builtin.  ``class C: object = lambda:
    # hs[0]; h = object()`` binds ``h`` to the handle, and asking
    # ``_fresh_value`` without the class namespace answered "fresh".
    if value is None or not _fresh_value(value, scope_bound):
        # An alias (``_h = _h``) keeps the handle, and anything else may hold
        # it, so neither ends the outer name's life.
        return set()
    return {target.id for target in targets if isinstance(target, ast.Name)}


def _statement_blocks(statement):
    """The statement lists a compound statement runs, in execution order."""
    if isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef,
                              ast.ClassDef)):
        return []
    if isinstance(statement, ast.If):
        return [statement.body, statement.orelse]
    if isinstance(statement, (ast.For, ast.AsyncFor, ast.While)):
        return [statement.body, statement.orelse]
    if isinstance(statement, ast.Try):
        return ([statement.body] + [handler.body for handler in statement.handlers]
                + [statement.orelse, statement.finalbody])
    if isinstance(statement, (ast.With, ast.AsyncWith)):
        return [statement.body]
    if isinstance(statement, ast.Match):
        return [case.body for case in statement.cases]
    return []


def _own_expression_nodes(statement):
    """Like :func:`_local_expression_nodes`, but keeps the lambda nodes.

    A ``lambda`` is skipped there (its body is another scope), yet the lambda
    *node* is the value an assignment binds -- ``_h = lambda *a: None`` -- and
    the block table needs its position to prove the binding ran (round 17).
    """
    stack = list(ast.iter_child_nodes(statement))
    while stack:
        node = stack.pop()
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef,
                             ast.ClassDef, ast.stmt)):
            continue
        yield node
        if isinstance(node, ast.Lambda):
            continue
        stack.extend(ast.iter_child_nodes(node))


def _certain_child_blocks(statement):
    """Blocks of *statement* that run whenever it runs (#1953 fix-r1 round 19).

    A ``finally`` body runs on every way out of its ``try``, and the taken arm
    of a statically decided ``if`` runs with the statement.  The other arms, a
    loop body (zero iterations), a ``with`` body (the manager may swallow the
    exception) and a class body may all be skipped.
    :func:`_certain_statements` already encodes that table for the flat scan
    (codex r6 P1-8 / P0-3 / P0-4); the round-17 block table had its own,
    narrower rule and flagged a clean file whose handle was overwritten in a
    ``finally`` block (GLM boundary r17 P1-2).
    """
    if isinstance(statement, ast.If):
        if _statically_true(statement.test):
            return {id(statement.body)}
        if _statically_false(statement.test):
            return {id(statement.orelse)}
        return set()
    if isinstance(statement, (ast.Try, getattr(ast, 'TryStar', ()))):
        return {id(statement.finalbody)}
    return set()


def _scope_block_slots(scope):
    """``(slots, parents)`` for one scope body (#1953 fix-r1 round 17).

    ``slots`` maps a statement, or one of its own expression nodes, to
    ``(id(block), index)``; ``parents`` maps a nested block to
    ``(id(parent block), index of the statement that runs it, always)``,
    where *always* says whether the block runs whenever that statement runs
    (:func:`_certain_child_blocks`).  The pair answers the question round 16
    got wrong: did this binding certainly run before that use?  A binding in
    the same block at a smaller index did, and so did one in a block that
    encloses the use; a binding in a *sibling* arm (the other ``if`` branch,
    a ``try`` body, an ``except`` handler) may simply not run.
    """
    if isinstance(scope, ast.Module):
        body = list(scope.body)
    elif isinstance(scope, ast.Lambda):
        body = [scope.body]
    else:
        body = list(getattr(scope, 'body', ()))
    slots = {}
    parents = {}
    stack = [body]
    while stack:
        block = stack.pop()
        for index, statement in enumerate(block):
            slots.setdefault(id(statement), (id(block), index))
            for node in _own_expression_nodes(statement):
                slots.setdefault(id(node), (id(block), index))
            certain = _certain_child_blocks(statement)
            for inner_block in _statement_blocks(statement):
                parents.setdefault(
                    id(inner_block),
                    (id(block), index, id(inner_block) in certain))
                stack.append(inner_block)
    return slots, parents


def _certain_before(slots, parents, binding, use):
    """True when *binding* certainly ran before *use* (#1953 fix-r1 round 17).

    Same block and a smaller index, or a block that encloses the use's block.
    Everything else is ambiguous -- the round-16 answer ("the latest binding
    by position") dropped a live write handle whenever the overwrite sat in
    an ``if`` arm or a ``try`` body and that path was not taken (boundary r16
    P0-1 / codex r16 P0-1).

    #1953 fix-r1 round 19 (GLM boundary r17 P1-2): a binding inside a block
    that always runs with its statement -- the ``finally`` body, the taken
    arm of a statically decided ``if`` -- climbs out through that block and
    is compared at the statement's own position.  Reading it as "may not
    have run" flagged a clean file whose handle was overwritten in a
    ``finally`` block.
    """
    bound = slots.get(id(binding))
    used = slots.get(id(use))
    if bound is None or used is None:
        return False
    uses = {}
    block, index = used
    while True:
        uses.setdefault(block, index)
        up = parents.get(block)
        if up is None:
            break
        block, index = up[0], up[1]
    block, index = bound
    while True:
        if block in uses and index < uses[block]:
            return True
        up = parents.get(block)
        if up is None or not up[2]:
            return False
        block, index = up[0], up[1]


def _enclosing_bound_names(tree, owner):
    """Names bound in *owner* or in any scope that encloses it (#1953 r9).

    The read-only call table and the fresh-constructor table both ask "does
    this spelling still mean the builtin here", and a shadow in an outer scope
    answers it just as well as one in the innermost scope (GLM boundary r5
    P1-2 / P1-3).
    """
    parents = _scope_parents(tree)
    scopes = []
    scope = owner
    while True:
        scopes.append(scope)
        if scope is None:
            break
        scope = parents.get(scope)
    names = set()
    for scope in scopes:
        names |= _scope_local_names(tree, scope)
    return names


def _certain_statements(tree):
    """Ids of statements that run whenever their block is entered (#1953 r9).

    Only these may end a name's life by rebinding it to a fresh value.  A
    rebind inside ``if cond:`` may simply not happen (codex r3 P0), and one
    after an unconditional ``raise`` never happens at all (codex r4 P0-1), so
    the scan keeps the handle live in both cases -- fail-closed.  The escape
    scan itself still looks at every statement: a source audit does not want a
    write hidden behind ``if False:``, which is exactly how the negative
    controls inject a body.

    #1953 fix-r1 round 10 (codex r6 P0-3 / P0-4 / P1-8): a class body runs in
    its own namespace and never rebinds an enclosing name, a ``with`` body can
    be cut short by the manager swallowing an exception, and a ``finally``
    block always runs.  Only function scopes are marked wholesale.
    """
    certain = set()

    def mark(statements):
        for statement in statements:
            certain.add(id(statement))
            if isinstance(statement, ast.If):
                if _statically_true(statement.test):
                    mark(statement.body)
                elif _statically_false(statement.test):
                    mark(statement.orelse)
            elif isinstance(statement, (ast.Try, getattr(ast, 'TryStar', ()))):
                # ``finally`` runs on every way out of the block, exceptions
                # included, so a rebinding there really does happen (codex r6
                # P1-8).  The body, the handlers and the ``else`` block may be
                # skipped, so none of them is certain.
                mark(statement.finalbody)

    mark(getattr(tree, 'body', ()))
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            # Only function scopes are marked: a class body runs in its own
            # namespace, so ``class C: h = object()`` never rebinds the outer
            # ``h`` -- marking it here let the flat scan drop a live handle
            # (codex r6 P0-3).  A ``with`` body is not certain either: the
            # context manager may swallow an exception and skip the rest of it
            # (codex r6 P0-4).
            mark(node.body)
    return certain


_CERTAIN_VALUE_CACHE = {}


def _certain_value_ids(tree):
    """Ids of right-hand sides whose assignment always runs (#1953 r11).

    :func:`_certain_statements` marks statements; the binding table works on
    right-hand-side nodes, so this is the same set projected onto values.
    Cached per tree the way ``_object_kinds`` is.
    """
    cached = _CERTAIN_VALUE_CACHE.get(id(tree))
    if cached is not None and cached[0] is tree:
        return cached[1]
    certain = _certain_statements(tree)
    values = set()
    for node in ast.walk(tree):
        if (isinstance(node, (ast.Assign, ast.AnnAssign))
                and id(node) in certain and node.value is not None):
            values.add(id(node.value))
    _CERTAIN_VALUE_CACHE[id(tree)] = (tree, values)
    return values


def _loop_target_used_after(tree, scope_root, loop, name):
    """True when *name* is still used as a handle after the loop.

    #1953 fix-r1 round 5 (codex r3 P0): the r4 version stopped at any
    rebinding, so ``h = h`` / ``if cond: h = None`` cleared the scan and the
    call went through.  #1953 fix-r1 round 7 (GLM boundary r3 P1): the handle
    also leaves through a second name (``_f = _h``) or a ``return``.

    #1953 fix-r1 round 9 (GLM concurrency r5 P0-1): round 8 asked "which
    statements can still run" and pruned a statically false ``if`` body.  For
    a source audit that is the wrong question -- and it made the rule blind to
    exactly the code the negative controls inject, so five of the round-8
    mutants SURVIVED.  Every statement of the scope *after* the loop is
    examined in source order, and a tracked name is never dropped: a rebinding
    may or may not run, so the handle stays live.  That is a superset of the
    round-8 answer (fail-closed, like every other rule here) and it retires
    the execution-order model -- ``_scan_block`` / ``_scan_statement`` /
    ``_find_loop_path`` / ``_fresh_value`` -- that produced the round 5..8
    regressions.
    """
    owner = None if isinstance(scope_root, ast.Module) else scope_root
    scope_bound = _enclosing_bound_names(tree, owner)
    certain = _certain_statements(tree)
    class_body = _class_body_statements(tree)
    tracked = {name}
    end = getattr(loop, 'end_lineno', None) or loop.lineno
    statements = [node for scope, node in _walk_scoped(tree)
                  if scope is owner and isinstance(node, ast.stmt)
                  and id(node) not in class_body]
    statements.sort(key=lambda node: (node.lineno, node.col_offset))
    for statement in statements:
        if statement.lineno <= end:
            continue
        used, aliases, fresh = _handle_use_in_statement(
            statement, tracked, scope_bound)
        if used:
            return True
        tracked |= aliases
        if id(statement) in certain:
            tracked -= fresh
    return False


def _comprehension_uses_handle(node, name, kinds):
    """True when *name* is called, written through, or used as a write receiver.

    Companion to :func:`_comprehension_parts` for the receiver rules: a
    comprehension target is bound to the element only inside its own
    comprehension, so ``[setattr(h, 'common', <stub>) for h in (md,)]`` writes
    the receiver while ``[h for h in (md,)]`` on its own writes nothing.
    """
    for part in _comprehension_parts(node):
        for child in ast.walk(part):
            if isinstance(child, ast.Call):
                if isinstance(child.func, ast.Name) and child.func.id == name:
                    return True
                written = _write_call_target(child.func, kinds)
                if written is not None:
                    receiver = written[1]
                    if receiver is None and child.args:
                        receiver = child.args[0]
                    if (isinstance(receiver, ast.Name)
                            and receiver.id == name):
                        return True
            if (isinstance(child, (ast.Subscript, ast.Attribute))
                    and isinstance(child.value, ast.Name)
                    and child.value.id == name
                    and isinstance(getattr(child, 'ctx', None), ast.Store)):
                return True
    return False


def _receiver_attribute_writes(tree, receivers, shared):
    """Receivers whose shared attribute a write replaced (#892 r26/r28).
    ``setattr(md, 'common', <stub>)`` rewrites the relay's shared attribute
    without any binding of ``md`` changing, so ``md.common.aggregate_status``
    keeps counting as the shared map.  A non-literal key on a receiver
    withdraws every one of them (GLM concurrency r25 P1-6).
    """
    poisoned = set()
    if not receivers:
        return poisoned
    kinds = _object_kinds(tree)
    # #892 r30: ``p = functools.partial(setattr, md); p('common', None)`` keeps
    # the write in a callable the alias table only knows as a partial, so the
    # receiver cannot be named; every receiver fails closed (MiniMax boundary
    # r29 P0-3).  The audited apparatus uses no ``partial`` at all.
    for scope, node in _walk_scoped(tree):
        if _unbounded_write(node, kinds):
            poisoned |= set(receivers)
            break
    # #892 r33: a dynamic ``getattr(obj, name)`` handle may be any bound
    # method, ``__setattr__``/``__setitem__`` included, but it writes *obj*.
    # The r32 guard withdrew every receiver in the file for any such call,
    # which both missed the handle held in a name (false PASS, GLM boundary
    # r32 P0-1 / concurrency r32 P1-1) and failed unrelated objects closed
    # (false FAIL, codex boundary r32 P1-1).
    simple = _simple_bindings(tree)
    parents = {}
    for scope, node in _walk_scoped(tree):
        for child in ast.iter_child_nodes(node):
            parents[child] = node
    dynamic_targets = []
    dynamic_handles = {}
    for scope, node in _walk_scoped(tree):
        if isinstance(node, ast.Call):
            # #892 r34: a ``getattr(obj, <dynamic>)`` handle that reaches a
            # call -- as the callee, or as an argument to a helper -- a name
            # (``h = ...``, ``h: object = ...``) or a return puts *obj* at
            # risk.  The r33 scan read only the direct-call and plain
            # assignment spellings (codex boundary r33 P0-4).  A handle merely
            # read into a data structure (``{name: getattr(md, name) for name
            # in patched}``, the apparatus's own save loop) is not tracked, so
            # the save/restore stays clean.
            if (_dynamic_getattr(node, kinds, simple)
                    and not _getattribute_pure_read(node, kinds, simple, parents)
                    and _handle_escapes(node, parents)):
                dynamic_targets.append((scope, node.args[0]))
            callee = node.func
            if (isinstance(callee, ast.Call)
                    and 'fn:attrgetter' in _expr_kinds(callee.func, kinds)
                    and _attrgetter_is_dynamic(callee) and node.args):
                dynamic_targets.append((scope, node.args[0]))
        elif isinstance(node, ast.Assign):
            value = node.value
            if (isinstance(value, ast.Call)
                    and 'fn:attrgetter' in _expr_kinds(value.func, kinds)
                    and _attrgetter_is_dynamic(value)):
                # ``h = op.attrgetter(name)`` takes its object at call time.
                for leaf in _leaf_targets(node.targets):
                    if isinstance(leaf, ast.Name):
                        dynamic_handles[(scope, leaf.id)] = 'call-time'
    # #892 r27: ``m = md`` hands the relay to another name; both spellings are
    # the same receiver (DeepSeek sibling r26 P1-4).  #892 r33: the table is
    # keyed by scope, so a binding inside one function no longer answers for a
    # same-named local of another (codex boundary r32 P1-2).
    scope_parent = _scope_parents(tree)
    roots = {(None, name): {name} for name in receivers}
    dict_roots = {}

    def _chain(scope):
        while True:
            yield scope
            if scope is None:
                return
            scope = scope_parent.get(scope)

    _binding_scopes = {id(item): owner for owner, item in _walk_scoped(tree)}

    def _visible(scope, before):
        return _visible_binding_table(simple, _binding_scopes,
                                      list(_chain(scope)), before,
                                      _certain_value_ids(tree))

    def _lookup(table, name, scope):
        for level in _chain(scope):
            key = (level, name)
            if key in table:
                return set(table[key]) & receivers
        return set()

    def receiver_roots(node, scope):
        if isinstance(node, ast.Name):
            return _lookup(roots, node.id, scope)
        if isinstance(node, ast.Subscript):
            # #892 r34: a star target holds the *list* of the elements it
            # covered (``a, *rest = (None, md); rest[0].common = <stub>``), so a
            # subscript off a receiver-rooted name is still that receiver
            # (codex boundary r33 P0-3).
            return receiver_roots(node.value, scope)
        # #1953 fix-r1 round 4 (GLM boundary r2 P1): the *result* container of a
        # comprehension over receivers holds them, so
        # ``_g = [h for h in (md,)]; _g[0].common = <stub>`` writes the
        # receiver.  The target is local to the comprehension, the container is
        # not.  Round 5 (codex r3 P1): a dict comprehension relays through its
        # *value*, and the iterable may be a name bound to the literal
        # (``items = (md,); _g = [h for h in items]``).
        if isinstance(node, (ast.ListComp, ast.SetComp, ast.GeneratorExp,
                             ast.DictComp)):
            element = node.value if isinstance(node, ast.DictComp) else node.elt
            # #1953 fix-r1 round 7 (codex r3 P1): a statically false filter
            # makes the comprehension yield nothing, so its result cannot
            # relay the receiver -- ``[h for h in (md,) if False]`` used to
            # fail the clean append-and-write closed.
            if _emptied_comprehension(node):
                return set()
            found = set()
            for generator in node.generators:
                names = set(_target_names(generator.target))
                if not (isinstance(element, ast.Name) and element.id in names):
                    continue
                for item in _bound_literal_elements(
                        generator.iter, _visible(scope, getattr(node, 'lineno', 0)),
                        shadowed=_shadowed_parameters(scope)):
                    found |= receiver_roots(item, scope)
            return found
        return set()

    def _relay_handle_roots(node, scope):
        """Receivers whose handles a relay comprehension carries (#1953 r7).

        ``g = [h for h in hs]; g[0]('common', <stub>)`` hands the same handles
        out of ``g`` that ``hs`` holds; ``hs`` is not a literal, its roots live
        in the alias table (GLM sibling r3 P2).
        """
        if _emptied_comprehension(node):
            return []
        element = node.value if isinstance(node, ast.DictComp) else node.elt
        if not isinstance(element, ast.Name):
            return []
        found = []
        for generator in node.generators:
            if element.id not in set(_target_names(generator.target)):
                continue
            name = _container_base(generator.iter)
            if name is None:
                continue
            for level in _chain(scope):
                if (level, name) in handle_containers:
                    found.extend(handle_containers[(level, name)])
                    break
        return found

    def namespace_dict_roots(node, scope):
        """Receivers whose ``__dict__`` mapping *node* evaluates to (#892 r28).

        ``md.__dict__``, ``vars(md)`` and a name bound to either (``d =
        md.__dict__``) are the same mapping; r27 judged only the first two, so
        ``d = md.__dict__; d['common'] = <stub>`` kept the relay trusted.
        """
        if isinstance(node, ast.Attribute) and node.attr == '__dict__':
            return receiver_roots(node.value, scope)
        if (isinstance(node, ast.Call) and _callable_name(node.func, kinds) == 'vars'
                and node.args):
            return receiver_roots(node.args[0], scope)
        # #892 r30: ``getattr(md, '__dict__')`` is the same mapping root, both as
        # a subscript target (``getattr(md, '__dict__')['common'] = <stub>``)
        # and through an alias (``_d = getattr(md, '__dict__')``).
        if _getattr_literal(node, kinds) == '__dict__':
            return receiver_roots(node.args[0], scope)
        # #1953 fix-r1 round 2 (GLM sibling r1b P1): the attribute spelling of
        # the same hand-over -- ``md.__getattribute__('__dict__')['common'] =
        # <stub>`` names the mapping just as ``getattr`` does, and only the
        # ``getattr`` spelling was decoded.
        # round 3 (codex r2 P0): the attribute name need not be a literal --
        # ``k = '__dict__'; md.__getattribute__(k)['common'] = <stub>`` is the
        # same mapping, and an attribute name we cannot prove to be a pure read
        # is exactly the case r35 already fails closed on.
        if (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
                and node.func.attr == '__getattribute__' and node.args):
            spelling = _constant_string(node.args[0], None)
            if spelling is None or spelling == '__dict__':
                return receiver_roots(node.func.value, scope)
        if isinstance(node, ast.Name):
            return _lookup(dict_roots, node.id, scope)
        return set()

    # #892 r30: a fixpoint, not a fixed number of rounds -- a four-hop
    # reverse-order alias chain (``_e = _d; _d = _c; ...; _a = md.__dict__``)
    # kept the last two links unresolved and the relay trusted (GLM
    # concurrency r29 P0-5).  #892 r32: the pairs come from the shared binding
    # walk, so a ``for``/``with``/unpacked/defaulted name resolves like a plain
    # assignment (codex boundary r31 P0-5).
    pairs = _scoped_alias_assignments(tree)
    while True:
        grown = False
        for scope, name, value, value_scope in pairs:
            for table, found in ((roots, receiver_roots(value, value_scope)),
                                 (dict_roots, namespace_dict_roots(value, value_scope))):
                key = (scope, name)
                if not found:
                    # A binding that is not a receiver shadows the outer name
                    # of the same spelling instead of inheriting it.
                    table.setdefault(key, set())
                    continue
                known = table.setdefault(key, set())
                if not found <= known:
                    known |= found
                    grown = True
        if not grown:
            break

    # #1953 fix-r1 round 17: the local binding table and the freshness guard
    # moved above the container scans.  A container relay (``_f = hs[0]``) is
    # cleared by the same proof a direct call needs (sibling r16 P1-3), and the
    # receiver of a *bound* handle is resolved in the scope that binds it
    # instead of from the file-global kinds table (codex r16 P0-3 / P0-4,
    # boundary r16 P0-2).
    scoped_bindings = {tree: _name_bindings(tree)}
    for scope, node in _walk_scoped(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
            scoped_bindings.setdefault(node, _name_bindings(node))
    scope_slots = {}
    scope_bound_names = {}

    def _bound_names(scope):
        if scope not in scope_bound_names:
            scope_bound_names[scope] = _enclosing_bound_names(tree, scope)
        return scope_bound_names[scope]

    def _block_slots(scope):
        if scope not in scope_slots:
            scope_slots[scope] = _scope_block_slots(scope)
        return scope_slots[scope]

    def _carried_container(name, scope):
        """Roots the name *name* carries as a handle container, or ``None``.

        #1953 fix-r1 round 18 (codex r17 P0-1): a container copy keeps the
        handles the original held, so the relay scans have to see the copy as
        the same container.  ``handle_containers`` is filled by the fixpoint
        below; reading it is safe because every caller runs after it.
        """
        for level in _chain(scope):
            if (level, name) in handle_containers:
                return handle_containers[(level, name)]
        return None

    def _value_carries(value, scope, seen=None):
        """True when *value* still hands write handles on (#1953 r19).

        The same question the container fixpoint above registers by
        (``_container_handle_roots`` / a tracked callee / a tracked alias / a
        relay comprehension), asked of the binding in force at a copy point.

        #1953 fix-r1 round 21 (GLM boundary r19 P2-5 / sibling r19 P1-2): the
        callee and alias branches read the per-name *history* table, so a chain
        that had certainly replaced the handles one hop upstream still read as
        carried (``m = [...]; m = [lambda]; z = list(m); w = list(z)`` failed a
        clean file).  They now ask :func:`_carries_at` at the hop's own point;
        the visited set stops a copy cycle.
        """
        seen = set() if seen is None else seen
        if id(value) in seen:
            return False
        seen.add(id(value))
        if _container_handle_roots(value, kinds, simple):
            return True
        called = _container_callee(value)
        if called is not None and _carries_at(called, scope, value, seen):
            return True
        if (isinstance(value, ast.Name)
                and _carries_at(value.id, scope, value, seen)):
            return True
        if isinstance(value, (ast.ListComp, ast.SetComp, ast.DictComp,
                              ast.GeneratorExp)):
            return bool(_relay_handle_roots(value, scope))
        return False

    def _touched_between(name, scope, start, end):
        """True when *name* is read or written strictly between two points.

        #1953 fix-r1 round 20 (codex r19 P0-2): the value in force at a copy
        is the latest binding only until something mutates that container in
        place -- ``hs = []; hs.append(getattr(md, '__setattr__')); copy =
        list(hs)`` copies the handle, but the binding value is the empty list
        and the copy read as fresh.  Any reference to the name between the
        binding and the copy keeps the carried answer; the false FAIL that
        costs is the price of not losing the write.
        """
        for node in ast.walk(scope):
            if not isinstance(node, ast.Name) or node.id != name:
                continue
            position = (getattr(node, 'lineno', 0),
                        getattr(node, 'col_offset', -1))
            if start < position < end:
                return True
        return False

    def _carries_at(name, scope, use, seen=None):
        """True when *name* still carries handles at *use* (#1953 r19).

        ``handle_containers`` is a per-name history: once ``hs`` held handles
        every later binding of that spelling looked like a handle container,
        so ``hs = [lambda *a: None]`` followed by ``hs = list(hs)`` kept the
        withdrawn receiver and failed a clean file (codex r18 P1-3).  Only a
        single binding that certainly ran before the copy may speak for the
        value in force; anything ambiguous keeps the carried answer.

        #1953 fix-r1 round 20 (codex r19 P0-2): a certainly-run rebinding to a
        fresh container is not the whole story -- the container can be filled
        in place after the rebinding.  Any reference in between keeps the
        carried answer.
        """
        if _carried_container(name, scope) is None:
            return False
        bindings = scoped_bindings.get(scope)
        if bindings is None:
            return True
        values = _bindings_at(bindings, name, use)
        if not values:
            return True
        limit = (getattr(use, 'lineno', 0), getattr(use, 'col_offset', -1))
        if any((line, getattr(value, 'col_offset', -1)) > limit
               for line, value in values):
            return True
        latest = max((line, getattr(value, 'col_offset', -1))
                     for line, value in values)
        last = [value for line, value in values
                if (line, getattr(value, 'col_offset', -1)) == latest]
        if len(last) != 1:
            return True
        slots, parents = _block_slots(scope)
        if not _certain_before(slots, parents, last[0], use):
            return True
        start = (getattr(last[0], 'lineno', 0),
                 getattr(last[0], 'col_offset', -1))
        if _touched_between(name, scope, start, limit):
            return True
        return _value_carries(last[0], scope, seen)

    def _copies_carried_container(value, scope):
        """True when *value* is a copy of a container that carries handles.

        ``hs = list(hs)`` / ``hs = tuple(hs)`` / ``hs = hs.copy()`` hand the
        same handles on under a new object; judging the constructor call as a
        fresh value ended the relay and a real write stayed invisible (codex
        r17 P0-1).  The shape alone is not enough -- an unrelated ``xs =
        list(range(3))`` stays fresh -- so the copied base must be a container
        the alias fixpoint already tracks.

        #1953 fix-r1 round 19 (codex r18 P0-1 / GLM boundary r18 P0-1): the
        base is decoded with the same ``_container_callee`` the fixpoint uses,
        so a nested copy (``list(tuple(hs))``) or a slice (``list(hs[:])``)
        is followed too; the hand-rolled one-level decoder read those as fresh
        and the write stayed invisible.  ``_carries_at`` then asks whether the
        name still carries at this point (codex r18 P1-3).
        """
        if not isinstance(value, ast.Call):
            return False
        base = _container_callee(value)
        if base is None:
            return False
        return _carries_at(base, scope, value)

    def _freshly_rebound(name, scope, use, allow_container=True):
        """True when the binding *name* holds at *use* is a provably fresh value.

        #1953 fix-r1 round 16: the round-15 flat scan drops an alias once a
        later assignment certainly replaces it (``_f = _h`` then ``_f =
        lambda *a: None``); the call-site guard here still asked
        ``_resolved_write_helper``, which fails closed on *every* reachable
        binding and so kept the write.  With the bound-handle kinds carrying
        their receiver (round 16) that turned ``_h = getattr(md, '__setattr__');
        _h = lambda *a: None; _h('common', <stub>)`` red -- the round-15 codex
        r14 P1 false FAIL, reintroduced.  Only a single, positionally latest
        binding counts, and a back-edge binding keeps the answer ambiguous
        (fail-closed).

        #1953 fix-r1 round 17 (boundary r16 P0-1 / codex r16 P0-1 / sibling
        r16 P1-1): "positionally latest" is not "certain".  ``if flag: _h =
        lambda *a: None`` may simply not run, and then the call still writes
        the receiver -- the round-16 version dropped that write and the ruler
        passed a poisoned file.  The overwrite only counts when the binding
        certainly ran before the use (:func:`_certain_before`), and the fresh
        value is judged against the enclosing scope's shadowed spellings
        (codex r16 P0-2), so ``object = lambda: _h; _h = object()`` keeps the
        handle live.

        ``allow_container=False`` is for the container/relay scans: a list or
        comprehension *result* is a fresh object, but it may carry the very
        handles the scan is following (``_hs = [getattr(md, _k) for _k in
        (_n,)]``), so it never ends a relay.
        """
        bindings = scoped_bindings.get(scope)
        if bindings is None:
            return False
        values = _bindings_at(bindings, name, use)
        if not values:
            return False
        limit = (getattr(use, 'lineno', 0), getattr(use, 'col_offset', -1))
        placed = [(line, getattr(value, 'col_offset', -1), value)
                  for line, value in values]
        if any((line, col) > limit for line, col, _value in placed):
            # A loop back edge reaches a binding written after the use; "the
            # last one" is not a static fact there.
            return False
        latest = max((line, col) for line, col, _value in placed)
        last = [value for line, col, value in placed if (line, col) == latest]
        if len(last) != 1:
            return False
        slots, parents = _block_slots(scope)
        if not _certain_before(slots, parents, last[0], use):
            return False
        if not allow_container and isinstance(
                last[0], (ast.List, ast.Tuple, ast.Set, ast.Dict, ast.ListComp,
                          ast.SetComp, ast.DictComp, ast.GeneratorExp)):
            return False
        if not allow_container and _copies_carried_container(last[0], scope):
            # Round 18 (codex r17 P0-1): a constructor copy of a tracked
            # container keeps the handles, so it does not end the relay.
            return False
        if not allow_container and isinstance(last[0], ast.Name):
            # #1953 fix-r1 round 21 (GLM sibling r19 P1-2): ``wc = list(w);
            # wx = wc`` -- the history table still lists ``wc`` as a handle
            # container, but the copy it holds was made after ``w`` was
            # certainly replaced by fresh values.  Ask ``_carries_at`` at the
            # alias's own point instead of reading the history; an ambiguous
            # or still-carrying binding keeps the carried answer.
            if _carried_container(last[0].id, scope) is not None:
                return not _carries_at(last[0].id, scope, last[0])
        return _fresh_value(last[0], _bound_names(scope))

    def _handle_receivers(name, scope, use):
        """Receivers a bound write handle *name* denotes at *use*, or ``None``.

        #1953 fix-r1 round 17 (codex r16 P0-3 / P0-4, boundary r16 P0-2): the
        kinds table is file-global, so two functions binding one spelling to
        handles of different receivers put *both* tags on the name and the
        round-16 ``sorted()`` pick read the wrong one -- a real write vanished
        in the file that called the second handle, and an uncalled function's
        receiver was withdrawn in the file that called the first.  The
        bindings in force win: the innermost scope that binds the spelling
        decides.  Its latest binding counts alone only when it certainly ran
        before the use; a conditional overwrite leaves both bindings in reach,
        so every one of them is read and an ambiguous handle keeps the write.

        ``None`` means "keep the file-global answer": the scope chain binds
        nothing here, or the binding in force is a handle this ruler cannot
        name (``partial``, ``attrgetter``, an unknown call result), where the
        conservative answer is the one already in place.  A set -- possibly
        empty -- is the answer the bindings give; empty means the name no
        longer denotes any write handle there (``_h = print``), so the call
        writes nothing.
        """
        for level in _chain(scope):
            bindings = scoped_bindings.get(level)
            if bindings is None:
                continue
            inner = level is scope
            visible = _bindings_at(bindings, name, use if inner else None)
            if not visible:
                continue
            latest = max((line, getattr(value, 'col_offset', -1))
                         for line, value in visible)
            group = [value for line, value in visible
                     if (line, getattr(value, 'col_offset', -1)) == latest]
            if not (inner and len(group) == 1
                    and _certain_before(*_block_slots(level), group[0], use)):
                # Two arms, a loop back edge, an outer scope: every visible
                # binding may be the one in force at the call.
                group = [value for _line, value in visible]
            receivers = set()
            for value in group:
                kinds_of = _expr_kinds(value, kinds)
                tagged = {kind.split(':', 2)[2] for kind in kinds_of
                          if kind.startswith('fn:write-handle:')}
                if tagged:
                    receivers = receivers | tagged
                    continue
                if any(kind in ('fn:write-handle', 'fn:partial')
                       or kind in {'fn:' + attr
                                   for attr in _MANAGED_WRITE_ATTRS}
                       for kind in kinds_of):
                    # A handle whose receiver the kinds table cannot name:
                    # fall back to the file-global decode, which fails closed.
                    return None
                if _fresh_value(value, _bound_names(level)):
                    continue
                if isinstance(value, ast.Name):
                    # A name the kinds table says nothing about is not a
                    # *tagged* handle (a tagged binding of that spelling
                    # anywhere would be in the table); an untagged dynamic one
                    # is what the dynamic-handle scan above is for.
                    continue
                if not kinds_of:
                    return None
                # A known, non-handle object (``_h = print``): no write.
            return receivers
        return None

    # #1953 fix-r1: a comprehension result carries the same handles a container
    # literal does, so the receiver is at risk once such a container is used as
    # a *caller* (``hs[0](...)`` / ``hs['__setattr__'](...)`` / ``next(hs)(...)``).
    # Judging the use instead of the comprehension keeps the apparatus save loop
    # clean: it reads the stored values back as restore operands and never
    # calls them (codex boundary r35 P0-1).
    handle_containers = {}

    def _carry(scope, name, carried):
        bucket = handle_containers.setdefault((scope, name), [])
        fresh = [root for root in carried if root not in bucket]
        bucket.extend(fresh)
        return bool(fresh)

    # Fixpoint, so ``hs = [...]`` / ``f = hs[0]`` / ``g = f`` all carry the
    # same roots: laundering a handle through a second name before calling it
    # is the same hand-over.
    while True:
        grown = False
        for scope, node in _walk_scoped(tree):
            if isinstance(node, ast.Assign):
                targets, value = node.targets, node.value
            elif isinstance(node, ast.AnnAssign) and node.value is not None:
                targets, value = [node.target], node.value
            else:
                continue
            carried = _container_handle_roots(value, kinds, simple)
            if not carried:
                called = _container_callee(value)
                if called is not None:
                    for level in _chain(scope):
                        if (level, called) in handle_containers:
                            carried = handle_containers[(level, called)]
                            break
            if not carried and isinstance(value, ast.Name):
                # ``other = hs`` hands the *container* over whole, and
                # ``other[0]('common', <stub>)`` takes the same handle out of
                # the second name; the fixpoint used to follow only a
                # subscript/call wrapper, so the alias lost the roots
                # (codex r1b P0).
                for level in _chain(scope):
                    if (level, value.id) in handle_containers:
                        carried = handle_containers[(level, value.id)]
                        break
            if not carried and isinstance(value, (ast.ListComp, ast.SetComp,
                                                  ast.DictComp, ast.GeneratorExp)):
                # #1953 fix-r1 round 8 (codex r4 P0-5 / GLM sibling r4 P1-2):
                # ``g = [h for h in hs]`` relays exactly the handles ``hs``
                # holds.  Only the *call* position decoded the relay, so
                # ``for f in g: f(...)`` and ``g[0].common = <stub>`` stayed
                # invisible; registering the relay here feeds all three.
                carried = _relay_handle_roots(value, scope)
            if not carried:
                continue
            for target in _leaf_targets(targets):
                if (isinstance(target, ast.Name)
                        and _carry(scope, target.id, carried)):
                    grown = True
        if not grown:
            break
    for scope, node in _walk_scoped(tree):
        if not isinstance(node, ast.Call):
            continue
        called = _container_callee(node.func)
        if called is None and isinstance(node.func, ast.Name):
            # ``f = hs[0]; f('common', <stub>)`` launders one handle into a
            # plain name first; the alias fixpoint above already recorded it.
            called = node.func.id
        if (called is not None and isinstance(node.func, ast.Name)
                and _freshly_rebound(called, scope, node, allow_container=False)):
            # #1953 fix-r1 round 17 (sibling r16 P1-3): a later, certain
            # fresh rebinding (``_f = lambda *a: None``) ends the relay -- the
            # name no longer holds the handle the container handed over, and
            # withdrawing the receiver anyway failed the clean file.
            continue
        carried = None
        if called is not None:
            for level in _chain(scope):
                if (level, called) in handle_containers:
                    carried = handle_containers[(level, called)]
                    break
        if carried is None and isinstance(node.func, ast.Subscript) and isinstance(
                node.func.value, (ast.ListComp, ast.SetComp, ast.DictComp,
                                  ast.GeneratorExp)):
            carried = _container_handle_roots(node.func.value, kinds, simple)
        if carried is None and isinstance(node.func, ast.Subscript):
            # #1953 fix-r1 round 7 (GLM sibling r3 P2): the comprehension may
            # be reachable through a name and relay a container the alias table
            # already knows (``g = [h for h in hs]; g[0](...)``).
            container = node.func.value
            if isinstance(container, ast.Name):
                values = simple.get(container.id) or ()
                if len(values) == 1:
                    container = values[0]
            if isinstance(container, (ast.ListComp, ast.SetComp, ast.DictComp,
                                      ast.GeneratorExp)):
                carried = (_container_handle_roots(container, kinds, simple)
                           or _relay_handle_roots(container, scope))
        for root in carried or ():
            poisoned |= (receiver_roots(root, scope)
                         | namespace_dict_roots(root, scope))
    for scope, node in _walk_scoped(tree):
        if not isinstance(node, (ast.For, ast.AsyncFor)):
            continue
        # #1953 fix-r1 round 2 (GLM concurrency r1b P1): the call position
        # already decodes ``list(hs)`` / ``iter(hs)`` wrappers, the iteration
        # position only accepted a bare name, so ``for h in list(hs):`` escaped
        # with the same handle.  Round 4 (GLM boundary r2 P0): the two
        # positions now share one decoder -- ``for h in hs[:]:`` /
        # ``for h in hs.copy():`` were still unseen.
        if isinstance(node.iter, ast.Name):
            iter_name = node.iter.id
        else:
            iter_name = _container_callee(node.iter)
        if iter_name is None:
            continue
        if _freshly_rebound(iter_name, scope, node, allow_container=False):
            # Round 17 (sibling r16 P1-3): the container was certainly
            # replaced by a fresh value before this loop, so it no longer
            # hands the old handle over.
            continue
        # #1953 fix-r1 round 3 (codex r2 P1): iterating a container of dynamic
        # handles only *hands them over* to the loop target, so
        # ``for h in hs: print(h)`` is a pure read.  Judge the loop target's
        # own use points -- a call or a store -- instead of withdrawing on the
        # iteration itself.  Round 4: the target outlives the loop, so the
        # rest of the scope counts too.
        target = node.target
        if not isinstance(target, ast.Name):
            continue
        body = list(node.body) + list(node.orelse)
        if not _loop_target_used_as_handle(body, target.id):
            if not _loop_target_used_after(tree, scope or tree, node, target.id):
                continue
        for level in _chain(scope):
            if (level, iter_name) in handle_containers:
                # ``for h in hs: h('common', <stub>)`` takes the same handles
                # out one at a time; iterating a container of dynamic handles
                # is a use like any other (#1953 fix-r1).
                for root in handle_containers[(level, iter_name)]:
                    poisoned |= (receiver_roots(root, scope)
                                 | namespace_dict_roots(root, scope))
                break

    # #1953 fix-r1 round 3 (codex r2 P1): a comprehension target is local to
    # its comprehension (Python 3), so carrying it in the enclosing alias table
    # made ``h = object(); [h for h in (md,)]; h.common = <stub>`` withdraw a
    # clean write.  Judge the write *inside* the comprehension instead, against
    # the literal elements the target is bound to.
    for scope, node in _walk_scoped(tree):
        if not isinstance(node, (ast.ListComp, ast.SetComp, ast.GeneratorExp,
                                 ast.DictComp)):
            continue
        # Round 9 (GLM sibling r5 P1-1 / boundary r5 P1-4): this is the fourth
        # consumer of a comprehension result and the only one the round-8
        # helper was not wired into.  Round 10 (codex r6 P0-2): the guard moved
        # into ``_comprehension_parts`` -- a statically false filter kills only
        # what runs after it, so the iterables and the earlier filters are
        # still judged instead of the whole comprehension being skipped.
        for generator in node.generators:
            names = [name for name in _target_names(generator.target)
                     if _comprehension_uses_handle(node, name, kinds)]
            if not names:
                continue
            if isinstance(generator.iter, (ast.List, ast.Tuple, ast.Set)):
                elements = generator.iter.elts
            else:
                elements = _bound_literal_elements(
                    generator.iter, _visible(scope, getattr(node, 'lineno', 0)),
                    shadowed=_shadowed_parameters(scope))
            for element in elements:
                poisoned |= (receiver_roots(element, scope)
                             | namespace_dict_roots(element, scope))

    # #1953 fix-r1 (codex r1b P0): the handle is not only *called* -- a
    # mapping or attribute write through an element writes the object the
    # handle came from just as well (``hs[0]['common'] = <stub>`` /
    # ``hs[0].common = <stub>``).  The call-site scan saw neither.
    for scope, node in _walk_scoped(tree):
        if not isinstance(node, (ast.Subscript, ast.Attribute)):
            continue
        if not isinstance(getattr(node, 'ctx', None), ast.Store):
            continue
        called = _container_callee(node.value)
        if called is None and isinstance(node.value, ast.Name):
            called = node.value.id
        if called is None:
            continue
        if (isinstance(node.value, ast.Name)
                and _freshly_rebound(called, scope, node, allow_container=False)):
            # Round 17 (sibling r16 P1-3): the same certain-overwrite proof
            # applies to the element-write spelling.
            continue
        for level in _chain(scope):
            if (level, called) in handle_containers:
                for root in handle_containers[(level, called)]:
                    poisoned |= (receiver_roots(root, scope)
                                 | namespace_dict_roots(root, scope))
                break

    for scope, target in dynamic_targets:
        poisoned |= receiver_roots(target, scope) | namespace_dict_roots(target, scope)
    for scope, node in _walk_scoped(tree):
        if not isinstance(node, ast.Call):
            continue
        target_expr = None
        if isinstance(node.func, ast.Name):
            if _freshly_rebound(node.func.id, scope, node, allow_container=False):
                # Round 17 (sibling r16 P1-3): a certain fresh rebinding ends
                # the dynamic-handle relay too.
                continue
            for level in _chain(scope):
                if (level, node.func.id) in dynamic_handles:
                    target_expr = dynamic_handles[(level, node.func.id)]
                    break
            if target_expr == 'call-time':
                target_expr = node.args[0] if node.args else None
        if target_expr is None:
            continue
        poisoned |= receiver_roots(target_expr, scope) | namespace_dict_roots(target_expr, scope)

    calls = []

    # #1953 fix-r1 (codex r1b P1): the kinds table is name-global, so a
    # comprehension target bound to ``operator.setitem`` makes every call of
    # the same spelling a write handle -- including one the function shadows
    # with a lambda afterwards.  Before a generic-handle call is read as a
    # write, check what the name denotes *in that scope at that call*; a plain
    # dynamic ``getattr`` handle still withdraws its own receiver through the
    # dynamic-target scan above.
    # #1953 fix-r1 round 17: ``scoped_bindings`` and the freshness guard moved
    # above the container scans, which need the same "is this name still the
    # handle it was bound to" answer (sibling r16 P1-3).
    def _generic_handle(func):
        if not isinstance(func, ast.Name) or func.id in _WRITE_HELPERS:
            return False
        found = kinds.get(func.id, ())
        # #1953 fix-r1 round 16: a *bound* handle keeps its receiver in the
        # kind (``fn:write-handle:<name>``); without the prefix test the
        # flow guard below skipped it, so ``_h = getattr(md, '__setattr__')``
        # followed by ``_h = lambda *a: None; _h('common', <stub>)`` withdrew
        # the receiver again (the round-15 codex r14 P1 false FAIL).
        return (any(kind == 'fn:write-handle'
                    or kind.startswith('fn:write-handle:') for kind in found)
                or any('fn:' + name in found for name in _MANAGED_WRITE_ATTRS))

    import_events = {}
    flow_marks = {}
    module_calls = []

    def _module_calls():
        if not module_calls:
            module_calls.append(_module_call_positions(tree))
        return module_calls[0]

    def _scope_import_events(scope):
        """``name -> [event]`` for every binding in *scope* (round 12, r14).

        #1953 fix-r1 round 12 (codex r8 P1-6 / r9 P0-1, P0-2, P1-6): the first
        round-12 version asked "does this scope import the name and bind it
        nowhere else", which is wrong in each direction.  A ``from builtins
        import setattr as w`` written at module level is the builtin a function
        calls (a chain lookup, not a same-scope one); a ``w = print`` *after*
        the call does not undo the call; and a later ``from builtins import
        print as w`` does.  Every binding keeps its source position, whether it
        is conditional and its loops, so the use point decides (round 14,
        codex r10 P0-1 / P0-2 / P0-3).
        """
        if scope in import_events:
            return import_events[scope]
        events = {}
        if scope is None:
            body = tree.body
        elif isinstance(scope, ast.Lambda):
            body = [scope.body]
        else:
            body = scope.body
        marks = flow_marks.setdefault(scope, _flow_marks(body))

        pending = []

        def add(name, node, helper, certain):
            conditional, loops = marks.get(id(node), (True, ()))
            items = events.setdefault(name, [])
            items.append(
                (node.lineno, node.col_offset, helper, conditional, loops,
                 certain))
            return items

        if isinstance(scope, (ast.FunctionDef, ast.AsyncFunctionDef,
                              ast.Lambda)):
            for name in _argument_names(scope.args):
                # A parameter shadows at every position.
                events.setdefault(name, []).append((-1, -1, None, False, (),
                                                    True))
        for level, node in _walk_scoped(tree):
            # ``_scope_parents`` maps a module-level function to ``None``, so
            # the module scope reaches here as ``None``; the walk's own owner
            # spelling is ``None`` as well (codex r9 P0-1).
            if (tree if level is None else level) is not (
                    tree if scope is None else scope):
                continue
            if isinstance(node, (ast.Import, ast.ImportFrom)):
                builtins_import = getattr(node, 'module', None) == 'builtins'
                for alias in node.names:
                    name = alias.asname or alias.name.split('.')[0]
                    helper = (alias.name
                              if builtins_import
                              and alias.name in _MANAGED_WRITE_ATTRS
                              else None)
                    add(name, node, helper, True)
            elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef,
                                   ast.ClassDef)):
                add(node.name, node, None, True)
            elif isinstance(node, ast.stmt):
                names, value = _local_bound_names(node)
                for name in names:
                    items = add(name, node, None, False)
                    if value is not None:
                        pending.append((items, len(items) - 1,
                                        (node.lineno, node.col_offset),
                                        items[-1][4], value))
            elif isinstance(node, ast.NamedExpr):
                for name in _target_names(node.target):
                    items = add(name, node, None, False)
                    pending.append((items, len(items) - 1,
                                    (node.lineno, node.col_offset),
                                    items[-1][4], node.value))
            elif isinstance(node, ast.match_case):
                for name in _match_capture_names(node.pattern):
                    add(name, node, None, False)
        # Round 15 (codex r14 P0-2 / P0-3): an assignment whose right side
        # spells a write helper keeps the spelling one on this side too.
        _resolve_assignment_helpers(events, pending)
        import_events[scope] = events
        return events

    def _write_import_at(name, scope, node):
        """The builtin write helper *name* denotes at *node*, or ``None``.

        Round 14 (codex r10 P0-1 / P0-2 / P0-3): source position is not enough.
        A binding inside a branch cannot erase an earlier write helper
        (fail-closed), a loop back edge puts a textually later binding in reach
        at the call, and a same-line rebinding is ordered by column.  An
        enclosing scope is resolved at call time, which this ruler does not
        model: the last module binding wins, and a write binding anywhere on
        the way keeps the spelling a write helper (codex r9 P0-1 / r10 P1-5).
        """
        # Round 15 (codex r14 P0-4): ``flow_marks`` is filled by
        # ``_scope_import_events``; reading it first left ``loops`` empty on
        # the very first resolution of a scope, so a module-scope loop back
        # edge was missed until an unrelated call warmed the cache.
        _scope_import_events(scope)
        loops = tuple(flow_marks.get(scope, {}).get(
            id(node), (True, ()))[1])
        outer = getattr(scope, 'name', None)
        for level in _chain(scope):
            if level is not None and level is not scope:
                # ``_chain`` walks outward, so the last name seen is the
                # module-level definition whose call site decides.
                outer = getattr(level, 'name', None)
            events = _scope_import_events(level).get(name)
            if not events:
                continue
            if level is scope:
                chosen = _binding_event_at(
                    events, (node.lineno, node.col_offset), loops)
                if chosen is None:
                    # A binding written after the call is not yet in force;
                    # keep looking outward.
                    continue
                return chosen[2]
            positions = (_module_calls().get(outer, ())
                         if level is None and outer else ())
            chosen = _binding_event_at(events, None, anywhere=True,
                                       positions=positions)
            return None if chosen is None else chosen[2]
        return None

    def collect(node, scope, loops):
        written = _setattr_arguments(node, kinds)
        writes = [] if written is None else [written]
        if writes and isinstance(node.func, ast.Name):
            # #1953 fix-r1 round 17: the *bound* spelling (two arguments:
            # key, value) writes the receiver the name holds.  The scope that
            # binds it decides which receiver that is (codex r16 P0-3/P0-4);
            # ``_freshly_rebound`` still comes first, because a certain fresh
            # overwrite means the name holds no handle at all.
            #
            # #1953 fix-r1 round 18 (codex r17 P1-3): the freshness guard is
            # about the *name*, not the arity.  Guarding only the two-argument
            # spelling kept ``h = setattr; h = lambda *a: None; h(md, 'common',
            # object())`` red -- the call writes nothing, but the receiver was
            # still withdrawn.  Only the receiver resolution below needs the
            # bound-handle shape.
            if _freshly_rebound(node.func.id, scope, node):
                writes = []
            elif len(node.args) == 2:
                receivers = _handle_receivers(node.func.id, scope, node)
                if receivers is not None:
                    # An empty set is an answer too: the bindings in force say
                    # the name no longer denotes a write handle.
                    writes = [(ast.Name(id=name, ctx=ast.Load()), written[1])
                              for name in sorted(receivers)]
        if writes and _generic_handle(node.func):
            bindings = scoped_bindings.get(scope)
            if bindings is not None and _resolved_write_helper(
                    node.func, bindings) is None:
                # #1953 fix-r1 round 11: ``from builtins import setattr as
                # _sa`` *is* the builtin, but an import leaves no binding for
                # ``_resolved_write_helper`` to follow, so the guard cleared
                # the write and ``calibrate-builtin-alias-setattr`` survived.
                # Round 12 asks the scope chain which binding is in force at
                # the call (codex r9 P0-1 / P0-2 / P1-6).
                if not (isinstance(node.func, ast.Name)
                        and not _bindings_at(bindings, node.func.id, node)
                        and _write_import_at(node.func.id, scope, node)):
                    writes = []
        for entry in writes:
            calls.append((node, scope, loops, entry))
        for child in ast.iter_child_nodes(node):
            collect(child, child if isinstance(
                child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda))
                else scope,
                (loops + (node,)) if isinstance(node, ast.For) else loops)

    collect(tree, tree, ())
    for node, scope, loops, written in calls:
        target, key = written
        found = receiver_roots(target, scope) or namespace_dict_roots(target, scope)
        if not found:
            continue
        # #892 r26: the key is resolved in its own scope and from the loop it
        # runs in -- a file-wide lookup sees every unrelated ``name`` binding
        # and would fail closed on the apparatus's own save/restore loop.
        values = None
        key_name = key.id if isinstance(key, ast.Name) else None
        for loop in reversed(loops):
            if key_name is not None and _first_target_name(loop.target) == key_name:
                # #892 r27: the loop's own body may rebind the key variable, so
                # the iterated dict's keys are not what the call writes (GLM
                # concurrency r26 P1-5).
                if _rebound_in_loop(loop, key_name):
                    values = None
                else:
                    values = _iterable_dict_keys(loop.iter, scope)
                break
        else:
            values = _constant_string_values(key, scope)
        if values is None:
            # #892 r26: an unbounded key can spell the shared attribute, so it
            # withdraws every receiver in the file.  A key whose every binding
            # is a literal dict iteration is only as dynamic as that dict and
            # is judged by its actual spellings (``for name, value in
            # patched.items(): setattr(md, name, value)`` is the apparatus's
            # own wiring control, never the shared name).
            poisoned |= found
        elif values & shared:
            poisoned |= found
    # #892 r27: the plain assignment spelling is the same write -- ``md.common
    # = <stub>`` builds no call at all, so every ``setattr`` rule was one
    # punctuation mark away from bypass (six r26 findings, GLM boundary r26
    # P0-1 / DeepSeek boundary r26 P0-1).
    for scope, node in _walk_scoped(tree):
        if isinstance(node, ast.Assign):
            targets = node.targets
        elif isinstance(node, (ast.AnnAssign, ast.AugAssign)):
            targets = [node.target]
        else:
            continue
        for target in _leaf_targets(targets):
            if isinstance(target, ast.Name):
                # #892 r30: ``_d = md.__dict__; _d |= {'common': <stub>}``
                # replaces the mapping through its alias.
                if isinstance(node, ast.AugAssign):
                    found = namespace_dict_roots(target, scope)
                    if found and _dict_replacement_hits(node.value, shared):
                        poisoned |= found
                continue
            if isinstance(target, ast.Attribute):
                found = receiver_roots(target.value, scope)
                if not found:
                    continue
                if target.attr in shared:
                    poisoned |= found
                # #892 r28: ``md.__dict__ |= {...}`` replaces the mapping the
                # relay answers from; only a literal dict of unrelated keys is
                # harmless.
                elif target.attr == '__dict__':
                    if _dict_replacement_hits(node.value, shared):
                        poisoned |= found
            elif isinstance(target, ast.Subscript):
                found = namespace_dict_roots(target.value, scope)
                if not found:
                    continue
                key = target.slice
                if not (isinstance(key, ast.Constant) and isinstance(key.value, str)):
                    poisoned |= found
                elif key.value in shared:
                    poisoned |= found
    # #892 r30: ``exec(src, md.__dict__)`` / ``eval(src, md.__dict__)`` run an
    # arbitrary write against the receiver's mapping with no AST node to read;
    # ``=(...)``-style tuple targets (``md.common, _x = None, None``) are the
    # plain assignment one punctuation mark away from it.
    for scope, node in _walk_scoped(tree):
        if not isinstance(node, ast.Call):
            continue
        helper = _callable_name(node.func, kinds)
        if helper not in ('exec', 'eval'):
            continue
        namespaces = node.args[1:]
        for namespace in namespaces:
            poisoned |= namespace_dict_roots(namespace, scope)
    # #892 r28: the mapping's own mutators are the subscript write one call
    # away -- ``md.__dict__.update(common=<stub>)``, ``setdefault('common',
    # <stub>)``, ``pop('common')``, ``clear()``.  r27 read ``update`` /
    # ``setdefault`` only for the module namespace (``globals().update(...)``),
    # so every receiver spelling stayed green.
    for scope, node in _walk_scoped(tree):
        written = _mapping_method_call(node, kinds, tree)
        if written is None:
            continue
        target, keys = written
        found = namespace_dict_roots(target, scope)
        if not found:
            continue
        if '*' in keys or keys & shared:
            poisoned |= found
    return frozenset(poisoned & receivers)


def _rebound_in_loop(loop, name):
    """True when *loop*'s own body rebinds its key variable (#892 r27)."""
    for node in ast.walk(loop):
        if node is loop:
            continue
        if isinstance(node, ast.Assign):
            targets = node.targets
        elif isinstance(node, (ast.AnnAssign, ast.AugAssign)):
            targets = [node.target]
        elif isinstance(node, ast.NamedExpr) and isinstance(node.target, ast.Name):
            if node.target.id == name:
                return True
            continue
        elif isinstance(node, (ast.For, ast.AsyncFor)):
            if name in _target_names(node.target):
                return True
            continue
        elif isinstance(node, (ast.With, ast.AsyncWith)):
            # #892 r32: ``with nullcontext('common') as k:`` rebinds the loop
            # key before the write, exactly as an assignment does (codex
            # boundary r31 P0-9).
            for item in node.items:
                if item.optional_vars is not None \
                        and name in _target_names(item.optional_vars):
                    return True
            continue
        elif isinstance(node, ast.ExceptHandler) and node.name == name:
            return True
        else:
            continue
        if any(name in _target_names(target) for target in targets):
            return True
    return False


def _first_target_name(target):
    """The first name a ``for`` target binds, or ``None`` (#892 r26)."""
    if isinstance(target, ast.Name):
        return target.id
    if isinstance(target, ast.Tuple) and target.elts:
        first = target.elts[0]
        if isinstance(first, ast.Name):
            return first.id
    return None


def _dict_keys_of(value, tree):
    """Literal string keys of a ``{...}`` expression, or ``None`` (#892 r26).

    A dict comprehension that re-keys a literal dict (``{name: getattr(md, name)
    for name in patched}``) holds exactly that dict's keys, so the apparatus's
    save/restore loop stays resolvable.
    """
    if isinstance(value, ast.DictComp) and len(value.generators) == 1:
        generator = value.generators[0]
        if (isinstance(value.key, ast.Name)
                and isinstance(generator.target, ast.Name)
                and value.key.id == generator.target.id):
            return _iterable_dict_keys(generator.iter, tree)
        return None
    if not isinstance(value, ast.Dict):
        return None
    keys = set()
    for key in value.keys:
        if not (isinstance(key, ast.Constant) and isinstance(key.value, str)):
            return None
        keys.add(key.value)
    return keys


def _dict_binding_keys(name, tree):
    """Every literal key *name* can hold as a dict, or ``None`` when unbounded.

    #892 r27: a literal dict is not frozen at its binding site -- ``d['x'] = v``,
    ``d.update({...})``, ``d |= {...}`` and ``d.setdefault(...)`` all add keys
    the binding walk never sees, so any mutation makes the key set unbounded
    instead of stale (GLM boundary r26 P0-2 / DeepSeek concurrency r26 P1-4).
    """
    keys = set()
    seen = False
    for node in ast.walk(tree):
        if isinstance(node, ast.Assign):
            targets = node.targets
            value = node.value
        elif isinstance(node, ast.AnnAssign):
            targets = [node.target]
            value = node.value
        elif isinstance(node, ast.AugAssign):
            targets = [node.target]
            value = None
        elif isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
            if (isinstance(node.func.value, ast.Name)
                    and node.func.value.id == name
                    and node.func.attr in ('update', 'setdefault', 'pop',
                                           'popitem', 'clear')):
                return None
            continue
        else:
            continue
        for target in targets:
            if isinstance(target, ast.Name) and target.id == name:
                found = None if value is None else _dict_keys_of(value, tree)
                if found is None:
                    return None
                keys |= found
                seen = True
            elif (isinstance(target, ast.Subscript)
                  and isinstance(target.value, ast.Name)
                  and target.value.id == name):
                return None
    return keys if seen else None


def _iterable_dict_keys(expr, tree):
    """Literal keys of the dict an iteration walks, or ``None`` (#892 r26)."""
    # ``.values()`` is deliberately absent: it walks the *values*, so its
    # spellings are not the dict's keys and resolving them as keys would let
    # ``for name in {'X': 'common'}.values(): setattr(md, name, ...)`` pass.
    if (isinstance(expr, ast.Call) and isinstance(expr.func, ast.Attribute)
            and expr.func.attr in ('items', 'keys')):
        return _iterable_dict_keys(expr.func.value, tree)
    if isinstance(expr, (ast.Tuple, ast.List)):
        # #892 r27: a literal sequence of ``(key, value)`` pairs iterates a
        # known key set, which is how the harness restores the modules it
        # saved (``for name, previous in (('common', saved), ...)``).  Without
        # it a legitimate restore read as an unbounded pre-seed.
        keys = set()
        for element in expr.elts:
            if not isinstance(element, (ast.Tuple, ast.List)) or not element.elts:
                return None
            first = element.elts[0]
            if not (isinstance(first, ast.Constant) and isinstance(first.value, str)):
                return None
            keys.add(first.value)
        return keys
    if isinstance(expr, ast.Name):
        return _dict_binding_keys(expr.id, tree)
    if isinstance(expr, ast.Dict):
        return _dict_keys_of(expr, tree)
    return None


def _iterable_dict_items(expr):
    """The ``key -> value`` expressions a literal pair sequence holds (#892 r27).

    ``for name, previous in (('common', saved), ...)`` restores each entry from
    the value paired with its *own* key, so the restore has to be recorded as
    ``saved`` for ``common`` -- recording the loop variable ``previous`` would
    poison a key the harness legitimately republishes.
    """
    if isinstance(expr, (ast.Tuple, ast.List)):
        pairs = list(expr.elts)
    elif isinstance(expr, ast.Dict):
        pairs = [ast.Tuple(elts=[key, value], ctx=ast.Load())
                 for key, value in zip(expr.keys, expr.values)]
    else:
        return None
    items = {}
    for element in pairs:
        if not isinstance(element, (ast.Tuple, ast.List)) or len(element.elts) < 2:
            return None
        key = element.elts[0]
        if not (isinstance(key, ast.Constant) and isinstance(key.value, str)):
            return None
        items[key.value] = element.elts[1]
    return items


def _constant_string_values(expr, tree):
    """The string constants *expr* can evaluate to, or ``None`` (#892 r26).

    Only the shapes the apparatus itself uses are resolved: a literal string,
    a name bound solely by a literal dict, and a ``for`` target over such a
    dict.  Anything else is unbounded, and the caller keeps its fail-closed
    whole-set withdrawal.
    """
    if isinstance(expr, ast.Constant) and isinstance(expr.value, str):
        return {expr.value}
    if not isinstance(expr, ast.Name):
        return None
    keys = set()
    seen = False
    dict_bound = False
    for node in ast.walk(tree):
        if isinstance(node, (ast.For, ast.comprehension)):
            target = node.target
            if isinstance(target, ast.Tuple):
                if not target.elts or not isinstance(target.elts[0], ast.Name):
                    continue
                first = target.elts[0]
            else:
                first = target
            if not (isinstance(first, ast.Name) and first.id == expr.id):
                continue
            found = _iterable_dict_keys(node.iter, tree)
            if found is None:
                return None
            keys |= found
            seen = True
        elif isinstance(node, (ast.Assign, ast.AnnAssign)):
            targets = (node.targets if isinstance(node, ast.Assign)
                       else [node.target])
            value = node.value
            for target in targets:
                if not (isinstance(target, ast.Name) and target.id == expr.id):
                    continue
                if value is None:
                    continue
                # #892 r27: a literal string binding is a key spelling the
                # audit can read (``_HELD_STATE = "..."`` then
                # ``globals()[_HELD_STATE] = state``), which is what keeps a
                # real file's own dynamic machinery from failing it closed.
                if isinstance(value, ast.Constant) and isinstance(value.value, str):
                    keys.add(value.value)
                    seen = True
                else:
                    dict_bound = True
    if dict_bound:
        found = _dict_binding_keys(expr.id, tree)
        if found is None:
            return None
        keys |= found
        seen = True
    return keys if seen else None


def _withdrawn_receivers(tree, rel=None, shared=None):
    """Relay receivers whose module-scope bindings are not proven (#892 r23).

    ``_function_shared_names`` withdrew only receivers rebound *inside* the
    audited function, and the module scan withdrew only the shared name
    itself, so ``md = <stub>`` at module scope -- or through ``global md`` from
    any function -- kept every ``md.common.aggregate_status`` in the file
    taken on the spelling alone (GLM boundary r22 P0-1).  A receiver keeps the
    relay only while every module-scope binding of it is a proven loader or a
    registered proven source; the ``md = None`` placeholder initializer cannot
    serve a verdict, so it does not withdraw.
    """
    trusted = _SHARED_MODULES if shared is None else shared
    receivers = set()
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        func = node.func
        if not (isinstance(func, ast.Attribute) and func.attr == 'aggregate_status'):
            continue
        base = func.value
        if (isinstance(base, ast.Attribute) and base.attr in trusted
                and isinstance(base.value, ast.Name)):
            receivers.add(base.value.id)
    if not receivers:
        return frozenset()
    bindings = _module_scope_binding_map(tree)
    loaders = _loader_names(tree, rel)
    proven = _proven_source_names(tree)
    # #892 r26: a dynamic write replaces the module binding with no statement
    # the graph sees, so ``globals()['md'] = <stub>`` kept the relay trusted
    # while every ``md.common.aggregate_status`` answered from the stub (GLM
    # boundary r25 P0-1).  A ``setattr(md, '<shared>', <stub>)`` attribute write
    # is the same withdrawal at one remove (GLM concurrency r25 P1-6).
    dynamic = _dynamic_write_names(tree)
    attribute_writes = _receiver_attribute_writes(tree, receivers, trusted)
    if '*' in dynamic:
        return frozenset(receivers)
    # #892 r25: a ``global`` rebind routed through a loader name the same
    # function shadows is a stub assignment, not a load (GLM sibling r24 P1-1).
    tainted = _tainted_global_rebinds(tree, loaders)
    poisoned = set(receivers & (tainted | dynamic | attribute_writes))
    for name in receivers:
        for value in bindings.get(name, ()):
            if isinstance(value, ast.Constant) and value.value is None:
                continue
            if not (_proven_source(value, rel, proven)
                    or any(_loads_shared_module(value, module, loaders)
                           for module in trusted)):
                poisoned.add(name)
                break
    return frozenset(poisoned)


def _module_shared_names(tree, rel=None):
    """Module names that still are the shared aggregator inside *tree* (#892 r20).

    ``common.aggregate_status`` only proves a shared call while ``common`` is
    still the shared module.  The checker trusted the spelling, so a file that
    rebinds the name -- ``common = SimpleNamespace(aggregate_status=lambda
    *a: ('PASS', 0))`` -- had every call to its own stub counted as the shared
    map, and the function was enumerated, or excused, on a claim the shared map
    never made (GLM boundary r19 P1-1).  A module-scope binding keeps the trust
    only when it is the apparatus loader call naming the module; everything
    else withdraws it.  #892 r23: the loader call itself is only the one whose
    callee still resolves to the guard loader (``_loader_names``).
    """
    trusted = set(_SHARED_MODULES)
    if not isinstance(tree, ast.Module):
        return frozenset(trusted)
    loaders = _loader_names(tree, rel)
    # #892 r26: a module-scope walrus or ``match`` capture rebinds the name
    # outside the statement walk (GLM concurrency r25 P0-1).
    trusted -= _module_extra_bindings(tree)
    # #892 r27: a dynamic write into the module namespace replaces the binding
    # with no statement this walk sees, so ``globals()['common'] = stub`` kept
    # the shared name trusted for every call in the file (GLM sibling r26 P0-1
    # / concurrency r26 P1-2).
    dynamic = _dynamic_write_names(tree)
    if '*' in dynamic:
        return frozenset()
    trusted -= dynamic
    for node in _own_nodes(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            trusted.discard(node.name)
        elif isinstance(node, ast.Assign):
            for target in node.targets:
                for name in _target_names(target):
                    if name in trusted and not _loads_shared_module(node.value, name,
                                                                    loaders):
                        trusted.discard(name)
        elif isinstance(node, (ast.AnnAssign, ast.AugAssign)):
            for name in _target_names(node.target):
                if name in trusted and not _loads_shared_module(node.value, name,
                                                                loaders):
                    trusted.discard(name)
        elif isinstance(node, (ast.For, ast.AsyncFor)):
            trusted.difference_update(_target_names(node.target))
        elif isinstance(node, (ast.With, ast.AsyncWith)):
            for item in node.items:
                if item.optional_vars is not None:
                    trusted.difference_update(_target_names(item.optional_vars))
        elif isinstance(node, ast.ExceptHandler) and node.name:
            trusted.discard(node.name)
    # #892 r21: ``global common`` rebinds the module binding at runtime from
    # inside a function, out of the walk above; the spelling would stay
    # trusted for every other function in the file.  A file that declares the
    # trusted name global must keep every binding of it loader-shaped, in any
    # scope -- the module rule, applied wherever the binding actually lands.
    global_names = {name for node in ast.walk(tree)
                    if isinstance(node, ast.Global) for name in node.names}
    if trusted & global_names:
        # #892 r25: judge each function's global rebinds against that
        # function's own local names -- a locally shadowed loader makes the
        # "load" a stub call (GLM sibling r24 P1-1).
        for function in ast.walk(tree):
            if not isinstance(function, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            local_names = _function_local_names(function)
            for node in _own_nodes(function):
                names, value = _local_bound_names(node)
                for name in names:
                    if (name in trusted
                            and not (value is not None
                                     and _loads_shared_module(value, name,
                                                              loaders,
                                                              local_names))):
                        trusted.discard(name)
        trusted -= _tainted_global_rebinds(tree, loaders)
    return frozenset(trusted)


# #892 r21: helpers a function may rebind the trusted ``common`` spelling to,
# besides the apparatus loader call itself.  ``_guard.py:run_entry`` binds the
# loader directly; the negative-controls harness needs two more spellings that
# load the shared module from committed bytes / from disk, because its whole
# point is to judge a tree whose working-tree ``common.py`` may be dirty.  Each
# entry names the file that calls the helper and the helper, with the load it
# performs; the in-file self test asserts the imports and defs exist, so a name
# cannot join this registry by typo and a stub cannot hide behind a lookalike
# name in another file.
PROVEN_SHARED_SOURCES = {
    ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/negative_controls.py',
     'committed_status_contract'):
        'cross-file loader: git-shows committed HEAD common.py and evaluates '
        'only its status tables and aggregate_status',
    ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/negative_controls.py',
     'load_pristine_common'):
        'same-file loader: importlib-loads the pristine common.py from disk '
        'inside the mutation window',
    # #892 r23: the fixture's own relay receiver.  ``md = load_session_domain()``
    # in ``main`` is the binding every ``md.common.aggregate_status`` answers
    # to; the module it loads binds ``common`` through the apparatus
    # ``_guard.load`` at module scope, so a proven registration -- not the
    # loader spelling -- keeps the relay trusted (GLM boundary r22 P0-1).
    ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py',
     'load_session_domain'):
        'module loader: importlib-loads scripts/gateway-parity/session_domain.py, '
        'whose module scope binds common via the apparatus _guard.load',
}

# #892 r26: the files that *define* a cross-file proven source.  The registry
# above keys on the importing file, which said nothing about the definition it
# resolves to: ``committed_status_contract`` is imported from
# ``tests/fixtures/negative_process.py``, and that file's binding graph was
# never judged, so a same-named redefinition or a decorated stub there kept the
# importing side's trust (GLM sibling r25 #2 / boundary r25 P1-5).  Each entry
# is validated against the same single-undecorated-def rule the importing file
# gets.
PROVEN_SHARED_DEFINITIONS = {
    ('src/pilotdeck-gateway/tests/fixtures/negative_process.py',
     'committed_status_contract'),
}


def _proven_source_names(tree):
    """Registered proven-source names that still resolve to their real def (#892 r24).

    The registry keys on ``(rel, name)``, so a same-named redefinition kept the
    trust: a second ``def load_session_domain`` returning a PASS stub -- Python
    runs the last def -- still spelled the registered call while the audit read
    the stub's verdicts as the real loader's (MiniMax sibling r23 #1 / DeepSeek
    sibling r23 #1).  A name is proven only while the module binding graph has
    exactly one binding for it and that binding is a top-level ``def``/``class``
    or an ``import``; a second def, an assignment, a ``global`` rebind or a
    loop/``with`` target all leave a different marker and withdraw it.  The
    caller intersects with the registry, so this is the "can it be traced to a
    real module-level definition" half of the proof, never the whole claim.
    """
    if not isinstance(tree, ast.Module):
        return frozenset()
    dynamic = _dynamic_write_names(tree)
    if '*' in dynamic:
        # Fail closed: a non-literal ``globals()``/``exec`` write can replace
        # any binding after the walk, so no name in this file can be traced to
        # its definition (GLM boundary r24 P1-4).
        return frozenset()
    bindings = _module_scope_binding_map(tree)
    # #892 r27: the in-file half of the proof is a real *function* definition.
    # ``_module_level_defs`` also reports classes, so ``class
    # load_session_domain:`` with a stub class attribute kept the trust while
    # ``load_session_domain()`` returned an instance (GLM boundary r26 P0-5).
    defined = {node.name for node in _module_level_defs(tree)
               if not node.decorator_list
               and isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))}
    # #892 r26: an imported name is only proven when the file that *defines* it
    # is registered too -- the import form alone says nothing about the def it
    # resolves to, and the defining file's binding graph was never judged (GLM
    # sibling r25 #2 / boundary r25 P1-5).  Every other imported spelling is
    # not a definition at all (GLM concurrency r25 P1-2).  #892 r27: the import
    # must name the registered definition file, or ``from <stub> import
    # committed_status_contract`` would keep the trust while resolving to a
    # stub (DeepSeek sibling r26 P1-2).
    imported = _module_imported_names(tree)
    sources = _module_import_sources(tree)
    definition_stems = {Path(rel).stem for rel, _name in PROVEN_SHARED_DEFINITIONS}
    imported_defs = {name for _rel, name in PROVEN_SHARED_DEFINITIONS
                     if name in imported
                     and sources.get(name)
                     # #892 r30: *every* import of the name must come from a
                     # proven module -- ``any`` let a stub module of the same
                     # name ride along beside the real import (GLM sibling r29
                     # P2-2).
                     and all(source.split('.')[-1] in definition_stems
                             for source in sources[name])}
    # #892 r25: a function-local rebinding -- nested ``def``, walrus target or
    # plain assignment -- shadows the module definition for that body, and the
    # module-scope receiver scan cannot see which function a ``global`` rebind
    # came from (GLM boundary r24 P0-2 / concurrency r24 P1-2).  Withdraw the
    # name for the whole file instead of trusting the body we cannot attribute.
    local_rebound = _file_local_names(tree)
    # #892 r26: the same rebind at *module* scope through a walrus or a ``match``
    # capture leaves the statement walk blind (GLM concurrency r25 P0-1).
    extra = _module_extra_bindings(tree)
    return frozenset(name for name in defined | imported_defs
                     if bindings.get(name) == [None]
                     and name not in local_rebound
                     and name not in extra
                     and name not in dynamic)


def _single_definition(tree, name):
    """True when *name* is bound by exactly one undecorated module-level def.

    #892 r26: the cross-file half of the proven-source proof.  The file that
    defines a registered source may use dynamic writes for its own machinery
    (``negative_process.py`` installs patched globals by name), so this check
    asks only the question the registry makes: is there one real definition,
    with no second binding and no decorator replacing the function object?
    """
    if not isinstance(tree, ast.Module):
        return False
    definitions = [node for node in _module_level_defs(tree)
                   if node.name == name]
    if len(definitions) != 1 or definitions[0].decorator_list:
        return False
    if not isinstance(definitions[0], (ast.FunctionDef, ast.AsyncFunctionDef)):
        return False
    # #892 r27: the definition file may run its own dynamic machinery, but a
    # write that reaches *this* name replaces the function object the registry
    # names, so the single-def rule has to consult it (GLM boundary r26 P0-4 /
    # sibling r26 P1-3 / DeepSeek concurrency r26 P1-7).
    dynamic = _dynamic_write_names(tree)
    if '*' in dynamic or name in dynamic:
        return False
    bindings = _module_scope_binding_map(tree)
    return (bindings.get(name) == [None]
            and name not in _module_extra_bindings(tree)
            and name not in _file_local_names(tree))


def _proven_source(value, rel=None, proven=frozenset(), local=frozenset()):
    """True when *value* is a call to a registered proven shared source.

    #892 r24: both halves of the proof have to hold -- the ``(rel, name)`` key
    is registered **and** the name resolves through the module binding graph to
    a real module-level definition (``_proven_source_names``).  A name rebound
    in the enclosing function (``committed_status_contract = <stub>`` inside
    ``main``) shadows the module binding for that call, so *local* withdraws it
    the same way (DeepSeek boundary r23 #3).  ``proven`` defaults to the empty
    set, so a caller that has no binding graph fails closed instead of trusting
    the spelling.
    """
    return (isinstance(value, ast.Call) and isinstance(value.func, ast.Name)
            and (rel, value.func.id) in PROVEN_SHARED_SOURCES
            and value.func.id in proven
            and value.func.id not in local)


def _local_bound_names(node):
    """(names, value) a statement binds, for trust withdrawal (#892 r21).

    Assignments, loop/with/except targets and imports all rebind a name to
    something the module-level scan never sees; ``None`` marks bindings whose
    value is not an expression (``for``/``with`` targets, imports, handlers).
    """
    if isinstance(node, ast.Assign):
        names = [name for target in node.targets for name in _target_names(target)]
        return names, node.value
    if isinstance(node, ast.AnnAssign):
        # #892 r26: ``name: Callable[...]`` with no value annotates, it does not
        # bind -- the healthy tree uses that spelling above the real def, and
        # counting it as a second binding failed a good tree closed (GLM
        # concurrency r25 P2-7).
        if node.value is None:
            return [], None
        return _target_names(node.target), node.value
    if isinstance(node, ast.AugAssign):
        return _target_names(node.target), node.value
    if isinstance(node, (ast.For, ast.AsyncFor)):
        return _target_names(node.target), None
    if isinstance(node, (ast.With, ast.AsyncWith)):
        names = [name for item in node.items if item.optional_vars is not None
                 for name in _target_names(item.optional_vars)]
        return names, None
    if isinstance(node, ast.ExceptHandler):
        return ([node.name] if node.name else []), None
    if isinstance(node, (ast.Import, ast.ImportFrom)):
        return [alias.asname or alias.name.split('.')[0]
                for alias in node.names], None
    return [], None


def _function_shared_names(function, shared=None, rel=None, loaders=frozenset(),
                           proven=frozenset()):
    """(trusted names, locally rebound receivers) inside *function* (#892 r21).

    The r20 withdrawal walked module scope only, so a *function-scope*
    ``common = SimpleNamespace(aggregate_status=...)`` kept the spelling
    trusted and the stub's verdicts were counted as the shared map's
    (GLM sibling r20 P1-2 / boundary r20 P1-1).  Inside one function the
    module rule applies again: a local binding keeps the trust only when it
    is the apparatus loader call or a registered proven source.  Parameters
    are the caller's contract (``_guard.py:_failure(error, policy, common)``)
    -- but an assignment *inside the body* rebinds the name like any other,
    parameter or not.  Names outside the trusted set can still relay to it
    (``md.common.aggregate_status``), so every locally bound name is also
    reported as a receiver whose relay is no longer taken on the spelling
    alone; ``global``-declared receivers bind at module scope and stay the
    module scan's business (``check_cleanup.main`` rebinds ``md`` there,
    judged by ``_withdrawn_receivers`` since #892 r23).

    #892 r24: the callee side is judged against the same scope.  A proven
    source or loader name rebound in this function -- or arriving as a
    parameter -- shadows the module binding for the whole body, so the call
    is not the registered definition even though it spells the name
    (``committed_status_contract = <stub>`` then
    ``common = committed_status_contract(ROOT)``; DeepSeek boundary r23 #3).
    """
    trusted = set(_SHARED_MODULES if shared is None else shared)
    rebound = set()
    declared_global = {name for node in ast.walk(function)
                       if isinstance(node, ast.Global) for name in node.names}
    # #892 r26: a parameter *with a default* is not the caller's contract --
    # the function supplies that binding itself -- so it shadows a proven
    # source or receiver exactly like an assignment (GLM boundary r25 P1-4).
    supplied = _default_bound_parameters(function)
    # #892 r26: one helper for every local-binding form; the four hand-rolled
    # unions drifted a binding form per round (GLM simplify r25 P2-6).
    local_names = _function_local_names(function) - declared_global
    rebound |= supplied - declared_global
    # #892 r33: a nested function that rebinds an enclosing local through
    # ``nonlocal`` replaces the receiver the enclosing body reads, but the
    # outer walk sees only the nested ``def`` name.  ``_outer(common)`` with
    # ``nonlocal common; common = object()`` kept ``common.aggregate_status``
    # counted as the shared call (codex boundary r32 P0-2).
    # #892 r34: a ``nonlocal`` *declaration* alone replaces nothing -- a
    # nested reader (``nonlocal md; return md``) withdrew the enclosing relay
    # and failed a clean file (codex boundary r33 P1-7).  Only a name the
    # nested function actually writes rebinds the enclosing binding.
    for nested in ast.walk(function):
        if not isinstance(nested, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        declared, written = set(), set()
        for node in _own_nodes(nested):
            if isinstance(node, ast.Nonlocal):
                declared.update(node.names)
                continue
            names, _value = _local_bound_names(node)
            written.update(names)
        # #892 r35: ``_local_bound_names`` reads statements only, so a
        # ``nonlocal common`` rebound through a walrus or a ``match`` capture
        # counted as a declaration with no write and kept the enclosing relay
        # trusted (codex boundary r34 P0-4).
        written.update(_walrus_names(nested) | _match_capture_names(nested))
        for name in declared & written:
            trusted.discard(name)
            rebound.add(name)
    # #892 r27: a walrus, a ``match`` capture or a nested ``def`` rebinds a
    # receiver exactly like an assignment.  ``_function_local_names`` already
    # unions them for the callee side, but ``rebound`` was still built from the
    # statement walk alone, so ``(md := <stub>)`` in the body kept every
    # ``md.common.aggregate_status`` counted as the shared call (DeepSeek
    # concurrency r26 P0-1).
    rebound |= local_names & (_walrus_names(function)
                              | _match_capture_names(function)
                              | _nested_definition_names(function))
    for node in _own_nodes(function):
        names, value = _local_bound_names(node)
        for name in names:
            if name in declared_global and name not in trusted:
                continue
            if name in trusted:
                if not (value is not None
                        and (_loads_shared_module(value, name, loaders,
                                                  local_names)
                             or _proven_source(value, rel, proven,
                                               local_names))):
                    trusted.discard(name)
                    rebound.add(name)
            else:
                rebound.add(name)
    return frozenset(trusted), frozenset(rebound)


def _is_shared_call(node, shared=None, rebound=frozenset()):
    """A call to the one shared ``common.aggregate_status`` (#892 r3).

    Substring matching let a local ``def aggregate_status`` stand in for the
    shared map; the shared call is the qualified attribute, never a bare local
    name (MiniMax boundary P1-5 / DeepSeek boundary P2-5).  *shared* is the set
    of names that still denote the shared module in the calling file, from
    ``_module_shared_names``; a rebound ``common`` is not in it (GLM boundary
    r19 P1-1).  *rebound* is the set of names the enclosing function binds
    locally (#892 r21): a relayed ``md.common.aggregate_status`` whose
    receiver was rebound to something else in that function is taken on the
    spelling alone no longer.
    """
    if not isinstance(node, ast.Call):
        return False
    func = node.func
    if not (isinstance(func, ast.Attribute) and func.attr == 'aggregate_status'):
        return False
    trusted = _SHARED_MODULES if shared is None else shared
    base = func.value
    if isinstance(base, ast.Name):
        return base.id in trusted
    if not (isinstance(base, ast.Attribute) and base.attr in trusted):
        return False
    if not isinstance(base.value, ast.Name):
        # #892 r27: ``md.inner.common.aggregate_status`` walks one attribute
        # deeper than any receiver rule follows, so the spelling is not the
        # shared call instead of being trusted by default (GLM boundary r26
        # P1-8).
        return False
    return base.value.id not in rebound


def _calls_aggregate(node, shared=None, rebound=frozenset()):
    return any(_is_shared_call(n, shared, rebound) for n in ast.walk(node))


# #892 r20: the module that owns the one shared aggregator.  A file that
# rebinds the name loses the trust (``_module_shared_names``).
_SHARED_MODULES = frozenset({'common'})


# #892 r1: a hard-coded status literal may only be *fail-closed*.  The clean
# half written straight into a summary slot claims a run the shared map never
# produced, which is exactly the bypass this check exists to stop (MiniMax
# boundary P0-2 / DeepSeek boundary P1-2).  The two halves are complement sets
# of the product word family (``STATUS_WORDS``, mirroring
# ``common.BATCH_STATUS_ORDER`` plus the cleanup-verification word), so a new
# word cannot silently join neither (GLM boundary r7 P2-4 / sibling P3-6 /
# MiniMax simplify r7 P2-1).
_CLEAN_STATUS_LITERALS = frozenset({'PASS', 'PASS_WITH_SKIPS',
                                    'PASS_WITH_CLEANUP_UNVERIFIED'})
_FAIL_CLOSED_LITERALS = STATUS_WORDS - _CLEAN_STATUS_LITERALS

# A call is only a proven slot source when its callee's own return path is
# audited -- i.e. the callee is one of the registered points above.  Any other
# call is opaque and fails closed (MiniMax boundary r11 P0-1).  The exemption
# list is the opposite claim ("this point is deliberately not audited"), so it
# may not widen this set: ``report['status'] = cleanup_status(...)`` would then
# look like a proven relay while the very same callee is exempted from the
# return-path audit (MiniMax sibling r12 P1-1).  Wrappers that only forward to a
# registered point are listed one by one, **with the point they forward to**, so
# the claim is machine-checkable instead of prose.
#
# Two registries, because "exempt" and "proven" are different claims and the
# r12 rule above only holds while they cannot be confused (MiniMax boundary
# r15 P1-1):
#
# * ``PROVEN_WRAPPER_CALLEES`` maps a wrapper to the *registered point* it
#   forwards to.  Every target must be in ``AGGREGATION_POINTS``, and no wrapper
#   may also appear in the exemption list.
# * ``EXEMPT_PROVEN_CALLEES`` lists points that are exempt from the batch rule
#   yet legitimately supply one status slot, each with the measured face it
#   carries.  Every key must already be in ``NON_AGGREGATION_POINTS`` (so the
#   exemption is justified there) and none may be a wrapper.
#
# Both properties are asserted by the in-file self test, so a future entry
# cannot re-open the contradiction by simply adding a name (the check is
# ``probe892_...`` registry assertions, not this comment).
PROVEN_WRAPPER_CALLEES = {
    ('scripts/gateway-parity/session_domain.py', 'deadline_report'):
        ('scripts/gateway-parity/session_domain.py', '_deadline_report'),
}
EXEMPT_PROVEN_CALLEES = {
    ('scripts/gateway-parity/session_domain.py', 'retirement_report'):
        'error-shape face built by _retirement_budget_blocked from the round budget, not a batch',
    ('scripts/gateway-parity/session_domain.py', 'validate_calibration'):
        'six-cell calibration report built from the measured raw folder, not a child batch',
    ('scripts/gateway-parity/session_domain.py', '_retirement_network_verdict'):
        'NETWORK_FALLBACK verdict computed from the lsof observation row set',
    ('scripts/gateway-parity/session_domain.py', 'cleanup_status'):
        'VERIFIED->PASS mapping of the launcher audit processTreeCleanup face',
}
# Keyed like every other registry (``(rel, name)``): a bare name trusted a
# same-named def in *another* file, so ``_guard.py`` defining its own ``main``
# inherited the registration of ``controls/session_domain.py:main`` (GLM
# boundary r13 P0-1).  The name-only view is kept for the in-file probes, which
# are scanned under a synthetic ``<self-test>`` rel.
AUDITED_CALLEE_KEYS = (frozenset(AGGREGATION_POINTS) | frozenset(PROVEN_WRAPPER_CALLEES)
                       | frozenset(EXEMPT_PROVEN_CALLEES))
AUDITED_CALLEE_NAMES = frozenset(name for _rel, name in AUDITED_CALLEE_KEYS)


def _relays_status(node, bindings, seen=None):
    """A slot that carries an already-aggregated value.

    Names / attributes / subscripts relay whatever the shared call produced, but
    a Name is only trusted when every local assignment to it comes from the
    shared call — ``status = 'PASS' if ... else 'FAIL'`` followed by
    ``report = {'status': status}`` is the same bypass as writing the literal
    into the dict (MiniMax P0-1).  A subscript whose base *is* locally bound is
    traced the same way: ``local = {'status': 'PASS'}`` then
    ``{'status': local['status']}`` is not a relay (DeepSeek sibling P1-1).
    Literals are accepted only when fail-closed.
    """
    if isinstance(node, ast.Constant):
        return isinstance(node.value, str) and node.value in _FAIL_CLOSED_LITERALS
    if isinstance(node, (ast.Attribute, ast.Subscript)):
        return _container_relays(node, bindings, seen)
    if isinstance(node, ast.Name):
        return _name_relays(node.id, bindings, seen)
    return False


def _container_relays(node, bindings, seen=None):
    """``x['status']`` / ``x.status`` relays only when ``x`` itself relays.

    *seen* breaks the cycle ``report = {'status': report['status']}``: a name
    already being traced cannot prove anything, so the slot stays flagged
    instead of exhausting the interpreter stack.
    """
    seen = set() if seen is None else seen
    base = node.value
    if isinstance(base, ast.Call):
        return _is_shared_call(base)
    if isinstance(base, (ast.Attribute, ast.Subscript)):
        return _container_relays(base, bindings, seen)
    if not isinstance(base, ast.Name):
        return False
    if base.id in seen:
        return False
    seen.add(base.id)
    values = bindings.get(base.id)
    if not values:
        # A parameter / module global: nothing local proves where the value came
        # from, so it is not a relay -- same rule as ``_name_relays`` (GLM
        # boundary P1-2: ``report['status'] = md['status']`` walked through).
        return False
    for _lineno, value in values:
        if _calls_aggregate(value):
            continue
        if isinstance(value, ast.Dict):
            slots = _status_slot_values(value)
            if slots and all(_relays_status(slot, bindings, seen) or _calls_aggregate(slot)
                             for slot in slots):
                continue
        if isinstance(value, (ast.Attribute, ast.Subscript)) \
                and _container_relays(value, bindings, seen):
            continue
        if isinstance(value, ast.Name) and _name_relays(value.id, bindings, seen):
            continue
        return False
    return True


class _NameBindings(dict):
    """Local binding table of one function, with loop membership (#892 r35).

    A ``for`` back edge reaches a binding written *after* the use, so filtering
    the table by ``lineno <= use`` alone hides it and the call reads the
    builtin: ``dict = builtins.dict`` / ``for i in (0, 1): ... dict.update(
    {'status': 'PASS'}) ... dict = report`` stayed green (codex boundary r34
    P0-1).  The table therefore records, for every node it walked, the loops
    that enclose it; ``_bindings_at`` adds back the bindings that share a loop
    with the use.
    """

    __slots__ = ('loops_of', 'comps_of', 'comp_bound', 'imported', 'scope',
                 'slots', 'builtins_modules')

    def __init__(self):
        super().__init__()
        self.loops_of = {}
        self.comps_of = {}
        # #1953 fix-r1 round 19: the function these bindings came from, and
        # its block table, so a by-name handle can be judged by the binding in
        # force rather than by every binding ever made (codex r18 P1-4).
        self.scope = None
        self.slots = None
        # #1953 fix-r1 round 12 (GLM sibling r8 P1): ``from builtins import
        # setattr as _sa`` leaves no assignment binding, so the status ruler
        # read ``_sa`` as an unknown name and a ``_sa(report, 'status',
        # 'PASS')`` write stayed green.  Every binding of such a spelling ride
        # along on the table as ``(lineno, builtin or None)`` events, so the
        # use point can pick the one in force (codex r9 P0-2 / P0-5 / P1-6).
        self.imported = {}
        # #1953 fix-r1 round 22 (codex r21 P0-1): ``name -> [event]`` for the
        # spellings this scope binds to the ``builtins`` module itself
        # (``import builtins as bi``), so ``bi.getattr`` reads as the factory
        # the same way the literal ``builtins.getattr`` does.
        self.builtins_modules = {}
        # #1953 fix-r1 (codex r1b P1): a comprehension target lives in the
        # comprehension's own scope, but the table recorded it like any other
        # local, so ``[h for h in (operator.setitem,)]`` made an unrelated
        # ``h = lambda *a: None`` later in the same function resolve to the
        # write helper and the clean function failed.  ``(name, lineno,
        # id(value))`` -> the comprehension that owns the binding.
        self.comp_bound = {}

    def loops_at(self, node):
        if node is None:
            return frozenset()
        return frozenset(self.loops_of.get(node, ()))

    def comps_at(self, node):
        """The comprehensions *node* sits in (#1953 fix-r1).

        A generator's loop variable is evaluated before the element but its
        ``col_offset`` is to the *right* of the call, so the ordinary
        source-position rule hid it: ``[h(report, 'status', 'PASS') for h in
        [operator.setitem]]`` resolved ``h`` to nothing and the write stayed
        green (codex boundary r35 P0-2).  The whole enclosing chain is kept so
        a nested comprehension still sees the outer generator's bindings.
        """
        if node is None:
            return frozenset()
        return frozenset(self.comps_of.get(node, ()))


def _name_bindings(function, outer_imports=None):
    """Every value assigned to a local name inside *function* (#892 r1).

    Each entry keeps its line number: the exit-code rule needs the binding that
    can reach a given ``return`` (nearest preceding), not every binding ever
    made (MiniMax boundary P0-2).  #892 r35: the walk also records the
    enclosing loops of every node, because a loop back edge makes a
    textually-later binding reachable.
    """
    bindings = _NameBindings()

    # #1953: the walk order is load-bearing -- ``_exit_terminal_bindings``
    # returns the first binding of a name it meets, so the table's insertion
    # order decides which spelling an exit relay is judged by.  Keep the
    # original ``_own_nodes`` order (an explicit LIFO stack, children pushed in
    # iteration order) rather than recursing in source order.
    stack = [(function, (), ())]
    while stack:
        node, loops, comp = stack.pop()
        bindings.loops_of[node] = loops
        bindings.comps_of[node] = comp
        if isinstance(node, ast.AnnAssign):
            if node.value is not None:
                _bind_values(node.target, node.value, bindings, node.lineno)
        elif isinstance(node, ast.Assign):
            for target in node.targets:
                _bind_values(target, node.value, bindings, node.lineno)
        elif isinstance(node, (ast.For, ast.AsyncFor)):
            # ``for report in rows:`` binds a loop variable whose value is not
            # proven; without this the subscript path would treat it as a
            # module global and trust it (DeepSeek boundary P1-4).
            _bind_values(node.target, node.iter, bindings, node.lineno)
        elif isinstance(node, ast.NamedExpr):
            # ``if (code := int(...)):`` binds before the ``return code`` the
            # same way an Assign does; without this the name looked unbound and
            # the computed code was invisible (GLM boundary r7 P1-1).
            _bind_values(node.target, node.value, bindings, node.lineno)
        elif isinstance(node, ast.Match):
            # #1953 fix-r1: ``match operator.setitem: case h:`` binds ``h`` to
            # the subject (a capture is the subject or a part of it), but
            # ``_local_bound_names`` reads statements only, so ``h(...)``
            # resolved to nothing and the write vanished (codex boundary r35
            # P0-3).  Binding the capture to the subject is the fail-closed
            # reading: the value derives from it.
            for name in _match_capture_names(node):
                _bind_values(ast.Name(id=name, ctx=ast.Store()), node.subject,
                             bindings, node.lineno)
        elif isinstance(node, (ast.ListComp, ast.SetComp, ast.GeneratorExp,
                               ast.DictComp)):
            # #1953 fix-r1: a comprehension's loop variable is a binding like a
            # ``for`` target, and ``[h(report, 'status', 'PASS') for h in
            # [operator.setitem]]`` needs it to resolve the callee (codex
            # boundary r35 P0-2).
            for generator in node.generators:
                _bind_iterable(generator.target, generator.iter, bindings,
                               node.lineno, node)
        inner = loops + (node,) if isinstance(
            node, (ast.For, ast.AsyncFor, ast.While)) else loops
        inner_comp = comp + (node,) if isinstance(
            node, (ast.ListComp, ast.SetComp, ast.GeneratorExp,
                   ast.DictComp)) else comp
        for child in ast.iter_child_nodes(node):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef,
                                  ast.Lambda, ast.ClassDef)):
                continue
            stack.append((child, inner, inner_comp))

    bindings.imported = _imported_write_aliases(function, outer_imports)
    # #1953 fix-r1 round 22 (codex r21 P0-1): the ``import builtins as bi``
    # aliases ride on the same table (module-level ones merged in by
    # ``_imported_write_aliases``), so :func:`_builtins_module` can read the
    # spelling in force at the use.
    bindings.builtins_modules = getattr(bindings.imported, 'builtins_modules',
                                        {})
    # #1953 fix-r1 round 19: the status ruler's by-name handle test has to ask
    # "which binding is in force at the call", and that needs the block table
    # of the scope the table was built from.
    bindings.scope = function
    return bindings


def _local_expression_nodes(statement):
    """The nodes of *statement*'s own expressions, never a nested scope's body."""
    stack = list(ast.iter_child_nodes(statement))
    while stack:
        node = stack.pop()
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef,
                             ast.ClassDef, ast.Lambda, ast.stmt)):
            continue
        yield node
        stack.extend(ast.iter_child_nodes(node))


def _flow_marks(statements):
    """``id(node) -> (conditional, loops)`` inside one scope body (r14).

    #1953 fix-r1 round 14 (codex r10 P0-1 / P0-2 / P0-3): round 13 decided
    which binding a use reads by source line alone, which is wrong three ways.
    A binding written inside ``if``/``try`` may never run, so it cannot hide an
    earlier write helper; a loop back edge makes a binding written *after* the
    use reachable at the use; and two bindings on one physical line need the
    column to order.  *conditional* is true unless the binding is a statement
    of the very list its use is judged in, and *loops* holds the ids of the
    loops the statement sits inside -- the two facts the resolver needs.
    """
    marks = {}

    def visit(block, conditional, loops):
        for statement in block:
            marks[id(statement)] = (conditional, loops)
            inner = (loops + (id(statement),) if isinstance(
                statement, (ast.For, ast.AsyncFor, ast.While)) else loops)
            for node in _local_expression_nodes(statement):
                marks.setdefault(id(node), (conditional, loops))
            for inner_block in _statement_blocks(statement):
                visit(inner_block, True, inner)

    visit(statements, False, ())
    return marks


def _event_position(event):
    """``(line, col)`` of a binding event."""
    return (event[0], event[1])


def _binding_event_at(events, position, loops=(), anywhere=False,
                      positions=(), call_only=False):
    """The binding event *events* put in force, or ``None`` (round 14).

    Events are ``(line, col, helper, conditional, loops, certain)``.  A use
    reads the last binding that certainly runs before it; a binding nested in
    a branch does not erase an earlier write helper (fail-closed: an
    unreachable branch must not hide a write), a binding on a loop back edge
    is in reach at a use written before it, and a later *conditional* write
    helper after the dominating binding keeps the spelling a write.  *anywhere*
    resolves a use whose position is unknown -- a nested function reading an
    enclosing import at run time -- and then every binding is in reach.

    #1953 fix-r1 round 15 (codex r14 P0-1): an enclosing scope is read at
    *call* time, not at the end of the module.  *positions* holds the
    module-scope call sites of the function being resolved; when one of them
    saw a write binding, that is the answer.  Without a call site the round-14
    rule stands (the last unconditional binding decides), which is what keeps
    an unconditional non-write import after the write import clean.

    #1953 fix-r1 round 23 (codex r22 P1-2): *call_only* says every call site
    of the function is known and none of them read a write binding, so the
    answer is "not a write helper" instead of falling back to the binding at
    the end of the module.  ``import safe as bi`` before the definition plus a
    later ``import builtins as bi`` used to read the *later* import even when
    every call ran before it (false FAIL); the write-helper table keeps the
    round-14 fallback because missing a write is the worse error there.
    """
    if anywhere and positions:
        writes = []
        for entry in positions:
            if (isinstance(entry, tuple) and len(entry) == 2
                    and isinstance(entry[0], tuple)):
                _call, _call_loops = entry
            else:
                _call, _call_loops = entry, ()
            item = _binding_event_at(events, _call, _call_loops)
            if item is not None and item[2] is not None:
                writes.append(item)
        if writes:
            return min(writes, key=_event_position)
        if call_only:
            return None
    if anywhere:
        unconditional = [item for item in events if item[3] is False]
        if not unconditional:
            writes = [item for item in events if item[2] is not None]
            return min(writes, key=_event_position) if writes else None
        chosen = max(unconditional, key=_event_position)
        if chosen[2] is not None:
            return chosen
        late = [item for item in events
                if item[2] is not None
                and _event_position(item) > _event_position(chosen)]
        return min(late, key=_event_position) if late else chosen
    before = [item for item in events if _event_position(item) <= position]
    after = [item for item in events if _event_position(item) > position]
    unconditional = [item for item in before if item[3] is False]
    chosen = None
    if unconditional:
        chosen = max(unconditional, key=_event_position)
        if chosen[2] is not None:
            return chosen
        late = [item for item in before
                if item[2] is not None
                and _event_position(item) > _event_position(chosen)]
        if late:
            return max(late, key=_event_position)
    else:
        writes = [item for item in before if item[2] is not None]
        if writes:
            return max(writes, key=_event_position)
    # A loop back edge re-runs the body with the binding written after the use
    # in force, so a write helper there is reachable at the use even when an
    # unconditional non-write binding precedes it (codex r10 P0-2).
    reachable = [item for item in after
                 if item[2] is not None and set(item[4]) & set(loops)]
    if reachable:
        return min(reachable, key=_event_position)
    return chosen


class _ModuleEvents(dict):
    """Module binding events plus the call sites that read them (round 15).

    ``calls`` maps a module-level name to the positions it is called at from
    module scope, and ``outer_def`` maps ``id`` of every function to the name
    of the module-level definition it ultimately sits in.  Both are needed to
    resolve an enclosing binding at *call* time (codex r14 P0-1); a plain
    ``dict`` of events stays valid everywhere the table is only read.
    """

    def __init__(self):
        super().__init__()
        self.calls = {}
        self.outer_def = {}
        # #1953 fix-r1 round 24: names the module also reads as a value, so
        # the recorded call sites are not the whole story for them.
        self.value_uses = None
        # #1953 fix-r1 round 22 (codex r21 P0-1): ``name -> [event]`` for the
        # names this scope binds to the ``builtins`` module itself
        # (``import builtins as bi``).  The marker rides in the helper slot of
        # its own table, so the write-helper events stay untouched.
        self.builtins_modules = {}


def _module_call_positions(tree, direct=None):
    """``name -> [((line, col), loops)]`` for calls at module scope (r15).

    A call inside an ``if`` still counts -- fail-closed.

    #1953 fix-r1 round 16 (codex r15 P0-2 / P0-3): each position carries the
    loops it sits in (a call in a loop body is re-run with the bindings the
    body wrote *after* it), and a definition header is scanned even though its
    body is not -- ``def g(x=f())`` calls ``f`` at definition time, in this
    scope, which is exactly the kind of call that ran before a later rebinding.
    """
    found = {}
    stack = [(child, ()) for child in reversed(tree.body)]
    while stack:
        node, loops = stack.pop()
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef,
                             ast.ClassDef, ast.Lambda)):
            header = _definition_header(node)
            stack.extend((item, loops) for item in reversed(header))
            continue
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
            found.setdefault(node.func.id, []).append(
                ((node.lineno, node.col_offset), loops))
            if direct is not None:
                direct.add(id(node.func))
        inner = loops
        if isinstance(node, (ast.For, ast.AsyncFor, ast.While)):
            inner = loops + (id(node),)
        for child in reversed(list(ast.iter_child_nodes(node))):
            stack.append((child, inner))
    return found


def _module_value_uses(tree):
    """Names the module reads somewhere other than as a direct call (round 24).

    #1953 fix-r1 round 24 (self-check of codex r22 P1-2): the round-23
    ``call_only`` answer is only sound when the module-scope call sites of the
    function are the *only* way it can run.  A function that is also handed
    out as a value -- ``_cbs.append(main)``, a decorator, a callback
    registered before a later ``import builtins as bi`` -- runs at a time the
    static call list does not describe, so the answer falls back to the
    round-14 rule there (a real write keeps being a write).
    """
    direct = set()
    _module_call_positions(tree, direct)
    uses = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Name) and id(node) not in direct:
            uses.add(node.id)
    return uses


def _module_outer_def(tree):
    """``id(function) -> module-level def name`` for every function (r15)."""
    parents = _scope_parents(tree)
    names = {}
    for node in list(parents) + [
            item for item in ast.walk(tree)
            if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef))]:
        scope = node
        while parents.get(scope) is not None:
            scope = parents[scope]
        names[id(node)] = getattr(scope, 'name', None)
    return names


def _helper_from_value(value, events, position, loops=()):
    """The write helper a bound *value* denotes, or ``None`` (round 15).

    ``from builtins import setattr as w`` is not the only way the spelling
    becomes a write helper: ``w = setattr`` and even ``w = w`` keep it one
    (codex r14 P0-2 / P0-3).  An earlier binding of the right-hand name in
    the *same* scope decides which helper it is; a right-hand name with no
    binding there falls back to the builtin when it spells one.  Anything
    else -- an attribute, a call, a name bound to something unknown -- stays
    unresolved, and the pass keeps its round-14 answer.
    """
    if not isinstance(value, ast.Name):
        return None
    # #1953 fix-r1 round 16 (codex r15 P0-4): a binding written *after* the
    # assignment but on the same loop's back edge is in force when the loop
    # re-runs, so it may not be filtered out by position alone.
    earlier = [item for item in events.get(value.id, ())
               if _event_position(item) < position
               or (loops and set(item[4]) & set(loops))]
    chosen = _binding_event_at(earlier, position, loops)
    if chosen is not None:
        return chosen[2]
    if value.id in _MANAGED_WRITE_ATTRS or value.id in _WRITE_HELPERS:
        return value.id
    # #1953 fix-r1 round 21 (GLM boundary r19 P0-1): ``ga = getattr`` at module
    # level is a factory alias like any other, and an unrecorded helper left
    # the imported spelling unresolved inside the function that used it.
    if value.id in _FACTORY_SPELLINGS:
        return value.id
    return None


def _resolve_assignment_helpers(events, pending):
    """Fill in write helpers for assignments whose right side names one (r15).

    *pending* holds ``(event list, index, position, loops, value)``.  A small
    fixpoint lets a copy chain settle in any order (``a = setattr; w = a;
    x = w``); each pass only rewrites events that are still unresolved.
    """
    for _pass in range(len(pending) + 1):
        changed = False
        for items, index, position, loops, value in pending:
            item = items[index]
            if item[2] is not None:
                continue
            helper = _helper_from_value(value, events, position, loops)
            if helper is not None:
                items[index] = item[:2] + (helper,) + item[3:]
                changed = True
        if not changed:
            return


def _import_alias_position(alias, node):
    """The position of one ``import`` alias, not of its statement (r23).

    #1953 fix-r1 round 23 (codex r22 P1-1): every name in one ``import``
    statement used to share the statement's position, so ``import builtins as
    bi, os as bi`` recorded both bindings at the same spot; the shadow mirror
    (``_mirror_builtins_shadows``) keys on the position, could not tell the
    two apart, and dropped the later ``os`` binding -- a harmless
    ``bi.getattr`` then read as the builtin factory (false FAIL).  ``ast.alias``
    carries its own column, which keeps the aliases apart in source order.
    """
    line = getattr(alias, 'lineno', None)
    col = getattr(alias, 'col_offset', None)
    if line is None or col is None:
        # #1953 fix-r1 round 24 (GLM boundary r23 P2-1): ``ast.alias`` carries
        # its own position only from Python 3.10 on.  Older interpreters fall
        # back to the statement position, which would put two aliases of one
        # statement in a tie again, so the alias order is folded into the
        # column to keep them apart in source order.
        names = list(getattr(node, 'names', ()))
        index = next((position for position, item in enumerate(names)
                      if item is alias), 0)
        return (node.lineno, node.col_offset + index)
    return (line, col)


def _resolve_builtins_module_copies(events, copies):
    """Keep ``bi = bi`` reading as the module at its own position (r23).

    #1953 fix-r1 round 23 (GLM boundary r22 P1-2): the r22 mirror marks every
    later binding of a ``builtins`` alias spelling as a shadow, so the no-op
    ``import builtins as bi; bi = bi`` lost the module reading and the write
    behind ``bi.getattr`` disappeared.  A right side that names the module in
    force at the assignment keeps the reading at that position.

    *copies* holds ``(name, position, conditional, loops, value)`` for every
    module-scope (or function-scope) assignment that bound *name*.
    """
    for name, position, conditional, loops, value in copies:
        items = events.builtins_modules.get(name)
        if not items or not isinstance(value, ast.Name):
            continue
        source = events.builtins_modules.get(value.id)
        if not source:
            continue
        # #1953 fix-r1 round 24 (codex r23 P1 / GLM boundary r23 P1): the copy
        # reads whatever the right-hand name holds *now*.  ``bi = Safe`` in
        # between took the module reading away, and the mirror that records
        # that shadow runs after this pass, so it cannot answer here.
        module_positions = {(item[0], item[1]) for item in source}
        prior = [item for item in events.get(value.id, ())
                 if item[3] is False
                 and (_event_position(item) < position
                      or (loops and set(item[4]) & set(loops)))]
        if prior and _event_position(
                max(prior, key=_event_position)) not in module_positions:
            continue
        earlier = [item for item in source
                   if _event_position(item) < position
                   or (loops and set(item[4]) & set(loops))]
        chosen = _binding_event_at(earlier, position, loops)
        if chosen is None or chosen[2] is None:
            continue
        if (position[0], position[1]) in {(item[0], item[1])
                                         for item in items}:
            continue
        items.append((position[0], position[1], True, conditional, loops,
                      False))


def _mirror_builtins_shadows(events):
    """Give every other binding of a ``builtins`` alias its own event (r22).

    #1953 fix-r1 round 22 (codex r21 P0-1): ``import builtins as bi`` is not
    the only binding of ``bi``.  An assignment or a parameter of the same
    spelling has to be able to take the module reading away at its own
    position, exactly like the write-helper events next door; without this a
    later ``bi = other`` still read as the module.  A binding inside a branch
    stays non-dominating, so it does not erase the import (fail-closed).
    """
    for _name, items in events.builtins_modules.items():
        marked = {(item[0], item[1]) for item in items}
        for item in events.get(_name, ()):
            if (item[0], item[1]) not in marked:
                items.append(item[:2] + (None,) + item[3:])


def _module_binding_events(tree):
    """``name -> [event]`` for every binding at module scope (round 14).

    #1953 fix-r1 round 13 (codex r9 P0-1 / P0-5) added the module-level
    import alias so a function body can resolve it; round 14 keeps the events
    instead of a bare ``name -> helper`` map because the *last* module binding
    before a call decides (codex r10 P1-5: an unconditional
    ``from builtins import print as w`` after the write import means the name
    is not a write helper), while a later write binding anywhere on the way
    stays fail-closed a write.
    """
    events = _ModuleEvents()
    marks = _flow_marks(tree.body)
    pending = []
    module_copies = []

    def add(name, node, helper, certain=True, position=None):
        conditional, loops = marks.get(id(node), (True, ()))
        if position is None:
            position = (node.lineno, node.col_offset)
        items = events.setdefault(name, [])
        items.append(
            (position[0], position[1], helper, conditional, loops,
             certain))
        return items

    for level, node in _walk_scoped(tree):
        if level is not None:
            continue
        if isinstance(node, (ast.Import, ast.ImportFrom)):
            builtins_import = getattr(node, 'module', None) == 'builtins'
            for alias in node.names:
                name = alias.asname or alias.name.split('.')[0]
                helper = (alias.name if builtins_import
                          and alias.name in _IMPORTED_HELPER_NAMES
                          else None)
                position = _import_alias_position(alias, node)
                add(name, node, helper, position=position)
                if isinstance(node, ast.Import) and alias.name == 'builtins':
                    # #1953 fix-r1 round 22 (codex r21 P0-1): ``import
                    # builtins as bi`` -- the alias names the module, and
                    # ``bi.getattr`` is the builtin factory.
                    conditional, loops = marks.get(id(node), (True, ()))
                    events.builtins_modules.setdefault(name, []).append(
                        (position[0], position[1], True, conditional,
                         loops, True))
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef,
                               ast.ClassDef)):
            add(node.name, node, None)
        elif isinstance(node, ast.stmt):
            names, value = _local_bound_names(node)
            for name in names:
                items = add(name, node, None, False)
                if value is not None:
                    pending.append((items, len(items) - 1,
                                    (node.lineno, node.col_offset),
                                    items[-1][4], value))
                    module_copies.append(
                        (name, (node.lineno, node.col_offset), items[-1][3],
                         items[-1][4], value))
        elif isinstance(node, ast.NamedExpr):
            for name in _target_names(node.target):
                items = add(name, node, None, False)
                pending.append((items, len(items) - 1,
                                (node.lineno, node.col_offset),
                                items[-1][4], node.value))
                module_copies.append(
                    (name, (node.lineno, node.col_offset), items[-1][3],
                     items[-1][4], node.value))
        elif isinstance(node, ast.match_case):
            for name in _match_capture_names(node.pattern):
                add(name, node, None, False)
                items[-1] = items[-1][:1] + (-1,) + items[-1][2:]
    _resolve_assignment_helpers(events, pending)
    _resolve_builtins_module_copies(events, module_copies)
    _mirror_builtins_shadows(events)
    events.calls = _module_call_positions(tree)
    events.outer_def = _module_outer_def(tree)
    events.value_uses = _module_value_uses(tree)
    return events


class _ImportedEvents(dict):
    """``name -> [event]`` plus the names bound to the ``builtins`` module.

    #1953 fix-r1 round 22 (codex r21 P0-1): an ``import`` alias needs the same
    position-aware record the write-helper events get; a plain set of names
    would treat ``bi = other`` after ``import builtins as bi`` as the module.
    """

    def __init__(self):
        super().__init__()
        self.builtins_modules = {}


def _imported_write_aliases(function, outer_imports=None):
    """``name -> [event]`` for *function*'s bindings (round 12, r14 shape).

    ``from builtins import setattr as _sa`` leaves no assignment binding for
    :func:`_resolved_write_helper` to follow, so the status ruler read ``_sa``
    as an unknown name and ``_sa(report, 'status', 'PASS')`` stayed green
    (GLM sibling r8 P1-1).  Every binding of the name is recorded with its
    source position, whether it is conditional and the loops it sits inside,
    so the use point can pick the one in force (codex r9 P0-2 / P0-5 / P1-6,
    codex r10 P0-1..P0-3 / P1-5).  *certain* is true when the event itself
    settles the name's value (an import, a parameter, a definition); an
    ordinary assignment keeps ``False`` because its right-hand side may be
    the builtin under another spelling.
    """
    events = _ImportedEvents()
    if isinstance(function, ast.Module):
        # #1953 fix-r1 round 16 (codex r15 P0-4): the module scope is a binding
        # scope like any other.  ``from builtins import setattr as _w`` at
        # module level left no event, so ``_x = _w`` resolved to nothing and
        # ``_x(md, 'common', <stub>)`` -- a loop back edge makes the later
        # import reachable at the call -- dropped the write.
        body = list(function.body)
        parameters = ()
    elif isinstance(function, (ast.FunctionDef, ast.AsyncFunctionDef,
                               ast.Lambda)):
        body = [function.body] if isinstance(function, ast.Lambda) else function.body
        parameters = _argument_names(function.args)
    else:
        return events
    marks = _flow_marks(body)

    pending = []
    module_copies = []

    def add(name, position, helper, conditional=False, loops=(), certain=True):
        items = events.setdefault(name, [])
        items.append(
            (position[0], position[1], helper, conditional, loops, certain))
        return items

    # #1953 fix-r1 round 13 (codex r9 P0-5): a module-level import is in force
    # at every call, so it sits before every parameter (-1) and statement:
    # any local binding at or before the use still shadows it, exactly like a
    # same-scope import.  Round 14 resolves the enclosing scope's own events
    # (codex r10 P1-5): a later unconditional non-write import there means the
    # spelling is not a write helper, while a write binding anywhere on the way
    # stays fail-closed a write.
    # #1953 fix-r1 round 15 (codex r14 P0-1): the module binding is read at
    # the call, so a call that ran before a later non-write rebinding still
    # saw the write helper.
    calls = getattr(outer_imports, 'calls', None) or {}
    outer_def = getattr(outer_imports, 'outer_def', None) or {}
    func_name = outer_def.get(id(function), getattr(function, 'name', None))
    positions = calls.get(func_name, ())
    # #1953 fix-r1 round 24 (self-check of codex r22 P1-2): the call list only
    # describes the module-scope calls; a name the module also reads as a value
    # can run at a time the list does not cover, so ``call_only`` stays off.
    value_uses = getattr(outer_imports, 'value_uses', None)
    call_only = (bool(positions) and func_name is not None
                 and value_uses is not None and func_name not in value_uses)
    for name, items in (outer_imports or {}).items():
        chosen = _binding_event_at(items, None, anywhere=True,
                                   positions=positions)
        if chosen is not None:
            add(name, (-2, -2), chosen[2])
    # #1953 fix-r1 round 22 (codex r21 P0-1): a module-level ``import builtins
    # as bi`` is in force at every call the same way its write-helper events
    # are (round 13), so the alias rides along with the same resolution.
    for name, items in (getattr(outer_imports, 'builtins_modules', None)
                        or {}).items():
        # #1953 fix-r1 round 23 (codex r22 P1-2): when every call site is
        # known and none of them ran after the module import, the alias is
        # not the module inside this function (see ``_binding_event_at``).
        chosen = _binding_event_at(items, None, anywhere=True,
                                   positions=positions,
                                   call_only=call_only)
        if chosen is not None:
            events.builtins_modules.setdefault(name, []).append(
                (-2, -2, chosen[2], False, (), True))
    for name in parameters:
        # A parameter shadows at every position.
        add(name, (-1, -1), None)
    for scope, node in _walk_scoped(function):
        if (function if scope is None else scope) is not function:
            continue
        conditional, loops = marks.get(id(node), (True, ()))
        if isinstance(node, (ast.Import, ast.ImportFrom)):
            builtins_import = getattr(node, 'module', None) == 'builtins'
            for alias in node.names:
                name = alias.asname or alias.name.split('.')[0]
                position = _import_alias_position(alias, node)
                add(name, position,
                    alias.name if builtins_import
                    and alias.name in _IMPORTED_HELPER_NAMES else None,
                    conditional, loops)
                if isinstance(node, ast.Import) and alias.name == 'builtins':
                    # #1953 fix-r1 round 22 (codex r21 P0-1): the same alias
                    # record the module scope keeps, for a function-local
                    # ``import builtins as bi``.
                    events.builtins_modules.setdefault(name, []).append(
                        (position[0], position[1], True, conditional,
                         loops, True))
            continue
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef,
                             ast.ClassDef)):
            add(node.name, (node.lineno, node.col_offset), None,
                conditional, loops)
        elif isinstance(node, ast.stmt):
            names, value = _local_bound_names(node)
            for name in names:
                items = add(name, (node.lineno, node.col_offset), None,
                            conditional, loops, certain=False)
                if value is not None:
                    pending.append((items, len(items) - 1,
                                    (node.lineno, node.col_offset), loops,
                                    value))
                    module_copies.append(
                        (name, (node.lineno, node.col_offset), conditional,
                         loops, value))
        elif isinstance(node, ast.NamedExpr):
            for name in _target_names(node.target):
                items = add(name, (node.lineno, node.col_offset), None,
                            conditional, loops, certain=False)
                pending.append((items, len(items) - 1,
                                (node.lineno, node.col_offset), loops,
                                node.value))
                module_copies.append(
                    (name, (node.lineno, node.col_offset), conditional,
                     loops, node.value))
        elif isinstance(node, ast.match_case):
            for name in _match_capture_names(node.pattern):
                add(name, (node.lineno, node.col_offset), None,
                    conditional, loops, certain=False)
    # Round 15 (codex r14 P0-2 / P0-3): ``w = setattr`` / ``w = w`` keep the
    # spelling a write helper.
    _resolve_assignment_helpers(events, pending)
    _resolve_builtins_module_copies(events, module_copies)
    _mirror_builtins_shadows(events)
    return events


def _bind_values(target, value, bindings, lineno, comp=None):
    if (isinstance(target, (ast.Tuple, ast.List))
            and isinstance(value, (ast.Tuple, ast.List))
            and len(target.elts) == len(value.elts)):
        for element_target, element_value in zip(target.elts, value.elts):
            _bind_values(element_target, element_value, bindings, lineno, comp)
        return
    for name in _target_names(target):
        bindings.setdefault(name, []).append((lineno, value))
        if comp is not None:
            # #1953 fix-r1 (codex r1b P1): tag the binding with the
            # comprehension that owns it so ``_bindings_at`` can keep it out
            # of the enclosing function's scope.
            bindings.comp_bound[(name, lineno, id(value))] = comp


def _bind_iterable(target, iterable, bindings, lineno, comp=None):
    """A comprehension variable takes each element of a literal sequence.

    ``[h(report, 'status', 'PASS') for h in [operator.setitem]]`` binds ``h``
    to the write helper, not to the list; binding the iterable itself left the
    callee unresolved and the write invisible (#1953 fix-r1, codex boundary
    r35 P0-2).  Any other iterable keeps the old "value is not proven"
    reading, which is what the subscript/relay rules already assume.
    """
    if isinstance(iterable, (ast.List, ast.Tuple, ast.Set)) and iterable.elts:
        for element in iterable.elts:
            _bind_values(target, element, bindings, lineno, comp)
        return
    _bind_values(target, iterable, bindings, lineno, comp)


def _name_relays(name, bindings, seen=None):
    """True when every local assignment to *name* is shared-derived."""
    seen = set() if seen is None else seen
    if name in seen:
        return False  # an alias cycle proves nothing (fail-closed, as _container_relays)
    seen.add(name)
    values = bindings.get(name)
    if not values:
        return False  # nothing in this function proves where it came from
    for _lineno, value in values:
        if _calls_aggregate(value):
            continue
        if isinstance(value, (ast.Attribute, ast.Subscript)) \
                and _container_relays(value, bindings, seen):
            continue
        if isinstance(value, ast.Name) and _name_relays(value.id, bindings, seen):
            continue
        return False
    return True


def _nearest_binding(name, bindings, lineno):
    """The local assignment that can reach ``lineno`` (nearest preceding)."""
    entries = [item for item in bindings.get(name, ()) if item[0] <= lineno]
    return max(entries, key=lambda item: item[0])[1] if entries else None


def _own_nodes(function):
    """Nodes of *function* itself, without descending into nested definitions.

    A nested ``def`` is its own aggregation point and the enumeration reports
    it under its own name; charging its return paths to the enclosing
    registered point would misattribute the failure (MiniMax boundary P2-4).
    """
    stack = [function]
    while stack:
        node = stack.pop()
        yield node
        for child in ast.iter_child_nodes(node):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef,
                                  ast.Lambda, ast.ClassDef)):
                continue
            stack.append(child)


def _returned_names(value):
    """Names a returned expression can hand back, one level of arms deep.

    ``return payload if cond else report`` returns one of two local summaries;
    until the arms were expanded only a plain tuple of names was resolved, so
    the bound dicts behind them were never slot-checked (GLM r6 boundary P0-1).
    """
    if isinstance(value, ast.Name):
        return [value.id]
    if isinstance(value, ast.IfExp):
        arms = (value.body, value.orelse)
    elif isinstance(value, ast.BoolOp):
        arms = value.values
    elif isinstance(value, ast.Tuple):
        arms = value.elts
    else:
        return []
    return [name for arm in arms for name in _returned_names(arm)]


def _comprehension_parts(node):
    """Every expression a comprehension evaluates (#1953 fix-r1 r1b P0).

    The element is the obvious one, but every generator also evaluates its
    iterable and its ``if`` filters, and both can hold the write
    (``[x for x in (operator.setitem(report, 'status', 'PASS'),)]`` /
    ``[x for x in (1,) if operator.setitem(report, 'status', 'PASS')]``).
    Only the element was scanned, so those derivations stayed green.

    #1953 fix-r1 round 10 (codex r6 P0-2): a statically false filter kills only
    what runs *after* it.  The iterable and the filters before it are evaluated
    -- ``[m for m in (md,) if setattr(m, 'common', <stub>) if False]`` writes
    the receiver -- so the walk stops at that filter instead of returning
    nothing at all.
    """
    parts = []
    for generator in node.generators:
        parts.append(generator.iter)
        stopped = False
        for condition in generator.ifs:
            parts.append(condition)
            if _statically_false(condition):
                stopped = True
                break
        if stopped:
            return parts
    parts[:0] = ([node.key, node.value] if isinstance(node, ast.DictComp)
                 else [node.elt])
    return parts


def _comprehension_calls(node):
    """Calls inside a comprehension element, without nested definition bodies.

    ``[h(report, 'status', 'PASS') for h in [operator.setitem]]`` is an
    expression, not a statement, so the per-statement write scan never reached
    it (#1953 fix-r1).
    """
    stack = [node]
    while stack:
        item = stack.pop()
        if isinstance(item, ast.Call):
            yield item
        for child in ast.iter_child_nodes(item):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef,
                                  ast.Lambda, ast.ClassDef)):
                continue
            stack.append(child)


def _target_names(target):
    if isinstance(target, ast.Name):
        return [target.id]
    if isinstance(target, (ast.Tuple, ast.List)):
        return [name for element in target.elts for name in _target_names(element)]
    return []


def _bind_target(target, value, bound):
    """Record ``target = value`` when the value is a summary dict.

    Tuple targets pair element-wise with tuple values so ``report, code = {...}, 0``
    binds only ``report``; anything else binds every name it can see.
    """
    if (isinstance(target, (ast.Tuple, ast.List))
            and isinstance(value, (ast.Tuple, ast.List))
            and len(target.elts) == len(value.elts)):
        for element_target, element_value in zip(target.elts, value.elts):
            _bind_target(element_target, element_value, bound)
        return
    dicts = _returned_summary_dicts(value)
    if not dicts:
        return
    for name in _target_names(target):
        bound.setdefault(name, []).extend(dicts)


def _bound_summary_dicts(function):
    """Summary dicts assigned to a name inside *function* (#892 r1).

    The dominant shape in the apparatus is ``report = {'status': ...}`` followed
    by ``return report``; without this the return path was not inspected at all
    (both reviewers' top finding).  Any target name counts: a new summary point
    called ``payload`` must be enumerated too (DeepSeek P1-3), and annotated or
    tuple-target assignments bind the same way.
    """
    bound = {}
    for node in _own_nodes(function):
        if isinstance(node, ast.AnnAssign):
            if node.value is not None:
                _bind_target(node.target, node.value, bound)
        elif isinstance(node, ast.Assign):
            for target in node.targets:
                _bind_target(target, node.value, bound)
    return bound


def _returned_summary_names(value, bound):
    """Names a return statement hands back that were bound to a summary dict.

    Reuses ``_returned_names`` so the enumeration layer expands conditional and
    boolean arms exactly like the slot layer; a second, narrower implementation
    left ``return payload if cond else report`` half-enumerated (GLM simplify
    r7 P2-2).
    """
    return [name for name in _returned_names(value) if name in bound]


def _optimistic_exit(node):
    """A literal ``0`` claims a clean run (#892 r1).

    ``None`` / ``False`` are judged by the caller, which knows whether the
    point is the ``_guard._failure`` sentinel: ``SystemExit(None)`` and
    ``SystemExit(False)`` both exit 0, so anywhere else they are optimistic
    exits too (DeepSeek boundary P0-1).  Fail-closed literals (``return 2`` for
    BLOCKED, ``return 3`` for NOT_RUN) stay legal.
    """
    return (isinstance(node, ast.Constant) and isinstance(node.value, int)
            and not isinstance(node.value, bool) and node.value == 0)


def _constant_exit_zero(node):
    """``None`` / ``False`` / ``0``: every one of them exits 0.

    A float is *not* zero here: ``raise SystemExit(0.0)`` prints the value and
    exits 1 (measured on CPython 3.14), so only the exact integer 0 counts
    (GLM boundary P1-1).  A walrus in return position computes the same value
    (``return (code := 0)``).
    """
    if isinstance(node, ast.NamedExpr):
        return _constant_exit_zero(node.value)
    return (isinstance(node, ast.Constant)
            and (node.value is None or node.value is False
                 or (isinstance(node.value, int) and not isinstance(node.value, bool)
                     and node.value == 0)))


def _name_is_optimistic(name, bindings, lineno, seen=None):
    """True when the nearest binding of *name* is, or relays, a constant zero."""
    seen = set() if seen is None else seen
    if name in seen:
        return False  # an alias cycle proves nothing (fail-closed)
    seen.add(name)
    value = _nearest_binding(name, bindings, lineno)
    if value is None:
        return False
    if _constant_exit_zero(value):
        return True
    return (isinstance(value, ast.Name)
            and _name_is_optimistic(value.id, bindings, lineno, seen))


def _exit_arms(node):
    """Split a returned expression into the exit expressions it can produce.

    ``0 if cond else shared`` must be judged per arm: the shared call in the
    other arm cannot excuse this one (DeepSeek P0-1).
    """
    if isinstance(node, ast.IfExp):
        return [arm for sub in (node.body, node.orelse) for arm in _exit_arms(sub)]
    if isinstance(node, ast.BoolOp):
        return [arm for sub in node.values for arm in _exit_arms(sub)]
    return [node]


def _exit_code_shaped(node):
    if isinstance(node, ast.Constant):
        return isinstance(node.value, int) and not isinstance(node.value, bool)
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
        # ``return bool(x)`` leaves the process with 0 when x is falsy, the same
        # inline computation as ``int(...)`` (MiniMax sibling r6 P2-2).
        return node.func.id in ('int', 'bool')
    if isinstance(node, ast.IfExp):
        return _exit_code_shaped(node.body) and _exit_code_shaped(node.orelse)
    return False


def _writes_status_slot(target):
    """True when *target* assigns a status slot (#892 r2/r3).

    ``status = 'PASS'`` (the bare variable) and ``x['status']`` are the same
    write family; only the subscript form used to be seen (MiniMax boundary P0-1).
    The slot layer and the aggregation-point enumeration used to keep a private
    four-branch copy of this shape each, and the copies drifted twice (tuple
    targets MiniMax simplify r14 P2-2, attribute spelling MiniMax sibling
    r17 P1-1, each time in only one layer); both families flagged the
    duplication again in r22 (GLM simplify P2-1 + anthropic simplify P2-1),
    so both callers share this one implementation via ``_is_status_target``
    and a shape change can no longer land in one layer only.
    """
    return _is_status_target(target)


def _status_write_values(target, value, out):
    """Collect the values an assignment writes into a ``['status']`` slot."""
    _slot_write_values(target, value, out, _writes_status_slot)


def _assigns_clean_status(value, bindings, seen=None, module=None,
                          status_slot=True, rel=None):
    """True when *value* can put a clean verdict into a ``['status']`` slot.

    Registered points also write their report incrementally
    (``report['status'] = overall``) instead of returning the dict, so the same
    fail-closed rule must cover those writes: changing one line of an already
    registered point to ``'PASS'`` is otherwise a silent bypass (DeepSeek
    sibling P1-2).  A relay, a shared call and a non-batch label such as
    ``'PENDING_REVIEW'`` stay legal.
    """
    for arm in _exit_arms(value):
        if isinstance(arm, ast.NamedExpr):
            # ``report['status'] = (status := 'PASS')`` writes the same value
            # through a walrus target (MiniMax sibling P1-2).  A miss may not
            # end the walk: the other arm of the same conditional is judged on
            # its own (GLM boundary r10 P2-4).
            if _assigns_clean_status(arm.value, bindings, seen, module,
                                     status_slot, rel):
                return True
            continue
        if (isinstance(arm, ast.Constant) and isinstance(arm.value, str)
                and arm.value in _CLEAN_STATUS_LITERALS):
            return True
        folded = _folded_string(arm)
        if folded is not None and folded in _CLEAN_STATUS_LITERALS:
            # ``'PA' + 'SS'`` / ``f'{"PASS"}'`` reach the slot as the clean
            # literal without ever being a ``Constant`` node (GLM boundary r19
            # P1-2).
            return True
        if not isinstance(arm, ast.Call) and any(
                isinstance(n, ast.Constant) and isinstance(n.value, str)
                and n.value in _CLEAN_STATUS_LITERALS for n in ast.walk(arm)):
            # A clean word that survives beside a runtime operand --
            # ``'PASS' + suffix`` -- is the same inline verdict the plain
            # literal is; the return path's ``_status_literals`` already read
            # it that way, so the write path may not disagree.  A call keeps
            # its own arm below, where a proven callee stays legal.
            return True
        if _has_passed_aggregate(arm) and not _calls_aggregate(arm):
            return True
        if isinstance(arm, (ast.Attribute, ast.Subscript)) \
                and not _container_relays(arm, bindings, seen):
            # ``report['status'] = upstream['status']`` reuses a status this
            # point never computed; a subscript only relays when its base does
            # (GLM boundary P1-2: the write family had no subscript branch at
            # all, so an unproven base walked through).
            return True
        if isinstance(arm, ast.Call) and not _proven_callee(arm, bindings,
                                                            module, rel) and (
                _status_literals(arm) or isinstance(arm.func, ast.Name)
                or status_slot):
            # ``report['status'] = report.get('status', 'PASS')`` hides the
            # clean literal one call deep; the write family treated every call
            # as a proven source (GLM boundary r9 P2-3).  An opaque call to a
            # helper the audit never sees is the same bypass one level further
            # out: ``report['status'] = _clean()`` with ``_clean()`` returning
            # 'PASS' left every rule green (MiniMax boundary r11 P0-1).  The
            # bare-name head is what separates a local helper from an imported
            # module call such as ``verdict = json.loads(proc.stdout)``, which
            # stays legal.
            return True
        if isinstance(arm, ast.Name):
            if _name_assigns_clean(arm.id, bindings, seen, module, status_slot,
                                   rel):
                return True
            if not bindings.get(arm.id):
                # A name with no local binding (module constant, parameter) is
                # judged exactly like the returned-dict path: fail-closed.  The
                # write family used to trust it while ``_relays_status`` flagged
                # it, so one source had two verdicts (MiniMax sibling P1-2).
                return True
    return False


def _name_assigns_clean(name, bindings, seen=None, module=None,
                        status_slot=True, rel=None):
    """True when any local assignment to *name* can be a clean verdict."""
    seen = set() if seen is None else seen
    if name in seen:
        return False
    seen.add(name)
    return any(_assigns_clean_status(value, bindings, seen, module, status_slot,
                                     rel)
               for _lineno, value in bindings.get(name, ()))


_SENTINEL_POINTS = frozenset({('scripts/gateway-parity/_guard.py', '_failure')})


def _slot_write_values(target, value, out, matches):
    """Pair tuple targets with tuple values, collecting the matching slots."""
    if (isinstance(target, (ast.Tuple, ast.List))
            and isinstance(value, (ast.Tuple, ast.List))
            and len(target.elts) == len(value.elts)):
        for element_target, element_value in zip(target.elts, value.elts):
            _slot_write_values(element_target, element_value, out, matches)
    elif matches(target):
        out.append(value)


def _is_verdict_target(target):
    return isinstance(target, ast.Name) and target.id == 'verdict'


def _bare_verdict_values(node):
    """Values written to a bare ``verdict`` name inside a registered point.

    ``verdict = 'PASS'`` (a plain local name, not a dict key) claims the batch
    verdict exactly like ``status = 'PASS'``; the write family accepted only the
    ``status`` spelling, so the other one walked through (MiniMax sibling r6
    P2-3).  Tuple targets pair element-wise for the same reason the status
    family does: ``verdict, code = 'PASS', 0`` used to hand back the whole tuple,
    which no clean-status test recognises (GLM boundary r7 P2-3).  Enumeration
    keeps its own narrower rule on purpose: a per-case helper's ``verdict``
    local must not turn into an aggregation point.
    """
    targets = node.targets if isinstance(node, ast.Assign) else [node.target]
    values = []
    for target in targets:
        _slot_write_values(target, node.value, values, _is_verdict_target)
    return values


def _summary_slot_violations(summary, bindings, lineno):
    """Judge every entry of a summary dict (#892 r3).

    Covers duplicate ``status``/``verdict`` keys and ``{**literal}`` expansions:
    both read as clean to a report consumer while the old code ignored them.
    """
    problems = []
    for key, value in _dict_pairs(summary):
        if key is None:
            if _relays_status(value, bindings) or _calls_aggregate(value):
                continue
            problems.append(f'summary unpack {ast.unparse(value)[:60]} at line {lineno}')
            continue
        if _status_key(key):
            if _relays_status(value, bindings) or _calls_aggregate(value):
                continue
            problems.append(f'status slot {ast.unparse(value)[:60]} at line {lineno}')
    return problems


def _name_feeds_shared(name, function):
    """True when *name* is handed to the shared call (#892 r3).

    ``result = {'status': 'PASS', 'rule': None}`` in a per-case verdict is a
    legitimate value as long as the batch routes it through
    ``common.aggregate_status(result['status'])``; the same literal in a summary
    that is printed and never aggregated is the bypass.
    """
    for node in _own_nodes(function):
        if _is_shared_call(node):
            for child in ast.walk(node):
                if isinstance(child, ast.Name) and child.id == name:
                    return True
    return False


def _direct_bindings(stmt, name):
    """Values *stmt* itself assigns to *name*, without entering nested blocks."""
    found = {}
    if isinstance(stmt, ast.AnnAssign):
        if stmt.value is not None:
            _bind_values(stmt.target, stmt.value, found, stmt.lineno)
    elif isinstance(stmt, ast.Assign):
        for target in stmt.targets:
            _bind_values(target, stmt.value, found, stmt.lineno)
    elif isinstance(stmt, (ast.For, ast.AsyncFor)):
        _bind_values(stmt.target, stmt.iter, found, stmt.lineno)
    return found.get(name, ())


def _child_blocks(stmt):
    """Statement lists nested directly inside *stmt*."""
    blocks = [value for _field, value in ast.iter_fields(stmt)
              if isinstance(value, list) and value and isinstance(value[0], ast.stmt)]
    blocks.extend(handler.body for handler in getattr(stmt, 'handlers', ()))
    blocks.extend(case.body for case in getattr(stmt, 'cases', ()))
    return blocks


def _statement_rebinds_shared(stmt, name):
    """True when running *stmt* always leaves *name* bound by the shared call."""
    direct = _direct_bindings(stmt, name)
    if direct:
        return all(_calls_aggregate(value) for _lineno, value in direct)
    if isinstance(stmt, ast.If):
        return bool(stmt.orelse) and _block_rebinds_shared(stmt.body, name) \
            and _block_rebinds_shared(stmt.orelse, name)
    if isinstance(stmt, (ast.With, ast.AsyncWith)):
        return _block_rebinds_shared(stmt.body, name)
    if isinstance(stmt, ast.Try):
        if stmt.finalbody:
            return _block_rebinds_shared(stmt.finalbody, name)
        if stmt.orelse and not _block_rebinds_shared(stmt.orelse, name):
            return False
        return _block_rebinds_shared(stmt.body, name) and all(
            _block_rebinds_shared(handler.body, name) for handler in stmt.handlers)
    return False


def _block_rebinds_shared(stmts, name):
    """True when some statement of *stmts* rebinds *name* from the shared call.

    A later constant rebinding inside the same block cannot hide behind this:
    that binding is judged on its own, and its own tail has no shared rebind.
    """
    return any(_statement_rebinds_shared(stmt, name) for stmt in stmts)


def _is_system_exit_raise(node, aliases=None):
    """``raise SystemExit`` / ``raise SystemExit(...)``: both leave with 0.

    An alias for the exception class -- ``from builtins import SystemExit as
    SE; raise SE(0)`` -- is the same raise under another name and resolves
    through ``_exit_target`` like the call family (GLM boundary r16 P0-1).
    """
    exc = node.exc
    callee = exc.func if isinstance(exc, ast.Call) else exc
    return _exit_target(callee, aliases) == 'builtins.SystemExit'


def _exits_within(stmt, aliases=None):
    """True when *stmt* can return or exit by itself, at any depth.

    A propagating exception is not an optimistic exit: it never returns 0, and a
    handler that rebinds the exit code is already part of the overwrite proof.
    ``raise SystemExit(...)`` is the exception, because it *does* exit.
    """
    for node in ast.walk(stmt):
        if isinstance(node, ast.Return):
            return True
        if isinstance(node, ast.Raise) and _is_system_exit_raise(node, aliases):
            return True
        if isinstance(node, ast.Call) and _is_exit_call(node, aliases):
            return True
    return False


def _overwritten_before_exit(later, name, aliases=None):
    """True when a later sibling rebinds *name* from the shared call in time.

    A ``return`` / ``raise`` between the zero binding and the overwrite leaves a
    path that still exits 0, and an exit inside the overwriting statement can run
    before the rebind, so neither shape excuses the initializer (MiniMax
    boundary P0-1: the earlier "any later sibling" rule was a real false PASS).
    """
    for stmt in later:
        if _statement_rebinds_shared(stmt, name):
            return not _exits_within(stmt, aliases)
        if _exits_within(stmt, aliases):
            return False
    return False


def _optimistic_binding_sites(function, name, aliases=None):
    """Linenos where a constant-zero binding of *name* can reach a return.

    ``code = 0`` as an initializer that every following path overwrites with the
    shared result is not an optimistic exit; ``code = 0`` inside one branch,
    while another branch is shared, still is (DeepSeek boundary P0-1 /
    concurrency P1-1).  Only later siblings of the *same* block can excuse a
    binding: a rebind elsewhere in an enclosing block does not dominate a
    ``return`` that runs before it.
    """
    sites = set()

    def visit(stmts):
        for index, stmt in enumerate(stmts):
            safe = _overwritten_before_exit(stmts[index + 1:], name, aliases)
            for lineno, value in _direct_bindings(stmt, name):
                if _constant_exit_zero(value) and not safe:
                    sites.add(lineno)
            for block in _child_blocks(stmt):
                visit(block)

    visit(function.body)
    return sites


def _exit_terminal_bindings(name, bindings, seen=None):
    """Terminal ``(owner, lineno, value)`` bindings reachable from *name*.

    A plain reassignment chain (``rc = 0; relay = rc; return relay``) is followed
    to the binding that actually produces the value; an alias cycle stops without
    proving anything (fail-closed).
    """
    seen = set() if seen is None else seen
    if name in seen:
        return []
    seen.add(name)
    terminals = []
    for lineno, value in bindings.get(name, ()):
        if isinstance(value, ast.Name) and value.id in bindings:
            terminals.extend(_exit_terminal_bindings(value.id, bindings, seen))
        else:
            terminals.append((name, lineno, value))
    return terminals


def _inline_exit_call(value):
    """The spelling of an inline exit-code computation, or None.

    ``int(...)`` and ``bool(...)`` both leave the process with a code the shared
    map never produced; a plain non-zero int literal stays legal.  The direct
    return arm already accepted both (``_exit_code_shaped``), while this relay
    arm knew only ``int``, so ``code = bool(x); return code`` was one source
    with two verdicts (MiniMax boundary r7 P0-1 / sibling r7 P1-1).
    """
    if isinstance(value, ast.Call) and isinstance(value.func, ast.Name):
        return value.func.id if value.func.id in ('int', 'bool') else None
    return None


def _container_holds_zero(value, bindings):
    """``holder = {'code': 0}; return holder['code']`` is an inline zero."""
    if not (isinstance(value, ast.Subscript) and isinstance(value.value, ast.Name)):
        return False
    for _lineno, container in bindings.get(value.value.id, ()):
        if isinstance(container, ast.Dict):
            if any(item is not None and (_constant_exit_zero(item) or _inline_exit_call(item))
                   for item in container.values):
                return True
    return False


def _helper_exits_zero(target, lookup, seen, aliases=None):
    """True when *target* has a path that leaves the process with 0.

    A bare ``return`` and a body that simply ends both hand ``None`` back, and
    an entry point that raises ``SystemExit(None)`` exits 0: the point-level
    rule already judged that shape (``no return or exit path``), so the
    delegation family may not pass it one call away (GLM boundary r10 P0-1).
    Exit spellings inside the helper body -- ``sys.exit(0)`` in statement
    position, a bare ``raise SystemExit`` -- are invisible to a walk that only
    looks at ``Return`` nodes, yet they leave the process at 0 just the same
    (GLM boundary r10 P1-2).  The helper's own body aliases are added to the
    module-level ones, so ``bail = sys.exit; bail(0)`` reads the same here
    (GLM boundary r16 P0-1).
    """
    hands_back = False
    raises_plain = False
    bindings = _name_bindings(target)
    aliases = _binding_exit_aliases(bindings, aliases, target)
    for item in ast.walk(target):
        if isinstance(item, ast.Return):
            hands_back = True
            if item.value is None:
                return True
            if _calls_aggregate(item.value):
                continue  # this return routes its own code through the map
            if _constant_exit_zero(item.value) or any(
                    _constant_exit_zero(arm) for arm in _exit_arms(item.value)):
                return True
            if any(isinstance(arm, ast.Name)
                   and _exit_name_violation(arm.id, bindings, target, lookup,
                                           aliases)
                   for arm in _exit_arms(item.value)):
                # ``code = 0; return code`` inside the helper is the same leak
                # as a literal zero: the point-level rule already judges the
                # relayed name, so the delegation family may not call it clean
                # one call away (MiniMax boundary r12 P0-1).
                return True
            if _delegated_exit(item.value, lookup, seen, aliases=aliases):
                return True
        elif isinstance(item, ast.Raise):
            if not _is_system_exit_raise(item, aliases):
                # ``raise ValueError(...)`` leaves the process non-zero, which
                # is not an optimistic exit: the point-level rule accepts any
                # ``raise`` as an exit path, so the delegation family may not
                # call the same helper "exits 0" (GLM boundary r11 P3-3 /
                # MiniMax boundary r11 P1-2).
                raises_plain = True
                continue
            hands_back = True
            argument = (item.exc.args[0]
                        if isinstance(item.exc, ast.Call) and item.exc.args
                        else None)
            if argument is None or _constant_exit_zero(argument):
                return True
        elif isinstance(item, ast.Call) and _is_exit_call(item, aliases):
            hands_back = True
            argument = item.args[0] if item.args else None
            if argument is None or _constant_exit_zero(argument):
                return True
    # A body that can fall off the end hands ``None`` back, and
    # ``SystemExit(None)`` exits 0: the raise flag may not veto that path
    # (GLM boundary r12 P0-2).
    return _body_completes(target.body, aliases) or (not hands_back and not raises_plain)


def _body_completes(body, aliases=None):
    """True when execution can reach the end of *body* (handing ``None`` back).

    ``raise ValueError`` on one branch and no ``return`` on the other leaves a
    path that ends the helper normally, so the process still exits 0; a global
    "this helper raises" flag vetoed exactly that shape (GLM boundary r12
    P0-2).  The scan is sequential -- an ordinary statement does not end the
    body -- and follows ``if`` / ``with`` / ``try`` bodies, because a point
    whose last statement is a ``try`` that always returns does not fall off its
    end.  Loops and ``match`` are treated as able to complete, which is the
    fail-closed direction.
    """
    for statement in body:
        if isinstance(statement, (ast.Return, ast.Raise)):
            return False
        if isinstance(statement, ast.Expr) and isinstance(statement.value, ast.Call) \
                and _is_exit_call(statement.value, aliases):
            return False
        if isinstance(statement, ast.If):
            # Both arms terminating makes everything after the ``if``
            # unreachable, so the body cannot reach its end either; otherwise
            # execution may continue with the next statement.
            if not _body_completes(statement.body, aliases) \
                    and not _body_completes(statement.orelse, aliases):
                return False
            continue
        if isinstance(statement, ast.With):
            if not _body_completes(statement.body, aliases):
                return False
            continue
        if isinstance(statement, ast.Try):
            # ``else`` runs only when the ``try`` body ends normally, while a
            # handler that falls off its own end rejoins the code after the
            # whole statement -- so the handler path is not gated by ``else``
            # and ``else`` is not gated by the handler path.  Folding them into
            # one flag let ``else: return ...`` cancel a completing handler,
            # and a non-completing ``finally`` could not veto at all
            # (MiniMax boundary r18 P0-1).
            completes = _body_completes(statement.body, aliases)
            if statement.orelse:
                completes = completes and _body_completes(
                    statement.orelse, aliases)
            if any(_body_completes(handler.body, aliases)
                   for handler in statement.handlers):
                completes = True
            if statement.finalbody and not _body_completes(
                    statement.finalbody, aliases):
                completes = False
            if not completes:
                return False
            continue
        # Anything else may raise, but assuming it does would wave through the
        # fall-off end this rule exists for; keep scanning (fail-closed).
    return True


def _callee_names(call, bindings):
    """Every local name a call's callee can stand for.

    ``f = _exit_code; return f(report)`` reaches the same helper through a
    binding, and ``helpers.exit_code(report)`` names it as an attribute; the
    delegation family used to know only the bare ``_exit_code(report)``
    spelling (GLM boundary r9 P1-2).
    """
    func = call.func
    names = []
    if isinstance(func, ast.Name):
        names.append(func.id)
        for _lineno, bound in bindings.get(func.id, ()):
            if isinstance(bound, ast.Name):
                names.append(bound.id)
    elif isinstance(func, ast.Attribute):
        names.append(func.attr)
    return names


def _nested_def_exits_zero(value, function, bindings=None, aliases=None):
    """``def _exit(): return 0`` called from a registered point."""
    if function is None or not (isinstance(value, ast.Call)
                                and isinstance(value.func, (ast.Name, ast.Attribute))):
        return False
    bindings = bindings or {}
    lookup = {node.name: node for node in ast.walk(function)
              if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))}
    target = next((lookup[name] for name in _callee_names(value, bindings)
                   if name in lookup), None)
    if target is None or target is function:
        return False
    return _helper_exits_zero(target, lookup,
                              set(_callee_names(value, bindings)), aliases)


def _delegated_exit(value, module, seen=None, bindings=None, aliases=None):
    """A registered point returning a same-module helper's own exit code.

    ``return _exit_code(report)`` hides the computation one call away: the
    return arm is a plain ``Call``, and a helper that never touches the shared
    function carries no marker, so the enumeration never sees it.  A helper that
    routes its own code through the map stays legal per *return*, not per
    mention: a decoy call whose result is thrown away does not excuse the
    ``return 0`` beside it, and a helper that delegates again is followed to the
    leaf (GLM concurrency P2-1 / MiniMax boundary P1-2).
    """
    if module is None or not isinstance(value, ast.Call):
        return False
    if not isinstance(value.func, (ast.Name, ast.Attribute)):
        return False
    if isinstance(value.func, ast.Attribute) and not _calls_aggregate(value):
        # ``helpers.exit_code(report)``: the attribute head names a member of
        # some other module, and resolving it by its bare attribute name found
        # this module's own same-named def -- so ``return helpers.main(report)``
        # was audited as if it were the local ``main`` (GLM boundary r15 P1-1).
        # Only the shared aggregator's own attribute spelling is proven.
        return True
    seen = set() if seen is None else seen
    names = _callee_names(value, bindings or {})
    target = next((module[name] for name in names if name in module), None)
    if target is None:
        # An attribute callee the module does not define (``helpers.exit_code``)
        # is not proven to route through the shared map, so it fails closed the
        # same way an unknown bare name does (GLM boundary r9 P1-2).
        return True
    name = target.name
    if name in seen:
        return False  # a delegation cycle proves nothing (fail-closed)
    seen.add(name)
    return _helper_exits_zero(target, module, seen, aliases)


def _exit_argument_violation(argument, bindings, function, module, point,
                             lineno, label, aliases=None):
    """Three-way judgement of one exit argument (#892 r23).

    The ``sys.exit(...)`` / ``os._exit(...)`` statement form and the
    ``raise SystemExit(...)`` form used to keep a private copy of this
    argument judgement each, so a shape taught to one spelling never reached
    the other; both spellings now share this one implementation (anthropic
    simplify r22 P2-3), with *label* naming the spelling in the
    optimistic-exit message.
    """
    if argument is None or _constant_exit_zero(argument) or (
            isinstance(argument, ast.Name)
            and _name_is_optimistic(argument.id, bindings, lineno)):
        return f'inline optimistic exit ({label}) at line {lineno}'
    if (_status_literals(argument) or _has_passed_aggregate(argument)) \
            and not _calls_aggregate(argument):
        return f'inline status/exit {ast.unparse(argument)[:60]} at line {lineno}'
    if _exit_provenance(argument, bindings, function, module, point, aliases):
        return f'inline exit code {ast.unparse(argument)[:60]} at line {lineno}'
    return None


def _exit_call_violation(call, bindings, function, module, point, lineno,
                         aliases=None):
    """Judge the argument of ``sys.exit(...)`` / ``os._exit(...)`` (#892 r7).

    ``sys.exit(...)`` takes the same exit-code provenance as ``return``; the
    statement form and the ``return sys.exit(0)`` form share this one judgement
    so the two spellings cannot drift apart again (GLM r6 sibling P1-1/P1-2).
    The three-way judgement itself lives in ``_exit_argument_violation``,
    shared with the ``raise SystemExit(...)`` form (r23).
    """
    argument = call.args[0] if call.args else None
    return _exit_argument_violation(
        argument, bindings, function, module, point, lineno,
        ast.unparse(call)[:50], aliases)


def _exit_provenance(argument, bindings, function, module, point,
                     aliases=None):
    """Why an exit call's argument still computes its code inline (#892 r6).

    ``sys.exit(...)``, ``os._exit(...)`` and ``raise SystemExit(...)`` take the
    same exit-code provenance as a ``return``: a zero relayed through a name, a
    zero read back out of a local dict, a nested helper that returns 0, a
    delegated helper and a boolean expression all reach exit 0 without the
    shared map.  The r5 fixes only reached the ``Return`` arm, so those shapes
    walked straight through the exit-call branches (GLM sibling P1-2/P1-3).
    A literal zero and a status literal are reported by the caller with its own
    wording, so they are skipped here.
    """
    for arm in _exit_arms(argument):
        if _calls_aggregate(arm):
            continue
        if _status_literals(arm) or _has_passed_aggregate(arm):
            continue
        if _constant_exit_zero(arm) and point not in _SENTINEL_POINTS:
            # ``sys.exit(0 if cond else 1)``: the earlier skip trusted the
            # caller to report a literal zero, but the callers only test the
            # whole argument, so an IfExp arm walked through (GLM r6 sibling
            # P1-1).  A plain literal is still reported by the caller first.
            return True
        if isinstance(arm, ast.Name):
            if _exit_name_violation(arm.id, bindings, function, module,
                                    aliases):
                return True
            continue
        if (_container_holds_zero(arm, bindings)
                or _nested_def_exits_zero(arm, function, bindings, aliases)
                or _delegated_exit(arm, module, bindings=bindings,
                                   aliases=aliases)):
            return True
        if isinstance(arm, (ast.Attribute, ast.Subscript)):
            # ``sys.exit(helpers.EXIT_OK)`` / ``raise SystemExit(CODES[key])``:
            # r19 taught the *return* arm that a code spelled as an attribute or
            # an untracked subscript has no provenance, but this function -- the
            # one that judges the same argument for every exit call -- got no
            # such branch, so the two spellings of one rule disagreed and the
            # exit-call side stayed blind (GLM sibling r19 P1, MiniMax boundary
            # r19 P0).  A subscript whose base routes through the shared call is
            # already skipped above by ``_calls_aggregate``; everything else
            # fails closed.
            return True
        if isinstance(arm, (ast.UnaryOp, ast.Compare)):
            return True
        if isinstance(arm, ast.Call) and _exit_code_shaped(arm):
            return True
    return False


def _module_delegated_exit(value, module, function, bindings, aliases=None):
    """``code = _exit_code(report)`` where the helper lives at module level.

    ``_delegated_exit`` judges the direct ``return _exit_code(report)``
    spelling, and ``_nested_def_exits_zero`` the nested one, but the value of a
    local name bound to a *module-level* helper had no branch at all: the same
    family had three verdicts (MiniMax boundary r10 P1-2).  A callee that is a
    nested def of this function stays with ``_nested_def_exits_zero`` so the two
    rules cannot contradict each other.
    """
    if module is None or not isinstance(value, ast.Call):
        return False
    if _calls_aggregate(value):
        # ``status, code = common.aggregate_status(...)``: the shared call is
        # the proof, not a delegation to judge (the healthy tree binds the
        # pair this way in five places, so this arm may not fire on it).
        return False
    if function is not None:
        local = {node.name for node in ast.walk(function)
                 if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))}
        if any(name in local for name in _callee_names(value, bindings)):
            return False
    return _delegated_exit(value, module, bindings=bindings, aliases=aliases)


def _exit_name_violation(name, bindings, function=None, module=None,
                         aliases=None):
    """Why an exit code travelling through *name* is inline, or None (#892 r3).

    A conditional expression that can produce 0 is always inline computation.
    A plain ``code = 0`` is inline too unless every following path overwrites it
    with the shared result: a shared assignment in *another* branch does not
    excuse it, because the zero branch still returns the default and the process
    exits 0 (DeepSeek boundary P0-1 / concurrency P1-1).  The same judgement is
    applied through plain reassignment chains, an ``int(...)`` computed into a
    local, a zero read back out of a local dict, a nested helper that returns a
    constant zero (GLM concurrency P1-1 / MiniMax boundary P1-4), and a module
    helper whose own code the local relays (MiniMax boundary r10 P1-2).
    """
    sites = {}
    terminals = _exit_terminal_bindings(name, bindings)
    if not terminals:
        # A module-level name or a parameter proves nothing about where the
        # code came from, and the status side already fails closed on exactly
        # this shape (``_name_relays`` returns False for an empty binding):
        # one source may not have two verdicts (GLM boundary r7 P1-1).
        return 'unbound'
    for owner, lineno, value in terminals:
        if isinstance(value, ast.Name):
            # ``_exit_terminal_bindings`` only falls through to a Name when that
            # name has no local binding, so the origin is invisible: fail closed
            # exactly like the direct ``return EXIT_OK`` spelling, which the r9
            # round already judged ``unbound``.  ``code = EXIT_OK; return code``
            # used to pass with no violation (MiniMax boundary r9 P0-1).
            return 'unbound'
        if isinstance(value, (ast.IfExp, ast.BoolOp)) and any(
                _constant_exit_zero(arm) for arm in _exit_arms(value)):
            return 'conditional'
        if _constant_exit_zero(value):
            if function is not None:
                if owner not in sites:
                    sites[owner] = _optimistic_binding_sites(function, owner,
                                                            aliases)
                if lineno not in sites[owner]:
                    continue  # an initializer that a later statement overwrites
            return 'literal'
        if isinstance(value, ast.Attribute):
            # ``code = helpers.EXIT_OK`` proves nothing about the origin, for
            # the same reason the direct ``return EXIT_OK`` spelling is
            # ``unbound`` (GLM boundary r18 P0-1).
            return 'attribute'
        if isinstance(value, ast.Subscript) and not _calls_aggregate(value):
            return 'subscript'
        spelling = _inline_exit_call(value)
        if spelling:
            return spelling + '()'
        if _container_holds_zero(value, bindings):
            return 'subscript'
        if _nested_def_exits_zero(value, function, aliases=aliases):
            return 'nested def'
        if _module_delegated_exit(value, module, function, bindings, aliases):
            return 'delegated'
    return None


_EXIT_MODULES = ('sys', 'os', 'builtins', 'posix')
_EXIT_FUNCTION_NAMES = ('exit', '_exit', 'quit')
_EXIT_CALL_TARGETS = frozenset(
    f'{module}.{name}' for module in _EXIT_MODULES
    for name in _EXIT_FUNCTION_NAMES)


def _exit_target(node, aliases=None):
    """The canonical exit spelling a callee expression stands for, or None.

    ``sys.exit`` / ``os._exit`` / ``exit`` / ``quit`` are the direct spellings;
    an alias -- ``import sys as s``, ``from sys import exit as bail``,
    ``from builtins import SystemExit as SE``, ``bail = sys.exit`` in a body --
    reaches the very same exit under another name.  Every predicate below used
    to match on the literal name, so an alias was invisible to all of them at
    once: a registered point could exit with ``s.exit(0)`` in statement
    position while every rule stayed green, and an unregistered function whose
    only evidence was that call was never enumerated (GLM boundary r16 P0-1).
    One resolver keeps the three predicate families from drifting apart.
    """
    aliases = aliases or {}
    if isinstance(node, ast.Name):
        if node.id in ('exit', 'quit', 'SystemExit'):
            return f'builtins.{node.id}'
        target = aliases.get(node.id)
        if target is not None and target.rsplit('.', 1)[-1] in (
                _EXIT_FUNCTION_NAMES + ('SystemExit',)):
            return target
        return None
    if (isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name)):
        module = aliases.get(node.value.id, node.value.id)
        if module not in _EXIT_MODULES:
            return None
        if node.attr in _EXIT_FUNCTION_NAMES:
            return f'{module}.{node.attr}'
        if node.attr == 'SystemExit' and module == 'builtins':
            return 'builtins.SystemExit'
    return None


def _import_exit_aliases(nodes):
    """Exit-module aliases bound by ``import`` statements in *nodes* (#892 r18).

    ``import sys as s`` binds *s* to the ``sys`` module (so ``s.exit`` is the
    same exit), ``from sys import exit as bail`` binds *bail* to the function
    itself, and ``from builtins import SystemExit as SE`` binds the class.  The
    module header and a function body use the very same statements, so both
    sources read them through this one helper: the earlier split (header only)
    left ``def publish(...): import sys as s; s.exit(0)`` resolving *s* as an
    unknown bare name and skipping every rule (GLM boundary r17 P0-1).
    """
    aliases = {}
    for node in nodes:
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name in _EXIT_MODULES:
                    aliases[alias.asname or alias.name] = alias.name
        elif isinstance(node, ast.ImportFrom) and node.module in _EXIT_MODULES:
            for alias in node.names:
                if alias.name in _EXIT_FUNCTION_NAMES:
                    aliases[alias.asname or alias.name] = f'{node.module}.{alias.name}'
                elif alias.name == 'SystemExit':
                    aliases[alias.asname or alias.name] = 'builtins.SystemExit'
    return aliases


def _module_exit_aliases(tree):
    """Local names a module binds to an exit spelling (#892 r17).

    Two sources: the module header's ``import`` statements (``import sys as
    s``, ``from sys import exit as bail``, ``from builtins import SystemExit
    as SE``) and module-level assignments whose value resolves to an exit
    spelling (``bail = sys.exit``).  The second was missing, so a module-level
    ``bail = sys.exit; bail(0)`` walked through every rule the function-body
    spelling was just taught to catch (GLM sibling r17 P1-1).  A rebinding
    inside a function body is read from that function's own imports and
    bindings by ``_binding_exit_aliases``; a rebinding inside a *called* helper
    is listed as a known boundary in the fixture README.
    """
    aliases = {}
    if not isinstance(tree, ast.Module):
        return aliases
    # Module scope includes the ``try:`` / ``if`` headers that guard an
    # optional import (``_own_nodes`` skips nested defs, so a function-local
    # alias never leaks up here).  Walking direct children only left
    # ``try:\n import sys as s\nexcept ImportError:\n s = None`` unresolved
    # (GLM boundary r17 P0-1, the ``_guard.py`` idiom).
    for name, target in _import_exit_aliases(_own_nodes(tree)).items():
        aliases[name] = target
    # Two passes so ``bail = sys.exit`` followed by ``quit_now = bail`` sees
    # the first alias before it is read (the chain is short and acyclic in
    # practice; a cycle simply stops resolving).
    for _pass in range(2):
        for node in _own_nodes(tree):
            if isinstance(node, ast.Assign):
                pairs = [(target, node.value) for target in node.targets]
            elif isinstance(node, ast.AnnAssign) and node.value is not None:
                pairs = [(node.target, node.value)]
            else:
                continue
            for target, value in pairs:
                resolved = _exit_target(value, aliases)
                if resolved is None:
                    continue
                for name in _target_names(target):
                    aliases[name] = resolved
    return aliases


def _binding_exit_aliases(bindings, aliases=None, function=None):
    """Aliases the function body itself binds (#892 r17/r18).

    ``bail = sys.exit`` comes from the assignment bindings; ``import sys as
    s`` / ``from sys import exit as bail`` come from the function's own
    ``import`` statements, which are not values and therefore never reach
    ``_name_bindings`` (GLM boundary r17 P0-1).
    """
    merged = dict(aliases or {})
    if function is not None:
        merged.update(_import_exit_aliases(_own_nodes(function)))
    # A single pass in binding order resolved ``bail = sys.exit; other = bail``
    # only when ``bail`` happened to come first in the dict, so the same two
    # lines had two verdicts depending on source order (MiniMax boundary r19
    # P1).  Iterate to a fixed point: the chain is short and a cycle simply
    # stops resolving.
    pending = [(name, value) for name, items in (bindings or {}).items()
               for _lineno, value in items]
    while pending:
        remaining = []
        progressed = False
        for name, value in pending:
            target = _exit_target(value, merged)
            if target is None:
                remaining.append((name, value))
                continue
            if merged.get(name) != target:
                merged[name] = target
                progressed = True
        if not progressed:
            break
        pending = remaining
    return merged


def _is_exit_call(call, aliases=None):
    """``sys.exit(...)`` / ``os._exit(...)`` / bare ``exit(...)``: no return.

    ``from sys import exit`` makes the bare-name call the same exit, so the
    spelling is accepted too (GLM boundary P1-1).  ``quit`` is the same site
    builtin and raises the same ``SystemExit``: leaving it out let
    ``quit(0)`` in statement position skip every rule (MiniMax boundary r13
    P0-1).  Module and body aliases resolve through ``_exit_target`` so the
    whole family -- ``s.exit``, ``bail``, ``builtins.exit``, ``posix._exit``
    -- lands on the same predicate (GLM boundary r16 P0-1).
    """
    return _exit_target(call.func, aliases) in _EXIT_CALL_TARGETS


def _proven_summary_source(value, bindings, module=None, rel=None):
    """True when the audit can see which slots *value* carries.

    A dict literal (or an arm of one), a call to an audited point, and a name
    this function bound to either are visible; a parameter, a module global or
    an attribute is not.  Reading the slots is what the bypass rule depends on,
    so an opaque operand must fail closed instead of passing silently (GLM r6
    boundary P1-2, r7 P1-2).  Every call used to count as proven, contradicting
    the sentence above: ``report.update(_clean())`` with an unregistered
    ``_clean()`` returning ``'PASS'`` wrote a clean status nobody audited
    (MiniMax boundary r11 P0-1).  A callee now counts only when it is one of
    the registered points (its own return path is audited there) or when the
    call itself routes through the shared function.
    """
    if _returned_summary_dicts(value):
        return True
    if isinstance(value, ast.Call):
        return _proven_callee(value, bindings, module, rel)
    if isinstance(value, ast.Name):
        return any(_returned_summary_dicts(item)
                   or (isinstance(item, ast.Call)
                       and _proven_callee(item, {}, module, rel))
                   for _lineno, item in bindings.get(value.id, ()))
    return False


def _proven_callee(call, bindings, module=None, rel=None):
    """True when *call*'s result has a slot source the audit can see (r11).

    The registries are keyed by ``(rel, name)``, so a bare name is not enough:
    every other delegation judgement (``_delegated_exit``, ``scan_aggregation_
    tree``) resolves the callee in its own module first.  Matching on the name
    alone trusted any ``def record(x): return 'PASS'`` in any file that shared a
    registered point's name (GLM boundary r12 P1-1), and a name-only set still
    let a local ``main`` inherit another file's registration (GLM boundary r13
    P0-1).  With a real ``rel`` the key must match exactly; the in-file probes
    are scanned under a synthetic ``<self-test>`` rel and keep the name rule.
    """
    if _calls_aggregate(call):
        return True
    if isinstance(call.func, ast.Attribute):
        # Same shape as ``_delegated_exit``: an attribute head may name any
        # module member, so matching its bare name against the ``(rel, name)``
        # registry trusted ``helpers.cleanup_status(...)`` inside the file that
        # registers ``cleanup_status`` (GLM boundary r15 P1-1).
        return False
    names = _callee_names(call, bindings or {})
    if rel is None or rel.startswith('<'):
        return any(name in AUDITED_CALLEE_NAMES for name in names)
    return any((rel, name) in AUDITED_CALLEE_KEYS for name in names)


def _unproven_expansion(call, bindings, module=None, rel=None):
    """``update(x)`` / ``update(**x)`` whose slots the function never built.

    ``update(x)`` carries the same keys as ``update(**x)``; judging only the
    keyword spelling left the positional one -- the shape the healthy tree
    itself uses with a helper call -- invisible (GLM boundary r7 P1-2).  Lists
    and tuples of pairs are read directly by ``_call_written_values``.
    """
    if call.func.attr != 'update':
        return False
    for argument in call.args:
        if isinstance(argument, (ast.Dict, ast.List, ast.Tuple)):
            continue
        if not _proven_summary_source(argument, bindings, module, rel):
            return True
    for keyword in call.keywords:
        if keyword.arg is not None:
            continue
        if not _proven_summary_source(keyword.value, bindings, module, rel):
            return True
    return False


_WRITE_HELPERS = ('setattr', 'setitem', 'setdefault', 'update')

# #1953 fix-r1 round 21 (GLM boundary r19 P0-1 / sibling r19 P1-1): ``getattr``
# is not a write helper by itself, but the alias tables have to record it like
# one.  ``from builtins import getattr as _ga`` leaves no assignment binding,
# so the import event was stored with a ``None`` helper and both the callee
# split and the by-name handle test read ``_ga`` as an unknown name -- a status
# write behind an imported factory stayed green.  Same for a module-level
# ``ga = getattr`` merged into the function's event table.
_FACTORY_SPELLINGS = ('getattr',)

# #1953 fix-r1 round 22 (GLM simplify r21 P2-1): the member set an imported
# spelling has to belong to was spelled out by hand in three event tables and
# had already drifted (one site was missing ``_MANAGED_WRITE_ATTRS``).  One
# place names it now.  ``_scope_import_events`` deliberately keeps the narrower
# ``_MANAGED_WRITE_ATTRS`` test: its consumer asks "is this callee a *setter*",
# and ``getattr`` is a read factory when it is handed three arguments, so
# recording it there would invent writes.
_IMPORTED_HELPER_NAMES = frozenset(_WRITE_HELPERS) | frozenset(
    _MANAGED_WRITE_ATTRS) | frozenset(_FACTORY_SPELLINGS)


def _bindings_at(bindings, name, node=None):
    """The bindings of *name* a use at *node* can see (#892 r34/r35).

    A name reads the binding that precedes the use, not the last one in the
    file: ``dict = report; dict.update({'status': 'PASS'}); dict =
    builtins.dict`` read the trailing rebinding as the builtin and dropped the
    write (codex boundary r33 P0-1).  #892 r35: "precedes" is a *source
    position*, not a line -- the r34 spelling put the call and the rebind on
    one physical line and ``max`` broke the ``lineno`` tie on walk order -- and
    a binding written after the use is still reachable through a loop back
    edge when both sit in the same loop (codex boundary r34 P0-1 / GLM
    concurrency P1-2).  An int *node* keeps the old line-only meaning.
    """
    values = (bindings or {}).get(name) or ()
    # #1953 fix-r1 (codex r1b P1): a comprehension target is not a function
    # local.  Java-/C-style scoping would leak it here and an unrelated later
    # binding of the same spelling then resolved together with it, which both
    # invented writes and hid the real (single) value.  Only a use that sits
    # inside the owning comprehension may see it.
    comp_bound = getattr(bindings, 'comp_bound', None)
    if node is None:
        if comp_bound is None:
            return list(values)
        return [(lineno, value) for lineno, value in values
                if (name, lineno, id(value)) not in comp_bound]
    if isinstance(node, ast.AST):
        limit = (getattr(node, 'lineno', 0), getattr(node, 'col_offset', -1))
        loops_at = getattr(bindings, 'loops_at', None)
        use_loops = loops_at(node) if loops_at is not None else frozenset()
        comps_at = getattr(bindings, 'comps_at', None)
        use_comp = comps_at(node) if comps_at is not None else frozenset()
    else:
        limit = (node, None)
        loops_at = None
        use_loops = frozenset()
        comps_at = None
        use_comp = frozenset()
    visible = []
    for lineno, value in values:
        if comp_bound is not None:
            owner = comp_bound.get((name, lineno, id(value)))
            if owner is not None and owner not in use_comp:
                continue
        if limit[1] is None:
            if lineno <= limit[0]:
                visible.append((lineno, value))
            continue
        position = (lineno, getattr(value, 'col_offset', -1))
        if position <= limit:
            visible.append((lineno, value))
        elif use_loops and loops_at is not None \
                and use_loops & loops_at(value):
            visible.append((lineno, value))
        elif use_comp and comps_at is not None \
                and use_comp & comps_at(value):
            # The comprehension's own loop variables are evaluated before the
            # element that uses them, whatever their column says.
            visible.append((lineno, value))
    return visible


def _builtin_receiver(node, bindings, depth=0, seen=None):
    """True when *node* denotes a builtin type/module (#892 r30).

    ``dict.update(report, ...)`` / ``operator.setitem(report, ...)`` name their
    target first; the bound spelling names it on the left of the dot.  The
    resolver follows one local alias, so ``_d = dict; _d.update(report, ...)``
    is read the same way.
    """
    # #892 r34: a cycle guard instead of a hop cap -- a chain longer than the
    # cap silently read as "not a write helper" (``b50 = b49 = ... =
    # __builtins__``; codex boundary r33 P0-6).
    seen = set() if seen is None else seen
    if id(node) in seen:
        return False
    seen.add(id(node))
    if isinstance(node, ast.Name):
        # #892 r32: a local that shadows the builtin name is not the builtin.
        # Reading ``dict`` as the builtin dropped the real target argument and
        # left ``dict = {...}; dict.update({'status': ...})`` invisible (GLM
        # boundary r31 P0-3).
        values = _bindings_at(bindings, node.id, node)
        if values:
            # #892 r33: the name denotes the *last* binding at run time, not
            # any binding it ever had.  Asking whether a historical one was the
            # builtin left ``dict = builtins.dict; dict = report;
            # dict.update({'status': 'PASS'})`` invisible (codex boundary
            # r32 P0-4).  #892 r34: "last" means the last one *before* the
            # use.  #892 r35: when a loop back edge (or two arms) puts more
            # than one binding in reach, "the last one" is not a static fact;
            # the name is the builtin only when *every* candidate is, so an
            # ambiguous read fails closed instead of excusing the write
            # (codex boundary r34 P0-1).
            return all(_builtin_receiver(value, bindings, depth + 1, set(seen))
                       for _line, value in values)
        if node.id in ('dict', 'type', 'object', 'builtins', 'operator',
                       '__builtins__'):
            return True
        return False
    if isinstance(node, ast.Attribute) and node.attr in ('dict', 'type', 'object'):
        return (isinstance(node.value, ast.Name)
                and node.value.id in ('builtins', '__builtins__'))
    return False


def _resolved_write_helper(node, bindings, depth=0, seen=None):
    """The builtin write callable *node* denotes through local aliases (#892 r30).

    ``sa = setattr; sa(report, 'status', 'PASS')`` and
    ``from builtins import setattr as _sa`` bind the same callable; the status
    ruler compared the bare name only and read neither (GLM sibling r29 P1-7).
    """
    seen = set() if seen is None else seen
    if id(node) in seen:
        return None
    seen.add(id(node))
    if isinstance(node, ast.Name):
        if node.id in _WRITE_HELPERS or node.id in _FACTORY_SPELLINGS + ('partial',):
            # #892 r32: ``g = getattr; g(report, '__setitem__')(...)`` binds
            # the helper through the same names the bare spelling uses; the
            # status ruler stopped at the four canonical names (codex sibling
            # r31 P2-11).  #1953 fix-r1 round 22 (GLM boundary r21 P3-4): the
            # factory half now reads the one constant, so extending
            # ``_FACTORY_SPELLINGS`` cannot leave this test behind.
            return node.id
        # #892 r35: every sibling binding gets its own path guard.  Sharing
        # one ``seen`` across them let the first path mark a node the second
        # path also needed, so ``h = a; h = a; h(...)`` resolved to
        # ``{'setattr', None}`` and the write vanished (codex boundary r34
        # P0-2 / GLM concurrency r34 P0-1).
        # #1953 fix-r1 round 12: the spelling may be a builtin write helper
        # this scope imported under a new name.  The use point decides which
        # binding is in force: an assignment *before* the call wins, but one
        # written after it cannot erase the import (codex r9 P0-2 / P0-5), and
        # a later non-write import replaces the helper (codex r9 P1-6).
        imported = getattr(bindings, 'imported', None) or {}
        events = imported.get(node.id, ())
        loops = tuple(id(item) for item in
                      getattr(bindings, 'loops_of', {}).get(node, ()))
        chosen = _binding_event_at(
            events, (getattr(node, 'lineno', -1),
                     getattr(node, 'col_offset', -1)), loops)
        local = _bindings_at(bindings, node.id, node)
        if chosen is not None:
            if chosen[2] is not None:
                return chosen[2]
            if chosen[5]:
                # #1953 fix-r1 round 14 (codex r10 P1-5): the binding in force
                # is a non-write import/parameter/definition, so the spelling
                # is not a write helper.  An ordinary assignment stays
                # *uncertain* and falls through to the value rules below.
                return None
        found = {_resolved_write_helper(value, bindings, depth + 1, set(seen))
                 for _line, value in _bindings_at(bindings, node.id, node)}
        if len(found) == 1:
            return found.pop() or None
        # #1953 fix-r1: a name with more than one reachable binding is not
        # "not a write helper".  ``h = lambda *a: None`` then ``h =
        # operator.setitem`` resolved to ``{'setitem', None}`` and the real
        # write vanished (codex boundary r35 P0-2).  When any reachable
        # binding is a write helper the callee is never treated as safe; the
        # spelling is only used to split the arguments, and the three-argument
        # form is the same for both managed names.
        writes = found & set(_WRITE_HELPERS)
        if writes:
            return 'setitem' if 'setitem' in writes else min(writes)
        return None
    if isinstance(node, ast.Attribute):
        # ``obj.__setattr__(...)`` / ``dict.__setitem__(obj, ...)`` /
        # ``operator.setitem(obj, ...)`` -- the dunder and the builtin names
        # are the same write; the arity tells bound from unbound.
        attr = node.attr.strip('_')
        if attr in _WRITE_HELPERS:
            return attr
    if isinstance(node, ast.Call):
        # ``getattr(obj, '__setattr__')('status', value)``
        callee = node.func
        if _getattr_spelling(callee, bindings):
            # #1953 fix-r1 round 19 (GLM boundary r17 P0-1): the callee may be
            # an alias of ``getattr`` (``ga = getattr; ga(obj, '__setattr__')``)
            # and the literal check let the call resolve to nothing, so the
            # bound handle it produced wrote an invisible status slot.
            name = 'getattr'
        elif ((isinstance(callee, ast.Name) and callee.id == 'partial')
              or (isinstance(callee, ast.Attribute) and callee.attr == 'partial')):
            # #892 r32: ``functools.partial(setattr, report)('status', value)``
            # is the same write; the main write rule already decoded partials
            # and the status ruler stayed one spelling behind (GLM boundary
            # r31 P1-5).
            name = 'partial'
        else:
            # #892 r35: the callee sits on the same alias path, so it shares
            # the cycle guard; dropping it started a fresh walk per hop and a
            # two-node alias cycle (``a = b(); b = a()``) recursed to the
            # interpreter limit.
            name = _resolved_write_helper(callee, bindings, depth + 1, seen)
        if name == 'partial' and node.args:
            inner = _resolved_write_helper(node.args[0], bindings, depth + 1,
                                           seen)
            if inner in _WRITE_HELPERS:
                return inner
            return None
        if name == 'getattr' and len(node.args) in (2, 3):
            literal = node.args[1]
            if isinstance(literal, ast.Constant) and isinstance(literal.value, str):
                attr = literal.value.strip('_')
                if attr in ('setattr', 'setitem'):
                    return attr
    if isinstance(node, ast.Subscript):
        # #892 r30: ``__builtins__['setattr'](report, 'status', ...)`` is the
        # builtin call when the fixture runs as ``__main__``.
        # #892 r32: the alias ``bi = __builtins__; bi['setattr']`` names the
        # same table (GLM sibling r31 P3-5).
        literal = node.slice
        if (isinstance(literal, ast.Constant) and isinstance(literal.value, str)
                and _name_resolves_to_builtins(node.value, bindings)
                and literal.value.strip('_') in _WRITE_HELPERS):
            return literal.value.strip('_')
    return None


def _name_resolves_to_builtins(node, bindings, depth=0, seen=None):
    """True when *node* is the ``builtins`` module table (#892 r32).

    #1953 fix-r1 round 22 (GLM sibling r21 P1-1): the bindings are read with
    :func:`_values_in_force`, like :func:`_builtins_module` does since round
    20.  ``_bindings_at`` returns every reachable binding, so a certain
    overwrite (``bi = __builtins__`` then ``bi = <something else>``) kept the
    superseded one and the subscript below decoded a write that is not there.
    """
    seen = set() if seen is None else seen
    if not isinstance(node, ast.Name) or id(node) in seen:
        return False
    seen.add(id(node))
    if node.id in ('builtins', '__builtins__'):
        return True
    # #892 r35: per-path guard, same regression as ``_resolved_write_helper``
    # -- one sibling's visit must not blind the next.
    return any(_name_resolves_to_builtins(value, bindings, depth + 1, set(seen))
               for value in _values_in_force(bindings, node.id, node))


def _partial_bound_arguments(node, bindings, depth=0, seen=None):
    """Leading positional arguments a ``partial`` handle pre-binds (#892 r33).

    ``functools.partial(setattr, report)`` returns a callable whose first
    argument is already ``report``; the status ruler read only the invocation's
    own arguments, so a pre-bound target/key made the write invisible.
    Returns ``None`` when *node* is not a partial at all.
    """
    seen = set() if seen is None else seen
    if id(node) in seen:
        return None
    seen.add(id(node))
    if isinstance(node, ast.Name):
        found = None
        # #892 r35: per-path guard here too -- the sibling loop reused one
        # ``seen`` and dropped the later candidate of the same binding.
        for _line, value in _bindings_at(bindings, node.id, node):
            got = _partial_bound_arguments(value, bindings, depth + 1,
                                           set(seen))
            if got is None:
                continue
            if found is not None and [ast.dump(item) for item in got] != [
                    ast.dump(item) for item in found]:
                return None
            found = got
        return found
    if isinstance(node, ast.Call):
        callee = node.func
        is_partial = ((isinstance(callee, ast.Name) and callee.id == 'partial')
                      or (isinstance(callee, ast.Attribute) and callee.attr == 'partial')
                      or _resolved_write_helper(callee, bindings, depth + 1) == 'partial')
        if is_partial and node.args:
            inner = _resolved_write_helper(node.args[0], bindings, depth + 1)
            if inner in _WRITE_HELPERS:
                return list(node.args[1:])
    return None


def _partial_unbound_target(node, bindings):
    """True when a ``partial`` handle pre-binds an *unbound* builtin method.

    ``functools.partial(dict.update, report, {...})`` names the mapping first,
    exactly like ``dict.update(report, ...)``; the expansion must drop that
    argument instead of reading it as a written slot (#892 r34, codex boundary
    r33 P0-5).
    """
    if not (isinstance(node, ast.Call) and node.args):
        return False
    first = node.args[0]
    return isinstance(first, ast.Attribute) and _builtin_receiver(first.value, bindings)


def _values_in_force(bindings, name, use):
    """The bindings of *name* in force at *use* (#1953 fix-r1 round 20).

    A name reads the binding that certainly ran before the use, and a single
    latest one speaks for it only when the block table proves it ran: a
    conditional overwrite, a loop back edge or a scope the table cannot place
    keeps every visible binding, the conservative answer for both rulers that
    ask this question (codex r19 P0-1 / P1-4).

    #1953 fix-r1 round 19 factored this out of ``_bound_handle_name``; round
    20 reuses it for the ``getattr`` spelling so that both ask it the same way.
    """
    values = _bindings_at(bindings, name, use)
    if not values:
        return []
    latest = max((line, getattr(value, 'col_offset', -1))
                 for line, value in values)
    group = [value for line, value in values
             if (line, getattr(value, 'col_offset', -1)) == latest]
    scope = getattr(bindings, 'scope', None)
    if len(group) == 1 and scope is not None:
        slots = getattr(bindings, 'slots', None)
        if slots is None:
            slots = _scope_block_slots(scope)
            bindings.slots = slots
        if not _certain_before(slots[0], slots[1], group[0], use):
            group = [value for _line, value in values]
    return group


# #1953 fix-r1 round 20 (codex r19 P1-3): ``builtins.getattr`` is the builtin
# factory; ``other.getattr`` is whatever that object defines and must not be
# read as it.  Only these spellings of the module itself count.
_BUILTINS_MODULE_NAMES = ('builtins', '__builtins__')


def _builtins_module(node, bindings, seen=None):
    """True when *node* names the ``builtins`` module (#1953 fix-r1 round 20).

    #1953 fix-r1 round 22 (codex r21 P0-1): ``import builtins as bi`` binds the
    module under a new spelling, and only the two literal names below counted,
    so ``bi.getattr`` read as whatever an unknown object defines and the status
    write it spells stayed green.  The import tables record these aliases
    (:func:`_module_binding_events` / :func:`_imported_write_aliases`); the
    binding in force at the use decides, so a certain rebinding of the name to
    something else still ends the reading.
    """
    seen = set() if seen is None else seen
    if id(node) in seen:
        return False
    seen.add(id(node))
    if not isinstance(node, ast.Name):
        return False
    values = (_values_in_force(bindings, node.id, node)
              if bindings is not None else [])
    if values and any(_builtins_module(value, bindings, seen)
                      for value in values):
        return True
    events = (getattr(bindings, 'builtins_modules', None)
              or {}).get(node.id, ())
    if events:
        loops = tuple(id(item) for item in
                      getattr(bindings, 'loops_of', {}).get(node, ()))
        chosen = _binding_event_at(
            events, (getattr(node, 'lineno', -1),
                     getattr(node, 'col_offset', -1)), loops)
        if chosen is not None:
            return chosen[2] is not None
    if values:
        return False
    return node.id in _BUILTINS_MODULE_NAMES


def _getattr_spelling(node, bindings, seen=None):
    """True when *node* names the builtin ``getattr`` (#1953 fix-r1 round 19).

    GLM boundary r17 P0-1: the bound-handle test accepted only the literal
    spelling, so ``ga = getattr; f = ga(report, '__setattr__'); f('status',
    'PASS')`` was decoded as the *unbound* builtin, its two arguments were
    read as three-argument ones and the status write vanished -- a false
    PASS.  ``_resolved_write_helper`` already follows these aliases for the
    callee split; this asks the same question of the handle's value.

    #1953 fix-r1 round 20 (codex r19 P1-3 / P1-4): the resolver read every
    binding a name ever had, so a factory alias that was certainly overwritten
    (``ga = getattr; ga = lambda ...; f = ga(...)``) still counted, and any
    attribute called ``getattr`` (``other.getattr``) was taken for the builtin.
    Both invented writes on clean files.  The name now reads the binding in
    force at its own use point, and the attribute form requires a ``builtins``
    receiver; an ambiguous binding set keeps the write direction.
    """
    seen = set() if seen is None else seen
    if id(node) in seen:
        return False
    seen.add(id(node))
    if isinstance(node, ast.Name):
        values = (_values_in_force(bindings, node.id, node)
                  if bindings is not None else [])
        if values and any(_getattr_spelling(value, bindings, seen)
                          for value in values):
            return True
        # #1953 fix-r1 round 21 (GLM boundary r19 P0-1 / sibling r19 P1-1):
        # an import alias (``from builtins import getattr as _ga``) or a
        # module-level ``ga = getattr`` leaves no assignment binding in the
        # function's table; the event table records the spelling in force.
        #
        # #1953 fix-r1 round 22 (codex r21 P0-2 / GLM boundary r21 P0-1): the
        # table is a *second* source, not a fallback for an empty assignment
        # list.  One assignment of any other value -- ``_ga = print`` before
        # the import, or ``if flag: _ga = dict`` after it, which
        # ``_values_in_force`` widens to the whole group -- short-circuited
        # the import event and the write behind it vanished.  The event in
        # force at the use decides which spelling is live.
        events = (getattr(bindings, 'imported', None) or {}).get(node.id, ())
        if events:
            loops = tuple(id(item) for item in
                          getattr(bindings, 'loops_of', {}).get(node, ()))
            chosen = _binding_event_at(
                events, (getattr(node, 'lineno', -1),
                         getattr(node, 'col_offset', -1)), loops)
            if chosen is not None:
                return chosen[2] == 'getattr'
        if values:
            return False
        return node.id == 'getattr'
    if isinstance(node, ast.Attribute):
        return (node.attr == 'getattr'
                and _builtins_module(node.value, bindings, seen))
    return False


def _bound_handle_value(value, bindings):
    """True when *value* spells a write handle already bound to a receiver.

    #1953 fix-r1 round 17 (GLM sibling r16 P1-2): ``_f = report.__setattr__``
    and ``_f = getattr(report, '__setattr__')`` hold the *bound* method, so
    ``_f('status', 'PASS')`` carries the key and the value as its only two
    arguments exactly like the inline spelling.  The status ruler read a
    by-name handle as the unbound builtin and dropped the two-argument write.

    #1953 fix-r1 round 19 (GLM boundary r17 P0-1): the callee may itself be an
    alias of ``getattr`` (``ga = getattr; f = ga(report, '__setattr__')``);
    :func:`_getattr_spelling` resolves it so that write is not lost.
    """
    if isinstance(value, ast.Attribute):
        return value.attr in _MANAGED_WRITE_ATTRS
    if isinstance(value, ast.Call):
        if not _getattr_spelling(value.func, bindings):
            return False
        if len(value.args) not in (2, 3):
            return False
        literal = value.args[1]
        return (isinstance(literal, ast.Constant)
                and literal.value in _MANAGED_WRITE_ATTRS)
    return False


def _bound_handle_name(node, bindings, call, seen=None):
    """True when the name *node* holds a receiver-bound write handle at *call*.

    #1953 fix-r1 round 18 (codex r17 P0-2): the handle may be handed over by
    name first -- ``g = getattr(report, '__setattr__'); f = g;
    f('status', 'PASS')`` -- and only the direct binding was decoded, so the
    status write vanished from the audit.  Follow name bindings recursively,
    with a visited set so a cycle cannot spin.

    #1953 fix-r1 round 19 (codex r18 P1-4 / P0-2): every hop used to read
    *every* binding the name ever had, which invented a write for
    ``g = report.__setitem__; g = print; f = g; f('status', 'PASS')``, and a
    fixed depth cap dropped a real one past eight hops.  The hop now reads the
    binding in force.  An ambiguous set -- a conditional overwrite, a loop back
    edge, a scope the block table cannot place -- falls back to "any hop is a
    handle", the conservative direction this ruler wants (the false PASS it
    exists to catch costs more than the false FAIL).  Recursion terminates on
    the visited set alone.

    #1953 fix-r1 round 20 (codex r19 P0-1): "in force" is the position of the
    *assignment that handed the value on*, not the final call.  ``g =
    report.__setitem__; f = g; g = print; f('status', 'PASS')`` copies the
    handle into ``f`` before the overwrite, so resolving the hop at the call
    read ``g`` as ``print`` and the real write vanished.
    """
    if not isinstance(node, ast.Name) or bindings is None:
        return False
    seen = set() if seen is None else seen
    if id(node) in seen:
        return False
    seen.add(id(node))
    for value in _values_in_force(bindings, node.id, call):
        if _bound_handle_value(value, bindings):
            return True
        if isinstance(value, ast.Name) and _bound_handle_name(
                value, bindings, value, seen):
            return True
    return False


def _call_written_values(call, bindings=None):
    """Values a ``update(...)`` / ``setdefault(...)`` call writes into a slot.

    Every argument shape counts: a dict literal, a list/tuple of ``(key,
    value)`` pairs, keyword arguments (``update(status='PASS')``) and a keyword
    expansion (``update(**{'status': 'PASS'})``).  ``setdefault`` carries the
    key and the value as two positionals, so it is read directly instead of
    through ``_returned_summary_dicts`` (DeepSeek boundary P0-2 / sibling P1-2:
    the earlier code only saw dict literals, leaving the documented
    ``setdefault`` support as dead code).  ``setattr(obj, 'status', value)`` and
    ``obj.__setitem__('status', value)`` write the very same slot under a third
    and fourth spelling, and both were read by nothing: a registered point
    could make its status write with either and stay green (GLM boundary r16
    P1-1).
    """
    written = []
    func = call.func
    helper = _resolved_write_helper(func, bindings)
    if helper in ('setattr', 'setitem'):
        # Bound form: ``obj.__setitem__('status', value)`` /
        # ``getattr(obj, '__setattr__')('status', value)``.  Unbound form:
        # ``dict.__setitem__(obj, 'status', value)`` /
        # ``object.__setattr__(obj, 'status', value)`` /
        # ``operator.setitem(obj, 'status', value)`` /
        # ``builtins.setattr(obj, 'status', value)`` / an alias of the builtin.
        # #892 r33: a ``partial`` handle pre-binds its leading positional
        # arguments, so ``p = functools.partial(setattr, report);
        # p('status', 'PASS')`` and
        # ``functools.partial(operator.setitem, report, 'status')('PASS')``
        # write the same slot as the inline spelling.  The ruler expanded only
        # the call's own arguments and dropped the bound ones (codex boundary
        # r32 P0-5 / GLM boundary r32 P1-2).
        prebound = _partial_bound_arguments(func, bindings)
        arguments = list(call.args) if prebound is None else prebound + list(call.args)
        if isinstance(func, ast.Attribute) and func.attr.startswith('__'):
            bound = len(arguments) == 2
        elif isinstance(func, ast.Call) and prebound is None:
            bound = True
        else:
            bound = len(arguments) == 2 and (
                isinstance(func, ast.Attribute)
                or (prebound is None
                    and _bound_handle_name(func, bindings, call)))
        if len(arguments) == 3:
            key, value = arguments[1], arguments[2]
        elif bound and len(arguments) == 2:
            key, value = arguments[0], arguments[1]
        else:
            return written
        if _status_key(key):
            written.append(value)
        return written
    if helper == 'setdefault' or (isinstance(func, ast.Attribute)
                                  and func.attr == 'setdefault'):
        # ``dict.setdefault(obj, 'status', value)`` names its target first and
        # the key second; reading ``args[0]`` as the key saw the *object* and
        # dropped the write (GLM sibling r29 P1-7).  #892 r34: a partial handle
        # pre-binds leading arguments here too (``functools.partial(
        # dict.setdefault, report, 'status')('PASS')``; codex boundary r33
        # P0-5), and an unbound builtin target keeps its first slot.
        prebound = _partial_bound_arguments(func, bindings)
        arguments = list(call.args) if prebound is None else prebound + list(call.args)
        if prebound is not None and _partial_unbound_target(func, bindings) and arguments:
            arguments = arguments[1:]
        if len(arguments) == 3:
            key, value = arguments[1], arguments[2]
        elif len(arguments) == 2:
            key, value = arguments[0], arguments[1]
        else:
            return written
        if _status_key(key):
            written.append(value)
        return written
    # Only an ``update``-shaped call writes the slots its arguments spell:
    # ``report.update({...})`` / ``dict.update(report, ...)`` / an alias of
    # either.  Reading every call's dict arguments made ``save(path, {'status':
    # ...})`` a status write (r30 first cut) -- an unrelated helper taking a
    # summary stays unrelated.
    if helper != 'update' and not (isinstance(func, ast.Attribute)
                                   and func.attr == 'update'):
        return written
    arguments = list(call.args)
    prebound = _partial_bound_arguments(func, bindings)
    if prebound is not None:
        # #892 r34: ``functools.partial(dict.update, report, {'status': ...})()``
        # writes the same slot as the inline spelling (codex boundary r33 P0-5).
        arguments = prebound + arguments
        if _partial_unbound_target(func, bindings) and arguments:
            arguments = arguments[1:]
    elif (isinstance(func, ast.Attribute) and func.attr == 'update'
            and arguments and _builtin_receiver(func.value, bindings)):
        # ``dict.update(report, ...)``: the first argument is the target, not a
        # value written into it.
        arguments = arguments[1:]
    for argument in arguments:
        for summary in _returned_summary_dicts(argument):
            written.extend(_status_slot_values(summary))
        if isinstance(argument, (ast.List, ast.Tuple)):
            for element in argument.elts:
                if isinstance(element, (ast.Tuple, ast.List)) and len(element.elts) == 2:
                    key = element.elts[0]
                    if _status_key(key):
                        written.append(element.elts[1])
    for keyword in call.keywords:
        if keyword.arg is None:
            for summary in _returned_summary_dicts(keyword.value):
                written.extend(_status_slot_values(summary))
        elif keyword.arg in _STATUS_KEYS:
            written.append(keyword.value)
    return written


def unshared_status_slots(function, point=None, module=None, rel=None,
                          aliases=None, outer_imports=None):
    """Status/exit computations a registered point must route through the
    shared function, per return path (#892).

    ``calls_shared`` only proves the function mentions ``aggregate_status``
    somewhere; one branch could still build a summary dict or an exit code on
    its own.  Every returned summary dict's ``status`` slot must relay an
    already-aggregated value (constant/name/subscript) or contain the shared
    call itself; any other returned expression that mentions a batch status
    word or aggregates ``passed`` rows must be the shared call's own result;
    and once a return path is an aggregation pair (a summary dict or a status
    word), its int-shaped exit-code element must come from the shared call —
    including when the code travels through a local variable.
    Returns a list of one-line violation descriptions.
    """
    violations = []
    bound = _bound_summary_dicts(function)
    bindings = _name_bindings(function, outer_imports)
    aliases = _binding_exit_aliases(bindings, aliases, function)
    for node in _own_nodes(function):
        if isinstance(node, (ast.Assign, ast.AnnAssign, ast.AugAssign)):
            # Incremental report writes (``report['status'] = overall``) never
            # appear in a Return (#892 r2, DeepSeek sibling P1-2); ``status =
            # 'PASS'``, ``report |= {...}`` and ``report.update({...})`` are the
            # same write family (#892 r3).
            written = []
            if isinstance(node, ast.Assign):
                for write_target in node.targets:
                    _status_write_values(write_target, node.value, written)
                    for name in _target_names(write_target):
                        if _name_feeds_shared(name, function):
                            # A per-case verdict is a *fresh* dict whose status
                            # a leaf check decided; that shape is the healthy
                            # ``controls-main`` one and stays exempt.  A dict
                            # rebuilt from an existing summary (``{**report,
                            # ...}``) is never a leaf verdict, so its slots are
                            # judged like any other write (GLM boundary P1-3).
                            for summary in _returned_summary_dicts(node.value):
                                if any(key is None for key, _item in _dict_pairs(summary)):
                                    written.extend(_status_slot_values(summary))
                            continue
                        for summary in _returned_summary_dicts(node.value):
                            written.extend(_status_slot_values(summary))
            elif isinstance(node, ast.AnnAssign):
                if node.value is None:
                    continue
                _status_write_values(node.target, node.value, written)
                if any(_name_feeds_shared(name, function)
                       for name in _target_names(node.target)):
                    # Same split as the Assign branch: a fresh leaf verdict stays
                    # legal, a summary rebuilt from an existing one is judged --
                    # ``report: dict = {**report, 'status': 'PASS'}`` was exempt
                    # only because it was annotated (MiniMax sibling r6 P2-1).
                    for summary in _returned_summary_dicts(node.value):
                        if any(key is None for key, _item in _dict_pairs(summary)):
                            written.extend(_status_slot_values(summary))
                else:
                    for summary in _returned_summary_dicts(node.value):
                        written.extend(_status_slot_values(summary))
            elif isinstance(node.op, ast.BitOr):
                for summary in _returned_summary_dicts(node.value):
                    written.extend(_status_slot_values(summary))
                _status_write_values(node.target, node.value, written)
                if not _proven_summary_source(node.value, bindings, module,
                                             rel):
                    # ``report |= payload`` writes the same slots as
                    # ``report.update(payload)`` (GLM boundary r7 P1-2).
                    violations.append(
                        f'inline status/exit update {ast.unparse(node)[:60]} '
                        f'at line {node.lineno}')
                    continue
            if any(_assigns_clean_status(item, bindings, module=module,
                                         rel=rel)
                   for item in written) or any(
                    _assigns_clean_status(item, bindings, module=module,
                                          rel=rel, status_slot=False)
                    for item in _bare_verdict_values(node)):
                violations.append(
                    f'inline status/exit assignment {ast.unparse(node.value)[:60]} '
                    f'at line {node.lineno}')
            continue
        if isinstance(node, ast.Expr) and isinstance(node.value, ast.Call):
            call = node.value
            if isinstance(call.func, ast.Attribute) and call.func.attr in ('update', 'setdefault'):
                written = _call_written_values(call, bindings)
                if any(_assigns_clean_status(item, bindings, module=module,
                                             rel=rel)
                       for item in written):
                    violations.append(
                        f'inline status/exit assignment {ast.unparse(node.value)[:60]} '
                        f'at line {node.lineno}')
                elif _unproven_expansion(call, bindings, module, rel):
                    violations.append(
                        f'inline status/exit assignment {ast.unparse(node.value)[:60]} '
                        f'at line {node.lineno}')
                continue
            written = _call_written_values(call, bindings)
            if written:
                # ``setattr(report, 'status', 'PASS')`` /
                # ``report.__setitem__('status', 'PASS')``: the same slot as
                # ``report['status'] = 'PASS'`` under another spelling, judged
                # by the same value rule (GLM boundary r16 P1-1).
                if any(_assigns_clean_status(item, bindings, module=module,
                                             rel=rel)
                       for item in written):
                    violations.append(
                        f'inline status/exit assignment {ast.unparse(node.value)[:60]} '
                        f'at line {node.lineno}')
                continue
            if _is_exit_call(call, aliases):
                # ``sys.exit(0)`` / ``os._exit(0)`` leave the process with 0
                # exactly like ``raise SystemExit(0)`` (MiniMax sibling P1-1);
                # the same reasoning applies to a name bound to a zero.
                violation = _exit_call_violation(
                    call, bindings, function, module, point, node.lineno,
                    aliases)
                if violation:
                    violations.append(violation)
            continue
        if isinstance(node, (ast.ListComp, ast.SetComp, ast.GeneratorExp,
                             ast.DictComp)):
            # #1953 fix-r1: a write performed inside a comprehension element
            # (``[h(report, 'status', 'PASS') for h in [operator.setitem]]``)
            # is not a statement, so the ``Expr`` branch above never saw it and
            # the subscript write went out with no violation (codex boundary
            # r35 P0-2).  The loop variable resolves through ``_name_bindings``'
            # generator handling.
            parts = _comprehension_parts(node)
            for part in parts:
                for inner in _comprehension_calls(part):
                    written = _call_written_values(inner, bindings)
                    if any(_assigns_clean_status(item, bindings, module=module,
                                                 rel=rel)
                           for item in written):
                        violations.append(
                            f'inline status/exit assignment '
                            f'{ast.unparse(inner)[:60]} at line {node.lineno}')
                        break
            continue
        if isinstance(node, ast.Raise) and _is_system_exit_raise(node, aliases):
            # ``raise SystemExit(0)`` leaves the process with 0 and never
            # returns through the shared map (MiniMax P1-2); the bare
            # ``raise SystemExit`` raises with no argument, i.e. exit 0 too
            # (GLM sibling P1-3).
            exc = node.exc
            argument = (exc.args[0] if isinstance(exc, ast.Call) and exc.args
                        else None)
            violation = _exit_argument_violation(
                argument, bindings, function, module, point, node.lineno,
                'SystemExit', aliases)
            if violation:
                violations.append(violation)
            continue
        if not isinstance(node, ast.Return):
            continue
        value = node.value
        if value is None:
            # A bare ``return`` hands back None, and SystemExit(None) exits 0.
            violations.append(f'inline optimistic exit (bare return) at line {node.lineno}')
            continue
        if isinstance(value, ast.Call) and _is_exit_call(value, aliases):
            # ``return sys.exit(0)`` evaluates the exit call, which leaves the
            # process before the ``return`` can happen: the Return arm needs
            # the same judgement as the statement form (GLM r6 sibling P1-2).
            violation = _exit_call_violation(
                value, bindings, function, module, point, node.lineno, aliases)
            if violation:
                violations.append(violation)
            continue
        if _constant_exit_zero(value) and point not in _SENTINEL_POINTS:
            # Explicit ``return None`` / ``return False`` exits 0 too; only
            # ``_guard._failure`` uses None as its "propagate unchanged"
            # sentinel and the caller re-raises (DeepSeek boundary P0-1).
            violations.append(f'inline optimistic exit {ast.unparse(value)[:60]} '
                              f'at line {node.lineno}')
            continue
        dicts = _returned_summary_dicts(value)
        # ``report = {...}; return report`` is the dominant shape here; resolve
        # the returned name back to the dict built inside this function.
        for name in _returned_names(value):
            dicts.extend(bound.get(name, []))
        is_pair = bool(dicts) or _status_literals(value) or _has_passed_aggregate(value)
        for summary in dicts:
            violations.extend(_summary_slot_violations(summary, bindings, node.lineno))
        elements = value.elts if isinstance(value, ast.Tuple) else [value]
        for element in elements:
            if any(element is summary for summary in dicts):
                continue
            if isinstance(element, ast.Name) and any(
                    summary is _nearest_binding(element.id, bindings, node.lineno)
                    for summary in bound.get(element.id, ())):
                continue  # its dict is already checked above
            for arm in _exit_arms(element):
                if (_status_literals(arm) or _has_passed_aggregate(arm)):
                    # A dict arm is judged by ``_summary_slot_violations`` above
                    # -- but only the dicts this return actually hands back, not
                    # every dict in the expression (GLM r6 boundary P0-1).
                    if not (_calls_aggregate(arm)
                            or any(arm is summary for summary in dicts)):
                        violations.append(f'inline status/exit {ast.unparse(arm)[:60]} '
                                          f'at line {node.lineno}')
                elif isinstance(arm, ast.Name):
                    # The exit code may travel through a local variable; a
                    # conditional that can yield 0 is inline computation
                    # (MiniMax boundary P0-2 / DeepSeek sibling P1-3).
                    kind = _exit_name_violation(arm.id, bindings, function,
                                                module, aliases)
                    if kind:
                        violations.append(f'inline exit code ({kind} relay) {arm.id} '
                                          f'at line {node.lineno}')
                elif (_container_holds_zero(arm, bindings)
                      or _nested_def_exits_zero(arm, function, bindings,
                                                aliases)
                      or _delegated_exit(arm, module, bindings=bindings,
                                         aliases=aliases)):
                    # A zero read back out of a local dict, or a nested helper
                    # that returns a constant zero (GLM concurrency P1-1 /
                    # MiniMax boundary P1-4).
                    violations.append(f'inline exit code {ast.unparse(arm)[:60]} '
                                      f'at line {node.lineno}')
                elif isinstance(arm, (ast.Attribute, ast.Subscript)):
                    # ``return helpers.EXIT_OK`` / ``return CODES[key]``: the
                    # value is not a local binding this pass can trace, so its
                    # origin is invisible -- the bare-name spelling one branch
                    # up is ``unbound`` and the status side fails closed on
                    # attributes too.  One source may not have two verdicts
                    # (GLM boundary r18 P0-1).
                    if not _calls_aggregate(arm):
                        kind = ('attribute' if isinstance(arm, ast.Attribute)
                                else 'subscript')
                        violations.append(
                            f'inline exit code ({kind} relay) '
                            f'{ast.unparse(arm)[:60]} at line {node.lineno}')
                elif isinstance(arm, (ast.UnaryOp, ast.Compare)):
                    # ``return not ok`` leaves the process with 0 on a clean
                    # batch and no shared call produced it (MiniMax boundary
                    # P1-1); a comparison is the same shape.
                    if not _calls_aggregate(arm):
                        violations.append(f'inline exit code {ast.unparse(arm)[:60]} '
                                          f'at line {node.lineno}')
                elif (not _calls_aggregate(arm)
                        and ((_constant_exit_zero(arm)
                              and point not in _SENTINEL_POINTS)
                             or _optimistic_exit(arm)
                             or (_exit_code_shaped(arm)
                                 and (is_pair or isinstance(arm, ast.Call))))):
                    violations.append(f'inline exit code {ast.unparse(arm)[:60]} '
                                      f'at line {node.lineno}')
    if point is not None and not _has_exit_path(function, aliases):
        # A registered aggregation point that returns nothing (and never exits)
        # leaves the entry point with ``None`` -- ``SystemExit(None)`` is exit 0
        # -- while the audit used to pass it vacuously: the per-return rules
        # have no return to look at (MiniMax boundary r9 P1-2).
        violations.append('no return or exit path')
    return violations


def _parameter_names(function):
    """Every parameter name of *function* (positional, keyword, ``*``/``**``).

    A report handed in as a parameter is still a summary being written:
    ``def publish(report): report |= {'status': 'PASS'}`` was invisible because
    the receiver gate only knew names bound to a dict literal inside the
    function (GLM boundary r11 P1-1).
    """
    arguments = function.args
    names = {arg.arg for arg in (*arguments.posonlyargs, *arguments.args,
                                 *arguments.kwonlyargs)}
    for extra in (arguments.vararg, arguments.kwarg):
        if extra is not None:
            names.add(extra.arg)
    return frozenset(names)


def _unproven_update_on_summary(call, bound, bindings, parameters=frozenset(),
                                module=None, rel=None):
    """``report.update(payload)`` whose slots this function never built.

    On the write side the payload is judged fail-closed by
    ``_unproven_expansion`` -- but only for a point that is already registered.
    The enumeration keyed on ``_call_written_values``, which sees nothing in an
    opaque payload, so an *unregistered* function writing its status this way
    was never enumerated at all (MiniMax boundary r9 P2-3).  A receiver that is
    a parameter counts the same as one bound to a dict literal (GLM boundary
    r11 P1-1).
    """
    receiver = call.func.value
    if isinstance(receiver, ast.Attribute):
        # ``self.report.update({'status': 'PASS'})``: the receiver gate only
        # knew bare names, so an attribute receiver made an unregistered
        # function invisible (GLM boundary r13 P1-1).  The payload has to
        # carry the status evidence itself, so an unrelated ``cache.update``
        # stays out of the enumeration.
        return bool(_status_literals(call)) and _unproven_expansion(
            call, bindings, module, rel)
    if not isinstance(receiver, ast.Name):
        return False
    if receiver.id not in bound and receiver.id not in parameters:
        return False
    return _unproven_expansion(call, bindings, module, rel)


def _augassign_on_summary(node, bound, bindings, parameters=frozenset(),
                          module=None, rel=None):
    """``report |= payload``: a status write the ``Assign`` family cannot see.

    ``ast.AugAssign`` is a sibling of ``ast.Assign``, not a subclass, so the
    enumeration never saw the ``|=`` family: a function whose only status
    evidence was ``report |= {'status': 'PASS'}`` became no aggregation point at
    all, was therefore never reported as unregistered and never audited, while
    the slot layer's augmented-write rule only runs on points that are already
    registered (MiniMax boundary r10 P0-1).  A parameter receiver takes the
    same rule, but only with status evidence in the payload: ``owned |=
    children`` on a set parameter is a union, not a summary write (GLM boundary
    r11 P1-1).
    """
    if not isinstance(node, ast.AugAssign):
        return False
    if isinstance(node.target, ast.Attribute):
        # ``self.report |= {'status': 'PASS'}``: the receiver gate only knew
        # bare names (GLM boundary r13 P1-1).  Only a status payload makes an
        # unknown receiver a summary write, so ``self.cache |= other`` stays
        # out of the enumeration.
        return bool(_status_literals(node.value))
    if not isinstance(node.target, ast.Name):
        return False
    known = node.target.id in bound
    if not known and node.target.id not in parameters:
        return False
    if isinstance(node.value, ast.Dict):
        return _dict_has_status(node.value)
    if _status_literals(node.value):
        return True
    if known:
        # The call payload is resolved like every other delegation judgement:
        # dropping *module*/*rel* here fell back to the name-only registry and
        # trusted any local ``run_all`` in any file (GLM boundary r14 P1-1).
        return not _proven_summary_source(node.value, bindings, module, rel)
    # A plain local receiver that is not a summary (``owned |= children``) is a
    # set union, not a summary write.  A *parameter* receiver has no such
    # excuse: an opaque payload may be a summary exactly like the literal form
    # above, and ``update()`` already fails closed on it (MiniMax sibling r12
    # P1-3).  A set-union helper taking the accumulator as a parameter would
    # now be reported; that boundary is listed in the README.
    return node.target.id in parameters and not _proven_summary_source(
        node.value, bindings, module, rel)


def _has_exit_path(function, aliases=None):
    """True when *every* path out of *function* is an explicit one.

    Judged by reachability, not by existence: one branch ``raise``-ing says
    nothing about the other branch, which can still fall off the end and hand
    ``None`` back (``SystemExit(None)`` is exit 0).  The helper-level rule was
    moved to reachability in r13; this point-level sibling kept the old
    "some raise somewhere" test and waved the same shape through (MiniMax
    sibling r13 P1-1).  Any ``raise`` still counts as an exit path for the
    paths that take it -- a propagating error leaves non-zero (MiniMax
    boundary r10 P2-4).
    """
    return not _body_completes(function.body, aliases)


def aggregation_markers(function, module=None, rel=None, aliases=None,
                        shared=None, rebound=frozenset(),
                        outer_imports=None):
    """Markers that make *function* a top-level status/exit computation.

    *module* is the file's module-level ``def`` map; the write-side rules need
    it to resolve a callee in the file it is defined in (GLM boundary r12
    P1-1).  *shared* / *rebound* are the function-scope trust view from
    ``_function_shared_names`` (#892 r21).
    """
    markers = set()
    # A summary dict assigned to any name and returned by that name is a
    # summary point, whatever the name is (DeepSeek P1-3).
    bound = _bound_summary_dicts(function)
    bindings = _name_bindings(function, outer_imports)
    aliases = _binding_exit_aliases(bindings, aliases, function)
    dict_names = set(bound) | {name for name, items in bindings.items()
                               if any(isinstance(value, ast.Dict)
                                      for _lineno, value in items)}
    parameters = _parameter_names(function)
    for node in ast.walk(function):
        if isinstance(node, ast.Call) and _is_exit_call(node, aliases):
            # ``sys.exit(0)`` / ``sys.exit('BLOCKED')`` / ``quit(...)``: the
            # raise family already enumerates ``raise SystemExit(...)``
            # (``raise-exit-status``), the call family did not, so an
            # unregistered point whose only evidence is an exit call stayed
            # invisible to the audit (MiniMax sibling r14 P2-1).  Every exit
            # call is an exit-code computation of its own, so the enumeration
            # does not require a status word in the argument.
            markers.add('exit-call-status')
        if isinstance(node, (ast.Assign, ast.AnnAssign)):
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            value = node.value
            if value is None:
                continue
            for target in targets:
                if _is_status_target(target) and (
                        _status_literals(value)
                        or _has_passed_aggregate(value)
                        or _calls_aggregate(value, shared, rebound)):
                    markers.add('assign-status')
                elif (_is_report_status(target, value)
                      and not _name_feeds_shared(target.id, function)):
                    markers.add('assign-report-status')
        elif isinstance(node, ast.Return) and node.value is not None:
            value = node.value
            if _returned_summary_dicts(value) or _returned_summary_names(value, bound):
                markers.add('return-report-status')
            if (isinstance(value, ast.Call) and isinstance(value.func, ast.Name)
                    and value.func.id == 'int' and _status_literals(value)):
                markers.add('return-int-status')
            if (isinstance(value, ast.Subscript) and isinstance(value.value, ast.Call)
                    and _is_shared_call(value.value, shared, rebound)):
                markers.add('return-aggregate-code')
            if isinstance(value, ast.IfExp) and (
                    _status_literals(value) or _has_passed_aggregate(value)):
                markers.add('return-conditional-status')
        elif (isinstance(node, ast.Raise) and _is_system_exit_raise(node, aliases)):
            # Every ``raise SystemExit(...)`` -- with a status word, with a bare
            # zero, with no argument at all -- leaves the process with an exit
            # code, exactly like ``sys.exit(...)`` above; requiring a status word
            # made the most optimistic spellings invisible (MiniMax sibling r15
            # P1-1).
            markers.add('raise-exit-status')
        elif (isinstance(node, ast.Expr) and isinstance(node.value, ast.Call)
              and (_call_written_values(node.value, bindings)
                   or (isinstance(node.value.func, ast.Attribute)
                       and _unproven_update_on_summary(node.value, dict_names,
                                                       bindings, parameters,
                                                       module, rel)))):
            # ``report.update({'status': ...})`` / ``setdefault`` /
            # ``setattr(report, 'status', ...)`` / ``report.__setitem__(
            # 'status', ...)`` without any other marker: the only status
            # evidence is the incremental write, so the function must still be
            # enumerated and registered (MiniMax sibling P1-1; the ``setattr``
            # spelling is GLM boundary r16 P1-1).
            markers.add('write-status')
        elif (isinstance(node, (ast.ListComp, ast.SetComp, ast.GeneratorExp,
                                ast.DictComp))
              and any(_call_written_values(inner, bindings)
                      for part in _comprehension_parts(node)
                      for inner in _comprehension_calls(part))):
            # #1953 fix-r1: the same write spelled inside a comprehension
            # element (``[h(report, 'status', 'PASS') for h in
            # [operator.setitem]]``) is still the only status evidence of the
            # function, so it has to enumerate the point too.
            markers.add('write-status')
        elif _augassign_on_summary(node, dict_names, bindings, parameters,
                                   module, rel):
            # ``report |= {'status': 'PASS'}`` carries the same evidence as the
            # ``update()`` spelling above and must enumerate the function too
            # (MiniMax boundary r10 P0-1).
            markers.add('write-status')
        elif _is_shared_call(node, shared, rebound):
            markers.add('calls-aggregate')
    return markers


def calls_shared(function, shared=None, rebound=frozenset()):
    return any(_is_shared_call(node, shared, rebound) for node in ast.walk(function))


def scan_aggregation_tree(tree, rel, aliases=None):
    """Aggregation points of *tree*, keyed by ``(rel, function name)``.

    A nested ``def`` with the same name as a top-level point used to overwrite
    the registered entry (``ast.walk`` is BFS, so the nested one won), leaving
    the real point unaudited (MiniMax boundary P2-6 / DeepSeek boundary P2-6).
    Duplicates are reported instead of silently replacing.
    """
    found = {}
    duplicates = []
    # #892 r27: ``aggregation_audit`` reads module-level definitions through
    # ``_module_level_defs`` (a helper inside ``if``/``try`` still runs at
    # import time); this scan kept ``tree.body``, so the two layers gave
    # opposite evidence about the same callee (DeepSeek sibling r26 P1-1).
    module = {node.name: node for node in _module_level_defs(tree)
              if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))}
    if aliases is None:
        aliases = _module_exit_aliases(tree)
    shared = _module_shared_names(tree, rel)
    loaders = _loader_names(tree, rel)
    # #892 r24: the same module binding graph also judges the proven-source
    # callees, so a same-named redefinition cannot stand in for the real def.
    proven = _proven_source_names(tree)
    # #892 r23: a receiver poisoned at module scope (or through ``global``)
    # loses the relay in *every* function of the file, not only where it is
    # rebound locally (GLM boundary r22 P0-1).
    poisoned = _withdrawn_receivers(tree, rel, shared)
    outer_imports = _module_binding_events(tree)
    for function in ast.walk(tree):
        if not isinstance(function, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        fn_shared, fn_rebound = _function_shared_names(
            function, shared, rel, loaders, proven)
        markers = aggregation_markers(function, module, rel, aliases,
                                      fn_shared, fn_rebound | poisoned,
                                      outer_imports)
        if not markers:
            continue
        key = (rel, function.name)
        if key in found:
            duplicates.append(key)
            continue
        found[key] = (markers, function)
    return found, duplicates


def aggregation_audit():
    """Enumerate every aggregation point and require the one shared function.

    The detector is self-tested with an inline summary (must be flagged) and a
    shared call (must be recognized), so deleting a call cannot pass silently.
    #892 adds a directly returned summary dict, a branch that computes an exit
    code around the shared call, a clean shared pair, a summary dict returned
    through a local name, and a bare optimistic exit code to the self-tests, and
    requires every registered point's return paths to compute batch status and
    exit codes only through the shared function.
    """
    files = [path for path in (
        sorted((ROOT / 'scripts/gateway-parity').rglob('*.py'))
        + sorted(Path(__file__).resolve().parent.glob('*.py')))
        if 'node_modules' not in path.parts and '__pycache__' not in path.parts]
    found = {}
    modules = {}
    aliases_by_rel = {}
    imports_by_rel = {}
    trusted_by_rel = {}
    loaders_by_rel = {}
    proven_by_rel = {}
    poisoned_by_rel = {}
    failures = []
    if not callable(getattr(md.common, 'aggregate_status', None)):
        failures.append('#164-fix-r7 aggregation-audit: common.aggregate_status is missing')
    product_words = set(getattr(md.common, 'BATCH_STATUS_ORDER', ()))
    cleanup_word = getattr(md, 'CLEANUP_UNVERIFIED_STATUS', None)
    if cleanup_word is not None:
        product_words.add(cleanup_word)
    if cleanup_word is None:
        # A caller may stub the product module -- ``test_safety`` injects a
        # guard-failure point through ``SimpleNamespace(common=common)`` -- and
        # then the cleanup word is simply absent, not gone from the product.
        # Only the direction a stub cannot fake stays enforced: every word the
        # product enum does expose must be mirrored (GLM sibling r12 P1-1).
        # An empty product enum is not "everything mirrored" (MiniMax boundary
        # r13 P2-3).
        mirror_ok = bool(product_words) and product_words <= set(STATUS_WORDS)
    else:
        mirror_ok = product_words == set(STATUS_WORDS)
    if not mirror_ok:
        # The copy is only safe while it mirrors the product enum: a word added
        # on one side would silently leave the other (GLM boundary r9 P2-4 /
        # MiniMax simplify r9 P2-1).  An *empty* product enum is the direction
        # that most needs the failure, so there is no ``product_words and``
        # short-circuit in front of it (MiniMax boundary r10 P2-5).
        failures.append('#892 aggregation-audit: STATUS_WORDS '
                        f'{sorted(STATUS_WORDS)} no longer mirrors the product '
                        f'enumeration {sorted(product_words)}')
    for path in files:
        rel = path.relative_to(ROOT).as_posix()
        try:
            tree = ast.parse(path.read_text(encoding='utf-8'))
        except (OSError, SyntaxError) as error:
            failures.append(f'#164-fix-r7 aggregation-audit: cannot scan '
                            f'{rel}: {type(error).__name__}: {error}')
            continue
        for node in ast.walk(tree):
            if (isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
                    and node.name == 'aggregate_status'
                    and rel != 'scripts/gateway-parity/common.py'):
                failures.append(f'#892 aggregation-audit: {rel} defines its own '
                                'aggregate_status; only common.py may implement it')
        # Module-level defs only: a nested ``def`` of the same name must not
        # shadow the helper a registered point actually delegates to (MiniMax
        # boundary P1-2); nested helpers are judged by ``_nested_def_exits_zero``.
        # #892 r26: "module level" means every block that runs at import time,
        # not just ``tree.body`` -- a helper defined inside ``if``/``try`` is
        # exactly the shape the binding graph now descends into, and the two
        # scans have to agree (GLM sibling r25 #6).
        modules[rel] = {node.name: node for node in _module_level_defs(tree)
                        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))}
        # #892 r21: the proven-source registry is only as good as its names --
        # a typo or a renamed helper would withdraw trust on a shape the healthy
        # tree actually uses, so every entry is asserted against its file.
        module_names = set(modules[rel]) | _module_imported_names(tree)
        for _proven_rel, helper in PROVEN_SHARED_SOURCES:
            if _proven_rel == rel and helper not in module_names:
                failures.append(f'#892 aggregation-audit: proven shared source '
                                f'{rel}:{helper} is neither defined nor imported there')
        imports_by_rel[rel] = _module_binding_events(tree)
        aliases_by_rel[rel] = _module_exit_aliases(tree)
        trusted_by_rel[rel] = _module_shared_names(tree, rel)
        loaders_by_rel[rel] = _loader_names(tree, rel)
        proven_by_rel[rel] = _proven_source_names(tree)
        poisoned_by_rel[rel] = _withdrawn_receivers(
            tree, rel, trusted_by_rel[rel])
        lost = sorted(_SHARED_MODULES - trusted_by_rel[rel])
        if lost:
            # One file may not bind the shared aggregator to anything but the
            # apparatus loader: with the spelling trusted, a stub served as the
            # shared map (GLM boundary r19 P1-1); with the spelling withdrawn
            # silently, the same function would simply stop being enumerated.
            # Either way the file is not auditable, so it fails closed here.
            failures.append(f'#892 aggregation-audit: {rel} rebinds the shared '
                            f'aggregator module {lost}; only the apparatus loader '
                            'may bind it')
        scanned, duplicates = scan_aggregation_tree(tree, rel)
        for duplicate in duplicates:
            failures.append(f'#892 aggregation-audit: duplicate aggregation point '
                            f'name {duplicate[0]}:{duplicate[1]}')
        found.update(scanned)

    # #892 r26: the registry keys on the *importing* file, so the file that
    # actually defines a cross-file proven source was never judged -- a
    # same-named redefinition or a decorated stub there kept the importer's
    # trust (GLM sibling r25 #2 / boundary r25 P1-5).  Each definition file is
    # parsed and held to the same single-undecorated-def rule.
    for definition_rel, definition_name in sorted(PROVEN_SHARED_DEFINITIONS):
        definition_path = ROOT / definition_rel
        try:
            definition_tree = ast.parse(
                definition_path.read_text(encoding='utf-8'))
        except (OSError, SyntaxError) as error:
            failures.append(f'#892 aggregation-audit: cannot scan proven '
                            f'definition {definition_rel}: '
                            f'{type(error).__name__}: {error}')
            continue
        if not _single_definition(definition_tree, definition_name):
            failures.append(f'#892 aggregation-audit: proven shared source '
                            f'{definition_rel}:{definition_name} no longer '
                            'resolves to exactly one undecorated definition')

    # #892 r26: every other file proves ``_load = _guard.load`` by trusting the
    # module, so the sanctioned definition itself has to keep its loader role --
    # a decorated ``def load`` replaces the function object at definition time
    # and no other file can see it (GLM sibling r25 #5 / boundary r25 P1-7).
    if GUARD_REL in loaders_by_rel and 'load' not in loaders_by_rel[GUARD_REL]:
        failures.append(f'#892 aggregation-audit: {GUARD_REL} no longer defines '
                        'exactly one undecorated load(...)')

    points = {}
    for key, point_id in sorted(AGGREGATION_POINTS.items()):
        rel, name = key
        if key not in found:
            points[point_id] = {'file': rel, 'function': name, 'shared': False}
            failures.append(f'#164-fix-r7 aggregation-audit: registered point missing '
                            f'{rel}:{name}')
            continue
        markers, function = found[key]
        fn_shared, fn_rebound = _function_shared_names(
            function, trusted_by_rel.get(rel), rel,
            loaders_by_rel.get(rel, frozenset()),
            proven_by_rel.get(rel, frozenset()))
        # #892 r23: module/global-poisoned receivers lose the relay here too.
        fn_rebound = fn_rebound | poisoned_by_rel.get(rel, frozenset())
        shared = (calls_shared(function, fn_shared, fn_rebound)
                  and 'return-int-status' not in markers)
        slots = unshared_status_slots(function, key, modules.get(rel), rel,
                                      aliases_by_rel.get(rel),
                                      imports_by_rel.get(rel))
        points[point_id] = {'file': rel, 'function': name, 'shared': shared,
                            'markers': sorted(markers), 'unsharedSlots': slots}
        if not shared:
            failures.append(f'#164-fix-r7 aggregation-audit: {rel}:{name} must call '
                            'common.aggregate_status')
        if 'return-int-status' in markers:
            failures.append(f'#164-fix-r7 aggregation-audit: {rel}:{name} must call '
                            'common.aggregate_status (found int(...) exit computation)')
        for slot in slots:
            failures.append(f'#892 aggregation-audit: {rel}:{name} must route every '
                            f'batch status/exit computation through '
                            f'common.aggregate_status (found {slot})')

    unclassified = []
    for key, (markers, _function) in sorted(found.items()):
        if key in AGGREGATION_POINTS or key in NON_AGGREGATION_POINTS:
            continue
        unclassified.append({'file': key[0], 'function': key[1],
                             'markers': sorted(markers)})
        failures.append(f'#164-fix-r7 aggregation-audit: unregistered aggregation point '
                        f'{key[0]}:{key[1]} markers={sorted(markers)}')

    probe = ast.parse(
        "def _bypass(rows):\n"
        "    report = {'status': 'PASS' if all(row['passed'] for row in rows) else 'FAIL'}\n"
        "    return report\n"
        "def _shared(rows):\n"
        "    status, code = common.aggregate_status('PASS', 'PASS_WITH_SKIPS')\n"
        "    return status, code\n")
    probe_found, _probe_duplicates = scan_aggregation_tree(probe, '<self-test>')
    bypass = probe_found.get(('<self-test>', '_bypass'))
    shared = probe_found.get(('<self-test>', '_shared'))
    if not bypass or calls_shared(bypass[1]):
        failures.append('#164-fix-r7 aggregation-audit: detector self-test did not '
                        'flag an inline summary')
    if not shared or not calls_shared(shared[1]):
        failures.append('#164-fix-r7 aggregation-audit: detector self-test did not '
                        'recognize a shared call')
    # #892: the enumeration must see a summary function that directly returns
    # the status dict, and the per-branch constraint must reject a branch that
    # computes a status/exit code around the shared call while another branch
    # still uses it.  A shared pair (status relay + shared exit code) must stay
    # clean, so deleting either detector cannot pass silently.
    probe892 = ast.parse(
        "def _direct_summary(rows):\n"
        "    return {'status': 'PASS' if all(row['passed'] for row in rows) else 'FAIL'}\n"
        "def _inline_branch(report):\n"
        "    if report.get('special'):\n"
        "        return 0 if report['status'] == 'PASS' else 1\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _shared_pair(report):\n"
        "    if report.get('special'):\n"
        "        return {'status': 'BLOCKED'}, common.aggregate_status('BLOCKED')[1]\n"
        "    status, code = common.aggregate_status(report['status'])\n"
        "    return {'status': status}, code\n"
        "def _assigned_summary(rows):\n"
        "    report = {'status': 'PASS', 'rows': rows}\n"
        "    return report\n"
        "def _bare_zero(report):\n"
        "    if report.get('special'):\n"
        "        return 0\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _conditional_arm(report):\n"
        "    if report.get('special'):\n"
        "        return 0 if report.get('nested') else 1\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _bare_return(report):\n"
        "    if report.get('special'):\n"
        "        return\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _payload_name(rows):\n"
        "    payload = {'status': 'PASS', 'rows': rows}\n"
        "    return payload\n"
        "def _relayed_status(rows):\n"
        "    status = 'PASS' if all(row['passed'] for row in rows) else 'FAIL'\n"
        "    report = {'status': status, 'rows': rows}\n"
        "    return report\n"
        "def _system_exit(report):\n"
        "    if report.get('special'):\n"
        "        raise SystemExit(0)\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _assigned_status(report, rows):\n"
        "    report['status'] = 'PASS' if all(row['passed'] for row in rows) else 'FAIL'\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _none_exit(report):\n"
        "    if report.get('special'):\n"
        "        return None\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _system_exit_none(report):\n"
        "    if report.get('special'):\n"
        "        raise SystemExit(None)\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _relayed_exit(report):\n"
        "    code = 0 if report.get('special') else 1\n"
        "    return code\n"
        "def _subscript_slot(rows):\n"
        "    local = {'status': 'PASS'}\n"
        "    return {'status': local['status'], 'rows': rows}\n"
        "def _lookalike(report):\n"
        "    def aggregate_status(*children):\n"
        "        return 'PASS', 0\n"
        "    status, code = aggregate_status(report['status'])\n"
        "    return {'status': status}, code\n"
        "def _updated_slot(report):\n"
        "    report.update({'status': 'PASS'})\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _duplicate_key(rows):\n"
        "    return {'status': 'BLOCKED', 'status': 'PASS', 'rows': rows}\n"
        "def _unpacked(rows):\n"
        "    return {**{'status': 'PASS'}, 'rows': rows}\n"
        "def _assign_only(rows):\n"
        "    payload = {'status': 'PASS', 'rows': rows}\n"
        "    print(payload)\n"
        "    return 0\n"
        "def _shared_excuse(report):\n"
        "    if report.get('special'):\n"
        "        code = 0\n"
        "    else:\n"
        "        _, code = common.aggregate_status(report['status'])\n"
        "    return code\n"
        "def _code_initializer(report):\n"
        "    code = 0\n"
        "    if report.get('special'):\n"
        "        _, code = common.aggregate_status('BLOCKED')\n"
        "    else:\n"
        "        _, code = common.aggregate_status(report['status'])\n"
        "    return code\n"
        "def _early_return_zero(report):\n"
        "    code = 0\n"
        "    if report.get('special'):\n"
        "        code = 0\n"
        "        return code\n"
        "    _, code = common.aggregate_status(report['status'])\n"
        "    return code\n"
        "def _two_hop_zero(report):\n"
        "    rc = 0\n"
        "    relay = rc\n"
        "    print(report)\n"
        "    return relay\n"
        "def _int_call_code(rows):\n"
        "    code = int(all(row['passed'] for row in rows))\n"
        "    return code\n"
        "def _dict_code_zero(report):\n"
        "    holder = {'code': 0}\n"
        "    print(report)\n"
        "    return holder['code']\n"
        "def _nested_def_zero(report):\n"
        "    def _exit():\n"
        "        return 0\n"
        "    print(report)\n"
        "    return _exit()\n"
        "def _verdict_slot(report):\n"
        "    report['verdict'] = 'PASS'\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _update_only(rows):\n"
        "    report = {'cases': rows}\n"
        "    report.update({'status': 'PASS' if all(r['passed'] for r in rows) else 'FAIL'})\n"
        "    return 0\n"
        "def _alias_cycle(status, code):\n"
        "    status, code = code, status\n"
        "    return {'status': status}, code\n"
        "def _sys_exit(report):\n"
        "    if report.get('special'):\n"
        "        sys.exit(0)\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _walrus_status(report):\n"
        "    report['status'] = (status := 'PASS')\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _module_status(report):\n"
        "    report['status'] = MODULE_STATUS\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _delegated_exit(report):\n"
        "    print(report)\n"
        "    return _exit_code(report)\n"
        "def _exit_code(report):\n"
        "    return 0 if report.get('ok') else 1\n"
        "def _rebuilt_summary(report):\n"
        "    report = {**report, 'status': 'PASS'}\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _boolean_exit(report):\n"
        "    ok = bool(report.get('cells'))\n"
        "    print(report)\n"
        "    return not ok\n"
        "def _sys_exit_relay(report):\n"
        "    holder = {'code': 0}\n"
        "    if report.get('special'):\n"
        "        sys.exit(holder['code'])\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _system_exit_bare(report):\n"
        "    if report.get('special'):\n"
        "        raise SystemExit\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _delegated_chain(report):\n"
        "    print(report)\n"
        "    return _exit_a(report)\n"
        "def _exit_a(report):\n"
        "    return _exit_b(report)\n"
        "def _exit_b(report):\n"
        "    return 0 if report.get('ok') else 1\n"
        "def _relay_zero_helper(report):\n"
        "    print(report)\n"
        "    return _relay_zero(report)\n"
        "def _relay_zero(report):\n"
        "    code = 0\n"
        "    print(report)\n"
        "    return code\n"
        "def _attr_head_clean(report):\n"
        "    report['status'] = helpers._clean()\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _mixed_branch_helper(report):\n"
        "    print(report)\n"
        "    return _mixed_code(report)\n"
        "def _mixed_code(rep):\n"
        "    if rep.get('special'):\n"
        "        raise ValueError('x')\n"
        "def _bare_exit_helper(report):\n"
        "    print(report)\n"
        "    return _bare_bail(report)\n"
        "def _bare_bail(report):\n"
        "    raise SystemExit\n"
        "def _shared_decoy(report):\n"
        "    print(report)\n"
        "    return _decoy_code(report)\n"
        "def _decoy_code(report):\n"
        "    common.aggregate_status('FAIL')\n"
        "    return 0\n"
        "def _verdict_dict(rows):\n"
        "    payload = {'verdict': 'PASS', 'rows': rows}\n"
        "    print(payload)\n"
        "    return 0\n"
        "def _container_slot(report):\n"
        "    report['status'] = upstream['status']\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _plain_nonzero(report):\n"
        "    if report.get('special'):\n"
        "        return 2\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _pair_literal(report):\n"
        "    if report.get('special'):\n"
        "        return {'status': 'BLOCKED'}, 2\n"
        "    status, code = common.aggregate_status(report['status'])\n"
        "    return {'status': status}, code\n"
        "def _conditional_summary(report):\n"
        "    return {'status': 'PASS'} if report.get('special') else report\n"
        "def _bound_summary_arm(report, fallback):\n"
        "    payload = {'status': 'PASS'}\n"
        "    return payload if report.get('special') else fallback\n"
        "def _expanded_update(report):\n"
        "    report.update(**payload)\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _exit_conditional(report):\n"
        "    if report.get('special'):\n"
        "        sys.exit(0 if report.get('cells') else 1)\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _ann_rebuilt_summary(report):\n"
        "    report: dict = {**report, 'status': 'PASS'}\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _bare_verdict(report):\n"
        "    verdict = 'PASS'\n"
        "    print(report)\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _bool_exit(report):\n"
        "    if report.get('special'):\n"
        "        return bool(report.get('cells'))\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _bool_relay_exit(report):\n"
        "    if report.get('special'):\n"
        "        code = bool(report.get('cells'))\n"
        "        return code\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _walrus_exit(report):\n"
        "    if (code := int(bool(report.get('cells')))):\n"
        "        return code\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _unbound_exit_name(report):\n"
        "    if report.get('special'):\n"
        "        return EXIT_CODE\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _update_positional(report):\n"
        "    report.update(extra)\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _augassign_update(report):\n"
        "    report |= extra\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _bound_update(report):\n"
        "    payload = {'status': 'BLOCKED', 'reason': 'x'}\n"
        "    report.update(payload)\n"
        "    report.update({'status': 'BLOCKED'})\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _tuple_verdict(report):\n"
        "    verdict, code = 'PASS', 0\n"
        "    print(report, code)\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _cleanup_word(report):\n"
        "    report['status'] = 'PASS_WITH_CLEANUP_UNVERIFIED'\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _unbound_exit_relay(report):\n"
        "    if report.get('special'):\n"
        "        code = EXIT_OK\n"
        "        return code\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _delegated_attribute(report):\n"
        "    print(report)\n"
        "    return helpers.exit_code(report)\n"
        "def _delegated_bound_name(report):\n"
        "    helper = _exit_code\n"
        "    print(report)\n"
        "    return helper(report)\n"
        "def _call_status_write(report):\n"
        "    report['status'] = report.get('status', 'PASS')\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _bound_rebinding(report):\n"
        "    report = {'status': report['status']}\n"
        "    report = 0\n"
        "    return report\n"
        "def _unproven_update_summary(rows):\n"
        "    report = {'cases': rows}\n"
        "    report.update(extra)\n"
        "    return report\n"
        "def _return_exit_call(report):\n"
        "    if report.get('special'):\n"
        "        return sys.exit(0)\n"
        "    return common.aggregate_status(report['status'])[1]\n"
        "def _delegated_value_relay(report):\n"
        "    code = _exit_code(report)\n"
        "    return code\n"
        "def _exit_bare(report):\n"
        "    print(report)\n"
        "def _bare_return_delegate(report):\n"
        "    print(report)\n"
        "    return _exit_bare(report)\n"
        "def _exit_statement(report):\n"
        "    sys.exit(0)\n"
        "def _statement_delegate(report):\n"
        "    return _exit_statement(report)\n"
        "def _augassign_summary(rows):\n"
        "    report = {'cases': rows}\n"
        "    report |= {'status': 'PASS'}\n"
        "    return report\n"
        "def _raising_only(report):\n"
        "    raise ValueError('bad')\n"
        "def _exit_call_status(report):\n"
        "    print(report)\n"
        "    sys.exit('BLOCKED')\n"
        "def _os_exit_zero(report):\n"
        "    print(report)\n"
        "    os._exit(0)\n"
        "def _tuple_slot_write(rows):\n"
        "    report = {'cases': rows}\n"
        "    report['status'], code = 'PASS', 0\n"
        "    return report\n"
        "def _bare_raise_exit(report):\n"
        "    print(report)\n"
        "    raise SystemExit\n"
        "def _zero_raise_exit(report):\n"
        "    print(report)\n"
        "    raise SystemExit(0)\n"
        "import sys as _s\n"
        "import builtins as _bi\n"
        "import posix as _px\n"
        "from sys import exit as _bail\n"
        "from builtins import SystemExit as _SE\n"
        "def _alias_sysexit(report):\n"
        "    print(report)\n"
        "    _s.exit(0)\n"
        "def _alias_from_import(report):\n"
        "    print(report)\n"
        "    _bail(0)\n"
        "def _alias_builtins_exit(report):\n"
        "    print(report)\n"
        "    _bi.exit(0)\n"
        "def _alias_posix_exit(report):\n"
        "    print(report)\n"
        "    _px._exit(0)\n"
        "def _alias_system_exit(report):\n"
        "    print(report)\n"
        "    raise _SE(0)\n"
        "def _alias_local_binding(report):\n"
        "    bail = _s.exit\n"
        "    bail(0)\n"
        "def _setattr_slot(rows):\n"
        "    report = {'cases': rows}\n"
        "    setattr(report, 'status', 'PASS')\n"
        "    return report\n"
        "def _dunder_setitem_slot(rows):\n"
        "    report = {'cases': rows}\n"
        "    report.__setitem__('status', 'PASS')\n"
        "    return report\n"
        "def _setattr_non_status(rows):\n"
        "    report = {'cases': rows}\n"
        "    setattr(report, 'cases', rows)\n"
        "    return report\n"
        "import operator as _op\n"
        "_module_alias = _s.exit\n"
        "try:\n"
        "    import sys as _try_s\n"
        "except ImportError:\n"
        "    _try_s = None\n"
        "def _module_binding_alias(report):\n"
        "    print(report)\n"
        "    _module_alias(0)\n"
        "def _local_import_alias(report):\n"
        "    import sys as _local_s\n"
        "    _local_s.exit(0)\n"
        "def _local_from_import(report):\n"
        "    from sys import exit as _leave\n"
        "    _leave(0)\n"
        "def _try_import_alias(report):\n"
        "    _try_s.exit(0)\n"
        "def _attr_status_slot(rows):\n"
        "    report = {'cases': rows}\n"
        "    report.status = 'PASS'\n"
        "    return report\n"
        "def _dunder_setattr_slot(rows):\n"
        "    report = {'cases': rows}\n"
        "    report.__setattr__('status', 'PASS')\n"
        "    return report\n"
        "def _unbound_setitem_slot(rows):\n"
        "    report = {'cases': rows}\n"
        "    dict.__setitem__(report, 'status', 'PASS')\n"
        "    return report\n"
        "def _operator_setitem_slot(rows):\n"
        "    report = {'cases': rows}\n"
        "    _op.setitem(report, 'status', 'PASS')\n"
        "    return report\n"
        "def _builtins_setattr_slot(rows):\n"
        "    report = {'cases': rows}\n"
        "    _bi.setattr(report, 'status', 'PASS')\n"
        "    return report\n"
        "def _builtins_subscript_setattr_slot(rows):\n"
        "    report = {'cases': rows}\n"
        "    __builtins__['setattr'](report, 'status', 'PASS')\n"
        "    return report\n"
        "def _alias_setattr_slot(rows):\n"
        "    report = {'cases': rows}\n"
        "    sa = setattr\n"
        "    sa(report, 'status', 'PASS')\n"
        "    return report\n"
        "def _unbound_setdefault_slot(rows):\n"
        "    report = {'cases': rows}\n"
        "    dict.setdefault(report, 'status', 'PASS')\n"
        "    return report\n"
        "def _getattr_dunder_slot(rows):\n"
        "    report = {'cases': rows}\n"
        "    getattr(report, '__setitem__')('status', 'PASS')\n"
        "    return report\n"
        "try:\n"
        "    _guarded_bail = _s.exit\n"
        "except ImportError:\n"
        "    _guarded_bail = None\n"
        "def _guarded_assignment_alias(report):\n"
        "    print(report)\n"
        "    _guarded_bail(0)\n"
        "def _attribute_exit_arm(rows):\n"
        "    if not rows:\n"
        "        return {'status': 'PASS'}, _helpers.EXIT_OK\n"
        "    return common.aggregate_status('PASS')\n"
        "def _subscript_exit_arm(rows):\n"
        "    if not rows:\n"
        "        return {'status': 'PASS'}, _CODES[rows]\n"
        "    return common.aggregate_status('PASS')\n"
        "def _attribute_code_relay(rows):\n"
        "    if not rows:\n"
        "        code = _helpers.EXIT_OK\n"
        "        return {'status': 'PASS'}, code\n"
        "    return common.aggregate_status('PASS')\n"
        "def _folded_key_dict(rows):\n"
        "    return {'sta' + 'tus': 'PASS'}\n"
        "def _folded_key_write(report):\n"
        "    report['sta' + 'tus'] = 'PASS'\n"
        "    return report\n"
        "def _shadowed_common_local(report):\n"
        "    common = _SN(aggregate_status=lambda *a: ('PASS', 0))\n"
        "    return common.aggregate_status(report)[1]\n"
        "def _loader_common_local(report):\n"
        "    common = _load('common', 'common.py')\n"
        "    return common.aggregate_status(report)[1]\n"
        "def _shadowed_receiver_local(report):\n"
        "    md = _SN(common=_SN(aggregate_status=lambda *a: ('PASS', 0)))\n"
        "    return md.common.aggregate_status(report)[1]\n"
        "def _param_common(report, common):\n"
        "    return common.aggregate_status(report)[1]\n"
        # #892 r24: the loader alias only keeps its role while ``_guard``
        # itself traces to a real module on the binding graph, so the probe
        # carries the same importlib bootstrap the real entries do.
        "import importlib.util\n"
        "_spec = importlib.util.spec_from_file_location('_guard', 'guard.py')\n"
        "_guard = importlib.util.module_from_spec(_spec)\n"
        "_load = _guard.load\n"
        "_shadow_load = _guard.load\n"
        "def _shadow_load(name, path):\n"
        "    return _SN(aggregate_status=lambda *a: ('PASS', 0))\n"
        "def _redefined_loader(report):\n"
        "    common = _shadow_load('common', 'common.py')\n"
        "    return common.aggregate_status(report)[1]\n"
        # #892 r24: the loader base is judged on the binding graph too.  A
        # ``.load`` attribute hung off a stub object spells a loader while
        # returning the stub, so it must not earn the role.
        "_stub_guard = _SN(load=lambda *a: _SN(aggregate_status=lambda *a: ('PASS', 0)))\n"
        "_stub_load = _stub_guard.load\n"
        "def _stub_loader(report):\n"
        "    common = _stub_load('common', 'common.py')\n"
        "    return common.aggregate_status(report)[1]\n"
        "md2 = _SN(common=_SN(aggregate_status=lambda *a: ('PASS', 0)))\n"
        "def _module_receiver(report):\n"
        "    return md2.common.aggregate_status(report)[1]\n"
        "md4 = _load('common', 'common.py')\n"
        "def _module_receiver_proven(report):\n"
        "    return md4.common.aggregate_status(report)[1]\n"
        "md3 = _load('common', 'common.py')\n"
        "def _poison_receiver():\n"
        "    global md3\n"
        "    md3 = _SN(common=_SN(aggregate_status=lambda *a: ('PASS', 0)))\n"
        "def _global_receiver(report):\n"
        "    return md3.common.aggregate_status(report)[1]\n")
    probe892_found, _probe892_duplicates = scan_aggregation_tree(probe892, '<self-test-892>')
    # #892 r21: the key side of the r20 constant folding (GLM sibling r20
    # P1-1) and the function-scope trust withdrawal (GLM sibling r20 P1-2 /
    # boundary P1-1), each with the healthy control beside it.
    folded_dict = probe892_found.get(('<self-test-892>', '_folded_key_dict'))
    if not folded_dict or not unshared_status_slots(folded_dict[1]):
        failures.append('#892 aggregation-audit: detector self-test did not flag a '
                        'summary keyed by a folded constant')
    folded_write = probe892_found.get(('<self-test-892>', '_folded_key_write'))
    if not folded_write or not unshared_status_slots(folded_write[1]):
        failures.append('#892 aggregation-audit: detector self-test did not flag a '
                        'folded-key status write')

    def _probe892_function(name):
        return next((node for node in probe892.body
                     if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
                     and node.name == name), None)

    shadow_local = _probe892_function('_shadowed_common_local')
    if shadow_local is None:
        failures.append('#892 aggregation-audit: detector self-test lost the '
                        'function-scope shadow probe')
    else:
        shadow_shared, _shadow_rebound = _function_shared_names(shadow_local)
        if ('common' in shadow_shared
                or calls_shared(shadow_local, shadow_shared, _shadow_rebound)):
            failures.append('#892 aggregation-audit: detector self-test still trusts '
                            'a function-scope shadowed common')
    loader_local = _probe892_function('_loader_common_local')
    if loader_local is None:
        failures.append('#892 aggregation-audit: detector self-test lost the '
                        'function-scope loader probe')
    else:
        # #892 r23: the loader probe only keeps trust through the module's
        # loader alias; ``_function_shared_names`` is handed that alias set the
        # way ``scan_aggregation_tree`` computes it for a real file.
        loader_shared, _loader_rebound = _function_shared_names(
            loader_local, loaders=_loader_names(probe892, '<self-test-892>'))
        if ('common' not in loader_shared
                or not calls_shared(loader_local, loader_shared, _loader_rebound)):
            failures.append('#892 aggregation-audit: detector self-test rejected the '
                            'apparatus loader bound inside a function')
    # #892 r23 (GLM boundary r22 P1-2): a ``def _load`` inserted after the
    # alias rebinds it to an ordinary function; the call keeps its spelling
    # while returning a stub, so the alias set must drop it.
    redefined_loader = _probe892_function('_redefined_loader')
    if redefined_loader is None:
        failures.append('#892 aggregation-audit: detector self-test lost the '
                        'redefined-loader probe')
    else:
        redefined_shared, _redefined_rebound = _function_shared_names(
            redefined_loader, loaders=_loader_names(probe892, '<self-test-892>'))
        if ('common' in redefined_shared
                or calls_shared(redefined_loader, redefined_shared,
                                _redefined_rebound)):
            failures.append('#892 aggregation-audit: detector self-test still '
                            'trusts a shared binding through a redefined '
                            'loader alias')
    stub_loader = _probe892_function('_stub_loader')
    if stub_loader is None:
        failures.append('#892 aggregation-audit: detector self-test lost the '
                        'stub-loader-base probe')
    else:
        stub_shared, _stub_rebound = _function_shared_names(
            stub_loader, loaders=_loader_names(probe892, '<self-test-892>'))
        if ('common' in stub_shared
                or calls_shared(stub_loader, stub_shared, _stub_rebound)):
            failures.append('#892 aggregation-audit: detector self-test still '
                            'trusts a loader alias hung off a stub object')
    # #892 r24: the proven-source registry keys on ``(rel, name)``, so a
    # same-named redefinition -- Python runs the last def -- and a function
    # parameter/local of the same name both kept the trust while the call went
    # to the stub.  Both are judged on the module binding graph now, with the
    # untampered shape beside each as the healthy control.
    check_rel = ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/'
                 'check_cleanup.py')
    negative_rel = ('src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/'
                    'negative_controls.py')
    # The name is spliced in rather than spelled out: the ``negative_controls``
    # mutation ``calibrate-shadowed-receiver-poison-fn`` anchors on the def line
    # of the proven source and requires it to occur exactly once in this file,
    # so quoting it here would disarm that case (r24 negative run reported
    # ``anchor expired: count=4``).
    proven_probe = 'load_session_domain'
    healthy_definition = ast.parse(
        f"def {proven_probe}():\n"
        "    return None\n")
    redefined_definition = ast.parse(
        f"def {proven_probe}():\n"
        "    return None\n"
        f"def {proven_probe}():\n"
        "    return 'PASS'\n")
    if proven_probe not in _proven_source_names(healthy_definition):
        failures.append('#892 aggregation-audit: detector self-test withdrew '
                        'trust from the real module-level proven source')
    elif not _proven_source(ast.parse(f'{proven_probe}()').body[0].value,
                            check_rel,
                            _proven_source_names(healthy_definition)):
        failures.append('#892 aggregation-audit: detector self-test stopped '
                        'recognizing the healthy proven source call')
    if proven_probe in _proven_source_names(redefined_definition):
        failures.append('#892 aggregation-audit: detector self-test still '
                        'traces a same-named redefined proven source')
    elif _proven_source(ast.parse(f'{proven_probe}()').body[0].value,
                        check_rel,
                        _proven_source_names(redefined_definition)):
        failures.append('#892 aggregation-audit: detector self-test still '
                        'trusts the redefined proven source call')
    rebound_source = ast.parse(
        "def _rebound_source(report):\n"
        "    committed_status_contract = lambda *a: None\n"
        "    common = committed_status_contract(report)\n"
        "    return common.aggregate_status(report)[1]\n")
    healthy_source = ast.parse(
        "def _healthy_source(report):\n"
        "    common = committed_status_contract(report)\n"
        "    return common.aggregate_status(report)[1]\n")
    rebound_call = _function_shared_names(
        rebound_source.body[0], _SHARED_MODULES, negative_rel, frozenset(),
        frozenset({'committed_status_contract'}))
    if ('common' in rebound_call[0]
            or calls_shared(rebound_source.body[0], *rebound_call)):
        failures.append('#892 aggregation-audit: detector self-test still '
                        'trusts a proven source rebound in the same function')
    healthy_call_names = _function_shared_names(
        healthy_source.body[0], _SHARED_MODULES, negative_rel, frozenset(),
        frozenset({'committed_status_contract'}))
    if ('common' not in healthy_call_names[0]
            or not calls_shared(healthy_source.body[0], *healthy_call_names)):
        failures.append('#892 aggregation-audit: detector self-test withdrew '
                        'trust from the unrebound proven source call')
    # #892 r23 (GLM boundary r22 P0-1): receiver relays poisoned at module
    # scope or through ``global`` lose the shared call file-wide, with the
    # proven module-scope receiver kept beside them as the healthy control.
    probe892_poisoned = _withdrawn_receivers(probe892, '<self-test-892>')
    module_receiver = _probe892_function('_module_receiver')
    if module_receiver is None:
        failures.append('#892 aggregation-audit: detector self-test lost the '
                        'module-scope receiver probe')
    elif ('md2' not in probe892_poisoned
            or calls_shared(module_receiver, _SHARED_MODULES,
                            probe892_poisoned)):
        failures.append('#892 aggregation-audit: detector self-test still '
                        'trusts a relay through a module-scope stub receiver')
    proven_receiver = _probe892_function('_module_receiver_proven')
    if proven_receiver is None:
        failures.append('#892 aggregation-audit: detector self-test lost the '
                        'proven module-scope receiver control')
    elif ('md4' in probe892_poisoned
            or not calls_shared(proven_receiver, _SHARED_MODULES,
                                probe892_poisoned)):
        failures.append('#892 aggregation-audit: detector self-test withdrew '
                        'trust from a proven module-scope receiver')
    global_receiver = _probe892_function('_global_receiver')
    if global_receiver is None:
        failures.append('#892 aggregation-audit: detector self-test lost the '
                        'globally poisoned receiver probe')
    elif ('md3' not in probe892_poisoned
            or calls_shared(global_receiver, _SHARED_MODULES,
                            probe892_poisoned)):
        failures.append('#892 aggregation-audit: detector self-test still '
                        'trusts a relay through a receiver poisoned by '
                        '``global``')
    # #1953 fix-r1: the read/hand-over split of the new rules, each with the
    # shape its fix must keep clean *and* the shape it must still catch.  The
    # clean half is what r35 broke (a plain ``__getattribute__`` read and a
    # constant subscript read failed the file closed); the catching half keeps
    # the narrower rule from quietly disarming the audit.
    _1953_rel = '<self-test-1953>'
    pure_read_probe = ast.parse(
        "def main():\n"
        "    getattr(md, '__getattribute__')('__doc__')\n"
        "    _keys = ('__doc__',)\n"
        "    _docs = [getattr(md, _keys[0])]\n"
        "    _saved = [getattr(md, '__doc__'), getattr(md, '__name__')]\n"
        "    _patched = {'a': 1}\n"
        "    _kept = {name: getattr(md, name) for name in _patched}\n"
        "    for name, value in _patched.items():\n"
        "        setattr(md, name, _kept[name])\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if _withdrawn_receivers(pure_read_probe, _1953_rel, frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test withdrew '
                        'the receiver for a pure __getattribute__/constant '
                        'subscript read (the save/restore loop included)')
    handover_probe = ast.parse(
        "def main():\n"
        "    getattr(md, '__getattribute__')('__dict__')['common'] = object()\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(handover_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        '__getattribute__ hand-over')
    held_probe = ast.parse(
        "def main():\n"
        "    _ga = getattr(md, '__getattribute__')\n"
        "    _ga('__dict__')['common'] = object()\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(held_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'held __getattribute__ handle')
    const_write_probe = ast.parse(
        "def main():\n"
        "    _keys = ('__setattr__',)\n"
        "    getattr(md, _keys[0])('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(const_write_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'constant-subscript write name')
    comprehension_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    _hs[0]('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(comprehension_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'comprehension handle escape')
    iterated_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs:\n"
        "        _h('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(iterated_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'iterated handle container')
    aliased_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    _f = _hs[0]\n"
        "    _f('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(aliased_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'aliased handle container')
    container_alias_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    _other = _hs\n"
        "    _other[0]('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(container_alias_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'container handed over under a second name')
    mapping_write_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    _hs[0]['common'] = object()\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(mapping_write_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'mapping write through a handle container')
    list_const_write_probe = ast.parse(
        "def main():\n"
        "    _keys = ['__doc__']\n"
        "    _keys[0] = '__setattr__'\n"
        "    getattr(md, _keys[0])('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(list_const_write_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test folded a '
                        'mutable list into a constant attribute name')
    comp_filter_probe = ast.parse(
        "def main(report, rows):\n"
        "    [row for row in rows"
        " if operator.setitem(report, 'status', 'PASS')]\n")
    if not unshared_status_slots(comp_filter_probe.body[0], module={},
                                 rel=_1953_rel):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'write in a comprehension filter/iterable')
    comp_scope_probe = ast.parse(
        "def main(report):\n"
        "    [h for h in (operator.setitem,)]\n"
        "    h = lambda *a: None\n"
        "    h(report, 'status', 'PASS')\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if (_withdrawn_receivers(comp_scope_probe, _1953_rel, frozenset({'common'}))
            or unshared_status_slots(comp_scope_probe.body[0], module={},
                                     rel=_1953_rel)):
        failures.append('#1953 aggregation-audit: detector self-test leaked a '
                        'comprehension-local binding into the function scope')
    comp_receiver_probe = ast.parse(
        "def main():\n"
        "    [setattr(_h, 'common', object()) for _h in (md,)]\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(comp_receiver_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'receiver write inside a comprehension')
    comp_receiver_scope_probe = ast.parse(
        "def main():\n"
        "    _h = object()\n"
        "    [_h for _h in (md,)]\n"
        "    _h.common = object()\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if _withdrawn_receivers(comp_receiver_scope_probe, _1953_rel,
                            frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test carried a '
                        'comprehension target into the enclosing scope')
    iterated_read_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs:\n"
        "        print(_h)\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if _withdrawn_receivers(iterated_read_probe, _1953_rel,
                            frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test withdrew '
                        'on a pure read of an iterated handle container')
    iterated_after_read_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs:\n"
        "        pass\n"
        "    print(_h)\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if _withdrawn_receivers(iterated_after_read_probe, _1953_rel,
                            frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test withdrew '
                        'on a pure read of a loop target after the loop')
    negative_subscript_probe = ast.parse(
        "def main():\n"
        "    _keys = ('__doc__',)\n"
        "    _docs = [getattr(md, _keys[-1])]\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if _withdrawn_receivers(negative_subscript_probe, _1953_rel,
                            frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test withdrew '
                        'on a negative constant subscript of a tuple')
    loop_rebind_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs:\n"
        "        pass\n"
        "    _h = _h\n"
        "    _h('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(loop_rebind_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'loop target through a self-rebinding')
    loop_finally_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    try:\n"
        "        for _h in _hs:\n"
        "            pass\n"
        "    finally:\n"
        "        _h('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(loop_finally_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'loop target used in a finally block')
    nested_shadow_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs:\n"
        "        pass\n"
        "    def inner(_h):\n"
        "        _h('common', object())\n"
        "    inner(object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if _withdrawn_receivers(nested_shadow_probe, _1953_rel,
                            frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test carried a '
                        'loop target into a nested definition')
    comp_dict_probe = ast.parse(
        "def main():\n"
        "    _g = {0: _h for _h in (md,)}\n"
        "    _g[0].common = object()\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(comp_dict_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'dict-comprehension result container')
    comp_alias_iter_probe = ast.parse(
        "def main():\n"
        "    _items = (md,)\n"
        "    _g = [_h for _h in _items]\n"
        "    _g[0].common = object()\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(comp_alias_iter_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'comprehension result over a name-bound literal')
    loop_rebind_if_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs:\n"
        "        pass\n"
        "    if False:\n"
        "        _h = None\n"
        "    _h('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(loop_rebind_if_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test ended the '
                        'loop-target scan on a conditional rebinding')
    loop_rebind_fresh_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs:\n"
        "        pass\n"
        "    _h = object()\n"
        "    _h()\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if _withdrawn_receivers(loop_rebind_fresh_probe, _1953_rel,
                            frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test withdrew '
                        'after a loop target was rebound to a fresh object')
    comp_result_pure_probe = ast.parse(
        "def main():\n"
        "    _g = [_h for _h in (md,)]\n"
        "    print(len(_g))\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if _withdrawn_receivers(comp_result_pure_probe, _1953_rel,
                            frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test withdrew '
                        'on a comprehension result that is only read')
    comp_filter_false_probe = ast.parse(
        "def main():\n"
        "    _g = [_h for _h in (md,) if False]\n"
        "    _g.append(object())\n"
        "    _g[0].common = object()\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if _withdrawn_receivers(comp_filter_false_probe, _1953_rel,
                            frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test withdrew '
                        'on a statically empty comprehension result')
    loop_return_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs:\n"
        "        pass\n"
        "    return _h\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(loop_return_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'loop target returned to the caller')
    loop_alias_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs:\n"
        "        pass\n"
        "    _f = _h\n"
        "    _f('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(loop_alias_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'loop target through an alias call')
    loop_alias_read_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs:\n"
        "        pass\n"
        "    _f = _h\n"
        "    print(_f)\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if _withdrawn_receivers(loop_alias_read_probe, _1953_rel,
                            frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test withdrew '
                        'on a loop target aliased for a pure read')
    slice_read_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs[:]:\n"
        "        print(_h)\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if _withdrawn_receivers(slice_read_probe, _1953_rel,
                            frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test withdrew '
                        'on a read of an iterated slice')
    copy_read_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs.copy():\n"
        "        print(_h)\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if _withdrawn_receivers(copy_read_probe, _1953_rel,
                            frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test withdrew '
                        'on a read of an iterated copy')
    starred_keys_probe = ast.parse(
        "def main():\n"
        "    _more = ('__setattr__',)\n"
        "    _keys = ('__doc__', *_more)\n"
        "    getattr(md, _keys[-2])('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(starred_keys_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test folded a '
                        'tuple with a starred element')
    comp_handle_relay_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    _g = [_h for _h in _hs]\n"
        "    _g[0]('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(comp_handle_relay_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost a '
                        'handle container relayed through a comprehension')
    getattribute_name_probe = ast.parse(
        "def main():\n"
        "    _key = '__dict__'\n"
        "    md.__getattribute__(_key)['common'] = object()\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    # #1953 fix-r1 round 8: block-ordered scanning, so a rebind that control
    # never reaches must not end the scan, and a rebind that does reach the
    # call must.  The pairs below pin both directions of each new rule.
    loop_finally_rebind_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    try:\n"
        "        for _h in _hs:\n"
        "            pass\n"
        "        raise RuntimeError()\n"
        "        _h = None\n"
        "    finally:\n"
        "        _h('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(loop_finally_rebind_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test trusted a '
                        'rebind after an unconditional raise')
    loop_closure_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs:\n"
        "        pass\n"
        "    def _inner():\n"
        "        _h('common', object())\n"
        "    _inner()\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(loop_closure_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test ignored a '
                        'nested definition that closes over the loop target')
    loop_shadowed_fresh_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    object = lambda: _hs[0]\n"
        "    for _h in _hs:\n"
        "        pass\n"
        "    _h = object()\n"
        "    _h('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(loop_shadowed_fresh_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test read a '
                        'shadowed constructor call as a fresh value')
    loop_with_alias_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs:\n"
        "        pass\n"
        "    import contextlib as _cl\n"
        "    with _cl.nullcontext(_h) as _f:\n"
        "        _f('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(loop_with_alias_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'loop target handed to a with-statement binding')
    loop_except_alias_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs:\n"
        "        pass\n"
        "    try:\n"
        "        raise Exception(_h)\n"
        "    except Exception as _e:\n"
        "        _f = _e.args[0]\n"
        "        _f('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(loop_except_alias_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost the '
                        'loop target round-tripped through an exception')
    comp_relay_chain_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    _g = [_h for _h in _hs]\n"
        "    _j = [_f for _f in _g]\n"
        "    _j[0]('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if 'md' not in _withdrawn_receivers(comp_relay_chain_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test lost a '
                        'handle container relayed through two comprehensions')
    loop_return_compare_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs:\n"
        "        pass\n"
        "    return _h is None\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if _withdrawn_receivers(loop_return_compare_probe, _1953_rel,
                            frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test withdrew '
                        'on a comparison against the loop target')
    loop_rebind_block_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs:\n"
        "        pass\n"
        "    if True:\n"
        "        _h = lambda: None\n"
        "        _h()\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if _withdrawn_receivers(loop_rebind_block_probe, _1953_rel,
                            frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test withdrew '
                        'after a rebind in a statically taken branch')
    comp_relay_empty_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    _g = [_h for _h in _hs if False]\n"
        "    _g.append(lambda *a: None)\n"
        "    _g[0]('common', object())\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if _withdrawn_receivers(comp_relay_empty_probe, _1953_rel,
                            frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test withdrew '
                        'on a relay through a statically empty comprehension')
    loop_local_shadow_probe = ast.parse(
        "def main():\n"
        "    _n = '__setattr__'\n"
        "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
        "    for _h in _hs:\n"
        "        pass\n"
        "    def _inner():\n"
        "        _h = object()\n"
        "        _h()\n"
        "    _inner()\n"
        "def reader(report):\n"
        "    return md.common.aggregate_status(report)[1]\n")
    if _withdrawn_receivers(loop_local_shadow_probe, _1953_rel,
                            frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test carried '
                        'the loop target past a nested local rebinding')
    # #1953 fix-r1 round 9 (codex r5 P0-1..P0-4, P1-5..P1-8 / GLM boundary
    # r5 P0-1, P1-2, P1-3, P2-5): the execution-order model of round 8 was
    # retired for a flat source-ordered scope walk.  Each probe below is one
    # shape the two reviewers reported; the escape count and the read count
    # are pinned separately so a future rewrite cannot quietly trade one for
    # the other.
    def _r9_probe(body, prelude='', signature='def main():\n'):
        head = ("    _n = '__setattr__'\n"
                "    _hs = [getattr(md, _k) for _k in (_n,)]\n"
                "    for _h in _hs:\n"
                "        pass\n")
        return ast.parse(prelude + signature + head + body
                         + "def reader(report):\n"
                         "    return md.common.aggregate_status(report)[1]\n")

    for _label, _body, _prelude, _signature, _want in (
            ("an assignment whose value calls the handle",
             "    _x = [_h('common', object())]\n", '', 'def main():\n', True),
            ("a definition default that writes at def time",
             "    def _inner(_x=_h('common', object())):\n"
             "        pass\n", '', 'def main():\n', True),
            ("a handle handed to an unknown callee in a read-only slot",
             "    print(_helper(_h))\n", '', 'def main():\n', True),
            ("a nested parameter that does not shadow the outer handle",
             "    def _inner():\n"
             "        def _unused(_h):\n"
             "            pass\n"
             "        _h('common', object())\n"
             "    _inner()\n", '', 'def main():\n', True),
            ("a handle handed to an unknown callee inside a comparison",
             "    if _sink(_h) == 0:\n"
             "        pass\n", '', 'def main():\n', True),
            ("a read-only builtin shadowed in an outer scope",
             "    _g = lambda: print(_h)\n"
             "    _g()\n", 'print = _sink\n', 'def main():\n', True),
            ("a constructor shadowed in an outer scope",
             "    _h = object()\n"
             "    _h('common', object())\n", 'object = lambda: None\n',
             'def main():\n', True),
            ("an ambiguous literal binding of the iterated name",
             "    for _m in _items:\n"
             "        _m.common = object()\n", '_items = ()\n_items = (md,)\n',
             'def main():\n', True),
            ("a bare handle in a truth test",
             "    if _h:\n"
             "        pass\n"
             "    while _h:\n"
             "        break\n", '', 'def main():\n', False),
            ("an attribute read on the handle",
             "    _name = _h.__name__\n", '', 'def main():\n', False),
            ("a parameter of a nested definition",
             "    def _inner(print):\n"
             "        pass\n"
             "    print(_h)\n", '', 'def main():\n', False),
            ("a class-body binding",
             "    class _C:\n"
             "        print = 1\n"
             "    print(_h)\n", '', 'def main():\n', False),
            ("a deleted loop target",
             "    del _h\n", '', 'def main():\n', False),
            ("a parameter that shadows a module-level literal",
             "    for _m in _items:\n"
             "        _m.common = object()\n", '_items = (md,)\n',
             'def main(_items):\n', False),
            ("a comprehension whose filter can never hold",
             "    _g = [operator.setitem(_h, 'common', object())"
             " for _h in (md,) if False]\n", '', 'def main():\n', False),
    ):
        _tree = _r9_probe(_body, _prelude, _signature)
        _got = 'md' in _withdrawn_receivers(_tree, _1953_rel,
                                           frozenset({'common'}))
        if _got != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 9)')
    for _label, _body, _prelude, _signature, _want in (
            ("a class body that rebinds the handle name",
             "    class _C:\n"
             "        _h = object()\n"
             "    _h('common', object())\n", '', 'def main():\n', True),
            ("a class body that defines the handle name",
             "    class _C:\n"
             "        def _h():\n"
             "            pass\n"
             "    _h('common', object())\n", '', 'def main():\n', True),
            ("a lambda default that writes at creation time",
             "    _f = lambda _x=_h('common', object()): None\n"
             "    _f()\n", '', 'def main():\n', True),
            ("the bound object behind a method",
             "    _x = _h.__self__\n"
             "    _x.common = object()\n", '', 'def main():\n', True),
            ("a tuple unpack that copies the handle",
             "    _f, _g = _h, _h\n", '', 'def main():\n', False),
            ("an assertion message",
             "    assert _h, _h\n", '', 'def main():\n', True),
            ("a literal bound in another function",
             "    _items = ()\n"
             "    for _m in _items:\n"
             "        _m.common = object()\n",
             "def _other():\n    _items = (md,)\n", 'def main():\n', False),
            ("a comprehension target that shadows the handle",
             "    _g = [_h('common', object()) for _h in (print,)]\n",
             '', 'def main():\n', False),
            ("a rebinding in a finally block",
             "    try:\n"
             "        pass\n"
             "    finally:\n"
             "        _h = object()\n"
             "    _h('common', object())\n", '', 'def main():\n', False),
    ):
        _tree = _r9_probe(_body, _prelude, _signature)
        _got = 'md' in _withdrawn_receivers(_tree, _1953_rel,
                                           frozenset({'common'}))
        if _got != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 10)')
    for _label, _body, _prelude, _signature, _want in (
            ("a literal rebinding that may not run",
             "    _items = (md,)\n"
             "    if _flag:\n"
             "        _items = ()\n"
             "    for _x in _items:\n"
             "        _x.common = object()\n", '', 'def main():\n', True),
            ("a tuple unpack that only copies one element",
             "    _f, _x = _h, [_h('common', object())]\n",
             '', 'def main():\n', True),
            ("a comprehension target that shadows only one of two names",
             "    _f = _h\n"
             "    [_f('common', object()) for _h in (None,)]\n",
             '', 'def main():\n', True),
            ("an assertion message that carries the handle",
             "    try:\n"
             "        assert False, _h\n"
             "    except AssertionError as _e:\n"
             "        _e.args[0]('common', object())\n",
             '', 'def main():\n', True),
            ("a class body that rebinds the handle name before the call",
             "    class _C:\n"
             "        _h = lambda *a: None\n"
             "        _h('common', object())\n", '', 'def main():\n', False),
            ("a builtin write reached through an import alias",
             "    from builtins import setattr as _bs\n"
             "    _bs(md, 'common', None)\n", '', 'def main():\n', True),
    ):
        _tree = _r9_probe(_body, _prelude, _signature)
        _got = 'md' in _withdrawn_receivers(_tree, _1953_rel,
                                           frozenset({'common'}))
        if _got != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 11)')
    # The use sits inside a function whose enclosing scope binds the name
    # *after* the use's line, so this one cannot go through ``_r9_probe``.
    if 'md' not in _withdrawn_receivers(
            ast.parse("def _early():\n"
                      "    for _x in _items:\n"
                      "        _x.common = object()\n"
                      "_items = (md,)\n"
                      "_early()\n"
                      "def reader(report):\n"
                      "    return md.common.aggregate_status(report)[1]\n"),
            _1953_rel, frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test missed an '
                        'enclosing-scope binding written after the use '
                        '(round 11)')
    for _label, _body, _prelude, _signature, _want in (
            ("a class body whose compound statement writes",
             "    class _C:\n"
             "        if _flag:\n"
             "            _h('common', object())\n", '', 'def main():\n', True),
            ("a method that closes over the outer handle",
             "    class _C:\n"
             "        _h = lambda *a: None\n"
             "        def _method(self):\n"
             "            _h('common', object())\n", '', 'def main():\n', True),
            ("a class body that shadows the constructor it calls",
             "    class _C:\n"
             "        object = lambda: _hs[0]\n"
             "        _h = object()\n"
             "        _h('common', object())\n", '', 'def main():\n', True),
            ("a nested unpack that only copies the handle",
             "    _f, (_g, _x) = _h, (_h, None)\n"
             "    print(_f, _g)\n", '', 'def main():\n', False),
            ("a parameter typed by another function's import",
             "    def _other():\n"
             "        from builtins import setattr as _writer\n"
             "    def _inner(_writer):\n"
             "        _writer(md, 'common', None)\n", '', 'def main():\n', False),
    ):
        _tree = _r9_probe(_body, _prelude, _signature)
        _got = 'md' in _withdrawn_receivers(_tree, _1953_rel,
                                           frozenset({'common'}))
        if _got != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 12)')
    for _label, _body, _prelude, _signature, _want in (
            ("a builtin write reached through a module-level import alias",
             "    _w(md, 'common', None)\n",
             "from builtins import setattr as _w\n", 'def main():\n', True),
            ("a later rebinding after the call",
             "    from builtins import setattr as _w13\n"
             "    _w13(md, 'common', None)\n"
             "    _w13 = print\n", '', 'def main():\n', True),
            ("a later non-write import after the call",
             "    from builtins import setattr as _w14\n"
             "    _w14(md, 'common', None)\n"
             "    from builtins import print as _w14\n"
             "    _w14('x')\n", '', 'def main():\n', True),
            ("a non-write import before the call",
             "    from builtins import setattr as _w15\n"
             "    from builtins import print as _w15\n"
             "    _w15(md, 'common', None)\n", '', 'def main():\n', False),
            ("a class body that copies the handle into another name",
             "    class _C:\n"
             "        _f = _h\n"
             "        _f('common', object())\n", '', 'def main():\n', True),
            ("a nested class whose method closes over the outer handle",
             "    class _C:\n"
             "        _h = lambda *a: None\n"
             "        class _D:\n"
             "            def _method(self):\n"
             "                _h('common', object())\n",
             '', 'def main():\n', True),
            ("a method that reads the handle with the builtin print",
             "    class _C:\n"
             "        _h = lambda *a: None\n"
             "        print = _sink\n"
             "        def _method(self):\n"
             "            print(_h)\n", '', 'def main():\n', False),
    ):
        _tree = _r9_probe(_body, _prelude, _signature)
        _got = 'md' in _withdrawn_receivers(_tree, _1953_rel,
                                           frozenset({'common'}))
        if _got != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 13)')
    # Round 14 (codex r10 P0-1 / P0-2 / P0-3 / P0-4): the binding in force is
    # decided by flow, not by source line alone.
    for _label, _body, _prelude, _signature, _want in (
            ("a conditional rebinding of the write alias",
             "    from builtins import setattr as _w18\n"
             "    if _flag:\n"
             "        from builtins import print as _w18\n"
             "    _w18(md, 'common', None)\n", '', 'def main():\n', True),
            ("a write alias rebound after the call inside its loop",
             "    from builtins import print as _w19\n"
             "    for _x in ():\n"
             "        _w19(md, 'common', None)\n"
             "        from builtins import setattr as _w19\n", '', 'def main():\n',
             True),
            ("a write import rebound after the call on the same line",
             "    from builtins import setattr as _w20;"
             " _w20(md, 'common', None);"
             " from builtins import print as _w20\n", '', 'def main():\n', True),
            ("a non-write import before the call on the same line",
             "    from builtins import print as _w21;"
             " _w21(md, 'common', None)\n", '', 'def main():\n', False),
            ("a class body that copies the handle inside a block",
             "    class _C:\n"
             "        if True:\n"
             "            _f = _h\n"
             "        _f('common', object())\n", '', 'def main():\n', True),
            ("a nested function that copies the handle before the call",
             "    def _g():\n"
             "        if True:\n"
             "            _f = _h\n"
             "        _f('common', object())\n"
             "    _g()\n", '', 'def main():\n', True),
            ("a nested function that only reads the copy",
             "    def _g():\n"
             "        _f = _h\n"
             "        print(_f)\n"
             "    _g()\n", '', 'def main():\n', False),
    ):
        _tree = _r9_probe(_body, _prelude, _signature)
        _got = 'md' in _withdrawn_receivers(_tree, _1953_rel,
                                           frozenset({'common'}))
        if _got != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 14)')
    # Round 15 (codex r14 P0-1 / P0-2 / P0-4, P1): the module binding is read
    # at the call site, an assignment whose right side names a write helper
    # keeps the spelling one, the first resolution of a module scope already
    # sees its loop back edges, and a certainly overwritten copy stops being
    # an alias.
    for _label, _source, _want in (
            ("a module call before the write alias is rebound",
             "from builtins import setattr as _w22\n"
             "def _w22_use():\n"
             "    _w22(md, 'common', object())\n"
             "_w22_use()\n"
             "from builtins import print as _w22\n", True),
            ("a module self assignment of the write alias",
             "from builtins import setattr as _w23\n"
             "_w23 = _w23\n"
             "def _w23_use():\n"
             "    _w23(md, 'common', object())\n", True),
            ("a module loop whose first use sits on the back edge",
             "from builtins import print as _w24\n"
             "for _w24_x in ():\n"
             "    _w24(md, 'common', object())\n"
             "    from builtins import setattr as _w24\n", True),
            ("an unconditional non-write import after the module write import",
             "from builtins import setattr as _w25\n"
             "from builtins import print as _w25\n"
             "def _w25_use():\n"
             "    _w25(md, 'common', object())\n", False)):
        _tree = ast.parse(_source
                          + "def reader(report):\n"
                          "    return md.common.aggregate_status(report)[1]\n")
        _got = 'md' in _withdrawn_receivers(_tree, _1953_rel,
                                           frozenset({'common'}))
        if _got != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 15)')
    if 'md' in _withdrawn_receivers(
            _r9_probe("    def _g():\n"
                      "        _f = _h\n"
                      "        _f = lambda *a: None\n"
                      "        _f('common', object())\n"
                      "    _g()\n"),
            _1953_rel, frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test withdrew '
                        'on a copy that is certainly overwritten (round 15)')
    # Round 16 (codex r15 P0-1 / P0-2 / P0-3 / P0-4): a bound write handle
    # keeps its receiver through the alias graph, a duplicated unpack target is
    # decided by its last element, and the module call sites of a definition
    # header and of a loop back edge are read at the call.
    for _label, _source, _want in (
            ("a duplicated unpack target that ends on the handle",
             "def _w26_use():\n"
             "    _w26 = getattr(md, '__setattr__')\n"
             "    _w26f, _w26f = None, _w26\n"
             "    _w26f('common', object())\n", True),
            ("a module call site on a loop back edge",
             "from builtins import print as _w27\n"
             "def _w27_use():\n"
             "    _w27(md, 'common', object())\n"
             "for _w27_x in (0, 1):\n"
             "    _w27_use()\n"
             "    from builtins import setattr as _w27\n", True),
            ("a call in a definition header default",
             "from builtins import setattr as _w28\n"
             "def _w28_use():\n"
             "    _w28(md, 'common', object())\n"
             "def _w28_h(_x=_w28_use()):\n"
             "    pass\n"
             "from builtins import print as _w28\n", True),
            ("an assignment on a loop back edge",
             "from builtins import print as _w29\n"
             "for _w29_x in (0, 1):\n"
             "    _w29a = _w29\n"
             "    _w29a(md, 'common', object())\n"
             "    from builtins import setattr as _w29\n", True),
            ("a bound handle that is certainly overwritten",
             "def _w30_use():\n"
             "    _w30 = getattr(md, '__setattr__')\n"
             "    _w30 = lambda *a: None\n"
             "    _w30('common', object())\n", False)):
        _tree = ast.parse(_source
                          + "def reader(report):\n"
                          "    return md.common.aggregate_status(report)[1]\n")
        _got = 'md' in _withdrawn_receivers(_tree, _1953_rel,
                                           frozenset({'common'}))
        if _got != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 16)')
    # Round 17 (boundary r16 P0-1/P0-2, codex r16 P0-1/P0-2/P0-3/P0-4, sibling
    # r16 P1-1/P1-3): a certain overwrite is not a positional one, the fresh
    # value is judged against the shadowed constructor, and a bound handle is
    # resolved in the scope that binds it instead of from the file-global kinds
    # table.
    for _label, _source, _want in (
            ("a bound handle overwritten only in an if arm",
             "def _w31_use(flag):\n"
             "    _w31 = getattr(md, '__setattr__')\n"
             "    if flag:\n"
             "        _w31 = lambda *a: None\n"
             "    _w31('common', object())\n", True),
            ("a bound handle overwritten in a try body",
             "def _w32_use():\n"
             "    _w32 = getattr(md, '__setattr__')\n"
             "    try:\n"
             "        _w32 = lambda *a: None\n"
             "    except Exception:\n"
             "        pass\n"
             "    _w32('common', object())\n", True),
            ("a bound handle recreated by a shadowed constructor",
             "def _w33_use():\n"
             "    _w33 = getattr(md, '__setattr__')\n"
             "    object = lambda: _w33\n"
             "    _w33 = object()\n"
             "    _w33('common', object())\n", True),
            ("a bound handle another function binds elsewhere",
             "def _w34_a():\n"
             "    _w34 = getattr(a, '__setattr__')\n"
             "def _w34_use():\n"
             "    _w34 = getattr(md, '__setattr__')\n"
             "    _w34('common', object())\n", True),
            ("a bound handle only another function binds to the receiver",
             "def _w35_a():\n"
             "    _w35 = getattr(md, '__setattr__')\n"
             "def _w35_use():\n"
             "    _w35 = getattr(z, '__setattr__')\n"
             "    _w35('common', object())\n", False),
            ("a bound handle rebound to a known non-handle",
             "def _w36_use():\n"
             "    _w36 = getattr(md, '__setattr__')\n"
             "    _w36 = print\n"
             "    _w36('common', object())\n", False)):
        _tree = ast.parse(_source
                          + "def reader(report):\n"
                          "    return md.common.aggregate_status(report)[1]\n")
        _got = 'md' in _withdrawn_receivers(_tree, _1953_rel,
                                            frozenset({'common'}))
        if _got != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 17)')
    for _label, _body, _want in (
            ("a duplicated unpack target that ends fresh",
             "    def _g():\n"
             "        _f, _f = _h, None\n"
             "        print(_f)\n"
             "    _g()\n", False),
            ("a relayed handle that is certainly overwritten",
             "    def _g():\n"
             "        _hs2 = [getattr(md, '__setattr__')]\n"
             "        _f = _hs2[0]\n"
             "        _f = lambda *a: None\n"
             "        _f('common', object())\n"
             "    _g()\n", False)):
        _tree = _r9_probe(_body)
        _got = 'md' in _withdrawn_receivers(_tree, _1953_rel,
                                            frozenset({'common'}))
        if _got != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 17)')
    for _label, _source, _want in (
            ("a status write through a by-name bound handle",
             "def main(report):\n"
             "    _f = getattr(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", True),
            ("a status write through an attribute bound handle",
             "def main(report):\n"
             "    _f = report.__setattr__\n"
             "    _f('status', 'PASS')\n", True)):
        _tree = ast.parse(_source)
        _function = _tree.body[-1]
        _bindings = _name_bindings(_function, _module_binding_events(_tree))
        _wrote = [value for _node in ast.walk(_function)
                  if isinstance(_node, ast.Call)
                  for value in _call_written_values(_node, _bindings)]
        if bool(_wrote) != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 17)')
    # Round 18 (codex r17 P0-1/P0-2/P1-3): a constructor copy of a tracked
    # container keeps the handles, a status handle survives a name-to-name
    # hand-over, and the freshness guard judges the name, not the arity.
    for _label, _source, _want in (
            ("a handle container copied by list()",
             "def _w37_use():\n"
             "    _w37 = [getattr(md, _k) for _k in ('__setattr__',)]\n"
             "    _w37 = list(_w37)\n"
             "    for _w37h in _w37:\n"
             "        _w37h('common', object())\n", True),
            ("a handle container copied by tuple()",
             "def _w38_use():\n"
             "    _w38 = [getattr(md, _k) for _k in ('__setattr__',)]\n"
             "    _w38 = tuple(_w38)\n"
             "    for _w38h in _w38:\n"
             "        _w38h('common', object())\n", True),
            ("a handle container copied but only read",
             "def _w39_use():\n"
             "    _w39 = [getattr(md, '__setattr__')]\n"
             "    _w39 = list(_w39)\n"
             "    print(_w39)\n", False),
            ("a live three-argument write alias",
             "def _w40_use():\n"
             "    _w40 = setattr\n"
             "    _w40(md, 'common', object())\n", True),
            ("a three-argument write alias that is certainly overwritten",
             "def _w41_use():\n"
             "    _w41 = setattr\n"
             "    _w41 = lambda *a: None\n"
             "    _w41(md, 'common', object())\n", False)):
        _tree = ast.parse(_source
                          + "def reader(report):\n"
                          "    return md.common.aggregate_status(report)[1]\n")
        _got = 'md' in _withdrawn_receivers(_tree, _1953_rel,
                                            frozenset({'common'}))
        if _got != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 18)')
    for _label, _source, _want in (
            ("a status write through a name-to-name handle",
             "def main(report):\n"
             "    _g = getattr(report, '__setattr__')\n"
             "    _f = _g\n"
             "    _f('status', 'PASS')\n", True),
            ("a status write through a three-hop handle",
             "def main(report):\n"
             "    _g = report.__setattr__\n"
             "    _f = _g\n"
             "    _h = _f\n"
             "    _h('status', 'PASS')\n", True)):
        _tree = ast.parse(_source)
        _function = _tree.body[-1]
        _bindings = _name_bindings(_function, _module_binding_events(_tree))
        _wrote = [value for _node in ast.walk(_function)
                  if isinstance(_node, ast.Call)
                  for value in _call_written_values(_node, _bindings)]
        if bool(_wrote) != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 18)')
    # Round 19 (GLM boundary r17 P0-1/P1-2): the handle factory may reach the
    # call under another name, and a handle overwritten in a block that always
    # runs -- a ``finally`` body, the taken arm of a statically decided
    # ``if`` -- is really gone.
    for _label, _source, _want in (
            ("a status write through an aliased getattr factory",
             "def main(report):\n"
             "    _ga = getattr\n"
             "    _f = _ga(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", True),
            ("a status write through an aliased builtins.getattr factory",
             "import builtins\n"
             "def main(report):\n"
             "    _ga = builtins.getattr\n"
             "    _f = _ga(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", True),
            ("a status write through a factory alias that is not getattr",
             "def main(report):\n"
             "    _ga = dict\n"
             "    _f = _ga(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", False),
            ("a status write through a nine-hop name chain",
             "def main(report):\n"
             "    _a0 = report.__setitem__\n"
             + ''.join(f"    _a{i} = _a{i - 1}\n" for i in range(1, 9))
             + "    _a8('status', 'PASS')\n", True),
            ("a status handle certainly overwritten by another name",
             "def main(report):\n"
             "    _g = report.__setitem__\n"
             "    _g = print\n"
             "    _f = _g\n"
             "    _f('status', 'PASS')\n", False),
            ("a status handle overwritten only in an if arm",
             "def main(report):\n"
             "    _g = report.__setitem__\n"
             "    if object():\n"
             "        _g = print\n"
             "    _f = _g\n"
             "    _f('status', 'PASS')\n", True)):
        _tree = ast.parse(_source)
        _function = _tree.body[-1]
        _bindings = _name_bindings(_function, _module_binding_events(_tree))
        _wrote = [value for _node in ast.walk(_function)
                  if isinstance(_node, ast.Call)
                  for value in _call_written_values(_node, _bindings)]
        if bool(_wrote) != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 19)')
    # Round 20 (codex r19 P0-1 / P1-3 / P1-4): a hop reads the binding in
    # force at its own assignment, and the getattr spelling resolves the
    # receiver that is really there -- not every binding the name ever had,
    # and not any attribute called ``getattr``.
    for _label, _source, _want in (
            ("a status write through an alias copied before the overwrite",
             "def main(report):\n"
             "    _g = report.__setitem__\n"
             "    _f = _g\n"
             "    _g = print\n"
             "    _f('status', 'PASS')\n", True),
            ("a status write through a two-name relay before the overwrite",
             "def main(report):\n"
             "    _g = report.__setitem__\n"
             "    _h = _g\n"
             "    _f = _h\n"
             "    _g = print\n"
             "    _h = print\n"
             "    _f('status', 'PASS')\n", True),
            ("a status write through a factory alias overwritten after the copy",
             "def main(report):\n"
             "    _ga = getattr\n"
             "    _gb = _ga\n"
             "    _ga = dict\n"
             "    _f = _gb(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", True),
            ("a status write through a getattr on another object",
             "def main(report):\n"
             "    _f = other.getattr(report, '__setitem__')\n"
             "    _f('status', 'PASS')\n", False),
            ("a status write through a factory alias overwritten before the call",
             "def main(report):\n"
             "    _ga = getattr\n"
             "    _ga = lambda *a: lambda *a: None\n"
             "    _f = _ga(report, '__setitem__')\n"
             "    _f('status', 'PASS')\n", False),
            ("a status write through a shadowed getattr name",
             "def main(report):\n"
             "    getattr = lambda *a: (lambda *a: None)\n"
             "    _f = getattr(report, '__setitem__')\n"
             "    _f('status', 'PASS')\n", False)):
        _tree = ast.parse(_source)
        _function = _tree.body[-1]
        _bindings = _name_bindings(_function, _module_binding_events(_tree))
        _wrote = [value for _node in ast.walk(_function)
                  if isinstance(_node, ast.Call)
                  for value in _call_written_values(_node, _bindings)]
        if bool(_wrote) != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 20)')
    # Round 21 (GLM boundary r19 P0-1 / sibling r19 P1-1): the getattr factory
    # reaches the call through an import alias or a module-level alias too.
    for _label, _source, _want in (
            ("a status write through an imported getattr factory",
             "def main(report):\n"
             "    from builtins import getattr as _ga21\n"
             "    _f = _ga21(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", True),
            ("a status write through a module-level getattr alias",
             "ga21 = getattr\n"
             "def main(report):\n"
             "    _f = ga21(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", True),
            ("a status write through an imported factory that is not getattr",
             "def main(report):\n"
             "    from builtins import dict as _ga22\n"
             "    _f = _ga22(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", False)):
        _tree = ast.parse(_source)
        _function = _tree.body[-1]
        _bindings = _name_bindings(_function, _module_binding_events(_tree))
        _wrote = [value for _node in ast.walk(_function)
                  if isinstance(_node, ast.Call)
                  for value in _call_written_values(_node, _bindings)]
        if bool(_wrote) != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 21)')
    # Round 22 (codex r21 P0-1 / P0-2, GLM boundary r21 P0-1 / P0-2): the
    # builtins module itself may be imported under another spelling, and the
    # import event is a second source of the factory spelling -- one
    # assignment of some other value must not hide it.
    for _label, _source, _want in (
            ("a status write through an imported builtins module alias",
             "def main(report):\n"
             "    import builtins as _bi23\n"
             "    _f = _bi23.getattr(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", True),
            ("a status write through a module-level builtins module alias",
             "import builtins as _bi24\n"
             "def main(report):\n"
             "    _f = _bi24.getattr(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", True),
            ("a status write through an assignment before the factory import",
             "def main(report):\n"
             "    _ga25 = print\n"
             "    from builtins import getattr as _ga25\n"
             "    _f = _ga25(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", True),
            ("a status write through a conditionally overwritten factory import",
             "def main(report, flag):\n"
             "    from builtins import getattr as _ga26\n"
             "    if flag:\n"
             "        _ga26 = dict\n"
             "    _f = _ga26(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", True),
            ("a status write through a certainly overwritten factory import",
             "def main(report):\n"
             "    from builtins import getattr as _ga27\n"
             "    _ga27 = print\n"
             "    _f = _ga27(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", False),
            ("a status write through a certainly replaced builtins module alias",
             "def main(report):\n"
             "    import builtins as _bi28\n"
             "    _bi28 = other\n"
             "    _f = _bi28.getattr(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", False)):
        _tree = ast.parse(_source)
        _function = _tree.body[-1]
        _bindings = _name_bindings(_function, _module_binding_events(_tree))
        _wrote = [value for _node in ast.walk(_function)
                  if isinstance(_node, ast.Call)
                  for value in _call_written_values(_node, _bindings)]
        if bool(_wrote) != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 22)')
    # Round 23 (codex r22 P1-1 / P1-2, GLM boundary r22 P1-2): two aliases of
    # one ``import`` statement are two bindings, a builtins import after every
    # call does not reach the calls, and ``bi = bi`` keeps the module reading;
    # the subscript twin of the module reading is pinned here as well.
    for _label, _source, _want in (
            ("a harmless second alias of the same import statement",
             "import builtins as _bi29, os as _bi29\n"
             "def main(report):\n"
             "    _f = _bi29.getattr(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", False),
            ("a builtins alias imported after every call of the function",
             "import json as _bi30\n"
             "def main(report):\n"
             "    _f = _bi30.getattr(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n"
             "main(object())\n"
             "import builtins as _bi30\n", False),
            ("a self-assigned builtins module alias",
             "import builtins as _bi31\n"
             "_bi31 = _bi31\n"
             "def main(report):\n"
             "    _f = _bi31.getattr(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", True),
            ("a subscript write through a copied builtins module name",
             "def main(report):\n"
             "    _bi32 = __builtins__\n"
             "    _bi32['setattr'](report, 'status', 'PASS')\n", True),
            ("a subscript write through a certainly replaced module name",
             "def main(report):\n"
             "    _bi33 = __builtins__\n"
             "    _bi33 = other\n"
             "    _bi33['setattr'](report, 'status', 'PASS')\n", False)):
        _tree = ast.parse(_source)
        _function = next(_node for _node in _tree.body
                         if isinstance(_node, (ast.FunctionDef,
                                               ast.AsyncFunctionDef)))
        _bindings = _name_bindings(_function, _module_binding_events(_tree))
        _wrote = [value for _node in ast.walk(_function)
                  if isinstance(_node, ast.Call)
                  for value in _call_written_values(_node, _bindings)]
        if bool(_wrote) != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 23)')
    # Round 24 (self-check of codex r22 P1-2): the round-23 "every call site is
    # known" answer only holds while the module-scope calls are the only way the
    # function runs.  A function also handed out as a value runs at a time the
    # static call list does not describe, so the later ``import builtins`` is
    # in force there and the write stays a write.
    for _label, _source, _want in (
            ("a builtins alias imported after a callback registration",
             "import json as _bi34\n"
             "_cbs = []\n"
             "def main(report):\n"
             "    _f = _bi34.getattr(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n"
             "main(object())\n"
             "_cbs.append(main)\n"
             "import builtins as _bi34\n", True),
            ("a builtins alias imported before a nested-scope call",
             "import json as _bi35\n"
             "def main(report):\n"
             "    _f = _bi35.getattr(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n"
             "def run():\n"
             "    main(object())\n"
             "main(object())\n"
             "import builtins as _bi35\n"
             "run()\n", True),
            ("a builtins alias imported after direct calls only",
             "import json as _bi36\n"
             "def main(report):\n"
             "    _f = _bi36.getattr(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n"
             "main(object())\n"
             "import builtins as _bi36\n", False),
            ("a self-assigned builtins alias after a certain replacement",
             "import builtins as _bi38\n"
             "class Safe:\n"
             "    @staticmethod\n"
             "    def getattr(obj, name):\n"
             "        return lambda *a, **k: None\n"
             "_bi38 = Safe\n"
             "_bi38 = _bi38\n"
             "def main(report):\n"
             "    _f = _bi38.getattr(report, '__setattr__')\n"
             "    _f('status', 'PASS')\n", False)):
        _tree = ast.parse(_source)
        _function = next(_node for _node in _tree.body
                         if isinstance(_node, (ast.FunctionDef,
                                               ast.AsyncFunctionDef)))
        _bindings = _name_bindings(_function, _module_binding_events(_tree))
        _wrote = [value for _node in ast.walk(_function)
                  if isinstance(_node, ast.Call)
                  for value in _call_written_values(_node, _bindings)]
        if bool(_wrote) != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 24)')
    for _label, _source, _want in (
            ("a handle certainly overwritten in a finally block",
             "def _w42_use():\n"
             "    _w42 = getattr(md, '__setattr__')\n"
             "    try:\n"
             "        pass\n"
             "    finally:\n"
             "        _w42 = lambda *a: None\n"
             "    _w42('common', object())\n", False),
            ("a handle certainly overwritten in a statically true if",
             "def _w43_use():\n"
             "    _w43 = getattr(md, '__setattr__')\n"
             "    if True:\n"
             "        _w43 = lambda *a: None\n"
             "    _w43('common', object())\n", False),
            ("a handle overwritten in an ordinary branch",
             "def _w44_use():\n"
             "    _w44 = getattr(md, '__setattr__')\n"
             "    if object():\n"
             "        _w44 = lambda *a: None\n"
             "    _w44('common', object())\n", True),
            ("a handle overwritten in a finally block that is not reached",
             "def _w45_use():\n"
             "    _w45 = getattr(md, '__setattr__')\n"
             "    try:\n"
             "        pass\n"
             "    finally:\n"
             "        _w45 = lambda *a: None\n"
             "    try:\n"
             "        pass\n"
             "    finally:\n"
             "        _w45 = getattr(md, '__setattr__')\n"
             "    _w45('common', object())\n", True),
            ("a handle container copied through a nested constructor",
             "def _w46_use():\n"
             "    _w46 = [getattr(md, _k) for _k in ('__setattr__',)]\n"
             "    _w46c = list(tuple(_w46))\n"
             "    for _w46h in _w46c:\n"
             "        _w46h('common', object())\n", True),
            ("a handle container copied through a slice",
             "def _w47_use():\n"
             "    _w47 = [getattr(md, _k) for _k in ('__setattr__',)]\n"
             "    _w47c = list(_w47[:])\n"
             "    for _w47h in _w47c:\n"
             "        _w47h('common', object())\n", True),
            ("a handle container certainly replaced before the copy",
             "def _w48_use():\n"
             "    _w48 = [getattr(md, _k) for _k in ('__setattr__',)]\n"
             "    _w48 = [lambda *a: None]\n"
             "    _w48c = list(_w48)\n"
             "    for _w48h in _w48c:\n"
             "        _w48h('common', object())\n", False),
            ("a handle container replaced only in an if arm before the copy",
             "def _w49_use():\n"
             "    _w49 = [getattr(md, _k) for _k in ('__setattr__',)]\n"
             "    if object():\n"
             "        _w49 = [lambda *a: None]\n"
             "    _w49c = list(_w49)\n"
             "    for _w49h in _w49c:\n"
             "        _w49h('common', object())\n", True)):
        _tree = ast.parse(_source
                          + "def reader(report):\n"
                          "    return md.common.aggregate_status(report)[1]\n")
        _got = 'md' in _withdrawn_receivers(_tree, _1953_rel,
                                            frozenset({'common'}))
        if _got != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 19)')
    # Round 20 (codex r19 P0-2): rebinding a name to a fresh container does
    # not undo a handle that is put into that container afterwards.
    for _label, _source, _want in (
            ("a handle container filled in place after a rebinding",
             "def _w50_use():\n"
             "    _w50 = [getattr(md, _k) for _k in ('__setattr__',)]\n"
             "    _w50 = []\n"
             "    _w50.append(getattr(md, '__setattr__'))\n"
             "    _w50c = list(_w50)\n"
             "    for _w50h in _w50c:\n"
             "        _w50h('common', object())\n", True),
            ("a handle container extended in place after a rebinding",
             "def _w51_use():\n"
             "    _w51 = [getattr(md, _k) for _k in ('__setattr__',)]\n"
             "    _w51 = []\n"
             "    _w51.extend([getattr(md, '__setattr__')])\n"
             "    _w51c = tuple(_w51)\n"
             "    for _w51h in _w51c:\n"
             "        _w51h('common', object())\n", True),
            ("a handle container emptied and copied",
             "def _w52_use():\n"
             "    _w52 = [getattr(md, _k) for _k in ('__setattr__',)]\n"
             "    _w52 = []\n"
             "    _w52c = list(_w52)\n"
             "    for _w52h in _w52c:\n"
             "        _w52h('common', object())\n", False),
            ("a handle container certainly replaced and copied",
             "def _w53_use():\n"
             "    _w53 = [getattr(md, _k) for _k in ('__setattr__',)]\n"
             "    _w53 = [lambda *a: None]\n"
             "    _w53c = list(_w53)\n"
             "    for _w53h in _w53c:\n"
             "        _w53h('common', object())\n", False)):
        _tree = ast.parse(_source
                          + "def reader(report):\n"
                          "    return md.common.aggregate_status(report)[1]\n")
        _got = 'md' in _withdrawn_receivers(_tree, _1953_rel,
                                            frozenset({'common'}))
        if _got != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 20)')
    # Round 21 (GLM sibling r19 P1-2): a container spelling that once carried
    # handles is not a handle container forever -- the copy or the alias in
    # force decides, hop by hop.
    for _label, _source, _want in (
            ("a handle container copied then aliased by a fresh name",
             "def _w54_use():\n"
             "    _w54 = [getattr(md, _k) for _k in ('__setattr__',)]\n"
             "    _w54 = [lambda *a: None]\n"
             "    _w54c = list(_w54)\n"
             "    _w54x = _w54c\n"
             "    for _w54h in _w54x:\n"
             "        _w54h('common', object())\n", False),
            ("a handle container replaced and copied twice",
             "def _w55_use():\n"
             "    _w55 = [getattr(md, _k) for _k in ('__setattr__',)]\n"
             "    _w55 = [lambda *a: None]\n"
             "    _w55c = list(_w55)\n"
             "    _w55d = list(_w55c)\n"
             "    for _w55h in _w55d:\n"
             "        _w55h('common', object())\n", False),
            ("a handle container still carrying when aliased",
             "def _w56_use():\n"
             "    _w56 = [getattr(md, _k) for _k in ('__setattr__',)]\n"
             "    _w56c = list(_w56)\n"
             "    _w56x = _w56c\n"
             "    for _w56h in _w56x:\n"
             "        _w56h('common', object())\n", True)):
        _tree = ast.parse(_source
                          + "def reader(report):\n"
                          "    return md.common.aggregate_status(report)[1]\n")
        _got = 'md' in _withdrawn_receivers(_tree, _1953_rel,
                                            frozenset({'common'}))
        if _got != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 21)')
    # Round 13, codex r9 P0-5 / P0-6: the status ruler reads an import alias by
    # the binding in force at the use, including a module-level one.
    for _label, _source, _want in (
            ("a module-level import alias",
             "from builtins import setattr as _sa13\n"
             "def main(report):\n"
             "    _sa13(report, 'status', 'PASS')\n", True),
            ("a call before the rebinding",
             "def main(report):\n"
             "    from builtins import setattr as _sa14\n"
             "    _sa14(report, 'status', 'PASS')\n"
             "    _sa14 = print\n", True),
            ("a later non-write import",
             "def main(report):\n"
             "    from builtins import setattr as _sa15\n"
             "    from builtins import print as _sa15\n"
             "    _sa15(report, 'status', 'PASS')\n", False),
            ("a rebinding before the call",
             "def main(report):\n"
             "    from builtins import setattr as _sa16\n"
             "    _sa16 = print\n"
             "    _sa16(report, 'status', 'PASS')\n", False),
            ("a parameter that shadows the module alias",
             "from builtins import setattr as _sa17\n"
             "def main(report, _sa17):\n"
             "    _sa17(report, 'status', 'PASS')\n", False),
    ):
        _tree = ast.parse(_source)
        _function = _tree.body[-1]
        _bindings = _name_bindings(_function, _module_binding_events(_tree))
        _wrote = [value for _node in ast.walk(_function)
                  if isinstance(_node, ast.Call)
                  for value in _call_written_values(_node, _bindings)]
        if bool(_wrote) != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 13)')
    # Round 14 (codex r10 P0-1 / P0-2 / P0-3 / P1-5): flow decides.
    for _label, _source, _want in (
            ("a conditional rebinding of the write alias",
             "from builtins import setattr as _sa18\n"
             "def main(report):\n"
             "    if _flag:\n"
             "        from builtins import print as _sa18\n"
             "    _sa18(report, 'status', 'PASS')\n", True),
            ("a write alias rebound after the call inside its loop",
             "def main(report):\n"
             "    from builtins import print as _sa19\n"
             "    for _x in ():\n"
             "        _sa19(report, 'status', 'PASS')\n"
             "        from builtins import setattr as _sa19\n", True),
            ("an unconditional non-write import after the write import",
             "from builtins import setattr as _sa20\n"
             "from builtins import print as _sa20\n"
             "def main(report):\n"
             "    _sa20(report, 'status', 'PASS')\n", False)):
        _tree = ast.parse(_source)
        _function = _tree.body[-1]
        _bindings = _name_bindings(_function, _module_binding_events(_tree))
        _wrote = [value for _node in ast.walk(_function)
                  if isinstance(_node, ast.Call)
                  for value in _call_written_values(_node, _bindings)]
        if bool(_wrote) != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 14)')
    # Round 15 (codex r14 P0-1 / P0-2 / P0-3, GLM sibling r14 P2-3): the same
    # three module shapes on the status side, plus the same-line rebinding the
    # r14 column discipline introduced.
    for _label, _source, _want in (
            ("a module call before the write alias is rebound",
             "from builtins import setattr as _sa22\n"
             "def main(report):\n"
             "    _sa22(report, 'status', 'PASS')\n"
             "main(None)\n"
             "from builtins import print as _sa22\n", True),
            ("a module self assignment of the write alias",
             "from builtins import setattr as _sa23\n"
             "_sa23 = _sa23\n"
             "def main(report):\n"
             "    _sa23(report, 'status', 'PASS')\n", True),
            ("a conditional assignment of the write helper",
             "from builtins import print as _sa24\n"
             "def main(report):\n"
             "    if _flag:\n"
             "        _sa24 = setattr\n"
             "    _sa24(report, 'status', 'PASS')\n", True),
            ("a non-write rebinding after the call on the same line",
             "def main(report):\n"
             "    from builtins import setattr as _sa25;"
             " _sa25 = print; _sa25(report, 'status', 'PASS')\n", False)):
        _tree = ast.parse(_source)
        _function = [node for node in _tree.body
                     if isinstance(node, (ast.FunctionDef,
                                          ast.AsyncFunctionDef))][-1]
        _bindings = _name_bindings(_function, _module_binding_events(_tree))
        _wrote = [value for _node in ast.walk(_function)
                  if isinstance(_node, ast.Call)
                  for value in _call_written_values(_node, _bindings)]
        if bool(_wrote) != _want:
            failures.append(
                '#1953 aggregation-audit: detector self-test '
                + ('missed ' if _want else 'withdrew on ')
                + _label + ' (round 15)')
    # Round 12, codex r8 P0-1: the enclosing scope rebinds the name to another
    # literal *after* the nested reader was called, so the first binding is
    # still the one the call saw.
    if 'md' not in _withdrawn_receivers(
            ast.parse("_items = (md,)\n"
                      "def _early():\n"
                      "    for _x in _items:\n"
                      "        _x.common = object()\n"
                      "_early()\n"
                      "_items = ()\n"
                      "def reader(report):\n"
                      "    return md.common.aggregate_status(report)[1]\n"),
            _1953_rel, frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test missed a '
                        'later literal rebinding in the enclosing scope '
                        '(round 12)')
    if 'md' not in _withdrawn_receivers(getattribute_name_probe, _1953_rel,
                                        frozenset({'common'})):
        failures.append('#1953 aggregation-audit: detector self-test trusted a '
                        'non-literal __getattribute__ mapping name')
    receiver_local = _probe892_function('_shadowed_receiver_local')
    if receiver_local is None:
        failures.append('#892 aggregation-audit: detector self-test lost the '
                        'function-scope receiver probe')
    else:
        receiver_shared, receiver_rebound = _function_shared_names(receiver_local)
        if ('md' not in receiver_rebound
                or calls_shared(receiver_local, receiver_shared, receiver_rebound)):
            failures.append('#892 aggregation-audit: detector self-test still trusts a '
                            'relayed shared call through a rebound receiver')
    param_local = _probe892_function('_param_common')
    if param_local is None:
        failures.append('#892 aggregation-audit: detector self-test lost the '
                        'parameter probe')
    elif not calls_shared(param_local):
        failures.append('#892 aggregation-audit: detector self-test stopped trusting '
                        'the common parameter contract')
    direct = probe892_found.get(('<self-test-892>', '_direct_summary'))
    if not direct or 'return-report-status' not in direct[0] or calls_shared(direct[1]):
        failures.append('#892 aggregation-audit: detector self-test did not flag a '
                        'directly returned summary dict')
    inline = probe892_found.get(('<self-test-892>', '_inline_branch'))
    if not inline or not unshared_status_slots(inline[1]):
        failures.append('#892 aggregation-audit: detector self-test did not flag a '
                        'branch exit computed around the shared call')
    pair = probe892_found.get(('<self-test-892>', '_shared_pair'))
    if not pair or unshared_status_slots(pair[1]) or not calls_shared(pair[1]):
        failures.append('#892 aggregation-audit: detector self-test rejected a '
                        'shared status/exit pair')
    assigned = probe892_found.get(('<self-test-892>', '_assigned_summary'))
    if not assigned or not unshared_status_slots(assigned[1]):
        failures.append('#892 aggregation-audit: detector self-test did not follow a '
                        'summary dict returned through a local name')
    bare = probe892_found.get(('<self-test-892>', '_bare_zero'))
    if not bare or not unshared_status_slots(bare[1]):
        failures.append('#892 aggregation-audit: detector self-test did not flag a '
                        'bare optimistic exit code')
    arm = probe892_found.get(('<self-test-892>', '_conditional_arm'))
    if not arm or not unshared_status_slots(arm[1]):
        failures.append('#892 aggregation-audit: detector self-test did not flag an '
                        'optimistic exit inside a conditional arm')
    bare_return = probe892_found.get(('<self-test-892>', '_bare_return'))
    if not bare_return or not unshared_status_slots(bare_return[1]):
        failures.append('#892 aggregation-audit: detector self-test did not flag a '
                        'bare return (SystemExit(None) exits 0)')
    payload = probe892_found.get(('<self-test-892>', '_payload_name'))
    if (not payload or 'return-report-status' not in payload[0]
            or not unshared_status_slots(payload[1])):
        failures.append('#892 aggregation-audit: detector self-test did not enumerate a '
                        'summary dict returned under an arbitrary name')
    relayed = probe892_found.get(('<self-test-892>', '_relayed_status'))
    if not relayed or not unshared_status_slots(relayed[1]):
        failures.append('#892 aggregation-audit: detector self-test did not follow a status '
                        'slot back through its local variable')
    system_exit = probe892_found.get(('<self-test-892>', '_system_exit'))
    if not system_exit or not unshared_status_slots(system_exit[1]):
        failures.append('#892 aggregation-audit: detector self-test did not flag an '
                        'optimistic raise SystemExit(0)')
    assigned_status = probe892_found.get(('<self-test-892>', '_assigned_status'))
    if not assigned_status or not unshared_status_slots(assigned_status[1]):
        failures.append('#892 aggregation-audit: detector self-test did not flag a clean '
                        "verdict written into a report['status'] slot")
    # #892 r3: each remaining bypass family gets its own probe, so deleting the
    # matching rule turns the audit red instead of silently reopening the hole.
    none_exit = probe892_found.get(('<self-test-892>', '_none_exit'))
    if not none_exit or not unshared_status_slots(none_exit[1]):
        failures.append('#892 aggregation-audit: detector self-test did not flag '
                        'return None from a registered exit point')
    system_exit_none = probe892_found.get(('<self-test-892>', '_system_exit_none'))
    if not system_exit_none or not unshared_status_slots(system_exit_none[1]):
        failures.append('#892 aggregation-audit: detector self-test did not flag '
                        'raise SystemExit(None)')
    relayed_exit = next((node for node in probe892.body
                         if getattr(node, 'name', None) == '_relayed_exit'), None)
    if not relayed_exit or not unshared_status_slots(relayed_exit):
        failures.append('#892 aggregation-audit: detector self-test did not follow an '
                        'exit code back through its local variable')
    subscript_slot = probe892_found.get(('<self-test-892>', '_subscript_slot'))
    if not subscript_slot or not unshared_status_slots(subscript_slot[1]):
        failures.append('#892 aggregation-audit: detector self-test did not follow a '
                        "status slot through a local dict's subscript")
    lookalike = probe892_found.get(('<self-test-892>', '_lookalike'))
    if not lookalike or not unshared_status_slots(lookalike[1]):
        failures.append('#892 aggregation-audit: detector self-test accepted a local '
                        'aggregate_status lookalike as the shared call')
    updated_slot = probe892_found.get(('<self-test-892>', '_updated_slot'))
    if not updated_slot or not unshared_status_slots(updated_slot[1]):
        failures.append('#892 aggregation-audit: detector self-test did not flag a clean '
                        'verdict written through dict.update()')
    duplicate_key = probe892_found.get(('<self-test-892>', '_duplicate_key'))
    if not duplicate_key or not unshared_status_slots(duplicate_key[1]):
        failures.append('#892 aggregation-audit: detector self-test did not flag a '
                        'duplicate status key whose runtime value is clean')
    unpacked = probe892_found.get(('<self-test-892>', '_unpacked'))
    if not unpacked or not unshared_status_slots(unpacked[1]):
        failures.append('#892 aggregation-audit: detector self-test did not flag a '
                        'clean status hidden in a {**...} expansion')
    assign_only = probe892_found.get(('<self-test-892>', '_assign_only'))
    if (not assign_only or 'assign-report-status' not in assign_only[0]
            or not unshared_status_slots(assign_only[1])):
        failures.append('#892 aggregation-audit: detector self-test did not enumerate a '
                        'summary dict assigned under an arbitrary name')
    # #892 r4: a shared assignment in the other branch must not excuse a zero
    # branch, while an initializer that every following path overwrites is fine
    # (the healthy ``session_domain.py:main`` shape — false-positiving it would
    # force a real exclusion instead of a judged exemption).
    shared_excuse = probe892_found.get(('<self-test-892>', '_shared_excuse'))
    if not shared_excuse or not unshared_status_slots(shared_excuse[1]):
        failures.append('#892 aggregation-audit: detector self-test let a shared '
                        'assignment in another branch excuse a literal exit code')
    code_initializer = probe892_found.get(('<self-test-892>', '_code_initializer'))
    if not code_initializer or unshared_status_slots(code_initializer[1]):
        failures.append('#892 aggregation-audit: detector self-test flagged a zero '
                        'initializer that every following path overwrites')
    # #892 r5: reachability beyond "some later sibling", reassignment chains,
    # inline int(), a zero read back out of a local dict, a nested helper that
    # returns 0, the verdict spelling, the write-only enumeration marker and an
    # alias cycle (MiniMax boundary P0-1/P1-3/P1-4, sibling P1-1, GLM P1-1/P2-1).
    for probe_name, message in (
            ('_early_return_zero', 'an early return inside a zero branch'),
            ('_two_hop_zero', 'an exit code relayed through two local names'),
            ('_int_call_code', 'an exit code computed with int()'),
            ('_dict_code_zero', 'a zero read back out of a local dict'),
            ('_nested_def_zero', 'a nested helper that returns a constant zero'),
            ('_verdict_slot', "a clean verdict written into a report['verdict'] slot"),
            ('_alias_cycle', 'a status slot fed by an alias cycle')):
        probe = next((node for node in probe892.body
                      if getattr(node, 'name', None) == probe_name), None)
        if not probe or not unshared_status_slots(probe):
            failures.append(f'#892 aggregation-audit: detector self-test did not flag '
                            f'{message}')
    for probe_name, message in (
            ('_sys_exit', 'an optimistic sys.exit(0)'),
            ('_walrus_status', "a walrus write into a report['status'] slot"),
            ('_module_status', 'an unresolvable name written into a status slot'),
            ('_rebuilt_summary', 'a clean literal rebuilt into a summary fed to the map')):
        probe = next((node for node in probe892.body
                      if getattr(node, 'name', None) == probe_name), None)
        if not probe or not unshared_status_slots(probe):
            failures.append(f'#892 aggregation-audit: detector self-test did not flag '
                            f'{message}')
    delegated = next((node for node in probe892.body
                      if getattr(node, 'name', None) == '_delegated_exit'), None)
    probe_modules = {node.name: node for node in ast.walk(probe892)
                     if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))}
    if not delegated or not unshared_status_slots(delegated, None, probe_modules):
        failures.append('#892 aggregation-audit: detector self-test did not flag an '
                        'exit code delegated to a same-module helper without a marker')
    # #892 r6: exit-0 spellings outside the ``Return`` arm, a boolean exit
    # expression, delegation chains and an unproven subscript slot (GLM
    # sibling P1-1/P1-2/P1-3, boundary P1-1/P1-2, MiniMax boundary P1-1/P1-2).
    for probe_name, message in (
            ('_boolean_exit', 'a boolean exit expression'),
            ('_sys_exit_relay', 'an exit code relayed into sys.exit()'),
            ('_system_exit_bare', 'a bare raise SystemExit'),
            ('_delegated_chain', 'an exit code delegated twice'),
            ('_relay_zero_helper', 'an exit code relayed through a helper local'),
            ('_bare_exit_helper', 'a bare raise SystemExit inside a helper'),
            ('_attr_head_clean', 'a clean verdict returned by an attribute call'),
            ('_mixed_branch_helper', 'a helper branch that raises beside a fall-off end'),
            ('_shared_decoy', 'a helper that mentions the map and then returns 0'),
            ('_container_slot', 'a status slot filled from an unproven subscript')):
        probe = next((node for node in probe892.body
                      if getattr(node, 'name', None) == probe_name), None)
        if not probe or not unshared_status_slots(probe, None, probe_modules):
            failures.append(f'#892 aggregation-audit: detector self-test did not flag '
                            f'{message}')
    # #892 r6: the README promises a non-zero exit literal is legal, but only on
    # its own -- a status/exit *pair* with a literal code is a batch computation
    # and still flagged (MiniMax boundary r5 P2-4).  Both sides are pinned here so
    # the promise cannot drift from the rule again.
    plain_nonzero = probe892_found.get(('<self-test-892>', '_plain_nonzero'))
    if not plain_nonzero or unshared_status_slots(plain_nonzero[1]):
        failures.append('#892 aggregation-audit: detector self-test rejected a '
                        'non-zero exit literal')
    pair_literal = probe892_found.get(('<self-test-892>', '_pair_literal'))
    if not pair_literal or not unshared_status_slots(pair_literal[1]):
        failures.append('#892 aggregation-audit: detector self-test did not flag a '
                        'status/exit pair whose code is a literal')
    verdict_dict = probe892_found.get(('<self-test-892>', '_verdict_dict'))
    if not verdict_dict or 'assign-report-status' not in verdict_dict[0]:
        failures.append('#892 aggregation-audit: detector self-test did not enumerate '
                        'a summary dict keyed ``verdict``')
    # #892 r7: the four return-path shapes the r6 reviews found still silent --
    # a summary dict in a conditional arm, a bound summary returned from an arm,
    # an unprovable ``update(**name)`` expansion, a zero arm inside an exit
    # call's argument and an exit call in ``return`` position (GLM r6 boundary
    # P0-1/P1-2, sibling P1-1/P1-2).
    for probe_name, needle, message in (
            ('_conditional_summary', "status slot 'PASS'",
             'a summary dict in a conditional arm'),
            ('_bound_summary_arm', "status slot 'PASS'",
             'a conditional arm returning a bound summary'),
            ('_expanded_update', 'report.update(**payload)',
             'a keyword expansion the function never built'),
            ('_exit_conditional', 'inline exit code 0 if',
             'a zero arm inside an exit call argument'),
            ('_return_exit_call', 'inline optimistic exit (sys.exit(0))',
             'an exit call in return position'),
            # #892 r8: three more false-pass shapes from the r6 reviews -- an
            # annotated rebuild of the summary, a bare ``verdict`` write and a
            # ``bool(...)`` exit code (MiniMax sibling r6 P2-1/P2-2/P2-3).
            ('_ann_rebuilt_summary', "inline status/exit assignment {**report",
             'an annotated summary rebuilt from the existing one'),
            ('_bare_verdict', "inline status/exit assignment 'PASS'",
             'a clean status written to a bare ``verdict`` name'),
            ('_bool_exit', 'inline exit code bool(',
             'an exit code computed with bool()'),
            # #892 r9: the relay and write spellings the r7 reviews found.
            ('_bool_relay_exit', '(bool() relay) code',
             'a bool() code relayed through a local'),
            ('_walrus_exit', '(int() relay) code',
             'a walrus-bound int() code'),
            ('_unbound_exit_name', '(unbound relay) EXIT_CODE',
             'an exit-code name with no local binding'),
            ('_update_positional', 'inline status/exit assignment report.update(extra)',
             'a positional update() with an invisible payload'),
            ('_augassign_update', 'inline status/exit update report |= extra',
             'an augmented |= write with an invisible payload'),
            ('_tuple_verdict', "inline status/exit assignment ('PASS', 0)",
             'a tuple-target clean verdict write'),
            ('_cleanup_word',
             "inline status/exit assignment 'PASS_WITH_CLEANUP_UNVERIFIED'",
             'the cleanup-verification word written straight into a slot'),
            # #892 r10: the r9-round shapes.
            ('_unbound_exit_relay', '(unbound relay) code',
             'an unbound exit-code name relayed through a local'),
            ('_delegated_attribute', 'inline exit code helpers.exit_code(report)',
             'an exit code delegated through an attribute callee'),
            ('_delegated_bound_name', 'inline exit code helper(report)',
             'an exit code delegated through a name bound to the helper'),
            ('_call_status_write', "inline status/exit assignment report.get(",
             'a clean status written through a call result'),
            ('_bound_rebinding', 'inline exit code (literal relay) report',
             'a summary name rebound to a bare zero before the return')):
        probe = next((node for node in probe892.body
                      if getattr(node, 'name', None) == probe_name), None)
        found_slots = unshared_status_slots(probe, None, probe_modules) if probe else []
        if not any(needle in slot for slot in found_slots):
            failures.append(f'#892 aggregation-audit: detector self-test did not flag '
                            f'{message} (found {found_slots!r})')
    # #892 r9: the fail-closed update rule may not swallow the legal shapes --
    # a locally built dict and a dict literal carry visible slots.
    bound_update = next((node for node in probe892.body
                         if getattr(node, 'name', None) == '_bound_update'), None)
    bound_slots = unshared_status_slots(bound_update, None, probe_modules) \
        if bound_update else None
    if not bound_update or bound_slots:
        failures.append('#892 aggregation-audit: detector self-test rejected a '
                        f'visible update() payload (found {bound_slots!r})')
    update_only = probe892_found.get(('<self-test-892>', '_update_only'))
    if (not update_only or 'write-status' not in update_only[0]
            or not unshared_status_slots(update_only[1])):
        failures.append('#892 aggregation-audit: detector self-test did not enumerate a '
                        'function whose only status evidence is a dict.update() write')
    # #892 r10: an opaque update() payload on a summary must make the function
    # enumerable even when the only write the audit can see is that payload.
    unproven = probe892_found.get(('<self-test-892>', '_unproven_update_summary'))
    if not unproven or 'write-status' not in unproven[0]:
        failures.append('#892 aggregation-audit: detector self-test did not enumerate a '
                        'function whose only status evidence is an opaque update()')
    # #892 r10: a registered point that never returns and never exits leaves the
    # entry point with None (exit 0); the per-return rules have nothing to see.
    no_return_probe = ast.parse(
        "def _silent(report):\n"
        "    print(report)\n")
    silent = next(node for node in no_return_probe.body
                  if getattr(node, 'name', None) == '_silent')
    if not any('no return or exit path' in slot
               for slot in unshared_status_slots(silent, ('probe', '_silent'),
                                                 probe_modules)):
        failures.append('#892 aggregation-audit: detector self-test did not flag a '
                        'registered point with no return or exit path')
    # #892 r11: the r10 reviews' delegation and enumeration holes, and the one
    # false alarm the new no-exit-path rule could raise.
    for probe_name, needle, message in (
            ('_delegated_value_relay', '(delegated relay) code',
             'an exit code relayed from a module-level helper through a local'),
            ('_bare_return_delegate', 'inline exit code _exit_bare(report)',
             'a module-level helper with a bare return (None exits 0)'),
            ('_statement_delegate', 'inline exit code _exit_statement(report)',
             'a module-level helper that exits with sys.exit(0) in statement position')):
        probe = next((node for node in probe892.body
                      if getattr(node, 'name', None) == probe_name), None)
        found_slots = unshared_status_slots(probe, None, probe_modules) if probe else []
        if not any(needle in slot for slot in found_slots):
            failures.append(f'#892 aggregation-audit: detector self-test did not flag '
                            f'{message} (found {found_slots!r})')
    augassign = probe892_found.get(('<self-test-892>', '_augassign_summary'))
    if (not augassign or 'write-status' not in augassign[0]
            or not unshared_status_slots(augassign[1])):
        failures.append('#892 aggregation-audit: detector self-test did not enumerate a '
                        'function whose only status evidence is an augmented |= write')
    raising = next((node for node in probe892.body
                    if getattr(node, 'name', None) == '_raising_only'), None)
    if raising is not None and any(
            'no return or exit path' in slot
            for slot in unshared_status_slots(raising, ('probe', '_raising_only'),
                                              probe_modules)):
        failures.append('#892 aggregation-audit: detector self-test reported a point '
                        'that raises (a propagating error is not an optimistic exit)')
    # #892 r15: the exit call is the call-family sibling of ``raise
    # SystemExit(...)`` and must enumerate on its own, and a tuple target
    # writes the same status slot as the plain subscript form.
    for probe_name, marker, message in (
            ('_exit_call_status', 'exit-call-status',
             'a function whose only exit evidence is sys.exit(<status>)'),
            ('_os_exit_zero', 'exit-call-status',
             'a function whose only exit evidence is os._exit(0)')):
        probe = probe892_found.get(('<self-test-892>', probe_name))
        if not probe or marker not in probe[0]:
            failures.append(f'#892 aggregation-audit: detector self-test did not enumerate '
                            f'{message}')
    tuple_slot = probe892_found.get(('<self-test-892>', '_tuple_slot_write'))
    if (not tuple_slot or 'assign-status' not in tuple_slot[0]
            or not unshared_status_slots(tuple_slot[1])):
        failures.append('#892 aggregation-audit: detector self-test did not enumerate a '
                        'tuple-target status write')
    for probe_name, message in (
            ('_bare_raise_exit', 'a bare raise SystemExit (exits 0)'),
            ('_zero_raise_exit', 'raise SystemExit(0)')):
        probe = probe892_found.get(('<self-test-892>', probe_name))
        if not probe or 'raise-exit-status' not in probe[0]:
            failures.append(f'#892 aggregation-audit: detector self-test did not enumerate '
                            f'{message}')
    # GLM boundary r16 P0-1 / P1-1: an exit spelling behind an import alias,
    # and a status write behind ``setattr`` / ``__setitem__``, must be seen by
    # the same rules as their literal spellings; ``_setattr_non_status`` is the
    # negative control that keeps a non-status attribute out of the family.
    alias_probe = ast.parse('import sys as s\n'
                            'from sys import exit as bail\n'
                            'from builtins import SystemExit as SE\n')
    alias_map = _module_exit_aliases(alias_probe)
    if (alias_map.get('s') != 'sys' or alias_map.get('bail') != 'sys.exit'
            or alias_map.get('SE') != 'builtins.SystemExit'):
        failures.append('#892 aggregation-audit: detector self-test lost an exit alias '
                        'bound by a module-level import')
    if _is_exit_call(ast.parse('other.exit(0)').body[0].value):
        failures.append('#892 aggregation-audit: detector self-test treated an arbitrary '
                        'attribute as a process exit')
    for probe_name, marker, message in (
            ('_alias_sysexit', 'exit-call-status', 'import sys as _s; _s.exit(0)'),
            ('_alias_from_import', 'exit-call-status',
             'from sys import exit as _bail; _bail(0)'),
            ('_alias_builtins_exit', 'exit-call-status', 'builtins.exit(0)'),
            ('_alias_posix_exit', 'exit-call-status', 'posix._exit(0)'),
            ('_alias_local_binding', 'exit-call-status', 'bail = sys.exit; bail(0)'),
            ('_alias_system_exit', 'raise-exit-status',
             'from builtins import SystemExit as _SE; raise _SE(0)')):
        probe = probe892_found.get(('<self-test-892>', probe_name))
        if not probe or marker not in probe[0]:
            failures.append(f'#892 aggregation-audit: detector self-test did not enumerate '
                            f'{message}')
    for probe_name in ('_setattr_slot', '_dunder_setitem_slot'):
        probe = probe892_found.get(('<self-test-892>', probe_name))
        if not probe or 'write-status' not in probe[0]:
            failures.append(f'#892 aggregation-audit: detector self-test did not enumerate '
                            f'a status write spelled {probe_name}')
    non_status = probe892_found.get(('<self-test-892>', '_setattr_non_status'))
    if non_status and 'write-status' in non_status[0]:
        failures.append('#892 aggregation-audit: detector self-test enumerated a setattr '
                        'of a non-status attribute')
    # GLM boundary r17 P0-1 / P1-1 and MiniMax sibling r17 P1-1 / GLM sibling
    # r17 P2-2: the alias sources a real tree actually uses -- a function-local
    # ``import sys as s``, a module-level ``bail = sys.exit``, a ``try:``
    # guarded import -- and the unbound write spellings
    # ``dict.__setitem__`` / ``operator.setitem`` / ``builtins.setattr``.
    # Each shape was invisible before r18 (the module walk read direct children
    # only, and the binding map never collected ``import`` statements), so a
    # registered point could exit 0 or stamp its own status and stay green.
    module_aliases = _module_exit_aliases(probe892)
    if module_aliases.get('_module_alias') != 'sys.exit':
        failures.append('#892 aggregation-audit: detector self-test lost a module-level '
                        'assignment alias (``bail = sys.exit``)')
    if module_aliases.get('_try_s') != 'sys':
        failures.append('#892 aggregation-audit: detector self-test lost an import alias '
                        'bound inside a module-level ``try:`` block')
    for probe_name, markers, message in (
            ('_module_binding_alias', ('exit-call-status',),
             'a module-level assignment alias (``bail = sys.exit; bail(0)``)'),
            ('_local_import_alias', ('exit-call-status',),
             'a function-local ``import sys as s; s.exit(0)``'),
            ('_local_from_import', ('exit-call-status',),
             'a function-local ``from sys import exit as bail; bail(0)``'),
            ('_try_import_alias', ('exit-call-status',),
             'an alias bound by a ``try:`` guarded import'),
            ('_attr_status_slot', ('assign-status', 'write-status'),
             'an attribute status write (``report.status = ...``)'),
            ('_dunder_setattr_slot', ('write-status',),
             'a status write spelled ``report.__setattr__(...)``'),
            ('_unbound_setitem_slot', ('write-status',),
             'a status write spelled ``dict.__setitem__(report, ...)``'),
            ('_operator_setitem_slot', ('write-status',),
             'a status write spelled ``operator.setitem(report, ...)``'),
            ('_builtins_setattr_slot', ('write-status',),
             'a status write spelled ``builtins.setattr(report, ...)``'),
            ('_builtins_subscript_setattr_slot', ('write-status',),
             'a status write spelled ``__builtins__[\'setattr\'](report, ...)``'),
            # #892 r30: the alias and the unbound/bound spellings the status
            # ruler read only by the bare name (GLM sibling r29 P1-7).
            ('_alias_setattr_slot', ('write-status',),
             'a status write through a ``setattr`` alias (``sa = setattr``)'),
            ('_unbound_setdefault_slot', ('write-status',),
             'a status write spelled ``dict.setdefault(report, ...)``'),
            ('_getattr_dunder_slot', ('write-status',),
             'a status write spelled ``getattr(report, \'__setitem__\')(...)``')):
        probe = probe892_found.get(('<self-test-892>', probe_name))
        if not probe or not set(markers) & set(probe[0]):
            failures.append(f'#892 aggregation-audit: detector self-test did not enumerate '
                            f'{message}')
    if not _is_status_target(ast.parse("report.status = 'PASS'").body[0].targets[0]):
        failures.append('#892 aggregation-audit: detector self-test rejected an attribute '
                        'status target')
    # GLM + anthropic simplify r22 P2-1: the slot layer delegates to the
    # enumeration's predicate, so the four spellings must agree in one place;
    # a probe per spelling pins the delegation instead of trusting the same
    # body to stay in sync by inspection.
    for snippet in ("status = 'PASS'", "x['status'] = 'PASS'",
                    "report.status = 'PASS'", "status, other = 'PASS', 1"):
        target = ast.parse(snippet).body[0].targets[0]
        if _writes_status_slot(target) != _is_status_target(target):
            failures.append('#892 aggregation-audit: detector self-test found the slot '
                            f'layer and the enumeration disagreeing on `{snippet}`')
    # GLM boundary r18 P0-1 / P1-1: an exit code handed back as an attribute or
    # as a subscript that no shared call produced has no traceable origin, so
    # it gets the same fail-closed verdict as the bare name one branch up; the
    # module alias bound inside a module-level ``try:`` is the shape
    # ``_guard.py`` itself uses.
    if module_aliases.get('_guarded_bail') != 'sys.exit':
        failures.append('#892 aggregation-audit: detector self-test lost an assignment alias '
                        'bound inside a module-level ``try:`` block')
    for probe_name, needle, message in (
            ('_guarded_assignment_alias', 'exit-call-status',
             'an exit call through a ``try:``-guarded module alias'),
            ('_attribute_exit_arm', 'attribute relay',
             'an exit code handed back as ``helpers.EXIT_OK``'),
            ('_subscript_exit_arm', 'subscript relay',
             'an exit code read out of an untracked container'),
            ('_attribute_code_relay', 'attribute relay',
             'an attribute exit code relayed through a local name')):
        probe = probe892_found.get(('<self-test-892>', probe_name))
        if not probe:
            failures.append(f'#892 aggregation-audit: detector self-test did not enumerate '
                            f'{message}')
            continue
        if needle == 'exit-call-status':
            if needle not in probe[0]:
                failures.append(f'#892 aggregation-audit: detector self-test did not '
                                f'enumerate {message}')
        elif not any(needle in slot for slot in unshared_status_slots(probe[1])):
            failures.append(f'#892 aggregation-audit: detector self-test did not flag '
                            f'{message}')
    # MiniMax boundary r18 P0-1: ``else`` runs only on the ``try`` body's normal
    # exit, so it may not cancel a handler that falls off its own end; and a
    # ``finally`` that always returns means the statement never falls through.
    try_else = ast.parse("def _try_else(policy):\n"
                         "    try:\n"
                         "        raise ValueError(policy)\n"
                         "    except ValueError:\n"
                         "        pass\n"
                         "    else:\n"
                         "        return 2\n").body[0]
    if not _body_completes(try_else.body):
        failures.append('#892 aggregation-audit: detector self-test let an ``else`` branch '
                        'cancel a handler that completes on its own')
    try_finally = ast.parse("def _try_finally(policy):\n"
                            "    try:\n"
                            "        raise ValueError(policy)\n"
                            "    finally:\n"
                            "        return 2\n").body[0]
    if _body_completes(try_finally.body):
        failures.append('#892 aggregation-audit: detector self-test treated a ``finally`` '
                        'that always returns as falling off the end')
    # GLM boundary r15 P1-1: an attribute head may name any module member, so it
    # may never inherit the ``(rel, name)`` registration of the file it is
    # written in.  Both call sites are asserted against the one rel that
    # registers ``cleanup_status`` / ``main``.
    registered_rel = 'scripts/gateway-parity/session_domain.py'
    attribute_call = ast.parse("report['status'] = helpers.cleanup_status(audit)\n").body[0].value
    if _proven_callee(attribute_call, {}, None, registered_rel):
        failures.append('#892 aggregation-audit: detector self-test trusted an attribute callee '
                        'through the (rel, name) registry')
    delegated = ast.parse('def publish(report):\n    return helpers.main(report)\n').body[0].body[0].value
    delegated_module = {node.name: node for node in
                        ast.parse('def main(report):\n    return 0\n').body}
    if not _delegated_exit(delegated, delegated_module):
        failures.append('#892 aggregation-audit: detector self-test audited an attribute callee '
                        'against the same-named local def')
    # MiniMax boundary r15 P1-1: the exemption list and the proven list are
    # different claims; assert the r12 rule instead of restating it in prose.
    if set(AGGREGATION_POINTS) & set(NON_AGGREGATION_POINTS):
        failures.append('#892 aggregation-audit: a point is both registered and exempt')
    if any(target not in AGGREGATION_POINTS for target in PROVEN_WRAPPER_CALLEES.values()):
        failures.append('#892 aggregation-audit: a proven wrapper forwards to an unregistered point')
    if set(EXEMPT_PROVEN_CALLEES) & set(PROVEN_WRAPPER_CALLEES):
        failures.append('#892 aggregation-audit: a callee is registered as both exempt and wrapper')
    if any(key not in NON_AGGREGATION_POINTS for key in EXEMPT_PROVEN_CALLEES):
        failures.append('#892 aggregation-audit: an exempt-proven callee has no exemption reason')
    if any(not reason.strip() for reason in EXEMPT_PROVEN_CALLEES.values()):
        failures.append('#892 aggregation-audit: an exempt-proven callee has an empty reason')
    return {'mode': 'aggregation-audit', 'points': points,
            'nonPoints': sorted(f'{rel}:{name}' for rel, name in NON_AGGREGATION_POINTS),
            'unclassified': unclassified, 'failures': failures,
            'failureCount': len(failures),
            'firstFailure': failures[0] if failures else None,
            'passed': not failures}


def control_failure(mode, error):
    """A setup failure still carries a dedicated, searchable message."""
    message = f'#164-fix-r7 {mode}: {type(error).__name__}: {error}'
    return {'mode': mode, 'passed': False, 'failures': [message],
            'failureCount': 1, 'firstFailure': message}


def load_negative_controls():
    path = Path(__file__).resolve().with_name('negative_controls.py')
    spec = importlib.util.spec_from_file_location(
        'session_cleanup_negative_controls', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def negative_restored_replays(output):
    """Replay negative_controls.main()'s final aggregation in memory.

    The real main() runs with one mutant selected; source writes, the child
    check and the restored-positive summary are patched, so no tracked byte is
    touched.  This is the final aggregation point the r6 review found: a
    restored PASS_WITH_SKIPS child must make the batch non-PASS, never PASS/0.
    """
    nc = load_negative_controls()
    name = nc.MUTATIONS[0][0]
    sources = {nc.COMMON_SOURCE, *(Path(row[1]) for row in nc.MUTATIONS)}
    real_write_text = Path.write_text
    real_write_bytes = Path.write_bytes

    def guarded_write_text(path, data, *args, **kwargs):
        if path in sources:
            return len(data)
        return real_write_text(path, data, *args, **kwargs)

    def guarded_write_bytes(path, data, *args, **kwargs):
        if path in sources:
            return len(data)
        return real_write_bytes(path, data, *args, **kwargs)

    @contextlib.contextmanager
    def fake_mutating(*_args, **_kwargs):
        yield {source: source.read_bytes() for source in sources}

    cases = (
        ('pass', 'darwin', 'PASS', 0, 'KILLED', True, ('PASS', 0)),
        ('skip', 'linux', 'PASS_WITH_SKIPS', 1, 'KILLED', True,
         ('PASS_WITH_SKIPS', 1)),
        ('survived', 'darwin', 'PASS', 0, 'SURVIVED', True, ('FAIL', 1)),
        ('invalid', 'darwin', 'PASS', 0, 'INVALID', True, ('BLOCKED', 2)),
        ('restored_failed', 'darwin', 'PASS', 0, 'KILLED', False, ('FAIL', 1)),
    )
    rows = {}
    for tag, platform, restored_status, restored_code, verdict, rows_ok, want in cases:
        case = output / ('negative-restored-' + tag)
        result_rows = ([{'mode': 'wiring', 'status': 'PASS_WITH_CLEANUP_UNVERIFIED',
                         'passed': True}]
                       + ([{'mode': 'clean', 'passed': None, 'skipped': 'non-darwin'}]
                          if restored_status == 'PASS_WITH_SKIPS' else [])) if rows_ok else []
        fake_summary = {'status': restored_status, 'results': result_rows}
        fake = SimpleNamespace(returncode=restored_code, stdout='', stderr='')
        saved = (nc.sys.argv, nc.sys.platform, nc.mutating_sources, nc.run_check,
                 nc.read_summary, nc.classify, nc.pgrep_launcher, nc.MUTATIONS)
        nc.MUTATIONS = [row for row in nc.MUTATIONS if row[0] == name]
        nc.mutating_sources = fake_mutating
        nc.run_check = lambda *_a, **_k: fake
        nc.read_summary = lambda *_a, **_k: (fake_summary, None)
        nc.classify = lambda *_a, **_k: (verdict, 'injected')
        nc.pgrep_launcher = lambda: ''
        nc.sys.argv = ['negative_controls.py', '--output', str(case), '--only', name]
        nc.sys.platform = platform
        try:
            with patch.object(Path, 'write_text', guarded_write_text), \
                    patch.object(Path, 'write_bytes', guarded_write_bytes):
                code = nc.main()
            report = json.loads((case / 'summary.json').read_text())
            observed = (report.get('status'), code)
            failures = [] if observed == want else [
                f'#164-fix-r7 negative-restored {tag}: {observed!r}, want {want!r}']
        except Exception as error:
            observed = (None, None)
            failures = [f'#164-fix-r7 negative-restored {tag}: '
                        f'{type(error).__name__}: {error}']
        finally:
            (nc.sys.argv, nc.sys.platform, nc.mutating_sources, nc.run_check,
             nc.read_summary, nc.classify, nc.pgrep_launcher, nc.MUTATIONS) = saved
        rows[f'{tag}Status'] = observed[0]
        rows[f'{tag}Returncode'] = observed[1]
        rows[f'{tag}Passed'] = not failures
        rows[f'{tag}Failures'] = failures
    return rows


def batch_aggregation_control(md, output):
    """Pin the batch status order, exit codes, and the calibrate mapping.

    The controls module is exercised through its real ``main()`` with only the
    per-case subprocess and the cleanup helper replaced in memory, so a revert
    to "only FAIL propagates" makes this row red.  The calibrate half drives
    ``session_domain.main()``'s real tail with a controls batch that reports
    PASS_WITH_SKIPS and requires BLOCKED / exit 2, never PASS.
    """
    controls = load_controls()

    def replay(tag, cleanup_status, source=True, failing=None):
        """Drive ``controls.main()`` with an in-memory case runner.

        #892 r6: ``failing`` makes exactly that case report FAIL, so the batch
        status must follow the case rows.  A clean status hardcoded into the
        argument list of the shared call (``aggregate_status('PASS',
        cleanup['status'])``) drops the FAIL branch without touching any slot,
        and this row is what catches it (MiniMax concurrency P1-1).
        """
        def fake_run(command, **_kwargs):
            name = command[command.index('--case') + 1]
            accept = (name == 'healthy' or name in controls.LABEL_KNOWN_ACCEPT) \
                and name != failing
            return SimpleNamespace(
                returncode=0 if accept else 1, stderr='',
                stdout=json.dumps({'rule': controls.CASES.get(name),
                                   'status': 'PASS' if accept else 'FAIL'}))

        case = output / ('batch-aggregation-' + tag)
        argv = ['session_domain.py', '--case', 'all', '--output', str(case)]
        if source:
            argv += ['--source', str(case / 'source')]
        stdout = io.StringIO()
        # This replay isolates aggregation; import safety has its own bound tests.
        with patch.object(controls.common, 'verify_loaded_apparatus'), \
                patch.object(controls.subprocess, 'run', side_effect=fake_run), \
                patch.object(controls, 'cleanup_controls',
                             return_value={'status': cleanup_status,
                                           'skipped': (['cleanup-empty-group']
                                                       if cleanup_status == 'PASS_WITH_SKIPS'
                                                       else [])}), \
                patch.object(sys, 'argv', argv), contextlib.redirect_stdout(stdout):
            code = controls.main()
        report = json.loads(stdout.getvalue())
        return {'status': report['status'], 'returncode': code,
                'cleanup': report['cleanup']['status'],
                'notRun': len(report['notRun'])}

    order = {
        'pass': controls.common.aggregate_status('PASS', None)[0],
        'skip': controls.common.aggregate_status('PASS', 'PASS_WITH_SKIPS')[0],
        'blocked': controls.common.aggregate_status('PASS', 'BLOCKED')[0],
        'notRun': controls.common.aggregate_status('PASS', 'NOT_RUN')[0],
        'fail': controls.common.aggregate_status('PASS', 'FAIL')[0],
        'unknown': controls.common.aggregate_status('PASS', 'NOT_A_STATUS')[0],
    }
    # #892 r9: the exit-code half of the same contract was unpinned -- a
    # NOT_RUN code of 1 or 2 kept every fixture green while README and the
    # frozen 20261031-1140-r2 entry register 3 (MiniMax boundary r7 P2-3).
    exit_codes = {
        'map': dict(controls.common.BATCH_EXIT_CODES),
        'pass': controls.common.aggregate_status('PASS', None)[1],
        'skip': controls.common.aggregate_status('PASS', 'PASS_WITH_SKIPS')[1],
        'notRun': controls.common.aggregate_status('PASS', 'NOT_RUN')[1],
        'blocked': controls.common.aggregate_status('PASS', 'BLOCKED')[1],
        'fail': controls.common.aggregate_status('PASS', 'FAIL')[1],
        'unknown': controls.common.aggregate_status('PASS', 'NOT_A_STATUS')[1],
    }
    negative_restored = negative_restored_replays(output)
    observed = {
        'withSkips': replay('with-skips', 'PASS_WITH_SKIPS'),
        'blocked': replay('blocked', 'BLOCKED'),
        'notRun': replay('not-run', 'PASS', source=False),
        'allPass': replay('all-pass', 'PASS'),
        'caseFailure': replay('case-failure', 'PASS', failing='healthy'),
        'order': order,
        'exitCodes': exit_codes,
        'negativeRestored': negative_restored,
        'summaries': {
            'allPass': summarize_results([{'passed': True}, {'passed': True}])[0],
            'withSkip': summarize_results([{'passed': True},
                                           {'passed': None, 'skipped': 'platform'}])[0],
            'failure': summarize_results([{'passed': True}, {'passed': False}])[0],
            'malformed': summarize_results([{'passed': True}, {'passed': None}])[0],
            'allSkipped': summarize_results([{'passed': None, 'skipped': 'platform'}])[0],
        },
    }

    # Calibrate tail: controls PASS_WITH_SKIPS must not satisfy require(PASS).
    script = output / 'batch-aggregation-calibrate-apparatus'
    (script / 'controls').mkdir(parents=True, exist_ok=False)
    (script / 'controls/session_domain.py').write_text(
        "def run_all(*_args, **_kwargs):\n"
        "    return {'status': 'PASS_WITH_SKIPS', "
        "'cleanupSkipped': ['cleanup-empty-group']}\n")
    folder = output / 'batch-aggregation-calibrate-run'
    audit = {'exitCode': 0, 'launcherAndFixtureReaped': True,
             'processTreeCleanup': md.CLEANUP_VERIFIED, 'pid': 1, 'pgid': 1}
    patched = {
        'SCRIPT': script,
        'E1_SOURCES': (),
        'run_fixture': lambda *_a, **_k: copy.deepcopy(audit),
        'validate_calibration': lambda *_a, **_k: {},
        'hashes': lambda: {'batch': 'fixed'},
        'frozen_check': lambda: {'exitCode': 0, 'stdout': '', 'stderr': ''},
        '_verify_cli_imports': lambda: None,  # Synthetic controls; provenance tested by test_safety.
        'baseline_dependency_hashes': lambda _baseline: {},
        'install_shutdown': lambda: None,
        'parity_lock': lambda **_kwargs: contextlib.nullcontext(),
    }
    saved = {name: getattr(md, name) for name in patched}
    captured = io.StringIO()
    try:
        for name, value in patched.items():
            setattr(md, name, value)
        with patch.object(sys, 'argv', [
                'session_domain.py', '--suite', 'calibrate',
                '--output', str(folder), '--baseline', str(folder / 'baseline')]), \
                contextlib.redirect_stdout(captured):
            calibrate_code = md.main()
        calibrate_report = json.loads((folder / 'report.json').read_text())
    finally:
        for name, value in saved.items():
            setattr(md, name, value)
    observed['calibrateSkipped'] = {
        'status': calibrate_report.get('status'),
        'executionStatus': calibrate_report.get('executionStatus'),
        'cleanupStatus': calibrate_report.get('cleanupStatus'),
        'controlsSkipped': calibrate_report.get('controlsSkipped'),
        'failureRule': (calibrate_report.get('failure') or {}).get('rule'),
        'returncode': calibrate_code,
    }
    expected = {
        'withSkips': {'status': 'PASS_WITH_SKIPS', 'returncode': 1},
        'blocked': {'status': 'BLOCKED', 'returncode': 2},
        'notRun': {'status': 'PASS_WITH_SKIPS', 'returncode': 1},
        'allPass': {'status': 'PASS', 'returncode': 0},
        # A failing case row must reach the batch status even though the
        # cleanup helper is clean and the shared call is still called.
        'caseFailure': {'status': 'FAIL', 'returncode': 1},
        'order': {'pass': 'PASS', 'skip': 'PASS_WITH_SKIPS',
                  'blocked': 'BLOCKED', 'notRun': 'NOT_RUN',
                  'fail': 'FAIL', 'unknown': 'FAIL'},
        'exitCodes': {'map': {'PASS': 0, 'PASS_WITH_SKIPS': 1, 'NOT_RUN': 3,
                              'BLOCKED': 2, 'FAIL': 1},
                      'pass': 0, 'skip': 1, 'notRun': 3, 'blocked': 2,
                      'fail': 1, 'unknown': 1},
        'summaries': {'allPass': 'PASS', 'withSkip': 'PASS_WITH_SKIPS',
                      'failure': 'FAIL', 'malformed': 'FAIL',
                      'allSkipped': 'FAIL'},
        'calibrateSkipped': {'status': 'BLOCKED', 'executionStatus': 'BLOCKED',
                             'cleanupStatus': md.CLEANUP_VERIFIED,
                             'controlsSkipped': ['cleanup-empty-group'],
                             'failureRule': 'CONTROLS_SKIPPED',
                             'returncode': 2},
        'negativeRestored': {
            'passStatus': 'PASS', 'passReturncode': 0,
            'skipStatus': 'PASS_WITH_SKIPS', 'skipReturncode': 1,
            'survivedStatus': 'FAIL', 'survivedReturncode': 1,
            'invalidStatus': 'BLOCKED', 'invalidReturncode': 2,
            'restored_failedStatus': 'FAIL', 'restored_failedReturncode': 1,
        },
    }
    audit = aggregation_audit()
    observed['aggregationAudit'] = {
        key: audit[key] for key in ('failureCount', 'firstFailure', 'passed')}
    failures = list(audit['failures'])
    for name, want in expected.items():
        got = observed.get(name) or {}
        for key, value in want.items():
            if got.get(key) != value:
                failures.append(
                    f'#164-fix-r7 batch-aggregation: {name}.{key}={got.get(key)!r}, '
                    f'want {value!r}')
    for row in negative_restored.values():
        if isinstance(row, list):
            failures.extend(row)
    return {'mode': 'batch-aggregation', **observed,
            'expected': expected, 'failures': failures,
            'failureCount': len(failures),
            'firstFailure': failures[0] if failures else None,
            'passed': not failures}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--mode', choices=('clean', 'leak', 'ignore', 'blocked', 'status',
                                           'eperm-live-member', 'identity-lost', 'census-unavailable',
                                           'census-snapshot', 'census-descendant', 'census-unstable',
                                           'census-truncated', 'census-enumeration',
                                           'pid-blocked', 'linux-filtered-proc',
                                           'signal-entry', 'reap-state', 'wiring',
                                           'runtime-negative', 'batch-aggregation',
                                           'aggregation-audit', 'all'),
                        default='all')
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    global md
    md = load_session_domain()
    results = []
    modes = ('clean', 'leak', 'ignore') if args.mode == 'all' else (
        () if args.mode in ('status', 'runtime-negative', 'blocked', 'wiring',
                            'batch-aggregation', 'aggregation-audit',
                            'eperm-live-member', 'identity-lost', 'census-unavailable',
                            'census-snapshot', 'census-descendant', 'census-unstable',
                            'census-truncated', 'census-enumeration', 'pid-blocked',
                            'linux-filtered-proc', 'signal-entry',
                            'reap-state') else (args.mode,))
    for mode in modes:
        result = run_fixture(md, output, mode)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('blocked', 'all'):
        result = blocked_control(md)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('eperm-live-member', 'all'):
        result = eperm_live_member_control(md, output)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('identity-lost', 'all'):
        result = identity_lost_control(md)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('census-unavailable', 'all'):
        result = census_unavailable_control(md)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('census-snapshot', 'all'):
        result = census_snapshot_race_control(md)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('census-descendant', 'all'):
        result = census_descendant_control(md)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('census-unstable', 'all'):
        result = census_unstable_control(md)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('census-truncated', 'all'):
        result = census_truncated_control(md)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('census-enumeration', 'all'):
        result = census_enumeration_failure_control(md)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('pid-blocked', 'all'):
        result = pid_blocked_control(md)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('linux-filtered-proc', 'all'):
        result = linux_filtered_proc_control(md)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('signal-entry', 'all'):
        result = signal_entry_control(md)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('reap-state', 'all'):
        result = reap_state_control(md, output)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('runtime-negative', 'all'):
        result = runtime_negative(md, output)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('status', 'all'):
        for result in status_controls(md):
            results.append({'mode': 'status', **result})
            print(json.dumps(results[-1], ensure_ascii=False), flush=True)
    if args.mode in ('wiring', 'all'):
        result = wiring_control(md, output)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('batch-aggregation', 'all'):
        try:
            result = batch_aggregation_control(md, output)
        except Exception as error:
            result = control_failure('batch-aggregation', error)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('aggregation-audit', 'all'):
        try:
            result = aggregation_audit()
        except Exception as error:
            result = control_failure('aggregation-audit', error)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    status, skipped = summarize_results(results)
    _, exit_code = md.common.aggregate_status(status)
    # #892 r24: the map that computes this exit code is itself under audit.  A
    # same-named redefinition of ``load_session_domain`` hands ``md`` a stub
    # whose ``aggregate_status`` maps every word -- including the audit's own
    # FAIL -- to PASS/0, so a detected failure would still leave as a green
    # summary (MiniMax sibling #1 / DeepSeek sibling #1).  A row that is not a
    # literal pass can never leave on the clean side: the checker's own verdict
    # stays fail-closed whatever the loaded module says.  ``1`` is the product's
    # ``common.BATCH_EXIT_CODES['FAIL']``.
    if exit_code == 0 and any(row.get('passed') is not True for row in results):
        status, exit_code = 'FAIL', 1
    summary = {'status': status, 'exitCode': exit_code, 'output': str(output),
               'skipped': [row.get('mode') for row in skipped], 'results': results}
    (output / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({'status': status, 'exitCode': exit_code,
                      'skipped': summary['skipped'],
                      'summary': str(output / 'summary.json')}, ensure_ascii=False))
    return exit_code


if __name__ == '__main__':
    raise SystemExit(main())

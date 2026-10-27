#!/usr/bin/env python3
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
import contextlib
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
from types import SimpleNamespace
import signal
import subprocess
import sys
import tempfile
import time
from unittest.mock import patch

# Loading the apparatus by explicit path is still an import: stop bytecode
# writes before the loader reads the source, otherwise a __pycache__ appears in
# the frozen apparatus directory and check_frozen (correctly) rejects it.
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-session-cleanup-pyc-')
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
    spec.loader.exec_module(module)
    return module


def load_controls():
    spec = importlib.util.spec_from_file_location('session_cleanup_controls', CONTROLS)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
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
    otherwise the missing result is malformed and fails closed.
    """
    executed = [row for row in results if row.get('passed') is not None]
    skipped = [row for row in results
               if row.get('passed') is None and row.get('skipped')]
    malformed = [row for row in results
                 if row.get('passed') is None and not row.get('skipped')]
    if (malformed or not executed
            or any(row.get('passed') is False for row in executed)):
        return 'FAIL', skipped
    return ('PASS_WITH_SKIPS' if skipped else 'PASS'), skipped


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


def batch_aggregation_control(md, output):
    """Pin the batch status order, exit codes, and the calibrate mapping.

    The controls module is exercised through its real ``main()`` with only the
    per-case subprocess and the cleanup helper replaced in memory, so a revert
    to "only FAIL propagates" makes this row red.  The calibrate half drives
    ``session_domain.main()``'s real tail with a controls batch that reports
    PASS_WITH_SKIPS and requires BLOCKED / exit 2, never PASS.
    """
    controls = load_controls()

    def fake_run(command, **_kwargs):
        name = command[command.index('--case') + 1]
        accept = name == 'healthy' or name in controls.LABEL_KNOWN_ACCEPT
        return SimpleNamespace(
            returncode=0 if accept else 1, stderr='',
            stdout=json.dumps({'rule': controls.CASES.get(name),
                               'status': 'PASS' if accept else 'FAIL'}))

    def replay(tag, cleanup_status, source=True):
        case = output / ('batch-aggregation-' + tag)
        argv = ['session_domain.py', '--case', 'all', '--output', str(case)]
        if source:
            argv += ['--source', str(case / 'source')]
        stdout = io.StringIO()
        with patch.object(controls.subprocess, 'run', side_effect=fake_run), \
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
        'pass': controls.aggregate_status('PASS', None),
        'skip': controls.aggregate_status('PASS', 'PASS_WITH_SKIPS'),
        'blocked': controls.aggregate_status('PASS', 'BLOCKED'),
        'notRun': controls.aggregate_status('PASS', 'NOT_RUN'),
        'fail': controls.aggregate_status('PASS', 'FAIL'),
        'unknown': controls.aggregate_status('PASS', 'NOT_A_STATUS'),
    }
    observed = {
        'withSkips': replay('with-skips', 'PASS_WITH_SKIPS'),
        'blocked': replay('blocked', 'BLOCKED'),
        'notRun': replay('not-run', 'PASS', source=False),
        'allPass': replay('all-pass', 'PASS'),
        'order': order,
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
        'blocked': {'status': 'BLOCKED', 'returncode': 1},
        'notRun': {'status': 'PASS_WITH_SKIPS', 'returncode': 1},
        'allPass': {'status': 'PASS', 'returncode': 0},
        'order': {'pass': 'PASS', 'skip': 'PASS_WITH_SKIPS',
                  'blocked': 'BLOCKED', 'notRun': 'NOT_RUN',
                  'fail': 'FAIL', 'unknown': 'FAIL'},
        'summaries': {'allPass': 'PASS', 'withSkip': 'PASS_WITH_SKIPS',
                      'failure': 'FAIL', 'malformed': 'FAIL',
                      'allSkipped': 'FAIL'},
        'calibrateSkipped': {'status': 'BLOCKED', 'executionStatus': 'BLOCKED',
                             'cleanupStatus': md.CLEANUP_VERIFIED,
                             'controlsSkipped': ['cleanup-empty-group'],
                             'failureRule': 'CONTROLS_SKIPPED',
                             'returncode': 2},
    }
    passed = True
    for name, want in expected.items():
        got = observed.get(name) or {}
        for key, value in want.items():
            if got.get(key) != value:
                passed = False
    return {'mode': 'batch-aggregation', **observed,
            'expected': expected, 'passed': bool(passed)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--mode', choices=('clean', 'leak', 'ignore', 'blocked', 'status',
                                           'eperm-live-member', 'identity-lost', 'census-unavailable',
                                           'census-snapshot', 'census-descendant', 'census-unstable',
                                           'census-truncated', 'census-enumeration',
                                           'pid-blocked', 'linux-filtered-proc',
                                           'signal-entry', 'reap-state', 'wiring',
                                           'runtime-negative', 'batch-aggregation', 'all'),
                        default='all')
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    global md
    md = load_session_domain()
    results = []
    modes = ('clean', 'leak', 'ignore') if args.mode == 'all' else (
        () if args.mode in ('status', 'runtime-negative', 'blocked', 'wiring',
                            'batch-aggregation',
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
        result = batch_aggregation_control(md, output)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    status, skipped = summarize_results(results)
    summary = {'status': status, 'output': str(output),
               'skipped': [row.get('mode') for row in skipped], 'results': results}
    (output / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({'status': status, 'skipped': summary['skipped'],
                      'summary': str(output / 'summary.json')}, ensure_ascii=False))
    return int(status != 'PASS')


if __name__ == '__main__':
    raise SystemExit(main())

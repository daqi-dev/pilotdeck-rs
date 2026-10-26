#!/usr/bin/env python3
"""Behavior controls for the ps-free process-group cleanup probe.

Run with ``python3 -I check_cleanup.py --output <new directory>``.  This file
does not mutate source; the post-commit source mutation controls are in
``negative_controls.py`` beside it.

Modes:
  clean  - reaped leader, empty group -> VERIFIED / PASS
  leak   - reaped leader, sleep grandchild -> GROUP_NOT_EMPTY / non-PASS
  ignore - grandchild ignores SIGTERM -> GROUP_NOT_EMPTY / non-PASS, then SIGKILL
  blocked - killpg denied -> BLOCKED / non-PASS
  status - status mapping for VERIFIED / GROUP_NOT_EMPTY / BLOCKED / NOT_VERIFIED
  runtime-negative - replace probe at runtime with VERIFIED; the leaky fixture must
                     then look accepted (old behavior), and the real probe must
                     still report GROUP_NOT_EMPTY afterwards
  all    - every mode above (default)
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import signal
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


def load_session_domain():
    spec = importlib.util.spec_from_file_location('session_domain', SESSION_DOMAIN)
    module = importlib.util.module_from_spec(spec)
    sys.modules['session_domain'] = module
    spec.loader.exec_module(module)
    return module


def pid_alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except OSError:
        return True


def group_empty(pgid):
    try:
        os.killpg(pgid, 0)
        return False
    except ProcessLookupError:
        return True
    except OSError:
        return False


def force_kill(pgid):
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(pgid, sig)
        except ProcessLookupError:
            return
        except OSError:
            return
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if group_empty(pgid):
                return
            time.sleep(.05)


def run_fixture(md, output, mode):
    folder = output / mode
    folder.mkdir(parents=True, exist_ok=False)
    pid_file = folder / 'grandchild.pid'
    command = [sys.executable, '-I', str(LAUNCHER), '--output', str(folder / 'six-cells'),
               '--mode', mode, '--child-pid-file', str(pid_file)]
    audit = md.run_fixture(folder, ROOT, command=command)
    grandchild = int(pid_file.read_text()) if pid_file.is_file() else None
    if not group_empty(audit['pgid']):
        force_kill(audit['pgid'])
    if not group_empty(audit['pgid']) and grandchild is not None:
        # Last resort for the fixture's own known PID; production reports
        # GROUP_NOT_EMPTY rather than guessing an unowned pid.
        try:
            os.kill(grandchild, signal.SIGKILL)
        except OSError:
            pass
        force_kill(audit['pgid'])
    result = {
        'mode': mode,
        'processTreeCleanup': audit['processTreeCleanup'],
        'topLevel': md.cleanup_status(audit),
        'psCensus': audit.get('psCensus'),
        'probe': audit.get('processGroupProbe'),
        'grandchildPid': grandchild,
        'grandchildAlive': bool(grandchild and pid_alive(grandchild)),
        'groupEmptyAfterRun': group_empty(audit['pgid']),
    }
    if mode == 'clean':
        expected = audit['processTreeCleanup'] == md.CLEANUP_VERIFIED
        expected = expected and result['topLevel'] == 'PASS' and result['groupEmptyAfterRun']
    else:
        expected = audit['processTreeCleanup'] == md.CLEANUP_GROUP_NOT_EMPTY
        expected = expected and result['topLevel'] == md.CLEANUP_UNVERIFIED_STATUS
        expected = expected and result['groupEmptyAfterRun']
        expected = expected and not result['grandchildAlive']
        expected = expected and result['probe'].get('emptyAfterSignal') is True
        expected = expected and result['probe'].get('signal') == (
            'SIGTERM' if mode == 'leak' else 'SIGKILL')
    result['passed'] = bool(expected)
    return result


def runtime_negative(md, output):
    """Show the branch is load-bearing without editing source.

    Replacing only the probe result with VERIFIED makes the leaky fixture look
    clean (this is what the old run_fixture effectively did after wait).  The
    real probe then has to detect the same kind of group.  The grandchild is
    force-killed between the two runs, so the second launcher is a fresh one.
    """
    bypass = output / 'runtime-negative-bypass'
    bypass.mkdir(parents=True, exist_ok=False)
    old_probe = md.probe_process_group
    audit = None
    md.probe_process_group = lambda pgid: {'pgid': pgid, 'probe': 'runtime-bypass',
                                           'status': md.CLEANUP_VERIFIED}
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
        if audit is not None:
            force_kill(audit['pgid'])
    real = run_fixture(md, output / 'runtime-negative-real', 'leak')
    result = {'mode': 'runtime-negative',
              'bypassAcceptedLeak': bypass_accepted,
              'bypass': bypass_result,
              'realDetectsLeak': real['processTreeCleanup'] == md.CLEANUP_GROUP_NOT_EMPTY
                                 and real['topLevel'] == md.CLEANUP_UNVERIFIED_STATUS,
              'real': {'processTreeCleanup': real['processTreeCleanup'],
                       'topLevel': real['topLevel']}}
    result['passed'] = bool(bypass_accepted and result['realDetectsLeak'])
    return result


def blocked_control(md):
    """An inaccessible probe is BLOCKED and maps to the non-PASS status."""
    try:
        with patch.object(md.os, 'killpg', side_effect=PermissionError('denied')):
            probe = md.probe_process_group(999999)
        status = probe['status']
        top = md.cleanup_status({'processTreeCleanup': status})
        error = None
    except Exception as exc:  # a removed BLOCKED branch must be visible, not a crash-only pass
        status = None
        top = None
        error = f'{type(exc).__name__}: {exc}'
    return {'mode': 'blocked', 'processTreeCleanup': status, 'topLevel': top,
            'error': error,
            'passed': status == md.CLEANUP_BLOCKED and top == md.CLEANUP_UNVERIFIED_STATUS}


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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--mode', choices=('clean', 'leak', 'ignore', 'blocked', 'status',
                                           'runtime-negative', 'all'),
                        default='all')
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    md = load_session_domain()
    results = []
    modes = ('clean', 'leak', 'ignore') if args.mode == 'all' else (
        () if args.mode in ('status', 'runtime-negative', 'blocked') else (args.mode,))
    for mode in modes:
        result = run_fixture(md, output, mode)
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.mode in ('blocked', 'all'):
        result = blocked_control(md)
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
    status = 'PASS' if all(result['passed'] for result in results) else 'FAIL'
    summary = {'status': status, 'output': str(output), 'results': results}
    (output / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({'status': status, 'summary': str(output / 'summary.json')}, ensure_ascii=False))
    return int(status != 'PASS')


if __name__ == '__main__':
    raise SystemExit(main())
